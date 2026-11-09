//! Fleet's controllers: pure functions from what is stored and observed to a
//! [`fleet_api::plan::Plan`]. No I/O happens here; the manager performs the
//! plan through the adapters.
//!
//! - [`TaskController`] runs one agent session for one brief.
//! - [`ChangeRequestController`] mirrors one change on a forge.
//! - [`ForgeController`] follows the changes a Task's worker declared and
//!   relays what is new into the Task's inbox.
//! - [`ConversationController`] mirrors one chat thread and keeps its tie
//!   to a Task honest.
//! - [`ListenerController`] judges one chat account's listener from its
//!   heartbeats.
//! - [`GatewayController`] keeps a Task's tie to its chat thread, relays
//!   the thread into the Task's inbox and a child's words into its
//!   parent's.
//!
//! Adapters sit above this crate and are unreachable from it, which the
//! compiler enforces:
//!
//! ```compile_fail
//! // A controller cannot reach the file store...
//! use fleet_store::FileStore;
//! ```
//!
//! ```compile_fail
//! // ...nor a runtime adapter...
//! use fleet_runtime::FakeRuntime;
//! ```
//!
//! ```compile_fail
//! // ...nor a forge adapter...
//! use fleet_forge::FakeForge;
//! ```
//!
//! ```compile_fail
//! // ...nor the manager that schedules it...
//! use fleet_manager::Manager;
//! ```
//!
//! ```compile_fail
//! // ...nor start a process.
//! use fleet_exec::run;
//! ```
//!
//! ```compile_fail
//! use fleet_plugin::StdioPlugins;
//! ```
//!
//! ```compile_fail
//! // ...nor a gateway adapter.
//! use fleet_gateway::FakeGateway;
//! ```
//!
//! ```compile_fail
//! // ...nor the telemetry exporter.
//! use fleet_telemetry::Telemetry;
//! ```
//!
//! The positive control: the port types it decides over are in scope.
//!
//! ```
//! use fleet_controllers::{
//!     ChangeRequestController, ConversationController, ForgeController, GatewayController,
//!     ListenerController, TaskController,
//! };
//! assert_eq!(TaskController::KIND, "Task");
//! assert_eq!(ChangeRequestController::KIND, "ChangeRequest");
//! assert_eq!(ForgeController::KIND, "Task");
//! assert_eq!(GatewayController::KIND, "Task");
//! assert_eq!(ConversationController::KIND, "Conversation");
//! assert_eq!(ListenerController::KIND, "Listener");
//! ```

pub mod change_request;
pub mod conversation;
pub mod forge;
pub mod gateway;
pub mod listener;
pub mod plugin;
pub mod task;

pub use change_request::{ChangeRequestController, ChangeRequestInput, ChangeRequestObservations};
pub use conversation::{ConversationController, ConversationInput, ConversationObservations};
pub use forge::{ForgeController, ForgeInput, ForgeObservations};
pub use gateway::{GatewayController, GatewayInput, GatewayObservations};
pub use listener::{ListenerController, ListenerInput, ListenerObservations};
pub use task::{MAX_ATTEMPTS, RETRY_DELAY_MS, SESSION_RECHECK_MS, TaskController, TaskInput};
