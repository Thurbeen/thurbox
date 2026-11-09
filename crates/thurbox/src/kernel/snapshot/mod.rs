//! The read side: an in-memory picture of the session engine that plugins
//! read from, refreshed on the kernel's own schedule.
//!
//! One rule: **Lua never blocks and never awaits.** A plugin
//! renders on the UI thread ~20×/s, so a read that could touch SQLite, git, a
//! subprocess or an unreachable SSH host would stall the loop — and would do
//! so for plugins nobody has written yet. Serving every read from a snapshot
//! removes the whole class.
//!
//! The cost is staleness, and the spec requires that be *visible* rather than
//! denied: every snapshot carries the instant it was taken, so a plugin can
//! render freshness instead of misrepresenting it.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod db;
pub mod fleet;
mod git;
mod probe;

use crate::session::settings::Settings;
use crate::session::{
    derive_state, with_output_quiescence, AgentRegistry, Assessment, Corroboration, SessionId,
    SessionState,
};
use crate::storage::{Database, HookRow};
use db::{DbWorker, Job, Reply, RowsRead, Write};
use git::{git_poll_interval, GitStats};
use probe::PaneProbe;

/// How often the snapshot is rebuilt. A read never waits on this; it only
/// determines how out of date the answer may be.
pub const REFRESH_INTERVAL: Duration = Duration::from_millis(400);

/// How long a refresh asked of a [`SnapshotSource`] waits for its answer.
/// A source speaks only when something moved, so after a command that moved
/// nothing no answer comes: past this, the rows stand as fresh. `fleet
/// watch` reads the store every 500 ms, so a write is seen well within it.
const SOURCE_SETTLE: Duration = Duration::from_secs(1);

/// How long "is the multiplexer installed, does each agent's command resolve"
/// is trusted before it is asked again.
///
/// Asking is a `stat` per absolute `PATH` entry per binary and no process
/// spawn, so the answer is cheap — but it is not free, and doing it on the
/// render path (a `which` per frame, per keystroke or per list row) is the
/// regression this window exists to prevent. Ten seconds is short enough that a
/// user who reads "install tmux", installs it, and comes back sees the flow
/// agree without restarting thurbox, and long enough that an idle instance
/// costs a few `stat`s a minute.
const PREFLIGHT_TTL: Duration = Duration::from_secs(10);

/// A session's git working tree, when it has been computed.
///
/// `None` on a row means *not computed yet*, which is deliberately distinct
/// from a clean tree — a stat that has not run must not read as "no changes".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitState {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub untracked: usize,
    pub dirty: bool,
    pub ahead: usize,
    pub behind: usize,
    /// Whether origin's default branch already holds this branch's work — see
    /// [`crate::git::merged_into_default`]. `None` is "not known", which a
    /// delete must not read as "safe to throw away".
    pub merged: Option<bool>,
}

/// One session, flattened to what a plugin needs to draw it.
///
/// Deliberately a plain owned struct rather than a borrow of engine state:
/// plugins read it on the UI thread while the engine mutates elsewhere.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub name: String,
    pub agent: String,
    /// What to draw, derived from the hook columns and the terminal by
    /// [`crate::session::SessionState`]'s read-time folds — never the raw
    /// column, which is [`Self::hook_state`].
    pub status: SessionState,
    pub cwd: Option<PathBuf>,
    pub repo: Option<String>,
    /// Every repository the session spans, in member order — one entry for a
    /// single-repo session. A multi-repo session's group is labelled with all
    /// of them (`a + b`), which `repo` alone cannot express.
    pub repos: Vec<String>,
    /// The directories those repositories are checked out in, in the same order.
    ///
    /// A worktree's *checkout*, not the repository root: opening a session in an
    /// editor has to land on the branch it is working, and `cwd` alone names
    /// only the first of them.
    pub member_dirs: Vec<PathBuf>,
    pub branch: Option<String>,
    /// The branch this session's work is measured *against* — `sessions.base_branch`,
    /// written once at spawn.
    ///
    /// Emphatically not [`Self::branch`], which is the session's own worktree
    /// branch. Confusing the two is not a cosmetic error: a diff taken against a
    /// session's own branch is empty, and an empty diff that reports itself ready
    /// is a wrong answer rather than a missing one. `None` means no base was ever
    /// recorded, and the diff falls back to uncommitted changes.
    pub base_branch: Option<String>,
    pub backend: String,
    /// Backend pane identifier (a tmux `%N`). `None` until the session has a
    /// pane; this is what a live terminal attaches to.
    pub backend_id: Option<String>,
    /// Bare host name for a remote session; `None` when local.
    pub remote_host: Option<String>,
    /// The agent's own session id, which names its statusline metrics file.
    pub agent_session_id: Option<String>,
    pub parent_id: Option<String>,
    pub display_order: Option<i64>,
    pub worktree_count: usize,
    /// Working-tree state, once it has been computed off the render path.
    pub git: Option<GitState>,
    /// The pane id of this session's companion shell, if it had one.
    ///
    /// Persisted because the shell's tmux window outlives the interface: without
    /// it, restarting forgets the shell you had open and leaves its window
    /// orphaned, and the next `shell` key spawns a second one beside it.
    pub shell_backend_id: Option<String>,
    /// Whether this session was **parked on purpose** — its pane was killed by
    /// `session stop`, and its row and checkout are intact.
    ///
    /// The interface relaunches a surveyed session that has no pane, because
    /// normally that means its agent died. A stopped session looks identical
    /// from the outside and is the opposite case: relaunching it would undo the
    /// very thing the operator asked for. Nothing distinguishes the two but
    /// this flag, which is why it is published rather than derived.
    pub stopped: bool,
    /// The raw persisted hook state, before [`derive_state`] interprets it.
    ///
    /// Kept beside the derived `status` because the two answer different
    /// questions: `status` is what to draw, this is what the agent last
    /// reported — and a remote hook event has to be compared against the latter,
    /// or an acknowledged `done` (derived `idle`) would be written back and
    /// resurrected on every reconnect.
    pub hook_state: Option<String>,
    /// The session's key (issue #1408): the address its driver gave it, for a
    /// pane that wants to tell a role apart from a label the user can rename.
    pub key: Option<String>,
    /// The agent a driver **declared** this session runs (`session reports-as`),
    /// when it declared one. A durable statement about the row.
    pub reports_as: Option<String>,
    /// The agent **observed** holding this session's pane, when it is not the
    /// one the row was created with.
    ///
    /// The third of three names, and the reason all three are published: `agent`
    /// is what the row was created as, `reports_as` is what a driver declared,
    /// and this is what is running right now. A driver that asks for a bare
    /// shell and starts an agent in it is otherwise a row labelled `zsh` with
    /// nothing to say about the claude in front of the user.
    ///
    /// A live reading with a short life (see `PANE_PROBE_TTL`) and no claim
    /// about what the agent is *doing* — that is `status`, which stays
    /// [`SessionState::Running`] precisely because no process inspection can
    /// tell a turn in flight from a prompt waiting for input. Published only
    /// for a row nothing has reported for (`assess`'s gate on `hook.state`),
    /// so it is never attached to a row that is already speaking for itself,
    /// regardless of what a stale probe answer still says.
    ///
    /// `None` also when an agent is demonstrably in the pane but *which* one
    /// is not determined — several registered profiles sharing one executable,
    /// which the shipped `agents.toml` teaches the user to create. The status
    /// still reads [`SessionState::Running`], because presence is observed;
    /// only the name is withheld. See [`crate::session::Corroboration`].
    pub detected_agent: Option<String>,
    /// The session's `ui.*` meta keys (issue #1523), cleaned for publishing
    /// ([`crate::kernel::lua_api::clean_meta_value`]) and capped at
    /// [`crate::kernel::lua_api::META_KEYS`]; empty when it has none. Every
    /// other key is a driver's or thurbox's own and never reaches a plugin.
    pub meta: std::collections::BTreeMap<String, String>,
}

/// A session that was deleted but not purged.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedRow {
    pub id: String,
    pub name: String,
    /// The agent it ran, which is what tells two same-named rows apart.
    pub agent: String,
    /// Epoch milliseconds, the same unit `taken_at_ms` carries: a plugin has no
    /// clock, so an age is only computable against the snapshot's own instant.
    pub deleted_at: u64,
    /// Worktrees the row still carries, so a restore that has checkouts to
    /// reattach can say so before it is chosen.
    pub worktrees: usize,
    /// True when the worktree directory was removed as well, so restoring
    /// recovers committed work only. The distinction has to be visible *before*
    /// the choice, not after.
    pub partial: bool,
    /// Why restoring this row will be refused without `best_effort`, if it will
    /// be — [`crate::session_ops::restore_refusal`]'s own sentence, carried so
    /// the restore surface asks exactly when the kernel would object, and asks
    /// about the right thing. Deriving the question from `partial` instead gets
    /// it wrong in both directions: a force-deleted row whose worktrees were
    /// only borrowed is restorable outright, and a row that is not
    /// force-deleted at all still cannot come back if the borrowed directory
    /// has since been removed.
    pub restore_refusal: Option<String>,
}

/// A task, flattened for rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    /// `todo` / `in_progress` / `done`.
    pub status: String,
    /// `local`, or the tracker an imported task came from.
    pub source: String,
    pub external_url: Option<String>,
    /// Epoch seconds. The detail view dates a task, which needs both.
    pub created_at: u64,
    pub updated_at: u64,
}

/// One recorded run of an automation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunRow {
    pub id: i64,
    pub started_at: i64,
    pub status: String,
    /// What the run reported: the error, the skip reason, or an exec's output
    /// tail. The only log an automation run keeps.
    pub detail: String,
    /// The session the run sent to or spawned, when it recorded one.
    pub session: Option<String>,
}

/// An automation, flattened for rendering.
///
/// The first fields summarise; the rest are the definition itself, so a pane
/// can show when it fires and edit it rather than only list it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AutomationRow {
    pub id: i64,
    pub name: String,
    pub schedule: String,
    pub action: String,
    pub enabled: bool,
    /// Outcome of the most recent run, when there has been one.
    pub last_outcome: Option<String>,
    pub last_detail: Option<String>,
    /// Recent runs, newest first — what you look at a scheduler for.
    pub runs: Vec<RunRow>,
    /// The schedule as `cron:<expr>` or `at:<unix_millis>`: what an edit's
    /// `trigger` takes back unchanged.
    pub trigger: String,
    /// IANA name; `None` is the system's own.
    pub timezone: Option<String>,
    /// What a send or spawn types; an exec automation runs `command` and
    /// ignores it.
    pub prompt: String,
    /// A send's target session.
    pub session: Option<String>,
    /// A spawn's repository, worktree branch, base and agent.
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub agent: Option<String>,
    /// How many further repositories a multi-repo spawn spans.
    pub extra_repos: usize,
    /// An exec automation's command line.
    pub command: Option<String>,
    /// Unix millis, as stored.
    pub created_at: u64,
    pub updated_at: u64,
    pub last_run_at: Option<u64>,
    /// `None` while disabled, and once a schedule has no future occurrence.
    pub next_run_at: Option<u64>,
}

/// Something a session can be created against.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoRow {
    pub path: String,
    pub name: String,
}

/// An agent a session can be launched with.
///
/// The command is published beside the name because v1's picker shows it — two
/// entries wrapping the same CLI are otherwise indistinguishable, which is the
/// whole reason a rebranded agent is a supported thing to configure.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRow {
    pub name: String,
    pub command: String,
    /// Whether `command` resolves to something runnable right now.
    ///
    /// Published so the create-session flow can say it *before* the user
    /// commits: a missing agent binary leaves the multiplexer with a window
    /// whose pane exits instantly, which it reports as a successful create, so
    /// the answer never arrives on its own. Probed on a TTL, never per frame —
    /// see `SnapshotStore::poll_preflight`.
    pub presence: crate::backend::preflight::Presence,
}

