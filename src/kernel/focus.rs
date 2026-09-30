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
    /// Whether this float actually painted on the last frame.
    ///
    /// Only meaningful with `floats`. A float is *allowed* to draw above the
    /// arrangement at any time; whether it *did* is a separate fact, and only the
    /// frame that just happened knows it — a closed modal returns no float and
    /// paints nothing.
    pub float_open: bool,
    /// Its slot was placed by the arrangement on the last frame.
    pub slot_placed: bool,
    pub chosen_in_switch: Option<bool>,
}

/// Is this plugin on screen right now?
///
/// The strict question, and the one the interface's own inventory answers with
/// `visible` / `hidden`: an alternate of a switch slot is placed in the sense
/// that its slot exists, but nothing of it is painted.
///
/// A float is judged by whether it *painted*, not by whether it may. Answering
/// the permission instead reported the confirmation dialog and the creation
/// wizard as on screen at all times — on the one screen whose whole job is
/// telling you which files are drawing. Note this is the reverse of
/// [`can_focus`]: focus may go to a float that is about to open, but nothing
/// about that makes it drawn yet.
pub fn is_drawn(placement: Placement) -> bool {
    if placement.floats {
        return placement.float_open;
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

/// Must a focus request wait for the arrangement to run again?
///
/// [`can_focus`] is asked against the placement of the frame that already
/// painted, and a pane that opens *its own slot* has not been placed in one yet:
/// the search strip asks the arrangement for a row and asks for focus in the same
/// action, and the arrangement only runs again on the next frame. Judged there,
/// the request is refused for a slot that is about to exist — so the chord opened
/// the strip and left focus behind it, and every character typed went to the pane
/// underneath.
///
/// So a request focus cannot take now is *held for one layout* and re-asked once
/// the arrangement has run. Exactly one, deliberately: a slot still not placed
/// then is one nothing brings forward (a closed column, a pane turned off in the
/// Interface tab), so the request expires rather than following focus around —
/// which is [`can_focus`]'s own guarantee, one frame later.
pub fn defer_until_placed(placement: Placement) -> bool {
    !can_focus(placement)
}

/// One entry of the focus ring, as the `Ctrl+H`/`Ctrl+L` cycle sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleEntry<'a> {
    pub placement: Placement,
    /// The `switch` slot it occupies; `None` for any other slot, and for a float.
    pub switch_slot: Option<&'a str>,
    /// It is the first focusable occupant of that switch slot — the one the slot
    /// shows when nothing else has been chosen. Ignored without `switch_slot`.
    pub default_in_slot: bool,
}

/// Where the cycle lands from `from`, stepping by `step`, or `None` to stay.
///
/// The cycle walks *columns*, not panes. [`can_focus`] lets focus rest on a
/// switch alternate because focusing one is what draws it — right for its own
/// key or pill, and wrong for a walk across the screen: stepping onto the
/// alternate swapped the agent's terminal for a pane nobody asked to see. So a
/// switch slot is one stop, its default occupant, and its alternates are reached
/// only by asking for them.
///
/// From an alternate the walk leaves the slot, so the default beside it is
/// passed over — unless nothing else is a stop at all, where it is the only way
/// out that stays in the cycle.
pub fn next_in_cycle(ring: &[CycleEntry], from: usize, step: isize) -> Option<usize> {
    let count = ring.len() as isize;
    let own_slot = ring.get(from).and_then(|entry| entry.switch_slot);
    let stop = |next: usize, leaving: bool| {
        let entry = ring[next];
        can_focus(entry.placement)
            && match entry.switch_slot {
                None => true,
                Some(slot) => entry.default_in_slot && !(leaving && own_slot == Some(slot)),
            }
    };
    [true, false].into_iter().find_map(|leaving| {
        (1..=count)
            .map(|hop| (from as isize + step * hop).rem_euclid(count) as usize)
            .find(|next| stop(*next, leaving))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(slot_placed: bool, chosen_in_switch: Option<bool>) -> Placement {
        Placement {
            floats: false,
            float_open: false,
            slot_placed,
            chosen_in_switch,
        }
    }

    fn float(open: bool) -> Placement {
        Placement {
            floats: true,
            float_open: open,
            slot_placed: false,
            chosen_in_switch: None,
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
        let it = float(true);
        assert!(is_drawn(it));
        assert!(can_focus(it));
    }

    #[test]
    fn a_closed_float_is_not_drawn_though_focus_may_still_go_there() {
        // The confirmation dialog and the creation wizard are floats that paint
        // nothing until invoked. Judging them by the permission to float rather
        // than by having floated reported both as on screen permanently, in the
        // inventory that exists to say what is drawing.
        let it = float(false);
        assert!(
            !is_drawn(it),
            "a float that painted nothing is not on screen"
        );
        // Unchanged, and for the same reason a switch alternate keeps it:
        // focusing a float is part of what opens it.
        assert!(can_focus(it));
    }

    #[test]
    fn a_pane_that_opens_its_own_slot_holds_its_focus_request() {
        // The search strip: the chord that shows it also asks for focus, and the
        // slot it asked for is not placed until the arrangement runs again. The
        // request has to survive that frame or the strip opens unfocused.
        assert!(defer_until_placed(placement(false, None)));
        // Once the slot is there, nothing is deferred — it is taken.
        assert!(!defer_until_placed(placement(true, None)));
        // A float needs no slot, so its request is honoured at once.
        assert!(!defer_until_placed(float(false)));
    }

    #[test]
    fn an_ordinary_occupant_is_drawn_whenever_its_slot_is() {
        assert!(is_drawn(placement(true, None)));
        assert!(can_focus(placement(true, None)));
    }

    fn column() -> CycleEntry<'static> {
        CycleEntry {
            placement: placement(true, None),
            switch_slot: None,
            default_in_slot: false,
        }
    }

    fn in_centre(default: bool) -> CycleEntry<'static> {
        CycleEntry {
            placement: placement(true, Some(default)),
            switch_slot: Some("center"),
            default_in_slot: default,
        }
    }

    #[test]
    fn the_cycle_stops_once_on_a_switch_slot_at_its_default_occupant() {
        // sessions, agent, and a pane that replaces the agent.
        let ring = [column(), in_centre(true), in_centre(false)];
        assert_eq!(next_in_cycle(&ring, 0, 1), Some(1));
        assert_eq!(
            next_in_cycle(&ring, 1, 1),
            Some(0),
            "stepped onto the alternate"
        );
        assert_eq!(
            next_in_cycle(&ring, 0, -1),
            Some(1),
            "backwards reached the alternate"
        );
    }

    #[test]
    fn an_alternate_leaves_its_slot_in_either_direction() {
        let ring = [column(), in_centre(true), in_centre(false)];
        assert_eq!(
            next_in_cycle(&ring, 2, -1),
            Some(0),
            "Ctrl+H only swapped back"
        );
        assert_eq!(next_in_cycle(&ring, 2, 1), Some(0));
    }

    #[test]
    fn an_alternate_with_nothing_else_to_go_to_falls_back_to_its_default() {
        let ring = [in_centre(true), in_centre(false)];
        assert_eq!(next_in_cycle(&ring, 1, 1), Some(0));
        assert_eq!(next_in_cycle(&ring, 1, -1), Some(0));
    }

    #[test]
    fn a_switch_slot_with_one_occupant_is_still_a_stop() {
        let ring = [column(), in_centre(true)];
        assert_eq!(next_in_cycle(&ring, 0, 1), Some(1));
        assert_eq!(next_in_cycle(&ring, 1, 1), Some(0));
    }

    #[test]
    fn floats_and_unplaced_slots_keep_their_rule() {
        let floating = CycleEntry {
            placement: float(false),
            switch_slot: None,
            default_in_slot: false,
        };
        let closed = CycleEntry {
            placement: placement(false, None),
            ..column()
        };
        let ring = [column(), closed, floating];
        assert_eq!(next_in_cycle(&ring, 0, 1), Some(2));
        assert_eq!(next_in_cycle(&ring, 2, 1), Some(0));
    }

    #[test]
    fn a_ring_with_no_stop_leaves_focus_where_it_is() {
        let closed = CycleEntry {
            placement: placement(false, None),
            ..column()
        };
        assert_eq!(next_in_cycle(&[closed, closed], 0, 1), None);
        assert_eq!(next_in_cycle(&[], 0, 1), None);
    }

    #[test]
    fn every_walk_across_any_ring_lands_on_a_stop_and_never_on_an_alternate() {
        // Exhaustive over small rings of the four kinds of entry: whatever the
        // mix, a step never lands on an alternate unless it started on one and
        // nothing else exists, and a stop is found whenever one exists.
        let kinds = [
            column(),
            in_centre(true),
            in_centre(false),
            CycleEntry {
                placement: placement(false, None),
                ..column()
            },
        ];
        for len in 1..=4usize {
            for code in 0..kinds.len().pow(len as u32) {
                let ring: Vec<_> = (0..len)
                    .map(|i| kinds[code / kinds.len().pow(i as u32) % kinds.len()])
                    .collect();
                for from in 0..len {
                    for step in [-1, 1] {
                        let landed = next_in_cycle(&ring, from, step);
                        let any_stop = ring.iter().any(|e| {
                            can_focus(e.placement) && (e.switch_slot.is_none() || e.default_in_slot)
                        });
                        assert_eq!(landed.is_some(), any_stop, "{ring:?} from {from}");
                        if let Some(at) = landed {
                            let entry = ring[at];
                            assert!(can_focus(entry.placement), "{ring:?} from {from}");
                            assert!(
                                entry.switch_slot.is_none() || entry.default_in_slot,
                                "landed on an alternate: {ring:?} from {from}"
                            );
                        }
                    }
                }
            }
        }
    }
}
