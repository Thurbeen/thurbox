//! The session-backend boundary as thurbox reaches it: the multiplexer runtime
//! (`runtime_mux`, ADR-41), and what thurbox injects into it — its settings
//! and its data directory ([`mux_settings`]).
//!
//! Every crossing into the runtime goes through this module, so
//! `tests/architecture_rules.rs` can see which of thurbox's modules reaches
//! which part of it: consumers name the contract, and only the composition
//! roots name the factory ([`wiring`]) or an adapter.

pub use runtime_mux::{
    contract, host_path, identity, output_wake, pane, preflight, psmux, registry, rmux, tmux,
    tmux_compat,
};
pub use runtime_mux::{
    AdoptedSession, AppCopy, AppCopyRequest, BackendError, BackendLiveness, BackendRegistry,
    BackendResult, Capabilities, DiscoveredSession, Key, Located, MuxSettings, Owner, PaneChunk,
    PaneId, PaneInfo, PaneIo, PaneOutput, PaneSize, PaneState, PaneStream, Placed, Session,
    SessionBackend, SessionParser, SpawnedSession, StatusChannel, TermSignals, WindowLifecycle,
    WindowRole, WindowSpec,
};

use crate::session::settings::Settings;

/// What thurbox injects into the runtime: `settings`' scrollback, and the
/// data directory this instance runs out of, which names its local socket
/// (ADR-12).
pub fn mux_settings(settings: &Settings) -> MuxSettings {
    MuxSettings {
        scrollback_lines: settings.scrollback_lines,
        data_dir: crate::paths::data_directory(),
        relocated_data_dir: crate::paths::relocated_data_dir(),
        socket: None,
    }
}

/// Which server this instance's sessions live on (ADR-12), resolved against
/// thurbox's own data directory.
pub mod instance {
    pub use runtime_mux::instance::{
        host_socket, learn_host_socket, learn_host_socket_dir, SOCKET_OVERRIDE_ENV,
        SOCKET_OWNER_ENV, TMUX_SOCKET,
    };

    /// The socket name this instance's local sessions live on — what
    /// `thurbox-cli version --json` reports so a peer attaching over ssh joins
    /// the right server. See [`runtime_mux::instance::local_socket_name`].
    pub fn local_socket_name() -> String {
        runtime_mux::instance::local_socket_name(&super::mux_settings(&Default::default()))
    }

    /// The socket directory this instance reports to a peer (`version --json`'s
    /// `tmux_tmpdir`). See [`runtime_mux::instance::local_socket_dir_report`].
    pub fn local_socket_dir_report() -> Option<String> {
        runtime_mux::instance::local_socket_dir_report(&super::mux_settings(&Default::default()))
    }
}

/// What fills the registry for a running process: the runtime's factory over
/// the hosts in thurbox's `hosts.toml`. Only the composition roots may call it
/// (`tests/architecture_rules.rs`); every other consumer is handed the
/// registry this builds.
pub mod wiring {
    use crate::session::settings::Settings;
    use crate::session::HostRegistry;

    pub use runtime_mux::wiring::{implements, AdapterFactory, BackendSpec};

    use super::{mux_settings, BackendRegistry};

    /// The registry as a running interface needs it: one backend per adapter
    /// on this machine and on every configured or discovered host, with this
    /// platform's own multiplexer as the default.
    ///
    /// The `HostRegistry` comes back alongside because a pane needs more than
    /// a connection — a remote session's launch directory resolves against its
    /// `HostDef` — and both halves must come from the same read of
    /// `hosts.toml`. The warnings are that read's, for callers that surface
    /// them. `settings` are the ones the process loaded.
    pub fn configured(settings: &Settings) -> (BackendRegistry, HostRegistry, Vec<String>) {
        let (hosts, warnings) = crate::config::hosts::cached_registry();
        (
            runtime_mux::wiring::for_hosts(hosts, &mux_settings(settings)),
            hosts.clone(),
            warnings.clone(),
        )
    }

    /// Every adapter for this machine and no host: what a command acting only
    /// on a local row needs, without reading `hosts.toml` or discovering WSL
    /// distros.
    pub fn local_only(settings: &Settings) -> BackendRegistry {
        runtime_mux::wiring::local_only(&mux_settings(settings))
    }
}