/// The local multiplexer every session's window is created in.
///
/// One row rather than a bare flag because the name differs by platform
/// (`psmux` on native Windows) and the fix differs with it: a Windows user
/// told to install tmux has been sent to the wrong project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxRow {
    pub binary: String,
    pub configured: Option<String>,
    pub available: Vec<String>,
    pub presence: crate::backend::preflight::Presence,
    /// What to do about it, empty when there is nothing to do.
    pub advice: String,
}

impl Default for MuxRow {
    /// What a snapshot with no store behind it reports: the binary this
    /// platform would use, and no claim about whether it is there.
    fn default() -> Self {
        Self {
            binary: crate::backend::preflight::local_multiplexer().to_string(),
            configured: None,
            available: vec![crate::backend::preflight::local_multiplexer().to_string()],
            presence: crate::backend::preflight::Presence::Unknown,
            advice: String::new(),
        }
    }
}

/// A machine a session can be created on.
///
/// The flow uses the name, detail and backend separately; the session list
/// also needs the host's platform, independent of its transport and mux.
#[derive(Debug, Clone, PartialEq)]
pub struct HostRow {
    pub name: String,
    pub detail: String,
    pub backend: String,
    pub platform: String,
    pub multiplexer: Option<String>,
    pub available_multiplexers: Vec<String>,
    /// The host's probe has not answered yet, so `available_multiplexers` is
    /// the platform default and preference only, and may still grow.
    pub probing: bool,
}

/// An immutable picture of the engine at one instant.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub sessions: Vec<SessionRow>,
    /// Deleted-but-restorable sessions.
    pub deleted: Vec<DeletedRow>,
    /// Repositories a new session can be created against.
    pub repos: Vec<RepoRow>,
    /// Agents, from the same registry the launcher uses.
    pub agents: Vec<AgentRow>,
    /// The registry's default agent, so a flow preselects what a bare launch
    /// would use rather than whichever agent happens to sort first.
    pub agent_default: String,
    /// Configured and discovered hosts; empty means local only.
    pub hosts: Vec<HostRow>,
    /// Whether the local multiplexer is installed, for a flow that wants to
    /// say so before the user commits to a session.
    pub mux: MuxRow,
    pub tasks: Vec<TaskRow>,
    pub automations: Vec<AutomationRow>,
    /// Epoch milliseconds this snapshot represents. Readable by plugins so
    /// they can render staleness.
    pub taken_at_ms: i64,
    /// Set when the last refresh failed, and the previous rows are retained;
    /// or when only the session meta could not be read, and the new rows carry
    /// the labels from before.
    pub error: Option<String>,
}

impl Snapshot {
    pub fn session(&self, id: &str) -> Option<&SessionRow> {
        self.sessions.iter().find(|row| row.id == id)
    }
}

/// Where a store's session rows come from when they are not thurbox's own
/// database's: Fleet's Tasks, through [`fleet::FleetWatch`] (ADR-41).
///
/// Polled on the store's refresh schedule, on the UI thread, so it never
/// blocks: the source reads on a worker of its own and hands over what it
/// has.
pub trait SnapshotSource: Send {
    /// What moved since the last call, or `None` when nothing did.
    fn poll(&mut self) -> Option<SourceRead>;
}

/// One answer of a [`SnapshotSource`]: every session row, and what is wrong
/// with the source, if anything is. The rows stand while the error does.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceRead {
    pub sessions: Vec<SessionRow>,
    pub error: Option<String>,
}

/// An editor request, answered: the session to open it over and the editor
/// setting as stored (`thurbox-cli editor set` / `editor mode`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRequest {
    pub session: String,
    pub command: Option<String>,
    pub mode: crate::session::settings::EditorMode,
}

/// Owns the snapshot and decides when to rebuild it.
///
/// Refresh happens on the kernel's schedule, never inside a plugin call, and
/// the database itself is only ever touched by the `db` worker's thread — so a
/// slow read or a lock another process holds shows up as stale rows, never as
/// a loop that stops painting (issue #1474).
pub struct SnapshotStore {
    db: Option<DbWorker>,
    /// The rows' source when the database is not it; see [`SnapshotSource`].
    source: Option<Box<dyn SnapshotSource>>,
    /// Answers applied from the source, and when a refresh was last asked of
    /// it: what [`Self::request_refresh`]'s ticket counts with a source.
    source_reads: u64,
    source_asked: Instant,
    /// Whether a read has been sent and not answered. At most one is: an
    /// answer is applied whole, and two in flight would only race.
    reading: bool,
    /// A full read is owed: something moved that the `data_version` gate
    /// cannot see, or an answer was discarded as stale.
    reread: bool,
    /// How many writes have been sent. A read answered with fewer predates a
    /// correction made in place, and applying it would undo that correction.
    writes: u64,
    /// Full reads sent, and the number of the last one applied — what
    /// [`Self::request_refresh`] hands out and [`Self::fresh_since`] checks.
    reads_sent: u64,
    reads_applied: u64,
    settles: u64,
    git: GitStats,
    /// What holds each unreported session's pane — see [`PaneProbe`].
    panes: PaneProbe,
    /// The registry shared by picker, default, and status coverage.
    registry: std::sync::Arc<AgentRegistry>,
    agents: Vec<AgentRow>,
    agent_default: String,
    /// Last observed contents or read error, sampled behind `registry_polled_at`.
    registry_contents: Option<Result<String, String>>,
    registry_polled_at: Option<Instant>,
    hosts: Vec<HostRow>,
    /// Whether the create flow is open, so each host may be probed for its
    /// multiplexers — see [`Self::probe_hosts`].
    probe_hosts: bool,
    /// The routes the process's registry serves, read once from it: which
    /// multiplexers the create flow may offer, here and on each host.
    served: std::collections::HashSet<crate::session::Route>,
    /// The process's backends, which the pane probe asks about a row's pane
    /// through: the registry's own handles, cloned once at open.
    backends: crate::backend::BackendRegistry,
    /// Whether the local multiplexer is installed. Beside `agents` because it
    /// is refreshed with them and for the same reason.
    mux: MuxRow,
    /// `settings.toml`'s `multiplexer`, as the store was built with it.
    configured_mux: Option<String>,
    /// When the multiplexer and the agent commands were last looked for.
    /// Gates [`Self::poll_preflight`]; see [`PREFLIGHT_TTL`].
    preflight_at: Instant,
    current: Snapshot,
    last_refresh: Option<Instant>,
    /// Remote hook events that named a pane no session claims yet.
    ///
    /// The subscription's first report routinely arrives before the pane has
    /// been adopted, so an unmatched event is parked rather than lost.
    pending_hooks: Vec<PendingHook>,
    /// Moves whenever anything in [`Self::current`] does.
    ///
    /// What gates the published tables and the pure-pane tree cache: a reader
    /// that saw this value and sees it again knows nothing it could read has
    /// changed. It is bumped **inside** each mutation rather than by callers,
    /// because a mutation that forgets to bump is the one failure here that is
    /// silent — see `tests/kernel_frame_cost.rs`.
    version: u64,
    /// A focus request another process left, claimed during a full read
    /// and handed out by [`Self::take_focus_request`].
    pending_focus: Option<String>,
    /// The database file, for a read that must not queue behind the worker's
    /// writes ([`Self::request_editor`]). `None` without a database.
    db_path: Option<PathBuf>,
    /// Editor settings read for [`Self::request_editor`], each handed out once
    /// by [`Self::take_editor`].
    editor: (
        std::sync::mpsc::Sender<EditorRequest>,
        std::sync::mpsc::Receiver<EditorRequest>,
    ),
    /// When each row's `hook_state` was stamped, by session id.
    ///
    /// Beside the rows rather than on [`SessionRow`] because it is an input to
    /// a fold, not something a pane draws: [`Self::apply_output_quiescence`]
    /// needs the block edge to tell a standing block from one the pane has
    /// since printed past, and every other consumer of that stamp already
    /// reads it from the database.
    hook_state_at: std::collections::HashMap<String, i64>,
    /// The hook report each row was last corrected to in place, by session
    /// id, numbered from `hook_corrections_made`: only the answer to that one
    /// may correct the row again. A stamp cannot say which report an answer
    /// is for — two can share a millisecond, and storage may stamp either
    /// later than the store did.
    hook_corrections: std::collections::HashMap<String, u64>,
    hook_corrections_made: u64,
}

/// A remote hook event waiting for the session it names to appear.
struct PendingHook {
    backend: String,
    pane: String,
    state: String,
    arrived: Instant,
}

impl SnapshotStore {
    /// Open against the real thurbox database.
    ///
    /// A database that will not open is not fatal: the kernel still runs and
    /// every read returns an empty snapshot carrying the error, which a plugin
    /// can render. Losing the UI because the DB is busy would be worse.
    pub fn open(backends: &crate::backend::BackendRegistry, settings: &Settings) -> Self {
        match DbWorker::open() {
            Ok((db, path)) => Self::build(Some(db), Some(path), None, backends, settings),
            Err(error) => Self::build(None, None, Some(error), backends, settings),
        }
    }

    /// Build a store over an already-open database (tests, and any caller that
    /// owns its own connection).
    pub fn with_database(
        database: Database,
        backends: &crate::backend::BackendRegistry,
        settings: &Settings,
    ) -> Self {
        let db_path = db::path_of(&database);
        Self::build(
            Some(DbWorker::spawn(database)),
            db_path,
            None,
            backends,
            settings,
        )
    }

    /// Build a store whose session rows come from `source` rather than the
    /// database. The writes the store makes to session rows (seen marks,
    /// panes, hook reports) have nowhere to land and are dropped.
    pub fn with_source(
        source: Box<dyn SnapshotSource>,
        backends: &crate::backend::BackendRegistry,
        settings: &Settings,
    ) -> Self {
        let mut store = Self::build(None, None, None, backends, settings);
        store.source = Some(source);
        store.poll_source();
        store
    }

    /// Apply what the source has, returning whether anything moved.
    fn poll_source(&mut self) -> bool {
        let Some(read) = self.source.as_mut().and_then(|source| source.poll()) else {
            return false;
        };
        self.source_reads += 1;
        self.current.sessions = read.sessions;
        self.current.error = read.error;
        self.current.taken_at_ms = taken_at_stamp();
        self.mark_changed();
        true
    }

    fn build(
        db: Option<DbWorker>,
        db_path: Option<PathBuf>,
        error: Option<String>,
        backends: &crate::backend::BackendRegistry,
        settings: &Settings,
    ) -> Self {
        let registry = read_registry();
        crate::agent::agent_config::publish_registry(&registry);
        let served: std::collections::HashSet<_> = backends.routes().cloned().collect();
        let mut store = Self {
            db,
            source: None,
            source_reads: 0,
            source_asked: Instant::now(),
            db_path,
            reading: false,
            reread: false,
            writes: 0,
            reads_sent: 0,
            reads_applied: 0,
            settles: 0,
            git: GitStats::new(git_poll_interval(settings.git_poll_secs)),
            panes: PaneProbe::default(),
            agents: read_agents(&registry),
            agent_default: registry.default_name().to_string(),
            registry,
            registry_contents: None,
            registry_polled_at: None,
            hosts: read_hosts(&served, false),
            probe_hosts: false,
            mux: read_mux(&served, settings.multiplexer.clone()),
            configured_mux: settings.multiplexer.clone(),
            served,
            backends: backends.clone(),
            preflight_at: Instant::now(),
            current: Snapshot {
                error,
                ..Snapshot::default()
            },
            last_refresh: None,
            pending_hooks: Vec::new(),
            version: 0,
            pending_focus: None,
            editor: std::sync::mpsc::channel(),
            hook_state_at: std::collections::HashMap::new(),
            hook_corrections: std::collections::HashMap::new(),
            hook_corrections_made: 0,
        };
        store.refresh();
        store
    }

