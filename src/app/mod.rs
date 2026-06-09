mod automation_state;
mod helpers;
mod key_handlers;
mod metrics_state;
pub(crate) mod modals;
mod new_session_state;
pub(crate) mod search;
mod state;
mod sync_state;
mod task_state;
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
    AgentDef, AgentRegistry, Automation, AutomationAction, AutomationRunStatus, AutomationSchedule,
    SessionConfig, SessionId, SessionInfo, SessionStatus, WorktreeInfo, DEFAULT_AGENT_NAME,
};
use crate::storage::Database;
use crate::storage::DeletedSessionInfo;
use crate::sync::{self, SharedWorktree, StateDelta, SyncState};
use crate::ui::layout;
use crate::ui::scrollbar::ScrollbarGeom;
use crate::ui::selection::{PaneBounds, Selection, TermPos};

const MOUSE_SCROLL_LINES: usize = 3;

/// How long the user has to press Ctrl+Z to undo a session delete.
const UNDO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long a status-bar message is shown before reverting to default counts.
const STATUS_MESSAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// If no output for this many milliseconds, consider session "Waiting".
const ACTIVITY_TIMEOUT_MS: u64 = 1000;

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
    /// A task-initiated spawn's `(task_id, title)`, captured at kickoff so the
    /// prompt is delivered + the task advanced when the session comes up.
    task_prompt: Option<(i64, String)>,
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
        match git::create_worktree_on(host, repo_path, new_branch, base_branch) {
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

/// Result of a background system-metrics refresh, delivered over `metrics_rx`.
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
}

/// One scrollbar rendered this frame: its geometry plus the scroll state it
/// drives. Built in [`App::view`], hit-tested by the mouse handlers.
pub(crate) struct ScrollbarHit {
    pub(crate) geom: ScrollbarGeom,
    pub(crate) target: ScrollTarget,
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
    /// The global search strip docked along the bottom (`Ctrl+A` by default).
    /// Captures all input while active; entered/left only via its keybinding /
    /// `Esc`.
    GlobalSearch,
    Terminal,
    FileViewer,
}

/// Which pane the terminal view is showing for a given session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalView {
    Claude,
    Shell,
}

/// Holds a recently deleted session for undo (Ctrl+Z) support.
struct PendingDelete {
    session: Session,
    session_id: SessionId,
    created_at: std::time::Instant,
}

/// Result sent by a background container/VM restore thread on completion.
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
    pub(crate) show_info_panel: bool,
    /// Whether the tasks panel column is shown (toggled like the file viewer).
    pub(crate) show_tasks_panel: bool,
    pub(crate) show_file_viewer: bool,
    pub(crate) file_viewer: crate::ui::file_viewer::FileViewerState,
    pub(crate) modal: modals::Modal,
    // (Delete project, add project, edit project modal state is now in self.modal)
    /// In-progress new-session wizard (also drives fork/restart re-spawns).
    pub(crate) new_session: new_session_state::NewSessionWizardState,
    /// Inter-instance DB sync (polls for changes from other thurbox instances).
    sync_state: SyncState,
    /// Worktree-to-main git sync (Ctrl+S).
    worktree_sync: sync_state::WorktreeSyncState,
    /// System/process metrics + the tick counter pacing periodic refreshes.
    metrics: metrics_state::MetricsState,
    /// A background system-metrics refresh is in flight (guards `sys` ownership
    /// so refreshes never overlap).
    metrics_refresh_in_progress: bool,
    /// Receives the result of a background system-metrics refresh, polled each
    /// tick. Mirrors `worktree_sync.rx`.
    metrics_rx: Option<mpsc::Receiver<MetricsRefresh>>,
    /// A background active-session git-stats refresh is in flight.
    git_stats_in_progress: bool,
    /// Receives the result of a background git-stats refresh, polled each tick.
    git_stats_rx: Option<mpsc::Receiver<(SessionId, Option<crate::session::GitStats>)>>,
    /// A background worktree-creation (`git worktree add`) is in flight for the
    /// new-session wizard; guards against re-entry and clobbering the pending
    /// continuation.
    worktree_create_in_progress: bool,
    /// Receives the result of a background worktree-creation, polled each tick.
    worktree_create_rx: Option<mpsc::Receiver<Result<Vec<WorktreeInfo>, String>>>,
    /// Continuation for a completed worktree-creation: the wizard inputs needed
    /// to resume the spawn flow once the worktrees exist.
    pending_worktree_create: Option<PendingWorktreeCreate>,
    /// A background `Session::spawn` (PTY/tmux window creation) is in flight for
    /// the interactive new-session flow. Programmatic spawns stay synchronous.
    session_spawn_in_progress: bool,
    /// Receives the spawned [`Session`] (or an error) from the background
    /// `Session::spawn`, polled each tick.
    session_spawn_rx: Option<mpsc::Receiver<Result<Session, String>>>,
    /// Continuation for a completed background spawn: the metadata + follow-up
    /// (task prompt) to apply once the session is live.
    pending_session_spawn: Option<PendingSessionSpawn>,
    /// Deferred inputs: `(session_id, data, tick_at_which_to_send)`.
    /// Used to introduce a small delay between pasting text and pressing Enter.
    deferred_inputs: Vec<(SessionId, Vec<u8>, u64)>,
    /// Per-session terminal view state (Claude vs Shell). Defaults to Claude.
    session_terminal_views: HashMap<SessionId, TerminalView>,
    /// Recently deleted session awaiting finalization or undo (Ctrl+Z).
    pending_delete: Option<PendingDelete>,
    // (Restore sessions modal state is now in self.modal)
    /// Active text selection (click+drag), uses screen-absolute coordinates.
    pub(crate) text_selection: Option<Selection>,
    /// Scrollbars rendered this frame, with the scroll state each drives.
    /// Cleared and rebuilt every [`App::view`]; hit-tested by the mouse handlers
    /// so a click/drag on a track scrolls the owning pane.
    pub(crate) scrollbar_hits: Vec<ScrollbarHit>,
    /// The scrollbar currently being dragged, if any. Set when a click lands on
    /// a track, cleared on mouse-up, so drags keep driving the same pane.
    pub(crate) dragging_scrollbar: Option<ScrollTarget>,
    /// Cached text extracted from the frame buffer for the current selection.
    selected_text_cache: Option<String>,
    /// Persistent clipboard handle to avoid "dropped too quickly" warnings on Linux.
    clipboard: Option<arboard::Clipboard>,
    /// Reusable buffer for session elapsed-ms in the view (avoids per-frame allocation).
    pub(crate) session_elapsed_buf: Vec<u64>,
    /// Persistent list state for the session section (preserves scroll offset).
    pub(crate) session_list_state: ratatui::widgets::ListState,
    /// Automations-pane UI state (cached list, selection, run history, editor).
    pub(crate) automation_ui: automation_state::AutomationUiState,
    /// Tasks-panel UI state (cached list, selection, editor, links).
    pub(crate) task_ui: task_state::TaskUiState,
    /// Global search strip (`Ctrl+A`): cross-scope search docked at the bottom.
    pub(crate) global_search: search::GlobalSearchState,
    /// Currently active theme preset, cached so the header doesn't hit SQLite
    /// every render. Kept in sync with `db.set_active_theme` writes.
    pub(crate) active_theme: crate::session::ThemePreset,
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
}

const EDITOR_NOT_CONFIGURED: &str =
    "No editor configured — set `editor_command` via MCP or export $EDITOR/$VISUAL";

