//! Declarative coding-agent definitions.
//!
//! A [`AgentDef`] describes how to launch one coding-agent CLI (claude, codex,
//! gemini, opencode, aider, …) as data: the command name plus a set of
//! argument-group templates. Definitions are loaded from
//! `~/.config/thurbox/agents.toml` (see [`crate::agent::agent_config`]) and
//! seeded with built-ins on first run, so users can register custom agents
//! without recompiling.
//!
//! This module is pure data + pure logic (no filesystem, no local imports
//! beyond serde/std) to satisfy the `session/` architecture rule. The TOML
//! loading and the `AgentProvider` bridge live in `crate::agent`.

use serde::{Deserialize, Serialize};

/// Placeholder substituted with a session id in resume/fork/new-session groups.
const ID_PLACEHOLDER: &str = "{id}";

/// One coding-agent CLI definition.
///
/// Each `*_args` group is appended to the final argument list **only** when its
/// driving value is present (a model is selected, the session is being resumed,
/// etc.), with `{model}` / `{id}` substituted token-by-token. This avoids any
/// "unresolved placeholder" heuristics: a group with no value is simply omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDef {
    /// Display + lookup name (e.g. `"claude"`). Unique within a registry.
    pub name: String,
    /// CLI executable to run (e.g. `"claude"`, `"opencode"`).
    pub command: String,
    /// Static arguments always passed, before any templated group. Bake a
    /// model or any other flag here if you want one (e.g. `["--model", "opus"]`).
    #[serde(default)]
    pub args: Vec<String>,
    /// Emitted when resuming a known session; `{id}` is substituted.
    #[serde(default)]
    pub resume_args: Vec<String>,
    /// Emitted when forking from a parent conversation; `{id}` is substituted.
    #[serde(default)]
    pub fork_args: Vec<String>,
    /// Emitted on a fresh spawn to pin a session id; `{id}` is substituted.
    #[serde(default)]
    pub new_session_args: Vec<String>,
    /// When true, this agent resumes its most-recent session in the launch
    /// directory using id-less `resume_args` (no `{id}` token). thurbox cannot
    /// pin or read back the agent's real session id for these CLIs, so restart
    /// relies on the agent's own "last session in this directory" resolution
    /// (e.g. `codex resume --last`, `opencode --continue`). Agents that pin ids
    /// (claude) leave this `false` and resume by a thurbox-known id instead.
    #[serde(default)]
    pub resume_latest: bool,
}

impl AgentDef {
    /// Build the CLI argument list for one launch.
    ///
    /// Session-selection precedence mirrors the historical Claude behaviour:
    /// fork wins over resume, which wins over a fresh `new_session` id. After
    /// the selection group come the static `args`. No model is ever passed —
    /// the agent uses its own default config (bake one into `args` if needed).
    pub fn build_args(
        &self,
        resume_id: Option<&str>,
        fork_id: Option<&str>,
        new_session_id: Option<&str>,
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();

        if let Some(id) = fork_id {
            out.extend(subst(&self.fork_args, ID_PLACEHOLDER, id));
        } else if let Some(id) = resume_id {
            out.extend(subst(&self.resume_args, ID_PLACEHOLDER, id));
        } else if let Some(id) = new_session_id {
            out.extend(subst(&self.new_session_args, ID_PLACEHOLDER, id));
        }

        out.extend(self.args.iter().cloned());

        out
    }

    /// Whether a restart should trigger this agent's resume group via
    /// "latest session in the launch directory" semantics rather than a
    /// thurbox-known session id. True only when [`Self::resume_latest`] is set
    /// and there are `resume_args` to emit.
    pub fn resumes_latest(&self) -> bool {
        self.resume_latest && !self.resume_args.is_empty()
    }
}

/// Replace `placeholder` with `value` in every token of `tokens`.
fn subst(tokens: &[String], placeholder: &str, value: &str) -> Vec<String> {
    tokens
        .iter()
        .map(|t| t.replace(placeholder, value))
        .collect()
}

/// A set of agent definitions plus the name of the default agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRegistry {
    /// Name of the agent selected by default in the picker / headless spawns.
    #[serde(default)]
    pub default: String,
    /// All known agent definitions, in display order.
    #[serde(default)]
    pub agents: Vec<AgentDef>,
}

impl AgentRegistry {
    /// Look up an agent definition by name.
    pub fn get(&self, name: &str) -> Option<&AgentDef> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// The default agent definition: the one named by `default`, falling back
    /// to the first defined agent.
    pub fn default_agent(&self) -> Option<&AgentDef> {
        self.get(&self.default).or_else(|| self.agents.first())
    }

