//! Keys, and where each one goes.
//!
//! Every chord resolves through one registry (`kernel::registry`), and the order
//! here is the whole of the policy: a modal first (it captures), then the
//! kernel's reserved chords, then an exclusive grab, then a plugin's declared
//! binding, then the focused plugin's raw `on_key` hook, and only then the key
//! goes to whatever surface the focused pane shows. A plugin-scoped claim never
//! outranks a global one — which is why search cannot take `Ctrl+N` from
//! new-session.

use super::paste::Input;
use super::*;

impl App {
    /// Drain EVERY pending event, not one per iteration, and dispatch each.
    ///
    /// Reading one event per 10ms poll cannot keep up with a mouse: a single drag
    /// emits events far faster than 100/s, so the queue grew without bound. That
    /// is felt as an unresponsive mouse, and the backlog outlives the process —
    /// the leftover reports are what printed `\x1b[<35;92;31M` into the terminal
    /// afterwards.
    ///
    /// The batch is also what `thurbox.*` is published for: once, before the first
    /// event that runs Lua, rather than once per event. A handler has to read
    /// something current, and nothing between two events of one batch can change
    /// what it would say — the snapshot is refreshed at the top of the iteration,
    /// and a command a handler queues is drained on the next one. Per event, a
    /// held-down key paid for the whole publish (every session's links, the
    /// interface inventory, the plugin lock) on every repeat.
    pub(crate) fn drain_input(&mut self, input_failures: &mut u32) -> Result<(), Box<dyn Error>> {
        let mut published = false;
        let mut waited = false;
        loop {
            // Only the first read waits; the rest take what is already queued —
            // except while the paste coalescer holds a key it cannot yet
            // decide about, which is worth a few milliseconds of the batch.
            let timeout = if waited {
                self.paste_burst.drain_timeout()
            } else {
                self.poll_timeout()
            };
            waited = true;
            let event = match next_event(timeout) {
                Ok(Some(event)) => {
                    *input_failures = 0;
                    // Anything the user does puts the loop back on the fast
                    // poll, so the frames that follow a keystroke are not paced
                    // by the idle timeout.
                    self.last_activity = Instant::now();
                    event
                }
                Ok(None) => break,
                // Input is not worth the process. A terminal can hand
                // crossterm a sequence it cannot parse — a burst of keys
                // interleaving with a mouse report is enough — and
                // propagating that error exited thurbox with every session
                // detached. Logged, dropped, and retried next iteration; only
                // a stream that keeps failing (a closed stdin, say) is fatal,
                // since polling a dead terminal would otherwise spin.
                Err(e) => {
                    *input_failures += 1;
                    tracing::warn!("reading input failed: {e}");
                    if *input_failures > INPUT_FAILURE_LIMIT {
                        return Err(Box::new(e));
                    }
                    break;
                }
            };
            match event {
                // Where the terminal reports no paste of its own, one arrives
                // here as keys and has to be recognised as one — the coalescer
                // hands back whichever of the two this turned out to be.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    for input in self.paste_burst.push(key, Instant::now()) {
                        self.apply_input(input, &mut published);
                    }
                }
                // Dropped rather than merely uncaptured when the feature is
                // off, so the flag stays authoritative even if a terminal
                // reports mouse events unasked. v1 does the same in
                // `App::update`.
                Event::Mouse(mouse) if self.mouse => {
                    self.publish_for_batch(&mut published);
                    self.on_mouse(mouse);
                    self.note_input();
                }
                // A bracketed paste from the terminal itself. Routed to
                // whatever has focus, exactly as `ctrl+v` is: a modal's
                // text field if one is open, else the focused terminal.
                Event::Paste(text) => {
                    self.publish_for_batch(&mut published);
                    self.on_paste(text);
                    self.note_input();
                }
                Event::Resize(cols, rows) => {
                    self.screen_size = (cols, rows);
                    self.note_input();
                }
                _ => {}
            }
        }
        // Nothing is queued behind the batch, so a run being watched has to go
        // somewhere: a paste, or the keys it was made of.
        for input in self.paste_burst.flush() {
            self.apply_input(input, &mut published);
        }
        Ok(())
    }

    /// Dispatch one resolved input, publishing `thurbox.*` once per batch.
    fn apply_input(&mut self, input: Input, published: &mut bool) {
        self.publish_for_batch(published);
        match input {
            Input::Key(key) => self.time_op("input_dispatch", |app| app.on_key(&key)),
            Input::Paste(text) => self.on_paste(text),
        }
        self.note_input();
    }

    pub(crate) fn on_key(&mut self, key: &KeyEvent) {
        if self.dispatch_to_modal(key) {
            return;
        }
        if self.dispatch_reserved(key) {
            return;
        }
        let press = to_press(key);
        if self.dispatch_grabbed(&press) {
            return;
        }
        if self.dispatch_declared(&press) {
            return;
        }
        if self.dispatch_raw(&press) {
            return;
        }
        if self.dispatch_session_input(key) {
            return;
        }
        // An `Esc` no pane claimed means "leave this one" — the v2 spelling of
        // v1 closing a modal, since a centre-slot pane is dismissed by focusing
        // whatever you came from.
        if key.code == KeyCode::Esc && self.focus_return != self.focus {
            let back = self.focus_return;
            self.focus_return = self.focus;
            if self.host.focusable().get(back).is_some() {
                self.focus = back;
            }
        }

        // Anything else is DROPPED. An unclaimed key does nothing.
        //
        // This used to quit on a bare `q` or `Esc`, which meant Escaping out of
        // the theme picker -- or any pane that does not claim Esc -- killed the
        // application. Quit is Ctrl+Q, reserved at the top of this function;
        // v1 has no bare-key quit either.
    }

    /// A system modal takes input before anything else — that is what makes it
    /// modal rather than a pane drawn on top.
    ///
    /// Two things still get through: the escape route (quit, reload, the perf
    /// HUD), and another modal's own opening chord, since opening one closes
    /// another. While help is capturing, neither does — binding `ctrl+q` has to
    /// be possible, and the kernel can allow it because it knows the capture
    /// lasts exactly one keystroke.
    pub(crate) fn dispatch_to_modal(&mut self, key: &KeyEvent) -> bool {
        if !self.modals.is_open() {
            return false;
        }
        if self.modals.captures_everything() {
            self.dispatch_modal_key(key);
            return true;
        }
        if let Some(kind) = self.modal_chord(key) {
            self.toggle_modal(kind);
            return true;
        }
        if !thurbox::kernel::modals::escapes(key) {
            self.dispatch_modal_key(key);
            return true;
        }
        false
    }

    /// The reserved minimum: focus, reload and quit always work, even if a
    /// plugin consumes every key it is offered.
    ///
    /// Quit is Ctrl+Q, not Ctrl+C: with a live terminal attached, Ctrl+C has to
    /// reach the agent so a turn can be interrupted. v1 reserves the same chord
    /// for the same reason.
    pub(crate) fn dispatch_reserved(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') if ctrl => self.quit = true,
            // F10, not F5: v1 spends F1-F9 and F12 on real UI (F5 is the tasks
            // panel), so the dev reload takes one of the two keys v1 leaves
            // free rather than shadowing a pane the user expects.
            KeyCode::F(10) => {
                self.reload_interface();
                self.collect_declarations();
                self.clamp_focus();
            }
            // Tab is NOT a focus key: it belongs to the agent. See RESERVED.
            // v1 binds focus movement to Ctrl+H/Ctrl+L as well, and refuses to
            // let a focused terminal keep either: they are how you get *out* of
            // one, so they cannot be among the chords handed to the agent.
            KeyCode::Char('h') if ctrl => self.cycle_focus(-1),
            KeyCode::Char('l') if ctrl => self.cycle_focus(1),
            // The HUD reports on this loop, so it is the kernel's own key rather
            // than a plugin's — nothing a plugin does can hide it. v1 spends F12
            // on the same thing for the same reason.
            // The one reserved key that is a *feature*: v1 gates the HUD behind
            // `[features] perf_hud` because opening it also turns on wall-clock
            // timing collection, which is not free.
            KeyCode::F(12) if self.config.features().perf_hud => {
                self.hud = !self.hud;
                self.dirty = true;
            }
            // Copy and paste are reserved, like v1's: they must work from any
            // pane, and `Ctrl+C` must not reach the agent when there is a
            // selection to copy — that is the one case where thurbox wins the
            // chord back from the terminal.
            KeyCode::Char('c') if ctrl && self.selection.is_some() => {
                // No focused session required: a selection over the session
                // list, a modal or the footer is still a selection, and v1
                // copies it. Only the fall-back-to-whole-screen path needs a
                // terminal, because only a terminal HAS a screen.
                let session = self.focused_session.clone();
                self.copy_selection_or_screen(session.as_deref());
            }
            KeyCode::Char('v') if ctrl => self.paste_into_focused(),
            _ => return false,
        }
        true
    }

    /// A float takes every key while it is up — that is what makes it a modal
    /// rather than merely a pane drawn on top.
    ///
    /// The reserved chords still work, so a modal can never trap you.
    pub(crate) fn dispatch_grabbed(&mut self, press: &KeyPress) -> bool {
        let Some(plugin) = self
            .grabbed
            .and_then(|index| self.host.plugins.get(index))
            .map(|plugin| plugin.name.clone())
        else {
            return false;
        };
        let index = self.grabbed.expect("just resolved through it");
        if let Some(action) = self
            .registry
            .resolve(press, Some(&plugin))
            .map(|b| b.action.clone())
        {
            match self.host.on_action(index, &action) {
                Ok(true) => return true,
                Ok(false) => {}
                Err(e) => self.errors.push(e),
            }
        }
        if let Err(e) = self.host.on_key(index, press) {
            self.errors.push(e);
        }
        true
    }

    /// A declared key: the registry resolves the chord to an action and the
    /// plugin that owns it.
    ///
    /// This is the path that can be rebound, conflict-checked and listed in
    /// help. Falls through (`false`) when nothing claimed the chord — including
    /// when it was deferred to the agent.
    pub(crate) fn dispatch_declared(&mut self, press: &KeyPress) -> bool {
        let focused_name = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.plugins.get(*index))
            .map(|plugin| plugin.name.clone());
        let Some((plugin, action, passthrough)) = self
            .registry
            .resolve(press, focused_name.as_deref())
            .map(|binding| {
                (
                    binding.plugin.clone(),
                    binding.action.clone(),
                    binding.passthrough,
                )
            })
        else {
            return false;
        };
        // v1's terminal passthrough: a chord the agent's own line editing needs
        // is left to the pty while a terminal has focus, and the command stays
        // reachable from every other pane (and its F-key alternate). Gated on
        // the bound chord, so rebinding a passthrough action onto a free key
        // makes it work in the terminal again.
        let defer_to_agent = passthrough
            && self.focused_wants_session_input()
            && is_ctrl_letter_chord(&canonical_chord(press));
        if defer_to_agent {
            return false;
        }
        // A chord the kernel declared for itself opens a system modal; there is
        // no plugin to hand it to.
        if plugin == thurbox::kernel::modals::OWNER {
            if let Some(kind) = ModalKind::from_action(&action) {
                self.toggle_modal(kind);
                return true;
            }
        }
        let Some(index) = self.host.index_of(&plugin) else {
            return false;
        };
        match self.host.on_action(index, &action) {
            Ok(true) => true,
            Ok(false) => false,
            Err(e) => {
                self.errors.push(e);
                false
            }
        }
    }

    /// Raw keys: the focused plugin, then the non-focusable listeners.
    pub(crate) fn dispatch_raw(&mut self, press: &KeyPress) -> bool {
        let focusable = self.host.focusable();
        let mut order: Vec<usize> = focusable.get(self.focus).copied().into_iter().collect();
        order.extend(
            (0..self.host.plugins.len()).filter(|index| !self.host.plugins[*index].focusable),
        );
        for index in order {
            match self.host.on_key(index, press) {
                Ok(true) => return true,
                Ok(false) => {}
                // A throwing handler must not swallow the key or the app.
                Err(e) => self.errors.push(e),
            }
        }
        false
    }

    /// Nothing claimed it. If the focused plugin asked for raw session input and
    /// its surface names a live session, the key belongs to the agent.
    ///
    /// The kernel does not know which plugin is "the terminal": it knows one
    /// declared `input = "session"` and which session the tree it returned
    /// pointed at. Replace that plugin and this still works.
    pub(crate) fn dispatch_session_input(&mut self, key: &KeyEvent) -> bool {
        if !self.focused_wants_session_input() {
            return false;
        }
        let Some(surface) = self.focused_surface.clone() else {
            return false;
        };
        let Some(bytes) = key_to_bytes(key.code, key.modifiers) else {
            return false;
        };
        // Whether it lands is the terminal's business; either way the key belongs
        // to the pane that asked for raw input and is not offered to anything
        // else.
        //
        // Routed by what the pane is SHOWING. A program pane's keys go to that
        // program and to nothing else — and a pane with nothing behind it
        // swallows nothing, since neither send finds a target.
        let delivered = match self.terminals.program_key(&surface).cloned() {
            Some(program) => self.terminals.send_to_program(&program, bytes),
            None => self.terminals.send(&surface, bytes),
        };
        // Delivered means consumed, which is what the rule above says and what
        // this now enforces. Falling through sent `Esc` to a program AND
        // dismissed the pane under it in one keypress — a game opening its menu
        // on a pane nobody is looking at.
        //
        // Gated on delivery rather than on having tried, because both sends
        // already report it: a surface naming a session that is no longer live
        // must not swallow the key, or `Esc` traps the user in a pane showing a
        // dead terminal.
        delivered
    }

    /// The modal this keystroke opens, if it is one of the kernel's own chords.
    ///
    /// Resolved through the registry rather than matched literally, so a
    /// rebound chord keeps opening its modal — including from inside another
    /// one.
    pub(crate) fn modal_chord(&self, key: &KeyEvent) -> Option<ModalKind> {
        let binding = self.registry.resolve(&to_press(key), None)?;
        (binding.plugin == thurbox::kernel::modals::OWNER)
            .then(|| ModalKind::from_action(&binding.action))
            .flatten()
    }

    /// Hand a keystroke to the open modal, and report whatever it says.
    pub(crate) fn dispatch_modal_key(&mut self, key: &KeyEvent) {
        // The registry's spelling of this keystroke, so a captured chord is
        // stored in the vocabulary the registry will later match — the three
        // encodings of `ctrl+/` folded into one, a capital folded to `shift+`.
        let chord = canonical_chord(&to_press(key));
        let message = self.with_modal_world(|modals, world| modals.on_key(key, &chord, world));
        if let Some(message) = message {
            self.toast(message);
        }
        self.dirty = true;
    }

    /// Run `act` against the modal layer with everything a modal may write to.
    ///
    /// The database is opened only for the theme picker, which is the one modal
    /// that persists outside the registry — a connection per keystroke would
    /// otherwise be paid by every keypress in help.
    /// Open or close a system modal.
    ///
    /// Goes through `abandon` first because a modal may have applied something
    /// while it was open — the theme picker previews on every cursor move — and
    /// closing it by its own chord is no more a choice than closing it with
    /// `Esc`.
    pub(crate) fn toggle_modal(&mut self, kind: ModalKind) {
        self.with_modal_world(|modals, world| {
            if modals.kind() == Some(kind) {
                modals.abandon(world);
            }
        });
        self.modals.toggle(kind);
        self.dirty = true;
    }

    /// Offer a keystroke to one specific plugin: its declared action first, then
    /// its raw handler.
    ///
    /// Unlike `on_key` this never walks the focus order — the caller has already
    /// decided who should get it (the pane under the pointer, or a float).
    pub(crate) fn dispatch_key_to(&mut self, index: usize, key: &KeyEvent) {
        let press = to_press(key);
        let Some(plugin) = self.host.plugins.get(index).map(|p| p.name.clone()) else {
            return;
        };
        if let Some(action) = self
            .registry
            .resolve(&press, Some(&plugin))
            .map(|binding| binding.action.clone())
        {
            match self.host.on_action(index, &action) {
                Ok(true) => return,
                Ok(false) => {}
                Err(e) => self.errors.push(e),
            }
        }
        if let Err(e) = self.host.on_key(index, &press) {
            self.errors.push(e);
        }
    }

    /// Deliver pasted text to whatever has focus.
    ///
    /// One path for both routes — `ctrl+v` and the terminal's own paste — so the
    /// two cannot come to behave differently. A surface that takes typing gets
    /// the characters replayed as keystrokes, which is how it already receives
    /// them; a terminal gets one bracketed paste, so a prompt with newlines in
    /// it does not fire on the first one.
    pub(crate) fn on_paste(&mut self, text: String) {
        if text.is_empty() {
            return;
        }

        // A modal or a float owns typed input while it is up, so it owns a paste
        // too. Control characters are dropped rather than replayed: `Enter` into
        // a filter or a name field would submit it mid-paste.
        if self.modals.is_open() {
            for ch in text.chars().filter(|ch| !ch.is_control()) {
                self.dispatch_modal_key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            return;
        }
        if let Some(index) = self.grabbed {
            for ch in text.chars().filter(|ch| !ch.is_control()) {
                self.dispatch_key_to(index, &KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            self.dirty = true;
            return;
        }

        let Some(session) = self.focused_session.clone() else {
            self.report(
                "nothing to paste into — focus a session's terminal first",
                Level::Error,
            );
            return;
        };
        let mut bytes = Vec::with_capacity(text.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");

        self.toast(if self.terminals.send(&session, bytes) {
            format!("pasted {} character(s)", text.chars().count())
        } else {
            "no live terminal to paste into".to_string()
        });
    }

    /// Paste the clipboard into the focused session's terminal.
    ///
    /// Sent as a bracketed paste, so a multi-line paste arrives as text rather
    /// than as a series of submissions — an agent prompt with newlines in it
    /// would otherwise fire on the first one.
    pub(crate) fn paste_into_focused(&mut self) {
        let Some(text) = thurbox::clipboard::paste(self.clipboard.as_mut()) else {
            // No OSC 52 read fallback: terminals disable clipboard *reads* by
            // default and probing for one can stall for seconds. The route that
            // does work here is the terminal's own paste chord, which arrives as
            // `Event::Paste` — so point at it rather than reporting a dead end.
            self.toast(thurbox::clipboard::PASTE_UNAVAILABLE_HINT);
            return;
        };
        self.on_paste(text);
    }
}
