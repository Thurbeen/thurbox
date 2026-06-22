//! Headless session operations — spawn and restart sessions without the TUI.
//!
//! Callers (MCP, CLI) use these helpers to drive the same local-tmux-backed
//! sessions the TUI manages, without requiring the TUI event loop. All
//! operations are synchronous against the SQLite database and the `tmux -L
//! thurbox` server.

pub mod builtin_hooks;
pub mod delete;
pub mod extensions;
pub mod restart;
pub mod spawn;

pub use builtin_hooks::{ensure_builtin_hooks_extension, HOOKS_EXTENSION_NAME};
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

use crate::session::{AutomationRunStatus, SessionConfig};

/// Run an `Exec` automation's shell command headlessly (`sh -c`, or `cmd /C` on
/// Windows) and report its outcome for the run history. No session/agent is
/// involved — this is the deterministic-scheduled-job path shared by the TUI and
/// the headless `automation tick`. stdout+stderr are tail-truncated so a chatty
/// command can't bloat the history.
pub fn run_exec_command(command: &str) -> (AutomationRunStatus, String) {
    use std::process::Command;
    let result = if cfg!(windows) {
        Command::new("cmd").args(["/C", command]).output()
    } else {
        Command::new("sh").args(["-c", command]).output()
    };
    let out = match result {
        Ok(out) => out,
        Err(e) => return (AutomationRunStatus::Error, format!("spawn failed: {e}")),
    };
    // Keep the last ~500 chars of each stream.
    let tail = |s: &[u8]| -> String {
        let t = String::from_utf8_lossy(s);
        let t = t.trim();
        let n = t.chars().count();
        if n > 500 {
            t.chars().skip(n - 500).collect()
        } else {
            t.to_string()
        }
    };
    let stdout = tail(&out.stdout);
    let stderr = tail(&out.stderr);
    let mut detail = stdout;
    if !stderr.is_empty() {
        if !detail.is_empty() {
            detail.push('\n');
        }
        detail.push_str(&stderr);
    }
    if out.status.success() {
        let msg = if detail.is_empty() {
            "ok".into()
        } else {
            detail
        };
        (AutomationRunStatus::Success, msg)
    } else {
        let code = out.status.code().map_or("signal".into(), |c| c.to_string());
        let msg = if detail.is_empty() {
            format!("exit {code}")
        } else {
            format!("exit {code}: {detail}")
        };
        (AutomationRunStatus::Error, msg)
    }
}

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