    /// The current snapshot. Always immediate.
    /// How many times the snapshot has changed. See [`Self::version`]'s field.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Record that the snapshot changed. Every write to `current` goes through
    /// a call to this in the same breath.
    fn mark_changed(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    pub fn current(&self) -> &Snapshot {
        &self.current
    }

    pub fn agent_registry(&self) -> &AgentRegistry {
        &self.registry
    }

    /// Adopt an edited agents.toml at the same cadence as settings polling.
    /// A failed edit keeps all four readers on the last good registry.
    pub fn poll_registry(&mut self) -> Option<Result<Vec<String>, String>> {
        const POLL_INTERVAL: Duration = Duration::from_secs(1);
        if self
            .registry_polled_at
            .is_some_and(|at| at.elapsed() < POLL_INTERVAL)
        {
            return None;
        }
        self.registry_polled_at = Some(Instant::now());
        let first_poll = self.registry_contents.is_none();
        let contents = crate::agent::agent_config::read_for_reload();
        if self.registry_contents.as_ref() == Some(&contents) {
            return None;
        }
        self.registry_contents = Some(contents.clone());
        let (registry, warnings) = match contents
            .and_then(|contents| crate::agent::agent_config::parse_for_reload(&contents))
        {
            Ok(loaded) => loaded,
            Err(error) => return Some(Err(error)),
        };
        if *self.registry != registry {
            self.agent_default = registry.default_name();
            self.agents = read_agents(&registry);
            self.registry = std::sync::Arc::new(registry);
            crate::agent::agent_config::publish_registry(&self.registry);
            // The picker's rows come from the registry, not the database, so
            // they publish now; each session's coverage is judged against it
            // too, which is what the read is for.
            self.current.agents = self.agents.clone();
            self.current.agent_default = self.agent_default.clone();
            self.mark_changed();
            self.request_refresh();
        } else if first_poll && warnings.is_empty() {
            return None;
        }
        Some(Ok(warnings))
    }

    /// Store the chosen theme, for every other process to pick up. Queued,
    /// never waited for: a lock held elsewhere delays the write, not a frame.
    pub fn persist_theme(&mut self, name: &str) {
        self.send_write(Write::Theme(name.to_string()));
    }

    /// Ask for the editor setting, to open the editor over `session` once it
    /// is read ([`Self::take_editor`]).
    ///
    /// Read on a thread of its own (`db::read_editor`), not on the worker:
    /// the worker's queue can hold a write waiting out another process's lock,
    /// and the key would then open the editor seconds later, over whatever the
    /// user had moved on to.
    pub fn request_editor(&mut self, session: &str) {
        db::read_editor(
            session.to_string(),
            self.db_path.clone(),
            self.editor.0.clone(),
        );
    }

    /// An editor setting a [`Self::request_editor`] asked for, once read.
    pub fn take_editor(&mut self) -> Option<EditorRequest> {
        self.editor.1.try_recv().ok()
    }

    /// Publish telemetry through our connection, without invalidating our rows.
    /// Queued, never waited for: a failure is logged by the worker.
    pub fn set_perf_snapshot(&mut self, json: &str) {
        self.send_write(Write::Perf(json.to_string()));
    }

    /// Rebuild if the refresh interval has elapsed *and* anything committed.
    /// Called from the event loop, never from a plugin. Returns whether rows
    /// read from the database were applied by this call.
    ///
    /// The interval alone would re-read five tables (plus the automation run
    /// history) every 400ms forever, on a database nobody wrote to.
    /// `PRAGMA data_version` reads an in-memory counter and moves whenever
    /// another connection commits. The snapshot generations then distinguish
    /// row changes from hook liveness and perf telemetry, so only row changes
    /// need the full rebuild (ADR-P6). The worker applies that gate; this
    /// applies whatever it answered, on every call, and asks again when due.
    ///
    /// Git stats are folded in either way: they arrive from worker threads, not
    /// from the database, so `data_version` says nothing about them.
    pub fn refresh_if_due(&mut self) -> bool {
        self.refresh_if_due_at(Instant::now())
    }

    /// Poll using the caller's monotonic clock. The row read and other workers
    /// retain their own timestamps; this instant controls only the poll cadence.
    pub fn refresh_if_due_at(&mut self, now: Instant) -> bool {
        let applied = self.drain_replies() | self.poll_source();
        let due = self
            .last_refresh
            .is_none_or(|at| now.saturating_duration_since(at) >= REFRESH_INTERVAL);
        if !due {
            return applied;
        }
        self.last_refresh = Some(now);
        // Asked either way, like the git stats below and for the same reason:
        // the answers come from worker threads, so `data_version` says nothing
        // about them — and without asking on this path a verdict would be
        // requested once and never refreshed.
        if self.poll_pane_probes() {
            self.reread = true;
        }
        // Asked here rather than with the rows, which stop being read
        // altogether on a database nobody writes to — which is exactly the
        // state thurbox is in while the user is off installing what was
        // missing.
        let preflight_moved = self.poll_preflight() | self.poll_hosts();
        self.send_read();
        self.current.taken_at_ms = taken_at_stamp();
        // Git stats landing rewrite rows here, so this changes more than the
        // timestamp, and the snapshot moved if they did.
        let git_moved = self.attach_git_stats();
        if git_moved || preflight_moved {
            self.mark_changed();
        }
        applied
    }

    /// Ask for every row now, without waiting for the interval or a commit.
    /// Returns a ticket [`Self::fresh_since`] accepts once rows read after
    /// this call are the ones published.
    pub fn request_refresh(&mut self) -> u64 {
        if self.source.is_some() {
            self.source_asked = Instant::now();
            return self.source_reads + 1;
        }
        self.reread = true;
        self.send_read();
        // The read that settles this is the next full one sent — this call's
        // own, or, with one already in flight, the one sent when it lands.
        if self.reread {
            self.reads_sent + 1
        } else {
            self.reads_sent
        }
    }

    /// Whether the rows published now were read after `ticket` was handed out.
    /// Always true without a database: there is nothing further to wait for.
    pub fn fresh_since(&self, ticket: u64) -> bool {
        if self.source.is_some() {
            return self.source_reads >= ticket || self.source_asked.elapsed() >= SOURCE_SETTLE;
        }
        self.db.is_none() || self.reads_applied >= ticket
    }

    /// Send a read unless one is in flight; full when one is owed.
    fn send_read(&mut self) {
        let Some(db) = &self.db else {
            return;
        };
        if self.reading {
            return;
        }
        let force = std::mem::take(&mut self.reread);
        if force {
            self.reads_sent += 1;
        }
        self.reading = true;
        db.send(Job::Read { force });
    }

    fn send_write(&mut self, write: Write) {
        if let Some(db) = &self.db {
            // Counted the way the worker counts: telemetry corrects no row, so
            // it must not make a read in flight look stale.
            if write.touches_rows() {
                self.writes += 1;
            }
            db.send(Job::Write(write));
        }
    }

    /// Apply every answer the worker has sent, returning whether rows were.
    fn drain_replies(&mut self) -> bool {
        let mut applied = false;
        while let Some(reply) = self.db.as_ref().and_then(|db| db.replies.try_recv().ok()) {
            applied |= self.apply_reply(reply);
        }
        applied
    }

    /// Wait until the worker has answered everything sent so far, and apply
    /// it, returning whether rows were. Never on the event loop: for start-up,
    /// and for a test that has to see a write or a read land.
    pub fn settle(&mut self) -> bool {
        self.wait().unwrap_or(false)
    }

    /// [`Self::settle`], answering `None` when there is no worker to wait for.
    fn wait(&mut self) -> Option<bool> {
        let db = self.db.as_ref()?;
        self.settles += 1;
        let token = self.settles;
        db.send(Job::Settle(token));
        let mut applied = false;
        loop {
            let reply = self.db.as_ref()?.replies.recv().ok()?;
            if matches!(reply, Reply::Settled(t) if t == token) {
                return Some(applied);
            }
            applied |= self.apply_reply(reply);
        }
    }

    /// Returns whether the reply was rows that were applied.
    fn apply_reply(&mut self, reply: Reply) -> bool {
        match reply {
            Reply::Rows { writes, read } => {
                self.reading = false;
                let number = self.reads_sent;
                if writes != self.writes {
                    self.reread = true;
                    self.send_read();
                    return false;
                }
                self.apply_rows(*read);
                self.reads_applied = number;
                self.send_owed_read();
                true
            }
            Reply::Hooks { writes, hooks } => {
                self.reading = false;
                if writes != self.writes || !self.patch_hooks(&hooks) {
                    self.reread = true;
                }
                self.send_owed_read();
                false
            }
            Reply::Unchanged => {
                self.reading = false;
                self.send_owed_read();
                false
            }
            Reply::Failed(error) => {
                self.reading = false;
                // The previous rows stand: a transient lock degrades to stale
                // data rather than a blank list.
                self.current.error = Some(error);
                self.current.taken_at_ms = taken_at_stamp();
                self.reads_applied = self.reads_sent;
                self.mark_changed();
                self.send_owed_read();
                false
            }
            Reply::Focus(id) => {
                self.pending_focus = Some(id);
                false
            }
            Reply::Stamped {
                session,
                correction,
                at,
                state,
            } => {
                // A later report corrected the row since: its own answer is
                // still to come, and until then its stamp and word stand.
                if self.hook_corrections.get(&session) != Some(&correction) {
                    return false;
                }
                self.hook_corrections.remove(&session);
                self.hook_state_at.insert(session.clone(), at);
                // The row was corrected in place to the word reported; storage
                // may have settled it as another.
                let Some(row) = self
                    .current
                    .sessions
                    .iter_mut()
                    .find(|row| row.id == session)
                else {
                    return false;
                };
                if row.hook_state.as_deref() == Some(state.as_str()) {
                    return false;
                }
                row.status = derive_state(Some(&state), Some(at), None);
                row.hook_state = Some(state);
                self.mark_changed();
                true
            }
            Reply::Rejected => {
                self.reread = true;
                self.send_read();
                false
            }
            Reply::Settled(_) => false,
        }
    }

    /// A full read owed while one was in flight goes out as that one lands.
    fn send_owed_read(&mut self) {
        if self.reread {
            self.send_read();
        }
    }

    /// Patch hook stamps into the rows, returning false when the rows need a
    /// full read instead.
    ///
    /// A real transition can commit between reading the generations and the
    /// stamps, so a row whose hook state differs is rebuilt rather than patched
    /// with a different raw state.
    fn patch_hooks(&mut self, hooks: &std::collections::HashMap<SessionId, HookRow>) -> bool {
        if self.current.sessions.iter().any(|row| {
            parse_id(&row.id)
                .and_then(|id| hooks.get(&id))
                .is_none_or(|hook| hook.state != row.hook_state)
        }) {
            return false;
        }
        let mut changed = false;
        for row in &mut self.current.sessions {
            let Some(hook) = parse_id(&row.id).and_then(|id| hooks.get(&id)) else {
                continue;
            };
            if let Some(stamp) = hook.state_at {
                self.hook_state_at.insert(row.id.clone(), stamp);
            } else {
                self.hook_state_at.remove(&row.id);
            }
            // Working/blocked are folded against output each tick. Only
            // done can change its visible state from a new stamp alone.
            if !row.stopped && row.hook_state.as_deref() == Some("done") {
                let status = derive_state(Some("done"), hook.state_at, hook.seen_at);
                if row.status != status {
                    row.status = status;
                    changed = true;
                }
            }
        }
        if changed {
            self.mark_changed();
        }
        true
    }

    /// Take the pane verdicts the workers have answered and ask for the ones
    /// that are missing or stale, returning whether any verdict moved.
    ///
    /// A moved verdict makes the derived rows stale in a way `PRAGMA
    /// data_version` cannot see, so it forces the rebuild rather than being
    /// patched into the rows: the status it feeds is decided by `assess`, and a
    /// second place that decided it would be the second answer this module was
    /// written to prevent.
    ///
    /// Only rows whose agent has reported nothing are asked about — everything
    /// else already has a better answer than a process listing can give.
    fn poll_pane_probes(&mut self) -> bool {
        let moved = self.panes.drain();

        let backends = &self.backends;
        let wanted: Vec<(String, String, String, _)> = self
            .current
            .sessions
            .iter()
            .filter(|row| {
                row.hook_state.is_none()
                    && !row.stopped
                    && !crate::session::Route::is_remote_key(&row.backend)
            })
            .filter_map(|row| {
                // A route nothing here serves has no pane to ask about.
                let backend = crate::session_ops::windows::backend_for(backends, &row.backend)
                    .ok()?
                    .clone();
                // The agent *binary*, not the agent name: `antigravity` runs
                // `agy`, and a pane's foreground process is spelled the way it
                // was invoked.
                let agent = row.reports_as.as_deref().unwrap_or(&row.agent);
                let command = self
                    .registry
                    .get(agent)
                    .map(|def| def.command.clone())
                    .unwrap_or_else(|| agent.to_string());
                Some((row.id.clone(), row.name.clone(), command, backend))
            })
            .collect();

        let probed: std::collections::HashSet<&str> =
            wanted.iter().map(|(id, _, _, _)| id.as_str()).collect();
        self.panes.retain(&probed);

        for (id, name, command, backend) in wanted {
            self.panes
                .request(backend, &id, &name, command, &self.registry);
        }
        moved
    }

    /// Attach whatever git stats are known and ask for anything stale.
    ///
    /// Asking on every refresh is free — `GitStats::request` is the one place that
    /// decides whether a stat is worth running, by its age. Asking only for rows
    /// with no answer yet is what froze every session's diffstat at its first
    /// reading for the life of the process.
    /// Returns whether any row's stats actually changed, so a caller that is
    /// deciding whether the snapshot moved can ask rather than assume.
    fn attach_git_stats(&mut self) -> bool {
        self.git.drain();
        let present: std::collections::HashSet<&str> = self
            .current
            .sessions
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        self.git.retain(&present);

        let mut wanted: Vec<(String, PathBuf, Option<crate::session::HostDef>)> = Vec::new();
        let mut moved = false;
        for row in &mut self.current.sessions {
            let stats = self.git.state(&row.id);
            if row.git != stats {
                row.git = stats;
                moved = true;
            }
            // A Fleet Task's workspace is on whichever machine its runtime
            // serves, which this one cannot reach with git.
            if row.backend == fleet::BACKEND {
                continue;
            }
            if let Some(cwd) = row.cwd.clone() {
                // A remote session's `cwd` is a path on its host, so its stats
                // are read there. A host this machine cannot name is skipped —
                // running `git` locally against that path would only fail.
                let host = if crate::session::Route::is_remote_key(&row.backend) {
                    match crate::session_ops::resolve_host(&row.backend).flatten() {
                        Some(host) => Some(host),
                        None => continue,
                    }
                } else {
                    None
                };
                wanted.push((row.id.clone(), cwd, host));
            }
        }
        for (id, cwd, host) in wanted {
            self.git.request(&id, cwd, host);
        }
        moved
    }

    /// Acknowledge a finished turn: stamp `seen_at` on a session whose state is
    /// `done`, so its filled dot becomes hollow.
    ///
    /// Called when focus *leaves* a session — looking at a finished turn and then
    /// moving on is what "seen" means, and it is the only thing that retires a
    /// `done`. Without it the blue dot is permanent, since [`derive_state`]
    /// reads a mark nobody writes.
    ///
    /// Asked only of a row these rows show as an unseen `done`, so moving the
    /// selection through any other session queues nothing; the worker checks
    /// the stored row again before stamping. The derived status is corrected in
    /// place as well: this write does not move `PRAGMA data_version` (it is our
    /// own connection), so a refresh would otherwise re-derive `done` from the
    /// row it just acknowledged.
    pub fn acknowledge(&mut self, session: &str) {
        if self.db.is_none() {
            return;
        }
        let Some(id) = parse_id(session) else { return };
        let Some(&at) = self.hook_state_at.get(session) else {
            return;
        };
        let Some(row) = self
            .current
            .sessions
            .iter_mut()
            .find(|row| row.id == session && row.status == SessionState::Done)
        else {
            return;
        };
        row.status = SessionState::Idle;
        self.mark_changed();
        self.send_write(Write::Seen(id, at));
    }

    /// Capture the cached report before delivering Enter. The worker compares
    /// it against storage after delivery so a concurrent hook can win.
    pub fn codex_submission_report(&self, session: &str) -> Option<HookRow> {
        let row = self.current.session(session)?;
        let codex = row.agent == "codex"
            || self
                .registry
                .get(&row.agent)
                .is_some_and(|def| def.is_family("codex"));
        if !codex || row.stopped {
            return None;
        }
        Some(HookRow {
            state: row.hook_state.clone(),
            state_at: self.hook_state_at.get(session).copied(),
            seen_at: None,
        })
    }

    /// A delivered Enter may submit a prompt. Publish the coarse pane state
    /// while [`Self::retire_codex_report`] retires the older one in storage.
    pub fn note_codex_submission(&mut self, session: &str, previous: &HookRow) {
        if previous.state.is_none() {
            return;
        }
        if let Some(row) = self
            .current
            .sessions
            .iter_mut()
            .find(|row| row.id == session)
        {
            row.hook_state = None;
            row.status = SessionState::Running;
            self.hook_state_at.remove(session);
            // The report this retires may still be waiting for its answer,
            // which must not put it back.
            self.hook_corrections.remove(session);
            self.mark_changed();
        }
    }

    /// Clear the report a delivered Enter superseded, on the queue its own write
    /// went through, so it runs after that write whichever lands first here.
    pub fn retire_codex_report(&mut self, session: &str, previous: &HookRow) {
        if previous.state.is_none() {
            return;
        }
        if let Some(id) = parse_id(session) {
            self.send_write(Write::Retire(id, previous.clone()));
        }
    }

    /// Apply remote agents' hook reports to the sessions they name.
    ///
    /// A remote agent cannot call `thurbox-cli session signal` — there is no CLI
    /// on the host, and it would write the host's own database — so its hooks set
    /// a tmux pane option instead, which arrives here over the control-mode
    /// subscription. Landing it in the same columns a local signal writes is what
    /// makes remote status work at all: everything downstream (the dot, the
    /// done→seen acknowledgment, notifications) is already shared.
    ///
    /// Returns how many were applied, so the caller can repaint only when
    /// something moved.
    pub fn apply_hook_states(
        &mut self,
        events: Vec<(String, String, String)>,
        now: Instant,
    ) -> usize {
        /// Unmatched events are retried this long — comfortably past a slow
        /// host's first attach — then dropped. v1 parks them the same way.
        const PENDING_TTL: Duration = Duration::from_secs(120);
        /// Bounded so another instance's panes cannot accumulate here.
        const PENDING_CAP: usize = 256;

        if self.db.is_none() {
            return 0;
        }
        let mut queue = std::mem::take(&mut self.pending_hooks);
        let mut writes = Vec::new();
        queue.extend(
            events
                .into_iter()
                .map(|(backend, pane, state)| PendingHook {
                    backend,
                    pane,
                    state,
                    arrived: now,
                }),
        );

        let mut applied = 0;
        for event in queue {
            // Remote-host-controlled free text: a word that is no state is not
            // even kept for a pane that appears later.
            if !crate::session_ops::signals::is_state(&event.state) {
                continue;
            }
            // A backend reports under the qualified route it serves; the row
            // may be stored under any spelling of it.
            let Some(row) = self.current.sessions.iter_mut().find(|row| {
                row.backend_id.as_deref() == Some(&event.pane)
                    && crate::session_ops::server_key(&row.backend) == event.backend
            }) else {
                // Pane ids collide across hosts, so an event is only ever matched
                // by backend *and* pane — and until that pair exists there is
                // nothing to write it to.
                if now.duration_since(event.arrived) < PENDING_TTL
                    && self.pending_hooks.len() < PENDING_CAP
                {
                    self.pending_hooks.push(event);
                }
                continue;
            };
            // Judged as the heartbeat's poll judges it, before the row is
            // corrected in place: a re-report on reconnect, or a state for a
            // parked session, must not be shown as though it had been written.
            if !crate::session_ops::signals::echo_changes(
                row.hook_state.as_deref(),
                row.stopped,
                &event.state,
            ) {
                continue;
            }
            let Some(id) = parse_id(&row.id) else {
                continue;
            };
            // Corrected in place for the same reason `acknowledge` does it: this
            // is our own connection, so `PRAGMA data_version` will not move and a
            // refresh would re-derive from the row we just wrote. `detected_agent`
            // is cleared here rather than left to that (absent) refresh, because
            // this path never reaches `assess` at all — it writes the row
            // directly, which is the one case its hook.state gate cannot cover.
            // Stamped now; the worker answers with the stamp and the word
            // storage chose.
            let stamped = crate::storage::current_time_millis() as i64;
            self.hook_state_at.insert(row.id.clone(), stamped);
            row.status = derive_state(Some(&event.state), Some(stamped), None);
            row.hook_state = Some(event.state.clone());
            row.detected_agent = None;
            self.hook_corrections_made += 1;
            let correction = self.hook_corrections_made;
            self.hook_corrections.insert(row.id.clone(), correction);
            writes.push(Write::Hook {
                id,
                row: row.id.clone(),
                state: event.state,
                provisional: stamped,
                correction,
            });
            applied += 1;
        }
        for write in writes {
            self.send_write(write);
        }
        if applied > 0 {
            self.mark_changed();
        }
        applied
    }

    /// Re-derive every `working` and `blocked` row against terminal quiescence,
    /// returning how many rows changed.
    ///
    /// Called each tick rather than folded into `refresh`, because the answer
    /// moves with the agent's output and `refresh` runs on the database's
    /// cadence — a row read once and left alone would report a turn as finished
    /// for as long as the snapshot stood.
    ///
    /// Re-derived from `hook_state` rather than adjusted from `status`, so the
    /// pass is idempotent and reverses itself: a session that goes quiet and
    /// then prints again is `working` once more without a database read.
    ///
    /// `blocked` is here for the opposite reason to `working` and on the same
    /// evidence. It is never time-gated — a real block is quiet for as long as
    /// it stands — but a block the pane has gone on printing past is over, and
    /// this is the only surface that can see that: `state_at` alone cannot say
    /// it, which is why the CLI still reports the latched word.
    ///
    /// `quiet_for` is asked only about the rows that are actually in one of
    /// those two states, and answers `None` for a session with no live pane. A
    /// closure rather than a map because the answer is one atomic load and a map
    /// would allocate every tick to carry the sessions nobody asked about
    /// (ADR-P10).
    pub fn apply_output_quiescence(&mut self, quiet_for: impl Fn(&str) -> Option<u64>) -> usize {
        let now = crate::storage::current_time_millis() as i64;
        let mut changed = 0;
        for row in &mut self.current.sessions {
            // A parked session has no process to be working or blocked in, and
            // `Stopped` outranks both — see `Assessment::state`. Its hook
            // columns still hold whatever stood before `session stop`, so
            // without this the pass would talk over the one state thurbox
            // knows first-hand.
            if row.stopped {
                continue;
            }
            let Some(state) = row
                .hook_state
                .as_deref()
                .and_then(SessionState::from_hook_state)
                .filter(|s| matches!(s, SessionState::Working | SessionState::Blocked))
            else {
                continue;
            };
            let age = self
                .hook_state_at
                .get(&row.id)
                .map(|at| u64::try_from((now - at).max(0)).unwrap_or(0));
            let derived = with_output_quiescence(state, quiet_for(&row.id), age);
            if row.status != derived {
                row.status = derived;
                changed += 1;
            }
        }
        if changed > 0 {
            self.mark_changed();
        }
        changed
    }

    /// Record — or forget — the pane id of a session's companion shell.
    ///
    /// Queued and corrected in place, like `acknowledge`: the write is on
    /// our own connection, so `PRAGMA data_version` will not move and a
    /// refresh would otherwise re-read the row we just wrote.
    pub fn remember_shell(&mut self, session: &str, pane: &str) -> Result<(), String> {
        self.write_shell(session, Some(pane.to_string()))
    }

    pub fn forget_shell(&mut self, session: &str) -> Result<(), String> {
        self.write_shell(session, None)
    }

    fn write_shell(&mut self, session: &str, pane: Option<String>) -> Result<(), String> {
        if self.db.is_none() {
            return Err("no database".to_string());
        }
        let Some(id) = parse_id(session) else {
            return Err(format!("not a session id: {session}"));
        };
        let Some(row) = self.current.session(session) else {
            return Err(format!("session not found: {session}"));
        };
        // Nothing to write when the value stands: the in-place correction below
        // keeps the snapshot row current, so it can answer without a read.
        if row.shell_backend_id == pane {
            return Ok(());
        }
        let seen = row.shell_backend_id.clone();
        // A targeted single-column UPDATE: the full `upsert_session` rewrite it
        // replaced also re-wrote the worktree rows, costing a commit per
        // statement to change one column. It lands only over the pane the row
        // showed here; one that moved on meanwhile drops it, and the rows are
        // read again.
        self.send_write(Write::Shell {
            id,
            seen,
            pane: pane.clone(),
        });
        if let Some(current) = self
            .current
            .sessions
            .iter_mut()
            .find(|row| row.id == session)
        {
            current.shell_backend_id = pane;
        }
        Ok(())
    }

    /// Record the pane id a session's agent window was adopted at.
    ///
    /// The migration path for rows persisted before local spawns recorded their
    /// pane id: the interface resolved this one by window name, and a name is
    /// not unique — persisting the id is what stops the row depending on it.
    /// Same shape as [`Self::remember_shell`]: a targeted single-column UPDATE
    /// plus an in-place correction of the snapshot row, since our own write
    /// does not move `data_version`.
    pub fn remember_pane(&mut self, session: &str, pane: &str) -> Result<(), String> {
        if self.db.is_none() {
            return Err("no database".to_string());
        }
        let Some(id) = parse_id(session) else {
            return Err(format!("not a session id: {session}"));
        };
        let Some(row) = self.current.session(session) else {
            return Err(format!("session not found: {session}"));
        };
        if row.backend_id.as_deref() == Some(pane) {
            return Ok(());
        }
        let seen = row.backend_id.clone().unwrap_or_default();
        // Claimed only over the pane the row showed here. One refused — the
        // row moved on or gone, or the pane another session's — brings the
        // rows round again.
        self.send_write(Write::Pane {
            id,
            seen,
            pane: pane.to_string(),
        });
        if let Some(current) = self
            .current
            .sessions
            .iter_mut()
            .find(|row| row.id == session)
        {
            current.backend_id = Some(pane.to_string());
        }
        Ok(())
    }

    /// Take a pending "focus this session" request, if another process left one.
    ///
    /// The database probe rides the worker's full reads, not this call: the claiming
    /// `DELETE … RETURNING` is a write statement that takes the WAL write lock
    /// even when it matches nothing, so running it per loop iteration put
    /// 20–100 write-lock cycles a second on the UI thread — and, behind the 5 s
    /// busy timeout, a moment of contention with `thurbox-cli` or the heartbeat
    /// could stall a frame for that long. The row can only appear via another
    /// connection's commit, which is exactly what moves `data_version` and
    /// brings a full read round, so riding that gate loses nothing but a
    /// sub-interval of latency on a notification click.
    pub fn take_focus_request(&mut self) -> Option<String> {
        self.pending_focus.take()
    }

    /// Look for the multiplexer and each agent's command again, at most once
    /// per [`PREFLIGHT_TTL`]. Returns whether any answer moved.
    ///
    /// The whole cost model of this feature lives here: the create-session flow
    /// reads a *published* answer, so the probe happens on the kernel's
    /// schedule and behind a window, never on a render, a keystroke or a row.
    fn poll_preflight(&mut self) -> bool {
        if self.preflight_at.elapsed() < PREFLIGHT_TTL {
            return false;
        }
        self.preflight_at = Instant::now();
        let mux = read_mux(&self.served, self.configured_mux.clone());
        let mut moved = mux != self.mux;
        self.mux = mux;
        for row in &mut self.agents {
            let presence = crate::backend::preflight::look_up(&row.command);
            if presence != row.presence {
                row.presence = presence;
                moved = true;
            }
        }
        if moved {
            // `refresh` may not run for minutes on an idle database, and until
            // it does `current` is what every reader sees.
            self.current.mux = self.mux.clone();
            self.current.agents = self.agents.clone();
        }
        moved
    }

    /// Whether the create flow is open. Only then is a host probed for which
    /// multiplexers it has: a closed flow costs no connection to any host.
    pub fn probe_hosts(&mut self, wanted: bool) {
        self.probe_hosts = wanted;
    }

    /// Re-read the host rows while the flow is open, which is how a probe
    /// answering in the background reaches the picker. A cache lookup per
    /// host, never a wait. Returns whether any row moved.
    fn poll_hosts(&mut self) -> bool {
        if !self.probe_hosts {
            return false;
        }
        let hosts = read_hosts(&self.served, true);
        if hosts == self.hosts {
            return false;
        }
        self.hosts = hosts;
        self.current.hosts = self.hosts.clone();
        true
    }

    /// Rebuild now, waiting for the read. Start-up and tests only: the event
    /// loop asks with [`Self::request_refresh`] and never waits.
    ///
    /// A failed read keeps the previous rows and records the error, so a
    /// transient database lock degrades to stale data rather than a blank list.
    pub fn refresh(&mut self) {
        self.last_refresh = Some(Instant::now());
        if self.db.is_none() {
            self.current.taken_at_ms = taken_at_stamp();
            self.mark_changed();
            return;
        }
        // A read answered stale is sent again once it lands, so this may take
        // more than one round.
        let ticket = self.request_refresh();
        while !self.fresh_since(ticket) {
            if self.wait().is_none() {
                return;
            }
        }
    }

    /// Turn a full read into the published rows.
    fn apply_rows(&mut self, read: RowsRead) {
        let taken_at_ms = taken_at_stamp();
        let RowsRead {
            sessions,
            hooks,
            bases,
            stopped,
            reports_as,
            mut launch_failures,
            meta,
            deleted,
            tasks,
            automations,
        } = read;
        // A meta read that failed keeps each row's labels as last read, and
        // the error says so; the rows themselves still move.
        let (mut meta, meta_error) = match meta {
            Ok(meta) => (meta, None),
            Err(error) => (
                self.current
                    .sessions
                    .iter()
                    .filter(|row| !row.meta.is_empty())
                    .filter_map(|row| Some((row.id.parse::<SessionId>().ok()?, row.meta.clone())))
                    .collect(),
                Some(error),
            ),
        };
        self.hook_state_at = hooks
            .iter()
            .filter_map(|(id, row)| Some((id.to_string(), row.state_at?)))
            .collect();
        let now = crate::storage::current_time_millis() as i64;

        self.git.drain();

        let rows: Vec<SessionRow> = sessions
            .into_iter()
            .map(|session| {
                let hook = hooks.get(&session.id);
                let hook_state = hook.and_then(|h| h.state.clone());
                let declared = reports_as.get(&session.id).cloned();
                // The whole assessment, not `derive_state` alone. Deriving from
                // the hook columns by themselves answers `idle` for a session
                // that never reported — laundering "we cannot know" into "the
                // agent says it is at rest", which is the one conflation
                // `session::hook_status` exists to prevent. The CLI has always
                // answered `running`/`uncovered`/`unreported` here; the screen
                // now answers the same words.
                let assessment = assess(
                    &self.registry,
                    declared.as_deref().unwrap_or(&session.agent),
                    hook,
                    now,
                    stopped.contains(&session.id),
                    &session.backend_type,
                    self.panes.get(&session.id.to_string()),
                )
                .with_launch_failure(launch_failures.remove(&session.id));
                let status = assessment.state();
                let detected_agent = assessment.detected_agent().map(str::to_string);
                let worktree = session.worktrees.first();
                let members = session_members(
                    session.cwd.as_deref(),
                    &session.worktrees,
                    &session.additional_dirs,
                );
                let repos: Vec<String> = members
                    .iter()
                    .filter_map(|(name, _)| name.clone())
                    .collect();
                let member_dirs: Vec<PathBuf> = members.into_iter().map(|(_, path)| path).collect();
                SessionRow {
                    id: session.id.to_string(),
                    name: session.name,
                    agent: session.agent,
                    status,
                    repo: repo_name(&session.cwd, worktree.map(|w| &w.repo_path)),
                    repos,
                    branch: worktree.map(|w| w.branch.clone()),
                    base_branch: bases.get(&session.id).cloned(),
                    cwd: session.cwd,
                    remote_host: remote_host_of(&session.backend_type),
                    agent_session_id: session.agent_session_id,
                    backend_id: Some(session.backend_id).filter(|id| !id.is_empty()),
                    backend: session.backend_type,
                    parent_id: session.parent_session_id.map(|id| id.to_string()),
                    display_order: session.display_order,
                    worktree_count: session.worktrees.len(),
                    git: None,
                    shell_backend_id: session.shell_backend_id.clone(),
                    stopped: stopped.contains(&session.id),
                    hook_state,
                    key: session.key,
                    reports_as: declared,
                    detected_agent,
                    member_dirs,
                    meta: meta.remove(&session.id).unwrap_or_default(),
                }
            })
            .collect();

        // Repositories worth offering: the ones sessions already live in. A
        // richer source (a scan, a bookmark list) can replace this without the
        // plugin changing, because it only ever sees the published list.
        let mut repos: Vec<RepoRow> = Vec::new();
        for row in &rows {
            let Some(path) = row.cwd.as_ref() else {
                continue;
            };
            let text = path.to_string_lossy().to_string();
            if repos.iter().any(|repo| repo.path == text) {
                continue;
            }
            repos.push(RepoRow {
                name: row.repo.clone().unwrap_or_else(|| text.clone()),
                path: text,
            });
        }
        repos.sort_by(|a, b| a.name.cmp(&b.name));

        self.current = Snapshot {
            sessions: rows,
            deleted,
            tasks,
            automations,
            repos,
            agents: self.agents.clone(),
            agent_default: self.agent_default.clone(),
            hosts: self.hosts.clone(),
            mux: self.mux.clone(),
            taken_at_ms,
            error: meta_error,
        };
        self.attach_git_stats();
        self.mark_changed();
    }
}

/// One row's whole state assessment, from the columns plus whatever the pane
/// probe has answered so far.
///
/// The same [`Assessment`] the CLI builds (`cli::sessions::SessionFacts::
/// assess`), with the pane verdict handed in rather than probed for: the render
/// loop may not shell out, so [`PaneProbe`] does it on a worker and the answer
/// arrives here later. `None` means *not looked at yet*, which resolves to
/// `uncovered`/`unreported` — both honest, and neither of them `idle`.
fn assess(
    registry: &AgentRegistry,
    agent: &str,
    hook: Option<&crate::storage::HookRow>,
    now: i64,
    parked: bool,
    backend: &str,
    pane: Option<&Corroboration>,
) -> Assessment {
    let assessment = Assessment::from_hooks(
        registry,
        agent,
        hook.and_then(|h| h.state.as_deref()),
        hook.and_then(|h| h.state_at),
        hook.and_then(|h| h.seen_at),
        now,
    );
    if parked {
        return assessment.parked();
    }
    // A remote pane lives on its own host's multiplexer, which is not a thing
    // this process can ask `ps` about — `unavailable`, never `unknown`.
    if crate::session::Route::is_remote_key(backend) {
        return assessment.pane_unavailable();
    }
    // An observation is published only for a row nothing has reported for.
    // `best_state` already answers from the hook columns whenever they hold
    // anything, so this changes nothing about the derived status — it only
    // stops a pane verdict, cached before the hook onset and not yet evicted
    // by that onset's own poll, from being attached to a row that now speaks
    // for itself.
    match pane {
        Some(corroboration) if !assessment.reported => {
            assessment.with_corroboration(corroboration.clone())
        }
        _ => assessment,
    }
}

/// The registry the launcher itself uses, read once.
///
/// One read rather than three: the picker's rows, the agent a bare launch
/// preselects and the coverage every state answer is judged against are all
/// this one file, and `load_or_seed` seeds it when it is missing.
fn read_registry() -> std::sync::Arc<AgentRegistry> {
    std::sync::Arc::new(crate::agent::agent_config::load_or_seed())
}

/// Agents from the registry the launcher itself uses — so the flow can never
/// offer one that would fail to launch.
fn read_agents(registry: &AgentRegistry) -> Vec<AgentRow> {
    registry
        .agents
        .iter()
        .map(|agent| AgentRow {
            name: agent.name.clone(),
            command: agent.command.clone(),
            presence: crate::backend::preflight::look_up(&agent.command),
        })
        .collect()
}

/// Which multiplexers this machine can run sessions in, and the one a create
/// uses when nothing names one — what to do when there is none.
fn read_mux(
    served: &std::collections::HashSet<crate::session::Route>,
    configured: Option<String>,
) -> MuxRow {
    use crate::session::{Multiplexer, Route};
    let available: Vec<Multiplexer> = crate::backend::preflight::local_multiplexers()
        .into_iter()
        .filter(|mux| served.contains(&Route::local(Some(*mux))))
        .collect();
    // What a bare local create would run: the configured one, else the one
    // the picker preselects. Named here so the warning names it too.
    let binary = configured
        .as_deref()
        .and_then(|name| Multiplexer::parse(name).ok())
        .or_else(|| Multiplexer::preferred(crate::shell::local_platform(), &available))
        .unwrap_or_else(Multiplexer::platform_default);
    let presence = crate::backend::preflight::look_up(binary.name());
    MuxRow {
        binary: binary.name().to_string(),
        configured,
        available: available.iter().map(|mux| mux.name().to_string()).collect(),
        presence,
        advice: match presence {
            crate::backend::preflight::Presence::Present => String::new(),
            _ => crate::backend::preflight::Dependency::Multiplexer(binary.name()).fix(),
        },
    }
}

/// Configured and discovered hosts. Empty means local only, and the flow skips
/// asking.
fn read_hosts(
    served: &std::collections::HashSet<crate::session::Route>,
    probe: bool,
) -> Vec<HostRow> {
    let (registry, _warnings) = crate::config::hosts::cached_registry();
    hosts_from(&registry.hosts, served, probe)
}

/// Each host's row, offering the multiplexers its probe found there, and
/// starting that probe when `probe` allows it. The probe is never waited for:
/// until it answers, a host offers what
/// [`crate::session::Multiplexer::available_on`] trusts it with unseen.
fn hosts_from(
    hosts: &[crate::session::HostDef],
    served: &std::collections::HashSet<crate::session::Route>,
    probe: bool,
) -> Vec<HostRow> {
    hosts
        .iter()
        .map(|host| {
            let answer = crate::backend::host_path::multiplexers_found(host, probe);
            let found = match &answer {
                crate::backend::host_path::MuxProbe::Found(found) => Some(found.as_slice()),
                _ => None,
            };
            HostRow {
                name: host.name.clone(),
                detail: host.picker_detail(),
                backend: host.backend_name(),
                platform: host.platform().name().to_string(),
                multiplexer: host.multiplexer.clone(),
                available_multiplexers: crate::session::Multiplexer::available_on(
                    host.platform(),
                    found,
                    host.multiplexer(),
                )
                .into_iter()
                .filter(|mux| served.contains(&host.route(Some(*mux))))
                .map(|mux| mux.name().to_string())
                .collect(),
                probing: answer == crate::backend::host_path::MuxProbe::Pending,
            }
        })
        .collect()
}

/// A remote session's host name — `ssh:devbox:rmux` → `devbox` — whether or
/// not `hosts.toml` still describes it.
///
/// A session's machine is published to the interface bare while the host
/// picker carries the prefixed backend name, and the two have to answer as one
/// vocabulary: the session list groups rows by the former and
/// creations-in-flight by the latter, so a second spelling put a creation under
/// a machine that did not exist.
pub(super) fn remote_host_of(backend: &str) -> Option<String> {
    crate::session::Route::parse(backend)
        .ok()?
        .host()
        .map(str::to_string)
}

/// Best-effort repo label: the worktree's repo directory name, else the cwd's.
///
/// Shared with the reorder command so a move stays inside the group the list
/// actually draws.
/// Every repository a session spans, in member order — mirroring v1's
/// `session_member_dirs`: one entry per worktree (or the cwd when there are
/// none), then each attached directory that is not already a worktree.
///
/// Names come from the path's last component rather than `git::repo_display_name`
/// deliberately: that helper resolves the name from the git *remote*, which
/// shells out on a cache miss, and this runs on the loop thread.
fn session_members(
    cwd: Option<&std::path::Path>,
    worktrees: &[crate::session::SharedWorktree],
    additional_dirs: &[PathBuf],
) -> Vec<(Option<String>, PathBuf)> {
    let leaf = |path: &std::path::Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
    };
    let mut members: Vec<(Option<String>, PathBuf)> = Vec::new();
    if worktrees.is_empty() {
        // The name comes from the repository, the directory from the checkout —
        // for a worktree those differ, and each half is wanted by a different
        // caller (the group label, the editor).
        members.extend(cwd.map(|path| (leaf(path), path.to_path_buf())));
    } else {
        members.extend(
            worktrees
                .iter()
                .map(|wt| (leaf(&wt.repo_path), wt.worktree_path.clone())),
        );
    }
    let worktree_paths: std::collections::HashSet<&std::path::Path> = worktrees
        .iter()
        .map(|wt| wt.worktree_path.as_path())
        .collect();
    for dir in additional_dirs {
        if !worktree_paths.contains(dir.as_path()) {
            members.push((leaf(dir), dir.clone()));
        }
    }
    members
}

