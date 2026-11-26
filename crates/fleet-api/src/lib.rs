//! ```compile_fail
//! use fleet_store::FileStore;
//! ```
//! ```compile_fail
//! use fleet_controllers::TaskController;
//! ```
//! ```compile_fail
//! use fleet_exec::run;
//! ```
//! ```compile_fail
//! use fleet_runtime::ExecRuntime;
//! ```
//! ```compile_fail
//! use fleet_forge::GithubForge;
//! ```
//! ```compile_fail
//! use fleet_plugin::StdioPlugins;
//! ```
//! ```compile_fail
//! use fleet_gateway::FakeGateway;
//! ```
//! ```compile_fail
//! use otel_env::Telemetry;
//! ```

pub mod board;
pub mod change_request;
pub mod condition;
pub mod config;
pub mod conversation;
pub mod cron_task;
pub mod doctor;
pub mod event;
pub mod follow;
pub mod forge;
pub mod gateway;
pub mod harness;
pub mod inbox;
pub mod integration;
pub mod kinds;
pub mod listener;
pub mod meta;
pub mod observation;
pub mod plan;
pub mod plugin;
pub mod result;
pub mod runtime;
pub mod screen;
pub mod session;
pub mod signal;
pub mod store;
pub mod subscription;
pub mod talk;
pub mod task;
pub mod task_event;
pub mod validate;
pub mod workspace;

pub use meta::{API_VERSION, Millis, Object, ObjectMeta, OwnerReference};
pub use observation::Observation;
pub use store::{Clock, DeleteOutcome, FieldManager, LeaseToken, Store, StoreError};
