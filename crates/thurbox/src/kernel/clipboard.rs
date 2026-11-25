use super::modals::OWNER;
use super::registry::{binding_from, Binding};

pub const COPY_ACTION: &str = "kernel.copy";

pub const PASTE_ACTION: &str = "kernel.paste";

const GROUP: &str = "Clipboard";

struct Chords {
    action: &'static str,
    description: &'static str,
    ctrl: &'static str,
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

pub fn bindings() -> Vec<Binding> {
    bindings_for(cfg!(target_os = "macos"))
}

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

    #[test]
    fn macos_adds_the_cmd_pair_and_no_other_platform_does() {
        let mac = chords(true);
        assert!(mac.contains(&"cmd+c".to_string()) && mac.contains(&"cmd+v".to_string()));
        assert!(!chords(false).iter().any(|chord| chord.starts_with("cmd")));
    }

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
            assert_eq!(binding.owner, OWNER);
            assert_eq!(binding.scope, Scope::Global);
            assert!(!binding.passthrough, "{}", binding.chord);
            assert_eq!(binding.group, GROUP);
        }
    }
}
