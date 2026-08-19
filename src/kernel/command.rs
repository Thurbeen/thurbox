//! The write side: commands a plugin issues, executed off the render path.
//!
//! Design.md D5's other half. A plugin **cannot** wait for anything, so a
//! state-changing operation is not a function that returns a result — it is a
//! command that is accepted immediately and whose effect appears in a later
//! snapshot. That removes a whole class of stall (SQLite contention, a git
//! shell-out, an unreachable SSH host) for plugins nobody has written yet.
//!
//! Three consequences the spec requires and this implements:
//!
//! - issuing a command returns at once, always;
//! - work in flight is *readable*, so a plugin can draw it rather than leaving
//!   an unexplained gap (v1 needed `PendingSpawn` for exactly this);
//! - a failure surfaces through a later snapshot, not as an immediate error.
//!
//! Each command runs on its own thread with its own database connection. That
//! is deliberate: a slow delete on an unreachable host must not queue behind
//! anything, nor hold a connection the UI thread wants.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use super::registry::Value as SettingValue;
use crate::session::SessionId;
use crate::storage::Database;

/// A state change a plugin asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Soft-delete by default; `force` also tears down the pane and worktrees.
    Delete {
        session: String,
        force: bool,
    },
    Restore {
        session: String,
        /// A force-deleted session lost its worktree, so restoring recovers
        /// committed work only. Refused unless the caller says it knows.
        best_effort: bool,
    },
    Restart {
        session: String,
    },
    /// Paste text into a running session's agent.
    Send {
        session: String,
        text: String,
    },
    /// Move a session up (-1) or down (+1) in the manual order.
    Reorder {
        session: String,
        delta: i64,
    },
    /// Persist an explicit manual order, densely renumbered.
    ///
    /// The permutation is computed by the pane that *renders* the list, because
    /// only it knows the rendered order — the repo grouping, the parent/child
    /// nesting, and therefore which block a move actually swaps. A kernel-side
    /// move can only swap adjacent rows of a flat list, which is how v1's
    /// whole-group and subtree moves got lost. Ids the database no longer has
    /// are ignored rather than refused, since the list may have shrunk between
    /// the paint and the press.
    Order {
        list: Vec<String>,
    },
    /// Bring a new session into existence.
    ///
    /// The slowest thing thurbox does — a fetch, a worktree checkout, possibly
    /// an ssh connect and a process launch — which is exactly why it is a
    /// command and why its phases are published.
    Create {
        name: String,
        repo: String,
        branch: Option<String>,
        base: Option<String>,
        agent: Option<String>,
        host: Option<String>,
        /// Further repositories this session spans, each either taking its own
        /// worktree on `branch` or attached as it is.
        ///
        /// One command carries every member rather than the flow issuing one
        /// per repository: the pipeline builds them together and rolls the
        /// whole thing back on failure, and a session half-created by three
        /// commands has no owner.
        extras: Vec<ExtraMember>,
    },
    /// Remember, forget, or import a folder of repositories.
    ///
    /// A read would be the wrong shape: this is an explicit act with a side
    /// effect that outlives the flow, and it is validated before it is written
    /// — a path that does not exist is refused here rather than failing minutes
    /// later at worktree creation.
    Bookmark {
        /// `""` for the local machine, else a backend name (`ssh:<name>` /
        /// `wsl:<name>`) — the scope key the memory is kept under.
        host: String,
        /// As typed, tilde and all: expanding it needs the target machine, so
        /// it happens on the worker.
        path: String,
        edit: BookmarkEdit,
    },
    /// Fork a session, recording it as the new session's parent.
    Fork {
        session: String,
        name: String,
    },
    /// Bring a session's worktree up to date with the branch it came from.
    Sync {
        session: String,
    },
    /// Create a task, or change one that exists.
    Task {
        /// `None` creates; `Some` edits.
        id: Option<i64>,
        title: Option<String>,
        status: Option<String>,
        delete: bool,
    },
    /// Hand a task to an agent — an existing session, or a new one.
    DispatchTask {
        task: i64,
        /// `None` creates a session for it.
        session: Option<String>,
    },
    /// Enable, disable, run or delete an automation.
    Automation {
        id: i64,
        enabled: Option<bool>,
        run_now: bool,
        delete: bool,
    },
    /// Copy a session's visible terminal contents to the clipboard.
    ///
    /// Applied on the UI thread: the vt100 screen lives behind a `!Send` VM
    /// neighbour and the clipboard wants the tty, neither of which a worker can
    /// reach.
    Copy {
        session: String,
    },
    /// Recompute a session's diff, discarding the one already published.
    ///
    /// UI-thread applied: it only drops a cache entry, and the loop re-requests
    /// on the next frame — the recompute itself is the worker's, as it always was.
    ///
    /// This exists because a diff was otherwise computed **once per session per
    /// process** and never again: `request` returns early when it already holds an
    /// answer, and nothing ever invalidated one. A pane watching an agent that is
    /// still writing code would show a diff frozen at first sight, which is worse
    /// than showing none. The store's own doctrine — a cached answer carries an
    /// age, not just a value — was not being met.
    Diff {
        session: String,
    },
    /// Focus a pane by name.
    ///
    /// UI-thread applied: focus is the loop's state, and there is nothing to
    /// wait for.
    Focus {
        plugin: String,
        /// Focus this pane, or — when it already holds focus — return to whatever
        /// was focused before it.
        ///
        /// Here rather than in each plugin because the kernel is what remembers
        /// where focus came from (`focus_return`, the same memory `Esc` uses). A
        /// pane implementing this itself would have to name the pane to go back
        /// to, and the only name it could hard-code is the one that happens to
        /// share its slot today — which is the user's arrangement, not the
        /// plugin's to assume.
        toggle: bool,
    },
    /// Open a shell beside a session's agent.
    ///
    /// UI-thread applied: the pane is wired into the same `!Send` world the
    /// agent's is.
    Shell {
        session: String,
    },
    /// Start — or close — an interactive program in a pane a plugin owns.
    ///
    /// UI-thread applied for the same reason `Shell` is: the pane is wired into
    /// the `!Send` world the agent's terminal lives in.
    ///
    /// `owner` is **stamped by the kernel** from the plugin currently executing,
    /// never read from Lua. That is what makes naming another plugin's pane
    /// impossible by construction rather than refused by a check — the same
    /// reasoning that keeps `run`'s implementation in the VM registry instead of
    /// in globals.
    Program {
        owner: String,
        name: String,
        /// What to run. Empty when closing.
        program: String,
        argv: Vec<String>,
        /// Give the pane up instead of starting it.
        close: bool,
    },
    /// Open a session's working directory in the configured editor.
    ///
    /// UI-thread applied, and the reason is the whole difficulty of v1's
    /// `Ctrl+O`: an editor needs a real controlling tty, which is either a tmux
    /// popup or this process's own terminal with the TUI stood down
    /// (`run_pending_editor` in `src/main.rs`). A worker thread has neither.
    Editor {
        session: String,
    },
    /// Open a link, falling back to copying it where nothing can open it.
    ///
    /// Applied on the UI thread with copy, for the same reason: the fallback
    /// needs the clipboard, which wants the tty.
    OpenLink {
        url: String,
    },
    /// Change or reset a declared setting.
    ///
    /// Like `Theme`, applied on the UI thread: it mutates the registry, which
    /// lives in-process.
    Setting {
        key: String,
        /// `None` resets to the plugin's declared default.
        value: Option<SettingValue>,
    },
    /// Make a theme active and persist the choice.
    ///
    /// The one command naming no session — the theme is global — which is why
    /// [`Command::session`] returns an empty string for it. It is also the one
    /// command applied on the UI thread rather than a worker, because it
    /// mutates in-process state a worker cannot reach.
    Theme {
        name: String,
    },
    /// Let a soft-deleted session's agent go, once its undo window has closed.
    ///
    /// Issued by the loop rather than by a plugin: it is a consequence of time
    /// passing, not of anything anyone pressed. Worktrees are left alone — they
    /// are what makes the undo lossless.
    Reap {
        session: String,
    },
    /// Write the user's settings back to `settings.toml`.
    ///
    /// Not parseable from a plugin, and deliberately: a pane may *read* what the
    /// user configured (`thurbox.settings`) but writing their file is the
    /// settings modal's business, which is kernel-owned chrome. So this variant
    /// has no arm in [`Command::parse`] — there is no spelling of it a plugin
    /// could produce.
    ///
    /// A command rather than a UI-thread write because `save_settings` re-parses
    /// the document to preserve its comments, and a read-only filesystem must
    /// surface as a reported failure like any other.
    Configure {
        settings: Box<crate::session::settings::Settings>,
    },
    /// Restore or remove one file of the interface itself.
    ///
    /// The reason this is a command at all: plugins have no filesystem, so the
    /// pane that lists them cannot write one — and must not be given the
    /// capability, since it is an ordinary bundled plugin and those hold none
    /// a user's own could not have (design D3). Applied on the UI thread like
    /// [`Command::Theme`]: it is two file operations, and the watcher turns the
    /// write into a reload, so a worker would only add a race.
    Plugin {
        /// Path relative to the interface directory.
        file: String,
        edit: PluginEdit,
    },
}

