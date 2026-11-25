use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use ratatui::layout::Rect;
use tracing::debug;

use super::Terminals;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProgramKey {
    pub owner: String,
    pub name: String,
}

impl ProgramKey {
    pub fn new(mod_name: &str, name: &str) -> Self {
        Self {
            owner: mod_name.to_string(),
            name: name.to_string(),
        }
    }

    pub fn surface_id(&self) -> String {
        format!("{PROGRAM_SURFACE_PREFIX}{}#{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramTransition {
    Started(ProgramKey),
    Replaced(ProgramKey, String),
}

pub fn is_program_surface(id: &str) -> bool {
    id.starts_with(PROGRAM_SURFACE_PREFIX)
}

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

pub const PROGRAM_SURFACE_PREFIX: &str = "program:";

pub const MAX_PROGRAMS_PER_MOD: usize = 4;

fn stale_program_keys<'a>(
    keys: impl Iterator<Item = &'a ProgramKey>,
    live: &[String],
) -> Vec<ProgramKey> {
    keys.filter(|key| !live.contains(&key.owner))
        .cloned()
        .collect()
}

fn admit_program(mod_name: &str, held: usize, program: &str) -> Result<(), String> {
    if program.trim().is_empty() {
        return Err("a program pane needs a program to run".to_string());
    }
    if held >= MAX_PROGRAMS_PER_MOD {
        return Err(format!(
            "{mod_name} already holds {held} program panes (the limit is \
             {MAX_PROGRAMS_PER_MOD})"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeysPlan {
    Send,
    Start,
    Refuse(String),
}

pub fn plan_keys(mod_name: &str, name: &str, running: bool, program: &str) -> KeysPlan {
    if running {
        return KeysPlan::Send;
    }
    if program.trim().is_empty() {
        return KeysPlan::Refuse(format!(
            "{mod_name} has no running program named {name:?} to type into"
        ));
    }
    KeysPlan::Start
}

pub(super) struct ProgramSlot {
    pub(super) pane: crate::backend::pane::ProgramPane,
    pub(super) painted: super::Painted,
    pub(super) program: String,
}

impl Terminals {
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
            self.program_transitions.push(ProgramTransition::Replaced(
                key.clone(),
                slot.program.clone(),
            ));
            self.release_program(key);
        }
        admit_program(&key.owner, self.program_count(&key.owner), program)?;

        let backend = Arc::clone(self.backends.default_backend());
        // This local-only spawn still runs inline; a companion shell opens
        // through Fleet, on a worker of its own.
        if let Err(e) = backend.ensure_ready() {
            return Err(format!("could not reach the local multiplexer: {e:#}"));
        }

        let window = crate::backend::identity::program_window_name(
            &crate::kernel::bundled::digest(&key.owner),
            &key.name,
        );
        let env: HashMap<String, String> = HashMap::new();
        let (rows, cols) = (rows.max(1), cols.max(1));

        // An existing window of that name is this same pane from a previous run of
        // the interface: the name is deterministic, so finding it IS the
        // re-adoption path — there is no stored pane id that could go stale.
        let existing = self.find_program_window(&backend, &window);
        let adopted = existing.is_some();
        let pane = match existing {
            Some(backend_id) => crate::backend::pane::ProgramPane::adopt(
                Arc::clone(&backend),
                &backend_id,
                program,
                rows,
                cols,
                self.scrollback,
            ),
            None => crate::backend::pane::ProgramPane::spawn(
                Arc::clone(&backend),
                &window,
                program,
                args,
                cwd,
                &env,
                rows,
                cols,
                self.scrollback,
            ),
        }
        .map_err(|e| format!("could not start {program}: {e:#}"))?;

        self.programs.insert(
            key.clone(),
            ProgramSlot {
                pane,
                painted: super::Painted {
                    size: Cell::new((rows, cols)),
                    rect: Cell::new(Rect::default()),
                    ..Default::default()
                },
                program: program.to_string(),
            },
        );
        if !adopted {
            // Spawned here, so this loop knows it ran — which is what lets its
            // ending be announced even if it dies before anything looks again.
            // An *adopted* window is deliberately not recorded: it belongs to a
            // previous run of the interface, and a corpse found on startup ended
            // before there was anything to tell.
            //
            // After the spawn, not before: a spawn that failed leaves no pane to
            // die, and claiming one started would make the *next* restart's
            // replacement look like a fresh death.
            self.program_transitions
                .push(ProgramTransition::Started(key.clone()));
        }
        Ok(())
    }

    fn find_program_window(
        &self,
        backend: &Arc<dyn crate::backend::SessionBackend>,
        window: &str,
    ) -> Option<String> {
        // `window_panes`, not `discover`: a program window has no session id to
        // resolve by, only its deterministic name. Discovery does list it, but
        // stamped `@thurbox_role=program` and unowned, so it can never be
        // adopted as a session's agent (ADR-25).
        let mut live = None;
        for (backend_id, dead) in backend.window_panes(window).unwrap_or_default() {
            if dead {
                if let Err(e) = backend.kill(&backend_id) {
                    debug!("could not clear the dead program window {window}: {e:#}");
                }
            } else if live.is_none() {
                // Normalised on the way, not taken as found: this window was
                // made by an earlier interface, possibly one that set
                // `remain-on-exit` for a whole session and landed it on
                // whichever window happened to be current. Left as it stood, a
                // program window carrying `on` is a pane whose exit can never be
                // announced — the corpse comes straight back on the first
                // restart after an upgrade, which is the one moment this is
                // about. One round trip, and only here, where the answer is
                // known from the name we looked the window up by.
                if let Err(e) = backend.set_pane_retention(&backend_id, false) {
                    debug!("could not clear remain-on-exit on {window}: {e:#}");
                }
                live = Some(backend_id);
            }
        }
        live
    }

    pub fn program_key(&self, surface: &str) -> Option<&ProgramKey> {
        if !is_program_surface(surface) {
            return None;
        }
        self.programs.keys().find(|key| key.surface_id() == surface)
    }

    pub fn program_count(&self, mod_name: &str) -> usize {
        self.programs
            .keys()
            .filter(|key| key.owner == mod_name)
            .count()
    }

    pub fn release_program(&mut self, key: &ProgramKey) -> bool {
        match self.programs.remove(key) {
            Some(slot) => {
                slot.pane.kill();
                true
            }
            None => false,
        }
    }

    pub fn retain_program_mods(&mut self, live: &[String]) {
        for key in stale_program_keys(self.programs.keys(), live) {
            self.release_program(&key);
        }
    }

    pub fn send_to_program(&self, key: &ProgramKey, bytes: Vec<u8>) -> Result<(), String> {
        let Some(slot) = self.programs.get(key) else {
            return Err(format!("there is no program pane named {:?}", key.name));
        };
        if slot.pane.has_exited() {
            return Err(format!("{} has exited", slot.program));
        }
        slot.pane.send_input(bytes).map_err(|e| format!("{e:#}"))
    }

    pub fn program_liveness(&self) -> Vec<(ProgramKey, String, bool)> {
        self.programs
            .iter()
            .map(|(key, slot)| (key.clone(), slot.program.clone(), slot.pane.has_exited()))
            .collect()
    }

    pub fn take_program_transitions(&mut self) -> Vec<ProgramTransition> {
        std::mem::take(&mut self.program_transitions)
    }

    pub fn program_state(&self, key: &ProgramKey) -> Option<(&str, bool)> {
        self.programs
            .get(key)
            .map(|slot| (slot.program.as_str(), slot.pane.has_exited()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_surface_is_told_from_a_session_surface() {
        let key = ProgramKey::new("mods/90_watch.lua", "watch");
        assert!(is_program_surface(&key.surface_id()));
        for not_a_program in [
            "5f9c1f6e-1b2a-4c3d-8e9f-0a1b2c3d4e5f",
            "5f9c1f6e-1b2a-4c3d-8e9f-0a1b2c3d4e5f#shell",
            "",
        ] {
            assert!(!is_program_surface(not_a_program), "{not_a_program:?}");
        }
    }

    #[test]
    fn a_program_surface_resolves_by_lookup_not_by_splitting() {
        let terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        // Nothing is running, so nothing resolves — including a well-formed id.
        let key = ProgramKey::new("mods/90_watch.lua", "watch");
        assert!(terminals.program_key(&key.surface_id()).is_none());
        assert!(terminals.program_key("not-a-program").is_none());
    }

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
    fn a_mod_may_not_hold_unbounded_panes() {
        for held in 0..MAX_PROGRAMS_PER_MOD {
            assert!(
                admit_program("mods/90_watch.lua", held, "watch").is_ok(),
                "{held} should be admitted"
            );
        }
        let refused = admit_program("mods/90_watch.lua", MAX_PROGRAMS_PER_MOD, "watch")
            .expect_err("the limit is a limit");
        assert!(refused.contains("90_watch.lua"), "{refused}");
        assert!(
            refused.contains(&MAX_PROGRAMS_PER_MOD.to_string()),
            "{refused}"
        );
    }

    #[test]
    fn a_pane_with_no_program_named_is_refused() {
        // Otherwise the pane spawns the backend's idea of an empty command line.
        assert!(admit_program("mods/90_watch.lua", 0, "   ").is_err());
        assert!(admit_program("mods/90_watch.lua", 0, "").is_err());
    }

    #[test]
    fn an_absent_program_pane_accepts_nothing_and_reports_nothing() {
        let terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let key = ProgramKey::new("mods/90_watch.lua", "watch");
        assert_eq!(terminals.program_count("mods/90_watch.lua"), 0);
        assert!(terminals.send_to_program(&key, b"x".to_vec()).is_err());
        assert!(terminals.program_state(&key).is_none());
    }

    #[test]
    fn keys_for_a_running_program_are_sent_even_with_a_program_named() {
        let plan = |program| plan_keys("mods/90_editor.lua", "editor", true, program);
        assert_eq!(plan("nvim"), KeysPlan::Send);
        assert_eq!(plan(""), KeysPlan::Send);
    }

    #[test]
    fn keys_for_a_program_that_is_not_running_start_the_one_named() {
        assert_eq!(
            plan_keys("mods/90_editor.lua", "editor", false, "nvim"),
            KeysPlan::Start
        );
    }

    #[test]
    fn keys_with_nothing_running_and_nothing_to_start_are_refused_by_name() {
        let KeysPlan::Refuse(error) = plan_keys("mods/90_editor.lua", "editor", false, "  ") else {
            panic!("nothing to type into and nothing to start must be refused");
        };
        assert!(error.contains("90_editor.lua"), "{error}");
        assert!(error.contains("\"editor\""), "{error}");
    }

    #[test]
    fn a_reload_keeps_a_live_mods_pane_and_releases_a_vanished_ones() {
        let watch = ProgramKey::new("mods/90_watch.lua", "watch");
        let gone = ProgramKey::new("mods/91_deleted.lua", "top");
        let keys = [watch.clone(), gone.clone()];

        // Both still loaded: nothing is released.
        let live = vec![
            "mods/90_watch.lua".to_string(),
            "mods/91_deleted.lua".to_string(),
        ];
        assert!(stale_program_keys(keys.iter(), &live).is_empty());

        // One edited away, renamed or turned off: only its pane goes.
        let live = vec!["mods/90_watch.lua".to_string()];
        assert_eq!(stale_program_keys(keys.iter(), &live), vec![gone]);

        let all: Vec<ProgramKey> = stale_program_keys(keys.iter(), &[]);
        assert_eq!(all.len(), 2);
        let _ = watch;
    }
}
