//! Loading and seeding of the agent-definition config file.
//!
//! Agents are defined declaratively in `~/.config/thurbox/agents.toml`. On
//! first run (or whenever the file is missing) the built-in definitions are
//! written out so users have a working starting point they can edit. If the
//! file exists but cannot be read or parsed, we fall back to the built-ins
//! rather than failing to start.

use std::path::PathBuf;

use crate::session::AgentRegistry;

/// Built-in agent definitions, also used to seed `agents.toml` on first run.
///
/// Kept deliberately small per agent: just the command, plus resume/fork/
/// session-id groups. `claude` pins a thurbox-generated id (`--session-id`) so
/// it can resume/fork by that exact id. The other built-ins can't pin or report
/// their session id, so they use `resume_latest = true` with id-less,
/// cwd-scoped flags (`codex resume --last`, `opencode --continue`, …): the agent
/// resolves "the last session in this directory" itself. Agents without any
/// resume group simply start fresh on restart. No model is passed — each agent
/// uses its own default config. Bake extra flags (including a model) into
/// `args` if you want them.
pub const BUILTIN_AGENTS_TOML: &str = r#"# Thurbox coding-agent definitions.
#
# Each [[agents]] entry describes how to launch one coding-agent CLI. The
# `*_args` groups are appended only when their value is present, with {id}
# substituted. `args` is always passed — put any extra flags (e.g. a model)
# there. Add your own [[agents]] entries to support any CLI.

default = "claude"

[[agents]]
name = "claude"
command = "claude"
resume_args = ["--resume", "{id}"]
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]

# codex can't pin or report its session id, so resume/fork target the most
# recent session in the launch directory. thurbox keeps that directory stable
# across restart (same cwd) and single-repo fork (child reuses the parent cwd).
[[agents]]
name = "codex"
command = "codex"
resume_args = ["resume", "--last"]
fork_args = ["fork", "--last"]
resume_latest = true

# gemini resumes the latest session in the launch directory; it has no fork
# (Ctrl+F falls back to a fresh session).
[[agents]]
name = "gemini"
command = "gemini"
resume_args = ["--resume", "latest"]
resume_latest = true

# `--continue` resumes the last session in the cwd; add `--fork` to branch it.
[[agents]]
name = "opencode"
command = "opencode"
resume_args = ["--continue"]
fork_args = ["--continue", "--fork"]
resume_latest = true

# aider restores the chat-history file (.aider.chat.history.md) in the cwd; it
# has no separate session id and no fork.
[[agents]]
name = "aider"
command = "aider"
resume_args = ["--restore-chat-history"]
resume_latest = true

[[agents]]
name = "vibe"
command = "vibe"
"#;

/// Path to the agent-definition config file:
/// `~/.config/thurbox/agents.toml` (sibling of `config.toml`).
pub fn agents_config_path() -> Option<PathBuf> {
    crate::paths::config_file().map(|p| p.with_file_name("agents.toml"))
}

/// Parse the built-in definitions. Infallible in practice (the const is a
/// valid document); falls back to an empty registry if that ever changes.
pub fn builtin_registry() -> AgentRegistry {
    toml::from_str(BUILTIN_AGENTS_TOML).unwrap_or(AgentRegistry {
        default: String::new(),
        agents: Vec::new(),
    })
}

/// Load the agent registry, seeding the config file with built-ins when it is
/// absent. Any read/parse error degrades gracefully to the built-in registry
/// so the TUI always starts with at least the bundled agents.
pub fn load_or_seed() -> AgentRegistry {
    let Some(path) = agents_config_path() else {
        tracing::warn!("Could not resolve agents.toml path; using built-in agents");
        return builtin_registry();
    };

    if !path.exists() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(error = %e, "Failed to create config dir for agents.toml");
                return builtin_registry();
            }
        }
        if let Err(e) = std::fs::write(&path, BUILTIN_AGENTS_TOML) {
            tracing::warn!(error = %e, "Failed to seed agents.toml; using built-in agents");
            return builtin_registry();
        }
        tracing::info!(path = %path.display(), "Seeded agents.toml with built-in agents");
        return builtin_registry();
    }

    match std::fs::read_to_string(&path) {
        Ok(contents) => match toml::from_str::<AgentRegistry>(&contents) {
            Ok(reg) if !reg.agents.is_empty() => reg,
            Ok(_) => {
                tracing::warn!("agents.toml has no agents; using built-in agents");
                builtin_registry()
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to parse agents.toml; using built-in agents");
                builtin_registry()
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "Failed to read agents.toml; using built-in agents");
            builtin_registry()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_parses_and_has_claude_default() {
        let reg = builtin_registry();
        assert_eq!(reg.default, "claude");
        assert!(reg.get("claude").is_some());
        assert!(reg.get("codex").is_some());
        assert!(reg.get("gemini").is_some());
        assert!(reg.get("opencode").is_some());
        assert!(reg.get("aider").is_some());
        assert!(reg.get("vibe").is_some());

        // Claude pins a thurbox id and resumes/forks by it.
        let claude = reg.get("claude").unwrap();
        assert!(!claude.resume_args.is_empty());
        assert!(!claude.resume_latest);
        assert!(claude.resume_args.iter().any(|t| t.contains("{id}")));

        // codex/opencode resume + fork via id-less, cwd-scoped flags.
        let codex = reg.get("codex").unwrap();
        assert_eq!(codex.resume_args, ["resume", "--last"]);
        assert_eq!(codex.fork_args, ["fork", "--last"]);
        assert!(codex.resume_latest);
        let opencode = reg.get("opencode").unwrap();
        assert_eq!(opencode.fork_args, ["--continue", "--fork"]);
        assert!(opencode.resume_latest);

        // gemini/aider resume their latest session but have no fork group.
        for name in ["gemini", "aider"] {
            let a = reg.get(name).unwrap();
            assert!(a.resume_latest, "{name} should resume latest");
            assert!(!a.resume_args.is_empty(), "{name} needs resume_args");
            assert!(a.fork_args.is_empty(), "{name} has no fork");
        }

        // No non-claude resume/fork token may carry a {id} placeholder — these
        // agents can't be addressed by a thurbox-known id.
        for name in ["codex", "gemini", "opencode", "aider"] {
            let a = reg.get(name).unwrap();
            assert!(
                !a.resume_args
                    .iter()
                    .chain(&a.fork_args)
                    .any(|t| t.contains("{id}")),
                "{name} must use id-less resume/fork flags"
            );
        }
    }

    #[test]
    fn load_or_seed_writes_file_when_absent_then_reads_it() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = agents_config_path().unwrap();
        assert!(!path.exists());

        let reg = load_or_seed();
        assert_eq!(reg.default, "claude");
        assert!(path.exists(), "agents.toml should have been seeded");

        // Second call reads the seeded file and yields the same registry.
        let reg2 = load_or_seed();
        assert_eq!(reg, reg2);
    }

    #[test]
    fn load_or_seed_falls_back_on_malformed_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = agents_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not = valid toml {{{").unwrap();

        let reg = load_or_seed();
        assert_eq!(reg.default, "claude");
    }

    #[test]
    fn load_or_seed_reads_custom_agent() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = agents_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "default = \"mine\"\n[[agents]]\nname = \"mine\"\ncommand = \"my-agent\"\n",
        )
        .unwrap();

        let reg = load_or_seed();
        assert_eq!(reg.default, "mine");
        assert_eq!(reg.get("mine").unwrap().command, "my-agent");
    }
}
