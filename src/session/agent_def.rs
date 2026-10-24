//! Declarative coding-agent definitions.
//!
//! A [`AgentDef`] describes how to launch one coding-agent CLI (claude, codex,
//! antigravity, opencode, aider, …) as data: the command name plus a set of
//! argument-group templates. Definitions are loaded from
//! `~/.config/thurbox/agents.toml` (see [`crate::agent::agent_config`]) and
//! seeded with built-ins on first run, so users can register custom agents
//! without recompiling.
//!
//! This module is pure data + pure logic (no filesystem, no local imports
//! beyond serde/std) to satisfy the `session/` architecture rule. The TOML
//! loading and the `AgentProvider` bridge live in `crate::agent`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Placeholder substituted with a session id in resume/fork/new-session groups.
/// Exposed so `session_ops` can reconstruct a path-pinned agent's session path
/// (omp) using the *same* token [`AgentDef::build_args`] substitutes.
pub const ID_PLACEHOLDER: &str = "{id}";

/// Substituted with `[telemetry] endpoint` in `telemetry_env` /
/// `telemetry_args`.
pub const OTLP_ENDPOINT_PLACEHOLDER: &str = "{otlp_endpoint}";
/// Substituted with `[telemetry] protocol` in `telemetry_env` /
/// `telemetry_args`.
pub const OTLP_PROTOCOL_PLACEHOLDER: &str = "{otlp_protocol}";

/// One coding-agent CLI definition.
///
/// Resume, fork and new-session groups are appended **only** when their
/// driving id is present, with
/// `{id}` substituted token-by-token. This avoids any "unresolved placeholder"
/// heuristics: a group with no value is simply omitted. Directory arguments
/// expand separately for the selected members, substituting `{dir}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDef {
    /// Display + lookup name (e.g. `"claude"`). Unique within a registry.
    pub name: String,
    /// CLI executable to run (e.g. `"claude"`, `"opencode"`).
    pub command: String,
    /// Static arguments always passed. Bake a
    /// model or any other flag here if you want one (e.g. `["--model", "opus"]`).
    #[serde(default)]
    pub args: Vec<String>,
    /// Repeated for each multi-repo member, substituting `{dir}` with its
    /// launch-host path. Empty for CLIs that need no explicit directory scope.
    #[serde(default)]
    pub additional_dir_args: Vec<String>,
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
    /// (e.g. `opencode --continue`). Agents that pin ids
    /// (claude) leave this `false` and resume by a thurbox-known id instead.
    #[serde(default)]
    pub resume_latest: bool,
    /// Names the hook *family* this CLI understands, letting the built-in
    /// `hooks` extension wire status hooks for a **custom** agent as if it were
    /// that built-in. A rebranded-claude agent sets `hook_schema = "claude"` so
    /// the same `--settings` patch (and remote rewrite) that targets the
    /// built-in `claude` also targets it. `None` = wire only if this agent's own
    /// `name` matches a built-in family. thurbox bakes in no agent knowledge —
    /// the *user* asserts the family here. See [`crate::agent::extension_config`]
    /// (`apply_agent_patches`) and `extensions/hooks/`.
    #[serde(default)]
    pub hook_schema: Option<String>,
    /// Environment set only while `[telemetry]` is on, for an agent whose OTel
    /// switch is not the spec's own (`CLAUDE_CODE_ENABLE_TELEMETRY`).
    /// `{otlp_endpoint}` and `{otlp_protocol}` are substituted. A variable the
    /// session's own `--env` sets wins.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub telemetry_env: BTreeMap<String, String>,
    /// Arguments appended only while `[telemetry]` is on, with the same
    /// substitution — for an agent configured by flags rather than env (codex's
    /// per-session `-c otel.*` overrides).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub telemetry_args: Vec<String>,
}

impl AgentDef {
    /// The registered name or an asserted/inherited hook family.
    pub fn is_family(&self, family: &str) -> bool {
        self.name == family || self.hook_schema.as_deref() == Some(family)
    }

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

