//! Loading and seeding of the agent-definition config file.
//!
//! Agents are defined declaratively in `~/.config/thurbox/agents.toml`. On
//! first run (or whenever the file is missing) the built-in definitions are
//! written out so users have a working starting point they can edit. If the
//! file exists but cannot be read or parsed, we fall back to the built-ins
//! rather than failing to start.

use std::path::PathBuf;

use crate::session::{AgentDef, AgentRegistry};

/// Built-in agent definitions, also used to seed `agents.toml` on first run.
///
/// Adding a built-in here is only step 1 — status-hook wiring
/// (`extensions/hooks/extension.toml`) and the doc lists are **not** updated
/// automatically. See the checklist in `docs/AGENTS.md`.
///
/// Kept deliberately small per agent: just the command, plus resume/fork/
/// session-id groups. `claude` and `pi` pin a thurbox-generated id
/// (`--session-id`) so they can resume/fork by that exact id; `omp` pins the
/// same id as a session directory (`--session-dir {home}/…/thurbox-{id}`)
/// since it generates its own id but accepts a directory. Codex reports its id
/// through `SessionStart`. The remaining built-ins use `resume_latest = true`
/// with id-less, cwd-scoped flags (`opencode --continue`, …):
/// the agent resolves "the last session in this directory" itself. Agents
/// without any resume group simply start fresh on restart. No model is passed —
/// each agent uses its own default config. Bake extra flags (including a model)
/// into `args` if you want them.
const BUILTIN_AGENTS_HEAD: &str = r#"# Thurbox coding-agent definitions.
#
# Each [[agents]] entry describes how to launch one coding-agent CLI. The
# resume/fork/new-session groups are appended when their id is present, with
# {id} substituted. additional_dir_args repeats per multi-repo member, with
# {dir} substituted. `args` is always passed — put extra flags (e.g. a model)
# there. Add your own [[agents]] entries to support any CLI.
#
# env = { KEY = "value" } freezes defaults at session creation, with {home}
# resolved on the launch host. Session --env overrides a key; restart/fork keep
# the saved values even after this file changes.
#
# telemetry_env / telemetry_args apply only while settings.toml's [telemetry]
# is on, with {otlp_endpoint} and {otlp_protocol} substituted. They carry an
# agent's own OTel switches; the spec's OTEL_EXPORTER_OTLP_* variables are set
# for every session already, which is all copilot, omp and opencode need.
#
# Unknown keys are reported on startup (and fail `thurbox-cli config
# validate`) but don't break the load — your agents stay in effect.

config_version = 1
default = "claude"

[[agents]]
name = "claude"
command = "claude"
additional_dir_args = ["--add-dir", "{dir}"]
resume_args = ["--resume", "{id}"]
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]
telemetry_env = { CLAUDE_CODE_ENABLE_TELEMETRY = "1", OTEL_METRICS_EXPORTER = "otlp", OTEL_LOGS_EXPORTER = "otlp" }

# Codex reports its conversation id in the SessionStart hook. Thurbox stores
# that id separately from its own row identity and addresses it exactly.
# Its OTel is per-session `-c` overrides, which never touch ~/.codex/config.toml;
# it ignores OTEL_EXPORTER_OTLP_ENDPOINT and wants a full URL per signal.
[[agents]]
name = "codex"
command = "codex"
resume_args = ["resume", "{id}"]
fork_args = ["fork", "{id}"]
telemetry_args = [
  "-c", 'otel.exporter={otlp-http={endpoint="{otlp_endpoint}/v1/logs",protocol="binary"}}',
  "-c", 'otel.metrics_exporter={otlp-http={endpoint="{otlp_endpoint}/v1/metrics",protocol="binary"}}',
  "-c", 'otel.trace_exporter={otlp-http={endpoint="{otlp_endpoint}/v1/traces",protocol="binary"}}',
]

# antigravity (the `agy` CLI, the Gemini CLI successor) resumes the latest
# session in the launch directory via `--continue`; it has no fork (Ctrl+F falls
# back to a fresh session).
[[agents]]
name = "antigravity"
command = "agy"
resume_args = ["--continue"]
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

# GitHub Copilot CLI (the `copilot` command) resumes the most recent session in
# the launch directory via `--continue`; it can't pin or report a session id and
# has no fork (Ctrl+F falls back to a fresh session).
[[agents]]
name = "copilot"
command = "copilot"
resume_args = ["--continue"]
resume_latest = true

