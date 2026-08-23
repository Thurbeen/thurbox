//! Program panes plugins asked for (`Capability::Program`).
//!
//! A pane belongs to a **plugin**, not a session: the kernel stamps the owner
//! from the plugin being rendered, so naming another plugin's pane is
//! impossible by construction. The slots live on [`Terminals`] beside the
//! session panes because a pane needs everything that struct already holds —
//! the backend registry, the paint seam, the redraw stamp, the rect memo — and
//! `SurfaceProvider` has one implementor by design.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use ratatui::layout::Rect;

use super::Terminals;

/// Which plugin's pane, and which of that plugin's panes.
///
/// The plugin is identified by **path**, the identity trust, the disabled set,
/// `run`'s attribution and the inventory all already use. Keying on a declared
/// name instead would let two files claim one pane by declaring the same name,
/// and would move a pane when its author renamed the plugin.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProgramKey {
    /// The owning plugin's path, relative to the interface directory.
    pub plugin: String,
    /// The name the plugin gave this pane.
    pub name: String,
}

impl ProgramKey {
    pub fn new(plugin: &str, name: &str) -> Self {
        Self {
            plugin: plugin.to_string(),
            name: name.to_string(),
        }
    }

    /// The surface id this pane is addressed by.
    ///
    /// Prefixed so it cannot be confused with a session id, which is a UUID, and
    /// so a paint can tell the two apart by inspection.
    pub fn surface_id(&self) -> String {
        format!("{PROGRAM_SURFACE_PREFIX}{}#{}", self.plugin, self.name)
    }
}

/// Does this surface id name a plugin's program rather than a session?
///
/// A prefix test, and deliberately **not** a parse. Splitting the id back into
/// `(plugin, name)` looked obvious and is ambiguous in both directions: a pane
/// name may contain the separator, and so — on POSIX — may a file name, so
/// neither the first nor the last `#` is reliably the right one. Since the keys
/// are what *generate* these ids, the way back is to look one up
/// ([`Terminals::program_key`]) rather than to take it apart.
pub fn is_program_surface(id: &str) -> bool {
    id.starts_with(PROGRAM_SURFACE_PREFIX)
}

/// Refuse a pane name that would make an unreadable window or an ambiguous id.
///
/// Checked where the ask arrives, so the author gets a message instead of a pane
/// that never appears. The name reaches a tmux window name, which tmux parses as
/// part of a target string.
pub fn validate_program_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a program pane needs a name".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "program pane name {name:?} must be letters, digits, `-` or `_`"
        ));
    }
    Ok(())
}

/// Marks a surface id as naming a plugin's program rather than a session.
///
/// A session id is a UUID, so no session can collide with this — but it is
/// spelled out rather than relying on that, because "a session id is a UUID" is a
/// fact about the id generator and this is a parsing rule.
pub const PROGRAM_SURFACE_PREFIX: &str = "program:";

/// The most panes one plugin may hold at once.
///
/// Matches `runs::MAX_CONCURRENT` so there is one number to remember rather than
/// two. `run`'s other bounds deliberately do not transfer — an output cap is
/// meaningless for a screen overwritten in place, and a timeout is the opposite of
/// what an interactive program wants — which leaves how many as the only thing
/// left to bound, and the reason the capability is granted separately.
pub const MAX_PROGRAMS_PER_PLUGIN: usize = 4;

/// Which panes belong to a plugin that is no longer loaded.
///
/// Separated from releasing them so the decision is testable without a
/// multiplexer — releasing kills a real process, choosing what to release is a set
/// difference. The same split `admit_program` makes, for the same reason.
fn stale_program_keys<'a>(
    keys: impl Iterator<Item = &'a ProgramKey>,
    live: &[String],
) -> Vec<ProgramKey> {
    keys.filter(|key| !live.contains(&key.plugin))
        .cloned()
        .collect()
}

