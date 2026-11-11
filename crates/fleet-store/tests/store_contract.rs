//! The store contract against the file store: exclusive create,
//! compare-and-swap without lost updates, torn and crashed writes, field
//! ownership, the manager lease and finalizers.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::thread;

use fleet_api::task::TaskStatus;
use fleet_api::{
    Clock, DeleteOutcome, FieldManager, Object, Observation, OwnerReference, Store, StoreError,
};
use fleet_store::FileStore;
use serde_json::{Value, json};

static SEQ: AtomicU32 = AtomicU32::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("store")
            .join(format!("{name}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        Dir(p)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        if !thread::panicking() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[derive(Debug, Default)]
struct TestClock(AtomicU64);

impl Clock for TestClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl TestClock {
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

fn task(name: &str) -> Object {
    serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Task",
        "metadata": {"name": name},
        "spec": {"harness": "h", "brief": "b", "workspace": []}
    }))
    .unwrap()
}

fn known(o: Observation<Option<Object>>) -> Option<Object> {
    match o {
        Observation::Known(v) => v,
        Observation::Unknown { why } => panic!("unexpected Unknown: {why}"),
    }
}

fn status(phase: &str) -> Value {
    json!({"phase": phase, "observedGeneration": 1})
}

#[test]
fn create_is_exclusive_under_a_race() {
    let dir = Dir::new("create-race");
    let store = FileStore::open(&dir.0).unwrap();
    drop(store);
    let wins = Arc::new(AtomicU32::new(0));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (root, wins) = (dir.0.clone(), wins.clone());
            thread::spawn(move || {
                let store = FileStore::open(&root).unwrap();
                match store.create(task("same"), FieldManager::Apply) {
                    Ok(_) => {
                        wins.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(StoreError::AlreadyExists(_)) => {}
                    Err(e) => panic!("{e}"),
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(wins.load(Ordering::SeqCst), 1);
    assert!(
        fs::read_dir(dir.0.join("tmp")).unwrap().next().is_none(),
        "no temp file left"
    );
}

#[test]
fn compare_and_swap_loses_no_update() {
    let dir = Dir::new("cas");
    let store = FileStore::open(&dir.0).unwrap();
    store.create(task("counter"), FieldManager::Apply).unwrap();
    let threads = 6;
    let per = 15;
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let root = dir.0.clone();
            thread::spawn(move || {
                let store = FileStore::open(&root).unwrap();
                let mut conflicts = 0;
                for _ in 0..per {
                    loop {
                        let cur = known(store.get("Task", "counter")).unwrap();
                        let n: u64 = cur
                            .metadata
                            .labels
                            .get("n")
                            .map_or(0, |v| v.parse().unwrap());
                        let labels = BTreeMap::from([("n".to_string(), (n + 1).to_string())]);
                        let rv = cur.metadata.resource_version;
                        match store.update_spec(
                            "Task",
                            "counter",
                            cur.spec.clone(),
                            Some(labels),
                            rv,
                            FieldManager::Apply,
                        ) {
                            Ok(_) => break,
                            Err(StoreError::Conflict { .. }) => conflicts += 1,
                            Err(e) => panic!("{e}"),
                        }
                    }
                }
                conflicts
            })
        })
        .collect();
    let conflicts: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let end = known(store.get("Task", "counter")).unwrap();
    assert_eq!(
        end.metadata.labels["n"],
        (threads * per).to_string(),
        "every increment landed"
    );
    assert_eq!(end.metadata.generation, 1, "labels do not bump generation");
    // Not asserted to be non-zero: the race is real but not guaranteed.
    let _ = conflicts;
}

#[test]
fn a_stale_write_is_a_conflict() {
    let dir = Dir::new("stale");
    let store = FileStore::open(&dir.0).unwrap();
    let a = store.create(task("t"), FieldManager::Apply).unwrap();
    let mut spec = a.spec.clone();
    spec["dispatch"] = json!(true);
    let b = store
        .update_spec(
            "Task",
            "t",
            spec.clone(),
            None,
            a.metadata.resource_version,
            FieldManager::Apply,
        )
        .unwrap();
    assert_eq!(b.metadata.generation, 2);
    let err = store
        .update_spec(
            "Task",
            "t",
            spec,
            None,
            a.metadata.resource_version,
            FieldManager::Apply,
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict { expected, actual, .. } if expected == a.metadata.resource_version && actual == b.metadata.resource_version)
    );
}

#[test]
fn a_crashed_write_leaves_the_old_record_and_no_temp() {
    let dir = Dir::new("torn");
    let store = FileStore::open(&dir.0).unwrap();
    let a = store.create(task("t"), FieldManager::Apply).unwrap();
    drop(store);
    // A writer that died after writing its temp file and before the rename.
    fs::write(dir.0.join("tmp").join("999-0.tmp"), b"{\"half\": ").unwrap();
    let store = FileStore::open(&dir.0).unwrap();
    assert!(
        fs::read_dir(dir.0.join("tmp")).unwrap().next().is_none(),
        "open removes a dead writer's temp file"
    );
    assert_eq!(
        known(store.get("Task", "t")).unwrap(),
        a,
        "the record is the last complete one"
    );
}

#[test]
fn a_torn_record_is_unknown_and_blocks_writes() {
    let dir = Dir::new("corrupt");
    let store = FileStore::open(&dir.0).unwrap();
    store.create(task("t"), FieldManager::Apply).unwrap();
    let path = dir.0.join("objects").join("Task").join("t.json");
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    assert!(store.get("Task", "t").is_unknown());
    assert!(store.list("Task").is_unknown(), "never a partial list");
    assert!(
        matches!(
            store.create(task("t"), FieldManager::Apply),
            Err(StoreError::Unknown(_))
        ),
        "a create cannot tell whether the name is free"
    );
    assert!(matches!(
        store.delete("Task", "t", FieldManager::Apply),
        Err(StoreError::Unknown(_))
    ));
    assert_eq!(
        known(store.get("Task", "absent")),
        None,
        "absence is still Known"
    );
}

#[test]
fn a_record_holding_another_name_is_corrupt() {
    let dir = Dir::new("misfiled");
    let store = FileStore::open(&dir.0).unwrap();
    store.create(task("a"), FieldManager::Apply).unwrap();
    let objects = dir.0.join("objects").join("Task");
    fs::copy(objects.join("a.json"), objects.join("b.json")).unwrap();
    assert!(store.get("Task", "b").is_unknown());
}

#[test]
fn each_field_manager_writes_only_its_own_status_fields() {
    let dir = Dir::new("owners");
    let clock = Arc::new(TestClock::default());
    let store = FileStore::open_with_clock(&dir.0, clock.clone()).unwrap();
    let t = store.create(task("t"), FieldManager::Apply).unwrap();
    let lease = store.acquire_lease("m", 10_000).unwrap();

    let err = store
        .update_status(
            "Task",
            "t",
            status("Briefed"),
            t.metadata.resource_version,
            FieldManager::Worker,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Forbidden(ref m) if m.contains("worker may not write status.")),
        "{err}"
    );
    let err = store
        .update_status(
            "Task",
            "t",
            status("Briefed"),
            t.metadata.resource_version,
            FieldManager::Apply,
            None,
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::Forbidden(_)), "{err}");

    let t = store
        .update_status(
            "Task",
            "t",
            status("Briefed"),
            t.metadata.resource_version,
            FieldManager::TaskController,
            Some(&lease),
        )
        .unwrap();
    let mut with_result = t.status.clone();
    with_result["result"] = json!({"outcome": "shipped", "generation": 1});
    let err = store
        .update_status(
            "Task",
            "t",
            with_result.clone(),
            t.metadata.resource_version,
            FieldManager::TaskController,
            Some(&lease),
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Forbidden(ref m) if m.contains("status.result")),
        "{err}"
    );
    let t = store
        .update_status(
            "Task",
            "t",
            with_result,
            t.metadata.resource_version,
            FieldManager::Worker,
            None,
        )
        .unwrap();
    let st: TaskStatus = serde_json::from_value(t.status).unwrap();
    assert!(st.result.is_some());

    let err = store.update_status(
        "Task",
        "t",
        json!({"phase": "NotAPhase"}),
        t.metadata.resource_version,
        FieldManager::TaskController,
        Some(&lease),
    );
    assert!(
        matches!(err, Err(StoreError::Invalid(_))),
        "status must parse as the kind's status"
    );
    let cfg: Object =
        serde_json::from_value(json!({"apiVersion": "fleet/v1alpha1", "kind": "TaskConfig",
        "metadata": {"name": "worker"}, "spec": {"harness": "h", "hostRef": "local"}}))
        .unwrap();
    assert!(
        matches!(
            store.create(cfg, FieldManager::Apply),
            Err(StoreError::Invalid(_))
        ),
        "unsupported kinds are refused"
    );
    let err = store.update_status(
        "TaskConfig",
        "worker",
        json!({}),
        1,
        FieldManager::TaskController,
        Some(&lease),
    );
    assert!(
        matches!(err, Err(StoreError::Forbidden(_))),
        "no other kind has a status"
    );
}

#[test]
fn a_controller_writes_only_under_the_current_lease() {
    let dir = Dir::new("lease");
    let clock = Arc::new(TestClock::default());
    let store = FileStore::open_with_clock(&dir.0, clock.clone()).unwrap();
    let t = store.create(task("t"), FieldManager::Apply).unwrap();
    let rv = t.metadata.resource_version;

    let err = store
        .update_status(
            "Task",
            "t",
            status("Spawning"),
            rv,
            FieldManager::TaskController,
            None,
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::LeaseLost(_)));

    let a = store.acquire_lease("a", 1_000).unwrap();
    assert!(
        matches!(store.acquire_lease("b", 1_000), Err(StoreError::LeaseHeld { ref holder, .. }) if holder == "a")
    );
    assert!(
        matches!(
            store.acquire_lease("a", 1_000),
            Err(StoreError::LeaseHeld { .. })
        ),
        "not even under the same holder name"
    );
    let a = store.renew_lease(&a, 1_000).unwrap();

    clock.advance(1_001);
    let err = store
        .update_status(
            "Task",
            "t",
            status("Spawning"),
            rv,
            FieldManager::TaskController,
            Some(&a),
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::LeaseLost(_)),
        "an expired lease fences its holder"
    );
    let b = store.acquire_lease("b", 1_000).unwrap();
    assert!(
        b.token > a.token,
        "the fencing token grows when the lease changes hands"
    );
    assert!(
        matches!(store.renew_lease(&a, 1_000), Err(StoreError::LeaseLost(_))),
        "a lost lease cannot be renewed back"
    );
    store
        .update_status(
            "Task",
            "t",
            status("Spawning"),
            rv,
            FieldManager::TaskController,
            Some(&b),
        )
        .unwrap();

    store.release_lease(&b).unwrap();
    let c = store.acquire_lease("c", 1_000).unwrap();
    assert!(c.token > b.token, "release keeps the token monotonic");
    assert!(matches!(
        store.update_status(
            "Task",
            "t",
            status("Spawned"),
            rv + 1,
            FieldManager::TaskController,
            Some(&b)
        ),
        Err(StoreError::LeaseLost(_))
    ));
}

