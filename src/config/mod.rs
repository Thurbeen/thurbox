//! thurbox's own config files, the siblings of `config.toml`: settings,
//! themes, lifecycle hooks and remote hosts. [`file`](mod@file) is the one loader every
//! one of them (and `agents.toml` and the extension manifests) goes through.

pub mod file;
pub mod hooks;
pub mod hosts;
pub mod settings;
pub mod themes;
