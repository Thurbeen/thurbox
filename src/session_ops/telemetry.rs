//! Agent-native OpenTelemetry (ADR-36): configure each spawned agent's *own*
//! exporter from `[telemetry]`, never export on its behalf.
//!
//! Two layers keep thurbox agent-neutral. Layer 1 is the OTel spec — the
//! `OTEL_EXPORTER_OTLP_*` variables plus `OTEL_RESOURCE_ATTRIBUTES` naming the
//! thurbox session — and is enough on its own for an agent that honours the
//! spec (copilot, omp, opencode). Layer 2 is the agent's `telemetry_env` /
//! `telemetry_args` from `agents.toml`, for the switches the spec does not
//! cover; it is data, so a moving agent surface is a config fix.

use std::collections::BTreeMap;

use crate::session::settings::TelemetrySettings;
use crate::session::{AgentDef, HostDef, SessionConfig};

const RESOURCE_ATTRIBUTES: &str = "OTEL_RESOURCE_ATTRIBUTES";

/// Add `[telemetry]`'s configuration for one launch to `config.env`, returning
/// the arguments to append to the agent's command line.
///
/// Called after [`super::inject_thurbox_env`] wherever that builds a pane's
/// environment — spawn, restart, `agent_launch_plan` and
/// [`super::session_process_env`] — so a restarted session reports exactly as
/// its spawn did. Idempotent, which `agent_launch_plan` relies on: it applies
/// this over an environment `session_process_env` already applied it to. A variable the
/// session's own `--env` sets wins over layers 1 and 2: it is the caller's
/// explicit choice for this one session. `OTEL_RESOURCE_ATTRIBUTES` is a list,
/// so the session's value is merged into the inherited one instead, its keys
/// winning, and thurbox's own keys win over both.
///
/// A remote session exports to its host's `otel_endpoint` instead of
/// `[telemetry] endpoint`: `localhost:4318` there is the host, not this
/// machine. A host that names none, or a session whose host is not in
/// `hosts.toml`, gets only the resource attributes, which still let a backend
/// the host does reach join its telemetry to the session.
pub(crate) fn apply(
    config: &mut SessionConfig,
    def: &AgentDef,
    session_name: &str,
    host: Option<&HostDef>,
    telemetry: &TelemetrySettings,
) -> Vec<String> {
    if !telemetry.configures_agents() {
        return Vec::new();
    }
    let remote = config
        .backend
        .as_deref()
        .is_some_and(crate::session::Route::is_remote_key);
    // The session's own value merges into thurbox's environment rather than
    // replacing it: an attribute the operator set for every session survives a
    // session that adds its own. A remote session has no inherited value: this
    // process's environment is not the host's.
    let process = (!remote)
        .then(|| std::env::var(RESOURCE_ATTRIBUTES).ok())
        .flatten();
    let session_id = config.session_id.map(|id| id.to_string());
    let attributes = resource_attributes(
        &merge(
            process.as_deref().unwrap_or_default(),
            config
                .env
                .get(RESOURCE_ATTRIBUTES)
                .map_or("", String::as_str),
        ),
        &[
            ("thurbox.session.id", session_id.as_deref().unwrap_or("")),
            ("thurbox.session.name", session_name),
            ("thurbox.agent", &config.agent),
        ],
    );
    config.env.insert(RESOURCE_ATTRIBUTES.into(), attributes);
    let endpoint = if remote {
        match host.and_then(|h| h.otel_endpoint.as_deref()) {
            Some(endpoint) => endpoint,
            None => return Vec::new(),
        }
    } else {
        telemetry.endpoint.as_str()
    };

    let mut spec = BTreeMap::from([
        (
            "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
            endpoint.to_string(),
        ),
        (
            "OTEL_EXPORTER_OTLP_PROTOCOL".to_string(),
            telemetry.protocol.clone(),
        ),
    ]);
    if !telemetry.headers.is_empty() {
        let headers: Vec<String> = telemetry
            .headers
            .iter()
            .map(|(k, v)| format!("{k}={}", percent_encode(v)))
            .collect();
        spec.insert("OTEL_EXPORTER_OTLP_HEADERS".into(), headers.join(","));
    }
    let (agent_env, args) = def.telemetry_launch(endpoint, &telemetry.protocol);
    for (k, v) in spec.into_iter().chain(agent_env) {
        config.env.entry(k).or_insert(v);
    }
    args
}

/// `base` with `over` appended, any pair of `base` whose key `over` also sets
/// dropped, so `over` wins a conflict.
fn merge(base: &str, over: &str) -> String {
    let over: Vec<String> = pairs(over).collect();
    let overridden: Vec<&str> = over.iter().map(|pair| key(pair)).collect();
    pairs(base)
        .filter(|pair| !overridden.contains(&key(pair)))
        .chain(over.iter().cloned())
        .collect::<Vec<_>>()
        .join(",")
}

/// `inherited` with thurbox's own non-empty attributes appended, every earlier
/// value for one of thurbox's keys dropped - empty or not - so the session's
/// identity can neither be claimed from outside nor outlive a launch that has
/// none. Values are percent-encoded, as the spec asks: a session name with a
/// comma or `=` would otherwise split the list.
fn resource_attributes(inherited: &str, ours: &[(&str, &str)]) -> String {
    let added = ours
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={}", percent_encode(v)));
    pairs(inherited)
        .filter(|pair| !ours.iter().any(|(k, _)| *k == key(pair)))
        .chain(added)
        .collect::<Vec<_>>()
        .join(",")
}