[[agents]]
name = "vibe"
command = "vibe"
telemetry_env = { VIBE_ENABLE_OTEL = "true", VIBE_OTEL_ENDPOINT = "{otlp_endpoint}" }

# pi (the pi.dev CLI) accepts a thurbox-generated session id at creation
# (`--session-id`), so — like claude — it resumes/forks by that exact id.
# Sessions live under ~/.pi/agent/, organized by working directory.
[[agents]]
name = "pi"
command = "pi"
resume_args = ["--session-id", "{id}"]
fork_args = ["--fork", "{id}"]
new_session_args = ["--session-id", "{id}"]

# omp (Oh My Pi, https://github.com/can1357/oh-my-pi) is Pi-compatible but
# generates its own internal session id and won't accept thurbox's UUID as one,
# and it only opens a session file that already holds entries. So thurbox maps
# its UUID to a session *directory* under OMP's default root
# (~/.omp/agent/sessions/): `--session-dir` starts a fresh session there, and
# `--session-dir … --continue` reopens the latest one in it (or starts fresh if
# none was ever written). The `{home}` token is
# expanded to the resolved home dir at spawn time (thurbox, not the shell —
# args are POSIX-quoted, so a literal `~` would never expand); it also
# translates onto the remote/WSL home. No fork_args: OMP has no way to pin a
# fork's target file to a thurbox UUID, so Ctrl+F starts a fresh session (see
# the OMP note in docs/CONFIG.md).
[[agents]]
name = "omp"
command = "omp"
resume_args = ["--session-dir", "{home}/.omp/agent/sessions/thurbox-{id}", "--continue"]
new_session_args = ["--session-dir", "{home}/.omp/agent/sessions/thurbox-{id}"]
"#;

/// The `shell` built-in as seeded, around this machine's
/// [`crate::shell::shell_agent_def`] — which a session on a host of the other
/// platform is given instead when its entry is still this stock one
/// (`session_ops::resolve_agent_def`).
///
/// Split out of the seed document because it is the one entry whose `command`
/// is not the same string everywhere, and the seeded file has to say what will
/// actually launch here.
const SHELL_AGENT_TOML: &str = r#"
# A plain interactive shell. The one built-in that is not a coding agent: it
# exists so a session can be *anything* — you run whatever you like in it, and
# an external driver can start its own tool with its own flags. It declares no
# resume/fork groups because a shell has no conversation to resume: restarting
# one replays this recipe in the same directory (its history and cwd live on
# disk, so it barely notices), and `--resume` is refused rather than silently
# starting fresh. Left as seeded, a session on a host of the other platform
# gets that host's shell instead (bash -i or powershell -NoLogo).
[[agents]]
name = "shell"
command = "{command}"
args = {args}
"#;

/// The seed document: the built-in agent definitions as they are written to a
/// fresh `agents.toml`, and the source [`builtin_registry`] parses.
///
/// A function rather than a `const` because one entry (`shell`) differs by
/// platform.
pub fn builtin_agents_toml() -> String {
    let shell = crate::shell::shell_agent_def(crate::session::Platform::local());
    let shell = SHELL_AGENT_TOML
        .replace("{command}", &shell.command)
        .replace("{args}", &format!("{:?}", shell.args));
    format!("{BUILTIN_AGENTS_HEAD}{shell}{BUILTIN_AGENTS_TAIL}")
}