#[test]
fn finalizers_hold_deletion_and_only_the_controller_removes_them() {
    let dir = Dir::new("finalizers");
    let store = FileStore::open(&dir.0).unwrap();
    let t = store.create(task("t"), FieldManager::Apply).unwrap();
    assert_eq!(t.metadata.finalizers, vec!["fleet.dev/release".to_string()]);
    assert!(matches!(
        store.delete("Task", "t", FieldManager::TaskController),
        Err(StoreError::Forbidden(_))
    ));
    assert_eq!(
        store.delete("Task", "t", FieldManager::Apply).unwrap(),
        DeleteOutcome::Marked
    );
    let t = known(store.get("Task", "t")).expect("still there");
    assert!(t.is_deleting());
    let mut spec = t.spec.clone();
    spec["dispatch"] = json!(true);
    assert!(matches!(
        store.update_spec(
            "Task",
            "t",
            spec,
            None,
            t.metadata.resource_version,
            FieldManager::Apply
        ),
        Err(StoreError::Forbidden(_))
    ));

    let lease = store.acquire_lease("m", 10_000).unwrap();
    let rv = t.metadata.resource_version;
    assert!(matches!(
        store.remove_finalizer(
            "Task",
            "t",
            "fleet.dev/release",
            rv,
            FieldManager::Worker,
            &lease
        ),
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        store.remove_finalizer(
            "Task",
            "t",
            "fleet.dev/release",
            rv - 1,
            FieldManager::TaskController,
            &lease
        ),
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(
        store
            .remove_finalizer(
                "Task",
                "t",
                "fleet.dev/release",
                rv,
                FieldManager::TaskController,
                &lease
            )
            .unwrap(),
        DeleteOutcome::Removed
    );
    assert_eq!(known(store.get("Task", "t")), None);
}

#[test]
fn nothing_owns_a_task() {
    let dir = Dir::new("no-owners");
    let store = FileStore::open(&dir.0).unwrap();
    let parent = store.create(task("parent"), FieldManager::Apply).unwrap();
    let mut child = task("child");
    child.metadata.owner_references = vec![OwnerReference {
        kind: "Task".into(),
        name: "parent".into(),
        uid: parent.metadata.uid.clone(),
        controller: true,
    }];
    assert!(
        matches!(
            store.create(child, FieldManager::Apply),
            Err(StoreError::Invalid(_))
        ),
        "an owner reference is refused"
    );
    assert_eq!(known(store.get("Task", "child")), None);
}
#[test]
fn metadata_the_store_assigns_cannot_be_supplied() {
    let dir = Dir::new("assigned");
    let store = FileStore::open(&dir.0).unwrap();
    let mut t = task("t");
    t.metadata.uid = "u1".into();
    assert!(matches!(
        store.create(t, FieldManager::Apply),
        Err(StoreError::Invalid(_))
    ));
    let mut t = task("t");
    t.status = json!({"phase": "Concluded"});
    assert!(matches!(
        store.create(t, FieldManager::Apply),
        Err(StoreError::Invalid(_))
    ));
    let mut t = task("../escape");
    t.metadata.name = "../escape".into();
    assert!(matches!(
        store.create(t, FieldManager::Apply),
        Err(StoreError::Invalid(_))
    ));
    assert!(
        store.get("Task", "../escape").is_unknown(),
        "an invalid name is refused, not looked up"
    );
}

#[test]
fn a_foreign_or_newer_directory_is_refused() {
    let dir = Dir::new("foreign");
    fs::create_dir_all(&dir.0).unwrap();
    fs::write(dir.0.join("notes.txt"), b"x").unwrap();
    assert!(matches!(
        FileStore::open(&dir.0),
        Err(StoreError::Invalid(_))
    ));
    let dir = Dir::new("newer");
    fs::create_dir_all(&dir.0).unwrap();
    fs::write(
        dir.0.join("store.json"),
        br#"{"format": "fleet-store", "version": 2}"#,
    )
    .unwrap();
    assert!(matches!(
        FileStore::open(&dir.0),
        Err(StoreError::Unsupported(_))
    ));
}

#[cfg(unix)]
#[test]
fn not_found_and_inaccessible_are_different_answers() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Dir::new("perm");
    let store = FileStore::open(&dir.0).unwrap();
    store.create(task("t"), FieldManager::Apply).unwrap();
    let kind_dir = dir.0.join("objects").join("Task");
    fs::set_permissions(&kind_dir, fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read_dir(&kind_dir).is_ok();
    let (get, list) = (store.get("Task", "t"), store.list("Task"));
    fs::set_permissions(&kind_dir, fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        return; // running as root: permissions do not apply
    }
    assert!(get.is_unknown(), "{get:?}");
    assert!(list.is_unknown(), "{list:?}");
}

#[test]
fn a_finalizer_never_comes_off_a_live_object() {
    let dir = Dir::new("live-finalizer");
    let store = FileStore::open(&dir.0).unwrap();
    let t = store.create(task("t"), FieldManager::Apply).unwrap();
    let lease = store.acquire_lease("m", 10_000).unwrap();
    let err = store
        .remove_finalizer(
            "Task",
            "t",
            "fleet.dev/release",
            t.metadata.resource_version,
            FieldManager::TaskController,
            &lease,
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::Forbidden(_)), "{err}");
    // So a later delete still waits for the release.
    assert_eq!(
        store.delete("Task", "t", FieldManager::Apply).unwrap(),
        DeleteOutcome::Marked
    );
}

#[test]
fn a_stale_token_is_refused_even_under_the_same_holder_name() {
    let dir = Dir::new("same-holder");
    let clock = Arc::new(TestClock::default());
    let store = FileStore::open_with_clock(&dir.0, clock.clone()).unwrap();
    let t = store.create(task("t"), FieldManager::Apply).unwrap();
    let old = store.acquire_lease("m", 1_000).unwrap();
    clock.advance(1_001);
    let new = store.acquire_lease("m", 1_000).unwrap();
    assert_eq!(old.holder, new.holder);
    assert!(new.token > old.token);
    let rv = t.metadata.resource_version;
    let err = store
        .update_status(
            "Task",
            "t",
            status("Spawning"),
            rv,
            FieldManager::TaskController,
            Some(&old),
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::LeaseLost(_)),
        "the token, not the name, fences: {err}"
    );
    assert!(matches!(
        store.renew_lease(&old, 1_000),
        Err(StoreError::LeaseLost(_))
    ));
    store
        .update_status(
            "Task",
            "t",
            status("Spawning"),
            rv,
            FieldManager::TaskController,
            Some(&new),
        )
        .unwrap();
}

#[test]
fn concurrent_creates_of_distinct_objects_get_distinct_uids_and_versions() {
    let dir = Dir::new("distinct");
    drop(FileStore::open(&dir.0).unwrap());
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let root = dir.0.clone();
            thread::spawn(move || {
                let store = FileStore::open(&root).unwrap();
                (0..5)
                    .map(|j| {
                        store
                            .create(task(&format!("t{i}-{j}")), FieldManager::Apply)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let all: Vec<Object> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    let uids: std::collections::BTreeSet<_> = all.iter().map(|o| o.metadata.uid.clone()).collect();
    let rvs: std::collections::BTreeSet<_> =
        all.iter().map(|o| o.metadata.resource_version).collect();
    assert_eq!(
        (uids.len(), rvs.len()),
        (40, 40),
        "every uid and resourceVersion is unique"
    );
    match FileStore::open(&dir.0).unwrap().list("Task") {
        Observation::Known(objs) => assert_eq!(objs.len(), 40),
        Observation::Unknown { why } => panic!("{why}"),
    }
}

#[test]
fn a_writer_waits_out_a_slow_lock_holder_instead_of_failing_busy() {
    // Under machine load a holder's fsyncs can keep the store lock longer
    // than any fixed polling bound; a waiter must still get through rather
    // than fail Busy, because every holder is making progress and the OS
    // frees the lock if one dies.
    let dir = Dir::new("slow-holder");
    let store = FileStore::open(&dir.0).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.0.join("lock"))
        .unwrap();
    lock.lock().unwrap();
    let root = dir.0.clone();
    let waiter = thread::spawn(move || {
        FileStore::open(&root).and_then(|s| s.create(task("late"), FieldManager::Apply))
    });
    thread::sleep(std::time::Duration::from_secs(11));
    lock.unlock().unwrap();
    let created = waiter.join().unwrap();
    assert!(
        created.is_ok(),
        "the waiter must get the lock once it is free: {created:?}"
    );
    assert!(known(store.get("Task", "late")).is_some());
}

fn change_request(name: &str) -> Object {
    serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "ChangeRequest",
        "metadata": {"name": name},
        "spec": {"url": "https://forge.test/acme/widgets/pull/7"}
    }))
    .unwrap()
}

/// A controller retires its own object by the uid and resourceVersion it
/// decided from, under the lease: a recreated object under the same name, a
/// rewritten one, a stale lease and another field manager all remove nothing.
#[test]
fn a_controllers_delete_is_fenced_by_uid_version_and_lease() {
    let dir = Dir::new("delete-fenced");
    let store = FileStore::open(&dir.0).unwrap();
    let cr = FieldManager::ChangeRequestController;
    let created = store
        .create(change_request("cr-1"), FieldManager::Apply)
        .unwrap();
    let (uid, rv) = (
        created.metadata.uid.clone(),
        created.metadata.resource_version,
    );
    let lease = store.acquire_lease("m", 60_000).unwrap();

    assert!(matches!(
        store.delete_fenced(
            "ChangeRequest",
            "cr-1",
            &uid,
            rv,
            FieldManager::Apply,
            &lease
        ),
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        store.delete_fenced("ChangeRequest", "cr-1", &uid, rv + 1, cr.clone(), &lease),
        Err(StoreError::Conflict { .. })
    ));
    // Recreated under the same name: a new uid the old plan did not name.
    store
        .delete("ChangeRequest", "cr-1", FieldManager::Apply)
        .unwrap();
    let again = store
        .create(change_request("cr-1"), FieldManager::Apply)
        .unwrap();
    assert_ne!(again.metadata.uid, uid);
    assert!(matches!(
        store.delete_fenced(
            "ChangeRequest",
            "cr-1",
            &uid,
            again.metadata.resource_version,
            cr.clone(),
            &lease
        ),
        Err(StoreError::Conflict { .. })
    ));
    assert!(
        known(store.get("ChangeRequest", "cr-1")).is_some(),
        "still there"
    );
    // A stale lease is refused even with the right identity.
    store.release_lease(&lease).unwrap();
    let (uid2, rv2) = (again.metadata.uid.clone(), again.metadata.resource_version);
    assert!(matches!(
        store.delete_fenced("ChangeRequest", "cr-1", &uid2, rv2, cr.clone(), &lease),
        Err(StoreError::LeaseLost(_))
    ));
    let lease = store.acquire_lease("m", 60_000).unwrap();
    assert_eq!(
        store
            .delete_fenced("ChangeRequest", "cr-1", &uid2, rv2, cr.clone(), &lease)
            .unwrap(),
        DeleteOutcome::Removed,
        "no finalizer: the record goes at once"
    );
    assert!(known(store.get("ChangeRequest", "cr-1")).is_none());
}

#[test]
fn change_request_status_belongs_to_its_controller_alone() {
    let dir = Dir::new("cr-status-owner");
    let store = FileStore::open(&dir.0).unwrap();
    let created = store
        .create(change_request("cr-2"), FieldManager::Apply)
        .unwrap();
    let rv = created.metadata.resource_version;
    let lease = store.acquire_lease("m", 60_000).unwrap();
    let status = json!({"phase": "Watching", "revision": 1, "everReady": true, "droppedBefore": 0});
    for by in [
        FieldManager::Apply,
        FieldManager::Worker,
        FieldManager::TaskController,
    ] {
        let err = store
            .update_status(
                "ChangeRequest",
                "cr-2",
                status.clone(),
                rv,
                by.clone(),
                Some(&lease),
            )
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Forbidden(_) | StoreError::LeaseLost(_)),
            "{by:?}: {err}"
        );
    }
    assert!(
        matches!(
            store.update_status(
                "ChangeRequest",
                "cr-2",
                json!({"phase": "Elsewhere"}),
                rv,
                FieldManager::ChangeRequestController,
                Some(&lease)
            ),
            Err(StoreError::Invalid(_))
        ),
        "a status its readers would refuse is not stored"
    );
    store
        .update_status(
            "ChangeRequest",
            "cr-2",
            status,
            rv,
            FieldManager::ChangeRequestController,
            Some(&lease),
        )
        .unwrap();
    // The spec is fixed once created.
    let mut other = change_request("cr-2").spec;
    other["url"] = json!("https://forge.test/acme/widgets/pull/8");
    let current = known(store.get("ChangeRequest", "cr-2")).unwrap();
    assert!(matches!(
        store.update_spec(
            "ChangeRequest",
            "cr-2",
            other,
            None,
            current.metadata.resource_version,
            FieldManager::Apply
        ),
        Err(StoreError::Invalid(_))
    ));
}
// --- The inbox: two typed mutations on a field nobody owns. ---------------

use fleet_api::inbox::{DeliveryReceipt, InboxAbout, InboxEntry, MAX_DELIVERIES, MAX_INBOX};

