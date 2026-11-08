//! How a command reaches a machine — quoting, the ssh and `wsl.exe` launchers,
//! a host's shell — as the multiplexer runtime defines it. One home for the
//! quoting rules, so this module only re-exports `runtime_mux::shell`; it
//! stays a module of its own so `tests/architecture_rules.rs` can say which
//! of thurbox's modules may launch anything.

pub use runtime_mux::shell::*;
