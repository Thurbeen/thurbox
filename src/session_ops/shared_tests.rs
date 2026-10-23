//! The four pipelines against a shareable host, with the host's CLI played by
//! a scripted runner (`host_cli::fake`): what gets delegated, in what words,
//! and how the host's answer lands in the local rows. No ssh, no tmux.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use serde_json::{json, Value};

use super::host_cli::{self, fake, Usable};
use super::mirror;
use super::spawn::SpawnRequest;
use crate::session::SessionId;
use crate::storage::Database;
use crate::sync::SharedSession;

const HOST: &str = "devbox";
const BACKEND: &str = "ssh:devbox";

/// The host as it would list a session it holds.
fn host_session(id: SessionId, name: &str) -> SharedSession {
    SharedSession {
        id,
        name: name.into(),
        agent: "codex".into(),
        backend_id: "%4".into(),
        backend_type: "local-tmux".into(),
        agent_session_id: Some("conv-on-host".into()),
        cwd: Some(PathBuf::from("/srv/repo")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
        key: None,
    }
}

/// What the scripted host holds: its active and deleted lists, which the
/// runner serves and the delegated verbs move rows between.
#[derive(Default)]
struct Host {
    active: Vec<SharedSession>,
    deleted: Vec<(SessionId, bool)>,
    /// When each deleted row was deleted, on the host's clock; `0` for a row
    /// the test seeded without one.
    deleted_at: std::collections::HashMap<SessionId, u64>,
    updated_at: std::collections::HashMap<SessionId, u64>,
    /// The host's clock, moved by each delete it takes.
    clock: u64,
    /// A canned refusal for the next `create`.
    refuse_create: Option<String>,
    delete_after_active_read: Vec<SessionId>,
}

struct Rig {
    _temp: tempfile::TempDir,
    /// This machine and the scripted host, each served.
    backends: crate::backend::BackendRegistry,
    _guard: crate::paths::TestPathGuard,
    db: Database,
    host: Rc<RefCell<Host>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        fake::clear();
        mirror::forget_lists();
    }
}

fn rig() -> Rig {
    let temp = tempfile::TempDir::new().unwrap();
    let guard = crate::paths::TestPathGuard::new(temp.path());
    let path = crate::agent::host_config::hosts_config_path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!("[[hosts]]\nname = \"{HOST}\"\ndestination = \"me@{HOST}\"\n"),
    )
    .unwrap();
    let host = Rc::new(RefCell::new(Host::default()));
    fake::force_usable(Usable::Yes(fake::cli()));
    let scripted = Rc::clone(&host);
    fake::install_runner(Box::new(move |_, args| {
        let mut host = scripted.borrow_mut();
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["session", "create", rest @ ..] => {
                if let Some(refusal) = host.refuse_create.take() {
                    return Err(fake::answered(&refusal));
                }
                let name = rest
                    .windows(2)
                    .find(|w| w[0] == "--name")
                    .map(|w| w[1])
                    .unwrap_or("unnamed");
                let key = rest
                    .windows(2)
                    .find(|w| w[0] == "--key")
                    .map(|w| w[1].to_string());
                let id = SessionId::default();
                let mut created = host_session(id, name);
                created.key = key;
                host.active.push(created);
                Ok(json!({ "id": id.to_string(), "name": name }))
            }
            ["session", "list", "--deleted", rest @ ..] => {
                let since: u64 = match rest {
                    ["--since", ms] => ms.parse().unwrap(),
                    _ => 0,
                };
                Ok(Value::Array(
                    host.deleted
                        .iter()
                        .map(|(id, force)| (id, force, host.deleted_at.get(id).copied().unwrap_or(0)))
                        .filter(|(id, _, at)| (*at).max(host.updated_at.get(id).copied().unwrap_or(*at)) >= since)
                        .map(|(id, force, at)| {
                            json!({ "id": id.to_string(), "force_deleted": force, "deleted_at": at, "updated_at": host.updated_at.get(id).copied().unwrap_or(at) })
                        })
                        .collect(),
                ))
            }
            ["session", "list"] => {
                let answer = Value::Array(
                    host.active
                        .iter()
                        .map(|s| mirror::session_to_json(s, Some("working"), None, Some(1)))
                        .collect(),
                );
                for id in std::mem::take(&mut host.delete_after_active_read) {
                    host.active.retain(|s| s.id != id);
                    host.clock += 1;
                    let at = host.clock;
                    host.deleted.push((id, false));
                    host.deleted_at.insert(id, at);
                }
                Ok(answer)
            }
            ["session", "delete", id, rest @ ..] => {
                let id: SessionId = id.parse().unwrap();
                if !host.active.iter().any(|s| s.id == id) {
                    // Word for word what the host's own resolver answers for a
                    // row it does not hold — see `cli::session_ref::resolve`.
                    return Err(fake::not_found(&format!("Session not found: {id}")));
                }
                host.active.retain(|s| s.id != id);
                let force = rest.contains(&"--force");
                host.deleted.push((id, force));
                host.clock += 1;
                let at = host.clock;
                host.deleted_at.insert(id, at);
                Ok(
                    json!({ "deleted": true, "killed_window": true, "removed_worktrees": ["/srv/wt"] }),
                )
            }
            ["session", "reap", id] => {
                let id: SessionId = id.parse().unwrap();
                let known = host.deleted.iter().any(|(gone, _)| *gone == id);
                Ok(json!({ "reaped": known, "id": id.to_string() }))
            }
            ["session", "restart", _id, ..] => Ok(json!({ "restarted": true })),
            ["session", "reparent", id, parent] => {
                let id: SessionId = id.parse().unwrap();
                let parent = (*parent != "--clear").then(|| parent.parse().unwrap());
                for row in host.active.iter_mut().filter(|s| s.id == id) {
                    row.parent_session_id = parent;
                }
                Ok(json!({ "session_id": id.to_string() }))
            }
            ["session", "restore", id, ..] => {
                let id: SessionId = id.parse().unwrap();
                host.deleted.retain(|(gone, _)| *gone != id);
                host.active.push(host_session(id, "back"));
                Ok(json!({ "restored": true, "worktrees_wanted": 1, "worktrees_recovered": 1 }))
            }
            other => Err(fake::answered(&format!(
                "unscripted host command: {}",
                other.join(" ")
            ))),
        }
    }));
    Rig {
        _temp: temp,
        backends: crate::backend::registry::inert_serving(&[crate::session::Route::remote(
            crate::session::Via::Ssh,
            HOST,
            Some(crate::session::Multiplexer::Tmux),
        )]),
        _guard: guard,
        db: Database::open_in_memory().unwrap(),
        host,
    }
}

