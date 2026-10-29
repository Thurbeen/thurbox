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
/// Placeholder substituted with the display name captured for this launch.
pub const NAME_PLACEHOLDER: &str = "{name}";

/// Placeholder expanded to the resolved home dir at spawn, on the launch host.
/// Substituted in `session_ops` (it needs the filesystem); named here so
/// [`AgentDef::placeholder_errors`] knows where it is allowed.
pub const HOME_PLACEHOLDER: &str = "{home}";

/// Placeholder substituted with each multi-repo member in
/// `additional_dir_args`.
pub const DIR_PLACEHOLDER: &str = "{dir}";

/// A whole `args` token marking where the session-selection group (fork,
/// resume or new-session) goes. Without it the group comes first, as it always
/// has. See [`AgentDef::place_session_group`].
pub const SESSION_ARGS_PLACEHOLDER: &str = "{session_args}";

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
/// `{id}` and `{name}` substituted token-by-token. This avoids any "unresolved placeholder"
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
    /// Emitted when resuming a known session; `{id}` and `{name}` are substituted.
    #[serde(default)]
    pub resume_args: Vec<String>,
    /// Emitted when forking from a parent conversation; `{id}` and `{name}` are substituted.
    #[serde(default)]
    pub fork_args: Vec<String>,
    /// Emitted on a fresh spawn to pin a session id; `{id}` and `{name}` are substituted.
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
    /// Defaults frozen into a session's launch environment at creation.
    /// `{home}` resolves on the launch host; session `--env` overrides a key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
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
    /// Per-entry template values: `{key}` in `command`, `args`, every
    /// `*_args` group, `env`, `setup` and the telemetry fields is replaced by
    /// the value, once, when the registry is loaded
    /// ([`Self::apply_vars`]). A variant (`extends`) merges these per key
    /// over its base's, so one template entry serves several roles.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, String>,
    /// Shell commands run in order, in the session's launch directory and with
    /// its launch environment, before the agent starts. They run as one
    /// `sh -c` script under `set -e` in the agent's own pane, on the host the
    /// agent runs on; a failing step aborts the launch and the agent never
    /// starts. See [`wrap_with_setup`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<String>,
}

impl AgentDef {
    /// The registered name or an asserted/inherited hook family.
    pub fn is_family(&self, family: &str) -> bool {
        self.name == family || self.hook_schema.as_deref() == Some(family)
    }

    /// Build the CLI argument list for one launch.
    ///
    /// Session-selection precedence mirrors the historical Claude behaviour:
    /// fork wins over resume, which wins over a fresh `new_session` id. The
    /// selected group goes where [`Self::place_session_group`] puts it. No
    /// model is ever passed — the agent uses its own default config (bake one
    /// into `args` if needed).
    pub fn build_args(
        &self,
        resume_id: Option<&str>,
        fork_id: Option<&str>,
        new_session_id: Option<&str>,
        session_name: &str,
    ) -> Vec<String> {
        let group = if let Some(id) = fork_id {
            subst_launch_group(&self.fork_args, id, session_name)
        } else if let Some(id) = resume_id {
            subst_launch_group(&self.resume_args, id, session_name)
        } else if let Some(id) = new_session_id {
            subst_launch_group(&self.new_session_args, id, session_name)
        } else {
            Vec::new()
        };
        self.place_session_group(group)
    }

    /// `args` with `group` in place of its [`SESSION_ARGS_PLACEHOLDER`] token,
    /// or ahead of every arg when there is no token — the historical order, so
    /// an entry that never names the token launches exactly as before.
    ///
    /// The token exists for CLIs whose flags must precede the group (claude's
    /// `--permission-mode` before `--session-id` reads naturally) or follow a
    /// subcommand the group starts with (codex takes its flags after `resume
    /// --last`, never before it, yet before nothing on a fresh launch).
    pub fn place_session_group(&self, group: Vec<String>) -> Vec<String> {
        match self.args.iter().position(|a| a == SESSION_ARGS_PLACEHOLDER) {
            Some(at) => {
                let mut out = self.args[..at].to_vec();
                out.extend(group);
                out.extend(self.args[at + 1..].iter().cloned());
                out
            }
            None => group.into_iter().chain(self.args.iter().cloned()).collect(),
        }
    }

