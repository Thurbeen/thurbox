//! The session-backend boundary: the contract every multiplexer adapter
//! implements, the pane machinery a backend's stream is wired into, the
//! registry that holds the adapters, and the adapters themselves.
//!
//! Consumers name the contract; only the registry's factory names an adapter.
//! `tests/architecture_rules.rs` holds each submodule to that.

pub mod contract;
pub mod identity;
pub mod instance;
mod osc8;
pub mod output_wake;
pub mod pane;
pub mod psmux;
pub mod registry;
pub mod rmux;
pub mod tmux;
pub mod tmux_compat;
pub mod wiring;

pub use contract::{
    AdoptedSession, BackendLiveness, DiscoveredSession, Key, Located, Owner, PaneSize, PaneState,
    Placed, SessionBackend, SpawnedSession, WindowRole, WindowSpec,
};
pub use pane::{AppCopy, AppCopyRequest, Session, SessionParser, TermSignals};
pub use registry::BackendRegistry;