fn create_request(name: &str) -> SpawnRequest {
    SpawnRequest {
        name: name.into(),
        repo_path: PathBuf::from("/srv/repo"),
        host: Some(HOST.into()),
        agent: Some("codex".into()),
        worktree_branch: Some("feat/x".into()),
        ..Default::default()
    }
}

#[test]
fn a_create_on_a_shareable_host_is_the_hosts_and_lands_with_its_id() {
    let rig = rig();
    let result =
        super::spawn_session_headless(&rig.db, &rig.backends, create_request("shared")).unwrap();

    let calls = fake::calls();
    assert_eq!(calls[0][..2], ["session", "create"]);
    assert!(calls[0].contains(&"--name".to_string()) && calls[0].contains(&"shared".to_string()));
    assert!(calls[0].contains(&"--agent".to_string()) && calls[0].contains(&"codex".to_string()));
    assert!(calls[0].contains(&"--worktree-branch".to_string()));
    // The row is the host's: its id, its facts, on the observer's backend name.
    let host_id = rig.host.borrow().active[0].id;
    assert_eq!(result.session_id, host_id);
    assert_eq!(result.sharing, None);
    let row = rig.db.get_session_by_id(host_id).unwrap().unwrap();
    assert_eq!(row.backend_type, BACKEND);
    assert_eq!(row.backend_id, "%4");
    assert_eq!(row.agent_session_id.as_deref(), Some("conv-on-host"));
    assert_eq!(
        rig.db
            .load_hook_state(host_id)
            .unwrap()
            .unwrap()
            .state
            .as_deref(),
        Some("working")
    );
}

/// A key is the host's to arbitrate (ADR-24): it is forwarded as `--key`, not
/// checked against the mirror, and the row lands with the key the host gave it.
#[test]
fn a_key_is_forwarded_to_the_host_and_lands_with_the_row() {
    let rig = rig();
    // A stale mirror row holding the key must not refuse a creation the host
    // itself decides.
    let mut stale = host_session(SessionId::default(), "old-lead");
    stale.backend_type = BACKEND.into();
    stale.key = Some("fleet.lead".into());
    rig.db.upsert_session(&stale).unwrap();

    let mut req = create_request("lead");
    req.key = Some("fleet.lead".into());
    let result = super::spawn_session_headless(&rig.db, &rig.backends, req).unwrap();

    let create = &fake::calls()[0];
    let at = create
        .iter()
        .position(|w| w == "--key")
        .expect("--key forwarded");
    assert_eq!(create[at + 1], "fleet.lead");
    assert_eq!(result.key.as_deref(), Some("fleet.lead"));
    assert_eq!(
        rig.db
            .get_session_by_id(result.session_id)
            .unwrap()
            .unwrap()
            .key
            .as_deref(),
        Some("fleet.lead")
    );
}

