//! Headless session operations — spawn and restart sessions without the TUI.
//!
//! Callers (MCP, CLI) use these helpers to drive the same local-tmux-backed
//! sessions the TUI manages, without requiring the TUI event loop. All
//! operations are synchronous against the SQLite database and the `tmux -L
//! thurbox` server.

pub mod restart;
pub mod spawn;

pub use restart::restart_session_headless;
pub use spawn::{spawn_session_headless, SpawnRequest, SpawnResult};

use crate::session::SessionConfig;

/// Build the (command, args) invocation for the default `ClaudeProvider`
/// from a fully populated [`SessionConfig`].
///
/// Centralised here so spawn and restart agree on the args and so the
/// `<P as AgentProvider>::method(&p, ...)` turbofish lives in one place.
fn build_claude_invocation(config: &SessionConfig) -> (String, Vec<String>) {
    let provider = crate::agent::claude::ClaudeProvider;
    type P = crate::agent::claude::ClaudeProvider;
    let command = <P as crate::agent::provider::AgentProvider>::command(&provider).to_string();
    let args = <P as crate::agent::provider::AgentProvider>::build_args(&provider, config);
    (command, args)
}

/// Inject the standard thurbox env hints (`THURBOX_SESSION_ID`,
/// `THURBOX_METRICS_DIR`) into a session config.
///
/// Mirrors `App::do_spawn_session` so headless and TUI sessions look
/// identical to the spawned process.
fn inject_thurbox_env(config: &mut SessionConfig, agent_session_id: &str) {
    config
        .permissions
        .env
        .insert("THURBOX_SESSION_ID".into(), agent_session_id.into());
    if let Some(dir) = crate::paths::metrics_directory() {
        config
            .permissions
            .env
            .insert("THURBOX_METRICS_DIR".into(), dir.to_string_lossy().into());
    }
}