fn pairs(list: &str) -> impl Iterator<Item = String> + '_ {
    list.split(',')
        .filter(|pair| !pair.trim().is_empty())
        .map(str::to_string)
}

fn key(pair: &str) -> &str {
    pair.split('=').next().unwrap_or_default().trim()
}

/// RFC 3986 percent-encoding of everything outside the unreserved set.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> TelemetrySettings {
        TelemetrySettings {
            enabled: true,
            ..TelemetrySettings::default()
        }
    }

    fn config(backend: Option<&str>) -> SessionConfig {
        SessionConfig {
            session_id: Some(crate::session::SessionId::default()),
            agent: "claude".into(),
            backend: backend.map(str::to_string),
            ..SessionConfig::default()
        }
    }

    // Remote, so this process's own OTEL_RESOURCE_ATTRIBUTES stays out of it.
    #[test]
    fn the_sessions_attributes_are_kept_but_never_override_its_identity() {
        let mut config = config(Some("ssh:devbox"));
        config.env.insert(
            RESOURCE_ATTRIBUTES.into(),
            "team=infra,thurbox.agent=spoofed,".into(),
        );
        apply(&mut config, &AgentDef::default(), "a b=c", &on());
        let id = config.session_id.unwrap();
        assert_eq!(
            config.env[RESOURCE_ATTRIBUTES],
            format!(
                "team=infra,thurbox.session.id={id},thurbox.session.name=a%20b%3Dc,\
                 thurbox.agent=claude"
            )
        );
    }

    #[test]
    fn a_launch_with_no_session_drops_an_inherited_session_identity() {
        let mut config = SessionConfig {
            session_id: None,
            ..config(Some("ssh:devbox"))
        };
        config.env.insert(
            RESOURCE_ATTRIBUTES.into(),
            "thurbox.session.id=old,thurbox.session.name=old".into(),
        );
        apply(&mut config, &AgentDef::default(), "", &on());
        assert_eq!(config.env[RESOURCE_ATTRIBUTES], "thurbox.agent=claude");
    }

    #[test]
    fn the_sessions_own_env_wins_over_both_layers() {
        let def = AgentDef {
            telemetry_env: BTreeMap::from([("OTEL_METRICS_EXPORTER".into(), "otlp".into())]),
            ..AgentDef::default()
        };
        let mut config = config(None);
        config.env.insert(
            "OTEL_EXPORTER_OTLP_ENDPOINT".into(),
            "http://mine:4318".into(),
        );
        config
            .env
            .insert("OTEL_METRICS_EXPORTER".into(), "console".into());
        apply(&mut config, &def, "s", None, &on());
        assert_eq!(
            config.env["OTEL_EXPORTER_OTLP_ENDPOINT"],
            "http://mine:4318"
        );
        assert_eq!(config.env["OTEL_METRICS_EXPORTER"], "console");
        assert_eq!(config.env["OTEL_EXPORTER_OTLP_PROTOCOL"], "http/protobuf");
    }

    #[test]
    fn a_remote_session_gets_the_attributes_and_no_exporter() {
        let def = AgentDef {
            telemetry_env: BTreeMap::from([("X".into(), "{otlp_endpoint}".into())]),
            telemetry_args: vec!["-c".into(), "{otlp_endpoint}".into()],
            ..AgentDef::default()
        };
        let mut config = config(Some("ssh:devbox"));
        let args = apply(&mut config, &def, "s", None, &on());
        assert!(args.is_empty());
        assert_eq!(config.env.keys().collect::<Vec<_>>(), [RESOURCE_ATTRIBUTES]);
    }

    #[test]
    fn a_remote_session_exports_to_its_hosts_endpoint_not_the_local_one() {
        let def = AgentDef {
            telemetry_args: vec!["{otlp_endpoint}/v1/logs".into()],
            ..AgentDef::default()
        };
        let host = HostDef {
            name: "devbox".into(),
            otel_endpoint: Some("http://collector.devbox:4318".into()),
            ..HostDef::default()
        };
        let mut config = config(Some("ssh:devbox"));
        let args = apply(&mut config, &def, "s", Some(&host), &on());
        assert_eq!(args, ["http://collector.devbox:4318/v1/logs"]);
        assert_eq!(
            config.env["OTEL_EXPORTER_OTLP_ENDPOINT"],
            "http://collector.devbox:4318"
        );
        assert_eq!(config.env["OTEL_EXPORTER_OTLP_PROTOCOL"], "http/protobuf");
    }

    #[test]
    fn layer_two_is_substituted() {
        let def = AgentDef {
            telemetry_env: BTreeMap::from([("P".into(), "{otlp_protocol}".into())]),
            telemetry_args: vec!["--otel={otlp_endpoint}/v1/traces".into()],
            ..AgentDef::default()
        };
        let mut config = config(None);
        let args = apply(&mut config, &def, "s", None, &on());
        assert_eq!(args, ["--otel=http://localhost:4318/v1/traces"]);
        assert_eq!(config.env["P"], "http/protobuf");
    }
}
