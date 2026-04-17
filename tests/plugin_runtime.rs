//! Integration test for the process-plugin runtime.
//!
//! Drives the runtime end-to-end against the `sample-process-plugin` fixture:
//! discovery from disk, manifest load, spawn, handshake, log capture, and
//! graceful `stop`. Mirrors the scenario operators will hit when they install
//! a real plugin under `~/.local/share/thurbox/admin/plugins/`.

use std::path::PathBuf;
use std::sync::Arc;

use thurbox::agent::backend::{AdoptedSession, DiscoveredSession, SessionBackend, SpawnedSession};
use thurbox::agent::registry::BackendRegistry;
use thurbox::plugin::{activation::FiredEvent, PluginRuntime};
use thurbox::session::PluginConfig;
use thurbox::storage::Database;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/sample-process-plugin")
}

/// Stub backend used as the registry default in plugin tests.
struct DefaultStub;

impl SessionBackend for DefaultStub {
    fn name(&self) -> &str {
        "stub-default"
    }
    fn check_available(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn ensure_ready(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn spawn(
        &self,
        _: &str,
        _: &str,
        _: &[String],
        _: Option<&std::path::Path>,
        _: &std::collections::HashMap<String, String>,
        _: u16,
        _: u16,
    ) -> anyhow::Result<SpawnedSession> {
        anyhow::bail!("stub")
    }
    fn adopt(&self, _: &str, _: u16, _: u16) -> anyhow::Result<AdoptedSession> {
        anyhow::bail!("stub")
    }
    fn discover(&self) -> anyhow::Result<Vec<DiscoveredSession>> {
        Ok(vec![])
    }
    fn resize(&self, _: &str, _: u16, _: u16) -> anyhow::Result<()> {
        Ok(())
    }
    fn is_dead(&self, _: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    fn kill(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn detach(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn pane_pid(&self, _: &str) -> anyhow::Result<Option<u32>> {
        Ok(None)
    }
}

fn fresh_registry() -> BackendRegistry {
    let stub: Arc<dyn SessionBackend> = Arc::new(DefaultStub);
    BackendRegistry::new(stub)
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_spawns_handshakes_and_stops_sample_plugin() {
    let fixture = fixture_path();
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("thurbox.db")).expect("open db");

    db.upsert_global_plugin(&PluginConfig {
        name: "sample-process-plugin".into(),
        path: fixture.clone(),
        version: "0.1.0".into(),
        enabled: true,
    })
    .expect("register plugin");

    let runtime = PluginRuntime::new();
    let mut registry = fresh_registry();
    let handle = tokio::runtime::Handle::current();
    let reports = runtime
        .start_all_process_plugins(&db, &mut registry, handle)
        .await;

    assert_eq!(reports.len(), 1, "one plugin should match");
    assert!(reports[0].is_ok(), "spawn failed: {:?}", reports[0].error);
    assert_eq!(reports[0].name, "sample-process-plugin");
    assert!(runtime.is_running("sample-process-plugin").await);

    runtime.shutdown_all().await;
    assert!(runtime.is_empty().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_does_not_respawn_already_running_plugin() {
    let fixture = fixture_path();
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("thurbox.db")).expect("open db");

    db.upsert_global_plugin(&PluginConfig {
        name: "sample-process-plugin".into(),
        path: fixture,
        version: "0.1.0".into(),
        enabled: true,
    })
    .expect("register plugin");

    let runtime = PluginRuntime::new();
    let mut registry = fresh_registry();
    let handle = tokio::runtime::Handle::current();
    let _ = runtime
        .start_all_process_plugins(&db, &mut registry, handle.clone())
        .await;
    assert_eq!(runtime.len().await, 1);

    // Second pass: already running, should be a no-op.
    let second = runtime
        .start_all_process_plugins(&db, &mut registry, handle)
        .await;
    assert!(second.is_empty(), "should not respawn: {second:?}");
    assert_eq!(runtime.len().await, 1);

    runtime.shutdown_all().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_plugin_does_not_spawn() {
    let fixture = fixture_path();
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("thurbox.db")).expect("open db");

    db.upsert_global_plugin(&PluginConfig {
        name: "sample-process-plugin".into(),
        path: fixture,
        version: "0.1.0".into(),
        enabled: false,
    })
    .expect("register plugin");

    let runtime = PluginRuntime::new();
    let mut registry = fresh_registry();
    let handle = tokio::runtime::Handle::current();
    let reports = runtime
        .start_all_process_plugins(&db, &mut registry, handle)
        .await;
    assert!(reports.is_empty(), "disabled plugin spawned: {reports:?}");
    assert!(runtime.is_empty().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn non_matching_event_does_not_spawn() {
    let fixture = fixture_path();
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("thurbox.db")).expect("open db");

    db.upsert_global_plugin(&PluginConfig {
        name: "sample-process-plugin".into(),
        path: fixture,
        version: "0.1.0".into(),
        enabled: true,
    })
    .expect("register plugin");

    let runtime = PluginRuntime::new();
    let reports = runtime
        .activate_event(&db, &FiredEvent::BackendSelected("nonexistent"))
        .await;
    assert!(
        reports.is_empty(),
        "wrong event spawned plugin: {reports:?}"
    );
    assert!(runtime.is_empty().await);
}
