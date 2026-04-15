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

use crate::session::{RolePermissions, SessionConfig};

/// Decide whether to pass `--resume <id>` vs `--session-id <id>` when
/// (re)spawning claude. Returns `Some(id.clone())` only when a transcript for
/// that id already exists on disk under `CLAUDE_CONFIG_DIR`/`~/.claude`.
///
/// Shared by [`restart::restart_session_headless`] and
/// `App::restart_active_session` so the headless and TUI paths agree.
pub(crate) fn resume_id_if_transcript_exists(
    agent_session_id: &str,
    permissions: &RolePermissions,
) -> Option<String> {
    let config_dir_override = permissions
        .env
        .get("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from);
    crate::paths::claude_transcript_exists(agent_session_id, config_dir_override.as_deref())
        .then(|| agent_session_id.to_string())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_id_is_none_when_no_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let mut perms = RolePermissions::default();
        perms
            .env
            .insert("CLAUDE_CONFIG_DIR".into(), tmp.path().display().to_string());
        assert_eq!(resume_id_if_transcript_exists("some-uuid", &perms), None);
    }

    #[test]
    fn resume_id_is_some_when_transcript_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "77777777-8888-9999-aaaa-bbbbbbbbbbbb";
        let proj = tmp.path().join("projects").join("-slug");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{sid}.jsonl")), b"").unwrap();

        let mut perms = RolePermissions::default();
        perms
            .env
            .insert("CLAUDE_CONFIG_DIR".into(), tmp.path().display().to_string());
        assert_eq!(
            resume_id_if_transcript_exists(sid, &perms),
            Some(sid.to_string())
        );
    }
}