fn entry(key: &str) -> InboxEntry {
    InboxEntry {
        key: key.into(),
        from: "cli/tester".into(),
        text: format!("message {key}"),
        about: None,
        answers: Vec::new(),
        amends: None,
        queued_at: 1,
    }
}

fn receipt(key: &str) -> DeliveryReceipt {
    DeliveryReceipt {
        key: key.into(),
        from: String::new(),
        at: 2,
        recovered: false,
        delivered_via: None,
        may_hold: false,
    }
}

fn inbox_status(obj: &Object) -> TaskStatus {
    serde_json::from_value(obj.status.clone()).unwrap_or_default()
}

#[test]
fn an_inbox_key_is_refused_while_queued_or_receipted() {
    let dir = Dir::new("inbox-dup");
    let store = FileStore::open(&dir.0).unwrap();
    let t = store.create(task("t"), FieldManager::Apply).unwrap();
    let lease = store.acquire_lease("m", 10_000).unwrap();
    let t = store
        .append_inbox(
            "t",
            entry("k1"),
            t.metadata.resource_version,
            FieldManager::Cli,
            None,
        )
        .unwrap();
    let err = store
        .append_inbox(
            "t",
            entry("k1"),
            t.metadata.resource_version,
            FieldManager::Cli,
            None,
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::AlreadyExists(_)), "{err}");
    assert_eq!(inbox_status(&t).inbox.len(), 1, "nothing was written");

    // Delivered: the receipt in, the entry out, one write.
    let rv = t.metadata.resource_version;
    let t = store
        .record_delivery(
            "t",
            t.status.clone(),
            receipt("k1"),
            rv,
            FieldManager::TaskController,
            &lease,
        )
        .unwrap();
    assert_eq!(t.metadata.resource_version, rv + 1, "one write");
    let st = inbox_status(&t);
    assert!(st.inbox.is_empty());
    assert_eq!(st.deliveries.receipts.len(), 1);
    assert_eq!(st.deliveries.receipts[0].key, "k1");
    assert_eq!(st.deliveries.receipts[0].from, "cli/tester");
    let err = store
        .append_inbox(
            "t",
            entry("k1"),
            t.metadata.resource_version,
            FieldManager::Cli,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::AlreadyExists(_)),
        "a receipted key is refused too: {err}"
    );
    // Recording a delivery nobody queued is refused, not invented.
    assert!(matches!(
        store.record_delivery(
            "t",
            t.status.clone(),
            receipt("k9"),
            t.metadata.resource_version,
            FieldManager::TaskController,
            &lease,
        ),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn the_inbox_bound_and_the_delivered_revision_hold_on_append() {
    let dir = Dir::new("inbox-bound");
    let store = FileStore::open(&dir.0).unwrap();
    let mut t = store.create(task("t"), FieldManager::Apply).unwrap();
    for i in 0..=MAX_INBOX {
        t = store
            .append_inbox(
                "t",
                entry(&format!("k{i}")),
                t.metadata.resource_version,
                FieldManager::Cli,
                None,
            )
            .unwrap_or_else(|e| panic!("append {i}: {e}"));
    }
    assert_eq!(inbox_status(&t).inbox.len(), MAX_INBOX + 1);
    let err = store
        .append_inbox(
            "t",
            entry("one-more"),
            t.metadata.resource_version,
            FieldManager::Cli,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Forbidden(ref m) if m.contains("still to deliver")),
        "{err}"
    );
    assert_eq!(inbox_status(&t).inbox.len(), MAX_INBOX + 1);

    // An entry's revision survives its receipt's rotation: the store keeps
    // the highest delivered revision per source. (A relay's source is
    // `forge/<change>`; until that caller exists, the rule is exercised on
    // a `cli/` source carrying `about`.)
    let dir2 = Dir::new("inbox-revision");
    let store2 = FileStore::open(&dir2.0).unwrap();
    let mut r = store2.create(task("r"), FieldManager::Apply).unwrap();
    let relay = |key: &str, revision: u64| InboxEntry {
        from: "cli/relay".into(),
        about: Some(InboxAbout {
            uid: "c1".into(),
            revision,
            head: Some("abc".into()),
        }),
        ..entry(key)
    };
    // Two hundred durable writes under one lease: it must outlast a loaded
    // gate, which is not what this case is about.
    let lease2 = store2.acquire_lease("m", 60_000).unwrap();
    r = store2
        .append_inbox(
            "r",
            relay("relay/7", 7),
            r.metadata.resource_version,
            FieldManager::Cli,
            None,
        )
        .unwrap();
    r = store2
        .record_delivery(
            "r",
            r.status.clone(),
            receipt("relay/7"),
            r.metadata.resource_version,
            FieldManager::TaskController,
            &lease2,
        )
        .unwrap();
    for i in 0..MAX_DELIVERIES {
        let k = format!("s{i}");
        r = store2
            .append_inbox(
                "r",
                entry(&k),
                r.metadata.resource_version,
                FieldManager::Cli,
                None,
            )
            .unwrap();
        r = store2
            .record_delivery(
                "r",
                r.status.clone(),
                receipt(&k),
                r.metadata.resource_version,
                FieldManager::TaskController,
                &lease2,
            )
            .unwrap();
    }
    let st = inbox_status(&r);
    assert_eq!(
        st.deliveries.receipts.len(),
        MAX_DELIVERIES,
        "newest 100 kept"
    );
    assert!(
        !st.deliveries.delivered("relay/7"),
        "the oldest rotated out"
    );
    assert_eq!(
        st.deliveries.revisions["c1"], 7,
        "kept per mirror uid, past the receipt"
    );
    let err = store2
        .append_inbox(
            "r",
            relay("relay/7b", 7),
            r.metadata.resource_version,
            FieldManager::Cli,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Forbidden(ref m) if m.contains("revision 7")),
        "{err}"
    );
    store2
        .append_inbox(
            "r",
            relay("relay/8", 8),
            r.metadata.resource_version,
            FieldManager::Cli,
            None,
        )
        .unwrap();
}

#[test]
fn only_named_callers_reach_the_inbox_and_never_through_a_status_write() {
    let dir = Dir::new("inbox-callers");
    let store = FileStore::open(&dir.0).unwrap();
    let t = store.create(task("t"), FieldManager::Apply).unwrap();
    let lease = store.acquire_lease("m", 10_000).unwrap();
    let rv = t.metadata.resource_version;
    for by in [
        FieldManager::Apply,
        FieldManager::Worker,
        FieldManager::TaskController,
    ] {
        let err = store
            .append_inbox("t", entry("k1"), rv, by.clone(), Some(&lease))
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)), "{by:?}: {err}");
    }
    let mut mislabelled = entry("k1");
    mislabelled.from = "forge/cr-1".into();
    assert!(matches!(
        store.append_inbox("t", mislabelled, rv, FieldManager::Cli, None),
        Err(StoreError::Invalid(_))
    ));
    let t = store
        .append_inbox("t", entry("k1"), rv, FieldManager::Cli, None)
        .unwrap();

    // No manager replaces the list: not the controller, not the worker.
    let mut replaced = t.status.clone();
    replaced["inbox"] = json!([]);
    for (by, lease) in [
        (FieldManager::TaskController, Some(&lease)),
        (FieldManager::Worker, None),
    ] {
        let err = store
            .update_status(
                "Task",
                "t",
                replaced.clone(),
                t.metadata.resource_version,
                by.clone(),
                lease,
            )
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Forbidden(ref m) if m.contains("status.inbox")),
            "{by:?}: {err}"
        );
    }
    // Not even in the record-and-remove write itself.
    let mut edited = t.status.clone();
    edited["inbox"][0]["text"] = json!("tampered");
    let err = store
        .record_delivery(
            "t",
            edited,
            receipt("k1"),
            t.metadata.resource_version,
            FieldManager::TaskController,
            &lease,
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::Forbidden(_)), "{err}");
    for by in [FieldManager::Cli, FieldManager::Worker, FieldManager::Apply] {
        let err = store
            .record_delivery(
                "t",
                t.status.clone(),
                receipt("k1"),
                t.metadata.resource_version,
                by.clone(),
                &lease,
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)), "{by:?}: {err}");
    }
    // The controller's own fields still move in that write, under the lease.
    let mut moved = t.status.clone();
    moved["phase"] = json!("Briefed");
    let t = store
        .record_delivery(
            "t",
            moved,
            receipt("k1"),
            t.metadata.resource_version,
            FieldManager::TaskController,
            &lease,
        )
        .unwrap();
    assert_eq!(t.status["phase"], "Briefed");
    assert!(t.status["inbox"].as_array().is_none_or(Vec::is_empty));
    let stale = store.acquire_lease("n", 10_000);
    assert!(stale.is_err(), "the lease is held");
    let t2 = store
        .append_inbox(
            "t",
            entry("k2"),
            t.metadata.resource_version,
            FieldManager::Cli,
            None,
        )
        .unwrap();
    store.release_lease(&lease).unwrap();
    assert!(matches!(
        store.record_delivery(
            "t",
            t2.status.clone(),
            receipt("k2"),
            t2.metadata.resource_version,
            FieldManager::TaskController,
            &lease
        ),
        Err(StoreError::LeaseLost(_))
    ));
    // A deleting Task takes no more messages.
    store.delete("Task", "t", FieldManager::Apply).unwrap();
    let t = known(store.get("Task", "t")).unwrap();
    assert!(matches!(
        store.append_inbox(
            "t",
            entry("k3"),
            t.metadata.resource_version,
            FieldManager::Cli,
            None
        ),
        Err(StoreError::Forbidden(_))
    ));
}

#[test]
fn concurrent_appends_and_removes_lose_nothing() {
    let dir = Dir::new("inbox-race");
    let store = FileStore::open(&dir.0).unwrap();
    store.create(task("t"), FieldManager::Apply).unwrap();
    let lease = store.acquire_lease("m", 60_000).unwrap();
    let senders = 4;
    let per = 6;
    let appenders: Vec<_> = (0..senders)
        .map(|s| {
            let root = dir.0.clone();
            thread::spawn(move || {
                let store = FileStore::open(&root).unwrap();
                for i in 0..per {
                    loop {
                        let cur = known(store.get("Task", "t")).unwrap();
                        let e = entry(&format!("s{s}-{i}"));
                        match store.append_inbox(
                            "t",
                            e,
                            cur.metadata.resource_version,
                            FieldManager::Cli,
                            None,
                        ) {
                            Ok(_) => break,
                            Err(StoreError::Conflict { .. } | StoreError::Forbidden(_)) => {
                                thread::sleep(std::time::Duration::from_millis(2));
                            }
                            Err(e) => panic!("{e}"),
                        }
                    }
                }
            })
        })
        .collect();
    // The controller, meanwhile, delivers whatever is at the head.
    let mut delivered = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while delivered < senders * per {
        assert!(
            std::time::Instant::now() < deadline,
            "delivered {delivered}"
        );
        let cur = known(store.get("Task", "t")).unwrap();
        let st = inbox_status(&cur);
        let Some(head) = st.inbox.first() else {
            thread::sleep(std::time::Duration::from_millis(2));
            continue;
        };
        match store.record_delivery(
            "t",
            cur.status.clone(),
            receipt(&head.key),
            cur.metadata.resource_version,
            FieldManager::TaskController,
            &lease,
        ) {
            Ok(_) => delivered += 1,
            Err(StoreError::Conflict { .. }) => {}
            Err(e) => panic!("{e}"),
        }
    }
    for h in appenders {
        h.join().unwrap();
    }
    let st = inbox_status(&known(store.get("Task", "t")).unwrap());
    assert!(st.inbox.is_empty(), "{:?}", st.inbox);
    let keys: std::collections::BTreeSet<_> = st
        .deliveries
        .receipts
        .iter()
        .map(|r| r.key.clone())
        .collect();
    assert_eq!(
        keys.len(),
        senders * per,
        "every entry delivered exactly once"
    );
    assert!(
        fs::read_dir(dir.0.join("tmp")).unwrap().next().is_none(),
        "no temp file left"
    );
}

