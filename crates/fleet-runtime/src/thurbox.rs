//! The runtime contract served by `thurbox-cli` (Q26, n9): sessions on this
//! machine, or on any host thurbox's own `hosts.toml` reaches over SSH or in
//! WSL. Thurbox owns how a host is reached; fleet passes `--host <name>` and
//! nothing else about it.
//!
//! Every call is one bounded `thurbox-cli … --json` through fleet-exec, so a
//! hung call costs `call_timeout` and leaves no process behind, and a call
//! that times out is Unknown, never a guess.
//!
//! Identity. A session's thurbox key is `<keyPrefix>.<uid>`: the Task's uid
//! is unique in its store and short enough for thurbox's 64-character keys,
//! and the prefix keeps two fleets on one host from adopting each other's
//! sessions. Its name, for people, is the Task's name. `--on-existing adopt`
//! makes a repeated create return the session that holds the key.
//!
//! The brief is sent once under its intent key, recorded in the session's
//! own metadata (`session meta`): `fleet.brief.pending` before the send,
//! `fleet.brief` after it. Read back, the pair says delivered, not delivered,
//! or "sent, unconfirmed", which is Unknown and never a second send.
//!
//! Workspace trust. Each harness is trusted in its workspace through the
//! interface `runtime.workspaceTrust` names for it. Codex takes a project
//! trust as a launch override, passed with `--agent-arg`: thurbox keeps it
//! with the session and replays it on restart. It names one directory, the
//! one Codex keys the session's cwd on, and persists nothing. Claude Code has
//! no per-session interface: Fleet reads whether Claude passed its trust
//! dialog, and never answers the dialog or edits Claude's own state.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use fleet_api::Observation;
use fleet_api::harness::{Trust, codex_override, profile};
use fleet_api::plan::{EffectOutcome, SpawnSpec};
use fleet_api::runtime::{Runtime, TrustGap, WorkspaceTrust};
use fleet_api::task::{LOCAL_HOST as LOCAL, SessionRef, Workspace, uid_of_session_key};
use fleet_api::workspace::WorkspaceRef;
use fleet_exec::{Outcome, Spec, run};
use serde_json::Value;

/// The longest text sent as one argument, in bytes. Windows caps a whole
/// command line near 32 767 characters, and an agent with no native inbox
/// can still be given the text by typing it.
pub const MAX_BRIEF: usize = 16_000;

/// The `message send --kind` of everything Fleet gives a session.
const MESSAGE_KIND: &str = "fleet";

/// The `delivered_via` of a message thurbox only queued.
const MAILBOX: &str = "mailbox";

/// The journal mark of a text thurbox only queued: never sent again.
const QUEUED: &str = "queued";

/// The most thurbox-cli calls one operation makes: a brief (meta get, meta
/// set, send, meta set).
pub const MAX_CALLS_PER_OPERATION: u32 = 4;

const BRIEF_META: &str = "fleet.brief";

/// Why a `{repositories}` workspace reaches this runtime unmade: Fleet makes
/// every one through its workspace port before the spawn.
const NOT_MADE: &str = "Fleet makes a repositories workspace before the spawn; thurbox only runs a session in a directory";

#[derive(Debug, Clone)]
pub struct ThurboxConfig {
    /// The `thurbox-cli` executable: a path, or a name found on PATH.
    pub cli: OsString,
    pub call_timeout: Duration,
    /// Prefix of every thurbox key fleet gives a session.
    pub key_prefix: String,
    /// The variable each harness reads its model from, for `Task.spec.model`.
    pub model_env: BTreeMap<String, String>,
    /// How each harness, by thurbox agent name, is trusted in its workspace.
    /// A harness not listed is never assumed trusted.
    pub workspace_trust: BTreeMap<String, TrustInterface>,
}

/// The way a harness is trusted in its workspace (`runtime.workspaceTrust`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustInterface {
    /// `codex`: a launch override trusting one directory for the session.
    Codex,
    /// `claude`: none Fleet may use; a person trusts the repository once.
    Claude,
    /// `none`: the harness has no workspace-trust gate.
    Ungated,
}

impl TrustInterface {
    pub fn parse(name: &str) -> Option<TrustInterface> {
        match name {
            "codex" => Some(TrustInterface::Codex),
            "claude" => Some(TrustInterface::Claude),
            "none" => Some(TrustInterface::Ungated),
            _ => None,
        }
    }
}

