use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use super::execute::Outcome;
use super::fleet::Fleet;
use super::{Command, Executor};

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

#[derive(Debug, Clone)]
pub struct InFlight {
    pub id: u64,
    pub kind: &'static str,
    pub session: String,
    pub subject: Option<String>,
    pub host: Option<String>,
    pub phase: Phase,
    pub error: Option<String>,
}

impl InFlight {
    pub(super) fn started(&mut self) {
        if self.phase == Phase::Queued {
            self.phase = Phase::Running;
        }
    }
}

pub(super) struct Progress {
    pub(super) id: u64,
    pub(super) phase: String,
}

struct Done {
    id: u64,
    error: Option<String>,
}

impl Done {
    fn from_result(id: u64, result: Result<Outcome, String>) -> Self {
        match result {
            Ok(Outcome::Done) => Self { id, error: None },
            Err(error) => Self {
                id,
                error: Some(error),
            },
        }
    }
}

const FAILURE_LINGER_MS: u64 = 8_000;

type SessionQueues = HashMap<String, VecDeque<(u64, Command)>>;

pub struct CommandBus {
    inflight: Arc<Mutex<Vec<InFlight>>>,
    finished_tx: Sender<Done>,
    finished_rx: Receiver<Done>,
    progress_tx: Sender<Progress>,
    progress_rx: Receiver<Progress>,
    next_id: AtomicU64,
    pending: Arc<Mutex<SessionQueues>>,
    failed_at: Vec<(u64, std::time::Instant)>,
    deleted: Vec<String>,
    executor: Arc<dyn Executor>,
    fleet: Option<String>,
}

impl CommandBus {
    pub fn fleet(fleets: impl Into<crate::fleet_link::Fleets>) -> Self {
        let fleets = fleets.into();
        let fleet = Some(fleets.in_force().name().to_string());
        Self {
            fleet,
            ..Self::with_executor(Arc::new(Fleet::new(fleets)))
        }
    }

    pub fn created_id(&self, name: &str, fleet: Option<&str>) -> Option<String> {
        let in_force = self.fleet.as_deref()?;
        let fleet = fleet.filter(|f| !f.is_empty()).unwrap_or(in_force);
        Some(crate::kernel::snapshot::row_id(
            fleet,
            &super::task_name(name),
        ))
    }

    fn with_executor(executor: Arc<dyn Executor>) -> Self {
        let (finished_tx, finished_rx) = channel();
        let (progress_tx, progress_rx) = channel();
        Self {
            inflight: Arc::new(Mutex::new(Vec::new())),
            finished_tx,
            finished_rx,
            progress_tx,
            progress_rx,
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            failed_at: Vec::new(),
            deleted: Vec::new(),
            executor,
            fleet: None,
        }
    }