#[test]
fn a_torn_inbox_write_leaves_the_last_complete_record() {
    let dir = Dir::new("inbox-torn");
    let store = FileStore::open(&dir.0).unwrap();
    let t = store.create(task("t"), FieldManager::Apply).unwrap();
    let t = store
        .append_inbox(
            "t",
            entry("k1"),
            t.metadata.resource_version,
            FieldManager::Cli,
            None,
        )
        .unwrap();
    drop(store);
    // An appender that died after its temp file, before the rename.
    fs::write(dir.0.join("tmp").join("999-0.tmp"), b"{\"half\": ").unwrap();
    let store = FileStore::open(&dir.0).unwrap();
    assert!(fs::read_dir(dir.0.join("tmp")).unwrap().next().is_none());
    assert_eq!(known(store.get("Task", "t")).unwrap(), t);
    assert_eq!(inbox_status(&t).inbox.len(), 1);
    // A record torn in place is Unknown, and blocks both mutations.
    let path = dir.0.join("objects").join("Task").join("t.json");
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    let lease = store.acquire_lease("m", 10_000).unwrap();
    assert!(matches!(
        store.append_inbox(
            "t",
            entry("k2"),
            t.metadata.resource_version,
            FieldManager::Cli,
            None
        ),
        Err(StoreError::Unknown(_))
    ));
    assert!(matches!(
        store.record_delivery(
            "t",
            t.status.clone(),
            receipt("k1"),
            t.metadata.resource_version,
            FieldManager::TaskController,
            &lease
        ),
        Err(StoreError::Unknown(_))
    ));
    assert_eq!(
        fs::read(&path).unwrap(),
        &bytes[..bytes.len() / 2],
        "never repaired"
    );
}

// --- The relay: status.forge and the inbox entries land together. ---------

#[test]
fn a_relay_lands_the_cursor_and_its_entries_together_or_not_at_all() {
    use fleet_api::follow::{ChangeEntry, ForgeStatus};
    let dir = Dir::new("relay");
    let store = FileStore::open(&dir.0).unwrap();
    let t = store.create(task("t"), FieldManager::Apply).unwrap();
    let lease = store.acquire_lease("m", 10_000).unwrap();
    let relay = |key: &str, revision: u64| InboxEntry {
        from: "forge/cr-1".into(),
        about: Some(InboxAbout {
            uid: "c1".into(),
            revision,
            head: Some("abc".into()),
        }),
        ..entry(key)
    };
    let forge = |cursor: u64| {
        let mut f = ForgeStatus::default();
        f.changes
            .insert("cr-1".into(), ChangeEntry::joined("c1", cursor));
        serde_json::to_value(f).unwrap()
    };
    // Only the Forge controller, under the lease, naming its source.
    for by in [
        FieldManager::Apply,
        FieldManager::Worker,
        FieldManager::Cli,
        FieldManager::TaskController,
        FieldManager::ChangeRequestController,
    ] {
        let err = store
            .record_relay(
                "t",
                forge(1),
                vec![relay("relay/1", 1)],
                t.metadata.resource_version,
                by.clone(),
                &lease,
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)), "{by:?}: {err}");
    }
    let mut mislabelled = relay("relay/1", 1);
    mislabelled.from = "cli/forge".into();
    assert!(matches!(
        store.record_relay(
            "t",
            forge(1),
            vec![mislabelled],
            t.metadata.resource_version,
            FieldManager::ForgeController,
            &lease
        ),
        Err(StoreError::Invalid(_))
    ));
    let stale = fleet_api::LeaseToken {
        token: lease.token.saturating_sub(1),
        ..lease.clone()
    };
    assert!(matches!(
        store.record_relay(
            "t",
            forge(1),
            vec![relay("relay/1", 1)],
            t.metadata.resource_version,
            FieldManager::ForgeController,
            &stale
        ),
        Err(StoreError::LeaseLost(_))
    ));
    let t = store
        .record_relay(
            "t",
            forge(1),
            vec![relay("relay/1", 1)],
            t.metadata.resource_version,
            FieldManager::ForgeController,
            &lease,
        )
        .unwrap();
    let st = inbox_status(&t);
    assert_eq!(st.forge.as_ref().unwrap().changes["cr-1"].cursor, 1);
    assert_eq!(st.inbox.len(), 1);

    // A duplicate key refuses the whole write: the cursor stays where it was.
    let err = store
        .record_relay(
            "t",
            forge(2),
            vec![relay("relay/2", 2), relay("relay/1", 1)],
            t.metadata.resource_version,
            FieldManager::ForgeController,
            &lease,
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::AlreadyExists(_)), "{err}");
    let st = inbox_status(&store.get("Task", "t").known().unwrap().clone().unwrap());
    assert_eq!(st.forge.as_ref().unwrap().changes["cr-1"].cursor, 1);
    assert_eq!(st.inbox.len(), 1);
    // A stale version is a conflict.
    assert!(matches!(
        store.record_relay(
            "t",
            forge(2),
            vec![relay("relay/2", 2)],
            t.metadata.resource_version - 1,
            FieldManager::ForgeController,
            &lease
        ),
        Err(StoreError::Conflict { .. })
    ));
    // The cursor alone, with nothing to append, is a write too.
    let t = store
        .record_relay(
            "t",
            forge(2),
            Vec::new(),
            t.metadata.resource_version,
            FieldManager::ForgeController,
            &lease,
        )
        .unwrap();
    assert_eq!(inbox_status(&t).forge.unwrap().changes["cr-1"].cursor, 2);

    // status.forge is the Forge controller's and nobody else's; declared
    // is the worker's.
    let mut other = t.status.clone();
    other["forge"]["changes"]["cr-1"]["cursor"] = json!(9);
    for (by, lease) in [
        (FieldManager::TaskController, Some(&lease)),
        (FieldManager::Worker, None),
    ] {
        let err = store
            .update_status(
                "Task",
                "t",
                other.clone(),
                t.metadata.resource_version,
                by.clone(),
                lease,
            )
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Forbidden(ref m) if m.contains("status.forge")),
            "{by:?}: {err}"
        );
    }
    let mut declared = t.status.clone();
    declared["declared"] = json!([{"kind": "ChangeRequest", "url": "u", "name": "cr-1"}]);
    let err = store
        .update_status(
            "Task",
            "t",
            declared.clone(),
            t.metadata.resource_version,
            FieldManager::TaskController,
            Some(&lease),
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Forbidden(ref m) if m.contains("status.declared")),
        "{err}"
    );
    let t = store
        .update_status(
            "Task",
            "t",
            declared,
            t.metadata.resource_version,
            FieldManager::Worker,
            None,
        )
        .unwrap();
    let mut bad = t.status.clone();
    bad["declared"] = json!([{"url": "no kind"}]);
    let err = store
        .update_status(
            "Task",
            "t",
            bad,
            t.metadata.resource_version,
            FieldManager::Worker,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Invalid(ref m) if m.contains("kind")),
        "{err}"
    );
    // The Forge controller may also append alone, under the lease.
    let t = store
        .append_inbox(
            "t",
            relay("relay/3", 3),
            t.metadata.resource_version,
            FieldManager::ForgeController,
            Some(&lease),
        )
        .unwrap();
    assert_eq!(inbox_status(&t).inbox.len(), 2);
    assert!(matches!(
        store.append_inbox(
            "t",
            relay("relay/4", 4),
            t.metadata.resource_version,
            FieldManager::ForgeController,
            None
        ),
        Err(StoreError::LeaseLost(_))
    ));
}

fn plugin_registration() -> fleet_api::plugin::Registration {
    serde_json::from_value(json!({"manifest":{"name":"issues","protocol":1,"kinds":[{"kind":"Issue","pollMs":1000,"finalizer":true,"immutableSpec":true,"spec":{"type":"object","required":["number"],"properties":{"number":{"type":"integer"}},"additionalProperties":false},"status":{"type":"object","properties":{"phase":{"type":"string"}},"additionalProperties":false}}]},"config":{"program":"fixture","cwd":"fixture"}})).unwrap()
}
fn plugin_object() -> Object {
    serde_json::from_value(json!({"apiVersion":"fleet/v1alpha1","kind":"Issue","metadata":{"name":"one"},"spec":{"number":1}})).unwrap()
}
#[test]
fn plugin_status_and_verbs_are_fenced_and_never_write_worker_results() {
    use fleet_api::plugin::{CREATOR, Mutation, Patch};
    let dir = Dir::new("plugin-rights");
    let clock = Arc::new(TestClock::default());
    let store = FileStore::open_with_clock(&dir.0, clock.clone())
        .unwrap()
        .with_plugins(&[plugin_registration()])
        .unwrap();
    let obj = store.create(plugin_object(), FieldManager::Apply).unwrap();
    let lease = store.acquire_lease("test", 100).unwrap();
    let writer = fleet_api::plugin::writer_for_store("issues");
    assert!(matches!(
        store.update_status(
            "Issue",
            "one",
            json!({"phase":"Ready"}),
            obj.metadata.resource_version,
            writer.clone(),
            None
        ),
        Err(StoreError::LeaseLost(_))
    ));
    assert!(
        store
            .update_status(
                "Issue",
                "one",
                json!({"phase":1}),
                obj.metadata.resource_version,
                writer.clone(),
                Some(&lease)
            )
            .is_err()
    );
    let updated = store
        .update_status(
            "Issue",
            "one",
            json!({"phase":"Ready","host":{}}),
            obj.metadata.resource_version,
            writer.clone(),
            Some(&lease),
        )
        .unwrap();
    assert!(
        store
            .update_spec(
                "Issue",
                "one",
                json!({"number":2}),
                None,
                updated.metadata.resource_version,
                FieldManager::Apply
            )
            .is_err()
    );
    store
        .plugin_mutate(Mutation::Create(task("child")), writer.clone(), &lease)
        .unwrap();
    let child = known(store.get("Task", "child")).unwrap();
    assert_eq!(child.metadata.labels[CREATOR], "issues");
    let bad = Patch {
        kind: "Task".into(),
        name: "child".into(),
        uid: child.metadata.uid.clone(),
        spec: json!({"brief":"overwrite"}),
    };
    assert!(
        store
            .plugin_mutate(Mutation::Patch(bad), writer.clone(), &lease)
            .is_err()
    );
    let stale = Patch {
        kind: "Task".into(),
        name: "child".into(),
        uid: "replaced".into(),
        spec: json!({"dispatch":true}),
    };
    assert!(
        store
            .plugin_mutate(Mutation::Patch(stale), writer.clone(), &lease)
            .is_err()
    );
    assert!(
        store
            .update_status(
                "Task",
                "child",
                json!({"result":{"outcome":"shipped","summary":"forged"}}),
                child.metadata.resource_version,
                writer.clone(),
                Some(&lease)
            )
            .is_err()
    );
    clock.advance(101);
    assert!(matches!(
        store.plugin_mutate(Mutation::Create(task("expired")), writer, &lease),
        Err(StoreError::LeaseLost(_))
    ));
    assert!(known(store.get("Task", "expired")).is_none());
}

// ----------------------------------------------------------------------
// The Gateway's kinds: a Conversation's spool and a Listener's beats have
// no owner and move only through their typed mutations, named by the
// account's listener; the controllers own the rest and retire their own.
// ----------------------------------------------------------------------

fn conversation(name: &str) -> Object {
    serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Conversation",
        "metadata": {"name": name},
        "spec": {"account": "fake-main", "address": "fake:C0123ABCD:1730000000000", "route": "eng"}
    }))
    .unwrap()
}

fn listener(name: &str) -> Object {
    serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Listener",
        "metadata": {"name": name},
        "spec": {"account": "fake-main"}
    }))
    .unwrap()
}

fn delivery(id: &str, message: &str) -> fleet_api::gateway::Delivery {
    serde_json::from_value(json!({
        "id": id, "verified": "connection", "receivedAt": 7,
        "event": {"type": "message", "conversation": {"platform": "fake", "chat": "C0123ABCD", "thread": "1730000000000"},
                  "id": message, "author": {"id": "U0123ABCD", "name": "Alex", "kind": "person"},
                  "at": 1_730_000_300_000u64, "text": "hi"}
    }))
    .unwrap()
}

fn listener_of(account: &str) -> FieldManager {
    FieldManager::Listener(account.into())
}

