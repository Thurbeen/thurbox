mod helpers;
mod key_handlers;
pub(crate) mod modals;
mod state;
mod view;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Position, Rect},
    widgets::{Block, Borders},
};
use tracing::{error, info, warn};

use crate::agent::{BackendRegistry, GenericProvider, Session, SessionBackend};
use crate::git;
use crate::session::{
    AgentRegistry, ScheduledCommand, SessionConfig, SessionId, SessionInfo, SessionStatus,
    WorktreeInfo, DEFAULT_AGENT_NAME,
};
use crate::storage::Database;
use crate::storage::DeletedSessionInfo;
use crate::sync::{self, SharedWorktree, StateDelta, SyncState};
use crate::ui::selection::{PaneBounds, Selection, TermPos};
use crate::ui::{info_panel, layout, project_list};

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

pub use modals::ScheduleCommandField;

pub(crate) struct TextInput {
    pub(crate) buffer: String,
    pub(crate) cursor: usize,
}

#[allow(dead_code)] // some editing methods only exercised by tests
impl TextInput {
    fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
        }
    }

    fn insert(&mut self, c: char) {
        let byte_pos = self.byte_offset();
        self.buffer.insert(byte_pos, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let byte_pos = self.byte_offset();
            self.buffer.remove(byte_pos);
        }
    }

    fn delete(&mut self) {
        let byte_pos = self.byte_offset();
        if byte_pos < self.buffer.len() {
            self.buffer.remove(byte_pos);
        }
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        let char_count = self.buffer.chars().count();
        if self.cursor < char_count {
            self.cursor += 1;
        }
    }

    fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.buffer.chars().count();
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    fn set(&mut self, value: &str) {
        self.buffer = value.to_string();
        self.cursor = value.chars().count();
    }

    fn value(&self) -> &str {
        &self.buffer
    }

    fn cursor_pos(&self) -> usize {
        self.cursor
    }

    /// Convert char-based cursor position to byte offset.
    fn byte_offset(&self) -> usize {
        self.buffer
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.buffer.len())
    }
}

pub enum AppMessage {
    KeyPress(KeyCode, KeyModifiers),
    /// Text pasted via the terminal's bracketed paste mode.
    Paste(String),
    MouseScrollUp,
    MouseScrollDown,
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

#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub level: StatusLevel,
    pub created_at: std::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFocus {
    SessionList,
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
    pub(crate) db: Database,
    pub(crate) focus: InputFocus,
    pub(crate) should_quit: bool,
    pub(crate) status_message: Option<StatusMessage>,
    terminal_rows: u16,
    pub(crate) terminal_cols: u16,
    session_counter: usize,
    pub(crate) show_info_panel: bool,
    pub(crate) show_file_viewer: bool,
    pub(crate) file_viewer: crate::ui::file_viewer::FileViewerState,
    pub(crate) modal: modals::Modal,
    // (Delete project, add project, edit project modal state is now in self.modal)
    pub(crate) pending_repo_path: Option<PathBuf>,
    pub(crate) pending_all_repos: Option<Vec<PathBuf>>,
    /// Normal (non-worktree) repos to include alongside worktree repos.
    pub(crate) pending_normal_repos: Vec<PathBuf>,
    pub(crate) pending_base_branch: Option<String>,
    pub(crate) pending_session_name: Option<String>,
    pub(crate) pending_spawn_config: Option<SessionConfig>,
    pub(crate) pending_spawn_worktrees: Vec<WorktreeInfo>,
    /// Extra working directories (non-primary worktrees + normal repos) to
    /// attach to the spawned session's `SessionInfo`. Consumed by
    /// `do_spawn_session`.
    pub(crate) pending_additional_dirs: Vec<PathBuf>,
    pub(crate) pending_fork: bool,
    pub(crate) pending_restart: bool,
    pub(crate) pending_spawn_name: Option<String>,
    /// Inter-instance DB sync (polls for changes from other thurbox instances).
    sync_state: SyncState,
    /// Worktree-to-main git sync (Ctrl+S).
    worktree_sync_in_progress: bool,
    worktree_sync_rx: Option<mpsc::Receiver<(SessionId, git::SyncResult)>>,
    worktree_sync_pending: usize,
    worktree_sync_completed: Vec<(SessionId, git::SyncResult)>,
    tick_count: u64,
    /// System info collector for CPU/RAM metrics.
    sys: sysinfo::System,
    /// Cached system metrics for the info panel.
    system_metrics: crate::ui::info_panel::SystemMetrics,
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
    /// Cached text extracted from the frame buffer for the current selection.
    selected_text_cache: Option<String>,
    /// Persistent clipboard handle to avoid "dropped too quickly" warnings on Linux.
    clipboard: Option<arboard::Clipboard>,
    /// Reusable buffer for session elapsed-ms in the view (avoids per-frame allocation).
    pub(crate) session_elapsed_buf: Vec<u64>,
    /// Persistent list state for the session section (preserves scroll offset).
    pub(crate) session_list_state: ratatui::widgets::ListState,
    /// Whether the search input is active (accepting keystrokes).
    pub(crate) search_active: bool,
    /// Search input buffer.
    pub(crate) search_input: TextInput,
    /// Per-session fuzzy match positions (parallel to flat session list).
    /// Empty vec means no search is active.
    pub(crate) session_match_positions: Vec<Option<project_list::SessionMatch>>,
    /// Cached pending scheduled commands, refreshed every ~1 second.
    pub(crate) cached_pending_commands: Vec<ScheduledCommand>,
    /// Currently active theme preset, cached so the header doesn't hit SQLite
    /// every render. Kept in sync with `db.set_active_theme` writes.
    pub(crate) active_theme: crate::session::ThemePreset,
    /// User-customizable global keybindings. Loaded from
    /// `~/.config/thurbox/keybindings.json` on startup, falling back to defaults
    /// when the file is missing or malformed.
    pub(crate) keybindings: crate::session::KeyBindings,
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

        Self {
            sessions: Vec::new(),
            active_index: 0,
            backends,
            agents,
            db,
            focus: InputFocus::SessionList,
            should_quit: false,
            status_message: None,
            terminal_rows: rows,
            terminal_cols: cols,
            session_counter,
            show_info_panel: false,
            show_file_viewer: false,
            file_viewer: crate::ui::file_viewer::FileViewerState::new(),
            modal: modals::Modal::None,
            pending_repo_path: None,
            pending_all_repos: None,
            pending_normal_repos: Vec::new(),
            pending_base_branch: None,
            pending_session_name: None,
            pending_spawn_config: None,
            pending_spawn_worktrees: Vec::new(),
            pending_additional_dirs: Vec::new(),
            pending_fork: false,
            pending_restart: false,
            pending_spawn_name: None,
            sync_state,
            worktree_sync_in_progress: false,
            worktree_sync_rx: None,
            worktree_sync_pending: 0,
            worktree_sync_completed: Vec::new(),
            tick_count: 0,
            sys: sysinfo::System::new(),
            system_metrics: info_panel::SystemMetrics {
                cpu_percent: 0.0,
                memory_used: 0,
                memory_total: 0,
                session_cpu_percent: 0.0,
                session_memory_bytes: 0,
            },
            deferred_inputs: Vec::new(),
            session_terminal_views: HashMap::new(),
            pending_delete: None,
            text_selection: None,
            selected_text_cache: None,
            clipboard: arboard::Clipboard::new().ok(),
            session_elapsed_buf: Vec::new(),
            session_list_state: ratatui::widgets::ListState::default(),
            search_active: false,
            search_input: TextInput::new(),
            session_match_positions: Vec::new(),
            cached_pending_commands: Vec::new(),
            active_theme,
            keybindings,
        }
    }

