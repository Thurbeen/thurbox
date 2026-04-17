//! `thurbox-mcp`-side client for the TUI's plugin control socket.
//!
//! One-request-per-connection, line-delimited JSON. Short-lived TCP-style
//! connections — no multiplexing, no connection pool. Plugins are a low-volume
//! surface (tool calls, not streaming), and opening a Unix socket is cheap
//! enough that a pool would be premature complexity.
//!
//! ## Why a short-lived connection per call
//!
//! - Every response is uniquely owned by one call — no id-to-future
//!   routing table, no shared state between concurrent callers.
//! - Connection cleanup is automatic on `Drop` at the end of `call`.
//! - "TUI not running" surfaces as a single, unambiguous error: connect
//!   refused or path missing. The MCP server degrades to "plugin tools
//!   unavailable" instead of hanging on a stale long-lived socket.
//!
//! ## Why this sits in `plugin_bridge` and not `mcp`
//!
//! The client and the TUI's listener share the same wire constants
//! ([`super::wire`]). Keeping both sides in one sealed module lets us rename
//! or extend an op and get a compile error on every mismatched call site. If
//! the client lived in `mcp`, the `mcp` module would indirectly depend on
//! plugin-runtime types — which violates the mcp-module isolation rule.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::wire::{Request, Response, OP_PLUGIN_CALL, OP_PLUGIN_LIST, OP_PLUGIN_LIST_TOOLS};

/// Errors surfaced by [`ControlClient`]. Deliberately a flat enum — callers
/// usually want to either succeed or convert to an MCP error string.
#[derive(Debug)]
pub enum ControlError {
    /// The socket path doesn't exist. Typically "TUI isn't running".
    NotAvailable(String),
    /// The socket exists but connecting/reading/writing failed.
    Io(std::io::Error),
    /// Decoding the response frame failed.
    Decode(String),
    /// The request framed fine but the listener returned an error body.
    Server(String),
    /// The server's response shape didn't match what we asked for.
    UnexpectedShape(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::NotAvailable(p) => {
                write!(f, "thurbox control socket not available at {p}")
            }
            ControlError::Io(e) => write!(f, "control socket I/O: {e}"),
            ControlError::Decode(e) => write!(f, "control socket decode error: {e}"),
            ControlError::Server(e) => write!(f, "{e}"),
            ControlError::UnexpectedShape(e) => {
                write!(f, "control socket unexpected response shape: {e}")
            }
        }
    }
}

impl std::error::Error for ControlError {}

impl From<std::io::Error> for ControlError {
    fn from(e: std::io::Error) -> Self {
        ControlError::Io(e)
    }
}

/// Stateless client that resolves to a Unix socket path at call time.
///
/// Cheap to clone (just a `PathBuf`). Construct once per MCP server process
/// and share across tool handlers.
#[derive(Debug, Clone)]
pub struct ControlClient {
    socket_path: PathBuf,
    next_id: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl ControlClient {
    /// Construct a client pointed at the given socket path. No I/O happens
    /// until the first `call`.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            next_id: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }

    /// The socket path this client will connect to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Low-level: send one op, receive one response body (or error string).
    ///
    /// Every higher-level method is a thin wrapper that decodes the result
    /// into a typed shape.
    pub async fn call(&self, op: &str, params: Value) -> Result<Value, ControlError> {
        if !self.socket_path.exists() {
            return Err(ControlError::NotAvailable(
                self.socket_path.display().to_string(),
            ));
        }

        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req = Request {
            id,
            op: op.to_string(),
            params,
        };
        let mut buf = serde_json::to_vec(&req)
            .map_err(|e| ControlError::Decode(format!("request encode: {e}")))?;
        buf.push(b'\n');

        let stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                    ControlError::NotAvailable(self.socket_path.display().to_string())
                }
                _ => ControlError::Io(e),
            })?;
        let (reader, mut writer) = stream.into_split();
        writer.write_all(&buf).await?;
        writer.flush().await?;

        let mut lines = BufReader::new(reader).lines();
        let line = lines.next_line().await?.ok_or_else(|| {
            ControlError::Io(std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
        })?;

        let response: Response = serde_json::from_str(&line)
            .map_err(|e| ControlError::Decode(format!("response decode: {e}")))?;

        if response.id != id {
            return Err(ControlError::UnexpectedShape(format!(
                "id mismatch: expected {id}, got {}",
                response.id
            )));
        }

        if !response.ok {
            return Err(ControlError::Server(
                response
                    .error
                    .unwrap_or_else(|| "unknown server error".into()),
            ));
        }
        Ok(response.result.unwrap_or(Value::Null))
    }

    /// Names of running plugins that declared `capabilities = ["mcp-tools"]`.
    pub async fn list_plugins(&self) -> Result<Vec<String>, ControlError> {
        let v = self.call(OP_PLUGIN_LIST, json!({})).await?;
        let arr = v
            .get("plugins")
            .and_then(Value::as_array)
            .ok_or_else(|| ControlError::UnexpectedShape("missing 'plugins' array".into()))?;
        arr.iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| ControlError::UnexpectedShape("plugin name not a string".into()))
            })
            .collect()
    }

    /// Ask a plugin for the tools it exposes. Returns the raw JSON the
    /// listener produced (`{ "tools": ..., "source": "manifest"|"runtime" }`)
    /// so the MCP handler can surface the source in its response without a
    /// second round-trip.
    pub async fn list_plugin_tools(&self, plugin: &str) -> Result<Value, ControlError> {
        self.call(OP_PLUGIN_LIST_TOOLS, json!({ "plugin": plugin }))
            .await
    }

    /// Invoke a tool on a plugin. `args` is forwarded verbatim.
    pub async fn call_plugin_tool(
        &self,
        plugin: &str,
        tool: &str,
        args: Value,
    ) -> Result<Value, ControlError> {
        self.call(
            OP_PLUGIN_CALL,
            json!({ "plugin": plugin, "tool": tool, "args": args }),
        )
        .await
    }
}

/// Construct a client pointed at the default socket path for the current
/// user (under `$XDG_RUNTIME_DIR`). Returns `None` if the runtime directory
/// can't be resolved — e.g. `$XDG_RUNTIME_DIR` unset and no fallback user.
pub fn default_client() -> Option<ControlClient> {
    crate::paths::control_socket_path().map(ControlClient::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_socket_returns_not_available() {
        let tmp = tempfile::tempdir().unwrap();
        let client = ControlClient::new(tmp.path().join("does-not-exist.sock"));
        let err = client.list_plugins().await.unwrap_err();
        assert!(
            matches!(err, ControlError::NotAvailable(_)),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn id_increments_across_calls() {
        let client = ControlClient::new("/tmp/unused.sock");
        let a = client
            .next_id
            .fetch_add(0, std::sync::atomic::Ordering::Relaxed);
        let _ = client
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let b = client
            .next_id
            .fetch_add(0, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(b, a + 1);
    }
}