#[test]
fn a_conversation_is_created_by_apply_or_its_accounts_listener_and_its_spool_has_no_owner() {
    use fleet_api::conversation::{ConversationStatus, RECEIVED_CAP, Spooled};
    let dir = Dir::new("conversation");
    let store = FileStore::open(&dir.0).unwrap();
    assert!(store.served_kinds().iter().any(|k| k == "Conversation"));
    assert!(store.served_kinds().iter().any(|k| k == "Listener"));
    for by in [
        FieldManager::Worker,
        FieldManager::Cli,
        FieldManager::ConversationController,
        FieldManager::ListenerController,
    ] {
        assert!(
            matches!(
                store.create(conversation("conv-1"), by.clone()),
                Err(StoreError::Forbidden(_))
            ),
            "{by:?} creates nothing"
        );
    }
    assert!(matches!(
        store.create(task("t"), listener_of("fake-main")),
        Err(StoreError::Forbidden(_))
    ));
    let c = store
        .create(conversation("conv-1"), listener_of("fake-main"))
        .unwrap();
    assert!(c.metadata.finalizers.is_empty(), "no finalizer");
    let rv = c.metadata.resource_version;
    let lease = store.acquire_lease("m", 60_000).unwrap();

    // Only the listener of the account the spec names spools; a redelivery
    // is a duplicate; the spool is bounded; nothing else changes.
    assert!(matches!(
        store.receive_event("conv-1", delivery("Ev1", "m1"), rv, listener_of("other")),
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        store.receive_event("conv-1", delivery("Ev1", "m1"), rv, FieldManager::Apply),
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        store.receive_event(
            "conv-1",
            delivery("Ev1", "m1"),
            rv + 1,
            listener_of("fake-main")
        ),
        Err(StoreError::Conflict { .. })
    ));
    let (c, outcome) = store
        .receive_event(
            "conv-1",
            delivery("Ev1", "m1"),
            rv,
            listener_of("fake-main"),
        )
        .unwrap();
    assert_eq!(outcome, Spooled::Written);
    let st: ConversationStatus = serde_json::from_value(c.status.clone()).unwrap();
    assert_eq!(st.received.len(), 1);
    assert_eq!(st.revision, 0, "the verb numbers nothing");
    let (same, outcome) = store
        .receive_event(
            "conv-1",
            delivery("Ev1", "m1"),
            c.metadata.resource_version,
            listener_of("fake-main"),
        )
        .unwrap();
    assert_eq!(outcome, Spooled::Duplicate);
    assert_eq!(same.metadata.resource_version, c.metadata.resource_version);
    let mut rv = c.metadata.resource_version;
    for i in 2..=RECEIVED_CAP {
        let (c, _) = store
            .receive_event(
                "conv-1",
                delivery(&format!("Ev{i}"), &format!("m{i}")),
                rv,
                listener_of("fake-main"),
            )
            .unwrap();
        rv = c.metadata.resource_version;
    }
    let err = store
        .receive_event(
            "conv-1",
            delivery("Ev-over", "m-over"),
            rv,
            listener_of("fake-main"),
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Forbidden(ref m) if m.contains("holds 100 unread events")),
        "{err}"
    );

    // No status write may touch the spool: not the controller's, not anyone's.
    let current = known(store.get("Conversation", "conv-1")).unwrap();
    let mut drained = current.status.clone();
    drained["received"] = json!([]);
    drained["revision"] = json!(1);
    for (by, lease_arg) in [
        (FieldManager::ConversationController, Some(&lease)),
        (listener_of("fake-main"), None),
        (FieldManager::Apply, None),
    ] {
        let err = store
            .update_status(
                "Conversation",
                "conv-1",
                drained.clone(),
                current.metadata.resource_version,
                by.clone(),
                lease_arg,
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)), "{by:?}: {err}");
    }
    // The controller drains it with its status, through record_received,
    // under the lease; it may remove entries and never add or alter one.
    for by in [
        FieldManager::Apply,
        FieldManager::ChangeRequestController,
        FieldManager::ListenerController,
        listener_of("fake-main"),
    ] {
        assert!(matches!(
            store.record_received(
                "conv-1",
                drained.clone(),
                current.metadata.resource_version,
                by.clone(),
                &lease
            ),
            Err(StoreError::Forbidden(_))
        ));
    }
    let stale = fleet_api::LeaseToken {
        token: lease.token.saturating_sub(1),
        ..lease.clone()
    };
    assert!(matches!(
        store.record_received(
            "conv-1",
            drained.clone(),
            current.metadata.resource_version,
            FieldManager::ConversationController,
            &stale
        ),
        Err(StoreError::LeaseLost(_))
    ));
    let mut grown = current.status.clone();
    grown["received"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(delivery("Ev-new", "m-new")).unwrap());
    assert!(matches!(
        store.record_received(
            "conv-1",
            grown,
            current.metadata.resource_version,
            FieldManager::ConversationController,
            &lease
        ),
        Err(StoreError::Forbidden(_))
    ));
    let mut altered = current.status.clone();
    altered["received"][0]["receivedAt"] = json!(99);
    assert!(matches!(
        store.record_received(
            "conv-1",
            altered,
            current.metadata.resource_version,
            FieldManager::ConversationController,
            &lease
        ),
        Err(StoreError::Forbidden(_))
    ));
    let mut half = current.status.clone();
    half["received"].as_array_mut().unwrap().drain(..50);
    half["revision"] = json!(1);
    let c = store
        .record_received(
            "conv-1",
            half,
            current.metadata.resource_version,
            FieldManager::ConversationController,
            &lease,
        )
        .unwrap();
    let st: ConversationStatus = serde_json::from_value(c.status.clone()).unwrap();
    assert_eq!(st.received.len(), 50);
    assert_eq!(st.revision, 1);
    let c = store
        .record_received(
            "conv-1",
            drained.clone(),
            c.metadata.resource_version,
            FieldManager::ConversationController,
            &lease,
        )
        .unwrap();
    let st: ConversationStatus = serde_json::from_value(c.status.clone()).unwrap();
    assert!(st.received.is_empty());
    // The same status again is no write.
    let again = store
        .record_received(
            "conv-1",
            c.status.clone(),
            c.metadata.resource_version,
            FieldManager::ConversationController,
            &lease,
        )
        .unwrap();
    assert_eq!(again.metadata.resource_version, c.metadata.resource_version);
    // A deleting Conversation takes no event, and the spec is fixed.
    assert!(matches!(
        store.update_spec(
            "Conversation",
            "conv-1",
            json!({"account": "fake-main", "address": "fake:C0123ABCD:2", "route": "eng"}),
            None,
            c.metadata.resource_version,
            FieldManager::Apply
        ),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(
        store
            .delete("Conversation", "conv-1", FieldManager::Apply)
            .unwrap(),
        DeleteOutcome::Removed,
        "no finalizer: the record goes at once"
    );
    assert!(matches!(
        store.receive_event("conv-1", delivery("Ev9", "m9"), 1, listener_of("fake-main")),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn a_conversations_controller_deletes_its_own_fenced_and_a_bad_status_is_refused() {
    let dir = Dir::new("conversation-delete");
    let store = FileStore::open(&dir.0).unwrap();
    let c = store
        .create(conversation("conv-2"), FieldManager::Apply)
        .unwrap();
    let lease = store.acquire_lease("m", 60_000).unwrap();
    let (uid, rv) = (c.metadata.uid.clone(), c.metadata.resource_version);
    assert!(matches!(
        store.record_received(
            "conv-2",
            json!({"phase": "Sideways"}),
            rv,
            FieldManager::ConversationController,
            &lease
        ),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.update_status(
            "Conversation",
            "conv-2",
            json!({"phase": "Untied", "revision": 0, "droppedBefore": 0}),
            rv,
            FieldManager::ConversationController,
            None
        ),
        Err(StoreError::LeaseLost(_))
    ));
    for by in [
        FieldManager::Apply,
        FieldManager::ListenerController,
        listener_of("fake-main"),
    ] {
        assert!(matches!(
            store.delete_fenced("Conversation", "conv-2", &uid, rv, by, &lease),
            Err(StoreError::Forbidden(_))
        ));
    }
    assert!(matches!(
        store.delete_fenced(
            "Conversation",
            "conv-2",
            "another-uid",
            rv,
            FieldManager::ConversationController,
            &lease
        ),
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(
        store
            .delete_fenced(
                "Conversation",
                "conv-2",
                &uid,
                rv,
                FieldManager::ConversationController,
                &lease
            )
            .unwrap(),
        DeleteOutcome::Removed
    );
}

#[test]
fn a_listeners_beats_and_refusals_move_only_through_the_verbs_mutations() {
    use fleet_api::listener::{Beat, BeatState, INSTANCE_CAP, ListenerStatus};
    let dir = Dir::new("listener");
    let store = FileStore::open(&dir.0).unwrap();
    let l = store
        .create(listener("listener-fake-main"), listener_of("fake-main"))
        .unwrap();
    let rv = l.metadata.resource_version;
    let lease = store.acquire_lease("m", 60_000).unwrap();
    let beat = |at: u64| Beat {
        state: BeatState::Connected,
        digest: "sha256:x".into(),
        at,
        sent_at: at,
        last_delivery_at: None,
    };
    for by in [
        FieldManager::Apply,
        FieldManager::ListenerController,
        listener_of("other"),
    ] {
        assert!(
            matches!(
                store.record_beat("listener-fake-main", "a1", beat(1), rv, by.clone()),
                Err(StoreError::Forbidden(_))
            ),
            "{by:?}"
        );
        assert!(matches!(
            store.record_refusal("listener-fake-main", "SpoolFull", false, 1, rv, by),
            Err(StoreError::Forbidden(_))
        ));
    }
    assert!(matches!(
        store.record_beat(
            "listener-fake-main",
            " ",
            beat(1),
            rv,
            listener_of("fake-main")
        ),
        Err(StoreError::Invalid(_))
    ));
    let l = store
        .record_beat(
            "listener-fake-main",
            "a1",
            beat(1),
            rv,
            listener_of("fake-main"),
        )
        .unwrap();
    assert!(matches!(
        store.record_beat(
            "listener-fake-main",
            "a1",
            beat(2),
            rv,
            listener_of("fake-main")
        ),
        Err(StoreError::Conflict { .. })
    ));
    let l = store
        .record_refusal(
            "listener-fake-main",
            "WrongVerified",
            true,
            5,
            l.metadata.resource_version,
            listener_of("fake-main"),
        )
        .unwrap();
    let st: ListenerStatus = serde_json::from_value(l.status.clone()).unwrap();
    assert_eq!(st.beats["a1"].at, 1);
    assert_eq!(st.refused.as_ref().unwrap().counts["WrongVerified"], 1);
    assert_eq!(st.refused.as_ref().unwrap().last_fault_at, Some(5));
    assert_eq!(st.beat_revision, 2);
    assert_eq!(st.seen, 0, "the controller's fields are untouched");
    let mut rv = l.metadata.resource_version;
    for i in 2..=INSTANCE_CAP + 1 {
        let l = store
            .record_beat(
                "listener-fake-main",
                &format!("a{i}"),
                beat(i as u64),
                rv,
                listener_of("fake-main"),
            )
            .unwrap();
        rv = l.metadata.resource_version;
    }
    let current = known(store.get("Listener", "listener-fake-main")).unwrap();
    let st: ListenerStatus = serde_json::from_value(current.status.clone()).unwrap();
    assert_eq!(st.beats.len(), INSTANCE_CAP);
    assert!(
        !st.beats.contains_key("a1"),
        "the oldest instance was evicted"
    );

    // The controller writes the rest under the lease and may not touch
    // beats, refused or beatRevision; nobody writes them through a status
    // write.
    let mut judged = current.status.clone();
    judged["seen"] = json!(st.beat_revision);
    judged["conditions"] = json!([{"type": "Accepted", "status": "True", "reason": "Loaded",
                                   "lastTransitionTime": 9, "observedGeneration": 1}]);
    assert!(matches!(
        store.update_status(
            "Listener",
            "listener-fake-main",
            judged.clone(),
            current.metadata.resource_version,
            FieldManager::ListenerController,
            None
        ),
        Err(StoreError::LeaseLost(_))
    ));
    for by in [
        FieldManager::Apply,
        FieldManager::ConversationController,
        listener_of("fake-main"),
    ] {
        assert!(matches!(
            store.update_status(
                "Listener",
                "listener-fake-main",
                judged.clone(),
                current.metadata.resource_version,
                by,
                Some(&lease)
            ),
            Err(StoreError::Forbidden(_))
        ));
    }
    let l = store
        .update_status(
            "Listener",
            "listener-fake-main",
            judged,
            current.metadata.resource_version,
            FieldManager::ListenerController,
            Some(&lease),
        )
        .unwrap();
    for field in ["beats", "refused", "beatRevision"] {
        let mut touched = l.status.clone();
        touched[field] = match field {
            "beats" => json!({}),
            "refused" => Value::Null,
            _ => json!(99),
        };
        if touched[field].is_null() {
            touched.as_object_mut().unwrap().remove(field);
        }
        let err = store
            .update_status(
                "Listener",
                "listener-fake-main",
                touched,
                l.metadata.resource_version,
                FieldManager::ListenerController,
                Some(&lease),
            )
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Forbidden(ref m) if m.contains(field)),
            "{field}: {err}"
        );
    }
    assert!(matches!(
        store.update_spec(
            "Listener",
            "listener-fake-main",
            json!({"account": "other"}),
            None,
            l.metadata.resource_version,
            FieldManager::Apply
        ),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(
        store
            .delete_fenced(
                "Listener",
                "listener-fake-main",
                &l.metadata.uid,
                l.metadata.resource_version,
                FieldManager::ListenerController,
                &lease
            )
            .unwrap(),
        DeleteOutcome::Removed
    );
}

#[test]
fn the_gateway_kinds_are_reserved_against_plugin_manifests_and_validated_on_write() {
    use fleet_api::plugin::{Manifest, validate_manifest};
    for kind in ["Conversation", "conversations", "LISTENER", "Listeners"] {
        let manifest: Manifest = serde_json::from_value(json!({
            "protocol": 1, "name": "chat", "kinds": [{"kind": kind, "pollMs": 1000, "spec": {"type": "object"}, "status": null}]
        }))
        .unwrap();
        assert!(validate_manifest(&manifest, "chat").is_err(), "{kind}");
    }
    let dir = Dir::new("gateway-validate");
    let store = FileStore::open(&dir.0).unwrap();
    for (what, spec) in [
        (
            "no route",
            json!({"account": "fake-main", "address": "fake:C1"}),
        ),
        (
            "bad address",
            json!({"account": "fake-main", "address": "fake", "route": "eng"}),
        ),
        (
            "bad account name",
            json!({"account": "Fake Main", "address": "fake:C1", "route": "eng"}),
        ),
        (
            "zero poll",
            json!({"account": "fake-main", "address": "fake:C1", "route": "eng", "pollMs": 0}),
        ),
        (
            "unknown field",
            json!({"account": "fake-main", "address": "fake:C1", "route": "eng", "retainMs": 1}),
        ),
    ] {
        let mut c = conversation("conv-x");
        c.spec = spec;
        assert!(
            matches!(
                store.create(c, FieldManager::Apply),
                Err(StoreError::Invalid(_))
            ),
            "{what}"
        );
    }
    let mut l = listener("listener-x");
    l.spec = json!({"account": "Not A Name"});
    assert!(matches!(
        store.create(l, FieldManager::Apply),
        Err(StoreError::Invalid(_))
    ));
    let mut owned = conversation("conv-y");
    owned.metadata.owner_references = vec![OwnerReference {
        kind: "Task".into(),
        name: "t".into(),
        uid: "u".into(),
        controller: true,
    }];
    assert!(matches!(
        store.create(owned, FieldManager::Apply),
        Err(StoreError::Invalid(_))
    ));
    // A record whose status its readers would refuse is Unknown, never a guess.
    let c = store
        .create(conversation("conv-z"), FieldManager::Apply)
        .unwrap();
    let path = dir
        .0
        .join("objects")
        .join("Conversation")
        .join("conv-z.json");
    let mut raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    raw["status"] = json!({"phase": "Untied", "messages": "none"});
    fs::write(&path, raw.to_string()).unwrap();
    assert!(store.get("Conversation", "conv-z").is_unknown());
    assert!(matches!(
        store.receive_event(
            "conv-z",
            delivery("Ev1", "m1"),
            c.metadata.resource_version,
            listener_of("fake-main")
        ),
        Err(StoreError::Unknown(_))
    ));
}

/// The Task a thread starts: created by the Conversation's controller alone,
/// under the lease, as the one create that may carry the conversation
/// labels; nobody sets or changes those labels otherwise.
#[test]
fn a_conversation_task_is_created_fenced_by_its_controller_and_its_labels_are_fixed() {
    use fleet_api::conversation::{CONVERSATION_LABEL, CONVERSATION_UID_LABEL};
    let dir = Dir::new("conversation-task");
    let store = FileStore::open(&dir.0).unwrap();
    let mut labelled = task("conv-1");
    labelled
        .metadata
        .labels
        .insert(CONVERSATION_LABEL.into(), "conv".into());
    labelled
        .metadata
        .labels
        .insert(CONVERSATION_UID_LABEL.into(), "c7".into());
    let by = FieldManager::ConversationController;
    // Unfenced, a controller creates nothing; apply and a listener never
    // set the labels.
    assert!(matches!(
        store.create(labelled.clone(), by.clone()),
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        store.create(labelled.clone(), FieldManager::Apply),
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        store.create(labelled.clone(), listener_of("fake-main")),
        Err(StoreError::Forbidden(_))
    ));
    let lease = store.acquire_lease("m", 60_000).unwrap();
    for other in [
        FieldManager::TaskController,
        FieldManager::ForgeController,
        FieldManager::ListenerController,
    ] {
        assert!(
            matches!(
                store.create_fenced(labelled.clone(), other.clone(), &lease),
                Err(StoreError::Forbidden(_))
            ),
            "{other:?}"
        );
    }
    assert!(
        store
            .create_fenced(task("not-a-conversation-kind"), by.clone(), &lease)
            .is_ok()
    );
    let stale = fleet_api::LeaseToken {
        holder: "m".into(),
        token: lease.token + 1,
        expires_at: lease.expires_at,
    };
    assert!(matches!(
        store.create_fenced(labelled.clone(), by.clone(), &stale),
        Err(StoreError::LeaseLost(_))
    ));
    let created = store
        .create_fenced(labelled.clone(), by.clone(), &lease)
        .unwrap();
    assert_eq!(created.metadata.labels, labelled.metadata.labels);
    assert_eq!(
        created.metadata.finalizers,
        vec!["fleet.dev/release".to_string()]
    );
    assert!(matches!(
        store.create_fenced(labelled.clone(), by.clone(), &lease),
        Err(StoreError::AlreadyExists(_))
    ));
    // Apply may change the spec's mutable fields but not those labels.
    let rv = created.metadata.resource_version;
    let mut changed = labelled.metadata.labels.clone();
    changed.insert(CONVERSATION_UID_LABEL.into(), "c8".into());
    assert!(matches!(
        store.update_spec(
            "Task",
            "conv-1",
            created.spec.clone(),
            Some(changed),
            rv,
            FieldManager::Apply
        ),
        Err(StoreError::Forbidden(_))
    ));
    let mut dropped = labelled.metadata.labels.clone();
    dropped.remove(CONVERSATION_LABEL);
    assert!(matches!(
        store.update_spec(
            "Task",
            "conv-1",
            created.spec.clone(),
            Some(dropped),
            rv,
            FieldManager::Apply
        ),
        Err(StoreError::Forbidden(_))
    ));
    let mut same = labelled.metadata.labels.clone();
    same.insert("topic".into(), "x".into());
    let kept = store
        .update_spec(
            "Task",
            "conv-1",
            created.spec.clone(),
            Some(same.clone()),
            rv,
            FieldManager::Apply,
        )
        .unwrap();
    assert_eq!(kept.metadata.labels, same);
    let mut plain = task("plain");
    plain.metadata.labels.insert("topic".into(), "x".into());
    let plain = store.create(plain, FieldManager::Apply).unwrap();
    let mut adding = plain.metadata.labels.clone();
    adding.insert(CONVERSATION_LABEL.into(), "conv".into());
    assert!(matches!(
        store.update_spec(
            "Task",
            "plain",
            plain.spec.clone(),
            Some(adding),
            plain.metadata.resource_version,
            FieldManager::Apply
        ),
        Err(StoreError::Forbidden(_))
    ));
}

// --- Answers: an append records what it answers, in the same write. ------

/// A Task briefed as its controller would leave it, with two declared
/// Questions as the worker would leave them.
fn with_questions(store: &FileStore, name: &str, lease: &fleet_api::LeaseToken) -> Object {
    let t = store.create(task(name), FieldManager::Apply).unwrap();
    let t = store
        .update_status(
            "Task",
            name,
            status("Briefed"),
            t.metadata.resource_version,
            FieldManager::TaskController,
            Some(lease),
        )
        .unwrap();
    let mut declared = t.status.clone();
    declared["declared"] = json!([
        {"kind": "Question", "id": "q1", "text": "Which export?"},
        {"kind": "Question", "id": "q2", "text": "Restart it?"},
        {"kind": "Comment", "id": "c1", "text": "Looking."},
    ]);
    store
        .update_status(
            "Task",
            name,
            declared,
            t.metadata.resource_version,
            FieldManager::Worker,
            None,
        )
        .unwrap()
}

#[test]
fn an_append_records_its_answers_first_wins_and_refuses_an_undeclared_question() {
    let dir = Dir::new("answers");
    let store = FileStore::open(&dir.0).unwrap();
    let lease = store.acquire_lease("m", 10_000).unwrap();
    let t = with_questions(&store, "t", &lease);
    let answering = |key: &str, ids: &[&str]| InboxEntry {
        answers: ids.iter().map(|s| s.to_string()).collect(),
        ..entry(key)
    };
    // An id that names no declared Question, or a Comment's id, refuses
    // the append whole.
    for bad in ["q9", "c1", ""] {
        let err = store
            .append_inbox(
                "t",
                answering("k1", &[bad]),
                t.metadata.resource_version,
                FieldManager::Cli,
                None,
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)), "{bad:?}: {err}");
        assert!(
            inbox_status(&known(store.get("Task", "t")).unwrap())
                .inbox
                .is_empty()
        );
    }
    let t = store
        .append_inbox(
            "t",
            answering("k1", &["q1"]),
            t.metadata.resource_version,
            FieldManager::Cli,
            None,
        )
        .unwrap();
    let st = inbox_status(&t);
    assert_eq!(st.inbox.len(), 1);
    assert_eq!(st.inbox[0].answers, vec!["q1".to_string()]);
    let first = st.answered["q1"].clone();
    assert_eq!(
        (first.key.as_str(), first.from.as_str(), first.at),
        ("k1", "cli/tester", 1)
    );
    assert!(!st.answered.contains_key("q2"));
    // The first answer wins; the later entry still lands.
    let t = store
        .append_inbox(
            "t",
            answering("k2", &["q1", "q2"]),
            t.metadata.resource_version,
            FieldManager::Cli,
            None,
        )
        .unwrap();
    let st = inbox_status(&t);
    assert_eq!(st.inbox.len(), 2);
    assert_eq!(st.answered["q1"], first);
    assert_eq!(st.answered["q2"].key, "k2");
    // Nobody owns status.answered: no status write may change it, and the
    // controller's delivery write passes it back unchanged.
    let mut cleared = t.status.clone();
    cleared["answered"] = json!({});
    for (by, lease) in [
        (FieldManager::TaskController, Some(&lease)),
        (FieldManager::Worker, None),
    ] {
        let err = store
            .update_status(
                "Task",
                "t",
                cleared.clone(),
                t.metadata.resource_version,
                by.clone(),
                lease,
            )
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Forbidden(ref m) if m.contains("status.answered")),
            "{by:?}: {err}"
        );
    }
    let t = store
        .record_delivery(
            "t",
            t.status.clone(),
            receipt("k1"),
            t.metadata.resource_version,
            FieldManager::TaskController,
            &lease,
        )
        .unwrap();
    assert_eq!(inbox_status(&t).answered["q1"], first);
}

#[test]
fn the_gateway_relay_lands_its_status_its_entries_and_their_answers_together_or_not_at_all() {
    use fleet_api::talk::{ConversationTie, GatewayStatus, TieState};
    let dir = Dir::new("gateway-relay");
    let store = FileStore::open(&dir.0).unwrap();
    let lease = store.acquire_lease("m", 10_000).unwrap();
    let t = with_questions(&store, "t", &lease);
    let gateway = |cursor: u64| {
        serde_json::to_value(GatewayStatus {
            conversation: Some(ConversationTie {
                name: "conv-1".into(),
                uid: "c1".into(),
                state: TieState::Tied,
            }),
            cursor,
            seen: cursor,
            ..GatewayStatus::default()
        })
        .unwrap()
    };
    let relay = |key: &str, revision: u64, ids: &[&str]| InboxEntry {
        from: "gateway/conv-1".into(),
        about: Some(InboxAbout {
            uid: "c1".into(),
            revision,
            head: None,
        }),
        answers: ids.iter().map(|s| s.to_string()).collect(),
        ..entry(key)
    };
    // Only the Gateway controller, under the lease, naming its source.
    for by in [
        FieldManager::Apply,
        FieldManager::Cli,
        FieldManager::TaskController,
        FieldManager::ConversationController,
    ] {
        let err = store
            .record_relay(
                "t",
                gateway(1),
                vec![relay("gw/1", 1, &[])],
                t.metadata.resource_version,
                by.clone(),
                &lease,
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)), "{by:?}: {err}");
    }
    let mut mislabelled = relay("gw/1", 1, &[]);
    mislabelled.from = "forge/conv-1".into();
    assert!(matches!(
        store.record_relay(
            "t",
            gateway(1),
            vec![mislabelled],
            t.metadata.resource_version,
            FieldManager::GatewayController,
            &lease
        ),
        Err(StoreError::Invalid(_))
    ));
    // An undeclared answer refuses the whole write: no cursor, no entry.
    let err = store
        .record_relay(
            "t",
            gateway(1),
            vec![relay("gw/1", 1, &["q9"])],
            t.metadata.resource_version,
            FieldManager::GatewayController,
            &lease,
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err}");
    let st = inbox_status(&known(store.get("Task", "t")).unwrap());
    assert!(st.gateway.is_none() && st.inbox.is_empty());
    let t = store
        .record_relay(
            "t",
            gateway(1),
            vec![relay("gw/1", 1, &["q1"])],
            t.metadata.resource_version,
            FieldManager::GatewayController,
            &lease,
        )
        .unwrap();
    let st = inbox_status(&t);
    assert_eq!(st.gateway.as_ref().unwrap().cursor, 1);
    assert_eq!(st.inbox.len(), 1);
    assert_eq!(st.answered["q1"].from, "gateway/conv-1");
    assert!(
        st.forge.is_none(),
        "the Forge controller's field is untouched"
    );
    // A duplicate key refuses the whole write: the cursor stays.
    let err = store
        .record_relay(
            "t",
            gateway(2),
            vec![relay("gw/2", 2, &[]), relay("gw/1", 1, &[])],
            t.metadata.resource_version,
            FieldManager::GatewayController,
            &lease,
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::AlreadyExists(_)), "{err}");
    assert_eq!(
        inbox_status(&known(store.get("Task", "t")).unwrap())
            .gateway
            .unwrap()
            .cursor,
        1
    );
    // status.gateway is the Gateway controller's and nobody else's: not
    // the Task controller's, not the worker's, and not the Forge
    // controller's relay write, which leaves it as it was.
    let mut other = t.status.clone();
    other["gateway"]["cursor"] = json!(9);
    for (by, lease) in [
        (FieldManager::TaskController, Some(&lease)),
        (FieldManager::Worker, None),
    ] {
        let err = store
            .update_status(
                "Task",
                "t",
                other.clone(),
                t.metadata.resource_version,
                by.clone(),
                lease,
            )
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Forbidden(ref m) if m.contains("status.gateway")),
            "{by:?}: {err}"
        );
    }
    let t = store
        .record_relay(
            "t",
            json!({"changes": {"cr-1": {"uid": "x", "cursor": 3}}}),
            Vec::new(),
            t.metadata.resource_version,
            FieldManager::ForgeController,
            &lease,
        )
        .unwrap();
    let st = inbox_status(&t);
    assert_eq!(st.forge.as_ref().unwrap().changes["cr-1"].cursor, 3);
    assert_eq!(st.gateway.as_ref().unwrap().cursor, 1);
    // An empty status is stored as none.
    let t = store
        .record_relay(
            "t",
            serde_json::to_value(GatewayStatus::default()).unwrap(),
            Vec::new(),
            t.metadata.resource_version,
            FieldManager::GatewayController,
            &lease,
        )
        .unwrap();
    assert!(t.status.get("gateway").is_none(), "{:#}", t.status);
}

