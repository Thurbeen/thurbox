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
pub mod presets;
pub mod registry;
pub mod repos;
pub mod runs;
pub mod selection;
pub mod snapshot;
pub mod terminal;
pub mod theme;
pub mod updates;
pub mod watch;

/// Hand `registry` everything the interface declares: every loaded plugin's
/// keys, settings, pills and chord-less commands, plus the kernel's own chords
/// and action-band entries.
///
/// One function because two readers must agree on what "declared" means. The
/// loop publishes from it, and `thurbox-cli plugin check` builds the same
/// registry to ask why a pill was dropped — and a registry missing the kernel's
/// own bindings would report a plugin's pill for `help.open` as naming an
/// action nothing declares, which is the diagnostic wrong in exactly the case
/// it exists for.
pub fn declare_interface(registry: &mut registry::Registry, host: &host::LuaHost) {
    let (mut bindings, settings, mut pills) = host.all_declarations();
    bindings.extend(modals::bindings());
    bindings.extend(clipboard::bindings());
    pills.extend(modals::pills());
    registry.declare_all(bindings, settings, pills);
    registry.declare_commands(host.commands());
}
