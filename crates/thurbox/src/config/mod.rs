//! thurbox's own config files, the siblings of `config.toml`: settings,
//! themes and the interface's state. [`file`](mod@file) is the one loader every
//! one of them goes through.

pub mod file;
pub mod interface_state;
pub mod settings;
pub mod themes;
