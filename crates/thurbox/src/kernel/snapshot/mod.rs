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
use crate::session::{SessionId, SessionState};
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
    /// The raw hook state Fleet reported, before it is read into `status`.
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
    /// only the name is withheld.
    pub detected_agent: Option<String>,
    /// The session's `ui.*` meta keys (issue #1523), cleaned for publishing
    /// ([`crate::kernel::lua_api::clean_meta_value`]) and capped at
    /// [`crate::kernel::lua_api::META_KEYS`]; empty when it has none. Every
    /// other key is a driver's or thurbox's own and never reaches a plugin.
    pub meta: std::collections::BTreeMap<String, String>,
    /// Where a Fleet row's status came from (the Task's `status.activity`);
    /// `None` for a row whose status Fleet's agent state did not decide.
    pub status_source: Option<StatusSource>,
}

/// Where a session's agent state came from: the agent's hooks, a screen
/// rule Fleet read it with, or neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSource {
    /// `hook`, `screen` or `none`.
    pub source: String,
    /// The screen rule, `<set>/<rule id>`, for a screen state.
    pub rule: Option<String>,
    /// When Fleet first saw this state from this source, in ms since the
    /// epoch.
    pub since_ms: Option<i64>,
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
}

/// A host Fleet defines, as the creation flow offers it: its name, the
/// detail shown beside it, and the `backend` a created Task names (empty for
/// this machine).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostRow {
    pub name: String,
    pub detail: String,
    pub backend: String,
}

/// An immutable picture of the engine at one instant.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub sessions: Vec<SessionRow>,
    /// Repositories a new session can be created against.
    pub repos: Vec<RepoRow>,
    /// The harnesses Fleet has profiles for (`fleet version --json`): the
    /// agents a session can run. `None` until Fleet has answered, which is
    /// not the same as no agents.
    pub agents: Option<Vec<AgentRow>>,
    /// Every host Fleet defines (`fleet runtime hosts --json`), `local`
    /// included when Fleet defines it, with an empty `backend`. `None` until
    /// Fleet has answered, which is not the same as no hosts.
    pub hosts: Option<Vec<HostRow>>,
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
    /// The harnesses Fleet has profiles for, once it has said.
    pub harnesses: Option<Vec<String>>,
    /// The hosts Fleet defines, once it has said.
    pub hosts: Option<Vec<String>>,
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
    /// Sessions deleted here that the source may still list: see
    /// [`Self::hide_until_gone`].
    gone: std::collections::HashSet<String>,
    git: GitStats,
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
    pub fn with_source(source: Box<dyn SnapshotSource>, settings: &Settings) -> Self {
        let mut store = Self::build(settings);
        store.source = Some(source);
        store.poll_source();
        store
    }

    /// A store with no session source: no sessions, agents or hosts.
    pub fn without_sessions(settings: &Settings) -> Self {
        Self::build(settings)
    }

    /// Apply what the source has, returning whether anything moved.
    fn poll_source(&mut self) -> bool {
        let Some(read) = self.source.as_mut().and_then(|source| source.poll()) else {
            return false;
        };
        self.source_reads += 1;
        let mut sessions = read.sessions;
        self.gone
            .retain(|id| sessions.iter().any(|row| &row.id == id));
        sessions.retain(|row| !self.gone.contains(&row.id));
        self.current.sessions = sessions;
        self.current.error = read.error;
        if let Some(harnesses) = read.harnesses {
            self.current.agents = Some(
                harnesses
                    .into_iter()
                    .map(|name| AgentRow { name })
                    .collect(),
            );
        }
        if let Some(hosts) = read.hosts {
            self.current.hosts = Some(hosts_from(&hosts));
        }
        self.current.taken_at_ms = taken_at_stamp();
        self.mark_changed();
        true
    }

    fn build(settings: &Settings) -> Self {
        Self {
            source: None,
            source_reads: 0,
            source_asked: Instant::now(),
            awaited: Vec::new(),
            gone: std::collections::HashSet::new(),
            git: GitStats::new(git_poll_interval(settings.git_poll_secs)),
            current: Snapshot {
                // This machine until Fleet lists its hosts, so a Fleet that is
                // slow or absent never holds the creation flow on a host step
                // with nothing to choose; Fleet's answer replaces it.
                hosts: Some(hosts_from(&[])),
                taken_at_ms: taken_at_stamp(),
                ..Snapshot::default()
            },
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
        self.current.taken_at_ms = taken_at_stamp();
        // Git stats landing rewrite rows here, so this changes more than the
        // timestamp, and the snapshot moved if they did.
        let git_moved = self.attach_git_stats();
        if git_moved {
            self.mark_changed();
        }
        applied
    }

    /// Stop publishing the row of `id`, a session whose delete succeeded,
    /// until the source no longer lists it. A source reads on a schedule of
    /// its own, so its next answers can still hold the session as it was.
    pub fn hide_until_gone(&mut self, id: String) {
        let before = self.current.sessions.len();
        self.current.sessions.retain(|row| row.id != id);
        if self.current.sessions.len() != before {
            self.mark_changed();
        }
        self.gone.insert(id);
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

        let mut wanted: Vec<(String, PathBuf)> = Vec::new();
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
                // A remote session's `cwd` is a path on its host, and thurbox
                // reads git on this machine only.
                if crate::session::Route::is_remote_key(&row.backend) {
                    continue;
                }
                wanted.push((row.id.clone(), cwd));
            }
        }
        for (id, cwd) in wanted {
            self.git.request(&id, cwd);
        }
        moved
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

/// The host rows the creation flow offers: each host Fleet defines, `local`
/// first. A row's `backend` is what a created Task names: empty for `local`
/// (this machine), else the Fleet host name, which is what a session row's
/// `remote_host` carries, so a creation in flight and its row group as one.
/// An empty list (Fleet's `null`: one runtime serves any host) offers this
/// machine alone.
fn hosts_from(hosts: &[String]) -> Vec<HostRow> {
    let local = HostRow {
        name: "local".to_string(),
        detail: "this machine".to_string(),
        ..HostRow::default()
    };
    if hosts.is_empty() {
        return vec![local];
    }
    let mut rows: Vec<HostRow> = hosts
        .iter()
        .filter(|host| host.as_str() != "local")
        .map(|host| HostRow {
            name: host.clone(),
            detail: "Fleet host".to_string(),
            backend: host.clone(),
        })
        .collect();
    if hosts.iter().any(|host| host == "local") {
        rows.insert(0, local);
    }
    rows
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
    crate::session::host_of_key(backend).map(str::to_string)
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
        assert_eq!(remote_host_of("local:tmux"), None);
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
    fn the_host_rows_are_fleets_hosts_local_first() {
        let rows = hosts_from(&["devbox".to_string(), "local".to_string()]);
        let names: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.name.as_str(), r.backend.as_str()))
            .collect();
        assert_eq!(names, [("local", ""), ("devbox", "devbox")]);
        // A Fleet with no `local` host offers none.
        let rows = hosts_from(&["devbox".to_string()]);
        assert!(rows.iter().all(|r| r.name != "local"), "{rows:?}");
    }

    #[test]
    fn idle_clock_does_not_invalidate_snapshot_rows() {
        let mut store = SnapshotStore::without_sessions(&Settings::default());
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