    /// [`Self::telemetry_env`] and [`Self::telemetry_args`] with
    /// [`OTLP_ENDPOINT_PLACEHOLDER`] and [`OTLP_PROTOCOL_PLACEHOLDER`]
    /// substituted.
    pub fn telemetry_launch(
        &self,
        endpoint: &str,
        protocol: &str,
    ) -> (BTreeMap<String, String>, Vec<String>) {
        let fill = |t: &str| {
            t.replace(OTLP_ENDPOINT_PLACEHOLDER, endpoint)
                .replace(OTLP_PROTOCOL_PLACEHOLDER, protocol)
        };
        let env = self
            .telemetry_env
            .iter()
            .map(|(k, v)| (k.clone(), fill(v)))
            .collect();
        let args = self.telemetry_args.iter().map(|a| fill(a)).collect();
        (env, args)
    }

    /// Directory-scope arguments, separate from conversation-selection args.
    /// Omit non-UTF8 paths rather than granting a different, lossy path.
    pub fn build_directory_args(&self, dirs: &[std::path::PathBuf]) -> Vec<String> {
        dirs.iter()
            .filter_map(|dir| dir.to_str())
            .flat_map(|dir| subst(&self.additional_dir_args, "{dir}", dir))
            .collect()
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
///
/// Unknown fields are tolerated but reported: the loader names every
/// unrecognized key in a startup warning (stale keys from older versions and
/// typos both surface without stranding the user on built-ins).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRegistry {
    /// Config-format version, for future migrations. Currently `1`.
    #[serde(default)]
    pub config_version: Option<u32>,
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

    #[test]
    fn directory_scope_keeps_native_paths_as_individual_arguments() {
        let def = AgentDef {
            additional_dir_args: vec!["--add-dir".into(), "{dir}".into()],
            ..Default::default()
        };
        let paths = [
            std::path::PathBuf::from(r"C:\projects\member space"),
            std::path::PathBuf::from("/repos/member space"),
        ];
        assert_eq!(
            def.build_directory_args(&paths),
            [
                "--add-dir",
                r"C:\projects\member space",
                "--add-dir",
                "/repos/member space"
            ]
        );
        assert!(def.build_directory_args(&[]).is_empty());
        assert!(AgentDef::default().build_directory_args(&paths).is_empty());
    }

    fn claude() -> AgentDef {
        AgentDef {
            name: "claude".into(),
            command: "claude".into(),
            args: vec![],
            resume_args: vec!["--resume".into(), "{id}".into()],
            fork_args: vec!["--resume".into(), "{id}".into(), "--fork-session".into()],
            new_session_args: vec!["--session-id".into(), "{id}".into()],
            resume_latest: false,
            hook_schema: None,
            telemetry_env: Default::default(),
            telemetry_args: Vec::new(),
            additional_dir_args: Vec::new(),
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
            hook_schema: None,
            telemetry_env: Default::default(),
            telemetry_args: Vec::new(),
            additional_dir_args: Vec::new(),
        };
        let args = d.build_args(None, None, Some("ignored"));
        assert_eq!(args, vec!["--quiet"]);
    }

    #[test]
    fn idless_resume_and_fork_pass_tokens_verbatim() {
        // A custom agent may still use id-less resume/fork groups.
        let d = AgentDef {
            name: "custom".into(),
            command: "custom".into(),
            args: vec![],
            resume_args: vec!["resume".into(), "--last".into()],
            fork_args: vec!["fork".into(), "--last".into()],
            new_session_args: vec![],
            resume_latest: true,
            hook_schema: None,
            telemetry_env: Default::default(),
            telemetry_args: Vec::new(),
            additional_dir_args: Vec::new(),
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
            hook_schema: None,
            telemetry_env: Default::default(),
            telemetry_args: Vec::new(),
            additional_dir_args: Vec::new(),
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
            config_version: None,
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
                    hook_schema: None,
                    telemetry_env: Default::default(),
                    telemetry_args: Vec::new(),
                    additional_dir_args: Vec::new(),
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
            config_version: None,
            default: "missing".into(),
            agents: vec![claude()],
        };
        assert_eq!(reg.default_agent().unwrap().name, "claude");
    }
}