    /// Build an [`AgentProvider`](crate::agent::AgentProvider) for a session
    /// config by looking its agent up in the registry. Falls back to the
    /// registry default, then to the built-in default, so a stale/unknown agent
    /// name never breaks spawning.
    pub fn provider_for(&self, config: &SessionConfig) -> Arc<dyn crate::agent::AgentProvider> {
        let def = self
            .agents
            .get(&config.agent)
            .or_else(|| self.agents.default_agent())
            .cloned()
            .unwrap_or_else(|| {
                crate::agent::agent_config::builtin_registry()
                    .default_agent()
                    .cloned()
                    .expect("built-in registry always has a default agent")
            });
        Arc::new(GenericProvider::new(def))
    }

    /// Open the repo picker modal for creating a new session.
    ///
    /// Loads bookmarks from the database, pre-selects repos from the active
    /// project (if any), and shows the repo picker modal.
    pub(crate) fn open_repo_picker(&mut self) {
        let bookmarks = match self.db.list_repo_bookmarks() {
            Ok(b) => b.into_iter().map(|bm| bm.repo_path).collect::<Vec<_>>(),
            Err(e) => {
                tracing::error!("Failed to load repo bookmarks: {e}");
                Vec::new()
            }
        };

        let selected = vec![false; bookmarks.len()];
        let worktree = vec![false; bookmarks.len()];
        let filtered_indices = (0..bookmarks.len()).collect();
        self.modal = modals::Modal::RepoPicker(modals::RepoPickerModal {
            bookmarks,
            selected,
            worktree,
            list_index: 0,
            path_input: modals::TextInput::new(),
            path_suggestion: None,
            focus: modals::RepoPickerFocus::default(),
            search_input: modals::TextInput::new(),
            filtered_indices,
        });
    }

    #[cfg(test)]
    fn next_session_name(&mut self) -> String {
        self.session_counter += 1;
        self.session_counter.to_string()
    }

    pub(crate) fn spawn_session_with_config(&mut self, config: &SessionConfig) {
        self.prepare_spawn(config.clone(), Vec::new());
    }

