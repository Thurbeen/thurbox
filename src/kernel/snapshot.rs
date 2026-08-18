//! The read side: an in-memory picture of the session engine that plugins
//! read from, refreshed on the kernel's own schedule.
//!
//! Design.md D5 in one rule: **Lua never blocks and never awaits.** A plugin
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

use crate::session::SessionId;
use crate::storage::Database;

/// How often the snapshot is rebuilt. A read never waits on this; it only
/// determines how out of date the answer may be.
pub const REFRESH_INTERVAL: Duration = Duration::from_millis(400);

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
    /// `working` / `blocked` / `done` / `idle` — derived below, not raw.
    pub status: String,
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
    /// The raw persisted hook state, before `derive_status` interprets it.
    ///
    /// Kept beside the derived `status` because the two answer different
    /// questions: `status` is what to draw, this is what the agent last
    /// reported — and a remote hook event has to be compared against the latter,
    /// or an acknowledged `done` (derived `idle`) would be written back and
    /// resurrected on every reconnect.
    pub hook_state: Option<String>,
}

/// A session that was deleted but not purged.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedRow {
    pub id: String,
    pub name: String,
    /// True when the worktree directory was removed as well, so restoring
    /// recovers committed work only. The distinction has to be visible *before*
    /// the choice, not after.
    pub partial: bool,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub started_at: i64,
    pub status: String,
    pub detail: String,
}

/// An automation, flattened for rendering.
#[derive(Debug, Clone, PartialEq)]
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
}

/// A machine a session can be created on.
///
/// Three fields rather than a name, because the flow needs all three and they
/// are not derivable from one another: the name identifies it, the detail is
/// what tells two apart on screen (`me@devbox`, `WSL`), and the backend is what
/// the create command and the bookmark scope are keyed by (`ssh:devbox`).
#[derive(Debug, Clone, PartialEq)]
pub struct HostRow {
    pub name: String,
    pub detail: String,
    pub backend: String,
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
    pub tasks: Vec<TaskRow>,
    pub automations: Vec<AutomationRow>,
    /// Epoch milliseconds this snapshot represents. Readable by plugins so
    /// they can render staleness.
    pub taken_at_ms: i64,
    /// Set when the last refresh failed; the previous rows are retained.
    pub error: Option<String>,
}

impl Snapshot {
    pub fn session(&self, id: &str) -> Option<&SessionRow> {
        self.sessions.iter().find(|row| row.id == id)
    }
}

/// What a git-stat worker reports back: the session, and its state when the
/// path turned out to be a repository.
type StatResult = (String, Option<GitState>);

/// How long a git stat is trusted before it is computed again.
///
/// v1 refreshes on the same ~5 s cadence (`GIT_REFRESH_TICKS`) for the same
/// reason: `worktree_stats` shells out to `git`, which is far too expensive per
/// frame — and answered only once, a session's diffstat freezes at whatever it was
/// the first time it was looked at and never moves again.
const GIT_STAT_TTL: Duration = Duration::from_secs(5);

/// One session's last git answer, and when it landed.
struct Stat {
    /// `None` when the path turned out not to be a repository.
    ///
    /// Recorded rather than dropped: a miss nobody remembers is asked again on
    /// every single refresh, which is a thread and a `git` process per
    /// non-repository session forever — and *every remote session* is a miss, its
    /// worktree being on another machine entirely.
    state: Option<GitState>,
    at: Instant,
}

/// Git stats computed off the render path.
///
/// `git::worktree_stats` shells out, so it cannot run during a refresh — that
/// happens on the loop. Fourth instance of the worker pattern: touch the world
/// on a thread, publish the result.
#[derive(Default)]
struct GitStats {
    known: std::collections::HashMap<String, Stat>,
    inflight: std::collections::HashSet<String>,
    channel: Option<(
        std::sync::mpsc::Sender<StatResult>,
        std::sync::mpsc::Receiver<StatResult>,
    )>,
}

impl GitStats {
    fn ensure_channel(&mut self) -> std::sync::mpsc::Sender<StatResult> {
        if self.channel.is_none() {
            self.channel = Some(std::sync::mpsc::channel());
        }
        self.channel.as_ref().expect("just created").0.clone()
    }

