#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub floats: bool,
    pub float_open: bool,
    pub slot_placed: bool,
    pub chosen_in_switch: Option<bool>,
}

pub fn is_drawn(placement: Placement) -> bool {
    if placement.floats {
        return placement.float_open;
    }
    if !placement.slot_placed {
        return false;
    }
    placement.chosen_in_switch.unwrap_or(true)
}

pub fn can_focus(placement: Placement) -> bool {
    placement.floats || placement.slot_placed
}

pub fn defer_until_placed(placement: Placement) -> bool {
    !can_focus(placement)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleEntry<'a> {
    pub placement: Placement,
    pub switch_slot: Option<&'a str>,
    pub default_in_slot: bool,
}

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
        let it = placement(true, Some(false));
        assert!(!is_drawn(it));
        assert!(
            can_focus(it),
            "an alternate focus cannot reach is unreachable"
        );
    }

    #[test]
    fn a_slot_the_arrangement_did_not_place_takes_neither() {
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
        let it = float(false);
        assert!(
            !is_drawn(it),
            "a float that painted nothing is not on screen"
        );
        assert!(can_focus(it));
    }

    #[test]
    fn a_pane_that_opens_its_own_slot_holds_its_focus_request() {
        assert!(defer_until_placed(placement(false, None)));
        assert!(!defer_until_placed(placement(true, None)));
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