    /// The default agent's name, or empty string when the registry is empty.
    pub fn default_name(&self) -> String {
        self.default_agent()
            .map(|a| a.name.clone())
            .unwrap_or_default()
    }

    /// All agent names in display order.
    pub fn names(&self) -> Vec<&str> {
        self.agents.iter().map(|a| a.name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> AgentDef {
        AgentDef {
            name: "claude".into(),
            command: "claude".into(),
            args: vec![],
            resume_args: vec!["--resume".into(), "{id}".into()],
            fork_args: vec!["--resume".into(), "{id}".into(), "--fork-session".into()],
            new_session_args: vec!["--session-id".into(), "{id}".into()],
            resume_latest: false,
        }
    }

    #[test]
    fn fresh_session_pins_id() {
        let d = claude();
        let args = d.build_args(None, None, Some("new-id"));
        assert_eq!(args, vec!["--session-id", "new-id"]);
        // No model is ever passed.
        assert!(!args.iter().any(|a| a == "--model"));
    }

    #[test]
    fn resume_takes_precedence_over_new() {
        let d = claude();
        let args = d.build_args(Some("resume-id"), None, Some("new-id"));
        assert_eq!(args, vec!["--resume", "resume-id"]);
    }

    #[test]
    fn fork_takes_precedence_over_resume() {
        let d = claude();
        let args = d.build_args(Some("resume-id"), Some("fork-id"), Some("new-id"));
        assert_eq!(args, vec!["--resume", "fork-id", "--fork-session"]);
    }

    #[test]
    fn static_args_only_when_no_session_group() {
        let d = AgentDef {
            name: "codex".into(),
            command: "codex".into(),
            args: vec!["--quiet".into()],
            resume_args: vec![],
            fork_args: vec![],
            new_session_args: vec![],
            resume_latest: false,
        };
        let args = d.build_args(None, None, Some("ignored"));
        assert_eq!(args, vec!["--quiet"]);
    }

    #[test]
    fn idless_resume_and_fork_pass_tokens_verbatim() {
        // Mirrors the seeded codex definition: id-less resume/fork groups that
        // resolve "latest in cwd" inside the agent and ignore any supplied id.
        let d = AgentDef {
            name: "codex".into(),
            command: "codex".into(),
            args: vec![],
            resume_args: vec!["resume".into(), "--last".into()],
            fork_args: vec!["fork".into(), "--last".into()],
            new_session_args: vec![],
            resume_latest: true,
        };
        // resume id present, but no {id} token -> tokens unchanged.
        assert_eq!(
            d.build_args(Some("ignored-uuid"), None, None),
            vec!["resume", "--last"]
        );
        // fork wins over resume, still id-less.
        assert_eq!(
            d.build_args(Some("ignored-uuid"), Some("also-ignored"), None),
            vec!["fork", "--last"]
        );
        assert!(d.resumes_latest());
    }

    #[test]
    fn resumes_latest_requires_flag_and_resume_args() {
        let mut d = AgentDef {
            name: "x".into(),
            command: "x".into(),
            args: vec![],
            resume_args: vec![],
            fork_args: vec![],
            new_session_args: vec![],
            resume_latest: true,
        };
        // Flag set but no resume_args -> nothing to emit, so not "resumes latest".
        assert!(!d.resumes_latest());
        d.resume_args = vec!["--continue".into()];
        assert!(d.resumes_latest());
        d.resume_latest = false;
        assert!(!d.resumes_latest());
    }

    #[test]
    fn registry_lookup_and_default() {
        let reg = AgentRegistry {
            default: "codex".into(),
            agents: vec![
                claude(),
                AgentDef {
                    name: "codex".into(),
                    command: "codex".into(),
                    args: vec![],
                    resume_args: vec![],
                    fork_args: vec![],
                    new_session_args: vec![],
                    resume_latest: false,
                },
            ],
        };
        assert_eq!(reg.get("claude").unwrap().command, "claude");
        assert_eq!(reg.default_agent().unwrap().name, "codex");
        assert_eq!(reg.names(), vec!["claude", "codex"]);
    }

    #[test]
    fn registry_default_falls_back_to_first() {
        let reg = AgentRegistry {
            default: "missing".into(),
            agents: vec![claude()],
        };
        assert_eq!(reg.default_agent().unwrap().name, "claude");
    }
}