impl ThurboxConfig {
    pub fn new(cli: impl Into<OsString>) -> ThurboxConfig {
        ThurboxConfig {
            cli: cli.into(),
            call_timeout: Duration::from_secs(30),
            key_prefix: "fleet".into(),
            model_env: BTreeMap::from([
                ("claude".into(), "ANTHROPIC_MODEL".into()),
                ("claude-code".into(), "ANTHROPIC_MODEL".into()),
            ]),
            // The harness profiles' own trust, for the harnesses they know.
            workspace_trust: ["codex", "claude", "claude-code"]
                .into_iter()
                .filter_map(|h| {
                    let interface = match profile(h).trust() {
                        Trust::LaunchOverride => TrustInterface::Codex,
                        Trust::Dialog => TrustInterface::Claude,
                        Trust::Unknown => return None,
                    };
                    Some((h.to_string(), interface))
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThurboxRuntime {
    cfg: ThurboxConfig,
}

/// One call's answer: the JSON it printed, or why there is none.
enum Answer {
    Ok(String),
    /// thurbox-cli answered no; the first line of what it said.
    No(String),
    Unknown(String),
}

fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    line.chars().take(200).collect()
}

impl ThurboxRuntime {
    pub fn new(cfg: ThurboxConfig) -> ThurboxRuntime {
        ThurboxRuntime { cfg }
    }

    pub fn thurbox_key(&self, key: &str) -> String {
        format!("{}.{}", self.cfg.key_prefix, uid_of_session_key(key))
    }

    fn call(&self, args: &[&str]) -> Answer {
        self.call_spec(Spec::new(&self.cfg.cli, self.cfg.call_timeout).args(args))
    }

    fn call_spec(&self, mut spec: Spec) -> Answer {
        // The manager's telemetry identity is not a session's: a session gets
        // only the OTEL_* its Task declares, which travel on `--env-stdin`.
        spec.env_remove.extend(
            std::env::vars_os()
                .filter_map(|(k, _)| k.into_string().ok())
                .filter(|k| k.starts_with("OTEL_")),
        );
        let what = spec
            .args
            .iter()
            .take(2)
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        match run(&spec) {
            Outcome::Exited {
                code: Some(0),
                stdout,
                ..
            } => Answer::Ok(String::from_utf8_lossy(&stdout).into_owned()),
            Outcome::Exited {
                code: Some(_),
                stdout,
                stderr,
                ..
            } => {
                // thurbox-cli prints a runtime error on stdout, not stderr.
                let why = first_line(&stderr);
                Answer::No(if why.is_empty() {
                    first_line(&stdout)
                } else {
                    why
                })
            }
            other => Answer::Unknown(format!("thurbox-cli {what}: {}", other.failure())),
        }
    }

    /// The directory the agent keys the session's trust on: a `{dir}` itself,
    /// and a workspace Fleet made names its own trust root, or is one.
    fn named_root(
        &self,
        workspace: &Workspace,
        ready: Option<&WorkspaceRef>,
    ) -> Result<String, String> {
        if let Some(r) = ready {
            return Ok(r.trust_root.clone().unwrap_or_else(|| r.dir.clone()));
        }
        match workspace {
            Workspace::Dir(w) => Ok(w.dir.clone()),
            Workspace::Repositories(_) => Err(NOT_MADE.into()),
        }
    }

    /// The trust root Fleet hands an agent: resolved through symlinks on
    /// this machine, as the agent resolves its cwd. A filesystem root or this
    /// machine's home is refused: trusting it would trust what it contains.
    fn trust_root(
        &self,
        host: &str,
        workspace: &Workspace,
        ready: Option<&WorkspaceRef>,
    ) -> Result<String, String> {
        let named = lexical(&self.named_root(workspace, ready)?)?;
        let root = if host == LOCAL {
            canonical(&named)
        } else {
            named
        };
        let trimmed = root.trim_end_matches(['/', '\\']);
        let b = trimmed.as_bytes();
        let fs_root = trimmed.is_empty() || (b.len() == 2 && b[1] == b':');
        if fs_root || (host == LOCAL && home().is_some_and(|h| canonical(&h) == root)) {
            return Err(format!(
                "refusing to trust {root}: a home directory or filesystem root is never trusted automatically"
            ));
        }
        Ok(root)
    }

    /// Codex trusts the session only through the override Fleet launched it
    /// with, and only in the workspace that override names.
    fn codex_trust(
        &self,
        session: &SessionRef,
        workspace: &Workspace,
        ready: Option<&WorkspaceRef>,
        row: &Value,
    ) -> Observation<WorkspaceTrust> {
        let mismatch = |why: String| {
            Observation::Known(WorkspaceTrust::Untrusted {
                gap: TrustGap::Mismatch,
                why,
            })
        };
        let root = match self.trust_root(&session.host, workspace, ready) {
            Ok(root) => root,
            Err(why) => return mismatch(why),
        };
        let expected = match codex_override(&root) {
            Ok(o) => o,
            Err(why) => return mismatch(why),
        };
        let Some(args) = row["agent_args"].as_array() else {
            return Observation::unknown("thurbox does not report the session's agent arguments");
        };
        // Fleet's override comes last, after the profile's hooks: Codex
        // keeps only the last `projects=`, so no other may precede it.
        let n = args.len();
        let other_trust = args[..n.saturating_sub(1)]
            .iter()
            .any(|a| a.as_str().is_some_and(|a| a.starts_with("projects=")));
        if n < 2 || args[n - 2] != "-c" || args[n - 1] != expected.as_str() || other_trust {
            return mismatch(format!(
                "the session was not started with Fleet's trust in {root}; delete the Task and apply it again, so the runtime starts one that is"
            ));
        }
        let Some(cwd) = row["cwd"].as_str() else {
            return Observation::unknown("thurbox does not report the session's directory");
        };
        let member = match (ready, workspace) {
            (Some(r), _) => cwd == r.dir,
            (None, Workspace::Dir(w)) => cwd == w.dir,
            (None, Workspace::Repositories(_)) => return mismatch(NOT_MADE.into()),
        };
        if !member {
            return mismatch(format!(
                "the session runs in {cwd}, which is not the Task's workspace; delete the Task and apply it again"
            ));
        }
        // Before its first hook, Codex may still draw its trust dialog: it
        // keys its cwd on another spelling of the root, through a symlink
        // the runtime cannot see. The dialog's default trusts; never brief it.
        if row["hook_reported"] != true {
            let capture =
                match self.json(&["session", "capture", &session.id, "--lines", "0", "--json"]) {
                    Ok(Some(v)) => v,
                    Ok(None) => return Observation::unknown("startup capture refused"),
                    Err(why) => return Observation::unknown(why),
                };
            let dialog = match codex_trust_dialog(session, &capture) {
                Ok(dialog) => dialog,
                Err(why) => return Observation::unknown(why),
            };
            if dialog {
                return mismatch(format!(
                    "Codex asks whether to trust {cwd}: it did not take Fleet's trust in {root}. Either a symlink or another spelling names that directory, or the agent entry or a wrapper passes its own -c projects= after Fleet's, and Codex keeps only the last. Name the workspace by its resolved path, or move those roots into Codex's config.toml, then delete the Task and apply it again"
                ));
            }
        }
        Observation::Known(WorkspaceTrust::Trusted {
            how: format!("Codex was launched trusting {root} for this session only"),
        })
    }

    /// Claude Code passes its workspace-trust dialog before it reports any
    /// hook. Until it does, its visible screen says whether it waits there.
    fn claude_trust(
        &self,
        session: &SessionRef,
        workspace: &Workspace,
        ready: Option<&WorkspaceRef>,
        row: &Value,
    ) -> Observation<WorkspaceTrust> {
        if row["hook_reported"] == true {
            return Observation::Known(WorkspaceTrust::Trusted {
                how: "Claude Code reported its lifecycle, past its workspace-trust dialog".into(),
            });
        }
        let capture =
            match self.json(&["session", "capture", &session.id, "--lines", "0", "--json"]) {
                Ok(Some(v)) => v,
                Ok(None) => return Observation::unknown("startup capture refused"),
                Err(why) => return Observation::unknown(why),
            };
        if !claude_trust_dialog(session, &capture) {
            return Observation::unknown(
                "Claude Code has not reported yet and shows no workspace-trust dialog",
            );
        }
        let cwd = row["cwd"].as_str().unwrap_or("its directory");
        let root = match (workspace, self.named_root(workspace, ready)) {
            (_, Ok(source)) if ready.is_some() => source,
            (Workspace::Repositories(_), Ok(source)) => source,
            (_, Ok(dir)) => format!("the repository root holding {dir}, or {dir} outside one"),
            (_, Err(why)) => why,
        };
        Observation::Known(WorkspaceTrust::Untrusted {
            gap: TrustGap::Unsupported,
            why: format!(
                "Claude Code asks whether to trust {cwd} and has no per-session trust interface, so Fleet neither answers it nor edits Claude's state. Trust {root} once on host {}: accept the dialog in the session, or set projects[<path>].hasTrustDialogAccepted in ~/.claude.json, for example from a thurbox session.pre_launch script. The brief follows.",
                session.host
            ),
        })
    }

    fn json(&self, args: &[&str]) -> Result<Option<Value>, String> {
        match self.call(args) {
            Answer::Ok(out) => serde_json::from_str(out.trim())
                .map(Some)
                .map_err(|e| format!("thurbox-cli {}: unreadable answer: {e}", args.join(" "))),
            Answer::No(_) => Ok(None),
            Answer::Unknown(why) => Err(why),
        }
    }

    /// The live rows carrying a thurbox key, on `host` only. Unknown when the
    /// list cannot be read, or when thurbox itself says the host's sessions
    /// cannot be reached: a mirrored row of an unreachable host is not proof
    /// of anything.
    fn rows(&self, host: &str, filter: (&str, &str)) -> Observation<Vec<Value>> {
        let rows = match self.call(&["session", "list", "--json", filter.0, filter.1]) {
            Answer::Ok(out) => match serde_json::from_str::<Value>(out.trim()) {
                Ok(Value::Array(rows)) => rows,
                Ok(_) => return Observation::unknown("thurbox-cli session list: not a list"),
                Err(e) => return Observation::unknown(format!("thurbox-cli session list: {e}")),
            },
            Answer::No(why) => {
                return Observation::unknown(format!("thurbox-cli session list: {why}"));
            }
            Answer::Unknown(why) => return Observation::unknown(why),
        };
        let mine: Vec<Value> = rows.into_iter().filter(|r| row_host(r) == host).collect();
        if mine.iter().any(|r| r["state"] == "unreachable") {
            return Observation::unknown(format!("host {host}: thurbox cannot reach its sessions"));
        }
        Observation::Known(mine)
    }

    /// The session's own row, by id. Unknown when it cannot be read.
    fn get(&self, session: &SessionRef) -> Observation<Value> {
        match self.call(&["session", "get", &session.id, "--json"]) {
            Answer::Ok(out) => match serde_json::from_str::<Value>(out.trim()) {
                Ok(v) if v["state"] == "unreachable" => {
                    Observation::unknown(format!("host {}: unreachable", session.host))
                }
                Ok(v) => Observation::Known(v),
                Err(e) => Observation::unknown(format!("thurbox-cli session get: {e}")),
            },
            Answer::No(why) => Observation::unknown(format!("session {}: {why}", session.id)),
            Answer::Unknown(why) => Observation::unknown(why),
        }
    }

    /// Before its first turn Codex may not have emitted any lifecycle hook.
    /// The agent `codex` runs Codex, and so does any harness
    /// `runtime.workspaceTrust` maps to `codex`, under whatever name.
    /// Process liveness is insufficient: prove the initial empty composer at
    /// the visible cursor, never by searching scrollback for an old prompt.
    fn initial_codex_ready(&self, session: &SessionRef, row: &Value) -> Observation<bool> {
        if row["id"] != session.id
            || row["key"] != self.thurbox_key(&session.key)
            || row_host(row) != session.host
            || !row["agent"].as_str().is_some_and(|a| {
                a == "codex" || self.cfg.workspace_trust.get(a) == Some(&TrustInterface::Codex)
            })
            || row["state"] != "running"
            || row["state_source"] != "process"
            || row["stopped"] != false
            || row["hook_reported"] != false
            || !row["hook_state"].is_null()
            || row["hook_state_contradicted"] != false
            || row["hook_corroboration"] != "agent"
            || !codex_process(&row["foreground_process"])
        {
            return Observation::unknown("session has no initial Codex readiness evidence");
        }
        let capture =
            match self.json(&["session", "capture", &session.id, "--lines", "0", "--json"]) {
                Ok(Some(v)) => v,
                Ok(None) => return Observation::unknown("initial composer capture refused"),
                Err(why) => return Observation::unknown(why),
            };
        let Some(output) = capture["output"].as_str() else {
            return Observation::unknown("initial composer capture has no output");
        };
        // The trust dialog can follow the composer: never ready under it.
        if codex_trust_dialog(session, &capture) != Ok(false) {
            return Observation::unknown("Codex shows its trust dialog over the composer");
        }
        let cursor_line = capture["cursor_row"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .and_then(|n| output.lines().nth(n));
        if capture["session_id"] == session.id
            && capture["ansi"] == false
            && capture["lines"] == 0
            && codex_process(&capture["foreground_process"])
            && capture["cursor_col"] == 2
            && cursor_line.map(str::trim_end) == Some("› Ask Codex to do anything")
            && output
                .lines()
                .any(|l| l.trim_start().starts_with(">_ OpenAI Codex ("))
            && output.lines().any(|l| {
                l.trim() == "To get started, describe a task or try one of these commands:"
            })
        {
            Observation::Known(true)
        } else {
            Observation::unknown("capture does not prove an empty initial Codex composer")
        }
    }

    fn meta(&self, session: &SessionRef, key: &str) -> Result<String, String> {
        match self.call(&["session", "meta", "get", &session.id, key]) {
            Answer::Ok(out) => Ok(out.trim().to_string()),
            Answer::No(why) => Err(format!("session meta {key}: {why}")),
            Answer::Unknown(why) => Err(why),
        }
    }

    /// Every key retains its own receipt; a later prompt cannot erase an
    /// earlier ambiguous send or the original brief's acknowledgement.
    fn delivery_journal(&self, session: &SessionRef) -> Result<BTreeMap<String, String>, String> {
        let value = self.meta(session, BRIEF_META)?;
        if value.is_empty() {
            return Ok(BTreeMap::new());
        }
        serde_json::from_str(&value).map_err(|e| format!("unreadable delivery journal: {e}"))
    }

    fn set_meta(&self, session: &SessionRef, key: &str, value: &str) -> Answer {
        // The value goes on stdin, as `meta set` reads it when none is given.
        self.call_spec(
            Spec::new(&self.cfg.cli, self.cfg.call_timeout)
                .args(["session", "meta", "set", &session.id, key])
                .stdin(value.as_bytes().to_vec()),
        )
    }

    /// The session's own row: Unknown when it cannot be read, or when the
    /// row under its id is another session's.
    fn own_row(&self, session: &SessionRef) -> Observation<Value> {
        match self.get(session) {
            Observation::Known(row)
                if row["id"] != session.id || row["key"] != self.thurbox_key(&session.key) =>
            {
                Observation::unknown("the session row is not this session's")
            }
            other => other,
        }
    }

    /// Before an effect on `session`: the answer when its key is no longer
    /// held by this exact session, as for a release.
    fn not_this_session(&self, session: &SessionRef) -> Option<Observation<EffectOutcome>> {
        match self.find(&session.host, &session.key) {
            Observation::Known(Some(s)) if s.id == session.id => None,
            Observation::Known(Some(_)) => Some(Observation::unknown(
                "the session key now belongs to a different session",
            )),
            Observation::Known(None) => Some(Observation::Known(EffectOutcome::Failed {
                why: "no session holds the key".into(),
            })),
            Observation::Unknown { why } => Some(Observation::Unknown { why }),
        }
    }

    /// The session's state word. A session with no window runs nothing, so
    /// its last report, kept across a reboot, is not its state: it is
    /// `stopped`, as one parked on purpose.
    fn state_word(&self, session: &SessionRef) -> Observation<String> {
        self.get(session).map(|v| {
            if v["stopped"] == true || v["hook_corroboration"] == "missing" {
                "stopped".to_string()
            } else {
                v["state"].as_str().unwrap_or("").to_string()
            }
        })
    }
}

fn codex_process(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|p| matches!(p.rsplit(['/', '\\']).next(), Some("codex" | "codex.exe")))
}

/// The path as an agent spells its cwd: no trailing separator. A `.` or
/// `..` component, or an empty one, is refused: only the host's filesystem
/// can resolve it, and a remote host's is out of reach.
fn lexical(path: &str) -> Result<String, String> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let body = trimmed.strip_prefix("\\\\").unwrap_or(trimmed);
    if body
        .split(['/', '\\'])
        .skip(1)
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err(format!(
            "cannot trust {path}: name it without `.`, `..` or repeated separators, which only its host can resolve"
        ));
    }
    Ok(trimmed.to_string())
}

/// The path through its symlinks, when this machine can resolve it. Windows
/// keeps the spelling given: its resolved form is a verbatim path agents
/// do not key trust on.
fn canonical(path: &str) -> String {
    #[cfg(unix)]
    if let Some(p) = std::fs::canonicalize(path)
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
    {
        return p;
    }
    path.to_string()
}

fn home() -> Option<String> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(var).ok().filter(|h| !h.is_empty())
}

/// Claude Code's workspace-trust dialog, current at the cursor: its header,
/// both choices and its footer, the cursor between them. Old text in the
/// scrollback is not a dialog.
fn claude_trust_dialog(session: &SessionRef, capture: &Value) -> bool {
    if capture["session_id"] != session.id || capture["ansi"] != false || capture["lines"] != 0 {
        return false;
    }
    let (Some(output), Some(cursor)) = (capture["output"].as_str(), capture["cursor_row"].as_u64())
    else {
        return false;
    };
    usize::try_from(cursor).is_ok_and(|c| fleet_api::harness::claude_trust_dialog(output, c))
}

/// Whether Codex 0.164's trust dialog is on the visible screen: its question
/// and its trusting default. It can follow the composer, so it is looked for
/// on the whole screen, not only at the cursor. Err when the capture is not
/// this session's plain visible screen: that proves neither answer.
fn codex_trust_dialog(session: &SessionRef, capture: &Value) -> Result<bool, &'static str> {
    let output = capture["output"].as_str();
    let (true, Some(output)) = (
        capture["session_id"] == session.id && capture["ansi"] == false && capture["lines"] == 0,
        output,
    ) else {
        return Err("the capture is not this session's visible screen");
    };
    Ok(fleet_api::harness::codex_trust_dialog(output))
}

