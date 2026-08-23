//! Headless extension activation, deactivation, and self-healing.
//!
//! An extension manifest (`ExtensionDef`) declares the sessions/automations an
//! opt-in extension needs. These helpers make that declaration real and keep it
//! real:
//!
//! - [`ensure_extension`] idempotently (re)creates any missing declared
//!   resources — the self-heal primitive run at TUI startup and on every
//!   headless `automation tick`.
//! - [`activate_extension`] = `ensure` + record the extension in the active set
//!   (SQLite `metadata`), so self-heal will resurrect its resources if deleted.
//! - [`deactivate_extension`] tears the resources down and clears the active-set
//!   entry, so self-heal stops resurrecting it. This is the real off-switch.
//!
//! Deleting an extension's session/automation by hand (TUI `Ctrl+D`, `clean`,
//! `thurbox-cli session/automation delete`) is therefore a no-op while the
//! extension is active: the next ensure pass recreates it. `deactivate` is how a
//! user turns an extension off for good.
//!
//! This module reaches no `agent::` symbols — spawn/delete go through the
//! sibling `session_ops` helpers and everything else is `storage`/`session`.

mod fs;
mod install;
mod lifecycle;

#[cfg(test)]
mod tests;

pub(crate) use fs::MANAGED_MARKER;
pub(crate) use install::HOOK_SIGNAL_MARKER;
pub use install::{
    install_extension, reinstall_extension, uninstall_extension, update_all_extensions,
    update_extension, InstallReport, ReinstallReport, UninstallReport, UpdateReport,
};
pub use lifecycle::{
    activate_extension, deactivate_extension, ensure_extension, extension_health,
    heal_active_extensions, DeactivateReport, EnsureReport, ExtensionHealth,
};
