mod automation;
mod automation_state;
mod background;
pub(crate) mod code_review;
mod config_reload;
mod helpers;
mod key_handlers;
mod metrics_state;
pub(crate) mod modals;
mod new_session_state;
mod notify_state;
pub(crate) mod search;
mod state;
mod sync_state;
mod task_state;
mod tasks;
mod view;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Position, Rect},
    widgets::{Block, Borders},
};
use tracing::{debug, error, info, warn};

use crate::agent::{BackendRegistry, GenericProvider, Session, SessionBackend};
use crate::git;
use crate::session::{
    AgentDef, AgentRegistry, SessionConfig, SessionId, SessionInfo, SessionStatus, WorktreeInfo,
    DEFAULT_AGENT_NAME,
};

use crate::storage::Database;
use crate::storage::DeletedSessionInfo;
use crate::sync::{self, SharedWorktree, StateDelta, SyncState};
use crate::ui::layout;
use crate::ui::scrollbar::ScrollbarGeom;
use crate::ui::selection::{PaneBounds, Selection, TermPos};
use notify_state::NotificationState;

const MOUSE_SCROLL_LINES: usize = 3;

/// How long the user has to press Ctrl+Z to undo a session delete.
const UNDO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long a status-bar message is shown before reverting to default counts.
const STATUS_MESSAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Ticks per `Working`-spinner frame. The loop ticks ~every 10 ms, so 12 ticks
/// ≈ 125 ms/frame ≈ 8 fps — a smooth spinner without thrashing the renderer.
const SPINNER_TICKS_PER_FRAME: u64 = 12;

/// Upper bound between forced repaints when nothing else marked the UI dirty.
/// The render loop only paints when state changed (a key, agent output, a
/// background poll landing) — this floor guarantees time-driven UI that nothing
/// explicitly flags (the live clock/metrics, cursor blink, a session going
/// quiet `Busy → Waiting`, an expiring status toast) still refreshes promptly.
/// 250 ms ≈ 4 fps when idle, vs. the old unconditional ~100 fps. See
/// `docs/PERFORMANCE.md`.
const FORCE_REDRAW_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Prompt sent to Claude sessions when a worktree rebase has conflicts.
const SYNC_CONFLICT_PROMPT: &str = "Please sync this worktree with main. Run: git fetch origin && git rebase origin/main -- if there are conflicts, resolve them and continue the rebase with git rebase --continue.";

/// Tick delay before sending Enter after pasting text into a session.
/// At ~10ms per tick, 10 ticks ≈ 100ms — enough for the app to process the pasted text.
const DEFERRED_INPUT_DELAY_TICKS: u64 = 10;

/// How often to refresh system metrics (in ticks). At ~10ms per tick, 100 ≈ 1 second.
const METRICS_REFRESH_TICKS: u64 = 100;

/// How often to refresh git stats for the active session (in ticks). Git stats
/// shell out to `git`, so they run on a slower cadence than other metrics
/// (~5 s) and only for the visible session.
const GIT_REFRESH_TICKS: u64 = 500;

/// Ticks (~10 ms each) between config-file mtime polls (~1 s). Cheap: two
/// `stat` calls per poll.
const CONFIG_RELOAD_TICKS: u64 = 100;

/// How often to refresh account usage / rate-limits (in ticks). At ~10ms per
/// tick, 30000 ≈ 5 minutes. Usage windows are coarse and fetching hits the
/// network, so this is deliberately slow; fires once early then every 5 min.
const USAGE_REFRESH_TICKS: u64 = 30_000;

/// Prepared inputs for a `Session::spawn`, produced on the UI thread by
/// [`App::build_spawn_inputs`] and consumed either inline (synchronous spawn)
/// or moved into a blocking task (interactive spawn).
struct SpawnInputs {
    /// Process-launch config (its `cwd` is the symlink workspace for multi-repo
    /// sessions; the primary repo is carried separately).
    config: SessionConfig,
    /// The primary repo path restored onto `SessionInfo.cwd` after spawn.
    primary_cwd: Option<PathBuf>,
    backend: Arc<dyn SessionBackend>,
    provider: Arc<dyn crate::agent::AgentProvider>,
    rows: u16,
    cols: u16,
}

/// Continuation for a backgrounded interactive `Session::spawn`: the metadata
/// and follow-up actions applied once the session is live (in
/// [`App::poll_session_spawn`]).
struct PendingSessionSpawn {
    primary_cwd: Option<PathBuf>,
    worktrees: Vec<WorktreeInfo>,
    additional_dirs: Vec<PathBuf>,
    /// Parent session (lead/worker linkage), captured at kickoff like the
    /// other wizard state so an overlapping flow can't steal it.
    parent_session_id: Option<SessionId>,
    /// A task-initiated spawn's `(task_id, title)`, captured at kickoff so the
    /// prompt is delivered + the task advanced when the session comes up.
    task_prompt: Option<(i64, String)>,
    /// Agent name for the spawn, captured so a failure toast names the real
    /// agent (codex/aider/…) rather than hardcoding "claude".
    agent: String,
    /// Base branch the worktree was forked from (worktree spawns only),
    /// persisted once the session is live so the code-review view can scope its
    /// diff to `<base>..HEAD`. `None` for bare-repo / fork spawns.
    base_branch: Option<String>,
}

/// One remote backend's discovery result: its `backend_type` plus the windows
/// its host reported. Sent once per backend by the restore threads.
type RemoteDiscovery = (String, Vec<crate::agent::backend::DiscoveredSession>);

/// In-flight background restore of remote-backed sessions. Startup readies +
/// discovers only *local* backends synchronously; each remote (`ssh:`/`wsl:`)
/// backend is readied on its own thread, because a single ssh connect can take
/// tens of seconds (or minutes for a down host) and must never block the first
/// frame. Each thread sends one [`RemoteDiscovery`] message; the backend's
/// sessions wait in `pending` and are adopted on the main thread once it
/// reports (in [`App::poll_remote_restore`]).
struct RemoteRestore {
    rx: mpsc::Receiver<RemoteDiscovery>,
    /// Sessions awaiting their backend's discovery, keyed by `backend_type`.
    pending: HashMap<String, Vec<sync::SharedSession>>,
}

/// Continuation for a backgrounded worktree-creation: the wizard inputs needed
/// to resume the spawn flow once the worktrees exist (in
/// [`App::poll_worktree_create`]).
struct PendingWorktreeCreate {
    /// Chosen backend (`ssh:<host>` or `None` for local).
    backend: Option<String>,
    /// Plain (non-worktree) repos to attach alongside the worktree repos.
    normal_repos: Vec<PathBuf>,
    /// Session name when already known (worktree flow); `None` routes through
    /// the name modal.
    session_name: Option<String>,
    /// Base branch the worktrees were forked from, carried through to the spawn
    /// so it can be persisted for the code-review view.
    base_branch: String,
}

/// Create one worktree per repo off the UI thread, rolling back any already
/// created if a later one fails. Returns the worktree infos in `repo_paths`
/// order, or a formatted error after rollback.
fn create_worktrees(
    host: Option<&crate::session::HostDef>,
    repo_paths: &[PathBuf],
    new_branch: &str,
    base_branch: &str,
) -> Result<Vec<WorktreeInfo>, String> {
    let mut worktree_infos: Vec<WorktreeInfo> = Vec::new();
    for repo_path in repo_paths {
        // Multi-repo spawn: the chosen base comes from the *primary* repo's
        // branch list and may not exist in an extra repo. Fall back to that
        // repo's own default branch — mirroring the headless `--add-repo
        // PATH[@BASE]` model where each repo resolves its own base — instead
        // of failing (and rolling back) the whole spawn.
        let repo_base = if git::branch_exists_on(host, repo_path, base_branch) {
            base_branch.to_string()
        } else {
            let branches = git::list_branches_on(host, repo_path).unwrap_or_default();
            match git::default_branch_on(host, repo_path, &branches) {
                Some(fallback) => {
                    tracing::info!(
                        "base '{base_branch}' not found in {}; forking its worktree \
                         from the repo's default branch '{fallback}'",
                        repo_path.display()
                    );
                    fallback
                }
                // No resolvable default: keep the original base so the error
                // below names the branch the user actually picked.
                None => base_branch.to_string(),
            }
        };
        match git::create_worktree_on(host, repo_path, new_branch, &repo_base) {
            Ok(worktree_path) => worktree_infos.push(WorktreeInfo {
                repo_path: repo_path.clone(),
                worktree_path,
                branch: new_branch.to_string(),
            }),
            Err(e) => {
                // Roll back already-created worktrees before bailing.
                for info in &worktree_infos {
                    if let Err(re) =
                        git::remove_worktree_on(host, &info.repo_path, &info.worktree_path)
                    {
                        error!("Failed to roll back worktree: {re}");
                    }
                }
                error!("Failed to create worktree in {}: {e}", repo_path.display());
                return Err(format!("{e:#}"));
            }
        }
    }
    Ok(worktree_infos)
}

/// Result of a background system-metrics refresh, delivered via `App::metrics_refresh`.
struct MetricsRefresh {
    /// The sysinfo collector, returned so it retains CPU-delta state across
    /// refreshes (it is moved into the worker for the duration).
    sys: sysinfo::System,
    /// Aggregate machine + active-session metrics for the info panel.
    metrics: crate::ui::info_panel::SystemMetrics,
    /// Per-session agent metrics parsed from statusline JSON files.
    agent_metrics: Vec<(SessionId, crate::session::AgentMetrics)>,
}

/// Collect machine + active-session + per-agent metrics off the UI thread.
///
/// Owns `sys` for the duration (CPU deltas need a persistent collector) and
/// returns it so the caller can move it back. `active` is the active session's
/// `(backend, backend_id)` for the PID lookup (a control-mode round-trip);
/// `metrics_files` pairs each session id with its statusline JSON path.
fn collect_system_metrics(
    mut sys: sysinfo::System,
    active: Option<(Arc<dyn SessionBackend>, String)>,
    metrics_files: Vec<(SessionId, PathBuf)>,
) -> MetricsRefresh {
    sys.refresh_cpu_all();
    sys.refresh_memory();

    let cpu_percent = sys.global_cpu_usage();
    let memory_used = sys.used_memory();
    let memory_total = sys.total_memory();

    // Resolve the active session's root PID and sample its CPU/RAM.
    let (session_cpu_percent, session_memory_bytes) = active
        .and_then(|(backend, id)| backend.pane_pid(&id).ok().flatten())
        .map(|pid| {
            let pid = sysinfo::Pid::from_u32(pid);
            let kind = sysinfo::ProcessRefreshKind::nothing()
                .with_memory()
                .with_cpu();
            sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), false, kind);
            sys.process(pid)
                .map(|p| (p.cpu_usage(), p.memory()))
                .unwrap_or((0.0, 0))
        })
        .unwrap_or((0.0, 0));

    let metrics = crate::ui::info_panel::SystemMetrics {
        cpu_percent,
        memory_used,
        memory_total,
        session_cpu_percent,
        session_memory_bytes,
    };

    // Poll agent metrics files written by the statusline script.
    let mut agent_metrics = Vec::new();
    for (session_id, path) in metrics_files {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&content) {
                agent_metrics.push((session_id, parse_agent_metrics(&raw)));
            }
        }
    }

    MetricsRefresh {
        sys,
        metrics,
        agent_metrics,
    }
}

/// Aggregate git stats (diff + dirty + ahead/behind) across a session's
/// worktree paths. Shells out to `git` per path, so it runs off the UI thread.
fn aggregate_git_stats(paths: &[PathBuf]) -> Option<crate::session::GitStats> {
    let mut agg: Option<crate::session::GitStats> = None;
    for path in paths {
        if let Some(stats) = crate::git::worktree_stats(path) {
            let acc = agg.get_or_insert_with(Default::default);
            acc.files_changed += stats.files_changed;
            acc.insertions += stats.insertions;
            acc.deletions += stats.deletions;
            acc.dirty |= stats.dirty;
            acc.ahead += stats.ahead;
            acc.behind += stats.behind;
        }
    }
    agg
}

/// Parse agent metrics from a Claude CLI statusline JSON value.
fn parse_agent_metrics(raw: &serde_json::Value) -> crate::session::AgentMetrics {
    use crate::session::AgentMetrics;
    AgentMetrics {
        model_id: raw
            .pointer("/model/id")
            .and_then(|v| v.as_str())
            .map(String::from),
        model_display_name: raw
            .pointer("/model/display_name")
            .and_then(|v| v.as_str())
            .map(String::from),
        total_cost_usd: raw.pointer("/cost/total_cost_usd").and_then(|v| v.as_f64()),
        total_duration_ms: raw
            .pointer("/cost/total_duration_ms")
            .and_then(|v| v.as_u64()),
        total_api_duration_ms: raw
            .pointer("/cost/total_api_duration_ms")
            .and_then(|v| v.as_u64()),
        total_lines_added: raw
            .pointer("/cost/total_lines_added")
            .and_then(|v| v.as_u64()),
        total_lines_removed: raw
            .pointer("/cost/total_lines_removed")
            .and_then(|v| v.as_u64()),
        total_input_tokens: raw
            .pointer("/context_window/total_input_tokens")
            .and_then(|v| v.as_u64()),
        total_output_tokens: raw
            .pointer("/context_window/total_output_tokens")
            .and_then(|v| v.as_u64()),
        context_window_size: raw
            .pointer("/context_window/context_window_size")
            .and_then(|v| v.as_u64()),
        used_percentage: raw
            .pointer("/context_window/used_percentage")
            .and_then(|v| v.as_u64())
            .map(|v| v.min(100) as u8),
        current_input_tokens: raw
            .pointer("/context_window/current_usage/input_tokens")
            .and_then(|v| v.as_u64()),
        current_output_tokens: raw
            .pointer("/context_window/current_usage/output_tokens")
            .and_then(|v| v.as_u64()),
        cache_creation_input_tokens: raw
            .pointer("/context_window/current_usage/cache_creation_input_tokens")
            .and_then(|v| v.as_u64()),
        cache_read_input_tokens: raw
            .pointer("/context_window/current_usage/cache_read_input_tokens")
            .and_then(|v| v.as_u64()),
        cli_version: raw
            .get("version")
            .and_then(|v| v.as_str())
            .map(String::from),
    }
}

pub use modals::{AutomationActionKind, AutomationField, TaskField, TriggerKind};

/// Ticks (~10 ms each) to wait after spawning a session before pasting its
/// automation prompt, giving the agent CLI time to come up (~3 s).
const AGENT_BOOT_DELAY_TICKS: u64 = 300;

pub enum AppMessage {
    KeyPress(KeyCode, KeyModifiers),
    /// Text pasted via the terminal's bracketed paste mode.
    Paste(String),
    /// Mouse wheel up/down, carrying the cursor position so the scroll can be
    /// routed to whichever pane is under the cursor.
    MouseScrollUp {
        x: u16,
        y: u16,
    },
    MouseScrollDown {
        x: u16,
        y: u16,
    },
    MouseClick {
        x: u16,
        y: u16,
        modifiers: KeyModifiers,
    },
    MouseDrag {
        x: u16,
        y: u16,
    },
    MouseUp {
        x: u16,
        y: u16,
    },
    /// Pointer moved with no button held — tracked for hover highlighting.
    MouseMove {
        x: u16,
        y: u16,
    },
    Resize(u16, u16),
    ExternalStateChange(StateDelta),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Success,
    Error,
}

/// Which scroll state a rendered scrollbar drives. Recorded per-frame in
/// [`App::scrollbar_hits`] so mouse clicks/drags on a track can be routed back
/// to the right pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollTarget {
    Terminal,
    TaskPreview,
    FileViewer,
    RunHistory,
    /// The code-review view's diff scrollbar.
    CodeReview,
    /// The active modal's list scrollbar — position is the selection index.
    Modal,
}

/// One scrollbar rendered this frame: its geometry plus the scroll state it
/// drives. Built in [`App::view`], hit-tested by the mouse handlers.
pub(crate) struct ScrollbarHit {
    pub(crate) geom: ScrollbarGeom,
    pub(crate) target: ScrollTarget,
}

/// What a left click on a recorded screen region does. Recorded per-frame in
/// [`App::click_targets`] (mirroring [`App::scrollbar_hits`]) so the mouse
/// handler can route clicks to rows and panes without re-deriving the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClickAction {
    /// Select the session at this display-order index (resolved through
    /// `render_order_indices()` at click time, like `Ctrl+J`/`Ctrl+K`).
    SelectSession(usize),
    /// Select the task at this index in the filtered tasks panel.
    SelectTask(usize),
    /// Select the automation at this index in the automations pane.
    SelectAutomation(usize),
    /// Select + activate the file-viewer row at this flattened tree index
    /// (expand/collapse a directory, open a file).
    SelectFileRow(usize),
    /// Focus the pane — the whole-rect fallback recorded after row targets.
    FocusPane(InputFocus),
    /// Select + activate the row in the active modal's list.
    ModalRow(usize),
    /// A global footer button — dispatches `Action` (Help/Settings/Theme/Quit)
    /// exactly as if its bound key were pressed. Only live when no modal is
    /// open (a modal swallows every click).
    Global(crate::session::Action),
    /// A modal footer button — replays a synthesized key through the open
    /// modal's own handler (Save→Enter/^S, Cancel→Esc, …) so the side effects
    /// match the keyboard path. Dispatched by `handle_modal_click`.
    ModalButton { code: KeyCode, mods: KeyModifiers },
    /// Select the index-th field of the active **editor modal** (Settings /
    /// Automation editor) — `index` is its position in that modal's visible
    /// field order. Dispatched by `handle_modal_click` → `select_modal_field`.
    ModalField(usize),
    /// Focus the repo picker's `Input`/`Search` sub-area (its editable fields).
    RepoFocus(modals::RepoPickerFocus),
    /// Focus an **in-pane editor** (automation / task) and select its index-th
    /// visible field. Dispatched by `activate_click_target`.
    PaneField { focus: InputFocus, index: usize },
    /// Select the code-review row at this index in `code_review.rows`.
    ReviewRow(usize),
    /// A code-review footer button.
    ReviewButton(code_review::ReviewButton),
    /// Jump the diff to the changed-file at this diff-file index (clicked in the
    /// changed-files list).
    ReviewFile(usize),
    /// Select the review-target-picker entry at this index (clicked while the
    /// picker is open).
    ReviewTarget(usize),
    /// Select a central-pane view from the tab strip in the pane's top border
    /// (Agent / Shell / Review). Dispatched by `activate_click_target`.
    CentralTab(CentralTab),
    /// Copy the current status-bar message to the clipboard (click the status
    /// row). Dispatched by `activate_click_target`.
    CopyStatus,
}

/// One clickable region rendered this frame: its rect plus what a click on it
/// does. First recorded match wins, so rows are pushed before their pane's
/// whole-rect `FocusPane` fallback.
pub(crate) struct ClickTarget {
    pub(crate) rect: Rect,
    pub(crate) action: ClickAction,
}

/// A scrollable pane identified by hit-testing the cursor against the layout,
/// used to route a mouse-wheel tick to the pane under the cursor. Broader than
/// [`ScrollTarget`] because the wheel also scrolls the selection-driven list
/// panes (which have no draggable scrollbar of their own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollPane {
    Terminal,
    TaskPreview,
    FileViewer,
    RunHistory,
    SessionList,
    TasksList,
    Automations,
    CodeReview,
    /// The changed-files list shown in the file-viewer column during a review.
    ReviewFiles,
}

#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub level: StatusLevel,
    pub created_at: std::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFocus {
    SessionList,
    /// The automations pane beneath the session list (selecting an automation).
    Automations,
    /// Editing the scoped automation in the central pane (like a session's
    /// terminal — reached with `Enter`/`Ctrl+L` from the automations pane).
    AutomationEditor,
    /// Browsing the scoped automation's run history (beneath the editor),
    /// reached with `Ctrl+L` from the editor. `j`/`k` select a run; `r` triggers
    /// a fresh run.
    AutomationRunHistory,
    /// The tasks panel on the right (selecting/acting on a task).
    TaskList,
    /// Editing the scoped task in the central pane (like a session's terminal —
    /// reached with `Enter`/`e` from the tasks panel; `Esc` returns to it).
    TaskEditor,
    /// The global search strip docked along the bottom (`Ctrl+/` by default).
    /// Captures all input while active; entered/left only via its keybinding /
    /// `Esc`.
    GlobalSearch,
    Terminal,
    FileViewer,
    /// The native code-review view occupying the central pane (toggled like the
    /// shell). Captures keys for its own navigation / commenting.
    CodeReview,
    /// The review's **changed-files list** in the file-viewer column (the
    /// navigation aid shown while a review is open). Focusable like the file
    /// viewer: `j`/`k` walk the files (the diff follows), `Enter` drops into the
    /// diff at the selected file, `r`/`R` toggle reviewed.
    ReviewFiles,
}

/// Which pane the terminal view is showing for a given session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalView {
    Claude,
    Shell,
}

/// The three mutually-exclusive central-pane views, surfaced as a clickable tab
/// strip in the pane's top border. `Agent`/`Shell` map to [`TerminalView`];
/// `Review` is the native code-review overlay. A tab click *selects* the view
/// (see `App::select_central_tab`), unlike the keyboard toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CentralTab {
    Agent,
    Shell,
    Review,
}

/// Holds a recently deleted session for undo (Ctrl+Z) support.
struct PendingDelete {
    session: Session,
    session_id: SessionId,
    created_at: std::time::Instant,
}

/// The TEA model: owns all session/UI state and coordinates side effects.
pub struct App {
    pub(crate) sessions: Vec<Session>,
    pub(crate) active_index: usize,
    backends: BackendRegistry,
    /// Registry of declarative agent definitions, used to build providers per
    /// session at spawn/restart time.
    pub(crate) agents: AgentRegistry,
    /// Configured remote SSH hosts (from `hosts.toml`), used to resolve the
    /// `HostDef` for a session's `ssh:<host>` backend when running git over SSH.
    pub(crate) hosts: crate::session::HostRegistry,
    pub(crate) db: Database,
    pub(crate) focus: InputFocus,
    pub(crate) should_quit: bool,
    pub(crate) status_message: Option<StatusMessage>,
    terminal_rows: u16,
    pub(crate) terminal_cols: u16,
    session_counter: usize,
    /// Whole-feature switches (`[features]` in settings.toml), copied out of
    /// the process-wide settings at construction so tests can flip flags
    /// without touching the first-writer-wins global.
    pub(crate) features: crate::session::settings::FeatureFlags,
    pub(crate) show_info_panel: bool,
    /// Whether the tasks panel column is shown (toggled like the file viewer).
    pub(crate) show_tasks_panel: bool,
    pub(crate) show_file_viewer: bool,
    pub(crate) file_viewer: crate::ui::file_viewer::FileViewerState,
    /// Open native code-review views, keyed by session — persisted per session
    /// like [`Self::session_terminal_views`] (the shell view), so switching
    /// sessions and returning keeps the review open. The active session's entry
    /// (if any) is reached via [`Self::active_review`] / [`Self::active_review_mut`].
    pub(crate) code_reviews: std::collections::HashMap<SessionId, code_review::CodeReviewState>,
    pub(crate) modal: modals::Modal,
    /// In-progress new-session wizard (also drives fork/restart re-spawns).
    pub(crate) new_session: new_session_state::NewSessionWizardState,
    /// Inter-instance DB sync (polls for changes from other thurbox instances).
    sync_state: SyncState,
    /// Worktree-to-main git sync (Ctrl+S).
    worktree_sync: sync_state::WorktreeSyncState,
    /// System/process metrics + the tick counter pacing periodic refreshes.
    metrics: metrics_state::MetricsState,
    /// Background system-metrics refresh (also guards `sys` ownership so
    /// refreshes never overlap), polled each tick.
    metrics_refresh: background::BackgroundTask<MetricsRefresh>,
    /// Background active-session git-stats refresh, polled each tick.
    git_stats: background::BackgroundTask<(SessionId, Option<crate::session::GitStats>)>,
    /// Cached update-check result, rendered as the header "update available"
    /// badge. `Some` only when `[features] version_check` is on and a newer
    /// release is known (from the on-disk cache). Read off the network — see
    /// [`crate::agent::version_check`].
    update_status: Option<crate::agent::version_check::UpdateStatus>,
    /// One-shot background GitHub update check (network), polled each tick. Fires
    /// once on startup when the cache is stale; on success the cache is rewritten
    /// and `update_status` re-read from it.
    version_check_task: background::BackgroundTask<Result<(), String>>,
    /// Background worktree-creation (`git worktree add`) for the new-session
    /// wizard, polled each tick; in-flight state guards against re-entry and
    /// clobbering the pending continuation.
    worktree_create: background::BackgroundTask<Result<Vec<WorktreeInfo>, String>>,
    /// Continuation for a completed worktree-creation: the wizard inputs needed
    /// to resume the spawn flow once the worktrees exist.
    pending_worktree_create: Option<PendingWorktreeCreate>,
    /// Background `Session::spawn` (PTY/tmux window creation) for the
    /// interactive new-session flow, polled each tick. Programmatic spawns
    /// stay synchronous.
    session_spawn: background::BackgroundTask<Result<Session, String>>,
    /// Continuation for a completed background spawn: the metadata + follow-up
    /// (task prompt) to apply once the session is live.
    pending_session_spawn: Option<PendingSessionSpawn>,
    /// Remote-backed sessions still being restored in the background (one
    /// discovery thread per host), drained each tick by
    /// [`Self::poll_remote_restore`]. `None` once every remote backend has
    /// reported (or when there was nothing remote to restore).
    remote_restore: Option<RemoteRestore>,
    /// Deferred inputs: `(session_id, data, tick_at_which_to_send)`.
    /// Used to introduce a small delay between pasting text and pressing Enter.
    deferred_inputs: Vec<(SessionId, Vec<u8>, u64)>,
    /// Per-session terminal view state (Claude vs Shell). Defaults to Claude.
    session_terminal_views: HashMap<SessionId, TerminalView>,
    /// Recently deleted session awaiting finalization or undo (Ctrl+Z).
    pending_delete: Option<PendingDelete>,
    /// Active text selection (click+drag), uses screen-absolute coordinates.
    pub(crate) text_selection: Option<Selection>,
    /// Scrollbars rendered this frame, with the scroll state each drives.
    /// Cleared and rebuilt every [`App::view`]; hit-tested by the mouse handlers
    /// so a click/drag on a track scrolls the owning pane.
    pub(crate) scrollbar_hits: Vec<ScrollbarHit>,
    /// The scrollbar currently being dragged, if any. Set when a click lands on
    /// a track, cleared on mouse-up, so drags keep driving the same pane.
    pub(crate) dragging_scrollbar: Option<ScrollTarget>,
    /// Clickable regions rendered this frame (list rows, pane focus areas,
    /// modal rows). Cleared and rebuilt every [`App::view`]; hit-tested by
    /// [`App::handle_mouse_click`]. First match wins.
    pub(crate) click_targets: Vec<ClickTarget>,
    /// Last pointer position from a motion event, used to highlight the
    /// hovered row in list panes and selector modals.
    pub(crate) mouse_hover: Option<(u16, u16)>,
    /// Cached text extracted from the frame buffer for the current selection.
    selected_text_cache: Option<String>,
    /// Persistent clipboard handle to avoid "dropped too quickly" warnings on Linux.
    clipboard: Option<arboard::Clipboard>,
    /// Persistent list state for the session section (preserves scroll offset).
    pub(crate) session_list_state: ratatui::widgets::ListState,
    /// Automations-pane UI state (cached list, selection, run history, editor).
    pub(crate) automation_ui: automation_state::AutomationUiState,
    /// Tasks-panel UI state (cached list, selection, editor, links).
    pub(crate) task_ui: task_state::TaskUiState,
    /// Global search strip (`Ctrl+/`): cross-scope search docked at the bottom.
    pub(crate) global_search: search::GlobalSearchState,
    /// Currently active theme (built-in preset or custom from themes.toml),
    /// cached so the header doesn't hit SQLite every render. Kept in sync with
    /// `db.set_active_theme` writes.
    pub(crate) active_theme: crate::session::theme_config::ThemeEntry,
    /// User-customizable global keybindings. Loaded from
    /// `~/.config/thurbox/keybindings.json` on startup, falling back to defaults
    /// when the file is missing or malformed.
    pub(crate) keybindings: crate::session::KeyBindings,
    /// Account-level usage/rate-limit info per agent name (the `/usage`
    /// equivalent), fetched in the background and shown for the active
    /// session's agent. Account-global, so keyed by agent rather than session.
    pub(crate) usage: HashMap<String, crate::session::AgentUsage>,
    /// Sends background usage-fetch results back to the app loop.
    usage_tx: mpsc::Sender<(String, crate::session::AgentUsage)>,
    /// Receives background usage-fetch results, drained each tick.
    usage_rx: mpsc::Receiver<(String, crate::session::AgentUsage)>,
    /// Config-load problems collected at startup (keybindings.json here,
    /// agents.toml/hosts.toml reported by main), shown joined in one status
    /// toast via [`Self::report_config_warnings`].
    config_warnings: Vec<String>,
    /// Receives the result of the silent startup auto-update, which runs on a
    /// background thread so a slow download never blocks the TUI from starting
    /// (`[features] auto_update`; see `main::spawn_auto_update`). `None` when the
    /// feature is off / this is a dev build (no thread spawned). Drained each
    /// tick by [`Self::poll_auto_update`]; sends one "Updated …" message only
    /// when binaries were actually replaced.
    auto_update_rx: Option<mpsc::Receiver<String>>,
    /// Last-seen mtimes of the live-reloadable config files (see
    /// [`Self::poll_config_reload`]).
    config_reload: config_reload::ConfigReloadState,
    /// OS notification dispatcher — `None` when the feature is disabled
    /// (`[features] notifications = false`) so the background thread never
    /// starts. The wrapper tracks per-session prior status + last-fired-at so
    /// dedup and "only on transition" logic live next to the sender.
    notification_state: Option<NotificationState>,
    /// Redraw-throttling dirty flag. The render loop paints only when this is
    /// set (or `FORCE_REDRAW_INTERVAL` elapsed). Starts `true` so the first
    /// frame always paints. Set by [`Self::request_redraw`] from `update`,
    /// state-changing tick steps, and the agent-output detector.
    needs_redraw: bool,
    /// Current frame of the `Working` status spinner (index into
    /// [`crate::ui::SPINNER_FRAMES`]). Advanced from `tick_count` in
    /// [`Self::refresh_session_statuses`]; only forces a repaint while a session
    /// is actually working, so an idle TUI still paints ~4 fps.
    spinner_frame: usize,
    /// The session that was focused on the previous status refresh. When focus
    /// moves off a `done` session, that session is marked "seen" (→ `Idle`), so
    /// the blue `Done` state stays visible until you actually switch away.
    last_active_session_id: Option<crate::session::SessionId>,
    /// Cached persisted hook-status rows (`session signal` state). Reloaded only
    /// when the DB's `data_version` moves (an *external* signal), not on every
    /// tick — the per-tick `data_version` read is far cheaper than the
    /// sessions-table scan it replaces. This process's own `seen_at` writes
    /// don't bump our `data_version`, so they're applied write-through in
    /// [`Self::refresh_session_statuses`]. See `docs/PERFORMANCE.md`.
    cached_hook_states: HashMap<crate::session::SessionId, crate::storage::HookRow>,
    /// `data_version` observed at the last [`Self::cached_hook_states`] reload
    /// (`None` = never loaded, which forces the first load).
    hook_states_version: Option<i64>,
    /// Remote hook events whose pane didn't match a session yet, kept for
    /// retry: the subscription's initial catch-up report arrives while the
    /// background restore is still discovering/adopting that host's windows,
    /// so dropping unmatched events would lose e.g. a `done` set while the TUI
    /// was closed. Entries carry their arrival time and expire (another
    /// instance's panes never match). See [`Self::drain_remote_hook_events`].
    pending_remote_hook_events: Vec<(String, String, String, std::time::Instant)>,
    /// When the last frame was painted, for the forced-redraw floor.
    last_draw_at: std::time::Instant,
    /// Cheap rolling signature of agent output across all sessions (sum of each
    /// session's monotonic `last_output_at`). A change means new output arrived
    /// — detected without locking any vt100 parser. See
    /// [`Self::detect_output_redraw`].
    last_output_gen: u64,
    /// Cached session-list ordering (`(content-signature, order)`). The order
    /// depends only on the session set's grouping/nesting inputs, so it is
    /// reused across frames until [`Self::session_order_signature`] changes,
    /// skipping the per-frame grouping/sort/nest work. See `render_left_panel`.
    cached_session_order: Option<(u64, crate::ui::project_list::SessionOrder)>,
}

const EDITOR_NOT_CONFIGURED: &str =
    "No editor configured — set `editor_command` via MCP or export $EDITOR/$VISUAL";

/// Output-quiescence threshold that breaks a *stuck* `working` hook state.
///
/// TUI agents continuously animate their in-progress line while a turn runs
/// (Claude's `… (Xs · esc to interrupt)` ticks the elapsed seconds at least
/// once a second), so a genuinely-working session is never quiet for long. But
/// when a turn is **interrupted** (Esc / Ctrl+C) Claude Code fires *no* hook —
/// it has no interrupt/idle-prompt event (verified against the hooks docs) — so
/// the persisted state stays `working` forever and the dot spins indefinitely.
/// If a `working` session has produced no terminal output for this long, the
/// agent is actually idle at its prompt, so we fall back to `Idle`. Hooks stay
/// the primary signal (this only rescues a missed `done`/`idle` edge); the
/// threshold is generous so a slow-but-live turn never trips it.
const WORKING_OUTPUT_STALE_MS: u64 = 10_000;

/// Map a session's persisted hook state to its rendered [`SessionStatus`]. Pure
/// so it's unit-testable without an `App`/DB. `exited` forces `Idle` (a crashed/
/// finished process); `just_seen` is `true` when the user just moved focus off a
/// `done` session this tick (acknowledged → `Idle`); `quiet_for_ms` is the time
/// since the session's last terminal output, used to rescue a stuck `working`
/// state (see [`WORKING_OUTPUT_STALE_MS`]). A `done` session is `Done` (blue)
/// until seen; `idle`/missing/unknown states are `Idle`.
fn derive_session_status(
    hook: Option<&crate::storage::HookRow>,
    exited: bool,
    just_seen: bool,
    quiet_for_ms: u64,
) -> SessionStatus {
    if exited {
        return SessionStatus::Idle;
    }
    match hook.and_then(|h| h.state.as_deref()) {
        // A live `working` turn keeps emitting output; a stuck one (interrupt /
        // crash / an agent that missed its done edge) goes quiet → fall to Idle.
        Some("working") if quiet_for_ms <= WORKING_OUTPUT_STALE_MS => SessionStatus::Working,
        Some("working") => SessionStatus::Idle,
        Some("blocked") => SessionStatus::Blocked,
        Some("done") => {
            let state_at = hook.and_then(|h| h.state_at).unwrap_or(0);
            let seen_at = hook.and_then(|h| h.seen_at).unwrap_or(0);
            if just_seen || seen_at >= state_at {
                SessionStatus::Idle
            } else {
                SessionStatus::Done
            }
        }
        _ => SessionStatus::Idle,
    }
}

/// Spin up the OS notification dispatcher when the feature is enabled,
/// returning `None` otherwise so the background thread never starts.
/// Reads the process-wide settings directly — they're already published by
/// `main` before `App::new` runs.
fn build_notification_state() -> Option<NotificationState> {
    let settings = crate::session::settings::global();
    if !settings.features.notifications {
        return None;
    }
    // Resolve the delivery backend (dbus / Windows-toast / macOS / none) from
    // the configured preference plus host probing, then start the dispatcher
    // for it. A `none` backend (e.g. WSL without powershell, or backend="off")
    // still starts the thread but drops every notification — the reason is
    // recorded for the `thurbox-cli notify` diagnostic rather than silently
    // lost as before.
    let backend = crate::notifications::detect_backend(settings.notifications.backend);
    if !backend.is_deliverable() {
        debug!(
            "notifications enabled but no deliverable backend: {}",
            backend.label()
        );
    }
    let sender = crate::notifications::start(backend);
    Some(NotificationState::new(sender, settings.notifications))
}

impl App {
    pub fn new(
        rows: u16,
        cols: u16,
        backends: BackendRegistry,
        agents: AgentRegistry,
        db: Database,
    ) -> Self {
        // Resolve the persisted active theme — built-in or custom — defaulting
        // to the Default preset when unset/unknown.
        let active_theme = db
            .get_active_theme()
            .ok()
            .flatten()
            .as_deref()
            .and_then(crate::ui::theme::find_theme_entry)
            .unwrap_or_else(|| {
                crate::session::theme_config::ThemeEntry::from_preset(
                    crate::session::ThemePreset::Default,
                )
            });

        // Load keybindings from JSON config or fall back to defaults. Problems
        // are collected into `config_warnings` so the first frame can surface
        // them in the status bar (a log-only warning is invisible in a TUI).
        let mut config_warnings: Vec<String> = Vec::new();
        let keybindings = match crate::storage::keybindings::load_keybindings_json() {
            Ok(Some(json)) => match crate::session::KeyBindings::from_json_with_warnings(&json) {
                Ok((bindings, warnings)) => {
                    config_warnings
                        .extend(warnings.iter().map(|w| format!("keybindings.json: {w}")));
                    bindings
                }
                Err(e) => {
                    config_warnings
                        .push(format!("keybindings.json: {e}; using default keybindings"));
                    crate::session::KeyBindings::default()
                }
            },
            Ok(None) => crate::session::KeyBindings::default(),
            Err(e) => {
                config_warnings.push(format!("keybindings.json: {e}; using default keybindings"));
                crate::session::KeyBindings::default()
            }
        };
        for w in &config_warnings {
            tracing::warn!("{w}");
        }

        let session_counter = db.get_session_counter().unwrap_or(0);

        let mut sync_state = SyncState::new();

        // Initialize the sync snapshot from the current DB state so the first
        // poll doesn't produce a false delta treating everything as "added".
        if let Ok(initial_state) = db.load_shared_state() {
            sync_state.set_initial_snapshot(initial_state);
        }

        let (usage_tx, usage_rx) = mpsc::channel();

        let mut app = Self {
            sessions: Vec::new(),
            active_index: 0,
            backends,
            agents,
            hosts: crate::session::HostRegistry::default(),
            db,
            focus: InputFocus::SessionList,
            should_quit: false,
            status_message: None,
            terminal_rows: rows,
            terminal_cols: cols,
            session_counter,
            features: crate::session::settings::global().features,
            show_info_panel: false,
            show_tasks_panel: false,
            show_file_viewer: false,
            file_viewer: crate::ui::file_viewer::FileViewerState::new(),
            code_reviews: std::collections::HashMap::new(),
            modal: modals::Modal::None,
            new_session: new_session_state::NewSessionWizardState::default(),
            sync_state,
            worktree_sync: sync_state::WorktreeSyncState::default(),
            metrics: metrics_state::MetricsState::new(),
            metrics_refresh: background::BackgroundTask::default(),
            git_stats: background::BackgroundTask::default(),
            // Seed the badge from the cache (no network); refreshed on first
            // tick if the flag is on and the cache is stale.
            update_status: if crate::session::settings::global().features.version_check {
                crate::agent::version_check::read_cached_status()
            } else {
                None
            },
            version_check_task: background::BackgroundTask::default(),
            worktree_create: background::BackgroundTask::default(),
            pending_worktree_create: None,
            session_spawn: background::BackgroundTask::default(),
            pending_session_spawn: None,
            remote_restore: None,
            deferred_inputs: Vec::new(),
            session_terminal_views: HashMap::new(),
            pending_delete: None,
            text_selection: None,
            scrollbar_hits: Vec::new(),
            dragging_scrollbar: None,
            click_targets: Vec::new(),
            mouse_hover: None,
            selected_text_cache: None,
            clipboard: arboard::Clipboard::new().ok(),
            session_list_state: ratatui::widgets::ListState::default(),
            automation_ui: automation_state::AutomationUiState::default(),
            task_ui: task_state::TaskUiState::default(),
            global_search: search::GlobalSearchState::default(),
            active_theme,
            keybindings,
            usage: HashMap::new(),
            usage_tx,
            usage_rx,
            config_warnings: Vec::new(),
            auto_update_rx: None,
            config_reload: config_reload::ConfigReloadState {
                agents_mtime: config_reload::agents_mtime(),
                keybindings_mtime: config_reload::keybindings_mtime(),
                settings_mtime: config_reload::settings_mtime(),
            },
            notification_state: build_notification_state(),
            needs_redraw: true,
            spinner_frame: 0,
            last_active_session_id: None,
            cached_hook_states: HashMap::new(),
            pending_remote_hook_events: Vec::new(),
            hook_states_version: None,
            last_draw_at: std::time::Instant::now(),
            last_output_gen: 0,
            cached_session_order: None,
        };
        app.report_config_warnings(config_warnings);
        app
    }

    /// Surface config-load warnings in the status bar (they are otherwise only
    /// visible in the log file, which nobody watches while the TUI owns the
    /// screen). Accumulates across calls — main reports agents.toml/hosts.toml
    /// problems after construction — and shows them joined in one toast.
    pub fn report_config_warnings(&mut self, warnings: Vec<String>) {
        if warnings.is_empty() {
            return;
        }
        self.config_warnings.extend(warnings);
        let text = format!("Config: {}", self.config_warnings.join(" · "));
        self.set_status(StatusLevel::Error, text);
    }

    /// Attach the receiver for the background startup auto-update (see
    /// `main::spawn_auto_update`). The update runs off-thread so its download
    /// never delays the first frame; the result is drained in [`Self::tick`].
    pub fn set_auto_update_receiver(&mut self, rx: mpsc::Receiver<String>) {
        self.auto_update_rx = Some(rx);
    }

    /// Drain the background auto-update result. The thread sends at most one
    /// message — only when binaries were actually replaced — so we surface it as
    /// an info toast and drop the receiver. A disconnected channel (the thread
    /// finished with nothing to report, or failed) also drops the receiver so we
    /// stop polling.
    fn poll_auto_update(&mut self) {
        let Some(rx) = &self.auto_update_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(msg) => {
                self.set_status(StatusLevel::Info, msg);
                self.auto_update_rx = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.auto_update_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Reload `agents.toml` / `keybindings.json` in place when their mtime
    /// changes — editing either takes effect without a restart. Self-writes
    /// (the F1 editor persisting a rebind) refresh the stored mtime at save
    /// time, so they don't re-toast here.
    fn poll_config_reload(&mut self) {
        if config_reload::agents_mtime() != self.config_reload.agents_mtime {
            self.reload_agents_config();
        }

        let kb_mtime = config_reload::keybindings_mtime();
        if kb_mtime != self.config_reload.keybindings_mtime {
            self.config_reload.keybindings_mtime = kb_mtime;
            self.reload_keybindings_config();
        }

        if config_reload::settings_mtime() != self.config_reload.settings_mtime {
            self.reload_settings_config();
        }
    }

    /// Reload `agents.toml` and toast the result. Caller has already detected an
    /// mtime change; this re-stats afterwards so a re-seeded file is recorded.
    fn reload_agents_config(&mut self) {
        let (registry, warnings) = crate::agent::agent_config::load_or_seed_with_warnings();
        self.agents = registry;
        // Re-stat after the load: a missing file gets re-seeded by it.
        self.config_reload.agents_mtime = config_reload::agents_mtime();
        self.toast_config_reload("agents.toml reloaded", &warnings);
    }

    /// Reload `keybindings.json` and toast the result. Caller has already
    /// recorded the new mtime.
    fn reload_keybindings_config(&mut self) {
        let (bindings, warnings) = Self::load_keybindings_with_warnings();
        self.keybindings = bindings;
        self.toast_config_reload("keybindings.json reloaded", &warnings);
    }

    /// Reload `settings.toml` when it changes on disk (a hand-edit, the in-TUI
    /// panel, or another instance). Re-applies the live feature flags in place;
    /// restart-only values stay frozen in the global, so the toast flags when a
    /// restart is needed. Caller has already detected the mtime change; this
    /// re-stats afterwards so a re-seeded file is recorded.
    fn reload_settings_config(&mut self) {
        let (settings, mut warnings) = crate::agent::settings_config::load_or_seed_with_warnings();
        if settings.restart_only_differs(crate::session::settings::global()) {
            warnings.push("restart to apply some changes".into());
        }
        self.apply_live_settings(&settings);
        self.config_reload.settings_mtime = config_reload::settings_mtime();
        self.toast_config_reload("settings.toml reloaded", &warnings);
    }

    /// Apply the **live** portion of `settings` (the UI-panel feature flags read
    /// from `App.features` each frame) and resize panes to match. The
    /// restart-only values are intentionally left to the next launch. Shared by
    /// the settings panel's save path and the live-reload poll.
    pub(crate) fn apply_live_settings(&mut self, settings: &crate::session::settings::Settings) {
        self.features = settings.features;
        self.enforce_feature_visibility();
        self.resize_sessions_to_content_area();
    }

    /// Tear down any panel/view/focus that a now-disabled live feature flag
    /// leaves stranded. The open-state booleans (`show_*`), the per-session
    /// shell views, and the open code reviews are all opt-in toggles that
    /// survive a flag flip, so without this a feature disabled at runtime would
    /// keep rendering its panel even though its tab/footer affordance is gone.
    /// Each branch only forces the *hidden* state, so it's idempotent and never
    /// re-opens anything when a flag is turned back on.
    fn enforce_feature_visibility(&mut self) {
        if !self.features.info_panel {
            self.show_info_panel = false;
        }
        if !self.features.file_viewer {
            self.show_file_viewer = false;
            if self.focus == InputFocus::FileViewer {
                self.focus = InputFocus::SessionList;
            }
        }
        if !self.features.tasks {
            self.show_tasks_panel = false;
            if matches!(self.focus, InputFocus::TaskList | InputFocus::TaskEditor) {
                self.focus = InputFocus::SessionList;
            }
        }
        if !self.features.automations
            && matches!(
                self.focus,
                InputFocus::Automations
                    | InputFocus::AutomationEditor
                    | InputFocus::AutomationRunHistory
            )
        {
            self.focus = InputFocus::SessionList;
        }
        if !self.features.global_search && self.global_search.active {
            self.close_global_search();
        }
        if !self.features.shell_pane {
            // Flip every session showing its shell back to the agent view (the
            // Shell tab/toggle is gone, so there's no way back otherwise).
            for view in self.session_terminal_views.values_mut() {
                if *view == TerminalView::Shell {
                    *view = TerminalView::Claude;
                }
            }
        }
        if !self.features.code_review && !self.code_reviews.is_empty() {
            self.code_reviews.clear();
            if matches!(self.focus, InputFocus::CodeReview | InputFocus::ReviewFiles) {
                self.focus = InputFocus::Terminal;
            }
        }
    }

    /// Record the current `settings.toml` mtime so the next reload poll doesn't
    /// treat the settings panel's own write as an external edit.
    pub(crate) fn mark_settings_saved(&mut self) {
        self.config_reload.settings_mtime = config_reload::settings_mtime();
    }

    /// Load the on-disk keybindings, falling back to defaults (with a warning)
    /// on any read/parse error.
    fn load_keybindings_with_warnings() -> (crate::session::KeyBindings, Vec<String>) {
        match crate::storage::keybindings::load_keybindings_json() {
            Ok(Some(json)) => match crate::session::KeyBindings::from_json_with_warnings(&json) {
                Ok((bindings, warnings)) => (
                    bindings,
                    warnings
                        .into_iter()
                        .map(|w| format!("keybindings.json: {w}"))
                        .collect(),
                ),
                Err(e) => (
                    crate::session::KeyBindings::default(),
                    vec![format!("keybindings.json: {e}; using default keybindings")],
                ),
            },
            Ok(None) => (crate::session::KeyBindings::default(), Vec::new()),
            Err(e) => (
                crate::session::KeyBindings::default(),
                vec![format!("keybindings.json: {e}; using default keybindings")],
            ),
        }
    }

    /// Toast the outcome of a live config reload: an info `ok` line when clean,
    /// otherwise the joined warnings (also logged).
    fn toast_config_reload(&mut self, ok: &str, warnings: &[String]) {
        if warnings.is_empty() {
            self.set_status(StatusLevel::Info, ok);
        } else {
            self.set_status(
                StatusLevel::Error,
                format!("Config: {}", warnings.join(" · ")),
            );
        }
        for w in warnings {
            warn!("{w}");
        }
    }

    /// Record the current `keybindings.json` mtime so the next reload poll
    /// doesn't treat our own write as an external edit.
    pub(crate) fn mark_keybindings_saved(&mut self) {
        self.config_reload.keybindings_mtime = config_reload::keybindings_mtime();
    }

    /// Build an [`AgentProvider`](crate::agent::AgentProvider) for a session
    /// config by looking its agent up in the registry. Falls back to the
    /// registry default, then to the built-in default, so a stale/unknown agent
    /// name never breaks spawning.
    ///
    /// For **adoption** (attaching to an already-running window). Paths that
    /// launch a new process use `launch_provider_for`, which also adapts the
    /// def's args for a remote host.
    pub fn provider_for(&self, config: &SessionConfig) -> Arc<dyn crate::agent::AgentProvider> {
        Arc::new(GenericProvider::new(self.agent_def_for(&config.agent)))
    }

    /// [`Self::provider_for`], plus remote arg adaptation: when `config` targets
    /// a remote (SSH/WSL) backend, the def's args that reference thurbox-managed
    /// config files by *local* path (claude's hooks `--settings …`) are rewritten
    /// for the host — materialized at a home-translated remote path, or stripped
    /// when no remote path can work — because an unresolvable path kills the
    /// agent on launch ("Settings file not found"). Shares the headless spawn's
    /// implementation; used by every path that launches a new agent process
    /// (spawn, restore, respawn-on-restore).
    fn launch_provider_for(&self, config: &SessionConfig) -> Arc<dyn crate::agent::AgentProvider> {
        let mut def = self.agent_def_for(&config.agent);
        if let Some(h) = self.host_for_backend(config.backend.as_deref()) {
            def.args = crate::session_ops::spawn::adapt_agent_args_for_remote(h, def.args);
        }
        Arc::new(GenericProvider::new(def))
    }

    /// Resolve the [`AgentDef`] for an agent name via the same fallback chain as
    /// [`Self::provider_for`] (named agent → registry default → built-in
    /// default). Used to decide resume/fork behaviour at restart time.
    fn agent_def_for(&self, agent: &str) -> AgentDef {
        self.agents
            .get(agent)
            .or_else(|| self.agents.default_agent())
            .cloned()
            .unwrap_or_else(|| {
                crate::agent::agent_config::builtin_registry()
                    .default_agent()
                    .cloned()
                    .expect("built-in registry always has a default agent")
            })
    }

    /// Entry point for the new-session wizard.
    ///
    /// When any off-local host is available — a configured SSH/WSL host
    /// (`hosts.toml`) or an auto-discovered WSL distro — first shows the host
    /// picker so the user can choose where the session runs; otherwise goes
    /// straight to the repo picker (preserving the local-only UX).
    pub(crate) fn start_new_session(&mut self) {
        // Clear any choice left over from a previously cancelled flow.
        self.new_session.backend = None;

        if self.hosts.is_empty() {
            self.open_repo_picker();
            return;
        }

        let mut choices = vec![crate::ui::host_picker_modal::HostChoice {
            label: "local".to_string(),
            backend: String::new(),
        }];
        for host in &self.hosts.hosts {
            choices.push(crate::ui::host_picker_modal::HostChoice {
                label: format!("{}  ({})", host.name, host.picker_detail()),
                backend: host.backend_name(),
            });
        }
        self.modal = modals::Modal::HostPicker(crate::ui::host_picker_modal::HostPickerState {
            choices,
            selected_index: 0,
        });
    }

    /// Open the repo picker modal for creating a new session.
    ///
    /// Loads the target host's bookmarks from the database (bookmarks are
    /// host-scoped — a remote target shows the repos previously used *on that
    /// host*, never local paths) and shows the repo picker modal. A remote
    /// target with no bookmarks yet opens with the path input focused for a
    /// typed remote path; once it has history it opens on the list like a
    /// local target.
    pub(crate) fn open_repo_picker(&mut self) {
        let remote = self.new_session.backend.is_some();
        let bookmarks = self.load_repo_bookmarks();
        let empty = bookmarks.is_empty();
        let mut rp = modals::RepoPickerModal::default();
        Self::rebuild_repo_picker_rows(&mut rp, bookmarks);
        if remote && empty {
            rp.focus = modals::RepoPickerFocus::Input;
        }
        self.modal = modals::Modal::RepoPicker(rp);
    }

    /// The host-scope key for repo bookmarks: the new-session wizard's target
    /// backend name (`ssh:<name>` / `wsl:<name>`), or `""` for local — the
    /// `repo_bookmarks.host` column (schema v39).
    pub(super) fn bookmark_host_key(&self) -> &str {
        self.new_session.backend.as_deref().unwrap_or_default()
    }

    /// Load persisted repo bookmarks for the new-session wizard's current
    /// target host, logging (and swallowing) any DB error.
    fn load_repo_bookmarks(&self) -> Vec<crate::storage::repo_bookmarks::RepoBookmark> {
        match self.db.list_repo_bookmarks(self.bookmark_host_key()) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Failed to load repo bookmarks: {e}");
                Vec::new()
            }
        }
    }

    /// Re-read bookmarks and rebuild the open repo picker's rows in place
    /// (re-scanning parent folders). Used after importing/deleting a bookmark.
    pub(crate) fn refresh_repo_picker_rows(&mut self) {
        let bookmarks = self.load_repo_bookmarks();
        let modals::Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        Self::rebuild_repo_picker_rows(rp, bookmarks);
    }

    /// (Re)build the repo picker rows from persisted bookmarks, **re-scanning**
    /// parent bookmarks for their current git sub-directories. Standalone repos
    /// become one row; a parent becomes a header row followed by an indented
    /// child row per discovered repo (children are ephemeral — never persisted).
    /// Preserves the existing search input by recomputing the filter.
    fn rebuild_repo_picker_rows(
        rp: &mut modals::RepoPickerModal,
        bookmarks: Vec<crate::storage::repo_bookmarks::RepoBookmark>,
    ) {
        use std::collections::HashSet;

        rp.bookmarks.clear();
        rp.selected.clear();
        rp.worktree.clear();
        rp.is_header.clear();
        rp.is_child.clear();

        // Scan each parent once; a path that appears as a child of any parent
        // takes precedence over a standalone bookmark of the same path, so the
        // repo is shown only once (grouped under its parent).
        let scans: HashMap<PathBuf, Vec<PathBuf>> = bookmarks
            .iter()
            .filter(|b| b.is_parent)
            .map(|b| {
                (
                    b.repo_path.clone(),
                    crate::git::scan_child_repos(&b.repo_path),
                )
            })
            .collect();
        let child_paths: HashSet<&PathBuf> = scans.values().flatten().collect();

        // `emitted` guards against any path being rendered twice (duplicate
        // bookmarks, a child shared by two parents, a parent nested in another).
        let mut emitted: HashSet<PathBuf> = HashSet::new();
        for bm in &bookmarks {
            Self::emit_bookmark_row(rp, bm, &scans, &child_paths, &mut emitted);
        }

        rp.list_index = 0;
        rp.recompute_filter();
    }

    /// Emit the row(s) for a single bookmark into the repo picker: a parent
    /// header followed by its scanned children, or a standalone repo.
    /// `emitted` dedupes paths across the whole list; `child_paths` lets a
    /// standalone bookmark be dropped when a parent already covers it.
    fn emit_bookmark_row(
        rp: &mut modals::RepoPickerModal,
        bm: &crate::storage::repo_bookmarks::RepoBookmark,
        scans: &HashMap<PathBuf, Vec<PathBuf>>,
        child_paths: &std::collections::HashSet<&PathBuf>,
        emitted: &mut std::collections::HashSet<PathBuf>,
    ) {
        if !bm.is_parent {
            // Drop a standalone bookmark that is already covered by a parent.
            if child_paths.contains(&bm.repo_path) {
                return;
            }
            if emitted.insert(bm.repo_path.clone()) {
                rp.push_row(bm.repo_path.clone(), false, false, false);
            }
            return;
        }
        if !emitted.insert(bm.repo_path.clone()) {
            return;
        }
        rp.push_row(bm.repo_path.clone(), false, true, false);
        for child in scans.get(&bm.repo_path).into_iter().flatten() {
            if emitted.insert(child.clone()) {
                rp.push_row(child.clone(), false, false, true);
            }
        }
    }

    #[cfg(test)]
    fn next_session_name(&mut self) -> String {
        self.session_counter += 1;
        self.session_counter.to_string()
    }

    pub(crate) fn spawn_session_with_config(&mut self, config: &SessionConfig) {
        let mut config = config.clone();
        // Apply the host chosen in the new-session wizard (None = local).
        if config.backend.is_none() {
            config.backend = self.new_session.backend.take();
        }
        self.prepare_spawn(config, Vec::new());
    }

    /// Route session creation through the name modal, then agent selection.
    ///
    /// Shows an empty session-name modal. After the user enters a name, the
    /// agent picker is shown, then spawn.
    pub(crate) fn prepare_spawn(&mut self, config: SessionConfig, worktrees: Vec<WorktreeInfo>) {
        // Show session name modal (empty — user types from scratch).
        self.new_session.spawn_config = Some(config);
        self.new_session.spawn_worktrees = worktrees;
        self.modal = modals::Modal::SessionName(modals::SessionNameModal::default());
    }

    /// Continue spawn after the user has chosen a session name: open the agent
    /// picker populated from the registry. With zero or one agent the picker is
    /// skipped and the session spawns immediately.
    fn finish_prepare_spawn(
        &mut self,
        name: String,
        config: SessionConfig,
        worktrees: Vec<WorktreeInfo>,
    ) {
        let names = self.agents.names();
        if names.len() <= 1 {
            let mut config = config;
            config.agent = names
                .first()
                .map(|s| s.to_string())
                .unwrap_or_else(|| DEFAULT_AGENT_NAME.to_string());
            self.do_spawn_session_async(name, &config, worktrees);
            return;
        }

        let default = self.agents.default_name();
        let selected_index = names.iter().position(|n| *n == default).unwrap_or(0);
        let choices = self
            .agents
            .agents
            .iter()
            .map(|a| crate::ui::agent_picker_modal::AgentChoice {
                name: a.name.clone(),
                command: a.command.clone(),
            })
            .collect();
        self.new_session.spawn_name = Some(name);
        self.new_session.spawn_config = Some(config);
        self.new_session.spawn_worktrees = worktrees;
        self.modal = modals::Modal::AgentPicker(crate::ui::agent_picker_modal::AgentPickerState {
            choices,
            selected_index,
        });
    }

    fn restart_active_session(&mut self) {
        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };
        let Some(agent_session_id) = session.info.agent_session_id.clone() else {
            return;
        };

        let agent = session.info.agent.clone();
        // Keep the same thurbox identity across a restart so injected env stays
        // stable (`THURBOX_SESSION`).
        let session_id = session.info.id;
        // Preserve a remote backend on the config — set *before* env injection
        // (which skips the local-path dir vars for remote sessions) and used to
        // adapt the relaunch args for the host.
        let backend_type = session.backend_name().to_string();
        // Rebuild the process cwd: the primary repo for a single-repo session,
        // or the (idempotently rebuilt) symlink workspace for a multi-repo one.
        let cwd = self.session_process_cwd(&session.info);

        let mut config = SessionConfig {
            resume_session_id: None,
            session_id: Some(session_id),
            agent_session_id: Some(agent_session_id.clone()),
            cwd,
            agent,
            fork_session_id: None,
            backend: crate::session::is_remote_backend(&backend_type).then_some(backend_type),
            ..SessionConfig::default()
        };
        // `Session::restart` replaces the session env wholesale, so re-inject the
        // standard `THURBOX_*` identity vars (the same set a fresh spawn gets via
        // `build_spawn_inputs`); otherwise the restarted agent loses its identity
        // and the metrics/status hooks break.
        crate::session_ops::inject_thurbox_env(&mut config, &agent_session_id, None);
        let def = self.agent_def_for(&config.agent);
        config.resume_session_id =
            crate::session_ops::resume_trigger_for(&def, &agent_session_id, &config.env);

        self.do_restart(config);
    }

    /// Execute the actual restart with the finalized config.
    fn do_restart(&mut self, config: SessionConfig) {
        let (rows, cols) = self.content_area_size();
        // Resolve the relaunch provider from the *current* registry (and adapt
        // its args for a remote backend) before restarting — the provider the
        // session stored at spawn/adopt time may predate both.
        let provider = self.launch_provider_for(&config);
        let Some(session) = self.active_session_mut() else {
            // The active session vanished (e.g. deleted by a concurrent CLI
            // command) before the restart fired — degrade to a no-op.
            self.new_session.restart = false;
            return;
        };
        session.set_provider(provider);
        let session_id = session.info.id;
        match session.restart(&config, rows, cols) {
            Ok(()) => {
                // Re-spawned fresh: clear stale hook-driven status so it doesn't
                // linger as Blocked/Working/Done until the agent re-reports (a
                // resumed agent may not re-fire its boot hook). Mirrors the
                // headless `restart_session_headless` path.
                let _ = self.db.clear_hook_state(session_id);
                // Our own write doesn't move this connection's `data_version`,
                // so force the status cache to reload and pick up the cleared row.
                self.invalidate_hook_state_cache();
                self.save_state();
                self.set_status(StatusLevel::Info, "Session restarted");
            }
            Err(e) => {
                error!("Failed to restart session: {e}");
                self.set_error(format!("Failed to restart session: {e:#}"));
            }
        }
        self.new_session.restart = false;
    }

    /// Open the active session's worktree (or cwd) in the configured editor.
    fn open_active_in_editor(&mut self) {
        if self.try_open_selected_file() {
            return;
        }
        let paths = match self.collect_active_session_paths() {
            Some(p) => p,
            None => return,
        };
        self.launch_editor_with_paths(&paths);
    }

    /// If the file viewer is focused on a file, open `[root, file]` so editors
    /// open the workspace and highlight the file. Returns true if handled.
    fn try_open_selected_file(&mut self) -> bool {
        if self.focus != InputFocus::FileViewer {
            return false;
        }
        let Some((file, root)) = self.file_viewer.selected_file_with_root() else {
            return false;
        };
        let Some(editor) = helpers::resolve_editor_command(&self.db) else {
            self.set_error(EDITOR_NOT_CONFIGURED);
            return true;
        };
        match helpers::open_in_editor(&[root, file.clone()], &editor) {
            Ok(()) => self.set_info(format!(
                "Opening {} in {editor}",
                file.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )),
            Err(e) => self.set_error(format!("Failed to launch editor `{editor}`: {e}")),
        }
        true
    }

    fn collect_active_session_paths(&mut self) -> Option<Vec<std::path::PathBuf>> {
        let Some(session) = self.sessions.get(self.active_index) else {
            self.set_error("No active session");
            return None;
        };
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        for wt in &session.info.worktrees {
            if !paths.contains(&wt.worktree_path) {
                paths.push(wt.worktree_path.clone());
            }
        }
        if paths.is_empty() {
            if let Some(cwd) = session.info.cwd.clone() {
                paths.push(cwd);
            }
        }
        for dir in &session.info.additional_dirs {
            if !paths.contains(dir) {
                paths.push(dir.clone());
            }
        }
        if paths.is_empty() {
            self.set_error("Active session has no worktree or cwd to open");
            return None;
        }
        Some(paths)
    }

    fn launch_editor_with_paths(&mut self, paths: &[std::path::PathBuf]) {
        let Some(editor) = helpers::resolve_editor_command(&self.db) else {
            self.set_error(EDITOR_NOT_CONFIGURED);
            return;
        };
        match helpers::open_in_editor(paths, &editor) {
            Ok(()) => self.set_info(format!("Opening {} path(s) in {editor}", paths.len())),
            Err(e) => self.set_error(format!("Failed to launch editor `{editor}`: {e}")),
        }
    }

    fn fork_active_session(&mut self) {
        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };

        let agent = session.info.agent.clone();
        let cwd = session.info.cwd.clone();
        let worktrees = session.info.worktrees.clone();
        let source_name = session.info.name.clone();
        let fork_session_id = session.info.agent_session_id.clone();

        let config = SessionConfig {
            resume_session_id: None,
            agent_session_id: None,
            cwd,
            agent,
            fork_session_id,
            ..SessionConfig::default()
        };

        self.new_session.spawn_config = Some(config);
        self.new_session.spawn_worktrees = worktrees;
        self.new_session.fork = true;
        self.new_session.parent_session_id = Some(session.info.id);

        let mut sn = modals::SessionNameModal::default();
        sn.name.set(&format!("{source_name}-fork"));
        self.modal = modals::Modal::SessionName(sn);
    }

    fn close_active_session(&mut self) {
        if self.sessions.is_empty() {
            return;
        }

        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };

        let session_id = session.info.id;

        // When soft-delete is disabled, a TUI delete is a destructive hard
        // delete (kills the tmux window, removes worktrees) with no Ctrl+Z
        // undo. Confirm before tearing anything down only when the session has
        // work at risk (uncommitted changes / unmerged commits, or a state we
        // can't verify); a known-clean session is deleted straight away.
        if !self.features.soft_delete {
            match self.assess_delete_risk(session) {
                Some(risk) => {
                    self.modal = modals::Modal::ConfirmDelete(modals::ConfirmDeleteModal {
                        session_id,
                        session_name: session.info.name.clone(),
                        risk,
                    });
                }
                None => self.confirm_hard_delete_session(session_id),
            }
            return;
        }

        if let Err(e) = self.db.soft_delete_session(session_id) {
            error!("Failed to soft-delete session in DB: {e}");
        }

        // Remove from the list only — do NOT kill the backend or remove
        // worktrees yet (Ctrl+Z undo / Ctrl+U restore reuse them).
        let removed_session = self.sessions.remove(self.active_index);
        let session_name = removed_session.info.name.clone();

        self.session_terminal_views.remove(&session_id);
        self.code_reviews.remove(&session_id);

        self.sync_active_session_to_project();

        // Finalize any existing pending delete before storing the new one.
        self.finalize_pending_delete();

        self.pending_delete = Some(PendingDelete {
            session: removed_session,
            session_id,
            created_at: std::time::Instant::now(),
        });

        self.set_status(
            StatusLevel::Info,
            format!("Deleted '{session_name}'. Ctrl+Z to undo"),
        );

        // Sync to shared state for other instances
        self.save_state();
    }

    /// Assess what a hard delete of `session` would destroy, so a clean session
    /// can skip the confirmation prompt. Returns `None` when the session is
    /// known-clean (delete silently) or `Some(risk)` describing the uncommitted
    /// changes / unmerged commits to confirm. Remote-host sessions can't be
    /// inspected cheaply, so they always confirm (`DeleteRisk::unknown`).
    fn assess_delete_risk(&self, session: &Session) -> Option<modals::DeleteRisk> {
        if session.info.remote_host.is_some() {
            return Some(modals::DeleteRisk::unknown());
        }

        // Inspect each worktree thurbox would tear down; for a non-worktree
        // session fall back to its cwd (the live agent's working dir).
        let paths: Vec<std::path::PathBuf> = if session.info.worktrees.is_empty() {
            session.info.cwd.iter().cloned().collect()
        } else {
            session
                .info
                .worktrees
                .iter()
                .map(|w| w.worktree_path.clone())
                .collect()
        };

        let stats: Vec<_> = paths
            .iter()
            .map(|p| crate::git::worktree_stats(p))
            .collect();
        modals::DeleteRisk::from_stats(&stats)
    }

    /// Hard-delete a session after the confirmation prompt (soft_delete off):
    /// soft-delete the row + disable pending sends on the UI thread (fast
    /// SQLite writes) so the modal closes and the row vanishes immediately,
    /// then defer the slow tmux `kill-window` + `git worktree remove` +
    /// symlink-workspace cleanup to a background task. There is no Ctrl+Z
    /// undo (the row stays restorable via Ctrl+U, which re-spawns fresh) —
    /// the confirmation modal is the safety net instead.
    fn confirm_hard_delete_session(&mut self, session_id: SessionId) {
        let Some(idx) = self.sessions.iter().position(|s| s.info.id == session_id) else {
            return;
        };

        // Snapshot the shared row before the soft-delete so the background
        // teardown still has the window name + worktrees + agent_session_id.
        let shared = match self.db.get_session_by_id(session_id) {
            Ok(opt) => opt,
            Err(e) => {
                error!("Hard-delete lookup for session {session_id} failed: {e}");
                None
            }
        };

        if let Err(e) = self.db.disable_send_automations_for_session(session_id) {
            error!("Failed to disable pending sends for session {session_id}: {e}");
        }
        if let Err(e) = self.db.soft_delete_session(session_id) {
            error!("Failed to soft-delete session {session_id}: {e}");
        }
        // Flag as force-deleted so the Ctrl+U restore list tags + blocks it —
        // its worktrees (and any uncommitted work) are gone with the teardown
        // below, so it can't be coherently restored.
        if let Err(e) = self.db.mark_session_force_deleted(session_id) {
            error!("Failed to mark session {session_id} force-deleted: {e}");
        }

        let removed_session = self.sessions.remove(idx);
        let session_name = removed_session.info.name.clone();
        self.session_terminal_views.remove(&session_id);
        self.code_reviews.remove(&session_id);

        if self.active_index >= self.sessions.len() {
            self.active_index = self.sessions.len().saturating_sub(1);
        }
        self.sync_active_session_to_project();

        // Drop the live PTY connection; the tmux window itself is killed by
        // the background teardown below.
        removed_session.kill();

        if let Some(shared) = shared {
            tokio::task::spawn_blocking(move || {
                let mut report = crate::session_ops::delete::ForceDeleteReport::default();
                crate::session_ops::delete::teardown_runtime_resources(&shared, &mut report);
            });
        }

        self.set_status(
            StatusLevel::Info,
            format!("Permanently deleted '{session_name}'"),
        );

        self.save_state();
    }

    /// Recreate git worktrees from shared worktree metadata.
    ///
    /// For each worktree whose branch still exists, runs `git worktree add`
    /// to restore it. Returns the successfully recreated worktrees.
    fn recreate_worktrees(worktrees: &[SharedWorktree]) -> Vec<WorktreeInfo> {
        let mut infos = Vec::new();
        for wt in worktrees {
            if git::branch_exists(&wt.repo_path, &wt.branch) {
                match git::add_existing_worktree(&wt.repo_path, &wt.branch) {
                    Ok(wt_path) => {
                        infos.push(WorktreeInfo {
                            repo_path: wt.repo_path.clone(),
                            worktree_path: wt_path,
                            branch: wt.branch.clone(),
                        });
                    }
                    Err(e) => {
                        error!("Failed to recreate worktree for {}: {e}", wt.branch);
                    }
                }
            }
        }
        infos
    }

    /// Finalize a pending delete — kill the backend session.
    ///
    /// Worktrees, containers, and VMs are intentionally preserved on disk
    /// so that restored sessions (Ctrl+U) can reuse them without re-cloning.
    fn finalize_pending_delete(&mut self) {
        if let Some(pending) = self.pending_delete.take() {
            // Clean up derived per-session artifacts (rebuilt on restore): the
            // agent metrics file and the multi-repo symlink workspace. Worktrees
            // are intentionally left on disk for Ctrl+U restore.
            if let Some(ref sid) = pending.session.info.agent_session_id {
                if let Some(metrics_dir) = crate::paths::metrics_directory() {
                    let _ = std::fs::remove_file(metrics_dir.join(format!("{sid}.json")));
                }
                let _ = crate::workspace::remove_workspace(sid);
                // A remote session's workspace lives on its host (see
                // `git::ensure_remote_workspace`) — tear it down there too, or
                // it leaks forever. Gated on multi-repo so a single-repo delete
                // never pays an ssh/wsl round-trip (no workspace exists).
                let info = &pending.session.info;
                let multi = session_member_dirs(
                    info.cwd.as_deref(),
                    &info.worktrees,
                    &info.additional_dirs,
                )
                .len()
                    >= 2;
                if multi {
                    if let Some(host) = info.remote_host.as_deref().and_then(|n| self.hosts.get(n))
                    {
                        if let Err(e) = crate::git::remove_remote_workspace(host, sid) {
                            warn!("failed to remove remote workspace for {sid}: {e:#}");
                        }
                    }
                }
            }
            pending.session.kill();
        }
    }

    /// Undo the most recent session delete (Ctrl+Z).
    fn undo_delete(&mut self) {
        let Some(pending) = self.pending_delete.take() else {
            return;
        };

        if let Err(e) = self.db.restore_session(pending.session_id) {
            error!("Failed to restore session in DB: {e}");
            self.set_error("Failed to undo delete");
            return;
        }

        let session_name = pending.session.info.name.clone();
        self.sessions.push(pending.session);
        self.active_index = self.sessions.len() - 1;
        self.save_state();

        self.set_status(StatusLevel::Success, format!("Restored '{session_name}'"));
    }

    /// Open the theme picker, pre-selecting the currently active theme
    /// (built-in preset or custom from themes.toml).
    fn open_theme_picker(&mut self) {
        let active = self.db.get_active_theme().ok().flatten();
        let entries = crate::ui::theme::all_theme_entries();
        let index = active
            .as_deref()
            .and_then(|name| entries.iter().position(|e| e.name == name))
            .unwrap_or(0);
        let original = crate::ui::theme::current();
        self.modal = modals::Modal::ThemePicker(modals::ThemePickerModal { index, original });
    }

    /// Open the Settings panel. The draft reflects the live source of truth:
    /// `self.features` for the feature flags (so in-session changes show), and
    /// `settings::global()` for the scalars + notifications (read once at
    /// startup, never mutated in-process).
    pub(crate) fn open_settings_panel(&mut self) {
        let draft = crate::session::settings::Settings {
            features: self.features,
            ..crate::session::settings::global().clone()
        };
        self.modal = modals::Modal::Settings(modals::SettingsModal::new(draft));
    }

    /// Persist the Settings panel draft to `settings.toml`, apply the live
    /// feature flags immediately, and toast the result. Keeps the modal open on
    /// a write error so edits aren't lost.
    pub(crate) fn submit_settings_panel(&mut self) {
        let (draft, restart) = match self.modal {
            modals::Modal::Settings(ref m) => (m.draft.clone(), m.restart_required_changed()),
            _ => return,
        };
        if let Err(e) = crate::agent::settings_config::save_settings(&draft) {
            self.set_error(format!("Failed to save settings: {e}"));
            return;
        }
        // Live-apply the feature flags that gate UI panels; restart-required
        // settings only take effect from the on-disk file on next launch.
        self.apply_live_settings(&draft);
        // Record our own write so the live-reload poll doesn't re-toast it.
        self.mark_settings_saved();
        self.modal.close();
        if restart {
            self.set_status(
                StatusLevel::Info,
                "Settings saved — some changes apply after restart",
            );
        } else {
            self.set_status(StatusLevel::Success, "Settings saved");
        }
    }

    fn open_restore_sessions_modal(&mut self) {
        match self.db.list_deleted_sessions() {
            Ok(list) => {
                self.modal =
                    modals::Modal::RestoreSessions(modals::RestoreSessionsModal { list, index: 0 });
            }
            Err(e) => {
                error!("Failed to list deleted sessions: {e}");
                self.set_error("Failed to list deleted sessions");
            }
        }
    }

    /// Restore a soft-deleted session: un-delete in DB, recreate worktrees, and spawn.
    ///
    /// Works for force-deleted sessions too, on a best-effort basis: force-delete
    /// removed the worktree directory but not the git branch, so
    /// [`Self::recreate_worktrees`] reattaches each branch that still exists
    /// (uncommitted work was lost on delete). `restore_session` also clears the
    /// `force_deleted` flag. The TUI gates this behind a confirm modal.
    fn restore_deleted_session(&mut self, deleted: DeletedSessionInfo) {
        let was_force_deleted = deleted.force_deleted;
        let wanted_worktrees = deleted.worktrees.len();

        if let Err(e) = self.db.restore_session(deleted.id) {
            error!("Failed to restore session in DB: {e}");
            self.set_error("Failed to restore session");
            return;
        }

        let worktree_infos = Self::recreate_worktrees(&deleted.worktrees);
        let recovered_worktrees = worktree_infos.len();
        let cwd = worktree_infos
            .first()
            .map(|wt| wt.worktree_path.clone())
            .or(deleted.cwd.clone());

        // Re-spawn on the session's *persisted* backend, not the local default:
        // a remote (`ssh:<host>`) session must land on its own host, or its
        // `backend_type` is corrupted (and a remote pane-id could collide with a
        // local one). Skip restore when this instance can't manage that backend.
        let Some(backend) = self.resolve_persisted_backend(&deleted.backend_type) else {
            self.set_error(format!(
                "Cannot restore '{}': backend '{}' is not available on this instance",
                deleted.name, deleted.backend_type
            ));
            return;
        };

        // Reuse the existing SessionId + inject identity/dir env so the restored
        // session's status hooks can attribute their `session signal` (otherwise
        // it renders Idle forever).
        let mut config = Self::restored_session_config(
            deleted.id,
            deleted.agent_session_id.clone(),
            deleted.agent,
            cwd,
            &deleted.backend_type,
        );
        config.resume_session_id = deleted.agent_session_id;

        let session_name = deleted.name.clone();
        let (rows, cols) = self.content_area_size();
        let provider = self.launch_provider_for(&config);

        match Session::spawn(
            session_name.clone(),
            rows,
            cols,
            &config,
            &backend,
            &provider,
        ) {
            Ok(mut session) => {
                session.info.id = deleted.id;
                session.info.worktrees = worktree_infos;
                session.info.parent_session_id = deleted.parent_session_id;
                // `DeletedSessionInfo` doesn't carry display_order: a restored
                // session simply re-appends at the end of its repo group.
                resolve_repo_display_names(&mut session.info);
                self.sessions.push(session);
                self.active_index = self.sessions.len() - 1;
                self.focus = InputFocus::Terminal;

                self.save_state();

                if was_force_deleted {
                    // Recovery is lossy: note it, and flag any worktree whose
                    // branch was gone (so couldn't be reattached).
                    let mut msg =
                        format!("Restored '{session_name}' (best-effort: uncommitted work lost");
                    if recovered_worktrees < wanted_worktrees {
                        msg.push_str(&format!(
                            ", {recovered_worktrees} of {wanted_worktrees} worktrees recovered"
                        ));
                    }
                    msg.push(')');
                    self.set_status(StatusLevel::Info, msg);
                } else {
                    self.set_status(StatusLevel::Success, format!("Restored '{session_name}'"));
                }
            }
            Err(e) => {
                error!("Failed to spawn restored session: {e}");
                self.set_error(format!("Failed to restore session: {e:#}"));
            }
        }
    }

    /// Apply shared session metadata to a local session info.
    /// Used when updating or adopting sessions from shared state.
    fn apply_shared_session_metadata(session: &mut Session, shared: &sync::SharedSession) {
        session.info.name = shared.name.clone();
        session.info.agent = shared.agent.clone();
        session.info.cwd = shared.cwd.clone();
        session.info.additional_dirs = shared.additional_dirs.clone();
        session.info.agent_session_id = shared.agent_session_id.clone();
        session.info.worktrees = shared.worktrees.iter().cloned().map(Into::into).collect();
        session.info.parent_session_id = shared.parent_session_id;
        session.info.display_order = shared.display_order;
        resolve_repo_display_names(&mut session.info);
    }

    pub fn update(&mut self, msg: AppMessage) {
        // Any input event is a potential visual change (a key, a mouse move
        // that re-highlights a row, a paste). Mark the UI dirty so the render
        // loop paints this iteration — keypress-to-screen stays immediate.
        self.request_redraw();

        // `[features] mouse = false`: capture is never enabled, so mouse
        // events shouldn't arrive — drop any that do (defense in depth, and
        // it makes the flag authoritative in tests).
        if !self.features.mouse
            && matches!(
                msg,
                AppMessage::MouseScrollUp { .. }
                    | AppMessage::MouseScrollDown { .. }
                    | AppMessage::MouseClick { .. }
                    | AppMessage::MouseDrag { .. }
                    | AppMessage::MouseUp { .. }
                    | AppMessage::MouseMove { .. }
            )
        {
            return;
        }
        match msg {
            AppMessage::KeyPress(code, mods) => self.handle_key(code, mods),
            AppMessage::Paste(text) => self.handle_paste(text),
            AppMessage::MouseScrollUp { x, y } => self.handle_mouse_scroll(x, y, true),
            AppMessage::MouseScrollDown { x, y } => self.handle_mouse_scroll(x, y, false),
            AppMessage::MouseClick { x, y, modifiers } => self.handle_mouse_click(x, y, modifiers),
            AppMessage::MouseDrag { x, y } => self.handle_mouse_drag(x, y),
            AppMessage::MouseUp { x, y } => self.handle_mouse_up(x, y),
            AppMessage::MouseMove { x, y } => self.mouse_hover = Some((x, y)),
            AppMessage::Resize(cols, rows) => self.handle_resize(cols, rows),
            AppMessage::ExternalStateChange(delta) => self.handle_external_state_change(delta),
        }
    }

    /// Get the current terminal view for the active session.
    pub(crate) fn active_terminal_view(&self) -> TerminalView {
        self.sessions
            .get(self.active_index)
            .and_then(|s| self.session_terminal_views.get(&s.info.id))
            .copied()
            .unwrap_or(TerminalView::Claude)
    }

    /// The id of the currently selected session, if any.
    pub(crate) fn active_session_id(&self) -> Option<SessionId> {
        self.sessions.get(self.active_index).map(|s| s.info.id)
    }

    /// The active session's open code-review view, if any. The review is stored
    /// per session in [`Self::code_reviews`] so it persists across switches.
    pub(crate) fn active_review(&self) -> Option<&code_review::CodeReviewState> {
        self.code_reviews.get(&self.active_session_id()?)
    }

    /// Mutable [`Self::active_review`].
    pub(crate) fn active_review_mut(&mut self) -> Option<&mut code_review::CodeReviewState> {
        let sid = self.active_session_id()?;
        self.code_reviews.get_mut(&sid)
    }

    /// Keep the central-pane focus consistent with the active session's review:
    /// promote `Terminal`→`CodeReview` when that session has a review open (the
    /// review owns the central pane, so terminal focus is meaningless there), and
    /// demote `CodeReview`→`Terminal` when it doesn't (after switching to a
    /// non-review session). Mirrors how the shell view follows the session; other
    /// focuses (session list, file viewer, …) are left untouched.
    pub(crate) fn sync_review_focus(&mut self) {
        let has_review = self.active_review().is_some();
        match self.focus {
            InputFocus::CodeReview | InputFocus::ReviewFiles if !has_review => {
                self.focus = InputFocus::Terminal
            }
            InputFocus::Terminal if has_review => self.focus = InputFocus::CodeReview,
            _ => {}
        }
    }

    pub(crate) fn with_active_parser(&self, f: impl FnOnce(&mut crate::agent::SessionParser)) {
        if let Some(session) = self.sessions.get(self.active_index) {
            let parser_arc = if self.active_terminal_view() == TerminalView::Shell {
                session.shell_pane.as_ref().map(|sp| &sp.parser)
            } else {
                None
            }
            .unwrap_or(&session.parser);
            if let Ok(mut parser) = parser_arc.lock() {
                f(&mut parser);
            }
        }
    }

    /// Toggle the terminal view between Claude and Shell for the active session.
    /// Lazily spawns the shell pane on first toggle.
    pub(crate) fn toggle_shell_view(&mut self) {
        // A review overlays the central pane; F8/Ctrl+T leaves it straight to
        // the shell (the user's "back to shell" expectation, same as the Shell
        // tab) rather than silently flipping the hidden terminal view behind the
        // review.
        if self.active_review().is_some() {
            self.select_central_tab(CentralTab::Shell);
            return;
        }
        match self.active_terminal_view() {
            TerminalView::Claude => self.show_shell_view(),
            TerminalView::Shell => self.show_agent_view(),
        }
    }

    /// Switch the active session's terminal view back to the agent CLI.
    fn show_agent_view(&mut self) {
        if let Some(sid) = self.active_session_id() {
            self.session_terminal_views
                .insert(sid, TerminalView::Claude);
        }
    }

    /// Switch the active session's terminal view to its shell, creating the
    /// shell pane on first use. Started in the same launch cwd as the agent (the
    /// multi-repo workspace when there is one), so switching lands you there.
    fn show_shell_view(&mut self) {
        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };
        let session_id = session.info.id;
        if session.shell_pane.is_none() {
            let (rows, cols) = self.content_area_size();
            // Resolve the launch cwd (host-aware for remote workspaces) before
            // taking the mutable session borrow; the immutable borrow of
            // `self.sessions` ends with this block. The *non-building* variant:
            // the agent is running in the workspace, and the ensure-style
            // rebuild would rm -rf its cwd out from under it.
            let shell_cwd = {
                let idx = self.active_index;
                match self.sessions.get(idx) {
                    Some(s) => self.session_process_cwd_existing(&s.info),
                    None => None,
                }
            };
            let Some(session) = self.active_session_mut() else {
                // Active session removed concurrently — nothing to switch to.
                return;
            };
            if let Err(e) = session.ensure_shell_pane(rows, cols, shell_cwd.as_deref()) {
                error!("Failed to create shell pane: {e}");
                self.set_error(format!("Failed to create shell: {e:#}"));
                return;
            }
            self.save_state();
        }
        self.session_terminal_views
            .insert(session_id, TerminalView::Shell);
    }

    /// Select a central-pane view from the top-border tab strip. Unlike the
    /// keyboard toggles this *selects* `tab` unambiguously: switching to
    /// Agent/Shell closes any open review first, and Review opens the review if
    /// it isn't already. Feature-gated tabs aren't rendered, so a click on a
    /// disabled view can't arrive here.
    pub(crate) fn select_central_tab(&mut self, tab: CentralTab) {
        match tab {
            CentralTab::Agent => {
                if self.active_review().is_some() {
                    self.close_code_review();
                }
                self.show_agent_view();
                self.focus_central_terminal();
            }
            CentralTab::Shell => {
                if self.active_review().is_some() {
                    self.close_code_review();
                }
                self.show_shell_view();
                self.focus_central_terminal();
            }
            CentralTab::Review => {
                if self.active_review().is_none() {
                    self.toggle_code_review();
                }
                // `toggle_code_review` can fail to open (e.g. no worktree); only
                // grab focus when a review is actually present.
                if self.active_review().is_some() && self.focus != InputFocus::CodeReview {
                    self.focus = InputFocus::CodeReview;
                    self.on_focus_changed();
                }
            }
        }
    }

    /// Focus the central terminal pane (shared by the Agent/Shell tab clicks).
    fn focus_central_terminal(&mut self) {
        if self.focus != InputFocus::Terminal {
            self.focus = InputFocus::Terminal;
            self.on_focus_changed();
        }
    }

    /// The central-pane tab currently shown: Review wins (it overlays the pane),
    /// else the per-session terminal view.
    pub(crate) fn active_central_tab(&self) -> CentralTab {
        if self.active_review().is_some() {
            CentralTab::Review
        } else if self.active_terminal_view() == TerminalView::Shell {
            CentralTab::Shell
        } else {
            CentralTab::Agent
        }
    }

    pub(crate) fn scroll_terminal_up(&mut self, lines: usize) {
        self.text_selection = None;
        self.with_active_parser(|parser| {
            let current = parser.screen().scrollback();
            parser.screen_mut().set_scrollback(current + lines);
        });
    }

    pub(crate) fn scroll_terminal_down(&mut self, lines: usize) {
        self.text_selection = None;
        self.with_active_parser(|parser| {
            let current = parser.screen().scrollback();
            parser
                .screen_mut()
                .set_scrollback(current.saturating_sub(lines));
        });
    }

    pub(crate) fn page_scroll_amount(&self) -> usize {
        let (rows, _) = self.content_area_size();
        (rows as usize) / 2
    }

    fn handle_mouse_click(&mut self, x: u16, y: u16, modifiers: KeyModifiers) {
        // A modal captures every click: a hit on one of its rows acts on it,
        // anything else is swallowed — clicks never reach the scrollbars,
        // panes, or selection beneath an overlay.
        if !matches!(self.modal, modals::Modal::None) {
            self.handle_modal_click(x, y);
            return;
        }

        let areas = self.screen_layout();
        let border_block = Block::default().borders(Borders::ALL);

        // Ctrl+Click: URL opening (terminal-relative, existing behavior)
        if modifiers.contains(KeyModifiers::CONTROL) {
            self.text_selection = None;
            self.open_ctrl_clicked_url(border_block.inner(areas.terminal), x, y);
            return;
        }

        // Grab a scrollbar thumb: a click on any rendered track starts a drag of
        // that pane's scroll state (and never starts a text selection).
        if self.try_grab_scrollbar(x, y, None) {
            return;
        }

        // While the global-search strip is open it owns all input (it is
        // entered/left only via its keybinding / Esc / Enter), so plain
        // clicks are swallowed rather than stealing focus from it.
        if self.global_search.active {
            return;
        }

        // Row / pane targets recorded by the last view() (first match wins:
        // rows are recorded before their pane's whole-rect fallback). A
        // consumed click stops here; session-list and terminal clicks fall
        // through so the same press still arms text selection.
        let pos = Position::new(x, y);
        if let Some(action) = self
            .click_targets
            .iter()
            .find(|t| t.rect.contains(pos))
            .map(|t| t.action)
        {
            if self.activate_click_target(action) {
                return;
            }
        }

        // Find which pane was clicked; use inner area (excluding borders).
        let pane_rects = [Some(areas.terminal), areas.left_panel, areas.info_panel];
        let pane_inner = pane_rects
            .into_iter()
            .flatten()
            .find(|r| r.contains(pos))
            .map(|r| border_block.inner(r));

        let Some(inner) = pane_inner else {
            self.text_selection = None;
            return;
        };

        let pane = PaneBounds::from_rect(inner);
        let anchor = TermPos {
            row: y as usize,
            col: x as usize,
        };
        self.text_selection = Some(Selection::new(anchor, pane));
    }

    /// Open the URL under a Ctrl+Click inside the terminal pane, if any.
    /// `inner` is the terminal's content area (borders excluded); a click
    /// outside it (or with no URL at that cell) is a no-op.
    fn open_ctrl_clicked_url(&mut self, inner: Rect, x: u16, y: u16) {
        use crate::ui::links;

        if !inner.contains(Position::new(x, y)) {
            return;
        }
        let screen_col = (x - inner.x) as usize;
        let screen_row = (y - inner.y) as usize;
        self.with_active_parser(|parser| {
            let rows = links::extract_screen_rows(parser.screen());
            let detected = links::detect_urls(&rows);
            if let Some(url) = links::url_at_position(&detected, screen_row, screen_col) {
                helpers::open_url(url);
            }
        });
    }

    /// Route a click while a modal is open: a hit on a recorded row selects
    /// it and immediately activates it with the row's primary key (replayed
    /// through the modal's own key handler so side effects match the keyboard
    /// path). Every other click — inside or outside the overlay — is
    /// swallowed, so a stray click can never discard typed input or fall
    /// through to the panes beneath.
    fn handle_modal_click(&mut self, x: u16, y: u16) {
        // The F1 editor consumes the next *keypress* while capturing; clicks
        // are ignored so they can't be mistaken for a chord.
        if self.help_is_capturing() {
            return;
        }
        // The modal's own scrollbar is grabbable (recorded under
        // `ScrollTarget::Modal`); the pane scrollbars beneath the overlay are
        // not.
        if self.try_grab_scrollbar(x, y, Some(ScrollTarget::Modal)) {
            return;
        }

        // Each `try_*` block filters the click registry to its own action type
        // (the registry also holds pane targets beneath the overlay; a plain
        // first-match would hit those and swallow the click). Their rects never
        // overlap, so the order is priority-for-clarity. First one to consume the
        // click wins.
        let pos = Position::new(x, y);
        if self.try_modal_button_click(pos) {
            return;
        }
        if self.try_modal_field_click(pos) {
            return;
        }
        if self.try_repo_focus_click(pos) {
            return;
        }
        self.try_modal_row_click(pos);
    }

    /// Footer buttons (`[ Save ]` / `[ Cancel ]` / …) replay their key through
    /// the modal's own handler, so a click is identical to the keypress.
    fn try_modal_button_click(&mut self, pos: Position) -> bool {
        let Some((code, mods)) = self.click_targets.iter().find_map(|t| match t.action {
            ClickAction::ModalButton { code, mods } if t.rect.contains(pos) => Some((code, mods)),
            _ => None,
        }) else {
            return false;
        };
        if matches!(self.modal, modals::Modal::Help(_)) {
            self.handle_help_key(code, mods);
        } else {
            self.handle_modal_key_if_open(code, mods);
        }
        true
    }

    /// Editor-field clicks select that field (no key replay — the user then
    /// adjusts/types with the keyboard, exactly as after Tab/↑↓). A Settings
    /// boolean row also toggles on click (its whole point is the on/off switch;
    /// scalar rows only select, so a stray click can't change a number).
    fn try_modal_field_click(&mut self, pos: Position) -> bool {
        let Some(index) = self.click_targets.iter().find_map(|t| match t.action {
            ClickAction::ModalField(i) if t.rect.contains(pos) => Some(i),
            _ => None,
        }) else {
            return false;
        };
        self.select_modal_field(index);
        if let modals::Modal::Settings(s) = &mut self.modal {
            if !s.field.is_scalar() {
                s.toggle();
            }
        }
        true
    }

    /// Repo picker: clicking the path-input / search field focuses it.
    fn try_repo_focus_click(&mut self, pos: Position) -> bool {
        let Some(focus) = self.click_targets.iter().find_map(|t| match t.action {
            ClickAction::RepoFocus(focus) if t.rect.contains(pos) => Some(focus),
            _ => None,
        }) else {
            return false;
        };
        if let modals::Modal::RepoPicker(ref mut rp) = self.modal {
            rp.focus = focus;
        }
        true
    }

    /// A list-row click selects the row and replays its activation key.
    fn try_modal_row_click(&mut self, pos: Position) {
        let Some(row) = self.click_targets.iter().find_map(|t| match t.action {
            ClickAction::ModalRow(row) if t.rect.contains(pos) => Some(row),
            _ => None,
        }) else {
            return;
        };
        let Some(confirm) = self.select_modal_row(row) else {
            return;
        };
        if matches!(self.modal, modals::Modal::Help(_)) {
            self.handle_help_key(confirm, KeyModifiers::NONE);
        } else {
            self.handle_modal_key_if_open(confirm, KeyModifiers::NONE);
        }
    }

    /// Move the open modal's selection to `row` (a row index recorded by this
    /// frame's renderer, so it is always in bounds) and return the key that
    /// activates a row there (see [`modals::Modal::list_selection`]).
    fn select_modal_row(&mut self, row: usize) -> Option<KeyCode> {
        // The repo picker routes keys by its internal focus; a row click always
        // means the list (mirrors the keyboard path), so force it before moving.
        if let modals::Modal::RepoPicker(ref mut rp) = self.modal {
            rp.focus = modals::RepoPickerFocus::List;
        }
        let (index, activation_key) = self.modal.list_selection()?;
        *index = row;
        Some(activation_key)
    }

    /// Select the index-th field of the active editor modal (its position in
    /// that modal's visible field order), so a click focuses a field exactly
    /// like Tab/↑↓ would. No-op for modals without a field list.
    fn select_modal_field(&mut self, index: usize) {
        match &mut self.modal {
            modals::Modal::Settings(s) => {
                if let Some(&field) = modals::SettingsField::ORDER.get(index) {
                    s.field = field;
                }
            }
            modals::Modal::AutomationEditor(a) => {
                if let Some(&field) = a.visible_fields().get(index) {
                    a.field = field;
                }
            }
            _ => {}
        }
    }

    /// Act on a clicked target. Returns `true` when the click is fully
    /// consumed; `false` lets the caller continue to text-selection arming
    /// (terminal / session-list / info panes keep their drag-select).
    fn activate_click_target(&mut self, action: ClickAction) -> bool {
        match action {
            ClickAction::SelectSession(display_idx) => {
                if let Some(&idx) = self.render_order_indices().get(display_idx) {
                    self.active_index = idx;
                }
                self.focus = InputFocus::SessionList;
                self.on_focus_changed();
                false
            }
            ClickAction::SelectTask(i) => {
                self.focus = InputFocus::TaskList;
                let len = self.task_ui.filtered_task_indices.len();
                if len > 0 {
                    self.task_ui.task_panel_index = i.min(len - 1);
                }
                // Same bookkeeping as entering the panel via the focus cycle
                // (refresh list + in-pane preview). Leaving an in-pane editor
                // this way discards unsaved edits, exactly like Esc/Ctrl+H.
                self.on_focus_changed();
                true
            }
            ClickAction::SelectAutomation(i) => {
                self.focus = InputFocus::Automations;
                let len = self.automation_ui.cached_automations.len();
                if len > 0 {
                    self.automation_ui.automation_panel_index = i.min(len - 1);
                }
                self.refresh_automation_view();
                true
            }
            ClickAction::SelectFileRow(i) => {
                self.focus = InputFocus::FileViewer;
                self.file_viewer.select_index(i);
                // Single click activates, like Enter: toggle a directory,
                // open a file in the editor.
                self.file_viewer_expand();
                true
            }
            ClickAction::FocusPane(focus) => {
                let changed = self.focus != focus;
                self.focus = focus;
                if changed {
                    self.on_focus_changed();
                }
                // Terminal and session-list clicks keep arming drag-select.
                !matches!(focus, InputFocus::Terminal | InputFocus::SessionList)
            }
            // Modal rows/buttons/fields are dispatched by `handle_modal_click`
            // before pane targets are even considered.
            ClickAction::ModalRow(_)
            | ClickAction::ModalButton { .. }
            | ClickAction::ModalField(_)
            | ClickAction::RepoFocus(_) => true,
            ClickAction::Global(action) => {
                self.dispatch_action(action);
                true
            }
            ClickAction::PaneField { focus, index } => {
                // Enter the editor if not already in it (a fresh sync resets the
                // field), then select the clicked field. When already focused we
                // only move the field — never re-sync — so unsaved edits survive.
                if self.focus != focus {
                    match focus {
                        InputFocus::AutomationEditor => self.enter_automation_editor(),
                        InputFocus::TaskEditor => self.enter_task_editor(),
                        _ => self.focus = focus,
                    }
                }
                self.select_pane_field(focus, index);
                true
            }
            ClickAction::ReviewRow(i) => {
                // A click in the diff body focuses the review pane (the
                // whole-pane `FocusPane` fallback is recorded after the row
                // targets, so it never wins on a row hit).
                self.focus = InputFocus::CodeReview;
                self.cr_select_row(i);
                true
            }
            ClickAction::ReviewButton(button) => {
                self.focus = InputFocus::CodeReview;
                self.cr_button(button);
                true
            }
            ClickAction::ReviewFile(fi) => {
                self.focus = InputFocus::ReviewFiles;
                self.cr_jump_to_file(fi);
                true
            }
            ClickAction::ReviewTarget(i) => {
                self.focus = InputFocus::CodeReview;
                self.cr_select_target(i);
                true
            }
            ClickAction::CentralTab(tab) => {
                self.select_central_tab(tab);
                true
            }
            ClickAction::CopyStatus => {
                self.copy_status_to_clipboard();
                true
            }
        }
    }

    /// Set the active field of the focused in-pane editor (automation / task) by
    /// its position in the editor's visible field order.
    fn select_pane_field(&mut self, focus: InputFocus, index: usize) {
        match focus {
            InputFocus::AutomationEditor => {
                if let Some(m) = self.automation_ui.automation_editor.as_mut() {
                    if let Some(&field) = m.visible_fields().get(index) {
                        m.field = field;
                    }
                }
            }
            InputFocus::TaskEditor => {
                if let Some(m) = self.task_ui.task_editor.as_mut() {
                    if let Some(&field) = m.visible_fields().get(index) {
                        m.field = field;
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether the F1 editor is mid chord-capture — any key (including a
    /// synthesized one) would become the new binding, so mouse handlers must
    /// stay silent.
    fn help_is_capturing(&self) -> bool {
        matches!(self.modal, modals::Modal::Help(ref h) if h.capturing)
    }

    /// Grab the scrollbar track under the cursor and start dragging it.
    /// `only` restricts which target may be grabbed: modals grab only their
    /// own bar, panes grab any. Returns whether a track was hit.
    fn try_grab_scrollbar(&mut self, x: u16, y: u16, only: Option<ScrollTarget>) -> bool {
        let Some(hit) = self
            .scrollbar_hits
            .iter()
            .find(|h| only.map_or(true, |t| h.target == t) && h.geom.contains(x, y))
        else {
            return false;
        };
        let target = hit.target;
        let pos = hit.geom.position_for_y(y);
        let content_len = hit.geom.content_len;
        self.text_selection = None;
        self.dragging_scrollbar = Some(target);
        self.apply_scrollbar_position(target, pos, content_len);
        true
    }

    fn handle_mouse_drag(&mut self, x: u16, y: u16) {
        // A scrollbar drag takes precedence: keep driving the grabbed pane's
        // scroll state (y can leave the track — `position_for_y` clamps it).
        if let Some(target) = self.dragging_scrollbar {
            if let Some(hit) = self.scrollbar_hits.iter().find(|h| h.target == target) {
                let pos = hit.geom.position_for_y(y);
                let content_len = hit.geom.content_len;
                self.apply_scrollbar_position(target, pos, content_len);
            }
            return;
        }

        if let Some(ref mut sel) = self.text_selection {
            let (cx, cy) = sel.pane.clamp(x, y);
            sel.cursor = TermPos {
                row: cy as usize,
                col: cx as usize,
            };
        }
    }

    fn handle_mouse_up(&mut self, x: u16, y: u16) {
        // End an in-progress scrollbar drag without touching the text selection.
        if self.dragging_scrollbar.take().is_some() {
            return;
        }

        self.handle_mouse_drag(x, y);

        if let Some(ref mut sel) = self.text_selection {
            sel.dragging = false;

            // If anchor == cursor, it was just a click (no drag) — clear selection
            if sel.anchor == sel.cursor {
                self.text_selection = None;
            }
        }
    }

    /// Apply a scrollbar position (in `0..content_len`) to the scroll state it
    /// drives. `content_len` is passed in (read from the hit) so the terminal
    /// arm can invert without re-borrowing `scrollbar_hits` across
    /// `with_active_parser`.
    fn apply_scrollbar_position(&mut self, target: ScrollTarget, pos: usize, content_len: usize) {
        match target {
            ScrollTarget::Terminal => {
                // The scrollbar position is inverted vs. scrollback (0 = bottom):
                // render uses `position = total - scrollback`, so invert back.
                let scrollback = content_len.saturating_sub(pos);
                self.text_selection = None;
                self.with_active_parser(|parser| {
                    parser.screen_mut().set_scrollback(scrollback);
                });
            }
            ScrollTarget::TaskPreview => {
                let max = self.task_preview_max_scroll();
                self.task_ui.task_preview_scroll = (pos as u16).min(max);
            }
            ScrollTarget::FileViewer => {
                self.file_viewer.select_index(pos);
            }
            ScrollTarget::RunHistory => {
                let max = self
                    .automation_ui
                    .cached_automation_runs
                    .len()
                    .saturating_sub(1);
                self.automation_ui.automation_run_index = pos.min(max);
            }
            ScrollTarget::CodeReview => {
                // The review is selection-primary: `render_rows` derives `scroll`
                // from `selected` every frame, so setting `scroll` directly here
                // would snap back. Move the selection instead (matching the wheel
                // + keyboard paths); the scroll offset follows on render.
                self.cr_select_row(pos);
            }
            ScrollTarget::Modal => self.step_modal_selection_to(pos),
        }
    }

    /// Move the open modal's selection to `target` by replaying Up/Down
    /// through its own key handler — keeps each modal's clamping and side
    /// effects (e.g. the theme picker's live preview) identical to keyboard
    /// navigation. Stops as soon as a step no longer makes progress.
    fn step_modal_selection_to(&mut self, target: usize) {
        // While the F1 editor is capturing, any key would be taken as the new
        // chord — never synthesize navigation there.
        if self.help_is_capturing() {
            return;
        }
        // The repo picker routes keys by its internal focus; a scrollbar drag
        // always means the list.
        if let modals::Modal::RepoPicker(ref mut rp) = self.modal {
            rp.focus = modals::RepoPickerFocus::List;
        }
        loop {
            let Some(current) = self.modal_selected_index() else {
                return;
            };
            if current == target {
                return;
            }
            let key = if target > current {
                KeyCode::Down
            } else {
                KeyCode::Up
            };
            self.synthesize_modal_nav(key);
            if self.modal_selected_index() == Some(current) {
                return; // clamped — can't get closer
            }
        }
    }

    /// Replay a navigation key through the open modal's key handler.
    fn synthesize_modal_nav(&mut self, code: KeyCode) {
        if matches!(self.modal, modals::Modal::Help(_)) {
            self.handle_help_key(code, KeyModifiers::NONE);
        } else {
            self.handle_modal_key_if_open(code, KeyModifiers::NONE);
        }
    }

    /// The open modal's current selection index, when it has a selectable list.
    /// Shares [`modals::Modal::list_selection`] with [`Self::select_modal_row`]
    /// (hence `&mut self`) so the two can't drift onto different modal sets.
    fn modal_selected_index(&mut self) -> Option<usize> {
        self.modal.list_selection().map(|(index, _)| *index)
    }

    /// Route a mouse-wheel tick to whichever pane is under the cursor, so the
    /// wheel scrolls the hovered pane (terminal, task preview, file viewer, run
    /// history, or a list pane) rather than always the terminal.
    fn handle_mouse_scroll(&mut self, x: u16, y: u16, up: bool) {
        // An open modal owns the wheel: one selection step per tick (like
        // j/k), never the panes beneath. Capture mode would treat the
        // synthesized key as the new chord, so it stays untouched.
        if !matches!(self.modal, modals::Modal::None) {
            if !self.help_is_capturing() {
                self.synthesize_modal_nav(if up { KeyCode::Up } else { KeyCode::Down });
            }
            return;
        }

        self.scroll_pane(self.pane_at(x, y), up, x, y);
    }

    /// Apply a wheel tick (`up`) to a specific scrollable pane (the terminal
    /// when `pane` is `None`/`Terminal`). `(x, y)` is the cursor position in
    /// screen cells, used to forward mouse coordinates to the inner PTY when
    /// the agent has mouse tracking enabled (Claude Code, vim, htop, …).
    fn scroll_pane(&mut self, pane: Option<ScrollPane>, up: bool, x: u16, y: u16) {
        let step: i32 = if up { -1 } else { 1 };
        match pane {
            Some(ScrollPane::Terminal) | None => {
                // Modern TUIs on the alternate screen (Claude Code, vim, htop,
                // …) enable mouse tracking and handle wheel scrolling
                // themselves; vt100's scrollback is empty on the alt screen so
                // the local fallback would be a silent no-op. Forward instead.
                if self.try_forward_wheel_to_pty(x, y, up) {
                    return;
                }
                if up {
                    self.scroll_terminal_up(MOUSE_SCROLL_LINES);
                } else {
                    self.scroll_terminal_down(MOUSE_SCROLL_LINES);
                }
            }
            Some(ScrollPane::TaskPreview) => {
                self.scroll_task_preview(step * MOUSE_SCROLL_LINES as i32)
            }
            Some(ScrollPane::FileViewer) => self
                .file_viewer
                .move_selection(step * MOUSE_SCROLL_LINES as i32),
            Some(ScrollPane::RunHistory) => self.move_run_history_selection(step),
            Some(ScrollPane::SessionList) => {
                if up {
                    self.switch_session_backward();
                } else {
                    self.switch_session_forward();
                }
            }
            Some(ScrollPane::TasksList) => self.move_task_selection(step),
            Some(ScrollPane::Automations) => self.move_automation_selection(step),
            Some(ScrollPane::CodeReview) => self.cr_move(step as isize),
            Some(ScrollPane::ReviewFiles) => self.cr_jump_file(!up),
        }
    }

    /// Step the tasks-panel selection by `delta`, clamped, refreshing the preview.
    fn move_task_selection(&mut self, delta: i32) {
        let len = self.task_ui.filtered_task_indices.len();
        if len == 0 {
            return;
        }
        let next = (self.task_ui.task_panel_index as i32 + delta).clamp(0, len as i32 - 1);
        let next = next as usize;
        if next != self.task_ui.task_panel_index {
            self.task_ui.task_panel_index = next;
            self.refresh_task_view();
        }
    }

    /// Hit-test `(x, y)` against the current layout to find the scrollable pane
    /// under the cursor (used for pane-aware wheel scrolling).
    fn pane_at(&self, x: u16, y: u16) -> Option<ScrollPane> {
        let areas = self.screen_layout();
        let pos = Position::new(x, y);
        let hit = |r: Option<Rect>| r.map(|r| r.contains(pos)).unwrap_or(false);

        if hit(areas.file_viewer) {
            // During a review this column hosts the changed-files list.
            if self.active_review().is_some() {
                return Some(ScrollPane::ReviewFiles);
            }
            return Some(ScrollPane::FileViewer);
        }
        if hit(areas.tasks_panel) {
            return Some(ScrollPane::TasksList);
        }
        if hit(areas.automations_panel) {
            return Some(ScrollPane::Automations);
        }
        if hit(areas.left_panel) {
            return Some(ScrollPane::SessionList);
        }
        if areas.terminal.contains(pos) {
            // The central pane hosts the terminal, the task preview, or the
            // automation run-history depending on focus.
            return Some(match self.focus {
                InputFocus::TaskList | InputFocus::TaskEditor => ScrollPane::TaskPreview,
                InputFocus::AutomationRunHistory => ScrollPane::RunHistory,
                InputFocus::CodeReview => ScrollPane::CodeReview,
                _ => ScrollPane::Terminal,
            });
        }
        None
    }

    /// Forward a wheel tick to the active session's PTY when the inner agent
    /// has enabled xterm mouse tracking — the convention modern TUIs (Claude
    /// Code, vim, htop, btop, …) use to subscribe to wheel events. Returns
    /// `true` when the event was forwarded so the caller skips the local
    /// scrollback fallback (which is a no-op on the alternate screen anyway).
    ///
    /// Only the SGR encoding (DECSET 1006) is supported: the legacy 1005/utf8
    /// and default encodings cap row/col at 223 and aren't used by anything
    /// that ships in 2024+. Falling back to vt100 scrollback for them is fine.
    fn try_forward_wheel_to_pty(&self, x: u16, y: u16, up: bool) -> bool {
        let Some(session) = self.sessions.get(self.active_index) else {
            return false;
        };

        let view = self.active_terminal_view();
        let parser_arc = if view == TerminalView::Shell {
            session.shell_pane.as_ref().map(|sp| &sp.parser)
        } else {
            None
        }
        .unwrap_or(&session.parser);

        let (mode, encoding) = {
            let Ok(parser) = parser_arc.lock() else {
                return false;
            };
            let screen = parser.screen();
            (
                screen.mouse_protocol_mode(),
                screen.mouse_protocol_encoding(),
            )
        };

        if mode == vt100::MouseProtocolMode::None || encoding != vt100::MouseProtocolEncoding::Sgr {
            return false;
        }

        // Map the screen-cell click to 1-based PTY cell coordinates. A wheel
        // tick outside the terminal pane (the cursor is hovering another panel)
        // is left to the local fallback.
        let inner = Block::default()
            .borders(Borders::ALL)
            .inner(self.screen_layout().terminal);
        if !inner.contains(Position::new(x, y)) {
            return false;
        }
        let col = u32::from(x - inner.x) + 1;
        let row = u32::from(y - inner.y) + 1;

        // Xterm wheel buttons: 64 = wheel up, 65 = wheel down. SGR encoding:
        // CSI < Cb ; Cx ; Cy M (press; release would be `m`).
        let button: u32 = if up { 64 } else { 65 };
        let bytes = format!("\x1b[<{button};{col};{row}M").into_bytes();

        let result = if view == TerminalView::Shell {
            // The branch above only set `view = Shell` when the pane exists;
            // unwrap is fine, but keep it defensive.
            session
                .shell_pane
                .as_ref()
                .map(|sp| sp.send_input(bytes))
                .unwrap_or(Ok(()))
        } else {
            session.send_input(bytes)
        };
        if let Err(e) = result {
            tracing::warn!("Failed to forward wheel event to PTY: {e}");
            return false;
        }
        true
    }

    fn copy_selection_to_clipboard(&mut self) {
        let text = match &self.selected_text_cache {
            Some(t) if !t.is_empty() => t.clone(),
            _ => return,
        };

        let Some(clipboard) = &mut self.clipboard else {
            self.set_error("Clipboard not available");
            return;
        };

        if let Err(e) = clipboard.set_text(&text) {
            self.set_error(format!("Clipboard write failed: {e}"));
            return;
        }

        self.text_selection = None;
        self.selected_text_cache = None;
        self.set_status(StatusLevel::Info, "Copied to clipboard");
    }

    /// Copy the current status-bar message (info / error / …) to the clipboard.
    /// Reachable via `Copy` (Ctrl+C, outside a focused terminal) or by clicking
    /// the status row — so a stray error/path can be pulled out of the TUI to
    /// paste elsewhere. A no-op (no toast) when nothing is shown.
    fn copy_status_to_clipboard(&mut self) {
        let Some(text) = self
            .status_message
            .as_ref()
            .map(|m| m.text.clone())
            .filter(|t| !t.is_empty())
        else {
            return; // nothing shown → no-op (no "copied" toast to overwrite it)
        };

        let Some(clipboard) = &mut self.clipboard else {
            self.set_error("Clipboard not available");
            return;
        };

        if let Err(e) = clipboard.set_text(&text) {
            self.set_error(format!("Clipboard write failed: {e}"));
            return;
        }

        self.set_status(StatusLevel::Info, "Status message copied to clipboard");
    }

    /// Wrap text in bracketed paste escape sequences and send it to the
    /// active session (or shell pane, if focused).
    fn send_paste_to_session(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(session) = self.sessions.get(self.active_index) {
            let mut paste = b"\x1b[200~".to_vec();
            paste.extend_from_slice(text.as_bytes());
            paste.extend_from_slice(b"\x1b[201~");
            let result = if let (TerminalView::Shell, Some(shell)) =
                (self.active_terminal_view(), &session.shell_pane)
            {
                shell.send_input(paste)
            } else {
                session.send_input(paste)
            };
            if let Err(e) = result {
                error!("Failed to send pasted input: {e}");
            }
        }
    }

    /// Handle a native paste event from crossterm's bracketed paste capture.
    fn handle_paste(&mut self, text: String) {
        self.text_selection = None;
        self.selected_text_cache = None;
        if self.try_paste_into_modal_input(&text) {
            return;
        }
        self.send_paste_to_session(&text);
    }

    pub(crate) fn paste_from_clipboard(&mut self) {
        self.text_selection = None;
        self.selected_text_cache = None;

        let Some(clipboard) = &mut self.clipboard else {
            self.set_error("Clipboard not available");
            return;
        };

        let text = match clipboard.get_text() {
            Ok(t) => t,
            Err(e) => {
                self.set_error(format!("Clipboard read failed: {e}"));
                return;
            }
        };

        if self.try_paste_into_modal_input(&text) {
            return;
        }
        self.send_paste_to_session(&text);
    }

    /// Route pasted text into the focused text input when one is open — a modal
    /// field or an in-pane editor. Returns `true` when consumed, signalling the
    /// caller to skip the default "send to session" behaviour.
    ///
    /// While *any* modal is open the paste is consumed regardless of whether a
    /// text field has focus, so it can never leak through to the terminal in the
    /// main pane behind the overlay. New modals with text inputs should add
    /// their target here so paste lands in them.
    fn try_paste_into_modal_input(&mut self, text: &str) -> bool {
        use modals::Modal;

        match &mut self.modal {
            Modal::WorktreeName(wn) => wn.name.insert_str(text),
            Modal::SessionName(sn) => sn.name.insert_str(text),
            Modal::RepoPicker(rp) => {
                match rp.focus {
                    modals::RepoPickerFocus::Input => rp.path_input.insert_str(text),
                    modals::RepoPickerFocus::Search => {
                        rp.search_input.insert_str(text);
                        rp.recompute_filter();
                    }
                    // The list has no text field; swallow so paste doesn't
                    // reach the terminal behind the overlay.
                    modals::RepoPickerFocus::List => {}
                }
                // Refresh the autocomplete suggestion (no-op unless the path
                // input is focused). Done after the `rp` borrow ends.
                self.update_repo_picker_path_suggestion();
            }
            Modal::AutomationEditor(m) => {
                if let Some(field) = m.active_field_mut() {
                    field.insert_str(text);
                }
            }
            // No modal: route to a focused in-pane editor if any.
            Modal::None => return self.try_paste_into_pane_editor(text),
            // Selector-only modals (agent/host/theme/branch pickers, lists, …)
            // have no text field, but still swallow the paste so it can't fall
            // through to the terminal beneath them.
            _ => {}
        }
        true
    }

    /// Route pasted text into a focused in-pane editor (the task or automation
    /// editor, which are panes rather than modals). Returns `true` when the
    /// editor pane is focused — inserting into its text field if one is focused,
    /// otherwise swallowing the paste so it can't leak into the terminal. Called
    /// only when no modal is open.
    fn try_paste_into_pane_editor(&mut self, text: &str) -> bool {
        // Resolve the focused editor's text field (or `None` for a
        // selector/multi-line field handled inline), then insert once below so
        // both editor arms share the tail.
        let field = match self.focus {
            InputFocus::TaskEditor => {
                let Some(editor) = self.task_ui.task_editor.as_mut() else {
                    return true;
                };
                // The description is a multi-line `TextArea`, handled here.
                if editor.field == modals::TaskField::Description {
                    editor.description.insert_str(text);
                    return true;
                }
                editor.active_field_mut()
            }
            InputFocus::AutomationEditor => match self.automation_ui.automation_editor.as_mut() {
                Some(editor) => editor.active_field_mut(),
                None => return true,
            },
            _ => return false,
        };
        if let Some(field) = field {
            field.insert_str(text);
        }
        true
    }

    pub(crate) fn spawn_worktree_session(
        &mut self,
        repo_paths: &[PathBuf],
        new_branch: &str,
        base_branch: &str,
        session_name: Option<String>,
    ) {
        if self.worktree_create.in_progress() {
            self.set_status(StatusLevel::Info, "Worktree creation already in progress…");
            return;
        }

        // Resolve the remote host (if any) so worktrees are created on the
        // session's target machine over SSH. Consume the wizard's choice.
        let backend = self.new_session.backend.take();
        let host = self.host_for_backend(backend.as_deref()).cloned();
        let normal_repos = std::mem::take(&mut self.new_session.normal_repos);

        let repo_paths = repo_paths.to_vec();
        let new_branch = new_branch.to_string();
        let base_branch = base_branch.to_string();

        // Shell out to `git worktree add` off the UI thread (one per repo, with
        // rollback on failure); the spawn flow resumes in `poll_worktree_create`.
        let tx = self.worktree_create.start();
        self.pending_worktree_create = Some(PendingWorktreeCreate {
            backend,
            normal_repos,
            session_name,
            base_branch: base_branch.clone(),
        });
        self.set_status(StatusLevel::Info, "Creating worktree(s)…");
        tokio::task::spawn_blocking(move || {
            let result = create_worktrees(host.as_ref(), &repo_paths, &new_branch, &base_branch);
            let _ = tx.send(result);
        });
    }

    /// Apply a completed background worktree-creation, if one has finished, and
    /// resume the spawn flow.
    fn poll_worktree_create(&mut self) {
        let result = match self.worktree_create.poll() {
            background::TaskPoll::Pending => return,
            background::TaskPoll::Died => {
                self.pending_worktree_create = None;
                self.set_error("Worktree creation failed (worker died)");
                return;
            }
            background::TaskPoll::Done(result) => result,
        };
        let Some(pending) = self.pending_worktree_create.take() else {
            return;
        };

        match result {
            Ok(worktree_infos) => self.continue_worktree_spawn(worktree_infos, pending),
            Err(e) => self.set_error(format!("Failed to create worktree: {e}")),
        }
    }

    /// Build the session config from freshly-created worktrees and continue into
    /// the name/agent modal (or spawn directly when the name is known).
    fn continue_worktree_spawn(
        &mut self,
        worktree_infos: Vec<WorktreeInfo>,
        pending: PendingWorktreeCreate,
    ) {
        let Some(primary) = worktree_infos.first() else {
            self.set_error("Worktree creation produced no worktrees");
            return;
        };
        let primary_path = primary.worktree_path.clone();

        // Combine remaining worktree paths + normal repos as additional dirs.
        let mut additional_dirs: Vec<PathBuf> = worktree_infos[1..]
            .iter()
            .map(|w| w.worktree_path.clone())
            .collect();
        additional_dirs.extend(pending.normal_repos);
        self.new_session.additional_dirs = additional_dirs;
        // Carry the fork point to the spawn so it can be persisted for the
        // code-review view (scopes the diff to `<base>..HEAD`).
        self.new_session.spawn_base_branch = Some(pending.base_branch);

        let config = SessionConfig {
            cwd: Some(primary_path),
            backend: pending.backend,
            ..SessionConfig::default()
        };

        if let Some(name) = pending.session_name {
            // Session name already known (worktree flow) — skip name modal.
            self.finish_prepare_spawn(name, config, worktree_infos);
        } else {
            self.prepare_spawn(config, worktree_infos);
        }
    }

    /// Install the configured remote-host registry (from `hosts.toml`). Called
    /// once at startup after the SSH backends are registered.
    pub fn set_hosts(&mut self, hosts: crate::session::HostRegistry) {
        self.hosts = hosts;
    }

    /// Resolve the [`HostDef`] for a backend name, or `None` for the local
    /// backend. Used to run git operations (worktree create/remove, branch
    /// listing) on the correct host.
    ///
    /// [`HostDef`]: crate::session::HostDef
    pub(crate) fn host_for_backend(
        &self,
        backend: Option<&str>,
    ) -> Option<&crate::session::HostDef> {
        // `get_by_backend` returns `None` for local (`non-ssh:`/`non-wsl:`)
        // names.
        self.hosts.get_by_backend(backend?)
    }

    /// The launch cwd for an *existing* session, derived from its persisted
    /// `SessionInfo`: the multi-repo symlink workspace ((re)built on the
    /// session's host, remote or local), or the primary repo when single-repo.
    /// Used by the restart path — the agent is about to relaunch, so the
    /// destructive workspace rebuild is safe there.
    fn session_process_cwd(&self, info: &SessionInfo) -> Option<PathBuf> {
        // `remote_host` is the bare host name (`None` = local).
        let host = info.remote_host.as_deref().and_then(|n| self.hosts.get(n));
        resolve_process_cwd(
            info.agent_session_id.as_deref(),
            info.cwd.clone(),
            &info.worktrees,
            &info.additional_dirs,
            host,
        )
    }

    /// Like [`session_process_cwd`](Self::session_process_cwd) but **never
    /// (re)builds** the workspace — it derives the same deterministic path
    /// without touching the filesystem. For callers that resolve the cwd of a
    /// session whose agent is *still running* there (the shell pane): the
    /// ensure-style rebuild is `rm -rf` + recreate, which would delete the
    /// running agent's cwd inode out from under it.
    fn session_process_cwd_existing(&self, info: &SessionInfo) -> Option<PathBuf> {
        let members =
            session_member_dirs(info.cwd.as_deref(), &info.worktrees, &info.additional_dirs);
        if members.len() < 2 {
            return info.cwd.clone();
        }
        let Some(id) = info.agent_session_id.as_deref() else {
            return info.cwd.clone();
        };
        let host = info.remote_host.as_deref().and_then(|n| self.hosts.get(n));
        let path = match host {
            // Only network cost is the (cached) remote `$HOME` lookup.
            Some(h) => crate::git::remote_workspace_dir(h, id)
                .map(PathBuf::from)
                .map_err(|e| error!("Failed to resolve remote workspace path: {e:#}")),
            None => crate::workspace::workspace_path(id)
                .map_err(|e| error!("Failed to resolve workspace path: {e}")),
        };
        path.ok().or_else(|| info.cwd.clone())
    }

    /// Resolve the backend for a *persisted* session by its `backend_type`, or
    /// `None` when this instance cannot manage it.
    ///
    /// An unknown off-local backend (`ssh:<host>` / `wsl:<distro>` — e.g. a host
    /// this instance hasn't loaded from `hosts.toml`, a distro not present here,
    /// or one another instance configured) is **skipped** rather than falling
    /// back to local — adopting an off-local session on the local backend would
    /// corrupt its `backend_type` and risk a pane-id collision (tmux numbers
    /// panes `%N` per server, so a remote `%1` can match an unrelated local
    /// `%1`). Legacy/local values (empty, `tmux`, `local-tmux`) still fall back
    /// to the default local backend.
    pub(crate) fn resolve_persisted_backend(
        &self,
        backend_type: &str,
    ) -> Option<Arc<dyn SessionBackend>> {
        if let Some(b) = self.backends.get(backend_type) {
            return Some(b.clone());
        }
        if crate::session::is_remote_backend(backend_type) {
            return None;
        }
        Some(self.backends.default_backend().clone())
    }

    /// Resolve the backend a session should spawn on, ensuring it is ready.
    ///
    /// Looks up `config.backend` in the registry (falling back to the default
    /// local backend), then calls `ensure_ready()` so a remote backend's SSH
    /// control-mode connection is established lazily on first use. Returns a
    /// status-line-friendly error if the backend is unknown or unreachable.
    pub(crate) fn backend_for(
        &self,
        config: &SessionConfig,
    ) -> Result<Arc<dyn SessionBackend>, String> {
        let backend = match config.backend.as_deref() {
            Some(name) if !name.is_empty() => self
                .backends
                .get(name)
                .cloned()
                .ok_or_else(|| format!("Unknown backend '{name}'"))?,
            _ => self.backends.default_backend().clone(),
        };
        backend
            .ensure_ready()
            .map_err(|e| format!("Backend '{}' not ready: {e:#}", backend.name()))?;
        Ok(backend)
    }

    /// Common spawn preparation shared by the sync and async paths: fill in
    /// defaults (agent, `agent_session_id`), inject statusline env vars, resolve
    /// the process cwd (a symlink workspace for multi-repo sessions), and select
    /// the backend + provider. Returns `None` after setting a status error when
    /// the backend is unknown or unreachable.
    fn build_spawn_inputs(
        &mut self,
        config: &SessionConfig,
        worktrees: &[WorktreeInfo],
        additional_dirs: &[PathBuf],
    ) -> Option<SpawnInputs> {
        let (rows, cols) = self.content_area_size();

        let mut config = config.clone();
        if config.agent.is_empty() {
            config.agent = self.agents.default_name();
        }
        let agent_session_id = config
            .agent_session_id
            .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
            .clone();
        // Mint the thurbox SessionId up front (unless a respawn supplied one) so
        // it can be injected as `THURBOX_SESSION` before launch and `Session::spawn`
        // reuses it. Stable across restarts.
        if config.session_id.is_none() {
            config.session_id = Some(SessionId::default());
        }

        // Inject identity + statusline env vars. `THURBOX_TASK` is left unset: TUI
        // task spawns track the task↔session link in-memory (`task_session_links`),
        // so only the headless `task run` path auto-tags messages with it.
        crate::session_ops::inject_thurbox_env(&mut config, &agent_session_id, None);

        // For a multi-repo session, launch the agent in a symlink workspace that
        // gathers every member dir; `info.cwd` keeps the primary repo (restored
        // after spawn). Single-repo sessions are unchanged.
        let primary_cwd = config.cwd.clone();
        let spawn_host = self.host_for_backend(config.backend.as_deref()).cloned();
        config.cwd = resolve_process_cwd(
            config.agent_session_id.as_deref(),
            primary_cwd.clone(),
            worktrees,
            additional_dirs,
            spawn_host.as_ref(),
        );

        let backend = match self.backend_for(&config) {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to select backend: {e}");
                self.set_error(e);
                return None;
            }
        };

        let provider = self.launch_provider_for(&config);

        Some(SpawnInputs {
            config,
            primary_cwd,
            backend,
            provider,
            rows,
            cols,
        })
    }

    /// Adopt a freshly spawned [`Session`] into the app: attach its metadata,
    /// select + focus it, persist, and run any task-initiated follow-up. Shared
    /// by the synchronous and backgrounded spawn paths.
    #[allow(clippy::too_many_arguments)]
    fn finalize_spawned_session(
        &mut self,
        mut session: Session,
        primary_cwd: Option<PathBuf>,
        worktrees: Vec<WorktreeInfo>,
        additional_dirs: Vec<PathBuf>,
        parent_session_id: Option<SessionId>,
        task_prompt: Option<(i64, String)>,
        base_branch: Option<String>,
    ) {
        session.info.cwd = primary_cwd;
        session.info.worktrees = worktrees;
        session.info.additional_dirs = additional_dirs;
        session.info.parent_session_id = parent_session_id;

        resolve_repo_display_names(&mut session.info);
        let session_id = session.info.id;
        self.sessions.push(session);
        self.active_index = self.sessions.len() - 1;
        self.focus = InputFocus::Terminal;
        self.status_message = None;

        self.save_state();

        // Persist the worktree's fork point (write-once, like the hook columns)
        // so the code-review view can scope its diff to `<base>..HEAD`. Runs
        // after `save_state` so the row exists; `upsert_session` never lists it.
        if let Some(base) = base_branch {
            if let Err(e) = self.db.set_session_base_branch(session_id, &base) {
                tracing::warn!("Failed to record session base branch: {e}");
            }
        }
        // No spawn-time status seed: a fresh session is `Idle` until the agent's
        // hooks report otherwise (claude's SessionStart → idle on boot, then
        // working/blocked/done). Seeding `working` made an idle session look
        // stuck working.

        // A task-initiated spawn (the trigger-time picker's "Spawn new session")
        // delivers the task title once the agent has booted, then advances the
        // task to in progress.
        if let Some((task_id, title)) = task_prompt {
            let new_id = self.sessions[self.active_index].info.id;
            // Record the link now — the session was named by the user, so the
            // `<title> · #<id>` convention can't recover it later.
            self.task_ui.task_session_links.insert(task_id, new_id);
            let prompt = self.task_agent_prompt(task_id, &title);
            self.send_prompt_to_session(new_id, &prompt, AGENT_BOOT_DELAY_TICKS);
            let status = self
                .task_ui
                .cached_tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| t.status)
                .unwrap_or_default();
            self.advance_task_to_in_progress(task_id, status);
            self.refresh_tasks();
        }
    }

    /// Spawn a session **synchronously** (blocks the caller on PTY/tmux
    /// creation). Used by programmatic callers that need the session present
    /// immediately — automations/tasks read the new id right back, and restore
    /// runs inside `tick()`. The interactive new-session flow uses
    /// [`Self::do_spawn_session_async`] instead so `Ctrl+N` doesn't freeze.
    pub(crate) fn do_spawn_session(
        &mut self,
        name: String,
        config: &SessionConfig,
        worktrees: Vec<WorktreeInfo>,
    ) {
        let additional_dirs = std::mem::take(&mut self.new_session.additional_dirs);
        let parent_session_id = self.new_session.parent_session_id.take();
        let base_branch = self.new_session.spawn_base_branch.take();
        let Some(inputs) = self.build_spawn_inputs(config, &worktrees, &additional_dirs) else {
            return;
        };

        match Session::spawn(
            name,
            inputs.rows,
            inputs.cols,
            &inputs.config,
            &inputs.backend,
            &inputs.provider,
        ) {
            Ok(session) => {
                let task_prompt = self.task_ui.pending_task_prompt.take();
                self.finalize_spawned_session(
                    session,
                    inputs.primary_cwd,
                    worktrees,
                    additional_dirs,
                    parent_session_id,
                    task_prompt,
                    base_branch,
                );
            }
            Err(e) => {
                error!("Failed to spawn session: {e}");
                self.set_error(format!("Failed to start {}: {e:#}", inputs.config.agent));
            }
        }
    }

    /// Spawn a session for the **interactive** new-session flow without blocking
    /// the UI: `Session::spawn` (PTY/tmux window creation, 500ms+) runs on a
    /// blocking task and the session is adopted in [`Self::poll_session_spawn`].
    /// Falls back to the synchronous path if a spawn is already in flight (so a
    /// double-trigger is never silently dropped).
    pub(crate) fn do_spawn_session_async(
        &mut self,
        name: String,
        config: &SessionConfig,
        worktrees: Vec<WorktreeInfo>,
    ) {
        if self.session_spawn.in_progress() {
            self.do_spawn_session(name, config, worktrees);
            return;
        }

        let additional_dirs = std::mem::take(&mut self.new_session.additional_dirs);
        let parent_session_id = self.new_session.parent_session_id.take();
        let base_branch = self.new_session.spawn_base_branch.take();
        let Some(inputs) = self.build_spawn_inputs(config, &worktrees, &additional_dirs) else {
            return;
        };
        let task_prompt = self.task_ui.pending_task_prompt.take();

        let SpawnInputs {
            config,
            primary_cwd,
            backend,
            provider,
            rows,
            cols,
        } = inputs;

        let agent = config.agent.clone();
        let tx = self.session_spawn.start();
        self.pending_session_spawn = Some(PendingSessionSpawn {
            primary_cwd,
            worktrees,
            additional_dirs,
            parent_session_id,
            task_prompt,
            agent,
            base_branch,
        });
        self.set_status(StatusLevel::Info, format!("Spawning {name}…"));

        tokio::task::spawn_blocking(move || {
            let result = Session::spawn(name, rows, cols, &config, &backend, &provider)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
    }

    /// Adopt a completed background `Session::spawn`, if one has finished.
    fn poll_session_spawn(&mut self) {
        let result = match self.session_spawn.poll() {
            background::TaskPoll::Pending => return,
            background::TaskPoll::Died => {
                self.pending_session_spawn = None;
                self.set_error("Session spawn failed (worker died)");
                return;
            }
            background::TaskPoll::Done(result) => result,
        };
        let Some(pending) = self.pending_session_spawn.take() else {
            return;
        };

        match result {
            Ok(session) => self.finalize_spawned_session(
                session,
                pending.primary_cwd,
                pending.worktrees,
                pending.additional_dirs,
                pending.parent_session_id,
                pending.task_prompt,
                pending.base_branch,
            ),
            Err(e) => {
                error!("Failed to spawn session: {e}");
                self.set_error(format!("Failed to start {}: {e}", pending.agent));
            }
        }
    }

    /// Clamp the active session index to the valid range after a session is removed.
    pub(crate) fn sync_active_session_to_project(&mut self) {
        if self.sessions.is_empty() {
            self.active_index = 0;
        } else if self.active_index >= self.sessions.len() {
            self.active_index = self.sessions.len() - 1;
        }
    }

    /// The rendered order of `self.sessions`, from the same
    /// `ui::project_list::compute_session_order` the rendering widget uses, so
    /// navigation and reordering operate on the exact order the user sees.
    fn session_order(&self) -> crate::ui::project_list::SessionOrder {
        let infos: Vec<&crate::session::SessionInfo> =
            self.sessions.iter().map(|s| &s.info).collect();
        crate::ui::project_list::compute_session_order(&infos)
    }

    /// Indices into `self.sessions` in the order they are rendered — the order
    /// `Ctrl+J`/`Ctrl+K` step through (repo groups in manual order).
    fn render_order_indices(&self) -> Vec<usize> {
        self.session_order().order
    }

    /// Move the active session one step up or down in the rendered order
    /// (`Shift+J`/`Shift+K` in the session list): root blocks swap within their
    /// repo group, whole groups swap past the group edge, nested children move
    /// among their siblings (see `ui::project_list::move_in_order`).
    ///
    /// On success every session is renumbered densely along the new order and
    /// persisted, so the order survives restarts and reaches other instances
    /// via the DB poll. The selection follows the moved row automatically
    /// (`active_index` is an input index, which the move never changes).
    pub(crate) fn move_active_session(&mut self, down: bool) {
        if self.sessions.is_empty() {
            return;
        }
        let ord = self.session_order();
        let Some(new_order) = crate::ui::project_list::move_in_order(&ord, self.active_index, down)
        else {
            return;
        };
        for (pos, &idx) in new_order.iter().enumerate() {
            self.sessions[idx].info.display_order = Some(pos as i64);
        }
        self.save_state();
    }

    /// Sort sessions alphabetically by name within each repo group
    /// (`Shift+S` in the session list). Group order is unchanged; parent/child
    /// nesting is preserved (children sort among their siblings). Renumbers
    /// every session's `display_order` densely along the new order so the
    /// arrangement survives restarts and reaches other instances via the DB
    /// poll. No-op on an empty list.
    pub(crate) fn sort_sessions_alphabetically(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let infos: Vec<&crate::session::SessionInfo> =
            self.sessions.iter().map(|s| &s.info).collect();
        let new_order = crate::ui::project_list::sort_alphabetically_within_groups(&infos);
        for (pos, &idx) in new_order.iter().enumerate() {
            self.sessions[idx].info.display_order = Some(pos as i64);
        }
        self.save_state();
    }

    /// Whether the active session is the first row in render order (top of the
    /// left column). Treats an empty list as "first" so `k` is a no-op there.
    pub(crate) fn active_is_first_in_order(&self) -> bool {
        match self.render_order_indices().first() {
            Some(&first) => first == self.active_index,
            None => true,
        }
    }

    /// Whether the active session is the last row in render order (the bottom of
    /// the session list, directly above the automations pane). Treats an empty
    /// list as "last" so `j` falls straight through into the automations pane.
    pub(crate) fn active_is_last_in_order(&self) -> bool {
        match self.render_order_indices().last() {
            Some(&last) => last == self.active_index,
            None => true,
        }
    }

    /// Select the last session in render order — used when navigating up out of
    /// the automations pane back into the session list.
    pub(crate) fn select_last_session(&mut self) {
        if let Some(&last) = self.render_order_indices().last() {
            self.active_index = last;
        }
    }

    /// Select the first session in render order — used when looping down out of
    /// the automations pane back to the top of the session list.
    pub(crate) fn select_first_session(&mut self) {
        if let Some(&first) = self.render_order_indices().first() {
            self.active_index = first;
        }
    }

    /// Switch to the next session in the **rendered** order (wraps around).
    pub(crate) fn switch_session_forward(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let order = self.render_order_indices();
        let pos = order
            .iter()
            .position(|&i| i == self.active_index)
            .unwrap_or(0);
        let next = (pos + 1) % order.len();
        self.active_index = order[next];
    }

    /// Switch to the previous session in the **rendered** order (wraps around).
    pub(crate) fn switch_session_backward(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let order = self.render_order_indices();
        let pos = order
            .iter()
            .position(|&i| i == self.active_index)
            .unwrap_or(0);
        let prev = if pos == 0 { order.len() - 1 } else { pos - 1 };
        self.active_index = order[prev];
    }

    fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.terminal_cols = cols;
        self.terminal_rows = rows;

        // Collapse the optional right-side panels if the terminal gets too
        // narrow (they only render at width >= 120 anyway).
        if cols < 120 {
            self.show_info_panel = false;
            self.show_tasks_panel = false;
            if self.focus == InputFocus::TaskList {
                self.focus = InputFocus::SessionList;
            }
        }

        self.resize_sessions_to_content_area();
    }

    /// Push the current content-area `(rows, cols)` to every session — call after any layout change.
    pub(crate) fn resize_sessions_to_content_area(&mut self) {
        let (rows, cols) = self.content_area_size();
        for session in &self.sessions {
            session.resize(rows, cols);
        }
    }

    pub fn tick(&mut self) {
        self.metrics.tick_count = self.metrics.tick_count.wrapping_add(1);

        self.tick_global_search_content();

        self.refresh_session_statuses();

        // Poll for sync results from background worktree sync threads
        self.poll_sync_results();

        // Poll for backgrounded interactive spawn work (worktree creation +
        // `Session::spawn`) so `Ctrl+N` never freezes the UI.
        self.poll_worktree_create();
        self.poll_session_spawn();

        // Adopt remote-backed sessions whose host discovery (started at
        // restore) has since completed.
        self.poll_remote_restore();

        // Send deferred inputs whose delay has elapsed
        self.drain_deferred_inputs();

        self.tick_expire_timers();

        self.poll_external_changes();

        // Fire due automations. The first tick forces an immediate catch-up
        // pass so automations missed while the TUI was down run right away;
        // afterwards it runs on the regular ~1 s cadence.
        self.process_automations(self.metrics.tick_count == 1);

        // Refresh cached automations + tasks for the UI (same cadence). The
        // first tick primes the caches so the panels aren't empty on open.
        if self.metrics.tick_count == 1 || self.metrics.tick_count % 100 == 0 {
            self.refresh_automations();
            self.refresh_tasks();
        }

        self.tick_background_refreshes();

        self.tick_version_check();

        self.poll_auto_update();
    }

    /// Snapshot of the deterministic render/tick performance counters. Used by
    /// the perf regression tests. See [`metrics_state::PerfCounters`].
    #[cfg(test)]
    pub(crate) fn perf_counters(&self) -> metrics_state::PerfCounters {
        self.metrics.perf
    }

    /// Mark the UI dirty so the render loop paints on its next iteration.
    /// Cheap and idempotent; over-marking only costs an extra (correct) frame.
    /// Internal-only — the loop in `main` drives painting via [`Self::should_redraw`].
    pub(crate) fn request_redraw(&mut self) {
        self.needs_redraw = true;
    }

    /// Whether the render loop should paint a frame this iteration: either state
    /// changed since the last paint, or the `FORCE_REDRAW_INTERVAL` floor
    /// elapsed (so time-driven UI — clock, metrics, cursor blink, quiet-session
    /// status transitions — still refreshes without an explicit dirty flag).
    pub fn should_redraw(&self) -> bool {
        self.needs_redraw || self.last_draw_at.elapsed() >= FORCE_REDRAW_INTERVAL
    }

    /// Record that a frame was just painted: clear the dirty flag, reset the
    /// forced-redraw timer, and count the requested redraw.
    pub fn mark_redrawn(&mut self) {
        self.needs_redraw = false;
        self.last_draw_at = std::time::Instant::now();
        self.metrics.bump(|p| &mut p.redraws_requested);
    }

    /// Record that a loop iteration skipped the paint because nothing changed.
    pub fn note_redraw_skipped(&mut self) {
        self.metrics.bump(|p| &mut p.redraws_skipped);
    }

    /// Detect new agent output since the last check and mark the UI dirty if so.
    /// Reads each session's monotonic `last_output_at` atomic (no parser lock),
    /// summing them into a rolling signature — a change means at least one
    /// session produced output, so the terminal needs repainting.
    pub fn detect_output_redraw(&mut self) {
        let output_gen = self
            .sessions
            .iter()
            .fold(0u64, |acc, s| acc.wrapping_add(s.last_output_at()));
        if output_gen != self.last_output_gen {
            self.last_output_gen = output_gen;
            self.needs_redraw = true;
        }
    }

    /// Content signature of the inputs that determine the session-list ordering.
    /// [`crate::ui::project_list::compute_session_order`] is a pure function of
    /// exactly these per-session fields (grouping by `repo_display_names`,
    /// sorting by `display_order`, nesting by `id`/`parent_session_id`) plus the
    /// session count/order — never status — so an unchanged signature means the
    /// cached order is still valid. Cheaper than recomputing the order
    /// (no grouping HashMap, sorts, nest recursion, or label allocations).
    fn session_order_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.sessions.len().hash(&mut h);
        for s in &self.sessions {
            s.info.id.hash(&mut h);
            s.info.display_order.hash(&mut h);
            s.info.parent_session_id.hash(&mut h);
            s.info.repo_display_names.hash(&mut h);
        }
        h.finish()
    }

    /// Drive the opt-in GitHub update check. Off the render path: on the first
    /// tick (when the flag is on and the on-disk cache is stale) it fires a
    /// single background network refresh; the result only ever lands by
    /// re-reading the cache, so rendering never makes a network call.
    fn tick_version_check(&mut self) {
        if !self.features.version_check {
            return;
        }

        // One attempt per launch: fire on the first tick if the cache is stale.
        if self.metrics.tick_count == 1
            && !self.version_check_task.in_progress()
            && crate::agent::version_check::cache_is_stale()
        {
            let tx = self.version_check_task.start();
            tokio::task::spawn_blocking(move || {
                let _ = tx.send(crate::agent::version_check::refresh_cache().map(|_| ()));
            });
        }

        // Apply a completed refresh by re-reading the cache (single source of
        // truth). A failed/dead refresh leaves the prior badge untouched.
        if let background::TaskPoll::Done(Ok(())) = self.version_check_task.poll() {
            self.update_status = crate::agent::version_check::read_cached_status();
        }
    }

    /// Expire the undo window for a pending delete and auto-clear stale status
    /// messages so default project/session counts reappear.
    fn tick_expire_timers(&mut self) {
        // Finalize pending delete after undo timeout
        if let Some(ref pending) = self.pending_delete {
            if pending.created_at.elapsed() >= UNDO_TIMEOUT {
                self.finalize_pending_delete();
            }
        }

        // Auto-expire status messages so default project/session counts reappear
        if let Some(ref msg) = self.status_message {
            if msg.created_at.elapsed() >= STATUS_MESSAGE_TIMEOUT {
                self.status_message = None;
            }
        }
    }

    /// Apply completed background metric/git-stat/usage refreshes and kick off
    /// the next ones on their cadences. All run off the UI thread (sysinfo +
    /// statusline file reads / `git` shell-outs) so a slow read never stalls
    /// rendering — mirrors the worktree-sync poll.
    fn tick_background_refreshes(&mut self) {
        self.poll_metrics_refresh();
        self.poll_git_stats();

        if self.metrics.tick_count % METRICS_REFRESH_TICKS == 0 {
            self.start_metrics_refresh();
        }
        if self.metrics.tick_count % GIT_REFRESH_TICKS == 0 {
            self.start_git_stats_refresh();
        }
        if self.metrics.tick_count % CONFIG_RELOAD_TICKS == 0 {
            self.poll_config_reload();
        }

        // Drain any completed background usage fetches into the cache.
        while let Ok((agent, usage)) = self.usage_rx.try_recv() {
            self.usage.insert(agent, usage);
        }
        // Kick off usage fetches early and then on a slow cadence.
        if self.metrics.tick_count % USAGE_REFRESH_TICKS == 1 {
            self.spawn_usage_fetches();
        }
    }

    /// Recompute each session's status/activity/notification for this tick.
    ///
    /// Status is **hooks-driven**: agents report `working`/`blocked`/`done` via
    /// `thurbox-cli session signal` (local sessions) or a tmux pane user option
    /// pushed over the control-mode subscription (remote sessions — drained
    /// below into the same hook columns), persisted in `sessions` and read here
    /// in one batch (see [`derive_session_status`]). A `done` session stays
    /// `Done` until the user moves focus *off* it (acknowledged → `Idle`). The
    /// OSC terminal title is still captured for the live activity line, but no
    /// longer drives status.
    fn refresh_session_statuses(&mut self) {
        // Before the data_version gate, so a persisted remote event reloads the
        // cache in this same tick.
        self.drain_remote_hook_events();
        self.metrics.bump(|p| &mut p.status_refreshes);
        let active_index = self.active_index;
        // Reload the persisted hook columns only when the DB actually changed —
        // an *external* `session signal` bumps `data_version`, but our own
        // `seen_at` writes (below) do not — otherwise reuse the cached map. This
        // replaces a full sessions-table scan on every (~10 ms) tick with a
        // cheap in-memory `PRAGMA data_version` read on idle ticks. See
        // `docs/PERFORMANCE.md`.
        let version = self.db.data_version().ok();
        if self.hook_states_version.is_none() || version != self.hook_states_version {
            self.metrics.bump(|p| &mut p.hook_state_loads);
            self.cached_hook_states = self.db.load_hook_states().unwrap_or_default();
            self.hook_states_version = version;
        }
        // "Seen" writes are deferred past the &mut self.sessions borrow below.
        let mut seen_writes: Vec<(crate::session::SessionId, i64)> = Vec::new();

        // A `done` session is acknowledged ("seen" → Idle) when the user moves
        // *off* it — not the instant it finishes under them — so the blue `Done`
        // state is actually visible after a turn for the session you're watching.
        // Detect the focus change and mark the session you just left, if it was
        // an unseen `done`.
        let active_id = self.sessions.get(active_index).map(|s| s.info.id);
        if active_id != self.last_active_session_id {
            if let Some((prev, state_at)) =
                self.unseen_done_on_focus_leave(&self.cached_hook_states)
            {
                seen_writes.push((prev, state_at));
            }
            self.last_active_session_id = active_id;
        }

        // Track whether any visible field changed so a quiet transition (no new
        // output, so the output detector won't catch it) still repaints promptly
        // instead of waiting for the forced-redraw floor.
        let changed = Self::apply_session_status_fields(
            &mut self.sessions,
            &self.cached_hook_states,
            &seen_writes,
        );

        // Persist the seen marks now that the sessions borrow is released, and
        // mirror them into the cache write-through: our own write doesn't move
        // `data_version`, so without this the next tick would reload nothing and
        // re-derive the just-acknowledged `done` session back to `Done`.
        // Guarded above by `seen_at < state_at`, so the focused session doesn't
        // bump `data_version` every tick.
        for (id, state_at) in seen_writes {
            let _ = self.db.mark_session_seen(id, state_at);
            if let Some(hook) = self.cached_hook_states.get_mut(&id) {
                hook.seen_at = Some(state_at);
            }
        }

        // Advance the spinner unconditionally (it must tick every call, not just
        // when no field changed — `||` would short-circuit past it), then redraw
        // on either trigger.
        let spinner_redraw = self.advance_spinner_frame();
        if changed || spinner_redraw {
            self.request_redraw();
        }
        self.dispatch_status_notifications();
    }

    /// Force [`Self::cached_hook_states`] to reload on the next status refresh.
    /// Needed after this process writes hook columns on its *own* DB connection
    /// (e.g. clearing state on restart): such writes don't move our connection's
    /// `data_version`, so the version gate wouldn't otherwise notice them.
    fn invalidate_hook_state_cache(&mut self) {
        self.hook_states_version = None;
    }

    /// Drain remote-hook status events from every backend and persist them,
    /// exactly as `thurbox-cli session signal` would have done locally.
    ///
    /// A remote agent's hooks set a tmux pane user option; the backend's
    /// control-mode subscription queues `(pane_id, state)` pairs (see
    /// [`crate::agent::backend::SessionBackend::take_hook_state_events`]).
    /// Each is resolved to a session by **backend name + pane id** — pane ids
    /// collide across hosts — and written through [`set_hook_state`]
    /// (`crate::storage`), so the whole derivation downstream (Done→seen
    /// acknowledgment, OS notifications, rollups, the stuck-working fallback)
    /// is shared with local sessions.
    fn drain_remote_hook_events(&mut self) {
        // The value is remote-host-controlled free text: allow-list it (the
        // same states `session signal` accepts) and never interpolate it.
        const VALID_STATES: [&str; 4] = ["working", "blocked", "done", "idle"];
        // Unmatched events are retried this long — comfortably past a slow
        // host's background restore — then dropped (bounded below, so another
        // instance's panes can't accumulate).
        const PENDING_TTL: std::time::Duration = std::time::Duration::from_secs(120);
        const PENDING_CAP: usize = 256;
        // Collect first: the registry borrow must end before &mut self below.
        let batches: Vec<(String, Vec<(String, String)>)> = self
            .backends
            .all_backends()
            .map(|b| (b.name().to_string(), b.take_hook_state_events()))
            .filter(|(_, events)| !events.is_empty())
            .collect();
        // Older pending retries first, so per-pane event order is preserved.
        let now = std::time::Instant::now();
        let mut queue = std::mem::take(&mut self.pending_remote_hook_events);
        for (backend_name, events) in batches {
            for (pane_id, state) in events {
                queue.push((backend_name.clone(), pane_id, state, now));
            }
        }
        // States applied *this drain*: the dedupe below must compare against
        // the latest write, not `cached_hook_states` (only invalidated, not
        // reloaded, mid-loop) — else the second event of a `working`→`done`
        // batch that matches the stale cached value is swallowed.
        let mut applied: HashMap<crate::session::SessionId, String> = HashMap::new();
        for (backend_name, pane_id, state, arrived) in queue {
            if !VALID_STATES.contains(&state.as_str()) {
                continue;
            }
            let Some(id) = self
                .sessions
                .iter()
                .find(|s| {
                    s.backend_name() == backend_name
                        && s.info.backend_id.as_deref() == Some(pane_id.as_str())
                })
                .map(|s| s.info.id)
            else {
                // No matching session *yet*: the subscription's initial report
                // often lands before the background restore adopts the pane,
                // so park the event for a later tick instead of losing it.
                if now.duration_since(arrived) < PENDING_TTL
                    && self.pending_remote_hook_events.len() < PENDING_CAP
                {
                    self.pending_remote_hook_events
                        .push((backend_name, pane_id, state, arrived));
                }
                continue;
            };
            // Dedupe against the current value: the subscription re-reports it
            // on (re)connect, and re-stamping an already-acknowledged `done`
            // would resurrect it as unseen and re-fire its OS notification on
            // every TUI restart.
            let current = applied.get(&id).map(String::as_str).or_else(|| {
                self.cached_hook_states
                    .get(&id)
                    .and_then(|h| h.state.as_deref())
            });
            if current == Some(state.as_str()) {
                continue;
            }
            if self.db.set_hook_state(id, &state).is_ok() {
                // Own-connection write: data_version won't move, force the
                // reload so this tick's derivation sees the exact row.
                self.invalidate_hook_state_cache();
                applied.insert(id, state);
            }
        }
    }

    /// If the just-left session (`last_active_session_id`) is an unseen `done`,
    /// return its `(id, state_at)` so the caller can queue a "seen" write.
    fn unseen_done_on_focus_leave(
        &self,
        hooks: &HashMap<crate::session::SessionId, crate::storage::HookRow>,
    ) -> Option<(crate::session::SessionId, i64)> {
        let prev = self.last_active_session_id?;
        let hook = hooks.get(&prev)?;
        if hook.state.as_deref() != Some("done") {
            return None;
        }
        let state_at = hook.state_at.unwrap_or(0);
        if hook.seen_at.unwrap_or(0) < state_at {
            Some((prev, state_at))
        } else {
            None
        }
    }

    /// Recompute each session's status/activity/notification from the hook rows
    /// and apply them in place. Returns whether any visible field changed.
    fn apply_session_status_fields(
        sessions: &mut [Session],
        hooks: &HashMap<crate::session::SessionId, crate::storage::HookRow>,
        seen_writes: &[(crate::session::SessionId, i64)],
    ) -> bool {
        let mut changed = false;
        for session in sessions.iter_mut() {
            let id = session.info.id;
            // `just_seen`: the focus-leave check above queued this session's seen
            // mark this tick (the DB write lands after this loop), so reflect it
            // now rather than waiting a tick.
            let just_seen = seen_writes.iter().any(|(sid, _)| *sid == id);
            let new_status = derive_session_status(
                hooks.get(&id),
                session.has_exited(),
                just_seen,
                session.millis_since_last_output(),
            );

            // Live activity text from the agent-emitted OSC terminal title.
            let new_activity = session.agent_title();
            // Retain the agent's latest pushed notification (OSC 9/777) so the
            // info panel can show it as a persistent "last signal".
            let new_notification = session.notification();

            if session.info.status != new_status
                || session.info.agent_activity != new_activity
                || session.info.notification != new_notification
            {
                changed = true;
            }
            session.info.status = new_status;
            session.info.agent_activity = new_activity;
            session.info.notification = new_notification;
        }
        changed
    }

    /// Advance the Working spinner from the (deterministic) tick counter, and
    /// report whether a repaint is needed because the frame ticked over *while*
    /// something is working — so an idle TUI still rests at ~4 fps but a working
    /// session animates smoothly.
    fn advance_spinner_frame(&mut self) -> bool {
        let new_frame = (self.metrics.tick_count / SPINNER_TICKS_PER_FRAME) as usize
            % crate::ui::SPINNER_FRAMES.len();
        let spinner_advanced = new_frame != self.spinner_frame;
        self.spinner_frame = new_frame;
        let any_working = self
            .sessions
            .iter()
            .any(|s| s.info.status == SessionStatus::Working);
        any_working && spinner_advanced
    }

    /// Current `Working`-spinner frame index (into [`crate::ui::SPINNER_FRAMES`]).
    pub(crate) fn spinner_frame(&self) -> usize {
        self.spinner_frame
    }

    /// Fire OS notifications for any session that just crossed into a
    /// needs-attention state this tick. No-op when the feature is disabled.
    fn dispatch_status_notifications(&mut self) {
        let Some(state) = self.notification_state.as_mut() else {
            return;
        };
        let active_index = self.active_index;
        let now = std::time::Instant::now();
        for (idx, session) in self.sessions.iter().enumerate() {
            let id = session.info.id;
            let status = session.info.status;
            let is_active = idx == active_index;
            if state.observe(id, status, is_active, now) != notify_state::TransitionDecision::Fire {
                continue;
            }
            let n = NotificationState::build_notification(
                id,
                &session.info.name,
                &session.info.agent,
                session.info.notification.as_deref(),
                state.sound_enabled(),
            );
            state.send(n);
        }
        // Cheap: bounds the bookkeeping after deletions / restarts.
        let live: Vec<SessionId> = self.sessions.iter().map(|s| s.info.id).collect();
        state.prune_to(&live);
    }

    /// Poll for external state changes from other thurbox instances (DB-based)
    /// and apply any theme change / session delta they produced.
    fn poll_external_changes(&mut self) {
        let Ok(Some(result)) = sync::poll_for_changes(&mut self.sync_state, &mut self.db) else {
            // Throttled (or errored): no `data_version` check ran this tick, so
            // it isn't counted. Even with no broader DB change, a notification
            // click may have landed: it writes a single row and the sync layer
            // doesn't distinguish, so we always check.
            self.apply_pending_focus_request();
            return;
        };
        // A `Some` result means the cheap `PRAGMA data_version` check actually
        // ran; `db_changed` further means it found a change and did the full
        // shared-state reload.
        self.metrics.bump(|p| &mut p.external_poll_checks);
        if result.db_changed {
            self.metrics.bump(|p| &mut p.external_poll_reloads);
            self.apply_external_theme_change();
            self.apply_pending_focus_request();
        }
        if !result.delta.is_empty() {
            self.handle_external_state_change(result.delta);
        }
    }

    /// Drain the OS-notification click handler's "focus this session" request
    /// (written from another thread/process) and switch the active session.
    /// Silently no-ops when the session has since been deleted or the
    /// stored value isn't a valid UUID.
    fn apply_pending_focus_request(&mut self) {
        let Ok(Some(raw)) = self.db.take_pending_focus_session_id() else {
            return;
        };
        let Ok(id) = raw.parse::<SessionId>() else {
            debug!("ignoring malformed pending_focus_session_id: {raw}");
            return;
        };
        let Some(idx) = self.sessions.iter().position(|s| s.info.id == id) else {
            debug!("focus request for unknown session {id}; ignoring");
            return;
        };
        self.active_index = idx;
        self.focus = InputFocus::Terminal;
        info!("focused session {id} from notification click");
    }

    /// Pick up theme changes made by other thurbox processes (e.g. an MCP
    /// `set_theme` call from another session).
    fn apply_external_theme_change(&mut self) {
        let Ok(Some(name)) = self.db.get_active_theme() else {
            return;
        };
        if name == self.active_theme.name {
            return;
        }
        let Some(entry) = crate::ui::theme::find_theme_entry(&name) else {
            return;
        };
        crate::ui::theme::set_active(entry.palette.clone());
        self.active_theme = entry;
    }

    /// Spawn background usage/rate-limit fetches for each distinct supported
    /// agent currently in the session list. Results return via `usage_tx` and
    /// are drained in [`Self::tick`]. Network/process work runs off the UI
    /// thread; unsupported agents are skipped entirely.
    fn spawn_usage_fetches(&self) {
        let mut seen = std::collections::HashSet::new();
        for session in &self.sessions {
            let agent = session.info.agent.clone();
            if !crate::usage::is_supported(&agent) || !seen.insert(agent.clone()) {
                continue;
            }
            let tx = self.usage_tx.clone();
            tokio::spawn(async move {
                let usage = crate::usage::fetch(&agent).await;
                let _ = tx.send((agent, usage));
            });
        }
    }

    /// Kick off a background git-stats refresh for the active session.
    ///
    /// Gathers the worktree paths on the UI thread (cheap) and shells out to
    /// `git` on a blocking task; the result is applied in [`Self::poll_git_stats`].
    /// Only one refresh runs at a time.
    fn start_git_stats_refresh(&mut self) {
        if self.git_stats.in_progress() {
            return;
        }
        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };
        let session_id = session.info.id;

        // Aggregate across all worktrees; fall back to the cwd if there are none.
        let paths: Vec<PathBuf> = if session.info.worktrees.is_empty() {
            session.info.cwd.iter().cloned().collect()
        } else {
            session
                .info
                .worktrees
                .iter()
                .map(|wt| wt.worktree_path.clone())
                .collect()
        };

        let tx = self.git_stats.start();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send((session_id, aggregate_git_stats(&paths)));
        });
    }

    /// Apply a completed background git-stats refresh, if one has finished.
    fn poll_git_stats(&mut self) {
        // A dead worker (e.g. panic) just clears the guard so the next
        // cadence can retry.
        if let background::TaskPoll::Done((session_id, stats)) = self.git_stats.poll() {
            if let Some(session) = self.sessions.iter_mut().find(|s| s.info.id == session_id) {
                session.info.git_stats = stats;
            }
        }
    }

    /// Kick off a background system-metrics refresh.
    ///
    /// The sysinfo collector is moved into the worker (and returned via the
    /// result) so CPU deltas persist across refreshes; statusline file reads and
    /// the active session's PID lookup (a control-mode round-trip) all run off
    /// the UI thread. Only one refresh runs at a time; the result is applied in
    /// [`Self::poll_metrics_refresh`].
    fn start_metrics_refresh(&mut self) {
        if self.metrics_refresh.in_progress() {
            return;
        }
        let Some(sys) = self.metrics.sys.take() else {
            return;
        };

        let active = self
            .sessions
            .get(self.active_index)
            .map(|s| s.backend_handle());

        let metrics_files: Vec<(SessionId, PathBuf)> = match crate::paths::metrics_directory() {
            Some(dir) => self
                .sessions
                .iter()
                .filter_map(|s| {
                    s.info
                        .agent_session_id
                        .as_ref()
                        .map(|sid| (s.info.id, dir.join(format!("{sid}.json"))))
                })
                .collect(),
            None => Vec::new(),
        };

        let tx = self.metrics_refresh.start();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(collect_system_metrics(sys, active, metrics_files));
        });
    }

    /// Apply a completed background metrics refresh, if one has finished.
    fn poll_metrics_refresh(&mut self) {
        match self.metrics_refresh.poll() {
            background::TaskPoll::Pending => {}
            background::TaskPoll::Done(refresh) => {
                self.metrics.sys = Some(refresh.sys);
                self.metrics.system_metrics = refresh.metrics;
                for (session_id, metrics) in refresh.agent_metrics {
                    if let Some(session) =
                        self.sessions.iter_mut().find(|s| s.info.id == session_id)
                    {
                        session.info.agent_metrics = Some(metrics);
                    }
                }
            }
            // Worker died without returning `sys` (e.g. panic): recreate the
            // collector so metrics resume next cadence. CPU-delta history is
            // lost, not correctness.
            background::TaskPoll::Died => {
                self.metrics.sys.get_or_insert_with(sysinfo::System::new);
            }
        }
    }

    /// Send deferred inputs whose scheduled tick has arrived.
    fn drain_deferred_inputs(&mut self) {
        let tick = self.metrics.tick_count;
        // Partition: send the ones that are ready, keep the rest.
        let mut remaining = Vec::new();
        for (session_id, data, send_at) in std::mem::take(&mut self.deferred_inputs) {
            if tick >= send_at {
                if let Some(session) = self.sessions.iter().find(|s| s.info.id == session_id) {
                    if let Err(e) = session.send_input(data) {
                        error!("Failed to send deferred input: {e}");
                    }
                }
            } else {
                remaining.push((session_id, data, send_at));
            }
        }
        self.deferred_inputs = remaining;
    }

    /// Poll for completed worktree sync results and handle them.
    fn poll_sync_results(&mut self) {
        let Some(rx) = &self.worktree_sync.rx else {
            return;
        };

        // Drain everything currently buffered. A worker thread that *panics*
        // drops its sender without sending every result, so `completed` may
        // never reach `pending`; detecting the channel disconnecting (all
        // senders gone) lets us finalize with whatever arrived instead of
        // leaving `in_progress` stuck forever.
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok((session_id, result)) => {
                    self.worktree_sync.completed.push((session_id, result));
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        let all_received = self.worktree_sync.completed.len() >= self.worktree_sync.pending;
        if all_received || disconnected {
            self.worktree_sync.in_progress = false;
            self.worktree_sync.rx = None;
            self.finish_sync();
        }
    }

    /// Finalize sync: compose status message and send conflict prompts.
    fn finish_sync(&mut self) {
        let results = std::mem::take(&mut self.worktree_sync.completed);
        let mut synced = 0usize;
        let mut conflicts = 0usize;
        let mut errors = Vec::new();

        for (session_id, result) in results {
            match result {
                git::SyncResult::Synced => synced += 1,
                git::SyncResult::Conflict(_) => {
                    conflicts += 1;
                    self.send_conflict_prompt(session_id);
                }
                git::SyncResult::Error(msg) => errors.push(msg),
            }
        }

        if !errors.is_empty() {
            self.set_error(format!("Sync failed: {}", errors.join(", ")));
        } else if conflicts > 0 {
            self.set_status(
                StatusLevel::Info,
                format!("{synced} synced, {conflicts} conflict(s) (sent to Claude)"),
            );
        } else {
            self.set_status(StatusLevel::Success, format!("{synced} worktree(s) synced"));
        }
    }

    /// Send a conflict resolution prompt to a session via bracketed paste,
    /// with a deferred Enter so the app processes the text first.
    fn send_conflict_prompt(&mut self, session_id: SessionId) {
        if let Some(session) = self.sessions.iter().find(|s| s.info.id == session_id) {
            let mut paste = b"\x1b[200~".to_vec();
            paste.extend_from_slice(SYNC_CONFLICT_PROMPT.as_bytes());
            paste.extend_from_slice(b"\x1b[201~");
            if let Err(e) = session.send_input(paste) {
                error!("Failed to send sync prompt to session: {e}");
            } else {
                self.deferred_inputs.push((
                    session_id,
                    b"\r".to_vec(),
                    self.metrics.tick_count + DEFERRED_INPUT_DELAY_TICKS,
                ));
            }
        }
    }

    /// Start syncing the active session's worktrees with their base ref.
    ///
    /// Worktrees sharing the same parent repo are synced sequentially (to avoid
    /// concurrent `index.lock` contention), while different repos sync in parallel.
    pub(crate) fn start_sync(&mut self) {
        if self.worktree_sync.in_progress {
            return;
        }

        let worktree_sessions: Vec<_> = self
            .active_session()
            .into_iter()
            .flat_map(|s| {
                s.info
                    .worktrees
                    .iter()
                    .map(move |wt| (s.info.id, wt.worktree_path.clone(), wt.repo_path.clone()))
            })
            .collect();

        if worktree_sessions.is_empty() {
            self.set_status(StatusLevel::Info, "No worktrees to sync");
            return;
        }

        let count = worktree_sessions.len();
        let (tx, rx) = mpsc::channel();

        // Group worktrees by repo so those sharing a repo sync sequentially.
        let mut by_repo = std::collections::HashMap::<PathBuf, Vec<(SessionId, PathBuf)>>::new();
        for (session_id, worktree_path, repo_path) in worktree_sessions {
            by_repo
                .entry(repo_path)
                .or_default()
                .push((session_id, worktree_path));
        }

        for worktrees in by_repo.into_values() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for (session_id, worktree_path) in worktrees {
                    // base_ref = None: derive the rebase target per-worktree
                    // (upstream → origin/HEAD → origin/main → origin/master).
                    let result = git::sync_worktree(&worktree_path, None);
                    let _ = tx.send((session_id, result));
                }
            });
        }

        self.worktree_sync.in_progress = true;
        self.worktree_sync.rx = Some(rx);
        self.worktree_sync.pending = count;
        self.worktree_sync.completed.clear();
        self.set_status(StatusLevel::Info, format!("Syncing {count} worktree(s)..."));
    }

    /// Handle external state changes detected from other instances.
    fn handle_external_state_change(&mut self, delta: StateDelta) {
        self.session_counter = self.session_counter.max(delta.counter_increment);
        self.apply_removed_sessions(delta.removed_sessions);
        self.apply_updated_sessions(delta.updated_sessions);
        self.apply_added_sessions(delta.added_sessions);
    }

    /// Drop sessions deleted by other instances.
    fn apply_removed_sessions(&mut self, removed: Vec<SessionId>) {
        for session_id in removed {
            if let Some(pos) = self.sessions.iter().position(|s| s.info.id == session_id) {
                // Detach rather than silently drop: detach unregisters the
                // pane, which EOFs the blocked reader thread. A plain drop
                // would leak that spawn_blocking thread for the process
                // lifetime (the deleting instance owns the actual teardown).
                self.sessions.remove(pos).detach();
                // Keep `active_index` anchored to the *same* session. When a
                // session before the active one is removed every later session
                // shifts down by one, so the active index must follow; removing
                // the active session itself falls through to the clamp below.
                if pos < self.active_index {
                    self.active_index -= 1;
                }
                // Clamp into bounds (handles removing the active/last session
                // and an emptied list) so later raw-index access can't panic.
                self.sync_active_session_to_project();
            }
        }
    }

    /// Apply metadata changes made to existing sessions by other instances.
    fn apply_updated_sessions(&mut self, updated: Vec<sync::SharedSession>) {
        for shared_session in updated {
            if let Some(session) = self
                .sessions
                .iter_mut()
                .find(|s| s.info.id == shared_session.id)
            {
                Self::apply_shared_session_metadata(session, &shared_session);
            }
        }
    }

    /// Adopt or spawn sessions added by other instances.
    ///
    /// Headless spawns (CLI/MCP) persist the DB row with an empty
    /// `backend_id` because only the TUI knows the real tmux pane id
    /// (`%N`). Before spawning, call `discover()` and look up the
    /// existing window by sanitized name — otherwise we'd create a
    /// duplicate `tb-<name>` window for the one the CLI already
    /// opened, and exact-match `send-keys` would then fail on
    /// "ambiguous window".
    ///
    /// `discover()` is cached per backend_type so a burst of added
    /// sessions only hits tmux once per backend.
    fn apply_added_sessions(&mut self, added: Vec<sync::SharedSession>) {
        let mut discovered_by_backend: HashMap<
            String,
            Vec<crate::agent::backend::DiscoveredSession>,
        > = HashMap::new();
        for shared_session in added {
            if self.sessions.iter().any(|s| s.info.id == shared_session.id) {
                continue;
            }

            // Skip sessions whose backend this instance can't manage (e.g. a
            // remote host not in our hosts.toml) — adopting them locally would
            // corrupt backend_type and risk a pane-id collision.
            let Some(backend) = self.resolve_persisted_backend(&shared_session.backend_type) else {
                continue;
            };

            let matching_backend_id = {
                let discovered = discovered_by_backend
                    .entry(shared_session.backend_type.clone())
                    .or_insert_with(|| {
                        // A remote backend needs its control-mode connection up
                        // before adopt(); ready it lazily on first use here.
                        match backend.ensure_ready() {
                            Ok(()) => backend.discover().unwrap_or_default(),
                            Err(e) => {
                                tracing::warn!(
                                    backend = backend.name(),
                                    "Backend not ready for adopted session: {e}"
                                );
                                Vec::new()
                            }
                        }
                    });
                Self::find_matching_discovered(&shared_session, discovered)
                    .map(|disc| disc.backend_id.clone())
            };

            let (rows, cols) = self.content_area_size();
            if let Some(backend_id) = matching_backend_id {
                // Either adoption succeeds or the discovered window already
                // exists and adoption fails transiently — in both cases we
                // must NOT fall through to spawn, because that would create
                // a second window with the same name.
                self.adopt_shared_session(&shared_session, &backend_id, &backend, rows, cols);
                continue;
            }

            // No matching discovered window. If the session has an
            // `agent_session_id`, spawn a fresh window with
            // `--session-id` so claude creates the conversation (e.g.
            // CLI-created sessions whose claude process never persisted
            // a conversation before we first adopt them).
            if shared_session.agent_session_id.is_some() {
                self.spawn_restored_session(&shared_session, &backend, rows, cols);
            }
        }
    }

    /// Adopt an already-running discovered window into our session list.
    fn adopt_shared_session(
        &mut self,
        shared_session: &sync::SharedSession,
        backend_id: &str,
        backend: &Arc<dyn crate::agent::backend::SessionBackend>,
        rows: u16,
        cols: u16,
    ) {
        let provider = {
            let cfg = SessionConfig {
                agent: shared_session.agent.clone(),
                ..SessionConfig::default()
            };
            self.provider_for(&cfg)
        };
        match Session::adopt(
            shared_session.name.clone(),
            rows,
            cols,
            backend_id,
            backend,
            &provider,
            HashMap::new(),
        ) {
            Ok(mut adopted_session) => {
                // Preserve the original session ID from shared state
                // (Session::adopt creates a new one, but we need the
                // consistent ID).
                adopted_session.info.id = shared_session.id;
                Self::apply_shared_session_metadata(&mut adopted_session, shared_session);
                self.sessions.push(adopted_session);
                // Persist the real pane_id (`%N`) back to the DB so future
                // lookups short-circuit on the backend_id match instead of
                // always falling back to name matching.
                self.save_state();
                tracing::debug!(
                    "Adopted session {} from another instance",
                    shared_session.name
                );
            }
            Err(e) => {
                tracing::debug!(
                    "Failed to adopt session {} by discovered id {}: {}",
                    shared_session.name,
                    backend_id,
                    e
                );
            }
        }
    }

    /// Build the [`SessionConfig`] for relaunching an *existing* session — either
    /// a startup-restore respawn ([`Self::spawn_restored_session`]) or a `Ctrl+U`
    /// undelete ([`Self::restore_deleted_session`]). Both reuse the session's
    /// stable `SessionId` and must inject the `THURBOX_*` identity/dir env so the
    /// agent's status hooks can attribute their `session signal` — without it the
    /// row's `hook_state` never updates and the session renders Idle forever
    /// (the bug these paths previously hit by calling `Session::spawn` directly).
    /// The caller sets `resume_session_id` afterward. Mirrors the headless
    /// `session_ops::restart_session_headless` shape.
    fn restored_session_config(
        id: crate::session::SessionId,
        agent_session_id: Option<String>,
        agent: String,
        cwd: Option<PathBuf>,
        backend_type: &str,
    ) -> SessionConfig {
        let mut config = SessionConfig {
            agent_session_id: agent_session_id.clone(),
            session_id: Some(id),
            cwd,
            agent,
            // Preserve a persisted off-local (`ssh:<host>` / `wsl:<distro>`)
            // backend — set *before* env injection, which skips the local-path
            // dir vars for remote sessions. Local stays `None`.
            backend: crate::session::is_remote_backend(backend_type)
                .then(|| backend_type.to_string()),
            ..SessionConfig::default()
        };
        // `THURBOX_SESSION` (derived from `session_id`) is the identity that
        // matters; an empty `THURBOX_SESSION_ID` for an id-less agent is harmless
        // since the CLI resolves identity from `THURBOX_SESSION` first.
        crate::session_ops::inject_thurbox_env(
            &mut config,
            agent_session_id.as_deref().unwrap_or_default(),
            None,
        );
        config
    }

    /// Spawn a fresh window for a restored session that has an
    /// `agent_session_id` but no matching discovered window.
    fn spawn_restored_session(
        &mut self,
        shared_session: &sync::SharedSession,
        backend: &Arc<dyn crate::agent::backend::SessionBackend>,
        rows: u16,
        cols: u16,
    ) {
        let Some(agent_sid) = shared_session.agent_session_id.as_ref() else {
            return;
        };
        let worktree_infos = Self::recreate_worktrees(&shared_session.worktrees);
        let cwd = worktree_infos
            .first()
            .map(|wt| wt.worktree_path.clone())
            .or(shared_session.cwd.clone());

        // Build the relaunch config reusing the existing SessionId and injecting
        // identity/dir env, so the agent's status hooks can attribute their
        // `session signal` (otherwise the row stays Idle). The injector runs
        // before `resume_trigger_for`, which only reads `CLAUDE_CONFIG_DIR`.
        let mut config = Self::restored_session_config(
            shared_session.id,
            Some(agent_sid.clone()),
            shared_session.agent.clone(),
            cwd,
            &shared_session.backend_type,
        );
        let def = self.agent_def_for(&config.agent);
        config.resume_session_id =
            crate::session_ops::resume_trigger_for(&def, agent_sid, &config.env);
        let provider = self.launch_provider_for(&config);

        if let Ok(mut spawned) = Session::spawn(
            shared_session.name.clone(),
            rows,
            cols,
            &config,
            backend,
            &provider,
        ) {
            spawned.info.id = shared_session.id;
            spawned.info.worktrees = worktree_infos;
            spawned.info.additional_dirs = shared_session.additional_dirs.clone();
            spawned.info.parent_session_id = shared_session.parent_session_id;
            spawned.info.display_order = shared_session.display_order;
            self.sessions.push(spawned);
            self.save_state();
            tracing::debug!(
                "Spawned restored session {} with --resume",
                shared_session.name
            );
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Persist state, then detach. The order is forced: `Session::detach`
    /// consumes the session by value, so `save_state` (which reads
    /// `session.info`) must run while `self.sessions` is intact. A hung save
    /// is bounded by the SQLite busy_timeout, after which upsert errors are
    /// logged and detach still runs.
    pub fn shutdown(mut self) {
        self.finalize_pending_delete();
        self.save_state();
        // Do NOT remove worktrees — they persist for resume.
        // Detach from backend sessions without killing them — they persist in tmux.
        for session in self.sessions {
            session.detach();
        }
    }

    /// Rebuild the file viewer tree from the currently active session's
    /// worktrees and additional directories. Called when the active session
    /// changes or when the file viewer is first opened.
    pub(crate) fn rebuild_file_viewer_for_active(&mut self) {
        if let Some(session) = self.sessions.get(self.active_index) {
            self.file_viewer.rebuild_from_session(&session.info);
        } else {
            self.file_viewer.clear();
        }
    }

    /// Set status bar message with the given severity level.
    fn set_status(&mut self, level: StatusLevel, text: impl Into<String>) {
        self.status_message = Some(StatusMessage {
            text: text.into(),
            level,
            created_at: std::time::Instant::now(),
        });
    }

    fn set_error(&mut self, text: impl Into<String>) {
        self.set_status(StatusLevel::Error, text.into());
    }

    fn set_info(&mut self, text: impl Into<String>) {
        self.set_status(StatusLevel::Info, text.into());
    }

    /// Persist session state to the SQLite database.
    ///
    /// Only writes sessions and the session counter. Project mutations
    /// (add/edit/delete) write to the DB at their point of change, avoiding
    /// race conditions where a blanket re-write overwrites another instance's edits.
    fn save_state(&self) {
        if let Err(e) = self.db.set_session_counter(self.session_counter) {
            error!("Failed to save session counter to DB: {e}");
        }

        for session in &self.sessions {
            let shared_session = self.session_to_shared(session);
            if let Err(e) = self.db.upsert_session(&shared_session) {
                error!("Failed to upsert session to DB: {e}");
            }
        }
    }

    /// Build a SharedSession from a local Session.
    fn session_to_shared(&self, session: &Session) -> sync::SharedSession {
        sync::SharedSession {
            id: session.info.id,
            name: session.info.name.clone(),
            agent: session.info.agent.clone(),
            backend_id: session.backend_id().to_string(),
            backend_type: session.backend_name().to_string(),
            agent_session_id: session.info.agent_session_id.clone(),
            cwd: session.info.cwd.clone(),
            additional_dirs: session.info.additional_dirs.clone(),
            worktrees: session
                .info
                .worktrees
                .iter()
                .cloned()
                .map(Into::into)
                .collect(),
            shell_backend_id: session.info.shell_backend_id.clone(),
            parent_session_id: session.info.parent_session_id,
            display_order: session.info.display_order,
            tombstone: false,
            tombstone_at: None,
        }
    }

    /// Load persisted session state from the database.
    ///
    /// Returns `Some(sessions, counter)` if there are active sessions in the DB,
    /// or `None` if no sessions exist (indicating a fresh start or first run).
    pub fn load_persisted_state_from_db(&self) -> Option<(Vec<sync::SharedSession>, usize)> {
        let sessions = self.db.list_active_sessions().ok()?;
        if sessions.is_empty() {
            return None;
        }

        // Only sessions with an agent_session_id are resumable.
        let resumable: Vec<sync::SharedSession> = sessions
            .into_iter()
            .filter(|s| s.agent_session_id.is_some())
            .collect();

        if resumable.is_empty() {
            return None;
        }

        let counter = self.db.get_session_counter().unwrap_or(0);
        Some((resumable, counter))
    }

    /// Restore sessions from the database on startup.
    ///
    /// Local sessions are restored synchronously by querying the local backend
    /// for its existing tmux windows. Sessions persisted with a remote
    /// (`ssh:<host>` / `wsl:<distro>`) `backend_type` are restored in the
    /// background instead — one discovery thread per host, drained by
    /// `poll_remote_restore` each tick — because readying a remote backend
    /// means an ssh connect that can take tens of seconds (or minutes for a
    /// down host) and must never block the first frame.
    pub fn restore_sessions(&mut self, sessions: Vec<sync::SharedSession>, session_counter: usize) {
        self.session_counter = session_counter;
        // Opt-in startup-restore breakdown (THURBOX_PERF_LOG). Local restore is
        // sequential — each session is adopted with a blocking
        // `capture_pane_text` — so per-backend discover and per-session adopt
        // timings show where the remaining time goes. Read once here, never
        // per tick.
        let perf_log = std::env::var_os("THURBOX_PERF_LOG").is_some();

        // Only sessions with an agent_session_id are resumable.
        let resumable: Vec<sync::SharedSession> = sessions
            .into_iter()
            .filter(|s| s.agent_session_id.is_some())
            .collect();

        let (remote, local): (Vec<_>, Vec<_>) = resumable
            .into_iter()
            .partition(|s| crate::session::is_remote_backend(&s.backend_type));

        let discovered_by_backend = self.discover_windows_by_backend(&local, perf_log);

        for shared in local {
            let discovered = discovered_by_backend
                .get(&shared.backend_type)
                .cloned()
                .unwrap_or_default();
            let adopt_start = perf_log.then(std::time::Instant::now);
            let name = perf_log.then(|| shared.name.clone());
            self.restore_single_session(shared, &discovered);
            if let (Some(start), Some(name)) = (adopt_start, name) {
                tracing::info!(
                    session = %name,
                    adopt_ms = start.elapsed().as_millis() as u64,
                    "restore_adopt"
                );
            }
        }

        self.start_remote_restore(remote, perf_log);

        // Claim ownership of restored sessions in the shared state
        self.save_state();
    }

    /// Kick off one discovery thread per distinct remote backend and queue its
    /// sessions for adoption in [`Self::poll_remote_restore`]. Sessions on a
    /// backend this instance can't manage (an unknown host) are left
    /// un-adopted, exactly like the synchronous path.
    fn start_remote_restore(&mut self, remote: Vec<sync::SharedSession>, perf_log: bool) {
        if remote.is_empty() {
            return;
        }
        let mut grouped: HashMap<String, Vec<sync::SharedSession>> = HashMap::new();
        for shared in remote {
            grouped
                .entry(shared.backend_type.clone())
                .or_default()
                .push(shared);
        }

        let (tx, rx) = mpsc::channel();
        let mut pending: HashMap<String, Vec<sync::SharedSession>> = HashMap::new();
        for (backend_type, sessions) in grouped {
            let Some(backend) = self.resolve_persisted_backend(&backend_type) else {
                continue;
            };
            Self::spawn_remote_discovery(backend, backend_type.clone(), perf_log, tx.clone());
            pending.insert(backend_type, sessions);
        }
        if !pending.is_empty() {
            self.remote_restore = Some(RemoteRestore { rx, pending });
        }
    }

    /// Ready + discover one remote backend on its own thread, reporting the
    /// result over `tx` (a dropped receiver — app shut down — is fine).
    fn spawn_remote_discovery(
        backend: Arc<dyn SessionBackend>,
        backend_type: String,
        perf_log: bool,
        tx: mpsc::Sender<RemoteDiscovery>,
    ) {
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let discovered = Self::ready_and_discover(&backend);
            if perf_log {
                tracing::info!(
                    backend = %backend_type,
                    windows = discovered.len() as u64,
                    discover_ms = start.elapsed().as_millis() as u64,
                    "restore_discover"
                );
            }
            let _ = tx.send((backend_type, discovered));
        });
    }

    /// Drain finished remote-backend discoveries and adopt their sessions.
    /// Adoption runs on the main thread but talks to the control-mode
    /// connection the background thread already brought up, so the expensive
    /// part (ssh connect + remote tmux ready) never blocks a frame.
    fn poll_remote_restore(&mut self) {
        let Some(state) = &mut self.remote_restore else {
            return;
        };
        let mut ready: Vec<RemoteDiscovery> = Vec::new();
        let mut disconnected = false;
        loop {
            match state.rx.try_recv() {
                Ok(msg) => ready.push(msg),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        self.adopt_remote_discoveries(ready);

        let finished = disconnected
            || self
                .remote_restore
                .as_ref()
                .is_some_and(|s| s.pending.is_empty());
        if finished {
            if let Some(state) = self.remote_restore.take() {
                for backend_type in state.pending.keys() {
                    warn!(
                        backend = %backend_type,
                        "Remote restore ended without a discovery result"
                    );
                }
            }
        }
    }

    /// Adopt every pending session whose backend just reported its windows.
    fn adopt_remote_discoveries(&mut self, ready: Vec<RemoteDiscovery>) {
        // Adopting makes each restored session active; a late-arriving host
        // must not steal the user's current selection, so snapshot + restore it.
        let prior_active = self.sessions.get(self.active_index).map(|s| s.info.id);
        let prior_focus = self.focus;
        let before = self.sessions.len();
        for (backend_type, discovered) in ready {
            let Some(sessions) = self
                .remote_restore
                .as_mut()
                .and_then(|s| s.pending.remove(&backend_type))
            else {
                continue;
            };
            for shared in sessions {
                // Another path (e.g. the DB sync) may have adopted it meanwhile.
                if self.sessions.iter().any(|s| s.info.id == shared.id) {
                    continue;
                }
                self.restore_single_session(shared, &discovered);
            }
        }
        // Count sessions actually added — an adopt/respawn that failed inside
        // `restore_single_session` must not inflate the toast.
        let adopted = self.sessions.len() - before;
        if adopted == 0 {
            return;
        }
        if let Some(id) = prior_active {
            if let Some(idx) = self.sessions.iter().position(|s| s.info.id == id) {
                self.active_index = idx;
                self.focus = prior_focus;
            }
        }
        self.save_state();
        self.set_status(
            StatusLevel::Info,
            format!("Restored {adopted} remote session(s)"),
        );
        self.request_redraw();
    }

    /// Discover existing backend windows once per distinct `backend_type`.
    ///
    /// Each backend is readied + discovered at most once. Only local backends
    /// reach this at startup (remote ones go through
    /// [`Self::start_remote_restore`]); unknown backend types map to an empty
    /// list so their sessions are left un-adopted rather than misadopted.
    fn discover_windows_by_backend(
        &self,
        resumable: &[sync::SharedSession],
        perf_log: bool,
    ) -> HashMap<String, Vec<crate::agent::backend::DiscoveredSession>> {
        let mut discovered_by_backend: HashMap<
            String,
            Vec<crate::agent::backend::DiscoveredSession>,
        > = HashMap::new();
        for shared in resumable {
            if discovered_by_backend.contains_key(&shared.backend_type) {
                continue;
            }
            let discover_start = perf_log.then(std::time::Instant::now);
            let disc = self.discover_windows_for_backend(&shared.backend_type);
            if let Some(start) = discover_start {
                tracing::info!(
                    backend = %shared.backend_type,
                    windows = disc.len() as u64,
                    discover_ms = start.elapsed().as_millis() as u64,
                    "restore_discover"
                );
            }
            discovered_by_backend.insert(shared.backend_type.clone(), disc);
        }
        discovered_by_backend
    }

    /// Ready + discover a single backend's windows, degrading to an empty list
    /// when the backend is unknown or not reachable (logged, never fatal).
    fn discover_windows_for_backend(
        &self,
        backend_type: &str,
    ) -> Vec<crate::agent::backend::DiscoveredSession> {
        // Skip discovery for backends this instance can't manage (unknown
        // remote hosts); their sessions are left un-adopted.
        let Some(backend) = self.resolve_persisted_backend(backend_type) else {
            return Vec::new();
        };
        Self::ready_and_discover(&backend)
    }

    /// Ready a backend and list its windows, degrading to an empty list on
    /// error (logged, never fatal). Associated (no `&self`) so the remote
    /// restore threads can run it off the UI thread.
    fn ready_and_discover(
        backend: &Arc<dyn SessionBackend>,
    ) -> Vec<crate::agent::backend::DiscoveredSession> {
        if let Err(e) = backend.ensure_ready() {
            warn!(
                backend = backend.name(),
                "Backend not ready during restore; skipping its sessions: {e}"
            );
            return Vec::new();
        }

        backend.discover().unwrap_or_else(|e| {
            warn!(
                backend = backend.name(),
                "Failed to discover sessions from backend: {e}"
            );
            Vec::new()
        })
    }

    /// Restore a single session synchronously (used during startup). The
    /// backend is selected from the session's persisted `backend_type`.
    fn restore_single_session(
        &mut self,
        shared: sync::SharedSession,
        discovered: &[crate::agent::backend::DiscoveredSession],
    ) {
        let name = shared.name.clone();

        let agent = if shared.agent.is_empty() {
            DEFAULT_AGENT_NAME.to_string()
        } else {
            shared.agent.clone()
        };

        let worktrees: Vec<WorktreeInfo> =
            shared.worktrees.iter().cloned().map(Into::into).collect();

        let Some(agent_session_id) = shared.agent_session_id.clone() else {
            return;
        };

        let matching_discovered = Self::find_matching_discovered(&shared, discovered);

        // Select the correct backend based on the persisted backend_type.
        // Skip sessions on a backend this instance can't manage (unknown remote
        // host) rather than misadopting them on local.
        let Some(backend) = self.resolve_persisted_backend(&shared.backend_type) else {
            return;
        };

        // Try to adopt the existing backend session.
        let provider = self.provider_for(&SessionConfig {
            agent: agent.clone(),
            ..SessionConfig::default()
        });
        let adopted = matching_discovered.and_then(|disc| {
            let (rows, cols) = self.content_area_size();
            match Session::adopt(
                name.clone(),
                rows,
                cols,
                &disc.backend_id,
                &backend,
                &provider,
                HashMap::new(),
            ) {
                Ok(session) => Some(session),
                Err(e) => {
                    error!("Failed to adopt session '{name}': {e}");
                    None
                }
            }
        });

        if let Some(session) = adopted {
            self.finish_adopted_session(session, &shared, agent, worktrees, discovered);
        } else {
            self.respawn_stale_session(name, shared, agent, agent_session_id, worktrees);
        }
    }

    /// Wire a freshly-adopted backend session into the app: copy persisted
    /// metadata, re-adopt its shell pane, and make it the active session.
    fn finish_adopted_session(
        &mut self,
        mut session: Session,
        shared: &sync::SharedSession,
        agent: String,
        worktrees: Vec<WorktreeInfo>,
        discovered: &[crate::agent::backend::DiscoveredSession],
    ) {
        session.info.id = shared.id;
        session.info.agent_session_id = shared.agent_session_id.clone();
        session.info.cwd = shared.cwd.clone();
        session.info.additional_dirs = shared.additional_dirs.clone();
        session.info.agent = agent;
        session.info.worktrees = worktrees;
        session.info.parent_session_id = shared.parent_session_id;
        session.info.display_order = shared.display_order;
        resolve_repo_display_names(&mut session.info);

        // Re-adopt shell pane if one was persisted
        if let Some(shell_bid) = &shared.shell_backend_id {
            let (rows, cols) = self.content_area_size();
            Self::readopt_shell_pane(&mut session, shell_bid, discovered, rows, cols);
        }

        self.sessions.push(session);
        self.active_index = self.sessions.len() - 1;
        self.focus = InputFocus::Terminal;
    }

    /// Re-adopt a persisted shell pane onto `session` if its backend window is
    /// still alive. Failures are non-fatal (logged only).
    fn readopt_shell_pane(
        session: &mut Session,
        shell_bid: &str,
        discovered: &[crate::agent::backend::DiscoveredSession],
        rows: u16,
        cols: u16,
    ) {
        if !discovered
            .iter()
            .any(|d| d.backend_id == *shell_bid && d.is_alive)
        {
            return;
        }
        if let Err(e) = session.adopt_shell_pane(shell_bid, rows, cols) {
            tracing::warn!("Failed to re-adopt shell pane: {e}");
        }
    }

    /// No matching backend session or adopt failed — respawn resuming when the
    /// agent supports it (a claude transcript exists for this
    /// `agent_session_id`, or a `resume_latest` agent resumes its last session
    /// in the cwd), otherwise start fresh (e.g. claude with `--session-id` so it
    /// creates the conversation, or any agent whose process never persisted one).
    fn respawn_stale_session(
        &mut self,
        name: String,
        shared: sync::SharedSession,
        agent: String,
        agent_session_id: String,
        worktrees: Vec<WorktreeInfo>,
    ) {
        // Reuse the original SessionId so the session's identity is stable across
        // restarts: `do_spawn_session` upserts in place (no soft-delete + new-row
        // churn), and `THURBOX_SESSION` is re-injected with the same id. Any
        // cached id / queued message addressed to this session stays valid.
        // Preserving a remote `backend` keeps the respawn on its own host —
        // without it `do_spawn_session` would silently relaunch the session on
        // the local tmux, pointed at worktree paths that only exist remotely.
        let backend = crate::session::is_remote_backend(&shared.backend_type)
            .then(|| shared.backend_type.clone());
        let mut config = SessionConfig {
            session_id: Some(shared.id),
            resume_session_id: None,
            agent_session_id: Some(agent_session_id.clone()),
            cwd: shared.cwd,
            agent,
            fork_session_id: None,
            backend,
            ..SessionConfig::default()
        };
        let def = self.agent_def_for(&config.agent);
        config.resume_session_id =
            crate::session_ops::resume_trigger_for(&def, &agent_session_id, &config.env);
        self.new_session.additional_dirs = shared.additional_dirs;
        self.new_session.parent_session_id = shared.parent_session_id;
        // After a reboot every session takes this path (the tmux server died),
        // so the manual list position must survive the respawn or one restart
        // would scramble the whole order. `do_spawn_session` pushes + persists
        // the fresh session; stamp the inherited order on it afterwards.
        let display_order = shared.display_order;
        let before = self.sessions.len();
        self.do_spawn_session(name, &config, worktrees);
        if self.sessions.len() > before && display_order.is_some() {
            if let Some(session) = self.sessions.last_mut() {
                session.info.display_order = display_order;
            }
            self.save_state();
        }
    }

    /// Find a discovered backend session matching a shared session.
    ///
    /// Tries to match by `backend_id` first; if that fails (e.g. the row
    /// was created by the headless CLI/MCP path which doesn't know the
    /// real tmux pane id yet), falls back to matching by the sanitized
    /// window name (`tb-<safe_name>`).
    fn find_matching_discovered<'a>(
        shared: &sync::SharedSession,
        discovered: &'a [crate::agent::backend::DiscoveredSession],
    ) -> Option<&'a crate::agent::backend::DiscoveredSession> {
        if !shared.backend_id.is_empty() {
            if let Some(d) = discovered
                .iter()
                .find(|d| d.backend_id == shared.backend_id && d.is_alive)
            {
                return Some(d);
            }
        }
        let expected_name = crate::agent::tmux::agent_window_name(&shared.name);
        discovered
            .iter()
            .find(|d| d.name == expected_name && d.is_alive)
    }

    /// Paste `text` into a session as a bracketed paste, then queue an Enter.
    /// `boot_delay_ticks` delays the paste itself — pass 0 for a session that is
    /// already running, or [`AGENT_BOOT_DELAY_TICKS`] for one just spawned so
    /// its agent CLI has time to come up.
    fn send_prompt_to_session(&mut self, session_id: SessionId, text: &str, boot_delay_ticks: u64) {
        let mut paste = b"\x1b[200~".to_vec();
        paste.extend_from_slice(text.as_bytes());
        paste.extend_from_slice(b"\x1b[201~");

        if boot_delay_ticks == 0 {
            let Some(session) = self.sessions.iter().find(|s| s.info.id == session_id) else {
                return;
            };
            if let Err(e) = session.send_input(paste) {
                error!("Failed to send prompt to session {session_id}: {e}");
                return;
            }
            self.deferred_inputs.push((
                session_id,
                b"\r".to_vec(),
                self.metrics.tick_count + DEFERRED_INPUT_DELAY_TICKS,
            ));
        } else {
            // Defer both paste and Enter so the freshly spawned agent can boot.
            let paste_at = self.metrics.tick_count + boot_delay_ticks;
            self.deferred_inputs.push((session_id, paste, paste_at));
            self.deferred_inputs.push((
                session_id,
                b"\r".to_vec(),
                paste_at + DEFERRED_INPUT_DELAY_TICKS,
            ));
        }
    }

    /// Spawn (or reuse) a named session — optionally on a fresh worktree — and
    /// queue `prompt` into it. The session is named `name`; a recurring caller
    /// reuses that session on later invocations (and after a TUI restart, where
    /// it is restored from the database by name). Shared by automations
    /// (`auto-<id>`) and tasks (`<title> · #<id>`).
    #[allow(clippy::too_many_arguments)]
    fn spawn_and_prompt(
        &mut self,
        name: String,
        repo_path: &std::path::Path,
        worktree_branch: Option<&str>,
        base_branch: Option<&str>,
        agent: Option<&str>,
        extra_repos: &[crate::session::ExtraRepo],
        prompt: &str,
    ) -> Result<SessionId, String> {
        // Reuse an existing session (this run or restored after restart).
        if let Some(existing) = self.sessions.iter().find(|s| s.info.name == name) {
            let id = existing.info.id;
            self.send_prompt_to_session(id, prompt, 0);
            return Ok(id);
        }

        // Expand a leading `~` — the path may have been typed by hand in the
        // editor (or set via the CLI), and git/`current_dir` don't expand it.
        let repo_path = crate::paths::expand_tilde(&repo_path.to_string_lossy());
        let repo_path = repo_path.as_path();

        let mut worktrees: Vec<WorktreeInfo> = Vec::new();
        if let Some(branch) = worktree_branch {
            let base = base_branch.unwrap_or("main");
            // Idempotent: a recurring caller reuses the worktree it made on the
            // first invocation instead of failing because the branch exists.
            let path = git::create_or_attach_worktree(repo_path, branch, base)
                .map_err(|e| format!("create worktree {branch} off {base}: {e}"))?;
            worktrees.push(WorktreeInfo {
                repo_path: repo_path.to_path_buf(),
                worktree_path: path,
                branch: branch.to_string(),
            });
        }

        // Multi-repo: each extra repo gets its own worktree on the shared branch
        // (off its own base, falling back to the primary's) or is attached as-is.
        let mut additional_dirs: Vec<PathBuf> = Vec::new();
        for extra in extra_repos {
            let extra_path = crate::paths::expand_tilde(&extra.repo_path.to_string_lossy());
            if extra.worktree {
                let branch = worktree_branch.ok_or_else(|| {
                    "a worktree extra-repo requires a worktree branch".to_string()
                })?;
                let base = extra
                    .base_branch
                    .as_deref()
                    .or(base_branch)
                    .unwrap_or("main");
                let path = git::create_or_attach_worktree(&extra_path, branch, base)
                    .map_err(|e| format!("create worktree {branch} off {base}: {e}"))?;
                worktrees.push(WorktreeInfo {
                    repo_path: extra_path.clone(),
                    worktree_path: path,
                    branch: branch.to_string(),
                });
            } else {
                additional_dirs.push(extra_path);
            }
        }

        let cwd = worktrees
            .first()
            .map(|w| w.worktree_path.clone())
            .unwrap_or_else(|| repo_path.to_path_buf());
        let mut config = SessionConfig {
            cwd: Some(cwd),
            ..SessionConfig::default()
        };
        if let Some(a) = agent {
            config.agent = a.to_string();
        }

        self.new_session.additional_dirs = additional_dirs;
        self.do_spawn_session(name.clone(), &config, worktrees);
        let session = self
            .sessions
            .iter()
            .find(|s| s.info.name == name)
            .ok_or_else(|| "session spawn failed".to_string())?;
        let id = session.info.id;
        self.send_prompt_to_session(id, prompt, AGENT_BOOT_DELAY_TICKS);
        Ok(id)
    }

    // ---- Tasks (right-side panel) ----------------------------------------

    /// The full agent prompt for a task (id + title + description + CLI hints),
    /// falling back to `title` if the task is no longer cached. Keeps the
    /// trigger paths from seeding an agent with just the bare title — the agent
    /// gets explicit context that it is solving a Thurbox task and how to fetch
    /// more / close it out (see [`crate::session::Task::agent_prompt`]).
    fn task_agent_prompt(&self, task_id: i64, title: &str) -> String {
        self.task_ui
            .cached_tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.agent_prompt())
            .unwrap_or_else(|| title.to_string())
    }

    /// Validate `m` and persist it. Returns `true` on success; on failure sets an
    /// error status and returns `false` (leaving the editor open).
    fn save_task(&mut self, m: &modals::TaskEditorModal) -> bool {
        let title = m.title.value().trim().to_string();
        if title.is_empty() {
            self.set_error("Title cannot be empty");
            return false;
        }
        // Trimmed-empty description persists as `None`.
        let description = {
            let d = m.description.value().trim();
            (!d.is_empty()).then(|| d.to_string())
        };
        let result = match m.editing_id {
            Some(id) => match self.db.get_task(id) {
                // The editor no longer authors the agent action — preserve any
                // action set out-of-band (e.g. via the CLI). The trigger-time
                // picker (`r`) is how the TUI runs an action.
                Ok(Some(mut task)) => {
                    task.title = title;
                    task.description = description;
                    task.status = m.status;
                    self.db.update_task(&task)
                }
                Ok(None) => {
                    self.set_error("Task no longer exists");
                    return false;
                }
                Err(e) => Err(e),
            },
            None => {
                let new = crate::storage::tasks::NewTask {
                    title,
                    description,
                    status: m.status,
                    action: None,
                    source: crate::session::SOURCE_LOCAL.to_string(),
                    external_id: None,
                    external_url: None,
                };
                self.db.create_task(&new).map(|_| ())
            }
        };
        if let Err(e) = result {
            self.set_error(format!("Failed to save task: {e}"));
            return false;
        }
        self.refresh_tasks();
        self.set_status(StatusLevel::Success, "Task saved");
        true
    }

    /// Compute the panel layout for `area` from the current panel visibility
    /// and feature flags — the single funnel into `layout::compute_layout`,
    /// so the view, mouse routing, and content sizing can never disagree.
    pub(crate) fn layout_for(&self, area: Rect) -> layout::PanelAreas {
        layout::compute_layout(
            area,
            self.show_info_panel,
            self.show_tasks_panel,
            // The review's changed-files list lives in the file-viewer column, so
            // force that column present while a review is open.
            self.show_file_viewer || self.active_review().is_some(),
            self.global_search.active,
            self.features.automations,
            self.automation_ui.cached_automations.len(),
            // Carve the transient status row whenever there's a message to show
            // (a status/error toast or the live sync spinner) — must match what
            // `render_status_message_row` renders so the row is never empty.
            self.worktree_sync.in_progress || self.status_message.is_some(),
        )
    }

    /// The layout for the whole terminal screen (mouse hit-testing, sizing).
    pub(crate) fn screen_layout(&self) -> layout::PanelAreas {
        self.layout_for(Rect::new(0, 0, self.terminal_cols, self.terminal_rows))
    }

    pub(crate) fn content_area_size(&self) -> (u16, u16) {
        let terminal = self.screen_layout().terminal;
        let inner = Block::default().borders(Borders::ALL).inner(terminal);
        (inner.height, inner.width)
    }
}

/// Populate `repo_display_names` on a session from worktree repo paths,
/// cwd, and additional_dirs, using git remote names where available.
///
/// Thin wrapper over [`session_member_dirs`] — the single source of truth for
/// *which* directories a session spans and in what order — keeping the displayed
/// repo names and the workspace symlink set from ever drifting.
fn resolve_repo_display_names(info: &mut SessionInfo) {
    info.repo_display_names =
        session_member_dirs(info.cwd.as_deref(), &info.worktrees, &info.additional_dirs)
            .into_iter()
            .filter_map(|(name, _)| name)
            .collect();
}

/// The `(display_name, directory)` pairs a session spans, in display order:
/// worktree repos first (name from the original `repo_path`, dir = the checkout),
/// then non-worktree `additional_dirs`; or the lone `cwd` repo when there are no
/// worktrees. `name` is `None` only for pathological paths with no resolvable
/// name. This is the canonical member set used for both the repo-name display and
/// the multi-repo symlink workspace.
fn session_member_dirs(
    cwd: Option<&std::path::Path>,
    worktrees: &[WorktreeInfo],
    additional_dirs: &[PathBuf],
) -> Vec<(Option<String>, PathBuf)> {
    let mut members: Vec<(Option<String>, PathBuf)> = Vec::new();

    let wt_paths: std::collections::HashSet<&std::path::Path> = worktrees
        .iter()
        .map(|wt| wt.worktree_path.as_path())
        .collect();

    if !worktrees.is_empty() {
        for wt in worktrees {
            members.push((
                git::repo_display_name(&wt.repo_path),
                wt.worktree_path.clone(),
            ));
        }
    } else if let Some(cwd) = cwd {
        members.push((git::repo_display_name(cwd), cwd.to_path_buf()));
    }

    for dir in additional_dirs {
        if !wt_paths.contains(dir.as_path()) {
            members.push((git::repo_display_name(dir), dir.clone()));
        }
    }

    members
}

/// The directory the agent process should launch in.
///
/// For a single-member session that's the member itself (`primary_cwd`). For a
/// multi-member session it is a per-session **symlink workspace** (built
/// idempotently from the members) so the agent sees every repo as a
/// subdirectory — agent-neutral, needing no per-CLI flag. On any failure it
/// falls back to `primary_cwd`.
fn resolve_process_cwd(
    agent_session_id: Option<&str>,
    primary_cwd: Option<PathBuf>,
    worktrees: &[WorktreeInfo],
    additional_dirs: &[PathBuf],
    host: Option<&crate::session::HostDef>,
) -> Option<PathBuf> {
    let members = session_member_dirs(primary_cwd.as_deref(), worktrees, additional_dirs);
    if members.len() < 2 {
        return primary_cwd;
    }
    let Some(id) = agent_session_id else {
        return primary_cwd;
    };

    let pairs: Vec<(String, PathBuf)> = members
        .into_iter()
        .map(|(name, dir)| {
            let label = name
                .or_else(|| dir.file_name().and_then(|s| s.to_str()).map(String::from))
                .unwrap_or_else(|| "repo".to_string());
            (label, dir)
        })
        .collect();

    crate::session_ops::spawn::build_multi_repo_workspace(host, id, &pairs).or(primary_cwd)
}

#[cfg(test)]
mod acceptance;

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::agent::SessionBackend;
    use crate::session::{Automation, AutomationAction, AutomationRunStatus, AutomationSchedule};

    // --- Session switching tests ---

    /// Inert backend for unit tests. `detached` counts `detach` calls so
    /// lifecycle tests can assert pane I/O teardown.
    #[derive(Default)]
    struct StubBackend {
        detached: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl SessionBackend for StubBackend {
        fn name(&self) -> &str {
            "stub"
        }
        fn check_available(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn ensure_ready(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn spawn(
            &self,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<&Path>,
            _: &std::collections::HashMap<String, String>,
            _: u16,
            _: u16,
        ) -> anyhow::Result<crate::agent::backend::SpawnedSession> {
            anyhow::bail!("stub backend does not spawn")
        }
        fn adopt(
            &self,
            _: &str,
            _: u16,
            _: u16,
        ) -> anyhow::Result<crate::agent::backend::AdoptedSession> {
            anyhow::bail!("stub backend does not adopt")
        }
        fn discover(&self) -> anyhow::Result<Vec<crate::agent::backend::DiscoveredSession>> {
            Ok(vec![])
        }
        fn resize(&self, _: &str, _: u16, _: u16) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_dead(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        fn kill(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn detach(&self, _: &str) -> anyhow::Result<()> {
            self.detached
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn pane_pid(&self, _: &str) -> anyhow::Result<Option<u32>> {
            Ok(None)
        }
    }

    fn stub_backend_arc() -> Arc<dyn SessionBackend> {
        Arc::new(StubBackend::default())
    }

    fn stub_provider() -> Arc<dyn crate::agent::AgentProvider> {
        Arc::new(crate::agent::GenericProvider::new(
            crate::agent::agent_config::builtin_registry()
                .default_agent()
                .unwrap()
                .clone(),
        ))
    }

    fn stub_agents() -> AgentRegistry {
        crate::agent::agent_config::builtin_registry()
    }

    fn stub_backend() -> BackendRegistry {
        BackendRegistry::new(stub_backend_arc())
    }

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn poll_config_reload_picks_up_agents_toml_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = crate::paths::TestPathGuard::new(tmp.path());
        let mut app = app_with_sessions(0);
        app.status_message = None;

        // No change → no reload, no toast.
        app.poll_config_reload();
        assert!(app.status_message.is_none());

        // An external edit appears on the next poll without a restart.
        let path = crate::agent::agent_config::agents_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "default = \"mine\"\n[[agents]]\nname = \"mine\"\ncommand = \"x\"\n",
        )
        .unwrap();

        app.poll_config_reload();
        assert_eq!(app.agents.default, "mine");
        assert_eq!(
            app.status_message.as_ref().map(|m| m.level),
            Some(StatusLevel::Info)
        );

        // Stable afterwards: no repeated toasts.
        app.status_message = None;
        app.poll_config_reload();
        assert!(app.status_message.is_none());
    }

    #[test]
    fn poll_config_reload_picks_up_keybindings_edit_but_not_self_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = crate::paths::TestPathGuard::new(tmp.path());
        let mut app = app_with_sessions(0);
        app.status_message = None;

        // External edit → rebind applies live.
        crate::storage::keybindings::save_keybindings_json(r#"{ "QuitApp": ["ctrl+x"] }"#).unwrap();
        app.poll_config_reload();
        assert_eq!(
            app.keybindings.chord_for(crate::session::Action::QuitApp),
            Some(&crate::session::KeyChord::ctrl('x'))
        );
        assert!(app.status_message.is_some());

        // A self-write (the F1 editor persisting) refreshes the stored mtime,
        // so the next poll stays quiet.
        app.status_message = None;
        crate::storage::keybindings::save_keybindings_json(r#"{ "QuitApp": ["ctrl+z"] }"#).unwrap();
        app.mark_keybindings_saved();
        app.poll_config_reload();
        assert!(app.status_message.is_none());
    }

    #[test]
    fn poll_config_reload_applies_settings_live_feature_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = crate::paths::TestPathGuard::new(tmp.path());
        let mut app = app_with_sessions(0);
        app.status_message = None;
        assert!(app.features.tasks);

        // An external edit disabling a live flag applies on the next poll.
        let path = crate::agent::settings_config::settings_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[features]\ntasks = false\n").unwrap();

        app.poll_config_reload();
        assert!(!app.features.tasks, "live feature flag reloaded from disk");
        assert!(app.status_message.is_some(), "reload toasts");

        // Stable afterwards: no repeated toasts.
        app.status_message = None;
        app.poll_config_reload();
        assert!(app.status_message.is_none());
    }

    #[test]
    fn mark_settings_saved_suppresses_self_write_toast() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = crate::paths::TestPathGuard::new(tmp.path());
        let mut app = app_with_sessions(0);
        app.status_message = None;

        let path = crate::agent::settings_config::settings_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[features]\nfile_viewer = false\n").unwrap();
        app.mark_settings_saved();

        app.poll_config_reload();
        assert!(
            app.status_message.is_none(),
            "a recorded self-write doesn't re-toast"
        );
    }

    /// A notification click handler writes `pending_focus_session_id` to the
    /// shared SQLite metadata; the TUI's poll picks it up and switches the
    /// active session + focus.
    #[test]
    fn apply_pending_focus_request_switches_active_session() {
        let mut app = app_with_sessions(3);
        let target_id = app.sessions[2].info.id;
        app.active_index = 0;
        app.focus = InputFocus::SessionList;

        // Simulate the click-handler write.
        app.db
            .conn_ref()
            .execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    crate::session::PENDING_FOCUS_SESSION_ID_KEY,
                    target_id.to_string()
                ],
            )
            .unwrap();

        app.apply_pending_focus_request();
        assert_eq!(app.active_index, 2);
        assert_eq!(app.focus, InputFocus::Terminal);
        // The row is consumed atomically, so a second call is a no-op.
        let prev_active = app.active_index;
        app.apply_pending_focus_request();
        assert_eq!(app.active_index, prev_active);
    }

    /// A focus request that doesn't match any current session is dropped
    /// (the session may have been deleted before the click landed).
    #[test]
    fn apply_pending_focus_request_ignores_unknown_session() {
        let mut app = app_with_sessions(2);
        app.active_index = 0;
        app.focus = InputFocus::SessionList;
        let bogus = crate::session::SessionId::default();
        app.db
            .conn_ref()
            .execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    crate::session::PENDING_FOCUS_SESSION_ID_KEY,
                    bogus.to_string()
                ],
            )
            .unwrap();

        app.apply_pending_focus_request();
        assert_eq!(app.active_index, 0, "no match → leave selection alone");
        assert_eq!(app.focus, InputFocus::SessionList);
        // But the row is still consumed so a stale id doesn't sit forever.
        assert_eq!(app.db.take_pending_focus_session_id().unwrap(), None);
    }

    /// Garbage in the metadata key is ignored gracefully and the row consumed.
    #[test]
    fn apply_pending_focus_request_tolerates_garbage() {
        let mut app = app_with_sessions(2);
        app.db
            .conn_ref()
            .execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
                rusqlite::params![crate::session::PENDING_FOCUS_SESSION_ID_KEY, "not-a-uuid"],
            )
            .unwrap();
        app.apply_pending_focus_request();
        assert_eq!(app.active_index, 0);
        assert_eq!(app.db.take_pending_focus_session_id().unwrap(), None);
    }

    #[test]
    fn apply_removed_sessions_detaches_pane_io() {
        let detached = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backend_arc: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            detached: Arc::clone(&detached),
        });
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            stub_agents(),
            test_db(),
        );
        app.sessions.push(Session::stub(
            "removed-elsewhere",
            &backend_arc,
            &stub_provider(),
        ));
        let session_id = app.sessions[0].info.id;

        app.apply_removed_sessions(vec![session_id]);

        assert!(app.sessions.is_empty());
        assert_eq!(
            detached.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "externally removed session must detach so the reader thread EOFs"
        );
    }

    /// Create an App with N stub sessions.
    fn app_with_sessions(count: usize) -> App {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            stub_agents(),
            test_db(),
        );
        for _i in 0..count {
            let session = Session::stub("test-session", &backend_arc, &provider);
            app.sessions.push(session);
        }
        if !app.sessions.is_empty() {
            app.active_index = 0;
        }
        app
    }

    /// Persist `app.sessions[idx]` to the DB so `load_hook_states` can find it
    /// by id (the stub harness pushes sessions without DB rows).
    fn persist_session(app: &App, idx: usize) -> crate::session::SessionId {
        let shared = app.session_to_shared(&app.sessions[idx]);
        app.db.upsert_session(&shared).unwrap();
        shared.id
    }

    /// Simulate an external `session signal`: write the hook state, then
    /// invalidate the status cache the way a real out-of-process signal would
    /// (its commit bumps this connection's `data_version`). The tests share one
    /// in-memory connection, so the bump must be emulated explicitly.
    fn signal_hook(app: &mut App, id: crate::session::SessionId, state: &str) {
        app.db.set_hook_state(id, state).unwrap();
        app.invalidate_hook_state_cache();
    }

    #[test]
    fn restored_session_config_injects_identity_env() {
        // Regression: a restored/undeleted session must carry `THURBOX_SESSION`
        // so its status hooks can attribute `session signal` — otherwise the row
        // stays Idle forever. The two relaunch paths previously skipped this.
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let id = crate::session::SessionId::default();
        let config = App::restored_session_config(
            id,
            Some("agent-conv-uuid".into()),
            "claude".into(),
            None,
            "local-tmux",
        );
        assert_eq!(config.session_id, Some(id));
        assert_eq!(config.backend, None, "local backend stays None");
        assert_eq!(
            config.env.get("THURBOX_SESSION"),
            Some(&id.to_string()),
            "THURBOX_SESSION must match the reused SessionId"
        );
        assert_eq!(
            config.env.get("THURBOX_SESSION_ID"),
            Some(&"agent-conv-uuid".to_string())
        );
        // The config/data dir overrides pin the hook's `thurbox-cli` to this DB.
        assert!(config
            .env
            .contains_key(crate::paths::CONFIG_DIR_OVERRIDE_ENV));
        assert!(config.env.contains_key(crate::paths::DATA_DIR_OVERRIDE_ENV));
    }

    #[test]
    fn restored_session_config_idless_agent_still_has_session_identity() {
        // An agent that can't report its own id (None) still gets `THURBOX_SESSION`
        // from the reused SessionId — the identity the CLI resolves from first.
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let id = crate::session::SessionId::default();
        let config = App::restored_session_config(id, None, "codex".into(), None, "local-tmux");
        assert_eq!(config.env.get("THURBOX_SESSION"), Some(&id.to_string()));
    }

    #[test]
    fn restored_session_config_remote_backend_carries_and_skips_local_dirs() {
        // A restored off-local session must set `backend` *before* env injection
        // so the local-path dir vars are skipped (they don't exist on the host)
        // — and so the relaunch provider adapts the def's args for the host.
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let id = crate::session::SessionId::default();
        let config = App::restored_session_config(
            id,
            Some("agent-conv-uuid".into()),
            "claude".into(),
            None,
            "ssh:devbox",
        );
        assert_eq!(config.backend.as_deref(), Some("ssh:devbox"));
        assert!(config.env.contains_key("THURBOX_SESSION"));
        assert!(!config
            .env
            .contains_key(crate::paths::CONFIG_DIR_OVERRIDE_ENV));
        assert!(!config.env.contains_key(crate::paths::DATA_DIR_OVERRIDE_ENV));
    }

    #[test]
    fn derive_session_status_covers_every_state() {
        use crate::storage::HookRow;
        let row = |state: &str, state_at: i64, seen_at: i64| HookRow {
            state: Some(state.into()),
            state_at: Some(state_at),
            seen_at: Some(seen_at),
        };

        // Exited forces Idle, even with a live hook state.
        assert_eq!(
            derive_session_status(Some(&row("working", 1, 0)), true, false, 0),
            SessionStatus::Idle
        );
        // No hook / idle / unknown → Idle.
        assert_eq!(
            derive_session_status(None, false, false, 0),
            SessionStatus::Idle
        );
        assert_eq!(
            derive_session_status(Some(&row("idle", 1, 0)), false, false, 0),
            SessionStatus::Idle
        );
        assert_eq!(
            derive_session_status(Some(&row("nonsense", 1, 0)), false, false, 0),
            SessionStatus::Idle
        );
        // working (with live output) / blocked map straight through.
        assert_eq!(
            derive_session_status(Some(&row("working", 1, 0)), false, false, 0),
            SessionStatus::Working
        );
        // Quiet *up to and including* the threshold is still live (boundary).
        assert_eq!(
            derive_session_status(
                Some(&row("working", 1, 0)),
                false,
                false,
                WORKING_OUTPUT_STALE_MS
            ),
            SessionStatus::Working,
            "quiescence at exactly the threshold is still Working"
        );
        assert_eq!(
            derive_session_status(Some(&row("blocked", 1, 0)), false, false, 0),
            SessionStatus::Blocked
        );
        // A `working` state that's gone quiet past the threshold is a stuck
        // edge (interrupt / crash) → fall back to Idle; blocked is unaffected.
        assert_eq!(
            derive_session_status(
                Some(&row("working", 1, 0)),
                false,
                false,
                WORKING_OUTPUT_STALE_MS + 1
            ),
            SessionStatus::Idle,
            "a quiet 'working' state past the staleness window reverts to Idle"
        );
        assert_eq!(
            derive_session_status(
                Some(&row("blocked", 1, 0)),
                false,
                false,
                WORKING_OUTPUT_STALE_MS + 1
            ),
            SessionStatus::Blocked,
            "blocked never times out on quiescence"
        );
        // done: unseen → Done; already-seen or just-seen → Idle.
        assert_eq!(
            derive_session_status(Some(&row("done", 5, 0)), false, false, 0),
            SessionStatus::Done
        );
        assert_eq!(
            derive_session_status(Some(&row("done", 5, 5)), false, false, 0),
            SessionStatus::Idle
        );
        assert_eq!(
            derive_session_status(Some(&row("done", 5, 0)), false, true, 0),
            SessionStatus::Idle
        );
    }

    #[test]
    fn refresh_maps_hook_state_to_status() {
        let mut app = app_with_sessions(1);
        let id = persist_session(&app, 0);

        // No hook fired yet → Idle (never-active default).
        app.refresh_session_statuses();
        assert_eq!(app.sessions[0].info.status, SessionStatus::Idle);

        for (state, expected) in [
            ("working", SessionStatus::Working),
            ("blocked", SessionStatus::Blocked),
        ] {
            signal_hook(&mut app, id, state);
            app.refresh_session_statuses();
            assert_eq!(
                app.sessions[0].info.status, expected,
                "hook '{state}' should map to {expected:?}"
            );
        }
    }

    #[test]
    fn refresh_recovers_stuck_working_after_output_goes_quiet() {
        // A `working` session whose `done`/`idle` edge never fired (e.g. the turn
        // was interrupted with Esc — Claude Code emits no hook for that) must not
        // spin forever: once its terminal goes quiet past the staleness window it
        // falls back to Idle. While output is still fresh it stays Working.
        let mut app = app_with_sessions(1);
        let id = persist_session(&app, 0);
        signal_hook(&mut app, id, "working");

        // Fresh output → genuinely working.
        app.refresh_session_statuses();
        assert_eq!(app.sessions[0].info.status, SessionStatus::Working);

        // Terminal goes quiet past the threshold (interrupt, no further hook) →
        // the stuck state is rescued to Idle even though the DB still says working.
        app.sessions[0].backdate_output_for_test(WORKING_OUTPUT_STALE_MS + 1_000);
        app.refresh_session_statuses();
        assert_eq!(
            app.sessions[0].info.status,
            SessionStatus::Idle,
            "an interrupted (quiet) 'working' session must not spin forever"
        );
    }

    #[test]
    fn refresh_done_shows_until_focus_leaves_then_idle() {
        // Two sessions; session 0 is the active (focused) one.
        let mut app = app_with_sessions(2);
        let _id0 = persist_session(&app, 0);
        let id1 = persist_session(&app, 1);
        app.active_index = 0;
        app.refresh_session_statuses(); // establish focus baseline (on session 0)

        // The FOCUSED session finishes: `Done` is visible (not instantly Idle) —
        // you should see the blue "done" for the session you're watching.
        app.active_index = 1;
        app.refresh_session_statuses(); // focus moves to 1 (baseline update)
        signal_hook(&mut app, id1, "done");
        app.refresh_session_statuses();
        assert_eq!(
            app.sessions[1].info.status,
            SessionStatus::Done,
            "a done session you're viewing shows Done, not instant Idle"
        );

        // Move focus OFF it → acknowledged → seen → Idle (persisted).
        app.active_index = 0;
        app.refresh_session_statuses();
        assert_eq!(app.sessions[1].info.status, SessionStatus::Idle);
        let row = app.db.load_hook_states().unwrap();
        let row = row.get(&id1).unwrap();
        assert!(
            row.seen_at.unwrap_or(0) >= row.state_at.unwrap_or(i64::MAX),
            "seen_at persisted at/after the done timestamp"
        );
    }

    #[test]
    fn refresh_seen_done_stays_idle_without_reload() {
        // Regression for the status-cache write-through (ADR-P6): once a `done`
        // session is acknowledged (focus left → seen), it must stay Idle on
        // later ticks even when nothing reloads the cache. Marking it seen is a
        // same-connection write that does NOT bump `data_version`, so without
        // mirroring `seen_at` into the cached row the next derive would see a
        // stale `seen_at < state_at` and flip it back to Done.
        let mut app = app_with_sessions(2);
        persist_session(&app, 0);
        let id1 = persist_session(&app, 1);
        app.active_index = 0;
        app.refresh_session_statuses(); // baseline focus on 0

        app.active_index = 1;
        app.refresh_session_statuses(); // focus → 1
        signal_hook(&mut app, id1, "done");
        app.refresh_session_statuses();
        assert_eq!(app.sessions[1].info.status, SessionStatus::Done);

        // Acknowledge by leaving focus: seen_at written + mirrored into cache.
        app.active_index = 0;
        app.refresh_session_statuses();
        assert_eq!(app.sessions[1].info.status, SessionStatus::Idle);

        // A further refresh with no external change must NOT reload the cache,
        // yet the session stays Idle (proves the write-through, not a reload).
        let loads_before = app.perf_counters().hook_state_loads;
        app.refresh_session_statuses();
        assert_eq!(
            app.perf_counters().hook_state_loads,
            loads_before,
            "no external change ⇒ no cache reload on the follow-up tick"
        );
        assert_eq!(
            app.sessions[1].info.status,
            SessionStatus::Idle,
            "an acknowledged done session must stay Idle via the seen_at write-through"
        );
    }

    #[test]
    fn refresh_done_unfocused_shows_done() {
        // A background session that finishes shows Done (blue) until visited.
        let mut app = app_with_sessions(2);
        persist_session(&app, 0);
        let id1 = persist_session(&app, 1);
        app.active_index = 0;
        signal_hook(&mut app, id1, "done");
        app.refresh_session_statuses();
        assert_eq!(app.sessions[1].info.status, SessionStatus::Done);
    }

    #[test]
    fn refresh_marks_dirty_on_status_change() {
        let mut app = app_with_sessions(1);
        let id = persist_session(&app, 0);
        app.refresh_session_statuses();
        app.mark_redrawn();
        assert!(!app.should_redraw(), "quiescent after a redraw");

        // An external hook write must make the next refresh repaint.
        signal_hook(&mut app, id, "blocked");
        app.refresh_session_statuses();
        assert!(
            app.should_redraw(),
            "a hook-driven status change must mark the UI dirty"
        );
    }

    #[test]
    fn working_session_animates_spinner_and_repaints() {
        let mut app = app_with_sessions(1);
        let id = persist_session(&app, 0);
        signal_hook(&mut app, id, "working");

        // Advance enough ticks to cross a spinner-frame boundary and confirm the
        // frame moves and the UI is marked dirty (so the live list animates).
        app.metrics.tick_count = 0;
        app.refresh_session_statuses();
        let f0 = app.spinner_frame();
        app.mark_redrawn();
        app.metrics.tick_count = SPINNER_TICKS_PER_FRAME; // next frame
        app.refresh_session_statuses();
        assert_ne!(app.spinner_frame(), f0, "spinner frame advances");
        assert!(
            app.should_redraw(),
            "a working session keeps the list repainting"
        );

        // The live glyph for Working is a spinner frame, not the static icon.
        let g = crate::ui::status_glyph(
            SessionStatus::Working,
            crate::ui::SPINNER_FRAMES[app.spinner_frame()],
        );
        assert!(crate::ui::SPINNER_FRAMES.contains(&g));
    }

    #[test]
    fn spinner_advances_even_when_a_status_field_also_changes() {
        // Regression: the spinner must tick on *every* refresh, never be
        // short-circuited past by the `||` when another visible field changed
        // the same tick. Session 0 stays Working (driving the spinner); session
        // 1 flips Idle→Blocked on the second refresh so `changed` is true.
        let mut app = app_with_sessions(2);
        let id0 = persist_session(&app, 0);
        let id1 = persist_session(&app, 1);
        signal_hook(&mut app, id0, "working");

        app.metrics.tick_count = 0;
        app.refresh_session_statuses();
        let f0 = app.spinner_frame();

        // Cross a spinner-frame boundary AND change session 1's status together.
        app.metrics.tick_count = SPINNER_TICKS_PER_FRAME;
        signal_hook(&mut app, id1, "blocked");
        app.refresh_session_statuses();

        assert_eq!(app.sessions[1].info.status, SessionStatus::Blocked);
        assert_ne!(
            app.spinner_frame(),
            f0,
            "spinner must advance even when another field changed the same tick"
        );
    }

    #[test]
    fn idle_session_does_not_force_spinner_repaints() {
        let mut app = app_with_sessions(1);
        persist_session(&app, 0); // no hook → Idle
        app.refresh_session_statuses();
        app.mark_redrawn();
        // No working session: crossing a spinner boundary must NOT force a paint.
        app.metrics.tick_count += SPINNER_TICKS_PER_FRAME;
        app.refresh_session_statuses();
        assert!(
            !app.should_redraw(),
            "an idle TUI must not repaint just to animate a (nonexistent) spinner"
        );
    }

    #[test]
    fn refresh_exited_session_is_idle_regardless_of_hook() {
        let mut app = app_with_sessions(1);
        let id = persist_session(&app, 0);
        signal_hook(&mut app, id, "blocked");
        app.sessions[0].mark_exited_for_test();
        app.refresh_session_statuses();
        assert_eq!(app.sessions[0].info.status, SessionStatus::Idle);
    }

    #[test]
    fn start_new_session_skips_host_picker_when_no_hosts() {
        let mut app = app_with_sessions(0);
        app.start_new_session();
        // No hosts configured → straight to the repo picker, no host step.
        assert!(matches!(app.modal, modals::Modal::RepoPicker(_)));
        assert!(app.new_session.backend.is_none());
    }

    #[test]
    fn start_new_session_shows_host_picker_with_hosts() {
        let mut app = app_with_sessions(0);
        app.set_hosts(crate::session::HostRegistry {
            config_version: None,
            hosts: vec![
                crate::session::HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                },
                crate::session::HostDef::wsl("Ubuntu"),
            ],
        });
        app.start_new_session();
        match app.modal {
            modals::Modal::HostPicker(ref hp) => {
                // "local" first, then each off-local host (ssh + wsl).
                assert_eq!(hp.choices.len(), 3);
                assert_eq!(hp.choices[0].backend, "");
                assert_eq!(hp.choices[1].backend, "ssh:devbox");
                assert_eq!(hp.choices[2].backend, "wsl:Ubuntu");
                assert!(hp.choices[2].label.contains("WSL"));
            }
            ref other => panic!("expected host picker, got {other:?}"),
        }
    }

    #[test]
    fn host_for_backend_resolves_ssh_and_wsl() {
        let mut app = app_with_sessions(0);
        app.set_hosts(crate::session::HostRegistry {
            config_version: None,
            hosts: vec![
                crate::session::HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                },
                crate::session::HostDef::wsl("Ubuntu"),
            ],
        });
        assert!(app.host_for_backend(None).is_none());
        assert!(app.host_for_backend(Some("local-tmux")).is_none());
        assert_eq!(
            app.host_for_backend(Some("ssh:devbox"))
                .unwrap()
                .destination,
            "me@devbox"
        );
        assert!(app.host_for_backend(Some("wsl:Ubuntu")).unwrap().is_wsl());
        assert!(app.host_for_backend(Some("ssh:unknown")).is_none());
    }

    #[test]
    fn resolve_persisted_backend_skips_unknown_ssh_but_falls_back_for_local() {
        let app = app_with_sessions(0);
        // Known backend → resolved.
        assert_eq!(
            app.resolve_persisted_backend("stub")
                .map(|b| b.name().to_string()),
            Some("stub".to_string())
        );
        // Legacy/local values fall back to the default backend.
        for legacy in ["", "tmux", "local-tmux"] {
            assert_eq!(
                app.resolve_persisted_backend(legacy)
                    .map(|b| b.name().to_string()),
                Some("stub".to_string()),
                "legacy '{legacy}' should fall back to default"
            );
        }
        // Unknown remote backend → skipped (None), never misadopted on local.
        assert!(app.resolve_persisted_backend("ssh:nope").is_none());
    }

    #[test]
    fn backend_for_defaults_to_registry_default() {
        let app = app_with_sessions(0);
        let config = SessionConfig::default();
        let backend = app.backend_for(&config).expect("default backend");
        assert_eq!(backend.name(), "stub");
    }

    #[test]
    fn backend_for_empty_name_uses_default() {
        let app = app_with_sessions(0);
        let config = SessionConfig {
            backend: Some(String::new()),
            ..SessionConfig::default()
        };
        let backend = app.backend_for(&config).expect("default backend");
        assert_eq!(backend.name(), "stub");
    }

    #[test]
    fn backend_for_unknown_backend_errors() {
        let app = app_with_sessions(0);
        let config = SessionConfig {
            backend: Some("ssh:does-not-exist".into()),
            ..SessionConfig::default()
        };
        match app.backend_for(&config) {
            Ok(b) => panic!("expected error, got backend {}", b.name()),
            Err(err) => assert!(err.contains("Unknown backend"), "got: {err}"),
        }
    }

    #[test]
    fn switch_forward_advances_to_next_session() {
        let mut app = app_with_sessions(3);
        app.active_index = 0;
        app.switch_session_forward();
        assert_eq!(app.active_index, 1);
    }

    #[test]
    fn switch_forward_at_last_session_wraps() {
        let mut app = app_with_sessions(3);
        app.active_index = 2;
        app.switch_session_forward();
        assert_eq!(app.active_index, 0);
    }

    #[test]
    fn switch_backward_moves_to_previous_session() {
        let mut app = app_with_sessions(3);
        app.active_index = 2;
        app.switch_session_backward();
        assert_eq!(app.active_index, 1);
    }

    #[test]
    fn switch_backward_at_first_session_wraps() {
        let mut app = app_with_sessions(3);
        app.active_index = 0;
        app.switch_session_backward();
        assert_eq!(app.active_index, 2);
    }

    #[test]
    fn switch_with_no_sessions_is_noop() {
        let mut app = app_with_sessions(0);
        app.switch_session_forward();
        assert_eq!(app.active_index, 0);
        app.switch_session_backward();
        assert_eq!(app.active_index, 0);
    }

    #[test]
    fn apply_removed_keeps_active_anchored_when_earlier_session_removed() {
        // Regression: a CLI `session delete` of a session *before* the active
        // one must shift `active_index` down so it keeps pointing at the SAME
        // session, not silently jump to a different one. [A, B(active), C];
        // delete A → [B, C], active must stay on B (now index 0).
        let mut app = app_with_sessions(3);
        app.active_index = 1;
        let active_id = app.sessions[1].info.id;
        let removed_id = app.sessions[0].info.id;

        app.apply_removed_sessions(vec![removed_id]);

        assert_eq!(app.sessions.len(), 2);
        assert_eq!(
            app.active_index, 0,
            "active_index should follow its session down after an earlier removal"
        );
        assert_eq!(
            app.sessions[app.active_index].info.id, active_id,
            "active session identity must be preserved across external removal"
        );
    }

    #[test]
    fn apply_removed_active_session_clamps_in_bounds() {
        // Deleting the active session (the last one) must leave `active_index`
        // in bounds so subsequent raw-index access (restart, shell toggle)
        // can't panic. [A, B, C(active)]; delete C → [A, B], active in bounds.
        let mut app = app_with_sessions(3);
        app.active_index = 2;
        let removed_id = app.sessions[2].info.id;

        app.apply_removed_sessions(vec![removed_id]);

        assert_eq!(app.sessions.len(), 2);
        assert!(
            app.active_index < app.sessions.len(),
            "active_index must stay in bounds after the active session is removed"
        );
    }

    #[test]
    fn apply_removed_all_sessions_resets_index() {
        // A CLI clearing every session must not leave a dangling index.
        let mut app = app_with_sessions(2);
        app.active_index = 1;
        let ids: Vec<_> = app.sessions.iter().map(|s| s.info.id).collect();

        app.apply_removed_sessions(ids);

        assert!(app.sessions.is_empty());
        assert_eq!(app.active_index, 0);
    }

    #[test]
    fn restart_with_stale_active_index_does_not_panic() {
        // If external state shrank the list and left `active_index` out of
        // bounds, hitting restart must degrade gracefully, never panic.
        let mut app = app_with_sessions(1);
        app.active_index = 5; // stale, out of bounds
                              // Should be a no-op, not an index-out-of-bounds panic.
        app.restart_active_session();
    }

    #[test]
    fn switch_follows_activity_and_repo_group_order() {
        use crate::session::SessionStatus;
        // DB order: [webapp/Working, infra/Blocked, webapp/Working].
        let mut app = app_with_sessions(3);
        app.sessions[0].info.repo_display_names = vec!["webapp".to_string()];
        app.sessions[0].info.status = SessionStatus::Working;
        app.sessions[1].info.repo_display_names = vec!["infra".to_string()];
        app.sessions[1].info.status = SessionStatus::Blocked;
        app.sessions[2].info.repo_display_names = vec!["webapp".to_string()];
        app.sessions[2].info.status = SessionStatus::Working;

        // Order is status-independent: by repo group (infra before webapp by
        // group label), so navigation visits [1] then [0, 2].
        app.active_index = 1;
        app.switch_session_forward();
        assert_eq!(app.active_index, 0, "infra → webapp/0");
        app.switch_session_forward();
        assert_eq!(app.active_index, 2, "webapp/0 → webapp/2");
        app.switch_session_forward();
        assert_eq!(app.active_index, 1, "webapp/2 → wrap to infra");
    }

    #[test]
    fn switch_with_single_session_is_noop() {
        let mut app = app_with_sessions(1);
        app.active_index = 0;
        app.switch_session_forward();
        assert_eq!(app.active_index, 0);
        app.switch_session_backward();
        assert_eq!(app.active_index, 0);
    }

    // --- Scroll tests ---

    fn parser_with_scrollback() -> vt100::Parser {
        let mut parser = vt100::Parser::new(24, 80, 100);
        for i in 0..50 {
            parser.process(format!("line {i}\r\n").as_bytes());
        }
        parser
    }

    #[test]
    fn scrollback_starts_at_zero() {
        let parser = parser_with_scrollback();
        assert_eq!(parser.screen().scrollback(), 0);
    }

    #[test]
    fn scrollback_increments() {
        let mut parser = parser_with_scrollback();
        parser.screen_mut().set_scrollback(5);
        assert_eq!(parser.screen().scrollback(), 5);
    }

    #[test]
    fn scrollback_clamps_to_max() {
        let mut parser = parser_with_scrollback();
        parser.screen_mut().set_scrollback(usize::MAX);
        let max = parser.screen().scrollback();
        // Should be clamped to the actual scrollback content, not usize::MAX
        assert!(max < usize::MAX);
        assert!(max > 0);
    }

    #[test]
    fn scrollback_restores_after_probe() {
        let mut parser = parser_with_scrollback();
        parser.screen_mut().set_scrollback(3);

        // Probe total scrollback (same technique as render_terminal)
        let saved = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(usize::MAX);
        let _total = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(saved);

        assert_eq!(parser.screen().scrollback(), 3);
    }

    #[test]
    fn scrollback_zero_stays_at_bottom() {
        let mut parser = parser_with_scrollback();
        assert_eq!(parser.screen().scrollback(), 0);

        // New output while at bottom keeps offset at 0
        parser.process(b"new line\r\n");
        assert_eq!(parser.screen().scrollback(), 0);
    }

    #[test]
    fn page_scroll_amount_is_half_content_height() {
        let app = App::new(50, 100, stub_backend(), stub_agents(), test_db());
        // rows = 50 - 4 = 46, half = 23
        assert_eq!(app.page_scroll_amount(), 23);
    }

    #[test]
    fn page_scroll_amount_small_terminal() {
        let app = App::new(6, 80, stub_backend(), stub_agents(), test_db());
        // rows = 6 - 4 = 2, half = 1
        assert_eq!(app.page_scroll_amount(), 1);
    }

    #[test]
    fn mouse_scroll_lines_constant() {
        assert_eq!(MOUSE_SCROLL_LINES, 3);
    }

    // --- Session naming tests ---

    #[test]
    fn next_session_name_starts_at_one() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        assert_eq!(app.next_session_name(), "1");
    }

    #[test]
    fn next_session_name_increments() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        assert_eq!(app.next_session_name(), "1");
        assert_eq!(app.next_session_name(), "2");
        assert_eq!(app.next_session_name(), "3");
    }

    #[test]
    fn next_session_name_continues_from_restored_counter() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.session_counter = 5;
        assert_eq!(app.next_session_name(), "6");
    }

    #[test]
    fn automations_pane_focus_cycle_and_nav() {
        use crate::session::{AutomationAction, AutomationSchedule};
        let make = |id: i64, name: &str| Automation {
            id,
            name: name.into(),
            enabled: true,
            schedule: AutomationSchedule::Once { at: 0 },
            timezone: None,
            action: AutomationAction::Send {
                session_id: SessionId::default(),
            },
            prompt: "p".into(),
            created_at: 0,
            updated_at: 0,
            last_run_at: None,
            next_run_at: None,
        };
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());

        // The automations pane is reached via j/k (part of the left column), and
        // its central counterpart is the editor: Ctrl+L/H from the pane move
        // into the editor and back, just like SessionList ↔ Terminal.
        app.focus = InputFocus::SessionList;
        assert_eq!(app.cycle_focus_forward(), InputFocus::Terminal);
        app.focus = InputFocus::Terminal;
        assert_eq!(app.cycle_focus_backward(), InputFocus::SessionList);
        app.focus = InputFocus::Automations;
        assert_eq!(app.cycle_focus_forward(), InputFocus::AutomationEditor);
        assert_eq!(app.cycle_focus_backward(), InputFocus::AutomationEditor);
        app.focus = InputFocus::AutomationEditor;
        assert_eq!(app.cycle_focus_backward(), InputFocus::Automations);

        // j/k navigate within the pane; past either end they loop out into the
        // session list (the column is circular).
        // Keys route through the real pipeline: focus = Automations scopes the
        // lookup to `KeyContext::Automations`, resolving j/k to the pane actions.
        app.automation_ui.cached_automations = vec![make(1, "a"), make(2, "b")];
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.automation_ui.automation_panel_index, 1);
        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.automation_ui.automation_panel_index, 0);
        // j past the last automation loops out to the session list.
        app.automation_ui.automation_panel_index = 1;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::SessionList);
    }

    #[test]
    fn session_and_automation_navigation_forms_a_loop() {
        use crate::session::{AutomationAction, AutomationSchedule};
        let make = |id: i64, name: &str| Automation {
            id,
            name: name.into(),
            enabled: true,
            schedule: AutomationSchedule::Once { at: 0 },
            timezone: None,
            action: AutomationAction::Send {
                session_id: SessionId::default(),
            },
            prompt: "p".into(),
            created_at: 0,
            updated_at: 0,
            last_run_at: None,
            next_run_at: None,
        };
        let mut app = app_with_sessions(2);
        app.automation_ui.cached_automations = vec![make(1, "a"), make(2, "b")];

        // Down past the last session drops into the automations pane.
        app.focus = InputFocus::SessionList;
        app.active_index = 1; // last session in render order
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::Automations);
        assert_eq!(app.automation_ui.automation_panel_index, 0);

        // Down past the last automation loops to the TOP session.
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE); // a → b
        assert_eq!(app.automation_ui.automation_panel_index, 1);
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE); // past last → top session
        assert_eq!(app.focus, InputFocus::SessionList);
        assert_eq!(app.active_index, 0, "looped to first session");

        // Up from the first session loops to the LAST automation.
        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::Automations);
        assert_eq!(
            app.automation_ui.automation_panel_index, 1,
            "looped to last automation"
        );

        // And k at the top automation hands back up to the last session.
        app.automation_ui.automation_panel_index = 0;
        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::SessionList);
        assert_eq!(app.active_index, 1, "back to last session");
    }

    // --- Role editor tests ---

    #[test]
    fn ctrl_h_cycles_focus_backward_from_terminal() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::Terminal;
        // Backward from the terminal lands on the session list (the automations
        // pane is not a cycle stop — it's reached via j/k).
        app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::SessionList);
    }

    #[test]
    fn ctrl_h_cycles_focus_backward_from_session_list() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::SessionList;
        app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::Terminal);
    }

    #[test]
    fn ctrl_c_copies_selection_or_falls_through() {
        let mut app = app_with_sessions(1);
        let initial_count = app.sessions.len();
        // With no selection, Ctrl+C should NOT close session (it falls through to terminal)
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.sessions.len(), initial_count);
    }

    #[test]
    fn any_key_clears_text_selection() {
        let mut app = app_with_sessions(1);
        app.text_selection = Some(Selection::new(
            TermPos { row: 0, col: 0 },
            PaneBounds::from_rect(ratatui::layout::Rect::new(0, 0, 80, 24)),
        ));
        assert!(app.text_selection.is_some());

        // Any non-copy key should clear the selection
        app.handle_key(KeyCode::Char('a'), KeyModifiers::empty());
        assert!(app.text_selection.is_none());
    }

    #[test]
    fn ctrl_v_clears_selection() {
        let mut app = app_with_sessions(1);
        app.text_selection = Some(Selection::new(
            TermPos { row: 0, col: 0 },
            PaneBounds::from_rect(ratatui::layout::Rect::new(0, 0, 80, 24)),
        ));

        // Ctrl+V should clear selection (paste)
        app.handle_key(KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert!(app.text_selection.is_none());
    }

    // --- Cmd/Super chord routing (kitty keyboard protocol) ---

    #[test]
    fn super_chord_dispatches_bound_global_action() {
        let mut app = app_with_sessions(1);
        app.keybindings.rebind(
            crate::session::Action::ToggleFileViewer,
            crate::session::KeyChord::cmd('e'),
        );
        assert!(!app.show_file_viewer);
        app.handle_key(KeyCode::Char('e'), KeyModifiers::SUPER);
        assert!(app.show_file_viewer, "bound Cmd chord must dispatch");
    }

    #[test]
    fn super_chord_never_types_into_modal_text_input() {
        let mut app = app_with_sessions(1);
        app.modal = modals::Modal::SessionName(modals::SessionNameModal::default());
        app.handle_key(KeyCode::Char('j'), KeyModifiers::SUPER);
        let modals::Modal::SessionName(ref sn) = app.modal else {
            panic!("modal must stay open");
        };
        assert_eq!(
            sn.name.value(),
            "",
            "Cmd+J must not insert a bare 'j' into the text input"
        );
    }

    /// Inside a modal text input, readline `Ctrl+W` (delete word) and `Ctrl+U`
    /// (kill to line start) edit the text like a terminal — and never insert a
    /// literal `w`/`u`, nor fire the global `FocusTasks`/`OpenRestoreSessions`
    /// chords those keys carry.
    #[test]
    fn ctrl_w_and_ctrl_u_edit_modal_text_like_a_terminal() {
        let mut app = app_with_sessions(1);
        app.modal = modals::Modal::SessionName(modals::SessionNameModal::default());
        if let modals::Modal::SessionName(ref mut sn) = app.modal {
            sn.name.set("hello world");
        }

        // Ctrl+W deletes the word before the cursor.
        app.handle_key(KeyCode::Char('w'), KeyModifiers::CONTROL);
        let modals::Modal::SessionName(ref sn) = app.modal else {
            panic!("modal must stay open");
        };
        assert_eq!(sn.name.value(), "hello ");

        // Ctrl+U clears to the start of the line.
        app.handle_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
        let modals::Modal::SessionName(ref sn) = app.modal else {
            panic!("modal must stay open");
        };
        assert_eq!(sn.name.value(), "");
    }

    /// The same readline editing works in the other modal text inputs that
    /// share `apply_text_input_key` — here the worktree/branch-name field and
    /// the repo-picker fuzzy-search field.
    #[test]
    fn ctrl_w_edits_worktree_name_and_repo_search_fields() {
        let mut app = app_with_sessions(1);

        app.modal = modals::Modal::WorktreeName(modals::WorktreeNameModal::default());
        if let modals::Modal::WorktreeName(ref mut wn) = app.modal {
            wn.name.set("feature branch");
        }
        app.handle_key(KeyCode::Char('w'), KeyModifiers::CONTROL);
        let modals::Modal::WorktreeName(ref wn) = app.modal else {
            panic!("modal must stay open");
        };
        assert_eq!(wn.name.value(), "feature ");

        let mut rp = modals::RepoPickerModal {
            focus: modals::RepoPickerFocus::Search,
            ..Default::default()
        };
        rp.search_input.set("foo bar");
        app.modal = modals::Modal::RepoPicker(rp);
        app.handle_key(KeyCode::Char('w'), KeyModifiers::CONTROL);
        let modals::Modal::RepoPicker(ref rp) = app.modal else {
            panic!("modal must stay open");
        };
        assert_eq!(rp.search_input.value(), "foo ");
    }

    #[test]
    fn unbound_super_chord_skips_focus_letter_hotkeys() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::TaskList;
        // Cmd+N is unbound: it must be swallowed, not treated as the task
        // list's plain `n` (new task) hotkey.
        app.handle_key(KeyCode::Char('n'), KeyModifiers::SUPER);
        assert_eq!(app.focus, InputFocus::TaskList, "no editor must open");
    }

    #[test]
    fn scroll_clears_selection() {
        let mut app = app_with_sessions(1);
        app.text_selection = Some(Selection::new(
            TermPos { row: 0, col: 0 },
            PaneBounds::from_rect(ratatui::layout::Rect::new(0, 0, 80, 24)),
        ));

        app.scroll_terminal_up(1);
        assert!(app.text_selection.is_none());
    }

    #[test]
    fn ctrl_d_deletes_session_from_session_list() {
        let mut app = app_with_sessions(2);
        app.focus = InputFocus::SessionList;
        let initial_count = app.sessions.len();
        app.handle_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(app.sessions.len() < initial_count);
    }

    #[test]
    fn ctrl_d_from_session_list_deletes_session() {
        let mut app = app_with_sessions(2);
        app.focus = InputFocus::SessionList;
        let initial_count = app.sessions.len();
        app.handle_key(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(app.sessions.len() < initial_count);
    }

    #[test]
    fn ctrl_r_no_crash_without_sessions() {
        let mut app = app_with_sessions(0);
        // App::new may toast warnings from the developer's real keybindings
        // file; this test only cares that Ctrl+R itself stays silent.
        app.status_message = None;
        app.focus = InputFocus::Terminal;
        // Should not crash when there are no sessions
        app.handle_key(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(app.status_message.is_none());
    }

    #[test]
    fn f1_shows_help_from_any_context() {
        let mut app = app_with_sessions(0);
        for focus in [InputFocus::SessionList, InputFocus::Terminal] {
            app.modal = modals::Modal::None;
            app.focus = focus;
            app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
            assert!(
                matches!(app.modal, modals::Modal::Help(_)),
                "F1 should show help from {focus:?}"
            );
        }
    }

    #[test]
    fn f1_does_not_activate_during_modal() {
        let mut app = app_with_sessions(0);
        app.modal = modals::Modal::RepoPicker(modals::RepoPickerModal::default());
        app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
        assert!(!matches!(app.modal, modals::Modal::Help(_)));
    }

    #[test]
    fn help_modal_navigation_clamps() {
        let mut app = app_with_sessions(0);
        app.modal = modals::Modal::Help(modals::HelpModal::default());

        // `k` at the top stays at index 0.
        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        let modals::Modal::Help(ref h) = app.modal else {
            panic!("help modal closed unexpectedly");
        };
        assert_eq!(h.selected, 0);

        // `j` past the end clamps to the last rebindable action.
        let last = crate::session::Action::rebindable_in_order().len() - 1;
        for _ in 0..(last + 5) {
            app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        }
        let modals::Modal::Help(ref h) = app.modal else {
            panic!("help modal closed unexpectedly");
        };
        assert_eq!(h.selected, last);
    }

    #[test]
    fn help_capture_then_key_rebinds_and_clears_capturing() {
        let base = std::env::temp_dir().join("thurbox-help-rebind-test");
        let _ = std::fs::remove_dir_all(&base);
        let _g = crate::paths::TestPathGuard::new(&base);

        let mut app = app_with_sessions(0);
        app.handle_key(KeyCode::F(1), KeyModifiers::NONE); // open help (selected = 0)
        app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE); // begin capture
        app.handle_key(KeyCode::Char('a'), KeyModifiers::CONTROL); // bind ctrl+a (free)

        let action = crate::session::Action::rebindable_in_order()[0];
        assert_eq!(
            app.keybindings
                .lookup(KeyCode::Char('a'), KeyModifiers::CONTROL),
            Some(action)
        );
        let modals::Modal::Help(ref h) = app.modal else {
            panic!("help modal closed unexpectedly");
        };
        assert!(!h.capturing, "capture flag should clear after binding");
    }

    #[test]
    fn help_capture_esc_cancels_without_rebinding() {
        let mut app = app_with_sessions(0);
        app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
        app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE); // begin capture
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE); // cancel capture

        // Still in the help modal, no longer capturing, and nothing was bound.
        // Ctrl+A is the probe because it is unbound by default (Ctrl+X is now
        // ToggleReview's default chord).
        let modals::Modal::Help(ref h) = app.modal else {
            panic!("Esc during capture should not close the help modal");
        };
        assert!(!h.capturing);
        assert_eq!(
            app.keybindings
                .lookup(KeyCode::Char('a'), KeyModifiers::CONTROL),
            None
        );
    }

    #[test]
    fn help_capturing_ctrl_q_rebinds_not_quits() {
        let base = std::env::temp_dir().join("thurbox-help-ctrlq-test");
        let _ = std::fs::remove_dir_all(&base);
        let _g = crate::paths::TestPathGuard::new(&base);

        let mut app = app_with_sessions(0);
        app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
        app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE); // begin capture
        app.handle_key(KeyCode::Char('q'), KeyModifiers::CONTROL); // would normally quit

        assert!(!app.should_quit, "capturing ctrl+q must rebind, not quit");
        let action = crate::session::Action::rebindable_in_order()[0];
        assert_eq!(
            app.keybindings
                .lookup(KeyCode::Char('q'), KeyModifiers::CONTROL),
            Some(action)
        );
    }

    #[test]
    fn help_reset_d_restores_defaults() {
        let base = std::env::temp_dir().join("thurbox-help-reset-test");
        let _ = std::fs::remove_dir_all(&base);
        let _g = crate::paths::TestPathGuard::new(&base);

        let mut app = app_with_sessions(0);
        app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
        let action = crate::session::Action::rebindable_in_order()[0];

        // Rebind the selected action to ctrl+x...
        app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
        app.handle_key(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(
            app.keybindings
                .lookup(KeyCode::Char('x'), KeyModifiers::CONTROL),
            Some(action)
        );

        // ...then `d` restores its compiled-in defaults.
        app.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(
            app.keybindings.chords_for(action),
            action.default_chords().as_slice()
        );
    }

    #[test]
    fn help_reset_all_restores_every_default() {
        let base = std::env::temp_dir().join("thurbox-help-reset-all-test");
        let _ = std::fs::remove_dir_all(&base);
        let _g = crate::paths::TestPathGuard::new(&base);

        let mut app = app_with_sessions(0);
        app.handle_key(KeyCode::F(1), KeyModifiers::NONE);

        // Rebind two distinct actions away from their defaults.
        let actions = crate::session::Action::rebindable_in_order();
        app.keybindings
            .rebind(actions[0], crate::session::KeyChord::ctrl('x'));
        app.keybindings
            .rebind(actions[1], crate::session::KeyChord::ctrl('y'));

        // Shift+D resets everything.
        app.handle_key(KeyCode::Char('D'), KeyModifiers::SHIFT);

        for action in crate::session::Action::all() {
            assert_eq!(
                app.keybindings.chords_for(*action),
                action.default_chords().as_slice(),
                "{action:?} should be reset to defaults"
            );
        }
        // The override file is gone, so defaults stay authoritative.
        assert_eq!(
            crate::storage::keybindings::load_keybindings_json().unwrap(),
            None
        );
    }

    #[test]
    fn focus_key_context_maps_focus_to_scope() {
        use crate::session::KeyContext;
        let mut app = app_with_sessions(1);

        app.focus = InputFocus::SessionList;
        assert_eq!(app.focus_key_context(), KeyContext::SessionList);
        app.focus = InputFocus::Terminal;
        assert_eq!(app.focus_key_context(), KeyContext::Terminal);
        app.focus = InputFocus::FileViewer;
        assert_eq!(app.focus_key_context(), KeyContext::FileViewer);

        // While the file-viewer search field is active, fall back to Global so
        // typed letters edit the query instead of navigating the tree.
        app.file_viewer.search_active = true;
        assert_eq!(app.focus_key_context(), KeyContext::Global);
        app.file_viewer.search_active = false;

        // The automations and tasks panes are their own scoped contexts, so
        // single letters (j/k/n/r/d/…) resolve there without leaking to the PTY.
        app.focus = InputFocus::Automations;
        assert_eq!(app.focus_key_context(), KeyContext::Automations);
        app.focus = InputFocus::TaskList;
        assert_eq!(app.focus_key_context(), KeyContext::Tasks);

        // The in-pane editors / run-history are capture sub-modes handled before
        // the lookup, so they stay on Global.
        app.focus = InputFocus::AutomationEditor;
        assert_eq!(app.focus_key_context(), KeyContext::Global);
        app.focus = InputFocus::TaskEditor;
        assert_eq!(app.focus_key_context(), KeyContext::Global);
    }

    #[test]
    fn file_viewer_scoped_keys_route_through_keybindings() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::FileViewer;
        assert!(!app.file_viewer.search_active);

        // `/` is the rebindable FileViewerSearch action.
        app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(app.file_viewer.search_active, "'/' should start the search");

        // Now in search mode the context falls back to Global, so plain letters
        // edit the query (routed to the literal search handler) rather than
        // triggering FileViewerDown etc.
        app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
        assert_eq!(app.file_viewer.search_query, "ab");
    }

    #[test]
    fn file_viewer_search_action_is_rebindable() {
        let base = std::env::temp_dir().join("thurbox-fv-rebind-test");
        let _ = std::fs::remove_dir_all(&base);
        let _g = crate::paths::TestPathGuard::new(&base);

        let mut app = app_with_sessions(1);
        // Rebind FileViewerSearch from `/` to `s`.
        app.keybindings.rebind(
            crate::session::Action::FileViewerSearch,
            crate::session::KeyChord::plain('s'),
        );
        app.focus = InputFocus::FileViewer;

        // `/` no longer opens search...
        app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(!app.file_viewer.search_active);
        // ...the new `s` chord does.
        app.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.file_viewer.search_active);
    }

    #[test]
    fn f2_toggles_info_panel() {
        let mut app = app_with_sessions(0);
        assert!(!app.show_info_panel);
        app.handle_key(KeyCode::F(2), KeyModifiers::NONE);
        assert!(app.show_info_panel);
        app.handle_key(KeyCode::F(2), KeyModifiers::NONE);
        assert!(!app.show_info_panel);
    }

    #[test]
    fn f5_toggles_tasks_panel() {
        let mut app = app_with_sessions(1);
        app.update(AppMessage::Resize(160, 40));
        assert!(!app.show_tasks_panel);
        // F5 shows + focuses the tasks panel (like F3 for the file viewer).
        app.handle_key(KeyCode::F(5), KeyModifiers::NONE);
        assert!(app.show_tasks_panel);
        assert_eq!(app.focus, InputFocus::TaskList);
        // F5 again hides it and drops focus back to the session list.
        app.handle_key(KeyCode::F(5), KeyModifiers::NONE);
        assert!(!app.show_tasks_panel);
        assert_eq!(app.focus, InputFocus::SessionList);
    }

    /// The theme picker's own opener chord (`F4`/`Ctrl+Y`) closes it when it's
    /// already open, restoring the live-preview original like `Esc`.
    #[test]
    fn f4_toggles_theme_picker_closed() {
        let mut app = app_with_sessions(1);
        app.handle_key(KeyCode::F(4), KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::ThemePicker(_)));
        // Re-pressing the opener dismisses it instead of being swallowed.
        app.handle_key(KeyCode::F(4), KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::None));
    }

    /// The Settings panel's own opener chord (`F6`/`Ctrl+,`) closes it when it's
    /// already open (discarding the draft, like `Esc`).
    #[test]
    fn f6_toggles_settings_closed() {
        let mut app = app_with_sessions(1);
        app.handle_key(KeyCode::F(6), KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::Settings(_)));
        app.handle_key(KeyCode::F(6), KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::None));
    }

    /// When the terminal is focused, readline/shell `Ctrl+<letter>` chords
    /// (here `Ctrl+W` = delete-word) defer to the PTY instead of running their
    /// thurbox command — but the same chord still works from the session list,
    /// and the `F`-key alternate works everywhere.
    #[test]
    fn terminal_focus_defers_readline_ctrl_chords_to_pty() {
        let mut app = app_with_sessions(1);
        app.update(AppMessage::Resize(160, 40));

        // From the session list, Ctrl+W (FocusTasks) toggles the tasks panel.
        app.focus = InputFocus::SessionList;
        app.handle_key(KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(app.show_tasks_panel, "Ctrl+W toggles tasks from the list");

        // Reset, then focus the terminal: Ctrl+W now forwards to the PTY, so
        // the tasks panel is left untouched.
        app.show_tasks_panel = false;
        app.focus = InputFocus::Terminal;
        app.handle_key(KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(
            !app.show_tasks_panel,
            "Ctrl+W should defer to the PTY when the terminal is focused"
        );
        assert_eq!(app.focus, InputFocus::Terminal);

        // The F5 alternate is not a Ctrl+letter chord, so it still toggles.
        app.handle_key(KeyCode::F(5), KeyModifiers::NONE);
        assert!(
            app.show_tasks_panel,
            "F5 keeps toggling tasks even in the terminal"
        );
    }

    /// Navigation chords are the keyboard escape route, so they keep working in
    /// the terminal even though `Ctrl+H` collides with readline's backspace.
    #[test]
    fn terminal_focus_keeps_navigation_chords_active() {
        let mut app = app_with_sessions(1);
        app.update(AppMessage::Resize(160, 40));
        app.focus = InputFocus::Terminal;

        app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_ne!(
            app.focus,
            InputFocus::Terminal,
            "Ctrl+H (FocusBackward) must still leave the terminal"
        );
    }

    /// Every `[features]` flag blocks its keybinding with a toast: state stays
    /// untouched and the chord is consumed (never forwarded to the PTY).
    #[test]
    fn disabled_features_block_actions_with_a_toast() {
        let mut app = app_with_sessions(1);
        app.update(AppMessage::Resize(160, 40));
        app.features = crate::session::settings::FeatureFlags {
            tasks: false,
            automations: false,
            file_viewer: false,
            global_search: false,
            info_panel: false,
            shell_pane: false,
            code_review: false,
            mouse: true,
            notifications: false,
            soft_delete: true,
            version_check: false,
            auto_update: false,
        };

        app.handle_key(KeyCode::F(5), KeyModifiers::NONE);
        assert!(!app.show_tasks_panel);
        assert_eq!(app.focus, InputFocus::SessionList);
        assert!(app.status_message.take().unwrap().text.contains("disabled"));

        app.handle_key(KeyCode::F(3), KeyModifiers::NONE);
        assert!(!app.show_file_viewer);
        assert!(app.status_message.take().unwrap().text.contains("disabled"));

        app.handle_key(KeyCode::F(2), KeyModifiers::NONE);
        assert!(!app.show_info_panel);
        assert!(app.status_message.take().unwrap().text.contains("disabled"));

        app.handle_key(KeyCode::Char('/'), KeyModifiers::CONTROL);
        assert!(!app.global_search.active);
        assert!(app.status_message.take().unwrap().text.contains("disabled"));

        app.handle_key(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert!(matches!(app.modal, modals::Modal::None));
        assert_eq!(app.focus, InputFocus::SessionList);
        assert!(app.status_message.take().unwrap().text.contains("disabled"));

        app.handle_key(KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert_eq!(app.active_terminal_view(), TerminalView::Claude);
        assert!(app.status_message.take().unwrap().text.contains("disabled"));

        // F7 (ToggleReview) is gated by the code_review flag.
        app.handle_key(KeyCode::F(7), KeyModifiers::NONE);
        assert!(app.active_review().is_none());
        assert_ne!(app.focus, InputFocus::CodeReview);
        assert!(app.status_message.take().unwrap().text.contains("disabled"));
    }

    /// The automations flag flows through `screen_layout` (the shared layout
    /// funnel): disabling it removes the pane and gives the session list the
    /// whole left column.
    #[test]
    fn screen_layout_drops_automations_pane_when_disabled() {
        let mut app = app_with_sessions(1);
        app.update(AppMessage::Resize(100, 30));
        let with = app.screen_layout();
        assert!(with.automations_panel.is_some());

        app.features.automations = false;
        let without = app.screen_layout();
        assert!(without.automations_panel.is_none());
        assert_eq!(
            without.left_panel.unwrap().height,
            with.left_panel.unwrap().height + with.automations_panel.unwrap().height,
            "session list absorbs the pane's rows"
        );
    }

    /// With automations disabled there is no pane beneath the session list, so
    /// `j`/`k` wrap within the list instead of flowing into the pane.
    #[test]
    fn session_list_wraps_when_automations_disabled() {
        let mut app = app_with_sessions(2);
        app.features.automations = false;
        app.focus = InputFocus::SessionList;
        app.active_index = 1; // last session in render order

        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::SessionList, "no pane to flow into");
        assert_eq!(app.active_index, 0, "j past the last wraps to the first");

        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::SessionList);
        assert_eq!(app.active_index, 1, "k above the first wraps to the last");
    }

    #[test]
    fn opening_tasks_panel_populates_central_preview() {
        // Focusing the tasks panel (F5/Ctrl+W) must build the central-pane
        // preview for the selected task, not leave the empty hint showing.
        let mut app = app_with_sessions(1);
        app.update(AppMessage::Resize(160, 40));
        app.db
            .create_task(&crate::storage::tasks::NewTask::local("only task"))
            .unwrap();
        app.refresh_tasks();

        app.handle_key(KeyCode::F(5), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::TaskList);
        let editor = app
            .task_ui
            .task_editor
            .as_ref()
            .expect("the central pane must mirror the selected task");
        assert_eq!(editor.title.value(), "only task");
    }

    fn session_parser_size(app: &App, index: usize) -> (u16, u16) {
        let parser = app.sessions[index].parser.lock().unwrap();
        parser.screen().size()
    }

    #[test]
    fn f3_toggle_resizes_session_parser() {
        let mut app = app_with_sessions(1);
        app.update(AppMessage::Resize(160, 40));
        let before = session_parser_size(&app, 0);

        app.handle_key(KeyCode::F(3), KeyModifiers::NONE);
        let after_open = session_parser_size(&app, 0);
        assert!(app.show_file_viewer);
        assert!(
            after_open.1 < before.1,
            "terminal width must shrink when file viewer opens: before={before:?}, after={after_open:?}",
        );

        app.handle_key(KeyCode::F(3), KeyModifiers::NONE);
        let after_close = session_parser_size(&app, 0);
        assert!(!app.show_file_viewer);
        assert_eq!(
            after_close, before,
            "terminal size must return to original after file viewer closes",
        );
    }

    #[test]
    fn f2_toggle_resizes_session_parser() {
        let mut app = app_with_sessions(1);
        app.update(AppMessage::Resize(160, 40));
        let before = session_parser_size(&app, 0);

        app.handle_key(KeyCode::F(2), KeyModifiers::NONE);
        let after = session_parser_size(&app, 0);
        assert!(app.show_info_panel);
        assert!(
            after.1 < before.1,
            "terminal width must shrink when info panel opens: before={before:?}, after={after:?}",
        );
    }

    #[test]
    fn ctrl_l_cycles_focus() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::SessionList;
        // SessionList → Terminal → SessionList (no file viewer). The automations
        // pane is not a cycle stop — it's reached via j/k from the list.
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::Terminal);
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::SessionList);
    }

    #[test]
    fn ctrl_l_includes_tasks_panel_when_visible() {
        let mut app = app_with_sessions(1);
        // With the tasks panel showing, the cycle is
        // SessionList → Terminal → TaskList → SessionList (no file viewer).
        app.show_tasks_panel = true;
        app.focus = InputFocus::SessionList;
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::Terminal);
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::TaskList);
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::SessionList);
        // Ctrl+H from the tasks panel steps back to the terminal.
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL); // → Terminal
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL); // → TaskList
        assert_eq!(app.focus, InputFocus::TaskList);
        app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::Terminal);
    }

    #[test]
    fn ctrl_l_skips_tasks_panel_when_hidden() {
        let mut app = app_with_sessions(1);
        // Panel off → TaskList is not a cycle stop.
        app.show_tasks_panel = false;
        app.focus = InputFocus::Terminal;
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::SessionList);
    }

    // --- In-pane task editing workflow ---

    #[test]
    fn task_n_opens_central_pane_editor() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::TaskList;
        app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        // n starts a new task in the central-pane editor (not a modal).
        assert_eq!(app.focus, InputFocus::TaskEditor);
        assert!(app.task_ui.task_editor.is_some());
        assert!(matches!(app.modal, modals::Modal::None));
    }

    #[test]
    fn task_enter_edits_existing_in_pane_and_esc_returns() {
        let mut app = app_with_sessions(1);
        app.db
            .create_task(&crate::storage::tasks::NewTask::local("fix bug"))
            .unwrap();
        app.refresh_tasks();
        app.focus = InputFocus::TaskList;
        app.task_ui.task_panel_index = 0;
        // Enter opens the editor in the central pane for the selected task.
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::TaskEditor);
        assert!(app
            .task_ui
            .task_editor
            .as_ref()
            .unwrap()
            .editing_id
            .is_some());
        // Esc discards and returns to the tasks panel.
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::TaskList);
    }

    #[test]
    fn task_editor_save_persists_and_returns_to_panel() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::TaskList;
        // New task → type a title → Enter saves.
        app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        for c in "ship it".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::TaskList);
        // The task was persisted.
        let tasks = app.db.list_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "ship it");
    }

    #[test]
    fn task_editor_save_creates_with_no_action_and_preserves_on_edit() {
        let mut app = app_with_sessions(1);
        // Create via the editor → action is None (the editor no longer authors it).
        app.focus = InputFocus::TaskList;
        app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        for c in "do thing".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let id = app.db.list_tasks().unwrap()[0].id;
        assert!(app.db.get_task(id).unwrap().unwrap().action.is_none());

        // Give it an action out-of-band (as the CLI would), then edit the title
        // through the editor: the action must survive.
        let mut t = app.db.get_task(id).unwrap().unwrap();
        t.action = Some(AutomationAction::Send {
            session_id: app.sessions[0].info.id,
        });
        app.db.update_task(&t).unwrap();
        app.refresh_tasks();

        app.task_ui.task_panel_index = 0;
        app.enter_task_editor();
        // Append to the title and save.
        app.handle_key(KeyCode::End, KeyModifiers::NONE);
        app.handle_key(KeyCode::Char('!'), KeyModifiers::NONE);
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        let saved = app.db.get_task(id).unwrap().unwrap();
        assert_eq!(saved.title, "do thing!");
        assert!(
            matches!(saved.action, Some(AutomationAction::Send { .. })),
            "edit must preserve the out-of-band action"
        );
    }

    #[test]
    fn task_action_picker_lists_send_per_session_plus_spawn() {
        let mut app = app_with_sessions(2);
        let id = app
            .db
            .create_task(&crate::storage::tasks::NewTask::local("t"))
            .unwrap();
        app.refresh_tasks();
        let task = app.db.get_task(id).unwrap().unwrap();
        app.open_task_action_picker(&task);
        let modals::Modal::TaskActionPicker(ref p) = app.modal else {
            panic!("expected the action picker");
        };
        // Two running sessions → two Send entries + a trailing SpawnNew.
        assert_eq!(p.choices.len(), 3);
        assert!(matches!(p.choices[2], modals::TaskActionChoice::SpawnNew));
        assert!(
            p.choices
                .iter()
                .filter(|c| matches!(c, modals::TaskActionChoice::Send(..)))
                .count()
                == 2
        );
    }

    #[test]
    fn task_r_key_opens_picker_then_enter_sends_and_closes() {
        // End-to-end through the key handlers: `r` opens the picker, `Enter`
        // runs the highlighted Send choice, closing the modal and advancing the
        // task. (One session → first choice is Send.)
        let mut app = app_with_sessions(1);
        let id = app
            .db
            .create_task(&crate::storage::tasks::NewTask::local("t"))
            .unwrap();
        app.refresh_tasks();
        app.focus = InputFocus::TaskList;
        app.task_ui.task_panel_index = 0;

        app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(
            matches!(app.modal, modals::Modal::TaskActionPicker(_)),
            "r opens the action picker"
        );

        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::None), "Enter closes it");
        assert_eq!(
            app.db.get_task(id).unwrap().unwrap().status,
            crate::session::TaskStatus::InProgress
        );
    }

    #[test]
    fn task_action_picker_esc_closes_without_running() {
        let mut app = app_with_sessions(1);
        let id = app
            .db
            .create_task(&crate::storage::tasks::NewTask::local("t"))
            .unwrap();
        app.refresh_tasks();
        let task = app.db.get_task(id).unwrap().unwrap();
        app.open_task_action_picker(&task);
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::None));
        // Status is untouched.
        assert_eq!(
            app.db.get_task(id).unwrap().unwrap().status,
            crate::session::TaskStatus::Todo
        );
    }

    #[test]
    fn send_task_to_session_advances_to_in_progress() {
        let mut app = app_with_sessions(1);
        let id = app
            .db
            .create_task(&crate::storage::tasks::NewTask::local("t"))
            .unwrap();
        app.refresh_tasks();
        let sid = app.sessions[0].info.id;
        app.send_task_to_session(id, "t", crate::session::TaskStatus::Todo, sid);
        assert_eq!(
            app.db.get_task(id).unwrap().unwrap().status,
            crate::session::TaskStatus::InProgress
        );
    }

    #[test]
    fn task_related_sessions_match_spawn_name_and_send_target() {
        let mut app = app_with_sessions(2);
        let id = app
            .db
            .create_task(&crate::storage::tasks::NewTask::local("t"))
            .unwrap();
        app.refresh_tasks();

        // No related session yet (generic stub names).
        let task = app.db.get_task(id).unwrap().unwrap();
        assert!(app.task_related_session_indices(&task).is_empty());

        // Rename session 1 to the spawn convention → it's related. Both the
        // current slugged form and the legacy bare `task-<id>` must match.
        app.sessions[1].info.name = task.spawn_session_name();
        assert_eq!(app.task_related_session_indices(&task), vec![1]);
        app.sessions[1].info.name = format!("task-{id}");
        assert_eq!(app.task_related_session_indices(&task), vec![1]);
    }

    #[test]
    fn task_related_sessions_honor_in_memory_link() {
        // A TUI spawn names the session by the user's choice (not `task-<id>`),
        // so the relation is recovered via `task_session_links`.
        let mut app = app_with_sessions(2);
        let id = app
            .db
            .create_task(&crate::storage::tasks::NewTask::local("t"))
            .unwrap();
        app.refresh_tasks();
        let task = app.db.get_task(id).unwrap().unwrap();

        // Session 0 keeps its generic stub name — no name/action match.
        assert!(app.task_related_session_indices(&task).is_empty());

        // Record the link the spawn tail would set, then it resolves.
        let sid = app.sessions[0].info.id;
        app.task_ui.task_session_links.insert(id, sid);
        assert_eq!(app.task_related_session_indices(&task), vec![0]);
    }

    #[test]
    fn open_task_related_session_focuses_terminal() {
        let mut app = app_with_sessions(2);
        let id = app
            .db
            .create_task(&crate::storage::tasks::NewTask::local("t"))
            .unwrap();
        app.sessions[1].info.name = format!("task-{id}");
        app.refresh_tasks();
        app.focus = InputFocus::TaskList;
        app.task_ui.task_panel_index = 0;

        app.handle_key(KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(app.active_index, 1, "jumps to the related session");
        assert_eq!(app.focus, InputFocus::Terminal);
    }

    #[test]
    fn open_task_related_session_no_session_keeps_focus() {
        let mut app = app_with_sessions(1);
        let _id = app
            .db
            .create_task(&crate::storage::tasks::NewTask::local("t"))
            .unwrap();
        app.refresh_tasks();
        app.focus = InputFocus::TaskList;
        app.task_ui.task_panel_index = 0;

        app.handle_key(KeyCode::Char('o'), KeyModifiers::NONE);
        // Nothing related is open → stay in the panel.
        assert_eq!(app.focus, InputFocus::TaskList);
    }

    #[test]
    fn scroll_task_preview_clamps_to_content() {
        let mut app = app_with_sessions(1);
        let id = app
            .db
            .create_task(&crate::storage::tasks::NewTask {
                description: Some("a\nb\nc".into()),
                ..crate::storage::tasks::NewTask::local("t")
            })
            .unwrap();
        let _ = id;
        app.refresh_tasks();
        app.focus = InputFocus::TaskList;
        app.task_ui.task_panel_index = 0;
        // Over-scroll up is clamped to 0.
        app.scroll_task_preview(-5);
        assert_eq!(app.task_ui.task_preview_scroll, 0);
        // Over-scroll down is clamped to the rendered line count.
        app.scroll_task_preview(1000);
        assert!(app.task_ui.task_preview_scroll <= 3);
    }

    #[test]
    fn apply_scrollbar_position_task_preview_clamps() {
        let mut app = app_with_sessions(1);
        app.db
            .create_task(&crate::storage::tasks::NewTask {
                description: Some("a\nb\nc".into()),
                ..crate::storage::tasks::NewTask::local("t")
            })
            .unwrap();
        app.refresh_tasks();
        app.focus = InputFocus::TaskList;
        app.task_ui.task_panel_index = 0;

        let max = app.task_preview_max_scroll();
        // A position past the end clamps to the max.
        app.apply_scrollbar_position(ScrollTarget::TaskPreview, 1000, 1000);
        assert_eq!(app.task_ui.task_preview_scroll, max);
        // Position 0 returns to the top.
        app.apply_scrollbar_position(ScrollTarget::TaskPreview, 0, 1000);
        assert_eq!(app.task_ui.task_preview_scroll, 0);
    }

    #[test]
    fn apply_scrollbar_position_terminal_inverts() {
        let mut app = app_with_sessions(1);
        // The stub parser keeps zero scrollback; swap in one that retains it.
        app.sessions[0].parser = Arc::new(std::sync::Mutex::new(
            vt100::Parser::new_with_callbacks(24, 80, 100, crate::agent::TermSignals::default()),
        ));
        // Seed the active session's parser with scrollback content.
        app.with_active_parser(|p| {
            for i in 0..50 {
                p.process(format!("line {i}\r\n").as_bytes());
            }
        });
        // Probe the total scrollback (the scrollbar's `content_len`).
        let mut total = 0usize;
        app.with_active_parser(|p| {
            let saved = p.screen().scrollback();
            p.screen_mut().set_scrollback(usize::MAX);
            total = p.screen().scrollback();
            p.screen_mut().set_scrollback(saved);
        });
        assert!(total > 0, "expected scrollback content");

        // Thumb at the top (pos 0) → fully scrolled up (scrollback == total).
        app.apply_scrollbar_position(ScrollTarget::Terminal, 0, total);
        let mut at_top = 0usize;
        app.with_active_parser(|p| at_top = p.screen().scrollback());
        assert_eq!(at_top, total);

        // Thumb at the bottom (pos == total) → back to the live tail (0).
        app.apply_scrollbar_position(ScrollTarget::Terminal, total, total);
        let mut at_bottom = 1usize;
        app.with_active_parser(|p| at_bottom = p.screen().scrollback());
        assert_eq!(at_bottom, 0);
    }

    #[test]
    fn scrollbar_click_starts_drag_not_selection() {
        let mut app = app_with_sessions(1);
        // Record a scrollbar track at a known location (as `view()` would).
        let track = Rect::new(40, 5, 1, 10);
        app.scrollbar_hits.push(ScrollbarHit {
            geom: ScrollbarGeom {
                track,
                content_len: 100,
                viewport: 10,
            },
            target: ScrollTarget::Terminal,
        });

        // A click on the track grabs the thumb — no text selection starts.
        app.handle_mouse_click(40, 7, KeyModifiers::NONE);
        assert_eq!(app.dragging_scrollbar, Some(ScrollTarget::Terminal));
        assert!(app.text_selection.is_none());

        // Mouse-up ends the drag.
        app.handle_mouse_up(40, 7);
        assert!(app.dragging_scrollbar.is_none());
    }

    #[test]
    fn pane_at_central_pane_follows_focus() {
        let mut app = app_with_sessions(1);
        // Pick a point guaranteed to be inside the central (terminal) pane.
        let areas = app.screen_layout();
        let cx = areas.terminal.x + areas.terminal.width / 2;
        let cy = areas.terminal.y + areas.terminal.height / 2;

        // The same central-pane point routes the wheel by focus.
        app.focus = InputFocus::Terminal;
        assert_eq!(app.pane_at(cx, cy), Some(ScrollPane::Terminal));

        app.focus = InputFocus::TaskList;
        assert_eq!(app.pane_at(cx, cy), Some(ScrollPane::TaskPreview));

        app.focus = InputFocus::AutomationRunHistory;
        assert_eq!(app.pane_at(cx, cy), Some(ScrollPane::RunHistory));
    }

    #[test]
    fn click_outside_scrollbar_starts_selection() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::Terminal;
        app.scrollbar_hits.push(ScrollbarHit {
            geom: ScrollbarGeom {
                track: Rect::new(118, 1, 1, 20),
                content_len: 100,
                viewport: 10,
            },
            target: ScrollTarget::Terminal,
        });

        // A click well away from the track falls through to text selection.
        app.handle_mouse_click(10, 10, KeyModifiers::NONE);
        assert!(app.dragging_scrollbar.is_none());
        assert!(app.text_selection.is_some());
    }

    // --- Mouse click targets (click-to-select/focus + modal rows) ---

    #[test]
    fn click_session_row_selects_and_focuses_list() {
        let mut app = app_with_sessions(3);
        app.focus = InputFocus::Terminal;
        // As recorded by view(): a row hitbox inside the left panel.
        app.click_targets.push(ClickTarget {
            rect: Rect::new(1, 3, 20, 1),
            action: ClickAction::SelectSession(2),
        });

        app.handle_mouse_click(5, 3, KeyModifiers::NONE);

        let order = app.render_order_indices();
        assert_eq!(app.active_index, order[2]);
        assert_eq!(app.focus, InputFocus::SessionList);
        // The same press still arms drag-select inside the left panel.
        assert!(app.text_selection.is_some());
    }

    #[test]
    fn click_terminal_pane_focuses_terminal_and_arms_selection() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::SessionList;
        let areas = app.screen_layout();
        app.click_targets.push(ClickTarget {
            rect: areas.terminal,
            action: ClickAction::FocusPane(InputFocus::Terminal),
        });
        let cx = areas.terminal.x + areas.terminal.width / 2;
        let cy = areas.terminal.y + areas.terminal.height / 2;

        app.handle_mouse_click(cx, cy, KeyModifiers::NONE);

        assert_eq!(app.focus, InputFocus::Terminal);
        assert!(app.text_selection.is_some());
    }

    #[test]
    fn click_with_modal_open_is_swallowed() {
        let mut app = app_with_sessions(1);
        app.modal = modals::Modal::ThemePicker(modals::ThemePickerModal {
            index: 0,
            original: crate::ui::theme::current(),
        });
        // A scrollbar track and a pane target beneath the overlay must both
        // be unreachable while the modal is open.
        app.scrollbar_hits.push(ScrollbarHit {
            geom: ScrollbarGeom {
                track: Rect::new(40, 5, 1, 10),
                content_len: 100,
                viewport: 10,
            },
            target: ScrollTarget::Terminal,
        });
        app.click_targets.push(ClickTarget {
            rect: Rect::new(0, 0, 120, 24),
            action: ClickAction::FocusPane(InputFocus::Terminal),
        });

        app.handle_mouse_click(40, 7, KeyModifiers::NONE);

        assert!(app.dragging_scrollbar.is_none());
        assert!(app.text_selection.is_none());
        assert!(matches!(app.modal, modals::Modal::ThemePicker(_)));
    }

    #[test]
    fn click_theme_picker_row_confirms_theme() {
        let mut app = app_with_sessions(1);
        app.modal = modals::Modal::ThemePicker(modals::ThemePickerModal {
            index: 2,
            original: crate::ui::theme::current(),
        });
        // As in a real frame: the pane targets beneath the overlay are
        // recorded first and overlap the modal — the modal row must still
        // win while a modal is open.
        app.click_targets.push(ClickTarget {
            rect: Rect::new(0, 0, 120, 24),
            action: ClickAction::FocusPane(InputFocus::Terminal),
        });
        app.click_targets.push(ClickTarget {
            rect: Rect::new(30, 8, 20, 1),
            action: ClickAction::ModalRow(0),
        });

        // Single click selects the row and confirms it (Enter-equivalent).
        app.handle_mouse_click(35, 8, KeyModifiers::NONE);

        assert!(matches!(app.modal, modals::Modal::None));
        assert_eq!(
            app.active_theme.name,
            crate::ui::theme::all_theme_entries()[0].name
        );
    }

    #[test]
    fn click_repo_picker_row_toggles_not_confirms() {
        let mut app = app_with_sessions(0);
        app.start_new_session(); // no hosts → opens the repo picker
        let modals::Modal::RepoPicker(ref mut rp) = app.modal else {
            panic!("expected repo picker");
        };
        // Seed two plain bookmarks (no headers/children).
        rp.bookmarks = vec!["/tmp/a".into(), "/tmp/b".into()];
        rp.selected = vec![false, false];
        rp.worktree = vec![false, false];
        rp.is_header = vec![false, false];
        rp.is_child = vec![false, false];
        rp.filtered_indices = vec![0, 1];
        app.click_targets.push(ClickTarget {
            rect: Rect::new(30, 9, 40, 1),
            action: ClickAction::ModalRow(1),
        });

        app.handle_mouse_click(35, 9, KeyModifiers::NONE);

        // The click toggled the row's checkbox (Space), not Enter: the
        // modal stays open and nothing was spawned.
        let modals::Modal::RepoPicker(ref rp) = app.modal else {
            panic!("repo picker must stay open after a row click");
        };
        assert_eq!(rp.list_index, 1);
        assert!(rp.selected[1]);
    }

    #[test]
    fn help_capture_ignores_clicks() {
        let mut app = app_with_sessions(1);
        app.modal = modals::Modal::Help(modals::HelpModal {
            selected: 0,
            capturing: true,
        });
        app.click_targets.push(ClickTarget {
            rect: Rect::new(30, 8, 20, 1),
            action: ClickAction::ModalRow(3),
        });

        app.handle_mouse_click(35, 8, KeyModifiers::NONE);

        // Still capturing, selection untouched.
        let modals::Modal::Help(ref h) = app.modal else {
            panic!("help must stay open");
        };
        assert!(h.capturing);
        assert_eq!(h.selected, 0);
    }

    #[test]
    fn click_while_global_search_open_is_swallowed() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::SessionList;
        app.handle_key(KeyCode::Char('/'), KeyModifiers::CONTROL);
        assert!(app.global_search.active);
        app.click_targets.push(ClickTarget {
            rect: Rect::new(1, 3, 20, 1),
            action: ClickAction::SelectSession(0),
        });

        app.handle_mouse_click(5, 3, KeyModifiers::NONE);

        // The strip keeps focus; no selection armed, no target activated.
        assert!(app.global_search.active);
        assert_eq!(app.focus, InputFocus::GlobalSearch);
        assert!(app.text_selection.is_none());
    }

    #[test]
    fn view_records_session_row_click_targets() {
        let mut app = app_with_sessions(2);
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.view(f)).unwrap();

        let rows: Vec<Rect> = app
            .click_targets
            .iter()
            .filter_map(|t| match t.action {
                ClickAction::SelectSession(_) => Some(t.rect),
                _ => None,
            })
            .collect();
        assert_eq!(rows.len(), 2, "one hitbox per rendered session row");

        // Click the second rendered row end-to-end through the registry.
        let target = app
            .click_targets
            .iter()
            .find(|t| t.action == ClickAction::SelectSession(1))
            .map(|t| t.rect)
            .unwrap();
        app.handle_mouse_click(target.x, target.y, KeyModifiers::NONE);
        let order = app.render_order_indices();
        assert_eq!(app.active_index, order[1]);
        assert_eq!(app.focus, InputFocus::SessionList);
    }

    #[test]
    fn view_drawn_modal_row_click_confirms() {
        // End-to-end through a real frame: the registry holds the pane
        // targets *and* the theme-picker rows; clicking a rendered row must
        // reach the modal, not the pane beneath it.
        let mut app = app_with_sessions(1);
        app.modal = modals::Modal::ThemePicker(modals::ThemePickerModal {
            index: 1,
            original: crate::ui::theme::current(),
        });
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.view(f)).unwrap();

        let row = app
            .click_targets
            .iter()
            .find(|t| t.action == ClickAction::ModalRow(0))
            .map(|t| t.rect)
            .expect("theme picker rows must be recorded");
        app.handle_mouse_click(row.x + 1, row.y, KeyModifiers::NONE);

        assert!(matches!(app.modal, modals::Modal::None));
        assert_eq!(
            app.active_theme.name,
            crate::ui::theme::all_theme_entries()[0].name
        );
    }

    #[test]
    fn mouse_move_updates_hover() {
        let mut app = app_with_sessions(1);
        app.update(AppMessage::MouseMove { x: 7, y: 9 });
        assert_eq!(app.mouse_hover, Some((7, 9)));
    }

    /// Render a frame so `click_targets` are recorded, then return the center of
    /// the first target whose action matches `pred`.
    fn rendered_target(app: &mut App, pred: impl Fn(&ClickAction) -> bool) -> Rect {
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.view(f)).unwrap();
        app.click_targets
            .iter()
            .find(|t| pred(&t.action))
            .map(|t| t.rect)
            .expect("matching click target recorded this frame")
    }

    /// Each footer button dispatches its global action when clicked.
    #[test]
    fn footer_help_button_click_opens_help() {
        let mut app = app_with_sessions(1);
        let r = rendered_target(&mut app, |a| {
            *a == ClickAction::Global(crate::session::Action::ToggleHelp)
        });
        app.handle_mouse_click(r.x, r.y, KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::Help(_)));
    }

    #[test]
    fn footer_settings_button_click_opens_settings() {
        let mut app = app_with_sessions(1);
        let r = rendered_target(&mut app, |a| {
            *a == ClickAction::Global(crate::session::Action::OpenSettings)
        });
        app.handle_mouse_click(r.x, r.y, KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::Settings(_)));
    }

    #[test]
    fn footer_theme_button_click_opens_theme_picker() {
        let mut app = app_with_sessions(1);
        let r = rendered_target(&mut app, |a| {
            *a == ClickAction::Global(crate::session::Action::OpenThemePicker)
        });
        app.handle_mouse_click(r.x, r.y, KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::ThemePicker(_)));
    }

    #[test]
    fn footer_quit_button_click_quits() {
        let mut app = app_with_sessions(1);
        let r = rendered_target(&mut app, |a| {
            *a == ClickAction::Global(crate::session::Action::QuitApp)
        });
        app.handle_mouse_click(r.x, r.y, KeyModifiers::NONE);
        assert!(app.should_quit);
    }

    /// A footer button click is swallowed while a modal is open (the modal owns
    /// every click), so it can't quit/navigate from underneath the overlay.
    #[test]
    fn footer_button_swallowed_while_modal_open() {
        let mut app = app_with_sessions(1);
        app.modal = modals::Modal::ThemePicker(modals::ThemePickerModal {
            index: 0,
            original: crate::ui::theme::current(),
        });
        // The footer still renders its buttons beneath the overlay.
        let r = rendered_target(&mut app, |a| {
            *a == ClickAction::Global(crate::session::Action::QuitApp)
        });
        app.handle_mouse_click(r.x, r.y, KeyModifiers::NONE);
        assert!(!app.should_quit, "modal must swallow the footer click");
        assert!(matches!(app.modal, modals::Modal::ThemePicker(_)));
    }

    /// The Settings modal's `[ Cancel ]` button closes it (Esc-equivalent).
    #[test]
    fn modal_cancel_button_closes() {
        let mut app = app_with_sessions(1);
        app.open_settings_panel();
        let r = rendered_target(&mut app, |a| {
            matches!(
                a,
                ClickAction::ModalButton {
                    code: KeyCode::Esc,
                    ..
                }
            )
        });
        app.handle_mouse_click(r.x, r.y, KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::None));
    }

    /// The Settings modal's `[ Save ]` button persists + closes (Ctrl+S).
    #[test]
    fn modal_save_button_saves_and_closes() {
        let mut app = app_with_sessions(1);
        app.open_settings_panel();
        let r = rendered_target(&mut app, |a| {
            matches!(
                a,
                ClickAction::ModalButton {
                    code: KeyCode::Char('s'),
                    mods: KeyModifiers::CONTROL,
                }
            )
        });
        app.handle_mouse_click(r.x, r.y, KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::None));
    }

    /// Render a frame, then return the (rect, index) of the first click target
    /// whose action matches `pred` (mapping the action to its field index).
    fn rendered_indexed_target(
        app: &mut App,
        pred: impl Fn(&ClickAction) -> Option<usize>,
    ) -> (Rect, usize) {
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.view(f)).unwrap();
        app.click_targets
            .iter()
            .find_map(|t| pred(&t.action).map(|i| (t.rect, i)))
            .expect("matching field click target recorded this frame")
    }

    /// Clicking a Settings field row selects that field (like Tab/↑↓).
    #[test]
    fn click_settings_field_selects_it() {
        let mut app = app_with_sessions(1);
        app.open_settings_panel();
        // Any field past the first, so the selection visibly changes.
        let (r, index) = rendered_indexed_target(&mut app, |a| match a {
            ClickAction::ModalField(i) if *i > 0 => Some(*i),
            _ => None,
        });
        app.handle_mouse_click(r.x, r.y, KeyModifiers::NONE);
        let modals::Modal::Settings(ref s) = app.modal else {
            panic!("settings modal must stay open");
        };
        assert_eq!(s.field, modals::SettingsField::ORDER[index]);
    }

    /// Clicking an Automation-editor field row selects that field.
    #[test]
    fn click_automation_editor_field_selects_it() {
        let mut app = app_with_sessions(1);
        app.open_automation_editor();
        let (r, index) = rendered_indexed_target(&mut app, |a| match a {
            ClickAction::ModalField(i) if *i > 0 => Some(*i),
            _ => None,
        });
        app.handle_mouse_click(r.x, r.y, KeyModifiers::NONE);
        let modals::Modal::AutomationEditor(ref m) = app.modal else {
            panic!("automation editor must stay open");
        };
        assert_eq!(m.field, m.visible_fields()[index]);
    }

    /// Clicking a field in the in-pane task editor focuses that field.
    #[test]
    fn click_task_editor_field_selects_it() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::TaskEditor;
        app.task_ui.task_editor = Some(modals::TaskEditorModal::new());
        // Index 2 = Status (default is Title), so the change is observable.
        let r = rendered_indexed_target(&mut app, |a| match a {
            ClickAction::PaneField {
                focus: InputFocus::TaskEditor,
                index,
            } if *index == 2 => Some(*index),
            _ => None,
        })
        .0;
        app.handle_mouse_click(r.x, r.y, KeyModifiers::NONE);
        let m = app.task_ui.task_editor.as_ref().unwrap();
        assert_eq!(m.field, modals::TaskField::Status);
    }

    /// Clicking the repo picker's path-input area focuses the input field.
    #[test]
    fn click_repo_picker_input_focuses_input() {
        let mut app = app_with_sessions(0);
        app.start_new_session(); // no hosts → opens the repo picker (List focus)
        let r = rendered_indexed_target(&mut app, |a| match a {
            ClickAction::RepoFocus(modals::RepoPickerFocus::Input) => Some(0),
            _ => None,
        })
        .0;
        app.handle_mouse_click(r.x, r.y, KeyModifiers::NONE);
        let modals::Modal::RepoPicker(ref rp) = app.modal else {
            panic!("repo picker must stay open");
        };
        assert_eq!(rp.focus, modals::RepoPickerFocus::Input);
    }

    /// Hovering a footer button brightens its fill to `accent_bright` (a
    /// button-like hover), distinct from the background band a list row gets.
    #[test]
    fn hovering_footer_button_brightens_it() {
        let mut app = app_with_sessions(1);
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.view(f)).unwrap();
        let r = app
            .click_targets
            .iter()
            .find(|t| matches!(t.action, ClickAction::Global(_)))
            .map(|t| t.rect)
            .expect("footer buttons recorded");
        app.update(AppMessage::MouseMove { x: r.x, y: r.y });
        terminal.draw(|f| app.view(f)).unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(
            buf[(r.x, r.y)].bg,
            crate::ui::theme::Theme::accent_bright(),
            "hovered footer button should brighten to accent_bright"
        );
    }

    /// `[features] mouse = false` drops every mouse message before dispatch.
    #[test]
    fn mouse_feature_flag_disables_all_mouse_handling() {
        let mut app = app_with_sessions(2);
        app.features.mouse = false;
        app.click_targets.push(ClickTarget {
            rect: Rect::new(1, 3, 20, 1),
            action: ClickAction::SelectSession(1),
        });

        app.update(AppMessage::MouseMove { x: 5, y: 3 });
        app.update(AppMessage::MouseClick {
            x: 5,
            y: 3,
            modifiers: KeyModifiers::NONE,
        });
        app.update(AppMessage::MouseScrollUp { x: 5, y: 3 });

        assert_eq!(app.mouse_hover, None);
        assert_eq!(app.active_index, 0);
        assert!(app.text_selection.is_none());
        assert_eq!(app.focus, InputFocus::SessionList);
    }

    fn automations_list_modal(count: usize) -> modals::Modal {
        modals::Modal::AutomationsList(modals::AutomationsListModal {
            index: 0,
            entries: (0..count)
                .map(|i| modals::AutomationListEntry {
                    id: i as i64,
                    name: format!("auto-{i}"),
                    summary: "daily".into(),
                    enabled: true,
                })
                .collect(),
        })
    }

    /// The wheel steps an open modal's selection (one row per tick, like j/k)
    /// instead of scrolling the panes beneath.
    #[test]
    fn wheel_in_modal_steps_selection() {
        let mut app = app_with_sessions(1);
        app.modal = automations_list_modal(5);

        app.handle_mouse_scroll(0, 0, false);
        app.handle_mouse_scroll(0, 0, false);
        let modals::Modal::AutomationsList(ref al) = app.modal else {
            panic!("modal must stay open");
        };
        assert_eq!(al.index, 2);

        app.handle_mouse_scroll(0, 0, true);
        let modals::Modal::AutomationsList(ref al) = app.modal else {
            panic!("modal must stay open");
        };
        assert_eq!(al.index, 1);
    }

    /// Clicking + dragging the modal's own scrollbar moves its selection;
    /// the clamp guard stops at the list end.
    #[test]
    fn modal_scrollbar_drag_moves_selection() {
        let mut app = app_with_sessions(1);
        app.modal = automations_list_modal(20);
        let track = Rect::new(70, 5, 1, 10);
        app.scrollbar_hits.push(ScrollbarHit {
            geom: ScrollbarGeom {
                track,
                content_len: 20,
                viewport: 10,
            },
            target: ScrollTarget::Modal,
        });

        // Grab the bottom of the track → selection jumps to the last row.
        app.handle_mouse_click(70, 14, KeyModifiers::NONE);
        assert_eq!(app.dragging_scrollbar, Some(ScrollTarget::Modal));
        let modals::Modal::AutomationsList(ref al) = app.modal else {
            panic!("modal must stay open");
        };
        assert_eq!(al.index, 19);

        // Drag back to the top.
        app.handle_mouse_drag(70, 5);
        let modals::Modal::AutomationsList(ref al) = app.modal else {
            panic!("modal must stay open");
        };
        assert_eq!(al.index, 0);

        app.handle_mouse_up(70, 5);
        assert!(app.dragging_scrollbar.is_none());
    }

    /// While a modal is open, the wheel never reaches the panes beneath it.
    #[test]
    fn wheel_in_modal_does_not_scroll_panes() {
        let mut app = app_with_sessions(2);
        app.modal = automations_list_modal(2);
        let before = app.active_index;
        // Coordinates over the session list, which would normally switch
        // sessions on wheel.
        app.handle_mouse_scroll(2, 3, false);
        assert_eq!(app.active_index, before);
    }

    /// While the F1 editor captures a chord, the wheel must not synthesize a
    /// key (it would become the new binding).
    #[test]
    fn wheel_during_help_capture_is_ignored() {
        let mut app = app_with_sessions(1);
        app.modal = modals::Modal::Help(modals::HelpModal {
            selected: 3,
            capturing: true,
        });
        app.handle_mouse_scroll(0, 0, false);
        let modals::Modal::Help(ref h) = app.modal else {
            panic!("help must stay open");
        };
        assert!(h.capturing, "capture must survive a wheel tick");
        assert_eq!(h.selected, 3);
    }

    // --- Wheel-to-PTY forwarding ---
    //
    // Modern alt-screen TUIs (Claude Code, vim, htop, btop, …) subscribe to
    // wheel events via xterm mouse tracking. Without forwarding, vt100's
    // scrollback no-ops on the alternate screen and the user sees a "dead"
    // wheel.

    /// Build a 1-session app that keeps the session's input-channel receiver so
    /// the test can inspect bytes the app writes to the PTY.
    fn app_with_input_rx() -> (App, tokio::sync::mpsc::Receiver<Vec<u8>>) {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(Arc::clone(&backend_arc)),
            stub_agents(),
            test_db(),
        );
        let (session, rx) = Session::stub_with_input_rx("test", &backend_arc, &provider);
        app.sessions.push(session);
        app.active_index = 0;
        (app, rx)
    }

    /// Drive the active session's vt100 parser with `bytes` (e.g. an alt-screen
    /// enter + mouse-mode DECSET sequence) so subsequent tests see the same
    /// state the agent would have produced.
    fn feed_parser(app: &App, bytes: &[u8]) {
        let session = &app.sessions[app.active_index];
        let mut parser = session.parser.lock().unwrap();
        parser.process(bytes);
    }

    /// Centre-of-terminal hit point in screen-cell coordinates plus the
    /// expected 1-based (col, row) the PTY should see.
    fn click_in_terminal(app: &App) -> ((u16, u16), (u32, u32)) {
        let term = app.screen_layout().terminal;
        let inner = Block::default().borders(Borders::ALL).inner(term);
        let x = inner.x + 5;
        let y = inner.y + 2;
        ((x, y), (6, 3))
    }

    /// With SGR mouse tracking on (`\e[?1000h` + `\e[?1006h`), wheel up is sent
    /// as `\e[<64;col;rowM` — the encoding Claude Code subscribes to. Vt100
    /// scrollback is left alone so the inner app owns the scroll.
    #[test]
    fn wheel_forwards_sgr_mouse_when_inner_app_subscribes() {
        let (mut app, mut rx) = app_with_input_rx();
        // Switch to the alternate screen and enable 1000+1006 mouse tracking,
        // the exact sequence Claude Code emits at startup.
        feed_parser(&app, b"\x1b[?1049h\x1b[?1000h\x1b[?1006h");

        let ((x, y), (col, row)) = click_in_terminal(&app);
        let before = app.sessions[0].parser.lock().unwrap().screen().scrollback();

        app.handle_mouse_scroll(x, y, true);

        let expected = format!("\x1b[<64;{col};{row}M").into_bytes();
        assert_eq!(rx.try_recv().ok(), Some(expected));
        let after = app.sessions[0].parser.lock().unwrap().screen().scrollback();
        assert_eq!(before, after, "vt100 scrollback must not move");
    }

    /// Wheel down uses xterm button 65 — the only thing that changes vs.
    /// wheel up.
    #[test]
    fn wheel_down_uses_button_65() {
        let (mut app, mut rx) = app_with_input_rx();
        feed_parser(&app, b"\x1b[?1049h\x1b[?1000h\x1b[?1006h");
        let ((x, y), (col, row)) = click_in_terminal(&app);

        app.handle_mouse_scroll(x, y, false);

        let expected = format!("\x1b[<65;{col};{row}M").into_bytes();
        assert_eq!(rx.try_recv().ok(), Some(expected));
    }

    /// No mouse tracking enabled → wheel still scrolls vt100's scrollback
    /// locally, the long-standing behavior for non-TUI shells. We don't assert
    /// the scrollback advances here (the stub parser is built with `0` history
    /// for hermeticity); the invariant we care about is the negative one — the
    /// PTY never sees the wheel.
    #[test]
    fn wheel_without_mouse_mode_does_not_forward() {
        let (mut app, mut rx) = app_with_input_rx();

        let ((x, y), _) = click_in_terminal(&app);
        app.handle_mouse_scroll(x, y, true);
        app.handle_mouse_scroll(x, y, false);

        assert!(
            rx.try_recv().is_err(),
            "no mouse mode → nothing forwarded to PTY"
        );
    }

    /// Mouse mode on but with the legacy encoding (no `?1006h`): we don't
    /// support the 223-cell-capped encoding, so we fall back to the local
    /// scrollback. The PTY sees nothing.
    #[test]
    fn wheel_with_legacy_encoding_does_not_forward() {
        let (mut app, mut rx) = app_with_input_rx();
        feed_parser(&app, b"\x1b[?1049h\x1b[?1000h");

        let ((x, y), _) = click_in_terminal(&app);
        app.handle_mouse_scroll(x, y, true);

        assert!(rx.try_recv().is_err(), "legacy encoding must not forward");
    }

    /// The hovered clickable row gets a background band in the rendered frame.
    #[test]
    fn hovered_session_row_gets_background_band() {
        let mut app = app_with_sessions(2);
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        // First draw records the row hitboxes; hover over the first row and
        // draw again so the highlight applies.
        terminal.draw(|f| app.view(f)).unwrap();
        let row = app
            .click_targets
            .iter()
            .find(|t| matches!(t.action, ClickAction::SelectSession(0)))
            .map(|t| t.rect)
            .unwrap();
        app.update(AppMessage::MouseMove { x: row.x, y: row.y });
        terminal.draw(|f| app.view(f)).unwrap();

        // The first session's hitbox spans its prepended repo-group header plus
        // the session line; the tint must land on the session line (the bottom
        // row) and spare the header. `row.y` is the header row for a group's
        // first session.
        let session_y = row.y + row.height - 1;
        let band = crate::ui::theme::Theme::selection_bg();
        let buffer = terminal.backend().buffer();
        assert_eq!(
            buffer[(row.x, session_y)].bg,
            band,
            "hovered session line must get the selection_bg band"
        );
        // The repo-group header above the session line is never tinted.
        assert_ne!(
            buffer[(row.x, row.y)].bg,
            band,
            "repo-group header must not get the hover band"
        );
        // A cell outside any clickable row keeps its non-band background.
        assert_ne!(buffer[(0, 0)].bg, band);
    }

    #[test]
    fn task_editor_e_chord_edits_not_global_binding() {
        // `e` inside the editor must edit the title, not fire the file-viewer
        // toggle / other global binding (capture-before-global).
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::TaskList;
        app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(app.task_ui.task_editor.as_ref().unwrap().title.value(), "e");
        assert_eq!(app.focus, InputFocus::TaskEditor);
    }

    // --- Global search (Ctrl+/, fully rebindable) tests ---

    #[test]
    fn ctrl_slash_opens_global_search() {
        // Ctrl+/ is the default binding. Terminals encode it as `Ctrl+/`
        // (kitty protocol) or as the raw 0x1F byte that crossterm decodes as
        // `Ctrl+7` / `Ctrl+_` (legacy) — all three open the strip.
        for c in ['/', '7', '_'] {
            let mut app = app_with_sessions(1);
            app.focus = InputFocus::SessionList;
            app.handle_key(KeyCode::Char(c), KeyModifiers::CONTROL);
            assert!(app.global_search.active, "Ctrl+{c} should open search");
            assert_eq!(app.focus, InputFocus::GlobalSearch);
            // Esc closes and restores the previous focus.
            app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
            assert!(!app.global_search.active);
            assert_eq!(app.focus, InputFocus::SessionList);
        }
    }

    #[test]
    fn ctrl_a_no_longer_opens_global_search() {
        // Ctrl+A was the old default; it's now free (a readline start-of-line
        // chord left to the terminal / modal text fields) and opens nothing.
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::SessionList;
        app.handle_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert!(
            !app.global_search.active,
            "Ctrl+A must not open search anymore"
        );
    }

    #[test]
    fn global_search_chord_is_rebindable() {
        let base = std::env::temp_dir().join("thurbox-gs-rebind-test");
        let _ = std::fs::remove_dir_all(&base);
        let _g = crate::paths::TestPathGuard::new(&base);

        let mut app = app_with_sessions(1);
        // Rebind global search from Ctrl+/ to Ctrl+X via the F1 editor.
        app.keybindings.rebind(
            crate::session::Action::GlobalSearch,
            crate::session::KeyChord::ctrl('x'),
        );

        // The old chord no longer opens it...
        app.focus = InputFocus::SessionList;
        app.handle_key(KeyCode::Char('/'), KeyModifiers::CONTROL);
        assert!(!app.global_search.active, "old Ctrl+/ must not open search");

        // ...and the new chord does.
        app.handle_key(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert!(app.global_search.active, "new Ctrl+X should open search");
    }

    #[test]
    fn global_search_matches_session_name() {
        let mut app = app_with_sessions(2);
        app.sessions[0].info.name = "alpha".into();
        app.sessions[1].info.name = "bravo".into();
        app.open_global_search();
        for c in "brav".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        let hit = app
            .global_search
            .results
            .iter()
            .find(|r| matches!(r.kind, search::SearchKind::Session))
            .expect("a session result");
        assert_eq!(hit.label, "bravo");
        assert_eq!(hit.target, search::SearchTarget::Session { index: 1 });
    }

    #[test]
    fn global_search_matches_task_and_automation() {
        let mut app = app_with_sessions(1);
        let tid = app
            .db
            .create_task(&crate::storage::tasks::NewTask::local("fix the widget"))
            .unwrap();
        app.refresh_tasks();
        let new = crate::storage::automations::NewAutomation {
            name: "widget-nightly".into(),
            enabled: true,
            schedule: crate::session::AutomationSchedule::Once { at: 0 },
            timezone: None,
            action: crate::session::AutomationAction::Send {
                session_id: SessionId::default(),
            },
            prompt: "go".into(),
            next_run_at: None,
        };
        let aid = app.db.create_automation(&new).unwrap();
        app.refresh_automations();

        app.open_global_search();
        for c in "widget".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(app
            .global_search
            .results
            .iter()
            .any(|r| r.target == search::SearchTarget::Task { id: tid }));
        assert!(app
            .global_search
            .results
            .iter()
            .any(|r| r.target == search::SearchTarget::Automation { id: aid }));
    }

    /// Disabled features contribute no search results, so a selection can
    /// never preview or jump into a pane the feature flags hide.
    #[test]
    fn global_search_omits_disabled_scopes() {
        let mut app = app_with_sessions(1);
        app.features.tasks = false;
        app.features.automations = false;
        app.db
            .create_task(&crate::storage::tasks::NewTask::local("fix the widget"))
            .unwrap();
        app.refresh_tasks();
        let new = crate::storage::automations::NewAutomation {
            name: "widget-nightly".into(),
            enabled: true,
            schedule: crate::session::AutomationSchedule::Once { at: 0 },
            timezone: None,
            action: crate::session::AutomationAction::Send {
                session_id: SessionId::default(),
            },
            prompt: "go".into(),
            next_run_at: None,
        };
        app.db.create_automation(&new).unwrap();
        app.refresh_automations();

        app.open_global_search();
        for c in "widget".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(app.global_search.results.iter().all(|r| !matches!(
            r.kind,
            search::SearchKind::Task | search::SearchKind::Automation
        )));
    }

    /// Automations fully off: the TUI must not claim/fire due automations on
    /// tick or at startup catch-up (the CLI surface stays in charge).
    #[test]
    fn process_automations_noops_when_feature_disabled() {
        let mut app = app_with_sessions(1);
        app.features.automations = false;
        let new = crate::storage::automations::NewAutomation {
            name: "nightly".into(),
            enabled: true,
            schedule: crate::session::AutomationSchedule::Once { at: 1 },
            timezone: None,
            action: crate::session::AutomationAction::Send {
                session_id: SessionId::default(),
            },
            prompt: "go".into(),
            next_run_at: Some(1),
        };
        app.db.create_automation(&new).unwrap();
        let now = crate::sync::current_time_millis();
        assert_eq!(app.db.due_automations(now).unwrap().len(), 1);

        app.process_automations(true);
        assert_eq!(
            app.db.due_automations(now).unwrap().len(),
            1,
            "a due automation must stay unclaimed while the feature is off"
        );
    }

    #[test]
    fn global_search_matches_task_description() {
        let mut app = app_with_sessions(1);
        let tid = app
            .db
            .create_task(&crate::storage::tasks::NewTask {
                description: Some("investigate the flaky parser".into()),
                ..crate::storage::tasks::NewTask::local("unrelated title")
            })
            .unwrap();
        app.refresh_tasks();

        app.open_global_search();
        for c in "flaky".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        let result = app
            .global_search
            .results
            .iter()
            .find(|r| r.target == search::SearchTarget::Task { id: tid });
        let result = result.expect("description should match the query");
        assert!(
            result.snippet.as_deref().unwrap_or("").contains("flaky"),
            "a description match carries a snippet"
        );
    }

    #[test]
    fn global_search_previewing_task_selects_it_for_central_pane() {
        // When the strip previews a task result, the owning panel's cursor moves
        // to it and the preview kind is Task — the two facts the central pane
        // uses to render the task's full-screen detail/markdown.
        let mut app = app_with_sessions(1);
        let tid = app
            .db
            .create_task(&crate::storage::tasks::NewTask {
                description: Some("rendered in the main pane".into()),
                ..crate::storage::tasks::NewTask::local("zzz unrelated")
            })
            .unwrap();
        app.refresh_tasks();

        app.open_global_search();
        for c in "main pane".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(
            app.global_search_preview_kind(),
            Some(search::SearchKind::Task),
            "the matched task result should be the live preview"
        );
        assert_eq!(
            app.selected_task().map(|t| t.id),
            Some(tid),
            "the previewed task must be the panel's selection so the central pane shows it"
        );
    }

    #[test]
    fn global_search_fuzzy_matches_task_description() {
        // A gapped (non-substring) query must still hit the description, the
        // same way it would the title — they share the fuzzy matcher.
        let mut app = app_with_sessions(1);
        let tid = app
            .db
            .create_task(&crate::storage::tasks::NewTask {
                description: Some("investigate the flaky parser".into()),
                ..crate::storage::tasks::NewTask::local("unrelated title")
            })
            .unwrap();
        app.refresh_tasks();

        app.open_global_search();
        for c in "invflaky".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(
            app.global_search
                .results
                .iter()
                .any(|r| r.target == search::SearchTarget::Task { id: tid }),
            "a gapped query should fuzzy-match the description"
        );
    }

    #[test]
    fn global_search_content_match_finds_session() {
        let mut app = app_with_sessions(2);
        app.sessions[0].info.name = "one".into();
        app.sessions[1].info.name = "two".into();
        // Write a distinctive token into session 1's buffer.
        {
            let mut parser = app.sessions[1].parser.lock().unwrap();
            parser.process(b"deploy failed: exit 1\r\n");
        }
        let snippet = app.session_content_match("deploy failed", 1);
        assert!(snippet.is_some(), "content scan should find the token");
        // The full content-rebuild path includes it as a Session result.
        app.open_global_search();
        for c in "deploy failed".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        app.recompute_global_search_content();
        assert!(app.global_search.results.iter().any(|r| {
            r.target == search::SearchTarget::Session { index: 1 } && r.snippet.is_some()
        }));
    }

    #[test]
    fn global_search_enter_switches_active_index() {
        let mut app = app_with_sessions(2);
        app.sessions[0].info.name = "alpha".into();
        app.sessions[1].info.name = "bravo".into();
        app.active_index = 0;
        app.open_global_search();
        for c in "bravo".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        // Select the first (only) session result and activate.
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.global_search.active);
        assert_eq!(app.active_index, 1);
        assert_eq!(app.focus, InputFocus::Terminal);
    }

    #[test]
    fn global_search_empty_query_has_no_results() {
        let mut app = app_with_sessions(1);
        app.open_global_search();
        assert!(app.global_search.results.is_empty());
    }

    #[test]
    fn global_search_previews_session_while_typing_and_navigating() {
        let mut app = app_with_sessions(3);
        app.sessions[0].info.name = "alpha".into();
        app.sessions[1].info.name = "bravo".into();
        app.sessions[2].info.name = "bronco".into();
        app.active_index = 0;
        app.open_global_search();
        // Typing "br" matches bravo + bronco; the preview moves to the first.
        for c in "br".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(app.active_index, 1, "preview follows the top result");
        // Down moves the preview to the next matching session.
        app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.active_index, 2, "preview follows ↓ selection");
        // Focus stays in the search box during preview.
        assert_eq!(app.focus, InputFocus::GlobalSearch);
    }

    #[test]
    fn global_search_cancel_restores_previous_state() {
        let mut app = app_with_sessions(3);
        app.sessions[0].info.name = "alpha".into();
        app.sessions[1].info.name = "bravo".into();
        app.sessions[2].info.name = "bronco".into();
        app.active_index = 0;
        app.focus = InputFocus::Terminal;
        let tasks_before = app.show_tasks_panel;

        app.open_global_search();
        for c in "bro".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        // Preview moved the active session away from 0.
        assert_ne!(app.active_index, 0);

        // Esc cancels → everything snaps back to the pre-search state.
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.global_search.active);
        assert_eq!(app.active_index, 0, "active session restored");
        assert_eq!(app.focus, InputFocus::Terminal, "focus restored");
        assert_eq!(app.show_tasks_panel, tasks_before, "panel toggles restored");
    }

    #[test]
    fn global_search_commit_keeps_jump_no_restore() {
        let mut app = app_with_sessions(2);
        app.sessions[0].info.name = "alpha".into();
        app.sessions[1].info.name = "bravo".into();
        app.active_index = 0;
        app.open_global_search();
        for c in "bravo".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        // Committed: snapshot dropped, jump kept (not restored to 0).
        assert!(app.global_search.snapshot.is_none());
        assert_eq!(app.active_index, 1);
    }

    #[test]
    fn global_search_query_gates_live_highlighting() {
        let mut app = app_with_sessions(1);
        // Inactive → no live-highlight query.
        assert_eq!(app.global_search_query(), None);
        app.open_global_search();
        // Active but empty query → still None (panels render normally).
        assert_eq!(app.global_search_query(), None);
        for c in "lo".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(app.global_search_query(), Some("lo"));
        app.close_global_search();
        assert_eq!(app.global_search_query(), None);
    }

    // --- Context-sensitive Ctrl+J/K tests ---

    #[test]
    fn ctrl_j_switches_session_when_session_list_focused() {
        let mut app = app_with_sessions(3);
        app.focus = InputFocus::SessionList;
        app.active_index = 0;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_index, 1);
    }

    #[test]
    fn ctrl_j_switches_session_when_terminal_focused() {
        let mut app = app_with_sessions(3);
        app.focus = InputFocus::Terminal;
        app.active_index = 0;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_index, 1);
    }

    #[test]
    fn ctrl_j_at_last_session_wraps_to_first() {
        let mut app = app_with_sessions(3);
        app.focus = InputFocus::SessionList;
        app.active_index = 2;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_index, 0);
    }

    #[test]
    fn ctrl_k_at_first_session_wraps_to_last() {
        let mut app = app_with_sessions(3);
        app.focus = InputFocus::SessionList;
        app.active_index = 0;
        app.handle_key(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.active_index, 2);
    }

    // --- Unified left-column (session list ↔ automations) navigation ---

    /// Add an enabled spawn automation to the DB and refresh the cache.
    fn add_test_automation(app: &mut App, name: &str) {
        let new = crate::storage::automations::NewAutomation {
            name: name.to_string(),
            enabled: true,
            schedule: AutomationSchedule::Cron {
                expr: "0 9 * * *".to_string(),
            },
            timezone: None,
            action: AutomationAction::Spawn {
                repo_path: std::path::PathBuf::from("/tmp/repo"),
                worktree_branch: None,
                base_branch: None,
                agent: None,
                extra_repos: Vec::new(),
            },
            prompt: "do stuff".to_string(),
            next_run_at: None,
        };
        app.db.create_automation(&new).unwrap();
        app.refresh_automations();
    }

    #[test]
    fn j_at_last_session_enters_automations_pane() {
        let mut app = app_with_sessions(3);
        app.focus = InputFocus::SessionList;
        app.active_index = 2; // last in render order (no admins)
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::Automations);
        assert_eq!(app.automation_ui.automation_panel_index, 0);
    }

    #[test]
    fn j_mid_session_list_advances_without_leaving() {
        let mut app = app_with_sessions(3);
        app.focus = InputFocus::SessionList;
        app.active_index = 0;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::SessionList);
        assert_eq!(app.active_index, 1);
    }

    #[test]
    fn k_at_first_session_loops_to_last_automation() {
        let mut app = app_with_sessions(3);
        add_test_automation(&mut app, "a");
        add_test_automation(&mut app, "b");
        app.focus = InputFocus::SessionList;
        app.active_index = 0;
        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        // Above the first session the column loops to the bottom: the last
        // automation in the pane.
        assert_eq!(app.focus, InputFocus::Automations);
        assert_eq!(app.automation_ui.automation_panel_index, 1);
    }

    #[test]
    fn k_at_top_of_automations_returns_to_last_session() {
        let mut app = app_with_sessions(3);
        add_test_automation(&mut app, "nightly");
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::SessionList);
        assert_eq!(app.active_index, 2); // last in render order
    }

    #[test]
    fn k_in_empty_automations_pane_returns_to_session_list() {
        let mut app = app_with_sessions(2);
        app.focus = InputFocus::Automations;
        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::SessionList);
    }

    #[test]
    fn j_at_bottom_automation_loops_to_first_session() {
        let mut app = app_with_sessions(2);
        add_test_automation(&mut app, "a");
        app.focus = InputFocus::Automations;
        app.active_index = 1;
        app.automation_ui.automation_panel_index = 0; // the only (= last) automation
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        // Past the last automation the column loops back to the top session.
        assert_eq!(app.focus, InputFocus::SessionList);
        assert_eq!(app.active_index, 0, "looped to first session");
    }

    #[test]
    fn j_between_automations_advances_selection() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        add_test_automation(&mut app, "b");
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.automation_ui.automation_panel_index, 1);
    }

    #[test]
    fn n_in_automations_pane_focuses_new_editor() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::Automations;
        app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        // The central-pane editor is focused (no overlay modal), with a new
        // (unsaved) automation.
        assert_eq!(app.focus, InputFocus::AutomationEditor);
        assert!(matches!(app.modal, modals::Modal::None));
        let editor = app
            .automation_ui
            .automation_editor
            .as_ref()
            .expect("editor present");
        assert!(editor.editing_id.is_none(), "should be a new automation");
    }

    #[test]
    fn enter_in_automations_pane_focuses_editor_for_existing() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::AutomationEditor);
        assert!(matches!(app.modal, modals::Modal::None));
        let editor = app
            .automation_ui
            .automation_editor
            .as_ref()
            .expect("editor present");
        assert!(
            editor.editing_id.is_some(),
            "should edit the existing automation"
        );
    }

    #[test]
    fn ctrl_l_from_automations_enters_editor_and_ctrl_h_returns() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.sync_automation_editor();
        // Ctrl+L moves focus into the central-pane editor (like a session).
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::AutomationEditor);
        // Ctrl+H returns to the automations list.
        app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::Automations);
    }

    #[test]
    fn navigating_automations_rebuilds_editor_preview() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        add_test_automation(&mut app, "b");
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.sync_automation_editor();
        let first = app
            .automation_ui
            .automation_editor
            .as_ref()
            .unwrap()
            .editing_id;
        assert_eq!(first, Some(app.automation_ui.cached_automations[0].id));
        // Moving down rebuilds the preview to mirror the next automation.
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.automation_ui.automation_panel_index, 1);
        let second = app
            .automation_ui
            .automation_editor
            .as_ref()
            .unwrap()
            .editing_id;
        assert_eq!(second, Some(app.automation_ui.cached_automations[1].id));
        assert_ne!(first, second);
    }

    #[test]
    fn leaving_automation_context_clears_editor() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        app.focus = InputFocus::Automations;
        app.sync_automation_editor();
        assert!(app.automation_ui.automation_editor.is_some());
        // Focusing a session drops the in-pane editor preview.
        app.focus = InputFocus::SessionList;
        app.sync_automation_editor();
        assert!(app.automation_ui.automation_editor.is_none());
    }

    #[test]
    fn editing_in_pane_and_saving_returns_to_list() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.sync_automation_editor();
        app.focus = InputFocus::AutomationEditor;
        // Edit the name, then save with Enter.
        if let Some(ed) = app.automation_ui.automation_editor.as_mut() {
            ed.field = AutomationField::Name;
            ed.name.set("renamed");
        }
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::Automations);
        let autos = app.db.list_automations().unwrap();
        assert_eq!(autos.len(), 1);
        assert_eq!(autos[0].name, "renamed");
    }

    #[test]
    fn esc_in_pane_editor_discards_and_returns_to_list() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.sync_automation_editor();
        app.focus = InputFocus::AutomationEditor;
        if let Some(ed) = app.automation_ui.automation_editor.as_mut() {
            ed.name.set("scratch");
        }
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::Automations);
        // The discarded edit was not persisted.
        let autos = app.db.list_automations().unwrap();
        assert_eq!(autos[0].name, "a");
    }

    #[test]
    fn ctrl_e_in_pane_editor_toggles_enabled_not_file_viewer() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.sync_automation_editor();
        app.focus = InputFocus::AutomationEditor;
        let before = app
            .automation_ui
            .automation_editor
            .as_ref()
            .unwrap()
            .enabled;
        let fv_before = app.show_file_viewer;
        // Ctrl+E is the global file-viewer toggle, but the pane editor must
        // capture it as "toggle enabled" instead.
        app.handle_key(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!(
            app.show_file_viewer, fv_before,
            "file viewer must not toggle"
        );
        assert_eq!(
            app.automation_ui
                .automation_editor
                .as_ref()
                .unwrap()
                .enabled,
            !before,
            "Ctrl+E should flip the editor's enabled flag"
        );
    }

    #[test]
    fn cycle_wraps_back_to_automations_not_session() {
        let mut app = app_with_sessions(2);
        add_test_automation(&mut app, "a");
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.sync_automation_editor();
        // Automations → editor → run history → back to Automations (never lands
        // on a session, mirroring how Esc returns to the selected automation).
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::AutomationEditor);
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::AutomationRunHistory);
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::Automations);
    }

    #[test]
    fn new_automation_editor_cycle_wraps_to_automations() {
        // A brand-new automation has no run history, so the ring is just
        // Automations ↔ editor — and Ctrl+L still returns to the list.
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::Automations;
        app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::AutomationEditor);
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::Automations);
    }

    #[test]
    fn enter_on_run_opens_related_session() {
        let mut app = app_with_sessions(2);
        add_test_automation(&mut app, "a");
        let auto_id = app.automation_ui.cached_automations[0].id;
        // Record a run with a typed related session (as fire_automation does).
        let target = app.sessions[1].info.id;
        app.db
            .record_automation_run(auto_id, AutomationRunStatus::Success, "sent", Some(target))
            .unwrap();
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.sync_automation_editor();
        app.focus = InputFocus::AutomationRunHistory;
        app.refresh_selected_automation_runs();
        app.automation_ui.automation_run_index = 0;

        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(app.focus, InputFocus::Terminal);
        assert_eq!(app.active_index, 1, "should jump to the referenced session");
    }

    #[test]
    fn enter_on_legacy_run_parses_session_from_detail() {
        let mut app = app_with_sessions(2);
        add_test_automation(&mut app, "a");
        let auto_id = app.automation_ui.cached_automations[0].id;
        // Pre-v28 rows have no related_session_id; only the free-text detail
        // (e.g. "session <uuid>") references the session.
        let target = app.sessions[1].info.id;
        app.db
            .record_automation_run(
                auto_id,
                AutomationRunStatus::Success,
                &format!("session {target}"),
                None,
            )
            .unwrap();
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.sync_automation_editor();
        app.focus = InputFocus::AutomationRunHistory;
        app.refresh_selected_automation_runs();
        app.automation_ui.automation_run_index = 0;

        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(app.focus, InputFocus::Terminal);
        assert_eq!(app.active_index, 1, "should jump to the referenced session");
    }

    #[test]
    fn enter_on_run_without_session_stays_in_history() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        let auto_id = app.automation_ui.cached_automations[0].id;
        // A skipped run has no session id in its detail.
        app.db
            .record_automation_run(
                auto_id,
                AutomationRunStatus::Skipped,
                "target session not running",
                None,
            )
            .unwrap();
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.sync_automation_editor();
        app.focus = InputFocus::AutomationRunHistory;
        app.refresh_selected_automation_runs();

        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        // No related session → stay put in the run-history panel.
        assert_eq!(app.focus, InputFocus::AutomationRunHistory);
    }

    #[test]
    fn ctrl_l_from_editor_enters_run_history_then_back() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.sync_automation_editor();
        app.focus = InputFocus::AutomationEditor;
        // Editor → run history → editor.
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::AutomationRunHistory);
        app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::AutomationEditor);
    }

    #[test]
    fn run_history_jk_moves_selection_and_r_triggers_run() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        let id = app.automation_ui.cached_automations[0].id;
        // Two recorded runs so j/k has something to move over.
        app.db
            .record_automation_run(id, AutomationRunStatus::Success, "one", None)
            .unwrap();
        app.db
            .record_automation_run(id, AutomationRunStatus::Error, "two", None)
            .unwrap();
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.sync_automation_editor();
        app.focus = InputFocus::AutomationRunHistory;
        app.refresh_selected_automation_runs();
        assert_eq!(app.automation_ui.automation_run_index, 0);
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.automation_ui.automation_run_index, 1);
        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.automation_ui.automation_run_index, 0);
        // `r` marks the automation due (next_run_at in the past/now).
        app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
        let auto = app.db.get_automation(id).unwrap().unwrap();
        let now = crate::sync::current_time_millis();
        assert!(
            auto.next_run_at.map(|n| n <= now).unwrap_or(false),
            "run-now should make the automation due"
        );
    }

    #[test]
    fn focusing_automations_loads_selected_run_history() {
        let mut app = app_with_sessions(1);
        add_test_automation(&mut app, "a");
        let id = app.automation_ui.cached_automations[0].id;
        app.db
            .record_automation_run(id, AutomationRunStatus::Success, "spawned x", None)
            .unwrap();
        // While the pane is unfocused the run cache is empty.
        assert!(app.automation_ui.cached_automation_runs.is_empty());
        // Entering the pane (via j from the last/only session) loads it.
        app.focus = InputFocus::SessionList;
        app.active_index = 0;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::Automations);
        assert_eq!(app.automation_ui.cached_automation_runs_id, Some(id));
        assert_eq!(app.automation_ui.cached_automation_runs.len(), 1);
    }

    #[test]
    fn spawn_automation_expands_tilde_in_repo_path() {
        let mut app = app_with_sessions(0);
        let mut m = modals::AutomationEditorModal::default();
        m.name.set("t");
        m.prompt.set("hi");
        m.action = AutomationActionKind::Spawn;
        m.trigger_kind = TriggerKind::Daily; // yields a future next_run
        m.repo.set("~/Repositories/thurbox");
        app.modal = modals::Modal::AutomationEditor(m);

        app.submit_automation_editor();

        let autos = app.db.list_automations().unwrap();
        assert_eq!(autos.len(), 1, "automation should have been created");
        match &autos[0].action {
            AutomationAction::Spawn { repo_path, .. } => {
                // `~` expands via the platform home dir: `$HOME` on Unix,
                // `%USERPROFILE%` on Windows (see `paths::expand_tilde`).
                let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
                let home = std::env::var(home_var).expect("home var set in tests");
                assert_eq!(
                    repo_path,
                    &std::path::PathBuf::from(home).join("Repositories/thurbox"),
                    "leading ~ should be expanded to an absolute path"
                );
            }
            other => panic!("expected a spawn action, got {other:?}"),
        }
    }

    #[test]
    fn send_automation_target_defaults_to_active_and_is_selectable() {
        let mut app = app_with_sessions(3);
        app.active_index = 1;
        app.open_automation_editor();

        // The Send target defaults to the active session, and every session is
        // offered as a choice.
        {
            let modals::Modal::AutomationEditor(ref m) = app.modal else {
                panic!("expected the automation editor");
            };
            assert_eq!(m.sessions.len(), 3);
            assert_eq!(
                m.selected_target().map(|(id, _)| *id),
                Some(app.sessions[1].info.id)
            );
        }

        // Cycle the Target selector to the next session, then submit.
        let expected_id;
        {
            let modals::Modal::AutomationEditor(ref mut m) = app.modal else {
                panic!("expected the automation editor");
            };
            m.name.set("ping");
            m.prompt.set("hi");
            m.trigger_kind = TriggerKind::Daily;
            m.field = AutomationField::Target;
            m.adjust(1); // index 1 -> 2
            expected_id = m.selected_target().map(|(id, _)| *id).unwrap();
        }
        app.submit_automation_editor();

        let autos = app.db.list_automations().unwrap();
        assert_eq!(autos.len(), 1);
        match &autos[0].action {
            AutomationAction::Send { session_id } => assert_eq!(*session_id, expected_id),
            other => panic!("expected a send action, got {other:?}"),
        }
    }

    #[test]
    fn send_automation_without_sessions_is_rejected() {
        let mut app = app_with_sessions(0);
        app.open_automation_editor();
        {
            let modals::Modal::AutomationEditor(ref mut m) = app.modal else {
                panic!("expected the automation editor");
            };
            m.name.set("x");
            m.prompt.set("y");
            m.trigger_kind = TriggerKind::Daily;
            // action defaults to Send, but there are no sessions to target.
        }
        app.submit_automation_editor();
        assert_eq!(
            app.db.list_automations().unwrap().len(),
            0,
            "a send automation with no target session must not be created"
        );
    }

    // --- DB persistence tests ---

    #[test]
    fn load_persisted_state_empty_db_returns_none() {
        let app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        assert!(app.load_persisted_state_from_db().is_none());
    }
    #[test]
    fn save_state_roundtrips_sessions() {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            stub_agents(),
            test_db(),
        );

        // Add a session
        let session = Session::stub("test-session", &backend_arc, &provider);
        app.sessions.push(session);

        // Save to DB (only persists sessions + counter, not projects)
        app.save_state();

        // Verify session in DB
        let sessions = app.db.list_active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "test-session");
    }

    #[test]
    fn save_state_persists_session_counter() {
        let mut app = App::new(24, 120, stub_backend(), stub_agents(), test_db());
        app.session_counter = 42;

        app.save_state();

        let counter = app.db.get_session_counter().unwrap();
        assert_eq!(counter, 42);
    }

    #[test]
    fn session_to_shared_maps_worktree() {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            stub_agents(),
            test_db(),
        );

        let mut session = Session::stub("test-session", &backend_arc, &provider);
        session.info.worktrees = vec![WorktreeInfo {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.git/wt/feat"),
            branch: "feat".to_string(),
        }];

        app.sessions.push(session);

        let shared = app.session_to_shared(&app.sessions[0]);
        assert_eq!(shared.worktrees.len(), 1);
        let wt = &shared.worktrees[0];
        assert_eq!(wt.branch, "feat");
        assert_eq!(wt.repo_path, PathBuf::from("/repo"));
    }

    #[test]
    fn session_to_shared_maps_parent_session_id() {
        // Regression guard: `save_state` upserts every session via
        // `session_to_shared`, so dropping the parent here would wipe a
        // CLI-set lead/worker link from the DB on the TUI's next save.
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            stub_agents(),
            test_db(),
        );

        let parent_id = SessionId::default();
        let mut session = Session::stub("worker", &backend_arc, &provider);
        session.info.parent_session_id = Some(parent_id);
        app.sessions.push(session);

        let shared = app.session_to_shared(&app.sessions[0]);
        assert_eq!(shared.parent_session_id, Some(parent_id));

        // And the metadata copy applies it back on adoption/update.
        let mut adopted = Session::stub("worker", &backend_arc, &provider);
        App::apply_shared_session_metadata(&mut adopted, &shared);
        assert_eq!(adopted.info.parent_session_id, Some(parent_id));
    }

    #[test]
    fn session_to_shared_maps_display_order() {
        // Regression guard: `save_state` upserts every session via
        // `session_to_shared`, so dropping the field here would wipe the
        // manual list order from the DB on the TUI's next save.
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            stub_agents(),
            test_db(),
        );

        let mut session = Session::stub("ordered", &backend_arc, &provider);
        session.info.display_order = Some(7);
        app.sessions.push(session);

        let shared = app.session_to_shared(&app.sessions[0]);
        assert_eq!(shared.display_order, Some(7));

        // And the metadata copy applies it back on adoption/update.
        let mut adopted = Session::stub("ordered", &backend_arc, &provider);
        App::apply_shared_session_metadata(&mut adopted, &shared);
        assert_eq!(adopted.info.display_order, Some(7));
    }

    #[test]
    fn move_active_session_renumbers_and_persists() {
        let mut app = app_with_sessions(3);
        for (i, s) in app.sessions.iter_mut().enumerate() {
            s.info.name = format!("s{i}");
        }
        app.active_index = 0;

        app.move_active_session(true);

        // Render order is now [s1, s0, s2], densely renumbered 0..n.
        assert_eq!(app.render_order_indices(), vec![1, 0, 2]);
        assert_eq!(app.sessions[1].info.display_order, Some(0));
        assert_eq!(app.sessions[0].info.display_order, Some(1));
        assert_eq!(app.sessions[2].info.display_order, Some(2));
        // The selection follows the moved row (input index unchanged).
        assert_eq!(app.active_index, 0);

        // Persisted: the DB lists sessions in the new order.
        let names: Vec<String> = app
            .db
            .list_active_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["s1", "s0", "s2"]);

        // A status change never moves a row.
        app.sessions[2].info.status = SessionStatus::Blocked;
        assert_eq!(app.render_order_indices(), vec![1, 0, 2]);
    }

    #[test]
    fn move_active_session_at_edge_is_noop() {
        let mut app = app_with_sessions(2);
        app.active_index = 0;
        app.move_active_session(false); // already at the top
        assert_eq!(app.render_order_indices(), vec![0, 1]);
        assert!(app.sessions.iter().all(|s| s.info.display_order.is_none()));
    }

    #[test]
    fn sort_sessions_alphabetically_renumbers_and_persists() {
        let mut app = app_with_sessions(3);
        // Names in deliberately non-alphabetical order: c, a, b.
        app.sessions[0].info.name = "c".to_string();
        app.sessions[1].info.name = "a".to_string();
        app.sessions[2].info.name = "b".to_string();
        app.active_index = 0;

        app.sort_sessions_alphabetically();

        // Render order is now [a, b, c], densely renumbered 0..n.
        assert_eq!(app.render_order_indices(), vec![1, 2, 0]);
        assert_eq!(app.sessions[1].info.display_order, Some(0));
        assert_eq!(app.sessions[2].info.display_order, Some(1));
        assert_eq!(app.sessions[0].info.display_order, Some(2));

        // Persisted: the DB lists sessions in the new order.
        let names: Vec<String> = app
            .db
            .list_active_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn sort_sessions_alphabetically_empty_is_noop() {
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(stub_backend_arc()),
            stub_agents(),
            test_db(),
        );
        app.sort_sessions_alphabetically(); // must not panic
        assert!(app.sessions.is_empty());
    }

    #[test]
    fn ctrl_r_no_op_without_agent_session_id() {
        let mut app = app_with_sessions(1);
        // App::new may toast warnings from the developer's real keybindings
        // file; this test only cares that Ctrl+R itself stays silent.
        app.status_message = None;
        // Session exists but has no agent_session_id
        app.sessions[0].info.agent_session_id = None;
        app.focus = InputFocus::Terminal;
        app.handle_key(KeyCode::Char('r'), KeyModifiers::CONTROL);
        // Should be a no-op (no error, no crash)
        assert!(app.status_message.is_none());
    }

    #[test]
    fn session_to_shared_maps_additional_dirs() {
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(backend_arc.clone()),
            stub_agents(),
            test_db(),
        );

        let mut session = Session::stub("test-session", &backend_arc, &provider);
        session.info.additional_dirs = vec![PathBuf::from("/repo2"), PathBuf::from("/repo3")];

        app.sessions.push(session);

        let shared = app.session_to_shared(&app.sessions[0]);
        assert_eq!(shared.additional_dirs.len(), 2);
        assert_eq!(shared.additional_dirs[0], PathBuf::from("/repo2"));
        assert_eq!(shared.additional_dirs[1], PathBuf::from("/repo3"));
    }

    // --- multi-repo member resolution + workspace cwd ---

    #[test]
    fn member_dirs_worktree_first_then_additional() {
        let worktrees = vec![WorktreeInfo {
            repo_path: PathBuf::from("/src/webapp"),
            worktree_path: PathBuf::from("/wt/webapp/feat"),
            branch: "feat".into(),
        }];
        let additional = vec![PathBuf::from("/src/infra")];
        let members = session_member_dirs(None, &worktrees, &additional);

        // Worktree repo: name from repo_path, dir = the checkout. Then the
        // non-worktree additional dir.
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].0.as_deref(), Some("webapp"));
        assert_eq!(members[0].1, PathBuf::from("/wt/webapp/feat"));
        assert_eq!(members[1].0.as_deref(), Some("infra"));
        assert_eq!(members[1].1, PathBuf::from("/src/infra"));
    }

    #[test]
    fn member_dirs_no_worktrees_uses_cwd_first() {
        let cwd = PathBuf::from("/src/primary");
        let additional = vec![PathBuf::from("/src/other")];
        let members = session_member_dirs(Some(&cwd), &[], &additional);

        assert_eq!(members.len(), 2);
        assert_eq!(members[0].1, PathBuf::from("/src/primary"));
        assert_eq!(members[1].1, PathBuf::from("/src/other"));
    }

    /// Init a real git repo at `dir` on branch `main` with one commit.
    fn init_repo(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        let git = |args: &[&str]| {
            let ok = crate::git::git_program()
                .args(args)
                .current_dir(dir)
                .output()
                .expect("run git")
                .status
                .success();
            assert!(ok, "git {args:?} failed in {}", dir.display());
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("file.txt"), "hi").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
    }

    #[test]
    fn create_worktrees_falls_back_to_extra_repos_default_branch() {
        // The chosen base exists only in the primary repo; the extra repo must
        // fork from its own default branch instead of failing the whole spawn.
        let tmp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path().join("data"));
        let primary = tmp.path().join("primary");
        let extra = tmp.path().join("extra");
        init_repo(&primary);
        init_repo(&extra);
        let ok = crate::git::git_program()
            .args(["branch", "feat-base"])
            .current_dir(&primary)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "creating feat-base in primary failed");

        let infos = create_worktrees(
            None,
            &[primary.clone(), extra.clone()],
            "wt-branch",
            "feat-base",
        )
        .expect("both worktrees created");

        assert_eq!(infos.len(), 2);
        for info in &infos {
            assert!(info.worktree_path.exists());
            assert_eq!(info.branch, "wt-branch");
        }
        assert_eq!(infos[0].repo_path, primary);
        assert_eq!(infos[1].repo_path, extra);
    }

    #[test]
    fn process_cwd_single_member_is_primary() {
        let cwd = PathBuf::from("/src/only");
        let out = resolve_process_cwd(Some("id-1"), Some(cwd.clone()), &[], &[], None);
        assert_eq!(out, Some(cwd));
    }

    #[test]
    fn process_cwd_multi_member_is_workspace() {
        let base = std::env::temp_dir().join("thurbox-procwd-test");
        let _ = std::fs::remove_dir_all(&base);
        let _g = crate::paths::TestPathGuard::new(&base);

        let primary = base.join("repo-a");
        let other = base.join("repo-b");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        let out = resolve_process_cwd(
            Some("sess-x"),
            Some(primary.clone()),
            &[],
            std::slice::from_ref(&other),
            None,
        )
        .unwrap();

        // cwd is now a workspace under the workspaces root, with a symlink per repo.
        let ws_root = crate::paths::workspaces_directory().unwrap();
        assert!(out.starts_with(&ws_root), "{out:?} not under {ws_root:?}");
        assert_eq!(std::fs::read_link(out.join("repo-a")).unwrap(), primary);
        assert_eq!(std::fs::read_link(out.join("repo-b")).unwrap(), other);
    }

    #[test]
    fn process_cwd_multi_member_without_session_id_falls_back_to_primary() {
        // No agent_session_id → no stable name for a workspace → use the primary
        // repo (and don't touch the filesystem).
        let primary = PathBuf::from("/src/a");
        let other = PathBuf::from("/src/b");
        let out = resolve_process_cwd(
            None,
            Some(primary.clone()),
            &[],
            std::slice::from_ref(&other),
            None,
        );
        assert_eq!(out, Some(primary));
    }

    #[test]
    fn set_error_creates_error_status() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.set_error("something failed");
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert_eq!(msg.text, "something failed");
    }

    #[test]
    fn set_status_creates_typed_status() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.set_status(StatusLevel::Success, "all good");
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Success);
        assert_eq!(msg.text, "all good");
    }

    #[test]
    fn set_status_replaces_previous() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.set_error("old error");
        app.set_status(StatusLevel::Info, "new info");
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert_eq!(msg.text, "new info");
    }

    // --- Repo picker row building ---

    fn repo_bookmark(path: &Path, is_parent: bool) -> crate::storage::repo_bookmarks::RepoBookmark {
        crate::storage::repo_bookmarks::RepoBookmark {
            repo_path: path.to_path_buf(),
            label: None,
            last_used_at: 0,
            use_count: 1,
            is_parent,
        }
    }

    #[test]
    fn rebuild_rows_dedupes_standalone_that_is_also_a_parent_child() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("alpha").join(".git")).unwrap();
        std::fs::create_dir_all(root.join("beta").join(".git")).unwrap();

        // A standalone bookmark for `alpha` AND a parent bookmark for `root`
        // whose scan also finds `alpha`. `alpha` must appear exactly once.
        let bookmarks = vec![
            repo_bookmark(&root.join("alpha"), false),
            repo_bookmark(root, true),
        ];
        let mut rp = modals::RepoPickerModal::default();
        App::rebuild_repo_picker_rows(&mut rp, bookmarks);

        // Rows: parent header, alpha (child), beta (child). The standalone
        // `alpha` was dropped in favour of the grouped child.
        assert_eq!(rp.bookmarks.len(), 3);
        assert_eq!(rp.is_header, vec![true, false, false]);
        let alpha_rows = rp.bookmarks.iter().filter(|p| p.ends_with("alpha")).count();
        assert_eq!(alpha_rows, 1, "alpha must not be duplicated");
        // The single `alpha` row is the grouped child (nested under the parent).
        let alpha_idx = rp
            .bookmarks
            .iter()
            .position(|p| p.ends_with("alpha"))
            .unwrap();
        assert!(rp.is_child[alpha_idx]);
    }

    #[test]
    fn rebuild_rows_dedupes_parent_child_that_is_also_a_parent() {
        // `root/sub` is a git repo found by scanning `root`, and is *also*
        // bookmarked as its own parent. Whichever order they are processed, the
        // `sub` path must render exactly once (no duplicate row).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("sub").join(".git")).unwrap();
        std::fs::create_dir_all(root.join("sub").join("leaf").join(".git")).unwrap();

        let bookmarks = vec![
            repo_bookmark(root, true),
            repo_bookmark(&root.join("sub"), true),
        ];
        let mut rp = modals::RepoPickerModal::default();
        App::rebuild_repo_picker_rows(&mut rp, bookmarks);

        let sub_rows = rp
            .bookmarks
            .iter()
            .filter(|p| p.file_name().is_some_and(|n| n == "sub"))
            .count();
        assert_eq!(sub_rows, 1, "sub must not be duplicated across parents");
    }

    #[test]
    fn rebuild_rows_collapse_hides_children() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("alpha").join(".git")).unwrap();
        std::fs::create_dir_all(root.join("beta").join(".git")).unwrap();

        let mut rp = modals::RepoPickerModal::default();
        App::rebuild_repo_picker_rows(&mut rp, vec![repo_bookmark(root, true)]);
        // Header + two children all visible.
        assert_eq!(rp.filtered_indices.len(), 3);

        // Collapse the parent header (row 0) → only the header stays visible.
        rp.toggle_collapsed(0);
        assert_eq!(rp.filtered_indices, vec![0]);

        // Expanding restores the children.
        rp.toggle_collapsed(0);
        assert_eq!(rp.filtered_indices.len(), 3);
    }

    // --- Worktree sync tests ---

    #[test]
    fn start_sync_with_no_sessions_shows_info() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.start_sync();
        assert!(!app.worktree_sync.in_progress);
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert_eq!(msg.text, "No worktrees to sync");
    }

    #[test]
    fn start_sync_ignores_if_already_in_progress() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.worktree_sync.in_progress = true;
        app.status_message = None;
        app.start_sync();
        // Should not set any new status message
        assert!(app.status_message.is_none());
    }

    #[test]
    fn ctrl_s_triggers_start_sync() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.handle_key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        // No sessions → info message
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.text, "No worktrees to sync");
    }

    #[test]
    fn start_sync_with_worktree_sessions_sets_in_progress() {
        let mut app = app_with_sessions(1);
        app.sessions[0].info.worktrees = vec![WorktreeInfo {
            repo_path: PathBuf::from("/tmp/nonexistent-repo"),
            worktree_path: PathBuf::from("/tmp/nonexistent-wt"),
            branch: "test-branch".to_string(),
        }];

        app.start_sync();
        assert!(app.worktree_sync.in_progress);
        assert_eq!(app.worktree_sync.pending, 1);
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert!(msg.text.contains("Syncing 1 worktree"));
    }

    #[test]
    fn start_sync_with_no_worktrees_in_active_project_shows_info() {
        let mut app = app_with_sessions(1);
        // Session has no worktrees
        assert!(app.sessions[0].info.worktrees.is_empty());
        app.start_sync();
        assert!(!app.worktree_sync.in_progress);
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert_eq!(msg.text, "No worktrees to sync");
    }

    #[test]
    fn start_sync_only_syncs_active_session() {
        let mut app = app_with_sessions(1);
        // Active session (index 0) has no worktrees by default.
        // Add a second session with a worktree — it should NOT be synced.
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut other = Session::stub("other-session", &backend_arc, &provider);
        other.info.worktrees = vec![WorktreeInfo {
            repo_path: PathBuf::from("/tmp/other-repo"),
            worktree_path: PathBuf::from("/tmp/other-wt"),
            branch: "other-branch".to_string(),
        }];
        app.sessions.push(other);
        // active_index is 0 (no worktrees), so sync should find nothing
        app.start_sync();
        assert!(!app.worktree_sync.in_progress);
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.text, "No worktrees to sync");
    }

    #[test]
    fn start_sync_ignores_inactive_session_worktrees() {
        let mut app = app_with_sessions(1);
        // Give the active session (index 0) a worktree.
        app.sessions[0].info.worktrees = vec![WorktreeInfo {
            repo_path: PathBuf::from("/tmp/active-repo"),
            worktree_path: PathBuf::from("/tmp/active-wt"),
            branch: "active-branch".to_string(),
        }];
        // Add an inactive session with its own worktree.
        let backend_arc = stub_backend_arc();
        let provider = stub_provider();
        let mut other = Session::stub("inactive-session", &backend_arc, &provider);
        other.info.worktrees = vec![WorktreeInfo {
            repo_path: PathBuf::from("/tmp/inactive-repo"),
            worktree_path: PathBuf::from("/tmp/inactive-wt"),
            branch: "inactive-branch".to_string(),
        }];
        app.sessions.push(other);
        // Only the active session's 1 worktree should be synced, not 2.
        app.start_sync();
        assert!(app.worktree_sync.in_progress);
        assert_eq!(app.worktree_sync.pending, 1);
    }

    #[test]
    fn tick_increments_tick_count() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        assert_eq!(app.metrics.tick_count, 0);
        app.tick();
        assert_eq!(app.metrics.tick_count, 1);
        app.tick();
        assert_eq!(app.metrics.tick_count, 2);
    }

    #[test]
    fn perf_hook_states_cached_across_idle_ticks() {
        // `refresh_session_statuses` reloads the persisted hook columns only
        // when the DB's `data_version` moves. With no external writer, the first
        // tick loads and every subsequent idle tick reuses the cache — so the
        // expensive sessions-table scan no longer runs ~100×/s. See
        // docs/PERFORMANCE.md (ADR-P2).
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        assert_eq!(app.perf_counters().hook_state_loads, 0);
        for _ in 0..5 {
            app.tick();
        }
        assert_eq!(
            app.perf_counters().hook_state_loads,
            1,
            "only the first tick loads; idle ticks reuse the cache"
        );
    }

    #[test]
    fn perf_hook_states_reload_on_external_change() {
        // An *external* `session signal` commits on another connection, bumping
        // this connection's `data_version` — which must invalidate the cache and
        // trigger exactly one fresh load. A file-backed DB is required so a
        // second connection shares the same database.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let db = Database::open(tmp.path()).unwrap();
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), db);

        app.tick();
        let after_first = app.perf_counters().hook_state_loads;
        assert_eq!(after_first, 1);

        // No external write yet: another idle tick stays cached.
        app.tick();
        assert_eq!(app.perf_counters().hook_state_loads, 1);

        // A different connection commits → `data_version` moves.
        let db2 = Database::open(tmp.path()).unwrap();
        db2.set_session_counter(7).unwrap();

        app.tick();
        assert_eq!(
            app.perf_counters().hook_state_loads,
            2,
            "an external commit must invalidate the cache exactly once"
        );
    }

    #[test]
    fn perf_external_poll_never_reloads_without_external_writes() {
        // With no *other* connection writing, `PRAGMA data_version` never moves,
        // so the cheap poll never escalates to a full shared-state reload. The
        // ratio of reloads to checks is the "is the poll doing real work?"
        // signal; here it must be zero.
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        for _ in 0..8 {
            app.tick();
        }
        assert_eq!(
            app.perf_counters().external_poll_reloads,
            0,
            "no external writes ⇒ no shared-state reload"
        );
        assert!(
            app.perf_counters().external_poll_reloads <= app.perf_counters().external_poll_checks,
            "reloads are a subset of checks"
        );
    }

    #[test]
    fn finish_sync_all_synced_shows_success() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let id = SessionId::default();
        app.worktree_sync.completed = vec![
            (id, git::SyncResult::Synced),
            (SessionId::default(), git::SyncResult::Synced),
        ];
        app.finish_sync();
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Success);
        assert!(msg.text.contains("2 worktree(s) synced"));
    }

    #[test]
    fn finish_sync_with_errors_shows_error() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.worktree_sync.completed = vec![(
            SessionId::default(),
            git::SyncResult::Error("fetch failed".into()),
        )];
        app.finish_sync();
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert!(msg.text.contains("Sync failed"));
        assert!(msg.text.contains("fetch failed"));
    }

    #[test]
    fn finish_sync_with_conflicts_shows_info() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.worktree_sync.completed = vec![
            (SessionId::default(), git::SyncResult::Synced),
            (
                SessionId::default(),
                git::SyncResult::Conflict("merge conflict".into()),
            ),
        ];
        app.finish_sync();
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert!(msg.text.contains("1 synced"));
        assert!(msg.text.contains("1 conflict"));
    }

    #[test]
    fn finish_sync_errors_take_priority_over_conflicts() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.worktree_sync.completed = vec![
            (
                SessionId::default(),
                git::SyncResult::Conflict("merge conflict".into()),
            ),
            (
                SessionId::default(),
                git::SyncResult::Error("network error".into()),
            ),
        ];
        app.finish_sync();
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert!(msg.text.contains("network error"));
    }

    #[test]
    fn poll_git_stats_applies_result_to_matching_session() {
        let mut app = app_with_sessions(1);
        let sid = app.sessions[0].info.id;

        let tx = app.git_stats.start();
        let stats = crate::session::GitStats {
            files_changed: 3,
            insertions: 10,
            deletions: 2,
            untracked: 0,
            dirty: true,
            ahead: 1,
            behind: 0,
        };
        tx.send((sid, Some(stats.clone()))).unwrap();

        app.poll_git_stats();

        assert_eq!(app.sessions[0].info.git_stats, Some(stats));
        assert!(!app.git_stats.in_progress());
    }

    #[test]
    fn poll_git_stats_disconnected_clears_guard() {
        let mut app = app_with_sessions(1);
        drop(app.git_stats.start()); // worker died without delivering

        app.poll_git_stats();

        assert!(!app.git_stats.in_progress());
    }

    #[test]
    fn poll_git_stats_empty_is_noop() {
        let mut app = app_with_sessions(1);
        let _tx = app.git_stats.start();

        // No result yet: guard stays set, rx retained for the next poll.
        app.poll_git_stats();

        assert!(app.git_stats.in_progress());
    }

    #[test]
    fn poll_metrics_refresh_restores_sys_and_applies_metrics() {
        let mut app = app_with_sessions(1);
        let sid = app.sessions[0].info.id;

        // Simulate the worker having taken `sys`.
        app.metrics.sys = None;

        let tx = app.metrics_refresh.start();
        let agent_metrics = crate::session::AgentMetrics {
            model_display_name: Some("Opus".into()),
            ..Default::default()
        };
        tx.send(MetricsRefresh {
            sys: sysinfo::System::new(),
            metrics: crate::ui::info_panel::SystemMetrics {
                cpu_percent: 42.0,
                memory_used: 100,
                memory_total: 200,
                session_cpu_percent: 5.0,
                session_memory_bytes: 50,
            },
            agent_metrics: vec![(sid, agent_metrics)],
        })
        .unwrap();

        app.poll_metrics_refresh();

        assert!(app.metrics.sys.is_some());
        // `SystemMetrics` has no `PartialEq`; assert via a representative field.
        assert_eq!(app.metrics.system_metrics.cpu_percent, 42.0);
        assert_eq!(app.metrics.system_metrics.memory_used, 100);
        // `AgentMetrics` has no `PartialEq`; assert via a representative field.
        assert_eq!(
            app.sessions[0]
                .info
                .agent_metrics
                .as_ref()
                .and_then(|m| m.model_display_name.as_deref()),
            Some("Opus"),
        );
        assert!(!app.metrics_refresh.in_progress());
    }

    #[test]
    fn poll_metrics_refresh_disconnected_recreates_sys() {
        let mut app = app_with_sessions(0);
        app.metrics.sys = None;
        drop(app.metrics_refresh.start()); // worker died without returning `sys`

        app.poll_metrics_refresh();

        assert!(app.metrics.sys.is_some());
        assert!(!app.metrics_refresh.in_progress());
    }

    #[test]
    fn poll_worktree_create_continues_into_name_modal() {
        let mut app = app_with_sessions(0);
        let tx = app.worktree_create.start();
        let wt = WorktreeInfo {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.worktrees/feat"),
            branch: "feat".into(),
        };
        tx.send(Ok(vec![wt])).unwrap();
        app.pending_worktree_create = Some(PendingWorktreeCreate {
            backend: None,
            normal_repos: vec![PathBuf::from("/other")],
            session_name: None, // no name yet → routes through the name modal
            base_branch: "main".into(),
        });

        app.poll_worktree_create();

        assert!(!app.worktree_create.in_progress());
        assert!(app.pending_worktree_create.is_none());
        // The non-worktree normal repo is carried into additional dirs.
        assert_eq!(
            app.new_session.additional_dirs,
            vec![PathBuf::from("/other")]
        );
        assert!(matches!(app.modal, modals::Modal::SessionName(_)));
        assert!(app.new_session.spawn_config.is_some());
    }

    #[test]
    fn poll_worktree_create_error_sets_status() {
        let mut app = app_with_sessions(0);
        let tx = app.worktree_create.start();
        tx.send(Err("branch exists".into())).unwrap();
        app.pending_worktree_create = Some(PendingWorktreeCreate {
            backend: None,
            normal_repos: vec![],
            session_name: None,
            base_branch: "main".into(),
        });

        app.poll_worktree_create();

        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert!(msg.text.contains("branch exists"));
        assert!(!app.worktree_create.in_progress());
        assert!(app.pending_worktree_create.is_none());
    }

    #[test]
    fn poll_worktree_create_disconnected_clears_guard() {
        let mut app = app_with_sessions(0);
        drop(app.worktree_create.start());
        app.pending_worktree_create = Some(PendingWorktreeCreate {
            backend: None,
            normal_repos: vec![],
            session_name: None,
            base_branch: "main".into(),
        });

        app.poll_worktree_create();

        assert!(!app.worktree_create.in_progress());
        assert!(app.pending_worktree_create.is_none());
    }

    #[test]
    fn poll_session_spawn_error_sets_status_and_adds_no_session() {
        let mut app = app_with_sessions(0);
        let tx = app.session_spawn.start();
        tx.send(Err("tmux exploded".into())).unwrap();
        app.pending_session_spawn = Some(PendingSessionSpawn {
            primary_cwd: None,
            worktrees: vec![],
            additional_dirs: vec![],
            parent_session_id: None,
            task_prompt: None,
            agent: "codex".into(),
            base_branch: None,
        });

        app.poll_session_spawn();

        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert!(msg.text.contains("tmux exploded"));
        // The toast names the real agent, not a hardcoded "claude".
        assert!(msg.text.contains("codex"), "got {}", msg.text);
        assert!(app.sessions.is_empty());
        assert!(!app.session_spawn.in_progress());
        assert!(app.pending_session_spawn.is_none());
    }

    #[test]
    fn poll_session_spawn_disconnected_clears_guard() {
        let mut app = app_with_sessions(0);
        drop(app.session_spawn.start());
        app.pending_session_spawn = Some(PendingSessionSpawn {
            primary_cwd: None,
            worktrees: vec![],
            additional_dirs: vec![],
            parent_session_id: None,
            task_prompt: None,
            agent: "claude".into(),
            base_branch: None,
        });

        app.poll_session_spawn();

        assert!(!app.session_spawn.in_progress());
        assert!(app.pending_session_spawn.is_none());
    }

    #[tokio::test]
    async fn do_spawn_session_async_roundtrips_to_error_for_stub_backend() {
        // End-to-end: kick off the background spawn, let the blocking task run,
        // and confirm the failure (the stub backend refuses to spawn) is
        // surfaced via the poll path with no session added.
        let mut app = app_with_sessions(0);
        let config = SessionConfig::default();
        app.do_spawn_session_async("x".into(), &config, vec![]);
        assert!(app.session_spawn.in_progress());

        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            app.poll_session_spawn();
            if !app.session_spawn.in_progress() {
                break;
            }
        }

        assert!(!app.session_spawn.in_progress());
        assert!(app.sessions.is_empty());
        assert_eq!(
            app.status_message.as_ref().map(|m| m.level),
            Some(StatusLevel::Error),
        );
    }

    #[test]
    fn do_spawn_session_async_falls_back_to_sync_when_in_flight() {
        // With a spawn already in flight, a second request must not clobber the
        // pending continuation — it falls back to the synchronous path (which,
        // with the stub backend, fails to spawn and reports an error).
        let mut app = app_with_sessions(0);
        let _in_flight_tx = app.session_spawn.start();
        let config = SessionConfig::default();

        app.do_spawn_session_async("second".into(), &config, vec![]);

        // The in-flight task is untouched (no new background task kicked off).
        assert!(app.session_spawn.in_progress());
        // The synchronous fallback ran and surfaced the stub spawn failure.
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
    }

    #[test]
    fn drain_deferred_inputs_sends_at_correct_tick() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let id = SessionId::default();
        app.deferred_inputs.push((id, b"hello".to_vec(), 5));

        // Before target tick: nothing drained
        app.metrics.tick_count = 4;
        app.drain_deferred_inputs();
        assert_eq!(app.deferred_inputs.len(), 1);

        // At target tick: drained (no matching session, but entry is removed)
        app.metrics.tick_count = 5;
        app.drain_deferred_inputs();
        assert!(app.deferred_inputs.is_empty());
    }

    #[test]
    fn drain_deferred_inputs_retains_future_items() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let id = SessionId::default();
        app.deferred_inputs.push((id, b"early".to_vec(), 5));
        app.deferred_inputs.push((id, b"late".to_vec(), 20));

        app.metrics.tick_count = 5;
        app.drain_deferred_inputs();
        assert_eq!(app.deferred_inputs.len(), 1);
        assert_eq!(app.deferred_inputs[0].2, 20);
    }

    #[test]
    fn send_conflict_prompt_noop_for_unknown_session() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.send_conflict_prompt(SessionId::default());
        assert!(app.deferred_inputs.is_empty());
    }

    #[test]
    fn send_conflict_prompt_no_deferred_when_send_fails() {
        let mut app = app_with_sessions(1);
        let sid = app.sessions[0].info.id;

        // Stub's channel rx is dropped, so send_input fails.
        // No deferred input should be created.
        app.send_conflict_prompt(sid);
        assert!(app.deferred_inputs.is_empty());
    }

    #[test]
    fn poll_sync_results_triggers_finish_when_all_received() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let (tx, rx) = mpsc::channel();
        let id = SessionId::default();

        tx.send((id, git::SyncResult::Synced)).unwrap();
        drop(tx);

        app.worktree_sync.in_progress = true;
        app.worktree_sync.rx = Some(rx);
        app.worktree_sync.pending = 1;

        app.poll_sync_results();

        assert!(!app.worktree_sync.in_progress);
        assert!(app.worktree_sync.rx.is_none());
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Success);
    }

    #[test]
    fn poll_sync_results_waits_for_all_pending() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let (tx, rx) = mpsc::channel();

        tx.send((SessionId::default(), git::SyncResult::Synced))
            .unwrap();
        // Don't drop tx — second result hasn't arrived yet

        app.worktree_sync.in_progress = true;
        app.worktree_sync.rx = Some(rx);
        app.worktree_sync.pending = 2;

        app.poll_sync_results();

        // Still in progress — only 1 of 2 received
        assert!(app.worktree_sync.in_progress);
        assert!(app.worktree_sync.rx.is_some());
        assert_eq!(app.worktree_sync.completed.len(), 1);
    }

    #[test]
    fn poll_auto_update_surfaces_message_and_drops_receiver() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let (tx, rx) = mpsc::channel();
        tx.send("Updated to v9.9.9 — restart thurbox to apply.".to_string())
            .unwrap();
        app.set_auto_update_receiver(rx);

        app.poll_auto_update();

        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert!(msg.text.contains("v9.9.9"), "got: {}", msg.text);
        // One-shot: the receiver is dropped so we stop polling.
        assert!(app.auto_update_rx.is_none());
    }

    #[test]
    fn poll_auto_update_disconnected_drops_receiver() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let (tx, rx) = mpsc::channel::<String>();
        drop(tx); // worker finished with nothing to report (up-to-date / failed)
        app.set_auto_update_receiver(rx);

        app.poll_auto_update();

        // No toast, and the dead channel is dropped so we stop polling it.
        assert!(app.status_message.is_none());
        assert!(app.auto_update_rx.is_none());
    }

    #[test]
    fn poll_auto_update_empty_keeps_receiver() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let (tx, rx) = mpsc::channel::<String>();
        app.set_auto_update_receiver(rx);

        app.poll_auto_update();

        // Worker still running (sender alive, nothing sent yet): keep polling.
        assert!(app.status_message.is_none());
        assert!(app.auto_update_rx.is_some());
        drop(tx);
    }

    #[test]
    fn poll_auto_update_without_receiver_is_noop() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        // Feature off / dev build: no thread spawned, so no receiver attached.
        assert!(app.auto_update_rx.is_none());
        app.poll_auto_update();
        assert!(app.status_message.is_none());
    }

    #[test]
    fn poll_sync_results_finishes_when_a_worker_dies_without_sending_all() {
        // A panicked worker drops its sender without sending every result, so
        // `completed` can never reach `pending`. The channel disconnecting must
        // still finalize, or `in_progress` would be stuck forever.
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let (tx, rx) = mpsc::channel();

        tx.send((SessionId::default(), git::SyncResult::Synced))
            .unwrap();
        // Simulate the other worker panicking: its sender is gone, and so is
        // ours — the channel is now fully disconnected with 1 of 2 results.
        drop(tx);

        app.worktree_sync.in_progress = true;
        app.worktree_sync.rx = Some(rx);
        app.worktree_sync.pending = 2;

        app.poll_sync_results();

        assert!(!app.worktree_sync.in_progress);
        assert!(app.worktree_sync.rx.is_none());
    }

    #[test]
    fn format_time_ago_seconds() {
        let now = crate::sync::current_time_millis();
        assert_eq!(super::view::format_time_ago(now - 5_000), "5s ago");
        assert_eq!(super::view::format_time_ago(now - 30_000), "30s ago");
    }

    #[test]
    fn format_time_ago_minutes() {
        let now = crate::sync::current_time_millis();
        assert_eq!(super::view::format_time_ago(now - 120_000), "2m ago");
        assert_eq!(super::view::format_time_ago(now - 3_540_000), "59m ago");
    }

    #[test]
    fn format_time_ago_hours() {
        let now = crate::sync::current_time_millis();
        assert_eq!(super::view::format_time_ago(now - 3_600_000), "1h ago");
        assert_eq!(super::view::format_time_ago(now - 7_200_000), "2h ago");
    }

    #[test]
    fn format_time_ago_days() {
        let now = crate::sync::current_time_millis();
        assert_eq!(super::view::format_time_ago(now - 86_400_000), "1d ago");
        assert_eq!(super::view::format_time_ago(now - 259_200_000), "3d ago");
    }

    #[test]
    fn format_time_ago_future_timestamp() {
        let now = crate::sync::current_time_millis();
        // Future timestamp should saturate to 0s
        assert_eq!(super::view::format_time_ago(now + 10_000), "0s ago");
    }

    // --- parse_agent_metrics tests ---

    #[test]
    fn parse_agent_metrics_full_json() {
        let json = serde_json::json!({
            "version": "2.1.58",
            "model": { "id": "claude-opus-4-6", "display_name": "Opus 4.6" },
            "cost": {
                "total_cost_usd": 0.0123,
                "total_duration_ms": 5000,
                "total_api_duration_ms": 3000,
                "total_lines_added": 156,
                "total_lines_removed": 23,
            },
            "context_window": {
                "total_input_tokens": 15200,
                "total_output_tokens": 4500,
                "context_window_size": 200000,
                "used_percentage": 8,
                "current_usage": {
                    "input_tokens": 1200,
                    "output_tokens": 300,
                    "cache_creation_input_tokens": 5000,
                    "cache_read_input_tokens": 2000,
                }
            }
        });
        let m = super::parse_agent_metrics(&json);
        assert_eq!(m.model_id.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(m.model_display_name.as_deref(), Some("Opus 4.6"));
        assert!((m.total_cost_usd.unwrap() - 0.0123).abs() < 1e-6);
        assert_eq!(m.total_input_tokens, Some(15200));
        assert_eq!(m.total_output_tokens, Some(4500));
        assert_eq!(m.context_window_size, Some(200000));
        assert_eq!(m.used_percentage, Some(8));
        assert_eq!(m.total_lines_added, Some(156));
        assert_eq!(m.total_lines_removed, Some(23));
        assert_eq!(m.cache_read_input_tokens, Some(2000));
        assert_eq!(m.cache_creation_input_tokens, Some(5000));
        assert_eq!(m.cli_version.as_deref(), Some("2.1.58"));
    }

    #[test]
    fn parse_agent_metrics_empty_json() {
        let json = serde_json::json!({});
        let m = super::parse_agent_metrics(&json);
        assert!(m.model_id.is_none());
        assert!(m.total_cost_usd.is_none());
        assert!(m.used_percentage.is_none());
    }

    #[test]
    fn parse_agent_metrics_partial_json() {
        let json = serde_json::json!({
            "model": { "display_name": "Sonnet" },
            "cost": { "total_cost_usd": 0.05 }
        });
        let m = super::parse_agent_metrics(&json);
        assert_eq!(m.model_display_name.as_deref(), Some("Sonnet"));
        assert!(m.model_id.is_none());
        assert!((m.total_cost_usd.unwrap() - 0.05).abs() < 1e-6);
        assert!(m.total_input_tokens.is_none());
    }

    // --- find_matching_discovered tests ---

    fn make_shared_session(backend_id: &str, name: &str) -> sync::SharedSession {
        sync::SharedSession {
            id: crate::session::SessionId::default(),
            name: name.to_string(),
            agent: String::new(),
            backend_id: backend_id.to_string(),
            backend_type: "tmux".to_string(),
            agent_session_id: Some("agent-123".to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    fn make_discovered(
        backend_id: &str,
        name: &str,
        is_alive: bool,
    ) -> crate::agent::backend::DiscoveredSession {
        crate::agent::backend::DiscoveredSession {
            backend_id: backend_id.to_string(),
            name: name.to_string(),
            is_alive,
        }
    }

    #[test]
    fn find_matching_discovered_by_backend_id() {
        let shared = make_shared_session("thurbox:@0", "1");
        let discovered = vec![
            make_discovered("thurbox:@0", "tb-1", true),
            make_discovered("thurbox:@1", "tb-2", true),
        ];
        let result = App::find_matching_discovered(&shared, &discovered);
        assert!(result.is_some());
        assert_eq!(result.unwrap().backend_id, "thurbox:@0");
    }

    #[test]
    fn find_matching_discovered_by_name_fallback() {
        let shared = make_shared_session("", "1");
        let discovered = vec![
            make_discovered("thurbox:@5", "tb-1", true),
            make_discovered("thurbox:@6", "tb-2", true),
        ];
        let result = App::find_matching_discovered(&shared, &discovered);
        assert!(result.is_some());
        assert_eq!(result.unwrap().backend_id, "thurbox:@5");
    }

    #[test]
    fn find_matching_discovered_skips_dead() {
        let shared = make_shared_session("thurbox:@0", "1");
        let discovered = vec![make_discovered("thurbox:@0", "tb-1", false)];
        let result = App::find_matching_discovered(&shared, &discovered);
        assert!(result.is_none());
    }

    #[test]
    fn find_matching_discovered_no_match() {
        let shared = make_shared_session("thurbox:@99", "99");
        let discovered = vec![make_discovered("thurbox:@0", "tb-1", true)];
        let result = App::find_matching_discovered(&shared, &discovered);
        assert!(result.is_none());
    }

    #[test]
    fn find_matching_discovered_empty_list() {
        let shared = make_shared_session("thurbox:@0", "1");
        let result = App::find_matching_discovered(&shared, &[]);
        assert!(result.is_none());
    }

    // --- Background remote restore tests ---

    /// Stub remote backend for the background remote-restore tests: reports
    /// one live window (`%9`) and adopts it with inert I/O streams.
    struct RemoteStubBackend;
    impl SessionBackend for RemoteStubBackend {
        fn name(&self) -> &str {
            "ssh:test-host"
        }
        fn check_available(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn ensure_ready(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn spawn(
            &self,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<&Path>,
            _: &std::collections::HashMap<String, String>,
            _: u16,
            _: u16,
        ) -> anyhow::Result<crate::agent::backend::SpawnedSession> {
            anyhow::bail!("remote stub does not spawn")
        }
        fn adopt(
            &self,
            _: &str,
            _: u16,
            _: u16,
        ) -> anyhow::Result<crate::agent::backend::AdoptedSession> {
            Ok(crate::agent::backend::AdoptedSession {
                output: Box::new(std::io::empty()),
                input: Box::new(std::io::sink()),
            })
        }
        fn discover(&self) -> anyhow::Result<Vec<crate::agent::backend::DiscoveredSession>> {
            Ok(vec![make_discovered("%9", "tb-remote-sess", true)])
        }
        fn resize(&self, _: &str, _: u16, _: u16) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_dead(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        fn kill(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn detach(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn pane_pid(&self, _: &str) -> anyhow::Result<Option<u32>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn remote_sessions_restore_in_background() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let mut app = app_with_sessions(0);
        app.backends.register(Arc::new(RemoteStubBackend));

        let mut shared = make_shared_session("%9", "remote-sess");
        shared.backend_type = "ssh:test-host".to_string();
        let id = shared.id;

        app.restore_sessions(vec![shared], 1);

        // The first frame must not wait on the remote host: nothing is adopted
        // synchronously; discovery runs on a background thread.
        assert!(app.sessions.is_empty());
        assert!(app.remote_restore.is_some());

        // Drain like tick() would until the discovery thread reports.
        drain_remote_restore(&mut app);
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].info.id, id);
        assert_eq!(app.sessions[0].backend_name(), "ssh:test-host");
    }

    #[test]
    fn remote_session_on_unknown_host_is_left_unadopted() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let mut app = app_with_sessions(0);

        let mut shared = make_shared_session("%9", "remote-sess");
        shared.backend_type = "ssh:unknown-host".to_string();

        app.restore_sessions(vec![shared], 1);

        // Same contract as the old synchronous path: an unmanageable backend's
        // sessions are left un-adopted, and nothing stays pending.
        assert!(app.sessions.is_empty());
        assert!(app.remote_restore.is_none());
    }

    /// Drive [`App::poll_remote_restore`] until the background discovery
    /// thread reports and the pending state clears.
    fn drain_remote_restore(app: &mut App) {
        for _ in 0..500 {
            app.poll_remote_restore();
            if app.remote_restore.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("discovery result never drained");
    }

    #[tokio::test]
    async fn remote_restore_preserves_active_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let mut app = app_with_sessions(1);
        app.backends.register(Arc::new(RemoteStubBackend));
        let prior_id = app.sessions[0].info.id;

        let mut shared = make_shared_session("%9", "remote-sess");
        shared.backend_type = "ssh:test-host".to_string();
        app.restore_sessions(vec![shared], 1);
        drain_remote_restore(&mut app);

        // The late-arriving host's session lands in the list without stealing
        // the user's current selection.
        assert_eq!(app.sessions.len(), 2);
        assert_eq!(app.sessions[app.active_index].info.id, prior_id);
    }

    #[test]
    fn remote_restore_skips_session_adopted_meanwhile() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let mut app = app_with_sessions(0);
        app.backends.register(Arc::new(RemoteStubBackend));

        let mut shared = make_shared_session("%9", "remote-sess");
        shared.backend_type = "ssh:test-host".to_string();
        let id = shared.id;
        app.restore_sessions(vec![shared], 1);

        // Another path (e.g. the DB sync) adopts the session while discovery
        // is still in flight.
        let backend_arc = stub_backend_arc();
        let mut session = Session::stub("remote-sess", &backend_arc, &stub_provider());
        session.info.id = id;
        app.sessions.push(session);

        drain_remote_restore(&mut app);

        // The drain must not create a duplicate for the already-present id.
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].info.id, id);
    }

    // --- Modal flow tests ---

    #[test]
    fn handle_paste_clears_selection() {
        let mut app = app_with_sessions(1);
        app.text_selection = Some(Selection::new(
            TermPos { row: 0, col: 0 },
            PaneBounds::from_rect(ratatui::layout::Rect::new(0, 0, 80, 24)),
        ));
        app.selected_text_cache = Some("old".to_string());

        app.handle_paste("hello".to_string());

        assert!(app.text_selection.is_none());
        assert!(app.selected_text_cache.is_none());
    }

    #[test]
    fn paste_message_dispatches_to_handle_paste() {
        let mut app = app_with_sessions(1);
        app.text_selection = Some(Selection::new(
            TermPos { row: 0, col: 0 },
            PaneBounds::from_rect(ratatui::layout::Rect::new(0, 0, 80, 24)),
        ));

        app.update(AppMessage::Paste("pasted text".to_string()));

        assert!(app.text_selection.is_none());
    }

    #[test]
    fn send_paste_to_session_noop_when_no_sessions() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        // Should not panic with no active sessions
        app.send_paste_to_session("hello");
    }

    #[test]
    fn send_paste_to_session_noop_for_empty_text() {
        let mut app = app_with_sessions(1);
        // Should return early without error for empty text
        app.send_paste_to_session("");
    }

    /// Value of the focused text field in the current modal (for the
    /// paste-routing tests). `None` for modals without a text field.
    fn focused_modal_text(app: &App) -> Option<String> {
        let text = match &app.modal {
            modals::Modal::WorktreeName(wn) => wn.name.value(),
            modals::Modal::SessionName(sn) => sn.name.value(),
            modals::Modal::RepoPicker(rp) => match rp.focus {
                modals::RepoPickerFocus::Search => rp.search_input.value(),
                _ => rp.path_input.value(),
            },
            _ => return None,
        };
        Some(text.to_string())
    }

    #[test]
    fn paste_routes_into_modal_text_inputs() {
        // (modal, pasted, expected) — single-line fields strip embedded
        // newlines, so a pasted trailing newline must not survive.
        let repo_input = modals::Modal::RepoPicker(modals::RepoPickerModal {
            focus: modals::RepoPickerFocus::Input,
            ..Default::default()
        });
        let cases: Vec<(modals::Modal, &str, &str)> = vec![
            (
                modals::Modal::WorktreeName(Default::default()),
                "feature/x",
                "feature/x",
            ),
            (
                modals::Modal::SessionName(Default::default()),
                "my session\n",
                "my session",
            ),
            (repo_input, "/tmp/repo", "/tmp/repo"),
        ];

        for (modal, pasted, expected) in cases {
            let mut app = app_with_sessions(1);
            app.modal = modal;
            assert!(
                app.try_paste_into_modal_input(pasted),
                "paste must be consumed"
            );
            assert_eq!(focused_modal_text(&app).as_deref(), Some(expected));
        }
    }

    #[test]
    fn paste_into_selector_only_modal_is_swallowed_not_sent_to_terminal() {
        let mut app = app_with_sessions(1);
        app.modal = modals::Modal::ThemePicker(modals::ThemePickerModal {
            index: 0,
            original: crate::ui::theme::current(),
        });

        // A theme picker has no text field, but the paste must still be
        // consumed so it can't leak into the terminal behind the overlay.
        let consumed = app.try_paste_into_modal_input("oops");

        assert!(consumed);
    }

    #[test]
    fn paste_falls_through_to_terminal_when_no_modal() {
        let mut app = app_with_sessions(1);
        // No modal, terminal focus: paste is NOT consumed here so the caller
        // sends it to the session.
        app.focus = InputFocus::Terminal;
        assert!(!app.try_paste_into_modal_input("to terminal"));
    }

    #[test]
    fn paste_routes_into_in_pane_task_editor_description() {
        let mut app = app_with_sessions(1);
        let mut editor = modals::TaskEditorModal::new();
        editor.field = modals::TaskField::Description;
        app.task_ui.task_editor = Some(editor);
        app.focus = InputFocus::TaskEditor;

        // The description is multi-line, so a pasted newline is preserved.
        let consumed = app.try_paste_into_modal_input("line one\nline two");

        assert!(consumed);
        let editor = app.task_ui.task_editor.as_ref().unwrap();
        assert_eq!(editor.description.value(), "line one\nline two");
    }

    // --- key_handlers: modal open/close + pane chords driven via handle_key ---

    #[test]
    fn ctrl_y_opens_theme_picker_then_j_and_enter_persists() {
        let mut app = app_with_sessions(1);
        app.handle_key(KeyCode::Char('y'), KeyModifiers::CONTROL);
        let presets = crate::session::ThemePreset::all();
        match app.modal {
            modals::Modal::ThemePicker(ref tp) => assert_eq!(tp.index, 0),
            ref other => panic!("expected the theme picker, got {other:?}"),
        }
        // `j` previews the next preset; `Enter` commits + persists it.
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::None));
        assert_eq!(app.active_theme.name, presets[1].as_str());
        assert_eq!(
            app.db.get_active_theme().unwrap().as_deref(),
            Some(presets[1].as_str())
        );
        // The active palette is process-global; restore the default so this
        // test doesn't leak into others (matching `set_active_switches_palette`).
        crate::ui::theme::set_active(crate::session::ThemePalette::default());
    }

    #[test]
    fn theme_picker_esc_closes_without_persisting() {
        let mut app = app_with_sessions(1);
        app.handle_key(KeyCode::F(4), KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::ThemePicker(_)));
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::None));
        // Esc doesn't write a theme choice to the DB.
        assert!(app.db.get_active_theme().unwrap().is_none());
    }

    #[test]
    fn ctrl_u_opens_restore_sessions_modal_and_esc_closes() {
        let mut app = app_with_sessions(1);
        // Empty DB → an empty (but open) restore modal.
        app.handle_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
        match app.modal {
            modals::Modal::RestoreSessions(ref rs) => assert!(rs.list.is_empty()),
            ref other => panic!("expected the restore-sessions modal, got {other:?}"),
        }
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::None));
    }

    #[test]
    fn branch_selector_esc_closes_and_clears_pending_repo_state() {
        let mut app = app_with_sessions(1);
        app.new_session.repo_path = Some(PathBuf::from("/repo"));
        app.new_session.all_repos = Some(vec![PathBuf::from("/repo")]);
        app.new_session.normal_repos = vec![PathBuf::from("/other")];
        app.modal = modals::Modal::BranchSelector(modals::BranchSelectorModal {
            index: 0,
            branches: vec!["main".into(), "dev".into()],
        });
        // j advances the selection; Esc aborts and wipes the pending spawn state.
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        match app.modal {
            modals::Modal::BranchSelector(ref bs) => assert_eq!(bs.index, 1),
            ref other => panic!("expected the branch selector, got {other:?}"),
        }
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.modal, modals::Modal::None));
        assert!(app.new_session.repo_path.is_none());
        assert!(app.new_session.all_repos.is_none());
        assert!(app.new_session.normal_repos.is_empty());
    }

    #[test]
    fn task_list_jk_navigates_selection() {
        let mut app = app_with_sessions(1);
        for t in ["one", "two", "three"] {
            app.db
                .create_task(&crate::storage::tasks::NewTask::local(t))
                .unwrap();
        }
        app.refresh_tasks();
        app.focus = InputFocus::TaskList;
        app.task_ui.task_panel_index = 0;
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.task_ui.task_panel_index, 1);
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.task_ui.task_panel_index, 2);
        // j at the last row stays put (no wrap).
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.task_ui.task_panel_index, 2);
        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.task_ui.task_panel_index, 1);
    }

    #[test]
    fn task_list_space_cycles_selected_status() {
        let mut app = app_with_sessions(1);
        let id = app
            .db
            .create_task(&crate::storage::tasks::NewTask::local("t"))
            .unwrap();
        app.refresh_tasks();
        app.focus = InputFocus::TaskList;
        app.task_ui.task_panel_index = 0;
        assert_eq!(
            app.db.get_task(id).unwrap().unwrap().status,
            crate::session::TaskStatus::Todo
        );
        app.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(
            app.db.get_task(id).unwrap().unwrap().status,
            crate::session::TaskStatus::InProgress
        );
    }

    #[test]
    fn task_list_d_soft_deletes_selected() {
        let mut app = app_with_sessions(1);
        app.db
            .create_task(&crate::storage::tasks::NewTask::local("doomed"))
            .unwrap();
        app.refresh_tasks();
        app.focus = InputFocus::TaskList;
        app.task_ui.task_panel_index = 0;
        app.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(
            app.db.list_tasks().unwrap().is_empty(),
            "d should soft-delete the selected task"
        );
    }

    #[test]
    fn task_list_esc_returns_to_session_list() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::TaskList;
        app.task_ui.task_editor = Some(modals::TaskEditorModal::new());
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.focus, InputFocus::SessionList);
        assert!(
            app.task_ui.task_editor.is_none(),
            "leaving the panel clears the editor"
        );
    }
}