/// Absolute on the host the path is for: a POSIX path, or a Windows drive
/// or UNC path, whatever this machine is.
fn is_absolute(dir: &str) -> bool {
    let b = dir.as_bytes();
    dir.starts_with('/')
        || dir.starts_with("\\\\")
        || (b.len() > 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'\\' | b'/'))
}

/// Whether a `session get` row shows its agent stopped: `stopped: true`,
/// or `activity: "stopped"`.
fn row_stopped(row: &Value) -> Observation<bool> {
    if row["stopped"] == true || row["activity"] == "stopped" {
        return Observation::Known(true);
    }
    match row["stopped"].as_bool() {
        Some(stopped) => Observation::Known(stopped),
        None => Observation::unknown("thurbox does not say whether the session is stopped"),
    }
}

fn row_host(row: &Value) -> &str {
    row["host"]
        .as_str()
        .filter(|h| !h.is_empty())
        .unwrap_or(LOCAL)
}

impl Runtime for ThurboxRuntime {
    fn prepare_execution(
        &self,
        execution: &fleet_api::config::Execution,
    ) -> Observation<fleet_api::config::Execution> {
        let mut result = execution.clone();
        if let Some(model) = &execution.model {
            let Some(var) = self.cfg.model_env.get(&execution.harness) else {
                return Observation::unknown(
                    "no model variable configured for harness; set runtime.modelEnv",
                );
            };
            let map = BTreeMap::from([(var.clone(), model.clone())]);
            if fleet_api::validate::validate_env(&map).is_err() {
                return Observation::unknown("invalid model environment mapping");
            }
            fleet_api::config::merge_env(&mut result.env, &map);
        }
        if fleet_api::validate::validate_env(&result.env).is_err() {
            return Observation::unknown("launch environment exceeds bounds");
        }
        Observation::Known(result)
    }