#[test]
fn a_parent_on_another_host_is_refused_before_any_round_trip() {
    let rig = rig();
    let local_parent = SessionId::default();
    let mut parent = host_session(local_parent, "lead");
    parent.backend_type = "local-tmux".into();
    rig.db.upsert_session(&parent).unwrap();
    let mut req = create_request("worker");
    req.parent_session_id = Some(local_parent);
    let err = super::spawn_session_headless(&rig.db, &rig.backends, req).unwrap_err();
    assert!(err.contains("same host"), "{err}");
    assert!(fake::calls().is_empty());
}

#[test]
fn the_hosts_refusal_is_the_callers_error_verbatim() {
    let rig = rig();
    rig.host.borrow_mut().refuse_create = Some("Unknown agent 'codex' on devbox".into());
    let err =
        super::spawn_session_headless(&rig.db, &rig.backends, create_request("nope")).unwrap_err();
    assert_eq!(err, "Unknown agent 'codex' on devbox".into());
    assert!(rig.db.list_active_sessions().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn a_local_pre_hook_veto_prevents_the_delegation() {
    let rig = rig();
    let hooks = crate::agent::hooks_config::hooks_config_path().unwrap();
    std::fs::write(
        &hooks,
        "[[hooks]]\nevent = \"session.pre_create\"\ncommand = \"exit 3\"\n",
    )
    .unwrap();
    let err = super::spawn_session_headless(&rig.db, &rig.backends, create_request("vetoed"))
        .unwrap_err();
    assert!(err.contains("exit 3"), "{err}");
    assert!(fake::calls().is_empty(), "nothing reached the host");
}

#[test]
fn a_fork_is_not_delegated_and_says_why() {
    // A fork resumes the parent's conversation in the parent's checkout, which
    // the host's `create` does not take; it stays on the path driven from here
    // — which, with no reachable host in a test, fails at the worktree. The
    // fact under test is that no `create` reached the scripted host.
    let rig = rig();
    let mut req = create_request("forked");
    req.fork_session_id = Some("parent-conv".into());
    let _ = super::spawn_session_headless(&rig.db, &rig.backends, req);
    assert!(fake::calls().is_empty());
}

#[test]
fn a_fork_without_a_pinned_conversation_is_not_delegated_either() {
    // A parent with no agent conversation id forks with no `fork_session_id`,
    // but still shares its checkout — which the host's `create` would drop,
    // and which `delegated_create_args` refuses. The worktrees it inherits are
    // what make it a fork, so it stays on this side as well.
    let rig = rig();
    let mut req = create_request("forked-bare");
    req.inherit_worktrees = vec![crate::sync::SharedWorktree {
        repo_path: PathBuf::from("/srv/repo"),
        worktree_path: PathBuf::from("/srv/worktrees/parent"),
        branch: "parent".into(),
        created_by_thurbox: true,
    }];
    // Driven from here, it fails later, at the worktree; delegated, it would
    // be refused up front as a fork the host's CLI cannot take.
    if let Err(e) = super::spawn_session_headless(&rig.db, &rig.backends, req) {
        assert!(!e.contains("a fork cannot be handed"), "{e}");
    }
    assert!(fake::calls().is_empty(), "{:?}", fake::calls());
}

#[test]
fn a_delete_is_performed_by_the_host_and_mirrored_here() {
    let rig = rig();
    let id = SessionId::default();
    let mut row = host_session(id, "doomed");
    row.backend_type = BACKEND.into();
    rig.db.upsert_session(&row).unwrap();
    rig.host
        .borrow_mut()
        .active
        .push(host_session(id, "doomed"));

    let report = super::delete_session_headless(&rig.db, &rig.backends, id, true).unwrap();
    let calls = fake::calls();
    assert_eq!(
        calls[0],
        vec!["session", "delete", &id.to_string(), "--force"]
    );
    assert!(report.killed_window);
    assert_eq!(report.removed_worktrees, vec!["/srv/wt".to_string()]);
    assert!(rig.db.get_session_by_id(id).unwrap().is_none());
    let gone = rig.db.get_deleted_session_by_id(id).unwrap().unwrap();
    assert!(gone.force_deleted);
}

#[test]
fn a_restart_asks_the_host_and_a_relaunch_says_if_missing() {
    let rig = rig();
    let id = SessionId::default();
    let mut row = host_session(id, "again");
    row.backend_type = BACKEND.into();
    rig.db.upsert_session(&row).unwrap();
    rig.host.borrow_mut().active.push(host_session(id, "again"));

    super::restart::restart_session_headless_with(&rig.db, &rig.backends, id, false).unwrap();
    assert_eq!(
        fake::calls()[0],
        vec!["session", "restart", &id.to_string()]
    );

    super::restart::restart_session_headless_with(&rig.db, &rig.backends, id, true).unwrap();
    let calls = fake::calls();
    let relaunch = calls
        .iter()
        .filter(|c| c.get(1).map(String::as_str) == Some("restart"))
        .nth(1)
        .unwrap();
    assert_eq!(relaunch.last().map(String::as_str), Some("--if-missing"));
}

/// `--if-missing` is "I am a repairer" on the wire, and a host reads both halves
/// out of it: put a window back only if one is gone, and stand down for a
/// restart hold somebody there already has. An unpark is neither — `stop`
/// killed the window on the host already, and the operator asked for it back —
/// so a hold left behind there would otherwise no-op a `session start` that has
/// cleared the parked mark here.
#[test]
fn an_unpark_reaches_the_host_as_a_plain_restart() {
    let rig = rig();
    let id = SessionId::default();
    let mut row = host_session(id, "parked");
    row.backend_type = BACKEND.into();
    rig.db.upsert_session(&row).unwrap();
    rig.host
        .borrow_mut()
        .active
        .push(host_session(id, "parked"));
    rig.db.set_session_stopped(id, true).unwrap();

    super::restart::start_session_headless(&rig.db, &rig.backends, id).unwrap();

    let restart = fake::calls()
        .into_iter()
        .find(|c| c.get(1).map(String::as_str) == Some("restart"))
        .expect("the unpark is delegated");
    assert_eq!(restart, vec!["session", "restart", &id.to_string()]);
}

#[test]
fn a_reparent_is_made_on_the_host_whose_record_wins() {
    let rig = rig();
    let (lead, worker) = (SessionId::default(), SessionId::default());
    for (id, name) in [(lead, "lead"), (worker, "worker")] {
        let mut row = host_session(id, name);
        row.backend_type = BACKEND.into();
        rig.db.upsert_session(&row).unwrap();
        rig.host.borrow_mut().active.push(host_session(id, name));
    }

    super::reparent::reparent_session_headless(&rig.db, worker, Some(lead)).unwrap();

    assert_eq!(
        fake::calls()[0],
        vec![
            "session",
            "reparent",
            &worker.to_string(),
            &lead.to_string()
        ]
    );
    let row = rig.db.get_session_by_id(worker).unwrap().unwrap();
    assert_eq!(
        row.parent_session_id,
        Some(lead),
        "mirrored back from the host"
    );
}

#[test]
fn a_restore_is_performed_by_the_host_and_the_row_returns() {
    let rig = rig();
    let id = SessionId::default();
    let mut row = host_session(id, "back");
    row.backend_type = BACKEND.into();
    rig.db.upsert_session(&row).unwrap();
    rig.db.soft_delete_session(id).unwrap();
    rig.db.mark_session_force_deleted(id).unwrap();
    rig.host.borrow_mut().deleted.push((id, true));

    let report = super::restore_session_headless(&rig.db, &rig.backends, id, true).unwrap();
    assert_eq!(
        fake::calls()[0],
        vec!["session", "restore", &id.to_string(), "--best-effort"]
    );
    assert!(report.best_effort);
    assert_eq!(report.worktrees_recovered, 1);
    assert!(rig.db.get_session_by_id(id).unwrap().is_some());
    assert!(rig.db.get_deleted_session_by_id(id).unwrap().is_none());
}

/// A mirror is a snapshot, so a namesake it still lists may already be gone on
/// the host. The local name guard is therefore asked of local rows only, and a
/// remote restore is delegated so the host decides it against the rows that
/// author it — `restore_refusal` conditions its worktree check on the same
/// thing for the same reason.
#[test]
fn a_remote_restore_is_not_refused_by_a_namesake_in_the_local_mirror() {
    let rig = rig();
    let stale = SessionId::default();
    let mut namesake = host_session(stale, "back");
    namesake.backend_type = BACKEND.into();
    rig.db.upsert_session(&namesake).unwrap();

    let id = SessionId::default();
    let mut row = host_session(id, "back");
    row.backend_type = BACKEND.into();
    rig.db.upsert_session(&row).unwrap();
    rig.db.soft_delete_session(id).unwrap();
    rig.host.borrow_mut().deleted.push((id, false));

    let report = super::restore_session_headless(&rig.db, &rig.backends, id, true).unwrap();
    assert_eq!(report.name, "back");
    assert_eq!(
        fake::calls()[0],
        vec!["session", "restore", &id.to_string(), "--best-effort"],
        "the host is what decides a name in its own namespace"
    );
}

#[test]
fn a_restore_on_a_remote_host_that_cannot_be_delegated_to_is_still_refused() {
    let rig = rig();
    fake::force_usable(Usable::No("no thurbox-cli there".into()));
    let id = SessionId::default();
    let mut row = host_session(id, "stuck");
    row.backend_type = BACKEND.into();
    rig.db.upsert_session(&row).unwrap();
    rig.db.soft_delete_session(id).unwrap();
    let err = super::restore_session_headless(&rig.db, &rig.backends, id, false).unwrap_err();
    assert!(err.contains("local-only"), "{err}");
    assert!(fake::calls().is_empty());
}

#[test]
fn a_sync_pass_adopts_what_the_host_holds_and_registers_what_it_does_not() {
    let rig = rig();
    let theirs = SessionId::default();
    rig.host
        .borrow_mut()
        .active
        .push(host_session(theirs, "theirs"));
    let legacy = SessionId::default();
    let mut mine = host_session(legacy, "legacy");
    mine.backend_type = BACKEND.into();
    mine.key = Some("fleet.legacy".into());
    rig.db.upsert_session(&mine).unwrap();

    let reports = mirror::sync(&rig.db, Some(HOST), false).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].adopted, vec![theirs]);
    assert_eq!(reports[0].unknown_local, vec![legacy]);
    assert!(reports[0].registered.is_empty());
    assert!(rig.db.get_session_by_id(theirs).unwrap().is_some());

    // `--adopt` hands the legacy row to the host as `session register`. The
    // scripted host does not implement it, so the row is reported, not
    // registered — the words on the wire are what is under test.
    let reports = mirror::sync(&rig.db, Some(HOST), true).unwrap();
    let register = fake::calls()
        .into_iter()
        .find(|c| c.get(1).map(String::as_str) == Some("register"))
        .expect("register was attempted");
    assert_eq!(register[2], "--json-row");
    let body: Value = serde_json::from_str(&register[3]).unwrap();
    assert_eq!(body["id"], legacy.to_string());
    assert_eq!(body["name"], "legacy");
    // The key goes with it, for the host's `register` to admit or drop under
    // its own claim — the one path by which `sync --adopt` gives a row a key.
    assert_eq!(body["key"], "fleet.legacy");
    assert!(reports[0].registered.is_empty());
}

