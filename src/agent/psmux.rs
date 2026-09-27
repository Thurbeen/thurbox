//! Native Windows psmux backend. The shared tmux protocol engine still owns
//! control-mode framing and transport, while this adapter owns psmux's
//! lifecycle capabilities and its registered identity.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use super::backend::{
    AdoptedSession, BackendLiveness, DiscoveredSession, SessionBackend, SpawnedSession,
};
use super::control_mode::PaneSnapshot;
use super::tmux::{TmuxBackend, WindowRole};

pub struct PsmuxBackend {
    protocol: TmuxBackend,
}

impl PsmuxBackend {
    pub fn local() -> Self {
        let mut protocol = TmuxBackend::local();
        protocol.set_name("local-psmux");
        Self { protocol }
    }

    pub fn from_host(host: &crate::session::HostDef) -> Self {
        let mut protocol = TmuxBackend::from_host(host);
        protocol.set_name(format!("{}:psmux", host.backend_name()));
        Self { protocol }
    }
}

impl SessionBackend for PsmuxBackend {
    fn name(&self) -> &str {
        self.protocol.name()
    }
    fn needs_liveness_poll(&self) -> bool {
        true
    }
    fn check_available(&self) -> Result<()> {
        self.protocol.check_available()
    }
    fn ensure_ready(&self) -> Result<()> {
        self.protocol.ensure_ready()
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
        self.protocol
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
        self.protocol
            .spawn_headless(session_id, window_name, command, args, cwd, env)
    }
    fn headless_liveness(&self, session_id: &str, session_name: &str) -> Result<BackendLiveness> {
        self.protocol.headless_liveness(session_id, session_name)
    }
    fn headless_live_pane(&self, session_id: &str, session_name: &str) -> Result<Option<String>> {
        self.protocol.headless_live_pane(session_id, session_name)
    }
    fn headless_owned_panes_in(
        &self,
        windows: &[DiscoveredSession],
        session_id: &str,
        session_name: &str,
    ) -> Vec<String> {
        self.protocol
            .headless_owned_panes_in(windows, session_id, session_name)
    }
    fn kill_headless(
        &self,
        session_id: &str,
        session_name: &str,
        agent_pane: &str,
        shell_pane: &str,
    ) -> Result<bool> {
        self.protocol
            .kill_headless(session_id, session_name, agent_pane, shell_pane)
    }
    fn headless_pane_pid(
        &self,
        backend_id: &str,
        session_id: &str,
        name: &str,
    ) -> Result<Option<u32>> {
        self.protocol
            .headless_pane_pid(backend_id, session_id, name)
    }
    fn headless_discover(&self) -> Result<Vec<DiscoveredSession>> {
        self.protocol.headless_discover()
    }

    fn adopt(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
        seed: Option<Vec<u8>>,
    ) -> Result<AdoptedSession> {
        self.protocol.adopt(backend_id, rows, cols, seed)
    }
    fn capture_history(&self, backend_id: &str) -> Result<Vec<u8>> {
        self.protocol.capture_history(backend_id)
    }
    fn title_seed(&self, backend_id: &str) -> Vec<u8> {
        self.protocol.title_seed(backend_id)
    }
    fn supports_snapshots(&self) -> bool {
        false
    }
    fn request_snapshot(&self, _backend_id: &str) -> Result<()> {
        anyhow::bail!("psmux cannot snapshot a pane in step with its output")
    }
    fn snapshot(&self, _backend_id: &str) -> Result<PaneSnapshot> {
        anyhow::bail!("psmux cannot snapshot a pane")
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        self.protocol.discover()
    }
    fn stamp_window(&self, _backend_id: &str, _session_id: &str, _role: WindowRole) -> Result<()> {
        // psmux stores these options globally, so a stamp would misidentify every window.
        Ok(())
    }
    fn window_panes(&self, window_name: &str) -> Result<Vec<(String, bool)>> {
        self.protocol.window_panes(window_name)
    }
    fn set_pane_retention(&self, _backend_id: &str, _keep: bool) -> Result<()> {
        Ok(())
    }
    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.protocol.resize(backend_id, rows, cols)
    }
    fn claim_size(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.resize(backend_id, rows, cols)
    }
    fn is_dead(&self, backend_id: &str) -> Result<bool> {
        self.protocol.is_dead(backend_id)
    }
    fn kill(&self, backend_id: &str) -> Result<()> {
        self.protocol.kill(backend_id)
    }
    fn detach(&self, backend_id: &str) -> Result<()> {
        self.protocol.detach(backend_id)
    }
    fn default_shell(&self) -> String {
        self.protocol.default_shell()
    }
    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        self.protocol.pane_pid(backend_id)
    }
    fn pane_pids(&self) -> Result<HashMap<String, u32>> {
        self.protocol.pane_pids()
    }
    fn shutdown(&self) {
        self.protocol.shutdown()
    }
}
