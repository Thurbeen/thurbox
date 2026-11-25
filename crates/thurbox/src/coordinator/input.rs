use std::error::Error;
use std::time::Instant;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

use thurbox::agent::input::key_to_bytes;
use thurbox::kernel::bands::Level;
use thurbox::kernel::clipboard;
use thurbox::kernel::host::{KeyPress, KeyReleases};
use thurbox::kernel::modals::ModalKind;
use thurbox::kernel::registry::{canonical_chord, is_ctrl_letter_chord};

use super::paste::{Input, PasteBurst};
use super::{next_event, to_press};
use crate::{App, INPUT_FAILURE_LIMIT};

impl App {
    pub(crate) fn drain_input(&mut self, input_failures: &mut u32) -> Result<(), Box<dyn Error>> {
        let mut published = false;
        let mut waited = false;
        loop {
            // Only the first read waits; the rest take what is already queued —
            // except while the paste coalescer holds a key it cannot yet
            // decide about, which is worth a few milliseconds of the batch.
            let waited_before = waited;
            let timeout = if waited {
                self.paste_burst.drain_timeout()
            } else {
                self.poll_timeout()
            };
            waited = true;
            let read = if waited_before {
                next_event(timeout)
            } else {
                self.wait_for_input(timeout)
            };
            let event = match read {
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
                Event::Key(key) => self.read_key(key, &mut published),
                // Dropped rather than merely uncaptured when the feature is
                // off, so the flag stays authoritative even if a terminal
                // reports mouse events unasked. v1 does the same in
                // `App::update`.
                Event::Mouse(mouse) if self.mouse => {
                    self.publish_for_batch(&mut published);
                    // Read before `on_mouse` consumes the event: only a left
                    // press, drag or release can move the selection, so a bare
                    // move or a wheel tick pays for no grid read.
                    let may_move_selection = matches!(
                        mouse.kind,
                        MouseEventKind::Down(MouseButton::Left)
                            | MouseEventKind::Drag(MouseButton::Left)
                            | MouseEventKind::Up(MouseButton::Left)
                    );
                    // A release that ends a drag still in progress — not one
                    // forwarded to a pty, and not a second release of a
                    // selection already finished.
                    let ends_drag = mouse.kind == MouseEventKind::Up(MouseButton::Left)
                        && self.selection.as_ref().is_some_and(|s| s.dragging);
                    self.on_mouse(mouse);
                    // The drag builds the selection here, but `selected_text` is
                    // only recomputed at paint time — so a chord queued behind it
                    // in this same batch would read the pre-drag selection.
                    // Refresh from the grid now and patch just the published
                    // scalar: a multi-cell drag reports once per crossed cell, so
                    // forcing a full republish here would rerun terminal sync,
                    // links, search, trust and inventory per cell. See
                    // `refresh_selection_text` and `LuaHost::set_published_selection`.
                    if may_move_selection && self.refresh_selection_text() {
                        self.host
                            .set_published_selection(self.selected_text.as_deref().unwrap_or(""));
                    }
                    // `on_mouse` has already dropped a release that never
                    // moved, so a click reaches here with no selection. A
                    // terminal's text came off its grid in the refresh above, so
                    // it is copied now, before a key queued behind the release
                    // can drop it; any other pane's text exists only once the
                    // next paint has read it.
                    if ends_drag {
                        match self.selection.clone() {
                            Some(sel) if self.grid_selection_text(&sel).is_some() => {
                                self.copy_on_select();
                            }
                            Some(_) => {
                                self.copy_after_paint = true;
                                self.dirty = true;
                            }
                            None => {}
                        }
                    }
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

    fn read_key(&mut self, key: KeyEvent, published: &mut bool) {
        // Before anything else looks at it, so the coalescer, the registry,
        // the fields and the pty encoder all see the same keystroke. See
        // `resolve_altgr`.
        let key = resolve_altgr(key, cfg!(windows));
        let before = self.holds.releases;
        let inputs = sort_key(
            &mut self.paste_burst,
            &mut self.holds,
            key,
            Instant::now(),
            cfg!(windows),
        );
        if self.holds.releases != before {
            // Published through the data epoch, so a pure pane reading
            // `thurbox.keyboard` is not served its old tree.
            self.data_epoch = self.data_epoch.wrapping_add(1);
            self.dirty = true;
        }
        for input in inputs {
            self.apply_input(input, published);
        }
    }

    fn apply_input(&mut self, input: Input, published: &mut bool) {
        // A release is routed here, in the order the coalescer gives it back,
        // rather than on arrival: a press it is still holding has not opened
        // its hold yet. See `sort_key`. One nobody holds — nearly every
        // release, on a terminal that reports them for every key — costs
        // nothing: no publish, and no activity that would repaint.
        if let Input::Key(key) = &input {
            if key.kind == KeyEventKind::Release {
                if let Some((mod_name, action)) = self.holds.release_target(key) {
                    self.publish_for_batch(published);
                    self.deliver_release(&mod_name, &action);
                    self.note_input();
                }
                return;
            }
        }
        self.publish_for_batch(published);
        match input {
            Input::Key(key) => self.time_op("input_dispatch", |app| app.on_key(&key)),
            Input::Paste(text) => self.on_paste(text),
        }
        self.note_input();
    }

    pub(crate) fn on_key(&mut self, key: &KeyEvent) {
        let press = to_press(key);
        // Resolved once, and used twice: it decides both whether the selection
        // survives this keystroke and, further down, whether the loop runs the
        // action itself.
        let kernel_action = self.kernel_action(&press);
        // Any key press clears the selection and still does what it does —
        // v1's rule. The one exception is the copy chord, which is what the
        // selection is for; it clears it itself once the copy is made. Asked of
        // the registry rather than matched literally, so the exception follows a
        // rebound copy instead of staying on `Ctrl+C`.
        //
        // Under copy-on-select the release already copied, so the chord is no
        // exception: it clears the selection like any key, finds none, and
        // falls through as the interrupt (Herdr's rule).
        let is_copy = kernel_action.as_deref() == Some(clipboard::COPY_ACTION)
            && !self.config.in_force().clipboard.copy_on_select;
        if !is_copy && self.selection.take().is_some() {
            self.dirty = true;
        }
        if self.dispatch_to_modal(key) {
            return;
        }
        if self.dispatch_reserved(key) {
            return;
        }
        if self.dispatch_clipboard(kernel_action.as_deref(), key) {
            return;
        }
        if self.dispatch_grabbed(&press) {
            return;
        }
        if self.dispatch_declared(&press, key.code, key.modifiers) {
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

    pub(crate) fn dispatch_reserved(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let action = match key.code {
            KeyCode::Char('q') if ctrl => "core.quit",
            KeyCode::F(10) => "kernel.reload",
            KeyCode::Char('h') if ctrl => "kernel.focus_previous",
            KeyCode::Char('l') if ctrl => "kernel.focus_next",
            KeyCode::F(12) => "kernel.perf_hud",
            _ => return false,
        };
        self.run_kernel_action(action)
    }

    pub(crate) fn run_kernel_action(&mut self, action: &str) -> bool {
        match action {
            "core.quit" | "kernel.quit" => self.quit = true,
            "kernel.reload" => self.reload_by_key(),
            "kernel.focus_previous" => self.cycle_focus_action(-1),
            "kernel.focus_next" => self.cycle_focus_action(1),
            "kernel.perf_hud" if self.config.features().perf_hud => {
                self.hud = !self.hud;
                self.dirty = true;
            }
            "kernel.perf_hud" => return false,
            _ => {
                if let Some(kind) = ModalKind::from_action(action) {
                    self.toggle_modal(kind);
                } else {
                    return self.run_clipboard_action(action).unwrap_or(false);
                }
            }
        }
        true
    }

    pub(crate) fn kernel_action(&self, press: &KeyPress) -> Option<String> {
        let binding = self.registry.resolve(press, None)?;
        (binding.owner == thurbox::kernel::modals::OWNER).then(|| binding.action.clone())
    }

    pub(crate) fn dispatch_clipboard(&mut self, action: Option<&str>, key: &KeyEvent) -> bool {
        let Some(action) = action else {
            return false;
        };
        match self.run_clipboard_action(action) {
            Some(true) => true,
            Some(false) => {
                action == clipboard::PASTE_ACTION && self.hand_unencodable_paste_over(key)
            }
            None => false,
        }
    }

    fn hand_unencodable_paste_over(&mut self, key: &KeyEvent) -> bool {
        if reaches_the_pty(key) {
            return false;
        }
        if self.overlay_owns_input() || !self.focused_wants_session_input() {
            return false;
        }
        let Some(surface) = self.focused_surface.clone() else {
            return false;
        };
        self.send_to_surface(&surface, vec![CTRL_V])
    }

    pub(crate) fn run_clipboard_action(&mut self, action: &str) -> Option<bool> {
        match action {
            clipboard::COPY_ACTION => {
                if self.selection.is_none() {
                    return Some(false);
                }
                // No focused session required: a selection over the session
                // list, a modal or the footer is still a selection, and v1
                // copies it.
                self.copy_selection();
                Some(true)
            }
            clipboard::PASTE_ACTION => Some(self.paste_into_focused()),
            _ => None,
        }
    }

    pub(crate) fn dispatch_grabbed(&mut self, press: &KeyPress) -> bool {
        let Some(mod_name) = self
            .grabbed
            .and_then(|index| self.host.mods.get(index))
            .map(|loaded| loaded.name.clone())
        else {
            return false;
        };
        let index = self.grabbed.expect("just resolved through it");
        if let Some(action) = self
            .registry
            .resolve(press, Some(&mod_name))
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

    pub(crate) fn dispatch_declared(
        &mut self,
        press: &KeyPress,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        let focused_name = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.mods.get(*index))
            .map(|loaded| loaded.name.clone());
        let Some((owner, action, passthrough, release)) = self
            .registry
            .resolve(press, focused_name.as_deref())
            .map(|binding| {
                (
                    binding.owner.clone(),
                    binding.action.clone(),
                    binding.passthrough,
                    binding.release,
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
        //
        // A dead pane is the exception: an agent that exited (a `/exit`, and
        // the window kept by `remain-on-exit`) is doing no line editing, and
        // tmux takes `send-keys` into a dead pane without complaint — so the
        // deferred chord vanished and `Ctrl+D` could not delete the session it
        // was looking at. There the chord keeps its list meaning. The backend
        // is asked only here, on the chord itself, never on the render path.
        let defer_to_agent = passthrough
            && self.focused_wants_session_input()
            && is_ctrl_letter_chord(&canonical_chord(press))
            && !self.focused_terminal_is_dead();
        if defer_to_agent {
            return false;
        }
        if owner == thurbox::kernel::modals::OWNER {
            if let Some(kind) = ModalKind::from_action(&action) {
                self.toggle_modal(kind);
                return true;
            }
        }
        let Some(index) = self.host.index_of(&owner) else {
            return false;
        };
        let handled = if release {
            self.host
                .on_action_with_args(index, &action, &[("event", "press")])
        } else {
            self.host.on_action(index, &action)
        };
        match handled {
            Ok(true) => {
                if release {
                    self.holds.hold(code, modifiers, owner, action);
                }
                true
            }
            Ok(false) => false,
            Err(e) => {
                self.errors.push(e);
                false
            }
        }
    }

    fn deliver_release(&mut self, mod_name: &str, action: &str) {
        self.dirty = true;
        let Some(index) = self.host.index_of(mod_name) else {
            return;
        };
        if let Err(e) = self
            .host
            .on_action_with_args(index, action, &[("event", "release")])
        {
            self.errors.push(e);
        }
    }

    pub(crate) fn dispatch_raw(&mut self, press: &KeyPress) -> bool {
        let focusable = self.host.focusable();
        let mut order: Vec<usize> = focusable.get(self.focus).copied().into_iter().collect();
        order.extend((0..self.host.mods.len()).filter(|index| !self.host.mods[*index].focusable));
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
        if self.held_input.holds(&surface) {
            self.held_input.hold_bytes(surface, bytes);
            return true;
        }
        let delivered = self.send_to_surface(&surface, bytes);
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

    pub(crate) fn modal_chord(&self, key: &KeyEvent) -> Option<ModalKind> {
        ModalKind::from_action(&self.kernel_action(&to_press(key))?)
    }

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

    pub(crate) fn toggle_modal(&mut self, kind: ModalKind) {
        self.with_modal_world(|modals, world| {
            if modals.kind() == Some(kind) {
                modals.abandon(world);
            }
        });
        self.modals.toggle(kind);
        self.dirty = true;
    }

    pub(crate) fn reload_by_key(&mut self) {
        self.reload_interface(super::events::ReloadReason::Key);
        self.collect_declarations();
        self.clamp_focus();
    }

    pub(crate) fn run_action(&mut self, mod_name: &str, action: &str) {
        self.dirty = true;
        if !self
            .registry
            .action_catalog()
            .iter()
            .any(|descriptor| descriptor.name == action && descriptor.owner == mod_name)
        {
            self.report(
                format!("no catalog action named {action:?} for {mod_name:?}"),
                Level::Error,
            );
            return;
        }
        if mod_name == thurbox::kernel::modals::OWNER {
            if let Some(kind) = ModalKind::from_action(action) {
                self.toggle_modal(kind);
                return;
            }
            // Chosen by name rather than pressed, so a decline falls through to
            // nothing and would look like a dead row: say why instead. Both
            // chords can decline and they decline for different reasons, so the
            // message follows the action rather than assuming copy.
            match self.run_clipboard_action(action) {
                Some(true) => return,
                Some(false) => {
                    self.toast(if action == thurbox::kernel::clipboard::PASTE_ACTION {
                        "nothing to paste — the clipboard holds no text"
                    } else {
                        "nothing to copy"
                    });
                    return;
                }
                None => {}
            }
            match action {
                thurbox::kernel::modals::palette::RELOAD_ACTION => self.reload_by_key(),
                thurbox::kernel::modals::palette::QUIT_ACTION => self.quit = true,
                other => self.report(format!("no kernel action named {other:?}"), Level::Error),
            }
            return;
        }
        let Some(index) = self.host.index_of(mod_name) else {
            self.report(format!("no mod named {mod_name:?}"), Level::Error);
            return;
        };
        if let Err(e) = self.host.on_action(index, action) {
            self.errors.push(e);
        }
    }

    pub(crate) fn dispatch_key_to(&mut self, index: usize, key: &KeyEvent) {
        let press = to_press(key);
        let Some(mod_name) = self.host.mods.get(index).map(|p| p.name.clone()) else {
            return;
        };
        if let Some(action) = self
            .registry
            .resolve(&press, Some(&mod_name))
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
        // And so does a pane holding the caret in a field of its own — the
        // search strip. It is not a surface, so this went to the terminal
        // behind it, or with none on screen was refused.
        let typing = if self.focused_typing {
            self.host.focusable().get(self.focus).copied()
        } else {
            None
        };
        // Straight to `on_key`, past the registry: a paste is text, and a pane
        // that binds a key a paste can hold — the new-session float's `j`/`k`/`space` — would
        // otherwise run the action for every one pasted instead of typing it.
        if let Some(index) = self.grabbed.or(typing) {
            for ch in text.chars().filter(|ch| !ch.is_control()) {
                let press = to_press(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
                if let Err(e) = self.host.on_key(index, &press) {
                    self.errors.push(e);
                }
            }
            self.dirty = true;
            return;
        }

        let Some(surface) = self.focused_surface.clone() else {
            self.report(NOTHING_TO_PASTE_INTO, Level::Error);
            return;
        };
        if self.holds_a_remote_image_paste(&surface, &text) {
            return;
        }
        if self.held_input.holds(&surface) {
            self.held_input
                .hold_paste(surface, text, None, std::time::Instant::now());
            return;
        }
        self.paste_text_into(&surface, &text);
    }

    fn holds_a_remote_image_paste(&mut self, surface: &str, text: &str) -> bool {
        let session = surface.strip_suffix("#shell").unwrap_or(surface);
        let remote = self
            .snapshots
            .current()
            .session(session)
            .is_some_and(|row| row.remote_host.is_some());
        let Some(paths) = remote
            .then(|| super::image_drop::dropped_images(text))
            .flatten()
        else {
            return false;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(paths.iter().all(|path| path.is_file()));
        });
        self.held_input.hold_paste(
            surface.to_string(),
            text.to_string(),
            Some(rx),
            std::time::Instant::now(),
        );
        true
    }

    pub(crate) fn poll_remote_drop(&mut self) {
        for release in self.held_input.release(std::time::Instant::now()) {
            match release {
                Release::Refuse => self.report(
                    "image drop isn't supported for remote sessions yet",
                    Level::Error,
                ),
                Release::Paste { surface, text } => self.paste_text_into(&surface, &text),
                Release::Bytes { surface, bytes } => {
                    self.send_to_surface(&surface, bytes);
                }
            }
            self.dirty = true;
        }
    }

    fn paste_text_into(&mut self, surface: &str, text: &str) {
        let text = paste_safe(text);
        if text.is_empty() {
            return;
        }
        let mut bytes = Vec::with_capacity(text.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");

        let delivered = self.send_to_surface(surface, bytes);
        self.toast(if delivered {
            format!("pasted {} character(s)", text.chars().count())
        } else {
            "no live terminal to paste into".to_string()
        });
    }

    pub(crate) fn paste_into_focused(&mut self) -> bool {
        // Inside WSL the local clipboard is not the one being copied into, so
        // this press cannot be answered here at all: Windows has to be asked,
        // and asking costs ~0.42 s. The press is claimed, the answer acts on it
        // — [`Self::poll_image_probe`].
        //
        // Not while an overlay owns typed input, though: a float's name field
        // cannot take a picture, so the question has no answer worth 0.42 s —
        // and asking anyway used to *swallow* the press, because the clipboard
        // stage runs before `dispatch_grabbed` and the answer then found the
        // float still up. Pasting a repository path into the new-session wizard
        // did nothing at all under WSL.
        if thurbox::clipboard::ImageProbe::applies() && !self.overlay_owns_input() {
            return self.ask_windows_about_this_press();
        }
        self.paste_text_or_decline()
    }

    pub(crate) fn overlay_owns_input(&self) -> bool {
        self.modals.is_open() || self.grabbed.is_some() || self.focused_typing
    }

    fn send_to_surface(&mut self, surface: &str, bytes: Vec<u8>) -> bool {
        let echo = self.expect_echo(surface);
        let delivered = match self.terminals.program_key(surface).cloned() {
            Some(program) => self.terminals.send_to_program(&program, bytes).is_ok(),
            None => self.terminals.send(surface, bytes),
        };
        if delivered {
            self.last_keystroke = Some(Instant::now());
            if let Some(echo) = echo {
                self.echo.push_back(echo);
                self.arm_echo_wake();
            }
        }
        delivered
    }

    fn ask_windows_about_this_press(&mut self) -> bool {
        let Some(surface) = self.focused_surface.clone() else {
            self.report(NOTHING_TO_PASTE_INTO, Level::Error);
            return true;
        };
        // Key auto-repeat outruns the answer: holding `Ctrl+V` makes presses at
        // tens a second against a question that takes a fifth of a second, and
        // every one of them is a paste owed. Past this many the repeat is no
        // longer someone asking for another paste.
        if self.paste_targets.len() >= MAX_WAITING_PASTES {
            tracing::debug!(
                "{MAX_WAITING_PASTES} pastes are already waiting on Windows; dropping this press"
            );
            return true;
        }
        self.paste_targets.push(surface);
        if self.image_probe.ask() {
            // This question describes the clipboard as it is now, so it answers
            // for the presses made by now — no more. A press that arrives while
            // it is out gets one of its own, asked when this one comes back.
            self.probed_presses = self.paste_targets.len();
        }
        true
    }

    fn paste_text_or_decline(&mut self) -> bool {
        let text = thurbox::clipboard::paste(self.clipboard.as_mut());
        match (paste_route(self.clipboard.is_some(), text.is_some()), text) {
            (PasteRoute::Hint, _) => {
                self.toast(thurbox::clipboard::PASTE_UNAVAILABLE_HINT);
                true
            }
            (PasteRoute::Send, Some(text)) => {
                self.on_paste(text);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn poll_image_probe(&mut self) {
        let Some(verdict) = self.image_probe.poll() else {
            return;
        };
        let answered = self.probed_presses.min(self.paste_targets.len());
        self.probed_presses = 0;
        let waiting = self.paste_targets.split_off(answered);
        let answered = std::mem::replace(&mut self.paste_targets, waiting);
        for surface in answered {
            self.deliver_probed_paste(verdict, &surface);
        }
        if !self.paste_targets.is_empty() && self.image_probe.ask() {
            self.probed_presses = self.paste_targets.len();
        }
    }

    fn deliver_probed_paste(&mut self, verdict: thurbox::clipboard::Verdict, surface: &str) {
        use thurbox::clipboard::Verdict;
        if verdict == Verdict::NotImage {
            // The same decision the unprobed press makes, read from the same
            // function: what Windows answered says only whether this press is
            // thurbox's to handle, never what handling it looks like.
            let text = thurbox::clipboard::paste(self.clipboard.as_mut());
            match (paste_route(self.clipboard.is_some(), text.is_some()), text) {
                (PasteRoute::Hint, _) => {
                    self.toast(thurbox::clipboard::PASTE_UNAVAILABLE_HINT);
                    return;
                }
                (PasteRoute::Send, Some(text)) => {
                    self.paste_text_into(surface, &text);
                    return;
                }
                _ => {}
            }
        }
        // Said out loud, because this is the one delivery the person may not
        // see happen: the press can land behind a modal or a float that went up
        // while the question was out, and what arrives there is a byte the
        // agent acts on rather than text appearing in a prompt. The text path
        // (`paste_text_into`) already reports itself the same way.
        if self.send_to_surface(surface, vec![CTRL_V]) {
            self.toast(match verdict {
                Verdict::Image => "image left to the agent to fetch",
                _ => "Windows could not be asked; the paste went to the agent",
            });
        } else {
            self.toast("no live terminal to paste into");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasteRoute {
    Hint,
    Send,
    GiveToAgent,
}

fn paste_safe(text: &str) -> std::borrow::Cow<'_, str> {
    let keep = |ch: char| !ch.is_control() || matches!(ch, '\t' | '\n' | '\r');
    if text.chars().all(keep) {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(text.chars().filter(|&ch| keep(ch)).collect())
    }
}

fn paste_route(clipboard_present: bool, yielded_text: bool) -> PasteRoute {
    match (clipboard_present, yielded_text) {
        (false, _) => PasteRoute::Hint,
        (true, true) => PasteRoute::Send,
        (true, false) => PasteRoute::GiveToAgent,
    }
}

fn reaches_the_pty(key: &KeyEvent) -> bool {
    key_to_bytes(key.code, key.modifiers).is_some()
}

const CTRL_V: u8 = 0x16;

const NOTHING_TO_PASTE_INTO: &str = "nothing to paste into — focus a session's terminal first";

const MAX_WAITING_PASTES: usize = 8;

fn resolve_altgr(key: KeyEvent, windows: bool) -> KeyEvent {
    if !windows {
        return key;
    }
    let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
    if !key.modifiers.contains(altgr) {
        return key;
    }
    let KeyCode::Char(ch) = key.code else {
        return key;
    };
    if ch.is_ascii_alphanumeric() || ch.is_control() {
        return key;
    }
    KeyEvent {
        modifiers: key.modifiers - altgr,
        ..key
    }
}

#[derive(Debug, Default)]
pub(crate) struct Holds {
    held: Vec<(KeyCode, KeyModifiers, String, String)>,
    pub(crate) releases: KeyReleases,
}

pub(crate) fn sort_key(
    burst: &mut PasteBurst,
    holds: &mut Holds,
    key: KeyEvent,
    at: Instant,
    windows: bool,
) -> Vec<Input> {
    if key.kind == KeyEventKind::Release {
        holds.releases = KeyReleases::Reported;
        return burst.push(key, at);
    }
    match holds.route(&key, windows) {
        Route::Press(key) => burst.push(key, at),
        Route::Release { .. } | Route::Drop => Vec::new(),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Press(KeyEvent),
    Release { owner: String, action: String },
    Drop,
}

fn hold_key(code: KeyCode) -> KeyCode {
    match code {
        KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
        other => other,
    }
}

impl Holds {
    pub(crate) fn route(&mut self, key: &KeyEvent, windows: bool) -> Route {
        let code = hold_key(key.code);
        let held = self.held.iter().position(|(held, ..)| *held == code);
        // Only once a release has arrived: a terminal that never sends one
        // never ends the hold, and would eat that key for good.
        let repeat = held.is_some_and(|at| self.held[at].1 != key.modifiers)
            && self.releases == KeyReleases::Reported;
        match key.kind {
            KeyEventKind::Release => {
                self.releases = KeyReleases::Reported;
                match held {
                    Some(at) => {
                        let (_, _, owner, action) = self.held.remove(at);
                        Route::Release { owner, action }
                    }
                    None => Route::Drop,
                }
            }
            KeyEventKind::Repeat if held.is_some() => Route::Drop,
            KeyEventKind::Press if (windows && held.is_some()) || repeat => Route::Drop,
            KeyEventKind::Press | KeyEventKind::Repeat => Route::Press(KeyEvent {
                kind: KeyEventKind::Press,
                ..*key
            }),
        }
    }

    pub(crate) fn release_target(&mut self, key: &KeyEvent) -> Option<(String, String)> {
        match self.route(key, false) {
            Route::Release { owner, action } => Some((owner, action)),
            Route::Press(_) | Route::Drop => None,
        }
    }

    pub(crate) fn hold(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        owner: String,
        action: String,
    ) {
        if self.releases == KeyReleases::Unsupported {
            return;
        }
        let code = hold_key(code);
        self.held.retain(|(held, ..)| *held != code);
        self.held.push((code, modifiers, owner, action));
    }

    pub(crate) fn clear(&mut self) {
        self.held.clear();
    }
}

pub(crate) const HELD_PASTE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Default)]
pub(crate) struct HeldInput {
    queue: std::collections::VecDeque<Held>,
}

enum Held {
    Paste {
        surface: String,
        text: String,
        answer: Option<std::sync::mpsc::Receiver<bool>>,
        deadline: std::time::Instant,
    },
    Bytes {
        surface: String,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Release {
    Refuse,
    Paste { surface: String, text: String },
    Bytes { surface: String, bytes: Vec<u8> },
}

impl HeldInput {
    pub(crate) fn holds(&self, surface: &str) -> bool {
        self.queue.iter().any(|held| held.surface() == surface)
    }

    pub(crate) fn hold_paste(
        &mut self,
        surface: String,
        text: String,
        answer: Option<std::sync::mpsc::Receiver<bool>>,
        now: std::time::Instant,
    ) {
        self.queue.push_back(Held::Paste {
            surface,
            text,
            answer,
            deadline: now + HELD_PASTE_WAIT,
        });
    }

    pub(crate) fn hold_bytes(&mut self, surface: String, bytes: Vec<u8>) {
        self.queue.push_back(Held::Bytes { surface, bytes });
    }

    pub(crate) fn release(&mut self, now: std::time::Instant) -> Vec<Release> {
        let mut out = Vec::new();
        let mut waiting: Vec<String> = Vec::new();
        let mut kept = std::collections::VecDeque::new();
        for held in std::mem::take(&mut self.queue) {
            if waiting.iter().any(|surface| surface == held.surface()) {
                kept.push_back(held);
                continue;
            }
            let local = match &held {
                Held::Paste {
                    answer: Some(rx),
                    deadline,
                    surface,
                    ..
                } => match rx.try_recv() {
                    Ok(local) => local,
                    Err(std::sync::mpsc::TryRecvError::Empty) if now < *deadline => {
                        waiting.push(surface.clone());
                        kept.push_back(held);
                        continue;
                    }
                    Err(_) => false,
                },
                _ => false,
            };
            out.push(match held {
                Held::Paste { .. } if local => Release::Refuse,
                Held::Paste { surface, text, .. } => Release::Paste { surface, text },
                Held::Bytes { surface, bytes } => Release::Bytes { surface, bytes },
            });
        }
        self.queue = kept;
        out
    }
}

impl Held {
    fn surface(&self) -> &str {
        match self {
            Held::Paste { surface, .. } | Held::Bytes { surface, .. } => surface,
        }
    }
}

#[cfg(test)]
mod held_input_tests {
    use super::{HeldInput, Release, HELD_PASTE_WAIT};
    use std::time::Instant;

    #[test]
    fn a_held_paste_holds_only_its_own_surface() {
        let mut held = HeldInput::default();
        let (_tx, rx) = std::sync::mpsc::channel();
        held.hold_paste("far".into(), "/x.png".into(), Some(rx), Instant::now());
        assert!(held.holds("far"));
        assert!(
            !held.holds("near"),
            "another session's input waits on nothing"
        );
    }

    #[test]
    fn a_paste_whose_check_never_answers_is_typed_after_the_bound() {
        let mut held = HeldInput::default();
        let (_tx, rx) = std::sync::mpsc::channel();
        let start = Instant::now();
        held.hold_paste("far".into(), "/x.png".into(), Some(rx), start);
        held.hold_bytes("far".into(), b"\r".to_vec());
        assert!(held.release(start).is_empty());
        assert_eq!(
            held.release(start + HELD_PASTE_WAIT),
            vec![
                Release::Paste {
                    surface: "far".into(),
                    text: "/x.png".into()
                },
                Release::Bytes {
                    surface: "far".into(),
                    bytes: b"\r".to_vec()
                },
            ]
        );
        assert!(!held.holds("far"));
    }

    #[test]
    fn a_pending_paste_does_not_stop_another_surfaces_input() {
        let mut held = HeldInput::default();
        let (_tx, rx) = std::sync::mpsc::channel();
        let start = Instant::now();
        held.hold_paste("far".into(), "/x.png".into(), Some(rx), start);
        held.hold_bytes("near".into(), b"a".to_vec());
        assert_eq!(
            held.release(start),
            vec![Release::Bytes {
                surface: "near".into(),
                bytes: b"a".to_vec()
            }]
        );
    }

    #[test]
    fn a_refused_paste_lets_what_followed_it_through() {
        let mut held = HeldInput::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let start = Instant::now();
        held.hold_paste("far".into(), "/x.png".into(), Some(rx), start);
        held.hold_bytes("far".into(), b"\r".to_vec());
        tx.send(true).unwrap();
        assert_eq!(
            held.release(start),
            vec![
                Release::Refuse,
                Release::Bytes {
                    surface: "far".into(),
                    bytes: b"\r".to_vec()
                }
            ]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, kind)
    }

    fn holding(releases: KeyReleases) -> Holds {
        let mut holds = Holds {
            releases,
            ..Holds::default()
        };
        holds.hold(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL,
            "voice".into(),
            "voice.toggle".into(),
        );
        holds
    }

    #[test]
    fn a_held_key_swallows_its_repeats_and_hands_its_release_to_the_holder() {
        let mut holds = holding(KeyReleases::Negotiated);
        let space = |kind| event(KeyCode::Char(' '), KeyModifiers::CONTROL, kind);
        assert_eq!(
            holds.route(&space(KeyEventKind::Repeat), false),
            Route::Drop
        );
        // Ctrl let go first: the release still finds the hold.
        assert_eq!(
            holds.route(
                &event(
                    KeyCode::Char(' '),
                    KeyModifiers::NONE,
                    KeyEventKind::Release
                ),
                false
            ),
            Route::Release {
                owner: "voice".into(),
                action: "voice.toggle".into()
            }
        );
        assert_eq!(holds.releases, KeyReleases::Reported);
        assert_eq!(
            holds.route(&space(KeyEventKind::Release), false),
            Route::Drop,
            "a second release has no holder"
        );
    }

    #[test]
    fn other_keys_repeat_as_presses_and_their_releases_go_nowhere() {
        let mut holds = holding(KeyReleases::Negotiated);
        let j = |kind| event(KeyCode::Char('j'), KeyModifiers::NONE, kind);
        assert_eq!(
            holds.route(&j(KeyEventKind::Repeat), false),
            Route::Press(j(KeyEventKind::Press))
        );
        assert_eq!(holds.route(&j(KeyEventKind::Release), false), Route::Drop);
    }

    #[test]
    fn windows_reports_auto_repeat_as_presses_of_the_held_key() {
        let mut holds = holding(KeyReleases::Negotiated);
        let press = event(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        assert_eq!(holds.route(&press, true), Route::Drop);
        assert_eq!(
            holds.route(&press, false),
            Route::Press(press),
            "not elsewhere"
        );
    }

    #[test]
    fn a_held_chords_text_repeat_is_swallowed_and_the_chord_itself_presses_again() {
        let mut holds = holding(KeyReleases::Reported);
        // kitty, `ctrl` let go first: the repeat arrives as plain text.
        let bare = event(KeyCode::Char(' '), KeyModifiers::NONE, KeyEventKind::Press);
        assert_eq!(holds.route(&bare, false), Route::Drop);
        // The chord pressed again with no release between is a new press.
        let chord = event(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        assert_eq!(holds.route(&chord, false), Route::Press(chord));
    }

    #[test]
    fn before_any_release_a_held_keys_text_still_types() {
        let mut holds = holding(KeyReleases::Negotiated);
        let bare = event(KeyCode::Char(' '), KeyModifiers::NONE, KeyEventKind::Press);
        assert_eq!(holds.route(&bare, false), Route::Press(bare));
    }

    #[test]
    fn a_release_waits_behind_its_press_in_the_coalescer() {
        let mut burst = super::super::paste::PasteBurst::new(true);
        let mut holds = Holds {
            releases: KeyReleases::Negotiated,
            ..Holds::default()
        };
        let now = Instant::now();
        let press = event(KeyCode::Char(' '), KeyModifiers::NONE, KeyEventKind::Press);
        let release = event(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        let mut inputs = sort_key(&mut burst, &mut holds, press, now, true);
        inputs.extend(sort_key(&mut burst, &mut holds, release, now, true));
        assert!(
            inputs.is_empty(),
            "the press is still in its run: {inputs:?}"
        );
        inputs.extend(burst.flush());
        assert_eq!(inputs, vec![Input::Key(press), Input::Key(release)]);

        // Dispatched in that order — the press taken by a release binding (as
        // `dispatch_declared` records it), then the release through the same
        // `release_target` `apply_input` asks — the hold is opened and closed.
        holds.hold(press.code, press.modifiers, "p".into(), "p.talk".into());
        assert_eq!(
            holds.release_target(&release),
            Some(("p".into(), "p.talk".into()))
        );
        assert_eq!(holds.release_target(&release), None, "closed once");
        assert_eq!(
            holds.route(&press, true),
            Route::Press(press),
            "not left held"
        );
    }

    #[test]
    fn a_shifted_press_is_closed_by_its_unshifted_release() {
        let mut burst = super::super::paste::PasteBurst::new(true);
        let mut holds = Holds {
            releases: KeyReleases::Negotiated,
            ..Holds::default()
        };
        let now = Instant::now();
        let press = event(KeyCode::Char('A'), KeyModifiers::SHIFT, KeyEventKind::Press);
        let release = event(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        let mut inputs = sort_key(&mut burst, &mut holds, press, now, true);
        inputs.extend(sort_key(&mut burst, &mut holds, release, now, true));
        inputs.extend(burst.flush());
        assert_eq!(inputs, vec![Input::Key(press), Input::Key(release)]);
        holds.hold(press.code, press.modifiers, "p".into(), "p.shout".into());
        assert_eq!(
            holds.release_target(&release),
            Some(("p".into(), "p.shout".into()))
        );
    }

    #[test]
    fn nothing_is_held_where_no_release_can_come() {
        let mut holds = holding(KeyReleases::Unsupported);
        let press = event(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        assert_eq!(holds.route(&press, true), Route::Press(press));
    }

    #[test]
    fn a_paste_keeps_its_text_lines_and_unicode() {
        let text = "l1 é漢\nl2\ttab\r\nl3 🙂";
        assert!(matches!(paste_safe(text), std::borrow::Cow::Borrowed(t) if t == text));
    }

    #[test]
    fn a_paste_cannot_spell_a_marker_or_a_control_key() {
        assert_eq!(
            paste_safe("echo a\x1b[201~echo b\r\x1b[200~\x03\x04\x7f\u{9b}c"),
            "echo a[201~echo b\r[200~c"
        );
    }

    #[test]
    fn a_clipboard_with_nothing_to_paste_hands_the_press_on() {
        assert_eq!(paste_route(true, false), PasteRoute::GiveToAgent);
        assert_eq!(paste_route(true, true), PasteRoute::Send);
        assert_eq!(
            paste_route(false, false),
            PasteRoute::Hint,
            "with no clipboard on this machine there is nothing for the agent \
             to read either"
        );
    }

    #[test]
    fn a_cmd_chord_cannot_be_handed_on_by_declining_it() {
        let cmd_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SUPER);
        let ctrl_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert!(
            !reaches_the_pty(&cmd_v),
            "a declined Cmd+V would reach the agent on its own"
        );
        assert!(reaches_the_pty(&ctrl_v));
    }

    fn altgr(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL | KeyModifiers::ALT)
    }

    #[test]
    fn altgr_punctuation_is_typed_rather_than_treated_as_a_chord() {
        for ch in ['\\', '|', '@', '[', ']', '{', '}', '~', '#', '€', 'ą'] {
            let resolved = resolve_altgr(altgr(ch), true);
            assert_eq!(resolved.code, KeyCode::Char(ch));
            assert!(
                resolved.modifiers.is_empty(),
                "{ch:?} kept {:?}",
                resolved.modifiers
            );
            // And the pty encoding is the character itself, not an ESC-wrapped
            // one — which is what a focused agent actually receives.
            assert_eq!(
                thurbox::agent::input::key_to_bytes(resolved.code, resolved.modifiers),
                Some(ch.to_string().into_bytes()),
            );
        }
    }

    #[test]
    fn a_real_ctrl_alt_chord_is_left_alone() {
        for ch in ['d', 'D', '7'] {
            assert_eq!(resolve_altgr(altgr(ch), true), altgr(ch));
        }
    }

    #[test]
    fn other_platforms_keep_ctrl_alt_as_a_chord() {
        assert_eq!(resolve_altgr(altgr('\\'), false), altgr('\\'));
    }

    #[test]
    fn one_modifier_alone_is_never_altgr() {
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let key = KeyEvent::new(KeyCode::Char('\\'), modifiers);
            assert_eq!(resolve_altgr(key, true), key);
        }
    }

    #[test]
    fn a_fourth_level_press_keeps_its_shift() {
        let key = KeyEvent::new(
            KeyCode::Char('¤'),
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        assert_eq!(
            resolve_altgr(key, true).modifiers,
            KeyModifiers::SHIFT,
            "the layout level that produced the character is not a chord"
        );
    }
}