    /// Route session creation through the name modal, then agent selection.
    ///
    /// Shows an empty session-name modal. After the user enters a name, the
    /// agent picker is shown, then spawn.
    pub(crate) fn prepare_spawn(&mut self, config: SessionConfig, worktrees: Vec<WorktreeInfo>) {
        // Show session name modal (empty — user types from scratch).
        self.pending_spawn_config = Some(config);
        self.pending_spawn_worktrees = worktrees;
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
            self.do_spawn_session(name, &config, worktrees, false);
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
        self.pending_spawn_name = Some(name);
        self.pending_spawn_config = Some(config);
        self.pending_spawn_worktrees = worktrees;
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
        let cwd = session.info.cwd.clone();

        let mut config = SessionConfig {
            resume_session_id: None,
            agent_session_id: Some(agent_session_id.clone()),
            cwd,
            agent,
            fork_session_id: None,
            ..SessionConfig::default()
        };
        config.resume_session_id =
            crate::session_ops::resume_id_if_transcript_exists(&agent_session_id, &config.env);

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
        self.pending_restart = false;
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

        self.pending_spawn_config = Some(config);
        self.pending_spawn_worktrees = worktrees;
        self.pending_fork = true;

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
            // Clean up agent metrics file.
            if let Some(ref sid) = pending.session.info.agent_session_id {
                if let Some(metrics_dir) = crate::paths::metrics_directory() {
                    let _ = std::fs::remove_file(metrics_dir.join(format!("{sid}.json")));
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
            AppMessage::MouseScrollUp => self.scroll_terminal_up(MOUSE_SCROLL_LINES),
            AppMessage::MouseScrollDown => self.scroll_terminal_down(MOUSE_SCROLL_LINES),
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

    pub(crate) fn with_active_parser(&self, f: impl FnOnce(&mut vt100::Parser)) {
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
                // Lazily create the shell pane if it doesn't exist
                if needs_shell {
                    let (rows, cols) = self.content_area_size();
                    let session = &mut self.sessions[self.active_index];
                    if let Err(e) = session.ensure_shell_pane(rows, cols) {
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
        let areas = layout::compute_layout(area, self.show_info_panel, self.show_file_viewer);
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
        if let Some(ref mut sel) = self.text_selection {
            let (cx, cy) = sel.pane.clamp(x, y);
            sel.cursor = TermPos {
                row: cy as usize,
                col: cx as usize,
            };
        }
    }

    fn handle_mouse_up(&mut self, x: u16, y: u16) {
        self.handle_mouse_drag(x, y);

        if let Some(ref mut sel) = self.text_selection {
            sel.dragging = false;

            // If anchor == cursor, it was just a click (no drag) — clear selection
            if sel.anchor == sel.cursor {
                self.text_selection = None;
            }
        }
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
        let mut worktree_infos = Vec::new();
        let mut worktree_paths = Vec::new();

        for repo_path in repo_paths {
            match git::create_worktree(repo_path, new_branch, base_branch) {
                Ok(worktree_path) => {
                    worktree_infos.push(WorktreeInfo {
                        repo_path: repo_path.clone(),
                        worktree_path: worktree_path.clone(),
                        branch: new_branch.to_string(),
                    });
                    worktree_paths.push(worktree_path);
                }
                Err(e) => {
                    // Roll back already-created worktrees
                    for info in &worktree_infos {
                        if let Err(re) = git::remove_worktree(&info.repo_path, &info.worktree_path)
                        {
                            error!("Failed to roll back worktree: {re}");
                        }
                    }
                    error!("Failed to create worktree in {}: {e}", repo_path.display());
                    self.set_error(format!("Failed to create worktree: {e:#}"));
                    return;
                }
            }
        }

        // Combine remaining worktree paths + normal repos as additional dirs.
        let normal_repos = std::mem::take(&mut self.pending_normal_repos);
        let mut additional_dirs: Vec<PathBuf> = worktree_paths[1..].to_vec();
        additional_dirs.extend(normal_repos);
        self.pending_additional_dirs = additional_dirs;
        let config = SessionConfig {
            cwd: Some(worktree_paths[0].clone()),
            ..SessionConfig::default()
        };

        if let Some(name) = session_name {
            // Session name already known (worktree flow) — skip name modal.
            self.finish_prepare_spawn(name, config, worktree_infos);
        } else {
            self.prepare_spawn(config, worktree_infos);
        }
    }

    pub(crate) fn do_spawn_session(
        &mut self,
        name: String,
        config: &SessionConfig,
        worktrees: Vec<WorktreeInfo>,
        is_admin: bool,
    ) {
        let (rows, cols) = self.content_area_size();

        let mut config = config.clone();
        if config.agent.is_empty() {
            config.agent = self.agents.default_name();
        }
        if config.agent_session_id.is_none() {
            config.agent_session_id = Some(uuid::Uuid::new_v4().to_string());
        }

        let additional_dirs = std::mem::take(&mut self.pending_additional_dirs);

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

        let backend: Arc<dyn SessionBackend> = Arc::clone(self.backends.default_backend());

        let provider = self.provider_for(&config);
        match Session::spawn(name, rows, cols, &config, &backend, &provider) {
            Ok(mut session) => {
                session.info.worktrees = worktrees;
                session.info.additional_dirs = additional_dirs;

                resolve_repo_display_names(&mut session.info);
                self.sessions.push(session);
                self.active_index = self.sessions.len() - 1;
                self.focus = InputFocus::Terminal;
                self.status_message = None;

                // Mark admin sessions
                if is_admin {
                    self.sessions.last_mut().unwrap().info.is_admin = true;
                }

                self.save_state();
            }
            Err(e) => {
                error!("Failed to spawn session: {e}");
                self.set_error(format!("Failed to start claude: {e:#}"));
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
    /// Mirrors `ui::project_list::OrderedSessions::new`: admin sessions
    /// are pinned to the top (stable), the rest follow DB order. Used by
    /// the session-navigation helpers so `Ctrl+J`/`Ctrl+K` step through
    /// the list the user actually sees.
    fn render_order_indices(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.sessions.len()).collect();
        order.sort_by_key(|&i| !self.sessions[i].info.is_admin);
        order
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

    /// Clear all search/filter state.
    pub(crate) fn clear_search(&mut self) {
        self.search_active = false;
        self.search_input.buffer.clear();
        self.search_input.cursor = 0;
        self.session_match_positions.clear();
    }

    /// Recompute fuzzy match positions based on the current search query and
    /// snap selection to the first match if the current selection is a non-match.
    pub(crate) fn recompute_search_filter(&mut self) {
        let query = &self.search_input.buffer;
        if query.is_empty() {
            self.session_match_positions.clear();
            return;
        }
        self.session_match_positions = self
            .sessions
            .iter()
            .map(|s| {
                let info = &s.info;
                let name = crate::fuzzy::fuzzy_match(query, &info.name).map(|m| m.positions);
                let agent = crate::fuzzy::fuzzy_match(query, &info.agent).map(|m| m.positions);
                let branch = info
                    .worktrees
                    .first()
                    .and_then(|wt| crate::fuzzy::fuzzy_match(query, &wt.branch))
                    .map(|m| m.positions);
                let cwd = info
                    .repo_display_names
                    .iter()
                    .find_map(|n| crate::fuzzy::fuzzy_match(query, n))
                    .map(|m| m.positions);
                let status_str = info.status.to_string();
                let status = crate::fuzzy::fuzzy_match(query, &status_str).map(|m| m.positions);
                project_list::SessionMatch::from_matches(name, agent, branch, cwd, status)
            })
            .collect();
        // Snap to first match if current selection is a non-match.
        let current_matches = self
            .session_match_positions
            .get(self.active_index)
            .map(|m| m.is_some())
            .unwrap_or(false);
        if !current_matches {
            if let Some(first) = self
                .session_match_positions
                .iter()
                .position(|m| m.is_some())
            {
                self.active_index = first;
            }
        }
    }

    /// Navigate forward to the next matching item during search.
    pub(crate) fn search_navigate_forward(&mut self) {
        if self.session_match_positions.is_empty() {
            self.switch_session_forward();
            return;
        }
        let start = self.active_index + 1;
        let len = self.session_match_positions.len();
        for offset in 0..len {
            let idx = (start + offset) % len;
            if self.session_match_positions[idx].is_some() {
                self.active_index = idx;
                return;
            }
        }
    }

    /// Navigate backward to the previous matching item during search.
    pub(crate) fn search_navigate_backward(&mut self) {
        if self.session_match_positions.is_empty() {
            self.switch_session_backward();
            return;
        }
        let len = self.session_match_positions.len();
        let start = if self.active_index == 0 {
            len - 1
        } else {
            self.active_index - 1
        };
        for offset in 0..len {
            let idx = (start + len - offset) % len;
            if self.session_match_positions[idx].is_some() {
                self.active_index = idx;
                return;
            }
        }
    }

    fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.terminal_cols = cols;
        self.terminal_rows = rows;

        // Collapse info panel if terminal gets too narrow
        if cols < 120 {
            self.show_info_panel = false;
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
        self.tick_count = self.tick_count.wrapping_add(1);

        for session in &mut self.sessions {
            session.info.status = if session.has_exited() {
                SessionStatus::Idle
            } else if session.millis_since_last_output() > ACTIVITY_TIMEOUT_MS {
                SessionStatus::Waiting
            } else {
                SessionStatus::Busy
            };
        }

        // Poll for sync results from background worktree sync threads
        self.poll_sync_results();

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

        // Poll for external state changes from other thurbox instances (DB-based)
        if let Ok(Some(result)) = sync::poll_for_changes(&mut self.sync_state, &mut self.db) {
            if result.db_changed {
                // Pick up theme changes made by other thurbox processes (e.g.
                // an MCP `set_theme` call from another session).
                if let Ok(Some(name)) = self.db.get_active_theme() {
                    if let Some(preset) = crate::session::ThemePreset::from_str(&name) {
                        if preset != self.active_theme {
                            crate::ui::theme::set_active(preset.palette());
                            self.active_theme = preset;
                        }
                    }
                }
            }
            if !result.delta.is_empty() {
                self.handle_external_state_change(result.delta);
            }
        }

        // Process scheduled commands (once per second)
        self.process_scheduled_commands();

        // Refresh cached pending commands for UI (same cadence)
        if self.tick_count % 100 == 0 {
            self.refresh_pending_commands();
        }

        // Refresh system metrics periodically
        if self.tick_count % METRICS_REFRESH_TICKS == 0 {
            self.refresh_system_metrics();
        }
    }

    /// Collect CPU/RAM metrics from sysinfo and poll agent metrics files.
    fn refresh_system_metrics(&mut self) {
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();

        let cpu_percent = self.sys.global_cpu_usage();
        let memory_used = self.sys.used_memory();
        let memory_total = self.sys.total_memory();

        // Refresh only the active session's root process for CPU/RAM.
        let (session_cpu, session_mem) = self.active_session_metrics();

        self.system_metrics = info_panel::SystemMetrics {
            cpu_percent,
            memory_used,
            memory_total,
            session_cpu_percent: session_cpu,
            session_memory_bytes: session_mem,
        };

        // Poll agent metrics files written by the statusline script.
        if let Some(metrics_dir) = crate::paths::metrics_directory() {
            for session in &mut self.sessions {
                if let Some(ref agent_sid) = session.info.agent_session_id {
                    let path = metrics_dir.join(format!("{agent_sid}.json"));
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&content) {
                            session.info.agent_metrics = Some(parse_agent_metrics(&raw));
                        }
                    }
                }
            }
        }
    }

    /// Compute CPU% and memory (bytes) for the active session's root process.
    fn active_session_metrics(&mut self) -> (f32, u64) {
        let active = match self.sessions.get(self.active_index) {
            Some(s) => s,
            None => return (0.0, 0),
        };

        let root_pid = match active.pane_pid() {
            Ok(Some(pid)) => sysinfo::Pid::from_u32(pid),
            _ => return (0.0, 0),
        };

        let refresh_kind = sysinfo::ProcessRefreshKind::nothing()
            .with_memory()
            .with_cpu();
        self.sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[root_pid]),
            false,
            refresh_kind,
        );

        if let Some(proc_info) = self.sys.process(root_pid) {
            (proc_info.cpu_usage(), proc_info.memory())
        } else {
            (0.0, 0)
        }
    }

    /// Send deferred inputs whose scheduled tick has arrived.
    fn drain_deferred_inputs(&mut self) {
        let tick = self.tick_count;
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
        if let Some(rx) = &self.worktree_sync_rx {
            while let Ok((session_id, result)) = rx.try_recv() {
                self.worktree_sync_completed.push((session_id, result));
            }

            if self.worktree_sync_completed.len() >= self.worktree_sync_pending {
                self.worktree_sync_in_progress = false;
                self.worktree_sync_rx = None;
                self.finish_sync();
            }
        }
    }

    /// Finalize sync: compose status message and send conflict prompts.
    fn finish_sync(&mut self) {
        let results = std::mem::take(&mut self.worktree_sync_completed);
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
                    self.tick_count + DEFERRED_INPUT_DELAY_TICKS,
                ));
            }
        }
    }

