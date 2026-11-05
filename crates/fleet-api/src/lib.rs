//! Fleet's resource model and ports: pure types, no I/O.
//!
//! Everything else in the workspace depends on this crate, and this crate
//! depends on no other workspace crate. That direction is enforced by the
//! compiler, as this probe shows: an adapter is not reachable from here.
//!
//! ```compile_fail
//! // fleet-api cannot name the file store: it is an adapter above it.
//! use fleet_store::FileStore;
//! ```
//!
//! ```compile_fail
//! // Nor a controller implementation.
//! use fleet_controllers::TaskController;
//! ```
//!
//! ```compile_fail
//! // Nor anything that runs a process: the spawner...
//! use fleet_exec::run;
//! ```
//!
//! ```compile_fail
//! // ...or the runtime adapters...
//! use fleet_runtime::ThurboxRuntime;
//! ```
//!
//! ```compile_fail
//! // ...or the forge adapters.
//! use fleet_forge::GithubForge;
//! ```
//!
//! ```compile_fail
//! use fleet_plugin::StdioPlugins;
//! ```
//!
//! ```compile_fail
//! // ...or the gateway adapters.
//! use fleet_gateway::FakeGateway;
//! ```
//!
//! ```compile_fail
//! // ...or the telemetry exporter.
//! use fleet_telemetry::Telemetry;
//! ```
//!
//! The positive control: the same shape of `use` compiles for this crate.
//!
//! ```
//! use fleet_api::Observation;
//! let seen: Observation<bool> = Observation::unknown("probe timed out");
//! assert!(seen.is_unknown());
//! ```

pub mod change_request;
pub mod condition;
pub mod config;
pub mod conversation;
pub mod follow;
pub mod forge;
pub mod gateway;
pub mod inbox;
pub mod kinds;
pub mod listener;
pub mod meta;
pub mod observation;
pub mod plan;
pub mod plugin;
pub mod result;
pub mod runtime;
pub mod session;
pub mod store;
pub mod talk;
pub mod task;
pub mod validate;

pub use meta::{API_VERSION, Millis, Object, ObjectMeta, OwnerReference};
pub use observation::Observation;
pub use store::{Clock, DeleteOutcome, FieldManager, LeaseToken, Store, StoreError};
