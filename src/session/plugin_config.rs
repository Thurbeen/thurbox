//! Plugin bundle configuration types.
//!
//! A thurbox plugin is a directory under `~/.local/share/thurbox/admin/plugins/<name>/`
//! containing a `thurbox-plugin.toml` manifest. Plugins may ship:
//!
//! - **Content** — static TOML/markdown merged into the effective skills, roles,
//!   mcp-server-configs, and themes lists. Declared under `[contributes]`.
//! - **A process** — an executable spawned by thurbox that speaks stdio JSON-RPC
//!   and declares `backend` and/or `mcp-tools` capabilities. Declared under `[process]`.
//!
//! Both sections are optional: a plugin can be content-only, process-only, or
//! hybrid. Parsing uses `#[serde(deny_unknown_fields)]` to reject typos early
//! and surface misnamed keys as plugin errors.
//!
//! ### Sibling data types
//! `SkillConfig`, `RoleConfig`, `McpServerConfig` are defined in the parent
//! `session` module — plugin content entries reference files on disk that
//! parse *into* those types, so the plugin module stays free of logic.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Runtime representation of a plugin row — name + location + enabled flag.
///
/// This is what the SQLite `plugins` table deserializes into and what
/// `list_disk_plugins()` returns for auto-discovered plugins. It's intentionally
/// small: the full manifest is parsed on demand via [`PluginManifest::load`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginConfig {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub version: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Parsed `thurbox-plugin.toml` manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    pub thurbox_plugin_api: u32,
    #[serde(default)]
    pub process: Option<ProcessSection>,
    #[serde(default)]
    pub contributes: Contributes,
}

/// `[process]` table — describes an executable thurbox will spawn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProcessSection {
    pub exec: PathBuf,
    pub capabilities: Vec<Capability>,
    #[serde(default = "default_activation_events")]
    pub activation_events: Vec<ActivationEvent>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

fn default_activation_events() -> Vec<ActivationEvent> {
    vec![ActivationEvent::OnStartupFinished]
}

/// Declared capabilities for a process plugin.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    Backend,
    McpTools,
}

/// When a process plugin should be spawned.
///
/// Multiple events OR together — any match triggers a spawn. Serialized as a
/// single colon-prefixed string so the manifest reads like VSCode's
/// `activationEvents`:
///
/// ```toml
/// activation_events = [
///     "onStartupFinished",
///     "onBackendSelected:gastown",
/// ]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(clippy::enum_variant_names)]
pub enum ActivationEvent {
    OnStartupFinished,
    OnBackendSelected(String),
    OnCommand(String),
    OnRole(String),
    OnWorkspaceContains(String),
}

impl ActivationEvent {
    /// Parse from manifest string form. Returns a readable error on malformed input.
    pub fn parse(s: &str) -> Result<Self, String> {
        if s == "onStartupFinished" {
            return Ok(Self::OnStartupFinished);
        }
        let (prefix, arg) = s
            .split_once(':')
            .ok_or_else(|| format!("Unknown activation event '{s}'"))?;
        match prefix {
            "onBackendSelected" => Ok(Self::OnBackendSelected(arg.to_string())),
            "onCommand" => Ok(Self::OnCommand(arg.to_string())),
            "onRole" => Ok(Self::OnRole(arg.to_string())),
            "onWorkspaceContains" => Ok(Self::OnWorkspaceContains(arg.to_string())),
            other => Err(format!("Unknown activation event prefix '{other}'")),
        }
    }

    pub fn as_manifest_string(&self) -> String {
        match self {
            Self::OnStartupFinished => "onStartupFinished".to_string(),
            Self::OnBackendSelected(s) => format!("onBackendSelected:{s}"),
            Self::OnCommand(s) => format!("onCommand:{s}"),
            Self::OnRole(s) => format!("onRole:{s}"),
            Self::OnWorkspaceContains(s) => format!("onWorkspaceContains:{s}"),
        }
    }
}

impl Serialize for ActivationEvent {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_manifest_string())
    }
}

