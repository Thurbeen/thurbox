//! Where a session runs: a host entry (`host_def`), the route naming a
//! machine and a multiplexer (`route`), the multiplexers a route can name
//! (`multiplexer`) and a machine's OS (`platform`). Pure data, and one node:
//! each is defined in terms of the others.

pub mod host_def;
pub mod multiplexer;
pub mod platform;
pub mod route;

pub use host_def::{HostDef, HostKind, HostRegistry, WslRepairPlan};
pub use multiplexer::{BackendChoice, Multiplexer};
pub use platform::Platform;
pub use route::{host_of_key, Place, Route, RouteError, Via, SSH_PREFIX, WSL_PREFIX};
