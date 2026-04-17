//! Control socket: the transport thurbox-mcp uses to reach running plugins.
//!
//! The socket itself (a Unix domain socket under `$XDG_RUNTIME_DIR`) is served
//! by a listener task spawned from `main`. This module's job is narrower:
//!
//! - [`dispatch`] is the pure-logic half. Given a `PluginRuntime` + a `Database`
//!   + an op string + a JSON params object, it produces a `Result<Value,
//!   String>`. It has no I/O of its own, which makes it trivial to unit-test.
//!
//! - The listener (added in a follow-up file) owns the `UnixListener`, frames
//!   bytes into [`crate::plugin_bridge::wire::Request`] records, and forwards
//!   each one to [`dispatch::dispatch`].
//!
//! Splitting the two means the listener can be tested end-to-end over a pair
//! of in-memory sockets without any plugin state, and `dispatch` can be
//! tested with a fake runtime without any socket at all.

pub mod dispatch;
pub mod listener;

pub use dispatch::dispatch;
pub use listener::{spawn, ControlSocketHandle};