#[test]
fn an_unknown_host_name_is_the_one_hard_failure_of_sync() {
    let rig = rig();
    let err = mirror::sync(&rig.db, Some("nowhere"), false).unwrap_err();
    assert!(err.contains("Unknown host 'nowhere'"), "{err}");
    assert!(err.contains(HOST), "names the configured hosts: {err}");
}

#[test]
fn a_host_with_sharing_off_is_used_the_old_way() {
    let rig = rig();
    let path = crate::agent::host_config::hosts_config_path().unwrap();
    std::fs::write(
        &path,
        format!(
            "[[hosts]]\nname = \"{HOST}\"\ndestination = \"me@{HOST}\"\nshare_sessions = false\n"
        ),
    )
    .unwrap();
    let hosts = crate::agent::host_config::load_all();
    let host = hosts.get(HOST).unwrap();
    // The forced verdict is bypassed by the config switch, before any probe.
    assert!(matches!(host_cli::usable(host), Usable::No(_)));
    assert_eq!(host_cli::delegated(host), None);
    let reports = mirror::sync(&rig.db, None, false).unwrap();
    assert!(reports.is_empty(), "{reports:?}");
}

#[test]
fn a_delete_the_host_does_not_know_is_taken_here_rather_than_failing() {
    // A fork minted locally on a shareable host, a row from before ADR-24, or
    // one a peer already deleted there: the host answers "Session not found",
    // and the delete used to abort on it — leaving the local row active and
    // attached, which reads as "delete does nothing".
    let rig = rig();
    let id = SessionId::default();
    let mut row = host_session(id, "unknown-there");
    row.backend_type = BACKEND.into();
    rig.db.upsert_session(&row).unwrap();

    let report = super::delete_session_headless(&rig.db, &rig.backends, id, true).unwrap();

    assert_eq!(
        fake::calls()[0],
        vec!["session", "delete", &id.to_string(), "--force"]
    );
    let note = report.host_unknown.expect("the fall-through is reported");
    assert!(note.contains(HOST), "{note}");
    assert!(
        rig.db.get_session_by_id(id).unwrap().is_none(),
        "the local row must go regardless"
    );
    assert!(
        rig.db
            .get_deleted_session_by_id(id)
            .unwrap()
            .unwrap()
            .force_deleted
    );
}