impl App {
    pub fn new(
        rows: u16,
        cols: u16,
        backends: BackendRegistry,
        agents: AgentRegistry,
        db: Database,
    ) -> Self {
        // Resolve the persisted active theme (defaults to Default if unset/unknown).
        let active_theme = db
            .get_active_theme()
            .ok()
            .flatten()
            .as_deref()
            .and_then(crate::session::ThemePreset::from_str)
            .unwrap_or(crate::session::ThemePreset::Default);

        // Load keybindings from JSON config or fall back to defaults.
        let keybindings = match crate::storage::keybindings::load_keybindings_json() {
            Ok(Some(json)) => crate::session::KeyBindings::from_json(&json).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse keybindings.json: {e}; using defaults");
                crate::session::KeyBindings::default()
            }),
            Ok(None) => crate::session::KeyBindings::default(),
            Err(e) => {
                tracing::warn!("Failed to read keybindings.json: {e}; using defaults");
                crate::session::KeyBindings::default()
            }
        };

        // Load session counter from DB
        let session_counter = db.get_session_counter().unwrap_or(0);

        let mut sync_state = SyncState::new();

        // Initialize the sync snapshot from the current DB state so the first
        // poll doesn't produce a false delta treating everything as "added".
        if let Ok(initial_state) = db.load_shared_state() {
            sync_state.set_initial_snapshot(initial_state);
        }

        let (usage_tx, usage_rx) = mpsc::channel();

        Self {
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
            show_info_panel: false,
            show_tasks_panel: false,
            show_file_viewer: false,
            file_viewer: crate::ui::file_viewer::FileViewerState::new(),
            modal: modals::Modal::None,
            new_session: new_session_state::NewSessionWizardState::default(),
            sync_state,
            worktree_sync: sync_state::WorktreeSyncState::default(),
            metrics: metrics_state::MetricsState::new(),
            metrics_refresh_in_progress: false,
            metrics_rx: None,
            git_stats_in_progress: false,
            git_stats_rx: None,
            worktree_create_in_progress: false,
            worktree_create_rx: None,
            pending_worktree_create: None,
            session_spawn_in_progress: false,
            session_spawn_rx: None,
            pending_session_spawn: None,
            deferred_inputs: Vec::new(),
            session_terminal_views: HashMap::new(),
            pending_delete: None,
            text_selection: None,
            scrollbar_hits: Vec::new(),
            dragging_scrollbar: None,
            selected_text_cache: None,
            clipboard: arboard::Clipboard::new().ok(),
            session_elapsed_buf: Vec::new(),
            session_list_state: ratatui::widgets::ListState::default(),
            automation_ui: automation_state::AutomationUiState::default(),
            task_ui: task_state::TaskUiState::default(),
            global_search: search::GlobalSearchState::default(),
            active_theme,
            keybindings,
            usage: HashMap::new(),
            usage_tx,
            usage_rx,
        }
    }

    /// Build an [`AgentProvider`](crate::agent::AgentProvider) for a session
    /// config by looking its agent up in the registry. Falls back to the
    /// registry default, then to the built-in default, so a stale/unknown agent
    /// name never breaks spawning.
    pub fn provider_for(&self, config: &SessionConfig) -> Arc<dyn crate::agent::AgentProvider> {
        Arc::new(GenericProvider::new(self.agent_def_for(&config.agent)))
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
    /// When remote hosts are configured (`hosts.toml`), first shows the host
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
                label: format!("{}  ({})", host.name, host.destination),
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
    /// Loads bookmarks from the database, pre-selects repos from the active
    /// project (if any), and shows the repo picker modal. For a remote target
    /// (`new_session.backend` set) local bookmarks don't apply, so the picker opens
    /// empty with the path input focused for a typed remote path.
    pub(crate) fn open_repo_picker(&mut self) {
        // Local bookmarks point at local paths, so they're meaningless for a
        // remote target: open the picker empty with the path input focused.
        let remote = self.new_session.backend.is_some();
        let bookmarks = if remote {
            Vec::new()
        } else {
            self.load_repo_bookmarks()
        };
        let mut rp = modals::RepoPickerModal::default();
        Self::rebuild_repo_picker_rows(&mut rp, bookmarks);
        if remote {
            rp.focus = modals::RepoPickerFocus::Input;
        }
        self.modal = modals::Modal::RepoPicker(rp);
    }

    /// Load persisted repo bookmarks, logging (and swallowing) any DB error.
    fn load_repo_bookmarks(&self) -> Vec<crate::storage::repo_bookmarks::RepoBookmark> {
        match self.db.list_repo_bookmarks() {
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
        // Rebuild the process cwd: the primary repo for a single-repo session,
        // or the (idempotently rebuilt) symlink workspace for a multi-repo one.
        let cwd = session_process_cwd(&session.info);

        let mut config = SessionConfig {
            resume_session_id: None,
            agent_session_id: Some(agent_session_id.clone()),
            cwd,
            agent,
            fork_session_id: None,
            ..SessionConfig::default()
        };
        let def = self.agent_def_for(&config.agent);
        config.resume_session_id =
            crate::session_ops::resume_trigger_for(&def, &agent_session_id, &config.env);

        self.do_restart(config);
    }

    /// Execute the actual restart with the finalized config.
    fn do_restart(&mut self, config: SessionConfig) {
        let (rows, cols) = self.content_area_size();
        let session = &mut self.sessions[self.active_index];
        match session.restart(&config, rows, cols) {
            Ok(()) => {
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

        // Soft-delete in DB
        if let Err(e) = self.db.soft_delete_session(session_id) {
            error!("Failed to soft-delete session in DB: {e}");
        }

        // Remove from the session list (do NOT kill backend or remove worktrees yet)
        let removed_session = self.sessions.remove(self.active_index);
        let session_name = removed_session.info.name.clone();

        // Clean up terminal view state
        self.session_terminal_views.remove(&session_id);

        self.sync_active_session_to_project();

        // Finalize any existing pending delete before storing the new one
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

    /// Open the restore deleted sessions modal (Ctrl+U).
    /// Open the theme picker, pre-selecting the currently active preset.
    fn open_theme_picker(&mut self) {
        let active = self.db.get_active_theme().ok().flatten();
        let presets = crate::session::ThemePreset::all();
        let index = active
            .as_deref()
            .and_then(crate::session::ThemePreset::from_str)
            .and_then(|p| presets.iter().position(|x| *x == p))
            .unwrap_or(0);
        self.modal = modals::Modal::ThemePicker(modals::ThemePickerModal { index });
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
    fn restore_deleted_session(&mut self, deleted: DeletedSessionInfo) {
        if let Err(e) = self.db.restore_session(deleted.id) {
            error!("Failed to restore session in DB: {e}");
            self.set_error("Failed to restore session");
            return;
        }

        let worktree_infos = Self::recreate_worktrees(&deleted.worktrees);
        let cwd = worktree_infos
            .first()
            .map(|wt| wt.worktree_path.clone())
            .or(deleted.cwd.clone());

        let config = SessionConfig {
            resume_session_id: deleted.agent_session_id.clone(),
            agent_session_id: deleted.agent_session_id,
            cwd,
            agent: deleted.agent,
            fork_session_id: None,
            ..SessionConfig::default()
        };

        let session_name = deleted.name.clone();
        let (rows, cols) = self.content_area_size();
        let provider = self.provider_for(&config);

        match Session::spawn(
            session_name.clone(),
            rows,
            cols,
            &config,
            self.backends.default_backend(),
            &provider,
        ) {
            Ok(mut session) => {
                session.info.id = deleted.id;
                session.info.worktrees = worktree_infos;
                resolve_repo_display_names(&mut session.info);
                self.sessions.push(session);
                self.active_index = self.sessions.len() - 1;
                self.focus = InputFocus::Terminal;

                self.save_state();

                self.set_status(StatusLevel::Success, format!("Restored '{session_name}'"));
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
        resolve_repo_display_names(&mut session.info);
    }

    pub fn update(&mut self, msg: AppMessage) {
        match msg {
            AppMessage::KeyPress(code, mods) => self.handle_key(code, mods),
            AppMessage::Paste(text) => self.handle_paste(text),
            AppMessage::MouseScrollUp { x, y } => self.handle_mouse_scroll(x, y, true),
            AppMessage::MouseScrollDown { x, y } => self.handle_mouse_scroll(x, y, false),
            AppMessage::MouseClick { x, y, modifiers } => self.handle_mouse_click(x, y, modifiers),
            AppMessage::MouseDrag { x, y } => self.handle_mouse_drag(x, y),
            AppMessage::MouseUp { x, y } => self.handle_mouse_up(x, y),
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
        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };
        let session_id = session.info.id;
        let needs_shell = session.shell_pane.is_none();

        let current = self
            .session_terminal_views
            .get(&session_id)
            .copied()
            .unwrap_or(TerminalView::Claude);

        match current {
            TerminalView::Claude => {
                // Lazily create the shell pane if it doesn't exist. Start it in
                // the same launch cwd as the agent (the multi-repo workspace when
                // there is one), so switching to the terminal lands you there.
                if needs_shell {
                    let (rows, cols) = self.content_area_size();
                    let shell_cwd = session_process_cwd(&self.sessions[self.active_index].info);
                    let session = &mut self.sessions[self.active_index];
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
            TerminalView::Shell => {
                self.session_terminal_views
                    .insert(session_id, TerminalView::Claude);
            }
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
        use crate::ui::links;

        let area = Rect::new(0, 0, self.terminal_cols, self.terminal_rows);
        let areas = layout::compute_layout(
            area,
            self.show_info_panel,
            self.show_tasks_panel,
            self.show_file_viewer,
            self.global_search.active,
            self.automation_ui.cached_automations.len(),
        );
        let border_block = Block::default().borders(Borders::ALL);

        // Ctrl+Click: URL opening (terminal-relative, existing behavior)
        if modifiers.contains(KeyModifiers::CONTROL) {
            self.text_selection = None;
            let inner = border_block.inner(areas.terminal);

            if inner.contains(Position::new(x, y)) {
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
            return;
        }

        // Grab a scrollbar thumb: a click on any rendered track starts a drag of
        // that pane's scroll state (and never starts a text selection).
        if let Some(hit) = self.scrollbar_hits.iter().find(|h| h.geom.contains(x, y)) {
            let target = hit.target;
            let pos = hit.geom.position_for_y(y);
            let content_len = hit.geom.content_len;
            self.text_selection = None;
            self.dragging_scrollbar = Some(target);
            self.apply_scrollbar_position(target, pos, content_len);
            return;
        }

        // Find which pane was clicked; use inner area (excluding borders).
        let pos = Position::new(x, y);
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
        }
    }

    /// Route a mouse-wheel tick to whichever pane is under the cursor, so the
    /// wheel scrolls the hovered pane (terminal, task preview, file viewer, run
    /// history, or a list pane) rather than always the terminal.
    fn handle_mouse_scroll(&mut self, x: u16, y: u16, up: bool) {
        let step: i32 = if up { -1 } else { 1 };
        match self.pane_at(x, y) {
            Some(ScrollPane::Terminal) | None => {
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
        }
    }

    /// Step the run-history selection by `delta`, clamped to the run count.
    fn move_run_history_selection(&mut self, delta: i32) {
        let len = self.automation_ui.cached_automation_runs.len();
        if len == 0 {
            self.automation_ui.automation_run_index = 0;
            return;
        }
        let next =
            (self.automation_ui.automation_run_index as i32 + delta).clamp(0, len as i32 - 1);
        self.automation_ui.automation_run_index = next as usize;
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

    /// Step the automations-pane selection by `delta`, clamped, refreshing the
    /// preview.
    fn move_automation_selection(&mut self, delta: i32) {
        let len = self.automation_ui.cached_automations.len();
        if len == 0 {
            return;
        }
        let next =
            (self.automation_ui.automation_panel_index as i32 + delta).clamp(0, len as i32 - 1);
        let next = next as usize;
        if next != self.automation_ui.automation_panel_index {
            self.automation_ui.automation_panel_index = next;
            self.refresh_automation_view();
        }
    }

    /// Hit-test `(x, y)` against the current layout to find the scrollable pane
    /// under the cursor (used for pane-aware wheel scrolling).
    fn pane_at(&self, x: u16, y: u16) -> Option<ScrollPane> {
        let area = Rect::new(0, 0, self.terminal_cols, self.terminal_rows);
        let areas = layout::compute_layout(
            area,
            self.show_info_panel,
            self.show_tasks_panel,
            self.show_file_viewer,
            self.global_search.active,
            self.automation_ui.cached_automations.len(),
        );
        let pos = Position::new(x, y);
        let hit = |r: Option<Rect>| r.map(|r| r.contains(pos)).unwrap_or(false);

        if hit(areas.file_viewer) {
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
                _ => ScrollPane::Terminal,
            });
        }
        None
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

    /// Route pasted text into the focused modal text input when one is open.
    /// Returns `true` when consumed, signalling the caller to skip the default
    /// "send to session" behaviour. New modals with text inputs should add
    /// their target here so paste works inside them.
    fn try_paste_into_modal_input(&mut self, _text: &str) -> bool {
        false
    }

    pub(crate) fn spawn_worktree_session(
        &mut self,
        repo_paths: &[PathBuf],
        new_branch: &str,
        base_branch: &str,
        session_name: Option<String>,
    ) {
        if self.worktree_create_in_progress {
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
        let (tx, rx) = mpsc::channel();
        self.worktree_create_in_progress = true;
        self.worktree_create_rx = Some(rx);
        self.pending_worktree_create = Some(PendingWorktreeCreate {
            backend,
            normal_repos,
            session_name,
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
        let Some(rx) = &self.worktree_create_rx else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.worktree_create_rx = None;
                self.worktree_create_in_progress = false;
                self.pending_worktree_create = None;
                self.set_error("Worktree creation failed (worker died)");
                return;
            }
        };
        self.worktree_create_rx = None;
        self.worktree_create_in_progress = false;
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
        // `get_by_backend` already returns `None` for non-`ssh:` names.
        self.hosts.get_by_backend(backend?)
    }

    /// Resolve the backend for a *persisted* session by its `backend_type`, or
    /// `None` when this instance cannot manage it.
    ///
    /// An unknown `ssh:<host>` backend (e.g. a host this instance hasn't loaded
    /// from `hosts.toml`, or another instance configured) is **skipped** rather
    /// than falling back to local — adopting a remote session on the local
    /// backend would corrupt its `backend_type` and risk a pane-id collision
    /// (tmux numbers panes `%N` per server, so a remote `%1` can match an
    /// unrelated local `%1`). Legacy/local values (empty, `tmux`, `local-tmux`)
    /// still fall back to the default local backend.
    pub(crate) fn resolve_persisted_backend(
        &self,
        backend_type: &str,
    ) -> Option<Arc<dyn SessionBackend>> {
        if let Some(b) = self.backends.get(backend_type) {
            return Some(b.clone());
        }
        if crate::session::is_ssh_backend(backend_type) {
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
        if config.agent_session_id.is_none() {
            config.agent_session_id = Some(uuid::Uuid::new_v4().to_string());
        }

        // Inject statusline env vars so the metrics script knows which session this is.
        if let Some(ref sid) = config.agent_session_id {
            config.env.insert("THURBOX_SESSION_ID".into(), sid.clone());
        }
        if let Some(metrics_dir) = crate::paths::metrics_directory() {
            config.env.insert(
                "THURBOX_METRICS_DIR".into(),
                metrics_dir.to_string_lossy().into(),
            );
        }

        // For a multi-repo session, launch the agent in a symlink workspace that
        // gathers every member dir; `info.cwd` keeps the primary repo (restored
        // after spawn). Single-repo sessions are unchanged.
        let primary_cwd = config.cwd.clone();
        config.cwd = resolve_process_cwd(
            config.agent_session_id.as_deref(),
            primary_cwd.clone(),
            worktrees,
            additional_dirs,
        );

        let backend = match self.backend_for(&config) {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to select backend: {e}");
                self.set_error(e);
                return None;
            }
        };

        let provider = self.provider_for(&config);
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
    fn finalize_spawned_session(
        &mut self,
        mut session: Session,
        primary_cwd: Option<PathBuf>,
        worktrees: Vec<WorktreeInfo>,
        additional_dirs: Vec<PathBuf>,
        task_prompt: Option<(i64, String)>,
    ) {
        session.info.cwd = primary_cwd;
        session.info.worktrees = worktrees;
        session.info.additional_dirs = additional_dirs;

        resolve_repo_display_names(&mut session.info);
        self.sessions.push(session);
        self.active_index = self.sessions.len() - 1;
        self.focus = InputFocus::Terminal;
        self.status_message = None;

        self.save_state();

        // A task-initiated spawn (the trigger-time picker's "Spawn new session")
        // delivers the task title once the agent has booted, then advances the
        // task to in progress.
        if let Some((task_id, title)) = task_prompt {
            let new_id = self.sessions[self.active_index].info.id;
            // Record the link now — the session was named by the user, so the
            // `task-<id>` convention can't recover it later.
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
                    task_prompt,
                );
            }
            Err(e) => {
                error!("Failed to spawn session: {e}");
                self.set_error(format!("Failed to start claude: {e:#}"));
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
        if self.session_spawn_in_progress {
            self.do_spawn_session(name, config, worktrees);
            return;
        }

        let additional_dirs = std::mem::take(&mut self.new_session.additional_dirs);
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

        let (tx, rx) = mpsc::channel();
        self.session_spawn_in_progress = true;
        self.session_spawn_rx = Some(rx);
        self.pending_session_spawn = Some(PendingSessionSpawn {
            primary_cwd,
            worktrees,
            additional_dirs,
            task_prompt,
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
        let Some(rx) = &self.session_spawn_rx else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.session_spawn_rx = None;
                self.session_spawn_in_progress = false;
                self.pending_session_spawn = None;
                self.set_error("Session spawn failed (worker died)");
                return;
            }
        };
        self.session_spawn_rx = None;
        self.session_spawn_in_progress = false;
        let Some(pending) = self.pending_session_spawn.take() else {
            return;
        };

        match result {
            Ok(session) => self.finalize_spawned_session(
                session,
                pending.primary_cwd,
                pending.worktrees,
                pending.additional_dirs,
                pending.task_prompt,
            ),
            Err(e) => {
                error!("Failed to spawn session: {e}");
                self.set_error(format!("Failed to start claude: {e}"));
            }
        }
    }

    /// Sync `active_index` to the active project's first session, or invalidate
    /// it when the project has no sessions.
    /// Clamp the active session index to the valid range after a session is removed.
    pub(crate) fn sync_active_session_to_project(&mut self) {
        if self.sessions.is_empty() {
            self.active_index = 0;
        } else if self.active_index >= self.sessions.len() {
            self.active_index = self.sessions.len() - 1;
        }
    }

    /// Indices into `self.sessions` in the order they are rendered.
    ///
    /// Delegates to the same `ui::project_list::compute_session_order` the
    /// rendering widget uses, so `Ctrl+J`/`Ctrl+K` step through the list in the
    /// exact order the user sees (repo groups ordered by activity/status).
    fn render_order_indices(&self) -> Vec<usize> {
        let infos: Vec<&crate::session::SessionInfo> =
            self.sessions.iter().map(|s| &s.info).collect();
        crate::ui::project_list::compute_session_order(&infos).order
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

        // Run the debounced global-search content scan once the query has been
        // settled for the debounce window (Instant-based, since tick cadence is
        // event-load-dependent).
        if self.global_search.active && self.global_search.content_dirty {
            let settled = self
                .global_search
                .query_changed_at
                .map(|t| {
                    t.elapsed() >= std::time::Duration::from_millis(search::CONTENT_DEBOUNCE_MS)
                })
                .unwrap_or(false);
            if settled {
                self.recompute_global_search_content();
                self.global_search.content_dirty = false;
            }
        }

        self.refresh_session_statuses();

        // Poll for sync results from background worktree sync threads
        self.poll_sync_results();

        // Poll for backgrounded interactive spawn work (worktree creation +
        // `Session::spawn`) so `Ctrl+N` never freezes the UI.
        self.poll_worktree_create();
        self.poll_session_spawn();

        // Send deferred inputs whose delay has elapsed
        self.drain_deferred_inputs();

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

        // Apply completed background metric/git-stat refreshes, then kick off
        // the next one on its cadence. Both run off the UI thread (sysinfo +
        // statusline file reads / `git` shell-outs) so a slow read never stalls
        // rendering — mirrors the worktree-sync poll above.
        self.poll_metrics_refresh();
        self.poll_git_stats();

        if self.metrics.tick_count % METRICS_REFRESH_TICKS == 0 {
            self.start_metrics_refresh();
        }
        if self.metrics.tick_count % GIT_REFRESH_TICKS == 0 {
            self.start_git_stats_refresh();
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
    fn refresh_session_statuses(&mut self) {
        let active_index = self.active_index;
        for (idx, session) in self.sessions.iter_mut().enumerate() {
            // The active session is being watched, so acknowledge any pending
            // attention signal (keeps it from flagging itself while focused).
            if idx == active_index {
                session.acknowledge_attention();
            }
            let needs_attention = session.needs_attention();

            session.info.status = if session.has_exited() {
                SessionStatus::Idle
            } else if session.millis_since_last_output() <= ACTIVITY_TIMEOUT_MS {
                // Actively producing output → working, regardless of past signals.
                SessionStatus::Busy
            } else if needs_attention {
                // Quiet after a bell / OSC 9 / OSC 777 → finished or needs input.
                SessionStatus::Attention
            } else {
                SessionStatus::Waiting
            };

            // Live activity text from the agent-emitted OSC terminal title.
            session.info.agent_activity = session.agent_title();
            // Retain the agent's latest pushed notification (OSC 9/777) so the
            // info panel can show it as a persistent "last signal", not only
            // while the session is in the attention state.
            session.info.notification = session.notification();
        }
    }

    /// Poll for external state changes from other thurbox instances (DB-based)
    /// and apply any theme change / session delta they produced.
    fn poll_external_changes(&mut self) {
        let Ok(Some(result)) = sync::poll_for_changes(&mut self.sync_state, &mut self.db) else {
            return;
        };
        if result.db_changed {
            self.apply_external_theme_change();
        }
        if !result.delta.is_empty() {
            self.handle_external_state_change(result.delta);
        }
    }

    /// Pick up theme changes made by other thurbox processes (e.g. an MCP
    /// `set_theme` call from another session).
    fn apply_external_theme_change(&mut self) {
        let Ok(Some(name)) = self.db.get_active_theme() else {
            return;
        };
        let Some(preset) = crate::session::ThemePreset::from_str(&name) else {
            return;
        };
        if preset != self.active_theme {
            crate::ui::theme::set_active(preset.palette());
            self.active_theme = preset;
        }
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
        if self.git_stats_in_progress {
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

        let (tx, rx) = mpsc::channel();
        self.git_stats_in_progress = true;
        self.git_stats_rx = Some(rx);
        tokio::task::spawn_blocking(move || {
            let _ = tx.send((session_id, aggregate_git_stats(&paths)));
        });
    }

    /// Apply a completed background git-stats refresh, if one has finished.
    fn poll_git_stats(&mut self) {
        let Some(rx) = &self.git_stats_rx else {
            return;
        };
        match rx.try_recv() {
            Ok((session_id, stats)) => {
                if let Some(session) = self.sessions.iter_mut().find(|s| s.info.id == session_id) {
                    session.info.git_stats = stats;
                }
                self.git_stats_rx = None;
                self.git_stats_in_progress = false;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            // Worker died without delivering (e.g. panic): clear the guard so the
            // next cadence can retry.
            Err(mpsc::TryRecvError::Disconnected) => {
                self.git_stats_rx = None;
                self.git_stats_in_progress = false;
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
        if self.metrics_refresh_in_progress {
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

        let (tx, rx) = mpsc::channel();
        self.metrics_refresh_in_progress = true;
        self.metrics_rx = Some(rx);
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(collect_system_metrics(sys, active, metrics_files));
        });
    }

    /// Apply a completed background metrics refresh, if one has finished.
    fn poll_metrics_refresh(&mut self) {
        let Some(rx) = &self.metrics_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(refresh) => {
                self.metrics.sys = Some(refresh.sys);
                self.metrics.system_metrics = refresh.metrics;
                for (session_id, metrics) in refresh.agent_metrics {
                    if let Some(session) =
                        self.sessions.iter_mut().find(|s| s.info.id == session_id)
                    {
                        session.info.agent_metrics = Some(metrics);
                    }
                }
                self.metrics_rx = None;
                self.metrics_refresh_in_progress = false;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            // Worker died without returning `sys` (e.g. panic): recreate the
            // collector so metrics resume next cadence. CPU-delta history is
            // lost, not correctness.
            Err(mpsc::TryRecvError::Disconnected) => {
                self.metrics.sys.get_or_insert_with(sysinfo::System::new);
                self.metrics_rx = None;
                self.metrics_refresh_in_progress = false;
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
        if let Some(rx) = &self.worktree_sync.rx {
            while let Ok((session_id, result)) = rx.try_recv() {
                self.worktree_sync.completed.push((session_id, result));
            }

            if self.worktree_sync.completed.len() >= self.worktree_sync.pending {
                self.worktree_sync.in_progress = false;
                self.worktree_sync.rx = None;
                self.finish_sync();
            }
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

    /// Start syncing the active session's worktrees with origin/main.
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
                    let result = git::sync_worktree(&worktree_path);
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
        // Update session counter to avoid conflicts
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
                if self.active_index >= self.sessions.len() {
                    self.sync_active_session_to_project();
                }
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
            // Skip if we already have this session
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

        let mut config = SessionConfig {
            resume_session_id: None,
            agent_session_id: Some(agent_sid.clone()),
            cwd,
            agent: shared_session.agent.clone(),
            fork_session_id: None,
            ..SessionConfig::default()
        };
        let def = self.agent_def_for(&config.agent);
        config.resume_session_id =
            crate::session_ops::resume_trigger_for(&def, agent_sid, &config.env);
        let provider = self.provider_for(&config);

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
        // Finalize any pending delete before shutting down
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
        // Sync session counter
        if let Err(e) = self.db.set_session_counter(self.session_counter) {
            error!("Failed to save session counter to DB: {e}");
        }

        // Upsert all sessions
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

        // Only restore sessions that have a agent_session_id (resumable)
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
    /// Sessions are restored synchronously by querying each session's backend
    /// for its existing tmux windows. Sessions persisted with a remote
    /// (`ssh:<host>`) `backend_type` are matched against that host's tmux, not
    /// the local one; a remote backend whose host is unreachable degrades to
    /// "no discovered windows" so its sessions are skipped rather than blocking
    /// startup.
    pub fn restore_sessions(&mut self, sessions: Vec<sync::SharedSession>, session_counter: usize) {
        self.session_counter = session_counter;

        // Only sessions with an agent_session_id are resumable.
        let resumable: Vec<sync::SharedSession> = sessions
            .into_iter()
            .filter(|s| s.agent_session_id.is_some())
            .collect();

        // Discover existing windows per backend. Each distinct backend_type is
        // readied + discovered at most once (a remote backend needs its
        // control-mode connection up before adopt()).
        let mut discovered_by_backend: HashMap<
            String,
            Vec<crate::agent::backend::DiscoveredSession>,
        > = HashMap::new();
        for shared in &resumable {
            if discovered_by_backend.contains_key(&shared.backend_type) {
                continue;
            }
            // Skip discovery for backends this instance can't manage (unknown
            // remote hosts); their sessions are left un-adopted rather than
            // misadopted on local.
            let Some(backend) = self.resolve_persisted_backend(&shared.backend_type) else {
                discovered_by_backend.insert(shared.backend_type.clone(), Vec::new());
                continue;
            };

            let disc = match backend.ensure_ready() {
                Ok(()) => backend.discover().unwrap_or_else(|e| {
                    warn!(
                        backend = backend.name(),
                        "Failed to discover sessions from backend: {e}"
                    );
                    Vec::new()
                }),
                Err(e) => {
                    warn!(
                        backend = backend.name(),
                        "Backend not ready during restore; skipping its sessions: {e}"
                    );
                    Vec::new()
                }
            };
            discovered_by_backend.insert(shared.backend_type.clone(), disc);
        }

        for shared in resumable {
            let discovered = discovered_by_backend
                .get(&shared.backend_type)
                .cloned()
                .unwrap_or_default();
            self.restore_single_session(shared, &discovered);
        }

        // Claim ownership of restored sessions in the shared state
        self.save_state();
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
        if let Err(e) = self.db.soft_delete_session(shared.id) {
            error!("Failed to soft-delete stale session {}: {e}", shared.id);
        }

        let mut config = SessionConfig {
            resume_session_id: None,
            agent_session_id: Some(agent_session_id.clone()),
            cwd: shared.cwd,
            agent,
            fork_session_id: None,
            ..SessionConfig::default()
        };
        let def = self.agent_def_for(&config.agent);
        config.resume_session_id =
            crate::session_ops::resume_trigger_for(&def, &agent_session_id, &config.env);
        self.new_session.additional_dirs = shared.additional_dirs;
        self.do_spawn_session(name, &config, worktrees);
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

    /// Process due scheduled commands from the database (fallback).
    ///
    /// The primary dispatch is via `tmux run-shell -b -d` timers set at
    /// scheduling time. This tick-loop catches commands whose tmux timer
    /// failed or was never set (e.g., scheduled while Thurbox was down).
    /// Throttled to once per second (~100 ticks at 10ms each).
    /// Fire any due automations. Called once per ~second from `tick()`; pass
    /// `force = true` for the one-shot startup catch-up pass (ignores cadence).
    fn process_automations(&mut self, force: bool) {
        if !force && self.metrics.tick_count % 100 != 0 {
            return;
        }
        let now = crate::sync::current_time_millis();
        let due = match self.db.due_automations(now) {
            Ok(autos) => autos,
            Err(e) => {
                error!("Failed to fetch due automations: {e}");
                return;
            }
        };
        for auto in due {
            // Claim before firing so a concurrent headless `automation tick`
            // (keeper window / systemd) can't double-fire the same automation.
            // Claim advances the schedule; `None` disables a spent one-shot.
            let next = auto.schedule.next_after(now, auto.timezone.as_deref());
            match self
                .db
                .claim_due_automation(auto.id, auto.next_run_at.unwrap_or(0), next, now)
            {
                Ok(true) => {}
                Ok(false) => {
                    debug!(
                        automation_id = auto.id,
                        "automation claim lost to a concurrent firer (headless tick / other instance)"
                    );
                    continue;
                }
                Err(e) => {
                    error!("Failed to claim automation {}: {e}", auto.id);
                    continue;
                }
            }
            let (status, detail, related) = self.fire_automation(&auto);
            if let Err(e) = self
                .db
                .record_automation_run(auto.id, status, &detail, related)
            {
                error!("Failed to record run for automation {}: {e}", auto.id);
            }
        }
    }

    /// Execute a single automation's action, returning its run status, detail,
    /// and the session it sent to / spawned (when one exists).
    fn fire_automation(
        &mut self,
        auto: &Automation,
    ) -> (AutomationRunStatus, String, Option<SessionId>) {
        match &auto.action {
            AutomationAction::Send { session_id } => {
                if self.sessions.iter().any(|s| s.info.id == *session_id) {
                    self.send_prompt_to_session(*session_id, &auto.prompt, 0);
                    info!("Automation {} sent prompt to {}", auto.id, session_id);
                    (
                        AutomationRunStatus::Success,
                        format!("sent to {session_id}"),
                        Some(*session_id),
                    )
                } else {
                    (
                        AutomationRunStatus::Skipped,
                        "target session not running".to_string(),
                        None,
                    )
                }
            }
            AutomationAction::Spawn {
                repo_path,
                worktree_branch,
                base_branch,
                agent,
            } => match self.spawn_for_automation(
                auto,
                repo_path,
                worktree_branch.as_deref(),
                base_branch.as_deref(),
                agent.as_deref(),
            ) {
                Ok(session_id) => (
                    AutomationRunStatus::Success,
                    format!("session {session_id}"),
                    Some(session_id),
                ),
                Err(e) => {
                    error!("Automation {} spawn failed: {e}", auto.id);
                    (AutomationRunStatus::Error, e, None)
                }
            },
        }
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

    /// Spawn (or reuse) the session for a `Spawn` automation and queue its
    /// prompt. Each automation owns one session named `auto-<id>`; a recurring
    /// automation reuses that session on later fires (and after a TUI restart,
    /// where it is restored from the database by name).
    fn spawn_for_automation(
        &mut self,
        auto: &Automation,
        repo_path: &std::path::Path,
        worktree_branch: Option<&str>,
        base_branch: Option<&str>,
        agent: Option<&str>,
    ) -> Result<SessionId, String> {
        self.spawn_and_prompt(
            format!("auto-{}", auto.id),
            repo_path,
            worktree_branch,
            base_branch,
            agent,
            &auto.prompt,
        )
    }

    /// Spawn (or reuse) a named session — optionally on a fresh worktree — and
    /// queue `prompt` into it. The session is named `name`; a recurring caller
    /// reuses that session on later invocations (and after a TUI restart, where
    /// it is restored from the database by name). Shared by automations
    /// (`auto-<id>`) and tasks (`task-<id>`).
    fn spawn_and_prompt(
        &mut self,
        name: String,
        repo_path: &std::path::Path,
        worktree_branch: Option<&str>,
        base_branch: Option<&str>,
        agent: Option<&str>,
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

        let worktrees = if let Some(branch) = worktree_branch {
            let base = base_branch.unwrap_or("main");
            // Idempotent: a recurring caller reuses the worktree it made on the
            // first invocation instead of failing because the branch exists.
            let path = git::create_or_attach_worktree(repo_path, branch, base)
                .map_err(|e| format!("create worktree {branch} off {base}: {e}"))?;
            vec![WorktreeInfo {
                repo_path: repo_path.to_path_buf(),
                worktree_path: path,
                branch: branch.to_string(),
            }]
        } else {
            Vec::new()
        };

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

    /// Open the automations list modal.
    fn open_automations_list(&mut self) {
        self.refresh_automations();
        let now = crate::sync::current_time_millis();
        let entries: Vec<modals::AutomationListEntry> = self
            .automation_ui
            .cached_automations
            .iter()
            .map(|a| modals::AutomationListEntry {
                id: a.id,
                name: a.name.clone(),
                summary: format_automation_summary(a, now),
                enabled: a.enabled,
            })
            .collect();
        self.modal =
            modals::Modal::AutomationsList(modals::AutomationsListModal { index: 0, entries });
    }

    /// Open a blank automation editor as a **centered overlay** (the Ctrl+P list
    /// path). A new `Send` automation defaults to the active session. The
    /// in-pane editor uses [`new_automation_in_pane`](Self::new_automation_in_pane)
    /// instead.
    fn open_automation_editor(&mut self) {
        self.modal = modals::Modal::AutomationEditor(self.blank_automation_editor());
    }

    /// `(id, name)` pairs for every session, used to populate the editor's Send
    /// target selector.
    fn session_target_choices(&self) -> Vec<(SessionId, String)> {
        self.sessions
            .iter()
            .map(|s| (s.info.id, s.info.name.clone()))
            .collect()
    }

    /// A blank editor for a new automation, with its Send target list populated
    /// from the running sessions and defaulting to the active one.
    fn blank_automation_editor(&self) -> modals::AutomationEditorModal {
        let mut m = modals::AutomationEditorModal::default();
        let active = self.sessions.get(self.active_index).map(|s| s.info.id);
        m.set_target_sessions(self.session_target_choices(), active);
        m
    }

    /// Open the centered-overlay editor pre-filled for an existing automation
    /// (the Ctrl+P list path).
    fn open_edit_automation(&mut self, id: i64) {
        let Some(auto) = self
            .automation_ui
            .cached_automations
            .iter()
            .find(|a| a.id == id)
            .cloned()
        else {
            return;
        };
        self.modal = modals::Modal::AutomationEditor(self.build_automation_editor(&auto));
    }

    /// Build an editor pre-filled from an existing automation, with its Send
    /// target list populated from the running sessions.
    fn build_automation_editor(&self, auto: &Automation) -> modals::AutomationEditorModal {
        let mut m = modals::AutomationEditorModal::from_automation(auto);
        let selected = match &auto.action {
            AutomationAction::Send { session_id } => Some(*session_id),
            AutomationAction::Spawn { .. } => None,
        };
        m.set_target_sessions(self.session_target_choices(), selected);
        m
    }

    /// Keep the in-pane automation editor (`self.automation_ui.automation_editor`) in sync with
    /// the current focus + selection:
    /// - [`InputFocus::Automations`] → mirror the selected automation (preview).
    /// - [`InputFocus::AutomationEditor`] → leave in-progress edits untouched.
    /// - anything else → drop it (we're no longer in the automation context).
    pub(crate) fn sync_automation_editor(&mut self) {
        match self.focus {
            InputFocus::Automations => {
                self.automation_ui.automation_editor = self
                    .automation_ui
                    .cached_automations
                    .get(self.automation_ui.automation_panel_index)
                    .cloned()
                    .map(|auto| self.build_automation_editor(&auto));
            }
            // Keep the editor + its run history intact while editing or
            // browsing history.
            InputFocus::AutomationEditor | InputFocus::AutomationRunHistory => {}
            _ => self.automation_ui.automation_editor = None,
        }
    }

    /// The id of the automation currently scoped in the central pane (the one
    /// being edited/previewed), if it's an existing automation.
    pub(crate) fn scoped_automation_id(&self) -> Option<i64> {
        self.automation_ui
            .automation_editor
            .as_ref()
            .and_then(|m| m.editing_id)
    }

    /// Open the session associated with the selected run-history entry.
    /// Switches to that session's terminal when it's still open, otherwise
    /// sets a status message.
    pub(crate) fn open_run_related_session(&mut self) {
        let Some(run) = self
            .automation_ui
            .cached_automation_runs
            .get(self.automation_ui.automation_run_index)
        else {
            return;
        };
        // Prefer the typed column (v28+). Pre-v28 rows only embed the id in
        // their free-text detail (e.g. "session <uuid>"), so fall back to the
        // first token that parses as a session id.
        let session_id = run.related_session_id.or_else(|| {
            run.detail
                .split_whitespace()
                .find_map(|tok| tok.parse::<SessionId>().ok())
        });
        let Some(session_id) = session_id else {
            self.set_status(StatusLevel::Info, "This run has no related session");
            return;
        };
        match self.sessions.iter().position(|s| s.info.id == session_id) {
            Some(idx) => {
                self.active_index = idx;
                self.focus = InputFocus::Terminal;
                // Leaving the automation context clears the editor/run cache.
                self.refresh_automation_view();
            }
            None => self.set_status(StatusLevel::Info, "Related session is no longer open"),
        }
    }

    /// Start a brand-new automation in the central pane and focus the editor.
    pub(crate) fn new_automation_in_pane(&mut self) {
        self.automation_ui.automation_editor = Some(self.blank_automation_editor());
        self.focus = InputFocus::AutomationEditor;
    }

    /// Re-sync the in-pane editor preview and the run-history cache after the
    /// automations selection or an automation itself changes.
    pub(crate) fn refresh_automation_view(&mut self) {
        self.sync_automation_editor();
        self.refresh_selected_automation_runs();
    }

    /// Move focus into the central-pane editor for the selected automation
    /// (mirrors `Enter` on a session focusing its terminal).
    pub(crate) fn enter_automation_editor(&mut self) {
        // Build the editor for the current selection (focus is still
        // `Automations`, so `sync` populates it), then focus it.
        self.sync_automation_editor();
        if self.automation_ui.automation_editor.is_some() {
            self.focus = InputFocus::AutomationEditor;
        }
    }

    /// Validate and submit the centered-overlay editor (create or update).
    fn submit_automation_editor(&mut self) {
        let modals::Modal::AutomationEditor(ref m) = self.modal else {
            return;
        };
        let m = m.clone();
        if self.save_automation(&m) {
            self.modal.close();
        }
    }

    /// Validate `m` and persist it (create or update). Returns `true` on success;
    /// on failure sets an error status and returns `false` (leaving the editor
    /// open). Refreshes the cached automations on success. Shared by the overlay
    /// and in-pane editors.
    fn save_automation(&mut self, m: &modals::AutomationEditorModal) -> bool {
        let name = m.name.value().trim().to_string();
        if name.is_empty() {
            self.set_error("Name cannot be empty");
            return false;
        }
        let prompt = m.prompt.value().trim().to_string();
        if prompt.is_empty() {
            self.set_error("Prompt cannot be empty");
            return false;
        }

        let now = crate::sync::current_time_millis();
        let schedule = match m.build_schedule(now) {
            Ok(s) => s,
            Err(e) => {
                self.set_error(e);
                return false;
            }
        };

        let timezone = m.timezone();

        let action = match m.action {
            modals::AutomationActionKind::Send => {
                let Some(session_id) = m.selected_target().map(|(id, _)| *id) else {
                    self.set_error("No target session — start a session first");
                    return false;
                };
                AutomationAction::Send { session_id }
            }
            modals::AutomationActionKind::Spawn => {
                let repo = m.repo.value().trim();
                if repo.is_empty() {
                    self.set_error("Repo path required for spawn action");
                    return false;
                }
                let worktree = m.worktree.value().trim();
                let agent = m.agent.value().trim();
                AutomationAction::Spawn {
                    // Expand `~` so the stored path is absolute (git and the
                    // session cwd don't expand it themselves).
                    repo_path: crate::paths::expand_tilde(repo),
                    worktree_branch: (!worktree.is_empty()).then(|| worktree.to_string()),
                    base_branch: None,
                    agent: (!agent.is_empty()).then(|| agent.to_string()),
                }
            }
        };

        let next_run_at = m
            .enabled
            .then(|| schedule.next_after(now, timezone.as_deref()))
            .flatten();

        let result = match m.editing_id {
            Some(id) => match self.db.get_automation(id) {
                Ok(Some(mut auto)) => {
                    auto.name = name;
                    auto.prompt = prompt;
                    auto.schedule = schedule;
                    auto.timezone = timezone;
                    auto.action = action;
                    auto.enabled = m.enabled;
                    auto.next_run_at = next_run_at;
                    self.db.update_automation(&auto)
                }
                Ok(None) => {
                    self.set_error("Automation no longer exists");
                    return false;
                }
                Err(e) => Err(e),
            },
            None => {
                let new = crate::storage::automations::NewAutomation {
                    name,
                    enabled: m.enabled,
                    schedule,
                    timezone,
                    action,
                    prompt,
                    next_run_at,
                };
                self.db.create_automation(&new).map(|_| ())
            }
        };

        if let Err(e) = result {
            error!("Failed to save automation: {e}");
            self.set_error("Failed to save automation");
            return false;
        }
        self.refresh_automations();
        self.set_status(StatusLevel::Success, "Automation saved");
        true
    }

    // ---- Tasks (right-side panel) ----------------------------------------

    /// Refresh the cached tasks from the database, keeping the filtered view and
    /// panel selection valid.
    pub(crate) fn refresh_tasks(&mut self) {
        match self.db.list_tasks() {
            Ok(tasks) => self.task_ui.cached_tasks = tasks,
            Err(e) => error!("Failed to list tasks: {e}"),
        }
        self.recompute_task_filter();
    }

    /// Rebuild [`Self::filtered_task_indices`] (all active tasks) and clamp the
    /// panel selection into range. The tasks panel shows every task now —
    /// filtering happens through the global `Ctrl+A` search.
    pub(crate) fn recompute_task_filter(&mut self) {
        self.task_ui.filtered_task_indices = (0..self.task_ui.cached_tasks.len()).collect();
        if self.task_ui.filtered_task_indices.is_empty() {
            self.task_ui.task_panel_index = 0;
        } else {
            self.task_ui.task_panel_index = self
                .task_ui
                .task_panel_index
                .min(self.task_ui.filtered_task_indices.len() - 1);
        }
    }

    /// The task currently selected in the panel (honoring the filter), if any.
    pub(crate) fn selected_task(&self) -> Option<&crate::session::Task> {
        let idx = *self
            .task_ui
            .filtered_task_indices
            .get(self.task_ui.task_panel_index)?;
        self.task_ui.cached_tasks.get(idx)
    }

    /// Indices into `self.sessions` of the **currently-open** sessions a task is
    /// related to, in display order:
    ///
    /// - the session named `task-<id>` — the spawn convention used by the
    ///   headless `task run` (and what survives a restart);
    /// - the target of a persisted `Send` action (`task.action`), when one is
    ///   set via the CLI; and
    /// - the in-memory `task_session_links` entry recorded when the task was
    ///   triggered from the TUI this run (TUI spawns get a user-chosen name, not
    ///   `task-<id>`, so this is how that link is recovered).
    ///
    /// Deduplicated. Empty when nothing related is open right now. This is the
    /// single source of truth for both the details panel and the *open* key.
    pub(crate) fn task_related_session_indices(&self, task: &crate::session::Task) -> Vec<usize> {
        let spawn_name = format!("task-{}", task.id);
        // Persisted `Send` action target (CLI-authored), plus the in-memory link
        // recorded when this task was triggered from the TUI this run.
        let send_target = match &task.action {
            Some(crate::session::AutomationAction::Send { session_id }) => Some(*session_id),
            _ => None,
        };
        let linked = self.task_ui.task_session_links.get(&task.id).copied();
        let mut out = Vec::new();
        for (i, s) in self.sessions.iter().enumerate() {
            let related = s.info.name == spawn_name
                || send_target.is_some_and(|t| t == s.info.id)
                || linked.is_some_and(|t| t == s.info.id);
            if related && !out.contains(&i) {
                out.push(i);
            }
        }
        out
    }

    /// Jump to the task's related session terminal (the first open one). Bound
    /// to `o` in the focused tasks panel; mirrors `open_run_related_session`.
    pub(crate) fn open_task_related_session(&mut self) {
        let Some(task) = self.selected_task().cloned() else {
            return;
        };
        match self.task_related_session_indices(&task).first() {
            Some(&idx) => {
                self.active_index = idx;
                self.focus = InputFocus::Terminal;
                // Leaving the tasks context clears the editor/preview cache.
                self.refresh_task_view();
            }
            None => self.set_status(
                StatusLevel::Info,
                "Task has no open session — run it with r",
            ),
        }
    }

    /// Scroll the full-screen task preview by `delta` rows, clamped to the
    /// rendered description length (a slight over-scroll is harmless).
    pub(crate) fn scroll_task_preview(&mut self, delta: i32) {
        let max = self.task_preview_max_scroll() as i32;
        let next = (self.task_ui.task_preview_scroll as i32 + delta).clamp(0, max);
        self.task_ui.task_preview_scroll = next as u16;
    }

    /// Largest valid `task_preview_scroll` for the selected task's description.
    ///
    /// Matches the line set that `ui::task_detail::render_task_detail` scrolls:
    /// the rendered markdown plus the one-row `description` header it prepends.
    /// Shared by keyboard (`PageUp`/`PageDown`), the wheel, and the scrollbar
    /// drag so all three agree on the clamp.
    pub(crate) fn task_preview_max_scroll(&self) -> u16 {
        let body = self
            .selected_task()
            .and_then(|t| t.description.as_deref())
            .filter(|d| !d.trim().is_empty())
            .map(|d| crate::ui::markdown::render_markdown(d).len())
            .unwrap_or(0);
        // `content_len = body + 1` (the header row); max index is `content_len - 1`.
        body as u16
    }

    /// Open the trigger-time action picker for `task`: one **Send** entry per
    /// running session, plus **Spawn new session…**. The chosen
    /// action runs immediately (nothing is persisted on the task).
    pub(crate) fn open_task_action_picker(&mut self, task: &crate::session::Task) {
        use modals::{Modal, TaskActionChoice, TaskActionPickerModal};
        let mut choices: Vec<TaskActionChoice> = self
            .session_target_choices()
            .into_iter()
            .map(|(id, name)| TaskActionChoice::Send(id, name))
            .collect();
        choices.push(TaskActionChoice::SpawnNew);
        self.modal = Modal::TaskActionPicker(TaskActionPickerModal {
            task_id: task.id,
            title: task.title.clone(),
            choices,
            selected: 0,
        });
    }

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

    /// Send a task's prompt to an existing session and advance it to
    /// `InProgress`.
    pub(crate) fn send_task_to_session(
        &mut self,
        task_id: i64,
        title: &str,
        status: crate::session::TaskStatus,
        session_id: SessionId,
    ) {
        let Some(name) = self
            .sessions
            .iter()
            .find(|s| s.info.id == session_id)
            .map(|s| s.info.name.clone())
        else {
            self.set_error("Target session is not running");
            return;
        };
        let prompt = self.task_agent_prompt(task_id, title);
        self.send_prompt_to_session(session_id, &prompt, 0);
        self.task_ui.task_session_links.insert(task_id, session_id);
        self.advance_task_to_in_progress(task_id, status);
        self.refresh_tasks();
        self.set_status(StatusLevel::Success, format!("Sent task to {name}"));
    }

    /// Advance a task `Todo → InProgress` (no-op for other states) now that an
    /// agent is acting on it. Shared by the Send and Spawn trigger paths.
    pub(crate) fn advance_task_to_in_progress(
        &mut self,
        task_id: i64,
        status: crate::session::TaskStatus,
    ) {
        if status == crate::session::TaskStatus::Todo {
            if let Err(e) = self
                .db
                .set_task_status(task_id, crate::session::TaskStatus::InProgress)
            {
                error!("Failed to advance task {task_id} status: {e}");
            }
        }
    }

    /// A blank task editor (title + description + status only).
    fn blank_task_editor(&self) -> modals::TaskEditorModal {
        modals::TaskEditorModal::new()
    }

    /// Build an editor pre-filled from an existing task.
    fn build_task_editor(&self, task: &crate::session::Task) -> modals::TaskEditorModal {
        modals::TaskEditorModal::from_task(task)
    }

    /// Keep the in-pane task editor (`self.task_ui.task_editor`) in sync with focus +
    /// selection:
    /// - [`InputFocus::TaskList`] → mirror the selected task (live preview).
    /// - [`InputFocus::TaskEditor`] → leave in-progress edits untouched.
    /// - anything else → drop it (we left the tasks context). Mirrors
    ///   [`Self::sync_automation_editor`].
    pub(crate) fn sync_task_editor(&mut self) {
        match self.focus {
            InputFocus::TaskList => {
                // A fresh preview starts unscrolled.
                self.task_ui.task_preview_scroll = 0;
                self.task_ui.task_editor = self
                    .selected_task()
                    .cloned()
                    .map(|task| self.build_task_editor(&task));
            }
            InputFocus::TaskEditor => {}
            _ => self.task_ui.task_editor = None,
        }
    }

    /// Re-sync the in-pane task editor preview after the selection or a task
    /// itself changes.
    pub(crate) fn refresh_task_view(&mut self) {
        self.sync_task_editor();
    }

    /// Start a brand-new task in the central pane and focus the editor.
    pub(crate) fn new_task_in_pane(&mut self) {
        self.task_ui.task_editor = Some(self.blank_task_editor());
        self.focus = InputFocus::TaskEditor;
    }

    /// Move focus into the central-pane editor for the selected task (mirrors
    /// `Enter` on a session focusing its terminal).
    pub(crate) fn enter_task_editor(&mut self) {
        self.sync_task_editor();
        if self.task_ui.task_editor.is_some() {
            self.focus = InputFocus::TaskEditor;
        }
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

    // ---- Global search (Ctrl+A bottom strip) -----------------------------

    /// Open the global-search strip: snapshot the current UI state (so cancel
    /// can restore it), clear the query, focus the strip, and seed the (cheap)
    /// metadata results.
    pub(crate) fn open_global_search(&mut self) {
        self.global_search.snapshot = Some(search::SearchSnapshot {
            focus: self.focus,
            active_index: self.active_index,
            task_panel_index: self.task_ui.task_panel_index,
            automation_panel_index: self.automation_ui.automation_panel_index,
            show_tasks_panel: self.show_tasks_panel,
            show_file_viewer: self.show_file_viewer,
        });
        self.global_search.active = true;
        self.global_search.query.clear();
        self.global_search.results.clear();
        self.global_search.selected = 0;
        self.global_search.query_changed_at = None;
        self.global_search.content_dirty = false;
        self.focus = InputFocus::GlobalSearch;
        self.recompute_global_search_metadata();
        self.resize_sessions_to_content_area();
    }

    /// Cancel the strip: restore the exact UI state captured at open time
    /// (selections, focus, and panel visibility the live preview may have
    /// changed). Bound to `Esc`.
    pub(crate) fn close_global_search(&mut self) {
        if let Some(snap) = self.global_search.snapshot.take() {
            self.active_index = snap.active_index.min(self.sessions.len().saturating_sub(1));
            self.task_ui.task_panel_index = snap.task_panel_index;
            self.automation_ui.automation_panel_index = snap.automation_panel_index;
            self.show_tasks_panel = snap.show_tasks_panel;
            self.show_file_viewer = snap.show_file_viewer;
            self.focus = snap.focus;
        }
        self.global_search.active = false;
        self.global_search.results.clear();
        self.global_search.query.clear();
        self.resize_sessions_to_content_area();
    }

    /// Note that the query changed: recompute the cheap metadata results now,
    /// live-preview the new top result, and flag the expensive content scan to
    /// run after the debounce settles.
    pub(crate) fn on_global_search_query_changed(&mut self) {
        self.recompute_global_search_metadata();
        self.preview_global_search_result();
        self.global_search.content_dirty = true;
        self.global_search.query_changed_at = Some(std::time::Instant::now());
    }

    /// Rebuild the metadata results (sessions/tasks/automations/files) — fast
    /// enough to run on every keystroke. Session buffer **content** matches are
    /// added separately by [`Self::recompute_global_search_content`].
    pub(crate) fn recompute_global_search_metadata(&mut self) {
        let query = self.global_search.query.value().to_string();
        let results = self.build_global_search_results(&query, false);
        self.global_search.results = results;
        self.global_search.clamp_selection();
    }

    /// Rebuild results including the debounced per-session buffer content scan.
    pub(crate) fn recompute_global_search_content(&mut self) {
        let query = self.global_search.query.value().to_string();
        let results = self.build_global_search_results(&query, true);
        self.global_search.results = results;
        self.global_search.clamp_selection();
        // The result set may have grown (content matches) — keep the preview in
        // sync with whatever is now selected.
        self.preview_global_search_result();
    }

    /// Assemble the grouped result list. `with_content` adds session buffer
    /// matches (the heavy path). Empty query → no results.
    fn build_global_search_results(
        &self,
        query: &str,
        with_content: bool,
    ) -> Vec<search::GlobalSearchResult> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        let query_lc = query.to_lowercase();
        // Group order: Sessions → Tasks → Automations → Files.
        let mut out = self.search_sessions(query, &query_lc, with_content);
        out.extend(self.search_tasks(query, &query_lc));
        out.extend(self.search_automations(query));
        out.extend(self.search_files(&query_lc));
        out
    }

    /// Session results: fuzzy metadata (name / agent / branch) plus, on the
    /// debounced heavy path, a buffer-content scan (skipping metadata matches).
    fn search_sessions(
        &self,
        query: &str,
        query_lc: &str,
        with_content: bool,
    ) -> Vec<search::GlobalSearchResult> {
        use search::{GlobalSearchResult, SearchKind, SearchTarget, MAX_PER_GROUP};
        let mut sessions: Vec<GlobalSearchResult> = Vec::new();
        for (i, session) in self.sessions.iter().enumerate() {
            if sessions.len() >= MAX_PER_GROUP {
                break;
            }
            let info = &session.info;
            let branch = info.worktrees.first().map(|w| w.branch.as_str());
            let meta_hit = crate::fuzzy::fuzzy_match(query, &info.name).is_some()
                || crate::fuzzy::fuzzy_match(query, &info.agent).is_some()
                || branch.is_some_and(|b| crate::fuzzy::fuzzy_match(query, b).is_some());
            if meta_hit {
                sessions.push(GlobalSearchResult {
                    kind: SearchKind::Session,
                    label: info.name.clone(),
                    snippet: None,
                    target: SearchTarget::Session { index: i },
                });
            }
        }
        if with_content {
            self.push_session_content_matches(query_lc, &mut sessions);
        }
        sessions
    }

    /// Append vt100 buffer-content matches to `out`, skipping sessions already
    /// present (matched on metadata) and respecting the per-group cap.
    fn push_session_content_matches(
        &self,
        query_lc: &str,
        out: &mut Vec<search::GlobalSearchResult>,
    ) {
        use search::{GlobalSearchResult, SearchKind, SearchTarget, MAX_PER_GROUP};
        let already: std::collections::HashSet<usize> = out
            .iter()
            .filter_map(|r| match r.target {
                SearchTarget::Session { index } => Some(index),
                _ => None,
            })
            .collect();
        for i in 0..self.sessions.len() {
            if out.len() >= MAX_PER_GROUP {
                break;
            }
            if already.contains(&i) {
                continue;
            }
            if let Some(snippet) = self.session_content_match(query_lc, i) {
                out.push(GlobalSearchResult {
                    kind: SearchKind::Session,
                    label: self.sessions[i].info.name.clone(),
                    snippet: Some(snippet),
                    target: SearchTarget::Session { index: i },
                });
            }
        }
    }

    /// Task results: fuzzy title, falling back to a fuzzy description match with
    /// a context snippet.
    fn search_tasks(&self, query: &str, query_lc: &str) -> Vec<search::GlobalSearchResult> {
        use search::{GlobalSearchResult, SearchKind, SearchTarget, MAX_PER_GROUP};
        let mut tasks: Vec<GlobalSearchResult> = Vec::new();
        for task in &self.task_ui.cached_tasks {
            if tasks.len() >= MAX_PER_GROUP {
                break;
            }
            let title_hit = crate::fuzzy::fuzzy_match(query, &task.title).is_some();
            // Title missed — match the description with the same fuzzy matcher
            // used for titles (so gapped queries hit too).
            let desc_hit = !title_hit
                && task
                    .description
                    .as_deref()
                    .is_some_and(|d| crate::fuzzy::fuzzy_match(query, d).is_some());
            if !title_hit && !desc_hit {
                continue;
            }
            // Snippet (description hits only): prefer a line containing the query
            // verbatim, else the first non-empty line, for useful context.
            let snippet = desc_hit.then(|| {
                let desc = task.description.as_deref().unwrap_or("");
                desc.lines()
                    .find(|l| l.to_lowercase().contains(query_lc))
                    .or_else(|| desc.lines().find(|l| !l.trim().is_empty()))
                    .map(|l| l.trim().chars().take(120).collect::<String>())
                    .unwrap_or_default()
            });
            tasks.push(GlobalSearchResult {
                kind: SearchKind::Task,
                label: task.title.clone(),
                snippet,
                target: SearchTarget::Task { id: task.id },
            });
        }
        tasks
    }

    /// Automation results: fuzzy name.
    fn search_automations(&self, query: &str) -> Vec<search::GlobalSearchResult> {
        use search::{GlobalSearchResult, SearchKind, SearchTarget, MAX_PER_GROUP};
        let mut automations: Vec<GlobalSearchResult> = Vec::new();
        for auto in &self.automation_ui.cached_automations {
            if automations.len() >= MAX_PER_GROUP {
                break;
            }
            if crate::fuzzy::fuzzy_match(query, &auto.name).is_some() {
                automations.push(GlobalSearchResult {
                    kind: SearchKind::Automation,
                    label: auto.name.clone(),
                    snippet: None,
                    target: SearchTarget::Automation { id: auto.id },
                });
            }
        }
        automations
    }

    /// File results: case-insensitive substring over the active session's tree.
    fn search_files(&self, query_lc: &str) -> Vec<search::GlobalSearchResult> {
        use search::{GlobalSearchResult, SearchKind, SearchTarget, MAX_PER_GROUP};
        let mut files: Vec<GlobalSearchResult> = Vec::new();
        let Some(info) = self.sessions.get(self.active_index).map(|s| &s.info) else {
            return files;
        };
        for (root, path, name) in crate::ui::file_viewer::enumerate_paths(info) {
            if files.len() >= MAX_PER_GROUP {
                break;
            }
            if name.to_lowercase().contains(query_lc) {
                files.push(GlobalSearchResult {
                    kind: SearchKind::File,
                    label: name,
                    snippet: None,
                    target: SearchTarget::File { root, path },
                });
            }
        }
        files
    }

    /// Search a session's visible buffer for `query_lc`, returning the first
    /// matching (trimmed) line as a snippet. Scans only the last
    /// [`search::CONTENT_LINE_CAP`] lines and tolerates a poisoned lock.
    fn session_content_match(&self, query_lc: &str, idx: usize) -> Option<String> {
        if query_lc.trim().is_empty() {
            return None;
        }
        let session = self.sessions.get(idx)?;
        let parser = session.parser.lock().ok()?;
        let contents = parser.screen().contents();
        drop(parser);
        let lines: Vec<&str> = contents.lines().collect();
        let start = lines.len().saturating_sub(search::CONTENT_LINE_CAP);
        for line in &lines[start..] {
            if line.to_lowercase().contains(query_lc) {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.chars().take(120).collect());
                }
            }
        }
        None
    }

    /// The active global-search query for live in-panel highlighting: `Some`
    /// when the strip is open with a non-empty query, else `None` (panels render
    /// normally). Used by the view to highlight matched rows and dim the rest.
    pub(crate) fn global_search_query(&self) -> Option<&str> {
        if !self.global_search.active {
            return None;
        }
        let q = self.global_search.query.value();
        (!q.trim().is_empty()).then_some(q)
    }

    /// The scope of the currently selected global-search result, while the strip
    /// is active. Lets the view force-show the selected (previewed) row in the
    /// owning panel even though focus stays in the search box.
    pub(crate) fn global_search_preview_kind(&self) -> Option<search::SearchKind> {
        if !self.global_search.active {
            return None;
        }
        self.global_search
            .results
            .get(self.global_search.selected)
            .map(|r| r.kind)
    }

    /// Live-preview the selected result without leaving the search box: move the
    /// matching panel's cursor (active session / task row / automation row) so
    /// the user sees where `Enter` would land. Files are *not* previewed (opening
    /// the file viewer per keystroke is heavy) — they only act on `Enter`.
    /// Cancelling restores all of this from the snapshot.
    pub(crate) fn preview_global_search_result(&mut self) {
        let Some(result) = self
            .global_search
            .results
            .get(self.global_search.selected)
            .cloned()
        else {
            return;
        };
        match result.target {
            search::SearchTarget::Session { index } => {
                if index < self.sessions.len() {
                    self.active_index = index;
                }
            }
            search::SearchTarget::Task { id } => {
                self.show_tasks_panel = true;
                self.refresh_tasks();
                if let Some(pos) = self
                    .task_ui
                    .filtered_task_indices
                    .iter()
                    .position(|&i| self.task_ui.cached_tasks.get(i).map(|t| t.id) == Some(id))
                {
                    self.task_ui.task_panel_index = pos;
                }
            }
            search::SearchTarget::Automation { id } => {
                if let Some(pos) = self
                    .automation_ui
                    .cached_automations
                    .iter()
                    .position(|a| a.id == id)
                {
                    self.automation_ui.automation_panel_index = pos;
                }
            }
            // Files aren't previewed live — they only open on `Enter`.
            search::SearchTarget::File { .. } => {}
        }
    }

    /// Jump to the selected search result's target, then close the strip.
    pub(crate) fn activate_global_search_result(&mut self) {
        let Some(result) = self
            .global_search
            .results
            .get(self.global_search.selected)
            .cloned()
        else {
            self.close_global_search();
            return;
        };
        // Commit: discard the snapshot (we keep the jump, don't restore) and
        // tear the strip down, then apply the jump target. Capture the pre-search
        // focus first, as the fallback when a stale target can't be opened.
        let fallback_focus = self
            .global_search
            .snapshot
            .as_ref()
            .map(|s| s.focus)
            .unwrap_or(InputFocus::SessionList);
        self.global_search.active = false;
        self.global_search.results.clear();
        self.global_search.query.clear();
        self.global_search.snapshot = None;
        match result.target {
            search::SearchTarget::Session { index } => {
                if index < self.sessions.len() {
                    self.active_index = index;
                    self.focus = InputFocus::Terminal;
                } else {
                    self.focus = fallback_focus;
                }
            }
            search::SearchTarget::Task { id } => {
                self.show_tasks_panel = true;
                self.refresh_tasks();
                if let Some(pos) = self
                    .task_ui
                    .filtered_task_indices
                    .iter()
                    .position(|&i| self.task_ui.cached_tasks.get(i).map(|t| t.id) == Some(id))
                {
                    self.task_ui.task_panel_index = pos;
                }
                self.focus = InputFocus::TaskList;
            }
            search::SearchTarget::Automation { id } => {
                if let Some(pos) = self
                    .automation_ui
                    .cached_automations
                    .iter()
                    .position(|a| a.id == id)
                {
                    self.automation_ui.automation_panel_index = pos;
                }
                self.focus = InputFocus::Automations;
                self.refresh_automation_view();
            }
            search::SearchTarget::File { root: _, path } => {
                self.show_file_viewer = true;
                self.rebuild_file_viewer_for_active();
                self.file_viewer.reveal_path(&path);
                self.focus = InputFocus::FileViewer;
            }
        }
        self.resize_sessions_to_content_area();
    }

    /// Refresh the cached automations from the database.
    fn refresh_automations(&mut self) {
        match self.db.list_automations() {
            Ok(autos) => self.automation_ui.cached_automations = autos,
            Err(e) => error!("Failed to list automations: {e}"),
        }
        // Keep the pane selection in range. The pane stays focusable even when
        // empty (so Ctrl+N can create the first automation), so focus is left
        // where it is.
        if self.automation_ui.cached_automations.is_empty() {
            self.automation_ui.automation_panel_index = 0;
        } else if self.automation_ui.automation_panel_index
            >= self.automation_ui.cached_automations.len()
        {
            self.automation_ui.automation_panel_index =
                self.automation_ui.cached_automations.len() - 1;
        }
        // Keep the central-pane run history fresh while the pane is scoped (e.g.
        // a just-fired automation gains a new run).
        self.refresh_selected_automation_runs();
    }

    /// Load the run history for the automation currently scoped in the central
    /// pane. No-op (and clears the cache) unless the automations context is
    /// active and points at a real automation.
    pub(crate) fn refresh_selected_automation_runs(&mut self) {
        // While editing / browsing history the runs belong to the scoped
        // automation; in list preview they follow the highlighted row.
        let editing_id = self
            .automation_ui
            .automation_editor
            .as_ref()
            .and_then(|m| m.editing_id);
        let selected_id = match self.focus {
            InputFocus::AutomationEditor | InputFocus::AutomationRunHistory => editing_id,
            InputFocus::Automations => self
                .automation_ui
                .cached_automations
                .get(self.automation_ui.automation_panel_index)
                .map(|a| a.id),
            _ => None,
        };
        let Some(id) = selected_id else {
            self.automation_ui.cached_automation_runs.clear();
            self.automation_ui.cached_automation_runs_id = None;
            self.automation_ui.automation_run_index = 0;
            return;
        };
        match self.db.list_automation_runs(id, 20) {
            Ok(runs) => {
                self.automation_ui.cached_automation_runs = runs;
                self.automation_ui.cached_automation_runs_id = Some(id);
            }
            Err(e) => {
                error!("Failed to list automation runs for {id}: {e}");
                self.automation_ui.cached_automation_runs.clear();
                self.automation_ui.cached_automation_runs_id = None;
            }
        }
        // Keep the run-history selection in range.
        let len = self.automation_ui.cached_automation_runs.len();
        if len == 0 {
            self.automation_ui.automation_run_index = 0;
        } else if self.automation_ui.automation_run_index >= len {
            self.automation_ui.automation_run_index = len - 1;
        }
    }

    /// Toggle an automation's enabled state, recomputing `next_run_at` on enable.
    fn toggle_automation_by_id(&mut self, id: i64) {
        let Ok(Some(mut auto)) = self.db.get_automation(id) else {
            return;
        };
        auto.enabled = !auto.enabled;
        auto.next_run_at = if auto.enabled {
            auto.schedule
                .next_after(crate::sync::current_time_millis(), auto.timezone.as_deref())
        } else {
            None
        };
        if let Err(e) = self.db.update_automation(&auto) {
            error!("Failed to toggle automation {id}: {e}");
        }
        self.refresh_automations();
    }

    /// Mark an automation due so the next tick fires it.
    fn run_automation_by_id(&mut self, id: i64) {
        match self.db.trigger_automation_now(id) {
            Ok(true) => {
                self.refresh_automations();
                self.set_status(StatusLevel::Success, "Automation will run now");
            }
            Ok(false) => self.set_error("Automation not found"),
            Err(e) => {
                error!("Failed to trigger automation {id}: {e}");
                self.set_error("Failed to trigger automation");
            }
        }
    }

    /// Delete an automation by ID and refresh the cache.
    fn delete_automation_by_id(&mut self, id: i64) {
        match self.db.delete_automation(id) {
            Ok(true) => {
                self.refresh_automations();
                self.set_status(StatusLevel::Success, "Automation deleted");
            }
            Ok(false) => self.set_error("Automation not found"),
            Err(e) => {
                error!("Failed to delete automation {id}: {e}");
                self.set_error("Failed to delete automation");
            }
        }
    }

    pub(crate) fn content_area_size(&self) -> (u16, u16) {
        let area = Rect::new(0, 0, self.terminal_cols, self.terminal_rows);
        let terminal = layout::compute_layout(
            area,
            self.show_info_panel,
            self.show_tasks_panel,
            self.show_file_viewer,
            self.global_search.active,
            self.automation_ui.cached_automations.len(),
        )
        .terminal;
        let inner = Block::default().borders(Borders::ALL).inner(terminal);
        (inner.height, inner.width)
    }
}

/// One-line summary of an automation for the list modal:
/// `<schedule> · <action> · <when>`.
fn format_automation_summary(auto: &Automation, now: u64) -> String {
    let schedule = match &auto.schedule {
        AutomationSchedule::Once { .. } => "once".to_string(),
        AutomationSchedule::Cron { expr } => expr.clone(),
    };
    let action = auto.action.kind();
    let when = if !auto.enabled {
        "disabled".to_string()
    } else if let Some(next) = auto.next_run_at {
        // `format_countdown` already includes the "in " prefix.
        view::format_countdown(next.saturating_sub(now))
    } else {
        "—".to_string()
    };
    format!("{schedule} · {action} · {when}")
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

    match crate::workspace::ensure_workspace(id, &pairs) {
        Ok(ws) => Some(ws),
        Err(e) => {
            error!("Failed to build multi-repo workspace: {e}");
            primary_cwd
        }
    }
}

/// The launch cwd for an *existing* session, derived from its persisted
/// `SessionInfo`: the multi-repo symlink workspace, or the primary repo when
/// single-repo. Used by the restart and shell-pane paths, which must agree on
/// the cwd so the companion shell lands exactly where the agent runs.
fn session_process_cwd(info: &SessionInfo) -> Option<PathBuf> {
    resolve_process_cwd(
        info.agent_session_id.as_deref(),
        info.cwd.clone(),
        &info.worktrees,
        &info.additional_dirs,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::agent::SessionBackend;

    // --- TextInput tests ---

    // --- Session switching tests ---

    /// Stub backend that does nothing — for unit tests only.
    struct StubBackend;
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
            Ok(())
        }
        fn pane_pid(&self, _: &str) -> anyhow::Result<Option<u32>> {
            Ok(None)
        }
    }

    fn stub_backend_arc() -> Arc<dyn SessionBackend> {
        Arc::new(StubBackend)
    }

    /// Stub backend that counts `detach` calls (for lifecycle tests).
    struct DetachCountingBackend {
        detached: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl SessionBackend for DetachCountingBackend {
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
    fn apply_removed_sessions_detaches_pane_io() {
        let detached = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backend_arc: Arc<dyn SessionBackend> = Arc::new(DetachCountingBackend {
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
            hosts: vec![crate::session::HostDef {
                name: "devbox".into(),
                destination: "me@devbox".into(),
                socket: None,
                session: None,
                ssh_opts: vec![],
                worktrees_dir: None,
            }],
        });
        app.start_new_session();
        match app.modal {
            modals::Modal::HostPicker(ref hp) => {
                // "local" first, then each ssh host.
                assert_eq!(hp.choices.len(), 2);
                assert_eq!(hp.choices[0].backend, "");
                assert_eq!(hp.choices[1].backend, "ssh:devbox");
            }
            ref other => panic!("expected host picker, got {other:?}"),
        }
    }

    #[test]
    fn host_for_backend_resolves_ssh_only() {
        let mut app = app_with_sessions(0);
        app.set_hosts(crate::session::HostRegistry {
            hosts: vec![crate::session::HostDef {
                name: "devbox".into(),
                destination: "me@devbox".into(),
                socket: None,
                session: None,
                ssh_opts: vec![],
                worktrees_dir: None,
            }],
        });
        assert!(app.host_for_backend(None).is_none());
        assert!(app.host_for_backend(Some("local-tmux")).is_none());
        assert_eq!(
            app.host_for_backend(Some("ssh:devbox"))
                .unwrap()
                .destination,
            "me@devbox"
        );
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
    fn switch_follows_activity_and_repo_group_order() {
        use crate::session::SessionStatus;
        // DB order: [webapp/Busy, infra/Attention, webapp/Busy].
        let mut app = app_with_sessions(3);
        app.sessions[0].info.repo_display_names = vec!["webapp".to_string()];
        app.sessions[0].info.status = SessionStatus::Busy;
        app.sessions[1].info.repo_display_names = vec!["infra".to_string()];
        app.sessions[1].info.status = SessionStatus::Attention;
        app.sessions[2].info.repo_display_names = vec!["webapp".to_string()];
        app.sessions[2].info.status = SessionStatus::Busy;

        // Rendered order: infra group (Attention) first → [1], then webapp → [0, 2].
        app.active_index = 1;
        app.switch_session_forward();
        assert_eq!(app.active_index, 0, "infra/attn → webapp/0");
        app.switch_session_forward();
        assert_eq!(app.active_index, 2, "webapp/0 → webapp/2");
        app.switch_session_forward();
        assert_eq!(app.active_index, 1, "webapp/2 → wrap to infra/attn");
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
        // Fill screen and scrollback by writing many lines
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
        app.automation_ui.cached_automations = vec![make(1, "a"), make(2, "b")];
        app.focus = InputFocus::Automations;
        app.automation_ui.automation_panel_index = 0;
        app.handle_automations_pane_key(KeyCode::Char('j'));
        assert_eq!(app.automation_ui.automation_panel_index, 1);
        app.handle_automations_pane_key(KeyCode::Char('k'));
        assert_eq!(app.automation_ui.automation_panel_index, 0);
        // j past the last automation loops out to the session list.
        app.automation_ui.automation_panel_index = 1;
        app.handle_automations_pane_key(KeyCode::Char('j'));
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
        app.handle_automations_pane_key(KeyCode::Char('j')); // a → b
        assert_eq!(app.automation_ui.automation_panel_index, 1);
        app.handle_automations_pane_key(KeyCode::Char('j')); // past last → top session
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
        app.handle_automations_pane_key(KeyCode::Char('k'));
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
        // Set up a fake selection
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
        for _ in 0..50 {
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
        app.handle_key(KeyCode::Char('x'), KeyModifiers::CONTROL); // bind ctrl+x

        let action = crate::session::Action::rebindable_in_order()[0];
        assert_eq!(
            app.keybindings
                .lookup(KeyCode::Char('x'), KeyModifiers::CONTROL),
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
        let modals::Modal::Help(ref h) = app.modal else {
            panic!("Esc during capture should not close the help modal");
        };
        assert!(!h.capturing);
        assert_eq!(
            app.keybindings
                .lookup(KeyCode::Char('x'), KeyModifiers::CONTROL),
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

        // Automations / tasks / editors are not a scoped keybinding context.
        app.focus = InputFocus::Automations;
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

        // Rename session 1 to the spawn convention `task-<id>` → it's related.
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
        let areas = layout::compute_layout(
            Rect::new(0, 0, app.terminal_cols, app.terminal_rows),
            app.show_info_panel,
            app.show_tasks_panel,
            app.show_file_viewer,
            app.global_search.active,
            app.automation_ui.cached_automations.len(),
        );
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

    // --- Global search (Ctrl+A, fully rebindable) tests ---

    #[test]
    fn ctrl_a_opens_global_search() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::SessionList;
        // Ctrl+A is the default binding.
        app.handle_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert!(app.global_search.active);
        assert_eq!(app.focus, InputFocus::GlobalSearch);
        // Esc closes and restores the previous focus.
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.global_search.active);
        assert_eq!(app.focus, InputFocus::SessionList);
    }

    #[test]
    fn ctrl_slash_no_longer_opens_global_search() {
        // The hardcoded Ctrl+/ intercept was removed: global search is opened
        // solely via the (rebindable) `Action::GlobalSearch` chord. The former
        // Ctrl+/ encodings (`/`, `_`, `7`) must no longer open the strip.
        for c in ['/', '_', '7'] {
            let mut app = app_with_sessions(1);
            app.focus = InputFocus::SessionList;
            app.handle_key(KeyCode::Char(c), KeyModifiers::CONTROL);
            assert!(
                !app.global_search.active,
                "Ctrl+{c} must not open search anymore"
            );
        }
    }

    #[test]
    fn global_search_chord_is_rebindable() {
        let base = std::env::temp_dir().join("thurbox-gs-rebind-test");
        let _ = std::fs::remove_dir_all(&base);
        let _g = crate::paths::TestPathGuard::new(&base);

        let mut app = app_with_sessions(1);
        // Rebind global search from Ctrl+A to Ctrl+X via the F1 editor.
        app.keybindings.rebind(
            crate::session::Action::GlobalSearch,
            crate::session::KeyChord::ctrl('x'),
        );

        // The old chord no longer opens it...
        app.focus = InputFocus::SessionList;
        app.handle_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert!(!app.global_search.active, "old Ctrl+A must not open search");

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
                let home = std::env::var("HOME").expect("HOME set in tests");
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
    fn ctrl_r_no_op_without_agent_session_id() {
        let mut app = app_with_sessions(1);
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

    #[test]
    fn process_cwd_single_member_is_primary() {
        let cwd = PathBuf::from("/src/only");
        let out = resolve_process_cwd(Some("id-1"), Some(cwd.clone()), &[], &[]);
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

        let (tx, rx) = mpsc::channel();
        let stats = crate::session::GitStats {
            files_changed: 3,
            insertions: 10,
            deletions: 2,
            dirty: true,
            ahead: 1,
            behind: 0,
        };
        tx.send((sid, Some(stats.clone()))).unwrap();
        app.git_stats_in_progress = true;
        app.git_stats_rx = Some(rx);

        app.poll_git_stats();

        assert_eq!(app.sessions[0].info.git_stats, Some(stats));
        assert!(!app.git_stats_in_progress);
        assert!(app.git_stats_rx.is_none());
    }

    #[test]
    fn poll_git_stats_disconnected_clears_guard() {
        let mut app = app_with_sessions(1);
        let (tx, rx) = mpsc::channel::<(SessionId, Option<crate::session::GitStats>)>();
        drop(tx); // worker died without delivering
        app.git_stats_in_progress = true;
        app.git_stats_rx = Some(rx);

        app.poll_git_stats();

        assert!(!app.git_stats_in_progress);
        assert!(app.git_stats_rx.is_none());
    }

    #[test]
    fn poll_git_stats_empty_is_noop() {
        let mut app = app_with_sessions(1);
        let (_tx, rx) = mpsc::channel::<(SessionId, Option<crate::session::GitStats>)>();
        app.git_stats_in_progress = true;
        app.git_stats_rx = Some(rx);

        // No result yet: guard stays set, rx retained for the next poll.
        app.poll_git_stats();

        assert!(app.git_stats_in_progress);
        assert!(app.git_stats_rx.is_some());
    }

    #[test]
    fn poll_metrics_refresh_restores_sys_and_applies_metrics() {
        let mut app = app_with_sessions(1);
        let sid = app.sessions[0].info.id;

        // Simulate the worker having taken `sys`.
        app.metrics.sys = None;
        app.metrics_refresh_in_progress = true;

        let (tx, rx) = mpsc::channel();
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
        app.metrics_rx = Some(rx);

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
        assert!(!app.metrics_refresh_in_progress);
        assert!(app.metrics_rx.is_none());
    }

    #[test]
    fn poll_metrics_refresh_disconnected_recreates_sys() {
        let mut app = app_with_sessions(0);
        app.metrics.sys = None;
        app.metrics_refresh_in_progress = true;
        let (tx, rx) = mpsc::channel::<MetricsRefresh>();
        drop(tx); // worker died without returning `sys`
        app.metrics_rx = Some(rx);

        app.poll_metrics_refresh();

        assert!(app.metrics.sys.is_some());
        assert!(!app.metrics_refresh_in_progress);
        assert!(app.metrics_rx.is_none());
    }

    #[test]
    fn poll_worktree_create_continues_into_name_modal() {
        let mut app = app_with_sessions(0);
        let (tx, rx) = mpsc::channel();
        let wt = WorktreeInfo {
            repo_path: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo/.worktrees/feat"),
            branch: "feat".into(),
        };
        tx.send(Ok(vec![wt])).unwrap();
        app.worktree_create_in_progress = true;
        app.worktree_create_rx = Some(rx);
        app.pending_worktree_create = Some(PendingWorktreeCreate {
            backend: None,
            normal_repos: vec![PathBuf::from("/other")],
            session_name: None, // no name yet → routes through the name modal
        });

        app.poll_worktree_create();

        assert!(!app.worktree_create_in_progress);
        assert!(app.worktree_create_rx.is_none());
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
        let (tx, rx) = mpsc::channel::<Result<Vec<WorktreeInfo>, String>>();
        tx.send(Err("branch exists".into())).unwrap();
        app.worktree_create_in_progress = true;
        app.worktree_create_rx = Some(rx);
        app.pending_worktree_create = Some(PendingWorktreeCreate {
            backend: None,
            normal_repos: vec![],
            session_name: None,
        });

        app.poll_worktree_create();

        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert!(msg.text.contains("branch exists"));
        assert!(!app.worktree_create_in_progress);
        assert!(app.pending_worktree_create.is_none());
    }

    #[test]
    fn poll_worktree_create_disconnected_clears_guard() {
        let mut app = app_with_sessions(0);
        let (tx, rx) = mpsc::channel::<Result<Vec<WorktreeInfo>, String>>();
        drop(tx);
        app.worktree_create_in_progress = true;
        app.worktree_create_rx = Some(rx);
        app.pending_worktree_create = Some(PendingWorktreeCreate {
            backend: None,
            normal_repos: vec![],
            session_name: None,
        });

        app.poll_worktree_create();

        assert!(!app.worktree_create_in_progress);
        assert!(app.worktree_create_rx.is_none());
        assert!(app.pending_worktree_create.is_none());
    }

    #[test]
    fn poll_session_spawn_error_sets_status_and_adds_no_session() {
        let mut app = app_with_sessions(0);
        let (tx, rx) = mpsc::channel::<Result<Session, String>>();
        tx.send(Err("tmux exploded".into())).unwrap();
        app.session_spawn_in_progress = true;
        app.session_spawn_rx = Some(rx);
        app.pending_session_spawn = Some(PendingSessionSpawn {
            primary_cwd: None,
            worktrees: vec![],
            additional_dirs: vec![],
            task_prompt: None,
        });

        app.poll_session_spawn();

        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert!(msg.text.contains("tmux exploded"));
        assert!(app.sessions.is_empty());
        assert!(!app.session_spawn_in_progress);
        assert!(app.session_spawn_rx.is_none());
        assert!(app.pending_session_spawn.is_none());
    }

    #[test]
    fn poll_session_spawn_disconnected_clears_guard() {
        let mut app = app_with_sessions(0);
        let (tx, rx) = mpsc::channel::<Result<Session, String>>();
        drop(tx);
        app.session_spawn_in_progress = true;
        app.session_spawn_rx = Some(rx);
        app.pending_session_spawn = Some(PendingSessionSpawn {
            primary_cwd: None,
            worktrees: vec![],
            additional_dirs: vec![],
            task_prompt: None,
        });

        app.poll_session_spawn();

        assert!(!app.session_spawn_in_progress);
        assert!(app.session_spawn_rx.is_none());
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
        assert!(app.session_spawn_in_progress);

        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            app.poll_session_spawn();
            if !app.session_spawn_in_progress {
                break;
            }
        }

        assert!(!app.session_spawn_in_progress);
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
        app.session_spawn_in_progress = true;
        let config = SessionConfig::default();

        app.do_spawn_session_async("second".into(), &config, vec![]);

        // The in-flight guard/rx are untouched (no new background task kicked off).
        assert!(app.session_spawn_in_progress);
        assert!(app.session_spawn_rx.is_none());
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
        assert_eq!(app.active_theme, presets[1]);
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
