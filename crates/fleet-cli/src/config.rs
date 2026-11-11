//! The one configuration file: versioned, typed, read through this loader
//! only. The one environment variable is the file's own location.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fleet_api::follow::ReadyPolicy;
use fleet_api::gateway::GatewayConfig;
use serde::Deserialize;

/// The config schema version this build reads (P9).
pub const CONFIG_VERSION: u32 = 1;

/// The one fleet setting read from the environment: where the config file
/// is. (The default data directory follows the platform's own variables.)
pub const CONFIG_ENV: &str = "FLEET_CONFIG";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub data: Data,
    /// The `[runtime]` section: the fake runtime, linked into Fleet for
    /// tests and trials. Exclusive with `[runtimes]`.
    #[serde(default)]
    pub runtime: Option<RuntimeConfig>,
    /// The `[runtimes.<name>]` sections: programs that speak the runtime
    /// protocol.
    #[serde(default)]
    pub runtimes: BTreeMap<String, ProgramConfig>,
    /// The `[hosts.<name>]` sections: the runtime serving each host a Task
    /// may name.
    #[serde(default)]
    pub hosts: BTreeMap<String, HostConfig>,
    /// A `[starter]` section from a release that bundled VEGA, read only
    /// to refuse it by name: `fleet up` needs no starter Task.
    #[serde(default)]
    starter: Option<toml::Value>,
    #[serde(default)]
    pub forge: Option<ForgeConfig>,
    #[serde(default)]
    pub plugins: BTreeMap<String, fleet_api::plugin::PluginConfig>,
    /// The `[gateway]` section: chat accounts, people and routes. Its rules
    /// are the API's ([`GatewayConfig::validate`]); the platform checks run
    /// when the gateway opens.
    #[serde(default)]
    pub gateway: Option<GatewayConfig>,
    /// Per `[gateway.routes]` entry, its brief as read from the file the
    /// route names when the config loaded: what each Task the route
    /// starts carries. An unreadable brief refuses the config.
    #[serde(skip)]
    pub gateway_briefs: BTreeMap<String, String>,
}

/// The `[forge]` section: the forges ChangeRequests are mirrored from, the
/// relay and draft defaults a Task's `spec.forge` may override, and the
/// ready policies a Task may select. A policy naming an unknown predicate
/// is refused here, at load.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgeConfig {
    /// The bound on every forge CLI call.
    pub call_timeout_ms: Option<u64>,
    /// How long a closed change stays watched before it settles.
    pub closed_grace_ms: Option<u64>,
    /// How long a settled ChangeRequest is retained before its controller
    /// deletes it.
    pub settled_ttl_ms: Option<u64>,
    /// Relay new notes to the worker (default true).
    pub relay_notes: Option<bool>,
    /// Relay passed or failed checks to the worker (default true).
    pub relay_checks: Option<bool>,
    /// Relay bot-authored notes to the worker (default false).
    pub relay_bots: Option<bool>,
    /// How long `checksPassed` waits for a check to appear on a checkless
    /// head before treating it as passed (default five minutes).
    pub no_check_grace_ms: Option<u64>,
    /// The ready policy applied to a Task that names none; absent, a draft
    /// is never taken out of draft unless the Task names a policy.
    pub draft_policy: Option<String>,
    /// The ready policies, by name; each a set of the fixed predicates.
    #[serde(default)]
    pub ready_policies: BTreeMap<String, ReadyPolicy>,
    /// One adapter per forge host; a URL on a host not listed is refused.
    #[serde(default)]
    pub hosts: BTreeMap<String, ForgeHostConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgeHostConfig {
    /// `azure-devops`, `bitbucket-cloud`, `github`, `gitlab` or `fake`.
    pub kind: String,
    /// The CLI executable, a path or a name on PATH (`az`, `curl`, `gh`, `glab`).
    pub cli: Option<String>,
    /// The fake forge's directory, relative to this file.
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
    /// `fake`, for tests and trials.
    pub kind: String,
    /// The fake host's directory.
    pub dir: Option<PathBuf>,
}

/// One `[runtimes.<name>]` section: a program that speaks the runtime
/// protocol (docs/runtime/README.md).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramConfig {
    /// The executable, a path or a name on PATH.
    pub program: String,
    /// Arguments before the verb.
    #[serde(default)]
    pub args: Vec<String>,
    /// The bound on every call but `workspace`.
    pub call_timeout_ms: Option<u64>,
    /// The bound on a `workspace` call, which may clone a repository; 600000
    /// when absent.
    pub workspace_timeout_ms: Option<u64>,
    /// The first part of every key Fleet gives the runtime; give a second
    /// Fleet on the same runtime its own.
    pub key_prefix: Option<String>,
}

/// One `[hosts.<name>]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostConfig {
    /// The `[runtimes.<name>]` that serves the host.
    pub runtime: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
        Config::parse(&text, path)
    }

    /// The config `text` read from `path`, validated as [`Config::load`] does.
    pub fn parse(text: &str, path: &Path) -> Result<Config, String> {
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
        if config.starter.is_some() {
            return Err(format!(
                "config {}: [starter] is no longer read: fleet up needs no starter Task. Remove the section; to run VEGA, apply its Task from its own repository (see the configuration page)",
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
                config.gateway_briefs.insert(name.clone(), brief);
            }
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

    /// `[runtimes]` and `[hosts]` agree: each host names a runtime that is
    /// there, every runtime serves a host, and `[runtime]` is not beside them.
    fn check_runtimes(&self) -> Result<(), String> {
        if self.runtime.is_some() && (!self.runtimes.is_empty() || !self.hosts.is_empty()) {
            return Err("[runtime] and [runtimes]/[hosts] exclude each other; keep one".into());
        }
        for (host, h) in &self.hosts {
            if !self.runtimes.contains_key(&h.runtime) {
                return Err(format!(
                    "hosts.{host}.runtime {:?} names no [runtimes.{}]",
                    h.runtime, h.runtime
                ));
            }
        }
        for (name, rt) in &self.runtimes {
            if rt.program.is_empty() {
                return Err(format!("runtimes.{name}.program is empty"));
            }
            if !self.hosts.values().any(|h| &h.runtime == name) {
                return Err(format!(
                    "no host names runtimes.{name}; add [hosts.<name>] runtime = {name:?}"
                ));
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

    pub fn empty() -> Config {
        Config {
            version: CONFIG_VERSION,
            data: Data::default(),
            runtime: None,
            runtimes: BTreeMap::new(),
            hosts: BTreeMap::new(),
            starter: None,
            forge: None,
            plugins: BTreeMap::new(),
            gateway: None,
            gateway_briefs: BTreeMap::new(),
        }
    }
}

/// Explicit CLI path, then FLEET_CONFIG, then one native per-user location.
/// No search through the current directory or its parents.
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

/// A dedicated data directory per platform: `$XDG_DATA_HOME/fleet`, `~/.local/share/fleet`, or
/// `%LOCALAPPDATA%\fleet`.
pub fn default_data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("fleet"));
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d).join("fleet"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share").join("fleet"))
}
