//! The write side: commands a plugin issues, executed off the render path.
//!
//! The other half of "Lua never blocks". A plugin **cannot** wait for
//! anything, so a state-changing operation is not a function that returns a
//! result — it is a command that is accepted immediately and whose effect
//! appears in a later snapshot. That removes a whole class of stall (SQLite
//! contention, a git shell-out, an unreachable SSH host) for plugins nobody
//! has written yet.
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

mod bus;
mod execute;
mod fleet;
pub(crate) use fleet::{dir_url, task_name};

pub use bus::{CommandBus, InFlight, Phase};

/// What runs an accepted command, on a command worker: Fleet
/// ([`fleet::Fleet`], its command line, ADR-41), the one implementation.
trait Executor: Send + Sync {
    fn execute(
        &self,
        command: &Command,
        id: u64,
        progress: &std::sync::mpsc::Sender<bus::Progress>,
    ) -> Result<execute::Outcome, String>;
}

use super::registry::Value as SettingValue;

/// A state change a plugin asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Recheck an external confirmation's target on the command worker.
    Guarded {
        inner: Box<Command>,
        session: String,
        backend_id: Option<String>,
        cwd: Option<std::path::PathBuf>,
        member_dirs: Vec<std::path::PathBuf>,
    },
    /// Soft-delete unless `force`; the interface chooses from the deletion preference.
    Delete { session: String, force: bool },
    /// Paste text into a running session's agent.
    Send { session: String, text: String },
    /// Persist an explicit manual order, densely renumbered.
    ///
    /// The permutation is computed by the pane that *renders* the list, because
    /// only it knows the rendered order — the repo grouping, the parent/child
    /// nesting, and therefore which block a move actually swaps. A kernel-side
    /// move can only swap adjacent rows of a flat list, which is how v1's
    /// whole-group and subtree moves got lost. Ids the database no longer has
    /// are ignored rather than refused, since the list may have shrunk between
    /// the paint and the press.
    Order { list: Vec<String> },
    /// Bring a new session into existence.
    ///
    /// A Fleet Task (ADR-41): the name, the repository its workspace is made
    /// from, the harness and the Fleet host. Fleet makes the workspace and
    /// starts the agent, so the command returns once the Task is applied.
    Create {
        name: String,
        repo: String,
        agent: Option<String>,
        host: Option<String>,
        /// The session's own worktree of `repo` (`git+file`), or `repo` as
        /// it is (`file`). `None`: a worktree for a git repository on this
        /// machine, the directory as it is otherwise.
        worktree: Option<bool>,
        /// The worktree's branch: an existing one, or a new one to make.
        /// `None`: a new branch named after the session.
        branch: Option<String>,
        /// What a new branch starts from. `None`: the repository's default.
        base: Option<String>,
        /// The other repositories the session spans, after `repo` and in the
        /// order chosen. A worktree of each is on the same `branch`, made from
        /// that repository's own default branch.
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
    /// Give a session a new name, and its windows with it.
    ///
    /// The name is judged on the worker, not at parse time: whether it collides
    /// is a database read, and one place judging all of it keeps a refusal in
    /// the same words the CLI uses. It arrives as `command.failed`.
    Rename { session: String, name: String },
    /// Copy a surface's terminal contents to the clipboard.
    ///
    /// `session` is a SURFACE name: a bare id is the agent's pane and
    /// `<id>#shell` its companion shell. A pane showing the shell asks for the
    /// shell — it already spells that name to render it — because the two are
    /// separate panes that may both be on screen.
    ///
    /// Applied on the UI thread: the vt100 screen lives behind a `!Send` VM
    /// neighbour and the clipboard wants the tty, neither of which a worker can
    /// reach.
    Copy { session: String },
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
    Diff { session: String },
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
    Shell { session: String },
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
        /// Type into the program instead of starting it: the bytes go to the
        /// pane's stdin exactly as if they had been typed at it.
        ///
        /// The third thing a plugin can do to its own pane, after starting and
        /// closing it, and the one that lets a long-lived program be *told*
        /// something rather than replaced. Restarting an editor to open a
        /// second file is the case that asked for it: the process is the
        /// expensive part, and `start_program` is idempotent, so without this
        /// the only way to change what it shows was to close it first.
        ///
        /// Sent WITH a program, it means "type at it, or start it if it is not
        /// running" — the coordinator picks, because whether the pane is alive
        /// is not something a plugin can read. A running pane that refuses the
        /// keys (its input channel is full) is reported, never started over.
        /// See `terminal::plan_keys`.
        ///
        /// Bytes, not text: what a program is told is a keystroke sequence, and
        /// a plugin is free to write one its program understands and UTF-8 does
        /// not.
        keys: Option<Vec<u8>>,
    },
    /// Open a session's working directory in the configured editor.
    ///
    /// UI-thread applied, and the reason is the whole difficulty of v1's
    /// `Ctrl+O`: an editor needs a real controlling tty, which is either a tmux
    /// popup or this process's own terminal with the TUI stood down
    /// (`run_pending_editor` in `src/main.rs`). A worker thread has neither.
    Editor { session: String },
    /// Open a link, falling back to copying it where nothing can open it.
    ///
    /// Applied on the UI thread with copy, for the same reason: the fallback
    /// needs the clipboard, which wants the tty.
    OpenLink { url: String },
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
    Theme { name: String },
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
    /// Publish a user event to every plugin subscribed to it.
    ///
    /// UI-thread applied: the event queue is the loop's own, and delivery is the
    /// next thing the loop does. `owner` is **stamped by the kernel** from the
    /// plugin executing, like [`Command::Program`]'s, so a plugin cannot forge
    /// the `source` its subscribers see. `name` already carries the `user.`
    /// prefix — a kernel name was refused at parse time.
    Emit {
        owner: String,
        name: String,
        payload: Vec<(String, super::events::Field)>,
    },
    /// Run a declared action, exactly as its chord or a click on it would.
    ///
    /// UI-thread applied, because the action registry and the kernel's own
    /// modals are the loop's. Without this a pane could reach `help.open` only
    /// by *painting* a node with `role = "action:…"` and waiting for a click —
    /// so a key handler could not open help, settings, themes or the palette
    /// at all, and three panes rebuilt a float shell rather than reuse the
    /// modal furniture the kernel already has.
    ///
    /// `fallback` is the plugin whose code issued it, stamped by the kernel:
    /// an action no binding declares is offered to that pane, which is how a
    /// pane reaches its own undeclared verbs. It never decides a grant. Whose
    /// request the action is comes from the queue entry instead
    /// ([`Command::runs_for`]), which through a chain of actions stays the pane
    /// that first asked, where this field names the handler that forwarded it.
    Action { fallback: String, action: String },
    /// A menu action with an explicit row target.
    ActionTarget {
        fallback: String,
        action: String,
        argument: String,
        value: String,
    },
    /// Say something in the message band.
    ///
    /// UI-thread applied: the band draws from state the loop holds, and its
    /// text expires on a timer the loop owns.
    ///
    /// The band is kernel chrome and stays kernel-drawn — a plugin contributes
    /// a sentence and a severity, exactly as it contributes a pill or a
    /// binding. That is what lets a pane report a refusal it made itself
    /// without spending a row of its own on a message line.
    Message {
        text: String,
        level: super::bands::Level,
    },
}

