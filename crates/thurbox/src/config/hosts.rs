//! `hosts.toml` in thurbox's config directory: the multiplexer runtime's
//! loader (`runtime_mux::hosts`), bound to where [`super::file`] keeps the
//! file and how it seeds it.

use std::path::PathBuf;

use runtime_mux::hosts::{self as loader, HostsConfig};
pub use runtime_mux::hosts::{parse_hosts, SEED_HOSTS_TOML};

use crate::session::{HostDef, HostRegistry};

/// thurbox's config directory, as the runtime's loader reads `hosts.toml`.
struct ConfigDir;

impl HostsConfig for ConfigDir {
    fn read_or_seed(&self, seed: &str) -> Result<Option<String>, String> {
        super::file::read_or_seed("hosts.toml", seed)
    }

    fn discover_wsl_hosts(&self) -> Result<Vec<HostDef>, String> {
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