/// May a plugin already holding `held` panes take one more, for `program`?
///
/// Pure, and separated from starting the pane for the reason `bundled::decide`
/// is: the decision is the part worth testing, and testing it through the spawn
/// would need a multiplexer, which the unit suite deliberately does not have.
///
/// The refusal is a message rather than a silent no, because the plugin surfaces
/// it — a pane that says why it is empty is the difference between a bound and a
/// bug.
fn admit_program(plugin: &str, held: usize, program: &str) -> Result<(), String> {
    if program.trim().is_empty() {
        return Err("a program pane needs a program to run".to_string());
    }
    if held >= MAX_PROGRAMS_PER_PLUGIN {
        return Err(format!(
            "{plugin} already holds {held} program panes (the limit is \
             {MAX_PROGRAMS_PER_PLUGIN})"
        ));
    }
    Ok(())
}

/// One plugin's program pane, and the rect it was last painted into.
pub(super) struct ProgramSlot {
    pub(super) pane: crate::agent::backend::ProgramPane,
    /// Last rect painted, so a resize happens on change rather than per frame.
    pub(super) size: Cell<(u16, u16)>,
    /// Where it was last painted, so a click can be mapped into its grid. Cleared
    /// each frame with every other surface's, so a pane that stopped drawing
    /// cannot be hit (see [`Terminals::forget_rects`]).
    pub(super) rect: Cell<Rect>,
    /// What the plugin asked to run, kept so a restart can re-adopt with the same
    /// program recorded and a report can name it.
    pub(super) program: String,
}

impl Terminals {
    /// Start `program` in a pane belonging to `key.plugin`, or keep the one it
    /// already has.
    ///
    /// **Idempotent**, deliberately: a plugin asks on every frame — the pattern
    /// `run` established — so asking for a pane that exists must be a map lookup
    /// and not a second copy of the program.
    ///
    /// The pane is born at `rows`/`cols`, which the caller passes as the rect it
    /// will be painted into where that is known. `open_shell` documents why: the
    /// render-time resize cannot correct a bad birth size when the size memo has
    /// already been set, so the pane looks settled while being a screen wide.
    pub fn start_program(
        &mut self,
        key: &ProgramKey,
        program: &str,
        args: &[String],
        cwd: Option<&std::path::Path>,
        rows: u16,
        cols: u16,
    ) -> Result<(), String> {
        if let Some(slot) = self.programs.get(key) {
            // Already running, or finished and not yet reaped. A finished one is
            // replaced, which is what makes "ask again to restart it" work.
            if !slot.pane.has_exited() {
                return Ok(());
            }
            self.release_program(key);
        }
        admit_program(&key.plugin, self.program_count(&key.plugin), program)?;

        // Local only, by design: a plugin-owned pane has no session and therefore
        // no host, so there is nothing to resolve one from. Recorded as an open
        // question rather than guessed at.
        let backend = Arc::clone(self.backends.default_backend());
        // Inline rather than on a worker, following `ensure_shell_pane`, which
        // spawns from the command drain the same way. Rule 5's concern is a *down
        // remote host* running out its ssh timeout; readying and spawning on the
        // local multiplexer is a round trip on a socket that is already there.
        if let Err(e) = backend.ensure_ready() {
            return Err(format!("could not reach the local multiplexer: {e:#}"));
        }

        let window = crate::agent::tmux::program_window_name(
            &crate::kernel::bundled::digest(&key.plugin),
            &key.name,
        );
        let env: HashMap<String, String> = HashMap::new();
        let (rows, cols) = (rows.max(1), cols.max(1));

        // An existing window of that name is this same pane from a previous run of
        // the interface: the name is deterministic, so finding it IS the
        // re-adoption path — there is no stored pane id that could go stale.
        let existing = self.find_program_window(&backend, &window);
        let pane = match existing {
            Some(backend_id) => crate::agent::backend::ProgramPane::adopt(
                Arc::clone(&backend),
                &backend_id,
                program,
                rows,
                cols,
            ),
            None => crate::agent::backend::ProgramPane::spawn(
                Arc::clone(&backend),
                &window,
                program,
                args,
                cwd,
                &env,
                rows,
                cols,
            ),
        }
        .map_err(|e| format!("could not start {program}: {e:#}"))?;

        self.programs.insert(
            key.clone(),
            ProgramSlot {
                pane,
                size: Cell::new((rows, cols)),
                rect: Cell::new(Rect::default()),
                program: program.to_string(),
            },
        );
        Ok(())
    }