const BUILTIN_AGENTS_TAIL: &str = r#"# ──────────────────────────────────────────────────────────────────────────
# Add your own agent (uncomment and edit)
# ──────────────────────────────────────────────────────────────────────────
#
# Any CLI works — thurbox only needs `command` plus the optional `*_args`
# groups below. The agent uses its OWN default config; thurbox never passes a
# model or permissions of its own.
#
# [[agents]]
# name = "my-agent"             # shown in the new-session agent picker
# command = "my-agent-cli"      # the executable on your PATH
# args = []                     # ALWAYS passed (see "Pin a model" below)
# resume_args = []              # appended on restart/resume, with {id} substituted
# fork_args = []                # appended on Ctrl+F fork, with {id} substituted
# new_session_args = []         # appended on a fresh spawn, with {id} substituted
# additional_dir_args = []      # repeated per multi-repo member, with {dir} substituted
# resume_latest = false         # true ⇒ resume "the last session in this dir"
#                               #   (id-less flags); leave false to pin by {id}
# telemetry_env = {}            # set only while [telemetry] is on
# telemetry_args = []           # appended only while [telemetry] is on
# hook_schema = "claude"        # OPTIONAL: name the hook FAMILY this CLI speaks
#                               #   so the built-in `hooks` extension wires its
#                               #   status hooks as if this were that built-in.
#                               #   A rebranded-claude CLI sets "claude" to get
#                               #   claude's --settings hook wiring under its own
#                               #   name. Omit if the agent has no known family.
#
# {id} is a thurbox-generated UUID. Only agents that accept it at creation
# (claude and pi both take `--session-id {id}`) can resume/fork by that exact
# id; for everything else use `resume_latest = true` with id-less, cwd-scoped flags
# (e.g. `["resume", "--last"]`). Omit every resume group to start fresh on
# restart. {home} expands to the resolved home dir at spawn (the remote home for
# an SSH/WSL host) — use it for an agent that wants a session *path* rather than
# a bare id (e.g. `["--session", "{home}/.foo/thurbox-{id}.jsonl"]`).
#
# ──────────────────────────────────────────────────────────────────────────
# Pin a model (or any flag) — put it in `args`, which is always passed
# ──────────────────────────────────────────────────────────────────────────
#
# thurbox is model-neutral; to force a model, bake the flag into `args`. E.g.
# a claude variant pinned to Opus, kept alongside the default `claude` entry:
#
# [[agents]]
# name = "claude-opus"
# command = "claude"
# args = ["--model", "opus"]    # always-on flag
# resume_args = ["--resume", "{id}"]
# fork_args = ["--resume", "{id}", "--fork-session"]
# new_session_args = ["--session-id", "{id}"]
# additional_dir_args = ["--add-dir", "{dir}"]
#
# Set `default = "claude-opus"` at the top of this file to make it the default.
"#;

/// Path to the agent-definition config file:
/// `~/.config/thurbox/agents.toml` (sibling of `config.toml`).
pub fn agents_config_path() -> Option<PathBuf> {
    crate::paths::config_file().map(|p| p.with_file_name("agents.toml"))
}

/// Parse the built-in definitions. Infallible in practice (the const is a
/// valid document); falls back to an empty registry if that ever changes.
pub fn builtin_registry() -> AgentRegistry {
    toml::from_str(&builtin_agents_toml()).unwrap_or(AgentRegistry {
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
    for warning in warnings {
        tracing::warn!("{warning}");
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
        return seed_agents_toml(&path);
    }

    match std::fs::read_to_string(&path) {
        Ok(contents) => parse_agents_toml(&contents),
        Err(e) => (
            builtin_registry(),
            vec![format!("Failed to read agents.toml: {e}")],
        ),
    }
}

/// Read a registry edit for the TUI's paced content comparison.
pub fn read_for_reload() -> Result<String, String> {
    let path = agents_config_path().ok_or("Could not resolve agents.toml path")?;
    std::fs::read_to_string(path).map_err(|e| format!("Failed to read agents.toml: {e}"))
}

/// Parse a live edit without replacing the running registry on invalid TOML.
pub fn parse_for_reload(contents: &str) -> Result<(AgentRegistry, Vec<String>), String> {
    parse_agents_toml_checked(contents).map_err(|warnings| warnings.join("; "))
}

/// Launch from the registry generation already published by this process.
/// A fresh CLI process has no cached generation and reads the file itself.
pub fn load_for_launch() -> AgentRegistry {
    cached_registry().unwrap_or_else(load_or_seed)
}

fn registry_cache() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, AgentRegistry>>
{
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, AgentRegistry>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Publish the generation the TUI has adopted for its launch worker.
pub fn publish_registry(registry: &AgentRegistry) {
    if let Some(path) = agents_config_path() {
        registry_cache()
            .lock()
            .unwrap()
            .insert(path, registry.clone());
    }
}

fn cached_registry() -> Option<AgentRegistry> {
    let path = agents_config_path()?;
    registry_cache().lock().unwrap().get(&path).cloned()
}

/// Write the bundled agents.toml on first run, degrading to the built-in
/// registry (with a warning) if the dir or file can't be created.
fn seed_agents_toml(path: &std::path::Path) -> (AgentRegistry, Vec<String>) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return (
                builtin_registry(),
                vec![format!("Failed to create config dir for agents.toml: {e}")],
            );
        }
    }
    if let Err(e) = std::fs::write(path, builtin_agents_toml()) {
        return (
            builtin_registry(),
            vec![format!("Failed to seed agents.toml: {e}")],
        );
    }
    tracing::info!(path = %path.display(), "Seeded agents.toml with built-in agents");
    (builtin_registry(), Vec::new())
}

