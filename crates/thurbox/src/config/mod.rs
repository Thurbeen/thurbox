//! thurbox's own config files, the siblings of `config.toml`: settings,
//! themes, automations, the interface's state and remote hosts. [`file`](mod@file) is the one loader every
//! one of them goes through.

pub mod automations;
pub mod file;
pub mod hosts;
pub mod interface_state;
pub mod settings;
pub mod themes;
