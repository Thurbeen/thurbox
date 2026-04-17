//! Typed proxies that turn `RpcClient` calls into capability-specific
//! operations against a single running plugin.
//!
//! Today there is one proxy — [`McpToolProxy`] for the `mcp-tools`
//! capability — but the file is named `proxy.rs` so the next capability
//! (e.g. `commands`, `autocomplete`) can add a sibling struct without
//! re-organizing.
//!
//! The underlying [`RpcClient`] is clone-friendly and tokio-based; the proxy
//! adds only three things on top of it:
//!
//! 1. The op name vocabulary for the capability (`mcp.list_tools`, `mcp.call`).
//! 2. An uncontroversial per-call timeout so a wedged plugin doesn't block
//!    `thurbox-mcp` indefinitely.
//! 3. Strongly-typed errors-as-strings at the module boundary, so the
//!    control-socket dispatcher can propagate them verbatim to the client.
//!
//! Compare with [`super::backend_adapter::PluginBackend`], which adapts the
//! same `RpcClient` into the synchronous `SessionBackend` trait. The backend
//! adapter needs typed per-op envelopes (`SpawnedSession`, `AdoptedSession`)
//! that don't generalize, which is why it lives in its own file.

use std::time::Duration;

use serde_json::{json, Value};

use super::rpc::RpcClient;

/// How long to wait for a single `mcp.*` call to the plugin.
///
/// Looser than the backend-adapter ceiling (`backend_adapter::RPC_TIMEOUT`
/// is 30s) because MCP tool calls can wrap network I/O, LLM round-trips,
/// or long-running external commands. Still bounded so a wedged plugin
/// can't hold the MCP request thread forever.
pub const MCP_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Adapter for a single running plugin that declared the `mcp-tools`
/// capability. Cheap to clone — `RpcClient` is already `Arc`'d internally.
#[derive(Clone)]
pub struct McpToolProxy {
    client: RpcClient,
    timeout: Duration,
}

impl McpToolProxy {
    pub fn new(client: RpcClient) -> Self {
        Self {
            client,
            timeout: MCP_CALL_TIMEOUT,
        }
    }

    /// Override the default timeout. Used by tests; production uses the default.
    #[cfg(test)]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Ask the plugin for its live tool list. Only used when the plugin's
    /// manifest has no `[[contributes.mcp_tools]]` rows — otherwise the host
    /// reads those statically and never wakes the plugin for discovery.
    pub async fn list_tools(&self) -> Result<Value, String> {
        self.client
            .call("mcp.list_tools", json!({}), self.timeout)
            .await
            .map_err(|e| e.to_string())
    }

    /// Invoke a tool on the plugin. `args` is passed through verbatim as the
    /// MCP-side tool schema defines.
    pub async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
        self.client
            .call(
                "mcp.call",
                json!({ "name": tool, "params": args }),
                self.timeout,
            )
            .await
            .map_err(|e| e.to_string())
    }
}