/// Top-level keys read directly by the resilient parser; anything else at the
/// document root is reported as an unknown field (typo / stale key).
const KNOWN_TOP_LEVEL_KEYS: [&str; 3] = ["config_version", "default", "agents"];

/// Parse agents.toml contents **resiliently**, entry by entry: one malformed
/// `[[agents]]` block is skipped (with a warning naming it) instead of
/// discarding every agent the user defined. We fall back to the built-in
/// registry only when the document is syntactically broken (unrecoverable) or
/// yields no usable agents at all.
///
/// This is deliberately more forgiving than `thurbox-cli config validate`,
/// which still strict-parses the whole document — `validate` is the diagnostic
/// that tells you to fix the file, while the TUI degrades gracefully so a
/// single typo never strands you on the built-ins.
fn parse_agents_toml(contents: &str) -> (AgentRegistry, Vec<String>) {
    match parse_agents_toml_checked(contents) {
        Ok(parsed) => parsed,
        Err(mut warnings) => {
            warnings.push("using built-in agents".into());
            (builtin_registry(), warnings)
        }
    }
}

fn parse_agents_toml_checked(contents: &str) -> Result<(AgentRegistry, Vec<String>), Vec<String>> {
    // A genuine syntax error can't be recovered per entry — fall back to built-ins.
    let table: toml::Table = match contents.parse() {
        Ok(table) => table,
        Err(e) => {
            return Err(vec![format!(
                "agents.toml: {}",
                compact_toml_error(&e.to_string())
            )])
        }
    };

    let mut warnings = Vec::new();
    for key in table.keys() {
        if !KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()) {
            warnings.push(format!("agents.toml: unknown field `{key}` (ignored)"));
        }
    }

    let config_version = table
        .get("config_version")
        .and_then(toml::Value::as_integer)
        .map(|v| v as u32);
    let default = table
        .get("default")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string();

    let mut agents = Vec::new();
    let builtins = builtin_registry();
    match table.get("agents") {
        Some(toml::Value::Array(entries)) => {
            for (index, entry) in entries.iter().enumerate() {
                if let Some(mut agent) = deserialize_agent(entry, index, &mut warnings) {
                    // Existing registries predate explicit symlink-target scope.
                    // An explicit empty group opts out; custom CLIs opt in as data.
                    if entry.get("additional_dir_args").is_none()
                        && agent.name == "claude"
                        && agent.command == "claude"
                    {
                        agent.additional_dir_args = vec!["--add-dir".into(), "{dir}".into()];
                    }
                    // Upgrade the former seeded Codex address without changing
                    // other definitions or user-supplied launch arguments.
                    if agent.name == "codex"
                        && agent.command == "codex"
                        && agent.resume_args == ["resume", "--last"]
                        && agent.fork_args == ["fork", "--last"]
                        && agent.resume_latest
                    {
                        agent.resume_args = vec!["resume".into(), "{id}".into()];
                        agent.fork_args = vec!["fork".into(), "{id}".into()];
                        agent.resume_latest = false;
                    }
                    upgrade_seeded_omp_args(&mut agent);
                    upgrade_seeded_telemetry(&mut agent, entry, &builtins);
                    agents.push(agent);
                }
            }
        }
        Some(_) => warnings.push("agents.toml: `agents` must be an array of tables".into()),
        None => {}
    }

    if agents.is_empty() {
        warnings.push("agents.toml has no usable agents".into());
        return Err(warnings);
    }

    Ok((
        AgentRegistry {
            config_version,
            default,
            agents,
        },
        warnings,
    ))
}

/// Give an entry seeded before telemetry existed its built-in's
/// `telemetry_env` / `telemetry_args`. Either key present, even empty, is the
/// user's own answer, and only an entry whose `name` and `command` both match
/// a built-in is one.
fn upgrade_seeded_telemetry(agent: &mut AgentDef, entry: &toml::Value, builtins: &AgentRegistry) {
    if entry.get("telemetry_env").is_some() || entry.get("telemetry_args").is_some() {
        return;
    }
    if let Some(builtin) = builtins
        .get(&agent.name)
        .filter(|b| b.command == agent.command)
    {
        agent.telemetry_env = builtin.telemetry_env.clone();
        agent.telemetry_args = builtin.telemetry_args.clone();
    }
}

