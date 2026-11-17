//! The `[telemetry]` table of a program's config file, and the one rule that
//! merges it with the environment: an `OTEL_*` variable set to a non-empty
//! value wins over the file's value for that variable. The file's headers go
//! only to the file's endpoint, so a variable that names another collector
//! drops them.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
const HEADERS: &str = "OTEL_EXPORTER_OTLP_HEADERS";
const RESOURCE: &str = "OTEL_RESOURCE_ATTRIBUTES";
const ENDPOINTS: [&str; 4] = [
    ENDPOINT,
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
];

/// The `[telemetry]` table: Fleet's `config.toml` and thurbox's
/// `settings.toml` each have one, with these keys (docs/observability.md).
/// Unknown keys are left to the file's own loader: Fleet refuses them
/// ([`TelemetryConfig::KEYS`]), thurbox reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// The collector's base URL, as `OTEL_EXPORTER_OTLP_ENDPOINT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Only `http/protobuf`, the one protocol the programs speak.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Where the export headers are read from: never the headers themselves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HeadersSource>,
    /// Export traces; on unless `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traces: Option<bool>,
    /// Export metrics; on unless `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<bool>,
    /// Export log records; off unless `true`. Only thurbox records any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<bool>,
    /// More resource attributes, as `OTEL_RESOURCE_ATTRIBUTES`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resource: BTreeMap<String, String>,
}

/// A secret's source: a file that holds the `OTEL_EXPORTER_OTLP_HEADERS`
/// value, `key=value` pairs separated by commas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadersSource {
    /// An absolute path, read at each start of the program.
    pub file: PathBuf,
}

