//! Pure op dispatcher for the control socket.
//!
//! Matches on the op string (one of the constants from
//! [`crate::plugin_bridge::wire`]) and returns the typed response body. The
//! caller — the listener task — wraps the result in a
//! [`crate::plugin_bridge::wire::Response`] frame and writes it back to the
//! client.
//!
//! Three ops today:
//!
//! - `plugin.list` — registry read: currently running plugins that declared
//!   `capabilities = ["mcp-tools"]`. No activation, no side effects.
//!
//! - `plugin.list_tools` — prefer the manifest's `[[contributes.mcp_tools]]`
//!   rows so we don't wake dormant plugins for discovery. Fall back to
//!   asking a running plugin's `mcp.list_tools` op when the manifest declares
//!   none.
//!
//! - `plugin.call` — proxy through `mcp.call`. The plugin must already be
//!   running (typically via `onStartupFinished`); lazy `OnCommand` activation
//!   in the control-socket path is a follow-up (see issue tracker). Gated by
//!   the [`super::super::proxy::MCP_CALL_TIMEOUT`] on the proxy side.
//!
//! ## Why [`DispatchCtx`] instead of `&Database`
//!
//! `rusqlite::Connection` is `!Sync`, so `&Database` is not `Send`. A
//! spawned per-connection task on a multi-threaded tokio runtime can't hold
//! `&Database` across an `.await`. Wrapping the DB in
//! [`Arc<std::sync::Mutex<Database>>`] makes the context `Send + Sync`; we
//! lock synchronously inside the sync paths and never hold the guard across
//! an await.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::plugin_bridge::wire::{OP_PLUGIN_CALL, OP_PLUGIN_LIST, OP_PLUGIN_LIST_TOOLS};
use crate::session::PluginManifest;
use crate::storage::Database;

use super::super::PluginRuntime;

/// Dependencies the dispatcher needs. Constructed once by the listener task
/// and shared across connections. Cheap to clone.
#[derive(Clone)]
pub struct DispatchCtx {
    db: Arc<Mutex<Database>>,
}

impl DispatchCtx {
    pub fn new(db: Database) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
        }
    }

    /// Test-only constructor that accepts a pre-wrapped DB.
    #[cfg(test)]
    pub fn from_arc(db: Arc<Mutex<Database>>) -> Self {
        Self { db }
    }

    /// Read `[[contributes.mcp_tools]]` from `<plugin>/plugin.toml`. Locks the
    /// DB briefly, drops the guard before returning — safe to call from async
    /// code.
    fn static_manifest_tools(&self, plugin_name: &str) -> Result<Option<Vec<Value>>, String> {
        let db = self
            .db
            .lock()
            .map_err(|_| "control-socket DB mutex poisoned".to_string())?;
        let effective = db
            .list_effective_plugins()
            .map_err(|e| format!("failed to list plugins: {e}"))?;
        drop(db);

        let (plugin, _src) = effective
            .into_iter()
            .find(|(p, _)| p.name == plugin_name)
            .ok_or_else(|| format!("unknown plugin: {plugin_name}"))?;

        let manifest = PluginManifest::load(&plugin.path)
            .map_err(|e| format!("failed to load manifest for '{plugin_name}': {e}"))?;

        if manifest.contributes.mcp_tools.is_empty() {
            return Ok(None);
        }

        let rows = manifest
            .contributes
            .mcp_tools
            .into_iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();
        Ok(Some(rows))
    }
}

/// Dispatch one op to the runtime. Infallible at the framing level — every
/// path returns either an `Ok(Value)` body or an `Err(String)` message the
/// listener will forward verbatim in the `error` field of a wire response.
pub async fn dispatch(
    runtime: &PluginRuntime,
    ctx: &DispatchCtx,
    op: &str,
    params: Value,
) -> Result<Value, String> {
    match op {
        OP_PLUGIN_LIST => list_plugins(runtime).await,
        OP_PLUGIN_LIST_TOOLS => list_tools(runtime, ctx, params).await,
        OP_PLUGIN_CALL => call_tool(runtime, params).await,
        other => Err(format!("unknown op: {other}")),
    }
}

async fn list_plugins(runtime: &PluginRuntime) -> Result<Value, String> {
    let names = runtime.list_mcp_tools_plugins().await;
    Ok(json!({ "plugins": names }))
}

async fn list_tools(
    runtime: &PluginRuntime,
    ctx: &DispatchCtx,
    params: Value,
) -> Result<Value, String> {
    let plugin_name = params
        .get("plugin")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing string field 'plugin'".to_string())?;

    // Prefer the static declarations in the manifest — they let us answer
    // discovery for dormant plugins without spawning them.
    if let Some(static_tools) = ctx.static_manifest_tools(plugin_name)? {
        return Ok(json!({ "tools": static_tools, "source": "manifest" }));
    }

    // No static rows: fall back to the running plugin.
    let proxy = runtime.mcp_proxy(plugin_name).await.ok_or_else(|| {
        format!("plugin '{plugin_name}' is not running with mcp-tools capability")
    })?;
    let live = proxy.list_tools().await?;
    Ok(json!({ "tools": live, "source": "runtime" }))
}

async fn call_tool(runtime: &PluginRuntime, params: Value) -> Result<Value, String> {
    let plugin_name = params
        .get("plugin")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing string field 'plugin'".to_string())?;
    let tool = params
        .get("tool")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing string field 'tool'".to_string())?;
    let args = params
        .get("args")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));

    let proxy = runtime.mcp_proxy(plugin_name).await.ok_or_else(|| {
        format!("plugin '{plugin_name}' is not running with mcp-tools capability")
    })?;
    proxy.call(tool, args).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> (DispatchCtx, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.db")).unwrap();
        (DispatchCtx::new(db), dir)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_plugins_on_empty_runtime_returns_empty_array() {
        let runtime = PluginRuntime::new();
        let (ctx, _dir) = ctx();
        let result = dispatch(&runtime, &ctx, OP_PLUGIN_LIST, json!({}))
            .await
            .unwrap();
        assert_eq!(result, json!({ "plugins": [] }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_op_returns_error_string() {
        let runtime = PluginRuntime::new();
        let (ctx, _dir) = ctx();
        let err = dispatch(&runtime, &ctx, "plugin.bogus", json!({}))
            .await
            .unwrap_err();
        assert!(err.contains("unknown op"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_tools_requires_plugin_field() {
        let runtime = PluginRuntime::new();
        let (ctx, _dir) = ctx();
        let err = dispatch(&runtime, &ctx, OP_PLUGIN_LIST_TOOLS, json!({}))
            .await
            .unwrap_err();
        assert!(err.contains("plugin"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn call_tool_requires_plugin_and_tool_fields() {
        let runtime = PluginRuntime::new();
        let (ctx, _dir) = ctx();

        let err = dispatch(&runtime, &ctx, OP_PLUGIN_CALL, json!({ "tool": "t" }))
            .await
            .unwrap_err();
        assert!(err.contains("plugin"), "got: {err}");

        let err = dispatch(&runtime, &ctx, OP_PLUGIN_CALL, json!({ "plugin": "p" }))
            .await
            .unwrap_err();
        assert!(err.contains("tool"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_tools_for_unknown_plugin_errors() {
        let runtime = PluginRuntime::new();
        let (ctx, _dir) = ctx();
        let err = dispatch(
            &runtime,
            &ctx,
            OP_PLUGIN_LIST_TOOLS,
            json!({ "plugin": "ghost" }),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("ghost") || err.contains("unknown plugin"),
            "got: {err}"
        );
    }
}
