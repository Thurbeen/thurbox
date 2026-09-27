//! A registered backend must own the lifecycle of its persisted sessions.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use anyhow::Result;
use thurbox::agent::backend::{AdoptedSession, DiscoveredSession, SpawnedSession};
use thurbox::agent::{BackendRegistry, SessionBackend};
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

struct ProbeBackend {
    name: &'static str,
    spawns: Arc<AtomicUsize>,
    kills: Arc<AtomicUsize>,
    panes: Mutex<Vec<DiscoveredSession>>,
}

impl ProbeBackend {
    fn new(name: &'static str, spawns: Arc<AtomicUsize>, kills: Arc<AtomicUsize>) -> Self {
        Self {
            name,
            spawns,
            kills,
            panes: Mutex::new(Vec::new()),
        }
    }
}

impl SessionBackend for ProbeBackend {
    fn name(&self) -> &str {
        self.name
    }
    fn check_available(&self) -> Result<()> {
        Ok(())
    }
    fn ensure_ready(&self) -> Result<()> {
        Ok(())
    }
    fn spawn(
        &self,
        window_name: &str,
        _: &str,
        _: &[String],
        _: Option<&Path>,
        _: &HashMap<String, String>,
        _: u16,
        _: u16,
    ) -> Result<SpawnedSession> {
        let next = self.spawns.fetch_add(1, Ordering::SeqCst) + 1;
        let backend_id = format!("probe-pane-{next}");
        self.panes.lock().unwrap().push(DiscoveredSession {
            backend_id: backend_id.clone(),
            name: window_name.to_string(),
            is_alive: true,
            session: String::new(),
            role: thurbox::agent::backend::WindowRole::Agent,
        });
        Ok(SpawnedSession {
            backend_id,
            output: Box::new(std::io::empty()),
            input: Box::new(std::io::sink()),
            size: None,
        })
    }
    fn spawn_headless(
        &self,
        session_id: &str,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Result<String> {
        let spawned = self.spawn(window_name, command, args, cwd, env, 24, 80)?;
        self.stamp_window(
            &spawned.backend_id,
            session_id,
            thurbox::agent::backend::WindowRole::Agent,
        )?;
        Ok(spawned.backend_id)
    }
    fn adopt(&self, _: &str, _: u16, _: u16, _: Option<Vec<u8>>) -> Result<AdoptedSession> {
        unreachable!()
    }
    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        Ok(self.panes.lock().unwrap().clone())
    }
    fn headless_discover(&self) -> Result<Vec<DiscoveredSession>> {
        self.discover()
    }
    fn stamp_window(
        &self,
        pane: &str,
        session: &str,
        _: thurbox::agent::backend::WindowRole,
    ) -> Result<()> {
        if let Some(found) = self
            .panes
            .lock()
            .unwrap()
            .iter_mut()
            .find(|p| p.backend_id == pane)
        {
            found.session = session.to_string();
        }
        Ok(())
    }
    fn resize(&self, _: &str, _: u16, _: u16) -> Result<()> {
        Ok(())
    }
    fn is_dead(&self, _: &str) -> Result<bool> {
        Ok(true)
    }
    fn kill(&self, pane: &str) -> Result<()> {
        self.kills.fetch_add(1, Ordering::SeqCst);
        self.panes.lock().unwrap().retain(|p| p.backend_id != pane);
        Ok(())
    }
    fn detach(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn pane_pid(&self, _: &str) -> Result<Option<u32>> {
        Ok(None)
    }
}

#[test]
fn registered_backend_receives_a_missing_session_relaunch() {
    let home = tempfile::tempdir().unwrap();
    thurbox::paths::set_test_dir(home.path());
    let config = thurbox::paths::config_file()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("agents.toml"),
        "default = \"probe\"\n\n[[agents]]\nname = \"probe\"\ncommand = \"sh\"\nargs = []\n",
    )
    .unwrap();
    let db = Database::open_in_memory().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let probe: Arc<dyn SessionBackend> = Arc::new(ProbeBackend::new(
        "local-probe",
        calls.clone(),
        Arc::new(AtomicUsize::new(0)),
    ));
    let mut registry = BackendRegistry::new(probe.clone());
    registry.register(probe);
    let id = SessionId::default();
    db.upsert_session(&SharedSession {
        id,
        name: "probe".into(),
        agent: "probe".into(),
        backend_id: "pane-1".into(),
        backend_type: "local-probe".into(),
        agent_session_id: Some(uuid::Uuid::new_v4().to_string()),
        cwd: None,
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    })
    .unwrap();

    let _ = thurbox::session_ops::restart::restart_session_headless_with_registry(
        &db, id, true, &registry,
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "registered backend never received relaunch"
    );
    assert_eq!(
        db.get_session_by_id(id).unwrap().unwrap().backend_id,
        "probe-pane-1"
    );
}