    /// Ask for a session's stats, unless a run is in flight or the last answer is
    /// still fresh.
    fn request(&mut self, session: &str, worktree: PathBuf) {
        if self.inflight.contains(session) {
            return;
        }
        if self
            .known
            .get(session)
            .is_some_and(|stat| stat.at.elapsed() < GIT_STAT_TTL)
        {
            return;
        }
        self.inflight.insert(session.to_string());
        let tx = self.ensure_channel();
        let session = session.to_string();
        std::thread::spawn(move || {
            let stats = crate::git::worktree_stats(&worktree).map(|s| GitState {
                files_changed: s.files_changed,
                insertions: s.insertions,
                deletions: s.deletions,
                untracked: s.untracked,
                dirty: s.dirty,
                ahead: s.ahead,
                behind: s.behind,
            });
            let _ = tx.send((session, stats));
        });
    }

    fn drain(&mut self) {
        let Some((_, rx)) = &self.channel else { return };
        while let Ok((session, stats)) = rx.try_recv() {
            self.inflight.remove(&session);
            // A miss is an answer too — see `Stat::state`.
            self.known.insert(
                session,
                Stat {
                    state: stats,
                    at: Instant::now(),
                },
            );
        }
    }

    /// Forget sessions that are no longer in the snapshot, so the cache tracks
    /// what exists rather than everything that ever did.
    fn retain(&mut self, present: &std::collections::HashSet<&str>) {
        self.known.retain(|id, _| present.contains(id.as_str()));
    }
}

/// Owns the snapshot and decides when to rebuild it.
///
/// Refresh happens on the kernel's schedule, never inside a plugin call — so a
/// slow database read shows up as one late frame, never as a plugin that hangs.
pub struct SnapshotStore {
    database: Option<Database>,
    git: GitStats,
    /// Read once at startup: these change only when their config files do, and
    /// re-reading them every 400ms would be a filesystem hit for nothing.
    agents: Vec<AgentRow>,
    agent_default: String,
    hosts: Vec<HostRow>,
    current: Snapshot,
    last_refresh: Option<Instant>,
    /// `PRAGMA data_version` as of the last successful rebuild. `None` means
    /// "never read", which forces one.
    last_data_version: Option<i64>,
    /// Remote hook events that named a pane no session claims yet.
    ///
    /// The subscription's first report routinely arrives before the pane has
    /// been adopted, so an unmatched event is parked rather than lost.
    pending_hooks: Vec<PendingHook>,
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
    pub fn open() -> Self {
        let (database, error) = match crate::paths::database_file() {
            Some(path) => match Database::open(&path) {
                Ok(db) => (Some(db), None),
                Err(e) => (None, Some(format!("open {}: {e}", path.display()))),
            },
            None => (
                None,
                Some("could not resolve the database path".to_string()),
            ),
        };
        let mut store = Self {
            database,
            git: GitStats::default(),
            agents: read_agents(),
            agent_default: read_agent_default(),
            hosts: read_hosts(),
            current: Snapshot {
                error,
                ..Snapshot::default()
            },
            last_refresh: None,
            last_data_version: None,
            pending_hooks: Vec::new(),
        };
        store.refresh();
        store
    }

    /// Build a store over an already-open database (tests, and any caller that
    /// owns its own connection).
    pub fn with_database(database: Database) -> Self {
        let mut store = Self {
            database: Some(database),
            git: GitStats::default(),
            agents: read_agents(),
            agent_default: read_agent_default(),
            hosts: read_hosts(),
            current: Snapshot::default(),
            last_refresh: None,
            last_data_version: None,
            pending_hooks: Vec::new(),
        };
        store.refresh();
        store
    }

    /// The current snapshot. Always immediate.
    pub fn current(&self) -> &Snapshot {
        &self.current
    }

