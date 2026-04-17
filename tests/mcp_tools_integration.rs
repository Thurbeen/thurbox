//! End-to-end integration test for the `mcp-tools` plugin capability.
//!
//! Spawns the `sample-mcp-plugin` example as a real process plugin, binds
//! the control-socket listener, and drives it through the `ControlClient`
//! — exercising the whole stack: manifest discovery, process spawn + handshake,
//! control-socket framing, dispatch routing, and `McpToolProxy` timeouts.
//!
//! Skipped when the example binary hasn't been built (same rule as
//! `tests/plugin_backend.rs`). CI builds examples explicitly before running
//! the test suite.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use thurbox::agent::backend::SessionBackend;
use thurbox::agent::registry::BackendRegistry;
use thurbox::plugin::control_socket;
use thurbox::plugin::PluginRuntime;
use thurbox::plugin_bridge::ControlClient;
use thurbox::session::PluginConfig;
use thurbox::storage::Database;

fn example_binary_path() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for profile in ["debug", "release"] {
        let p = root
            .join("target")
            .join(profile)
            .join("examples")
            .join("sample-mcp-plugin");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn make_plugin_dir(example: &Path, include_static_tools: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let static_tools = if include_static_tools {
        r#"
[[contributes.mcp_tools]]
name = "echo"
description = "Echo back the input."
input_schema = { type = "object" }
"#
    } else {
        ""
    };
    let manifest = format!(
        r#"name = "sample-mcp-plugin"
version = "0.1.0"
thurbox_plugin_api = 1

[process]
exec = "{exec}"
capabilities = ["mcp-tools"]
activation_events = ["onStartupFinished"]
{static_tools}"#,
        exec = example.display(),
        static_tools = static_tools
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
        _: &std::collections::HashMap<String, String>,
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

async fn spin_up_plugin(
    plugin_dir: &Path,
) -> (
    Arc<PluginRuntime>,
    ControlClient,
    control_socket::ControlSocketHandle,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("thurbox.db")).unwrap();
    db.upsert_global_plugin(&PluginConfig {
        name: "sample-mcp-plugin".into(),
        path: plugin_dir.to_path_buf(),
        version: "0.1.0".into(),
        enabled: true,
    })
    .unwrap();

    let runtime = Arc::new(PluginRuntime::new());
    let mut registry = fresh_registry();
    let handle = tokio::runtime::Handle::current();
    let reports = runtime
        .start_all_process_plugins(&db, &mut registry, handle)
        .await;
    for r in &reports {
        if let Some(e) = &r.error {
            panic!("plugin spawn failed: {} — {e}", r.name);
        }
    }
    assert!(runtime.is_running("sample-mcp-plugin").await);

    let socket_path = tmp.path().join("control.sock");
    let listener_db = Database::open(&tmp.path().join("thurbox.db")).unwrap();
    let socket_handle = control_socket::spawn(Arc::clone(&runtime), listener_db, socket_path.clone())
        .await
        .unwrap();
    let client = ControlClient::new(socket_path);

    (runtime, client, socket_handle, tmp)
}

#[tokio::test(flavor = "multi_thread")]
async fn list_plugins_reports_the_running_mcp_tools_plugin() {
    let Some(example) = example_binary_path() else {
        eprintln!("skipping: sample-mcp-plugin example binary not built");
        return;
    };
    let plugin_dir = make_plugin_dir(&example, false);
    let (runtime, client, _socket, _tmp) = spin_up_plugin(plugin_dir.path()).await;

    let plugins = client.list_plugins().await.unwrap();
    assert_eq!(plugins, vec!["sample-mcp-plugin"]);

    runtime.shutdown_all().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn list_plugin_tools_reads_static_manifest_without_rpc() {
    let Some(example) = example_binary_path() else {
        eprintln!("skipping: sample-mcp-plugin example binary not built");
        return;
    };
    let plugin_dir = make_plugin_dir(&example, true);
    let (runtime, client, _socket, _tmp) = spin_up_plugin(plugin_dir.path()).await;

    let tools = client.list_plugin_tools("sample-mcp-plugin").await.unwrap();
    assert_eq!(tools["source"], "manifest");
    let arr = tools["tools"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "echo");

    runtime.shutdown_all().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn list_plugin_tools_falls_back_to_runtime_when_manifest_is_empty() {
    let Some(example) = example_binary_path() else {
        eprintln!("skipping: sample-mcp-plugin example binary not built");
        return;
    };
    let plugin_dir = make_plugin_dir(&example, false);
    let (runtime, client, _socket, _tmp) = spin_up_plugin(plugin_dir.path()).await;

    let tools = client.list_plugin_tools("sample-mcp-plugin").await.unwrap();
    assert_eq!(tools["source"], "runtime");
    let arr = tools["tools"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "echo");

    runtime.shutdown_all().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn call_plugin_tool_round_trips_through_the_plugin() {
    let Some(example) = example_binary_path() else {
        eprintln!("skipping: sample-mcp-plugin example binary not built");
        return;
    };
    let plugin_dir = make_plugin_dir(&example, true);
    let (runtime, client, _socket, _tmp) = spin_up_plugin(plugin_dir.path()).await;

    let result = client
        .call_plugin_tool(
            "sample-mcp-plugin",
            "echo",
            serde_json::json!({ "hello": "world" }),
        )
        .await
        .unwrap();
    assert_eq!(result, serde_json::json!({ "echoed": { "hello": "world" } }));

    runtime.shutdown_all().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn call_plugin_tool_surfaces_plugin_error_for_unknown_tool() {
    let Some(example) = example_binary_path() else {
        eprintln!("skipping: sample-mcp-plugin example binary not built");
        return;
    };
    let plugin_dir = make_plugin_dir(&example, true);
    let (runtime, client, _socket, _tmp) = spin_up_plugin(plugin_dir.path()).await;

    let err = client
        .call_plugin_tool("sample-mcp-plugin", "bogus-tool", serde_json::json!({}))
        .await
        .unwrap_err();
    match err {
        thurbox::plugin_bridge::ControlError::Server(msg) => {
            assert!(msg.contains("bogus-tool") || msg.contains("unknown tool"), "got: {msg}")
        }
        other => panic!("expected Server error, got {other:?}"),
    }

    runtime.shutdown_all().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn call_plugin_tool_errors_when_plugin_not_running() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("control.sock");
    let db = Database::open(&tmp.path().join("thurbox.db")).unwrap();
    let runtime = Arc::new(PluginRuntime::new());
    let _handle = control_socket::spawn(Arc::clone(&runtime), db, socket_path.clone())
        .await
        .unwrap();

    let client = ControlClient::new(socket_path);
    let err = client
        .call_plugin_tool("never-spawned", "echo", serde_json::json!({}))
        .await
        .unwrap_err();
    match err {
        thurbox::plugin_bridge::ControlError::Server(msg) => {
            assert!(
                msg.contains("never-spawned") && msg.contains("not running"),
                "got: {msg}"
            )
        }
        other => panic!("expected Server error, got {other:?}"),
    }
}
