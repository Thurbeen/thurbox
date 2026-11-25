use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fleet_api::follow::ReadyPolicy;
use fleet_api::gateway::GatewayConfig;
use serde::Deserialize;

pub const CONFIG_VERSION: u32 = 1;

pub const CONFIG_ENV: &str = "FLEET_CONFIG";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub data: Data,
    #[serde(default)]
    pub runtime: Option<RuntimeConfig>,
    #[serde(default)]
    pub runtimes: BTreeMap<String, ProgramConfig>,
    #[serde(default)]
    hosts: Option<toml::Value>,
    #[serde(default)]
    starter: Option<toml::Value>,
    #[serde(default)]
    pub forge: Option<ForgeConfig>,
    #[serde(default)]
    pub plugins: BTreeMap<String, fleet_api::plugin::PluginConfig>,
    #[serde(default)]
    pub gateway: Option<GatewayConfig>,
    #[serde(skip)]
    pub gateway_briefs: BTreeMap<String, String>,
    #[serde(default)]
    pub telemetry: Option<otel_env::TelemetryConfig>,
}

pub fn telemetry(path: &Path) -> Result<Option<otel_env::TelemetryConfig>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read config {}: {e}", path.display())),
    };
    let fail = |e: String| format!("config {}: {e}", path.display());
    let table: toml::Table = toml::from_str(&text).map_err(|e| fail(e.to_string()))?;
    let Some(telemetry) = table.get("telemetry") else {
        return Ok(None);
    };
    check_telemetry_keys(telemetry).map_err(fail)?;
    let config: otel_env::TelemetryConfig = telemetry
        .clone()
        .try_into()
        .map_err(|e: toml::de::Error| fail(format!("telemetry: {e}")))?;
    config.validate().map_err(fail)?;
    Ok(Some(config))
}

