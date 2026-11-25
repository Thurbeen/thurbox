pub mod contract;
pub mod host;
pub mod host_path;
pub mod hyperlink;
pub mod identity;
pub mod instance;
mod osc8;
pub mod output_wake;
pub mod pane;
pub mod path_lookup;
pub mod place;
pub mod preflight;
pub mod protocol;
pub mod psmux;
pub mod registry;
pub mod rmux;
pub mod session_id;
pub mod settings;
pub mod shell;
pub mod tmux;
pub mod tmux_compat;
pub mod view;
pub mod wiring;
pub mod workspace;

pub use contract::{
    AdoptedSession, BackendError, BackendLiveness, BackendResult, Capabilities, DiscoveredSession,
    Key, Located, Owner, PaneChunk, PaneId, PaneIo, PaneOutput, PaneSize, PaneState, PaneStream,
    Placed, SessionBackend, SpawnedSession, StatusChannel, WindowLifecycle, WindowRole, WindowSpec,
};
pub use host::{host_def, multiplexer, platform, route};
pub use host_def::{HostDef, HostKind, HostRegistry};
pub use hyperlink::{HyperlinkRun, HyperlinkTable, VisibleRun};
pub use multiplexer::{BackendChoice, Multiplexer};
pub use pane::{AppCopy, AppCopyRequest, PaneInfo, Session, SessionParser, TermSignals};
pub use platform::Platform;
pub use registry::BackendRegistry;
pub use route::{host_of_key, Place, Route, RouteError, Via, SSH_PREFIX, WSL_PREFIX};
pub use session_id::SessionId;
pub use settings::MuxSettings;
