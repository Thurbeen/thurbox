//! Fleet as the snapshot's source (ADR-41): the Tasks, read through `fleet
//! watch --json`, each published as a session row; and the `fleet up` that
//! keeps them reconciling.
//!
//! Both run on threads of their own, so the store's poll only drains a
//! channel. thurbox reads nothing of Fleet's store directly and links no
//! Fleet crate: the command line and its NDJSON are the contract
//! (`docs/fleet/contracts.md`, the command line).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use super::{
    repo_name, AgentRow, Deleting, FleetRow, FleetState, HostRow, SessionRow, SnapshotSource,
    SourceRead, StatusSource,
};
use crate::fleet_link::FleetLink;
use crate::session::SessionState;

/// How long a watch that ended waits before it is started again, resuming
/// from the last revision it read.
const RETRY: Duration = Duration::from_secs(2);

/// How often a manager that is not ours, or that would not start, is asked
/// about again: each ask is a `fleet up`, which exits at once while another
/// holds the lease.
const MANAGER_CHECK: Duration = Duration::from_secs(10);

/// How long a Task marked for deletion stays off the list while Fleet tears
/// its session down. Past it, the row comes back marked so a teardown that
/// does not end can be seen.
///
/// Fleet releases a session once it is at rest. A release is one runtime
/// operation: at most two calls, each bounded by the host's runtime
/// `callTimeoutMs` (15 s by default, ssh hosts included) plus 4 s of grace,
/// so 38 s. A failed one is tried again after 30 s, and the third failure
/// raises NeedsAttention. Three attempts and the two waits between them take
/// 174 s at most, so three minutes covers every release Fleet still expects
/// to finish on its own. A session busy when it is deleted is released only
/// once it is at rest, and can pass the mark that way.
pub const DELETION_STUCK_AFTER: Duration = Duration::from_secs(180);

/// `fleet up`'s exit code when another manager holds the lease.
const LEASE_HELD: i32 = 6;

/// The backend every Task's row names: no multiplexer of thurbox's, but
/// Fleet, whose runtime keeps the terminal.
pub const BACKEND: &str = "fleet";

/// The Task label holding the name a session was given in thurbox, which a
/// Task's own name (a DNS-like name) may not spell.
pub const NAME_LABEL: &str = "thurbox.dev/name";

enum Update {
    /// Every row, and the Tasks that went since the last rows: see
    /// [`SourceRead::removed`].
    Rows(Vec<SessionRow>, Vec<String>),
    /// What the running watch warns of, or `None` once it reads again.
    Watch(Option<String>),
    /// Why the watch ended: until it reads again, the Fleet cannot be asked,
    /// so its rows are unreachable rather than gone.
    Lost(String),
    /// Why no manager runs, or `None` once one does.
    Manager(Option<String>),
    /// The harnesses Fleet has profiles for: the agents a session can run.
    Harnesses(Vec<String>),
    /// The places Fleet defines: where a session can run.
    Places(Vec<HostRow>),
    /// Whether the Fleet is up, from its `fleet version --json`, and why not.
    State(FleetState, Option<String>),
    /// The home directory of a remote Fleet's machine.
    Home(String),
}

/// The Tasks as session rows, followed by `fleet watch` on a worker.
pub struct FleetWatch {
    /// The Fleet's name, which each row's id starts with.
    name: String,
    updates: Receiver<Update>,
    read: SourceRead,
    watch_error: Option<String>,
    /// Whether the watch ended and has not read since: its rows are drawn
    /// unreachable.
    lost: bool,
    manager_error: Option<String>,
    /// The Tasks as `fleet watch` last read them, before the interface's own
    /// order is laid over them.
    tasks: Vec<SessionRow>,
    /// Whether the watch has delivered rows yet.
    rows_seen: bool,
    /// When the next Task marked for deletion runs past
    /// [`DELETION_STUCK_AFTER`], in ms since the epoch: the rows change then
    /// though the watch says nothing.
    next_stuck_ms: Option<i64>,
    /// `interface.json` as last read, and the generation it was read at.
    interface: Option<(u64, crate::config::interface_state::InterfaceState)>,
    /// The running `fleet watch`, killed when the source goes.
    child: Arc<Mutex<Option<Child>>>,
    /// The Fleet as the creation flow offers it, as last asked.
    fleet: FleetRow,
    /// Held while the source lives: the `version` worker, which sends only
    /// a change, ends once it is gone.
    _alive: Arc<()>,
    /// What [`SnapshotSource::refresh_places`] reads through.
    link: FleetLink,
    tx: Sender<Update>,
    /// Set while a refresh is on its way, so a picker opened again before it
    /// answers starts no second one.
    refreshing: Arc<std::sync::atomic::AtomicBool>,
    /// The state last sent, shared by the `version` worker and a refresh.
    published: Published,
}

/// The Fleet state last sent to the source: the one owner both writers
/// compare with, so neither leaves the other's answer standing.
type Published = Arc<Mutex<Option<(FleetState, Option<String>)>>>;

/// Send `next` unless it is what was last sent. `false` once the source is
/// gone.
fn publish_state(
    published: &Published,
    tx: &Sender<Update>,
    next: (FleetState, Option<String>),
) -> bool {
    let Ok(mut last) = published.lock() else {
        return false;
    };
    if last.as_ref() == Some(&next) {
        return true;
    }
    *last = Some(next.clone());
    tx.send(Update::State(next.0, next.1)).is_ok()
}

impl FleetWatch {
    /// Follow the Tasks of the Fleet `link` reaches.
    pub fn spawn(link: FleetLink) -> Self {
        Self::start(link, false)
    }

    /// [`Self::spawn`], and keep a `fleet up` running: one is started
    /// whenever no manager holds the lease. A manager thurbox started
    /// outlives it, in a process group of its own: Tasks keep reconciling
    /// with no interface open.
    ///
    /// Only for a Fleet on this machine: a Fleet on another machine keeps its
    /// own manager (a user systemd unit there, docs/where-sessions-run.md),
    /// and thurbox never starts one through a launcher, where it would end
    /// with the connection.
    pub fn with_manager(link: FleetLink) -> Self {
        Self::start(link, true)
    }

