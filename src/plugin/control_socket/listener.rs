//! Unix-domain-socket listener that exposes [`dispatch`] to `thurbox-mcp`.
//!
//! The socket path is the one [`crate::paths::control_socket_path`] returns —
//! deterministic per-user under `$XDG_RUNTIME_DIR`. Only the current user has
//! file-mode access (0o600 on the socket node itself).
//!
//! Framing is newline-delimited JSON: each line a
//! [`crate::plugin_bridge::wire::Request`], each response line a
//! [`crate::plugin_bridge::wire::Response`]. Identical framing to the
//! plugin-stdio RPC — one protocol for every cross-process hop in thurbox.
//!
//! The listener task panics nowhere and logs on every error path; a wedged
//! client cannot take down the TUI. Dropping the returned
//! [`ControlSocketHandle`] aborts the listener task and removes the socket
//! file — clean up is Drop-driven so main doesn't have to remember to call
//! a shutdown helper.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

use crate::plugin_bridge::wire::{Request, Response};
use crate::storage::Database;

use super::super::PluginRuntime;
use super::dispatch::{self, DispatchCtx};

/// Owns the listener task and the socket-file cleanup. Drop to stop.
pub struct ControlSocketHandle {
    task: Option<JoinHandle<()>>,
    socket_path: PathBuf,
}

impl ControlSocketHandle {
    /// The path the socket is bound to. Exposed for logging only.
    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }
}

impl Drop for ControlSocketHandle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        // Best-effort — if the path was never bound the remove is a no-op.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Bind the socket and start accepting connections.
///
/// Returns an error only on the initial bind; per-connection failures are
/// logged inside the listener task. If a stale socket file is present from a
/// previous crash the function removes it before binding — we own the path,
/// so reclaiming it is safe.
pub async fn spawn(
    runtime: Arc<PluginRuntime>,
    db: Database,
    socket_path: PathBuf,
) -> std::io::Result<ControlSocketHandle> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Clean up anything left over from a previous run.
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;

    // 0o600 — only the current user can connect.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&socket_path, perms);
    }

    let ctx = DispatchCtx::new(db);
    let task = tokio::spawn(accept_loop(listener, runtime, ctx));

    Ok(ControlSocketHandle {
        task: Some(task),
        socket_path,
    })
}

async fn accept_loop(listener: UnixListener, runtime: Arc<PluginRuntime>, ctx: DispatchCtx) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let runtime = Arc::clone(&runtime);
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    serve_connection(stream, runtime, ctx).await;
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "control socket accept failed");
                // Brief backoff so a wedged accept doesn't spin the CPU.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn serve_connection(
    stream: tokio::net::UnixStream,
    runtime: Arc<PluginRuntime>,
    ctx: DispatchCtx,
) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => return, // client closed
            Err(e) => {
                tracing::debug!(error = %e, "control socket read ended");
                return;
            }
        };

        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                // We can't echo an id back if the frame didn't parse. Send a
                // best-effort error with id=0 and keep the connection open.
                let body = Response::err(0, format!("invalid frame: {e}"));
                if write_frame(&mut writer, &body).await.is_err() {
                    return;
                }
                continue;
            }
        };

        let req_id = req.id;
        let response = match dispatch::dispatch(&runtime, &ctx, &req.op, req.params).await {
            Ok(v) => Response::ok(req_id, v),
            Err(e) => Response::err(req_id, e),
        };

        if write_frame(&mut writer, &response).await.is_err() {
            return;
        }
    }
}

async fn write_frame<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &Response,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(response)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_bridge::wire::OP_PLUGIN_LIST;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread")]
    async fn listener_round_trips_plugin_list_over_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("control.sock");
        let db = Database::open(&tmp.path().join("t.db")).unwrap();
        let runtime = Arc::new(PluginRuntime::new());

        let handle = spawn(Arc::clone(&runtime), db, socket.clone())
            .await
            .unwrap();

        let mut client = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let frame = serde_json::to_vec(&Request {
            id: 42,
            op: OP_PLUGIN_LIST.to_string(),
            params: json!({}),
        })
        .unwrap();
        client.write_all(&frame).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.flush().await.unwrap();

        let (reader, _writer) = client.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        let line = lines.next_line().await.unwrap().expect("got a response");
        let resp: Response = serde_json::from_str(&line).unwrap();

        assert_eq!(resp.id, 42);
        assert!(resp.ok, "expected ok, got error: {:?}", resp.error);
        assert_eq!(resp.result.unwrap(), json!({ "plugins": [] }));

        drop(handle);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn listener_returns_error_for_unknown_op() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("control.sock");
        let db = Database::open(&tmp.path().join("t.db")).unwrap();
        let runtime = Arc::new(PluginRuntime::new());

        let handle = spawn(Arc::clone(&runtime), db, socket.clone())
            .await
            .unwrap();

        let mut client = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let frame = serde_json::to_vec(&Request {
            id: 7,
            op: "plugin.bogus".into(),
            params: json!({}),
        })
        .unwrap();
        client.write_all(&frame).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.flush().await.unwrap();

        let (reader, _writer) = client.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();

        assert_eq!(resp.id, 7);
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("unknown op"));

        drop(handle);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn listener_keeps_connection_open_after_malformed_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("control.sock");
        let db = Database::open(&tmp.path().join("t.db")).unwrap();
        let runtime = Arc::new(PluginRuntime::new());

        let handle = spawn(Arc::clone(&runtime), db, socket.clone())
            .await
            .unwrap();

        let mut client = tokio::net::UnixStream::connect(&socket).await.unwrap();
        // Malformed JSON first — server should reply with an error-framed
        // response (id=0) and hold the connection open.
        client.write_all(b"not-json\n").await.unwrap();
        client.flush().await.unwrap();

        let valid = serde_json::to_vec(&Request {
            id: 9,
            op: OP_PLUGIN_LIST.to_string(),
            params: json!({}),
        })
        .unwrap();
        client.write_all(&valid).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.flush().await.unwrap();

        let (reader, _writer) = client.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();

        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        assert_eq!(resp.id, 0);
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("invalid frame"));

        let line = lines.next_line().await.unwrap().unwrap();
        let resp: Response = serde_json::from_str(&line).unwrap();
        assert_eq!(resp.id, 9);
        assert!(resp.ok, "second request should succeed after bad frame");

        drop(handle);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn socket_file_is_removed_on_handle_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("control.sock");
        let db = Database::open(&tmp.path().join("t.db")).unwrap();
        let runtime = Arc::new(PluginRuntime::new());

        let handle = spawn(Arc::clone(&runtime), db, socket.clone())
            .await
            .unwrap();
        assert!(socket.exists());
        drop(handle);

        // Give the drop handler a tick to remove the file.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!socket.exists());
    }
}