pub(crate) fn repo_name(cwd: &Option<PathBuf>, repo_path: Option<&PathBuf>) -> Option<String> {
    repo_path
        .or(cwd.as_ref())
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
}

/// The instant a snapshot was taken, to the second.
///
/// Deliberately coarser than the clock. `taken_at_ms` is published, and its only
/// reader is `widgets.now_ms` feeding `time_ago`, which floors the difference to
/// whole seconds — so sub-second precision is unobservable to the interface,
/// while re-stamping it moved [`SnapshotStore::version`] 2.5 times a second
/// and capped how long any pure pane could stay cached (`frame-cost`).
///
/// Quantising the published *value* rather than delaying the signal is what
/// keeps "a plugin never reads a stale published value" true: the field means
/// "when these rows were read, to the second", and it is never behind that.
fn taken_at_stamp() -> i64 {
    now_ms() / 1000 * 1000
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse a session id the way a plugin would have written it.
pub fn parse_id(raw: &str) -> Option<SessionId> {
    raw.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::probe::Probe;
    use super::*;

    #[test]
    fn a_remote_backend_yields_its_host_name() {
        assert_eq!(remote_host_of("ssh:devbox").as_deref(), Some("devbox"));
        assert_eq!(remote_host_of("wsl:Ubuntu").as_deref(), Some("Ubuntu"));
        assert_eq!(remote_host_of("local-tmux"), None);
    }

    #[test]
    fn a_repo_label_prefers_the_worktree_repo() {
        let cwd = Some(PathBuf::from("/tmp/worktrees/feature-x"));
        let repo = PathBuf::from("/home/me/src/thurbox");
        assert_eq!(repo_name(&cwd, Some(&repo)).as_deref(), Some("thurbox"));
        assert_eq!(repo_name(&cwd, None).as_deref(), Some("feature-x"));
        assert_eq!(repo_name(&None, None), None);
    }

    #[test]
    fn the_preflight_answer_is_cached_rather_than_probed_on_every_tick() {
        // The cost model of the whole feature. `refresh_if_due` runs on the
        // event loop, so a probe there is a `PATH` walk per tick; the answer
        // must come from the window instead. Removing the binary and finding
        // the store still says `Present` is what proves nothing looked again.
        let dir = tempfile::tempdir().expect("tempdir");
        let mux = dir
            .path()
            .join(crate::backend::preflight::local_multiplexer());
        std::fs::write(&mux, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        }

        crate::paths::with_path(dir.path(), || {
            let database = Database::open_in_memory().expect("in-memory database opens");
            let mut store = SnapshotStore::with_database(
                database,
                &crate::backend::registry::inert(),
                &Settings::default(),
            );
            assert_eq!(
                store.current().mux.presence,
                crate::backend::preflight::Presence::Present,
                "the probe at construction must find the binary that is there"
            );

            std::fs::remove_file(&mux).expect("remove");
            // Each tick has to clear REFRESH_INTERVAL, or `refresh_if_due`
            // returns before reaching the probe at all and the assertion below
            // would hold whether or not the window exists. Well inside
            // PREFLIGHT_TTL, which is what is being pinned.
            for _ in 0..3 {
                std::thread::sleep(REFRESH_INTERVAL + Duration::from_millis(50));
                store.refresh_if_due();
            }
            assert_eq!(
                store.current().mux.presence,
                crate::backend::preflight::Presence::Present,
                "the answer moved inside the TTL, so something probed on the tick"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn the_local_picker_offers_only_the_multiplexers_installed_here() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let install = |name: &str| {
            let binary = dir.path().join(name);
            std::fs::write(&binary, "#!/bin/sh\n").expect("write stand-in");
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))
                .expect("chmod");
        };
        let served = [
            crate::session::Route::local(Some(crate::session::Multiplexer::Tmux)),
            crate::session::Route::local(Some(crate::session::Multiplexer::Psmux)),
            crate::session::Route::local(Some(crate::session::Multiplexer::Rmux)),
        ]
        .into_iter()
        .collect();
        crate::paths::with_path(dir.path(), || {
            let none = read_mux(&served, None);
            assert!(none.available.is_empty(), "{none:?}");
            assert_eq!(none.presence, crate::backend::preflight::Presence::Missing);

            install("rmux");
            let only_rmux = read_mux(&served, None);
            assert_eq!(only_rmux.available, vec!["rmux"]);
            assert_eq!(only_rmux.binary, "rmux", "the one installed is the default");
            assert_eq!(
                only_rmux.presence,
                crate::backend::preflight::Presence::Present
            );

            // psmux is native Windows: a binary of that name on a POSIX PATH
            // does not make it a choice here.
            install("psmux");
            install("tmux");
            let both = read_mux(&served, None);
            assert_eq!(both.available, vec!["tmux", "rmux"]);
            assert_eq!(both.binary, "tmux");
        });
    }

    #[test]
    fn a_host_offers_what_its_probe_found_and_never_waits_for_one() {
        use crate::session::{HostDef, Multiplexer};
        let probed = HostDef {
            name: "probed-host".into(),
            ..Default::default()
        };
        let pending = HostDef {
            name: "pending-host".into(),
            ..Default::default()
        };
        crate::backend::host_path::seed_multiplexers(&probed, Some(vec![Multiplexer::Rmux]));
        let served: std::collections::HashSet<_> = [&probed, &pending]
            .into_iter()
            .flat_map(|host| Multiplexer::ALL.map(|mux| host.route(Some(mux))))
            .collect();
        let rows = hosts_from(&[probed, pending], &served, false);
        assert_eq!(rows[0].available_multiplexers, vec!["rmux"]);
        assert_eq!(
            rows[1].available_multiplexers,
            vec!["tmux"],
            "a host not looked at yet offers its platform default"
        );
        assert!(!rows[0].probing && rows[1].probing);
    }

    /// The automation rows carry the definition and its timing, not only a
    /// summary — a pane cannot edit what it cannot read.
    #[test]
    fn an_automation_row_carries_its_definition_and_timing() {
        use crate::session::automation::{
            AutomationAction, AutomationRunStatus, AutomationSchedule,
        };
        use crate::storage::automations::NewAutomation;

        let database = Database::open_in_memory().expect("in-memory database opens");
        let id = database
            .create_automation(&NewAutomation {
                owner_session_id: None,
                name: "sync".into(),
                enabled: true,
                schedule: AutomationSchedule::Once {
                    at: 4_000_000_000_000,
                },
                timezone: Some("UTC".into()),
                action: AutomationAction::Exec {
                    command: "make sync".into(),
                },
                prompt: String::new(),
                next_run_at: Some(4_000_000_000_000),
                key: None,
                run: Default::default(),
            })
            .expect("insert");
        database
            .record_automation_run(id, AutomationRunStatus::Error, "exit 2: boom", None)
            .expect("run");
        let store = SnapshotStore::with_database(
            database,
            &crate::backend::registry::inert(),
            &Settings::default(),
        );
        let row = &store.current().automations[0];
        assert_eq!(row.schedule, "once", "the old summary is unchanged");
        assert_eq!(row.trigger, "at:4000000000000");
        assert_eq!(row.timezone.as_deref(), Some("UTC"));
        assert_eq!(row.command.as_deref(), Some("make sync"));
        assert_eq!(row.repo, None);
        assert_eq!(row.next_run_at, Some(4_000_000_000_000));
        assert!(row.created_at > 0 && row.updated_at >= row.created_at);
        assert_eq!(row.runs.len(), 1);
        assert!(row.runs[0].id > 0);
        assert_eq!(row.runs[0].status, "error");
        assert_eq!(row.runs[0].detail, "exit 2: boom");
    }

    #[test]
    fn an_in_memory_database_yields_an_empty_but_stamped_snapshot() {
        let database = Database::open_in_memory().expect("in-memory database opens");
        let store = SnapshotStore::with_database(
            database,
            &crate::backend::registry::inert(),
            &Settings::default(),
        );
        let snapshot = store.current();
        assert!(snapshot.sessions.is_empty());
        assert!(snapshot.taken_at_ms > 0, "snapshot must carry its instant");
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
    }

    #[test]
    fn a_read_is_immediate_and_repeatable() {
        let database = Database::open_in_memory().expect("in-memory database opens");
        let store = SnapshotStore::with_database(
            database,
            &crate::backend::registry::inert(),
            &Settings::default(),
        );
        // The property that matters: reading never consults the database.
        for _ in 0..1000 {
            assert!(store.current().sessions.is_empty());
        }
    }

    #[test]
    fn a_single_repo_session_has_one_member_at_its_cwd() {
        let members = session_members(Some(std::path::Path::new("/src/thurbox")), &[], &[]);
        assert_eq!(
            members,
            vec![(Some("thurbox".to_string()), PathBuf::from("/src/thurbox"))]
        );
    }

    #[test]
    fn a_member_is_named_for_its_repository_but_points_at_its_checkout() {
        // The distinction the editor depends on: opening the repository root
        // would land on whatever branch that has, not the one being worked.
        let worktrees = vec![crate::session::SharedWorktree {
            repo_path: PathBuf::from("/src/thurbox"),
            worktree_path: PathBuf::from("/worktrees/fix-osc52"),
            branch: "fix/osc52".into(),
            created_by_thurbox: true,
        }];
        let members = session_members(Some(std::path::Path::new("/src/thurbox")), &worktrees, &[]);
        assert_eq!(
            members,
            vec![(
                Some("thurbox".to_string()),
                PathBuf::from("/worktrees/fix-osc52")
            )]
        );
    }

    #[test]
    fn every_repository_of_a_multi_repo_session_is_a_member() {
        let worktrees = vec![
            crate::session::SharedWorktree {
                repo_path: PathBuf::from("/src/a"),
                worktree_path: PathBuf::from("/worktrees/a"),
                branch: "feat/x".into(),
                created_by_thurbox: true,
            },
            crate::session::SharedWorktree {
                repo_path: PathBuf::from("/src/b"),
                worktree_path: PathBuf::from("/worktrees/b"),
                branch: "feat/x".into(),
                created_by_thurbox: true,
            },
        ];
        // An attached directory that is already a worktree must not appear twice.
        let extra = vec![PathBuf::from("/worktrees/a"), PathBuf::from("/reference")];
        let members = session_members(None, &worktrees, &extra);
        let dirs: Vec<PathBuf> = members.into_iter().map(|(_, path)| path).collect();
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/worktrees/a"),
                PathBuf::from("/worktrees/b"),
                PathBuf::from("/reference"),
            ]
        );
    }

    /// Seed an in-memory store with one session whose pane probe already found
    /// a foreign agent, so `detected_agent` is published for it — the
    /// "firstmate" scenario: a bare shell with a real agent running in it.
    fn store_with_a_detected_agent() -> (SnapshotStore, crate::session::SharedSession) {
        let database = Database::open_in_memory().expect("in-memory database opens");
        let row = crate::session::SharedSession {
            id: SessionId::default(),
            name: "shell".into(),
            agent: "zsh".into(),
            backend_id: "%7".into(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        };
        database.upsert_session(&row).expect("upsert");
        let mut store = SnapshotStore::with_database(
            database,
            &crate::backend::registry::inert(),
            &Settings::default(),
        );
        store.panes.known.insert(
            row.id.to_string(),
            Probe {
                corroboration: Corroboration::ForeignAgent(Some("claude".into())),
                at: Instant::now(),
            },
        );
        store.refresh();
        let published = store
            .current()
            .sessions
            .iter()
            .find(|s| s.id == row.id.to_string())
            .expect("row published");
        assert_eq!(published.detected_agent.as_deref(), Some("claude"));
        assert!(published.hook_state.is_none());
        (store, row)
    }

    #[test]
    fn a_hook_report_clears_its_row_detected_agent_at_once() {
        let (mut store, row) = store_with_a_detected_agent();

        // The same pane starts delivering real hook events — through the exact
        // entry point production code uses, with nothing else run in between.
        store.apply_hook_states(
            // Named by the route that serves the row, as a backend reports.
            vec![(
                crate::session_ops::server_key("local-tmux"),
                "%7".to_string(),
                "working".to_string(),
            )],
            Instant::now(),
        );

        let published = store
            .current()
            .sessions
            .iter()
            .find(|s| s.id == row.id.to_string())
            .expect("row published");
        assert_eq!(published.hook_state.as_deref(), Some("working"));
        assert_eq!(
            published.detected_agent, None,
            "a pane verdict published before the hook onset must not survive it"
        );
    }

    #[test]
    fn a_hook_report_evicts_a_stale_pane_probe() {
        let (mut store, row) = store_with_a_detected_agent();

        store.apply_hook_states(
            // Named by the route that serves the row, as a backend reports.
            vec![(
                crate::session_ops::server_key("local-tmux"),
                "%7".to_string(),
                "working".to_string(),
            )],
            Instant::now(),
        );
        store.poll_pane_probes();

        assert_eq!(
            store.panes.get(&row.id.to_string()),
            None,
            "a row that now reports its own hook state must leave the probed cache"
        );
    }

    /// The captain's report, end to end on the surface he was looking at: a
    /// session reading `blocked` long after its agent finished.
    ///
    /// Driven through the store rather than the pure fold because the part that
    /// can be wrong is that the tick pass reaches `blocked` rows at all — it
    /// used to filter to `working` and nothing else, so no amount of evidence
    /// could retire a latched block.
    #[test]
    fn a_block_the_pane_printed_past_stops_being_drawn() {
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        let database = Database::open(temp.path()).expect("open store connection");
        let row = crate::session::SharedSession {
            id: SessionId::default(),
            name: "crewmate".into(),
            agent: "zsh".into(),
            backend_id: "%7".into(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        };
        database.upsert_session(&row).expect("upsert");
        database.set_hook_state(row.id, "blocked").expect("signal");
        let mut store = SnapshotStore::with_database(
            database,
            &crate::backend::registry::inert(),
            &Settings::default(),
        );
        store.refresh();
        let id = row.id.to_string();
        assert_eq!(status_of(&store, &id), SessionState::Blocked);

        // A fresh block is quiet from its own edge, so nothing here moves it —
        // however long the operator leaves it standing.
        assert_eq!(store.apply_output_quiescence(|_| Some(60_000)), 0);
        assert_eq!(status_of(&store, &id), SessionState::Blocked);

        // Age the edge to what the captain saw: stamped 2547s ago, and the
        // pane printed all the way to 17s ago.
        let stamped = crate::storage::current_time_millis() as i64 - 2_547_000;
        store.hook_state_at.insert(id.clone(), stamped);

        assert_eq!(store.apply_output_quiescence(|_| Some(17_000)), 1);
        assert_eq!(
            status_of(&store, &id),
            SessionState::Idle,
            "an agent that printed for 42 minutes after the block edge was not waiting on anyone"
        );
        // The stored column is never touched: the fold is read-time, and the
        // agent's own last word stays readable.
        assert_eq!(
            store
                .current()
                .sessions
                .iter()
                .find(|s| s.id == id)
                .expect("row published")
                .hook_state
                .as_deref(),
            Some("blocked")
        );

        // Reversible, like the `working` half: printing again is a turn.
        assert_eq!(store.apply_output_quiescence(|_| Some(0)), 1);
        assert_eq!(status_of(&store, &id), SessionState::Working);
    }

    fn status_of(store: &SnapshotStore, id: &str) -> SessionState {
        store
            .current()
            .sessions
            .iter()
            .find(|s| s.id == id)
            .expect("row published")
            .status
    }

    #[test]
    fn codex_input_retires_only_the_report_it_observed() {
        codex_input_retires_report("codex");
    }

    #[test]
    fn inherited_codex_input_retires_only_the_report_it_observed() {
        codex_input_retires_report("codex-variant");
    }

    fn codex_input_retires_report(agent: &str) {
        // A file, so the test reaches it through a connection of its own: the
        // store's lives on its worker thread.
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        let database = Database::open(temp.path()).expect("db");
        let driver = Database::open(temp.path()).expect("second connection");
        let row = crate::session::SharedSession {
            id: SessionId::default(),
            name: "codex-pane".into(),
            agent: agent.into(),
            backend_id: "%7".into(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        };
        database.upsert_session(&row).expect("persist");
        database.set_hook_state(row.id, "idle").expect("idle");
        let mut store = SnapshotStore::with_database(
            database,
            &crate::backend::registry::inert(),
            &Settings::default(),
        );
        let (registry, warnings) = crate::agent::agent_config::parse_for_reload(
            "[[agents]]\nname='codex'\ncommand='codex'\n[[agents]]\nname='codex-variant'\nextends='codex'\n",
        )
        .expect("registry");
        assert!(warnings.is_empty(), "{warnings:?}");
        store.registry = std::sync::Arc::new(registry);
        let id = row.id.to_string();
        assert_eq!(status_of(&store, &id), SessionState::Idle);

        let old = store.codex_submission_report(&id).expect("old report");
        store.note_codex_submission(&id, &old);
        assert!(driver
            .clear_hook_state_if_unchanged(row.id, &old)
            .expect("retire report"));
        assert_eq!(status_of(&store, &id), SessionState::Running);
        assert_eq!(store.current().session(&id).unwrap().hook_state, None);

        driver.set_hook_state(row.id, "idle").expect("idle again");
        store.refresh();
        let old = store.codex_submission_report(&id).expect("second report");
        driver
            .set_hook_state(row.id, "working")
            .expect("prompt hook");
        store.note_codex_submission(&id, &old);
        assert!(!driver
            .clear_hook_state_if_unchanged(row.id, &old)
            .expect("newer report wins"));
        assert_eq!(
            driver
                .load_hook_state(row.id)
                .unwrap()
                .unwrap()
                .state
                .as_deref(),
            Some("working")
        );
    }

    #[test]
    fn remote_codex_report_uses_its_persisted_timestamp_for_retirement() {
        // A file, so the test reaches it through a connection of its own: the
        // store's lives on its worker thread.
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        let database = Database::open(temp.path()).expect("db");
        let driver = Database::open(temp.path()).expect("second connection");
        let row = crate::session::SharedSession {
            id: SessionId::default(),
            name: "remote-codex".into(),
            agent: "codex".into(),
            backend_id: "%7".into(),
            backend_type: "ssh:fixture".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        };
        database.upsert_session(&row).expect("persist");
        database.set_hook_state(row.id, "working").expect("working");
        // A future stamp also models repeated reports within one millisecond:
        // set_hook_state keeps each stamp distinct even when the clock has not moved.
        let future = crate::storage::current_time_millis() as i64 + 60_000;
        database
            .conn_ref()
            .execute(
                "UPDATE sessions SET hook_state_at = ?1 WHERE id = ?2",
                rusqlite::params![future, row.id.to_string()],
            )
            .expect("seed later stamp");
        let mut store = SnapshotStore::with_database(
            database,
            &crate::backend::registry::inert(),
            &Settings::default(),
        );
        assert_eq!(
            store.apply_hook_states(
                vec![("ssh:fixture:tmux".into(), "%7".into(), "idle".into())],
                Instant::now(),
            ),
            1
        );
        // Enter lands before the worker has answered with the stamp it
        // stored, so the report carries the store's own provisional one.
        let id = row.id.to_string();
        let old = store.codex_submission_report(&id).expect("cached report");
        store.note_codex_submission(&id, &old);
        store.retire_codex_report(&id, &old);
        store.settle();
        assert_eq!(
            driver.load_hook_state(row.id).unwrap().unwrap().state,
            None,
            "the report Enter superseded survived its retirement"
        );
        // Nor may the hook write's late answer show it again.
        assert_eq!(store.current().session(&id).unwrap().hook_state, None);
        assert_eq!(status_of(&store, &id), SessionState::Running);
    }

    /// The local path: an agent's own `thurbox-cli session signal` writes the
    /// hook state through a second connection, never through
    /// `apply_hook_states`, so only `refresh_if_due`'s normal DB-read cadence
    /// ever sees it. Driven exactly as the coordinator drives it — no
    /// hand-rolled `poll_pane_probes`/`refresh` call — because the ordering
    /// between that cadence and the pane-probe cache is the thing under test.
    #[test]
    fn a_local_hook_report_does_not_resurrect_a_stale_detected_agent() {
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        let path = temp.path();

        let database = Database::open(path).expect("open store connection");
        let row = crate::session::SharedSession {
            id: SessionId::default(),
            name: "shell".into(),
            agent: "zsh".into(),
            backend_id: "%7".into(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        };
        database.upsert_session(&row).expect("upsert");
        let mut store = SnapshotStore::with_database(
            database,
            &crate::backend::registry::inert(),
            &Settings::default(),
        );

        store.panes.known.insert(
            row.id.to_string(),
            Probe {
                corroboration: Corroboration::ForeignAgent(Some("claude".into())),
                at: Instant::now(),
            },
        );
        store.refresh();
        let published = store
            .current()
            .sessions
            .iter()
            .find(|s| s.id == row.id.to_string())
            .expect("row published");
        assert_eq!(published.detected_agent.as_deref(), Some("claude"));

        let driver = Database::open(path).expect("open a second connection");
        driver
            .set_hook_state(row.id, "working")
            .expect("write the hook state through the other connection");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            store.refresh_if_due();
            let seen = store
                .current()
                .sessions
                .iter()
                .find(|s| s.id == row.id.to_string())
                .expect("row published")
                .hook_state
                .clone();
            if seen.as_deref() == Some("working") || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let published = store
            .current()
            .sessions
            .iter()
            .find(|s| s.id == row.id.to_string())
            .expect("row published");
        assert_eq!(published.hook_state.as_deref(), Some("working"));
        assert_eq!(
            published.detected_agent, None,
            "a pane verdict cached before a hook onset seen through another \
             connection must not publish once the row reports for itself"
        );
    }
    /// A store over a file with one local session whose turn is an unseen
    /// `done`, and a second connection to the same file.
    fn store_with_a_finished_turn() -> (tempfile::NamedTempFile, Database, SnapshotStore, String) {
        let (temp, driver, store, id) = store_with_one_session("local-tmux", "done");
        assert_eq!(status_of(&store, &id), SessionState::Done);
        (temp, driver, store, id)
    }

    /// A store over a file holding one session on `backend` at hook `state`,
    /// and a second connection to the same file.
    fn store_with_one_session(
        backend: &str,
        state: &str,
    ) -> (tempfile::NamedTempFile, Database, SnapshotStore, String) {
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        let database = Database::open(temp.path()).expect("db");
        let row = crate::session::SharedSession {
            id: SessionId::default(),
            name: "finished".into(),
            agent: "claude".into(),
            backend_id: "%7".into(),
            backend_type: backend.into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        };
        database.upsert_session(&row).expect("persist");
        database.set_hook_state(row.id, state).expect("hook state");
        let driver = Database::open(temp.path()).expect("second connection");
        let store = SnapshotStore::with_database(
            database,
            &crate::backend::registry::inert(),
            &Settings::default(),
        );
        (temp, driver, store, row.id.to_string())
    }

    /// Hold `driver`'s write lock while `queue` runs, behind a perf write that
    /// waits on it, then commit `meanwhile` and let the queue through: the
    /// writes `queue` sent run against what `meanwhile` committed.
    fn race(
        store: &mut SnapshotStore,
        driver: &Database,
        queue: impl FnOnce(&mut SnapshotStore),
        meanwhile: &str,
        id: &str,
    ) {
        driver.conn_ref().execute_batch("BEGIN IMMEDIATE").unwrap();
        store.set_perf_snapshot("{}");
        queue(store);
        driver.conn_ref().execute(meanwhile, [id]).unwrap();
        driver.conn_ref().execute_batch("COMMIT").unwrap();
        store.settle();
        store.settle();
    }

    /// A theme is chosen while another process holds the write lock. The
    /// store queues it rather than waiting, and it lands once the lock goes.
    #[test]
    fn a_theme_chosen_under_a_held_lock_is_stored_once_it_is_released() {
        let (_temp, driver, mut store, _id) = store_with_one_session("ssh:fixture", "working");
        driver.conn_ref().execute_batch("BEGIN IMMEDIATE").unwrap();
        store.persist_theme("nord");
        driver.conn_ref().execute_batch("COMMIT").unwrap();
        store.settle();
        assert_eq!(driver.get_active_theme().unwrap().as_deref(), Some("nord"));
    }

    #[test]
    fn an_editor_request_is_answered_once_under_a_held_lock() {
        let (_temp, driver, mut store, id) = store_with_one_session("ssh:fixture", "working");
        driver.set_editor_command("nvim -p").unwrap();
        driver
            .set_editor_mode(crate::session::settings::EditorMode::Terminal)
            .unwrap();
        // Under a held write lock: the read must neither wait for it nor queue
        // behind a write that does.
        driver.conn_ref().execute_batch("BEGIN IMMEDIATE").unwrap();
        store.set_perf_snapshot("{}");
        store.request_editor(&id);
        let deadline = Instant::now() + Duration::from_secs(2);
        let answer = loop {
            if let Some(answer) = store.take_editor() {
                break Some(answer);
            }
            if Instant::now() > deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        driver.conn_ref().execute_batch("COMMIT").unwrap();
        assert_eq!(
            answer,
            Some(EditorRequest {
                session: id,
                command: Some("nvim -p".into()),
                mode: crate::session::settings::EditorMode::Terminal,
            })
        );
        assert_eq!(store.take_editor(), None, "handed out once");
    }

    /// The store decided to write this remote report because the row showed
    /// another state. By the time it runs, another writer has stored the same
    /// state and the user has acknowledged it, so writing it again would
    /// re-stamp the turn and bring back a dot the user already retired.
    #[test]
    fn a_queued_hook_report_is_decided_again_against_what_storage_now_holds() {
        let (_temp, driver, mut store, id) = store_with_one_session("ssh:fixture", "working");
        race(
            &mut store,
            &driver,
            |store| {
                store.apply_hook_states(
                    vec![("ssh:fixture:tmux".into(), "%7".into(), "done".into())],
                    Instant::now(),
                );
            },
            "UPDATE sessions SET hook_state = 'done', hook_state_at = 5, seen_at = 5 WHERE id = ?1",
            &id,
        );
        let hook = driver
            .load_hook_state(id.parse().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            (hook.state.as_deref(), hook.state_at, hook.seen_at),
            (Some("done"), Some(5), Some(5)),
            "an acknowledged turn was re-stamped unseen"
        );
    }

    #[test]
    fn a_queued_shell_pane_is_dropped_when_the_row_moved_on() {
        let (_temp, driver, mut store, id) = store_with_one_session("local-tmux", "idle");
        race(
            &mut store,
            &driver,
            |store| store.remember_shell(&id, "%9").unwrap(),
            "UPDATE sessions SET shell_backend_id = '%5' WHERE id = ?1",
            &id,
        );
        let stored = driver
            .get_session_by_id(id.parse().unwrap())
            .unwrap()
            .unwrap()
            .shell_backend_id;
        assert_eq!(
            stored.as_deref(),
            Some("%5"),
            "a stale shell pane overwrote a newer one"
        );
        assert_eq!(
            store
                .current()
                .session(&id)
                .unwrap()
                .shell_backend_id
                .as_deref(),
            Some("%5"),
            "the rows were not read again after the write was dropped"
        );
    }

    #[test]
    fn a_queued_agent_pane_is_dropped_when_the_row_moved_on() {
        let (_temp, driver, mut store, id) = store_with_one_session("local-tmux", "idle");
        race(
            &mut store,
            &driver,
            |store| store.remember_pane(&id, "%8").unwrap(),
            "UPDATE sessions SET backend_id = '%9' WHERE id = ?1",
            &id,
        );
        let stored = driver
            .get_session_by_id(id.parse().unwrap())
            .unwrap()
            .unwrap()
            .backend_id;
        assert_eq!(
            stored, "%9",
            "a stale adopted pane overwrote a restart's new one"
        );
    }

    /// The read is queued first, so the worker answers it from the row as it
    /// stood before the acknowledgment. Applying that answer would put the
    /// filled dot back, and nothing would read the row again: the write is
    /// this store's own, so `data_version` never moves for it.
    #[test]
    fn a_read_answered_before_a_queued_write_does_not_undo_its_correction() {
        let (_temp, driver, mut store, id) = store_with_a_finished_turn();

        store.request_refresh();
        store.acknowledge(&id);
        assert_eq!(status_of(&store, &id), SessionState::Idle);
        store.settle();
        store.settle();

        assert_eq!(status_of(&store, &id), SessionState::Idle);
        let hook = driver
            .load_hook_state(id.parse().unwrap())
            .unwrap()
            .unwrap();
        assert!(hook.seen_at >= hook.state_at, "the acknowledgment landed");
    }

    /// A turn that finishes between leaving the session and the worker reaching
    /// the queued acknowledgment is one nobody has seen. The perf write ahead
    /// of it waits on the lock, which is what holds the acknowledgment back
    /// until that turn has committed.
    #[test]
    fn an_acknowledgment_queued_behind_a_lock_leaves_a_newer_turn_unseen() {
        let (_temp, driver, mut store, id) = store_with_a_finished_turn();
        let session = id.parse().unwrap();
        let first = driver.load_hook_state(session).unwrap().unwrap().state_at;

        race(
            &mut store,
            &driver,
            |store| store.acknowledge(&id),
            "UPDATE sessions SET hook_state_at = hook_state_at + 1000 WHERE id = ?1",
            &id,
        );

        let hook = driver.load_hook_state(session).unwrap().unwrap();
        assert!(hook.state_at > first);
        assert!(
            hook.seen_at
                .is_none_or(|seen| seen < hook.state_at.unwrap()),
            "a turn that finished after the user left was marked seen"
        );
    }

    #[test]
    fn dropping_the_store_lands_the_writes_still_queued() {
        let (_temp, driver, mut store, id) = store_with_a_finished_turn();

        store.acknowledge(&id);
        drop(store);

        let hook = driver
            .load_hook_state(id.parse().unwrap())
            .unwrap()
            .unwrap();
        assert!(
            hook.seen_at.is_some() && hook.seen_at >= hook.state_at,
            "an acknowledgment queued before a clean exit was lost"
        );
    }

    #[test]
    fn idle_clock_does_not_invalidate_snapshot_rows() {
        let database = Database::open_in_memory().unwrap();
        let mut store = SnapshotStore::with_database(
            database,
            &crate::backend::registry::inert(),
            &Settings::default(),
        );
        store.refresh();
        let version = store.version();
        store.current.taken_at_ms = 0;
        store.last_refresh = Some(Instant::now() - REFRESH_INTERVAL);
        store.refresh_if_due();
        assert!(store.current.taken_at_ms > 0);
        assert_eq!(
            store.version(),
            version,
            "the clock alone invalidated every snapshot reader"
        );
    }
}
