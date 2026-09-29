//! The session-backend boundary: the contract every multiplexer adapter
//! implements, the pane machinery a backend's stream is wired into, the
//! registry that holds the adapters, and the adapters themselves.
//!
//! Consumers name the contract; only the registry's factory names an adapter.
//! `tests/architecture_rules.rs` holds each submodule to that.

mod osc8;
pub mod output_wake;
pub mod pane;
pub mod registry;
pub mod tmux;
pub mod tmux_compat;

pub use pane::{
    AdoptedSession, BackendLiveness, DiscoveredSession, PaneSize, Session, SessionBackend,
    SessionParser, SpawnedSession, TermSignals,
};
pub use registry::BackendRegistry;