    /// A window of this exact name already running on `backend`, if there is one.
    ///
    /// This is the whole of re-adoption after a restart: the name is deterministic
    /// (`tmux::program_window_name`), so it is enough to look. Nothing is
    /// persisted, and therefore nothing can be stale — which is the failure a
    /// stored pane id invites and this repository has been bitten by before.
    ///
    /// A tmux round trip, so it is called when a pane is *started* and never per
    /// frame.
    fn find_program_window(
        &self,
        backend: &Arc<dyn crate::agent::SessionBackend>,
        window: &str,
    ) -> Option<String> {
        // `find_window`, not `discover`: discovery filters to agent windows
        // (`tb-`) and a program pane's prefix is `tbp-`, so it would never be seen
        // there. That is also why it cannot be adopted as a session.
        backend.find_window(window).ok().flatten()
    }

    /// The key a surface id names, by matching against the keys that generate
    /// them — see `is_program_surface` for why this is a lookup and not a parse.
    ///
    /// Linear over a map holding at most `MAX_PROGRAMS_PER_PLUGIN` per plugin,
    /// on a path that runs once per program surface per frame.
    pub fn program_key(&self, surface: &str) -> Option<&ProgramKey> {
        if !is_program_surface(surface) {
            return None;
        }
        self.programs.keys().find(|key| key.surface_id() == surface)
    }

    /// How many panes a plugin is holding.
    pub fn program_count(&self, plugin: &str) -> usize {
        self.programs
            .keys()
            .filter(|key| key.plugin == plugin)
            .count()
    }

    /// Give up one pane, killing the program in it.
    ///
    /// Killed rather than left running: unlike a session, nothing in the interface
    /// could ever show it again, and an unreachable program is worse than a closed
    /// one.
    pub fn release_program(&mut self, key: &ProgramKey) -> bool {
        match self.programs.remove(key) {
            Some(slot) => {
                slot.pane.kill();
                true
            }
            None => false,
        }
    }

    /// Release every pane held by a plugin that is no longer loaded.
    ///
    /// Mirrors `runs::retain_plugins`, which `reload_interface` already calls for
    /// the same reason: a plugin edited away, renamed, removed or turned off must
    /// not leave anything of itself behind across reloads.
    pub fn retain_program_plugins(&mut self, live: &[String]) {
        for key in stale_program_keys(self.programs.keys(), live) {
            self.release_program(&key);
        }
    }

    /// Send bytes to a plugin's program.
    #[must_use = "the caller decides whether the keystroke was consumed from this"]
    pub fn send_to_program(&self, key: &ProgramKey, bytes: Vec<u8>) -> bool {
        let Some(slot) = self.programs.get(key) else {
            return false;
        };
        if slot.pane.has_exited() {
            return false;
        }
        slot.pane.send_input(bytes).is_ok()
    }

