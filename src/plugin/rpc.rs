//! Line-delimited JSON-RPC client over a child process's stdio.
//!
//! Wire shape:
//! - Request:  `{"id": N, "op": "<name>", "params": {...}}\n`
//! - Response: `{"id": N, "ok": true,  "result": {...}}\n`
//!   or `{"id": N, "ok": false, "error":  "..."}\n`
//!
//! The id correlates a request to its response. The plugin process is
//! expected to read one request per line on stdin and write one response
//! per line on stdout. Lines that fail to parse are logged and dropped —
//! a plugin's own debug prints belong on stderr (captured by [`super::output`]).
//!
//! [`RpcClient`] spawns one background reader task per child; calls park
//! on a `oneshot::Receiver` until that reader matches a response by id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{oneshot, Mutex};

/// Outbound request frame.
#[derive(Debug, Clone, Serialize)]
pub struct RpcRequest<'a> {
    pub id: u64,
    pub op: &'a str,
    pub params: Value,
}

/// Inbound response frame. Matches both ok and error shapes via flatten.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcResponse {
    pub id: u64,
    pub ok: bool,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Shared map from request id → pending oneshot sender.
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>>;

/// JSON-RPC client for a single plugin process.
///
/// Cheap to clone (`Arc` internally). Drop the last clone to disconnect:
/// in-flight calls will fail with [`RpcError::Disconnected`].
#[derive(Clone)]
pub struct RpcClient {
    next_id: Arc<AtomicU64>,
    pending: Pending,
    stdin: Arc<Mutex<ChildStdin>>,
}

impl RpcClient {
    /// Build a client and spawn a background task that reads responses
    /// from `stdout`, dispatching each to the matching pending call.
    pub fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let pending_for_reader = Arc::clone(&pending);
        tokio::spawn(reader_loop(stdout, pending_for_reader));
        Self {
            next_id: Arc::new(AtomicU64::new(1)),
            pending,
            stdin: Arc::new(Mutex::new(stdin)),
        }
    }

    /// Send `op` with `params` and await the matching response.
    ///
    /// `timeout` bounds how long we wait — a plugin that never replies should
    /// not wedge the runtime. On timeout the pending entry is cleared.
    pub async fn call(
        &self,
        op: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let frame = serde_json::to_vec(&RpcRequest { id, op, params })
            .map_err(|e| RpcError::Encode(e.to_string()))?;

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(&frame)
                .await
                .map_err(|e| RpcError::Io(e.to_string()))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|e| RpcError::Io(e.to_string()))?;
            stdin
                .flush()
                .await
                .map_err(|e| RpcError::Io(e.to_string()))?;
        }

        let response = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&id);
                return Err(RpcError::Disconnected);
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                return Err(RpcError::Timeout);
            }
        };

        if response.ok {
            Ok(response.result.unwrap_or(Value::Null))
        } else {
            Err(RpcError::Plugin(
                response.error.unwrap_or_else(|| "unknown".to_string()),
            ))
        }
    }

    /// Fire-and-forget notification — same wire shape but no response is awaited.
    /// The plugin may ignore it. Notifications still carry an id so a future
    /// protocol revision can switch them to request/response without breaking
    /// the framing.
    pub async fn notify(&self, op: &str, params: Value) -> Result<(), RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = serde_json::to_vec(&RpcRequest { id, op, params })
            .map_err(|e| RpcError::Encode(e.to_string()))?;
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(&frame)
            .await
            .map_err(|e| RpcError::Io(e.to_string()))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| RpcError::Io(e.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|e| RpcError::Io(e.to_string()))?;
        Ok(())
    }
}

async fn reader_loop(stdout: ChildStdout, pending: Pending) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<RpcResponse>(trimmed) {
                    Ok(resp) => {
                        if let Some(tx) = pending.lock().await.remove(&resp.id) {
                            let _ = tx.send(resp);
                        } else {
                            tracing::warn!(
                                id = resp.id,
                                "Plugin RPC response without pending call"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, line = %trimmed, "Plugin RPC: failed to parse line");
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "Plugin RPC reader IO error");
                break;
            }
        }
    }
}

/// Errors emitted by [`RpcClient::call`].
#[derive(Debug)]
pub enum RpcError {
    Encode(String),
    Io(String),
    Plugin(String),
    Disconnected,
    Timeout,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Encode(e) => write!(f, "failed to encode request: {e}"),
            RpcError::Io(e) => write!(f, "io error: {e}"),
            RpcError::Plugin(e) => write!(f, "plugin returned error: {e}"),
            RpcError::Disconnected => write!(f, "plugin disconnected before responding"),
            RpcError::Timeout => write!(f, "plugin did not respond within timeout"),
        }
    }
}

impl std::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_with_canonical_field_order() {
        let req = RpcRequest {
            id: 7,
            op: "handshake",
            params: serde_json::json!({"api_version": 1}),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"id\":7"));
        assert!(s.contains("\"op\":\"handshake\""));
        assert!(s.contains("\"api_version\":1"));
    }

    #[test]
    fn response_parses_ok_shape() {
        let r: RpcResponse =
            serde_json::from_str(r#"{"id":3,"ok":true,"result":{"v":1}}"#).unwrap();
        assert_eq!(r.id, 3);
        assert!(r.ok);
        assert_eq!(r.result.as_ref().unwrap()["v"], 1);
        assert!(r.error.is_none());
    }

    #[test]
    fn response_parses_error_shape() {
        let r: RpcResponse = serde_json::from_str(r#"{"id":4,"ok":false,"error":"boom"}"#).unwrap();
        assert_eq!(r.id, 4);
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("boom"));
        assert!(r.result.is_none());
    }

    #[test]
    fn response_rejects_missing_id() {
        let bad = serde_json::from_str::<RpcResponse>(r#"{"ok":true}"#);
        assert!(bad.is_err());
    }
}
