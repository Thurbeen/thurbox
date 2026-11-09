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

pub mod fleet;
mod git;

use crate::session::settings::Settings;
use crate::session::{AgentRegistry, SessionId, SessionState};
use git::{git_poll_interval, GitStats};

/// How often the snapshot is rebuilt. A read never waits on this; it only
/// determines how out of date the answer may be.
pub const REFRESH_INTERVAL: Duration = Duration::from_millis(400);

/// How long a refresh asked of a [`SnapshotSource`] waits for its answer.
/// A source speaks only when something moved, so after a command that moved
/// nothing no answer comes: past this, the rows stand as fresh. `fleet
/// watch` reads the store every 500 ms, so a write is seen well within it.
const SOURCE_SETTLE: Duration = Duration::from_secs(1);

/// How long a refresh awaiting a created session waits for its row: a
/// creation Fleet refused late, or a watch that is down, must not hold the
/// creation's report for good.
const SOURCE_AWAIT: Duration = Duration::from_secs(10);

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
    /// The raw persisted hook state, before [`crate::session::derive_state`] interprets it.
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

impl SessionRow {
    /// Whether this machine reaches the session's directories at all: its
    /// own, or a host thurbox's routes reach. A Fleet Task on another host
    /// (`remote_host` set) names a path there, which no route of thurbox's
    /// reaches, so its files, diff and git state are not read here, even
    /// where the same path exists locally.
    pub fn workspace_reachable(&self) -> bool {
        self.backend != fleet::BACKEND || self.remote_host.is_none()
    }

    /// Whether the session's directories are on this machine, so its own
    /// filesystem reads them: reachable, and on no remote route.
    pub fn workspace_on_this_machine(&self) -> bool {
        self.workspace_reachable() && !crate::session::Route::is_remote_key(&self.backend)
    }
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
    /// be, carried so the restore surface asks exactly when the kernel would
    /// object.
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

/// Where a store's session rows come from: Fleet's Tasks, through
/// [`fleet::FleetWatch`] (ADR-41).
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

/// Owns the snapshot and decides when to rebuild it.
///
/// Refresh happens on the kernel's schedule, never inside a plugin call, and
/// the source reads on a worker of its own, so a slow source shows up as stale
/// rows, never as a loop that stops painting.
pub struct SnapshotStore {
    /// Where the session rows come from; see [`SnapshotSource`]. `None` is a
    /// store with no sessions (tests of the other rows).
    source: Option<Box<dyn SnapshotSource>>,
    /// Answers applied from the source, and when a refresh was last asked of
    /// it: what [`Self::request_refresh`]'s ticket counts.
    source_reads: u64,
    source_asked: Instant,
    /// The names of the sessions the last refresh awaits: see
    /// [`Self::request_refresh_awaiting`].
    awaited: Vec<String>,
    git: GitStats,
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
    /// Moves whenever anything in [`Self::current`] does.
    ///
    /// What gates the published tables and the pure-pane tree cache: a reader
    /// that saw this value and sees it again knows nothing it could read has
    /// changed. It is bumped **inside** each mutation rather than by callers,
    /// because a mutation that forgets to bump is the one failure here that is
    /// silent — see `tests/kernel_frame_cost.rs`.
    version: u64,
}

impl SnapshotStore {
    /// Build a store whose session rows come from `source`.
    pub fn with_source(
        source: Box<dyn SnapshotSource>,
        backends: &crate::backend::BackendRegistry,
        settings: &Settings,
    ) -> Self {
        let mut store = Self::build(backends, settings);
        store.source = Some(source);
        store.poll_source();
        store
    }

