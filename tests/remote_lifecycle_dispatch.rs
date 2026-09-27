//! A configured remote suffix reaches its registered backend for direct delete.
//! One test per binary keeps the process-wide host cache scoped to this fixture.

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

struct RemoteProbe {
    name: &'static str,
    pane: Mutex<Option<DiscoveredSession>>,
    kills: Arc<AtomicUsize>,
}

impl SessionBackend for RemoteProbe {
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
        _: &str,
        _: &str,
        _: &[String],
        _: Option<&Path>,
        _: &HashMap<String, String>,
        _: u16,
        _: u16,
    ) -> Result<SpawnedSession> {
        unreachable!()
    }
    fn spawn_headless(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[String],
        _: Option<&Path>,
        _: &HashMap<String, String>,
    ) -> Result<String> {
        anyhow::bail!("this probe cannot spawn")
    }
    fn adopt(&self, _: &str, _: u16, _: u16, _: Option<Vec<u8>>) -> Result<AdoptedSession> {
        unreachable!()
    }
    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        Ok(self.pane.lock().unwrap().iter().cloned().collect())
    }
    fn headless_discover(&self) -> Result<Vec<DiscoveredSession>> {
        self.discover()
    }
    fn stamp_window(&self, _: &str, _: &str, _: thurbox::agent::backend::WindowRole) -> Result<()> {
        Ok(())
    }
    fn resize(&self, _: &str, _: u16, _: u16) -> Result<()> {
        Ok(())
    }
    fn is_dead(&self, _: &str) -> Result<bool> {
        Ok(false)
    }
    fn kill(&self, _: &str) -> Result<()> {
        self.kills.fetch_add(1, Ordering::SeqCst);
        *self.pane.lock().unwrap() = None;
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
fn a_registered_remote_suffix_receives_direct_force_delete() {
    let home = tempfile::tempdir().unwrap();
    thurbox::paths::set_test_dir(home.path());
    let config = thurbox::paths::config_file()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("hosts.toml"),
        "[[hosts]]\nname = 'example'\ndestination = 'unused'\nshare_sessions = false\n",
    )
    .unwrap();

    let id = SessionId::default();
    let kills = Arc::new(AtomicUsize::new(0));
    let local: Arc<dyn SessionBackend> = Arc::new(RemoteProbe {
        name: "local-tmux",
        pane: Mutex::new(None),
        kills: Arc::new(AtomicUsize::new(0)),
    });
    let remote: Arc<dyn SessionBackend> = Arc::new(RemoteProbe {
        name: "ssh:example:herdr",
        pane: Mutex::new(Some(DiscoveredSession {
            backend_id: "opaque-pane".into(),
            name: "probe".into(),
            is_alive: true,
            session: id.to_string(),
            role: thurbox::agent::backend::WindowRole::Agent,
        })),
        kills: kills.clone(),
    });
    let mut registry = BackendRegistry::new(local);
    registry.register(remote);
    let db = Database::open_in_memory().unwrap();
    db.upsert_session(&SharedSession {
        id,
        name: "probe".into(),
        agent: "probe".into(),
        backend_id: "opaque-pane".into(),
        backend_type: "ssh:example:herdr".into(),
        agent_session_id: None,
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

    let report = thurbox::session_ops::delete::delete_session_headless_with_registry(
        &db, id, true, &registry,
    )
    .unwrap();
    assert!(report.killed_window);
    assert_eq!(kills.load(Ordering::SeqCst), 1);
}