#[test]
fn any_other_refusal_from_the_host_still_aborts_the_delete() {
    // The counterpart: only "not found" falls through. A host that failed for
    // some other reason may still be holding the session, and deleting the
    // local row would strand it there.
    let rig = rig();
    let id = SessionId::default();
    let mut row = host_session(id, "wedged");
    row.backend_type = BACKEND.into();
    rig.db.upsert_session(&row).unwrap();
    fake::install_runner(Box::new(|_, _| Err(fake::answered("worktree is locked"))));

    let err = super::delete_session_headless(&rig.db, &rig.backends, id, true).unwrap_err();
    assert_eq!(err, "worktree is locked".into());
    assert!(rig.db.get_session_by_id(id).unwrap().is_some());
}

#[test]
fn a_soft_deleted_row_on_a_shareable_host_is_reaped_there() {
    // The host's row is soft-deleted too, and a host running only `thurbox-cli`
    // has no interface to collect it once the undo window closes. Nothing here
    // asked it to, so every remote soft delete leaked its windows for good.
    let rig = rig();
    let id = SessionId::default();
    let mut row = host_session(id, "doomed");
    row.backend_type = BACKEND.into();
    rig.db.upsert_session(&row).unwrap();
    rig.host
        .borrow_mut()
        .active
        .push(host_session(id, "doomed"));

    super::delete_session_headless(&rig.db, &rig.backends, id, false).unwrap();
    let reaped = super::reap_soft_deleted(&rig.db, &rig.backends, id).unwrap();

    assert!(reaped, "the row was processed");
    let reap = fake::calls()
        .into_iter()
        .find(|c| c.get(1).map(String::as_str) == Some("reap"))
        .expect("the host was asked to reap it");
    assert_eq!(reap, vec!["session", "reap", &id.to_string()]);
}

