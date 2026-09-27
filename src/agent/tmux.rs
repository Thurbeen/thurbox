//! tmux backend. Its transport and protocol are fixed to tmux; common pane
//! bookkeeping lives in the mux backend core.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use super::backend::{
    AdoptedSession, BackendLiveness, DiscoveredSession, SessionBackend, SpawnedSession,
};
use super::control_mode::PaneSnapshot;
use super::mux::MuxBackend;
pub use super::mux::{
    agent_pane_path, agent_window, agent_window_alive, automation_heartbeat_running,
    capture_pane_text, ensure_automation_heartbeat, host_socket, kill_remote_windows,
    kill_shell_window, kill_window, kill_window_at, known_host_socket, learn_host_socket,
    list_local_hook_states, list_remote_hook_states, local_socket_name, local_window_index,
    pane_state, remote_window_index, rename_session_windows, resolve_cli_binary, resolve_key,
    send_key_now, send_prompt_after_delay, send_prompt_now, send_text_now, set_own_pane_state,
    spawn_window, spawn_window_remote, stamp_local_window, stop_automation_heartbeat,
    window_exists, window_pane_pid, PanePath, PaneState, ResolvedKey, SessionPanes,
    WindowIndex, NAMED_KEYS, SOCKET_OVERRIDE_ENV, SOCKET_OWNER_ENV, WINDOW_ROLE_OPTION,
    WINDOW_SESSION_OPTION,
};
pub(crate) use super::mux::{
    agent_window_name, program_window_name, sanitize_window_name, shell_window_name, TMUX_SOCKET,
};
pub use crate::agent::backend::{Located, WindowRole};
use super::transport::TmuxTransport;

pub struct TmuxBackend {
    pub(crate) core: MuxBackend<TmuxTransport>,
}

pub type LocalTmuxBackend = TmuxBackend;

impl Default for TmuxBackend {
    fn default() -> Self {
        Self::local()
    }
}

impl TmuxBackend {
    pub fn new() -> Self {
        Self::local()
    }
    pub fn with_transport(
        transport: TmuxTransport,
        socket: impl Into<String>,
        session: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            core: MuxBackend::with_transport(transport, socket, session, name),
        }
    }
    pub(crate) fn set_name(&mut self, name: impl Into<String>) {
        self.core.set_name(name);
    }
    pub fn local() -> Self {
        let mut core = MuxBackend::<TmuxTransport>::local();
        core.set_name("local-tmux");
        Self { core }
    }

    pub fn from_host(host: &crate::session::HostDef) -> Self {
        let mut core = MuxBackend::<TmuxTransport>::from_host(host);
        core.set_name(host.backend_name());
        Self { core }
    }
}

impl SessionBackend for TmuxBackend {
    fn name(&self) -> &str {
        self.core.name()
    }
    fn needs_liveness_poll(&self) -> bool {
        self.core.needs_liveness_poll()
    }
    fn check_available(&self) -> Result<()> {
        self.core.check_available()
    }
    fn ensure_ready(&self) -> Result<()> {
        self.core.ensure_ready()
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
        self.core
            .spawn(window_name, command, args, cwd, env, rows, cols)
    }
    fn spawn_headless(
        &self,
        session_id: &str,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Result<String> {
        self.core
            .spawn_headless(session_id, window_name, command, args, cwd, env)
    }
    fn headless_liveness(&self, session_id: &str, session_name: &str) -> Result<BackendLiveness> {
        self.core.headless_liveness(session_id, session_name)
    }
    fn headless_live_pane(&self, session_id: &str, session_name: &str) -> Result<Option<String>> {
        self.core.headless_live_pane(session_id, session_name)
    }
    fn headless_owned_panes_in(
        &self,
        windows: &[DiscoveredSession],
        session_id: &str,
        session_name: &str,
    ) -> Vec<String> {
        self.core
            .headless_owned_panes_in(windows, session_id, session_name)
    }
    fn kill_headless(
        &self,
        session_id: &str,
        session_name: &str,
        agent_pane: &str,
        shell_pane: &str,
    ) -> Result<bool> {
        self.core
            .kill_headless(session_id, session_name, agent_pane, shell_pane)
    }
    fn headless_pane_pid(
        &self,
        backend_id: &str,
        session_id: &str,
        name: &str,
    ) -> Result<Option<u32>> {
        self.core.headless_pane_pid(backend_id, session_id, name)
    }
    fn headless_discover(&self) -> Result<Vec<DiscoveredSession>> {
        self.core.headless_discover()
    }

    fn adopt(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
        seed: Option<Vec<u8>>,
    ) -> Result<AdoptedSession> {
        self.core.adopt(backend_id, rows, cols, seed)
    }
    fn capture_history(&self, backend_id: &str) -> Result<Vec<u8>> {
        self.core.capture_history(backend_id)
    }
    fn title_seed(&self, backend_id: &str) -> Vec<u8> {
        self.core.title_seed(backend_id)
    }
    fn supports_snapshots(&self) -> bool {
        self.core.supports_snapshots()
    }
    fn request_snapshot(&self, backend_id: &str) -> Result<()> {
        self.core.request_snapshot(backend_id)
    }
    fn snapshot(&self, backend_id: &str) -> Result<PaneSnapshot> {
        self.core.snapshot(backend_id)
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        self.core.discover()
    }
    fn stamp_window(&self, backend_id: &str, session_id: &str, role: WindowRole) -> Result<()> {
        self.core.stamp_window(backend_id, session_id, role)
    }
    fn window_panes(&self, window_name: &str) -> Result<Vec<(String, bool)>> {
        self.core.window_panes(window_name)
    }
    fn set_pane_retention(&self, backend_id: &str, keep: bool) -> Result<()> {
        self.core.set_pane_retention(backend_id, keep)
    }
    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.core.resize(backend_id, rows, cols)
    }
    fn claim_size(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.core.claim_size(backend_id, rows, cols)
    }
    fn is_dead(&self, backend_id: &str) -> Result<bool> {
        self.core.is_dead(backend_id)
    }
    fn kill(&self, backend_id: &str) -> Result<()> {
        self.core.kill(backend_id)
    }
    fn detach(&self, backend_id: &str) -> Result<()> {
        self.core.detach(backend_id)
    }
    fn default_shell(&self) -> String {
        self.core.default_shell()
    }
    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        self.core.pane_pid(backend_id)
    }
    fn pane_pids(&self) -> Result<HashMap<String, u32>> {
        self.core.pane_pids()
    }
    fn pane_ids(&self) -> Result<std::collections::HashSet<String>> {
        self.core.pane_ids()
    }
    fn shutdown(&self) {
        self.core.shutdown()
    }
    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        self.core.take_hook_state_events()
    }
}