/// A further repository a new session spans.
///
/// The kernel's spelling of [`crate::session::automation::ExtraRepo`], which is
/// what it becomes: a plugin names a path and whether it takes a worktree, and
/// the base branch is the session's own — a per-member base is reachable
/// headlessly but nothing in the flow asks for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraMember {
    pub path: String,
    pub worktree: bool,
}

/// What to do to a remembered repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkEdit {
    /// Remember it (or touch its recency, which is how the flow re-selects a
    /// path that was already there).
    Add,
    /// Forget it, and a parent's children with it.
    Remove,
    /// Remember it as a folder *of* repositories, replacing its children with a
    /// fresh scan.
    Parent,
}

/// What to do to one file of the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginEdit {
    /// Write the embedded copy back, undoing an edit or a removal alike.
    Restore,
    /// Delete it. A bundled file stays deleted; delivery will not write it
    /// again.
    Remove,
}

impl Command {
    /// Short stable name, as a plugin wrote it and as it is published back.
    pub fn kind(&self) -> &'static str {
        match self {
            Command::Delete { .. } => "delete",
            Command::Restore { .. } => "restore",
            Command::Restart { .. } => "restart",
            Command::Send { .. } => "send",
            Command::Reorder { .. } => "reorder",
            Command::Create { .. } => "create",
            Command::Fork { .. } => "fork",
            Command::Sync { .. } => "sync",
            Command::Copy { .. } => "copy",
            Command::Diff { .. } => "diff",
            Command::Shell { .. } => "shell",
            Command::Program { .. } => "program",
            Command::Editor { .. } => "editor",
            Command::Focus { .. } => "focus",
            Command::OpenLink { .. } => "open",
            Command::Task { .. } => "task",
            Command::DispatchTask { .. } => "dispatch",
            Command::Automation { .. } => "automation",
            Command::Theme { .. } => "theme",
            Command::Plugin { .. } => "plugin",
            Command::Bookmark { .. } => "bookmark",
            Command::Reap { .. } => "reap",
            Command::Configure { .. } => "configure",
            Command::Order { .. } => "order",
            Command::Setting { .. } => "set",
        }
    }

    /// The session this command concerns.
    pub fn session(&self) -> &str {
        match self {
            Command::Delete { session, .. }
            | Command::Restore { session, .. }
            | Command::Restart { session }
            | Command::Send { session, .. }
            | Command::Reorder { session, .. }
            | Command::Fork { session, .. }
            | Command::Sync { session }
            | Command::Copy { session }
            | Command::Diff { session }
            | Command::Editor { session }
            | Command::Reap { session }
            | Command::Shell { session } => session,
            // Names no session yet — that is the point of creating one.
            Command::Create { .. } => "",
            // A dispatch may name one, when it is sending rather than creating.
            Command::DispatchTask { session, .. } => session.as_deref().unwrap_or(""),
            Command::Task { .. } | Command::Automation { .. } => "",
            // Global, not per-session.
            Command::Theme { .. }
            | Command::Plugin { .. }
            | Command::Bookmark { .. }
            | Command::Configure { .. }
            | Command::Setting { .. }
            | Command::OpenLink { .. }
            | Command::Order { .. }
            // A plugin's pane belongs to the plugin, not to a session — which is
            // the whole point of it, and why it must never appear in anything
            // that enumerates sessions.
            | Command::Program { .. }
            | Command::Focus { .. } => "",
        }
    }

    /// Whether the loop applies this itself instead of handing it to a worker.
    ///
    /// These reach in-process state a worker has no access to: the active theme,
    /// the registry, the clipboard, the terminal, a spawned editor.
    ///
    /// One list, consulted rather than restated, because `execute` both refuses
    /// them at its top and marks them `unreachable!` at its tail — and the two had
    /// already drifted. `Editor` was missing from the guard while listed in the
    /// tail, so an `Editor` command that did reach a worker would have panicked it
    /// instead of being refused; it was unreachable only because the loop happens
    /// to `continue` on that variant first.
    pub fn applied_on_ui_thread(&self) -> bool {
        matches!(
            self,
            Command::Theme { .. }
                | Command::Setting { .. }
                | Command::Copy { .. }
                | Command::OpenLink { .. }
                | Command::Shell { .. }
                | Command::Program { .. }
                | Command::Editor { .. }
                | Command::Focus { .. }
                | Command::Plugin { .. }
        )
    }

    /// What this command concerns when it names no session.
    ///
    /// For a creation that is the repository, which is what lets the session
    /// list draw the placeholder inside the group the session will land in
    /// rather than in a limbo of its own.
    pub fn subject(&self) -> Option<String> {
        match self {
            Command::Create { repo, .. } => std::path::Path::new(repo)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .or_else(|| Some(repo.clone())),
            _ => None,
        }
    }

    /// Build from the `(kind, options)` pair a plugin passes.
    ///
    /// Rejected rather than guessed: an unknown kind or a missing field is a
    /// plugin error, reported like any other.
    pub fn parse(kind: &str, args: Args) -> Result<Self, String> {
        let Args {
            session,
            text,
            delta,
            force,
            flag,
            toggle,
            number,
            reset,
            repo,
            branch,
            base,
            agent,
            host,
            status,
            list,
            file,
            action,
            extras,
            owner,
            argv,
        } = args;
        // The interface's own files name no session, and the verb is explicit:
        // removing a plugin is destructive, so it is never the default.
        if kind == "plugin" {
            let Some(file) = file.filter(|f| !f.is_empty()) else {
                return Err("command \"plugin\" needs a file".to_string());
            };
            let edit = match action.as_deref() {
                Some("restore") => PluginEdit::Restore,
                Some("remove") => PluginEdit::Remove,
                _ => {
                    return Err(
                        "command \"plugin\" needs action = \"restore\" or \"remove\"".to_string(),
                    )
                }
            };
            return Ok(Command::Plugin { file, edit });
        }
        // A setting names a `plugin.id`, not a session.
        if kind == "set" {
            let Some(key) = text.clone().filter(|t| !t.is_empty()) else {
                return Err("command \"set\" needs a plugin.setting key".to_string());
            };
            let value = if reset {
                None
            } else if let Some(flag) = flag {
                Some(SettingValue::Bool(flag))
            } else if let Some(number) = number {
                Some(SettingValue::Number(number))
            } else {
                return Err("command \"set\" needs a flag, a number, or reset = true".to_string());
            };
            return Ok(Command::Setting { key, value });
        }
        // Tasks and automations name a numeric id, not a session.
        if kind == "task" {
            let id = number.map(|n| n as i64);
            if id.is_none() && text.is_none() {
                return Err(
                    "command \"task\" needs a title to create, or a number to change".to_string(),
                );
            }
            return Ok(Command::Task {
                id,
                title: text,
                status,
                delete: reset,
            });
        }
        if kind == "dispatch" {
            let Some(task) = number.map(|n| n as i64) else {
                return Err("command \"dispatch\" needs a task number".to_string());
            };
            return Ok(Command::DispatchTask {
                task,
                session: Some(session).filter(|s| !s.is_empty()),
            });
        }
        if kind == "automation" {
            let Some(id) = number.map(|n| n as i64) else {
                return Err("command \"automation\" needs an id".to_string());
            };
            return Ok(Command::Automation {
                id,
                enabled: flag,
                run_now: force,
                delete: reset,
            });
        }
        // Creation names a repository, not a session.
        if kind == "create" {
            let Some(repo) = repo.filter(|r| !r.is_empty()) else {
                return Err("command \"create\" needs a repo".to_string());
            };
            return Ok(Command::Create {
                name: text.unwrap_or_default(),
                repo,
                branch: branch.filter(|b| !b.is_empty()),
                base: base.filter(|b| !b.is_empty()),
                agent: agent.filter(|a| !a.is_empty()),
                host: host.filter(|h| !h.is_empty()),
                extras,
            });
        }
        // Repository memory names a path and a host, not a session. The verb is
        // explicit for the same reason the plugin command's is: forgetting is
        // destructive, so it is never the default.
        if kind == "bookmark" {
            let Some(path) = repo
                .map(|path| path.trim().to_string())
                .filter(|p| !p.is_empty())
            else {
                return Err("command \"bookmark\" needs a repo path".to_string());
            };
            let edit = match action.as_deref() {
                Some("add") => BookmarkEdit::Add,
                Some("remove") => BookmarkEdit::Remove,
                Some("parent") => BookmarkEdit::Parent,
                _ => {
                    return Err(
                        "command \"bookmark\" needs action = \"add\", \"remove\" or \"parent\""
                            .to_string(),
                    )
                }
            };
            return Ok(Command::Bookmark {
                host: host.unwrap_or_default(),
                path,
                edit,
            });
        }
        // Focus names a pane, not a session.
        if kind == "focus" {
            return match text.clone().filter(|t| !t.is_empty()) {
                Some(plugin) => Ok(Command::Focus { plugin, toggle }),
                None => Err("command \"focus\" needs a plugin name".to_string()),
            };
        }
        // A link names a url, not a session.
        if kind == "open" {
            return match text.filter(|t| !t.is_empty()) {
                Some(url) => Ok(Command::OpenLink { url }),
                None => Err("command \"open\" needs a url".to_string()),
            };
        }
        // Theme is global; every other command acts on one session.
        if kind == "theme" {
            return match text {
                Some(name) if !name.is_empty() => Ok(Command::Theme { name }),
                _ => Err("command \"theme\" needs a name".to_string()),
            };
        }
        // An explicit order names every session at once rather than one, so it
        // is resolved before the single-session guard below.
        if kind == "order" {
            return if list.is_empty() {
                Err("command \"order\" needs a list of session ids".to_string())
            } else {
                Ok(Command::Order { list })
            };
        }
        // A plugin's program pane belongs to the plugin, not to a session, so it is
        // resolved before the single-session guard below — the same reason `order`
        // and `plugin` are.
        if kind == "program" {
            let name = text.unwrap_or_default();
            super::terminal::validate_program_name(&name)?;
            // The verb, spelled the way `plugin` and `bookmark` spell theirs.
            let close = matches!(action.as_deref(), Some("close") | Some("stop"));
            let program = repo.unwrap_or_default();
            if !close && program.trim().is_empty() {
                return Err(
                    "command \"program\" needs a program to run (or action = \"close\")"
                        .to_string(),
                );
            }
            return Ok(Command::Program {
                owner,
                name,
                program,
                argv,
                close,
            });
        }
        if session.is_empty() {
            return Err(format!("command {kind:?} needs a session"));
        }
        match kind {
            "delete" => Ok(Command::Delete { session, force }),
            "restore" => Ok(Command::Restore {
                session,
                best_effort: force,
            }),
            "restart" => Ok(Command::Restart { session }),
            "send" => match text {
                Some(text) => Ok(Command::Send { session, text }),
                None => Err("command \"send\" needs text".to_string()),
            },
            "reorder" => match delta {
                Some(delta) if delta != 0 => Ok(Command::Reorder { session, delta }),
                _ => Err("command \"reorder\" needs a non-zero delta".to_string()),
            },
            "fork" => Ok(Command::Fork {
                session,
                name: text.unwrap_or_default(),
            }),
            "sync" => Ok(Command::Sync { session }),
            "copy" => Ok(Command::Copy { session }),
            "diff" => Ok(Command::Diff { session }),
            "shell" => Ok(Command::Shell { session }),
            "editor" => Ok(Command::Editor { session }),
            other => Err(format!(
                "unknown command {other:?} — try create, fork, sync, copy, diff, \
                 delete, restore, restart, send, reorder, theme or set"
            )),
        }
    }
}

