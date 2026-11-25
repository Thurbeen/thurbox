use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
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

const RETRY: Duration = Duration::from_secs(2);

const MANAGER_CHECK: Duration = Duration::from_secs(10);

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

pub const DELETION_STUCK_AFTER: Duration = Duration::from_secs(180);

pub const BACKEND: &str = "fleet";

pub const NAME_LABEL: &str = "thurbox.dev/name";

enum Update {
    Rows(Vec<SessionRow>, Vec<String>),
    Watch(Option<String>),
    Lost(String),
    Harnesses(Vec<String>),
    Places(Vec<HostRow>),
    State(FleetState, Option<String>),
    Home(String),
    Incompatible(String),
}

pub struct FleetWatch {
    name: String,
    updates: Receiver<Update>,
    read: SourceRead,
    watch_error: Option<String>,
    lost: bool,
    tasks: Vec<SessionRow>,
    rows_seen: bool,
    next_stuck_ms: Option<i64>,
    interface: Option<(u64, crate::config::interface_state::InterfaceState)>,
    child: Arc<Mutex<Option<Child>>>,
    fleet: FleetRow,
    _alive: Arc<()>,
    link: FleetLink,
    tx: Sender<Update>,
    refreshing: Arc<std::sync::atomic::AtomicBool>,
    reading_home: Arc<std::sync::atomic::AtomicBool>,
    admitted: Arc<std::sync::atomic::AtomicBool>,
    published: Published,
}

type Published = Arc<Mutex<Option<(FleetState, Option<String>)>>>;

