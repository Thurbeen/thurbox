//! `hosts.toml` in thurbox's config directory: the multiplexer runtime's
//! loader (`runtime_mux::hosts`), bound to where [`super::file`] keeps the
//! file and how it seeds it.

use std::collections::HashSet;
use std::path::PathBuf;

use runtime_mux::hosts::{self as loader, HostsConfig};
pub use runtime_mux::hosts::{parse_hosts, SEED_HOSTS_TOML};

use crate::session::{HostDef, HostRegistry, WslRepairPlan};

/// thurbox's config directory, as the runtime's loader reads `hosts.toml`.
struct ConfigDir;

impl HostsConfig for ConfigDir {
    fn read_or_seed(&self, seed: &str) -> Result<Option<String>, String> {
        super::file::read_or_seed("hosts.toml", seed)
    }

    fn discover_wsl_hosts(&self) -> Result<Vec<HostDef>, String> {
        #[cfg(test)]
        if let Some(stub) = DISCOVERY_STUB
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return stub;
        }
        loader::discover_wsl_hosts()
    }
}

/// Path to the remote-host config file: `~/.config/thurbox/hosts.toml`
/// (sibling of `config.toml`).
pub fn hosts_config_path() -> Option<PathBuf> {
    super::file::path("hosts.toml")
}

/// See [`runtime_mux::hosts::load_or_seed_with_warnings`].
pub fn load_or_seed_with_warnings() -> (HostRegistry, Vec<String>) {
    loader::load_or_seed_with_warnings(&ConfigDir)
}

/// See [`runtime_mux::hosts::load_all`].
pub fn load_all() -> HostRegistry {
    loader::load_all(&ConfigDir)
}

/// See [`runtime_mux::hosts::cached_registry`].
pub fn cached_registry() -> &'static (HostRegistry, Vec<String>) {
    loader::cached_registry(&ConfigDir)
}

/// See [`runtime_mux::hosts::wsl_repair_plan`].
pub fn wsl_repair_plan() -> Result<WslRepairPlan, String> {
    loader::wsl_repair_plan(&ConfigDir)
}

/// See [`runtime_mux::hosts::served_host_names`].
pub fn served_host_names() -> Result<HashSet<String>, String> {
    loader::served_host_names(&ConfigDir)
}

/// What WSL discovery answers instead of running `wsl.exe` while a test holds
/// [`with_discovered_wsl`]: the suite runs on Windows too, where `wsl.exe` is
/// always there, so without it an outcome would depend on the runner's distros.
#[cfg(test)]
static DISCOVERY_STUB: std::sync::Mutex<Option<Result<Vec<HostDef>, String>>> =
    std::sync::Mutex::new(None);

/// Run `f` with WSL discovery answering `stub` instead of consulting the
/// machine's own `wsl.exe`, restoring what was there before.
///
/// Serialized on a process-wide lock, and the **only** way a test may set it,
/// for the reason `paths::with_wsl_distro` gives: under plain
/// `cargo test` these tests share one process. Nest it *inside*
/// `with_wsl_distro` when a test needs both, so the two locks are always taken
/// in one order.
#[cfg(test)]
pub(crate) fn with_discovered_wsl<T>(
    stub: Result<Vec<HostDef>, String>,
    f: impl FnOnce() -> T,
) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = DISCOVERY_STUB
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .replace(stub);
    let out = f();
    *DISCOVERY_STUB.lock().unwrap_or_else(|e| e.into_inner()) = saved;
    out
}