    /// Start syncing the active session's worktrees with origin/main.
    ///
    /// Worktrees sharing the same parent repo are synced sequentially (to avoid
    /// concurrent `index.lock` contention), while different repos sync in parallel.
    pub(crate) fn start_sync(&mut self) {
        if self.worktree_sync_in_progress {
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

        self.worktree_sync_in_progress = true;
        self.worktree_sync_rx = Some(rx);
        self.worktree_sync_pending = count;
        self.worktree_sync_completed.clear();
        self.set_status(StatusLevel::Info, format!("Syncing {count} worktree(s)..."));
    }

    /// Handle external state changes detected from other instances.
    fn handle_external_state_change(&mut self, delta: StateDelta) {
        // Update session counter to avoid conflicts
        self.session_counter = self.session_counter.max(delta.counter_increment);

        // Handle removed sessions (deleted by other instances)
        for session_id in delta.removed_sessions {
            if let Some(pos) = self.sessions.iter().position(|s| s.info.id == session_id) {
                self.sessions.remove(pos);
                if self.active_index >= self.sessions.len() {
                    self.sync_active_session_to_project();
                }
            }
        }

        // Handle updated sessions (metadata changed by other instances)
        for shared_session in delta.updated_sessions {
            if let Some(session) = self
                .sessions
                .iter_mut()
                .find(|s| s.info.id == shared_session.id)
            {
                Self::apply_shared_session_metadata(session, &shared_session);
            }
        }

        // Handle added sessions from other instances.
        //
        // Headless spawns (CLI/MCP) persist the DB row with an empty
        // `backend_id` because only the TUI knows the real tmux pane id
        // (`%N`). Before spawning, call `discover()` and look up the
        // existing window by sanitized name — otherwise we'd create a
        // duplicate `tb-<name>` window for the one the CLI already
        // opened, and exact-match `send-keys` would then fail on
        // "ambiguous window".
        //
        // `discover()` is cached per backend_type so a burst of added
        // sessions only hits tmux once per backend.
        let mut discovered_by_backend: HashMap<
            String,
            Vec<crate::agent::backend::DiscoveredSession>,
        > = HashMap::new();
        for shared_session in delta.added_sessions {
            // Skip if we already have this session
            if self.sessions.iter().any(|s| s.info.id == shared_session.id) {
                continue;
            }

            let backend = self
                .backends
                .get(&shared_session.backend_type)
                .cloned()
                .unwrap_or_else(|| self.backends.default_backend().clone());

            let discovered = discovered_by_backend
                .entry(shared_session.backend_type.clone())
                .or_insert_with(|| backend.discover().unwrap_or_default());
            let matching_discovered = Self::find_matching_discovered(&shared_session, discovered);

            let (rows, cols) = self.content_area_size();
            if let Some(disc) = matching_discovered {
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
                    &disc.backend_id,
                    &backend,
                    &provider,
                    HashMap::new(),
                ) {
                    Ok(mut adopted_session) => {
                        // Preserve the original session ID from shared
                        // state (Session::adopt creates a new one, but we
                        // need the consistent ID).
                        adopted_session.info.id = shared_session.id;
                        Self::apply_shared_session_metadata(&mut adopted_session, &shared_session);
                        self.sessions.push(adopted_session);
                        // Persist the real pane_id (`%N`) back to the DB
                        // so future lookups short-circuit on the
                        // backend_id match instead of always falling back
                        // to name matching.
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
                            disc.backend_id,
                            e
                        );
                    }
                }
                // Either adoption succeeded or the discovered window
                // already exists and adoption failed transiently — in
                // both cases we must NOT fall through to spawn, because
                // that would create a second window with the same name.
                continue;
            }

            // No matching discovered window. If the session has an
            // `agent_session_id`, spawn a fresh window with
            // `--session-id` so claude creates the conversation (e.g.
            // CLI-created sessions whose claude process never persisted
            // a conversation before we first adopt them).
            if let Some(ref agent_sid) = shared_session.agent_session_id {
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
                config.resume_session_id =
                    crate::session_ops::resume_id_if_transcript_exists(agent_sid, &config.env);
                let provider = self.provider_for(&config);

                if let Ok(mut spawned) = Session::spawn(
                    shared_session.name.clone(),
                    rows,
                    cols,
                    &config,
                    &backend,
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
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

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
    /// All sessions live on the local-tmux backend and are restored
    /// synchronously by querying tmux for existing windows.
    pub fn restore_sessions(&mut self, sessions: Vec<sync::SharedSession>, session_counter: usize) {
        self.session_counter = session_counter;

        let mut local_sessions = Vec::new();
        for shared in sessions {
            if shared.agent_session_id.is_none() {
                continue; // Skip sessions without a claude session ID
            }
            local_sessions.push(shared);
        }

        // --- Local-tmux sessions: restore synchronously (fast) ---
        // Discover existing sessions from the default (local-tmux) backend only.
        let mut discovered = Vec::new();
        let default_backend = self.backends.default_backend().clone();
        match default_backend.discover() {
            Ok(disc) => discovered.extend(disc),
            Err(e) => {
                warn!(
                    backend = default_backend.name(),
                    "Failed to discover sessions from backend: {e}"
                );
            }
        }

        for shared in local_sessions {
            self.restore_single_session(shared, &discovered);
        }

        // Claim ownership of restored sessions in the shared state
        self.save_state();
    }

    /// Restore a single local-tmux session synchronously (used during startup).
    fn restore_single_session(
        &mut self,
        shared: sync::SharedSession,
        discovered: &[crate::agent::backend::DiscoveredSession],
    ) {
        let name = shared.name.clone();
        let session_id = shared.id;

        let agent = if shared.agent.is_empty() {
            DEFAULT_AGENT_NAME.to_string()
        } else {
            shared.agent.clone()
        };

        let worktrees: Vec<WorktreeInfo> =
            shared.worktrees.iter().cloned().map(Into::into).collect();

        let agent_session_id = match shared.agent_session_id {
            Some(ref id) => id.clone(),
            None => return,
        };

        let matching_discovered = Self::find_matching_discovered(&shared, discovered);

        // Select the correct backend based on the persisted backend_type.
        let backend = self
            .backends
            .get(&shared.backend_type)
            .cloned()
            .unwrap_or_else(|| self.backends.default_backend().clone());

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

        if let Some(mut session) = adopted {
            session.info.id = session_id;
            session.info.agent_session_id = Some(agent_session_id.clone());
            session.info.cwd = shared.cwd.clone();
            session.info.additional_dirs = shared.additional_dirs.clone();
            session.info.agent = agent;
            session.info.worktrees = worktrees.clone();
            resolve_repo_display_names(&mut session.info);

            // Re-adopt shell pane if one was persisted
            if let Some(shell_bid) = &shared.shell_backend_id {
                if discovered
                    .iter()
                    .any(|d| d.backend_id == *shell_bid && d.is_alive)
                {
                    let (rows, cols) = self.content_area_size();
                    if let Err(e) = session.adopt_shell_pane(shell_bid, rows, cols) {
                        tracing::warn!("Failed to re-adopt shell pane: {e}");
                    }
                }
            }

            self.sessions.push(session);
            self.active_index = self.sessions.len() - 1;
            self.focus = InputFocus::Terminal;
        } else {
            // No matching backend session or adopt failed — respawn with
            // `--resume` when a transcript already exists for this
            // agent_session_id, otherwise `--session-id` so claude creates
            // the conversation (e.g. CLI-created sessions whose claude
            // process never persisted a conversation).
            if let Err(e) = self.db.soft_delete_session(session_id) {
                error!("Failed to soft-delete stale session {session_id}: {e}");
            }

            let mut config = SessionConfig {
                resume_session_id: None,
                agent_session_id: Some(agent_session_id.clone()),
                cwd: shared.cwd,
                agent,
                fork_session_id: None,
                ..SessionConfig::default()
            };
            config.resume_session_id =
                crate::session_ops::resume_id_if_transcript_exists(&agent_session_id, &config.env);
            self.pending_additional_dirs = shared.additional_dirs;
            self.do_spawn_session(name, &config, worktrees, false);
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

    /// Process due scheduled commands from the database (fallback).
    ///
    /// The primary dispatch is via `tmux run-shell -b -d` timers set at
    /// scheduling time. This tick-loop catches commands whose tmux timer
    /// failed or was never set (e.g., scheduled while Thurbox was down).
    /// Throttled to once per second (~100 ticks at 10ms each).
    fn process_scheduled_commands(&mut self) {
        if self.tick_count % 100 != 0 {
            return;
        }

        let commands = match self.db.due_scheduled_commands() {
            Ok(cmds) => cmds,
            Err(e) => {
                error!("Failed to fetch due scheduled commands: {e}");
                return;
            }
        };

        for cmd in commands {
            // Skip if we don't have this session — the tmux timer can handle it independently.
            let Some(session) = self.sessions.iter().find(|s| s.info.id == cmd.session_id) else {
                continue;
            };

            let mut paste = b"\x1b[200~".to_vec();
            paste.extend_from_slice(cmd.command_text.as_bytes());
            paste.extend_from_slice(b"\x1b[201~");
            if let Err(e) = session.send_input(paste) {
                error!(
                    "Failed to send scheduled command {} to session {}: {e}",
                    cmd.id, cmd.session_id
                );
            } else {
                self.deferred_inputs.push((
                    cmd.session_id,
                    b"\r".to_vec(),
                    self.tick_count + DEFERRED_INPUT_DELAY_TICKS,
                ));
                info!(
                    "Executed scheduled command {} for session {}",
                    cmd.id, cmd.session_id
                );
            }

            // Mark as executed to prevent the tmux timer from duplicating
            if let Err(e) = self.db.mark_scheduled_command_executed(cmd.id) {
                error!(
                    "Failed to mark scheduled command {} as executed: {e}",
                    cmd.id
                );
            }
        }
    }

    /// Open the scheduled commands list modal showing all pending commands.
    fn open_scheduled_commands_list(&mut self) {
        self.refresh_pending_commands();
        let commands: Vec<modals::ScheduledCommandListEntry> = self
            .cached_pending_commands
            .iter()
            .map(|cmd| {
                let session_name = self
                    .sessions
                    .iter()
                    .find(|s| s.info.id == cmd.session_id)
                    .map(|s| s.info.name.clone())
                    .unwrap_or_else(|| "?".to_string());
                let now = crate::sync::current_time_millis();
                let remaining = cmd.scheduled_at.saturating_sub(now);
                modals::ScheduledCommandListEntry {
                    id: cmd.id,
                    session_name,
                    command_text: cmd.command_text.clone(),
                    countdown: view::format_countdown(remaining),
                }
            })
            .collect();
        self.modal = modals::Modal::ScheduledCommandsList(modals::ScheduledCommandsListModal {
            index: 0,
            commands,
        });
    }

    /// Open the schedule-command modal for the active session.
    fn open_schedule_command_modal(&mut self) {
        if self.sessions.is_empty() {
            self.set_error("No active session to schedule a command for");
            return;
        }
        self.modal = modals::Modal::ScheduleCommand(modals::ScheduleCommandModal::default());
    }

    /// Open the schedule-command modal pre-filled for editing an existing command.
    fn open_edit_scheduled_command(&mut self, entry: modals::ScheduledCommandListEntry) {
        let now = crate::sync::current_time_millis();
        let cached = self
            .cached_pending_commands
            .iter()
            .find(|cmd| cmd.id == entry.id);
        let remaining_minutes = cached
            .map(|cmd| cmd.scheduled_at.saturating_sub(now) / 60_000)
            .unwrap_or(1)
            .max(1);
        let session_id = cached.map(|cmd| cmd.session_id).unwrap_or_default();

        let mut modal = modals::ScheduleCommandModal::default();
        modal.command.set(&entry.command_text);
        modal.delay_minutes.set(&remaining_minutes.to_string());
        modal.editing = Some(modals::EditingCommand {
            id: entry.id,
            session_id,
            session_name: entry.session_name.clone(),
        });
        self.modal = modals::Modal::ScheduleCommand(modal);
    }

    /// Validate and submit the schedule-command modal.
    fn submit_schedule_command(&mut self) {
        let modals::Modal::ScheduleCommand(ref sc) = self.modal else {
            return;
        };
        let command_text = sc.command.value().trim().to_string();
        if command_text.is_empty() {
            self.set_error("Command cannot be empty");
            return;
        }
        let delay_minutes: u64 = match sc.delay_minutes.value().trim().parse() {
            Ok(v) if v > 0 => v,
            _ => {
                self.set_error("Delay must be a positive number of minutes");
                return;
            }
        };
        let editing = sc.editing.clone();

        let (session_id, session_name) = self.resolve_schedule_session(&editing);
        let Some(session_id) = session_id else {
            self.set_error("No active session");
            return;
        };

        let delay_seconds = delay_minutes * 60;
        let now = crate::sync::current_time_millis();
        let scheduled_at = now + delay_seconds * 1000;

        let id = match self
            .db
            .create_scheduled_command(session_id, &command_text, scheduled_at)
        {
            Ok(id) => id,
            Err(e) => {
                error!("Failed to create scheduled command: {e}");
                self.set_error("Failed to schedule command");
                return;
            }
        };

        if let Some(ref ed) = editing {
            let _ = self.db.cancel_scheduled_command(ed.id);
        }
        self.setup_tmux_timer(&session_name, &command_text, delay_seconds, id);
        self.modal.close();
        self.refresh_pending_commands();
        let verb = if editing.is_some() {
            "updated"
        } else {
            "scheduled"
        };
        self.set_status(
            StatusLevel::Success,
            format!("Command {verb} for {session_name} in {delay_minutes}m"),
        );
    }

    /// Resolve the target session for a scheduled command (editing or active).
    fn resolve_schedule_session(
        &self,
        editing: &Option<modals::EditingCommand>,
    ) -> (Option<SessionId>, String) {
        if let Some(ref ed) = editing {
            return (Some(ed.session_id), ed.session_name.clone());
        }
        match self.sessions.get(self.active_index) {
            Some(s) => (Some(s.info.id), s.info.name.clone()),
            None => (None, String::new()),
        }
    }

    /// Set up a tmux background timer for a scheduled command.
    fn setup_tmux_timer(
        &self,
        session_name: &str,
        command_text: &str,
        delay_seconds: u64,
        id: i64,
    ) {
        if let Some(db_path) = crate::paths::database_file() {
            if let Err(e) = crate::agent::tmux::schedule_tmux_command(
                session_name,
                command_text,
                delay_seconds,
                id,
                &db_path,
            ) {
                warn!("Failed to set tmux timer for command {id}: {e}");
            }
        }
    }

    /// Refresh the cached list of pending scheduled commands from the database.
    fn refresh_pending_commands(&mut self) {
        match self.db.list_pending_scheduled_commands() {
            Ok(cmds) => self.cached_pending_commands = cmds,
            Err(e) => error!("Failed to list pending commands: {e}"),
        }
    }

    /// Cancel a scheduled command by ID and refresh the cache.
    fn cancel_scheduled_command_by_id(&mut self, id: i64) {
        match self.db.cancel_scheduled_command(id) {
            Ok(true) => {
                self.refresh_pending_commands();
                self.set_status(StatusLevel::Success, "Scheduled command cancelled");
            }
            Ok(false) => self.set_error("Command already executed or cancelled"),
            Err(e) => {
                error!("Failed to cancel scheduled command {id}: {e}");
                self.set_error("Failed to cancel command");
            }
        }
    }

    pub(crate) fn content_area_size(&self) -> (u16, u16) {
        let area = Rect::new(0, 0, self.terminal_cols, self.terminal_rows);
        let terminal =
            layout::compute_layout(area, self.show_info_panel, self.show_file_viewer).terminal;
        let inner = Block::default().borders(Borders::ALL).inner(terminal);
        (inner.height, inner.width)
    }
}

/// Populate `repo_display_names` on a session from worktree repo paths,
/// cwd, and additional_dirs, using git remote names where available.
///
/// The order matches `build_repo_entries`: worktree repos first (by their
/// original `repo_path`), then non-worktree additional dirs.
fn resolve_repo_display_names(info: &mut SessionInfo) {
    let mut names = Vec::new();

    // Collect worktree checkout paths for filtering additional_dirs later.
    let wt_paths: std::collections::HashSet<&std::path::Path> = info
        .worktrees
        .iter()
        .map(|wt| wt.worktree_path.as_path())
        .collect();

    if !info.worktrees.is_empty() {
        // Worktree repos: resolve from original repo_path (has git remote).
        for wt in &info.worktrees {
            if let Some(name) = git::repo_display_name(&wt.repo_path) {
                names.push(name);
            }
        }
    } else if let Some(ref cwd) = info.cwd {
        // No worktrees: use cwd as the primary repo.
        if let Some(name) = git::repo_display_name(cwd) {
            names.push(name);
        }
    }

    // Non-worktree additional dirs (skip paths that are worktree checkouts).
    for dir in &info.additional_dirs {
        if !wt_paths.contains(dir.as_path()) {
            if let Some(name) = git::repo_display_name(dir) {
                names.push(name);
            }
        }
    }

    info.repo_display_names = names;
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::agent::SessionBackend;

    // --- TextInput tests ---

    #[test]
    fn text_input_new_is_empty() {
        let input = TextInput::new();
        assert_eq!(input.value(), "");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_insert_appends_chars() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor_pos(), 3);
    }

    #[test]
    fn text_input_insert_at_middle() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('c');
        input.move_left();
        input.insert('b');
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_backspace_removes_before_cursor() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        input.backspace();
        assert_eq!(input.value(), "ab");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_backspace_at_start_is_noop() {
        let mut input = TextInput::new();
        input.insert('a');
        input.home();
        input.backspace();
        assert_eq!(input.value(), "a");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_delete_removes_at_cursor() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        input.home();
        input.delete();
        assert_eq!(input.value(), "bc");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_delete_at_end_is_noop() {
        let mut input = TextInput::new();
        input.insert('a');
        input.delete();
        assert_eq!(input.value(), "a");
        assert_eq!(input.cursor_pos(), 1);
    }

    #[test]
    fn text_input_move_left_and_right() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        assert_eq!(input.cursor_pos(), 2);

        input.move_left();
        assert_eq!(input.cursor_pos(), 1);

        input.move_right();
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_move_left_at_zero_is_noop() {
        let mut input = TextInput::new();
        input.move_left();
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_move_right_at_end_is_noop() {
        let mut input = TextInput::new();
        input.insert('a');
        input.move_right();
        assert_eq!(input.cursor_pos(), 1);
    }

    #[test]
    fn text_input_home_and_end() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');

        input.home();
        assert_eq!(input.cursor_pos(), 0);

        input.end();
        assert_eq!(input.cursor_pos(), 3);
    }

    #[test]
    fn text_input_clear_resets_buffer_and_cursor() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.clear();
        assert_eq!(input.value(), "");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_multibyte_chars() {
        let mut input = TextInput::new();
        input.insert('é');
        input.insert('ñ');
        assert_eq!(input.value(), "éñ");
        assert_eq!(input.cursor_pos(), 2);

        input.move_left();
        input.insert('ü');
        assert_eq!(input.value(), "éüñ");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_backspace_multibyte() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('é');
        input.insert('b');
        input.backspace();
        assert_eq!(input.value(), "aé");

        input.backspace();
        assert_eq!(input.value(), "a");
    }

    #[test]
    fn text_input_delete_multibyte() {
        let mut input = TextInput::new();
        input.insert('é');
        input.insert('b');
        input.home();
        input.delete();
        assert_eq!(input.value(), "b");
        assert_eq!(input.cursor_pos(), 0);
    }

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
    fn switch_forward_follows_admin_pinned_render_order() {
        // Sessions stored in DB order: [normal-0, normal-1, admin-2].
        // Rendered order (admin pinned): [admin-2, normal-0, normal-1].
        // Stepping forward from index 0 (normal-0) must visit the *next
        // rendered row*, normal-1 (index 1), not admin-2. Stepping
        // forward from normal-1 must wrap to admin-2 (index 2).
        let mut app = app_with_sessions(3);
        app.sessions[2].info.is_admin = true;

        app.active_index = 0;
        app.switch_session_forward();
        assert_eq!(app.active_index, 1, "normal-0 → normal-1");
        app.switch_session_forward();
        assert_eq!(app.active_index, 2, "normal-1 → admin-2 (wrap)");
        app.switch_session_forward();
        assert_eq!(app.active_index, 0, "admin-2 → normal-0");
    }

    #[test]
    fn switch_backward_follows_admin_pinned_render_order() {
        let mut app = app_with_sessions(3);
        app.sessions[2].info.is_admin = true;

        app.active_index = 0; // normal-0, rendered row 1
        app.switch_session_backward();
        assert_eq!(app.active_index, 2, "normal-0 → admin-2 (prev row)");
        app.switch_session_backward();
        assert_eq!(app.active_index, 1, "admin-2 → normal-1 (wrap)");
        app.switch_session_backward();
        assert_eq!(app.active_index, 0, "normal-1 → normal-0");
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

    // --- Role editor tests ---

    #[test]
    fn text_input_set_replaces_content_and_moves_cursor_to_end() {
        let mut input = TextInput::new();
        input.insert('x');
        input.set("hello");
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor_pos(), 5);
    }

    #[test]
    fn text_input_set_empty_clears() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.set("");
        assert_eq!(input.value(), "");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn ctrl_h_cycles_focus_backward_from_terminal() {
        let mut app = app_with_sessions(1);
        app.focus = InputFocus::Terminal;
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
                matches!(app.modal, modals::Modal::Help),
                "F1 should show help from {focus:?}"
            );
        }
    }

    #[test]
    fn f1_does_not_activate_during_modal() {
        let mut app = app_with_sessions(0);
        app.modal = modals::Modal::RepoPicker(modals::RepoPickerModal::default());
        app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
        assert!(!matches!(app.modal, modals::Modal::Help));
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
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::Terminal);
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, InputFocus::SessionList);
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

    // --- Worktree sync tests ---

    #[test]
    fn start_sync_with_no_sessions_shows_info() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.start_sync();
        assert!(!app.worktree_sync_in_progress);
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Info);
        assert_eq!(msg.text, "No worktrees to sync");
    }