#[test]
fn a_plugins_append_records_its_answers_under_the_same_checks() {
    use fleet_api::plugin::{Append, Mutation};
    let dir = Dir::new("plugin-answers");
    let store = FileStore::open(&dir.0)
        .unwrap()
        .with_plugins(&[plugin_registration()])
        .unwrap();
    let lease = store.acquire_lease("m", 10_000).unwrap();
    let writer = fleet_api::plugin::writer_for_store("issues");
    store
        .plugin_mutate(Mutation::Create(task("own")), writer.clone(), &lease)
        .unwrap();
    let t = known(store.get("Task", "own")).unwrap();
    let t = store
        .update_status(
            "Task",
            "own",
            status("Briefed"),
            t.metadata.resource_version,
            FieldManager::TaskController,
            Some(&lease),
        )
        .unwrap();
    let mut declared = t.status.clone();
    declared["declared"] = json!([{"kind": "Question", "id": "q1", "text": "?"}]);
    let t = store
        .update_status(
            "Task",
            "own",
            declared,
            t.metadata.resource_version,
            FieldManager::Worker,
            None,
        )
        .unwrap();
    let append = |key: &str, ids: &[&str]| {
        Mutation::Append(Append {
            task: "own".into(),
            uid: t.metadata.uid.clone(),
            key: key.into(),
            text: "the nightly one".into(),
            answers: ids.iter().map(|s| s.to_string()).collect(),
        })
    };
    assert!(matches!(
        store.plugin_mutate(append("a1", &["q9"]), writer.clone(), &lease),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(
        store
            .plugin_mutate(append("a1", &["q1"]), writer.clone(), &lease)
            .unwrap(),
        json!("applied")
    );
    let st = inbox_status(&known(store.get("Task", "own")).unwrap());
    assert_eq!(st.answered["q1"].from, "plugin/issues");
    assert_eq!(
        st.answered["q1"].key,
        format!("task/own/{}/plugin/issues/a1", t.metadata.uid)
    );
    assert_eq!(st.inbox[0].answers, vec!["q1".to_string()]);
}

// --- The parent labels: authority, checked when the child is created. ----

fn labelled(name: &str, parent: &str, uid: &str) -> Object {
    let mut t = task(name);
    t.metadata
        .labels
        .insert("gateway.fleet.dev/parent".into(), parent.into());
    t.metadata
        .labels
        .insert("gateway.fleet.dev/parent-uid".into(), uid.into());
    t
}

#[test]
fn the_parent_labels_are_accepted_only_at_creation_from_a_writer_with_scope_over_the_parent() {
    use fleet_api::plugin::Mutation;
    let dir = Dir::new("parent-labels");
    let any: fleet_api::plugin::Registration = serde_json::from_value(json!({
        "manifest":{"name":"broad","protocol":1,"kinds":[{"kind":"Ticket","pollMs":1000,"spec":{"type":"object"},"status":null}]},
        "config":{"program":"fixture","cwd":"fixture","tasks":"any"}
    }))
    .unwrap();
    let store = FileStore::open(&dir.0)
        .unwrap()
        .with_plugins(&[plugin_registration(), any])
        .unwrap();
    let lease = store.acquire_lease("m", 10_000).unwrap();
    let p = store.create(task("p"), FieldManager::Apply).unwrap();
    let uid = p.metadata.uid.clone();
    // Both labels or neither; the parent must exist with that uid.
    let mut half = task("c");
    half.metadata
        .labels
        .insert("gateway.fleet.dev/parent".into(), "p".into());
    let err = store.create(half, FieldManager::Apply).unwrap_err();
    assert!(
        matches!(err, StoreError::Invalid(ref m) if m.contains("gateway.fleet.dev/parent-uid")),
        "{err}"
    );
    let err = store
        .create(labelled("c", "nobody", &uid), FieldManager::Apply)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Invalid(ref m) if m.contains("does not exist")),
        "{err}"
    );
    let err = store
        .create(labelled("c", "p", "u0-wrong"), FieldManager::Apply)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Invalid(ref m) if m.contains("uid")),
        "{err}"
    );
    // A parent the store cannot read is Unknown: retried, never refused.
    let path = dir.0.join("objects").join("Task").join("p.json");
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    assert!(matches!(
        store.create(labelled("c", "p", &uid), FieldManager::Apply),
        Err(StoreError::Unknown(_))
    ));
    fs::write(&path, &bytes).unwrap();
    assert!(store.get("Task", "c").known().unwrap().is_none());
    // A person may name any parent.
    let c = store
        .create(labelled("c", "p", &uid), FieldManager::Apply)
        .unwrap();
    // Once set, the labels never change: not removed, not pointed elsewhere,
    // while other labels may.
    let q = store.create(task("q"), FieldManager::Apply).unwrap();
    let mut moved = c.metadata.labels.clone();
    moved.insert("gateway.fleet.dev/parent".into(), "q".into());
    moved.insert(
        "gateway.fleet.dev/parent-uid".into(),
        q.metadata.uid.clone(),
    );
    for labels in [BTreeMap::new(), moved] {
        let err = store
            .update_spec(
                "Task",
                "c",
                c.spec.clone(),
                Some(labels),
                c.metadata.resource_version,
                FieldManager::Apply,
            )
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Forbidden(ref m) if m.contains("gateway.fleet.dev/parent")),
            "{err}"
        );
    }
    let mut more = c.metadata.labels.clone();
    more.insert("topic".into(), "x".into());
    let c = store
        .update_spec(
            "Task",
            "c",
            c.spec.clone(),
            Some(more),
            c.metadata.resource_version,
            FieldManager::Apply,
        )
        .unwrap();
    assert_eq!(c.metadata.labels["gateway.fleet.dev/parent"], "p");
    assert_eq!(c.metadata.labels["topic"], "x");
    // A plugin over its own Tasks may name only a parent it created; one
    // over any Task may name any.
    let own = fleet_api::plugin::writer_for_store("issues");
    let err = store
        .plugin_mutate(
            Mutation::Create(labelled("pc", "p", &uid)),
            own.clone(),
            &lease,
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Forbidden(ref m) if m.contains("may not name Task/p as a parent")),
        "{err}"
    );
    assert!(store.get("Task", "pc").known().unwrap().is_none());
    store
        .plugin_mutate(Mutation::Create(task("own-parent")), own.clone(), &lease)
        .unwrap();
    let own_uid = known(store.get("Task", "own-parent")).unwrap().metadata.uid;
    assert_eq!(
        store
            .plugin_mutate(
                Mutation::Create(labelled("pc", "own-parent", &own_uid)),
                own,
                &lease
            )
            .unwrap(),
        json!("created")
    );
    let broad = fleet_api::plugin::writer_for_store("broad");
    assert_eq!(
        store
            .plugin_mutate(Mutation::Create(labelled("bc", "p", &uid)), broad, &lease)
            .unwrap(),
        json!("created")
    );
    // A parent being deleted takes no child: nothing would read its inbox.
    assert_eq!(
        store.delete("Task", "p", FieldManager::Apply).unwrap(),
        DeleteOutcome::Marked
    );
    let err = store
        .create(labelled("d", "p", &uid), FieldManager::Apply)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Forbidden(ref m) if m.contains("being deleted")),
        "{err}"
    );
}