    /// Rebuild if the refresh interval has elapsed *and* anything committed.
    /// Called from the event loop, never from a plugin.
    ///
    /// The interval alone would re-read five tables (plus one query per
    /// automation) every 400ms forever, on a database nobody wrote to.
    /// `PRAGMA data_version` reads an in-memory counter and moves whenever
    /// another connection commits — which covers every writer that matters, since
    /// the command bus holds its own — so an idle thurbox stops querying
    /// altogether. v1 gates its per-tick session read the same way (ADR-P6).
    ///
    /// Git stats are folded in either way: they arrive from worker threads, not
    /// from the database, so `data_version` says nothing about them.
    pub fn refresh_if_due(&mut self) -> bool {
        // `Option::is_none_or` would read better but is stable only from 1.82;
        // this crate's MSRV is 1.75.
        let due = self
            .last_refresh
            .map_or(true, |at| at.elapsed() >= REFRESH_INTERVAL);
        if !due {
            return false;
        }
        if self.rows_are_current() {
            self.last_refresh = Some(Instant::now());
            self.current.taken_at_ms = now_ms();
            self.attach_git_stats();
            return false;
        }
        self.refresh();
        true
    }

    /// Whether the stored rows still reflect the database.
    ///
    /// False before the first successful read, and false as soon as any other
    /// connection commits. A read failure leaves the recorded version unset, so
    /// the next call retries rather than trusting stale rows.
    fn rows_are_current(&mut self) -> bool {
        let Some(database) = &self.database else {
            return true;
        };
        let Some(seen) = self.last_data_version else {
            return false;
        };
        match database.data_version() {
            Ok(current) => current == seen,
            Err(_) => false,
        }
    }

    /// Attach whatever git stats are known and ask for anything stale.
    ///
    /// Asking on every refresh is free — `GitStats::request` is the one place that
    /// decides whether a stat is worth running, by its age. Asking only for rows
    /// with no answer yet is what froze every session's diffstat at its first
    /// reading for the life of the process.
    fn attach_git_stats(&mut self) {
        self.git.drain();
        let present: std::collections::HashSet<&str> = self
            .current
            .sessions
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        self.git.retain(&present);

        let mut wanted: Vec<(String, PathBuf)> = Vec::new();
        for row in &mut self.current.sessions {
            row.git = self.git.known.get(&row.id).and_then(|stat| stat.state);
            if let Some(cwd) = row.cwd.clone() {
                wanted.push((row.id.clone(), cwd));
            }
        }
        for (id, cwd) in wanted {
            self.git.request(&id, cwd);
        }
    }

