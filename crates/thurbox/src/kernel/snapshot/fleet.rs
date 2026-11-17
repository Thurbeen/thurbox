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

/// How often a [`FleetWatch`]'s workers ask Fleet again: after 2 s while
/// it does not answer, and every 10 s once it does, unless a test needs
/// them sooner.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct Cadence {
    pub retry: Duration,
    pub manager_check: Duration,
}

impl Default for Cadence {
    fn default() -> Self {
        Self {
            retry: RETRY,
            manager_check: MANAGER_CHECK,
        }
    }
}

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
    /// Why thurbox cannot speak to this Fleet: nothing more is asked of it.
    Incompatible(String),
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
    /// Set while a remote Fleet's home is being asked for.
    reading_home: Arc<std::sync::atomic::AtomicBool>,
    /// Set once the Fleet's `version` said it speaks the API thurbox
    /// writes: nothing but that `version` is asked of it before.
    admitted: Arc<std::sync::atomic::AtomicBool>,
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
    // Incompatible is final: a later probe's answer never replaces it.
    if last.as_ref() == Some(&next)
        || last
            .as_ref()
            .is_some_and(|(state, _)| *state == FleetState::Incompatible)
    {
        return true;
    }
    *last = Some(next.clone());
    tx.send(Update::State(next.0, next.1)).is_ok()
}

impl FleetWatch {
    /// Follow the Tasks of the Fleet `link` reaches.
    pub fn spawn(link: FleetLink) -> Self {
        Self::start(link, false, Cadence::default())
    }

    /// [`Self::spawn`], asking Fleet again at `cadence`.
    #[doc(hidden)]
    pub fn spawn_every(link: FleetLink, cadence: Cadence) -> Self {
        Self::start(link, false, cadence)
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
        Self::start(link, true, Cadence::default())
    }

