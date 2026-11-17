//! A full disk pauses the manager instead of ending it (#1866). The file
//! store under the manager fails every write with `Full` while the disk is
//! full, as the file store does on `ENOSPC`; the runtime is the fake one,
//! counting its spawns.

mod support;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fleet_api::inbox::{DeliveryReceipt, InboxEntry};
use fleet_api::{
    Clock, DeleteOutcome, FieldManager, LeaseToken, Object, Observation, Store, StoreError,
};
use fleet_manager::{Manager, RunOptions, RunReport};
use fleet_runtime::{FakeHost, FakeRuntime};
use fleet_store::FileStore;
use serde_json::Value;

use support::{Env, task};

const TTL: u64 = 2_000;

#[derive(Debug)]
struct TestClock(AtomicU64);

impl Clock for TestClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Disk {
    Free,
    Full,
    FullAfterLease,
}

struct Filling<'a> {
    inner: &'a FileStore,
    data: PathBuf,
    disk: Cell<Disk>,
}

impl Filling<'_> {
    fn write<T>(
        &self,
        file: &str,
        f: impl FnOnce() -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let lease = file == "lease.json";
        match self.disk.get() {
            Disk::Full => Err(StoreError::Full(self.data.join(file).display().to_string())),
            Disk::FullAfterLease if !lease => {
                Err(StoreError::Full(self.data.join(file).display().to_string()))
            }
            _ => f(),
        }
    }
}

