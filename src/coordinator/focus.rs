//! Which pane has focus, and which panes may.
//!
//! Focus is an index into the host's focusable plugins, but the rules are about
//! *visibility*: a slot the arrangement did not place is not a cycle stop, and a
//! switch slot's hidden occupants are not either. `pending_focus` is the one
//! subtlety — a pane that opens its own slot asks for focus a frame before the
//! slot exists, so the request is held for exactly one layout and re-asked
//! there.

use super::*;

impl App {
    /// Move focus onto a plugin, if focus can rest there at all.
    ///
    /// A request focus cannot take is *held for one layout* rather than dropped —
    /// see `kernel::focus::defer_until_placed` for why, and `apply_pending_focus`,
    /// which re-asks it once the arrangement has run.
    pub(crate) fn focus_plugin(&mut self, index: usize) {
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

    /// Take a focus request that was waiting for its slot to be placed.
    ///
    /// Attempted once and then forgotten, whether or not it landed: a slot still
    /// not placed is one the arrangement declines to give (a column closed, a pane
    /// turned off), and a request that outlived its layout would chase focus
    /// across frames the user has since moved on from. Which is also why the check
    /// is here rather than left to `focus_plugin`: that one re-arms the request it
    /// cannot honour, so calling it blind would make the wait permanent.
    pub(crate) fn apply_pending_focus(&mut self) {
        let Some(index) = self.pending_focus.take() else {
            return;
        };
        if self.can_focus_plugin(index) {
            self.focus_plugin(index);
        }
    }

    pub(crate) fn cycle_focus(&mut self, step: isize) {
        let focusable = self.host.focusable();
        let count = focusable.len().max(1) as isize;
        // Walk at most one full lap: if nothing is visible (a pathological
        // arrangement), leave focus where it was rather than spinning.
        for hop in 1..=count {
            let next = (self.focus as isize + step * hop).rem_euclid(count) as usize;
            if focusable
                .get(next)
                .is_some_and(|index| self.can_focus_plugin(*index))
            {
                self.focus = next;
                return;
            }
        }
    }

    /// Plugins may have come and gone under us; focus must land on one that
    /// still exists.
    pub(crate) fn clamp_focus(&mut self) {
        let count = self.host.focusable().len();
        self.focus = self.focus.min(count.saturating_sub(1));
    }

    /// Whether focus may rest on a plugin — which for a `switch` alternate is
    /// **not** the same question, since focusing it is what brings it forward.
    /// See `kernel::focus`.
    pub(crate) fn can_focus_plugin(&self, index: usize) -> bool {
        thurbox::kernel::focus::can_focus(self.placement(index))
    }

    /// Whether a plugin would be drawn right now: its slot was placed on the
    /// last frame, or it floats above the arrangement and so needs no slot.
    pub(crate) fn is_visible_plugin(&self, index: usize) -> bool {
        thurbox::kernel::focus::is_drawn(self.placement(index))
    }

    /// Where a plugin sits, as the two focus rules need to see it.
    pub(crate) fn placement(&self, index: usize) -> thurbox::kernel::focus::Placement {
        let Some(plugin) = self.host.plugins.get(index) else {
            return thurbox::kernel::focus::Placement {
                floats: false,
                float_open: false,
                slot_placed: false,
                chosen_in_switch: None,
            };
        };
        // A `switch` slot draws ONE of its occupants; the rest are alternatives.
        let chosen_in_switch =
            matches!(self.host.slot_mode(&plugin.slot), SlotMode::Switch).then(|| {
                // `slot_selection` stores a position WITHIN the slot's members,
                // not a plugin index — comparing it to one made the terminal
                // look unselected and cycled focus off it at startup. Resolve it.
                let members = self.host.in_slot(&plugin.slot);
                let chosen = self.slot_selection.get(&plugin.slot).copied().unwrap_or(0);
                members.get(chosen) == Some(&index)
            });
        thurbox::kernel::focus::Placement {
            floats: plugin.floats,
            float_open: self.drawn_floats.contains(&index),
            slot_placed: self.visible_slots.contains(&plugin.slot),
            chosen_in_switch,
        }
    }

    /// Select a session and put the input focus on the pane that shows it.
    ///
    /// Only for a request made *now*: a clicked notification, or
    /// `thurbox-cli session focus`. A session this instance just created is
    /// deliberately not one — creation finishes on a worker seconds after the
    /// wizard closed, so steering the view then lands at a moment the user did
    /// not choose, and taking the selection back after every one of them made
    /// creating several sessions in a row a fight with the cursor.
    ///
    /// The selection travels through the store rather than being set here,
    /// because the list is a plugin and it is the only thing that knows the
    /// rendered order.
    pub(crate) fn focus_on_session(&mut self, id: &str) {
        self.host.set_shared_string("focus_session", id);
        if let Some(index) = self.host.index_of("agent") {
            if let Some(position) = self.host.focusable().iter().position(|i| *i == index) {
                self.focus = position;
            }
        }
        self.dirty = true;
    }

    /// Whether the focused plugin declared `input = "session"`.
    pub(crate) fn focused_wants_session_input(&self) -> bool {
        self.host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.plugins.get(*index))
            .is_some_and(|plugin| plugin.session_input)
    }
}