    fn start(link: FleetLink, manager: bool, cadence: Cadence) -> Self {
        let (tx, updates) = channel();
        // The first poll publishes the Fleet, connecting, before it answers.
        let _ = tx.send(Update::State(FleetState::Connecting, None));
        let alive = Arc::new(());
        let published = Published::default();
        let reading_home = Arc::new(std::sync::atomic::AtomicBool::new(!link.is_local()));
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
        // Nothing else is asked of a Fleet before its `version` says it
        // speaks the API thurbox writes: its home, the manager, the places
        // and the watch start once it has.
        let admitted_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let admitted: Box<dyn FnOnce() + Send> = {
            let (link, tx, reading) = (link.clone(), tx.clone(), reading_home.clone());
            let manager = manager && link.is_local();
            let flag = admitted_flag.clone();
            Box::new(move || {
                flag.store(true, std::sync::atomic::Ordering::Release);
                if !link.is_local() {
                    let (link, tx) = (link.clone(), tx.clone());
                    std::thread::spawn(move || {
                        read_home(&link, &tx);
                        reading.store(false, std::sync::atomic::Ordering::Release);
                    });
                }
                if manager {
                    let (link, tx) = (link.clone(), tx.clone());
                    std::thread::spawn(move || keep_manager(&link, &tx, cadence.manager_check));
                }
                {
                    let (link, tx) = (link.clone(), tx.clone());
                    std::thread::spawn(move || read_places(&link, &tx, cadence.retry));
                }
                std::thread::spawn(move || watch(&link, &tx, &held, cadence.retry));
            })
        };
        {
            let (link, tx, alive, published) = (
                link.clone(),
                tx.clone(),
                Arc::downgrade(&alive),
                published.clone(),
            );
            std::thread::spawn(move || {
                read_version(&link, &tx, &alive, &published, admitted, cadence);
            });
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
            reading_home,
            admitted: admitted_flag,
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
        // Nothing is asked of a Fleet before it is admitted, nor ever of one
        // thurbox cannot speak to.
        if fleet != self.name
            || !self.admitted.load(Ordering::Acquire)
            || self.refreshing.swap(true, Ordering::AcqRel)
        {
            return;
        }
        // A home not read at startup is asked for again with the places, on
        // a worker of its own: a slow one must not hold the next refresh.
        // Once read it is kept; it is not asked of a Fleet that cannot be
        // reached, nor again while one ask is on its way.
        if !self.link.is_local()
            && self.fleet.home.is_none()
            && self.fleet.state != FleetState::Unreachable
            && !self.reading_home.swap(true, Ordering::AcqRel)
        {
            let (link, tx, reading) = (
                self.link.clone(),
                self.tx.clone(),
                self.reading_home.clone(),
            );
            std::thread::spawn(move || {
                read_home(&link, &tx);
                reading.store(false, Ordering::Release);
            });
        }
        let (link, tx, refreshing, published) = (
            self.link.clone(),
            self.tx.clone(),
            self.refreshing.clone(),
            self.published.clone(),
        );
        std::thread::spawn(move || {
            let places = ask(&link, &["runtime", "places", "--json"]);
            let (state, answer) = probe(&link);
            // Done before anything is sent: an opening that follows what
            // this one reads is a new refresh, never one skipped as running.
            refreshing.store(false, Ordering::Release);
            if let Some(places) = places {
                let _ = tx.send(Update::Places(places_from(&places, &machine(&link))));
            }
            if let Some(answer) = answer {
                let _ = tx.send(Update::Harnesses(harnesses_of(&answer)));
            }
            publish_state(&published, &tx, state);
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
                // Incompatible is final: nothing said after replaces it.
                Update::Lost(_) | Update::State(..)
                    if self.fleet.state == FleetState::Incompatible => {}
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
                Update::Incompatible(why) => {
                    self.lost = false;
                    self.fleet.state = FleetState::Incompatible;
                    self.fleet.why = Some(why.clone());
                    self.watch_error = Some(why);
                }
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
/// every `retry` ([`RETRY`]) until it answers: the places the creation flow offers
/// are Fleet's.
fn read_places(link: &FleetLink, tx: &Sender<Update>, retry: Duration) {
    let machine = machine(link);
    loop {
        if let Some(answer) = ask(link, &["runtime", "places", "--json"]) {
            let _ = tx.send(Update::Places(places_from(&answer, &machine)));
            return;
        }
        std::thread::sleep(retry);
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

/// Ask Fleet for its `version --json` at `cadence` (every [`MANAGER_CHECK`], [`RETRY`]
/// while it does not answer): which harnesses it has profiles for (the
/// agents the creation flow offers are Fleet's, and thurbox keeps no list
/// of its own), and whether its manager runs. Only a change is sent.
///
/// The first answer admits the Fleet (`admitted` runs) only when it speaks
/// [`crate::fleet_link::TASK_API`]; one that does not is incompatible, and
/// nothing more is asked of it. A Fleet that does not answer is unreachable
/// meanwhile, never incompatible: that is known only from its answer.
fn read_version(
    link: &FleetLink,
    tx: &Sender<Update>,
    alive: &std::sync::Weak<()>,
    published: &Published,
    admitted: Box<dyn FnOnce() + Send>,
    cadence: Cadence,
) {
    let mut admitted = Some(admitted);
    let mut harnesses = None;
    while alive.strong_count() > 0 {
        let (next, answer) = probe(link);
        // Not admitted yet and no answer: the list says why, as it would of
        // a watch that ended, so a Fleet that never answers is not silent.
        if answer.is_none() && admitted.is_some() {
            let why = next
                .1
                .clone()
                .unwrap_or_else(|| "fleet version gave no answer".into());
            if tx.send(Update::Lost(why)).is_err() {
                return;
            }
        }
        if next.0 == FleetState::Incompatible {
            let why = next.1.clone().unwrap_or_default();
            publish_state(published, tx, next);
            let _ = tx.send(Update::Incompatible(why));
            return;
        }
        if let Some(answer) = answer {
            if let Some(admit) = admitted.take() {
                admit();
            }
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
            cadence.manager_check
        } else {
            cadence.retry
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
/// when it gave one. A Fleet whose answer lacks the API thurbox writes is
/// [`FleetState::Incompatible`] ([`crate::fleet_link::incompatible`]). A Fleet answers whether its manager holds the lease
/// in `manager` (M4: thurbox never starts a remote one, so it says when
/// none runs).
fn probe(link: &FleetLink) -> ((FleetState, Option<String>), Option<Value>) {
    // Within the Fleet's probe deadline: one that does not answer in time
    // is unreachable, never left hanging.
    let out = match link.bounded_output(&["version", "--json"], link.probe_timeout()) {
        Ok(out) => out,
        Err(why) => return ((FleetState::Unreachable, Some(why)), None),
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
    // Admission is decided here, once: every reader of a Fleet's state
    // (the `version` worker, a place refresh) sees the same verdict.
    if let Some(why) = crate::fleet_link::incompatible(&answer) {
        return ((FleetState::Incompatible, Some(why)), Some(answer));
    }
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
fn watch(link: &FleetLink, tx: &Sender<Update>, held: &Mutex<Option<Child>>, retry: Duration) {
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
        std::thread::sleep(retry);
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
/// exits, say why and start another `every` ([`MANAGER_CHECK`]) later. One that
/// exits for a held lease is no error: another manager runs. Nor is one that
/// was stopped (exit 0), as [`restart_manager`] stops it after an update.
fn keep_manager(link: &FleetLink, tx: &Sender<Update>, every: Duration) {
    loop {
        let dir = crate::paths::data_directory();
        let error = match dir
            .ok_or_else(|| "fleet up: no data directory for its log".to_string())
            .and_then(|dir| manage(link, &dir, tx))
        {
            Ok(Some(LEASE_HELD | 0)) => None,
            Ok(code) => Some(format!(
                "fleet up exited{}",
                code.map(|c| format!(" with {c}")).unwrap_or_default()
            )),
            Err(why) => Some(why),
        };
        if tx.send(Update::Manager(error)).is_err() {
            return;
        }
        std::thread::sleep(every);
    }
}

/// One `fleet up`, waited for. Its standard error goes to a file in `dir`,
/// thurbox's own data directory, rather than a pipe: a pipe would end the
/// manager with the interface, the first time it wrote after thurbox exited.
/// The file is read back and removed when the manager exits; one whose
/// manager outlived the interface stays readable, and a later start removes
/// it once that manager is gone ([`clear_old_logs`]).
fn manage(link: &FleetLink, dir: &Path, tx: &Sender<Update>) -> Result<Option<i32>, String> {
    let started = start_manager(link, dir)?;
    if tx.send(Update::Manager(None)).is_err() {
        return Ok(Some(LEASE_HELD));
    }
    started.wait()
}

/// A `fleet up` started in a process group of its own, its standard error
/// in a log named for it.
struct StartedManager {
    child: Child,
    reader: std::fs::File,
    log: PathBuf,
}

impl StartedManager {
    /// Its exit code, or the error it wrote; its log is read back and removed.
    fn wait(mut self) -> Result<Option<i32>, String> {
        let status = self.child.wait().map_err(|e| format!("fleet up: {e}"))?;
        let mut stderr = String::new();
        let _ = self.reader.rewind();
        let _ = self.reader.read_to_string(&mut stderr);
        let _ = std::fs::remove_file(&self.log);
        if status.code() == Some(LEASE_HELD) {
            return Ok(status.code());
        }
        match stderr.lines().find_map(|line| line.strip_prefix("error: ")) {
            Some(why) => Err(format!("fleet up: {why}")),
            None => Ok(status.code()),
        }
    }
}

fn start_manager(link: &FleetLink, dir: &Path) -> Result<StartedManager, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("fleet up: log: {e}"))?;
    clear_old_logs(dir);
    let dir = dir.to_path_buf();
    let (log, reader, path) = open_log(&dir).map_err(|e| format!("fleet up: log: {e}"))?;
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
    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(format!("could not start fleet up: {e}"));
        }
    };
    // Named for the manager from here on, so a later start can tell a log
    // whose manager still runs from one nobody writes.
    let named = dir.join(format!("fleet-up-{}.log", child.id()));
    let log = match std::fs::rename(&path, &named) {
        Ok(()) => named,
        Err(_) => path,
    };
    Ok(StartedManager { child, reader, log })
}

/// How long a new `fleet up` may take to take the lease.
const START_BOUND: Duration = Duration::from_secs(60);

/// How often the lease is read while a restart waits on it.
const LEASE_POLL: Duration = Duration::from_millis(100);

/// How long a released lease is left for another starter to take before
/// thurbox starts a manager itself: a service supervisor that restarts
/// `fleet up`, or an interface that keeps one running.
const TAKEOVER_GRACE: Duration = Duration::from_secs(2);

/// What [`restart_manager`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    /// No `fleet up` held the lease, and none was started.
    NotRunning,
    /// A `fleet up` runs, but the `program` it would restart on is not the
    /// `fleet` the update installed: it is left running.
    NotReplaced { program: String },
    /// The `fleet up` that held the lease stopped, and `started` holds it.
    Restarted { stopped: u32, started: u32 },
}

/// Restart the `fleet up` that holds the lease of the Fleet on this machine
/// `link` reaches, so it runs `installed`, the `fleet` an update just
/// replaced. A Fleet whose `program` resolves to another file is left alone:
/// restarting it would run the same binary again.
///
/// The old manager is stopped as Ctrl-C stops it (SIGTERM): it finishes its
/// sweep, releases the lease and leaves every session running. That sweep is
/// bounded by Fleet's own call timeouts, which run to minutes, so the wait
/// follows the lease rather than a clock of thurbox's. The new one starts as
/// the interface starts one, with the same `fleet` command line and
/// environment, but naming `installed` itself rather than the name `PATH`
/// would resolve again, unless another starter takes the lease first. A Fleet with
/// no manager running is left without one.
pub fn restart_manager(link: &FleetLink, installed: &Path) -> Result<Restart, String> {
    let Some(stopped) = lease_holder(link)? else {
        return Ok(Restart::NotRunning);
    };
    if !starts(link, installed) {
        return Ok(Restart::NotReplaced {
            program: link.program().to_string(),
        });
    }
    let link = &link.with_program(installed.to_string_lossy());
    terminate(stopped)?;
    let holder = loop {
        let holder = lease_holder(link)?;
        if holder != Some(stopped) {
            break holder;
        }
        std::thread::sleep(LEASE_POLL);
    };
    let taken = match holder {
        Some(started) => Some(started),
        None => poll_lease(link, TAKEOVER_GRACE, Option::is_some)?.flatten(),
    };
    if let Some(started) = taken {
        return Ok(Restart::Restarted { stopped, started });
    }
    let dir = crate::paths::data_directory()
        .ok_or_else(|| "fleet up: no data directory for its log".to_string())?;
    let (tx, exited) = channel();
    let manager = start_manager(link, &dir)?;
    std::thread::spawn(move || {
        let _ = tx.send(manager.wait());
    });
    let deadline = std::time::Instant::now() + START_BOUND;
    loop {
        if let Some(started) = lease_holder(link)? {
            return Ok(Restart::Restarted { stopped, started });
        }
        match exited.try_recv() {
            Ok(Ok(Some(LEASE_HELD))) => {}
            Ok(Ok(code)) => {
                return Err(format!(
                    "fleet up exited{}",
                    code.map(|c| format!(" with {c}")).unwrap_or_default()
                ))
            }
            Ok(Err(why)) => return Err(why),
            Err(_) => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err("the new fleet up did not take the lease".to_string());
        }
        std::thread::sleep(LEASE_POLL);
    }
}

/// Whether the `fleet up` `link` starts runs the file `installed`: its
/// `program` as a path, or as a name found on `PATH`.
fn starts(link: &FleetLink, installed: &Path) -> bool {
    let program = link.program();
    let resolved = if Path::new(program).components().count() > 1 {
        Some(PathBuf::from(program))
    } else {
        crate::paths::resolve_on_path(program).or_else(|| {
            crate::paths::resolve_on_path(&format!("{program}{}", std::env::consts::EXE_SUFFIX))
        })
    };
    let canonical = |path: &Path| std::fs::canonicalize(path).ok();
    resolved
        .is_some_and(|path| canonical(&path).is_some() && canonical(&path) == canonical(installed))
}

/// The lease holder each `fleet version --json` names, until `done` holds
/// for one, or `None` once `bound` has passed.
fn poll_lease(
    link: &FleetLink,
    bound: Duration,
    done: impl Fn(&Option<u32>) -> bool,
) -> Result<Option<Option<u32>>, String> {
    let deadline = std::time::Instant::now() + bound;
    loop {
        let holder = lease_holder(link)?;
        if done(&holder) {
            return Ok(Some(holder));
        }
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(LEASE_POLL);
    }
}

/// The pid of the `fleet up` holding the manager lease, from `fleet version
/// --json`'s `manager`: `None` when none holds it. A lease that cannot be
/// read is an error, never "not running".
fn lease_holder(link: &FleetLink) -> Result<Option<u32>, String> {
    let version = link.version()?;
    let manager = &version["manager"];
    match manager["held"].as_bool() {
        Some(false) => Ok(None),
        Some(true) => manager["holder"]
            .as_str()
            .and_then(|holder| holder.strip_prefix("fleet-up-"))
            .and_then(|process| process.split('@').next())
            .and_then(|pid| pid.parse().ok())
            .map(Some)
            .ok_or_else(|| format!("the manager lease names no fleet up: {manager}")),
        None => Err(format!(
            "the manager lease cannot be read: {}",
            manager["why"]
                .as_str()
                .unwrap_or("fleet version reports none")
        )),
    }
}

/// Ask the `fleet up` `pid` to stop, as Ctrl-C does. A pid that is not a
/// running `fleet` is never signalled.
fn terminate(pid: u32) -> Result<(), String> {
    let id = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[id]), true);
    let process = system
        .process(id)
        .filter(|process| process.name().to_string_lossy().starts_with("fleet"))
        .ok_or_else(|| format!("fleet up {pid} holds the lease but does not run here"))?;
    match process.kill_with(sysinfo::Signal::Term) {
        Some(true) => Ok(()),
        Some(false) => Err(format!("fleet up {pid} could not be stopped")),
        None => Err(format!(
            "fleet up {pid} cannot be stopped gracefully on this platform"
        )),
    }
}

/// Restart the manager of each Fleet on this machine among `links` on the
/// `fleet` an update just installed beside thurbox ([`restart_manager`]),
/// and say what came of it: what a finished update reports. Empty when no
/// manager ran.
pub fn restart_managers(links: &[FleetLink]) -> String {
    match crate::update::self_update::installed_fleet() {
        Ok(installed) => restart_managers_on(links, &installed),
        Err(why) => format!("Restart any running `fleet up` to apply: {why}."),
    }
}

fn restart_managers_on(links: &[FleetLink], installed: &Path) -> String {
    links
        .iter()
        .filter(|link| link.is_local())
        .filter_map(|link| match restart_manager(link, installed) {
            Ok(Restart::NotRunning) => None,
            Ok(Restart::NotReplaced { program }) => Some(format!(
                "Fleet {}'s `fleet up` keeps running {program}, which this update did not replace.",
                link.name()
            )),
            Ok(Restart::Restarted { .. }) => Some(format!(
                "Fleet {}'s `fleet up` restarted on the new release.",
                link.name()
            )),
            Err(why) => Some(format!(
                "Restart Fleet {}'s `fleet up` to apply: {why}.",
                link.name()
            )),
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    let session_state = match status["sessionState"].as_str() {
        Some(state @ ("Lost" | "Gone")) => Some(state),
        _ if true_condition(status, "Paused").is_some() => Some("Paused"),
        state => state,
    };
    let agent_state = status["agentState"].as_str();
    let repo = repo_name(&cwd, None);
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
        state_why: state_why(session_state, status),
    }
}

fn true_condition<'a>(status: &'a Value, kind: &str) -> Option<&'a Value> {
    status["conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["type"] == kind && c["status"] == "True")
}

fn state_why(session_state: Option<&str>, status: &Value) -> Option<String> {
    let reason = |kind: &str| true_condition(status, kind).and_then(|c| c["reason"].as_str());
    if let Some(stalled) = reason("Stalled") {
        return Some(format!("stalled · {stalled}"));
    }
    match session_state? {
        "Paused" => Some(match reason("Paused") {
            Some("Idle") => "paused · idle".to_string(),
            Some("External") => "stopped · outside Fleet".to_string(),
            _ => "stopped".to_string(),
        }),
        "Lost" => Some(match reason("NeedsAttention") {
            Some("CannotResume") => "lost · cannot resume".to_string(),
            Some("ReviveFailed") => "lost · revive failed".to_string(),
            _ => "lost · window gone".to_string(),
        }),
        "Gone" => Some("gone · session ended".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::process::Command;

    use super::*;

    #[cfg(unix)]
    const SOON: Cadence = Cadence {
        retry: Duration::from_millis(100),
        manager_check: Duration::from_secs(1),
    };

    #[test]
    fn a_stall_or_a_failed_revive_is_the_rows_why() {
        let why = |state: &str, kind: &str, reason: &str| {
            state_why(
                Some(state),
                &serde_json::json!({"conditions": [
                    {"type": kind, "status": "True", "reason": reason},
                ]}),
            )
        };
        assert_eq!(
            why("Running", "Stalled", "SpawnAttemptsExhausted").as_deref(),
            Some("stalled · SpawnAttemptsExhausted")
        );
        assert_eq!(
            why("Lost", "NeedsAttention", "ReviveFailed").as_deref(),
            Some("lost · revive failed")
        );
        assert_eq!(
            why("Lost", "NeedsAttention", "CannotResume").as_deref(),
            Some("lost · cannot resume")
        );
        assert_eq!(why("Running", "Paused", "Idle"), None);
        assert_eq!(
            why("Paused", "Paused", "Unknown").as_deref(),
            Some("stopped")
        );
    }

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

    /// A Fleet that first could not be asked, then answers that it speaks
    /// another API, is incompatible: the unreachable it was meanwhile does
    /// not stand over that answer.
    #[cfg(unix)]
    #[test]
    fn incompatible_after_a_failed_first_probe_stays_incompatible() {
        let remote = crate::fleet_link::testing::Remote::new(
            "here=$(dirname \"$0\")\n\
             [ \"$1\" = version ] || exit 1\n\
             [ -e \"$here/asked\" ] || { touch \"$here/asked\"; echo 'error: not yet' >&2; exit 1; }\n\
             echo '{\"version\":\"9.0.0\",\"apiVersions\":[\"fleet/v2\"]}'",
        );
        crate::paths::with_path(remote.path(), || {
            let mut source = FleetWatch::spawn_every(remote.link.clone(), SOON);
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut published = None;
            while published != Some(FleetState::Incompatible) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "{published:?}: {:?}",
                    remote.calls()
                );
                if let Some(read) = source.poll() {
                    published = read.fleets.first().map(|fleet| fleet.state);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            std::thread::sleep(Duration::from_millis(300));
            if let Some(read) = source.poll() {
                assert_eq!(read.fleets[0].state, FleetState::Incompatible);
            }
        });
    }

    /// Opening the place picker asks nothing of a Fleet not yet admitted,
    /// even while its first `version` is slow, nor of one thurbox cannot
    /// speak to, which stays incompatible.
    #[cfg(unix)]
    #[test]
    fn the_place_picker_asks_nothing_before_admission_or_of_an_incompatible_fleet() {
        let slow = crate::fleet_link::testing::Remote::new(
            "case \"$1\" in\n\
             version) sleep 2; echo '{\"apiVersions\":[\"fleet/v1alpha1\"]}' ;;\n\
             *) exec sleep 30 ;;\n\
             esac",
        );
        crate::paths::with_path(slow.path(), || {
            let mut source = FleetWatch::spawn(slow.link.clone());
            std::thread::sleep(Duration::from_millis(300));
            source.refresh_places("devbox");
            std::thread::sleep(Duration::from_millis(700));
            let calls = slow.calls();
            assert!(
                calls.iter().all(|c| c.ends_with("version --json")),
                "{calls:?}"
            );
            drop(source);
        });

        let v2 = crate::fleet_link::testing::Remote::new(
            "[ \"$1\" = version ] && exec echo '{\"version\":\"9.0.0\",\"apiVersions\":[\"fleet/v2\"]}'\n\
             exit 1",
        );
        crate::paths::with_path(v2.path(), || {
            let mut source = FleetWatch::spawn(v2.link.clone());
            let state = |source: &mut FleetWatch| {
                source.poll();
                source.fleet.state
            };
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while state(&mut source) != FleetState::Incompatible {
                assert!(std::time::Instant::now() < deadline, "never incompatible");
                std::thread::sleep(Duration::from_millis(20));
            }
            source.refresh_places("devbox");
            std::thread::sleep(Duration::from_millis(500));
            assert_eq!(state(&mut source), FleetState::Incompatible);
            // Nothing else either: no places, no watch, and no read of this
            // remote Fleet's home (`sh -c`).
            let calls = v2.calls();
            assert!(!calls.iter().any(|c| c.contains("sh -c")), "{calls:?}");
            assert!(
                calls.iter().all(|c| c.ends_with("version --json")),
                "{calls:?}"
            );
        });
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
             version) echo '{\"apiVersions\":[\"fleet/v1alpha1\"],\"harnesses\":[]}' ;;\n\
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
    /// answers marks it up again: the published state has one owner. Only
    /// the refresh is refused, and every poll is held until the refresh has
    /// published, so no poll's own answer can stand in for it.
    #[cfg(unix)]
    #[test]
    fn a_fleet_unreachable_for_one_refresh_is_up_again_at_the_next_poll() {
        let remote = crate::fleet_link::testing::Remote::new(
            "here=$(dirname \"$0\")\n\
             case \"$1\" in\n\
             version) [ -e \"$here/refused\" ] && { rm \"$here/refused\"; echo 'no route' >&2; exit 255; }\n\
             while [ -e \"$here/hold\" ]; do sleep 0.05; done\n\
             echo '{\"apiVersions\":[\"fleet/v1alpha1\"],\"manager\":{\"held\":true}}' ;;\n\
             runtime) [ -e \"$here/down\" ] && { rm \"$here/down\"; touch \"$here/refused\"; exit 255; }\n\
             echo '{\"places\":null}' ;;\n\
             watch) exec sleep 60 ;;\n\
             esac",
        );
        let down = remote.dir().join("remote/down");
        let hold = remote.dir().join("remote/hold");
        crate::paths::with_path(remote.path(), || {
            let mut store = super::super::SnapshotStore::with_source(
                Box::new(FleetWatch::spawn_every(remote.link.clone(), SOON)),
                &Default::default(),
            );
            let until = |store: &mut super::super::SnapshotStore, want: FleetState| {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
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
            std::fs::write(&hold, "").expect("hold");
            std::fs::write(&down, "").expect("down");
            store.want_places(Some("devbox".into()));
            until(&mut store, FleetState::Unreachable);
            std::fs::remove_file(&hold).expect("release");
            until(&mut store, FleetState::Up);
        });
    }

    /// A remote Fleet's home is asked for once it is admitted. One that
    /// cannot be reached is never admitted, so neither startup nor a place
    /// step opened on it asks for its home: each ask could wait out its
    /// whole bound.
    #[cfg(unix)]
    #[test]
    fn an_unreachable_fleets_home_is_not_asked_again_on_each_opening() {
        let remote = crate::fleet_link::testing::Remote::new(
            "case \"$1\" in\n\
             version) exit 255 ;;\n\
             runtime) echo '{\"places\":null}' ;;\n\
             watch) exec sleep 60 ;;\n\
             esac",
        );
        std::fs::write(remote.dir().join("remote/sh"), "#!/bin/sh\nexit 255\n").expect("sh");
        std::process::Command::new("chmod")
            .args(["+x"])
            .arg(remote.dir().join("remote/sh"))
            .status()
            .expect("chmod");
        let asked = || {
            remote
                .calls()
                .iter()
                .filter(|c| c.contains("sh -c"))
                .count()
        };
        crate::paths::with_path(remote.path(), || {
            let source = FleetWatch::spawn(remote.link.clone());
            let mut store =
                super::super::SnapshotStore::with_source(Box::new(source), &Default::default());
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                store.refresh_if_due();
                let state = store.current().fleets.first().map(|f| f.state);
                if state == Some(FleetState::Unreachable) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "{state:?}: {:?}",
                    remote.calls()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            for _ in 0..3 {
                store.want_places(Some("devbox".into()));
                store.want_places(None);
            }
            std::thread::sleep(Duration::from_millis(500));
            assert_eq!(asked(), 0, "{:?}", remote.calls());
        });
    }

    /// A Fleet's state is its manager's lease, from `fleet version --json`:
    /// up while it is held, down when it is not, and unreachable when ssh
    /// itself fails (exit 255).
    #[cfg(unix)]
    #[test]
    fn a_fleets_state_is_its_managers_lease() {
        for (answer, state) in [
            (
                r#"{"apiVersions":["fleet/v1alpha1"],"manager":{"held":true}}"#,
                FleetState::Up,
            ),
            (
                r#"{"apiVersions":["fleet/v1alpha1"],"manager":{"held":false}}"#,
                FleetState::Down,
            ),
            (
                r#"{"apiVersions":["fleet/v1alpha1"],"manager":{"held":null,"why":"no store"}}"#,
                FleetState::Unknown,
            ),
            // The API comes first: a Fleet that does not speak it is that,
            // whatever its manager does.
            (
                r#"{"apiVersions":["fleet/v2"],"manager":{"held":true}}"#,
                FleetState::Incompatible,
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
             version) echo '{\"apiVersions\":[\"fleet/v1alpha1\"],\"harnesses\":[\"claude\"]}' ;;\n\
             runtime) echo '{\"places\":[{\"place\":\"mux/local\",\"runtime\":\"mux\",\"alias\":\"local\",\"ready\":true},{\"place\":\"mux/devbox\",\"runtime\":\"mux\",\"ready\":false,\"why\":\"no tmux\"}]}' ;;\n\
             watch) echo '{\"type\":\"synced\",\"revision\":1,\"names\":[]}'; exec sleep 30 ;;\n\
             up) exit 6 ;;\n\
             esac",
        );
        crate::paths::with_path(remote.path(), || {
            let (tx, _rx) = channel();
            let data = tempfile::tempdir().expect("data");
            assert_eq!(manage(&remote.link, data.path(), &tx), Ok(Some(LEASE_HELD)));
            let mut source = FleetWatch::spawn(remote.link.clone());
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

    /// A Fleet whose `fleet version` gives no answer is not silent: the
    /// source says why, as unreachable, never incompatible. That a probe
    /// past its deadline gives no answer is `bounded_output`'s
    /// (`fleet_link::tests::a_probe_past_its_deadline_is_stopped_with_its_group`).
    #[cfg(unix)]
    #[test]
    fn a_fleet_that_gives_no_version_is_reported_unreachable() {
        let bin = tempfile::tempdir().expect("bin");
        let missing = bin.path().join("fleet");
        let mut source = FleetWatch::spawn(FleetLink::local(missing.to_string_lossy()));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let read = loop {
            if let Some(read) = source.poll().filter(|read| read.error.is_some()) {
                break read;
            }
            assert!(std::time::Instant::now() < deadline, "no error");
            std::thread::sleep(Duration::from_millis(20));
        };
        let error = read.error.unwrap_or_default();
        assert!(error.contains("could not start fleet version"), "{error}");
        assert_eq!(read.fleets[0].state, FleetState::Unreachable, "{error}");
    }

    /// thurbox never starts `fleet up` on a Fleet on another machine (M4):
    /// asked to keep a manager, the source follows that Fleet and starts
    /// none.
    #[cfg(unix)]
    #[test]
    fn a_remote_fleet_gets_no_manager() {
        let remote = crate::fleet_link::testing::Remote::new(
            "case \"$1\" in\n\
             version) echo '{\"apiVersions\":[\"fleet/v1alpha1\"],\"harnesses\":[]}' ;;\n\
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

    #[test]
    fn the_paused_condition_stops_a_row_whose_session_is_not_lost_or_gone() {
        let task = |session_state: &str| {
            serde_json::json!({
                "metadata": {"name": "t"},
                "spec": {"harness": "codex", "host": "local", "workspace": []},
                "status": {"phase": "Concluded", "sessionState": session_state,
                           "agentState": "idle",
                           "conditions": [{"type": "Paused", "status": "True", "reason": "Idle"}]},
            })
        };
        let row = |state: &str| {
            let row = row_of(&FleetLink::local("fleet"), &task(state));
            (row.status, row.stopped, row.state_why)
        };
        assert_eq!(
            row("Idle"),
            (SessionState::Stopped, true, Some("paused · idle".into()))
        );
        assert_eq!(
            row("Lost"),
            (
                SessionState::Failed,
                false,
                Some("lost · window gone".into())
            )
        );
        assert_eq!(
            row("Gone"),
            (
                SessionState::Failed,
                false,
                Some("gone · session ended".into())
            )
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
            "#!/bin/sh\n[ \"$1\" = version ] && exec echo '{\"apiVersions\":[\"fleet/v1alpha1\"],\"harnesses\":[\"claude-code\",\"codex\"]}'\nexec sleep 30\n",
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

    /// The line a stand-in `fleet` script starts with to be admitted: its
    /// `fleet version --json` speaks the API thurbox writes.
    #[cfg(unix)]
    const ADMITTED: &str =
        "[ \"$1\" = version ] && exec echo '{\"apiVersions\":[\"fleet/v1alpha1\"]}'";

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
            ADMITTED.to_string(),
            "[ \"$1\" = watch ] || exit 1".to_string(),
            task("gone", "u1", 1),
            event(serde_json::json!({"type": "synced", "revision": 1, "names": ["gone"]})),
            "sleep 0.3".to_string(),
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
        let slower_than_the_script_so_the_replacement_lands_in_one_poll =
            Duration::from_millis(500);
        while !ids.iter().any(|id| id == "local:kept") {
            assert!(std::time::Instant::now() < deadline, "no second read");
            std::thread::sleep(slower_than_the_script_so_the_replacement_lands_in_one_poll);
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
            ADMITTED.to_string(),
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
        let listed_stuck = |seen: &[(Duration, Vec<String>)]| {
            seen.iter()
                .any(|(_, ids)| ids.iter().any(|id| id == "local:stuck"))
        };
        while !listed_stuck(&seen) && started.elapsed() < Duration::from_secs(10) {
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

    /// A manager that was stopped (exit 0), as an update's restart stops
    /// it, is no error: the interface says nothing while it is replaced.
    #[cfg(unix)]
    #[test]
    fn a_stopped_manager_is_no_error() {
        let bin = tempfile::tempdir().expect("bin");
        let data = tempfile::tempdir().expect("data");
        let _paths = crate::paths::TestPathGuard::new(data.path());
        let program = fake_fleet(bin.path(), "exit 0");
        let (tx, rx) = channel();
        let link = FleetLink::local(program.to_string_lossy());
        std::thread::spawn(move || keep_manager(&link, &tx, Duration::from_millis(50)));
        let reports: Vec<Option<String>> = (0..4)
            .map(|_| match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Update::Manager(report)) => report,
                Ok(_) => Some("not a manager report".into()),
                Err(e) => Some(format!("no report: {e}")),
            })
            .collect();
        assert_eq!(reports, [None, None, None, None]);
    }

    /// Only a running `fleet` is ever signalled: a pid the lease names that
    /// is another program is refused and keeps running.
    #[cfg(unix)]
    #[test]
    fn terminate_refuses_a_process_that_is_not_fleet() {
        let mut other = Command::new("sleep").arg("30").spawn().expect("sleep");
        let why = terminate(other.id()).expect_err("not a fleet");
        assert!(why.contains("does not run here"), "{why}");
        assert_eq!(other.try_wait().expect("wait"), None, "it was signalled");
        let _ = other.kill();
        let _ = other.wait();
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