/// Inject the standard thurbox env hints into a session config so a
/// `thurbox-cli` call running *inside* the session can prove its own identity
/// without scraping panes or names:
///
/// - `THURBOX_SESSION` — the thurbox [`SessionId`] (the registry key). Read by
///   the mailbox CLI to auto-stamp provenance and default the inbox to "me".
///   Requires `config.session_id` to be set before calling.
/// - `THURBOX_SESSION_ID` — the *agent's* conversation id (`agent_session_id`),
///   consumed by the metrics statusline. Distinct from `THURBOX_SESSION`.
/// - `THURBOX_TASK` — the originating task id, when this session was spawned for
///   a task (so messages auto-tag `from_task_id`). Headless `task run` only; the
///   TUI task-spawn path tracks the link in-memory instead.
/// - `THURBOX_METRICS_DIR` — metrics output dir.
/// - `THURBOX_CONFIG_DIR` / `THURBOX_DATA_DIR` — the resolved config/data dirs,
///   so the agent's `thurbox-cli` (its status hook) targets the same DB the TUI
///   reads regardless of XDG / PATH / a stale tmux-server env.
///
/// Kept in sync with `App::build_spawn_inputs` so headless and TUI sessions look
/// identical to the spawned process (modulo `THURBOX_TASK` as noted above).
///
/// Shared by the headless spawn/restart paths and the TUI `Ctrl+R` restart
/// (`App::restart_active_session`), so a restarted session keeps the same
/// identity env a fresh spawn would have had.
pub(crate) fn inject_thurbox_env(
    config: &mut SessionConfig,
    agent_session_id: &str,
    task_id: Option<i64>,
) {
    config
        .env
        .insert("THURBOX_SESSION_ID".into(), agent_session_id.into());
    if let Some(id) = config.session_id {
        config.env.insert("THURBOX_SESSION".into(), id.to_string());
    }
    if let Some(task_id) = task_id {
        config
            .env
            .insert("THURBOX_TASK".into(), task_id.to_string());
    }
    if let Some(dir) = crate::paths::metrics_directory() {
        config
            .env
            .insert("THURBOX_METRICS_DIR".into(), dir.to_string_lossy().into());
    }
    // Pin the agent's `thurbox-cli` (its status hook) to the *same* config/data
    // dirs this thurbox resolved, so a status `signal` always lands in the DB
    // the TUI reads — independent of XDG, which `thurbox-cli` is on PATH, or a
    // stale tmux-server env. Derived from the resolved file paths' parents.
    if let Some(dir) = crate::paths::config_file().and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        config.env.insert(
            crate::paths::CONFIG_DIR_OVERRIDE_ENV.into(),
            dir.to_string_lossy().into(),
        );
    }
    if let Some(dir) =
        crate::paths::database_file().and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        config.env.insert(
            crate::paths::DATA_DIR_OVERRIDE_ENV.into(),
            dir.to_string_lossy().into(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;

    #[cfg(unix)]
    #[test]
    fn run_exec_command_reports_success_and_failure() {
        // A zero-exit command → Success, with stdout captured.
        let (status, detail) = run_exec_command("printf hello");
        assert_eq!(status, AutomationRunStatus::Success);
        assert!(detail.contains("hello"), "got {detail}");

        // A non-zero exit → Error, with the exit code in the detail.
        let (status, detail) = run_exec_command("exit 3");
        assert_eq!(status, AutomationRunStatus::Error);
        assert!(detail.contains('3'), "got {detail}");

        // No output on success collapses to a friendly "ok".
        let (status, detail) = run_exec_command("true");
        assert_eq!(status, AutomationRunStatus::Success);
        assert_eq!(detail, "ok");
    }

    #[test]
    fn inject_env_sets_identity_and_task() {
        let sid = SessionId::default();
        let mut config = SessionConfig {
            session_id: Some(sid),
            ..SessionConfig::default()
        };
        inject_thurbox_env(&mut config, "agent-conv-uuid", Some(42));
        // The thurbox session key and the agent conversation id are distinct.
        assert_eq!(config.env.get("THURBOX_SESSION"), Some(&sid.to_string()));
        assert_eq!(
            config.env.get("THURBOX_SESSION_ID"),
            Some(&"agent-conv-uuid".to_string())
        );
        assert_eq!(config.env.get("THURBOX_TASK"), Some(&"42".to_string()));
    }

    #[test]
    fn inject_env_pins_config_and_data_dirs() {
        // The agent's status hook must target the same DB the TUI reads, so the
        // resolved config/data dirs are injected for `thurbox-cli` to honour.
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let mut config = SessionConfig {
            session_id: Some(SessionId::default()),
            ..SessionConfig::default()
        };
        inject_thurbox_env(&mut config, "agent-conv-uuid", None);

        let cfg_dir = config
            .env
            .get(crate::paths::CONFIG_DIR_OVERRIDE_ENV)
            .expect("config dir injected");
        let data_dir = config
            .env
            .get(crate::paths::DATA_DIR_OVERRIDE_ENV)
            .expect("data dir injected");
        // They match the parents of the resolved config/db files.
        assert_eq!(
            Some(std::path::Path::new(cfg_dir)),
            crate::paths::config_file()
                .as_deref()
                .and_then(|p| p.parent())
        );
        assert_eq!(
            Some(std::path::Path::new(data_dir)),
            crate::paths::database_file()
                .as_deref()
                .and_then(|p| p.parent())
        );
    }

    #[test]
    fn inject_env_omits_task_when_absent() {
        let mut config = SessionConfig {
            session_id: Some(SessionId::default()),
            ..SessionConfig::default()
        };
        inject_thurbox_env(&mut config, "agent-conv-uuid", None);
        assert!(config.env.contains_key("THURBOX_SESSION"));
        assert!(!config.env.contains_key("THURBOX_TASK"));
    }

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