    fn start(link: FleetLink, manager: bool) -> Self {
        let (tx, updates) = channel();
        if manager && link.is_local() {
            let (link, tx) = (link.clone(), tx.clone());
            std::thread::spawn(move || keep_manager(&link, &tx));
        }
        // The first poll publishes the Fleet, connecting, before it answers.
        let _ = tx.send(Update::State(FleetState::Connecting, None));
        let alive = Arc::new(());
        let published = Published::default();
        {
            let (link, tx, alive, published) = (
                link.clone(),
                tx.clone(),
                Arc::downgrade(&alive),
                published.clone(),
            );
            std::thread::spawn(move || read_version(&link, &tx, &alive, &published));
        }
        {
            let (link, tx) = (link.clone(), tx.clone());
            std::thread::spawn(move || read_places(&link, &tx));
        }
        if !link.is_local() {
            let (link, tx) = (link.clone(), tx.clone());
            std::thread::spawn(move || read_home(&link, &tx));
        }
        // The Fleet's own `local` until it lists its places, so a Fleet that
        // is slow or absent never holds the creation flow on a place step
        // with nothing to choose; its answer replaces it.
        let fleet = FleetRow {
            name: link.name().to_string(),
            transport: link.transport(),
            remote: !link.is_local(),
            places: Some(vec![HostRow::local(&machine(&link))]),
            ..FleetRow::default()
        };
        let child = Arc::new(Mutex::new(None));
        let held = Arc::clone(&child);
        let name = link.name().to_string();
        {
            let (link, tx) = (link.clone(), tx.clone());
            std::thread::spawn(move || watch(&link, &tx, &held));
        }
        Self {
            name,
            updates,
            read: SourceRead::default(),
            watch_error: None,
            lost: false,
            manager_error: None,
            tasks: Vec::new(),
            rows_seen: false,
            next_stuck_ms: None,
            interface: None,
            child,
            fleet,
            _alive: alive,
            link,
            tx,
            refreshing: Arc::default(),
            published,
        }
    }
}

impl SnapshotSource for FleetWatch {
    /// One `runtime places` and one `version`, on a worker. Each is bounded
    /// as any call through the link is (ssh's `ConnectTimeout`), and a
    /// Fleet that does not answer keeps what it said last.
    fn refresh_places(&mut self, fleet: &str) {
        use std::sync::atomic::Ordering;
        if fleet != self.name || self.refreshing.swap(true, Ordering::AcqRel) {
            return;
        }
        // A home not read at startup is asked for again with the places, on
        // a worker of its own: a slow one must not hold the next refresh.
        if !self.link.is_local() && self.fleet.home.is_none() {
            let (link, tx) = (self.link.clone(), self.tx.clone());
            std::thread::spawn(move || read_home(&link, &tx));
        }
        let (link, tx, refreshing, published) = (
            self.link.clone(),
            self.tx.clone(),
            self.refreshing.clone(),
            self.published.clone(),
        );
        std::thread::spawn(move || {
            if let Some(answer) = ask(&link, &["runtime", "places", "--json"]) {
                let _ = tx.send(Update::Places(places_from(&answer, &machine(&link))));
            }
            let (state, answer) = probe(&link);
            if let Some(answer) = answer {
                let _ = tx.send(Update::Harnesses(harnesses_of(&answer)));
            }
            publish_state(&published, &tx, state);
            refreshing.store(false, Ordering::Release);
        });
    }

    fn poll(&mut self) -> Option<SourceRead> {
        let mut moved = false;
        while let Ok(update) = self.updates.try_recv() {
            moved = true;
            match update {
                Update::Rows(rows, removed) => {
                    self.tasks = rows;
                    self.rows_seen = true;
                    self.read.removed.extend(removed);
                }
                Update::Watch(error) => {
                    self.lost &= error.is_some();
                    self.watch_error = error;
                }
                Update::Lost(why) => {
                    self.lost = true;
                    self.watch_error = Some(why);
                }
                Update::Manager(error) => self.manager_error = error,
                Update::Harnesses(harnesses) => {
                    self.fleet.agents = Some(
                        harnesses
                            .iter()
                            .map(|name| AgentRow { name: name.clone() })
                            .collect(),
                    );
                    self.read.harnesses = Some(harnesses);
                }
                Update::Places(places) => {
                    self.fleet.places = Some(places.clone());
                    self.read.hosts = Some(places);
                }
                Update::State(state, why) => {
                    self.fleet.state = state;
                    self.fleet.why = why;
                }
                Update::Home(home) => self.fleet.home = Some(home),
            }
        }
        // The interface's own file, read again only when this process wrote
        // it: what it holds is changed from this interface alone.
        let generation = crate::config::interface_state::generation();
        if self
            .interface
            .as_ref()
            .is_none_or(|(seen, _)| *seen != generation)
        {
            self.interface = Some((generation, crate::config::interface_state::load()));
            // Before the first rows there is nothing to lay it over.
            moved |= self.rows_seen;
        }
        let now = now_ms();
        moved |= self.next_stuck_ms.is_some_and(|at| at <= now);
        if !moved {
            return None;
        }
        let stuck_after = DELETION_STUCK_AFTER.as_millis() as i64;
        let stuck_at = |row: &SessionRow| row.deleting.as_ref().map(|d| d.since_ms + stuck_after);
        self.next_stuck_ms = self
            .tasks
            .iter()
            .filter_map(stuck_at)
            .filter(|at| *at > now)
            .min();
        let state = self.interface.as_ref().map(|(_, state)| state);
        self.read.sessions = self
            .tasks
            .iter()
            // A Task marked for deletion is going: its controller is
            // releasing the session. It is shown again only once that has
            // taken longer than a release does.
            .filter(|row| stuck_at(row).is_none_or(|at| at <= now))
            .cloned()
            .map(|mut row| {
                // A Fleet that cannot be asked has not lost its sessions: an
                // unknown observation never means absence.
                if self.lost {
                    row.status = SessionState::Unreachable;
                }
                if let Some(state) = state {
                    row.display_order = state
                        .order
                        .iter()
                        .position(|id| *id == row.id)
                        .map(|at| at as i64);
                }
                row
            })
            .collect();
        let errors: Vec<&str> = [&self.watch_error, &self.manager_error]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        self.read.error = (!errors.is_empty()).then(|| errors.join("; "));
        let mut fleet = self.fleet.clone();
        // A watch that ended says more than the last `version`: the Fleet
        // cannot be asked now.
        if self.lost {
            fleet.state = FleetState::Unreachable;
            fleet.why.clone_from(&self.watch_error);
        }
        self.read.fleets = vec![fleet];
        let read = self.read.clone();
        self.read.removed.clear();
        Some(read)
    }
}

/// The Tasks of every Fleet thurbox lists, as one list: one [`FleetWatch`]
/// per Fleet, each row's id naming its Fleet. Each Fleet's harnesses and
/// places are offered with it, the creation flow's choice of Fleet; the
/// unscoped ones are the Fleet in force's.
pub struct FleetWatches {
    in_force: String,
    watches: Vec<FleetWatch>,
    /// Each watch's last answer, in the order of `watches`.
    reads: Vec<SourceRead>,
}

impl FleetWatches {
    /// Follow `watches` as one list; `in_force` names the Fleet whose
    /// harnesses and places are offered.
    pub fn new(in_force: &str, watches: Vec<FleetWatch>) -> Self {
        let reads = vec![SourceRead::default(); watches.len()];
        Self {
            in_force: in_force.to_string(),
            watches,
            reads,
        }
    }
}

impl SnapshotSource for FleetWatches {
    fn refresh_places(&mut self, fleet: &str) {
        for watch in &mut self.watches {
            watch.refresh_places(fleet);
        }
    }