#[test]
fn a_delete_taken_while_the_host_was_unusable_is_pushed_on_the_next_pass() {
    // The delete lands locally (the host's CLI is in backoff), so the host
    // still lists the row as active. The mirror used to read that as "restore
    // me" and undo the delete within one interval, forever.
    let rig = rig();
    let id = SessionId::default();
    let mut row = host_session(id, "deleted-here");
    row.backend_type = BACKEND.into();
    rig.db.upsert_session(&row).unwrap();
    rig.host
        .borrow_mut()
        .active
        .push(host_session(id, "deleted-here"));

    fake::force_usable(Usable::No("ssh timed out".into()));
    super::delete_session_headless(&rig.db, &rig.backends, id, false).unwrap();
    assert!(rig.db.get_session_by_id(id).unwrap().is_none());

    fake::force_usable(Usable::Yes(fake::cli()));
    let reports = mirror::sync(&rig.db, Some(HOST), false).unwrap();

    assert_eq!(reports[0].tombstoned, vec![id]);
    assert!(reports[0].restored.is_empty(), "the tombstone stands");
    assert!(
        rig.db.get_session_by_id(id).unwrap().is_none(),
        "the row must not come back"
    );
    // And the host is told, so the two sides converge instead of disagreeing
    // for as long as the row exists.
    assert!(
        fake::calls()
            .iter()
            .any(|c| c.as_slice() == ["session", "delete", &id.to_string(), "--soft"]),
        "the delete was pushed to the host: {:?}",
        fake::calls()
    );
    assert!(rig.host.borrow().active.is_empty());
}