    pub fn dispatch(&self, command: Command) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let entry = InFlight {
            id,
            kind: command.kind(),
            session: command.session().to_string(),
            subject: command.subject(),
            host: command.host(),
            phase: Phase::Queued,
            error: None,
        };
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.push(entry);
        }

        let session = command.session().to_string();
        if !session.is_empty() {
            let mut pending = self.pending.lock().unwrap();
            match pending.entry(session.clone()) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().push_back((id, command));
                    return id;
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(VecDeque::new());
                }
            }
        }

        let inflight = self.inflight.clone();
        let finished = self.finished_tx.clone();
        let progress = self.progress_tx.clone();
        let executor = Arc::clone(&self.executor);
        let pending = Arc::clone(&self.pending);
        std::thread::spawn(move || {
            let mut next = Some((id, command));
            while let Some((id, command)) = next {
                if let Ok(mut list) = inflight.lock() {
                    if let Some(entry) = list.iter_mut().find(|entry| entry.id == id) {
                        entry.started();
                    }
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    executor.execute(&command, id, &progress)
                }))
                .unwrap_or_else(|_| Err("command worker panicked".into()));
                let _ = finished.send(Done::from_result(id, result));
                if session.is_empty() {
                    break;
                }
                let mut queues = pending.lock().unwrap();
                next = queues.get_mut(&session).and_then(VecDeque::pop_front);
                if next.is_none() {
                    queues.remove(&session);
                }
            }
        });
        id
    }

    #[doc(hidden)]
    pub fn finish_for_test(&self, id: u64, error: Option<String>) {
        let _ = self.finished_tx.send(Done { id, error });
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = self.drain_progress();
        changed |= self.drain_finished();
        changed |= self.sweep_expired_failures();
        changed
    }

    fn drain_progress(&mut self) -> bool {
        let mut changed = false;
        while let Ok(update) = self.progress_rx.try_recv() {
            let Ok(mut list) = self.inflight.lock() else {
                continue;
            };
            if let Some(entry) = list.iter_mut().find(|entry| entry.id == update.id) {
                entry.phase = Phase::Stage(update.phase);
                changed = true;
            }
        }
        changed
    }

    fn drain_finished(&mut self) -> bool {
        let mut changed = false;
        while let Ok(done) = self.finished_rx.try_recv() {
            changed = true;
            let Ok(mut list) = self.inflight.lock() else {
                continue;
            };
            match done.error {
                None => {
                    if let Some(entry) = list
                        .iter()
                        .find(|entry| entry.id == done.id && entry.kind == "delete")
                    {
                        self.deleted.push(entry.session.clone());
                    }
                    list.retain(|entry| entry.id != done.id);
                }
                Some(error) => {
                    if let Some(entry) = list.iter_mut().find(|entry| entry.id == done.id) {
                        entry.phase = Phase::Failed;
                        entry.error = Some(error);
                    }
                    self.failed_at.push((done.id, std::time::Instant::now()));
                }
            }
        }
        changed
    }

    pub fn take_deleted(&mut self) -> Vec<String> {
        std::mem::take(&mut self.deleted)
    }

    fn sweep_expired_failures(&mut self) -> bool {
        let expired: Vec<u64> = self
            .failed_at
            .iter()
            .filter(|(_, at)| at.elapsed().as_millis() as u64 >= FAILURE_LINGER_MS)
            .map(|(id, _)| *id)
            .collect();
        if expired.is_empty() {
            return false;
        }
        if let Ok(mut list) = self.inflight.lock() {
            list.retain(|entry| !expired.contains(&entry.id));
        }
        self.failed_at.retain(|(id, _)| !expired.contains(id));
        true
    }

    pub fn has_inflight(&self) -> bool {
        self.inflight
            .lock()
            .map(|list| !list.is_empty())
            .unwrap_or(false)
    }

    pub fn inflight(&self) -> Vec<InFlight> {
        self.inflight
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default()
    }

    pub fn first_running(&self) -> Option<InFlight> {
        self.inflight
            .lock()
            .ok()?
            .iter()
            .find(|entry| entry.phase != Phase::Failed)
            .cloned()
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn bus() -> CommandBus {
        CommandBus::fleet(crate::fleet_link::FleetLink::local("thurbox-test-no-fleet"))
    }

    fn delete(bus: &CommandBus, session: &str) -> u64 {
        bus.dispatch(Command::Delete {
            session: session.into(),
            force: false,
        })
    }

    #[test]
    fn a_refused_delete_never_hides_its_row_even_once_its_failure_is_swept() {
        let mut bus = bus();
        let refused = delete(&bus, "refused");
        bus.finish_for_test(refused, Some("refused by fleet".into()));
        assert!(bus.poll());
        assert!(bus.take_deleted().is_empty());

        let linger = std::time::Duration::from_millis(FAILURE_LINGER_MS);
        for (_, at) in &mut bus.failed_at {
            *at = std::time::Instant::now()
                .checked_sub(linger)
                .expect("a clock past the linger");
        }
        assert!(bus.poll());
        assert!(
            !bus.inflight().iter().any(|item| item.id == refused),
            "the failure lingered past its time"
        );
        assert!(bus.take_deleted().is_empty());
    }

    #[test]
    fn a_delete_that_succeeded_hides_its_row_for_good() {
        let mut bus = bus();
        let deleted = delete(&bus, "deleted");
        bus.finish_for_test(deleted, None);
        assert!(bus.poll());
        assert_eq!(bus.take_deleted(), ["deleted"]);
    }
}
