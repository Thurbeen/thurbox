pub mod host_def;
pub mod multiplexer;
pub mod platform;
pub mod route;

pub use host_def::{HostDef, HostKind, HostRegistry};
pub use multiplexer::{BackendChoice, Multiplexer};
pub use platform::Platform;
pub use route::{host_of_key, Place, Route, RouteError, Via, SSH_PREFIX, WSL_PREFIX};