fn publish_state(
    published: &Published,
    tx: &Sender<Update>,
    next: (FleetState, Option<String>),
) -> bool {
    let Ok(mut last) = published.lock() else {
        return false;
    };
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
    pub fn spawn(link: FleetLink) -> Self {
        Self::spawn_every(link, Cadence::default())
    }

    #[doc(hidden)]
    pub fn spawn_every(link: FleetLink, cadence: Cadence) -> Self {
        let (tx, updates) = channel();
        let _ = tx.send(Update::State(FleetState::Connecting, None));
        let alive = Arc::new(());
        let published = Published::default();
        let reading_home = Arc::new(std::sync::atomic::AtomicBool::new(!link.is_local()));
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
        let admitted_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let admitted: Box<dyn FnOnce() + Send> = {
            let (link, tx, reading) = (link.clone(), tx.clone(), reading_home.clone());
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
    fn refresh_places(&mut self, fleet: &str) {
        use std::sync::atomic::Ordering;
        if fleet != self.name
            || !self.admitted.load(Ordering::Acquire)
            || self.refreshing.swap(true, Ordering::AcqRel)
        {
            return;
        }
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
                Update::Lost(_) | Update::State(..)
                    if self.fleet.state == FleetState::Incompatible => {}
                Update::Lost(why) => {
                    self.lost = true;
                    self.watch_error = Some(why);
                }
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
        let generation = crate::config::interface_state::generation();
        if self
            .interface
            .as_ref()
            .is_none_or(|(seen, _)| *seen != generation)
        {
            self.interface = Some((generation, crate::config::interface_state::load()));
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
            .filter(|row| stuck_at(row).is_none_or(|at| at <= now))
            .cloned()
            .map(|mut row| {
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
        self.read.error.clone_from(&self.watch_error);
        let mut fleet = self.fleet.clone();
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

pub struct FleetWatches {
    in_force: String,
    watches: Vec<FleetWatch>,
    reads: Vec<SourceRead>,
}

impl FleetWatches {
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

fn read_home(link: &FleetLink, tx: &Sender<Update>) {
    if let Some(home) = link.home(crate::fleet_link::REMOTE_PROBE_TIMEOUT) {
        let _ = tx.send(Update::Home(home));
    }
}

fn machine(link: &FleetLink) -> String {
    if link.is_local() {
        "this machine".to_string()
    } else {
        format!("{} itself", link.name())
    }
}

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
        let here = is_fleets_machine(&id);
        let detail = if here {
            format!("{machine} ({runtime})")
        } else if place["dynamic"].as_bool() == Some(true) {
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
            here,
        });
    }
    rows
}

fn ask(link: &FleetLink, args: &[&str]) -> Option<Value> {
    link.command(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice::<Value>(&out.stdout).ok())
}

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

fn harnesses_of(answer: &Value) -> Vec<String> {
    answer["harnesses"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| h.as_str().map(str::to_string))
        .collect()
}

fn probe(link: &FleetLink) -> ((FleetState, Option<String>), Option<Value>) {
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
    if let Some(why) = crate::fleet_link::incompatible(&answer) {
        return ((FleetState::Incompatible, Some(why)), Some(answer));
    }
    let manager = &answer["manager"];
    if manager["held"] != Value::Bool(true) {
        if let Some(why) = link.no_config() {
            return ((FleetState::Down, Some(why)), Some(answer));
        }
    }
    let state = match manager["held"].as_bool() {
        Some(true) => (FleetState::Up, None),
        Some(false) => (FleetState::Down, link.not_running(manager)),
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
    // Drained beside stdout so a full stderr pipe cannot stall the watch.
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

pub fn stale_managers(links: &[FleetLink], to: &str) -> String {
    let installed = crate::update::self_update::installed_fleet().ok();
    links
        .iter()
        .filter(|link| link.is_local())
        .filter_map(|link| {
            let manager = link.version().ok()?["manager"].take();
            (manager["held"] == Value::Bool(true) && manager["version"].as_str() != Some(to))
                .then_some((link, manager))
        })
        .map(|(link, manager)| match &installed {
            Some(installed) if !runs(link, installed) => format!(
                "Fleet {}'s manager runs {}, which this update did not replace.",
                link.name(),
                link.program()
            ),
            _ => format!(
                "Fleet {}'s manager still runs the old release · {}.",
                link.name(),
                link.restart_manager(&manager)
            ),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn runs(link: &FleetLink, installed: &std::path::Path) -> bool {
    let program = link.program();
    let resolved = if std::path::Path::new(program).components().count() > 1 {
        Some(PathBuf::from(program))
    } else {
        crate::paths::resolve_on_path(program).or_else(|| {
            crate::paths::resolve_on_path(&format!("{program}{}", std::env::consts::EXE_SUFFIX))
        })
    };
    let canonical = |path: &std::path::Path| std::fs::canonicalize(path).ok();
    resolved
        .is_some_and(|path| canonical(&path).is_some() && canonical(&path) == canonical(installed))
}

fn workspace_git_dir_name(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("git+https://")
        .or_else(|| url.strip_prefix("git+ssh://"))?;
    let path = rest.split(['?', '#']).next()?.trim_end_matches('/');
    let last = path.rsplit(['/', ':']).next().unwrap_or_default();
    let last = String::from_utf8_lossy(&percent_decoded(last)).into_owned();
    let last = last.strip_suffix(".git").unwrap_or(&last);
    let name: String = last
        .chars()
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') => c,
            _ => '_',
        })
        .collect();
    match name.trim_start_matches('.') {
        "" => Some("repo".into()),
        name => Some(name.into()),
    }
}

fn local_dir(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("file://")
        .or_else(|| url.strip_prefix("git+file://"))?;
    let path = path.split(['?', '#']).next().unwrap_or(path);
    String::from_utf8(percent_decoded(path))
        .ok()
        .filter(|p| !p.is_empty())
}

fn percent_decoded(path: &str) -> Vec<u8> {
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
    bytes
}

pub(crate) fn is_fleets_machine(place: &str) -> bool {
    place == "local"
        || place
            .split_once('/')
            .is_some_and(|(_, name)| name == "local")
}

pub fn row_of(link: &FleetLink, task: &Value) -> SessionRow {
    let text = |value: &Value| value.as_str().map(str::to_string);
    let (spec, status) = (&task["spec"], &task["status"]);
    let task_name = text(&task["metadata"]["name"]).unwrap_or_default();
    let name = text(&task["metadata"]["labels"][NAME_LABEL]).unwrap_or_else(|| task_name.clone());
    let id = super::row_id(link.name(), &task_name);
    let place = text(&status["execution"]["host"]).or_else(|| text(&spec["host"]));
    let elsewhere = place.clone().filter(|place| !is_fleets_machine(place));
    let remote_host = match (link.is_local(), elsewhere) {
        (true, place) => place,
        (false, None) => Some(link.name().to_string()),
        (false, Some(place)) => Some(format!("{} · {place}", link.name())),
    };
    let cwd = text(&status["workspace"]["dirs"][0])
        .or_else(|| spec["workspace"][0].as_str().and_then(local_dir))
        .map(PathBuf::from);
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
    let repo = repo_name(&cwd, None).or_else(|| {
        spec["workspace"][0]
            .as_str()
            .and_then(workspace_git_dir_name)
    });
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
        backend_id: None,
        remote_host,
        place,
        agent_session_id: text(&status["conversation"]),
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
        state_why: state_why(
            session_state,
            status,
            if spec["dispatch"] == false {
                Dispatch::Held
            } else {
                Dispatch::On
            },
        ),
    }
}

fn true_condition<'a>(status: &'a Value, kind: &str) -> Option<&'a Value> {
    status["conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["type"] == kind && c["status"] == "True")
}

pub(crate) const SESSION_ENDED: &str = "gone · session ended";

pub(crate) const RESUMING: &str = "paused · resuming";

#[derive(Clone, Copy, PartialEq)]
enum Dispatch {
    On,
    Held,
}

fn resume_due(status: &Value) -> bool {
    let paused_at = true_condition(status, "Paused").and_then(|c| c["lastTransitionTime"].as_i64());
    let woken = status["wakeRequestedAt"]
        .as_i64()
        .zip(paused_at)
        .is_some_and(|(asked, paused)| asked >= paused);
    let entry_waiting = status["inbox"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|entry| {
            status["deliveryAttempts"][entry["key"].as_str().unwrap_or_default()]["failed"] != true
        });
    woken || entry_waiting || status["resumeAttempts"].as_u64().is_some_and(|n| n > 0)
}

fn state_why(session_state: Option<&str>, status: &Value, dispatch: Dispatch) -> Option<String> {
    let reason = |kind: &str| true_condition(status, kind).and_then(|c| c["reason"].as_str());
    if let Some(stalled) = reason("Stalled") {
        return Some(format!("stalled · {stalled}"));
    }
    match session_state? {
        "Paused" => Some(match reason("Paused") {
            _ if dispatch == Dispatch::On && resume_due(status) => RESUMING.to_string(),
            Some("Idle") => "paused · idle".to_string(),
            Some("Requested") => "paused · on request".to_string(),
            Some("External") => "stopped · outside Fleet".to_string(),
            _ => "stopped".to_string(),
        }),
        "Lost" => Some(match reason("NeedsAttention") {
            Some("CannotResume") => "lost · cannot resume".to_string(),
            Some("ReviveFailed") => "lost · revive failed".to_string(),
            _ => "lost · window gone".to_string(),
        }),
        "Gone" => Some(SESSION_ENDED.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::path::Path;

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
                Dispatch::On,
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

    #[test]
    fn a_paused_task_fleet_is_about_to_resume_reads_as_resuming() {
        let why = |extra: serde_json::Value| {
            let mut status = serde_json::json!({"conditions": [
                {"type": "Paused", "status": "True", "reason": "Idle", "lastTransitionTime": 10},
            ]});
            status
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            state_why(Some("Paused"), &status, Dispatch::On)
        };
        assert_eq!(why(serde_json::json!({})).as_deref(), Some("paused · idle"));
        assert_eq!(
            why(serde_json::json!({"wakeRequestedAt": 9})).as_deref(),
            Some("paused · idle")
        );
        for due in [
            serde_json::json!({"wakeRequestedAt": 10}),
            serde_json::json!({"inbox": [{"key": "inbox/1"}]}),
            serde_json::json!({"resumeAttempts": 1}),
        ] {
            assert_eq!(why(due.clone()).as_deref(), Some(RESUMING), "{due}");
        }
        assert_eq!(
            why(serde_json::json!({"inbox": [{"key": "inbox/1"}],
                "deliveryAttempts": {"inbox/1": {"failed": true}}}))
            .as_deref(),
            Some("paused · idle")
        );
        let held = serde_json::json!({"inbox": [{"key": "inbox/1"}], "wakeRequestedAt": 10,
            "conditions": [{"type": "Paused", "status": "True", "reason": "Idle",
                "lastTransitionTime": 10}]});
        assert_eq!(
            state_why(Some("Paused"), &held, Dispatch::Held).as_deref(),
            Some("paused · idle")
        );
    }

    #[test]
    fn the_places_offered_are_fleets_local_first() {
        let answer = serde_json::json!({"places": [
            {"place": "mux/wsl-dev", "runtime": "mux", "alias": null, "ready": false, "why": "no distro"},
            {"place": "mux/local", "runtime": "mux", "alias": "local", "ready": true},
            {"place": "vm/sandbox", "runtime": "vm", "dynamic": true, "ready": null},
            {"place": "herdr/local", "runtime": "herdr", "name": "local", "alias": null, "ready": true},
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
                (
                    "herdr/local",
                    "box itself (herdr)",
                    "herdr/local",
                    Some(true)
                ),
            ]
        );
        assert_eq!(rows[1].why.as_deref(), Some("no distro"));
        let any = places_from(&serde_json::json!({"places": null}), "this machine");
        assert_eq!(any, [HostRow::local("this machine")]);
    }

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
            let calls = v2.calls();
            assert!(!calls.iter().any(|c| c.contains("sh -c")), "{calls:?}");
            assert!(
                calls.iter().all(|c| c.ends_with("version --json")),
                "{calls:?}"
            );
        });
    }

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
            store.want_places(None);
            std::fs::write(&places, answer(true)).expect("places");
            store.want_places(Some("devbox".into()));
            until(&mut store, Some(true));
        });
    }

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

    #[cfg(unix)]
    #[test]
    fn a_local_fleet_whose_named_config_is_missing_names_fleet_init_on_it() {
        use crate::session::settings::FleetDef;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("dir");
        let program = dir.path().join("fleet");
        std::fs::write(
            &program,
            "#!/bin/sh\necho '{\"apiVersions\":[\"fleet/v1alpha1\"],\"manager\":{\"held\":null,\"why\":\"no config\"}}'\n",
        )
        .expect("program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let config = dir.path().join("missing.toml");
        let link = FleetLink::of(
            "laptop",
            &FleetDef {
                transport: Default::default(),
                program: program.to_string_lossy().into_owned(),
                config: Some(config.to_string_lossy().into_owned()),
            },
        );
        let ((state, why), _) = probe(&link);
        assert_eq!(state, FleetState::Down);
        assert_eq!(
            why,
            Some(format!(
                "no Fleet config · fleet --config {} init",
                config.display()
            ))
        );
    }

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

    #[cfg(unix)]
    #[test]
    fn every_call_of_the_source_goes_through_its_link() {
        let remote = crate::fleet_link::testing::Remote::new(
            "case \"$1\" in\n\
             version) echo '{\"apiVersions\":[\"fleet/v1alpha1\"],\"harnesses\":[\"claude\"]}' ;;\n\
             runtime) echo '{\"places\":[{\"place\":\"mux/local\",\"runtime\":\"mux\",\"alias\":\"local\",\"ready\":true},{\"place\":\"mux/devbox\",\"runtime\":\"mux\",\"ready\":false,\"why\":\"no tmux\"}]}' ;;\n\
             watch) echo '{\"type\":\"synced\",\"revision\":1,\"names\":[]}'; exec sleep 30 ;;\n\
             esac",
        );
        crate::paths::with_path(remote.path(), || {
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
        for call in ["version --json", "runtime places --json"] {
            assert!(remote.called(call), "{call}: {:?}", remote.calls());
        }
    }

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

    #[test]
    fn a_held_row_names_its_repository_as_workspace_git_names_the_directory() {
        let held = |url: &str| {
            let task = serde_json::json!({
                "metadata": {"name": "held"},
                "spec": {"harness": "claude", "host": "local", "dispatch": false,
                         "workspace": [url]},
                "status": {},
            });
            row_of(&FleetLink::local("fleet"), &task).repo
        };
        for (url, name) in [
            (
                "git+https://github.com/LeTuR/websites.git",
                Some("websites"),
            ),
            (
                "git+https://github.com/LeTuR/websites.git?branch=b#main",
                Some("websites"),
            ),
            ("git+https://host/team/%77ebsites.git", Some("websites")),
            ("git+https://host/team/my%20repo.git", Some("my_repo")),
            ("git+https://host/org/.github.git", Some("github")),
            ("git+ssh://git@host/org/repo/", Some("repo")),
            ("git+https://host/org/.git", Some("repo")),
            ("s3://bucket/data", None),
        ] {
            assert_eq!(held(url).as_deref(), name, "{url}");
        }
    }

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

    #[cfg(unix)]
    const ADMITTED: &str =
        "[ \"$1\" = version ] && exec echo '{\"apiVersions\":[\"fleet/v1alpha1\"]}'";

    #[cfg(unix)]
    fn fake_fleet(dir: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("fleet");
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).expect("fleet");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        program
    }

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
}