impl Store for Filling<'_> {
    fn served_kinds(&self) -> Vec<String> {
        self.inner.served_kinds()
    }
    fn validate_plugin_status(&self, kind: &str, status: &Value) -> Result<(), StoreError> {
        self.inner.validate_plugin_status(kind, status)
    }
    fn launch(&self, task: &Object) -> Result<Option<fleet_api::config::Execution>, StoreError> {
        self.inner.launch(task)
    }
    fn freeze_launch(
        &self,
        task: &Object,
        execution: &fleet_api::config::Execution,
        lease: &LeaseToken,
    ) -> Result<fleet_api::config::Execution, StoreError> {
        self.write("launches", || {
            self.inner.freeze_launch(task, execution, lease)
        })
    }
    fn get(&self, kind: &str, name: &str) -> Observation<Option<Object>> {
        self.inner.get(kind, name)
    }
    fn list(&self, kind: &str) -> Observation<Vec<Object>> {
        self.inner.list(kind)
    }
    fn create(&self, obj: Object, by: FieldManager) -> Result<Object, StoreError> {
        self.write("objects", || self.inner.create(obj, by))
    }
    fn update_spec(
        &self,
        kind: &str,
        name: &str,
        spec: Value,
        labels: Option<BTreeMap<String, String>>,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError> {
        self.write("objects", || {
            self.inner
                .update_spec(kind, name, spec, labels, expected, by)
        })
    }
    fn update_status(
        &self,
        kind: &str,
        name: &str,
        status: Value,
        expected: u64,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Object, StoreError> {
        self.write("objects", || {
            self.inner
                .update_status(kind, name, status, expected, by, lease)
        })
    }
    fn delete(
        &self,
        kind: &str,
        name: &str,
        by: FieldManager,
    ) -> Result<DeleteOutcome, StoreError> {
        self.write("objects", || self.inner.delete(kind, name, by))
    }
    fn delete_fenced(
        &self,
        kind: &str,
        name: &str,
        uid: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        self.write("objects", || {
            self.inner
                .delete_fenced(kind, name, uid, expected, by, lease)
        })
    }
    fn append_inbox(
        &self,
        name: &str,
        entry: InboxEntry,
        expected: u64,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Object, StoreError> {
        self.write("objects", || {
            self.inner.append_inbox(name, entry, expected, by, lease)
        })
    }
    fn record_delivery(
        &self,
        name: &str,
        status: Value,
        receipt: DeliveryReceipt,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        self.write("objects", || {
            self.inner
                .record_delivery(name, status, receipt, expected, by, lease)
        })
    }
    fn record_relay(
        &self,
        name: &str,
        status: Value,
        appends: Vec<InboxEntry>,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        self.write("objects", || {
            self.inner
                .record_relay(name, status, appends, expected, by, lease)
        })
    }
    fn remove_finalizer(
        &self,
        kind: &str,
        name: &str,
        finalizer: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        self.write("objects", || {
            self.inner
                .remove_finalizer(kind, name, finalizer, expected, by, lease)
        })
    }
    fn acquire_lease(&self, holder: &str, ttl: u64) -> Result<LeaseToken, StoreError> {
        self.write("lease.json", || self.inner.acquire_lease(holder, ttl))
    }
    fn renew_lease(&self, lease: &LeaseToken, ttl: u64) -> Result<LeaseToken, StoreError> {
        self.write("lease.json", || self.inner.renew_lease(lease, ttl))
    }
    fn reclaim_lease(&self, lease: &LeaseToken, ttl: u64) -> Result<LeaseToken, StoreError> {
        self.write("lease.json", || self.inner.reclaim_lease(lease, ttl))
    }
    fn release_lease(&self, lease: &LeaseToken) -> Result<(), StoreError> {
        self.write("lease.json", || self.inner.release_lease(lease))
    }
}

struct World {
    env: Env,
    clock: Arc<TestClock>,
    store: FileStore,
    host: FakeHost,
}

impl World {
    fn new(name: &str) -> World {
        let env = Env::new(name);
        let clock = Arc::new(TestClock(AtomicU64::new(1_794_798_000_000)));
        let store = FileStore::open_with_clock(&env.data, clock.clone()).expect("store");
        let host = FakeHost::open(&env.fake).expect("fake host");
        World {
            env,
            clock,
            store,
            host,
        }
    }

    fn add_task(&self, name: &str) {
        let obj: Object = serde_json::from_value(task(name, true)).expect("task");
        self.store.create(obj, FieldManager::Apply).expect("create");
    }

    fn spawns(&self) -> u64 {
        self.host.calls("spawn").expect("calls")
    }

    fn serve(&self, filling: &Filling, mut wait: impl FnMut(&RunReport) -> bool) {
        let runtime = FakeRuntime::new(FakeHost::open(&self.env.fake).expect("fake host"));
        let manager = Manager {
            plugins: None,
            store: filling,
            runtime: &runtime,
            clock: self.clock.as_ref(),
            holder: "disk-full-test".into(),
            forge: None,
            signals: None,
            agent_telemetry: None,
            change_request_defaults: Default::default(),
            forge_defaults: Default::default(),
            gateway: None,
        };
        manager
            .serve(
                RunOptions {
                    max_passes: 5,
                    lease_ttl: TTL,
                },
                &mut wait,
            )
            .expect("a full disk pauses the manager; it never ends it");
    }

    fn filling(&self, disk: Disk) -> Filling<'_> {
        Filling {
            inner: &self.store,
            data: self.env.data.clone(),
            disk: Cell::new(disk),
        }
    }
}

#[test]
fn a_full_disk_pauses_the_manager_and_it_resumes_on_its_own_expired_lease() {
    let w = World::new("disk-full-pause");
    let filling = w.filling(Disk::Free);
    let seen: RefCell<Vec<(Option<u64>, u64, u64)>> = RefCell::new(Vec::new());
    let lease_path = w.env.data.join("lease.json").display().to_string();
    w.serve(&filling, |report| {
        let paused = report.paused.as_ref().map(|p| {
            assert_eq!(p.why, format!("disk full at {lease_path}"), "{report:#?}");
            p.retry_after
        });
        seen.borrow_mut()
            .push((paused, w.spawns(), report.lease_token));
        match seen.borrow().len() {
            1 => {
                w.add_task("first");
                filling.disk.set(Disk::Full);
            }
            2 => {
                w.clock.0.fetch_add(TTL * 3, Ordering::SeqCst);
            }
            3..=5 => {}
            6 => filling.disk.set(Disk::Free),
            7 => {}
            _ => return false,
        }
        true
    });
    let seen = seen.into_inner();
    let token = seen[0].2;
    assert_eq!(
        seen.iter().map(|s| s.0).collect::<Vec<_>>(),
        [
            None,
            Some(5_000),
            Some(10_000),
            Some(20_000),
            Some(40_000),
            Some(60_000),
            None,
            None,
        ],
        "a full disk pauses, backing off from 5 s up to 60 s, and resumes: {seen:?}"
    );
    assert!(
        seen[..6].iter().all(|s| s.1 == 0),
        "no effect runs while its intent cannot be persisted: {seen:?}"
    );
    assert_eq!(
        seen[6].1, 1,
        "the Task spawns once the disk has room: {seen:?}"
    );
    assert_eq!(seen[7].1, 1, "and only once: {seen:?}");
    assert!(
        seen.iter().all(|s| s.2 == token),
        "the manager renews its own expired lease with an unchanged token: {seen:?}"
    );
}

#[test]
fn a_disk_that_fills_mid_sweep_pauses_before_the_effect_and_runs_it_once_later() {
    let w = World::new("disk-full-mid-sweep");
    w.add_task("first");
    let filling = w.filling(Disk::FullAfterLease);
    let seen: RefCell<Vec<(bool, u64)>> = RefCell::new(Vec::new());
    w.serve(&filling, |report| {
        seen.borrow_mut()
            .push((report.paused.is_some(), w.spawns()));
        match seen.borrow().len() {
            1 => filling.disk.set(Disk::Free),
            2 => {}
            _ => return false,
        }
        true
    });
    assert_eq!(
        seen.into_inner(),
        [(true, 0), (false, 1), (false, 1)],
        "the intent's write fails, so the spawn waits for room and then runs once"
    );
}

#[test]
fn a_manager_that_starts_on_a_full_disk_waits_for_room() {
    let w = World::new("disk-full-start");
    w.add_task("first");
    let filling = w.filling(Disk::Full);
    let seen: RefCell<Vec<(bool, u64)>> = RefCell::new(Vec::new());
    w.serve(&filling, |report| {
        seen.borrow_mut()
            .push((report.paused.is_some(), w.spawns()));
        match seen.borrow().len() {
            1 => filling.disk.set(Disk::Free),
            2 => {}
            _ => return false,
        }
        true
    });
    assert_eq!(seen.into_inner(), [(true, 0), (false, 1), (false, 1)]);
}

#[test]
fn a_manager_stopped_while_paused_stops_cleanly() {
    let w = World::new("disk-full-stop");
    let filling = w.filling(Disk::Free);
    let mut sweeps = 0;
    w.serve(&filling, |report| {
        sweeps += 1;
        if sweeps == 1 {
            filling.disk.set(Disk::Full);
            return true;
        }
        assert!(report.paused.is_some(), "{report:#?}");
        false
    });
    assert_eq!(sweeps, 2);
}