// --- Amendments: one append-only mutation over spec and inbox. -------------

fn amendment(generation: u64) -> fleet_api::task::Amendment {
    fleet_api::task::Amendment {
        text: format!("amendment {generation}"),
        reason: None,
        outputs: Default::default(),
        from: "cli/tester".into(),
        at: 1,
        generation,
    }
}

fn amends(t: &Object, generation: u64) -> InboxEntry {
    InboxEntry {
        amends: Some(generation),
        ..entry(&fleet_api::inbox::amend_key(
            "t",
            &t.metadata.uid,
            generation,
        ))
    }
}

#[test]
fn an_amendment_lands_in_spec_and_inbox_together_and_only_through_amend() {
    let dir = Dir::new("amend");
    let store = FileStore::open(&dir.0).unwrap();
    let t = store.create(task("t"), FieldManager::Apply).unwrap();
    let rv = t.metadata.resource_version;
    // A new Task carries none.
    let mut born = task("u");
    born.spec["amendments"] = serde_json::to_value(vec![amendment(2)]).unwrap();
    assert!(matches!(
        store.create(born, FieldManager::Apply),
        Err(StoreError::Invalid(_))
    ));
    // No other caller queues an amendment, through any append.
    assert!(matches!(
        store.append_inbox("t", amends(&t, 2), rv, FieldManager::Cli, None),
        Err(StoreError::Forbidden(_))
    ));
    for by in [
        FieldManager::Apply,
        FieldManager::Worker,
        FieldManager::TaskController,
    ] {
        let err = store
            .amend_task("t", amendment(2), amends(&t, 2), rv, by.clone())
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)), "{by:?}: {err}");
    }
    // The generation, the key and the entry's `amends` name one generation.
    for (a, e) in [(amendment(3), amends(&t, 2)), (amendment(2), amends(&t, 3))] {
        assert!(matches!(
            store.amend_task("t", a, e, rv, FieldManager::Cli),
            Err(StoreError::Invalid(_))
        ));
    }
    assert!(matches!(
        store.amend_task("t", amendment(2), amends(&t, 2), rv + 1, FieldManager::Cli),
        Err(StoreError::Conflict { .. })
    ));
    let done = store
        .amend_task("t", amendment(2), amends(&t, 2), rv, FieldManager::Cli)
        .unwrap();
    assert_eq!(done.metadata.generation, 2);
    assert_eq!(done.spec["amendments"][0]["text"], "amendment 2");
    assert!(
        done.spec.get("host").is_none(),
        "the spec is never re-encoded"
    );
    assert_eq!(inbox_status(&done).inbox[0].amends, Some(2));
    // An update leaves them as they are, or is refused.
    let mut dropped = done.spec.clone();
    dropped.as_object_mut().unwrap().remove("amendments");
    assert!(matches!(
        store.update_spec(
            "Task",
            "t",
            dropped,
            None,
            done.metadata.resource_version,
            FieldManager::Apply
        ),
        Err(StoreError::Invalid(_))
    ));
    // A Stalled Task is refused.
    let lease = store.acquire_lease("m", 10_000).unwrap();
    let stalled = store
        .update_status(
            "Task",
            "t",
            json!({"phase": "Stalled", "observedGeneration": 2, "inbox": done.status["inbox"]}),
            done.metadata.resource_version,
            FieldManager::TaskController,
            Some(&lease),
        )
        .unwrap();
    assert!(matches!(
        store.amend_task(
            "t",
            amendment(3),
            amends(&t, 3),
            stalled.metadata.resource_version,
            FieldManager::Cli
        ),
        Err(StoreError::Forbidden(_))
    ));
}

