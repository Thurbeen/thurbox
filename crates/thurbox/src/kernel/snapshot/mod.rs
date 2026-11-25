use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub mod fleet;
mod git;

use crate::session::settings::Settings;
use crate::session::{SessionId, SessionState};
use git::{git_poll_interval, GitStats};

pub const REFRESH_INTERVAL: Duration = Duration::from_millis(400);

const SOURCE_SETTLE: Duration = Duration::from_secs(1);

const SOURCE_AWAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitState {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub untracked: usize,
    pub dirty: bool,
    pub ahead: usize,
    pub behind: usize,
    pub merged: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub name: String,
    pub agent: String,
    pub status: SessionState,
    pub cwd: Option<PathBuf>,
    pub repo: Option<String>,
    pub repos: Vec<String>,
    pub member_dirs: Vec<PathBuf>,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub backend: String,
    pub backend_id: Option<String>,
    pub remote_host: Option<String>,
    pub place: Option<String>,
    pub agent_session_id: Option<String>,
    pub parent_id: Option<String>,
    pub display_order: Option<i64>,
    pub worktree_count: usize,
    pub git: Option<GitState>,
    pub shell_backend_id: Option<String>,
    pub stopped: bool,
    pub hook_state: Option<String>,
    pub key: Option<String>,
    pub reports_as: Option<String>,
    pub detected_agent: Option<String>,
    pub meta: std::collections::BTreeMap<String, String>,
    pub status_source: Option<StatusSource>,
    pub deleting: Option<Deleting>,
    pub state_why: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSource {
    pub source: String,
    pub rule: Option<String>,
    pub since_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deleting {
    pub since_ms: i64,
    pub why: Option<String>,
}

impl SessionRow {
    pub fn workspace_reachable(&self) -> bool {
        self.backend != fleet::BACKEND || self.remote_host.is_none()
    }

    pub fn fleet(&self) -> Option<&str> {
        self.id.split_once(':').map(|(fleet, _)| fleet)
    }

    pub fn task(&self) -> &str {
        task_of(&self.id)
    }

    pub fn workspace_on_this_machine(&self) -> bool {
        self.workspace_reachable() && !crate::session::Route::is_remote_key(&self.backend)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepoRow {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentRow {
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostRow {
    pub name: String,
    pub detail: String,
    pub backend: String,
    pub ready: Option<bool>,
    pub why: Option<String>,
    pub here: bool,
}

impl HostRow {
    pub fn local(machine: &str) -> Self {
        Self {
            name: "local".to_string(),
            detail: machine.to_string(),
            here: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FleetState {
    #[default]
    Connecting,
    Up,
    Down,
    Unreachable,
    Unknown,
    Incompatible,
}

impl FleetState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Up => "up",
            Self::Down => "down",
            Self::Unreachable => "unreachable",
            Self::Unknown => "unknown",
            Self::Incompatible => "incompatible",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FleetRow {
    pub name: String,
    pub transport: String,
    pub remote: bool,
    pub state: FleetState,
    pub why: Option<String>,
    pub home: Option<String>,
    pub agents: Option<Vec<AgentRow>>,
    pub places: Option<Vec<HostRow>>,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub sessions: Vec<SessionRow>,
    pub repos: Vec<RepoRow>,
    pub agents: Option<Vec<AgentRow>>,
    pub hosts: Option<Vec<HostRow>>,
    pub fleets: Vec<FleetRow>,
    pub taken_at_ms: i64,
    pub error: Option<String>,
}

impl Snapshot {
    pub fn session(&self, id: &str) -> Option<&SessionRow> {
        self.sessions.iter().find(|row| row.id == id)
    }

    pub fn resolve(&self, given: &str) -> Result<&SessionRow, Unresolved> {
        if let Some(row) = self.session(given) {
            return Ok(row);
        }
        let matches: Vec<&SessionRow> = self
            .sessions
            .iter()
            .filter(|row| row.fleet().is_some() && row.task() == given)
            .collect();
        match matches.as_slice() {
            [row] => Ok(row),
            [] => Err(Unresolved::Missing),
            several => Err(Unresolved::Ambiguous(
                several.iter().map(|row| row.id.clone()).collect(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unresolved {
    Missing,
    Ambiguous(Vec<String>),
}

impl Unresolved {
    pub fn message(&self, given: &str) -> String {
        match self {
            Self::Missing => format!("no session {given} in this interface"),
            Self::Ambiguous(ids) => format!(
                "{given} is a session of several Fleets; name one of {}",
                ids.join(", ")
            ),
        }
    }
}

pub fn task_of(id: &str) -> &str {
    id.split_once(':').map_or(id, |(_, task)| task)
}

pub fn row_id(fleet: &str, task: &str) -> String {
    format!("{fleet}:{task}")
}

pub trait SnapshotSource: Send {
    fn poll(&mut self) -> Option<SourceRead>;

    fn refresh_places(&mut self, _fleet: &str) {}
}

pub const WANT_PLACES: &str = "want_places";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceRead {
    pub sessions: Vec<SessionRow>,
    pub error: Option<String>,
    pub harnesses: Option<Vec<String>>,
    pub hosts: Option<Vec<HostRow>>,
    pub fleets: Vec<FleetRow>,
    pub removed: Vec<String>,
}

pub struct SnapshotStore {
    source: Option<Box<dyn SnapshotSource>>,
    source_reads: u64,
    source_asked: Instant,
    awaited: Vec<String>,
    gone: std::collections::HashSet<String>,
    git: GitStats,
    current: Snapshot,
    last_refresh: Option<Instant>,
    places_wanted: Option<String>,
    version: u64,
}

impl SnapshotStore {
    pub fn with_source(source: Box<dyn SnapshotSource>, settings: &Settings) -> Self {
        let mut store = Self::build(settings);
        store.source = Some(source);
        store.poll_source();
        store
    }

    pub fn without_sessions(settings: &Settings) -> Self {
        Self::build(settings)
    }

    fn poll_source(&mut self) -> bool {
        let Some(read) = self.source.as_mut().and_then(|source| source.poll()) else {
            return false;
        };
        self.source_reads += 1;
        let mut sessions = read.sessions;
        self.gone
            .retain(|id| !read.removed.contains(id) && sessions.iter().any(|row| &row.id == id));
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
            self.current.hosts = Some(hosts);
        }
        self.current.fleets = read.fleets;
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
                hosts: Some(vec![HostRow::local("this machine")]),
                taken_at_ms: taken_at_stamp(),
                ..Snapshot::default()
            },
            last_refresh: None,
            places_wanted: None,
            version: 0,
        }
    }

    pub fn want_places(&mut self, fleet: Option<String>) {
        if fleet == self.places_wanted {
            return;
        }
        if let (Some(fleet), Some(source)) = (&fleet, self.source.as_mut()) {
            source.refresh_places(fleet);
        }
        self.places_wanted = fleet;
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn mark_changed(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    pub fn current(&self) -> &Snapshot {
        &self.current
    }

    pub fn refresh_if_due(&mut self) -> bool {
        self.refresh_if_due_at(Instant::now())
    }

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
        let git_moved = self.attach_git_stats();
        if git_moved {
            self.mark_changed();
        }
        applied
    }

    pub fn hide_until_gone(&mut self, id: String) {
        if self
            .current
            .session(&id)
            .is_some_and(|row| row.deleting.is_some())
        {
            return;
        }
        let before = self.current.sessions.len();
        self.current.sessions.retain(|row| row.id != id);
        if self.current.sessions.len() != before {
            self.mark_changed();
        }
        self.gone.insert(id);
    }

    pub fn request_refresh_awaiting(&mut self, created: Vec<String>) -> u64 {
        let ticket = self.request_refresh();
        self.awaited = created;
        ticket
    }

    pub fn request_refresh(&mut self) -> u64 {
        self.awaited.clear();
        self.source_asked = Instant::now();
        self.source_reads + 1
    }

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
                    .all(|id| self.current.session(id).is_some());
        }
        self.source_reads >= ticket || asked >= SOURCE_SETTLE
    }

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
            if !row.workspace_reachable() {
                continue;
            }
            if let Some(cwd) = row.cwd.clone() {
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

    pub fn refresh(&mut self) {
        self.last_refresh = Some(Instant::now());
        self.poll_source();
        self.attach_git_stats();
        self.current.taken_at_ms = taken_at_stamp();
        self.mark_changed();
    }
}

pub(super) fn remote_host_of(backend: &str) -> Option<String> {
    crate::session::host_of_key(backend).map(str::to_string)
}

pub(crate) fn repo_name(cwd: &Option<PathBuf>, repo_path: Option<&PathBuf>) -> Option<String> {
    repo_path
        .or(cwd.as_ref())
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
}

fn taken_at_stamp() -> i64 {
    now_ms() / 1000 * 1000
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn parse_id(raw: &str) -> Option<SessionId> {
    raw.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_task_name_resolves_only_when_one_fleet_holds_it() {
        let row = |fleet: &str, task: &str| {
            let link = crate::fleet_link::FleetLink::of(fleet, &Default::default());
            fleet::row_of(&link, &serde_json::json!({"metadata": {"name": task}}))
        };
        let snapshot = Snapshot {
            sessions: vec![
                row("box", "review"),
                row("laptop", "review"),
                row("box", "audit"),
            ],
            ..Snapshot::default()
        };
        assert_eq!(
            snapshot.resolve("box:review").map(|r| r.id.as_str()),
            Ok("box:review")
        );
        assert_eq!(
            snapshot.resolve("audit").map(|r| r.id.as_str()),
            Ok("box:audit")
        );
        let ambiguous = snapshot.resolve("review").unwrap_err();
        assert_eq!(
            ambiguous,
            Unresolved::Ambiguous(vec!["box:review".into(), "laptop:review".into()])
        );
        assert!(ambiguous
            .message("review")
            .contains("box:review, laptop:review"));
        assert_eq!(snapshot.resolve("absent").unwrap_err(), Unresolved::Missing);
        let audit = snapshot.session("box:audit").unwrap();
        assert_eq!((audit.fleet(), audit.task()), (Some("box"), "audit"));
    }

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

    struct Fed(std::sync::mpsc::Receiver<SourceRead>);

    impl SnapshotSource for Fed {
        fn poll(&mut self) -> Option<SourceRead> {
            self.0.try_recv().ok()
        }
    }

    fn fed() -> (std::sync::mpsc::Sender<SourceRead>, SnapshotStore) {
        let (tx, rx) = std::sync::mpsc::channel();
        let store = SnapshotStore::with_source(Box::new(Fed(rx)), &Settings::default());
        (tx, store)
    }

    fn made(name: &str) -> SourceRead {
        let link = crate::fleet_link::FleetLink::local("fleet");
        SourceRead {
            sessions: vec![fleet::row_of(
                &link,
                &serde_json::json!({"metadata": {"name": name}}),
            )],
            ..SourceRead::default()
        }
    }

    #[test]
    fn a_refresh_ticket_waits_for_the_source_to_answer() {
        let (tx, mut store) = fed();
        let ticket = store.request_refresh();
        assert!(!store.fresh_since(ticket), "nothing was read since");
        tx.send(made("made")).unwrap();
        store.refresh_if_due();
        assert!(store.fresh_since(ticket));
        assert!(store.current().session("local:made").is_some());

        let quiet = store.request_refresh();
        assert!(!store.fresh_since(quiet));
        store.source_asked -= SOURCE_SETTLE;
        assert!(store.fresh_since(quiet), "a source that has nothing to say");
    }

    #[test]
    fn a_refresh_awaiting_a_created_session_waits_for_its_row() {
        let (tx, mut store) = fed();
        let ticket = store.request_refresh_awaiting(vec!["local:made".into()]);
        store.source_asked -= SOURCE_SETTLE;
        store.refresh_if_due();
        assert!(!store.fresh_since(ticket), "fresh before the row arrived");
        tx.send(made("made")).unwrap();
        store.refresh_if_due();
        assert!(store.fresh_since(ticket));
        assert!(store.current().session("local:made").is_some());

        let late = store.request_refresh_awaiting(vec!["local:never".into()]);
        store.source_asked -= SOURCE_AWAIT;
        assert!(store.fresh_since(late), "a row that never comes");
    }
}
