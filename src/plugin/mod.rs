//! Process-plugin runtime.
//!
//! Plugins with a `[process]` section in their manifest declare an executable
//! that thurbox spawns and talks to over stdio JSON-RPC. This module owns the
//! lifecycle: spawning, performing the handshake, capturing stderr to
//! per-plugin log files, registering plugin-contributed backends with the
//! agent registry, and sending a graceful `stop` on shutdown.
//!
//! The `[contributes]` half (skills/roles/MCP servers/themes) lives entirely
//! in `storage::plugins` — that side has no runtime cost and runs the same
//! whether or not a plugin's process ever spawns.
//!
//! ## v1 activation policy
//!
//! Every enabled process plugin spawns at boot. The `activation_events` field
//! is parsed and surfaced — `OnBackendSelected:<name>` entries are used to
//! register backend names with the agent registry — but the lazy gating
//! (`OnRole`, `OnCommand`, `OnWorkspaceContains`) is informational for now.
//! Once an operator has more than a handful of plugins this turns into work
//! we want to defer.
//!
//! ## Architecture
//!
//! Allowed imports: `agent`, `session`, `storage`, `paths`, `sync`. The
//! `agent` import is *only* for the [`SessionBackend`] trait the backend
//! adapter implements. Forbidden: `app`, `ui`, `mcp`, `git`.

pub mod activation;
pub mod backend_adapter;
pub mod control_socket;
pub mod handshake;
pub mod output;
pub mod proxy;
pub mod rpc;

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::agent::backend::SessionBackend;
use crate::agent::registry::BackendRegistry;
use crate::session::{ActivationEvent, Capability, PluginConfig, PluginManifest};
use crate::storage::Database;

use activation::{any_event_matches, FiredEvent};
use backend_adapter::PluginBackend;
use handshake::{perform_handshake, HandshakeError, HandshakeResponse};
use output::PluginLogWriter;
use proxy::McpToolProxy;
use rpc::{RpcClient, RpcError};

/// One running process plugin.
pub struct RunningPlugin {
    pub name: String,
    pub manifest: PluginManifest,
    pub client: RpcClient,
    pub handshake: HandshakeResponse,
    /// Held to keep the child alive — dropping the runtime drops these.
    child: Arc<Mutex<Child>>,
}

impl RunningPlugin {
    /// Send the `stop` op (best-effort), then wait for the child to exit.
    /// Falls back to `kill()` if the child does not exit promptly.
    pub async fn shutdown(self) {
        let _ = self.client.notify("stop", Value::Null).await;
        let mut child = self.child.lock().await;
        // Give the plugin a brief grace period to exit cleanly on its own.
        let grace = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
        if grace.is_err() {
            tracing::warn!(plugin = %self.name, "Plugin did not exit on stop; killing");
            let _ = child.start_kill();
        }
    }
}

/// Owns spawned plugins for the lifetime of the host process.
///
/// Cheap to construct; spawn happens via [`PluginRuntime::start_all_process_plugins`]
/// at boot or [`PluginRuntime::activate_event`] for activation-event-driven
/// spawns added in a follow-up.
pub struct PluginRuntime {
    running: Mutex<HashMap<String, RunningPlugin>>,
}