    /// A store with no session source: the agents, hosts and multiplexer rows
    /// alone.
    pub fn without_sessions(
        backends: &crate::backend::BackendRegistry,
        settings: &Settings,
    ) -> Self {
        Self::build(backends, settings)
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

    fn build(backends: &crate::backend::BackendRegistry, settings: &Settings) -> Self {
        let registry = read_registry();
        crate::agent::agent_config::publish_registry(&registry);
        let served: std::collections::HashSet<_> = backends.routes().cloned().collect();
        let agents = read_agents(&registry);
        let agent_default = registry.default_name().to_string();
        let hosts = read_hosts(&served, false);
        let mux = read_mux(&served, settings.multiplexer.clone());
        Self {
            source: None,
            source_reads: 0,
            source_asked: Instant::now(),
            awaited: Vec::new(),
            git: GitStats::new(git_poll_interval(settings.git_poll_secs)),
            current: Snapshot {
                agents: agents.clone(),
                agent_default: agent_default.clone(),
                hosts: hosts.clone(),
                mux: mux.clone(),
                taken_at_ms: taken_at_stamp(),
                ..Snapshot::default()
            },
            agents,
            agent_default,
            registry,
            registry_contents: None,
            registry_polled_at: None,
            hosts,
            probe_hosts: false,
            mux,
            configured_mux: settings.multiplexer.clone(),
            served,
            preflight_at: Instant::now(),
            last_refresh: None,
            version: 0,
        }
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

    /// Apply what the source has answered and, when the refresh interval has
    /// elapsed, fold in the git stats and the preflight answers. Called from
    /// the event loop, never from a plugin. Returns whether rows from the
    /// source were applied by this call.
    pub fn refresh_if_due(&mut self) -> bool {
        self.refresh_if_due_at(Instant::now())
    }

    /// Poll using the caller's monotonic clock.
    pub fn refresh_if_due_at(&mut self, now: Instant) -> bool {
        let applied = self.poll_source();
        let due = self
            .last_refresh
            .is_none_or(|at| now.saturating_duration_since(at) >= REFRESH_INTERVAL);
        if !due {
            return applied;
        }
        self.last_refresh = Some(now);
        let preflight_moved = self.poll_preflight() | self.poll_hosts();
        self.current.taken_at_ms = taken_at_stamp();
        // Git stats landing rewrite rows here, so this changes more than the
        // timestamp, and the snapshot moved if they did.
        let git_moved = self.attach_git_stats();
        if git_moved || preflight_moved {
            self.mark_changed();
        }
        applied
    }

    /// [`Self::request_refresh`], for commands that created sessions named
    /// `created`. The ticket is fresh once a row of each name is published,
    /// or 10 seconds after the request: a source answers when it sees a
    /// write, which can be later than the second a quiet source is given.
    pub fn request_refresh_awaiting(&mut self, created: Vec<String>) -> u64 {
        let ticket = self.request_refresh();
        self.awaited = created;
        ticket
    }

    /// Ask for every row now, without waiting for the interval. Returns a
    /// ticket [`Self::fresh_since`] accepts once the source has answered
    /// since, or a second has passed.
    pub fn request_refresh(&mut self) -> u64 {
        self.awaited.clear();
        self.source_asked = Instant::now();
        self.source_reads + 1
    }

    /// Whether the rows published now were read after `ticket` was handed out.
    /// Always true without a source: there is nothing further to wait for.
    pub fn fresh_since(&self, ticket: u64) -> bool {
        if self.source.is_none() {
            return true;
        }
        let asked = self.source_asked.elapsed();
        if !self.awaited.is_empty() {
            return asked >= SOURCE_AWAIT
                || self
                    .awaited
                    .iter()
                    .all(|name| self.current.sessions.iter().any(|row| &row.name == name));
        }
        self.source_reads >= ticket || asked >= SOURCE_SETTLE
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
            // A Fleet Task on another host: no route of thurbox's reaches
            // it with git (see `SessionRow::workspace_reachable`).
            if !row.workspace_reachable() {
                continue;
            }
            if let Some(cwd) = row.cwd.clone() {
                // A remote session's `cwd` is a path on its host, so its stats
                // are read there. A host this machine cannot name is skipped —
                // running `git` locally against that path would only fail.
                let host = if crate::session::Route::is_remote_key(&row.backend) {
                    match crate::config::hosts::cached_registry()
                        .0
                        .resolve(&row.backend)
                        .cloned()
                    {
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

    /// Apply what the source has now and fold in the git stats, without
    /// waiting on the interval. Start-up and tests.
    pub fn refresh(&mut self) {
        self.last_refresh = Some(Instant::now());
        self.poll_source();
        self.attach_git_stats();
        self.current.taken_at_ms = taken_at_stamp();
        self.mark_changed();
    }
}

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
            let mut store = SnapshotStore::without_sessions(
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

    #[test]
    fn idle_clock_does_not_invalidate_snapshot_rows() {
        let mut store = SnapshotStore::without_sessions(
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
