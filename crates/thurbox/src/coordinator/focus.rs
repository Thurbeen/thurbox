use thurbox::kernel::layout::SlotMode;

use crate::App;

impl App {
    pub(crate) fn focus_mod(&mut self, index: usize) {
        if thurbox::kernel::focus::defer_until_placed(self.placement(index)) {
            self.pending_focus = Some(index);
            return;
        }
        if let Some(position) = self.host.focusable().iter().position(|i| *i == index) {
            if position != self.focus {
                self.focus_return = self.focus;
            }
            self.focus = position;
        }
    }

    pub(crate) fn apply_pending_focus(&mut self) {
        let Some(index) = self.pending_focus.take() else {
            return;
        };
        if self.can_focus_mod(index) {
            self.focus_mod(index);
        }
    }

    pub(crate) fn cycle_focus_action(&mut self, step: isize) {
        if let Some(index) = self.grabbed {
            let direction = if step > 0 { "next" } else { "previous" };
            match self.host.on_focus_cycle(index, direction) {
                Ok(true) => {
                    self.dirty = true;
                    return;
                }
                Ok(false) => {}
                Err(error) => self.errors.push(error),
            }
        }
        self.cycle_focus(step);
    }

    pub(crate) fn cycle_focus(&mut self, step: isize) {
        let focusable = self.host.focusable();
        let ring: Vec<_> = focusable
            .iter()
            .map(|&index| {
                let loaded = &self.host.mods[index];
                let switch_slot = (!loaded.floats
                    && matches!(self.host.slot_mode(&loaded.slot), SlotMode::Switch))
                .then_some(loaded.slot.as_str());
                let default_in_slot =
                    switch_slot.is_some_and(|slot| self.host.switch_default(slot) == Some(index));
                thurbox::kernel::focus::CycleEntry {
                    placement: self.placement(index),
                    switch_slot,
                    default_in_slot,
                }
            })
            .collect();
        if let Some(next) = thurbox::kernel::focus::next_in_cycle(&ring, self.focus, step) {
            self.focus = next;
        }
    }

    pub(crate) fn clamp_focus(&mut self) {
        let count = self.host.focusable().len();
        self.focus = self.focus.min(count.saturating_sub(1));
    }

    pub(crate) fn can_focus_mod(&self, index: usize) -> bool {
        thurbox::kernel::focus::can_focus(self.placement(index))
    }

    pub(crate) fn is_visible_mod(&self, index: usize) -> bool {
        thurbox::kernel::focus::is_drawn(self.placement(index))
    }

    pub(crate) fn placement(&self, index: usize) -> thurbox::kernel::focus::Placement {
        let Some(loaded) = self.host.mods.get(index) else {
            return thurbox::kernel::focus::Placement {
                floats: false,
                float_open: false,
                slot_placed: false,
                chosen_in_switch: None,
            };
        };
        let chosen_in_switch =
            matches!(self.host.slot_mode(&loaded.slot), SlotMode::Switch).then(|| {
                let members = self.host.in_slot(&loaded.slot);
                let chosen = self.slot_selection.get(&loaded.slot).copied().unwrap_or(0);
                members.get(chosen) == Some(&index)
            });
        thurbox::kernel::focus::Placement {
            floats: loaded.floats,
            float_open: self.drawn_floats.contains(&index),
            slot_placed: self.visible_slots.contains(&loaded.slot),
            chosen_in_switch,
        }
    }

    pub(crate) fn focus_on_session(&mut self, id: &str) {
        self.host.set_shared_string("focus_session", id);
        if let Some(index) = self.host.index_of("agent") {
            if let Some(position) = self.host.focusable().iter().position(|i| *i == index) {
                self.focus = position;
            }
        }
        self.dirty = true;
    }

    pub(crate) fn focused_wants_session_input(&self) -> bool {
        self.host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.mods.get(*index))
            .is_some_and(|loaded| loaded.session_input)
    }

    pub(crate) fn focused_terminal_is_dead(&self) -> bool {
        self.focused_surface
            .as_deref()
            .and_then(|surface| self.terminals.is_dead(surface))
            .unwrap_or(false)
    }
}