    fn poll(&mut self) -> Option<SourceRead> {
        let mut removed = Vec::new();
        let mut moved = false;
        for (watch, read) in self.watches.iter_mut().zip(&mut self.reads) {
            if let Some(mut next) = watch.poll() {
                removed.append(&mut next.removed);
                *read = next;
                moved = true;
            }
        }
        if !moved {
            return None;
        }
        let in_force = self
            .watches
            .iter()
            .position(|watch| watch.name == self.in_force)
            .map(|at| &self.reads[at]);
        let errors: Vec<String> = self
            .watches
            .iter()
            .zip(&self.reads)
            .filter_map(|(watch, read)| {
                read.error
                    .as_ref()
                    .map(|why| format!("{}: {why}", watch.name))
            })
            .collect();
        Some(SourceRead {
            sessions: self
                .reads
                .iter()
                .flat_map(|read| read.sessions.iter().cloned())
                .collect(),
            error: (!errors.is_empty()).then(|| errors.join("; ")),
            harnesses: in_force.and_then(|read| read.harnesses.clone()),
            hosts: in_force.and_then(|read| read.hosts.clone()),
            fleets: self
                .reads
                .iter()
                .flat_map(|read| read.fleets.iter().cloned())
                .collect(),
            removed,
        })
    }
}

impl Drop for FleetWatch {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.kill();
            }
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Ask Fleet which places it defines (`fleet runtime places --json`), again
/// every [`RETRY`] until it answers: the places the creation flow offers
/// are Fleet's.
fn read_places(link: &FleetLink, tx: &Sender<Update>) {
    let machine = machine(link);
    loop {
        if let Some(answer) = ask(link, &["runtime", "places", "--json"]) {
            let _ = tx.send(Update::Places(places_from(&answer, &machine)));
            return;
        }
        std::thread::sleep(RETRY);
    }
}

/// Ask a remote Fleet's machine for its home once, bounded as a probe of
/// it is. One that does not answer is left unknown, and the creation flow
/// starts its path field empty rather than at a path it would refuse.
fn read_home(link: &FleetLink, tx: &Sender<Update>) {
    if let Some(home) = link.home(crate::fleet_link::REMOTE_PROBE_TIMEOUT) {
        let _ = tx.send(Update::Home(home));
    }
}

/// How the machine a Fleet runs on reads beside its `local` place.
fn machine(link: &FleetLink) -> String {
    if link.is_local() {
        "this machine".to_string()
    } else {
        format!("{} itself", link.name())
    }
}

/// The places the creation flow offers, from `fleet runtime places --json`:
/// the one Fleet calls `local` first, with an empty backend (a Task names
/// it `local`), then each other place by its `<runtime>/<place>`. `null`
/// (one runtime serves any place) offers `local` alone. `machine` is how
/// the Fleet's own machine reads.
pub(crate) fn places_from(answer: &Value, machine: &str) -> Vec<HostRow> {
    let Some(places) = answer["places"].as_array() else {
        return vec![HostRow::local(machine)];
    };
    let mut rows = Vec::new();
    for place in places {
        let runtime = place["runtime"].as_str().unwrap_or_default();
        let ready = place["ready"].as_bool();
        let why = place["why"].as_str().map(str::to_string);
        if place["alias"].as_str() == Some("local") {
            rows.insert(
                0,
                HostRow {
                    detail: format!("{machine} ({runtime})"),
                    ready,
                    why,
                    ..HostRow::local(machine)
                },
            );
            continue;
        }
        let id = place["place"].as_str().unwrap_or_default().to_string();
        let detail = if place["dynamic"].as_bool() == Some(true) {
            format!("a new machine per session ({runtime})")
        } else {
            format!("a {runtime} place")
        };
        rows.push(HostRow {
            name: id.clone(),
            detail,
            backend: id,
            ready,
            why,
        });
    }
    rows
}

/// One `fleet <args>` answer as JSON, or `None` when it failed.
fn ask(link: &FleetLink, args: &[&str]) -> Option<Value> {
    link.command(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice::<Value>(&out.stdout).ok())
}

/// Ask Fleet for its `version --json` every [`MANAGER_CHECK`], [`RETRY`]
/// while it does not answer: which harnesses it has profiles for (the
/// agents the creation flow offers are Fleet's, and thurbox keeps no list
/// of its own), and whether its manager runs. Only a change is sent.
fn read_version(
    link: &FleetLink,
    tx: &Sender<Update>,
    alive: &std::sync::Weak<()>,
    published: &Published,
) {
    let mut harnesses = None;
    while alive.strong_count() > 0 {
        let (next, answer) = probe(link);
        if let Some(answer) = answer {
            let named = harnesses_of(&answer);
            if harnesses.as_ref() != Some(&named) {
                harnesses = Some(named.clone());
                if tx.send(Update::Harnesses(named)).is_err() {
                    return;
                }
            }
        }
        let answered = next.0 != FleetState::Unreachable;
        if !publish_state(published, tx, next) {
            return;
        }
        std::thread::sleep(if answered && harnesses.is_some() {
            MANAGER_CHECK
        } else {
            RETRY
        });
    }
}

/// The harnesses a `fleet version --json` answer names.
fn harnesses_of(answer: &Value) -> Vec<String> {
    answer["harnesses"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| h.as_str().map(str::to_string))
        .collect()
}

/// One `fleet version --json`: the Fleet's state and why, and its answer
/// when it gave one. A Fleet answers whether its manager holds the lease
/// in `manager` (M4: thurbox never starts a remote one, so it says when
/// none runs).
fn probe(link: &FleetLink) -> ((FleetState, Option<String>), Option<Value>) {
    let out = match link
        .command(&["version", "--json"])
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            let why = format!("could not start fleet: {e}");
            return ((FleetState::Unreachable, Some(why)), None);
        }
    };
    if let Some(why) = link.unreachable(&out) {
        return ((FleetState::Unreachable, Some(why)), None);
    }
    let answer = out
        .status
        .success()
        .then(|| serde_json::from_slice::<Value>(&out.stdout).ok())
        .flatten();
    let Some(answer) = answer else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = stderr
            .lines()
            .find_map(|line| line.strip_prefix("error: "))
            .unwrap_or(stderr.trim());
        return (
            (FleetState::Unknown, Some(format!("fleet version: {why}"))),
            None,
        );
    };
    let manager = &answer["manager"];
    let state = match manager["held"].as_bool() {
        Some(true) => (FleetState::Up, None),
        Some(false) => (FleetState::Down, None),
        None => (
            FleetState::Unknown,
            Some(
                manager["why"]
                    .as_str()
                    .unwrap_or("this Fleet does not report its manager — update Fleet")
                    .to_string(),
            ),
        ),
    };
    (state, Some(answer))
}

