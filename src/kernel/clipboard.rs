//! The clipboard chords, declared rather than matched.
//!
//! Copy and paste used to be literal `KeyCode::Char('c')`/`('v')` arms in the
//! loop, ahead of the registry — which meant help listed them under *Fixed (not
//! rebindable)* and nobody could move them. They are ordinary bindings now,
//! owned by [`super::modals::OWNER`] like the modal chords: listed in help,
//! offered by the palette, conflict-checked, and rebindable. The loop still
//! *runs* them itself, because there is no Lua plugin behind them.
//!
//! **macOS gets `Cmd` as well.** `Ctrl+C` on a terminal means interrupt, so a
//! Mac user reaches for `Cmd+C` — and it did nothing, because the Command key
//! was dropped at the input boundary before any chord was resolved (issue
//! #1024). The modifier is carried through now ([`super::host::KeyPress::cmd`]),
//! and macOS builds declare the `Cmd` pair beside the `Ctrl` one — the same
//! "Cmd mirrors the Ctrl primary" pattern v1 used for its four Cmd alternates.
//! Arriving at all still needs the kitty keyboard protocol, which is pushed at
//! startup: iTerm2 3.5+, kitty, WezTerm and Ghostty report `Cmd`, Terminal.app
//! does not — and the emulator gets the chord first, so one that swallows its
//! own unperformed `Cmd+C` keeps it from thurbox whatever is declared here
//! (`docs/FEATURES.md` → Text Selection and Copy-Paste).

use super::modals::OWNER;
use super::registry::{binding_from, Binding};

/// Copy the current selection. Fires only while there **is** one: with none,
/// the chord belongs to whatever has focus, so `Ctrl+C` still interrupts the
/// agent. That guard is the loop's — see `coordinator::input`.
pub const COPY_ACTION: &str = "kernel.copy";

/// Paste the clipboard into the focused session's terminal.
pub const PASTE_ACTION: &str = "kernel.paste";

/// The group these two are listed under in help.
const GROUP: &str = "Clipboard";

/// One clipboard action: what it is called, what help says about it, and the
/// chord it takes on each platform.
///
/// Both chords on one row rather than two flat lists, because "Cmd mirrors the
/// Ctrl primary" is the rule — spelling them together makes it impossible to
/// add one and forget the other.
struct Chords {
    action: &'static str,
    description: &'static str,
    ctrl: &'static str,
    /// Declared on macOS builds only.
    cmd: &'static str,
}

const CLIPBOARD: [Chords; 2] = [
    Chords {
        action: COPY_ACTION,
        description: "copy the selection",
        ctrl: "ctrl+c",
        cmd: "cmd+c",
    },
    Chords {
        action: PASTE_ACTION,
        description: "paste",
        ctrl: "ctrl+v",
        cmd: "cmd+v",
    },
];

/// The clipboard chords as registry declarations, for the platform this build
/// runs on.
pub fn bindings() -> Vec<Binding> {
    bindings_for(cfg!(target_os = "macos"))
}

/// Split out so both platforms' tables are testable from either.
fn bindings_for(macos: bool) -> Vec<Binding> {
    CLIPBOARD
        .iter()
        .flat_map(|row| {
            let mut chords = vec![row.ctrl];
            if macos {
                chords.push(row.cmd);
            }
            chords.into_iter().map(move |chord| {
                binding_from(
                    OWNER,
                    chord,
                    row.action,
                    row.description,
                    Some("global"),
                    // Never deferred to the agent. `Ctrl+C` reaching the agent
                    // is a *fall-through* decided per press by whether there is
                    // a selection, not a property of the binding: marking it
                    // passthrough would give the agent the chord even with one,
                    // and copying is the one case where thurbox wins it back.
                    false,
                    Some(GROUP),
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::registry::{normalise_chord, Scope};

    fn chords(macos: bool) -> Vec<String> {
        bindings_for(macos)
            .into_iter()
            .map(|binding| binding.chord)
            .collect()
    }

    #[test]
    fn both_platforms_bind_the_ctrl_pair() {
        for macos in [false, true] {
            let bound = chords(macos);
            assert!(bound.contains(&"ctrl+c".to_string()), "macos={macos}");
            assert!(bound.contains(&"ctrl+v".to_string()), "macos={macos}");
        }
    }

    /// The issue this module exists for: on a Mac the Command key is what a
    /// user presses to copy, and `Ctrl+C` is spent on interrupt.
    #[test]
    fn macos_adds_the_cmd_pair_and_no_other_platform_does() {
        let mac = chords(true);
        assert!(mac.contains(&"cmd+c".to_string()) && mac.contains(&"cmd+v".to_string()));
        assert!(!chords(false).iter().any(|chord| chord.starts_with("cmd")));
    }

    /// A chord the registry would spell differently is worse than none: the
    /// declaration would silently never match the press that made it.
    #[test]
    fn every_chord_is_already_canonical() {
        for row in CLIPBOARD {
            for chord in [row.ctrl, row.cmd] {
                assert_eq!(normalise_chord(chord), chord);
            }
        }
    }

    #[test]
    fn the_declarations_are_global_and_owned_by_the_kernel() {
        for binding in bindings_for(true) {
            assert_eq!(binding.plugin, OWNER);
            assert_eq!(binding.scope, Scope::Global);
            assert!(!binding.passthrough, "{}", binding.chord);
            assert_eq!(binding.group, GROUP);
        }
    }
}