    /// Acknowledge a finished turn: stamp `seen_at` on a session whose state is
    /// `done`, so its filled dot becomes hollow.
    ///
    /// Called when focus *leaves* a session — looking at a finished turn and then
    /// moving on is what "seen" means, and it is the only thing that retires a
    /// `done`. Without it the blue dot is permanent, since `derive_status` reads
    /// a mark nobody writes.
    ///
    /// The derived status is corrected in place as well: this write does not move
    /// `PRAGMA data_version` (it is our own connection), so a refresh would
    /// otherwise re-derive `done` from the row it just acknowledged.
    pub fn acknowledge(&mut self, session: &str) {
        let Some(database) = &self.database else {
            return;
        };
        let Some(id) = parse_id(session) else { return };
        let Ok(hooks) = database.load_hook_states() else {
            return;
        };
        let Some(hook) = hooks.get(&id) else { return };
        if hook.state.as_deref() != Some("done") {
            return;
        }
        let Some(state_at) = hook.state_at else {
            return;
        };
        if hook.seen_at.is_some_and(|seen| seen >= state_at) {
            return;
        }
        if database.mark_session_seen(id, state_at).is_err() {
            return;
        }
        if let Some(row) = self
            .current
            .sessions
            .iter_mut()
            .find(|row| row.id == session)
        {
            row.status = "idle".to_string();
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

        let Some(database) = &self.database else {
            return 0;
        };
        let mut queue = std::mem::take(&mut self.pending_hooks);
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
            // Remote-host-controlled free text: allow-list it, never interpret
            // anything else as a state.
            if !crate::session::HOOK_STATES.contains(&event.state.as_str()) {
                continue;
            }
            let Some(row) = self.current.sessions.iter_mut().find(|row| {
                row.backend == event.backend && row.backend_id.as_deref() == Some(&event.pane)
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
            // The subscription re-reports on every reconnect, so writing an
            // unchanged state would re-stamp `state_at` and resurrect an
            // already-acknowledged `done` as unseen.
            if row.hook_state.as_deref() == Some(event.state.as_str()) {
                continue;
            }
            let Some(id) = parse_id(&row.id) else {
                continue;
            };
            if database.set_hook_state(id, &event.state).is_err() {
                continue;
            }
            // Corrected in place for the same reason `acknowledge` does it: this
            // is our own connection, so `PRAGMA data_version` will not move and a
            // refresh would re-derive from the row we just wrote.
            row.status = derive_status(Some(&event.state), Some(now_ms()), None);
            row.hook_state = Some(event.state);
            applied += 1;
        }
        applied
    }

    /// Record — or forget — the pane id of a session's companion shell.
    ///
    /// Written straight through and corrected in place, like `acknowledge`: this
    /// is our own connection, so `PRAGMA data_version` will not move and a
    /// refresh would otherwise re-read the row we just wrote.
    pub fn remember_shell(&mut self, session: &str, pane: &str) -> Result<(), String> {
        self.write_shell(session, Some(pane.to_string()))
    }

    pub fn forget_shell(&mut self, session: &str) -> Result<(), String> {
        self.write_shell(session, None)
    }

    fn write_shell(&mut self, session: &str, pane: Option<String>) -> Result<(), String> {
        let Some(database) = &self.database else {
            return Err("no database".to_string());
        };
        let Some(id) = parse_id(session) else {
            return Err(format!("not a session id: {session}"));
        };
        let Some(mut row) = database
            .get_session_by_id(id)
            .map_err(|e| format!("read session: {e}"))?
        else {
            return Err(format!("session not found: {session}"));
        };
        if row.shell_backend_id == pane {
            return Ok(());
        }
        row.shell_backend_id = pane.clone();
        database
            .upsert_session(&row)
            .map_err(|e| format!("record shell pane: {e}"))?;
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

    /// Take a pending "focus this session" request, if another process left one.
    ///
    /// One `DELETE … RETURNING`, so the row is claimed atomically and two
    /// instances cannot both act on it. v1 drains the same row in
    /// `apply_pending_focus_request`.
    pub fn take_focus_request(&self) -> Option<String> {
        self.database
            .as_ref()
            .and_then(|database| database.take_pending_focus_session_id().ok().flatten())
    }

    /// Rebuild now.
    ///
    /// A failed read keeps the previous rows and records the error, so a
    /// transient database lock degrades to stale data rather than a blank list.
    pub fn refresh(&mut self) {
        self.last_refresh = Some(Instant::now());
        let taken_at_ms = now_ms();
        // Recorded BEFORE the reads: a commit landing between the two would
        // otherwise be recorded as already-seen and never picked up.
        self.last_data_version = self
            .database
            .as_ref()
            .and_then(|database| database.data_version().ok());

        let Some(database) = &self.database else {
            self.current.taken_at_ms = taken_at_ms;
            return;
        };

        let sessions = match database.list_active_sessions() {
            Ok(sessions) => sessions,
            Err(e) => {
                self.current.error = Some(format!("list sessions: {e}"));
                self.current.taken_at_ms = taken_at_ms;
                return;
            }
        };
        let hooks = database.load_hook_states().unwrap_or_default();
        let bases = database.load_base_branches().unwrap_or_default();

        self.git.drain();

        let rows: Vec<SessionRow> = sessions
            .into_iter()
            .map(|session| {
                let hook = hooks.get(&session.id);
                let status = derive_status(
                    hook.and_then(|h| h.state.as_deref()),
                    hook.and_then(|h| h.state_at),
                    hook.and_then(|h| h.seen_at),
                );
                let hook_state = hook.and_then(|h| h.state.clone());
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
                    hook_state,
                    member_dirs,
                }
            })
            .collect();

        // Deleted sessions, so a restore surface has something to list.
        let deleted = database
            .list_deleted_sessions()
            .map(|list| {
                list.into_iter()
                    .map(|row| DeletedRow {
                        id: row.id.to_string(),
                        name: row.name,
                        partial: row.force_deleted,
                    })
                    .collect()
            })
            .unwrap_or_default();

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

        // Tasks and automations: already fully functional headlessly, so this
        // only surfaces them.
        let tasks = database
            .list_tasks()
            .map(|list| {
                list.into_iter()
                    .map(|task| TaskRow {
                        id: task.id,
                        title: task.title,
                        description: task.description,
                        status: task.status.as_str().to_string(),
                        source: task.source,
                        external_url: task.external_url,
                        created_at: task.created_at,
                        updated_at: task.updated_at,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let automations = database
            .list_automations()
            .map(|list| {
                list.into_iter()
                    .map(|auto| {
                        let history = database
                            .list_automation_runs(auto.id, 10)
                            .unwrap_or_default();
                        let runs: Vec<RunRow> = history
                            .iter()
                            .map(|run| RunRow {
                                started_at: run.started_at as i64,
                                status: run.status.as_str().to_string(),
                                detail: run.detail.clone(),
                            })
                            .collect();
                        let last = history.into_iter().next();
                        AutomationRow {
                            id: auto.id,
                            name: auto.name,
                            // The expression itself when it is a cron, else
                            // the kind — what a pane wants to show.
                            schedule: match &auto.schedule {
                                crate::session::automation::AutomationSchedule::Cron { expr } => {
                                    expr.clone()
                                }
                                other => other.kind().to_string(),
                            },
                            action: auto.action.kind().to_string(),
                            enabled: auto.enabled,
                            last_outcome: last.as_ref().map(|run| run.status.as_str().to_string()),
                            last_detail: last.map(|run| run.detail).filter(|d| !d.is_empty()),
                            runs,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        self.current = Snapshot {
            sessions: rows,
            deleted,
            tasks,
            automations,
            repos,
            agents: self.agents.clone(),
            agent_default: self.agent_default.clone(),
            hosts: self.hosts.clone(),
            taken_at_ms,
            error: None,
        };
        self.attach_git_stats();
    }
}

/// Agents from the registry the launcher itself uses — so the flow can never
/// offer one that would fail to launch.
fn read_agents() -> Vec<AgentRow> {
    crate::agent::agent_config::load_or_seed()
        .agents
        .iter()
        .map(|agent| AgentRow {
            name: agent.name.clone(),
            command: agent.command.clone(),
        })
        .collect()
}

/// The agent a bare launch would use, so the flow preselects the same one.
fn read_agent_default() -> String {
    crate::agent::agent_config::load_or_seed()
        .default_name()
        .to_string()
}

/// Configured and discovered hosts. Empty means local only, and the flow skips
/// asking.
fn read_hosts() -> Vec<HostRow> {
    let (registry, _warnings) = crate::agent::host_config::load_all_with_warnings();
    registry
        .hosts
        .iter()
        .map(|host| HostRow {
            name: host.name.clone(),
            detail: host.picker_detail(),
            backend: host.backend_name(),
        })
        .collect()
}

/// Map the persisted hook state onto a status name.
///
/// Mirrors v1's derivation, including the stuck-`working` fallback: agents fire
/// no hook when a turn is interrupted, so a `working` state that has not moved
/// for a while is reported as idle rather than spinning forever. The stored row
/// is not touched — this is a read-time decision, exactly as v1 does it.
pub fn derive_status(state: Option<&str>, state_at: Option<i64>, seen_at: Option<i64>) -> String {
    /// How long a `working` state may stand without an update before it is
    /// treated as finished. v1 uses the same 10s bound.
    const WORKING_STALE_MS: i64 = 10_000;

    match state {
        Some("working") => {
            let stale = state_at.is_some_and(|at| now_ms().saturating_sub(at) > WORKING_STALE_MS);
            if stale {
                "idle".to_string()
            } else {
                "working".to_string()
            }
        }
        Some("blocked") => "blocked".to_string(),
        Some("done") => {
            // Acknowledged once the user moved off it, which v1 records by
            // stamping seen_at at or after the state change.
            let acknowledged = match (seen_at, state_at) {
                (Some(seen), Some(at)) => seen >= at,
                _ => false,
            };
            if acknowledged {
                "idle".to_string()
            } else {
                "done".to_string()
            }
        }
        _ => "idle".to_string(),
    }
}

/// Fold what the terminal store knows into a session's status.
///
/// The hook state says what the *agent* is doing; it cannot say that the machine
/// the agent runs on has gone away, because a host that is down reports nothing
/// at all — it just leaves the last state standing. So a remote session with no
/// live pane reads `unreachable` rather than the stale `idle` its row still
/// carries, which is v1's placeholder row by another name.
///
/// Local sessions are never unreachable: this *is* their machine, and a missing
/// pane there means the agent has not been launched, not that it cannot be
/// reached.
pub fn with_reachability(status: &str, backend: &str, attach_error: Option<&str>) -> String {
    if attach_error.is_some() && crate::session::is_remote_backend(backend) {
        return "unreachable".to_string();
    }
    status.to_string()
}

/// A remote session's bare host name, read off its backend name.
fn remote_host_of(backend: &str) -> Option<String> {
    backend
        .strip_prefix("ssh:")
        .or_else(|| backend.strip_prefix("wsl:"))
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
    worktrees: &[crate::sync::state::SharedWorktree],
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
    fn an_unreported_session_is_idle() {
        assert_eq!(derive_status(None, None, None), "idle");
        assert_eq!(derive_status(Some("unknown"), None, None), "idle");
    }

    #[test]
    fn a_fresh_working_state_is_working() {
        let now = now_ms();
        assert_eq!(derive_status(Some("working"), Some(now), None), "working");
    }

    #[test]
    fn a_stale_working_state_falls_back_to_idle() {
        // Agents fire no hook on interrupt, so without this a cancelled turn
        // would spin forever. v1 guards the same way.
        let long_ago = now_ms() - 60_000;
        assert_eq!(derive_status(Some("working"), Some(long_ago), None), "idle");
    }

    #[test]
    fn blocked_is_never_time_gated() {
        let long_ago = now_ms() - 600_000;
        assert_eq!(
            derive_status(Some("blocked"), Some(long_ago), None),
            "blocked"
        );
    }

    #[test]
    fn done_stays_done_until_acknowledged() {
        let at = now_ms();
        assert_eq!(derive_status(Some("done"), Some(at), None), "done");
        assert_eq!(derive_status(Some("done"), Some(at), Some(at - 1)), "done");
        assert_eq!(derive_status(Some("done"), Some(at), Some(at)), "idle");
    }

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
    fn an_in_memory_database_yields_an_empty_but_stamped_snapshot() {
        let database = Database::open_in_memory().expect("in-memory database opens");
        let store = SnapshotStore::with_database(database);
        let snapshot = store.current();
        assert!(snapshot.sessions.is_empty());
        assert!(snapshot.taken_at_ms > 0, "snapshot must carry its instant");
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
    }

    #[test]
    fn a_read_is_immediate_and_repeatable() {
        let database = Database::open_in_memory().expect("in-memory database opens");
        let store = SnapshotStore::with_database(database);
        // The property that matters: reading never consults the database.
        for _ in 0..1000 {
            assert!(store.current().sessions.is_empty());
        }
    }

    #[test]
    fn a_reachable_session_keeps_the_status_its_hooks_reported() {
        assert_eq!(with_reachability("working", "ssh:devbox", None), "working");
        assert_eq!(with_reachability("done", "local-tmux", None), "done");
    }

    #[test]
    fn a_remote_session_with_no_pane_is_unreachable() {
        assert_eq!(
            with_reachability("idle", "ssh:devbox", Some("host unreachable")),
            "unreachable"
        );
        assert_eq!(
            with_reachability("working", "wsl:ubuntu", Some("connect timed out")),
            "unreachable"
        );
    }

    #[test]
    fn a_local_session_without_a_pane_is_not_unreachable() {
        // This is the machine it runs on: no pane means the agent has not been
        // launched, which the pane itself says. Calling it unreachable would
        // claim a host problem that does not exist.
        assert_eq!(
            with_reachability("idle", "local-tmux", Some("session has no pane yet")),
            "idle"
        );
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
        let worktrees = vec![crate::sync::state::SharedWorktree {
            repo_path: PathBuf::from("/src/thurbox"),
            worktree_path: PathBuf::from("/worktrees/fix-osc52"),
            branch: "fix/osc52".into(),
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
            crate::sync::state::SharedWorktree {
                repo_path: PathBuf::from("/src/a"),
                worktree_path: PathBuf::from("/worktrees/a"),
                branch: "feat/x".into(),
            },
            crate::sync::state::SharedWorktree {
                repo_path: PathBuf::from("/src/b"),
                worktree_path: PathBuf::from("/worktrees/b"),
                branch: "feat/x".into(),
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
}
