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

pub fn mux_settings(settings: &Settings) -> MuxSettings {
    MuxSettings {
        scrollback_lines: settings.scrollback_lines,
        data_dir: crate::paths::data_directory(),
        relocated_data_dir: crate::paths::relocated_data_dir(),
        socket: None,
    }
}

pub mod instance {
    pub use runtime_mux::instance::{
        host_socket, learn_host_socket, learn_host_socket_dir, SOCKET_OVERRIDE_ENV,
        SOCKET_OWNER_ENV, TMUX_SOCKET,
    };

    pub fn local_socket_name() -> String {
        runtime_mux::instance::local_socket_name(&super::mux_settings(&Default::default()))
    }

    pub fn local_socket_dir_report() -> Option<String> {
        runtime_mux::instance::local_socket_dir_report(&super::mux_settings(&Default::default()))
    }
}

pub mod wiring {
    use crate::session::settings::Settings;

    pub use runtime_mux::wiring::{implements, AdapterFactory, BackendSpec};

    use super::{mux_settings, BackendRegistry};

    pub fn local_only(settings: &Settings) -> BackendRegistry {
        runtime_mux::wiring::local_only(&mux_settings(settings))
    }
}
