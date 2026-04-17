//! Cross-process bridge between `thurbox-mcp` and the TUI's plugin runtime.
//!
//! The TUI owns the one and only [`PluginRuntime`](crate::plugin::PluginRuntime);
//! `thurbox-mcp` is a separate binary that shares only the SQLite database.
//! For MCP tools that need to reach a running plugin (call a plugin-contributed
//! tool, proxy `mcp.call`), the TUI exposes a Unix domain control socket at
//! [`crate::paths::control_socket_path`] and `thurbox-mcp` connects to it.
//!
//! This module is the **sealed communication primitive** that both sides use:
//!
//! - [`wire`] — the shared request/response frame types and op-name constants.
//!   One file, one source of truth. Both the TUI listener (in
//!   `plugin::control_socket`) and the MCP client (in [`client`]) reference
//!   these same strings and structs.
//! - [`client::ControlClient`] — the `thurbox-mcp`-side client. Clone-friendly,
//!   short-lived connection per call. Must tolerate "TUI not running" cleanly:
//!   a missing socket or refused connection returns a clean error so the rest
//!   of the MCP surface (roles, sessions, themes) keeps working.
//!
//! ## Architecture
//!
//! `plugin_bridge` imports only `crate::paths` from the thurbox crate plus
//! stdlib + `serde`/`serde_json`/`tokio`. It must not import `agent`, `app`,
//! `ui`, `git`, `mcp`, `plugin`, `session`, `storage`, `sync`, or
//! `session_ops` — it's a transport layer, not a consumer of any of them.
//! Enforced by `tests/architecture_rules.rs::plugin_bridge_module_isolation`.

pub mod client;
pub mod wire;

pub use client::{ControlClient, ControlError};
