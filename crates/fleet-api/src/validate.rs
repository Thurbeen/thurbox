//! Validation on write: every object the store accepts passes these.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::change_request::{self, ChangeRequestSpec};
use crate::config::{ENV_PROVIDER, EnvProviderSpec, EnvSource, TASK_CONFIG, TaskConfigSpec};
use crate::conversation::{self, ConversationSpec};
use crate::gateway::ConversationRef;
use crate::kinds::is_served;
use crate::listener::{self, ListenerSpec};
use crate::meta::{API_VERSION, Object};
use crate::store::StoreError;
use crate::task::{self, TaskSpec};
use std::collections::{BTreeMap, BTreeSet};

/// DNS-subdomain-like: lowercase alphanumerics, `-` and `.`, at most 253,
/// starting and ending with an alphanumeric. Names are file names in the
/// store, so this is also what keeps a name from escaping its directory.
pub fn validate_name(name: &str) -> Result<(), StoreError> {
    let ok_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.';
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if name.is_empty()
        || name.len() > 253
        || !name.chars().all(ok_char)
        || !edge(name.chars().next())
        || !edge(name.chars().last())
        || name.contains("..")
    {
        return Err(StoreError::Invalid(format!(
            "name {name:?}: use 1-253 lowercase letters, digits, '-' or '.', starting and ending with a letter or digit"
        )));
    }
    Ok(())
}

/// Refuse a kind this build does not serve.
pub fn validate_kind(kind: &str) -> Result<(), StoreError> {
    if is_served(kind) {
        Ok(())
    } else {
        Err(StoreError::Invalid(format!("unknown kind {kind:?}")))
    }
}

pub fn validate_object(obj: &Object) -> Result<(), StoreError> {
    if obj.api_version != API_VERSION {
        return Err(StoreError::Invalid(format!(
            "apiVersion {:?} is not served; this build reads and writes {API_VERSION}",
            obj.api_version
        )));
    }
    validate_kind(&obj.kind)?;
    validate_name(&obj.metadata.name)?;
    for o in &obj.metadata.owner_references {
        validate_name(&o.name)?;
    }
    validate_spec(&obj.kind, &obj.spec)
}

pub fn validate_spec(kind: &str, spec: &Value) -> Result<(), StoreError> {
    validate_kind(kind)?;
    match kind {
        TASK_CONFIG => {
            let s: TaskConfigSpec = typed(kind, spec)?;
            validate_execution(s.harness.as_deref(), s.host.as_deref(), s.model.as_deref())?;
            validate_pause_after(s.pause_after_ms)?;
            validate_env(&s.env)?;
            for r in &s.env_from {
                validate_name(&r.name)?;
            }
            return Ok(());
        }
        ENV_PROVIDER => {
            let s: EnvProviderSpec = typed(kind, spec)?;
            validate_env(&s.data)?;
            if s.sources.len() > 32 {
                return Err(StoreError::Invalid("too many environment sources".into()));
            }
            for source in s.sources {
                match source {
                    EnvSource::File { path } => validate_absolute(&path)?,
                    EnvSource::Command {
                        program,
                        args,
                        cwd,
                        timeout_ms,
                    } => {
                        validate_absolute(&program)?;
                        validate_absolute(&cwd)?;
                        if !(1..=15_000).contains(&timeout_ms)
                            || args.iter().any(|a| a.contains('\0'))
                        {
                            return Err(StoreError::Invalid(
                                "invalid environment command timeout or arguments".into(),
                            ));
                        }
                    }
                }
            }
            return Ok(());
        }
        change_request::KIND => return validate_change_request_spec(spec),
        conversation::KIND => return validate_conversation_spec(spec),
        listener::KIND => {
            let s: ListenerSpec = typed(kind, spec)?;
            return validate_name(&s.account);
        }
        _ => {}
    }
    let s: TaskSpec = typed(kind, spec)?;
    if let Some(r) = &s.config_ref {
        validate_name(&r.name)?;
    }
    validate_env(&s.env)?;
    validate_pause_after(s.pause_after_ms)?;
    validate_execution(
        if s.config_ref.is_some() && !spec.as_object().is_some_and(|m| m.contains_key("harness")) {
            None
        } else {
            Some(&s.harness)
        },
        Some(&s.host),
        s.model.as_deref(),
    )?;
    if s.host.trim().is_empty() {
        return Err(StoreError::Invalid("spec.host must not be empty".into()));
    }
    // No brief makes an interactive Task; a blank one is a mistake.
    if !s.brief.is_empty() && s.brief.trim().is_empty() {
        return Err(StoreError::Invalid(
            "spec.brief is blank: leave it out for an interactive Task".into(),
        ));
    }
    validate_workspace(&s.workspace)
        .map_err(|e| StoreError::Invalid(format!("spec.workspace{e}")))?;
    validate_amendments(&s)?;
    // Both reach the runtime as arguments: never as a flag.
    for (field, value) in [("spec.harness", &s.harness), ("spec.host", &s.host)] {
        if value.starts_with('-') {
            return Err(StoreError::Invalid(format!(
                "{field} must not start with '-'"
            )));
        }
    }
    Ok(())
}