/// Options a plugin passed alongside a command name.
///
/// A struct rather than a parameter list because commands want different
/// fields, and threading six positional `Option`s through was already at the
/// point where a caller could transpose two of them silently.
#[derive(Debug, Clone, Default)]
pub struct Args {
    pub session: String,
    pub text: Option<String>,
    pub delta: Option<i64>,
    pub force: bool,
    pub flag: Option<bool>,
    /// `focus`: return to the previous pane when the named one already has focus.
    pub toggle: bool,
    pub number: Option<f64>,
    pub reset: bool,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub agent: Option<String>,
    pub host: Option<String>,
    /// A task status name, for the task command.
    pub status: Option<String>,
    /// An ordered list of session ids, for the order command.
    pub list: Vec<String>,
    /// A path within the interface directory, for the plugin command.
    pub file: Option<String>,
    /// A verb: the plugin and bookmark commands each take one.
    pub action: Option<String>,
    /// Further repositories a create spans, in the order they were chosen.
    pub extras: Vec<ExtraMember>,
    /// The plugin that issued this command, by path.
    ///
    /// **Stamped by the kernel**, never read from the options table — a plugin
    /// that could set this could act as another one. Empty for a command that did
    /// not come from a plugin.
    pub owner: String,
    /// Arguments for a program a plugin asked to run.
    pub argv: Vec<String>,
}

/// How far along a command is.
///
/// `Stage` carries a name from the operation itself — creation reports which
/// part of the pipeline it reached, because "working" is exactly the wrong
/// answer when the question is *why is this slow*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Queued,
    Running,
    Stage(String),
    Failed,
}

impl Phase {
    pub fn as_str(&self) -> &str {
        match self {
            Phase::Queued => "queued",
            Phase::Running => "running",
            Phase::Stage(name) => name,
            Phase::Failed => "failed",
        }
    }
}

/// A command that has been accepted but whose effect is not yet visible.
#[derive(Debug, Clone)]
pub struct InFlight {
    pub id: u64,
    pub kind: &'static str,
    pub session: String,
    /// What the command concerns when it names no session yet — the repository
    /// a creation is for, so a pending row can be grouped where the session
    /// will actually land.
    pub subject: Option<String>,
    pub phase: Phase,
    /// Set once the command has failed; retained briefly so a plugin can show
    /// it, then swept.
    pub error: Option<String>,
}

impl InFlight {
    /// Note that the worker has started, unless this row is already finished.
    ///
    /// A phase only ever moves forward. The worker announces itself from its own
    /// thread, so the announcement can be scheduled *after* something has already
    /// resolved the row — and an unguarded write then puts a failed command back
    /// on the progress line, where it hides the error that explains it. Only a
    /// row still reading `Queued` has anything to learn from "it started".
    fn started(&mut self) {
        if self.phase == Phase::Queued {
            self.phase = Phase::Running;
        }
    }
}

/// A phase change reported from a running command.
struct Progress {
    id: u64,
    phase: String,
}

/// A finished command, reported back to the loop.
struct Done {
    id: u64,
    error: Option<String>,
}

/// How long a failed command stays readable before being swept.
const FAILURE_LINGER_MS: u64 = 8_000;

/// Accepts commands, runs them off the render path, and reports progress.
pub struct CommandBus {
    inflight: Arc<Mutex<Vec<InFlight>>>,
    finished_tx: Sender<Done>,
    finished_rx: Receiver<Done>,
    progress_tx: Sender<Progress>,
    progress_rx: Receiver<Progress>,
    next_id: AtomicU64,
    /// When each failure was recorded, so it can be swept after lingering.
    failed_at: Vec<(u64, std::time::Instant)>,
}

impl CommandBus {
    pub fn new() -> Self {
        let (finished_tx, finished_rx) = channel();
        let (progress_tx, progress_rx) = channel();
        Self {
            inflight: Arc::new(Mutex::new(Vec::new())),
            finished_tx,
            finished_rx,
            progress_tx,
            progress_rx,
            next_id: AtomicU64::new(1),
            failed_at: Vec::new(),
        }
    }