    /// Replace every `{key}` naming one of [`Self::vars`] in every templated
    /// field, then report what is left unresolved.
    ///
    /// One pass, and a value is inserted verbatim: a var's value may carry the
    /// launch-time placeholders (`{home}`, and `{id}` or `{name}` in a session
    /// group),
    /// which resolve later like any other, but not another var — that is left
    /// as written and reported. An unknown placeholder is an error rather than
    /// text passed to the agent: a misspelt `{rol}` would otherwise launch
    /// with a literal brace pair in its argv.
    pub fn apply_vars(&mut self) -> Result<(), Vec<String>> {
        let mut errors: Vec<String> = self
            .vars
            .keys()
            .filter_map(|name| {
                if !is_placeholder_name(name) {
                    Some(format!(
                        "vars: `{name}` is not a valid name (letters, digits and `_`, \
                         not starting with a digit)"
                    ))
                } else if RESERVED_PLACEHOLDERS.contains(&name.as_str()) {
                    Some(format!("vars: `{name}` is reserved by thurbox"))
                } else {
                    None
                }
            })
            .collect();
        if !self.vars.is_empty() {
            let vars = self.vars.clone();
            let fill = |t: &mut String| *t = fill_vars(t, &vars);
            fill(&mut self.command);
            for group in [
                &mut self.args,
                &mut self.resume_args,
                &mut self.fork_args,
                &mut self.new_session_args,
                &mut self.additional_dir_args,
                &mut self.telemetry_args,
                &mut self.setup,
            ] {
                group.iter_mut().for_each(fill);
            }
            self.env.values_mut().for_each(fill);
            self.telemetry_env.values_mut().for_each(fill);
        }
        errors.extend(self.placeholder_errors());
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Every placeholder this definition holds where nothing would substitute
    /// it, one message per field and name.
    ///
    /// What each field may name is the set its own substitution handles:
    /// `{id}` and `{name}` only in the session groups, `{dir}` only in
    /// `additional_dir_args`, the OTLP pair only in the telemetry fields,
    /// `{home}` wherever the launch expands it, and
    /// [`SESSION_ARGS_PLACEHOLDER`] only as one whole `args` token.
    pub fn placeholder_errors(&self) -> Vec<String> {
        const HOME: &[&str] = &["home"];
        const SESSION: &[&str] = &["home", "id", "name"];
        const DIR: &[&str] = &["home", "dir"];
        const TELEMETRY: &[&str] = &["otlp_endpoint", "otlp_protocol"];
        let mut errors = Vec::new();
        let mut check = |field: &str, value: &str, allowed: &[&str]| {
            for name in placeholders(value) {
                if !allowed.contains(&name) {
                    let message = if name == "session_args" {
                        format!("{field}: {{session_args}} must be a whole `args` token")
                    } else {
                        format!("{field}: unknown placeholder {{{name}}}")
                    };
                    if !errors.contains(&message) {
                        errors.push(message);
                    }
                }
            }
        };
        check("command", &self.command, HOME);
        for a in self.args.iter().filter(|a| *a != SESSION_ARGS_PLACEHOLDER) {
            check("args", a, HOME);
        }
        for (field, group, allowed) in [
            ("resume_args", &self.resume_args, SESSION),
            ("fork_args", &self.fork_args, SESSION),
            ("new_session_args", &self.new_session_args, SESSION),
            ("additional_dir_args", &self.additional_dir_args, DIR),
            ("telemetry_args", &self.telemetry_args, TELEMETRY),
            ("setup", &self.setup, HOME),
        ] {
            for token in group {
                check(field, token, allowed);
            }
        }
        for (key, value) in &self.env {
            check(&format!("env.{key}"), value, HOME);
        }
        for (key, value) in &self.telemetry_env {
            check(&format!("telemetry_env.{key}"), value, TELEMETRY);
        }
        if self
            .args
            .iter()
            .filter(|a| *a == SESSION_ARGS_PLACEHOLDER)
            .count()
            > 1
        {
            errors.push("args: {session_args} may appear only once".into());
        }
        errors
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

fn subst_launch_group(tokens: &[String], id: &str, name: &str) -> Vec<String> {
    subst(&subst(tokens, ID_PLACEHOLDER, id), NAME_PLACEHOLDER, name)
}

/// The names thurbox substitutes itself, which a `vars` entry may not shadow.
const RESERVED_PLACEHOLDERS: &[&str] = &[
    "id",
    "name",
    "home",
    "dir",
    "session_args",
    "otlp_endpoint",
    "otlp_protocol",
];

/// A placeholder name: an identifier, so TOML-ish text such as codex's
/// `-c 'projects={"/x"={trust_level="trusted"}}'` or
/// `{otlp-http={endpoint=…}}` is never mistaken for one.
fn is_placeholder_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Each `{key}` in `text` where `key` [is a placeholder
/// name](is_placeholder_name), with its byte range.
fn placeholder_spans(text: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(open) = text[from..].find('{').map(|i| i + from) {
        let rest = &text[open + 1..];
        match rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            Some(len) if rest[len..].starts_with('}') && is_placeholder_name(&rest[..len]) => {
                spans.push((open..open + len + 2, &rest[..len]));
                from = open + len + 2;
            }
            _ => from = open + 1,
        }
    }
    spans
}

fn placeholders(text: &str) -> impl Iterator<Item = &str> {
    placeholder_spans(text).into_iter().map(|(_, name)| name)
}

/// `text` with each `{key}` naming a var replaced by its value. Scanned once,
/// so a value containing a placeholder is inserted as it is.
fn fill_vars(text: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (span, name) in placeholder_spans(text) {
        if let Some(value) = vars.get(name) {
            out.push_str(&text[last..span.start]);
            out.push_str(value);
            last = span.end;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// The line the setup script prints, on the pane, when a step fails — and the
/// text a launch-failure check looks for to tell a failed setup from an agent
/// that died on its own. Followed by `<step> failed (exit <status>)`.
pub const SETUP_FAILURE_MARKER: &str = "thurbox: setup step ";

/// `$0` of the setup shell, as `ps` shows it while the setup runs.
const SETUP_ARGV0: &str = "thurbox-setup";

/// The `(command, args)` that runs `setup` and then becomes the agent.
///
/// `sh -c SCRIPT thurbox-setup COMMAND ARGS…`: the script runs each step in
/// turn under `set -e` and then `exec "$@"`, so the agent replaces the shell
/// — same pid, same pane, same environment — and anything appended to `args`
/// after this still reaches it. The steps are inlined rather than each given
/// its own `sh -c` so that `set -e` judges them as a script would: a guard
/// such as `[ -f x ] && x` does not fail the launch when `x` is absent, while
/// `x` failing does. A failing step leaves [`SETUP_FAILURE_MARKER`] and its
/// exit status on the pane and ends the shell with that status; the agent
/// never starts. No setup returns the launch unchanged.
pub fn wrap_with_setup(setup: &[String], command: &str, args: &[String]) -> (String, Vec<String>) {
    if setup.is_empty() {
        return (command.to_string(), args.to_vec());
    }
    let mut script = String::from(
        "thurbox_step=0\n\
         trap 'thurbox_status=$?; if [ \"$thurbox_status\" -ne 0 ]; then \
         printf \"\\n%s%s failed (exit %s)\\n\" \"$thurbox_marker\" \"$thurbox_step\" \
         \"$thurbox_status\" >&2; fi' EXIT\n",
    );
    script.push_str(&format!(
        "thurbox_marker='{SETUP_FAILURE_MARKER}'\nset -e\n"
    ));
    for (i, step) in setup.iter().enumerate() {
        script.push_str(&format!("thurbox_step={}\n{step}\n", i + 1));
    }
    script.push_str("trap - EXIT\nexec \"$@\"\n");
    let wrapped = [
        "-c".to_string(),
        script,
        SETUP_ARGV0.to_string(),
        command.to_string(),
    ]
    .into_iter()
    .chain(args.iter().cloned())
    .collect();
    ("sh".to_string(), wrapped)
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
            env: Default::default(),
            telemetry_env: Default::default(),
            telemetry_args: Vec::new(),
            additional_dir_args: Vec::new(),
            vars: Default::default(),
            setup: Vec::new(),
        }
    }

    #[test]
    fn fresh_session_pins_id() {
        let d = claude();
        let args = d.build_args(None, None, Some("new-id"), "worker");
        assert_eq!(args, vec!["--session-id", "new-id"]);
        // No model is ever passed.
        assert!(!args.iter().any(|a| a == "--model"));
    }

    #[test]
    fn resume_takes_precedence_over_new() {
        let d = claude();
        let args = d.build_args(Some("resume-id"), None, Some("new-id"), "worker");
        assert_eq!(args, vec!["--resume", "resume-id"]);
    }

    #[test]
    fn fork_takes_precedence_over_resume() {
        let d = claude();
        let args = d.build_args(Some("resume-id"), Some("fork-id"), Some("new-id"), "worker");
        assert_eq!(args, vec!["--resume", "fork-id", "--fork-session"]);
    }

    #[test]
    fn session_name_is_one_literal_argument_in_every_conversation_group() {
        let mut d = claude();
        d.resume_args = vec!["resume={id}".into(), "{name}".into()];
        d.fork_args = vec!["fork={id}".into(), "{name}".into()];
        d.new_session_args = vec!["new={id}".into(), "{name}".into()];
        let name = "quoted 'name' λ;$HOME *";

        assert_eq!(
            d.build_args(None, None, Some("new-id"), name),
            ["new=new-id", name]
        );
        assert_eq!(
            d.build_args(Some("resume-id"), None, None, name),
            ["resume=resume-id", name]
        );
        assert_eq!(
            d.build_args(None, Some("fork-id"), None, name),
            ["fork=fork-id", name]
        );
    }

    #[test]
    fn definitions_without_the_name_placeholder_keep_their_argv() {
        let d = claude();
        assert_eq!(
            d.build_args(None, None, Some("new-id"), "ignored"),
            ["--session-id", "new-id"]
        );
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
            env: Default::default(),
            telemetry_env: Default::default(),
            telemetry_args: Vec::new(),
            additional_dir_args: Vec::new(),
            vars: Default::default(),
            setup: Vec::new(),
        };
        let args = d.build_args(None, None, Some("ignored"), "worker");
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
            env: Default::default(),
            telemetry_env: Default::default(),
            telemetry_args: Vec::new(),
            additional_dir_args: Vec::new(),
            vars: Default::default(),
            setup: Vec::new(),
        };
        // resume id present, but no {id} token -> tokens unchanged.
        assert_eq!(
            d.build_args(Some("ignored-uuid"), None, None, "worker"),
            vec!["resume", "--last"]
        );
        // fork wins over resume, still id-less.
        assert_eq!(
            d.build_args(Some("ignored-uuid"), Some("also-ignored"), None, "worker",),
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
            env: Default::default(),
            telemetry_env: Default::default(),
            telemetry_args: Vec::new(),
            additional_dir_args: Vec::new(),
            vars: Default::default(),
            setup: Vec::new(),
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
                    env: Default::default(),
                    telemetry_env: Default::default(),
                    telemetry_args: Vec::new(),
                    additional_dir_args: Vec::new(),
                    vars: Default::default(),
                    setup: Vec::new(),
                },
            ],
        };
        assert_eq!(reg.get("claude").unwrap().command, "claude");
        assert_eq!(reg.default_agent().unwrap().name, "codex");
        assert_eq!(reg.names(), vec!["claude", "codex"]);
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn without_the_token_the_group_still_comes_first() {
        let d = AgentDef {
            args: strings(&["--settings", "/h/claude.json"]),
            ..claude()
        };
        assert_eq!(
            d.build_args(None, None, Some("new-id"), "worker"),
            ["--session-id", "new-id", "--settings", "/h/claude.json"]
        );
    }

    #[test]
    fn the_token_places_the_group_after_leading_flags() {
        let d = AgentDef {
            args: strings(&[
                "--permission-mode",
                "auto",
                "--model",
                "opus",
                "{session_args}",
                "--settings",
                "/h/claude.json",
            ]),
            ..claude()
        };
        assert_eq!(
            d.build_args(None, None, Some("new-id"), "worker"),
            [
                "--permission-mode",
                "auto",
                "--model",
                "opus",
                "--session-id",
                "new-id",
                "--settings",
                "/h/claude.json"
            ]
        );
        assert_eq!(
            d.build_args(Some("old"), None, None, "worker"),
            [
                "--permission-mode",
                "auto",
                "--model",
                "opus",
                "--resume",
                "old",
                "--settings",
                "/h/claude.json"
            ]
        );
    }

    #[test]
    fn a_codex_subcommand_group_leads_and_its_flags_follow() {
        // codex takes its flags after `resume`/`fork`, never before them, and
        // before nothing at all on a fresh launch.
        let d = AgentDef {
            name: "codex-coder".into(),
            command: "codex".into(),
            args: strings(&["{session_args}", "--approve-for-me", "--add-dir", "/skills"]),
            resume_args: strings(&["resume", "--last"]),
            fork_args: strings(&["fork", "--last"]),
            resume_latest: true,
            ..AgentDef::default()
        };
        assert_eq!(
            d.build_args(Some("x"), None, None, "worker"),
            [
                "resume",
                "--last",
                "--approve-for-me",
                "--add-dir",
                "/skills"
            ]
        );
        assert_eq!(
            d.build_args(None, Some("x"), None, "worker"),
            ["fork", "--last", "--approve-for-me", "--add-dir", "/skills"]
        );
        // No new_session_args: the token expands to nothing.
        assert_eq!(
            d.build_args(None, None, Some("x"), "worker"),
            ["--approve-for-me", "--add-dir", "/skills"]
        );
        assert_eq!(
            d.place_session_group(strings(&["fork"])),
            ["fork", "--approve-for-me", "--add-dir", "/skills"]
        );
    }

    #[test]
    fn vars_fill_every_templated_field() {
        let mut d = AgentDef {
            name: "claude-coder".into(),
            command: "{bin}".into(),
            args: strings(&["--role={role}", "{session_args}"]),
            resume_args: strings(&["--resume", "{id}", "--tag", "{role}"]),
            new_session_args: strings(&["--session-id", "{id}"]),
            setup: strings(&["[ -x scripts/role.sh ] && scripts/role.sh {role} --agent claude"]),
            env: [("ROLE".to_string(), "{role}".to_string())].into(),
            vars: [
                ("role".to_string(), "coder".to_string()),
                ("bin".to_string(), "claude".to_string()),
            ]
            .into(),
            ..AgentDef::default()
        };
        d.apply_vars().unwrap();
        assert_eq!(d.command, "claude");
        assert_eq!(d.args, ["--role=coder", "{session_args}"]);
        assert_eq!(d.resume_args, ["--resume", "{id}", "--tag", "coder"]);
        assert_eq!(
            d.setup,
            ["[ -x scripts/role.sh ] && scripts/role.sh coder --agent claude"]
        );
        assert_eq!(d.env["ROLE"], "coder");
        assert_eq!(
            d.build_args(None, None, Some("u"), "worker"),
            ["--role=coder", "--session-id", "u"]
        );
    }

    #[test]
    fn a_var_value_is_inserted_once_and_may_carry_launch_placeholders() {
        let mut d = AgentDef {
            command: "x".into(),
            args: strings(&["--settings", "{settings}"]),
            vars: [("settings".to_string(), "{home}/.config/x.json".to_string())].into(),
            ..AgentDef::default()
        };
        d.apply_vars().unwrap();
        assert_eq!(d.args, ["--settings", "{home}/.config/x.json"]);

        let mut nested = AgentDef {
            command: "x".into(),
            args: strings(&["{a}"]),
            vars: [
                ("a".to_string(), "{b}".to_string()),
                ("b".to_string(), "never".to_string()),
            ]
            .into(),
            ..AgentDef::default()
        };
        assert_eq!(
            nested.apply_vars().unwrap_err(),
            ["args: unknown placeholder {b}"]
        );
    }

    #[test]
    fn unknown_and_misplaced_placeholders_are_errors() {
        let mut d = AgentDef {
            command: "{bin}".into(),
            args: strings(&["--role", "{rol}", "{id}", "x{session_args}"]),
            additional_dir_args: strings(&["--add-dir", "{dir}"]),
            setup: strings(&["echo {dir}"]),
            env: [("A".to_string(), "{otlp_endpoint}".to_string())].into(),
            vars: [("role".to_string(), "coder".to_string())].into(),
            ..AgentDef::default()
        };
        assert_eq!(
            d.apply_vars().unwrap_err(),
            [
                "command: unknown placeholder {bin}",
                "args: unknown placeholder {rol}",
                "args: unknown placeholder {id}",
                "args: {session_args} must be a whole `args` token",
                "setup: unknown placeholder {dir}",
                "env.A: unknown placeholder {otlp_endpoint}",
            ]
        );
    }

    #[test]
    fn var_names_must_be_identifiers_and_not_reserved() {
        let mut d = AgentDef {
            command: "x".into(),
            vars: [
                ("id".to_string(), "1".to_string()),
                ("name".to_string(), "1".to_string()),
                ("a-b".to_string(), "1".to_string()),
            ]
            .into(),
            ..AgentDef::default()
        };
        let errors = d.apply_vars().unwrap_err();
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("`name` is reserved")));
        assert!(errors
            .iter()
            .any(|e| e.contains("`a-b` is not a valid name")));
        assert!(errors.iter().any(|e| e.contains("`id` is reserved")));
    }

    #[test]
    fn the_token_may_appear_only_once() {
        let d = AgentDef {
            command: "x".into(),
            args: strings(&["{session_args}", "{session_args}"]),
            ..AgentDef::default()
        };
        assert_eq!(
            d.placeholder_errors(),
            ["args: {session_args} may appear only once"]
        );
    }

    #[test]
    fn toml_shaped_braces_are_not_placeholders() {
        // codex's own `-c` values are TOML inline tables; none of these is a
        // placeholder, so none is an error and none is touched.
        let mut d = AgentDef {
            command: "codex".into(),
            args: strings(&[
                "-c",
                r#"projects={"/w"={trust_level="trusted"}}"#,
                "-c",
                "x={}",
                "{ role }",
            ]),
            telemetry_args: strings(&[
                "-c",
                r#"otel.exporter={otlp-http={endpoint="{otlp_endpoint}/v1/logs"}}"#,
            ]),
            vars: [("role".to_string(), "coder".to_string())].into(),
            ..AgentDef::default()
        };
        let before = d.clone();
        d.apply_vars().unwrap();
        assert_eq!(d.args, before.args);
        assert_eq!(d.telemetry_args, before.telemetry_args);
    }

    #[test]
    fn no_setup_leaves_the_launch_alone() {
        assert_eq!(
            wrap_with_setup(&[], "claude", &strings(&["--x"])),
            ("claude".to_string(), strings(&["--x"]))
        );
    }

    #[test]
    fn setup_becomes_a_shell_that_execs_the_agent() {
        let (command, args) = wrap_with_setup(&strings(&["true"]), "claude", &strings(&["--x"]));
        assert_eq!(command, "sh");
        assert_eq!(args[0], "-c");
        assert_eq!(&args[2..], ["thurbox-setup", "claude", "--x"]);
        assert!(args[1].ends_with("exec \"$@\"\n"));
    }

    /// The script itself, run by the real `sh`: the order, the guard, the env
    /// and the abort are its whole contract.
    #[cfg(unix)]
    mod setup_script {
        use super::*;

        fn run(setup: &[&str], tmp: &std::path::Path) -> std::process::Output {
            let (command, args) = wrap_with_setup(
                &strings(setup),
                "sh",
                &strings(&["-c", "echo agent:$ROLE:\"$1\"", "agent", "arg one"]),
            );
            std::process::Command::new(command)
                .args(args)
                .current_dir(tmp)
                .env("ROLE", "coder")
                .output()
                .unwrap()
        }

        #[test]
        fn steps_run_in_order_in_the_cwd_and_then_the_agent_runs() {
            let tmp = tempfile::tempdir().unwrap();
            let out = run(
                &[
                    "echo one > log",
                    "[ -f missing.sh ] && sh missing.sh",
                    "echo two:$ROLE >> log",
                ],
                tmp.path(),
            );
            assert!(out.status.success(), "{out:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "agent:coder:arg one\n"
            );
            assert_eq!(
                std::fs::read_to_string(tmp.path().join("log")).unwrap(),
                "one\ntwo:coder\n"
            );
        }

        #[test]
        fn a_failing_step_aborts_with_its_status_and_the_marker() {
            let tmp = tempfile::tempdir().unwrap();
            let out = run(
                &[
                    "echo one > log",
                    "echo 'error: role not found' >&2; exit 3",
                    "echo never >> log",
                ],
                tmp.path(),
            );
            assert_eq!(out.status.code(), Some(3));
            assert!(out.stdout.is_empty(), "the agent must not start: {out:?}");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(stderr.contains("error: role not found"), "{stderr}");
            assert!(
                stderr.contains(&format!("{SETUP_FAILURE_MARKER}2 failed (exit 3)")),
                "{stderr}"
            );
            assert_eq!(
                std::fs::read_to_string(tmp.path().join("log")).unwrap(),
                "one\n"
            );
        }

        #[test]
        fn a_guarded_command_that_runs_and_fails_aborts() {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(tmp.path().join("role.sh"), "exit 1\n").unwrap();
            let out = run(&["[ -f role.sh ] && sh role.sh"], tmp.path());
            assert_eq!(out.status.code(), Some(1));
            assert!(out.stdout.is_empty(), "{out:?}");
        }
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