/// Amendments: each says something, names its sender, creates a later
/// generation than the one before, and declares only outputs not declared
/// before it.
pub fn validate_amendments(s: &TaskSpec) -> Result<(), StoreError> {
    let mut declared: BTreeSet<&str> = s.outputs.keys().map(String::as_str).collect();
    if s.is_interactive() && !s.amendments.is_empty() {
        return Err(StoreError::Invalid(
            "an interactive Task has no brief to amend: fleet send reaches its session".into(),
        ));
    }
    let mut generation = 1;
    for (i, a) in s.amendments.iter().enumerate() {
        let at = format!("spec.amendments[{i}]");
        if a.text.trim().is_empty() {
            return Err(StoreError::Invalid(format!("{at}.text must not be empty")));
        }
        if a.from.trim().is_empty() {
            return Err(StoreError::Invalid(format!("{at}.from must not be empty")));
        }
        if a.generation <= generation {
            return Err(StoreError::Invalid(format!(
                "{at}.generation {} must be above {generation}",
                a.generation
            )));
        }
        generation = a.generation;
        for name in a.outputs.keys() {
            if !declared.insert(name) {
                return Err(StoreError::Invalid(format!(
                    "{at}: output {name:?} is already declared; an output is never retyped or dropped"
                )));
            }
        }
    }
    Ok(())
}

/// A Task's spec is fixed once created, like a Pod's: only `dispatch` and
/// `forge` may change, and `amendments` only through the store's amend
/// mutation. A different brief is a different Task. A
/// ChangeRequest, a Conversation and a Listener are fixed whole.
pub fn validate_spec_update(kind: &str, old: &Value, new: &Value) -> Result<(), StoreError> {
    validate_spec(kind, new)?;
    if kind == change_request::KIND {
        let a: ChangeRequestSpec = typed(kind, old)?;
        let b: ChangeRequestSpec = typed(kind, new)?;
        if a != b {
            return Err(StoreError::Invalid(
                "ChangeRequest spec is immutable; a different change is a different ChangeRequest"
                    .into(),
            ));
        }
        return Ok(());
    }
    if kind == conversation::KIND || kind == listener::KIND {
        if old != new {
            return Err(StoreError::Invalid(format!(
                "{kind} spec is immutable; a different thread or account is a different {kind}"
            )));
        }
        return Ok(());
    }
    if kind != task::KIND {
        return Ok(());
    }
    let mut a: TaskSpec = typed(kind, old)?;
    let b: TaskSpec = typed(kind, new)?;
    // Explicit presence affects inheritance, so preserve it in immutability checks.
    for field in ["harness", "host", "model"] {
        if old.get(field).is_some() != new.get(field).is_some() {
            return Err(StoreError::Invalid(
                "Task execution field presence is immutable".into(),
            ));
        }
    }
    if a.amendments != b.amendments {
        return Err(StoreError::Invalid(
            "Task spec.amendments only grows, through fleet amend; it never changes in a spec update"
                .into(),
        ));
    }
    a.dispatch = b.dispatch;
    a.forge = b.forge.clone();
    if a != b {
        return Err(StoreError::Invalid(
            "Task spec is immutable except spec.dispatch and spec.forge; fleet amend adds to the brief, and a different brief is a new Task"
                .into(),
        ));
    }
    Ok(())
}

pub fn typed<T: DeserializeOwned>(kind: &str, spec: &Value) -> Result<T, StoreError> {
    serde_json::from_value(spec.clone())
        .map_err(|e| StoreError::Invalid(format!("{kind} spec: {e}")))
}

