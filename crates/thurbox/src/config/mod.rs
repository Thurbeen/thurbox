//! thurbox's own config files, the siblings of `config.toml`: settings,
//! themes, lifecycle hooks and remote hosts. [`file`](mod@file) is the one loader every
//! one of them (and `agents.toml`) goes through.

pub mod automations;
pub mod file;
pub mod hooks;
pub mod hosts;
pub mod interface_state;
pub mod settings;
pub mod themes;