#[test]
fn the_gateway_controller_creates_an_opened_threads_conversation_and_spools_only_fleets_post() {
    let dir = Dir::new("gateway-opened");
    let store = FileStore::open(&dir.0).unwrap();
    let lease = store.acquire_lease("m", 60_000).unwrap();
    let by = FieldManager::GatewayController;
    assert!(
        matches!(
            store.create(conversation("conv-1"), by.clone()),
            Err(StoreError::Forbidden(_))
        ),
        "a controller creates under the lease only"
    );
    assert!(matches!(
        store.create_fenced(task("t"), by.clone(), &lease),
        Err(StoreError::Forbidden(_))
    ));
    let c = store
        .create_fenced(conversation("conv-1"), by.clone(), &lease)
        .unwrap();
    let rv = c.metadata.resource_version;
    // A person's message is the listener's to spool, never the controller's.
    assert!(matches!(
        store.receive_event("conv-1", delivery("Ev1", "m1"), rv, by.clone()),
        Err(StoreError::Forbidden(_))
    ));
    let mut own = delivery("fleet/task/t/u1/gateway/question/q1", "m0");
    own.verified = fleet_api::gateway::Verified::Fleet;
    let (_, spooled) = store
        .receive_event("conv-1", own.clone(), rv, by.clone())
        .unwrap();
    assert_eq!(spooled, fleet_api::conversation::Spooled::Written);
    let rv = store
        .get("Conversation", "conv-1")
        .known()
        .cloned()
        .flatten()
        .unwrap()
        .metadata
        .resource_version;
    let (_, again) = store.receive_event("conv-1", own, rv, by).unwrap();
    assert_eq!(again, fleet_api::conversation::Spooled::Duplicate);
}

#[test]
fn only_a_concluded_conversation_task_is_retired_and_only_by_its_conversation() {
    use fleet_api::conversation::{CONVERSATION_LABEL, CONVERSATION_UID_LABEL};
    let dir = Dir::new("retire-task");
    let store = FileStore::open(&dir.0).unwrap();
    let lease = store.acquire_lease("m", 60_000).unwrap();
    let by = FieldManager::ConversationController;
    let mut labelled = task("conv-1");
    labelled
        .metadata
        .labels
        .insert(CONVERSATION_LABEL.into(), "conv".into());
    labelled
        .metadata
        .labels
        .insert(CONVERSATION_UID_LABEL.into(), "c7".into());
    let created = store.create_fenced(labelled, by.clone(), &lease).unwrap();
    let uid = created.metadata.uid.clone();
    let retire = |uid: &str, conv: &str, by: FieldManager| {
        store.retire_conversation_task("conv-1", uid, conv, by, &lease)
    };
    // Running: never retired.
    assert!(matches!(
        retire(&uid, "c7", by.clone()),
        Err(StoreError::Forbidden(ref m)) if m.contains("not Concluded")
    ));
    store
        .update_status(
            "Task",
            "conv-1",
            status("Concluded"),
            created.metadata.resource_version,
            FieldManager::TaskController,
            Some(&lease),
        )
        .unwrap();
    // Another writer, another thread, another Task under the name: refused.
    assert!(matches!(
        retire(&uid, "c7", FieldManager::TaskController),
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        retire(&uid, "c8", by.clone()),
        Err(StoreError::Forbidden(ref m)) if m.contains("not created for this thread")
    ));
    assert!(matches!(
        retire("u-other", "c7", by.clone()),
        Err(StoreError::Conflict { .. })
    ));
    // Concluded and this thread's: marked, its finalizer releasing it; a
    // second call is the same answer.
    assert_eq!(
        retire(&uid, "c7", by.clone()).unwrap(),
        DeleteOutcome::Marked
    );
    assert!(known(store.get("Task", "conv-1")).unwrap().is_deleting());
    assert_eq!(
        retire(&uid, "c7", by.clone()).unwrap(),
        DeleteOutcome::Marked
    );
    assert!(matches!(
        store.retire_conversation_task("absent", &uid, "c7", by, &lease),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn a_plugins_amend_is_fenced_on_its_generation_and_keeps_fleet_amends_checks() {
    use fleet_api::plugin::{Amend, Append, Mutation};
    let dir = Dir::new("plugin-amend");
    let store = FileStore::open(&dir.0)
        .unwrap()
        .with_plugins(&[plugin_registration()])
        .unwrap();
    let lease = store.acquire_lease("m", 10_000).unwrap();
    let writer = fleet_api::plugin::writer_for_store("issues");
    let mut own = task("own");
    own.spec["outputs"] = json!({"prUrl": {"type": "string"}});
    store
        .plugin_mutate(Mutation::Create(own), writer.clone(), &lease)
        .unwrap();
    let uid = known(store.get("Task", "own")).unwrap().metadata.uid;
    let amend = |task: &str, uid: &str, generation: u64, outputs: Value| {
        Mutation::Amend(Amend {
            task: task.into(),
            uid: uid.into(),
            text: "Review the new push.".into(),
            reason: Some(" head moved ".into()),
            outputs: serde_json::from_value(outputs).unwrap(),
            if_generation: generation,
        })
    };
    // An output name already declared is refused, and nothing is written.
    assert!(matches!(
        store.plugin_mutate(amend("own", &uid, 1, json!({"prUrl": {"type": "string"}})), writer.clone(), &lease),
        Err(StoreError::Invalid(why)) if why.contains("already declared")
    ));
    assert_eq!(
        store
            .plugin_mutate(
                amend("own", &uid, 1, json!({"verdict": {"type": "string"}})),
                writer.clone(),
                &lease
            )
            .unwrap(),
        json!("applied")
    );
    let t = known(store.get("Task", "own")).unwrap();
    assert_eq!(t.metadata.generation, 2);
    let amendments = t.spec["amendments"].as_array().unwrap();
    assert_eq!(amendments.len(), 1);
    assert_eq!(amendments[0]["from"], "plugin/issues");
    assert_eq!(amendments[0]["reason"], "head moved");
    assert_eq!(amendments[0]["generation"], 2);
    let st = inbox_status(&t);
    assert_eq!(st.inbox.len(), 1);
    assert_eq!(st.inbox[0].key, format!("task/own/{uid}/amend/2"));
    assert_eq!(st.inbox[0].from, "plugin/issues");
    assert_eq!(st.inbox[0].amends, Some(2));

    // The same verb again is refused on its precondition: one amendment.
    assert!(matches!(
        store.plugin_mutate(amend("own", &uid, 1, json!({})), writer.clone(), &lease),
        Err(StoreError::Precondition(why)) if why.contains("generation 2, not 1")
    ));
    assert_eq!(
        known(store.get("Task", "own")).unwrap().spec["amendments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // A replaced Task is refused on its uid.
    assert!(matches!(
        store.plugin_mutate(
            amend("own", "replaced", 2, json!({})),
            writer.clone(),
            &lease
        ),
        Err(StoreError::Forbidden(_))
    ));

    // A full inbox refuses it.
    for n in 0..=MAX_INBOX {
        let _ = store.plugin_mutate(
            Mutation::Append(Append {
                task: "own".into(),
                uid: uid.clone(),
                key: format!("m{n}"),
                text: "more".into(),
                answers: Vec::new(),
            }),
            writer.clone(),
            &lease,
        );
    }
    assert!(matches!(
        store.plugin_mutate(amend("own", &uid, 2, json!({})), writer.clone(), &lease),
        Err(StoreError::Forbidden(why)) if why.contains("still to deliver")
    ));

    // A Task this plugin did not create is foreign under `tasks = "own"`.
    let foreign = store.create(task("foreign"), FieldManager::Apply).unwrap();
    assert!(matches!(
        store.plugin_mutate(amend("foreign", &foreign.metadata.uid, 1, json!({})), writer.clone(), &lease),
        Err(StoreError::Forbidden(why)) if why.contains("foreign Task")
    ));

    // A Stalled Task refuses it; so does a deleting one.
    store
        .plugin_mutate(Mutation::Create(task("stalled")), writer.clone(), &lease)
        .unwrap();
    let s = known(store.get("Task", "stalled")).unwrap();
    store
        .update_status(
            "Task",
            "stalled",
            status("Stalled"),
            s.metadata.resource_version,
            FieldManager::TaskController,
            Some(&lease),
        )
        .unwrap();
    assert!(matches!(
        store.plugin_mutate(amend("stalled", &s.metadata.uid, 1, json!({})), writer.clone(), &lease),
        Err(StoreError::Forbidden(why)) if why.contains("Stalled")
    ));
    store
        .plugin_mutate(Mutation::Create(task("going")), writer.clone(), &lease)
        .unwrap();
    let g = known(store.get("Task", "going")).unwrap();
    store.delete("Task", "going", FieldManager::Apply).unwrap();
    assert!(known(store.get("Task", "going")).unwrap().is_deleting());
    assert!(matches!(
        store.plugin_mutate(
            amend("going", &g.metadata.uid, g.metadata.generation, json!({})),
            writer,
            &lease
        ),
        Err(StoreError::Forbidden(_))
    ));
    assert_eq!(
        known(store.get("Task", "foreign"))
            .unwrap()
            .metadata
            .generation,
        1
    );
}
