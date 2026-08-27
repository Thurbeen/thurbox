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
    }
}

/// What the scripted host holds: its active and deleted lists, which the
/// runner serves and the delegated verbs move rows between.
#[derive(Default)]
struct Host {
    active: Vec<SharedSession>,
    deleted: Vec<(SessionId, bool)>,
    /// A canned refusal for the next `create`.
    refuse_create: Option<String>,
}

struct Rig {
    _temp: tempfile::TempDir,
    _guard: crate::paths::TestPathGuard,
    db: Database,
    host: Rc<RefCell<Host>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        fake::clear();
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
                    return Err(refusal);
                }
                let name = rest
                    .windows(2)
                    .find(|w| w[0] == "--name")
                    .map(|w| w[1])
                    .unwrap_or("unnamed");
                let id = SessionId::default();
                host.active.push(host_session(id, name));
                Ok(json!({ "id": id.to_string(), "name": name }))
            }
            ["session", "list", "--deleted"] => Ok(Value::Array(
                host.deleted
                    .iter()
                    .map(|(id, force)| json!({ "id": id.to_string(), "force_deleted": force }))
                    .collect(),
            )),
            ["session", "list"] => Ok(Value::Array(
                host.active
                    .iter()
                    .map(|s| mirror::session_to_json(s, Some("working"), None))
                    .collect(),
            )),
            ["session", "delete", id, rest @ ..] => {
                let id: SessionId = id.parse().unwrap();
                host.active.retain(|s| s.id != id);
                let force = rest.contains(&"--force");
                host.deleted.push((id, force));
                Ok(
                    json!({ "deleted": true, "killed_window": true, "removed_worktrees": ["/srv/wt"] }),
                )
            }
            ["session", "restart", _id, ..] => Ok(json!({ "restarted": true })),
            ["session", "restore", id, ..] => {
                let id: SessionId = id.parse().unwrap();
                host.deleted.retain(|(gone, _)| *gone != id);
                host.active.push(host_session(id, "back"));
                Ok(json!({ "restored": true, "worktrees_wanted": 1, "worktrees_recovered": 1 }))
            }
            other => Err(format!("unscripted host command: {}", other.join(" "))),
        }
    }));
    Rig {
        _temp: temp,
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
    let result = super::spawn_session_headless(&rig.db, create_request("shared")).unwrap();

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

#[test]
fn a_parent_on_another_host_is_refused_before_any_round_trip() {
    let rig = rig();
    let local_parent = SessionId::default();
    let mut parent = host_session(local_parent, "lead");
    parent.backend_type = "local-tmux".into();
    rig.db.upsert_session(&parent).unwrap();
    let mut req = create_request("worker");
    req.parent_session_id = Some(local_parent);
    let err = super::spawn_session_headless(&rig.db, req).unwrap_err();
    assert!(err.contains("same host"), "{err}");
    assert!(fake::calls().is_empty());
}

#[test]
fn the_hosts_refusal_is_the_callers_error_verbatim() {
    let rig = rig();
    rig.host.borrow_mut().refuse_create = Some("Unknown agent 'codex' on devbox".into());
    let err = super::spawn_session_headless(&rig.db, create_request("nope")).unwrap_err();
    assert_eq!(err, "Unknown agent 'codex' on devbox");
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
    let err = super::spawn_session_headless(&rig.db, create_request("vetoed")).unwrap_err();
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
    let _ = super::spawn_session_headless(&rig.db, req);
    assert!(fake::calls().is_empty());
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

    let report = super::delete_session_headless(&rig.db, id, true).unwrap();
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

    super::restart::restart_session_headless_with(&rig.db, id, false).unwrap();
    assert_eq!(
        fake::calls()[0],
        vec!["session", "restart", &id.to_string()]
    );

    super::restart::restart_session_headless_with(&rig.db, id, true).unwrap();
    let calls = fake::calls();
    let relaunch = calls
        .iter()
        .filter(|c| c.get(1).map(String::as_str) == Some("restart"))
        .nth(1)
        .unwrap();
    assert_eq!(relaunch.last().map(String::as_str), Some("--if-missing"));
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

    let report = super::restore_session_headless(&rig.db, id, true).unwrap();
    assert_eq!(
        fake::calls()[0],
        vec!["session", "restore", &id.to_string(), "--best-effort"]
    );
    assert!(report.best_effort);
    assert_eq!(report.worktrees_recovered, 1);
    assert!(rig.db.get_session_by_id(id).unwrap().is_some());
    assert!(rig.db.get_deleted_session_by_id(id).unwrap().is_none());
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
    let err = super::restore_session_headless(&rig.db, id, false).unwrap_err();
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
