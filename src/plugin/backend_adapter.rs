//! Adapt an out-of-process plugin into the [`SessionBackend`] trait.
//!
//! ## Wire model
//!
//! The plugin owns one Unix domain socket per spawned/adopted session. Thurbox
//! talks to the plugin over its existing JSON-RPC stdio channel for control
//! ops (`backend.spawn`, `backend.adopt`, `backend.resize`, …). The plugin
//! responds with a `socket_path`; thurbox connects to that socket and treats
//! it as the session's bidirectional byte stream.
//!
//! Stdio is reserved for control. Mixing per-session bytes onto the same
//! stdio framing would force ad-hoc multiplexing here — Unix sockets give us
//! one I/O peer per session for free, and `std::os::unix::net::UnixStream`
//! already implements `Read + Send` and `Write + Send` so it slots straight
//! into [`SpawnedSession`] / [`AdoptedSession`] without an async bridge.
//!
//! ## Sync ↔ async bridge
//!
//! [`SessionBackend`] is a synchronous trait, but the underlying RPC client
//! is async. Thurbox runs under a multi-threaded tokio runtime, so we use
//! `block_in_place` + `Handle::block_on` to call into the runtime from a
//! synchronous context. The handle is captured at construction time so the
//! adapter doesn't depend on `tokio::runtime::Handle::current()` at call time
//! (which would panic if invoked from outside the runtime, e.g., from a
//! `spawn_blocking` reader thread).

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio::runtime::Handle;

use crate::agent::backend::{AdoptedSession, DiscoveredSession, SessionBackend, SpawnedSession};

use super::rpc::RpcClient;

/// How long to wait for a single backend RPC to complete.
///
/// Long enough to cover a slow first spawn (image pull, initial handshake on
/// the plugin's downstream service) but short enough that a wedged plugin
/// doesn't lock up the TUI thread.
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// One backend contributed by a process plugin.
///
/// Cloned by `BackendRegistry::get`; cheap to clone — `RpcClient` is
/// internally `Arc`'d.
pub struct PluginBackend {
    backend_name: String,
    client: RpcClient,
    handle: Handle,
}

impl PluginBackend {
    /// Build an adapter for a plugin that's already passed handshake.
    pub fn new(backend_name: String, client: RpcClient, handle: Handle) -> Self {
        Self {
            backend_name,
            client,
            handle,
        }
    }

    /// Drive an async RPC call to completion from a synchronous context.
    ///
    /// Uses `block_in_place` to release the current worker thread while
    /// blocking — required to avoid deadlocking the runtime when called from
    /// the main thread.
    fn call_blocking(&self, op: &str, params: Value) -> Result<Value> {
        let client = self.client.clone();
        let op_owned = op.to_string();
        let backend_name = self.backend_name.clone();
        let handle = self.handle.clone();
        tokio::task::block_in_place(move || {
            handle.block_on(async move {
                client
                    .call(&op_owned, params, RPC_TIMEOUT)
                    .await
                    .map_err(|e| {
                        anyhow!("plugin backend '{backend_name}' rpc {op_owned} failed: {e}")
                    })
            })
        })
    }

    fn connect_socket(&self, path: &str) -> Result<(UnixStream, UnixStream)> {
        let read = UnixStream::connect(path).map_err(|e| {
            anyhow!(
                "plugin backend '{}': connect to {path}: {e}",
                self.backend_name
            )
        })?;
        let write = read.try_clone().map_err(|e| {
            anyhow!(
                "plugin backend '{}': try_clone unix stream: {e}",
                self.backend_name
            )
        })?;
        Ok((read, write))
    }
}

impl SessionBackend for PluginBackend {
    fn name(&self) -> &str {
        &self.backend_name
    }

    fn check_available(&self) -> Result<()> {
        // The plugin already passed handshake before being registered — if it
        // weren't reachable we wouldn't be here. Any later failure surfaces
        // through individual RPC calls.
        Ok(())
    }

    fn ensure_ready(&self) -> Result<()> {
        Ok(())
    }

    fn spawn(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> Result<SpawnedSession> {
        let params = json!({
            "window_name": window_name,
            "command": command,
            "args": args,
            "cwd": cwd.map(|p| p.display().to_string()),
            "env": env,
            "rows": rows,
            "cols": cols,
        });
        let result = self.call_blocking("backend.spawn", params)?;
        let backend_id = result
            .get("backend_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("plugin backend.spawn missing backend_id"))?
            .to_string();
        let socket_path = result
            .get("socket_path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("plugin backend.spawn missing socket_path"))?;
        let (read, write) = self.connect_socket(socket_path)?;
        Ok(SpawnedSession {
            backend_id,
            output: Box::new(read),
            input: Box::new(write),
        })
    }

    fn adopt(&self, backend_id: &str, rows: u16, cols: u16) -> Result<AdoptedSession> {
        let params = json!({
            "backend_id": backend_id,
            "rows": rows,
            "cols": cols,
        });
        let result = self.call_blocking("backend.adopt", params)?;
        let socket_path = result
            .get("socket_path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("plugin backend.adopt missing socket_path"))?;
        let (read, write) = self.connect_socket(socket_path)?;
        Ok(AdoptedSession {
            output: Box::new(read),
            input: Box::new(write),
        })
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        let result = self.call_blocking("backend.discover", json!({}))?;
        let arr = result
            .as_array()
            .ok_or_else(|| anyhow!("plugin backend.discover did not return an array"))?;
        let mut out = Vec::with_capacity(arr.len());
        for v in arr {
            let backend_id = v
                .get("backend_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("backend.discover entry missing backend_id"))?
                .to_string();
            let name = v
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let is_alive = v.get("is_alive").and_then(Value::as_bool).unwrap_or(true);
            out.push(DiscoveredSession {
                backend_id,
                name,
                is_alive,
            });
        }
        Ok(out)
    }

    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.call_blocking(
            "backend.resize",
            json!({
                "backend_id": backend_id,
                "rows": rows,
                "cols": cols,
            }),
        )?;
        Ok(())
    }

    fn is_dead(&self, backend_id: &str) -> Result<bool> {
        let result = self.call_blocking("backend.is_dead", json!({ "backend_id": backend_id }))?;
        Ok(result.get("dead").and_then(Value::as_bool).unwrap_or(false))
    }

    fn kill(&self, backend_id: &str) -> Result<()> {
        self.call_blocking("backend.kill", json!({ "backend_id": backend_id }))?;
        Ok(())
    }

    fn detach(&self, backend_id: &str) -> Result<()> {
        self.call_blocking("backend.detach", json!({ "backend_id": backend_id }))?;
        Ok(())
    }

    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        let result = self.call_blocking("backend.pane_pid", json!({ "backend_id": backend_id }))?;
        Ok(result.get("pid").and_then(Value::as_u64).map(|n| n as u32))
    }
}