/// Deserialize one `[[agents]]` entry, returning `None` (and pushing a warning
/// that names the entry) when it is malformed so the caller can skip it. Unknown
/// fields within a valid entry are reported but kept.
fn deserialize_agent(
    entry: &toml::Value,
    index: usize,
    warnings: &mut Vec<String>,
) -> Option<AgentDef> {
    // Label by name when present (the useful identifier), else by position.
    let label = entry
        .get("name")
        .and_then(toml::Value::as_str)
        .map(|n| format!("\"{n}\""))
        .unwrap_or_else(|| format!("#{index}"));

    let mut unknowns = Vec::new();
    let result: Result<AgentDef, _> =
        serde_ignored::deserialize(entry.clone(), |path| unknowns.push(path.to_string()));

    match result {
        Ok(agent) => {
            for field in unknowns {
                warnings.push(format!(
                    "agents.toml: agent {label}: unknown field `{field}` (ignored)"
                ));
            }
            Some(agent)
        }
        Err(e) => {
            warnings.push(format!(
                "agents.toml: skipped agent {label}: {}",
                compact_toml_error(&e.to_string())
            ));
            None
        }
    }
}

/// The omp arg groups an older thurbox seeded into `agents.toml`: a session
/// *file* under `--session`. OMP 18.10 refuses a file that does not exist yet
/// (`Session "…" not found`, exit 1), so every new omp session died at launch.
const LEGACY_OMP_NEW_SESSION_ARGS: [&str; 2] =
    ["--session", "{home}/.omp/agent/sessions/thurbox-{id}.jsonl"];
const LEGACY_OMP_RESUME_ARGS: [&str; 2] =
    ["--resume", "{home}/.omp/agent/sessions/thurbox-{id}.jsonl"];