    fn env_source(
        &self,
        source: &fleet_api::config::EnvSource,
    ) -> Observation<std::collections::BTreeMap<String, String>> {
        crate::resolve_env_source(source)
    }

    fn operation_bound(&self) -> fleet_api::Millis {
        let one = fleet_exec::worst_case(self.cfg.call_timeout) * MAX_CALLS_PER_OPERATION;
        u64::try_from(one.as_millis()).unwrap_or(u64::MAX)
    }

    fn host(&self, host: &str) -> Observation<bool> {
        let doctor = match self.json(&["doctor", "--json"]) {
            Ok(Some(v)) => v,
            Ok(None) => return Observation::unknown("thurbox-cli doctor refused"),
            Err(why) => return Observation::unknown(why),
        };
        let check = |key: &str| {
            doctor["checks"]
                .as_array()
                .and_then(|cs| cs.iter().find(|c| c["key"] == key))
                .map(|c| c["level"].as_str().unwrap_or("").to_string())
        };
        let level = if host == LOCAL {
            check("multiplexer")
        } else {
            check(&format!("host:{host}"))
        };
        match level.as_deref() {
            Some("ok") | Some("warn") => {}
            // Not configured, or thurbox says its launcher is missing.
            _ => return Observation::Known(false),
        }
        if host == LOCAL {
            return Observation::Known(true);
        }
        let prefix = format!("{}.", self.cfg.key_prefix);
        self.rows(host, ("--key-prefix", &prefix)).map(|_| true)
    }