/// Status must parse as the kind's typed status: a writer cannot store a
/// shape its readers would refuse.
pub fn validate_status(kind: &str, status: &Value) -> Result<(), StoreError> {
    if status.is_null() {
        return Ok(());
    }
    validate_kind(kind)?;
    match kind {
        task::KIND => {
            let st: task::TaskStatus = serde_json::from_value(status.clone())
                .map_err(|e| StoreError::Invalid(format!("Task status: {e}")))?;
            crate::follow::validate_declared(&st.declared)
                .map_err(|why| StoreError::Invalid(format!("Task status: {why}")))
        }
        change_request::KIND => {
            serde_json::from_value::<change_request::ChangeRequestStatus>(status.clone())
                .map(|_| ())
                .map_err(|e| StoreError::Invalid(format!("ChangeRequest status: {e}")))
        }
        conversation::KIND => {
            serde_json::from_value::<conversation::ConversationStatus>(status.clone())
                .map(|_| ())
                .map_err(|e| StoreError::Invalid(format!("Conversation status: {e}")))
        }
        listener::KIND => serde_json::from_value::<listener::ListenerStatus>(status.clone())
            .map(|_| ())
            .map_err(|e| StoreError::Invalid(format!("Listener status: {e}"))),
        _ => Err(StoreError::Invalid(
            "passive configuration has no status".into(),
        )),
    }
}

/// A ChangeRequest names one change by URL and bounds its own cadence.
fn validate_change_request_spec(spec: &Value) -> Result<(), StoreError> {
    let s: ChangeRequestSpec = typed(change_request::KIND, spec)?;
    let url = s.url.trim();
    let host = url
        .split_once("://")
        .map(|(scheme, rest)| (scheme, rest.split(['/', '?', '#']).next().unwrap_or("")))
        .filter(|(scheme, host)| {
            matches!(*scheme, "https" | "http") && !host.is_empty() && !host.contains('@')
        });
    if host.is_none() || url.contains(char::is_whitespace) || url.contains('\0') {
        return Err(StoreError::Invalid(
            "spec.url must be an http(s) URL naming a forge host".into(),
        ));
    }
    if s.poll_ms == Some(0) {
        return Err(StoreError::Invalid("spec.pollMs must be at least 1".into()));
    }
    Ok(())
}

/// A Conversation names an account, a route and one thread by address; the
/// adapter judges the address's parts, the structure is checked here.
fn validate_conversation_spec(spec: &Value) -> Result<(), StoreError> {
    let s: ConversationSpec = typed(conversation::KIND, spec)?;
    validate_name(&s.account).map_err(|e| StoreError::Invalid(format!("spec.account: {e}")))?;
    validate_name(&s.route).map_err(|e| StoreError::Invalid(format!("spec.route: {e}")))?;
    if ConversationRef::split(&s.address).is_none() || s.address.contains('\0') {
        return Err(StoreError::Invalid(format!(
            "spec.address {:?} is not <platform>:<chat>[:<thread>]",
            s.address
        )));
    }
    if s.poll_ms == Some(0) {
        return Err(StoreError::Invalid("spec.pollMs must be at least 1".into()));
    }
    Ok(())
}

/// `pauseAfterMs`, where it is set, is at least 1.
pub fn validate_pause_after(ms: Option<crate::Millis>) -> Result<(), StoreError> {
    if ms == Some(0) {
        return Err(StoreError::Invalid(
            "pauseAfterMs must be at least 1".into(),
        ));
    }
    Ok(())
}

pub fn validate_execution(
    harness: Option<&str>,
    host: Option<&str>,
    model: Option<&str>,
) -> Result<(), StoreError> {
    for (field, value) in [("harness", harness), ("host", host), ("model", model)]
        .into_iter()
        .filter_map(|(f, v)| v.map(|v| (f, v)))
    {
        if value.trim().is_empty() || value.starts_with('-') || value.contains('\0') {
            return Err(StoreError::Invalid(format!(
                "invalid {field}: empty, flag-shaped or NUL"
            )));
        }
    }
    Ok(())
}

/// A workspace is a list of absolute URLs, `<scheme>://…`: Fleet checks
/// that, and nothing else. Each reaches the runtime as an argument, so none
/// holds whitespace or a NUL byte. The error starts with the path inside the
/// workspace, such as `[0] …`, so a caller prefixes its own field.
pub fn validate_workspace(workspace: &task::Workspace) -> Result<(), String> {
    for (i, url) in workspace.urls().iter().enumerate() {
        if task::url_scheme(url).is_none() || url.chars().any(|c| c.is_whitespace() || c == '\0') {
            return Err(format!(
                "[{i}] {url:?} is not an absolute URL: a workspace is a list of absolute URLs, `<scheme>://…`"
            ));
        }
    }
    Ok(())
}