/// What to do to a remembered repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkEdit {
    /// Remember it (or touch its recency, which is how the flow re-selects a
    /// path that was already there).
    Add,
    /// Forget it, and a parent's children with it.
    Remove,
    /// Remember it as a folder *of* repositories, replacing its children with a
    /// fresh scan.
    Parent,
    /// Make the directory, which must not exist or be empty, then remember it.
    /// The last three are one command each so nothing lands between the
    /// `mkdir` and what goes into it — see [`crate::git::create_repo_dir`].
    Create,
    /// [`Self::Create`], then `git init` it.
    Init,
    /// [`Self::Create`], then `git clone <url>` into it.
    Clone { url: String },
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
            Command::Guarded { inner, .. } => inner.kind(),
            Command::Delete { .. } => "delete",
            Command::Send { .. } => "send",
            Command::Create { .. } => "create",
            Command::Rename { .. } => "rename",
            Command::Copy { .. } => "copy",
            Command::Diff { .. } => "diff",
            Command::Shell { .. } => "shell",
            Command::Program { .. } => "program",
            Command::Editor { .. } => "editor",
            Command::Focus { .. } => "focus",
            Command::OpenLink { .. } => "open",
            Command::Theme { .. } => "theme",
            Command::Plugin { .. } => "plugin",
            Command::Bookmark { .. } => "bookmark",
            Command::Configure { .. } => "configure",
            Command::Order { .. } => "order",
            Command::Setting { .. } => "set",
            Command::Emit { .. } => "emit",
            Command::Action { .. } | Command::ActionTarget { .. } => "action",
            Command::Message { .. } => "message",
        }
    }

    /// The session this command concerns.
    pub fn session(&self) -> &str {
        match self {
            Command::Guarded { session, .. } => session,
            Command::Delete { session, .. }
            | Command::Send { session, .. }
            | Command::Rename { session, .. }
            | Command::Copy { session }
            | Command::Diff { session }
            | Command::Editor { session }
            | Command::Shell { session } => session,
            Command::ActionTarget { argument, value, .. } if argument == "session_id" => value,
            Command::ActionTarget { .. } => "",
            // Names no session yet — that is the point of creating one.
            Command::Create { .. } => "",
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
            | Command::Emit { .. }
            // Chrome, not a row: an action names a verb and a message a
            // sentence.
            | Command::Action { .. }
            | Command::Message { .. }
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
                | Command::Emit { .. }
                | Command::Action { .. }
                | Command::ActionTarget { .. }
                | Command::Message { .. }
        )
    }

    /// The capability the issuing plugin must hold for this to be applied.
    ///
    /// Every variant answers for itself, with no catch-all arm: a new verb does
    /// not compile until someone decides what it needs, so a gap is a choice
    /// written down here rather than a default nobody made.
    ///
    /// `sessions` covers what drives an agent or loses something for good:
    /// `send`, `create`, `delete`, and restoring or removing a file of
    /// the interface. `run` covers what reaches the outside world as the user:
    /// a URL opened without anyone clicking it. `program` covers a plugin's own
    /// program pane.
    pub fn requires(&self) -> Option<super::host::Capability> {
        use super::host::Capability;
        match self {
            Command::Guarded { inner, .. } => inner.requires(),
            Command::OpenLink { .. } => Some(Capability::Run),
            Command::Program { .. } => Some(Capability::Program),
            Command::Send { .. }
            | Command::Create { .. }
            | Command::Plugin { .. }
            // Fleet has no undo window, so every delete is for good.
            | Command::Delete { .. } => Some(Capability::Sessions),
            // Arrange or name what already exists; nothing is typed into an
            // agent and nothing is lost.
            Command::Order { .. } | Command::Rename { .. } | Command::Bookmark { .. } => None,
            // The interface's own state, or something the user then drives
            // themselves: a pane focused, a shell or editor opened beside a
            // session, a theme, a setting, a message, a screen copied.
            Command::Focus { .. }
            | Command::Shell { .. }
            | Command::Editor { .. }
            | Command::Copy { .. }
            | Command::Diff { .. }
            | Command::Theme { .. }
            | Command::Setting { .. }
            | Command::Message { .. }
            | Command::Emit { .. } => None,
            // The kernel's paste types the clipboard into the focused agent, so
            // a pane may not press it for the user. Any other action runs its
            // owner's handler, and what that issues is checked as this pane's
            // request (`LuaHost::acting_for`).
            Command::Action { action, .. } | Command::ActionTarget { action, .. }
                if action == super::clipboard::PASTE_ACTION =>
            {
                Some(Capability::Sessions)
            }
            Command::Action { .. } | Command::ActionTarget { .. } => None,
            // Issued by the kernel, never parsed from a plugin.
            Command::Configure { .. } => None,
        }
    }

    /// The chain an `action` runs for: the one the queue says made it, which
    /// through a chain of actions starts with the pane that first asked
    /// (`LuaHost::acting_for`). `None` for any other command.
    pub fn runs_for(&self, chain: &[String]) -> Option<Vec<String>> {
        matches!(self, Command::Action { .. } | Command::ActionTarget { .. })
            .then(|| chain.to_vec())
    }

    /// Why the plugin at `owner` may not have this applied, when it may not.
    ///
    /// The coordinator's whole decision, kept here so it is tested without a
    /// running interface: `may(owner, capability)` is the host's grant check,
    /// and `owner` is the path the kernel stamped when the command was issued —
    /// empty for Lua that belongs to no plugin, which holds no grant.
    pub fn refusal(
        &self,
        owner: &str,
        may: impl Fn(&str, super::host::Capability) -> bool,
    ) -> Option<String> {
        self.refusal_by(&[owner.to_string()], may)
    }

    /// Why this may not be applied for a request made by `chain`, when it may
    /// not: allowed only with what every plugin in the chain holds, so the
    /// least-privileged one decides, and the refusal names it.
    pub fn refusal_by(
        &self,
        chain: &[String],
        may: impl Fn(&str, super::host::Capability) -> bool,
    ) -> Option<String> {
        let need = self.requires()?;
        let owner = chain
            .iter()
            .find(|member| !may(member, need))
            .map(String::as_str)
            .or_else(|| chain.is_empty().then_some(""))?;
        let who = if owner.is_empty() {
            "Lua outside any plugin"
        } else {
            owner
        };
        let doing = match self.kind() {
            "send" => "type into a session",
            "create" => "create a session",
            "plugin" => "change a file of the interface",
            "open" => "open a link",
            "program" => "run a program in a pane",
            "action" => "paste into a session",
            _ => "delete a session for good",
        };
        Some({
            format!(
                "{who} may not {doing} without the {:?} capability — declare it and trust \
                 the file in settings → Interface",
                need.as_str()
            )
        })
    }

    /// What this command concerns when it names no session.
    ///
    /// For a creation that is the repository, which is what lets the session
    /// list draw the placeholder inside the group the session will land in
    /// rather than in a limbo of its own. For a repository-memory write it is
    /// the path, exactly as issued: writes run independently, and the creation
    /// flow ties a failure to the write it is waiting on by it.
    pub fn subject(&self) -> Option<String> {
        match self {
            Command::Create { repo, .. } => std::path::Path::new(repo)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .or_else(|| Some(repo.clone())),
            Command::Bookmark { path, .. } => Some(path.clone()),
            _ => None,
        }
    }

    /// The machine this command's session will land on, when it names one and
    /// no session exists yet to be asked.
    ///
    /// The counterpart to [`Self::subject`], and published for the same reason:
    /// with the session list grouped by host, the placeholder has to be drawn
    /// under the machine the session is actually being created on. Without it
    /// the list can only guess, and a guess there is a row that says the wrong
    /// machine and then jumps when the real one lands.
    pub fn host(&self) -> Option<String> {
        match self {
            // Bare, because that is how a session's machine is published and
            // the two are compared. A create carries whichever spelling its
            // caller had — the interface's host picker passes the prefixed
            // backend name (`ssh:devbox`), `thurbox-cli --host` the bare one —
            // and `resolve_host` takes either, so only this side needs to
            // settle on one. Left prefixed, the session list drew a creation
            // under a machine named `ssh:devbox` beside the real `devbox`.
            Command::Create { host, .. } => host.as_deref().map(|name| {
                crate::session::host_of_key(name)
                    .unwrap_or(name)
                    .to_string()
            }),
            _ => None,
        }
    }

    /// Build from the `(kind, options)` pair a plugin passes.
    ///
    /// Rejected rather than guessed: an unknown kind or a missing field is a
    /// plugin error, reported like any other.
    pub fn parse(kind: &str, args: Args) -> Result<Self, String> {
        match kind {
            "emit" => Self::parse_emit(args),
            // Chrome a pane contributes to. Neither names a session: one names a
            // verb the registry already knows, the other a sentence.
            "action" => Self::parse_action(args),
            "message" => Self::parse_message(args),
            "plugin" => Self::parse_plugin(args),
            "set" => Self::parse_setting(args),
            // Tasks and automations name a numeric id, not a session.
            // Creation names a repository, not a session.
            "create" => Self::parse_create(args),
            "bookmark" => Self::parse_bookmark(args),
            // Focus names a pane, not a session.
            "focus" => match args.text.filter(|t| !t.is_empty()) {
                Some(plugin) => Ok(Command::Focus {
                    plugin,
                    toggle: args.toggle,
                }),
                None => Err("command \"focus\" needs a plugin name".to_string()),
            },
            // A link names a url, not a session.
            "open" => match args.text.filter(|t| !t.is_empty()) {
                Some(url) => Ok(Command::OpenLink { url }),
                // Names the field, because the field is what goes wrong:
                // `{ url = ... }` is what "needs a url" invites, and it parses
                // to no text and so to no visible effect at all.
                None => Err("command \"open\" needs a url in text".to_string()),
            },
            // Theme is global.
            "theme" => match args.text {
                Some(name) if !name.is_empty() => Ok(Command::Theme { name }),
                _ => Err("command \"theme\" needs a name".to_string()),
            },
            // An explicit order names every session at once rather than one.
            "order" => {
                if args.list.is_empty() {
                    Err("command \"order\" needs a list of session ids".to_string())
                } else {
                    Ok(Command::Order { list: args.list })
                }
            }
            "program" => Self::parse_program(args),
            // Every other command acts on one session.
            _ => Self::parse_session_command(kind, args),
        }
    }

    /// An event names nothing the kernel owns: the name is the subject, and
    /// every other field travels as the payload. Refused for a kernel name so
    /// a plugin cannot forge what only the kernel derives.
    fn parse_emit(args: Args) -> Result<Self, String> {
        let Some(name) = args.text.filter(|t| !t.is_empty()) else {
            return Err("command \"emit\" needs an event name in text".to_string());
        };
        let name = super::events::user_event_name(&name)?;
        Ok(Command::Emit {
            owner: args.owner,
            name,
            payload: args.payload,
        })
    }

    fn parse_action(args: Args) -> Result<Self, String> {
        let Some(action) = args.text.filter(|t| !t.is_empty()) else {
            // Names the field, because the field is what goes wrong:
            // `{ action = … }` is what this verb invites, and an option no
            // verb reads is collected and ignored — so it would enqueue a
            // no-op with nothing to report.
            return Err("command \"action\" needs an action id in text".to_string());
        };
        if !args.session.is_empty() {
            let value = args.session;
            Ok(Command::ActionTarget {
                fallback: args.owner.clone(),
                action,
                argument: "session_id".into(),
                value,
            })
        } else if let Some(value) = args.target {
            Ok(Command::ActionTarget {
                fallback: args.owner.clone(),
                action,
                argument: "target".into(),
                value,
            })
        } else {
            Ok(Command::Action {
                fallback: args.owner,
                action,
            })
        }
    }

    fn parse_message(args: Args) -> Result<Self, String> {
        let Some(text) = args.text.filter(|t| !t.is_empty()) else {
            return Err("command \"message\" needs text".to_string());
        };
        let level = match args.level.as_deref() {
            None => super::bands::Level::Info,
            Some(name) => super::bands::Level::parse(name).ok_or_else(|| {
                format!(
                    "command \"message\" got level {name:?} — try \"info\", \
                     \"success\" or \"error\""
                )
            })?,
        };
        Ok(Command::Message { text, level })
    }

    /// The interface's own files name no session, and the verb is explicit:
    /// removing a plugin is destructive, so it is never the default.
    fn parse_plugin(args: Args) -> Result<Self, String> {
        let Some(file) = args.file.filter(|f| !f.is_empty()) else {
            return Err("command \"plugin\" needs a file".to_string());
        };
        let edit = match args.action.as_deref() {
            Some("restore") => PluginEdit::Restore,
            Some("remove") => PluginEdit::Remove,
            _ => {
                return Err(
                    "command \"plugin\" needs action = \"restore\" or \"remove\"".to_string(),
                )
            }
        };
        Ok(Command::Plugin { file, edit })
    }

    /// A setting names a `plugin.id`, not a session.
    fn parse_setting(args: Args) -> Result<Self, String> {
        let Some(key) = args.text.filter(|t| !t.is_empty()) else {
            return Err("command \"set\" needs a plugin.setting key".to_string());
        };
        let value = if args.reset {
            None
        } else if let Some(flag) = args.flag {
            Some(SettingValue::Bool(flag))
        } else if let Some(number) = args.number {
            Some(SettingValue::Number(number))
        } else if let Some(value) = args.value {
            Some(SettingValue::Text(value))
        } else {
            return Err("command \"set\" needs a flag, number, value, or reset = true".to_string());
        };
        Ok(Command::Setting { key, value })
    }

    fn parse_create(args: Args) -> Result<Self, String> {
        let Some(repo) = args.repo.filter(|r| !r.is_empty()) else {
            return Err("command \"create\" needs a repo".to_string());
        };
        Ok(Command::Create {
            name: args.text.unwrap_or_default(),
            repo,
            agent: args.agent.filter(|a| !a.is_empty()),
            host: args.host.filter(|h| !h.is_empty()),
            worktree: args.worktree,
            branch: args.branch.filter(|b| !b.is_empty()),
            base: args.base.filter(|b| !b.is_empty()),
            extras: args.extras,
        })
    }

    /// Repository memory names a path and a host, not a session. The verb is
    /// explicit for the same reason the plugin command's is: forgetting is
    /// destructive, so it is never the default.
    fn parse_bookmark(args: Args) -> Result<Self, String> {
        let Some(path) = args
            .repo
            .map(|path| path.trim().to_string())
            .filter(|p| !p.is_empty())
        else {
            return Err("command \"bookmark\" needs a repo path".to_string());
        };
        let edit = match args.action.as_deref() {
            Some("add") => BookmarkEdit::Add,
            Some("remove") => BookmarkEdit::Remove,
            Some("parent") => BookmarkEdit::Parent,
            Some("create") => BookmarkEdit::Create,
            Some("init") => BookmarkEdit::Init,
            Some("clone") => match args.text.as_deref().map(str::trim) {
                // A leading `-` is refused rather than left to `git clone --`:
                // the flow's field takes any text, and this says why it failed.
                Some(url) if !url.is_empty() && !url.starts_with('-') => BookmarkEdit::Clone {
                    url: url.to_string(),
                },
                _ => return Err("command \"bookmark\" clone needs a url in text".to_string()),
            },
            _ => {
                return Err("command \"bookmark\" needs action = \"add\", \"remove\", \
                     \"parent\", \"create\", \"init\" or \"clone\""
                    .to_string())
            }
        };
        Ok(Command::Bookmark {
            host: args.host.unwrap_or_default(),
            path,
            edit,
        })
    }

    /// A plugin's program pane belongs to the plugin, not to a session.
    fn parse_program(args: Args) -> Result<Self, String> {
        let name = args.text.unwrap_or_default();
        super::terminal::validate_program_name(&name)?;
        // The verb, spelled the way `plugin` and `bookmark` spell theirs.
        let close = matches!(args.action.as_deref(), Some("close") | Some("stop"));
        let program = args.repo.unwrap_or_default();
        // Typing is checked before the program is required: `keys` alone
        // names a pane that is already running, so asking for `repo` as well
        // would be asking what to start something already started with. Both
        // together are legal and mean "type, or start if it is not there".
        let keys = args.keys.filter(|keys| !keys.is_empty());
        if keys.is_some() && close {
            return Err(
                "command \"program\" cannot type into a pane and close it at once".to_string(),
            );
        }
        if keys.is_none() && !close && program.trim().is_empty() {
            return Err(
                "command \"program\" needs a program to run (or action = \"close\", or keys)"
                    .to_string(),
            );
        }
        Ok(Command::Program {
            owner: args.owner,
            name,
            program,
            argv: args.argv,
            close,
            keys,
        })
    }

    /// The commands that act on one session, which each must name.
    fn parse_session_command(kind: &str, args: Args) -> Result<Self, String> {
        let Args {
            session,
            text,
            force,
            ..
        } = args;
        if session.is_empty() {
            return Err(format!("command {kind:?} needs a session"));
        }
        match kind {
            "delete" => Ok(Command::Delete { session, force }),
            "send" => match text {
                Some(text) => Ok(Command::Send { session, text }),
                None => Err("command \"send\" needs text".to_string()),
            },

            // An empty name is still a rename: the worker refuses it with the
            // reason the CLI gives, which a parse error here could not match.
            "rename" => Ok(Command::Rename {
                session,
                name: text.unwrap_or_default(),
            }),
            "copy" => Ok(Command::Copy { session }),
            "diff" => Ok(Command::Diff { session }),
            "shell" => Ok(Command::Shell { session }),
            "editor" => Ok(Command::Editor { session }),
            other => Err(format!(
                "unknown command {other:?} — try create, rename, copy, diff, delete, \
                 send, order, theme, set, emit, action or message"
            )),
        }
    }
}