#[test]
fn mirror_keeps_a_registered_mux_route_and_the_hosts_pane_id() {
    let calls = Arc::new(AtomicUsize::new(0));
    let local: Arc<dyn SessionBackend> = Arc::new(ProbeBackend::new(
        "local-probe",
        calls.clone(),
        Arc::new(AtomicUsize::new(0)),
    ));
    let mut registry = BackendRegistry::new(local);
    registry.register(Arc::new(ProbeBackend::new(
        "ssh:example:probe",
        calls,
        Arc::new(AtomicUsize::new(0)),
    )));
    let id = SessionId::default();
    let row = thurbox::session_ops::mirror::session_from_json_with_registry(
        &serde_json::json!({
            "id": id.to_string(), "name": "probe", "backend_type": "local-probe",
            "backend_id": "opaque-pane"
        }),
        "ssh:example",
        &registry,
    )
    .unwrap();
    assert_eq!(row.session.backend_type, "ssh:example:probe");
    assert_eq!(row.session.backend_id, "opaque-pane");
}

#[test]
fn ambiguous_unstamped_panes_are_never_killed_or_relaunched() {
    let spawns = Arc::new(AtomicUsize::new(0));
    let kills = Arc::new(AtomicUsize::new(0));
    let backend = ProbeBackend::new("local-probe", spawns, kills.clone());
    backend
        .panes
        .lock()
        .unwrap()
        .extend(["pane-a", "pane-b"].map(|backend_id| DiscoveredSession {
            backend_id: backend_id.into(),
            name: "same-name".into(),
            is_alive: true,
            session: String::new(),
            role: thurbox::agent::backend::WindowRole::Agent,
        }));
    let live = backend.headless_liveness("session-a", "same-name").unwrap();
    assert_eq!(live, thurbox::agent::backend::BackendLiveness::Unknown);
    assert!(!live.permits_relaunch());
    assert!(!backend
        .kill_headless("session-a", "same-name", "pane-a", "")
        .unwrap());
    assert_eq!(kills.load(Ordering::SeqCst), 0);
}

#[test]
fn create_and_force_delete_use_the_registered_backend() {
    let home = tempfile::tempdir().unwrap();
    thurbox::paths::set_test_dir(home.path());
    let config = thurbox::paths::config_file()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("agents.toml"),
        "default = \"probe\"\n\n[[agents]]\nname = \"probe\"\ncommand = \"sh\"\nargs = []\n",
    )
    .unwrap();
    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args).current_dir(repo.path());
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_PREFIX",
            "GIT_NAMESPACE",
        ] {
            cmd.env_remove(var);
        }
        assert!(cmd.status().unwrap().success());
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.path().join("README.md"), "probe\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    let spawns = Arc::new(AtomicUsize::new(0));
    let kills = Arc::new(AtomicUsize::new(0));
    let backend: Arc<dyn SessionBackend> = Arc::new(ProbeBackend::new(
        "local-tmux",
        spawns.clone(),
        kills.clone(),
    ));
    let registry = BackendRegistry::new(backend);
    let db = Database::open_in_memory().unwrap();
    let created = thurbox::session_ops::spawn::spawn_session_headless_with_registry(
        &db,
        thurbox::session_ops::SpawnRequest {
            name: "probe".into(),
            repo_path: repo.path().to_path_buf(),
            worktree_branch: None,
            base_branch: None,
            existing_worktree: None,
            agent: Some("probe".into()),
            command: None,
            args: Vec::new(),
            env: Default::default(),
            resume_session_id: None,
            agent_session_id: None,
            host: None,
            multiplexer: None,
            parent_session_id: None,
            task_id: None,
            extra_repos: Vec::new(),
            fork_session_id: None,
            inherit_worktrees: Vec::new(),
        },
        None,
        &registry,
    )
    .unwrap();
    assert_eq!(created.backend_id, "probe-pane-1");
    assert_eq!(spawns.load(Ordering::SeqCst), 1);
    registry
        .get("local-tmux")
        .unwrap()
        .kill("probe-pane-1")
        .unwrap();
    assert_eq!(kills.load(Ordering::SeqCst), 1);
    thurbox::session_ops::restart::restart_session_headless_with_registry(
        &db,
        created.session_id,
        true,
        &registry,
    )
    .unwrap();
    assert_eq!(spawns.load(Ordering::SeqCst), 2);
    assert_eq!(kills.load(Ordering::SeqCst), 1);
    assert_eq!(
        db.get_session_by_id(created.session_id)
            .unwrap()
            .unwrap()
            .backend_id,
        "probe-pane-2"
    );
    let deleted = thurbox::session_ops::delete::delete_session_headless_with_registry(
        &db,
        created.session_id,
        true,
        &registry,
    )
    .unwrap();
    assert!(deleted.killed_window);
    assert_eq!(kills.load(Ordering::SeqCst), 2);
    let restored = thurbox::session_ops::restore::restore_session_headless_with_registry(
        &db,
        created.session_id,
        true,
        &registry,
    )
    .unwrap();
    assert!(
        restored.respawn_error.is_none(),
        "{:?}",
        restored.respawn_error
    );
    assert_eq!(spawns.load(Ordering::SeqCst), 3);
    assert_eq!(
        db.get_session_by_id(created.session_id)
            .unwrap()
            .unwrap()
            .backend_id,
        "probe-pane-3"
    );
}