impl<'de> Deserialize<'de> for ActivationEvent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// `[contributes]` block — every subsection is optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Contributes {
    #[serde(default)]
    pub skills: Vec<SkillContribution>,
    #[serde(default)]
    pub roles: Vec<RoleContribution>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerContribution>,
    #[serde(default)]
    pub themes: Vec<ThemeContribution>,
    #[serde(default)]
    pub configuration: Vec<ConfigurationSchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SkillContribution {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RoleContribution {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpServerContribution {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ThemeContribution {
    pub name: String,
    pub path: PathBuf,
}

/// One `[[contributes.configuration]]` entry — a tunable the plugin exposes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationSchema {
    pub key: String,
    #[serde(rename = "type")]
    pub ty: ConfigurationType,
    pub default: toml::Value,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConfigurationType {
    String,
    Int,
    Bool,
    Duration,
    Path,
    Enum,
}

impl PluginManifest {
    pub const FILENAME: &'static str = "thurbox-plugin.toml";

    /// Load and parse the manifest for a plugin directory. Also runs structural
    /// validation (capability/op consistency, enum values, min/max order).
    pub fn load(plugin_dir: &std::path::Path) -> Result<Self, String> {
        let manifest_path = plugin_dir.join(Self::FILENAME);
        let raw = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("Failed to read {}: {e}", manifest_path.display()))?;
        let manifest: Self =
            toml::from_str(&raw).map_err(|e| format!("Malformed manifest: {e}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), String> {
        crate::paths::validate_safe_name(&self.name)
            .map_err(|e| format!("Invalid plugin name '{}': {e}", self.name))?;
        for s in &self.contributes.skills {
            crate::paths::validate_safe_name(&s.name)
                .map_err(|e| format!("Invalid skill contribution name '{}': {e}", s.name))?;
        }
        for r in &self.contributes.roles {
            crate::paths::validate_safe_name(&r.name)
                .map_err(|e| format!("Invalid role contribution name '{}': {e}", r.name))?;
        }
        for m in &self.contributes.mcp_servers {
            crate::paths::validate_safe_name(&m.name)
                .map_err(|e| format!("Invalid mcp server contribution name '{}': {e}", m.name))?;
        }
        for t in &self.contributes.themes {
            crate::paths::validate_safe_name(&t.name)
                .map_err(|e| format!("Invalid theme contribution name '{}': {e}", t.name))?;
        }
        for cfg in &self.contributes.configuration {
            cfg.validate_self()?;
        }
        Ok(())
    }
}

impl ConfigurationSchema {
    fn validate_self(&self) -> Result<(), String> {
        if self.key.is_empty() {
            return Err("configuration key cannot be empty".into());
        }
        if matches!(self.ty, ConfigurationType::Enum) && self.values.is_empty() {
            return Err(format!(
                "configuration key '{}': type = \"enum\" requires non-empty values",
                self.key
            ));
        }
        if let (Some(lo), Some(hi)) = (self.min, self.max) {
            if lo > hi {
                return Err(format!(
                    "configuration key '{}': min ({lo}) is greater than max ({hi})",
                    self.key
                ));
            }
        }
        self.validate_value(&self.default)
            .map_err(|e| format!("configuration key '{}': default value {e}", self.key))?;
        Ok(())
    }

    /// Check a value against this schema. Used both for the declared default
    /// and for user-supplied overrides from `set_plugin_setting`.
    pub fn validate_value(&self, value: &toml::Value) -> Result<(), String> {
        match self.ty {
            ConfigurationType::String => {
                value.as_str().ok_or("expected string")?;
            }
            ConfigurationType::Bool => {
                value.as_bool().ok_or("expected bool")?;
            }
            ConfigurationType::Int => {
                let n = value.as_integer().ok_or("expected integer")?;
                if let Some(lo) = self.min {
                    if n < lo {
                        return Err(format!("value {n} < min {lo}"));
                    }
                }
                if let Some(hi) = self.max {
                    if n > hi {
                        return Err(format!("value {n} > max {hi}"));
                    }
                }
            }
            ConfigurationType::Path => {
                value.as_str().ok_or("expected string path")?;
            }
            ConfigurationType::Duration => {
                let s = value
                    .as_str()
                    .ok_or("expected duration string, e.g. \"15s\"")?;
                parse_duration(s).map_err(|e| e.to_string())?;
            }
            ConfigurationType::Enum => {
                let s = value.as_str().ok_or("expected enum string")?;
                if !self.values.iter().any(|v| v == s) {
                    return Err(format!("value '{s}' not in {:?}", self.values));
                }
            }
        }
        Ok(())
    }
}

/// Parse a short duration of the form `<n>s` / `<n>m` / `<n>h` / `<n>ms`.
///
/// Deliberately minimal to avoid pulling in a new crate — the configuration
/// schema uses durations for timeouts and poll intervals, where these units
/// cover every realistic case.
pub fn parse_duration(s: &str) -> Result<std::time::Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".into());
    }
    let (num, suffix) = if let Some(stripped) = s.strip_suffix("ms") {
        (stripped, "ms")
    } else if let Some(stripped) = s.strip_suffix('s') {
        (stripped, "s")
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, "m")
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, "h")
    } else {
        return Err(format!("duration '{s}' missing unit (ms, s, m, h)"));
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| format!("duration '{s}' has non-numeric count"))?;
    Ok(match suffix {
        "ms" => std::time::Duration::from_millis(n),
        "s" => std::time::Duration::from_secs(n),
        "m" => std::time::Duration::from_secs(n * 60),
        "h" => std::time::Duration::from_secs(n * 3600),
        _ => unreachable!(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_event_parse_roundtrip() {
        let cases = [
            ("onStartupFinished", ActivationEvent::OnStartupFinished),
            (
                "onBackendSelected:gastown",
                ActivationEvent::OnBackendSelected("gastown".into()),
            ),
            ("onCommand:foo", ActivationEvent::OnCommand("foo".into())),
            ("onRole:polecat", ActivationEvent::OnRole("polecat".into())),
            (
                "onWorkspaceContains:.beads/",
                ActivationEvent::OnWorkspaceContains(".beads/".into()),
            ),
        ];
        for (s, expected) in cases {
            let parsed = ActivationEvent::parse(s).unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(parsed.as_manifest_string(), s);
        }
    }

    #[test]
    fn activation_event_rejects_unknown_prefix() {
        assert!(ActivationEvent::parse("onGarbage:foo").is_err());
        assert!(ActivationEvent::parse("no-colon-bare-word").is_err());
    }

    #[test]
    fn manifest_content_only_parses() {
        let raw = r#"
            name = "sample"
            version = "0.1.0"
            thurbox_plugin_api = 1

            [[contributes.skills]]
            name = "publish"
            path = "skills/publish"
        "#;
        let m: PluginManifest = toml::from_str(raw).unwrap();
        assert_eq!(m.name, "sample");
        assert!(m.process.is_none());
        assert_eq!(m.contributes.skills.len(), 1);
    }

    #[test]
    fn manifest_process_only_parses_with_defaults() {
        let raw = r#"
            name = "proc-only"
            version = "0.1.0"
            thurbox_plugin_api = 1

            [process]
            exec = "bin/p"
            capabilities = ["backend"]
        "#;
        let m: PluginManifest = toml::from_str(raw).unwrap();
        let p = m.process.unwrap();
        assert_eq!(
            p.activation_events,
            vec![ActivationEvent::OnStartupFinished]
        );
    }

    #[test]
    fn manifest_hybrid_parses() {
        let raw = r#"
            name = "hybrid"
            version = "0.1.0"
            thurbox_plugin_api = 1

            [process]
            exec = "bin/p"
            capabilities = ["backend", "mcp-tools"]
            activation_events = ["onBackendSelected:fake"]

            [[contributes.skills]]
            name = "s1"
            path = "skills/s1"

            [[contributes.configuration]]
            key = "poll_interval"
            type = "duration"
            default = "15s"
        "#;
        let m: PluginManifest = toml::from_str(raw).unwrap();
        let p = m.process.unwrap();
        assert_eq!(
            p.capabilities,
            vec![Capability::Backend, Capability::McpTools]
        );
        assert_eq!(
            p.activation_events,
            vec![ActivationEvent::OnBackendSelected("fake".into())]
        );
        assert_eq!(m.contributes.configuration.len(), 1);
    }

    #[test]
    fn manifest_rejects_unknown_top_level_field() {
        let raw = r#"
            name = "x"
            version = "0.1.0"
            thurbox_plugin_api = 1
            extra_field = true
        "#;
        assert!(toml::from_str::<PluginManifest>(raw).is_err());
    }

    #[test]
    fn manifest_rejects_unknown_process_field() {
        let raw = r#"
            name = "x"
            version = "0.1.0"
            thurbox_plugin_api = 1

            [process]
            exec = "bin/p"
            capabilities = ["backend"]
            mystery = true
        "#;
        assert!(toml::from_str::<PluginManifest>(raw).is_err());
    }

    #[test]
    fn configuration_enum_requires_values() {
        let raw = r#"
            name = "x"
            version = "0.1.0"
            thurbox_plugin_api = 1

            [[contributes.configuration]]
            key = "level"
            type = "enum"
            default = "info"
        "#;
        let m: PluginManifest = toml::from_str(raw).unwrap();
        // Structural parse succeeds; validate() catches it.
        assert!(m.validate().is_err());
    }

    #[test]
    fn configuration_int_enforces_min_max() {
        let schema = ConfigurationSchema {
            key: "workers".into(),
            ty: ConfigurationType::Int,
            default: toml::Value::Integer(3),
            description: String::new(),
            min: Some(1),
            max: Some(16),
            values: vec![],
        };
        schema.validate_self().unwrap();
        assert!(schema.validate_value(&toml::Value::Integer(0)).is_err());
        assert!(schema.validate_value(&toml::Value::Integer(20)).is_err());
        schema.validate_value(&toml::Value::Integer(8)).unwrap();
    }

    #[test]
    fn parse_duration_supports_common_units() {
        assert_eq!(
            parse_duration("500ms").unwrap(),
            std::time::Duration::from_millis(500)
        );
        assert_eq!(
            parse_duration("15s").unwrap(),
            std::time::Duration::from_secs(15)
        );
        assert_eq!(
            parse_duration("2m").unwrap(),
            std::time::Duration::from_secs(120)
        );
        assert_eq!(
            parse_duration("1h").unwrap(),
            std::time::Duration::from_secs(3600)
        );
        assert!(parse_duration("15").is_err());
        assert!(parse_duration("1d").is_err());
    }
}
