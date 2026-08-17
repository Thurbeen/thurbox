//! Where focus may rest, and what is actually drawn.
//!
//! Two questions that look like one and are not, which is how the plugins pane
//! became unreachable: it is an *alternate* occupant of the centre `switch`
//! slot, and the loop asked "is it drawn?" when deciding whether focus could
//! stay on it. The answer was no — a switch slot draws one occupant — so the
//! frame after `F11` focused it, the guard that stops focus stranding on a
//! closed column moved focus straight back off. The pane could be selected and
//! never seen, and the key looked dead.
//!
//! The distinction is that focusing a switch alternate is *what makes it drawn*.
//! The selection is written during that slot's render, so at the moment focus
//! lands the pane is not yet the chosen one — judging it by the previous frame's
//! selection is judging it by the state it is about to change.
//!
//! Kept here rather than in the binary because it is the rule that was wrong,
//! and a rule worth a test is worth a home.

/// A plugin's position in its slot, as far as these two questions care.
///
/// `chosen_in_switch` is `None` for a slot that draws every occupant (a stack,
/// or a slot with one), `Some(true)` for the visible occupant of a `switch`
/// slot and `Some(false)` for one of its alternates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Floats above the arrangement, so it needs no slot at all.
    pub floats: bool,
    /// Its slot was placed by the arrangement on the last frame.
    pub slot_placed: bool,
    pub chosen_in_switch: Option<bool>,
}

/// Is this plugin on screen right now?
///
/// The strict question, and the one the interface's own inventory answers with
/// `visible` / `hidden`: an alternate of a switch slot is placed in the sense
/// that its slot exists, but nothing of it is painted.
pub fn is_drawn(placement: Placement) -> bool {
    if placement.floats {
        return true;
    }
    if !placement.slot_placed {
        return false;
    }
    placement.chosen_in_switch.unwrap_or(true)
}

/// May focus rest on this plugin?
///
/// Differs from [`is_drawn`] in exactly one case, and it is the case that made a
/// pane unreachable: a switch alternate is not drawn *yet*, and focusing it is
/// what brings it forward. Refusing focus there leaves the pane with no way in
/// at all — the focus ring skips it and its own opening chord is undone a frame
/// later.
///
/// A slot the arrangement did not place is still refused: nothing brings that
/// forward, so focus really would strand on a pane the user cannot see.
pub fn can_focus(placement: Placement) -> bool {
    placement.floats || placement.slot_placed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(slot_placed: bool, chosen_in_switch: Option<bool>) -> Placement {
        Placement {
            floats: false,
            slot_placed,
            chosen_in_switch,
        }
    }

    #[test]
    fn the_visible_occupant_of_a_switch_slot_is_both_drawn_and_focusable() {
        let it = placement(true, Some(true));
        assert!(is_drawn(it));
        assert!(can_focus(it));
    }

    #[test]
    fn an_alternate_is_not_drawn_but_focus_may_still_go_there() {
        // The whole bug in one assertion. Focusing the alternate is what makes
        // it the drawn one; judging it by the selection it is about to change
        // means it can be selected and never seen.
        let it = placement(true, Some(false));
        assert!(!is_drawn(it));
        assert!(
            can_focus(it),
            "an alternate focus cannot reach is unreachable"
        );
    }

    #[test]
    fn a_slot_the_arrangement_did_not_place_takes_neither() {
        // A closed column: nothing brings it forward, so focus really would
        // strand on something the user cannot see.
        let it = placement(false, None);
        assert!(!is_drawn(it));
        assert!(!can_focus(it));

        let alternate = placement(false, Some(false));
        assert!(!is_drawn(alternate));
        assert!(!can_focus(alternate));
    }

    #[test]
    fn a_float_needs_no_slot() {
        let it = Placement {
            floats: true,
            slot_placed: false,
            chosen_in_switch: None,
        };
        assert!(is_drawn(it));
        assert!(can_focus(it));
    }

    #[test]
    fn an_ordinary_occupant_is_drawn_whenever_its_slot_is() {
        assert!(is_drawn(placement(true, None)));
        assert!(can_focus(placement(true, None)));
    }
}
