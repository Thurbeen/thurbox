//! Headless session operations — spawn and restart sessions without the TUI.
//!
//! Callers (MCP, CLI) use these helpers to drive the same local-tmux-backed
//! sessions the TUI manages, without requiring the TUI event loop. All
//! operations are synchronous against the SQLite database and the `tmux -L
//! thurbox` server.

pub mod delete;
pub mod extensions;
pub mod restart;
pub mod spawn;

pub use delete::{delete_session_headless, ForceDeleteReport};
pub use extensions::{
    activate_extension, deactivate_extension, ensure_extension, extension_health,
    heal_active_extensions, install_extension, reinstall_extension, uninstall_extension,
    update_all_extensions, update_extension, DeactivateReport, EnsureReport, ExtensionHealth,
    InstallReport, ReinstallReport, UninstallReport, UpdateReport,
};
pub use restart::restart_session_headless;
pub use spawn::{spawn_session_headless, SpawnRequest, SpawnResult};

use std::collections::HashMap;

use crate::session::SessionConfig;

/// Decide whether to pass the agent's resume group vs starting fresh when
/// (re)spawning. Returns `Some(id.clone())` only when a Claude transcript for
/// that id already exists on disk under `CLAUDE_CONFIG_DIR`/`~/.claude`.
///
/// Claude-specific: only the `claude` agent persists resumable transcripts.
/// For agents without an on-disk transcript this returns `None`, which makes
/// restart start a fresh conversation (the live tmux process is what provides
/// cross-restart persistence in the general case).
///
/// Shared by [`restart::restart_session_headless`] and
/// `App::restart_active_session` so the headless and TUI paths agree.
pub(crate) fn resume_id_if_transcript_exists(
    agent_session_id: &str,
    env: &HashMap<String, String>,
) -> Option<String> {
    let config_dir_override = env.get("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from);
    crate::paths::claude_transcript_exists(agent_session_id, config_dir_override.as_deref())
        .then(|| agent_session_id.to_string())
}

/// Decide the `resume_session_id` to use when restarting a session, given the
/// agent's definition.
///
/// - Agents that resume "the latest session in the launch directory"
///   ([`AgentDef::resumes_latest`]) get the session id back as a non-`None`
///   *trigger*: their `resume_args` are id-less (no `{id}` token), so the value
///   itself is ignored — its presence is what makes [`AgentDef::build_args`]
///   emit the resume group. Restart always reuses the session's directory, so
///   the agent's own "last in cwd" resolution targets the right conversation.
/// - Everyone else (claude) falls back to the transcript check, which returns
///   the pinned id only when a resumable transcript exists on disk.
///
/// Shared by the headless restart path and `App`'s restart/restore paths so
/// they agree on when to resume vs. start fresh.
pub(crate) fn resume_trigger_for(
    def: &crate::session::AgentDef,
    agent_session_id: &str,
    env: &HashMap<String, String>,
) -> Option<String> {
    if def.resumes_latest() {
        return Some(agent_session_id.to_string());
    }
    resume_id_if_transcript_exists(agent_session_id, env)
}

/// Resolve the [`AgentDef`](crate::session::AgentDef) for an agent name from
/// the on-disk registry, mirroring [`build_agent_invocation`]'s fallback chain
/// (named agent → registry default → built-in default).
pub(crate) fn resolve_agent_def(agent: &str) -> crate::session::AgentDef {
    let registry = crate::agent::agent_config::load_or_seed();
    registry
        .get(agent)
        .or_else(|| registry.default_agent())
        .cloned()
        .unwrap_or_else(|| {
            crate::agent::agent_config::builtin_registry()
                .default_agent()
                .cloned()
                .expect("built-in registry always has a default agent")
        })
}

/// Build the `(command, args)` invocation for the agent named by
/// `config.agent`, looked up in the on-disk agent registry (falling back to
/// the registry default, then to the built-in default).
///
/// Centralised here so headless spawn and restart agree on the args.
fn build_agent_invocation(config: &SessionConfig) -> (String, Vec<String>) {
    let def = resolve_agent_def(&config.agent);
    let provider = crate::agent::GenericProvider::new(def);
    // Reach the provider trait methods via fully-qualified call syntax so this
    // module imports nothing from the agent module (architecture rule:
    // session_ops must stay free of agent-layer imports).
    let command =
        <crate::agent::GenericProvider as crate::agent::AgentProvider>::command(&provider)
            .to_string();
    let args = <crate::agent::GenericProvider as crate::agent::AgentProvider>::build_args(
        &provider, config,
    );
    (command, args)
}

/// Inject the standard thurbox env hints (`THURBOX_SESSION_ID`,
/// `THURBOX_METRICS_DIR`) into a session config.
///
/// Mirrors `App::do_spawn_session` so headless and TUI sessions look
/// identical to the spawned process.
fn inject_thurbox_env(config: &mut SessionConfig, agent_session_id: &str) {
    config
        .env
        .insert("THURBOX_SESSION_ID".into(), agent_session_id.into());
    if let Some(dir) = crate::paths::metrics_directory() {
        config
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
        let mut env = HashMap::new();
        env.insert("CLAUDE_CONFIG_DIR".into(), tmp.path().display().to_string());
        assert_eq!(resume_id_if_transcript_exists("some-uuid", &env), None);
    }

    #[test]
    fn resume_id_is_some_when_transcript_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "77777777-8888-9999-aaaa-bbbbbbbbbbbb";
        let proj = tmp.path().join("projects").join("-slug");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{sid}.jsonl")), b"").unwrap();

        let mut env = HashMap::new();
        env.insert("CLAUDE_CONFIG_DIR".into(), tmp.path().display().to_string());
        assert_eq!(
            resume_id_if_transcript_exists(sid, &env),
            Some(sid.to_string())
        );
    }

    #[test]
    fn resume_trigger_latest_agent_always_triggers() {
        // A resume_latest agent (codex) triggers resume regardless of any
        // on-disk claude transcript; the returned id is just the trigger.
        let codex = crate::agent::agent_config::builtin_registry()
            .get("codex")
            .unwrap()
            .clone();
        assert!(codex.resumes_latest());
        let env = HashMap::new();
        assert_eq!(
            resume_trigger_for(&codex, "thurbox-uuid", &env),
            Some("thurbox-uuid".to_string())
        );
    }

    #[test]
    fn resume_trigger_claude_defers_to_transcript() {
        // claude is not resume_latest, so it only resumes when a transcript
        // exists — same behaviour as resume_id_if_transcript_exists.
        let claude = crate::agent::agent_config::builtin_registry()
            .get("claude")
            .unwrap()
            .clone();
        assert!(!claude.resumes_latest());

        let tmp = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert("CLAUDE_CONFIG_DIR".into(), tmp.path().display().to_string());
        // No transcript yet -> no resume.
        assert_eq!(resume_trigger_for(&claude, "missing", &env), None);

        // With a transcript -> resume by the pinned id.
        let sid = "11111111-2222-3333-4444-555555555555";
        let proj = tmp.path().join("projects").join("-slug");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{sid}.jsonl")), b"").unwrap();
        assert_eq!(
            resume_trigger_for(&claude, sid, &env),
            Some(sid.to_string())
        );
    }
}