/// Swap the legacy seeded omp arg groups for the current built-in ones.
///
/// `agents.toml` is seeded once and then owned by the user, so a fixed built-in
/// never reaches an existing install on its own. Only an entry still carrying
/// *both* legacy groups verbatim is rewritten — that pair is what the seed
/// wrote, so it is the seed and not a user's choice; any edit to either group
/// leaves the entry alone. Matched on the args rather than the agent name, so a
/// renamed copy of the seeded entry is upgraded too. In memory only: the file is
/// never rewritten behind the user's back.
fn upgrade_seeded_omp_args(agent: &mut AgentDef) {
    if agent.new_session_args != LEGACY_OMP_NEW_SESSION_ARGS
        || agent.resume_args != LEGACY_OMP_RESUME_ARGS
    {
        return;
    }
    let Some(omp) = builtin_registry()
        .agents
        .into_iter()
        .find(|a| a.name == "omp")
    else {
        return;
    };
    tracing::info!(
        agent = %agent.name,
        "agents.toml: upgraded the legacy omp --session/--resume file args to --session-dir"
    );
    agent.new_session_args = omp.new_session_args;
    agent.resume_args = omp.resume_args;
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

    const OMP_NEW_SESSION_ARGS: [&str; 2] =
        ["--session-dir", "{home}/.omp/agent/sessions/thurbox-{id}"];
    const OMP_RESUME_ARGS: [&str; 3] = [
        "--session-dir",
        "{home}/.omp/agent/sessions/thurbox-{id}",
        "--continue",
    ];

    fn omp_entry(new_session: &str, resume: &str) -> String {
        format!(
            "default = \"omp\"\n[[agents]]\nname = \"omp\"\ncommand = \"omp\"\n\
             resume_args = {resume}\nnew_session_args = {new_session}\n"
        )
    }

    #[test]
    fn legacy_seeded_omp_args_are_upgraded_on_load() {
        // The exact pair an older thurbox seeded: an existing install keeps it
        // in agents.toml forever, so the loader has to replace it.
        let toml = omp_entry(
            r#"["--session", "{home}/.omp/agent/sessions/thurbox-{id}.jsonl"]"#,
            r#"["--resume", "{home}/.omp/agent/sessions/thurbox-{id}.jsonl"]"#,
        );
        let (reg, warnings) = parse_agents_toml(&toml);
        assert!(warnings.is_empty(), "{warnings:?}");
        let omp = reg.get("omp").unwrap();
        assert_eq!(omp.new_session_args, OMP_NEW_SESSION_ARGS);
        assert_eq!(omp.resume_args, OMP_RESUME_ARGS);
    }

    #[test]
    fn edited_omp_args_are_left_alone() {
        // Either group changed by hand → the user's choice, not the seed.
        let toml = omp_entry(
            r#"["--session", "{home}/.omp/agent/sessions/thurbox-{id}.jsonl"]"#,
            r#"["--resume", "/elsewhere/thurbox-{id}.jsonl"]"#,
        );
        let (reg, _) = parse_agents_toml(&toml);
        let omp = reg.get("omp").unwrap();
        assert_eq!(omp.new_session_args, LEGACY_OMP_NEW_SESSION_ARGS);
        assert_eq!(
            omp.resume_args,
            ["--resume", "/elsewhere/thurbox-{id}.jsonl"]
        );
    }

    #[test]
    fn the_seed_round_trips_through_the_loader_unchanged() {
        // What the seed writes is what the loader yields — the upgrade must not
        // fire on (or mangle) the current built-in.
        let (reg, warnings) = parse_agents_toml(&builtin_agents_toml());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(reg, builtin_registry());
    }

    #[test]
    fn telemetry_switches_upgrade_seeded_builtins_and_preserve_explicit_choices() {
        let (registry, warnings) = parse_agents_toml(
            r#"
[[agents]]
name = "claude"
command = "claude"
[[agents]]
name = "codex"
command = "codex"
telemetry_args = []
[[agents]]
name = "vibe"
command = "my-vibe-wrapper"
"#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let builtins = builtin_registry();
        assert_eq!(
            registry.get("claude").unwrap().telemetry_env,
            builtins.get("claude").unwrap().telemetry_env
        );
        assert_eq!(
            registry.get("claude").unwrap().telemetry_env["CLAUDE_CODE_ENABLE_TELEMETRY"],
            "1"
        );
        assert!(registry.get("codex").unwrap().telemetry_args.is_empty());
        assert!(registry.get("vibe").unwrap().telemetry_env.is_empty());
        assert_eq!(builtins.get("codex").unwrap().telemetry_args.len(), 6);
    }

    #[test]
    fn claude_directory_scope_upgrades_missing_groups_and_preserves_explicit_choices() {
        let (registry, warnings) = parse_agents_toml(
            r#"
[[agents]]
name = "claude"
command = "claude"
args = ["--permission-mode", "plan"]
[[agents]]
name = "family"
command = "wrapper"
hook_schema = "claude"
[[agents]]
name = "opt-out"
command = "claude"
hook_schema = "claude"
additional_dir_args = []
[[agents]]
name = "custom"
command = "claude"
[[agents]]
name = "other"
command = "other"
additional_dir_args = ["--directory={dir}"]
"#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            registry.get("claude").unwrap().additional_dir_args,
            ["--add-dir", "{dir}"]
        );
        assert_eq!(
            registry.get("claude").unwrap().args,
            ["--permission-mode", "plan"]
        );
        assert!(registry
            .get("family")
            .unwrap()
            .additional_dir_args
            .is_empty());
        assert!(registry
            .get("opt-out")
            .unwrap()
            .additional_dir_args
            .is_empty());
        assert!(registry
            .get("custom")
            .unwrap()
            .additional_dir_args
            .is_empty());
        assert_eq!(
            registry.get("other").unwrap().additional_dir_args,
            ["--directory={dir}"]
        );
        let (opt_out, warnings) = parse_agents_toml(
            "[[agents]]\nname = 'claude'\ncommand = 'claude'\nadditional_dir_args = []\n",
        );
        assert!(warnings.is_empty());
        assert!(opt_out
            .get("claude")
            .unwrap()
            .additional_dir_args
            .is_empty());
    }

    #[test]
    fn builtin_registry_parses_and_has_claude_default() {
        let reg = builtin_registry();
        assert_eq!(reg.default, "claude");
        assert!(reg.get("claude").is_some());
        assert!(reg.get("codex").is_some());
        assert!(reg.get("antigravity").is_some());
        assert!(reg.get("opencode").is_some());
        assert!(reg.get("aider").is_some());
        assert!(reg.get("copilot").is_some());
        assert!(reg.get("vibe").is_some());
        assert!(reg.get("pi").is_some());
        assert!(reg.get("omp").is_some());

        // Claude pins a thurbox id and resumes/forks by it.
        let claude = reg.get("claude").unwrap();
        assert!(!claude.resume_args.is_empty());
        assert!(!claude.resume_latest);
        assert!(claude.resume_args.iter().any(|t| t.contains("{id}")));

        // pi pins the same way (new_session + resume via --session-id, fork
        // via --fork), so it is NOT a resume_latest / id-less agent.
        let pi = reg.get("pi").unwrap();
        assert!(!pi.resume_latest);
        assert_eq!(pi.new_session_args, ["--session-id", "{id}"]);
        assert_eq!(pi.resume_args, ["--session-id", "{id}"]);
        assert_eq!(pi.fork_args, ["--fork", "{id}"]);

        // omp (Oh My Pi) pins by a deterministic session DIRECTORY, not a bare
        // id: `--session-dir {home}/…/thurbox-{id}` on create, the same plus
        // `--continue` on restart. It has no native fork (Ctrl+F → fresh
        // session).
        let omp = reg.get("omp").unwrap();
        assert!(!omp.resume_latest);
        assert_eq!(omp.new_session_args, OMP_NEW_SESSION_ARGS);
        assert_eq!(omp.resume_args, OMP_RESUME_ARGS);
        assert!(omp.fork_args.is_empty(), "omp has no native fork target");

        // Codex resumes by its hook-reported id; opencode is cwd-scoped.
        let codex = reg.get("codex").unwrap();
        assert_eq!(codex.resume_args, ["resume", "{id}"]);
        assert_eq!(codex.fork_args, ["fork", "{id}"]);
        assert!(!codex.resume_latest);
        let opencode = reg.get("opencode").unwrap();
        assert_eq!(opencode.fork_args, ["--continue", "--fork"]);
        assert!(opencode.resume_latest);

        // antigravity/aider/copilot resume their latest session but have no
        // fork group.
        for name in ["antigravity", "aider", "copilot"] {
            let a = reg.get(name).unwrap();
            assert!(a.resume_latest, "{name} should resume latest");
            assert!(!a.resume_args.is_empty(), "{name} needs resume_args");
            assert!(a.fork_args.is_empty(), "{name} has no fork");
        }

        // These agents still use id-less, cwd-scoped resume flags.
        for name in ["antigravity", "opencode", "aider", "copilot"] {
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

    /// The seed must carry copy-pasteable examples (add-your-own-agent +
    /// pin-a-model) but keep them commented, so parsing still yields exactly
    /// the built-ins — a fresh install boots on pure defaults.
    #[test]
    fn seed_documents_examples_yet_stays_builtin_only() {
        for marker in [
            "Add your own agent",
            "Pin a model",
            "claude-opus",
            "[\"--model\", \"opus\"]",
        ] {
            assert!(
                builtin_agents_toml().contains(marker),
                "agents.toml seed must document example '{marker}'"
            );
        }
        // Examples are commented, so the seed parses to just the built-ins.
        let reg = builtin_registry();
        assert_eq!(reg.default, "claude");
        assert_eq!(reg.agents.len(), 10, "examples must stay commented out");
        assert!(
            reg.get("claude-opus").is_none(),
            "example must not register"
        );
        // The platform's own shell, composed into the seed separately — a
        // session must be able to be something other than a coding agent.
        let shell = reg.get("shell").expect("the shell built-in is seeded");
        assert!(!shell.command.is_empty(), "the shell names an executable");
        assert!(
            shell.resume_args.is_empty() && shell.fork_args.is_empty(),
            "a shell has no conversation to resume or fork"
        );
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
    fn one_malformed_entry_is_skipped_and_the_rest_survive() {
        // The real-world footgun: `args` given a bare string instead of an
        // array. Previously this failed the whole document and stranded the
        // user on the built-ins; now only the bad entry is dropped.
        let toml = r#"
default = "claude"

[[agents]]
name = "claude"
command = "claude"

[[agents]]
name = "claude-bypass"
command = "claude"
args = "--dangerously-skip-permissions"

[[agents]]
name = "shepherd"
command = "claude"
args = ["--model", "claude-haiku-4-5"]
"#;
        let (reg, warnings) = parse_agents_toml(toml);

        // The two valid agents load; the bad one is skipped.
        assert_eq!(reg.names(), vec!["claude", "shepherd"]);
        assert!(reg.get("claude-bypass").is_none());
        // And the warning names the skipped agent so it's actionable.
        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        assert!(
            warnings[0].contains("claude-bypass") && warnings[0].contains("skipped"),
            "warning must name the skipped agent: {}",
            warnings[0]
        );
    }

    #[test]
    fn malformed_entry_without_name_is_labeled_by_index() {
        let toml = r#"
[[agents]]
name = "ok"
command = "ok"

[[agents]]
command = "missing-name"
"#;
        let (reg, warnings) = parse_agents_toml(toml);
        assert_eq!(reg.names(), vec!["ok"]);
        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        assert!(
            warnings[0].contains("#1"),
            "nameless entry should be labeled by index: {}",
            warnings[0]
        );
    }

    #[test]
    fn all_entries_malformed_falls_back_to_builtins() {
        let toml = "[[agents]]\nargs = \"oops\"\n";
        let (reg, warnings) = parse_agents_toml(toml);
        assert_eq!(reg.default, "claude", "should fall back to built-ins");
        assert!(reg.get("codex").is_some());
        assert!(
            warnings.iter().any(|w| w.contains("no usable agents")),
            "got: {warnings:?}"
        );
    }

    #[test]
    fn agents_key_of_wrong_type_falls_back_to_builtins() {
        // `agents` must be an array of tables; a scalar is a type error that
        // leaves zero usable agents.
        let (reg, warnings) = parse_agents_toml("agents = 3\n");
        assert_eq!(reg.default, "claude");
        assert!(
            warnings.iter().any(|w| w.contains("array of tables")),
            "got: {warnings:?}"
        );
    }

    #[test]
    fn syntax_error_still_falls_back_to_builtins() {
        let (reg, warnings) = parse_agents_toml("this is not = valid toml {{{");
        assert_eq!(reg.default, "claude");
        assert!(warnings.iter().any(|w| w.contains("using built-in agents")));
    }

    #[test]
    fn unknown_top_level_key_is_reported_but_agents_survive() {
        let toml = "stray = true\n[[agents]]\nname = \"mine\"\ncommand = \"x\"\n";
        let (reg, warnings) = parse_agents_toml(toml);
        assert_eq!(reg.names(), vec!["mine"]);
        assert!(
            warnings.iter().any(|w| w.contains("stray")),
            "got: {warnings:?}"
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

    #[test]
    fn old_seeded_codex_address_is_upgraded_without_rewriting_custom_agents() {
        let (registry, warnings) = parse_agents_toml(
            "default = 'codex'\n[[agents]]\nname = 'codex'\ncommand = 'codex'\n\
             args = ['--model', 'example']\nresume_args = ['resume', '--last']\n\
             fork_args = ['fork', '--last']\nresume_latest = true\n\
             [[agents]]\nname = 'custom'\ncommand = 'codex'\n\
             resume_args = ['resume', '--last']\nresume_latest = true\n",
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let codex = registry.get("codex").unwrap();
        assert_eq!(codex.resume_args, ["resume", "{id}"]);
        assert_eq!(codex.fork_args, ["fork", "{id}"]);
        assert_eq!(codex.args, ["--model", "example"]);
        assert!(!codex.resume_latest);
        let custom = registry.get("custom").unwrap();
        assert_eq!(custom.resume_args, ["resume", "--last"]);
        assert!(custom.resume_latest);
    }

    #[test]
    fn legacy_codex_and_omp_seeds_in_one_file_are_both_upgraded() {
        // An install seeded before either fix carries both legacy entries side
        // by side; each upgrade must fire on its own entry and only on it.
        let (registry, warnings) = parse_agents_toml(
            "default = 'codex'\n[[agents]]\nname = 'codex'\ncommand = 'codex'\n\
             resume_args = ['resume', '--last']\nfork_args = ['fork', '--last']\n\
             resume_latest = true\n\
             [[agents]]\nname = 'omp'\ncommand = 'omp'\n\
             resume_args = ['--resume', '{home}/.omp/agent/sessions/thurbox-{id}.jsonl']\n\
             new_session_args = ['--session', '{home}/.omp/agent/sessions/thurbox-{id}.jsonl']\n",
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let codex = registry.get("codex").unwrap();
        assert_eq!(codex.resume_args, ["resume", "{id}"]);
        assert_eq!(codex.fork_args, ["fork", "{id}"]);
        assert!(!codex.resume_latest);
        assert!(codex.new_session_args.is_empty());
        let omp = registry.get("omp").unwrap();
        assert_eq!(omp.new_session_args, OMP_NEW_SESSION_ARGS);
        assert_eq!(omp.resume_args, OMP_RESUME_ARGS);
        assert!(omp.fork_args.is_empty());
        assert!(!omp.resume_latest);
    }
}