/// A repository a session spans besides its first, and whether it gets a
/// worktree of its own (`git+file`) or is used as it is (`file`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraMember {
    pub path: String,
    pub worktree: bool,
}

/// Options a plugin passed alongside a command name.
///
/// A struct rather than a parameter list because commands want different
/// fields, and threading six positional `Option`s through was already at the
/// point where a caller could transpose two of them silently.
#[derive(Debug, Clone, Default)]
pub struct Args {
    pub session: String,
    pub target: Option<String>,
    pub text: Option<String>,
    pub value: Option<String>,
    pub force: bool,
    pub flag: Option<bool>,
    /// `focus`: return to the previous pane when the named one already has focus.
    pub toggle: bool,
    pub number: Option<f64>,
    pub reset: bool,
    pub repo: Option<String>,
    pub agent: Option<String>,
    pub host: Option<String>,
    /// `create`: a worktree of the repository, or the directory as it is.
    pub worktree: Option<bool>,
    /// `create`: the worktree's branch.
    pub branch: Option<String>,
    /// `create`: what a new branch starts from.
    pub base: Option<String>,
    /// `create`: the other repositories the session spans.
    pub extras: Vec<ExtraMember>,
    /// An ordered list of session ids, for the order command.
    pub list: Vec<String>,
    /// A path within the interface directory, for the plugin command.
    pub file: Option<String>,
    /// A verb: the plugin and bookmark commands each take one.
    pub action: Option<String>,
    /// The plugin that issued this command, by path.
    ///
    /// **Stamped by the kernel**, never read from the options table — a plugin
    /// that could set this could act as another one. Empty for a command that did
    /// not come from a plugin.
    pub owner: String,
    /// Arguments for a program a plugin asked to run.
    pub argv: Vec<String>,
    /// Bytes to type into a program a plugin already started.
    ///
    /// Read off the Lua table as bytes, so a sequence that is not UTF-8 arrives
    /// rather than vanishing — see `install_command`.
    pub keys: Option<Vec<u8>>,
    /// Every other scalar field of the options table, for a command that
    /// forwards them whole — an event's payload.
    pub payload: Vec<(String, super::events::Field)>,
    /// How severe a message is: `info`, `success` or `error`.
    pub level: Option<String>,
}