fn check_telemetry_keys(table: &toml::Value) -> Result<(), String> {
    let Some(table) = table.as_table() else {
        return Ok(());
    };
    for key in table.keys() {
        if !otel_env::TelemetryConfig::KEYS.contains(&key.as_str()) {
            return Err(format!(
                "telemetry.{key} is not a key; use one of {}",
                otel_env::TelemetryConfig::KEYS.join(", ")
            ));
        }
    }
    if let Some(headers) = table.get("headers").and_then(toml::Value::as_table)
        && let Some(key) = headers.keys().find(|k| k.as_str() != "file")
    {
        return Err(format!("telemetry.headers.{key} is not a key; use file"));
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgeConfig {
    pub call_timeout_ms: Option<u64>,
    pub closed_grace_ms: Option<u64>,
    pub settled_ttl_ms: Option<u64>,
    pub relay_notes: Option<bool>,
    pub relay_checks: Option<bool>,
    pub relay_bots: Option<bool>,
    pub no_check_grace_ms: Option<u64>,
    pub draft_policy: Option<String>,
    #[serde(default)]
    pub ready_policies: BTreeMap<String, ReadyPolicy>,
    #[serde(default)]
    pub hosts: BTreeMap<String, ForgeHostConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgeHostConfig {
    pub kind: String,
    pub cli: Option<String>,
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Data {
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeConfig {
    pub kind: String,
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramConfig {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub call_timeout_ms: Option<u64>,
    pub workspace_timeout_ms: Option<u64>,
    pub key_prefix: Option<String>,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub places: BTreeMap<String, toml::Table>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostPlace {
    pub id: String,
    pub runtime: String,
    pub place: fleet_api::runtime::Place,
    pub local: bool,
}

pub const LOCAL: &str = fleet_api::runtime::Place::LOCAL;

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
        Config::parse(&text, path)
    }

    pub fn parse(text: &str, path: &Path) -> Result<Config, String> {
        let mut config = Config::parse_settings(text, path)?;
        config.read_briefs(path)?;
        Ok(config)
    }

    pub fn parse_settings(text: &str, path: &Path) -> Result<Config, String> {
        if let Some((why, _)) = fleet_2_config(text, path) {
            return Err(format!(
                "config {path} is a Fleet 2.x config ({why}); this Fleet does not read it, nor \
                 the store it names. Run `fleet migrate --dry-run` to see the plan, then \
                 `fleet migrate`: it writes a Fleet 3 config and store and keeps the 2.x ones \
                 as .v2; or keep Fleet and thurbox 2.x (install with VERSION=v2.x.y). See \
                 docs/migrating-to-v3.md",
                path = path.display(),
            ));
        }
        let mut config: Config =
            toml::from_str(text).map_err(|e| format!("config {}: {e}", path.display()))?;
        if config.version != CONFIG_VERSION {
            return Err(format!(
                "config {}: version {} is not served; this build reads version {CONFIG_VERSION}",
                path.display(),
                config.version
            ));
        }
        // Relative paths in the file are relative to the file, not to the
        // directory fleet happens to run in.
        let base = path.parent().unwrap_or(Path::new("."));
        if let Some(d) = &config.data.dir {
            config.data.dir = Some(base.join(d));
        }
        if let Some(rt) = config.runtime.as_mut()
            && let Some(d) = &rt.dir
        {
            rt.dir = Some(base.join(d));
        }
        config
            .check_runtimes()
            .map_err(|e| format!("config {}: {e}", path.display()))?;
        if config.hosts.is_some() {
            return Err(format!(
                "config {}: [hosts] is gone: a Task names a place, `<runtime>/<place>`. Remove [hosts]; a runtime with no [runtimes.<name>.places] table has the one place `local`",
                path.display()
            ));
        }
        if config.starter.is_some() {
            return Err(format!(
                "config {}: [starter] is no longer read: fleet up needs no starter Task. Remove the section",
                path.display()
            ));
        }
        if let Some(forge) = config.forge.as_mut() {
            for host in forge.hosts.values_mut() {
                if let Some(d) = &host.dir {
                    host.dir = Some(base.join(d));
                }
            }
            if let Some(name) = &forge.draft_policy
                && !forge.ready_policies.contains_key(name)
            {
                return Err(format!(
                    "config {}: forge.draftPolicy {name:?} is not defined under [forge.readyPolicies]",
                    path.display()
                ));
            }
        }
        if let Some(gateway) = config.gateway.as_mut() {
            gateway
                .validate()
                .map_err(|e| format!("config {}: {e}", path.display()))?;
            for account in gateway.accounts.values_mut() {
                if let Some(d) = &account.dir {
                    account.dir = Some(base.join(d));
                }
            }
        }
        if let Some(telemetry) = &config.telemetry {
            let raw: toml::Table =
                toml::from_str(text).map_err(|e| format!("config {}: {e}", path.display()))?;
            if let Some(table) = raw.get("telemetry") {
                check_telemetry_keys(table)
                    .map_err(|e| format!("config {}: {e}", path.display()))?;
            }
            telemetry
                .validate()
                .map_err(|e| format!("config {}: {e}", path.display()))?;
        }
        for (name, plugin) in &config.plugins {
            fleet_api::validate::validate_name(name).map_err(|e| e.to_string())?;
            if !Path::new(&plugin.program).is_absolute()
                || !Path::new(&plugin.cwd).is_absolute()
                || !(1..=300_000).contains(&plugin.call_timeout_ms)
                || plugin.args.iter().any(|a| a.contains('\0'))
            {
                return Err(
                    "plugin requires absolute program/cwd and a bounded callTimeoutMs".into(),
                );
            }
            let vars = plugin
                .pass_env
                .iter()
                .map(|k| (k.clone(), String::new()))
                .collect();
            fleet_api::validate::validate_env(&vars).map_err(|e| e.to_string())?;
            for source in &plugin.env_from {
                fleet_api::validate::validate_name(&source.name).map_err(|e| e.to_string())?;
            }
        }
        Ok(config)
    }

    pub fn read_briefs(&mut self, path: &Path) -> Result<(), String> {
        let Some(gateway) = &self.gateway else {
            return Ok(());
        };
        for (name, route) in &gateway.routes {
            let brief = std::fs::read_to_string(&route.brief).map_err(|e| {
                format!(
                    "config {}: gateway.routes.{name}: brief {}: {e}",
                    path.display(),
                    route.brief.display()
                )
            })?;
            if brief.trim().is_empty() {
                return Err(format!(
                    "config {}: gateway.routes.{name}: brief {} is empty",
                    path.display(),
                    route.brief.display()
                ));
            }
            self.gateway_briefs.insert(name.clone(), brief);
        }
        Ok(())
    }

    fn check_runtimes(&self) -> Result<(), String> {
        if self.runtime.is_some() && !self.runtimes.is_empty() {
            return Err("[runtime] and [runtimes] exclude each other; keep one".into());
        }
        if self.runtimes.len() > 1 && self.default_runtime().is_none() {
            return Err(format!(
                "several runtimes and no default: mark one `default = true`, the one whose `local` place `local` names (have {})",
                self.runtimes.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        for (name, rt) in &self.runtimes {
            fleet_api::validate::validate_name(name)
                .map_err(|e| format!("runtimes.{name}: {e}"))?;
            for place in rt.places.keys() {
                fleet_api::validate::validate_name(place)
                    .map_err(|e| format!("runtimes.{name}.places.{place}: {e}"))?;
            }
            if rt.program.is_empty() {
                return Err(format!("runtimes.{name}.program is empty"));
            }
            if let Some(ms) = rt.call_timeout_ms
                && !(1..=300_000).contains(&ms)
            {
                return Err(format!("runtimes.{name}.callTimeoutMs must be 1 to 300000"));
            }
            if let Some(p) = &rt.key_prefix
                && (p.len() > 32
                    || fleet_api::validate::validate_name(p).is_err()
                    || p.contains('.'))
            {
                return Err(format!(
                    "runtimes.{name}.keyPrefix {p:?}: use at most 32 lowercase letters, digits and '-'"
                ));
            }
        }
        Ok(())
    }

    pub fn default_runtime(&self) -> Option<&str> {
        let marked: Vec<&String> = self
            .runtimes
            .iter()
            .filter(|(_, rt)| rt.default)
            .map(|(name, _)| name)
            .collect();
        match (marked.as_slice(), self.runtimes.len()) {
            ([one], _) => Some(one.as_str()),
            ([], 1) => self.runtimes.keys().next().map(String::as_str),
            _ => None,
        }
    }

    pub fn no_local(&self) -> Option<String> {
        let default = self.default_runtime()?;
        (!self.places().iter().any(|p| p.local)).then(|| {
            format!(
                "no `local` place: the default runtime {default} has none; add [runtimes.{default}.places.local] or name a place"
            )
        })
    }

    pub fn places(&self) -> Vec<HostPlace> {
        let default = self.default_runtime();
        let mut places = Vec::new();
        for (runtime, rt) in &self.runtimes {
            let configured: Vec<(String, toml::Table)> = if rt.places.is_empty() {
                vec![(LOCAL.to_string(), toml::Table::new())]
            } else {
                rt.places.clone().into_iter().collect()
            };
            for (name, table) in configured {
                let config = match serde_json::to_value(&table) {
                    Ok(serde_json::Value::Object(config)) => config,
                    _ => serde_json::Map::new(),
                };
                places.push(HostPlace {
                    id: format!("{runtime}/{name}"),
                    local: Some(runtime.as_str()) == default && name == LOCAL,
                    runtime: runtime.clone(),
                    place: fleet_api::runtime::Place { name, config },
                });
            }
        }
        places
    }

    pub fn empty() -> Config {
        Config {
            version: CONFIG_VERSION,
            data: Data::default(),
            runtime: None,
            runtimes: BTreeMap::new(),
            hosts: None,
            starter: None,
            forge: None,
            plugins: BTreeMap::new(),
            gateway: None,
            gateway_briefs: BTreeMap::new(),
            telemetry: None,
        }
    }
}

pub(crate) fn fleet_2_config(text: &str, path: &Path) -> Option<(String, PathBuf)> {
    const FLEET_2_KEYS: [&str; 4] = ["cli", "callTimeoutMs", "keyPrefix", "repositories"];
    let table: toml::Table = toml::from_str(text).ok()?;
    let runtime = table.get("runtime")?.as_table()?;
    let why = if runtime.get("kind").and_then(toml::Value::as_str) == Some("thurbox") {
        "[runtime] kind = \"thurbox\"".to_string()
    } else {
        let key = FLEET_2_KEYS.iter().find(|k| runtime.contains_key(**k))?;
        format!("[runtime] {key}")
    };
    let base = path.parent().unwrap_or(Path::new("."));
    let store = match table
        .get("data")
        .and_then(|d| d.get("dir"))
        .and_then(toml::Value::as_str)
    {
        Some(dir) => base.join(dir),
        None => default_data_dir().unwrap_or_else(|| PathBuf::from("fleet")),
    };
    Some((why, store))
}

pub fn discover(explicit: Option<PathBuf>) -> Option<PathBuf> {
    explicit
        .or_else(|| std::env::var_os(CONFIG_ENV).map(PathBuf::from))
        .or_else(default_config_path)
}

pub fn default_config_path() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("APPDATA")
            .filter(|d| !d.is_empty())
            .map(|d| PathBuf::from(d).join("fleet/config.toml"));
    }
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d).join("fleet/config.toml"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/fleet/config.toml"))
}

pub fn default_data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("fleet"));
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d).join("fleet"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share").join("fleet"))
}