    /// What is running in a pane, and whether it has ended — for a pane that wants
    /// to report rather than paint a frozen grid.
    pub fn program_state(&self, key: &ProgramKey) -> Option<(&str, bool)> {
        self.programs
            .get(key)
            .map(|slot| (slot.program.as_str(), slot.pane.has_exited()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program surface is distinguishable from a session's by inspection, and a
    /// session id must never read as one — otherwise a plugin could aim
    /// keystrokes at somebody's agent.
    #[test]
    fn a_program_surface_is_told_from_a_session_surface() {
        let key = ProgramKey::new("plugins/90_watch.lua", "watch");
        assert!(is_program_surface(&key.surface_id()));
        for not_a_program in [
            "5f9c1f6e-1b2a-4c3d-8e9f-0a1b2c3d4e5f",
            "5f9c1f6e-1b2a-4c3d-8e9f-0a1b2c3d4e5f#shell",
            "",
        ] {
            assert!(!is_program_surface(not_a_program), "{not_a_program:?}");
        }
    }

    /// The id is resolved by looking up the key that generated it, never by
    /// splitting it.
    ///
    /// Splitting looked obvious and is ambiguous in both directions — a pane name
    /// may contain the separator, and on POSIX so may a file name — so neither the
    /// first nor the last `#` is reliably the right one. A lookup cannot be wrong.
    #[test]
    fn a_program_surface_resolves_by_lookup_not_by_splitting() {
        let terminals = Terminals::new();
        // Nothing is running, so nothing resolves — including a well-formed id.
        let key = ProgramKey::new("plugins/90_watch.lua", "watch");
        assert!(terminals.program_key(&key.surface_id()).is_none());
        assert!(terminals.program_key("not-a-program").is_none());
    }

    /// The name reaches a tmux window name, which tmux parses as part of a target
    /// string — so it is checked where the ask arrives rather than mangled.
    #[test]
    fn a_program_pane_name_is_checked_when_it_is_asked_for() {
        for good in ["watch", "top-2", "my_pane", "a1"] {
            assert!(validate_program_name(good).is_ok(), "{good}");
        }
        for bad in ["", "watch#2", "a b", "../x", "hé"] {
            assert!(validate_program_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_plugin_may_not_hold_unbounded_panes() {
        for held in 0..MAX_PROGRAMS_PER_PLUGIN {
            assert!(
                admit_program("plugins/90_watch.lua", held, "watch").is_ok(),
                "{held} should be admitted"
            );
        }
        let refused = admit_program("plugins/90_watch.lua", MAX_PROGRAMS_PER_PLUGIN, "watch")
            .expect_err("the limit is a limit");
        // The refusal names the plugin and the limit, because the plugin shows it.
        assert!(refused.contains("90_watch.lua"), "{refused}");
        assert!(
            refused.contains(&MAX_PROGRAMS_PER_PLUGIN.to_string()),
            "{refused}"
        );
    }

    #[test]
    fn a_pane_with_no_program_named_is_refused() {
        // Otherwise the pane spawns the backend's idea of an empty command line.
        assert!(admit_program("plugins/90_watch.lua", 0, "   ").is_err());
        assert!(admit_program("plugins/90_watch.lua", 0, "").is_err());
    }

    /// Nothing running means nothing to send to, and nothing to report — asserted
    /// because "no pane" must not read as "a pane that swallowed your key".
    ///
    /// This return value is **load-bearing for key routing**: the loop consumes a
    /// keystroke only when a send reports delivery. A pane whose surface names a
    /// session that is no longer live therefore does not swallow `Esc`, which is
    /// what keeps the user from being trapped in a pane showing a dead terminal.
    #[test]
    fn an_absent_program_pane_accepts_nothing_and_reports_nothing() {
        let terminals = Terminals::new();
        let key = ProgramKey::new("plugins/90_watch.lua", "watch");
        assert_eq!(terminals.program_count("plugins/90_watch.lua"), 0);
        assert!(!terminals.send_to_program(&key, b"x".to_vec()));
        assert!(terminals.program_state(&key).is_none());
    }

    /// A plugin that is still loaded keeps its pane; one that is gone does not.
    ///
    /// The first half is the one that matters most: a reload happens after every
    /// edit, and losing a running program to one would make reloading unusable.
    #[test]
    fn a_reload_keeps_a_live_plugins_pane_and_releases_a_vanished_ones() {
        let watch = ProgramKey::new("plugins/90_watch.lua", "watch");
        let gone = ProgramKey::new("plugins/91_deleted.lua", "top");
        let keys = [watch.clone(), gone.clone()];

        // Both still loaded: nothing is released.
        let live = vec![
            "plugins/90_watch.lua".to_string(),
            "plugins/91_deleted.lua".to_string(),
        ];
        assert!(stale_program_keys(keys.iter(), &live).is_empty());

        // One edited away, renamed or turned off: only its pane goes.
        let live = vec!["plugins/90_watch.lua".to_string()];
        assert_eq!(stale_program_keys(keys.iter(), &live), vec![gone]);

        // Every plugin gone (a failed reload leaves none loaded): all of them.
        let all: Vec<ProgramKey> = stale_program_keys(keys.iter(), &[]);
        assert_eq!(all.len(), 2);
        let _ = watch;
    }
}