pub fn validate_env(env: &BTreeMap<String, String>) -> Result<(), StoreError> {
    let mut names = BTreeSet::new();
    let mut bytes = 0;
    for (k, v) in env {
        let mut chars = k.chars();
        if !chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            || v.contains('\0')
            || !names.insert(k.to_ascii_uppercase())
        {
            return Err(StoreError::Invalid(
                "invalid environment key, NUL value or case collision".into(),
            ));
        }
        bytes += k.len() + v.len();
    }
    if bytes > crate::config::SOURCE_CAP {
        return Err(StoreError::Invalid("environment exceeds 64 KiB".into()));
    }
    Ok(())
}

fn validate_absolute(path: &str) -> Result<(), StoreError> {
    if !is_absolute_path(path) {
        return Err(StoreError::Invalid(
            "environment source paths must be absolute".into(),
        ));
    }
    Ok(())
}

/// Whether a path is absolute in either portable form, `/…` or `C:/…`,
/// whatever this build's platform: a config file is read on one platform
/// and names paths the adapters interpret, so the rule is the same
/// everywhere. A NUL byte is never a path.
pub fn is_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    !path.contains('\0')
        && (path.starts_with('/')
            || (bytes.len() > 2
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Workspace;

    #[test]
    fn a_workspace_names_something_usable() {
        assert!(validate_workspace(&Workspace::at("/w")).is_ok());
        assert!(validate_workspace(&Workspace::default()).is_ok());
        assert!(
            validate_workspace(&Workspace::of([
                "git+https://example.invalid/app.git#main",
                "git+ssh://git@example.invalid/lib.git?branch=x",
            ]))
            .is_ok()
        );
        for refused in ["/w", "w", "C:/w", "file://", "git+https://a b/c", "x://\0"] {
            let err = validate_workspace(&Workspace::of([refused])).unwrap_err();
            assert!(err.contains("a list of absolute URLs"), "{refused}: {err}");
        }
    }

    fn amended(amendments: serde_json::Value) -> Value {
        serde_json::json!({
            "harness": "claude-code", "brief": "b", "workspace": ["file:///w"],
            "outputs": {"prUrl": {"type": "string"}},
            "amendments": amendments
        })
    }

    #[test]
    fn amendments_say_something_in_generation_order_with_new_outputs() {
        let one = serde_json::json!({"text": "t", "from": "cli/a", "at": 1, "generation": 2});
        assert!(validate_spec(task::KIND, &amended(serde_json::json!([one]))).is_ok());
        for (bad, want) in [
            (
                serde_json::json!([{"text": " ", "from": "cli/a", "at": 1, "generation": 2}]),
                "text",
            ),
            (
                serde_json::json!([{"text": "t", "from": "", "at": 1, "generation": 2}]),
                "from",
            ),
            (
                serde_json::json!([{"text": "t", "from": "cli/a", "at": 1, "generation": 1}]),
                "above 1",
            ),
            (
                serde_json::json!([one, {"text": "t", "from": "cli/a", "at": 1, "generation": 2}]),
                "above 2",
            ),
            (
                serde_json::json!([{"text": "t", "from": "cli/a", "at": 1, "generation": 2,
                "outputs": {"prUrl": {"type": "integer"}}}]),
                "already declared",
            ),
            (
                serde_json::json!([
                    {"text": "t", "from": "cli/a", "at": 1, "generation": 2, "outputs": {"x": {"type": "string"}}},
                    {"text": "t", "from": "cli/a", "at": 1, "generation": 3, "outputs": {"x": {"type": "string"}}}
                ]),
                "already declared",
            ),
        ] {
            let err = validate_spec(task::KIND, &amended(bad.clone()))
                .unwrap_err()
                .to_string();
            assert!(err.contains(want), "{bad}: {err}");
        }
    }

    #[test]
    fn a_spec_update_never_changes_amendments() {
        let one = serde_json::json!({"text": "t", "from": "cli/a", "at": 1, "generation": 2});
        let two = serde_json::json!({"text": "u", "from": "cli/a", "at": 2, "generation": 3});
        let old = amended(serde_json::json!([one]));
        // Neither dropped, rewritten nor appended by apply or a plugin patch.
        for new in [
            amended(serde_json::json!([])),
            amended(serde_json::json!([{"text": "x", "from": "cli/a", "at": 1, "generation": 2}])),
            amended(serde_json::json!([one, two])),
        ] {
            let err = validate_spec_update(task::KIND, &old, &new)
                .unwrap_err()
                .to_string();
            assert!(err.contains("fleet amend"), "{err}");
        }
        let mut dispatched = old.clone();
        dispatched["dispatch"] = serde_json::json!(true);
        assert!(validate_spec_update(task::KIND, &old, &dispatched).is_ok());
    }
}