impl Default for PluginRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginRuntime {
    pub fn new() -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
        }
    }

    /// True if a plugin with this name is currently running.
    pub async fn is_running(&self, name: &str) -> bool {
        self.running.lock().await.contains_key(name)
    }

    /// Number of currently running plugins.
    pub async fn len(&self) -> usize {
        self.running.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.running.lock().await.is_empty()
    }

    /// Spawn every enabled process plugin and register any backends it contributes.
    ///
    /// Called once at boot, before [`crate::app::App::new`] consumes the
    /// `BackendRegistry`. After this returns the registry is frozen — any
    /// later plugin enable goes through [`PluginRuntime::activate_event`]
    /// and *cannot* contribute new backends, which is fine: a backend swap
    /// requires a thurbox restart in v1.
    ///
    /// Plugins without a `[process]` section, with `enabled = false`, or
    /// that fail handshake are skipped — but every failure is logged so the
    /// operator can find it in the per-plugin log file.
    pub async fn start_all_process_plugins(
        &self,
        db: &Database,
        registry: &mut BackendRegistry,
        rpc_handle: tokio::runtime::Handle,
    ) -> Vec<SpawnReport> {
        let plugins = match db.list_effective_plugins() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "Failed to list plugins for boot spawn");
                return Vec::new();
            }
        };
        let mut reports = Vec::new();
        for (plugin, _src) in plugins {
            if !plugin.enabled {
                continue;
            }
            if self.is_running(&plugin.name).await {
                continue;
            }
            let manifest = match PluginManifest::load(&plugin.path) {
                Ok(m) => m,
                Err(e) => {
                    reports.push(SpawnReport::error(
                        plugin.name.clone(),
                        format!("manifest load failed: {e}"),
                    ));
                    continue;
                }
            };
            if manifest.process.is_none() {
                continue;
            }
            match self
                .spawn_plugin(db, &plugin, &manifest, Some(registry), &rpc_handle)
                .await
            {
                Ok(()) => reports.push(SpawnReport::ok(plugin.name)),
                Err(e) => reports.push(SpawnReport::error(plugin.name, e.to_string())),
            }
        }
        reports
    }

    /// Try to spawn every enabled plugin matching `event` that isn't already running.
    ///
    /// Used by tests and reserved for follow-up wiring of lazy activation
    /// events (the boot path uses [`Self::start_all_process_plugins`]
    /// instead). Plugins spawned this way cannot contribute backends — the
    /// `BackendRegistry` is owned by [`crate::app::App`] by this point.
    pub async fn activate_event(&self, db: &Database, event: &FiredEvent<'_>) -> Vec<SpawnReport> {
        let plugins = match db.list_effective_plugins() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "Failed to list plugins for activation");
                return Vec::new();
            }
        };
        let mut reports = Vec::new();
        for (plugin, _src) in plugins {
            if !plugin.enabled {
                continue;
            }
            if self.is_running(&plugin.name).await {
                continue;
            }
            let manifest = match PluginManifest::load(&plugin.path) {
                Ok(m) => m,
                Err(e) => {
                    reports.push(SpawnReport::error(
                        plugin.name.clone(),
                        format!("manifest load failed: {e}"),
                    ));
                    continue;
                }
            };
            let Some(process) = manifest.process.clone() else {
                continue;
            };
            if !any_event_matches(event, &process.activation_events) {
                continue;
            }
            let handle = tokio::runtime::Handle::current();
            match self
                .spawn_plugin(db, &plugin, &manifest, None, &handle)
                .await
            {
                Ok(()) => reports.push(SpawnReport::ok(plugin.name)),
                Err(e) => reports.push(SpawnReport::error(plugin.name, e.to_string())),
            }
        }
        reports
    }

    /// Send the `stop` op to every running plugin and wait for them to exit.
    pub async fn shutdown_all(&self) {
        let drained: Vec<RunningPlugin> = {
            let mut map = self.running.lock().await;
            map.drain().map(|(_, p)| p).collect()
        };
        for plugin in drained {
            plugin.shutdown().await;
        }
    }

    /// Send a `config.updated` notification to a running plugin.
    ///
    /// No-op if the plugin isn't currently running. Errors are logged but
    /// not propagated — a wedged plugin shouldn't break the calling MCP
    /// tool round-trip.
    pub async fn notify_config_updated(&self, plugin_name: &str, key: &str, value: &Value) {
        let map = self.running.lock().await;
        let Some(running) = map.get(plugin_name) else {
            return;
        };
        let payload = serde_json::json!({
            "key": key,
            "value": value,
        });
        if let Err(e) = running.client.notify("config.updated", payload).await {
            tracing::warn!(plugin = %plugin_name, key = %key, error = %e, "config.updated notify failed");
        }
    }

    /// Return a proxy for talking to a running plugin's `mcp-tools` capability.
    ///
    /// Returns `None` when the plugin isn't running, isn't a process plugin,
    /// or didn't declare the `mcp-tools` capability. The proxy holds a clone
    /// of the underlying [`RpcClient`], so it stays valid until the plugin
    /// shuts down — but the caller should not cache it across a
    /// `shutdown_all` / restart boundary.
    pub async fn mcp_proxy(&self, plugin_name: &str) -> Option<McpToolProxy> {
        let map = self.running.lock().await;
        let running = map.get(plugin_name)?;
        let process = running.manifest.process.as_ref()?;
        if !process.capabilities.contains(&Capability::McpTools) {
            return None;
        }
        Some(McpToolProxy::new(running.client.clone()))
    }

    /// Names of currently running plugins that declared the `mcp-tools`
    /// capability, sorted for deterministic output. Used by the control
    /// socket's `plugin.list` op.
    pub async fn list_mcp_tools_plugins(&self) -> Vec<String> {
        let map = self.running.lock().await;
        let mut names: Vec<String> = map
            .values()
            .filter(|p| {
                p.manifest
                    .process
                    .as_ref()
                    .is_some_and(|proc| proc.capabilities.contains(&Capability::McpTools))
            })
            .map(|p| p.name.clone())
            .collect();
        names.sort();
        names
    }

    async fn spawn_plugin(
        &self,
        db: &Database,
        plugin: &PluginConfig,
        manifest: &PluginManifest,
        registry: Option<&mut BackendRegistry>,
        rpc_handle: &tokio::runtime::Handle,
    ) -> Result<(), SpawnError> {
        let process = manifest
            .process
            .as_ref()
            .ok_or_else(|| SpawnError::Misconfigured("no [process] section".into()))?;

        let exec = plugin.path.join(&process.exec);

        let mut cmd = Command::new(&exec);
        cmd.args(&process.args)
            .env_clear()
            .envs(std::env::vars())
            .envs(process.env.clone())
            .current_dir(&plugin.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .map_err(|e| SpawnError::Spawn(format!("{exec:?}: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SpawnError::Spawn("child had no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SpawnError::Spawn("child had no stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| SpawnError::Spawn("child had no stderr".into()))?;

        // Stream stderr into the per-plugin log file as the plugin runs.
        if let Some(log_path) = crate::paths::plugin_log_path(&plugin.name) {
            let plugin_name = plugin.name.clone();
            tokio::spawn(drain_stderr_to_log(plugin_name, stderr, log_path));
        }

        let client = RpcClient::new(stdin, stdout);

        let effective_config = build_effective_configuration(db, &plugin.name, manifest);
        let handshake = perform_handshake(&client, &plugin.name, effective_config)
            .await
            .map_err(SpawnError::Handshake)?;

        // Register a PluginBackend for every `OnBackendSelected:<name>` activation
        // event — that's how plugins declare which backend names they own.
        if let Some(registry) = registry {
            for event in &process.activation_events {
                if let ActivationEvent::OnBackendSelected(backend_name) = event {
                    if registry.has(backend_name) {
                        tracing::warn!(
                            plugin = %plugin.name,
                            backend = %backend_name,
                            "Plugin backend name collides with an existing backend; keeping the first"
                        );
                        continue;
                    }
                    let adapter = PluginBackend::new(
                        backend_name.clone(),
                        client.clone(),
                        rpc_handle.clone(),
                    );
                    let arc: Arc<dyn SessionBackend> = Arc::new(adapter);
                    registry.register(arc);
                    tracing::info!(
                        plugin = %plugin.name,
                        backend = %backend_name,
                        "Registered plugin backend"
                    );
                }
            }
        }

        let running = RunningPlugin {
            name: plugin.name.clone(),
            manifest: manifest.clone(),
            client,
            handshake,
            child: Arc::new(Mutex::new(child)),
        };
        self.running
            .lock()
            .await
            .insert(plugin.name.clone(), running);
        Ok(())
    }
}

/// Build the JSON object passed as `effective_configuration` in the handshake.
///
/// Reads the manifest's `[[contributes.configuration]]` schema, layers any
/// user overrides from `plugin_settings`, and emits `{ key: value, … }`. A
/// plugin with no schema gets `{}` — never `null` — so the wire shape is
/// stable.
fn build_effective_configuration(
    db: &Database,
    plugin_name: &str,
    manifest: &PluginManifest,
) -> Value {
    let schema = &manifest.contributes.configuration;
    if schema.is_empty() {
        return Value::Object(Map::new());
    }
    let effective = match db.list_plugin_settings_with_defaults(plugin_name, schema) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(plugin = %plugin_name, error = %e, "Failed to read effective configuration; sending defaults only");
            return schema_defaults_to_json(schema);
        }
    };
    let mut map = Map::with_capacity(effective.len());
    for setting in effective {
        // toml::Value → serde_json::Value via serialize.
        match serde_json::to_value(&setting.effective_value) {
            Ok(v) => {
                map.insert(setting.key, v);
            }
            Err(e) => {
                tracing::warn!(plugin = %plugin_name, key = %setting.key, error = %e, "Failed to encode setting; skipping");
            }
        }
    }
    Value::Object(map)
}

fn schema_defaults_to_json(schema: &[crate::session::ConfigurationSchema]) -> Value {
    let mut map = Map::with_capacity(schema.len());
    for cfg in schema {
        if let Ok(v) = serde_json::to_value(&cfg.default) {
            map.insert(cfg.key.clone(), v);
        }
    }
    Value::Object(map)
}

async fn drain_stderr_to_log(
    plugin_name: String,
    mut stderr: tokio::process::ChildStderr,
    log_path: std::path::PathBuf,
) {
    use tokio::io::AsyncReadExt;

    let mut writer = match PluginLogWriter::open(log_path, output::MAX_LOG_BYTES) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(plugin = %plugin_name, error = %e, "Failed to open plugin log");
            return;
        }
    };
    let mut buf = vec![0u8; 4096];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if let Err(e) = writer.write_chunk(&buf[..n]) {
                    tracing::warn!(plugin = %plugin_name, error = %e, "Plugin log write failed");
                }
            }
            Err(e) => {
                tracing::debug!(plugin = %plugin_name, error = %e, "Plugin stderr drain ended");
                break;
            }
        }
    }
}

