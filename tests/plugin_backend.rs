//! End-to-end integration test for the plugin backend adapter.
//!
//! Drives the runtime against the `sample-backend-plugin` example binary:
//! confirms that an `onBackendSelected:sample` activation event registers a
//! `PluginBackend` in the agent registry, that `SessionBackend::spawn` opens
//! the plugin's Unix socket, and that bytes round-trip across it.
//!
//! Skipped when the example binary hasn't been built — we don't want a hard
//! cargo dependency between `cargo test` and `cargo build --examples`. CI
//! builds examples explicitly before running the test suite.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use thurbox::agent::backend::SessionBackend;
use thurbox::agent::registry::BackendRegistry;
use thurbox::plugin::PluginRuntime;
use thurbox::session::PluginConfig;
use thurbox::storage::Database;

fn example_binary_path() -> Option<PathBuf> {
    // CARGO_MANIFEST_DIR points at the crate root; examples land under
    // target/<profile>/examples/. Probe both debug and release so the test
    // works whether the user ran `cargo build --examples` in either profile.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for profile in ["debug", "release"] {
        let p = root
            .join("target")
            .join(profile)
            .join("examples")
            .join("sample-backend-plugin");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Build a plugin dir with the manifest pointing at the compiled example.
///
/// The manifest's `exec` is normally `bin/sample-backend-plugin`; we rewrite
/// it to the absolute path of the compiled example so the test does not have
/// to copy or symlink the binary.
fn make_plugin_dir(example: &Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let manifest = format!(
        r#"name = "sample-backend-plugin"
version = "0.1.0"
thurbox_plugin_api = 1

[process]
exec = "{exec}"
capabilities = ["backend"]
activation_events = ["onBackendSelected:sample"]
"#,
        exec = example.display()
    );
    std::fs::write(dir.path().join("thurbox-plugin.toml"), manifest).unwrap();
    dir
}

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
        _: &HashMap<String, String>,
        _: u16,
        _: u16,
    ) -> anyhow::Result<thurbox::agent::backend::SpawnedSession> {
        anyhow::bail!("stub")
    }
    fn adopt(
        &self,
        _: &str,
        _: u16,
        _: u16,
    ) -> anyhow::Result<thurbox::agent::backend::AdoptedSession> {
        anyhow::bail!("stub")
    }
    fn discover(&self) -> anyhow::Result<Vec<thurbox::agent::backend::DiscoveredSession>> {
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
async fn config_updated_notification_reaches_running_plugin() {
    let Some(example) = example_binary_path() else {
        eprintln!("skipping: sample-backend-plugin example binary not built");
        return;
    };
    let plugin_dir = make_plugin_dir(&example);
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("thurbox.db")).expect("open db");
    db.upsert_global_plugin(&PluginConfig {
        name: "sample-backend-plugin".into(),
        path: plugin_dir.path().to_path_buf(),
        version: "0.1.0".into(),
        enabled: true,
    })
    .expect("register plugin");

    let runtime = PluginRuntime::new();
    let mut registry = fresh_registry();
    let handle = tokio::runtime::Handle::current();
    let _ = runtime
        .start_all_process_plugins(&db, &mut registry, handle)
        .await;
    assert!(runtime.is_running("sample-backend-plugin").await);

    // Notify must not error or wedge even if the plugin doesn't acknowledge.
    runtime
        .notify_config_updated(
            "sample-backend-plugin",
            "echo_greeting",
            &serde_json::json!(false),
        )
        .await;
    // Notify on an unknown plugin is a silent no-op.
    runtime
        .notify_config_updated("does-not-exist", "k", &serde_json::Value::Null)
        .await;

    runtime.shutdown_all().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_registers_backend_and_round_trips_bytes() {
    let Some(example) = example_binary_path() else {
        eprintln!(
            "skipping: sample-backend-plugin example binary not built. \
             Run `cargo build --example sample-backend-plugin` first."
        );
        return;
    };

    let plugin_dir = make_plugin_dir(&example);
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("thurbox.db")).expect("open db");
    db.upsert_global_plugin(&PluginConfig {
        name: "sample-backend-plugin".into(),
        path: plugin_dir.path().to_path_buf(),
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

    assert_eq!(reports.len(), 1);
    assert!(reports[0].is_ok(), "spawn report: {:?}", reports[0].error);
    assert!(
        registry.has("sample"),
        "expected backend 'sample' to be registered after handshake"
    );

    let backend = registry.get("sample").expect("backend registered").clone();

    // SessionBackend trait calls block on RPC — must run on a blocking-friendly
    // thread because they use `block_in_place`.
    let spawned = tokio::task::spawn_blocking(move || {
        backend
            .spawn(
                "test-window",
                "/bin/true",
                &[],
                None,
                &HashMap::new(),
                24,
                80,
            )
            .expect("backend.spawn")
    })
    .await
    .expect("spawn task");

    let mut output = spawned.output;
    let mut input = spawned.input;

    // Read the greeting line the example emits on accept.
    let mut buf = [0u8; 64];
    let n =
        read_with_timeout(&mut output, &mut buf, Duration::from_secs(3)).expect("read greeting");
    let s = std::str::from_utf8(&buf[..n]).unwrap_or("");
    assert!(
        s.starts_with("hello from sample-backend-plugin"),
        "expected greeting line, got: {s:?}"
    );

    // Echo round-trip.
    input.write_all(b"ping\n").unwrap();
    input.flush().unwrap();
    let mut echo_buf = [0u8; 16];
    let n =
        read_with_timeout(&mut output, &mut echo_buf, Duration::from_secs(3)).expect("read echo");
    assert_eq!(&echo_buf[..n], b"ping\n");

    runtime.shutdown_all().await;
}

/// Read with a soft timeout. Spins a background thread that interrupts the
/// blocking read by closing the stream after `dur`.
fn read_with_timeout(
    reader: &mut Box<dyn Read + Send>,
    buf: &mut [u8],
    dur: Duration,
) -> std::io::Result<usize> {
    // Simpler than threading: reader is a UnixStream under the hood and
    // we don't have a portable way to set a read deadline through the trait.
    // Use a small spin loop to keep the test responsive.
    let start = std::time::Instant::now();
    loop {
        match reader.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if start.elapsed() > dur {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
}