#[test]
fn a_long_deleted_history_is_read_whole_once_and_then_only_by_what_changed() {
    let rig = rig();
    fake::force_usable(Usable::Yes(host_cli::CliInfo {
        deleted_changes_since: true,
        ..fake::cli()
    }));
    // ~1600 sessions deleted on the host long ago, one of them still held here.
    let stale = {
        let mut host = rig.host.borrow_mut();
        for i in 0..1600 {
            let id = SessionId::default();
            host.deleted.push((id, false));
            host.deleted_at.insert(id, 1_000 + i);
        }
        host.clock = 5_000;
        host.deleted[0].0
    };
    let mut held = host_session(stale, "stale");
    held.backend_type = BACKEND.into();
    rig.db.upsert_session(&held).unwrap();
    let live = SessionId::default();
    rig.host
        .borrow_mut()
        .active
        .push(host_session(live, "live"));

    let reports = mirror::sync(&rig.db, Some(HOST), false).unwrap();
    assert_eq!(reports[0].deleted, vec![stale]);
    assert!(reports[0].unknown_local.is_empty(), "{:?}", reports[0]);
    assert!(rig.db.get_session_by_id(live).unwrap().is_some());

    // The host deletes `live` itself, after everything the first pass read.
    {
        let mut host = rig.host.borrow_mut();
        host.active.retain(|s| s.id != live);
        host.deleted.push((live, false));
        host.deleted_at.insert(live, 5_001);
    }
    let before = fake::calls().len();
    let reports = mirror::sync(&rig.db, Some(HOST), false).unwrap();
    let deleted_lists: Vec<Vec<String>> = fake::calls()[before..]
        .iter()
        .filter(|c| c.get(2).map(String::as_str) == Some("--deleted"))
        .cloned()
        .collect();
    assert_eq!(
        deleted_lists,
        vec![vec![
            "session".to_string(),
            "list".into(),
            "--deleted".into(),
            "--since".into(),
            "2599".into(),
        ]],
        "the second pass asks for what was deleted since the newest row it holds"
    );
    assert_eq!(reports[0].deleted, vec![live]);
    // Still known to have been deleted there, though this pass never listed it.
    assert!(rig.db.get_deleted_session_by_id(stale).unwrap().is_some());
    assert!(reports[0].unknown_local.is_empty(), "{:?}", reports[0]);
}

#[test]
fn a_delete_between_active_and_deleted_reads_survives_the_next_delta() {
    let rig = rig();
    fake::force_usable(Usable::Yes(host_cli::CliInfo {
        deleted_changes_since: true,
        ..fake::cli()
    }));
    let first = SessionId::default();
    let later = SessionId::default();
    {
        let mut host = rig.host.borrow_mut();
        host.active.push(host_session(first, "first"));
        host.active.push(host_session(later, "later"));
    }
    mirror::sync(&rig.db, Some(HOST), false).unwrap();
    rig.host.borrow_mut().delete_after_active_read = vec![first, later];
    mirror::sync(&rig.db, Some(HOST), false).unwrap();
    let reports = mirror::sync(&rig.db, Some(HOST), false).unwrap();
    for id in [first, later] {
        assert!(
            rig.db.get_deleted_session_by_id(id).unwrap().is_some(),
            "the later deleted answer wins for {id}: {reports:?}"
        );
        assert!(rig.db.get_session_by_id(id).unwrap().is_none());
    }
    assert!(reports[0].unknown_local.is_empty(), "{:?}", reports[0]);
}

#[test]
fn force_deletion_of_an_older_tombstone_reaches_the_next_observer() {
    let rig = rig();
    fake::force_usable(Usable::Yes(host_cli::CliInfo {
        deleted_changes_since: true,
        ..fake::cli()
    }));
    let old = SessionId::default();
    let newer = SessionId::default();
    {
        let mut host = rig.host.borrow_mut();
        host.active.push(host_session(old, "old"));
        host.active.push(host_session(newer, "newer"));
    }
    mirror::sync(&rig.db, Some(HOST), false).unwrap();
    {
        let mut host = rig.host.borrow_mut();
        host.active.clear();
        host.deleted = vec![(old, false), (newer, false)];
        host.deleted_at.insert(old, 10);
        host.deleted_at.insert(newer, 20);
    }
    mirror::sync(&rig.db, Some(HOST), false).unwrap();
    assert!(
        !rig.db
            .get_deleted_session_by_id(old)
            .unwrap()
            .unwrap()
            .force_deleted
    );
    rig.host.borrow_mut().deleted[0].1 = true;
    rig.host.borrow_mut().updated_at.insert(old, 30);
    mirror::sync(&rig.db, Some(HOST), false).unwrap();
    assert!(
        rig.db
            .get_deleted_session_by_id(old)
            .unwrap()
            .unwrap()
            .force_deleted,
        "a force-delete mark must propagate even below the deletion-time cursor"
    );
}

