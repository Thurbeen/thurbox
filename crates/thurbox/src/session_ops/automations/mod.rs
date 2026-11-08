//! Automations, one implementation per verb (issue #1468).
//!
//! The interface's pane, `thurbox-cli automation` and the heartbeat each used
//! to carry their own copy of saving, firing and scheduling, and the copies
//! had drifted: the CLI's edit skipped the timezone check, the heartbeat's
//! loop lived in `cli`. Every verb is here now, and the callers keep only
//! their argument parsing and output.
//!
//! - [`mod@save`]: create (keyed and idempotent), edit, enable, run-now, delete.
//! - [`mod@reference`]: an automation by id, key or unique name.
//! - [`mod@fire`]: what one fire does — a send, a spawn, or a bounded exec.
//! - [`mod@tick`]: the due pass and `run --wait`, with the run row and run lock.
//! - [`mod@tick_log`]: what the last tick did, and every one that failed.

pub mod fire;
pub mod reference;
pub mod save;
pub mod tick;
pub mod tick_log;

pub use fire::ExecJob;
pub use reference::{resolve, RefError};
pub use save::{create, edit, remove, set_enabled, trigger, Draft, OnExisting, SaveError, Saved};
pub use tick::{run_now, tick, DuePass, Fired, RunNowError, Skipped};