    #[test]
    fn start_sync_ignores_if_already_in_progress() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        app.worktree_sync_in_progress = true;
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
        assert!(app.worktree_sync_in_progress);
        assert_eq!(app.worktree_sync_pending, 1);
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
        assert!(!app.worktree_sync_in_progress);
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
        assert!(!app.worktree_sync_in_progress);
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
        assert!(app.worktree_sync_in_progress);
        assert_eq!(app.worktree_sync_pending, 1);
    }

    #[test]
    fn tick_increments_tick_count() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        assert_eq!(app.tick_count, 0);
        app.tick();
        assert_eq!(app.tick_count, 1);
        app.tick();
        assert_eq!(app.tick_count, 2);
    }

    #[test]
    fn finish_sync_all_synced_shows_success() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let id = SessionId::default();
        app.worktree_sync_completed = vec![
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
        app.worktree_sync_completed = vec![(
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
        app.worktree_sync_completed = vec![
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
        app.worktree_sync_completed = vec![
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
    fn drain_deferred_inputs_sends_at_correct_tick() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let id = SessionId::default();
        app.deferred_inputs.push((id, b"hello".to_vec(), 5));

        // Before target tick: nothing drained
        app.tick_count = 4;
        app.drain_deferred_inputs();
        assert_eq!(app.deferred_inputs.len(), 1);

        // At target tick: drained (no matching session, but entry is removed)
        app.tick_count = 5;
        app.drain_deferred_inputs();
        assert!(app.deferred_inputs.is_empty());
    }

    #[test]
    fn drain_deferred_inputs_retains_future_items() {
        let mut app = App::new(24, 80, stub_backend(), stub_agents(), test_db());
        let id = SessionId::default();
        app.deferred_inputs.push((id, b"early".to_vec(), 5));
        app.deferred_inputs.push((id, b"late".to_vec(), 20));

        app.tick_count = 5;
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

        app.worktree_sync_in_progress = true;
        app.worktree_sync_rx = Some(rx);
        app.worktree_sync_pending = 1;

        app.poll_sync_results();

        assert!(!app.worktree_sync_in_progress);
        assert!(app.worktree_sync_rx.is_none());
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

        app.worktree_sync_in_progress = true;
        app.worktree_sync_rx = Some(rx);
        app.worktree_sync_pending = 2;

        app.poll_sync_results();

        // Still in progress — only 1 of 2 received
        assert!(app.worktree_sync_in_progress);
        assert!(app.worktree_sync_rx.is_some());
        assert_eq!(app.worktree_sync_completed.len(), 1);
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
}