/// Mirror one session from the scripted host, then rewrite `hosts.toml` to
/// `config` and sync again: what is left of the row, and every call the host
/// was asked to run after the rewrite.
fn sync_after_hosts_toml(config: &str) -> (Rig, SessionId, Vec<Vec<String>>) {
    let rig = rig();
    let id = SessionId::default();
    rig.host
        .borrow_mut()
        .active
        .push(host_session(id, "mirrored"));
    mirror::sync(&rig.db, None, false).unwrap();
    assert!(rig.db.get_session_by_id(id).unwrap().is_some());
    let before = fake::calls().len();
    let path = crate::agent::host_config::hosts_config_path().unwrap();
    std::fs::write(path, config).unwrap();
    crate::agent::host_config::with_discovered_wsl(Ok(Vec::new()), || {
        mirror::sync(&rig.db, None, false).unwrap();
    });
    let calls = fake::calls().split_off(before);
    (rig, id, calls)
}

#[test]
fn a_removed_hosts_mirrored_rows_are_forgotten_without_touching_the_host() {
    let (rig, id, calls) = sync_after_hosts_toml("");
    assert!(rig.db.get_session_by_id(id).unwrap().is_none());
    assert!(
        rig.db.get_deleted_session_by_id(id).unwrap().is_none(),
        "forgotten, not tombstoned: a tombstone would be pushed back as a delete"
    );
    assert!(calls.is_empty(), "nothing may reach the host: {calls:?}");
}

#[test]
fn a_removed_hosts_deleted_rows_are_forgotten_too() {
    let rig = rig();
    let id = SessionId::default();
    let mut row = host_session(id, "gone");
    row.backend_type = BACKEND.into();
    rig.db.upsert_session(&row).unwrap();
    rig.db.soft_delete_session(id).unwrap();
    let path = crate::agent::host_config::hosts_config_path().unwrap();
    std::fs::write(path, "").unwrap();
    crate::agent::host_config::with_discovered_wsl(Ok(Vec::new()), || {
        mirror::sync(&rig.db, None, false).unwrap();
    });
    assert!(rig.db.get_deleted_session_by_id(id).unwrap().is_none());
}

#[test]
fn an_unreadable_hosts_toml_forgets_nothing() {
    let (rig, id, _) = sync_after_hosts_toml("[[hosts]\nname = ");
    assert!(
        rig.db.get_session_by_id(id).unwrap().is_some(),
        "a file that does not parse says nothing about which hosts were removed"
    );
}

#[test]
fn local_rows_and_a_configured_hosts_rows_are_kept() {
    let local = SessionId::default();
    let (rig, id, _) = sync_after_hosts_toml(&format!(
        "[[hosts]]\nname = \"{HOST}\"\ndestination = \"me@{HOST}\"\nshare_sessions = false\n"
    ));
    let mut row = host_session(local, "here");
    row.backend_type = "local:tmux".into();
    rig.db.upsert_session(&row).unwrap();
    crate::agent::host_config::with_discovered_wsl(Ok(Vec::new()), || {
        mirror::sync(&rig.db, None, false).unwrap();
    });
    assert!(rig.db.get_session_by_id(id).unwrap().is_some());
    assert!(rig.db.get_session_by_id(local).unwrap().is_some());
}

#[test]
fn distros_that_could_not_be_listed_forget_nothing() {
    let rig = rig();
    let id = SessionId::default();
    let mut row = host_session(id, "on-a-distro");
    row.backend_type = "wsl:Ubuntu".into();
    rig.db.upsert_session(&row).unwrap();
    crate::agent::host_config::with_discovered_wsl(Err("wsl.exe timed out".into()), || {
        mirror::sync(&rig.db, None, false).unwrap();
    });
    assert!(
        rig.db.get_session_by_id(id).unwrap().is_some(),
        "an unlisted distro is unknown, not removed"
    );
    crate::agent::host_config::with_discovered_wsl(Ok(Vec::new()), || {
        mirror::sync(&rig.db, None, false).unwrap();
    });
    assert!(rig.db.get_session_by_id(id).unwrap().is_none());
}

#[test]
fn inside_wsl_without_wsl_exe_sibling_distros_are_not_forgotten() {
    if cfg!(windows) || crate::paths::which_on_path("wsl.exe") {
        eprintln!("skipping: wsl.exe is reachable here");
        return;
    }
    let rig = rig();
    let id = SessionId::default();
    let mut row = host_session(id, "on-a-sibling");
    row.backend_type = "wsl:Debian".into();
    rig.db.upsert_session(&row).unwrap();
    // Interop off (`appendWindowsPath = false`): no `wsl.exe` on PATH, which
    // discovery reads as "no distros" — true off WSL, not inside it.
    crate::session::host_def::with_wsl_distro(Some("Ubuntu"), || {
        mirror::sync(&rig.db, None, false).unwrap();
    });
    assert!(
        rig.db.get_session_by_id(id).unwrap().is_some(),
        "a sibling distro that could not be listed is unknown, not removed"
    );
}
