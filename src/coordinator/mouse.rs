//! Pointer input: hit-testing, hover, selection and links.
//!
//! Everything here reads `click_targets`, the identified nodes of the frame just
//! painted, scanned in reverse so the innermost node under a point — and, across
//! plugins, the one painted last — wins. Bands keep their own list: a click on
//! one must not focus a pane, and there is no plugin index to record.

use super::*;

impl App {
    pub(crate) fn on_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.on_click(mouse.column, mouse.row, mouse.modifiers)
            }
            MouseEventKind::Drag(MouseButton::Left) => self.drag_selection(mouse.column, mouse.row),
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(selection) = &mut self.selection {
                    selection.dragging = false;
                    // A press that never moved is a click, not a selection —
                    // v1's rule on release. Keeping it armed made every later
                    // `Ctrl+C` a copy of the whole screen instead of the
                    // interrupt the shell was waiting for.
                    if selection.anchor == selection.cursor {
                        self.selection = None;
                    }
                }
            }
            MouseEventKind::ScrollUp => self.on_scroll(mouse.column, mouse.row, true),
            MouseEventKind::ScrollDown => self.on_scroll(mouse.column, mouse.row, false),
            // A bare move only matters when it changes what is under the
            // pointer. Anything else — and there is a LOT of it, one report per
            // cell crossed — is dropped without touching `dirty`.
            MouseEventKind::Moved => self.hover(mouse.column, mouse.row),
            _ => {}
        }
    }

    /// A wheel tick, routed the way v1's `handle_mouse_scroll` routes one.
    ///
    /// Three legs, in order: an open modal owns the wheel outright; a live
    /// terminal that asked for mouse tracking gets the tick forwarded to its
    /// pty; otherwise the pane **under the pointer** scrolls — not the focused
    /// one, so you can spin the wheel over a list without leaving the pane you
    /// are working in.
    ///
    /// A tick becomes an `up`/`down` keystroke rather than a scroll command of
    /// its own: every scrollable pane already declares those, so the wheel and
    /// the arrow keys cannot come to mean different things — the same reasoning
    /// behind the `key:<chord>` click role.
    pub(crate) fn on_scroll(&mut self, x: u16, y: u16, up: bool) {
        // The selection is in screen cells and the text under them is about
        // to move; v1 drops it on every scroll for the same reason.
        self.selection = None;
        let code = if up { KeyCode::Up } else { KeyCode::Down };
        let key = KeyEvent::new(code, KeyModifiers::NONE);

        // A modal takes the wheel as one selection step, never the panes it
        // covers. While help is capturing a chord it is left alone: the capture
        // would record the synthesized keystroke as the new binding.
        if self.modals.is_open() {
            if !self.modals.captures_everything() {
                self.dispatch_modal_key(&key);
            }
            return;
        }

        // A float owns the wheel for the same reason it owns clicks.
        if let Some(index) = self.grabbed {
            self.dispatch_key_to(index, &key);
            self.dirty = true;
            return;
        }

        if self.terminals.forward_wheel(x, y, up) {
            self.dirty = true;
            return;
        }

        if let Some(target) = self.target_at(x, y) {
            self.dispatch_key_to(target.plugin, &key);
            self.dirty = true;
        }
    }

    /// Track the affordance under the pointer, repainting only when it changes.
    pub(crate) fn hover(&mut self, x: u16, y: u16) {
        // A modal owns the pointer while it is up, exactly as it owns clicks.
        // It has to be asked directly: it paints cell by cell and records no
        // `click_targets`, so hit-testing them would find only the panes it
        // covers — which are unreachable anyway, which is why the pane hover is
        // dropped rather than left frozen under the dim. v1 draws the same line
        // in `apply_hover_highlight`.
        if self.modals.is_open() {
            let moved = self.modals.on_hover(x, y);
            let dropped = self.hovered.take().is_some();
            if moved || dropped {
                self.dirty = true;
            }
            return;
        }

        let under = self
            .band_target_at(x, y)
            .map(|hit| hit.identity.clone())
            .or_else(|| self.target_at(x, y).map(|target| target.identity))
            .filter(|identity| !identity.is_empty());
        if under != self.hovered {
            self.hovered = under;
            self.dirty = true;
        }
    }

    /// A left press: modal, then link, then target, then a text selection.
    ///
    /// The order is v1's `handle_mouse_click`, minus the scrollbar leg it has
    /// and this does not.
    pub(crate) fn on_click(&mut self, x: u16, y: u16, modifiers: KeyModifiers) {
        // A system modal takes every click while it is up, the mouse half of
        // capturing input: a press that misses its rows is swallowed rather
        // than reaching the pane it covers.
        if self.modals.is_open() {
            let message = self.with_modal_world(|modals, world| modals.on_click(x, y, world));
            if let Some(message) = message {
                self.toast(message);
            }
            self.dirty = true;
            return;
        }

        // A chrome band's button, which is not a pane: pressing one runs its
        // action and leaves focus where it was. v1's footer pills behave the same
        // — you press Help without leaving the terminal you were in.
        if let Some(hit) = self.band_target_at(x, y) {
            match hit.identity.click_verb() {
                // `clicked` is only the fallback owner, and a band has no plugin
                // to fall back to; the action's own declaration is what resolves
                // it, exactly as for a pill drawn by a pane.
                Some(ClickVerb::Action(action)) => {
                    self.run_clicked_action(&action, self.focus);
                    self.dirty = true;
                    return;
                }
                Some(ClickVerb::Url(url)) => {
                    self.open_or_copy_link(&url);
                    self.dirty = true;
                    return;
                }
                _ => {}
            }
        }

        let target = self.target_at(x, y);

        // A float takes every click while it is up — the mouse half of what
        // makes it a modal rather than a pane drawn on top. A press that misses
        // its buttons is swallowed rather than reaching what it covers.
        if let Some(grabbed) = self.grabbed {
            if let Some(target) = target.filter(|target| target.plugin == grabbed) {
                self.dispatch_click(target, x, y);
            }
            return;
        }

        if modifiers.contains(KeyModifiers::CONTROL) {
            // A modified press is a link open, never the start of a selection.
            self.selection = None;
            self.open_clicked_link(x, y);
            return;
        }

        if let Some(target) = target {
            if self.dispatch_click(target, x, y) {
                return;
            }
        }
        self.begin_selection(x, y);
    }

    /// Act on a hit target. `true` means the press is spent.
    ///
    /// A press that only focused a pane returns `false`, so the *same* press
    /// can still arm a drag-selection over the terminal it just focused — v1's
    /// rule, and why `FocusPane(Terminal)` is one of its two non-consuming
    /// click actions.
    pub(crate) fn dispatch_click(&mut self, target: ClickTarget, x: u16, y: u16) -> bool {
        // Every click focuses the pane it landed in, before the target acts. In
        // a `switch` slot that is also how a view is selected, so a tab pill
        // and a Tab press bring the same pane forward.
        self.focus_plugin(target.plugin);

        match target.identity.click_verb() {
            Some(ClickVerb::Action(action)) => {
                self.run_clicked_action(&action, target.plugin);
                true
            }
            // Replayed as a real keystroke, through the very handler the
            // keyboard uses — so a click on a button cannot do something its
            // key does not.
            Some(ClickVerb::Key(chord)) => {
                if let Some(key) = key_event_from_chord(&chord) {
                    self.on_key(&key);
                }
                true
            }
            Some(ClickVerb::Focus(plugin)) => {
                if let Some(index) = self.host.index_of(&plugin) {
                    self.focus_plugin(index);
                }
                true
            }
            // The same opener a `Ctrl+Click` on an agent's link rides, so the
            // two cannot open the same URL in two different places.
            Some(ClickVerb::Url(url)) => {
                self.open_or_copy_link(&url);
                true
            }
            // A pane that paints itself cell by cell (the theme picker, the
            // settings modal) has no per-row nodes to carry identity, so an
            // identity-less hit must still reach it — with coordinates local to
            // its rect, which is all it needs to map y back to a row. Without
            // this the only such panes you could click were the ones built from
            // `widgets.list`, which is exactly the half that worked.
            None => {
                let click = Click {
                    id: target.identity.id.clone(),
                    classes: target.identity.classes.clone(),
                    role: target.identity.role.clone(),
                    x: x.saturating_sub(target.rect.x),
                    y: y.saturating_sub(target.rect.y),
                };
                match self.host.on_click(target.plugin, &click) {
                    Ok(handled) => handled,
                    Err(e) => {
                        self.errors.push(e);
                        false
                    }
                }
            }
        }
    }

    /// Run a declared action, on the plugin that declared it.
    ///
    /// The registry already maps action → owner, so a pill can name an action
    /// belonging to a pane it has never heard of — which is exactly what the
    /// footer does. Falling back to the clicked plugin covers an action a
    /// plugin handles without declaring a key for it.
    pub(crate) fn run_clicked_action(&mut self, action: &str, clicked: usize) {
        // Quit is reserved rather than declared, so it has no binding for the
        // registry lookup below to resolve: the band carries the entry itself
        // and the press lands here. The mouse half of `dispatch_reserved`.
        if action == bands::QUIT_ACTION {
            self.quit = true;
            return;
        }
        // The footer names these by action, as it names a pane's — so a pill
        // reaches a modal the same way its chord does.
        if let Some(kind) = ModalKind::from_action(action) {
            self.toggle_modal(kind);
            return;
        }
        let owner = self
            .registry
            .bindings()
            .iter()
            .find(|binding| binding.action == action)
            .and_then(|binding| self.host.index_of(&binding.plugin))
            .unwrap_or(clicked);
        if let Err(e) = self.host.on_action(owner, action) {
            self.errors.push(e);
        }
    }

    /// The target under a point, innermost and topmost first.
    pub(crate) fn target_at(&self, x: u16, y: u16) -> Option<ClickTarget> {
        let position = ratatui::layout::Position::new(x, y);
        self.click_targets
            .iter()
            .rev()
            .find(|target| target.rect.contains(position))
            .cloned()
    }

    /// The band button under a point, if any.
    ///
    /// Scanned in reverse for the same reason the pane targets are: the last
    /// recorded hit is the topmost, so overlapping entries resolve to the one
    /// actually visible.
    pub(crate) fn band_target_at(&self, x: u16, y: u16) -> Option<thurbox::kernel::bands::Hit> {
        let position = ratatui::layout::Position::new(x, y);
        self.band_targets
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(position))
            .cloned()
    }

    /// Which session's terminal surface a point falls in, and where that
    /// surface was painted.
    pub(crate) fn surface_at(&self, x: u16, y: u16) -> Option<(String, Rect)> {
        let position = ratatui::layout::Position::new(x, y);
        self.snapshots
            .current()
            .sessions
            .iter()
            .filter_map(|row| Some((row.id.clone(), self.terminals.last_rect(&row.id)?)))
            .find(|(_, rect)| rect.contains(position))
    }

    /// The content area of the pane under a point.
    ///
    /// The pane's rect is the click target carrying an EMPTY identity, which the
    /// paint walk records before the tree — so this is the same geometry a click
    /// falls back to. The border test itself is
    /// `PaneBounds::content_at`, where it is unit-tested.
    pub(crate) fn pane_inner_at(&self, x: u16, y: u16) -> Option<Rect> {
        let position = ratatui::layout::Position::new(x, y);
        let pane = self
            .click_targets
            .iter()
            .rev()
            .find(|target| target.identity.is_empty() && target.rect.contains(position))
            .map(|target| target.rect)?;
        PaneBounds::content_at(pane, x, y).map(|bounds| bounds.rect())
    }

    /// Arm a drag-to-select over the terminal surface under the point.
    ///
    /// Confined to that surface's own rect: a drag that leaves the pane clamps
    /// rather than selecting the session list beside it.
    pub(crate) fn begin_selection(&mut self, x: u16, y: u16) {
        // Anywhere, not only over a terminal. v1 selects across the whole
        // interface and decides how to READ it afterwards — the vt100 grid when
        // the selection sits in a terminal, the painted frame otherwise
        // (`apply_selection_highlight`). v2 refused to start one outside a
        // terminal "because it could not be copied from", which was only true
        // while nothing read the frame buffer.
        //
        // But it is confined to ONE PANE, as v1 confines it
        // (`pane_rects` → `border_block.inner`). Falling back to the whole
        // screen is what made it feel trigger-happy: a press in the session list
        // armed a screen-wide selection, so a single cell of pointer drift
        // painted a band clear across the interface instead of a few columns of
        // the list.
        //
        // Anchoring to the surface rect when there is one keeps grid extraction
        // exact: the pane rect is what converts screen coordinates into grid
        // coordinates.
        let rect = match self.surface_at(x, y) {
            Some((_, rect)) => Some(rect),
            None => self.pane_inner_at(x, y),
        };
        let Some(rect) = rect else {
            // Not inside any pane's content — a border, or a gap. v1 clears the
            // selection here rather than starting one.
            self.selection = None;
            return;
        };
        let pane = PaneBounds::from_rect(rect);
        let (x, y) = pane.clamp(x, y);
        self.selection = Some(Selection::new(
            TermPos {
                row: y as usize,
                col: x as usize,
            },
            pane,
        ));
    }

    pub(crate) fn drag_selection(&mut self, x: u16, y: u16) {
        let Some(selection) = &mut self.selection else {
            return;
        };
        let (x, y) = selection.pane.clamp(x, y);
        selection.cursor = TermPos {
            row: y as usize,
            col: x as usize,
        };
    }

    /// Copy the selection to the clipboard.
    ///
    /// Only the selection: there is deliberately no fall-back to the whole
    /// visible screen. That fall-back fired whenever the selection was empty
    /// — which, until a click stopped arming one, was after every click into
    /// a terminal — so `Ctrl+C` in a shell pushed tens of kilobytes of OSC 52
    /// at the outer terminal and never interrupted anything. A pane that
    /// wants the screen copied has `command("copy")` for it.
    pub(crate) fn copy_selection(&mut self) {
        // The selection was read off the frame that painted it, whichever pane
        // that was.
        let message = match self.selected_text.clone() {
            Some(text) if !text.trim().is_empty() => {
                let outcome = thurbox::clipboard::copy(
                    &text,
                    self.clipboard.as_mut(),
                    thurbox::session::settings::global().clipboard.provider,
                );
                match outcome {
                    Ok(route) => format!(
                        "copied {} line(s){}",
                        text.lines().count(),
                        route.toast_suffix()
                    ),
                    Err(e) => format!("copy failed: {e}"),
                }
            }
            _ => "nothing to copy".to_string(),
        };
        self.toast(message);
        self.selection = None;
    }

    /// Open the link under a `Ctrl+Click`, if there is one.
    ///
    /// Silent when there is not: v1 emits no toast for a control-click on plain
    /// text, because the chord is also how you click *through* the terminal.
    ///
    /// A pane's `url:` node is resolved as well as a session's OSC 8 run. The
    /// re-printed escapes already hand the chord to the outer terminal wherever
    /// it understands them, so this leg is what makes the same press work in an
    /// emulator that does not — or on a bare tty.
    pub(crate) fn open_clicked_link(&mut self, x: u16, y: u16) {
        if let Some(url) = self.clicked_node_url(x, y) {
            self.open_or_copy_link(&url);
            return;
        }
        let Some((session, rect)) = self.surface_at(x, y) else {
            return;
        };
        let row = usize::from(y.saturating_sub(rect.y));
        let col = usize::from(x.saturating_sub(rect.x));
        if let Some(url) = self.terminals.url_at(&session, row, col) {
            self.open_or_copy_link(&url);
        }
    }

    /// The link a painted node declares under a point.
    ///
    /// Bands before panes, the order `on_click` resolves a plain press in. But
    /// **every** target under the point is considered, innermost first, rather
    /// than only the topmost one: a `url:` box with a styled child inside it
    /// records the child last, so the topmost-only rule the other verbs follow
    /// would leave the chord finding nothing over cells the paint pass had
    /// already wrapped in OSC 8 — the two legs of one verb disagreeing, with
    /// nothing to see. `Ctrl+Click` is a link gesture and nothing else, so
    /// looking past a node that declares no link costs no other behaviour.
    pub(crate) fn clicked_node_url(&self, x: u16, y: u16) -> Option<String> {
        let position = ratatui::layout::Position::new(x, y);
        let bands = self
            .band_targets
            .iter()
            .rev()
            .map(|hit| (hit.rect, &hit.identity));
        let panes = self
            .click_targets
            .iter()
            .rev()
            .map(|target| (target.rect, &target.identity));
        bands
            .chain(panes)
            .filter(|(rect, _)| rect.contains(position))
            .find_map(|(_, identity)| match identity.click_verb() {
                Some(ClickVerb::Url(url)) => Some(url),
                _ => None,
            })
    }

    /// Open a link, or copy it where nothing can open one.
    ///
    /// v1's `open_ctrl_clicked_url`. A remote host or a bare tty has no
    /// browser, so the clipboard's OSC 52 leg carries the URL back to the
    /// user's own machine instead — the same leg `Ctrl+C` rides — and the toast
    /// says which of the two happened.
    pub(crate) fn open_or_copy_link(&mut self, url: &str) {
        let opened = open_url(url);
        let message = match opened {
            Ok(()) => format!("Opening {url}"),
            Err(reason) => {
                let outcome = thurbox::clipboard::copy(
                    url,
                    self.clipboard.as_mut(),
                    thurbox::session::settings::global().clipboard.provider,
                );
                match outcome {
                    Ok(route) => format!(
                        "{reason} — copied {url} to clipboard{}",
                        route.toast_suffix()
                    ),
                    Err(e) => format!("{reason}, and the clipboard failed: {e}"),
                }
            }
        };
        self.toast(message);
    }

    /// Hand every visible link back to the terminal thurbox runs in.
    ///
    /// The only route to a browser when the agent is on a remote host: the
    /// outer terminal opens the link, so it has to be told the runs are links.
    /// Every attached session is offered rather than only the focused one,
    /// because the paints are validated against the drawn buffer — a session
    /// not painted this frame contributes nothing on its own.
    ///
    /// A pane's [`ClickVerb::Url`] nodes ride the same leg, which is the whole
    /// point of the verb: a plugin hands the kernel cells and can emit no
    /// escape of its own, so this is the only place its content can become a
    /// link the outer terminal knows about.
    pub(crate) fn paint_outer_hyperlinks(&self, buf: &ratatui::buffer::Buffer) {
        let mut paints = Vec::new();
        for row in &self.snapshots.current().sessions {
            paints.extend(self.terminals.hyperlink_paints(&row.id, buf));
        }
        // A band's hit carries no plugin, which is also what makes it unable to
        // be its own float — hence the `Option`.
        let panes = self
            .click_targets
            .iter()
            .map(|target| (Some(target.plugin), target.rect, &target.identity));
        let bands = self
            .band_targets
            .iter()
            .map(|hit| (None, hit.rect, &hit.identity));
        for (plugin, rect, identity) in panes.chain(bands) {
            let Some(ClickVerb::Url(url)) = identity.click_verb() else {
                continue;
            };
            if self.link_paint_obscured(plugin, rect) {
                continue;
            }
            paints.extend(thurbox::kernel::terminal::drawn_link_paints(
                buf, rect, &url,
            ));
        }
        if !paints.is_empty() {
            let _ = thurbox::kernel::terminal::paint_hyperlinks(&paints);
        }
    }

    /// Is something drawn over these cells, so that linking them would link
    /// somebody else's glyphs?
    ///
    /// `hyperlink_paints` gets this for free by matching the glyphs it expects
    /// against the frame — a run the frame no longer prints there drops out. A
    /// pane's node has no label to match (the text is in the plugin's tree, and
    /// wrapping and scroll have moved it since), so what covers it is checked
    /// directly instead: a modal owns the whole screen while it is up, and a
    /// float owns its rect. Without this a modal over a `url:` node would make
    /// the modal's own text `Ctrl+Click` to that url.
    pub(crate) fn link_paint_obscured(&self, plugin: Option<usize>, rect: Rect) -> bool {
        if self.modals.is_open() {
            return true;
        }
        self.drawn_floats.iter().any(|index| {
            Some(*index) != plugin
                && self
                    .last_floats
                    .get(index)
                    .is_some_and(|(float, _)| float.intersects(rect))
        })
    }
}