/// Run `fleet watch` until the source goes, starting it again whenever it
/// ends.
fn watch(link: &FleetLink, tx: &Sender<Update>, held: &Mutex<Option<Child>>) {
    let mut tasks: BTreeMap<String, Value> = BTreeMap::new();
    let mut removed = Vec::new();
    let mut since = 0u64;
    loop {
        let why = match follow(link, &mut tasks, &mut removed, &mut since, tx, held) {
            Ok(()) => return,
            Err(why) => why,
        };
        if tx.send(Update::Lost(why)).is_err() {
            return;
        }
        std::thread::sleep(RETRY);
    }
}

/// One `fleet watch`, from `since`. `Ok` when the source went; otherwise why
/// the watch ended. `removed` gathers the names of the Tasks that went
/// (deleted, marked for deletion, or replaced by a Task of another uid) until
/// rows are sent with them.
fn follow(
    link: &FleetLink,
    tasks: &mut BTreeMap<String, Value>,
    removed: &mut Vec<String>,
    since: &mut u64,
    tx: &Sender<Update>,
    held: &Mutex<Option<Child>>,
) -> Result<(), String> {
    let mut child = link
        .command(&["watch", "--json", "--since", &since.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start fleet watch: {e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // Drained beside stdout so a chatty stderr cannot stall the watch. A
    // warning (a store it cannot read) is the source's error while the watch
    // runs, until the `synced` that follows the next read that works; the
    // last line is the reason the watch ended.
    let warnings = tx.clone();
    let last_error = std::thread::spawn(move || {
        let mut last = String::new();
        for line in stderr
            .map(BufReader::new)
            .into_iter()
            .flat_map(|r| r.lines())
        {
            match line {
                Ok(line) if !line.trim().is_empty() => {
                    if let Some(warning) = line.strip_prefix("warning: ") {
                        let _ =
                            warnings.send(Update::Watch(Some(format!("fleet watch: {warning}"))));
                    }
                    last = line;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        last
    });
    *held.lock().map_err(|_| "fleet watch: poisoned")? = Some(child);
    let mut synced = false;
    for line in stdout
        .map(BufReader::new)
        .into_iter()
        .flat_map(|r| r.lines())
    {
        let Ok(line) = line else { break };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(revision) = event["revision"].as_u64() {
            *since = (*since).max(revision);
        }
        match event["type"].as_str() {
            Some("changed") => {
                let task = &event["object"];
                if let Some(name) = task["metadata"]["name"].as_str() {
                    let replaced = tasks
                        .get(name)
                        .is_some_and(|held| held["metadata"]["uid"] != task["metadata"]["uid"]);
                    if replaced || !task["metadata"]["deletionTimestamp"].is_null() {
                        removed.push(name.to_string());
                    }
                    tasks.insert(name.to_string(), task.clone());
                }
            }
            Some("deleted") => {
                if let Some(name) = event["name"].as_str() {
                    tasks.remove(name);
                    removed.push(name.to_string());
                }
            }
            // A resumed watch reports only what moved: a Task deleted while
            // it was down is one `synced` does not name.
            Some("synced") => {
                let names: Vec<&str> = event["names"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                tasks.retain(|name, _| {
                    let kept = names.contains(&name.as_str());
                    if !kept {
                        removed.push(name.clone());
                    }
                    kept
                });
                synced = true;
                if tx.send(Update::Watch(None)).is_err() {
                    return stop(held);
                }
            }
            _ => continue,
        }
        if !synced {
            continue;
        }
        let rows = tasks.values().map(|task| row_of(link, task)).collect();
        let gone = std::mem::take(removed)
            .iter()
            .map(|task| super::row_id(link.name(), task))
            .collect();
        if tx.send(Update::Rows(rows, gone)).is_err() {
            return stop(held);
        }
    }
    let status = held
        .lock()
        .ok()
        .and_then(|mut child| child.take())
        .map(|mut child| child.wait());
    let why = last_error.join().unwrap_or_default();
    Err(match status {
        Some(Ok(status)) => format!("fleet watch exited ({status}): {why}"),
        _ => format!("fleet watch ended: {why}"),
    })
}

fn stop(held: &Mutex<Option<Child>>) -> Result<(), String> {
    if let Some(mut child) = held.lock().ok().and_then(|mut child| child.take()) {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

/// Keep a `fleet up` running while the source does: start one, and when it
/// exits, say why and start another after [`MANAGER_CHECK`]. One that
/// exits for a held lease is no error: another manager runs.
fn keep_manager(link: &FleetLink, tx: &Sender<Update>) {
    loop {
        let dir = crate::paths::data_directory();
        let error = match dir
            .ok_or_else(|| "fleet up: no data directory for its log".to_string())
            .and_then(|dir| manage(link, &dir, tx))
        {
            Ok(Some(LEASE_HELD)) => None,
            Ok(code) => Some(format!(
                "fleet up exited{}",
                code.map(|c| format!(" with {c}")).unwrap_or_default()
            )),
            Err(why) => Some(why),
        };
        if tx.send(Update::Manager(error)).is_err() {
            return;
        }
        std::thread::sleep(MANAGER_CHECK);
    }
}

/// One `fleet up`, waited for. Its standard error goes to a file in `dir`,
/// thurbox's own data directory, rather than a pipe: a pipe would end the
/// manager with the interface, the first time it wrote after thurbox exited.
/// The file is read back and removed when the manager exits; one whose
/// manager outlived the interface stays readable, and a later start removes
/// it once that manager is gone ([`clear_old_logs`]).
fn manage(link: &FleetLink, dir: &Path, tx: &Sender<Update>) -> Result<Option<i32>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("fleet up: log: {e}"))?;
    clear_old_logs(dir);
    let dir = dir.to_path_buf();
    let (log, mut reader, path) = open_log(&dir).map_err(|e| format!("fleet up: log: {e}"))?;
    let mut command = link.command(&["up"]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(format!("could not start fleet up: {e}"));
        }
    };
    // Named for the manager from here on, so a later start can tell a log
    // whose manager still runs from one nobody writes.
    let named = dir.join(format!("fleet-up-{}.log", child.id()));
    let path = match std::fs::rename(&path, &named) {
        Ok(()) => named,
        Err(_) => path,
    };
    if tx.send(Update::Manager(None)).is_err() {
        return Ok(Some(LEASE_HELD));
    }
    let status = child.wait().map_err(|e| format!("fleet up: {e}"))?;
    let mut stderr = String::new();
    let _ = reader.rewind();
    let _ = reader.read_to_string(&mut stderr);
    let _ = std::fs::remove_file(&path);
    if status.code() == Some(LEASE_HELD) {
        return Ok(status.code());
    }
    match stderr.lines().find_map(|line| line.strip_prefix("error: ")) {
        Some(why) => Err(format!("fleet up: {why}")),
        None => Ok(status.code()),
    }
}

/// Remove the `fleet up` logs in `dir` whose process is gone: a manager
/// that outlived its interface, and has exited since. A log is named for the
/// manager it holds (`fleet-up-<pid>.log`), or, before the manager started,
/// for the interface (`fleet-up-<pid>-<n>.log`).
fn clear_old_logs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut system = sysinfo::System::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix("fleet-up-"))
            .and_then(|rest| rest.strip_suffix(".log"))
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        let pid = sysinfo::Pid::from_u32(pid);
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        if system.process(pid).is_none() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The `n`th name this process gives a `fleet up` log: unique to it, so two
/// interfaces sharing a data directory never touch each other's.
fn log_name(n: u64) -> String {
    format!("fleet-up-{}-{n}.log", std::process::id())
}

/// `fleet up`'s standard error: a file in `dir`, a handle to read it back
/// (the same file description: rewind before reading), and its path.
///
/// Created exclusively under a name of this process's own: a name already
/// taken, by a link or anything else, is never opened, and the next one is
/// tried instead.
fn open_log(dir: &Path) -> std::io::Result<(std::fs::File, std::fs::File, PathBuf)> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    loop {
        let path = dir.join(log_name(
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(log) => {
                let reader = log.try_clone()?;
                return Ok((log, reader, path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
}

/// One Task as a session row.
///
/// The directory a `file` or `git+file` URL names, without a query or a
/// fragment, `%XX` decoded; `None` for any other URL.
fn local_dir(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("file://")
        .or_else(|| url.strip_prefix("git+file://"))?;
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let mut bytes = Vec::with_capacity(path.len());
    let mut rest = path.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        let hex = tail
            .get(..2)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b, hex) {
            (b'%', Some(decoded)) => {
                bytes.push(decoded);
                rest = &tail[2..];
            }
            _ => {
                bytes.push(b);
                rest = tail;
            }
        }
    }
    String::from_utf8(bytes).ok().filter(|p| !p.is_empty())
}

/// The row of a Task of the Fleet `link` reaches. Its id is
/// `<fleet>:<task>` (M1): two Fleets can each hold a Task of one name, and
/// every command addresses a Task by its name in its Fleet
/// ([`SessionRow::task`]). The row's name is the one the session was given
/// ([`NAME_LABEL`]), or the Task's for a Task made elsewhere.
///
/// `remote_host` says where the session is when that is not this machine:
/// for a Fleet on another machine, that Fleet's name, with the place when it
/// is not the Fleet's `local` (`box · vm/sandbox`); for a Fleet here, a place
/// other than `local`. What a row says of git, panes and hook
/// columns is thurbox's own engine's and stays empty here; the status is
/// Fleet's `agentState` (the agent's hooks, else its screen) over its
/// `sessionState`, and `status.activity` says which source decided it.
pub fn row_of(link: &FleetLink, task: &Value) -> SessionRow {
    let text = |value: &Value| value.as_str().map(str::to_string);
    let (spec, status) = (&task["spec"], &task["status"]);
    let task_name = text(&task["metadata"]["name"]).unwrap_or_default();
    let name = text(&task["metadata"]["labels"][NAME_LABEL]).unwrap_or_else(|| task_name.clone());
    let id = super::row_id(link.name(), &task_name);
    let place = text(&spec["host"]).filter(|place| place != "local");
    let remote_host = match (link.is_local(), place) {
        (true, place) => place,
        (false, None) => Some(link.name().to_string()),
        (false, Some(place)) => Some(format!("{} · {place}", link.name())),
    };
    // Where the session runs once the runtime made its workspace; before,
    // the directory its first URL names on this machine.
    let cwd = text(&status["workspace"]["dirs"][0])
        .or_else(|| spec["workspace"][0].as_str().and_then(local_dir))
        .map(PathBuf::from);
    // Every directory the session spans, in order: the runtime's answer, or
    // before it, the session's own directory.
    let member_dirs: Vec<PathBuf> = status["workspace"]["dirs"]
        .as_array()
        .map(|dirs| {
            dirs.iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from)
                .collect()
        })
        .filter(|dirs: &Vec<PathBuf>| !dirs.is_empty())
        .unwrap_or_else(|| cwd.iter().cloned().collect());
    let session_state = status["sessionState"].as_str();
    let agent_state = status["agentState"].as_str();
    let repo = repo_name(&cwd, None);
    // The source of a status the agent state decided: not of a stopped,
    // lost or gone session's.
    let status_source = match (session_state, &status["activity"]["source"]) {
        (Some("Paused" | "Lost" | "Gone"), _) | (_, Value::Null) => None,
        (_, source) => Some(StatusSource {
            source: text(source).unwrap_or_default(),
            rule: text(&status["activity"]["rule"]),
            since_ms: status["activity"]["at"].as_i64(),
        }),
    };
    SessionRow {
        id,
        name,
        agent: text(&spec["harness"]).unwrap_or_default(),
        // A stopped, lost or gone session is that, whatever its agent last
        // said; otherwise the agent's own report, as v2 showed it, and only
        // with no report what the runtime proved.
        status: match (session_state, agent_state) {
            (Some("Paused"), _) => SessionState::Stopped,
            (Some("Lost" | "Gone"), _) => SessionState::Failed,
            (_, Some("working")) => SessionState::Working,
            (_, Some("attention")) => SessionState::Blocked,
            (_, Some("idle")) => SessionState::Idle,
            (Some("Running"), _) => SessionState::Running,
            (Some("Idle"), _) => SessionState::Idle,
            _ => SessionState::Unreported,
        },
        repos: repo.iter().cloned().collect(),
        repo,
        member_dirs,
        cwd,
        branch: None,
        base_branch: None,
        backend: BACKEND.to_string(),
        // The terminal is the stream `fleet attach` names, not a pane of
        // thurbox's backends.
        backend_id: None,
        remote_host,
        agent_session_id: text(&status["conversation"]),
        // A parent is a Task of the same Fleet.
        parent_id: text(&task["metadata"]["labels"]["gateway.fleet.dev/parent"])
            .map(|parent| super::row_id(link.name(), &parent)),
        display_order: None,
        worktree_count: 0,
        git: None,
        shell_backend_id: None,
        stopped: session_state == Some("Paused"),
        hook_state: None,
        key: text(&status["session"]["key"]),
        reports_as: None,
        detected_agent: None,
        meta: BTreeMap::new(),
        status_source,
        deleting: task["metadata"]["deletionTimestamp"]
            .as_i64()
            .map(|since_ms| Deleting {
                since_ms,
                why: status["conditions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|c| c["type"] == "Released")
                    .and_then(|c| {
                        text(&c["message"])
                            .filter(|m| !m.is_empty())
                            .or_else(|| text(&c["reason"]))
                    }),
            }),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::process::Command;

    use super::*;

    /// The places offered are Fleet's: `local` first, named `local` with no
    /// backend, then each other place by its `<runtime>/<place>`, each with
    /// whether it is ready. A Fleet whose one runtime serves any place
    /// (`null`) offers `local` alone.
    #[test]
    fn the_places_offered_are_fleets_local_first() {
        let answer = serde_json::json!({"places": [
            {"place": "mux/wsl-dev", "runtime": "mux", "alias": null, "ready": false, "why": "no distro"},
            {"place": "mux/local", "runtime": "mux", "alias": "local", "ready": true},
            {"place": "vm/sandbox", "runtime": "vm", "dynamic": true, "ready": null},
        ]});
        let rows = places_from(&answer, "box itself");
        let shown: Vec<(&str, &str, &str, Option<bool>)> = rows
            .iter()
            .map(|r| {
                (
                    r.name.as_str(),
                    r.detail.as_str(),
                    r.backend.as_str(),
                    r.ready,
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("local", "box itself (mux)", "", Some(true)),
                ("mux/wsl-dev", "a mux place", "mux/wsl-dev", Some(false)),
                (
                    "vm/sandbox",
                    "a new machine per session (vm)",
                    "vm/sandbox",
                    None
                ),
            ]
        );
        assert_eq!(rows[1].why.as_deref(), Some("no distro"));
        let any = places_from(&serde_json::json!({"places": null}), "this machine");
        assert_eq!(any, [HostRow::local("this machine")]);
    }

    /// A Fleet's places are read again when the place picker opens on it, so
    /// a place's readiness is the one at that opening, not at startup.
    #[cfg(unix)]
    #[test]
    fn the_places_are_read_again_when_the_place_picker_opens() {
        let remote = crate::fleet_link::testing::Remote::new(
            "here=$(dirname \"$0\")\n\
             case \"$1\" in\n\
             runtime) cat \"$here/places.json\" ;;\n\
             version) echo '{\"harnesses\":[]}' ;;\n\
             watch) exec sleep 30 ;;\n\
             esac",
        );
        let places = remote.dir().join("remote/places.json");
        let answer = |ready: bool| {
            format!(r#"{{"places":[{{"place":"mux/gpu","runtime":"mux","ready":{ready}}}]}}"#)
        };
        std::fs::write(&places, answer(true)).expect("places");
        crate::paths::with_path(remote.path(), || {
            let mut store = super::super::SnapshotStore::with_source(
                Box::new(FleetWatch::spawn(remote.link.clone())),
                &Default::default(),
            );
            let ready = |store: &mut super::super::SnapshotStore| {
                store.refresh_if_due();
                store
                    .current()
                    .fleets
                    .first()?
                    .places
                    .as_ref()?
                    .iter()
                    .find(|p| p.name == "mux/gpu")?
                    .ready
            };
            let until = |store: &mut super::super::SnapshotStore, want: Option<bool>| {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while ready(store) != want {
                    assert!(std::time::Instant::now() < deadline, "never {want:?}");
                    std::thread::sleep(Duration::from_millis(20));
                }
            };
            until(&mut store, Some(true));
            std::fs::write(&places, answer(false)).expect("places");
            store.want_places(Some("devbox".into()));
            until(&mut store, Some(false));
            // Asked once per opening: a second opening reads again.
            store.want_places(None);
            std::fs::write(&places, answer(true)).expect("places");
            store.want_places(Some("devbox".into()));
            until(&mut store, Some(true));
        });
    }

    /// A refresh that fails once (ssh exits 255 while the place picker
    /// opens) marks the Fleet unreachable, and the next `version` poll that
    /// answers marks it up again: the published state has one owner.
    #[cfg(unix)]
    #[test]
    fn a_fleet_unreachable_for_one_refresh_is_up_again_at_the_next_poll() {
        let remote = crate::fleet_link::testing::Remote::new(
            "here=$(dirname \"$0\")\n\
             case \"$1\" in\n\
             version) [ -e \"$here/down\" ] && { echo 'no route' >&2; exit 255; }; echo '{\"manager\":{\"held\":true}}' ;;\n\
             runtime) [ -e \"$here/down\" ] && exit 255; echo '{\"places\":null}' ;;\n\
             watch) exec sleep 60 ;;\n\
             esac",
        );
        let down = remote.dir().join("remote/down");
        crate::paths::with_path(remote.path(), || {
            let mut store = super::super::SnapshotStore::with_source(
                Box::new(FleetWatch::spawn(remote.link.clone())),
                &Default::default(),
            );
            let until = |store: &mut super::super::SnapshotStore, want: FleetState| {
                let deadline = std::time::Instant::now() + Duration::from_secs(25);
                loop {
                    store.refresh_if_due();
                    let state = store.current().fleets.first().map(|f| f.state);
                    if state == Some(want) {
                        return;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "never {want:?}: {state:?}"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
            };
            until(&mut store, FleetState::Up);
            std::fs::write(&down, "").expect("down");
            store.want_places(Some("devbox".into()));
            until(&mut store, FleetState::Unreachable);
            std::fs::remove_file(&down).expect("up");
            until(&mut store, FleetState::Up);
        });
    }

    /// A Fleet's state is its manager's lease, from `fleet version --json`:
    /// up while it is held, down when it is not, and unreachable when ssh
    /// itself fails (exit 255).
    #[cfg(unix)]
    #[test]
    fn a_fleets_state_is_its_managers_lease() {
        for (answer, state) in [
            (r#"{"manager":{"held":true}}"#, FleetState::Up),
            (r#"{"manager":{"held":false}}"#, FleetState::Down),
            (
                r#"{"manager":{"held":null,"why":"no store"}}"#,
                FleetState::Unknown,
            ),
        ] {
            let remote = crate::fleet_link::testing::Remote::new(&format!("echo '{answer}'"));
            let ((got, _), _) = crate::paths::with_path(remote.path(), || probe(&remote.link));
            assert_eq!(got, state, "{answer}");
        }
    }

    /// Every call the source makes of Fleet goes through its link: the
    /// watch, the manager, and the `version` and `runtime places` reads, each
    /// logged by a stand-in `ssh` in front of a Fleet only that `ssh` can
    /// reach.
    ///
    /// The manager is run by itself and waited for rather than through
    /// [`FleetWatch::with_manager`]: that worker starts `fleet up` again
    /// every [`MANAGER_CHECK`], and one outliving the test would do so after
    /// the stand-in `ssh` left `PATH`.
    #[cfg(unix)]
    #[test]
    fn every_call_of_the_source_goes_through_its_link() {
        let remote = crate::fleet_link::testing::Remote::new(
            "case \"$1\" in\n\
             version) echo '{\"harnesses\":[\"claude\"]}' ;;\n\
             runtime) echo '{\"places\":[{\"place\":\"mux/local\",\"runtime\":\"mux\",\"alias\":\"local\",\"ready\":true},{\"place\":\"mux/devbox\",\"runtime\":\"mux\",\"ready\":false,\"why\":\"no tmux\"}]}' ;;\n\
             watch) echo '{\"type\":\"synced\",\"revision\":1,\"names\":[]}'; exec sleep 30 ;;\n\
             up) exit 6 ;;\n\
             esac",
        );
        crate::paths::with_path(remote.path(), || {
            let (tx, _rx) = channel();
            let data = tempfile::tempdir().expect("data");
            assert_eq!(manage(&remote.link, data.path(), &tx), Ok(Some(LEASE_HELD)));
            let mut source = FleetWatch::start(remote.link.clone(), false);
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut read = SourceRead::default();
            while read.harnesses.is_none()
                || read.hosts.is_none()
                || !remote.called("watch --json --since 0")
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "calls: {:?}",
                    remote.calls()
                );
                if let Some(next) = source.poll() {
                    read.harnesses = read.harnesses.or(next.harnesses);
                    read.hosts = read.hosts.or(next.hosts);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(read.harnesses, Some(vec!["claude".to_string()]));
            let places: Vec<(String, String, Option<bool>)> = read
                .hosts
                .unwrap()
                .into_iter()
                .map(|p| (p.name, p.backend, p.ready))
                .collect();
            assert_eq!(
                places,
                [
                    ("local".into(), String::new(), Some(true)),
                    ("mux/devbox".into(), "mux/devbox".into(), Some(false)),
                ]
            );
            drop(source);
        });
        for call in ["up", "version --json", "runtime places --json"] {
            assert!(remote.called(call), "{call}: {:?}", remote.calls());
        }
    }

    /// thurbox never starts `fleet up` on a Fleet on another machine (M4):
    /// asked to keep a manager, the source follows that Fleet and starts
    /// none.
    #[cfg(unix)]
    #[test]
    fn a_remote_fleet_gets_no_manager() {
        let remote = crate::fleet_link::testing::Remote::new(
            "case \"$1\" in\n\
             version) echo '{\"harnesses\":[]}' ;;\n\
             runtime) echo '{\"hosts\":null}' ;;\n\
             watch) echo '{\"type\":\"synced\",\"revision\":1,\"names\":[]}'; exec sleep 30 ;;\n\
             up) exit 6 ;;\n\
             esac",
        );
        crate::paths::with_path(remote.path(), || {
            let mut source = FleetWatch::with_manager(remote.link.clone());
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut harnesses = None;
            while harnesses.is_none() || !remote.called("watch --json --since 0") {
                assert!(
                    std::time::Instant::now() < deadline,
                    "calls: {:?}",
                    remote.calls()
                );
                if let Some(next) = source.poll() {
                    harnesses = harnesses.or(next.harnesses);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            drop(source);
        });
        assert!(!remote.called("up"), "{:?}", remote.calls());
    }

    /// A session over several repositories spans every directory the runtime
    /// made, in order: the editor (`Ctrl+O`) opens them all, not the first.
    #[test]
    fn a_row_spans_every_directory_of_its_workspace() {
        let task = serde_json::json!({
            "metadata": {"name": "pair"},
            "spec": {"harness": "claude", "host": "local", "workspace": [
                "git+file:///src/a?branch=pair", "git+file:///src/b?branch=pair"
            ]},
            "status": {"workspace": {"key": "k", "host": "local", "dirs": ["/w/k/a", "/w/k/b"]}},
        });
        let row = row_of(&FleetLink::local("fleet"), &task);
        assert_eq!(row.cwd, Some(PathBuf::from("/w/k/a")));
        assert_eq!(
            row.member_dirs,
            vec![PathBuf::from("/w/k/a"), PathBuf::from("/w/k/b")]
        );
    }

    /// A row names where its agent state came from, but a paused, lost or
    /// gone session's status is not the agent state's.
    #[test]
    fn a_row_carries_the_source_of_its_agent_state() {
        let task = |session_state: &str| {
            serde_json::json!({
                "metadata": {"name": "t"},
                "spec": {"harness": "codex", "host": "local", "workspace": []},
                "status": {"sessionState": session_state, "agentState": "attention",
                           "activity": {"source": "screen", "rule": "codex/action-required", "at": 7}},
            })
        };
        let row = row_of(&FleetLink::local("fleet"), &task("Running"));
        assert_eq!(row.status, SessionState::Blocked);
        assert_eq!(
            row.status_source,
            Some(StatusSource {
                source: "screen".into(),
                rule: Some("codex/action-required".into()),
                since_ms: Some(7),
            })
        );
        assert_eq!(
            row_of(&FleetLink::local("fleet"), &task("Paused")).status_source,
            None
        );
        let mut bare = task("Running");
        bare["status"].as_object_mut().unwrap().remove("activity");
        assert_eq!(
            row_of(&FleetLink::local("fleet"), &bare).status_source,
            None
        );
    }

    /// The agents a session can run are the harnesses Fleet has profiles
    /// for, read from `fleet version --json`: thurbox keeps no list of its own.
    #[cfg(unix)]
    #[test]
    fn the_agents_are_the_harnesses_fleet_reports() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fleet = dir.path().join("fleet");
        std::fs::write(
            &fleet,
            "#!/bin/sh\n[ \"$1\" = version ] && exec echo '{\"harnesses\":[\"claude-code\",\"codex\"]}'\nexec sleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut source = FleetWatch::spawn(FleetLink::local(fleet.to_string_lossy()));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let harnesses = loop {
            if let Some(harnesses) = source.poll().and_then(|read| read.harnesses) {
                break harnesses;
            }
            assert!(std::time::Instant::now() < deadline, "Fleet never answered");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(harnesses, ["claude-code", "codex"]);
    }

    /// Whatever stands where a log would go, a link included, is never
    /// opened: what it pointed at keeps its contents.
    #[cfg(unix)]
    #[test]
    fn a_planted_link_at_the_log_path_is_not_followed() {
        let dir = tempfile::tempdir().expect("dir");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep").expect("victim");
        for name in (0..64).map(log_name) {
            std::os::unix::fs::symlink(&victim, dir.path().join(name)).expect("link");
        }

        let (mut log, mut reader, _) = open_log(dir.path()).expect("a free name");
        std::io::Write::write_all(&mut log, b"error: written\n").expect("write");
        let mut read = String::new();
        reader.rewind().expect("rewind");
        reader.read_to_string(&mut read).expect("read");
        assert_eq!(read, "error: written\n");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    }

    /// A stand-in `fleet` running `script` as `fleet up`.
    #[cfg(unix)]
    fn fake_fleet(dir: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("fleet");
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).expect("fleet");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        program
    }

    /// A Task deleted and made again under its name between two polls is
    /// reported removed, though its name is still in the rows: the snapshot
    /// stops hiding the deleted one on that, not on the name going.
    #[cfg(unix)]
    #[test]
    fn a_task_replaced_between_two_polls_is_reported_removed() {
        let bin = tempfile::tempdir().expect("bin");
        let event = |value: Value| format!("echo '{value}'");
        let task = |name: &str, uid: &str, revision: u64| {
            event(serde_json::json!({"type": "changed", "revision": revision,
                "object": {"metadata": {"name": name, "uid": uid}}}))
        };
        let script = [
            "[ \"$1\" = watch ] || exit 1".to_string(),
            task("gone", "u1", 1),
            event(serde_json::json!({"type": "synced", "revision": 1, "names": ["gone"]})),
            "sleep 1".to_string(),
            event(serde_json::json!({"type": "deleted", "revision": 2, "name": "gone"})),
            task("gone", "u2", 2),
            task("kept", "u3", 3),
            "exec sleep 30".to_string(),
        ]
        .join("\n");
        let mut source = FleetWatch::spawn(FleetLink::local(
            fake_fleet(bin.path(), &script).to_string_lossy(),
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let (mut removed, mut ids) = (Vec::new(), Vec::new());
        while !ids.iter().any(|id| id == "local:kept") {
            assert!(std::time::Instant::now() < deadline, "no second read");
            // Slower than the script, so the replacement lands in one poll.
            std::thread::sleep(Duration::from_millis(1500));
            if let Some(read) = source.poll() {
                removed.extend(read.removed);
                ids = read.sessions.into_iter().map(|row| row.id).collect();
            }
        }
        assert_eq!(removed, ["local:gone"]);
        assert!(ids.iter().any(|id| id == "local:gone"), "{ids:?}");
    }

    /// A Task marked for deletion is off the list until its teardown has run
    /// past [`DELETION_STUCK_AFTER`], then back, marked, though the watch
    /// said nothing more; one Fleet drops in time is never listed.
    #[cfg(unix)]
    #[test]
    fn a_deletion_is_listed_again_only_once_it_runs_past_the_timeout() {
        let bin = tempfile::tempdir().expect("bin");
        let stuck_after = DELETION_STUCK_AFTER.as_millis() as i64;
        let task = |name: &str, marked: i64| {
            serde_json::json!({"type": "changed", "revision": 1, "object": {
                "metadata": {"name": name, "uid": name, "deletionTimestamp": marked}}})
        };
        let script = [
            "[ \"$1\" = watch ] || exit 1".to_string(),
            format!("echo '{}'", task("stuck", now_ms() - stuck_after + 2_000)),
            format!("echo '{}'", task("prompt", now_ms())),
            r#"echo '{"type": "synced", "revision": 1, "names": ["prompt", "stuck"]}'"#.into(),
            "sleep 1".into(),
            r#"echo '{"type": "deleted", "revision": 2, "name": "prompt"}'"#.into(),
            "exec sleep 30".into(),
        ]
        .join("\n");
        let mut source = FleetWatch::spawn(FleetLink::local(
            fake_fleet(bin.path(), &script).to_string_lossy(),
        ));
        let started = std::time::Instant::now();
        let mut seen: Vec<(Duration, Vec<String>)> = Vec::new();
        while started.elapsed() < Duration::from_secs(5) {
            if let Some(read) = source.poll() {
                let ids = read.sessions.iter().map(|row| row.id.clone()).collect();
                seen.push((started.elapsed(), ids));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            seen.iter()
                .all(|(_, ids)| !ids.contains(&"local:prompt".to_string())),
            "{seen:?}"
        );
        let shown = seen
            .iter()
            .find(|(_, ids)| ids.contains(&"local:stuck".to_string()))
            .unwrap_or_else(|| panic!("never listed as stuck: {seen:?}"));
        assert!(
            shown.0 >= Duration::from_millis(1_000),
            "listed early: {seen:?}"
        );
    }

    #[cfg(unix)]
    fn logs(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("dir")
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("fleet-up-"))
            .collect();
        names.sort();
        names
    }

    /// A manager that outlives the interface keeps writing to a log that is
    /// still there to read, named for its pid.
    #[cfg(unix)]
    #[test]
    fn a_running_managers_log_stays_readable() {
        let bin = tempfile::tempdir().expect("bin");
        let data = tempfile::tempdir().expect("data");
        let program = fake_fleet(bin.path(), "echo 'warning: still here' >&2; exec sleep 30");
        let (tx, _rx) = channel();
        let dir = data.path().to_path_buf();
        std::thread::spawn(move || {
            let _ = manage(&FleetLink::local(program.to_string_lossy()), &dir, &tx);
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let (name, text) = loop {
            let found = logs(data.path()).into_iter().find_map(|name| {
                let text = std::fs::read_to_string(data.path().join(&name)).ok()?;
                text.contains("still here").then_some((name, text))
            });
            if let Some(found) = found {
                break found;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no readable log: {:?}",
                logs(data.path())
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let pid: u32 = name
            .trim_start_matches("fleet-up-")
            .trim_end_matches(".log")
            .parse()
            .unwrap_or_else(|_| panic!("named for the manager's pid: {name}"));
        let _ = Command::new("kill").arg(pid.to_string()).status();
        assert!(text.contains("warning: still here"));
    }

    /// A manager that exits is read back, and its log goes.
    #[cfg(unix)]
    #[test]
    fn an_exited_managers_log_is_read_then_removed() {
        let bin = tempfile::tempdir().expect("bin");
        let data = tempfile::tempdir().expect("data");
        let program = fake_fleet(bin.path(), "echo 'error: no starter' >&2; exit 1");
        let (tx, _rx) = channel();
        let why = manage(
            &FleetLink::local(program.to_string_lossy()),
            data.path(),
            &tx,
        )
        .expect_err("it failed");
        assert_eq!(why, "fleet up: no starter");
        assert_eq!(logs(data.path()), Vec::<String>::new());
    }

    /// A log whose process is gone is cleared at the next start; one whose
    /// process runs is kept.
    #[cfg(unix)]
    #[test]
    fn logs_of_gone_processes_are_cleared() {
        let data = tempfile::tempdir().expect("data");
        let mut gone = Command::new("true").spawn().expect("true");
        let gone_pid = gone.id();
        gone.wait().expect("wait");
        let alive = std::process::id();
        for pid in [gone_pid, alive] {
            std::fs::write(data.path().join(format!("fleet-up-{pid}.log")), "x").expect("log");
        }
        clear_old_logs(data.path());
        assert_eq!(logs(data.path()), vec![format!("fleet-up-{alive}.log")]);
    }

    /// Two interfaces sharing a data directory each get a log of their own.
    #[test]
    fn each_manager_start_has_a_log_of_its_own() {
        let dir = tempfile::tempdir().expect("dir");
        let (mut first, mut first_read, first_path) = open_log(dir.path()).expect("first");
        let (mut second, mut second_read, second_path) = open_log(dir.path()).expect("second");
        assert_ne!(first_path, second_path);
        std::io::Write::write_all(&mut first, b"first").expect("write");
        std::io::Write::write_all(&mut second, b"second").expect("write");
        for (reader, want) in [(&mut first_read, "first"), (&mut second_read, "second")] {
            let mut read = String::new();
            reader.rewind().expect("rewind");
            reader.read_to_string(&mut read).expect("read");
            assert_eq!(read, want);
        }
        assert!(first_path.is_file() && second_path.is_file());
    }
}
