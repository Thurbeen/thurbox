pub mod bands;
pub mod bookmarks;
pub mod bundled;
pub mod clipboard;
pub mod command;
pub mod config;
pub mod convert;
pub mod diff;
pub mod events;
pub mod files;
pub mod focus;
pub mod host;
pub mod inventory;
pub mod layout;
pub mod lua_api;
pub mod messages;
pub mod metrics;
pub mod mod_migration;
pub mod modals;
pub mod node;
pub mod notify;
pub mod packages;
pub mod paint;
pub mod perf;
pub mod registry;
pub mod repos;
pub mod runs;
pub mod search;
pub mod selection;
pub mod snapshot;
pub mod terminal;
pub mod theme;
pub mod updates;
pub mod watch;
pub mod worker_store;

pub fn declare_interface(registry: &mut registry::Registry, host: &host::LuaHost) {
    let (mut bindings, settings, mut pills) = host.all_declarations();
    bindings.retain(|binding| !registry::protected_action(&binding.action));
    bindings.extend(modals::bindings());
    bindings.extend(clipboard::bindings());
    pills.extend(modals::pills());
    registry.declare_all(bindings, settings, pills);
    registry.declare_commands(
        host.commands()
            .into_iter()
            .filter(|command| !registry::protected_action(&command.action))
            .collect(),
    );
    registry.declare_action_metadata(host.action_declarations());
}
