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
#
# Unknown keys are reported on startup (and fail `thurbox-cli config
# validate`) but don't break the load — your agents stay in effect.

config_version = 1
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
        config_version: None,
        default: String::new(),
        agents: Vec::new(),
    })
}

/// Load the agent registry, seeding the config file with built-ins when it is
/// absent. Any read/parse error degrades gracefully to the built-in registry
/// so the TUI always starts with at least the bundled agents; the warnings are
/// logged here (headless callers) — the TUI uses
/// [`load_or_seed_with_warnings`] to surface them in the status bar too.
pub fn load_or_seed() -> AgentRegistry {
    let (registry, warnings) = load_or_seed_with_warnings();
    for w in &warnings {
        tracing::warn!("{w}");
    }
    registry
}

/// [`load_or_seed`], also returning user-facing warnings for anything that
/// silently degraded (parse error → built-ins, seed failure, …).
pub fn load_or_seed_with_warnings() -> (AgentRegistry, Vec<String>) {
    let Some(path) = agents_config_path() else {
        return (
            builtin_registry(),
            vec!["Could not resolve agents.toml path; using built-in agents".into()],
        );
    };

    if !path.exists() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return (
                    builtin_registry(),
                    vec![format!("Failed to create config dir for agents.toml: {e}")],
                );
            }
        }
        if let Err(e) = std::fs::write(&path, BUILTIN_AGENTS_TOML) {
            return (
                builtin_registry(),
                vec![format!("Failed to seed agents.toml: {e}")],
            );
        }
        tracing::info!(path = %path.display(), "Seeded agents.toml with built-in agents");
        return (builtin_registry(), Vec::new());
    }

    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            match parse_toml_reporting_unknown::<AgentRegistry>(&contents, "agents.toml") {
                Ok((reg, warnings)) if !reg.agents.is_empty() => (reg, warnings),
                Ok(_) => (
                    builtin_registry(),
                    vec!["agents.toml has no agents; using built-in agents".into()],
                ),
                Err(e) => (
                    builtin_registry(),
                    vec![format!(
                        "agents.toml: {}; using built-in agents",
                        compact_toml_error(&e.to_string())
                    )],
                ),
            }
        }
        Err(e) => (
            builtin_registry(),
            vec![format!("Failed to read agents.toml: {e}")],
        ),
    }
}

/// Parse a TOML config document leniently, reporting every unknown field by
/// path instead of failing on it. Stale keys from older thurbox versions and
/// typos both surface as warnings without stranding the user on defaults; a
/// real syntax/type error still fails the parse.
pub(crate) fn parse_toml_reporting_unknown<T: serde::de::DeserializeOwned>(
    contents: &str,
    file_label: &str,
) -> Result<(T, Vec<String>), toml::de::Error> {
    let mut warnings = Vec::new();
    let de = toml::de::Deserializer::parse(contents)?;
    let value = serde_ignored::deserialize(de, |path| {
        warnings.push(format!("{file_label}: unknown field `{path}` (ignored)"));
    })?;
    Ok((value, warnings))
}

/// Collapse a (possibly multi-line) toml error to "<position>: <message>" for
/// compact status-bar display. toml errors render as a header line with the
/// position, a source snippet, then the message — keep the first and last
/// meaningful lines and drop the snippet in between.
pub(crate) fn compact_toml_error(s: &str) -> String {
    let lines: Vec<&str> = s
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('|') && !l.starts_with(char::is_numeric))
        .collect();
    match (lines.first(), lines.last()) {
        (Some(first), Some(last)) if first != last => format!("{first}: {last}"),
        (Some(first), _) => (*first).to_string(),
        _ => s.to_string(),
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
    fn load_or_seed_reports_unknown_field_but_keeps_agents() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = agents_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Typo'd field: `resumeargs` instead of `resume_args`. The user's
        // agents must stay in effect (stale keys from older thurbox versions
        // are common); the warning names the bad key.
        std::fs::write(
            &path,
            "default = \"mine\"\n[[agents]]\nname = \"mine\"\ncommand = \"x\"\nresumeargs = []\n",
        )
        .unwrap();

        let (reg, warnings) = load_or_seed_with_warnings();
        assert_eq!(reg.default, "mine", "user agents must stay in effect");
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("resumeargs"),
            "warning must name the unknown field: {}",
            warnings[0]
        );
    }

    #[test]
    fn compact_toml_error_keeps_position_and_message() {
        // A type error (string field given an integer) still fails the parse.
        let err = toml::from_str::<AgentRegistry>("default = 1\n").unwrap_err();
        let compact = compact_toml_error(&err.to_string());
        assert!(compact.contains("string"), "got: {compact}");
        assert!(!compact.contains('\n'), "must be one line: {compact}");
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