#[cfg(test)]
mod tests {
    /// The host picker passes the backend name, `thurbox-cli --host` the bare
    /// one, and `resolve_host` takes either — so the only place the two have to
    /// become one spelling is here, where the interface reads it back and
    /// compares it against a session's own (bare) machine.
    #[test]
    fn a_creations_host_is_the_bare_machine_whichever_spelling_it_arrived_in() {
        let asked = |host: &str| {
            Command::parse(
                "create",
                Args {
                    repo: Some("/srv/repo".into()),
                    host: Some(host.into()),
                    ..Default::default()
                },
            )
            .expect("parse")
            .host()
        };
        assert_eq!(asked("ssh:devbox").as_deref(), Some("devbox"));
        assert_eq!(asked("wsl:Ubuntu").as_deref(), Some("Ubuntu"));
        // Already bare, and left alone.
        assert_eq!(asked("devbox").as_deref(), Some("devbox"));
        // Nothing to name: this machine.
        assert_eq!(
            Command::parse(
                "create",
                Args {
                    repo: Some("/srv/repo".into()),
                    ..Default::default()
                }
            )
            .expect("parse")
            .host(),
            None
        );
    }

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
        // No name at all still parses: the worker's refusal carries the reason.
        for (text, name) in [(Some("renamed"), "renamed"), (None, "")] {
            let parsed = Command::parse(
                "rename",
                Args {
                    session: "s1".into(),
                    text: text.map(str::to_string),
                    ..Args::default()
                },
            );
            assert_eq!(
                parsed,
                Ok(Command::Rename {
                    session: "s1".into(),
                    name: name.into()
                })
            );
            assert_eq!(parsed.as_ref().map(Command::kind), Ok("rename"));
            assert_eq!(parsed.as_ref().map(Command::session), Ok("s1"));
        }
    }

    /// Every kind against the fields it reads and the refusal each missing one
    /// earns, compared whole: a plugin sees the message verbatim, so its wording
    /// is part of what `parse` promises.
    #[test]
    fn each_kind_reads_its_own_fields_and_names_what_is_missing() {
        fn text(value: &str) -> Option<String> {
            Some(value.to_string())
        }
        let session = |s: &str| Args {
            session: s.into(),
            ..Args::default()
        };
        let err = |message: &str| Err(message.to_string());
        let cases: Vec<(&str, Args, Result<Command, String>)> = vec![
            (
                "emit",
                Args {
                    text: text("deploy"),
                    owner: "plugins/x.lua".into(),
                    ..Args::default()
                },
                Ok(Command::Emit {
                    owner: "plugins/x.lua".into(),
                    name: "user.deploy".into(),
                    payload: Vec::new(),
                }),
            ),
            (
                "emit",
                Args {
                    text: text(""),
                    ..Args::default()
                },
                err("command \"emit\" needs an event name in text"),
            ),
            (
                "emit",
                Args {
                    text: text("user.deploy"),
                    ..Args::default()
                },
                err("command \"emit\": name \"user.deploy\" without the \"user.\" prefix — it is added for you"),
            ),
            (
                "plugin",
                Args {
                    file: text("plugins/a.lua"),
                    action: text("restore"),
                    ..Args::default()
                },
                Ok(Command::Plugin {
                    file: "plugins/a.lua".into(),
                    edit: PluginEdit::Restore,
                }),
            ),
            (
                "plugin",
                Args {
                    file: text("plugins/a.lua"),
                    action: text("remove"),
                    ..Args::default()
                },
                Ok(Command::Plugin {
                    file: "plugins/a.lua".into(),
                    edit: PluginEdit::Remove,
                }),
            ),
            (
                "plugin",
                Args {
                    file: text(""),
                    action: text("remove"),
                    ..Args::default()
                },
                err("command \"plugin\" needs a file"),
            ),
            (
                "plugin",
                Args {
                    file: text("plugins/a.lua"),
                    ..Args::default()
                },
                err("command \"plugin\" needs action = \"restore\" or \"remove\""),
            ),
            (
                "set",
                Args {
                    text: text("p.k"),
                    reset: true,
                    flag: Some(true),
                    ..Args::default()
                },
                Ok(Command::Setting {
                    key: "p.k".into(),
                    value: None,
                }),
            ),
            (
                "set",
                Args {
                    text: text("p.k"),
                    flag: Some(false),
                    number: Some(2.0),
                    ..Args::default()
                },
                Ok(Command::Setting {
                    key: "p.k".into(),
                    value: Some(SettingValue::Bool(false)),
                }),
            ),
            (
                "set",
                Args {
                    text: text("p.k"),
                    number: Some(2.5),
                    ..Args::default()
                },
                Ok(Command::Setting {
                    key: "p.k".into(),
                    value: Some(SettingValue::Number(2.5)),
                }),
            ),
            (
                "set",
                Args {
                    text: text("p.k"),
                    value: text("one"),
                    ..Args::default()
                },
                Ok(Command::Setting {
                    key: "p.k".into(),
                    value: Some(SettingValue::Text("one".into())),
                }),
            ),
            (
                "set",
                Args {
                    text: text("p.k"),
                    ..Args::default()
                },
                err("command \"set\" needs a flag, number, value, or reset = true"),
            ),
            (
                "set",
                Args {
                    flag: Some(true),
                    ..Args::default()
                },
                err("command \"set\" needs a plugin.setting key"),
            ),
            (
                "create",
                Args {
                    repo: text("/srv/repo"),
                    text: text("named"),
                    agent: text(""),
                    host: text("box"),
                    ..Args::default()
                },
                Ok(Command::Create {
                    name: "named".into(),
                    repo: "/srv/repo".into(),
                    agent: None,
                    host: Some("box".into()),
                    worktree: None,
                    branch: None,
                    base: None,
                    extras: Vec::new(),
                }),
            ),
            (
                "create",
                Args {
                    repo: text(""),
                    ..Args::default()
                },
                err("command \"create\" needs a repo"),
            ),
            (
                "bookmark",
                Args {
                    repo: text("  /srv/repo  "),
                    action: text("add"),
                    ..Args::default()
                },
                Ok(Command::Bookmark {
                    host: String::new(),
                    path: "/srv/repo".into(),
                    edit: BookmarkEdit::Add,
                }),
            ),
            (
                "bookmark",
                Args {
                    repo: text("/srv/repo"),
                    host: text("box"),
                    action: text("remove"),
                    ..Args::default()
                },
                Ok(Command::Bookmark {
                    host: "box".into(),
                    path: "/srv/repo".into(),
                    edit: BookmarkEdit::Remove,
                }),
            ),
            (
                "bookmark",
                Args {
                    repo: text("/srv/repo"),
                    action: text("parent"),
                    ..Args::default()
                },
                Ok(Command::Bookmark {
                    host: String::new(),
                    path: "/srv/repo".into(),
                    edit: BookmarkEdit::Parent,
                }),
            ),
            (
                "bookmark",
                Args {
                    repo: text("   "),
                    action: text("add"),
                    ..Args::default()
                },
                err("command \"bookmark\" needs a repo path"),
            ),
            (
                "bookmark",
                Args {
                    repo: text("/srv/repo"),
                    action: text("forget"),
                    ..Args::default()
                },
                err(
                    "command \"bookmark\" needs action = \"add\", \"remove\", \"parent\", \
                     \"create\", \"init\" or \"clone\"",
                ),
            ),
            (
                "focus",
                Args {
                    text: text(""),
                    ..Args::default()
                },
                err("command \"focus\" needs a plugin name"),
            ),
            (
                "open",
                Args {
                    text: text("https://example.com"),
                    ..Args::default()
                },
                Ok(Command::OpenLink {
                    url: "https://example.com".into(),
                }),
            ),
            (
                "open",
                Args::default(),
                err("command \"open\" needs a url in text"),
            ),
            (
                "theme",
                Args {
                    text: text("nord"),
                    ..Args::default()
                },
                Ok(Command::Theme {
                    name: "nord".into(),
                }),
            ),
            (
                "theme",
                Args {
                    text: text(""),
                    ..Args::default()
                },
                err("command \"theme\" needs a name"),
            ),
            (
                "theme",
                Args::default(),
                err("command \"theme\" needs a name"),
            ),
            (
                "order",
                Args {
                    list: vec!["a".into(), "b".into()],
                    ..Args::default()
                },
                Ok(Command::Order {
                    list: vec!["a".into(), "b".into()],
                }),
            ),
            (
                "order",
                Args::default(),
                err("command \"order\" needs a list of session ids"),
            ),
            (
                "program",
                Args {
                    text: text("watch"),
                    repo: text("cargo"),
                    argv: vec!["test".into()],
                    keys: Some(Vec::new()),
                    owner: "plugins/x.lua".into(),
                    ..Args::default()
                },
                Ok(Command::Program {
                    owner: "plugins/x.lua".into(),
                    name: "watch".into(),
                    program: "cargo".into(),
                    argv: vec!["test".into()],
                    close: false,
                    keys: None,
                }),
            ),
            (
                "program",
                Args {
                    text: text("watch"),
                    action: text("stop"),
                    ..Args::default()
                },
                Ok(Command::Program {
                    owner: String::new(),
                    name: "watch".into(),
                    program: String::new(),
                    argv: Vec::new(),
                    close: true,
                    keys: None,
                }),
            ),
            (
                "program",
                Args {
                    text: text("watch"),
                    action: text("close"),
                    keys: Some(b"q".to_vec()),
                    ..Args::default()
                },
                err("command \"program\" cannot type into a pane and close it at once"),
            ),
            (
                "program",
                Args {
                    text: text("watch"),
                    repo: text("  "),
                    ..Args::default()
                },
                err("command \"program\" needs a program to run (or action = \"close\", or keys)"),
            ),
            (
                "program",
                Args::default(),
                err("a program pane needs a name"),
            ),
            (
                "copy",
                session("s1"),
                Ok(Command::Copy {
                    session: "s1".into(),
                }),
            ),
            (
                "shell",
                session("s1"),
                Ok(Command::Shell {
                    session: "s1".into(),
                }),
            ),
            (
                "editor",
                session("s1"),
                Ok(Command::Editor {
                    session: "s1".into(),
                }),
            ),
            (
                "delete",
                Args {
                    force: true,
                    ..session("s1")
                },
                Ok(Command::Delete {
                    session: "s1".into(),
                    force: true,
                }),
            ),
            ("send", session("s1"), err("command \"send\" needs text")),
        ];
        for (kind, args, expected) in cases {
            assert_eq!(
                Command::parse(kind, args.clone()),
                expected,
                "{kind} {args:?}"
            );
        }
    }

    /// Every verb a plugin can issue, and what it needs. `requires` has no
    /// catch-all arm, so a new variant does not compile until it decides; this
    /// pins each decision, so changing one is a visible edit here too.
    #[test]
    fn every_verb_states_what_it_requires() {
        use super::super::host::Capability::{Program, Run, Sessions};
        let with = |args: Args| Args {
            session: "s1".into(),
            owner: "plugins/30_x.lua".into(),
            ..args
        };
        let verbs: Vec<(&str, Args, Option<super::super::host::Capability>)> = vec![
            (
                "send",
                with(Args {
                    text: Some("x".into()),
                    ..Args::default()
                }),
                Some(Sessions),
            ),
            (
                "create",
                with(Args {
                    repo: Some("/r".into()),
                    text: Some("n".into()),
                    ..Args::default()
                }),
                Some(Sessions),
            ),
            (
                "delete",
                with(Args {
                    force: true,
                    ..Args::default()
                }),
                Some(Sessions),
            ),
            ("delete", with(Args::default()), Some(Sessions)),
            (
                "plugin",
                with(Args {
                    file: Some("plugins/x.lua".into()),
                    action: Some("remove".into()),
                    ..Args::default()
                }),
                Some(Sessions),
            ),
            (
                "open",
                with(Args {
                    text: Some("https://x".into()),
                    ..Args::default()
                }),
                Some(Run),
            ),
            (
                "program",
                with(Args {
                    text: Some("p".into()),
                    repo: Some("top".into()),
                    ..Args::default()
                }),
                Some(Program),
            ),
            (
                "order",
                with(Args {
                    list: vec!["s1".into()],
                    ..Args::default()
                }),
                None,
            ),
            (
                "rename",
                with(Args {
                    text: Some("n".into()),
                    ..Args::default()
                }),
                None,
            ),
            (
                "focus",
                with(Args {
                    text: Some("p".into()),
                    ..Args::default()
                }),
                None,
            ),
            ("shell", with(Args::default()), None),
            ("editor", with(Args::default()), None),
            ("copy", with(Args::default()), None),
            ("diff", with(Args::default()), None),
            (
                "theme",
                with(Args {
                    text: Some("t".into()),
                    ..Args::default()
                }),
                None,
            ),
            (
                "set",
                with(Args {
                    text: Some("p.k".into()),
                    flag: Some(true),
                    ..Args::default()
                }),
                None,
            ),
            (
                "message",
                with(Args {
                    text: Some("m".into()),
                    ..Args::default()
                }),
                None,
            ),
            (
                "emit",
                with(Args {
                    text: Some("e".into()),
                    ..Args::default()
                }),
                None,
            ),
            (
                "action",
                with(Args {
                    text: Some("help.open".into()),
                    ..Args::default()
                }),
                None,
            ),
            (
                "action",
                with(Args {
                    text: Some(super::super::clipboard::PASTE_ACTION.into()),
                    ..Args::default()
                }),
                Some(Sessions),
            ),
        ];
        for (verb, args, expected) in verbs {
            let command = Command::parse(verb, args).unwrap_or_else(|e| panic!("{verb}: {e}"));
            assert_eq!(command.requires(), expected, "{verb}: {command:?}");
        }
    }

    /// A pane reaching the action registry from a key handler, which is the
    /// only way it can open help, settings, themes or the palette without
    /// painting a node and waiting for a click.
    #[test]
    fn an_action_command_carries_the_action_and_the_plugin_that_asked() {
        assert_eq!(
            Command::parse(
                "action",
                Args {
                    text: Some("help.open".into()),
                    owner: "plugins/10_sessions.lua".into(),
                    ..Args::default()
                }
            ),
            Ok(Command::Action {
                fallback: "plugins/10_sessions.lua".into(),
                action: "help.open".into(),
            })
        );
        // Names the field, because the field is what goes wrong: `{ action = … }`
        // is what the verb invites, and it parses to no text at all.
        let error = Command::parse("action", Args::default()).unwrap_err();
        assert!(error.contains("text"), "{error}");
    }

    /// The message band is kernel chrome, so a plugin contributes to it rather
    /// than drawing it — and it names a severity the band already badges.
    #[test]
    fn a_message_names_a_level_the_band_can_badge() {
        assert_eq!(
            Command::parse(
                "message",
                Args {
                    text: Some("nothing to undo".into()),
                    ..Args::default()
                }
            ),
            Ok(Command::Message {
                text: "nothing to undo".into(),
                level: crate::kernel::bands::Level::Info,
            })
        );
        assert_eq!(
            Command::parse(
                "message",
                Args {
                    text: Some("gone".into()),
                    level: Some("error".into()),
                    ..Args::default()
                }
            ),
            Ok(Command::Message {
                text: "gone".into(),
                level: crate::kernel::bands::Level::Error,
            })
        );
        // Refused rather than quietly read as info: a level nobody badges is a
        // typo, and a typo that renders as a normal message is invisible.
        let error = Command::parse(
            "message",
            Args {
                text: Some("gone".into()),
                level: Some("critical".into()),
                ..Args::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("critical"), "{error}");
        assert!(error.contains("success"), "{error}");
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
    fn send_needs_text_and_a_removed_verb_is_refused() {
        assert!(Command::parse(
            "send",
            Args {
                session: "s1".into(),
                ..Args::default()
            }
        )
        .is_err());
        // A verb no engine runs any more is no command at all.
        for gone in [
            "reorder",
            "restore",
            "restart",
            "fork",
            "sync",
            "task",
            "automation",
        ] {
            assert!(Command::parse(
                gone,
                Args {
                    session: "s1".into(),
                    number: Some(1.0),
                    ..Args::default()
                }
            )
            .is_err());
        }
    }

    #[test]
    fn dispatch_returns_immediately_and_reports_the_command_in_flight() {
        // The property that matters: accepting a command does not wait for it.
        let bus = CommandBus::fleet("thurbox-test-no-fleet".into());
        let started = std::time::Instant::now();
        // No fleet to run fails fast in the worker, which is fine — what is asserted
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
        let mut bus = CommandBus::fleet("thurbox-test-no-fleet".into());
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
                    failed.error.as_deref().unwrap_or("").contains("fleet"),
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
        let mut bus = CommandBus::fleet("thurbox-test-no-fleet".into());
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
        let mut bus = CommandBus::fleet("thurbox-test-no-fleet".into());
        let failed = bus.dispatch(Command::Delete {
            session: "not-a-uuid".into(),
            force: false,
        });
        bus.finish_for_test(failed, Some("bad session id".into()));
        bus.poll();
        let running = bus.dispatch(Command::Rename {
            session: "s1".into(),
            name: "renamed".into(),
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
            host: None,
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
        let bus = CommandBus::fleet("thurbox-test-no-fleet".into());
        assert!(bus.first_running().is_none(), "nothing dispatched yet");
        let id = bus.dispatch(Command::Rename {
            session: "s1".into(),
            name: "renamed".into(),
        });
        let found = bus.first_running().expect("the dispatched command");
        assert_eq!(found.id, id);
        assert_eq!(found.kind, "rename");
    }

    #[test]
    fn a_session_with_work_in_flight_reads_as_busy() {
        let bus = CommandBus::fleet("thurbox-test-no-fleet".into());
        assert!(!bus.is_busy("s1"));
        bus.dispatch(Command::Rename {
            session: "s1".into(),
            name: "renamed".into(),
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
    fn a_command_that_succeeds_is_gone_from_the_list_by_the_time_poll_returns() {
        // The property that made "watch the in-flight list for completions"
        // wrong, and that cost two attempts at fixing a frozen restart: a
        // successful command is retired INSIDE `poll`, so anything sampling the
        // list afterwards never sees it. Only a failure lingers, for the panes.
        //
        // Anything that needs to know a command finished must therefore have
        // recorded it when it was DISPATCHED.
        let mut bus = CommandBus::fleet("thurbox-test-no-fleet".into());
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
        let mut bus = CommandBus::fleet("thurbox-test-no-fleet".into());
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
            keys: None,
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

    /// Typing names a pane that is already running, so it asks for no program —
    /// and it is not a way to close one.
    #[test]
    fn a_program_command_can_type_into_a_pane_it_already_started() {
        let ask = |keys: Option<&[u8]>, program: &str, action: Option<&str>| {
            Command::parse(
                "program",
                Args {
                    owner: "plugins/50_editor.lua".into(),
                    text: Some("editor".into()),
                    repo: (!program.is_empty()).then(|| program.to_string()),
                    keys: keys.map(<[u8]>::to_vec),
                    action: action.map(str::to_string),
                    ..Args::default()
                },
            )
        };

        let typed = ask(Some(b":e /tmp/x\r"), "", None).expect("keys need no program");
        match typed {
            Command::Program {
                keys, close, name, ..
            } => {
                assert_eq!(keys.as_deref(), Some(b":e /tmp/x\r".as_slice()));
                assert!(!close);
                assert_eq!(name, "editor");
            }
            other => panic!("expected a program command, got {other:?}"),
        }

        // Empty keys are nothing to say, not a request to start something with no
        // program — so they fall through to the ordinary refusal.
        let error = ask(Some(b""), "", None).expect_err("should refuse");
        assert!(error.contains("program"), "{error}");

        // Two different things to do to one pane in one call.
        let error = ask(Some(b"q"), "", Some("close")).expect_err("should refuse");
        assert!(error.contains("close"), "{error}");

        // Keys AND a program is the fallback form: both survive parsing, and
        // `apply_program` is what chooses between them from the pane's liveness.
        let both = ask(Some(b":e /tmp/x\r"), "nvim", None).expect("keys may carry a fallback");
        match both {
            Command::Program {
                keys,
                program,
                close,
                ..
            } => {
                assert_eq!(keys.as_deref(), Some(b":e /tmp/x\r".as_slice()));
                assert_eq!(program, "nvim");
                assert!(!close);
            }
            other => panic!("expected a program command, got {other:?}"),
        }
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