/// Outcome of trying to spawn a single plugin during an activation pass.
#[derive(Debug, Clone)]
pub struct SpawnReport {
    pub name: String,
    pub error: Option<String>,
}

impl SpawnReport {
    fn ok(name: String) -> Self {
        Self { name, error: None }
    }
    fn error(name: String, error: String) -> Self {
        Self {
            name,
            error: Some(error),
        }
    }
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

/// Reasons spawning can fail. Wrapped in [`SpawnReport`] for the runtime;
/// surfaced to the operator via the per-plugin log file and `list_plugins`.
#[derive(Debug)]
pub enum SpawnError {
    Misconfigured(String),
    Spawn(String),
    Handshake(HandshakeError),
    Rpc(RpcError),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::Misconfigured(s) => write!(f, "{s}"),
            SpawnError::Spawn(s) => write!(f, "spawn failed: {s}"),
            SpawnError::Handshake(e) => write!(f, "handshake failed: {e}"),
            SpawnError::Rpc(e) => write!(f, "rpc failed: {e}"),
        }
    }
}

impl std::error::Error for SpawnError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_runtime_reports_nothing_running() {
        let rt = PluginRuntime::new();
        assert!(rt.is_empty().await);
        assert_eq!(rt.len().await, 0);
        assert!(!rt.is_running("anything").await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_all_on_empty_runtime_is_noop() {
        let rt = PluginRuntime::new();
        rt.shutdown_all().await;
        assert!(rt.is_empty().await);
    }

    #[test]
    fn build_effective_configuration_empty_schema_yields_empty_object() {
        // Manifest with no configuration schema — function should not touch the DB.
        let manifest = PluginManifest {
            name: "demo".into(),
            version: "0.1.0".into(),
            description: String::new(),
            author: String::new(),
            thurbox_plugin_api: 1,
            process: None,
            contributes: Default::default(),
        };
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.db")).unwrap();
        let v = build_effective_configuration(&db, "demo", &manifest);
        assert_eq!(v, Value::Object(Map::new()));
    }
}
