//! The v2 plugin kernel: a session engine with a Lua-driven renderer.
//!
//! The kernel owns no pane. It owns a *vocabulary* ([`node`], four primitives),
//! the arithmetic that resolves where things go ([`layout`]), a read-only view
//! of the session engine ([`snapshot`]), and the VM that plugins run in
//! ([`host`]). Everything a user sees — the session list included — is Lua.

pub mod bands;
pub mod bundled;
pub mod clipboard;
pub mod command;
pub mod config;
pub mod consent;
pub mod convert;
pub mod diff;
pub mod events;
pub mod files;
pub mod focus;
pub mod host;
pub mod inventory;
pub mod layout;
pub mod messages;
pub mod metrics;
pub mod modals;
pub mod node;
pub mod notify;
pub mod packages;
pub mod paint;
pub mod perf;
pub mod reaper;
pub mod registry;
pub mod repos;
pub mod runs;
pub mod selection;
pub mod snapshot;
pub mod terminal;
pub mod theme;
pub mod updates;
pub mod watch;