impl TelemetryConfig {
    /// The keys of the table, for a loader that refuses any other.
    pub const KEYS: &'static [&'static str] = &[
        "endpoint", "protocol", "headers", "traces", "metrics", "logs", "resource",
    ];

    /// The table's own rules; each error names the key, as
    /// `telemetry.<key>`. The headers file is read later, at start.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(endpoint) = &self.endpoint {
            if !endpoint.trim().starts_with("http://") {
                return Err(format!(
                    "telemetry.endpoint {endpoint:?}: use an http:// URL; the programs have no TLS client"
                ));
            }
        }
        if let Some(protocol) = &self.protocol {
            if protocol.trim() != crate::PROTOCOL {
                return Err(format!(
                    "telemetry.protocol {protocol:?} is not supported; the exporter speaks {}",
                    crate::PROTOCOL
                ));
            }
        }
        if let Some(headers) = &self.headers {
            if !headers.file.is_absolute() {
                return Err(format!(
                    "telemetry.headers.file {}: use an absolute path",
                    headers.file.display()
                ));
            }
        }
        for (key, value) in &self.resource {
            if key == "service.name" {
                return Err(
                    "telemetry.resource: service.name is each program's own; group the programs with service.namespace"
                        .into(),
                );
            }
            if key.trim().is_empty() || key.contains([',', '=']) || value.contains(',') {
                return Err(format!(
                    "telemetry.resource {key:?}: a key is not empty and holds no `,` or `=`, a value holds no `,`"
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn vars(&self) -> Result<FileVars, String> {
        self.validate()?;
        let mut vars = BTreeMap::new();
        if let Some(endpoint) = &self.endpoint {
            vars.insert(
                ENDPOINT.to_string(),
                endpoint.trim().trim_end_matches('/').to_string(),
            );
        }
        if let Some(protocol) = &self.protocol {
            vars.insert(
                "OTEL_EXPORTER_OTLP_PROTOCOL".to_string(),
                protocol.trim().to_string(),
            );
        }
        if let Some(source) = &self.headers {
            let text = std::fs::read_to_string(&source.file).map_err(|e| {
                format!(
                    "telemetry.headers.file {}: {}",
                    source.file.display(),
                    e.kind()
                )
            })?;
            let text = text.trim();
            if !text.is_empty() {
                vars.insert(HEADERS.to_string(), text.to_string());
            }
        }
        for (signal, on, default) in [
            ("TRACES", self.traces, true),
            ("METRICS", self.metrics, true),
            ("LOGS", self.logs, false),
        ] {
            if let Some(on) = on.filter(|on| *on != default) {
                vars.insert(
                    format!("OTEL_{signal}_EXPORTER"),
                    if on { "otlp" } else { "none" }.to_string(),
                );
            }
        }
        if !self.resource.is_empty() {
            let pairs: Vec<String> = self
                .resource
                .iter()
                .map(|(k, v)| format!("{}={}", k.trim(), v.trim()))
                .collect();
            vars.insert(RESOURCE.to_string(), pairs.join(","));
        }
        Ok(FileVars {
            vars,
            resource: self.resource.clone(),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FileVars {
    vars: BTreeMap<String, String>,
    resource: BTreeMap<String, String>,
}

/// Where a value in force comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The variable of that name.
    Env(String),
    /// The `[telemetry]` table.
    File,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Env(name) => f.write_str(name),
            Source::File => f.write_str("config file"),
        }
    }
}

pub(crate) struct Merged<'a, E: Fn(&str) -> Option<String>> {
    pub env: E,
    pub file: &'a FileVars,
}

impl<E: Fn(&str) -> Option<String>> Merged<'_, E> {
    fn env_set(&self, name: &str) -> Option<String> {
        (self.env)(name).filter(|v| !v.trim().is_empty())
    }

    pub fn lookup(&self, name: &str) -> Option<(String, Source)> {
        if let Some(v) = self.env_set(name) {
            return Some((v, Source::Env(name.to_string())));
        }
        if name == HEADERS && ENDPOINTS.iter().any(|e| self.env_set(e).is_some()) {
            return None;
        }
        self.file.vars.get(name).map(|v| (v.clone(), Source::File))
    }

    pub fn var(&self, name: &str) -> Option<String> {
        self.lookup(name).map(|(v, _)| v)
    }

    pub fn file_wins(&self, name: &str) -> bool {
        matches!(self.lookup(name), Some((_, Source::File)))
    }

    pub fn file_resource(&self) -> Vec<(String, String)> {
        if self.file_wins(RESOURCE) {
            self.file.resource.clone().into_iter().collect()
        } else {
            Vec::new()
        }
    }

    pub fn child_env(&self) -> Vec<(String, String)> {
        self.file
            .vars
            .keys()
            .filter(|name| self.file_wins(name))
            .filter_map(|name| self.var(name).map(|v| (name.clone(), v)))
            .collect()
    }
}

pub(crate) fn header_pairs(value: &str) -> std::collections::HashMap<String, String> {
    value
        .split(',')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> TelemetryConfig {
        toml::from_str(text).expect("a table")
    }

    fn merged<'a>(
        env: &'a [(&'a str, &'a str)],
        file: &'a FileVars,
    ) -> Merged<'a, impl Fn(&str) -> Option<String> + 'a> {
        Merged {
            env: move |name: &str| {
                env.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            },
            file,
        }
    }

    #[test]
    fn the_table_stands_for_the_otel_variables() {
        let dir = std::env::temp_dir().join(format!("otel-env-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let headers = dir.join("headers");
        std::fs::write(&headers, "authorization=Bearer%20x\n").unwrap();
        let config = table(&format!(
            "endpoint = \"http://c:4318/\"\nprotocol = \"http/protobuf\"\nheaders = {{ file = '{}' }}\ntraces = false\nmetrics = true\nlogs = true\nresource = {{ \"service.namespace\" = \"fleet\", \"deployment.environment.name\" = \"local\" }}\n",
            headers.display()
        ));
        let vars = config.vars().unwrap().vars;
        assert_eq!(
            vars.into_iter().collect::<Vec<_>>(),
            [
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer%20x"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                (
                    "OTEL_RESOURCE_ATTRIBUTES",
                    "deployment.environment.name=local,service.namespace=fleet"
                ),
                ("OTEL_TRACES_EXPORTER", "none"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
        );
    }

    #[test]
    fn a_table_against_the_rules_is_refused_naming_its_key() {
        for (text, key) in [
            ("endpoint = \"https://c\"", "telemetry.endpoint"),
            ("protocol = \"grpc\"", "telemetry.protocol"),
            ("headers = { file = \"rel\" }", "telemetry.headers.file"),
            ("resource = { \"service.name\" = \"x\" }", "service.name"),
            ("resource = { \"a,b\" = \"x\" }", "telemetry.resource"),
            ("resource = { \"a\" = \"x,y\" }", "telemetry.resource"),
        ] {
            let why = table(text).validate().unwrap_err();
            assert!(why.contains(key), "{text}: {why}");
        }
        let missing = table("headers = { file = \"/nonexistent/otel-headers\" }")
            .vars()
            .unwrap_err();
        assert!(missing.contains("telemetry.headers.file"), "{missing}");
        assert!(!missing.contains("Bearer"));
    }

    #[test]
    fn a_variable_wins_over_the_file_one_variable_at_a_time() {
        let file = FileVars {
            vars: [
                (ENDPOINT, "http://file:4318"),
                (HEADERS, "a=b"),
                (RESOURCE, "k=file"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into(),
            resource: [("k".to_string(), "file".to_string())].into(),
        };
        let only_file = merged(&[], &file);
        assert_eq!(
            only_file.lookup(ENDPOINT),
            Some(("http://file:4318".into(), Source::File))
        );
        assert_eq!(only_file.var(HEADERS).as_deref(), Some("a=b"));
        assert_eq!(only_file.file_resource(), [("k".into(), "file".into())]);
        assert_eq!(only_file.child_env().len(), 3);

        let env = [(RESOURCE, "k=env"), ("OTEL_EXPORTER_OTLP_PROTOCOL", "")];
        let env_attrs = merged(&env, &file);
        assert_eq!(
            env_attrs.lookup(RESOURCE),
            Some(("k=env".into(), Source::Env(RESOURCE.into())))
        );
        assert!(env_attrs.file_resource().is_empty());
        assert_eq!(env_attrs.var(ENDPOINT).as_deref(), Some("http://file:4318"));
        assert!(!env_attrs.child_env().iter().any(|(k, _)| k == RESOURCE));
    }

    #[test]
    fn the_files_headers_go_only_to_the_files_endpoint() {
        let file = FileVars {
            vars: [(ENDPOINT, "http://file:4318"), (HEADERS, "a=b")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into(),
            resource: BTreeMap::new(),
        };
        for endpoint in ENDPOINTS {
            let env = [(endpoint, "http://other:4318")];
            let m = merged(&env, &file);
            assert_eq!(m.var(HEADERS), None, "{endpoint}");
            assert!(!m.child_env().iter().any(|(k, _)| k == HEADERS));
        }
        let env = [(ENDPOINT, "http://other:4318"), (HEADERS, "c=d")];
        assert_eq!(merged(&env, &file).var(HEADERS).as_deref(), Some("c=d"));
    }

    #[test]
    fn headers_split_into_pairs() {
        assert_eq!(
            header_pairs("a=b, c = d%20e ,bad,=x,f="),
            [("a", "b"), ("c", "d%20e")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into()
        );
    }
}