    /// Accept a command. Returns immediately, always.
    pub fn dispatch(&self, command: Command) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let entry = InFlight {
            id,
            kind: command.kind(),
            session: command.session().to_string(),
            subject: command.subject(),
            phase: Phase::Queued,
            error: None,
        };
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.push(entry);
        }

        let inflight = self.inflight.clone();
        let finished = self.finished_tx.clone();
        let progress = self.progress_tx.clone();
        // One thread per command: a slow operation on an unreachable host must
        // not queue behind, or in front of, anything else.
        std::thread::spawn(move || {
            if let Ok(mut list) = inflight.lock() {
                if let Some(entry) = list.iter_mut().find(|entry| entry.id == id) {
                    entry.started();
                }
            }
            let error = execute(&command, id, &progress).err();
            let _ = finished.send(Done { id, error });
        });
        id
    }

    /// Fold finished commands into the in-flight list.
    ///
    /// Returns true when anything completed, so the caller can refresh the
    /// snapshot immediately rather than waiting out the refresh interval —
    /// which is what makes a delete feel instant.
    /// Report a command finished, as a worker would.
    ///
    /// The worker path is a thread and a channel; a test that wants to observe
    /// what `poll` does with a completion needs the completion without the
    /// thread.
    #[doc(hidden)]
    pub fn finish_for_test(&self, id: u64, error: Option<String>) {
        let _ = self.finished_tx.send(Done { id, error });
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;

        // Phase changes first, so a command that reports and then finishes in
        // the same window does not show a stale stage.
        while let Ok(update) = self.progress_rx.try_recv() {
            if let Ok(mut list) = self.inflight.lock() {
                if let Some(entry) = list.iter_mut().find(|entry| entry.id == update.id) {
                    entry.phase = Phase::Stage(update.phase);
                    changed = true;
                }
            }
        }

        while let Ok(done) = self.finished_rx.try_recv() {
            changed = true;
            if let Ok(mut list) = self.inflight.lock() {
                match done.error {
                    // Success: the effect is in the database now, so the row
                    // has nothing left to say.
                    None => list.retain(|entry| entry.id != done.id),
                    Some(error) => {
                        if let Some(entry) = list.iter_mut().find(|entry| entry.id == done.id) {
                            entry.phase = Phase::Failed;
                            entry.error = Some(error);
                        }
                        self.failed_at.push((done.id, std::time::Instant::now()));
                    }
                }
            }
        }

        // Sweep failures that have been readable long enough.
        let expired: Vec<u64> = self
            .failed_at
            .iter()
            .filter(|(_, at)| at.elapsed().as_millis() as u64 >= FAILURE_LINGER_MS)
            .map(|(id, _)| *id)
            .collect();
        if !expired.is_empty() {
            if let Ok(mut list) = self.inflight.lock() {
                list.retain(|entry| !expired.contains(&entry.id));
            }
            self.failed_at.retain(|(id, _)| !expired.contains(id));
            changed = true;
        }
        changed
    }

    /// Everything accepted but not yet done, for publishing to plugins.
    pub fn inflight(&self) -> Vec<InFlight> {
        self.inflight
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default()
    }

    /// The first command that is still *running*, for the progress line.
    ///
    /// A failed entry stays in the list for `FAILURE_LINGER_MS` so a pane can
    /// draw a failed row, and that is longer than a message is retained — so
    /// describing one as progress does not merely mislabel it, it hides the
    /// error explaining the failure for the whole time that error is readable.
    /// The failure is reported through the message band instead.
    pub fn first_running(&self) -> Option<InFlight> {
        self.inflight
            .lock()
            .ok()?
            .iter()
            .find(|entry| entry.phase != Phase::Failed)
            .cloned()
    }

    /// Whether any command is in flight for a session.
    pub fn is_busy(&self, session: &str) -> bool {
        self.inflight
            .lock()
            .map(|list| {
                list.iter()
                    .any(|entry| entry.session == session && entry.phase != Phase::Failed)
            })
            .unwrap_or(false)
    }
}

impl Default for CommandBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Run one command. Called on the command's own thread, never the UI thread.
fn execute(command: &Command, id: u64, progress: &Sender<Progress>) -> Result<(), String> {
    // Handled by the loop before dispatch, because they mutate in-process state
    // the worker cannot reach. Reaching here means a caller bypassed that.
    if command.applied_on_ui_thread() {
        return Err("applied on the UI thread, not dispatched".to_string());
    }

    // Creation names a repository rather than a session, so it runs before the
    // id is parsed — there is nothing to parse yet.
    if let Command::Create {
        name,
        repo,
        branch,
        base,
        agent,
        host,
        extras,
    } = command
    {
        return create(name, repo, branch, base, agent, host, extras, id, progress);
    }

    // Repository memory names a path, not a session, so it too runs before the
    // id parse.
    if let Command::Bookmark { host, path, edit } = command {
        return bookmark(host, path, *edit);
    }

    // The settings file names nothing at all.
    if let Command::Configure { settings } = command {
        return crate::agent::settings_config::save_settings(settings)
            .map_err(|e| format!("write settings.toml: {e}"));
    }

    // Keyed by nothing at all: an explicit order names every session at once.
    if let Command::Order { list } = command {
        let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
        let db = Database::open(&path).map_err(|e| format!("open database: {e}"))?;
        return order(&db, list);
    }

    // Tasks and automations are keyed by number, not by session id.
    if matches!(
        command,
        Command::Task { .. } | Command::DispatchTask { .. } | Command::Automation { .. }
    ) {
        let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
        let db = Database::open(&path).map_err(|e| format!("open database: {e}"))?;
        return match command {
            Command::Task {
                id,
                title,
                status,
                delete,
            } => task(&db, *id, title, status, *delete),
            Command::DispatchTask { task, session } => {
                dispatch_task(&db, *task, session.as_deref())
            }
            Command::Automation {
                id,
                enabled,
                run_now,
                delete,
            } => automation(&db, *id, *enabled, *run_now, *delete),
            _ => unreachable!(),
        };
    }

    let id: SessionId = command
        .session()
        .parse()
        .map_err(|_| format!("not a session id: {}", command.session()))?;

    // Its own connection: sharing the UI thread's would mean locking against
    // the very reads this is supposed to keep instant.
    let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
    let db = Database::open(&path).map_err(|e| format!("open database: {e}"))?;

    match command {
        Command::Delete { force, .. } => {
            crate::session_ops::delete_session_headless(&db, id, *force).map(|_| ())
        }
        // Restoring is the row, its worktrees and its agent — clearing the flag
        // alone gives back a session that can never attach (parity-gap #11).
        Command::Restore { best_effort, .. } => {
            crate::session_ops::restore_session_headless(&db, id, *best_effort).map(|report| {
                if let Some(error) = report.respawn_error {
                    // Restored, but without its agent: worth saying, not worth
                    // undoing — `restart` will try again.
                    tracing::warn!("restored {} but could not launch it: {error}", report.name);
                }
            })
        }
        Command::Restart { .. } => crate::session_ops::restart_session_headless(&db, id),
        Command::Reap { .. } => crate::session_ops::reap_soft_deleted(&db, id).map(|_| ()),
        Command::Send { text, .. } => {
            let session = db
                .get_session_by_id(id)
                .map_err(|e| format!("get session: {e}"))?
                .ok_or_else(|| format!("session not found: {id}"))?;
            crate::agent::tmux::send_prompt_now(&session.name, text)
                .map_err(|e| format!("send: {e}"))
        }
        Command::Reorder { delta, .. } => reorder(&db, id, *delta),
        // Handled above, before the session id is parsed.
        Command::Order { .. } => Ok(()),
        Command::Fork { name, .. } => fork(&db, id, name),

        Command::Sync { .. } => sync(&db, id),
        // Unreachable: guarded above, and kept exhaustive so adding a command
        // is a compile error here rather than a silent no-op.
        // Handled above, before the session id is parsed.
        Command::Create { .. }
        | Command::Bookmark { .. }
        | Command::Configure { .. }
        | Command::Task { .. }
        | Command::DispatchTask { .. }
        | Command::Automation { .. } => unreachable!("handled before the id parse"),
        Command::Theme { .. }
        | Command::Setting { .. }
        | Command::Copy { .. }
        | Command::Diff { .. }
        | Command::OpenLink { .. }
        | Command::Shell { .. }
        | Command::Program { .. }
        | Command::Editor { .. }
        | Command::Focus { .. }
        | Command::Plugin { .. } => unreachable!("applied on the UI thread"),
    }
}