    /// On the local host the directory is made here. Thurbox has no verb
    /// that makes a directory on another host, so a remote host is refused
    /// with what to do instead.
    fn create_dir(&self, host: &str, dir: &str) -> Observation<Result<bool, String>> {
        if !is_absolute(dir) {
            return Observation::Known(Err(format!(
                "{dir:?} must be an absolute path on the host"
            )));
        }
        if host != LOCAL {
            return Observation::Known(Err(format!(
                "the thurbox runtime cannot create {dir} on host {host}: it makes directories on the local host only; use a TaskConfig on the local host, or a repositories workspace"
            )));
        }
        let existed = std::path::Path::new(dir).is_dir();
        match std::fs::create_dir_all(dir) {
            Ok(()) => Observation::Known(Ok(!existed)),
            Err(e) => Observation::Known(Err(format!("cannot create {dir}: {e}"))),
        }
    }

    /// On the local host the directory is removed here; a directory that is
    /// gone already is removed. A remote host is refused, as `create_dir`
    /// refuses it, so no directory there is Fleet's.
    fn remove_dir(&self, host: &str, dir: &str) -> Observation<Result<(), String>> {
        if !is_absolute(dir) {
            return Observation::Known(Err(format!(
                "{dir:?} must be an absolute path on the host"
            )));
        }
        if host != LOCAL {
            return Observation::Known(Err(format!(
                "the thurbox runtime cannot remove {dir} on host {host}: it removes directories on the local host only"
            )));
        }
        match std::fs::remove_dir_all(dir) {
            Ok(()) => Observation::Known(Ok(())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Observation::Known(Ok(())),
            Err(e) => Observation::Known(Err(format!("cannot remove {dir}: {e}"))),
        }
    }

    fn spawn(&self, spec: &SpawnSpec) -> Observation<EffectOutcome> {
        // A session started without its trust stays untrusted: a mapping
        // added later cannot reach it. Nothing starts until one exists.
        let Some(trust) = self.cfg.workspace_trust.get(&spec.harness) else {
            return Observation::Known(EffectOutcome::TrustUnmapped {
                harness: spec.harness.clone(),
            });
        };
        let workspace = match (&spec.ready, &spec.workspace) {
            // Fleet made the workspace: thurbox only runs the session in it.
            (Some(r), _) => {
                if let Some(d) = std::iter::once(&r.dir)
                    .chain(&r.extra_dirs)
                    .find(|d| !is_absolute(d))
                {
                    return Observation::Known(EffectOutcome::Failed {
                        why: format!("the workspace {d:?} Fleet made is not an absolute path"),
                    });
                }
                // The other worktrees are spanned as they are: Fleet made
                // them, so thurbox makes no worktree of its own.
                let mut args = vec!["--repo-path".to_string(), r.dir.clone()];
                for d in &r.extra_dirs {
                    args.extend(["--add-dir".to_string(), d.clone()]);
                }
                args
            }
            // thurbox resolves --repo-path as an absolute path on the target
            // host; a relative one would mean whatever directory fleet ran in.
            (None, Workspace::Dir(w)) if !is_absolute(&w.dir) => {
                return Observation::Known(EffectOutcome::Failed {
                    why: format!(
                        "workspace.dir {:?} must be an absolute path on the host for the thurbox runtime",
                        w.dir
                    ),
                });
            }
            (None, Workspace::Dir(w)) => vec!["--repo-path".to_string(), w.dir.clone()],
            (None, Workspace::Repositories(_)) => {
                return Observation::Known(EffectOutcome::Failed {
                    why: NOT_MADE.into(),
                });
            }
        };
        let key = self.thurbox_key(&spec.key);
        let name: String = spec
            .key
            .rsplit_once('.')
            .map_or(&*spec.key, |(n, _)| n)
            .chars()
            .take(64)
            .collect();
        let mut args: Vec<String> = [
            "session",
            "create",
            "--json",
            "--name",
            &name,
            "--key",
            &key,
            "--on-existing",
            "adopt",
            "--agent",
            &spec.harness,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        args.extend(workspace);
        if spec.host != LOCAL {
            args.extend(["--host".to_string(), spec.host.clone()]);
        }
        // The hooks that report the agent's state through `fleet signal`, as
        // its harness profile installs them; thurbox keeps them with the
        // session and replays them on restart.
        let hooks = match trust {
            TrustInterface::Codex => profile("codex").hook_args(),
            TrustInterface::Claude => profile("claude").hook_args(),
            TrustInterface::Ungated => profile(&spec.harness).hook_args(),
        };
        for a in hooks {
            // `=`: thurbox takes a value starting with `-` only joined.
            args.push(format!("--agent-arg={a}"));
        }
        if *trust == TrustInterface::Codex {
            let trust = self
                .trust_root(&spec.host, &spec.workspace, spec.ready.as_ref())
                .and_then(|root| codex_override(&root));
            match trust {
                // `=`: thurbox takes a value starting with `-` only joined.
                Ok(o) => args.extend(["--agent-arg=-c".to_string(), format!("--agent-arg={o}")]),
                Err(why) => {
                    return Observation::Known(EffectOutcome::Failed {
                        why: format!("workspace trust: {why}"),
                    });
                }
            }
        }
        let mut env = spec.env.clone();
        if let Some(model) = &spec.model
            && !spec.environment_frozen
        {
            let Some(var) = self.cfg.model_env.get(&spec.harness) else {
                return Observation::Known(EffectOutcome::Failed {
                    why: format!(
                        "no model variable is configured for harness {}; set runtime.modelEnv",
                        spec.harness
                    ),
                });
            };
            if fleet_api::validate::validate_env(&BTreeMap::from([(var.clone(), model.clone())]))
                .is_err()
            {
                return Observation::Known(EffectOutcome::Failed {
                    why: "invalid model environment variable".into(),
                });
            }
            fleet_api::config::merge_env(&mut env, &BTreeMap::from([(var.clone(), model.clone())]));
        }
        let answer = if env.is_empty() {
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            self.call(&argv)
        } else {
            args.push("--env-stdin".into());
            let bytes = match serde_json::to_vec(&env) {
                Ok(b) => b,
                Err(_) => return Observation::unknown("environment encoding failed"),
            };
            self.call_spec(
                Spec::new(&self.cfg.cli, self.cfg.call_timeout)
                    .args(&args)
                    .stdin(bytes),
            )
        };
        match answer {
            Answer::Ok(out) => {
                let row: Value = match serde_json::from_str(out.trim()) {
                    Ok(v) => v,
                    Err(e) => {
                        return Observation::unknown(format!("thurbox-cli session create: {e}"));
                    }
                };
                let Some(id) = row["id"].as_str() else {
                    return Observation::unknown("thurbox-cli session create: no id in its answer");
                };
                Observation::Known(EffectOutcome::Spawned {
                    session: SessionRef {
                        id: id.to_string(),
                        host: spec.host.clone(),
                        key: spec.key.clone(),
                    },
                    adopted: row["created"] == false,
                })
            }
            Answer::No(_) => Observation::Known(EffectOutcome::Failed {
                why: "thurbox session creation refused".into(),
            }),
            Answer::Unknown(why) => Observation::unknown(why),
        }
    }

    fn find(&self, host: &str, key: &str) -> Observation<Option<SessionRef>> {
        let tk = self.thurbox_key(key);
        match self.rows(host, ("--key", &tk)) {
            Observation::Known(rows) => match rows.as_slice() {
                [] => Observation::Known(None),
                [row] => match row["id"].as_str() {
                    Some(id) => Observation::Known(Some(SessionRef {
                        id: id.to_string(),
                        host: host.to_string(),
                        key: key.to_string(),
                    })),
                    None => Observation::unknown("a session row without an id"),
                },
                _ => Observation::unknown(format!("key {tk} is held by more than one session")),
            },
            Observation::Unknown { why } => Observation::Unknown { why },
        }
    }

    fn ready(&self, session: &SessionRef) -> Observation<bool> {
        match self.get(session) {
            Observation::Known(row) if row["stopped"] == true => Observation::Known(false),
            Observation::Known(row) => match row["state"].as_str() {
                // A working turn takes a message on the agent's own inbox.
                Some("idle" | "done" | "working") => Observation::Known(true),
                // A dialog holds the agent, and its answer is a person's.
                Some("blocked" | "stopped") => Observation::Known(false),
                _ => self.initial_codex_ready(session, &row),
            },
            Observation::Unknown { why } => Observation::Unknown { why },
        }
    }

    fn workspace_trust(
        &self,
        session: &SessionRef,
        workspace: &Workspace,
        ready: Option<&WorkspaceRef>,
    ) -> Observation<WorkspaceTrust> {
        let row = match self.get(session) {
            Observation::Known(row) => row,
            Observation::Unknown { why } => return Observation::Unknown { why },
        };
        if row["id"] != session.id
            || row["key"] != self.thurbox_key(&session.key)
            || row_host(&row) != session.host
        {
            return Observation::unknown("the session row is not this session's");
        }
        let Some(agent) = row["agent"].as_str() else {
            return Observation::unknown("thurbox reports no agent for the session");
        };
        match self.cfg.workspace_trust.get(agent) {
            Some(TrustInterface::Codex) => self.codex_trust(session, workspace, ready, &row),
            Some(TrustInterface::Claude) => self.claude_trust(session, workspace, ready, &row),
            Some(TrustInterface::Ungated) => Observation::Known(WorkspaceTrust::Trusted {
                how: format!("runtime.workspaceTrust says harness {agent} has no trust gate"),
            }),
            None => Observation::Known(WorkspaceTrust::Untrusted {
                gap: TrustGap::Unsupported,
                why: format!(
                    "Fleet knows no workspace-trust interface for harness {agent}; set runtime.workspaceTrust.{agent} to \"none\" if it has no trust gate"
                ),
            }),
        }
    }

    fn brief(
        &self,
        session: &SessionRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        let mut journal = match self.delivery_journal(session) {
            Ok(journal) => journal,
            Err(why) => return Observation::unknown(why),
        };
        match journal.get(intent_key).map(String::as_str) {
            Some("delivered") => {
                return Observation::Known(EffectOutcome::Delivered {
                    already: true,
                    via: None,
                    may_hold: false,
                });
            }
            Some(QUEUED) => {
                return Observation::Known(EffectOutcome::Queued {
                    why: "thurbox kept it in the mailbox, which the agent does not read".into(),
                });
            }
            Some(_) => {
                return Observation::unknown("prompt delivery is pending and cannot be repeated");
            }
            None => {}
        }
        if text.len() > MAX_BRIEF {
            return Observation::Known(EffectOutcome::Failed {
                why: format!(
                    "the text is {} bytes; Fleet sends at most {MAX_BRIEF}",
                    text.len()
                ),
            });
        }
        journal.insert(intent_key.to_string(), "pending".into());
        let pending = serde_json::to_string(&journal).expect("string map");
        match self.set_meta(session, BRIEF_META, &pending) {
            Answer::Ok(_) => {}
            Answer::No(why) => return Observation::Known(EffectOutcome::Failed { why }),
            Answer::Unknown(why) => return Observation::unknown(why),
        }
        // The agent's own inbox, never the terminal composer: thurbox picks
        // the route (a Claude Code socket, a Codex queue, else its mailbox),
        // so a working turn takes it too. Fleet is no session, so no
        // caller's session or task becomes the sender.
        let mut spec = Spec::new(&self.cfg.cli, self.cfg.call_timeout).args([
            "message",
            "send",
            "--to",
            &session.id,
            "--kind",
            MESSAGE_KIND,
            // `--body=`: a text that starts with `-` is text, never a flag.
            &format!("--body={text}"),
            "--json",
        ]);
        spec.env_remove
            .extend(["THURBOX_SESSION".to_string(), "THURBOX_TASK".to_string()]);
        let answer = match self.call_spec(spec) {
            Answer::Ok(out) => out,
            // A failed send may still have enqueued the message: never
            // sent again on it.
            Answer::No(why) => {
                return Observation::unknown(format!(
                    "send failed and may have enqueued it: {why}"
                ));
            }
            Answer::Unknown(why) => return Observation::unknown(why),
        };
        let row: Value = serde_json::from_str(answer.trim()).unwrap_or(Value::Null);
        let via = row["delivered_via"].as_str().map(str::to_string);
        // thurbox falls back to its mailbox when it cannot reach the agent:
        // a remote session, an unbound Codex, a failed socket, any other
        // harness. Nothing reads it there, so it is not a delivery.
        let (mark, outcome) = if via.as_deref() == Some(MAILBOX) {
            let note = row["delivery_note"]
                .as_str()
                .unwrap_or("thurbox gave no reason");
            let why =
                format!("thurbox kept it in the mailbox, which the agent does not read: {note}");
            (QUEUED, EffectOutcome::Queued { why })
        } else {
            let may_hold = row["may_hold"] == true;
            let outcome = EffectOutcome::Delivered {
                already: false,
                via,
                may_hold,
            };
            ("delivered", outcome)
        };
        journal.insert(intent_key.to_string(), mark.into());
        let marked = serde_json::to_string(&journal).expect("string map");
        match self.set_meta(session, BRIEF_META, &marked) {
            Answer::Ok(_) => Observation::Known(outcome),
            Answer::No(why) | Answer::Unknown(why) => {
                Observation::unknown(format!("sent, but the receipt was not recorded: {why}"))
            }
        }
    }

    fn brief_delivered(&self, session: &SessionRef, intent_key: &str) -> Observation<bool> {
        match self.delivery_journal(session) {
            Ok(journal) => match journal.get(intent_key).map(String::as_str) {
                Some("delivered") => Observation::Known(true),
                Some(QUEUED) => Observation::unknown(
                    "thurbox kept it in the mailbox, which the agent does not read",
                ),
                Some(_) => Observation::unknown(
                    "the prompt was being sent and its delivery was never confirmed",
                ),
                None => Observation::Known(false),
            },
            Err(why) => Observation::unknown(why),
        }
    }

    fn at_rest(&self, session: &SessionRef) -> Observation<bool> {
        match self.state_word(session) {
            Observation::Known(s) => match s.as_str() {
                "idle" | "done" | "stopped" => Observation::Known(true),
                "working" | "blocked" => Observation::Known(false),
                other => Observation::unknown(format!("session state {other:?} does not say")),
            },
            Observation::Unknown { why } => Observation::Unknown { why },
        }
    }

    fn release(&self, session: &SessionRef) -> Observation<EffectOutcome> {
        // The controller proved this exact session at rest. A reused key
        // cannot transfer that proof to another session.
        match self.find(&session.host, &session.key) {
            Observation::Known(None) => return Observation::Known(EffectOutcome::Released),
            Observation::Known(Some(s)) if s.id == session.id => {}
            Observation::Known(Some(_)) => {
                return Observation::unknown("the session key now belongs to a different session");
            }
            Observation::Unknown { why } => return Observation::Unknown { why },
        }
        // Fleet makes and cleans every repositories workspace: thurbox only
        // ever ran the session in a directory, and has no worktree to keep.
        match self.call(&["session", "delete", &session.id, "--force"]) {
            Answer::Ok(_) => Observation::Known(EffectOutcome::Released),
            Answer::No(why) => Observation::Known(EffectOutcome::Failed { why }),
            Answer::Unknown(why) => Observation::unknown(why),
        }
    }

    /// thurbox keeps a session's row across a reboot and reports, on `get`,
    /// what holds its window: `missing` when there is none, which is lost.
    /// Any window that is there, even a dead one, is not: `restart
    /// --if-missing` leaves it alone. A stopped session was parked on purpose
    /// and is not lost either.
    fn lost(&self, session: &SessionRef) -> Observation<bool> {
        let row = match self.get(session) {
            Observation::Known(row) => row,
            Observation::Unknown { why } => return Observation::Unknown { why },
        };
        if row["id"] != session.id || row["key"] != self.thurbox_key(&session.key) {
            return Observation::unknown("the session row is not this session's");
        }
        if row["stopped"] == true {
            return Observation::Known(false);
        }
        match row["hook_corroboration"].as_str() {
            Some("missing") => Observation::Known(true),
            Some("agent" | "foreign-agent" | "shell" | "other" | "dead") => {
                Observation::Known(false)
            }
            other => Observation::unknown(format!(
                "thurbox cannot tell whether the session's window is there ({other:?})"
            )),
        }
    }

    /// `restart --if-missing`: thurbox relaunches the agent only when its
    /// window is gone, resuming its conversation in the same row and
    /// worktree, and leaves a running or stopped session alone.
    /// thurbox restarts the agent with its own resume arguments, which it
    /// keeps with the session: the conversation is its to name.
    fn revive(&self, session: &SessionRef, _: Option<&str>) -> Observation<EffectOutcome> {
        match self.call(&["session", "restart", &session.id, "--if-missing", "--json"]) {
            Answer::Ok(_) => Observation::Known(EffectOutcome::Revived),
            Answer::No(why) => Observation::Known(EffectOutcome::Failed { why }),
            Answer::Unknown(why) => Observation::unknown(why),
        }
    }

    fn can_pause(&self) -> bool {
        true
    }

    fn paused(&self, session: &SessionRef) -> Observation<bool> {
        match self.own_row(session) {
            Observation::Known(row) => row_stopped(&row),
            Observation::Unknown { why } => Observation::Unknown { why },
        }
    }

    /// `session stop`: thurbox kills the pane and keeps the row, the
    /// checkout and the conversation.
    fn pause(&self, session: &SessionRef) -> Observation<EffectOutcome> {
        if let Some(refused) = self.not_this_session(session) {
            return refused;
        }
        match self.call(&["session", "stop", &session.id, "--json"]) {
            Answer::Ok(_) => Observation::Known(EffectOutcome::Paused),
            Answer::No(why) => Observation::Known(EffectOutcome::Failed { why }),
            Answer::Unknown(why) => Observation::unknown(why),
        }
    }

    /// `session start`: thurbox puts the pane back and resumes the
    /// conversation, and leaves a running session alone.
    fn resume(&self, session: &SessionRef, _: Option<&str>) -> Observation<EffectOutcome> {
        if let Some(refused) = self.not_this_session(session) {
            return refused;
        }
        match self.call(&["session", "start", &session.id, "--json"]) {
            Answer::Ok(_) => Observation::Known(EffectOutcome::Resumed),
            Answer::No(why) => Observation::Known(EffectOutcome::Failed { why }),
            Answer::Unknown(why) => Observation::unknown(why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_api::task::{RepositoriesWorkspace, Repository};

    fn workspace(list: &[(&str, Option<&str>)]) -> RepositoriesWorkspace {
        RepositoriesWorkspace {
            repositories: list
                .iter()
                .map(|(url, reference)| Repository {
                    name: url.rsplit('/').next().unwrap().to_string(),
                    url: url.to_string(),
                    reference: reference.map(str::to_string),
                })
                .collect(),
        }
    }

    fn runtime() -> ThurboxRuntime {
        ThurboxRuntime::new(ThurboxConfig::new("thurbox-cli"))
    }

    #[test]
    fn a_directory_is_made_and_removed_on_the_local_host_only_and_by_an_absolute_path() {
        let rt = runtime();
        let remote = rt.create_dir("build-box", "/work/conversations/conv-3f9a1c0d2b7e");
        assert!(
            matches!(&remote, Observation::Known(Err(why)) if why.contains("host build-box") && why.contains("local host only")),
            "{remote:?}"
        );
        let removed = rt.remove_dir("build-box", "/work/conversations/conv-3f9a1c0d2b7e");
        assert!(
            matches!(&removed, Observation::Known(Err(why)) if why.contains("local host only")),
            "{removed:?}"
        );
        let relative = rt.create_dir(LOCAL, "work/conversations/conv-3f9a1c0d2b7e");
        assert!(
            matches!(&relative, Observation::Known(Err(why)) if why.contains("absolute path")),
            "{relative:?}"
        );
    }

    #[test]
    fn a_filesystem_root_is_never_a_trust_root() {
        let rt = runtime();
        for dir in ["/", "C:\\", "C:/"] {
            let err = rt
                .trust_root("remote", &Workspace::at(dir), None)
                .unwrap_err();
            assert!(err.contains("never trusted automatically"), "{dir}: {err}");
        }
        assert_eq!(
            rt.trust_root("remote", &Workspace::at("/work/t"), None)
                .unwrap(),
            "/work/t"
        );
        let repos = Workspace::Repositories(workspace(&[
            ("https://example.invalid/a", None),
            ("https://example.invalid/b", None),
        ]));
        // Fleet makes a repositories workspace: unmade, it has no root.
        assert!(rt.trust_root("remote", &repos, None).is_err());
        // A workspace Fleet made names its trust root, or is its own.
        let mut ready = WorkspaceRef {
            key: "t.u1".into(),
            host: "remote".into(),
            dir: "/w/t.u1/a".into(),
            extra_dirs: Vec::new(),
            trust_root: Some("/src/made".into()),
        };
        assert_eq!(
            rt.trust_root("remote", &repos, Some(&ready)).unwrap(),
            "/src/made"
        );
        ready.trust_root = None;
        assert_eq!(
            rt.trust_root("remote", &repos, Some(&ready)).unwrap(),
            "/w/t.u1/a"
        );
    }

    #[test]
    fn only_a_readable_codex_screen_answers_for_its_trust_dialog() {
        let s = SessionRef {
            id: "s1".into(),
            host: "local".into(),
            key: "t.u1".into(),
        };
        let screen = |output: &str| serde_json::json!({"session_id": "s1", "ansi": false, "lines": 0, "output": output});
        let dialog = "  Trust this folder? Codex can read.\n› 1. Trust and continue\n";
        assert_eq!(codex_trust_dialog(&s, &screen(dialog)), Ok(true));
        assert_eq!(
            codex_trust_dialog(&s, &screen("› Ask Codex to do anything\n")),
            Ok(false)
        );
        for (field, value) in [
            ("output", Value::Null),
            ("session_id", "s2".into()),
            ("lines", 200.into()),
            ("ansi", true.into()),
        ] {
            let mut c = screen(dialog);
            c[field] = value;
            assert!(codex_trust_dialog(&s, &c).is_err(), "{field}");
        }
    }

    #[test]
    fn a_trust_root_is_named_as_an_agent_spells_its_cwd() {
        assert_eq!(lexical("/work/t/").unwrap(), "/work/t");
        assert_eq!(lexical("C:\\work\\t\\").unwrap(), "C:\\work\\t");
        assert_eq!(lexical("\\\\host\\share\\t").unwrap(), "\\\\host\\share\\t");
        for bad in ["/work/../t", "/work/./t", "/work//t", "C:\\a\\..\\t"] {
            assert!(lexical(bad).is_err(), "{bad}");
        }
    }

    /// Claude Code 2.1.312's dialog, as `session capture --lines 0` read it.
    fn dialog(cursor: u64) -> Value {
        serde_json::json!({
            "session_id": "s1", "ansi": false, "lines": 0, "cursor_row": cursor,
            "output": " Accessing workspace:\n\n /w\n\n Security guide\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n"
        })
    }

    #[test]
    fn only_a_cursor_current_claude_trust_dialog_is_one() {
        let s = SessionRef {
            id: "s1".into(),
            host: "local".into(),
            key: "t.u1".into(),
        };
        assert!(claude_trust_dialog(&s, &dialog(5)));
        assert!(!claude_trust_dialog(&s, &dialog(9)), "cursor below it");
        assert!(!claude_trust_dialog(&s, &dialog(0)), "cursor on its header");
        let mut other = dialog(5);
        other["session_id"] = "s2".into();
        assert!(!claude_trust_dialog(&s, &other));
        let mut scrollback = dialog(5);
        scrollback["lines"] = 200.into();
        assert!(!claude_trust_dialog(&s, &scrollback));
        let mut imports = dialog(5);
        imports["output"] = " External imports:\n ❯ No, disable external imports\n   Yes, allow external imports\n Enter to confirm · Esc to cancel\n".into();
        assert!(!claude_trust_dialog(&s, &imports), "another dialog");
    }

    #[test]
    fn a_stopped_row_is_paused() {
        use serde_json::json;
        for (row, stopped) in [
            (json!({"stopped": false, "activity": "idle"}), Some(false)),
            (json!({"stopped": false}), Some(false)),
            (json!({"stopped": false, "activity": "stopped"}), Some(true)),
            (json!({"stopped": true, "activity": "idle"}), Some(true)),
            (json!({"activity": "idle"}), None),
        ] {
            match (row_stopped(&row), stopped) {
                (Observation::Known(s), Some(want)) => assert_eq!(s, want, "{row}"),
                (Observation::Unknown { .. }, None) => {}
                (got, want) => panic!("{row}: stopped {got:?}, want {want:?}"),
            }
        }
    }
}
