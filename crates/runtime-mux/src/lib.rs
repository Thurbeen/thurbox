//! The multiplexer runtime: agent sessions as windows of a terminal
//! multiplexer (tmux, psmux, RMUX), on this machine or on a host reached over
//! ssh or `wsl.exe`.
//!
//! The session-backend boundary: the contract every multiplexer adapter
//! implements, the pane machinery a backend's stream is wired into, the
//! registry that holds the adapters, and the adapters themselves — with the
//! host model a route names (`host_def`, `route`, `multiplexer`, `platform`),
//! how a command reaches a host (`shell`, `host_path`), whether the binaries
//! it needs are there (`preflight`) and `hosts.toml` (`hosts`).
//!
//! Consumers name the contract; only the registry's factory names an adapter.
//! `tests/architecture_rules.rs` holds each module to that. The crate knows
//! nothing of the program embedding it: the settings it honours and where that
//! program keeps its data are passed in ([`MuxSettings`],
//! [`hosts::HostsConfig`]).

pub mod contract;
pub mod host;
pub mod host_path;
pub mod hosts;
pub mod hyperlink;
pub mod identity;
pub mod instance;
mod osc8;
pub mod output_wake;
pub mod pane;
pub mod path_lookup;
pub mod preflight;
pub mod psmux;
pub mod registry;
pub mod rmux;
pub mod session_id;
pub mod settings;
pub mod shell;
pub mod tmux;
pub mod tmux_compat;
pub mod toml_file;
pub mod wiring;

pub use contract::{
    AdoptedSession, BackendError, BackendLiveness, BackendResult, Capabilities, DiscoveredSession,
    Key, Located, Owner, PaneChunk, PaneId, PaneIo, PaneOutput, PaneSize, PaneState, PaneStream,
    Placed, SessionBackend, SpawnedSession, StatusChannel, WindowLifecycle, WindowRole, WindowSpec,
};
pub use host::{host_def, multiplexer, platform, route};
pub use host_def::{HostDef, HostKind, HostRegistry, WslRepairPlan};
pub use hyperlink::{HyperlinkRun, HyperlinkTable, VisibleRun};
pub use multiplexer::{BackendChoice, Multiplexer};
pub use pane::{AppCopy, AppCopyRequest, PaneInfo, Session, SessionParser, TermSignals};
pub use platform::Platform;
pub use registry::BackendRegistry;
pub use route::{Place, Route, RouteError, Via, SSH_PREFIX, WSL_PREFIX};
pub use session_id::SessionId;
pub use settings::MuxSettings;