/// Create a session.
///
/// The whole pipeline — repo resolution, worktree checkout, multi-repo
/// workspace, agent launch — already exists as `spawn_session_headless` and is
/// what `thurbox-cli session create` uses. Reusing it unchanged means creation
/// behaves identically whether it came from a plugin or a script, and there is
/// one cleanup path for a failure rather than two.
#[allow(clippy::too_many_arguments)]
fn create(
    name: &str,
    repo: &str,
    branch: &Option<String>,
    base: &Option<String>,
    agent: &Option<String>,
    host: &Option<String>,
    extras: &[ExtraMember],
    id: u64,
    progress: &Sender<Progress>,
) -> Result<(), String> {
    let path = crate::paths::database_file().ok_or("could not resolve the database path")?;
    let db = Database::open(&path).map_err(|e| format!("open database: {e}"))?;

    let repo_path = crate::paths::expand_tilde(repo);
    // Only a local target can be checked here. Statting a *remote* path on this
    // machine is worse than not checking at all: it refuses a perfectly good
    // repository whenever the two filesystems disagree, which for a WSL distro
    // or an ssh host is always. The flow already validated it on the host when
    // the path was remembered, and the spawn reports the truth either way.
    if host.is_none() && !repo_path.is_dir() {
        return Err(format!("not a directory: {}", repo_path.display()));
    }

    // An unnamed session takes the branch, else the repo directory — the same
    // defaults the CLI applies, so a plugin need not invent one.
    let name = if name.is_empty() {
        branch
            .clone()
            .or_else(|| {
                repo_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "session".to_string())
    } else {
        name.to_string()
    };

    let request = crate::session_ops::spawn::SpawnRequest {
        name,
        repo_path,
        worktree_branch: branch.clone(),
        base_branch: base.clone(),
        agent: agent.clone(),
        agent_session_id: None,
        host: host.clone(),
        parent_session_id: None,
        task_id: None,
        // Each extra either takes its own worktree on the shared branch — off
        // its own base, which here is the session's — or is attached as it is.
        // Two or more members is what makes the agent launch in a symlink
        // workspace, and that is `spawn`'s decision, not this one's.
        extra_repos: extras
            .iter()
            .map(|extra| crate::session::automation::ExtraRepo {
                repo_path: crate::paths::expand_tilde(&extra.path),
                worktree: extra.worktree,
                base_branch: None,
            })
            .collect(),
        fork_session_id: None,
        inherit_worktrees: Vec::new(),
    };
    // Report each stage, so a slow creation says *which* part is slow: a
    // stalled fetch and a stalled ssh connect look identical otherwise.
    let progress = progress.clone();
    let report = move |phase: crate::session_ops::spawn::SpawnPhase| {
        let _ = progress.send(Progress {
            id,
            phase: phase.as_str().to_string(),
        });
    };
    crate::session_ops::spawn::spawn_session_headless_with_progress(&db, request, Some(&report))
        .map(|_| ())
}

/// Remember, forget or import a repository path.
///
/// Runs here rather than in the flow because every branch of it touches the
/// world: expanding a `~` on an ssh host is a round trip, so is establishing
/// whether a path is a repository, and scanning a folder is another. The
/// refusal of a missing path is the point — catching a typo now beats failing
/// minutes later inside `git worktree add`.
fn bookmark(host: &str, path: &str, edit: BookmarkEdit) -> Result<(), String> {
    let db_path = crate::paths::database_file().ok_or("could not resolve the database path")?;
    let db = Database::open(&db_path).map_err(|e| format!("open database: {e}"))?;

    // Removal names a path that is already remembered — an absolute one, since
    // the rows the flow offers come from the database — so it needs neither the
    // filesystem nor the host. Done BEFORE the host is resolved, which is what
    // lets a bookmark be forgotten after its host has been taken out of
    // `hosts.toml`.
    if edit == BookmarkEdit::Remove {
        let removed = db
            .delete_repo_bookmark(host, &crate::paths::expand_tilde(path))
            .map_err(|e| format!("forget repo: {e}"))?;
        return if removed {
            Ok(())
        } else {
            Err(format!("not a remembered repository: {path}"))
        };
    }

    // `""` is local; the flow's key is the same string `repo_bookmarks.host`
    // stores, so nothing is translated here.
    let remote = if host.is_empty() {
        None
    } else {
        let (registry, _warnings) = crate::agent::host_config::load_all_with_warnings();
        Some(
            registry
                .resolve(host)
                .cloned()
                .ok_or_else(|| format!("no such host: {host}"))?,
        )
    };

    let expanded = match remote.as_ref() {
        Some(host) => std::path::PathBuf::from(
            crate::git::expand_remote_tilde(host, path).map_err(|e| format!("{e:#}"))?,
        ),
        None => crate::paths::expand_tilde(path),
    };

    if edit == BookmarkEdit::Parent {
        let children = match remote.as_ref() {
            Some(host) => crate::git::scan_child_repos_on(host, &expanded.to_string_lossy())
                .map_err(|e| format!("{e:#}"))?,
            None => {
                if !expanded.is_dir() {
                    return Err(format!("Path not found: {}", expanded.display()));
                }
                crate::git::scan_child_repos(&expanded)
            }
        };
        db.upsert_repo_bookmark_kind(host, &expanded, true)
            .map_err(|e| format!("remember folder: {e}"))?;
        // Replace rather than merge: re-importing is how a folder is refreshed,
        // so a repository that has since been deleted must stop being offered.
        db.replace_parent_children(host, &expanded, &children)
            .map_err(|e| format!("remember folder contents: {e}"))?;
        return if children.is_empty() {
            // Not a failure — the folder is remembered — but the user asked a
            // question and "none" is the answer, so it is reported rather than
            // looking like an import that silently did nothing.
            Err(format!(
                "No repositories found under {}",
                expanded.display()
            ))
        } else {
            Ok(())
        };
    }

    // Add: establish what it is, refuse what is not there, and record the
    // git-ness so the flow knows whether worktree mode is even possible.
    let is_git = match remote.as_ref() {
        Some(host) => match crate::git::classify_path_on(host, &expanded.to_string_lossy())
            .map_err(|e| format!("{e:#}"))?
        {
            crate::git::PathClass::Git => Some(true),
            crate::git::PathClass::Dir => Some(false),
            crate::git::PathClass::Missing => {
                return Err(format!(
                    "Path not found on '{}': {}",
                    host.name,
                    expanded.display()
                ))
            }
        },
        None => {
            if !expanded.is_dir() {
                return Err(format!("Path not found: {}", expanded.display()));
            }
            Some(crate::git::is_git_repo(&expanded))
        }
    };

    // Touches recency as well as adding, which is what lets the flow re-select
    // a path that was already remembered: the row it asked for is the newest
    // one (design.md D2).
    db.upsert_repo_bookmark_checked(host, &expanded, is_git)
        .map_err(|e| format!("remember repo: {e}"))
}

/// Fork a session: a new one on the same repository, recording its parent.
fn fork(db: &Database, id: SessionId, name: &str) -> Result<(), String> {
    let source = db
        .get_session_by_id(id)
        .map_err(|e| format!("get session: {e}"))?
        .ok_or_else(|| format!("session not found: {id}"))?;

    // The parent's *working directory* — its worktree for a worktree session —
    // not the repository root. A cwd-scoped agent (`codex resume --last`,
    // `opencode --continue`) resolves "the last session here" from it, so the
    // repo root would find nothing to continue.
    let repo_path = source
        .cwd
        .clone()
        .or_else(|| {
            source
                .worktrees
                .first()
                .map(|worktree| worktree.worktree_path.clone())
        })
        .ok_or("the source session has no directory to fork in")?;

    let name = if name.is_empty() {
        format!("{}-fork", source.name)
    } else {
        name.to_string()
    };

    let request = crate::session_ops::spawn::SpawnRequest {
        name,
        repo_path,
        // No new worktree: a fork works beside its parent, on the same branch.
        worktree_branch: None,
        base_branch: None,
        agent: Some(source.agent.clone()),
        agent_session_id: None,
        // A fork of a remote session belongs on that session's host, or it would
        // silently become a local session pointed at a path that is not here.
        host: host_name_for(&source.backend_type),
        parent_session_id: Some(source.id),
        task_id: None,
        extra_repos: Vec::new(),
        // What actually makes it a fork: the agent resumes the parent's
        // conversation into a new one (`fork_args`).
        fork_session_id: source.agent_session_id.clone(),
        // Shared, not created — so the fork shows its branch and can be synced.
        inherit_worktrees: source.worktrees.clone(),
    };
    crate::session_ops::spawn::spawn_session_headless(db, request).map(|_| ())
}

/// Bring a session's worktree up to date with the branch it came from.
///
/// The bare host name behind a persisted backend (`ssh:devbox` → `devbox`), or
/// `None` for a local session.
///
/// `SpawnRequest.host` names a host as `hosts.toml` does, while a session row
/// stores the backend it runs on — the registry is what maps between them, and
/// a name it does not know is treated as local rather than guessed at.
fn host_name_for(backend_type: &str) -> Option<String> {
    crate::session_ops::resolve_host(backend_type)
        .flatten()
        .map(|host| host.name)
}

/// Refuses rather than asking the user to be careful: a sync that discards
/// uncommitted work is indistinguishable from a bug at the moment it happens.
fn sync(db: &Database, id: SessionId) -> Result<(), String> {
    let session = db
        .get_session_by_id(id)
        .map_err(|e| format!("get session: {e}"))?
        .ok_or_else(|| format!("session not found: {id}"))?;

    if session.worktrees.is_empty() {
        return Err("this session has no worktree to sync".to_string());
    }

    // A multi-repo session has one worktree per repository, all on the same
    // branch — syncing only the first left the others behind, which is worse than
    // not syncing at all: the session then spans repositories at different bases.
    //
    // On the session's own machine, too. A remote worktree's path does not exist
    // here, so the local `git` would either fail or — with an unlucky path
    // collision — rebase something else entirely.
    let host = crate::session_ops::resolve_host(&session.backend_type).ok_or_else(|| {
        format!(
            "'{}' runs on backend '{}', which is not in hosts.toml — cannot reach \
             the machine its worktrees live on",
            session.name, session.backend_type
        )
    })?;

    // `sync_worktree` stashes, rebases and pops — and on conflict aborts and
    // restores the stash. So uncommitted work is never lost, which is why this
    // does not pre-refuse a dirty worktree the way a naive rebase would have to.
    let base = db
        .get_session_base_branch(id)
        .map_err(|e| format!("read base branch: {e}"))?;

    let mut synced = 0;
    for worktree in &session.worktrees {
        match crate::git::sync_worktree_on(host.as_ref(), &worktree.worktree_path, base.as_deref())
        {
            crate::git::SyncResult::Synced => synced += 1,
            // Not an error in the "something broke" sense: the rebase was undone
            // and the worktree is exactly as it was. Reported so the user knows
            // why nothing moved — and reported for the repository it happened in,
            // since the others may have synced.
            crate::git::SyncResult::Conflict(detail) => {
                return Err(format!(
                "sync stopped in {} — conflicts, nothing changed there ({synced} synced): {detail}",
                worktree.branch
            ))
            }
            crate::git::SyncResult::Error(detail) => {
                return Err(format!(
                    "sync failed in {} ({synced} synced): {detail}",
                    worktree.branch
                ))
            }
        }
    }
    Ok(())
}

/// Create, edit, retitle or delete a task.
fn task(
    db: &Database,
    id: Option<i64>,
    title: &Option<String>,
    status: &Option<String>,
    delete: bool,
) -> Result<(), String> {
    use crate::session::task::TaskStatus;
    use crate::storage::tasks::NewTask;

    let Some(id) = id else {
        let title = title.clone().ok_or("a new task needs a title")?;
        return db
            .create_task(&NewTask::local(title))
            .map(|_| ())
            .map_err(|e| format!("create task: {e}"));
    };

    if delete {
        return db
            .soft_delete_task(id)
            .map(|_| ())
            .map_err(|e| format!("delete task: {e}"));
    }

    if let Some(status) = status {
        // The same three names `thurbox-cli task edit --status` accepts, so a
        // plugin and a script speak one vocabulary.
        let status = match status.as_str() {
            "todo" => TaskStatus::Todo,
            "in_progress" => TaskStatus::InProgress,
            "done" => TaskStatus::Done,
            other => {
                return Err(format!(
                    "not a task status: {other:?} — try todo, in_progress or done"
                ))
            }
        };
        db.set_task_status(id, status)
            .map(|_| ())
            .map_err(|e| format!("set status: {e}"))?;
    }
    if let Some(title) = title {
        let mut existing = db
            .get_task(id)
            .map_err(|e| format!("get task: {e}"))?
            .ok_or_else(|| format!("no task #{id}"))?;
        existing.title = title.clone();
        db.update_task(&existing)
            .map_err(|e| format!("update task: {e}"))?;
    }
    Ok(())
}

/// Hand a task to an agent.
///
/// The prompt is `Task::agent_prompt()` — the same one `thurbox-cli task run`
/// builds — so an agent gets identical context however it was handed the work,
/// and there is one place to change what it is told.
fn dispatch_task(db: &Database, task_id: i64, session: Option<&str>) -> Result<(), String> {
    use crate::session::task::TaskStatus;

    let task = db
        .get_task(task_id)
        .map_err(|e| format!("get task: {e}"))?
        .ok_or_else(|| format!("no task #{task_id}"))?;
    let prompt = task.agent_prompt();

    match session {
        Some(session) => {
            let id: SessionId = session
                .parse()
                .map_err(|_| format!("not a session id: {session}"))?;
            let target = db
                .get_session_by_id(id)
                .map_err(|e| format!("get session: {e}"))?
                .ok_or_else(|| format!("session not found: {id}"))?;
            crate::agent::tmux::send_prompt_now(&target.name, &prompt)
                .map_err(|e| format!("send: {e}"))?;
        }
        None => {
            // Create a session for the task, then hand it the prompt once its
            // pane is live. `spawn_session_headless` returns after the agent is
            // launched, so the delivery is a follow-up rather than part of it.
            let repo = task_repo(db)?;
            let request = crate::session_ops::spawn::SpawnRequest {
                name: format!("task-{task_id}"),
                repo_path: repo,
                worktree_branch: None,
                base_branch: None,
                agent: None,
                agent_session_id: None,
                host: None,
                parent_session_id: None,
                task_id: Some(task_id),
                extra_repos: Vec::new(),
                fork_session_id: None,
                inherit_worktrees: Vec::new(),
            };
            let spawned = crate::session_ops::spawn::spawn_session_headless(db, request)?;
            // The agent needs a moment to be ready for input; sending into a
            // shell that has not drawn its prompt loses the text.
            crate::agent::tmux::send_prompt_after_delay(&spawned.name, &prompt, 3)
                .map_err(|e| format!("send: {e}"))?;
        }
    }

    // Acting on a task moves it out of not-started, whichever way it was
    // dispatched.
    if task.status == TaskStatus::Todo {
        db.set_task_status(task_id, TaskStatus::InProgress)
            .map_err(|e| format!("advance task: {e}"))?;
    }
    Ok(())
}

/// Enable, disable, run or delete an automation.
fn automation(
    db: &Database,
    id: i64,
    enabled: Option<bool>,
    run_now: bool,
    delete: bool,
) -> Result<(), String> {
    if delete {
        return db
            .delete_automation(id)
            .map(|_| ())
            .map_err(|e| format!("delete automation: {e}"));
    }
    if let Some(enabled) = enabled {
        db.set_automation_enabled(id, enabled)
            .map_err(|e| format!("set enabled: {e}"))?;
    }
    if run_now {
        // Marks it due; the scheduler — TUI, heartbeat or cron — runs it, so
        // there is one execution path rather than a second one here.
        db.trigger_automation_now(id)
            .map_err(|e| format!("trigger: {e}"))?;
    }
    Ok(())
}

/// A repository to create a task's session in.
///
/// The most recently used one, because a task with no repo of its own belongs
/// wherever you are working — and asking would mean blocking, which a command
/// cannot do.
fn task_repo(db: &Database) -> Result<std::path::PathBuf, String> {
    db.list_active_sessions()
        .map_err(|e| format!("list sessions: {e}"))?
        .into_iter()
        .find_map(|session| {
            session
                .worktrees
                .first()
                .map(|w| w.repo_path.clone())
                .or(session.cwd)
        })
        .ok_or_else(|| "no repository to create a session in — start one session first".to_string())
}

/// Serializes reordering.
///
/// Reorder is a read-modify-write over *every* row, and commands run on
/// independent threads — so holding a key down would otherwise have two moves
/// read the same order, swap the same pair, and land as one. Ordering is the
/// only command with this shape; the rest touch a single row and need no lock.
static ORDER_LOCK: Mutex<()> = Mutex::new(());

/// Move a session `delta` places in the manual order and renumber densely.
///
/// Renumbering every row (rather than nudging one) is what v1 does, and it is
/// what makes the order stable: a session that has never been moved sorts last
/// by `display_order IS NULL`, and one move gives everything a definite place.
fn reorder(db: &Database, id: SessionId, delta: i64) -> Result<(), String> {
    // Poisoning only means an earlier reorder panicked; the order is still
    // consistent on disk, so recovering is better than refusing every future
    // move.
    let _guard = ORDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut sessions = db
        .list_active_sessions()
        .map_err(|e| format!("list sessions: {e}"))?;

    // The order the list renders in: manual position first, then name — the
    // same comparator ui/plugins/10_sessions.lua uses, or a move would appear
    // to jump.
    sessions.sort_by(|a, b| {
        a.display_order
            .unwrap_or(i64::MAX)
            .cmp(&b.display_order.unwrap_or(i64::MAX))
            .then_with(|| a.name.cmp(&b.name))
    });

    let at = sessions
        .iter()
        .position(|s| s.id == id)
        .ok_or_else(|| format!("session not in the active list: {id}"))?;

    // A move stays inside its repo group, because that is how the list is
    // drawn: swapping past a group edge would reorder the underlying list
    // while the screen appeared not to change at all.
    let repo_of = |s: &crate::sync::SharedSession| {
        super::snapshot::repo_name(&s.cwd, s.worktrees.first().map(|w| &w.repo_path))
    };
    let group = repo_of(&sessions[at]);

    let step = if delta > 0 { 1i64 } else { -1 };
    let mut target = at as i64 + step;
    while target >= 0 && target < sessions.len() as i64 {
        if repo_of(&sessions[target as usize]) == group {
            break;
        }
        target += step;
    }
    if target < 0 || target >= sessions.len() as i64 {
        // Already at its group's edge; not an error, just nothing to do.
        return Ok(());
    }
    sessions.swap(at, target as usize);

    for (position, session) in sessions.iter_mut().enumerate() {
        session.display_order = Some(position as i64);
        db.upsert_session(session)
            .map_err(|e| format!("persist order: {e}"))?;
    }
    Ok(())
}

/// Persist an explicit manual order and renumber densely.
///
/// `list` is the rendered order, as computed by the pane that drew it. Sessions
/// the list did not mention keep their relative order and follow the ones it
/// did — so a permutation of a filtered view cannot silently discard rows it
/// never showed.
fn order(db: &Database, list: &[String]) -> Result<(), String> {
    // Same lock as `reorder`: two concurrent renumberings must not interleave.
    let _guard = ORDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut sessions = db
        .list_active_sessions()
        .map_err(|e| format!("list sessions: {e}"))?;

    let rank: std::collections::HashMap<&str, usize> = list
        .iter()
        .enumerate()
        .map(|(position, id)| (id.as_str(), position))
        .collect();

    // Unmentioned rows sort after every mentioned one, keeping the order they
    // already had among themselves.
    let existing = |session: &crate::sync::SharedSession| session.display_order.unwrap_or(i64::MAX);
    let mut indexed: Vec<(usize, i64, usize)> = sessions
        .iter()
        .enumerate()
        .map(|(index, session)| {
            let key = rank
                .get(session.id.to_string().as_str())
                .copied()
                .unwrap_or(usize::MAX);
            (key, existing(session), index)
        })
        .collect();
    indexed.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    let permutation: Vec<usize> = indexed.into_iter().map(|(_, _, index)| index).collect();
    for (position, index) in permutation.into_iter().enumerate() {
        sessions[index].display_order = Some(position as i64);
    }
    for session in sessions.iter_mut() {
        db.upsert_session(session)
            .map_err(|e| format!("persist order: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_parse_from_what_a_plugin_writes() {
        assert_eq!(
            Command::parse(
                "delete",
                Args {
                    session: "s1".into(),
                    ..Args::default()
                }
            ),
            Ok(Command::Delete {
                session: "s1".into(),
                force: false
            })
        );
        assert_eq!(
            Command::parse(
                "send",
                Args {
                    session: "s1".into(),
                    text: Some("hi".into()),
                    ..Args::default()
                }
            ),
            Ok(Command::Send {
                session: "s1".into(),
                text: "hi".into()
            })
        );
        assert_eq!(
            Command::parse(
                "reorder",
                Args {
                    session: "s1".into(),
                    delta: Some(-1),
                    ..Args::default()
                }
            ),
            Ok(Command::Reorder {
                session: "s1".into(),
                delta: -1
            })
        );
    }

    #[test]
    fn an_unknown_kind_is_refused_with_the_alternatives() {
        let error = Command::parse(
            "explode",
            Args {
                session: "s1".into(),
                ..Args::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("unknown command"), "{error}");
        assert!(error.contains("delete"), "{error}");
    }

    /// `toggle` is what lets one key both enter and leave a pane.
    ///
    /// Off by default, so every existing `command("focus", …)` keeps meaning "go
    /// there" — a plugin that wanted the old behaviour did not have to change.
    #[test]
    fn focus_carries_whether_it_toggles() {
        let plain = Command::parse(
            "focus",
            Args {
                text: Some("review".into()),
                ..Args::default()
            },
        )
        .expect("focus parses");
        assert!(matches!(plain, Command::Focus { toggle: false, .. }));

        let toggling = Command::parse(
            "focus",
            Args {
                text: Some("review".into()),
                toggle: true,
                ..Args::default()
            },
        )
        .expect("focus parses");
        match toggling {
            Command::Focus { plugin, toggle } => {
                assert_eq!(plugin, "review");
                assert!(toggle);
            }
            other => panic!("{other:?}"),
        }
    }

    /// A refresh names its session and is applied on the UI thread, so it must
    /// parse, carry the id, and never be routed to the worker dispatch (which
    /// asserts `unreachable!` for UI-thread commands).
    #[test]
    fn a_diff_refresh_parses_and_names_its_session() {
        let command = Command::parse(
            "diff",
            Args {
                session: "s1".into(),
                ..Args::default()
            },
        )
        .expect("diff parses");
        assert_eq!(command.kind(), "diff");
        assert_eq!(command.session(), "s1");
        assert!(matches!(command, Command::Diff { .. }));
        // And it is refused without one, like every session-scoped command.
        assert!(Command::parse("diff", Args::default()).is_err());
    }

    #[test]
    fn a_command_without_a_session_is_refused() {
        let error = Command::parse("delete", Args::default()).unwrap_err();
        assert!(error.contains("needs a session"), "{error}");
    }

    #[test]
    fn send_needs_text_and_reorder_needs_a_delta() {
        assert!(Command::parse(
            "send",
            Args {
                session: "s1".into(),
                ..Args::default()
            }
        )
        .is_err());
        assert!(Command::parse(
            "reorder",
            Args {
                session: "s1".into(),
                ..Args::default()
            }
        )
        .is_err());
        // A zero delta would be a no-op that still renumbered every row.
        assert!(Command::parse(
            "reorder",
            Args {
                session: "s1".into(),
                delta: Some(0),
                ..Args::default()
            }
        )
        .is_err());
    }

    #[test]
    fn dispatch_returns_immediately_and_reports_the_command_in_flight() {
        // The property that matters: accepting a command does not wait for it.
        let bus = CommandBus::new();
        let started = std::time::Instant::now();
        // A bogus id fails fast in the worker, which is fine — what is asserted
        // here is that *dispatch* did not block on any of it.
        bus.dispatch(Command::Delete {
            session: "not-a-uuid".into(),
            force: false,
        });
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "dispatch must return immediately"
        );
        assert_eq!(bus.inflight().len(), 1);
        assert_eq!(bus.inflight()[0].kind, "delete");
    }

    #[test]
    fn a_failure_surfaces_through_the_inflight_list_not_the_call() {
        let mut bus = CommandBus::new();
        bus.dispatch(Command::Delete {
            session: "not-a-uuid".into(),
            force: false,
        });

        // Poll until the worker reports back.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            bus.poll();
            let inflight = bus.inflight();
            if inflight.iter().any(|entry| entry.phase == Phase::Failed) {
                let failed = inflight
                    .iter()
                    .find(|entry| entry.phase == Phase::Failed)
                    .expect("failed entry");
                assert!(
                    failed.error.as_deref().unwrap_or("").contains("session id"),
                    "{:?}",
                    failed.error
                );
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the failure never surfaced");
    }

    #[test]
    fn a_failed_command_is_not_reported_as_work_in_flight() {
        // The progress line is derived from this, and it outranks the message
        // band. A failure lingers longer than a message is retained, so counting
        // it as progress hid the error for exactly as long as the error existed.
        let mut bus = CommandBus::new();
        let id = bus.dispatch(Command::Delete {
            session: "not-a-uuid".into(),
            force: false,
        });
        bus.finish_for_test(id, Some("bad session id".into()));
        bus.poll();

        assert_eq!(bus.inflight().len(), 1, "the failed row is still drawable");
        assert!(
            bus.first_running().is_none(),
            "a failure must not be described as work in progress"
        );
    }

    #[test]
    fn a_failure_is_skipped_to_find_the_work_still_running_behind_it() {
        // The failure is FIRST in the list, so a `first()` would stop there and
        // report nothing in flight while a creation was genuinely running — the
        // band would go quiet mid-spawn.
        let mut bus = CommandBus::new();
        let failed = bus.dispatch(Command::Delete {
            session: "not-a-uuid".into(),
            force: false,
        });
        bus.finish_for_test(failed, Some("bad session id".into()));
        bus.poll();
        let running = bus.dispatch(Command::Restore {
            session: "s1".into(),
            best_effort: false,
        });

        let found = bus.first_running().expect("the running command");
        assert_eq!(found.id, running);
        assert_ne!(found.phase, Phase::Failed);
    }

    #[test]
    fn a_worker_announcing_itself_cannot_revive_a_finished_row() {
        // The worker sets `Running` from its own thread, so that write races
        // whatever resolved the row first. Losing the race used to overwrite
        // `Failed` — which put the failure back on the progress line, hiding the
        // error that explains it, and made
        // `a_failure_is_skipped_to_find_the_work_still_running_behind_it` fail
        // about one run in twenty.
        let mut entry = InFlight {
            id: 1,
            kind: "delete",
            session: "s1".into(),
            subject: None,
            phase: Phase::Failed,
            error: Some("bad session id".into()),
        };
        entry.started();
        assert_eq!(entry.phase, Phase::Failed, "a phase only moves forward");

        // A queued row is exactly what the announcement is for.
        let mut queued = InFlight {
            phase: Phase::Queued,
            ..entry
        };
        queued.started();
        assert_eq!(queued.phase, Phase::Running);
    }

    #[test]
    fn work_in_flight_is_reported_as_running() {
        // The other half of the contract: an ordinary in-flight command must
        // still drive the progress line.
        let bus = CommandBus::new();
        assert!(bus.first_running().is_none(), "nothing dispatched yet");
        let id = bus.dispatch(Command::Restore {
            session: "s1".into(),
            best_effort: false,
        });
        let found = bus.first_running().expect("the dispatched command");
        assert_eq!(found.id, id);
        assert_eq!(found.kind, "restore");
    }

    #[test]
    fn a_session_with_work_in_flight_reads_as_busy() {
        let bus = CommandBus::new();
        assert!(!bus.is_busy("s1"));
        bus.dispatch(Command::Restore {
            session: "s1".into(),
            best_effort: false,
        });
        assert!(bus.is_busy("s1"));
        assert!(!bus.is_busy("other"));
    }

    #[test]
    fn phases_have_stable_names() {
        // Published to plugins, so these are contract, not debug output.
        assert_eq!(Phase::Queued.as_str(), "queued");
        assert_eq!(Phase::Running.as_str(), "running");
        assert_eq!(Phase::Failed.as_str(), "failed");
    }

    #[test]
    fn syncing_a_session_on_an_unknown_host_refuses_rather_than_syncing_locally() {
        // The paths in a remote session's worktrees do not exist here. Running
        // the local `git` against them either fails or — on a path collision —
        // rebases something else entirely, so an unreachable host is a refusal.
        let db = Database::open_in_memory().expect("db");
        let session = crate::sync::SharedSession {
            id: SessionId::default(),
            name: "demo".into(),
            agent: "claude".into(),
            backend_id: "%1".into(),
            backend_type: "ssh:host-that-is-not-configured".into(),
            agent_session_id: Some("sid".into()),
            cwd: Some(std::path::PathBuf::from("/srv/worktree")),
            additional_dirs: Vec::new(),
            worktrees: vec![crate::sync::SharedWorktree {
                repo_path: std::path::PathBuf::from("/srv/repo"),
                worktree_path: std::path::PathBuf::from("/srv/worktree"),
                branch: "feat/x".into(),
            }],
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&session).expect("upsert");

        let error = super::sync(&db, session.id).unwrap_err();
        assert!(error.contains("hosts.toml"), "{error}");
    }

    #[test]
    fn a_command_that_succeeds_is_gone_from_the_list_by_the_time_poll_returns() {
        // The property that made "watch the in-flight list for completions"
        // wrong, and that cost two attempts at fixing a frozen restart: a
        // successful command is retired INSIDE `poll`, so anything sampling the
        // list afterwards never sees it. Only a failure lingers, for the panes.
        //
        // Anything that needs to know a command finished must therefore have
        // recorded it when it was DISPATCHED.
        let mut bus = CommandBus::new();
        let id = bus.dispatch(Command::Focus {
            plugin: "agent".into(),
            toggle: false,
        });
        assert!(
            bus.inflight().iter().any(|item| item.id == id),
            "it is in the list while it is running"
        );

        // Focus is applied on the UI thread and never reaches a worker, so drive
        // the same retirement the worker path uses.
        bus.finish_for_test(id, None);
        assert!(bus.poll(), "poll reports that something finished");
        assert!(
            !bus.inflight().iter().any(|item| item.id == id),
            "and it is already gone — there is no 'done' left to observe"
        );
    }

    #[test]
    fn a_command_that_fails_lingers_so_a_pane_can_draw_it() {
        let mut bus = CommandBus::new();
        let id = bus.dispatch(Command::Focus {
            plugin: "agent".into(),
            toggle: false,
        });
        bus.finish_for_test(id, Some("no such pane".to_string()));
        assert!(bus.poll());
        let item = bus
            .inflight()
            .into_iter()
            .find(|item| item.id == id)
            .expect("a failure stays visible");
        assert_eq!(item.error.as_deref(), Some("no such pane"));
    }

    // ── a plugin's program is not a session ────────────────────────────────

    /// The command names no session, which is what keeps a program pane out of
    /// everything that enumerates them.
    #[test]
    fn a_program_command_names_no_session() {
        let program = Command::Program {
            owner: "plugins/90_watch.lua".into(),
            name: "watch".into(),
            program: "watch".into(),
            argv: Vec::new(),
            close: false,
        };
        assert_eq!(program.session(), "");
        assert_eq!(program.kind(), "program");
        // Applied by the loop: the pane is wired into the `!Send` world the
        // agent's terminal lives in, exactly as a shell's is.
        assert!(program.applied_on_ui_thread());
    }

    #[test]
    fn a_program_command_needs_a_program_or_a_close() {
        let ask = |program: &str, action: Option<&str>| {
            Command::parse(
                "program",
                Args {
                    owner: "plugins/90_watch.lua".into(),
                    text: Some("watch".into()),
                    repo: (!program.is_empty()).then(|| program.to_string()),
                    action: action.map(str::to_string),
                    ..Args::default()
                },
            )
        };
        assert!(ask("watch", None).is_ok());
        // Closing needs no program — the pane is being given up.
        assert!(ask("", Some("close")).is_ok());
        // Starting nothing is a mistake worth reporting, not an empty command line
        // handed to the multiplexer.
        let error = ask("", None).expect_err("should refuse");
        assert!(error.contains("program"), "{error}");
    }

    #[test]
    fn a_program_command_refuses_a_name_that_would_not_survive_a_window_name() {
        for bad in ["", "watch#2", "a b", "../x"] {
            let refused = Command::parse(
                "program",
                Args {
                    owner: "plugins/90_watch.lua".into(),
                    text: Some(bad.to_string()),
                    repo: Some("watch".into()),
                    ..Args::default()
                },
            );
            assert!(refused.is_err(), "{bad:?} should be refused");
        }
    }
}
