//! Painting a frame, and deciding whether one is owed.
//!
//! Demand-driven: a frame happens when something changed or the 250 ms floor
//! elapsed. `draw` compares each plugin's returned tree against the last one and
//! only marks the frame changed when it differs; a float diffs its own tree and
//! rect, and a chrome band — which has no tree — diffs the cells it just
//! painted. Marking a band changed for having been *drawn* held `dirty` set
//! after every frame and stopped the loop settling at all.

use super::*;

impl App {
    /// Demand-driven paint: when something changed, or when the forced-redraw
    /// floor elapsed.
    ///
    /// `draw` diffs each plugin's tree and clears `dirty` when every one matched,
    /// so an idle screen settles at the floor.
    pub(crate) fn paint_if_due(
        &mut self,
        terminal: &mut DefaultTerminal,
    ) -> Result<(), Box<dyn Error>> {
        let since_paint = self.last_paint.elapsed();
        let floor = if self.input_dirty {
            MIN_FRAME_INTERVAL
        } else {
            OUTPUT_FRAME_INTERVAL
        };
        let due = self.dirty && since_paint >= floor;
        if due || since_paint >= FORCE_REDRAW_INTERVAL {
            // Published HERE rather than every iteration: a plugin only reads
            // `thurbox.*` while it renders, so rebuilding those tables on a tick
            // that paints nothing is pure waste. At `drain_input`'s 10ms poll
            // that was 100 rebuilds a second to feed a screen that redraws four
            // times.
            let timing = self.perf_timing_active();
            let republish_start = timing.then(Instant::now);
            self.republish();
            if let Some(start) = republish_start {
                self.timings.republish.record(start.elapsed());
            }
            let draw_start = timing.then(Instant::now);
            let painted = terminal.draw(|frame| self.draw(frame))?;
            if let Some(start) = draw_start {
                self.timings.frame.record(start.elapsed());
            }
            // While `painted` still borrows the terminal — it only needs
            // `&self` and the cells, and this order is what lets the frame
            // buffer be read in place instead of cloned whole (10,000 cells a
            // frame) just to end the borrow. The backend has flushed by the
            // time `draw` returns, so the escapes cannot interleave with
            // ratatui's own output; the cursor corrections below come after,
            // which also leaves the caret where the modal put it.
            self.paint_outer_hyperlinks(painted.buffer);
            // A modal captures input, so it owns the caret — or nothing
            // does. The panes underneath still draw, and one with a text
            // field claims the cursor as it goes; a frame that ends with a
            // cursor position SHOWS it, so it would blink behind the modal,
            // which reads as the screen refreshing wrongly rather than as a
            // misplaced cursor. Corrected after the frame because a `Frame`'s
            // cursor can be set but never unset, and the modal draws last.
            if self.modals.is_open() {
                match self.modals.caret() {
                    Some(position) => {
                        terminal.show_cursor()?;
                        terminal.set_cursor_position(position)?;
                    }
                    None => terminal.hide_cursor()?,
                }
            }
            self.last_paint = Instant::now();
            self.input_dirty = false;
            self.frames += 1;
            Counters::bump(&self.perf.frames);
            if !self.first_frame_logged {
                self.first_frame_logged = true;
                self.startup.first_frame_ms = self.process_start.elapsed().as_millis() as u64;
                if self.perf_log {
                    let s = &self.startup;
                    tracing::info!(
                        config_init_ms = s.config_init_ms,
                        db_open_ms = s.db_open_ms,
                        theme_activate_ms = s.theme_activate_ms,
                        extension_heal_ms = s.extension_heal_ms,
                        heartbeat_ms = s.heartbeat_ms,
                        ui_build_ms = s.ui_build_ms,
                        first_frame_ms = s.first_frame_ms,
                        "startup"
                    );
                }
            }
        } else {
            Counters::bump(&self.perf.skipped);
        }
        Ok(())
    }

    pub(crate) fn draw(&mut self, frame: &mut Frame) {
        self.errors.clear();
        self.focused_session = None;
        self.focused_surface = None;
        self.changed_this_frame = false;
        self.click_targets.clear();
        self.band_targets.clear();
        // Surface rects are recorded while painting, so they are cleared with the
        // other per-frame hit-testing state rather than left to go stale.
        self.terminals.forget_rects();
        let area = frame.area();
        self.last_area = area;

        // 1. The arrangement decides where slots go — before any plugin runs.
        let region = match self.host.arrangement(area.width, area.height) {
            Ok(region) => {
                self.layout_error = None;
                region
            }
            Err(e) => {
                self.layout_error = Some(e);
                // A broken arrangement must not take the plugins with it, so
                // fall back to giving everything to the centre.
                std::rc::Rc::new(thurbox::kernel::layout::Region {
                    slot: Some("center".to_string()),
                    ..Default::default()
                })
            }
        };
        let placed = resolve(&region, area);
        // A reflow owes a full repaint — see `last_placed`. Marked as a change
        // too, so this frame is the one that pays it rather than whichever one
        // the redraw floor gets to next.
        let reflowed = self.last_placed != placed;
        if reflowed {
            self.last_placed = placed.clone();
            self.changed_this_frame = true;
        }
        self.visible_slots = placed.iter().map(|s| s.slot.clone()).collect();
        // A focus request that named a slot this layout has only just placed —
        // the search strip's, which shows itself and asks for focus in one
        // action. Taken here because this is the first moment the slot exists,
        // and before the guard below, which would otherwise read the pane as one
        // focus cannot rest on and walk straight off it.
        self.apply_pending_focus();
        // Closing the column you were standing in must not strand focus on it.
        // Corrected here rather than at key time because the toggle only shows
        // up in the arrangement on the frame AFTER it is flipped.
        //
        // Asked as `can_focus`, not `is_drawn`, and the difference is load-
        // bearing: this runs BEFORE the switch slot below records its selection,
        // so a pane focused by its own opening chord is still an alternate at
        // this point. Judging it drawn would move focus off it every time —
        // which is exactly what made the plugins pane unreachable by `F11`.
        let focusable_now = self.host.focusable();
        if focusable_now
            .get(self.focus)
            .is_some_and(|index| !self.can_focus_plugin(*index))
        {
            self.cycle_focus(1);
        }

        // 2. Each slot divides its rect among its plugins, by their DECLARED
        //    sizes — which is why this can happen before rendering.
        let focusable = self.host.focusable();
        let focused_plugin = focusable.get(self.focus).copied();

        self.draw_slots(frame, &placed, focused_plugin);

        // 2b. Floats, above the arrangement. A plugin only floats on the
        //     frames it returns a float node, so a modal opens and closes with
        //     no separate channel for the kernel to keep in sync.
        self.draw_floats(frame, area);

        // System modals, above the arrangement and above every plugin float —
        // they are not in the layout, so nothing below could shrink them and
        // they shrink nothing. Deliberately NOT marked as a change: a modal's
        // content only moves when a key arrives, and that already marks the
        // screen dirty, so an open modal settles like everything else instead
        // of repainting sixty times a second.
        if self.modals.is_open() {
            // Given the area WITHOUT the chrome bands, for two reasons that are
            // really one: a modal dims everything it is handed
            // (`modals::chrome::modal_frame`), and the bands are the surfaces
            // that must stay readable while it is up. The message band exists so
            // an error is never hidden — dimming it works against its own
            // purpose — and the footer's chips read as buttons only by their
            // fill, so dimming turned every one of them into plain text until
            // the modal closed. Centring inside this area also keeps a tall
            // modal off the bands rather than merely darkening them.
            let content = self.content_area(area, &placed);
            let ui_dir = self.ui_dir.display().to_string();
            let inventory = std::mem::take(&mut self.inventory);
            self.modals.render(
                frame,
                content,
                &self.registry,
                &self.themes,
                self.config.on_disk(),
                thurbox::kernel::modals::interface::Files {
                    rows: &inventory,
                    dir: &ui_dir,
                },
            );
            self.inventory = inventory;
        }

        // The counters, above the floats: a diagnostic a modal could cover
        // would be useless exactly when a modal is what you are diagnosing.
        if self.hud {
            render_hud(frame, hud_area(area), &self.perf.read(), &self.timings);
            // The counters move on every iteration, so the HUD is never
            // settled — while it is up, the loop keeps painting.
            self.changed_this_frame = true;
        }

        // The selection is painted last, over whatever is beneath — it is a
        // highlight on cells that already exist, not a widget of its own.
        // A press that nothing else consumed arms a selection so the *same*
        // press can start a drag — but a press that never moved is a CLICK, and
        // painting it would reverse the one cell under the pointer for no reason
        // the user can see. So an unextended selection is armed but not drawn,
        // and contributes no copyable text.
        if let Some(selection) = self
            .selection
            .clone()
            .filter(|selection| selection.anchor != selection.cursor)
        {
            // v1's `apply_selection_highlight`: the theme's own selection pair,
            // not reverse video. REVERSED inverts whatever each cell already
            // had, so a selection over styled text came out a different colour
            // per span and matched no theme; naming the roles means the
            // selection looks the same everywhere and follows every palette.
            let style = self.themes.selection_style();
            thurbox::kernel::selection::highlight_buffer(frame.buffer_mut(), &selection, style);
            // Deliberately NOT a change, for the reason the system modals below
            // are not: the highlight is already in this frame's buffer, and it is
            // re-applied on every later paint. Moving it takes a mouse event, which
            // marks the screen dirty by itself -- so marking here only kept the
            // loop at the frame cap for as long as anything stayed selected.

            // Read here, not at copy time: outside a terminal the only record of
            // what is selected is the frame we are holding.
            let from_grid = self
                .surface_at(selection.pane.rect().x, selection.pane.rect().y)
                .filter(|(_, rect)| *rect == selection.pane.rect())
                .and_then(|(session, rect)| {
                    self.terminals
                        .selected_text(&session, &selection, (rect.x, rect.y))
                });
            let text = from_grid.unwrap_or_else(|| {
                thurbox::kernel::selection::extract_text_from_buffer(frame.buffer_mut(), &selection)
            });
            self.selected_text = (!text.trim().is_empty()).then_some(text);
        } else {
            self.selected_text = None;
        }

        // An expired message is a change, so it is swept before the settle check
        // below. Where a live one is DRAWN is the message band's business — it
        // used to be painted over the centre pane's bottom border, because a
        // Lua-owned arrangement left no other row spare.
        if self
            .status
            .as_ref()
            .is_some_and(|(_, _, at)| at.elapsed() >= STATUS_TTL)
        {
            self.status = None;
            self.changed_this_frame = true;
        }

        // Settled: nothing moved this frame, so stop repainting until
        // something does or the floor elapses.
        self.dirty = self.changed_this_frame;

        // 3. A reload failure outranks anything a stale-but-working plugin says.
        //    The floor comes last of the three: while it is up the host is the
        //    bundled copy and reports no error of its own, so without it the
        //    interface would silently be something other than what the user
        //    edited.
        if let Some(error) = self
            .host
            .error
            .clone()
            .or_else(|| self.layout_error.clone())
            .or_else(|| self.floor.clone())
        {
            paint::render_error(frame, error_area(area), "reload failed", &error);
        }

        // Last, so all three cover every pane, float and toast painted above,
        // and in this order: the width normalisation changes symbols and the
        // background repaint reads styles, so neither can undo the other, and
        // the reflow's forced print comes after both because it is the finished
        // cells that have to reach the terminal.
        paint::normalize_ambiguous_width(frame.buffer_mut());
        self.repaint_theme_background(frame);
        if reflowed {
            paint::force_full_repaint(frame.buffer_mut());
        }
    }

    /// Step 2: each placed slot divides its rect among its plugins, by their
    /// DECLARED sizes — which is why this can happen before rendering.
    pub(crate) fn draw_slots(
        &mut self,
        frame: &mut Frame,
        placed: &[thurbox::kernel::layout::SlotRect],
        focused_plugin: Option<usize>,
    ) {
        for slot in placed {
            // A band is a slot the KERNEL occupies: the arrangement names and
            // places it exactly as it does a pane's region, and the contents come
            // from here rather than from any plugin. Drawing one runs no Lua, so
            // no plugin can break it.
            if let Some(band) = Band::from_slot(&slot.slot) {
                self.render_band(frame, slot.rect, band);
                continue;
            }
            // Copied out: both draw paths take `&mut self`, which a borrow of
            // the host's index list would forbid — and a slot holds a handful
            // of occupants.
            let members = self.host.in_slot(&slot.slot).to_vec();
            if members.is_empty() {
                continue;
            }
            match self.host.slot_mode(&slot.slot) {
                SlotMode::Switch => self.draw_switch_slot(frame, slot, &members, focused_plugin),
                SlotMode::Stack => self.draw_stack_slot(frame, slot, &members, focused_plugin),
            }
        }
    }

    /// One visible occupant; the rest are alternatives waiting to be selected.
    ///
    /// The focused plugin wins, so tabbing into a pane brings it forward.
    pub(crate) fn draw_switch_slot(
        &mut self,
        frame: &mut Frame,
        slot: &thurbox::kernel::layout::SlotRect,
        members: &[usize],
        focused_plugin: Option<usize>,
    ) {
        let visible = members
            .iter()
            .position(|index| focused_plugin == Some(*index))
            .unwrap_or_else(|| {
                self.slot_selection
                    .get(&slot.slot)
                    .copied()
                    .unwrap_or(0)
                    .min(members.len().saturating_sub(1))
            });
        self.slot_selection.insert(slot.slot.clone(), visible);
        if let Some(&index) = members.get(visible) {
            self.draw_plugin(frame, index, slot.rect, focused_plugin == Some(index));
        }
    }

    /// Every occupant, stacked vertically by declared size.
    pub(crate) fn draw_stack_slot(
        &mut self,
        frame: &mut Frame,
        slot: &thurbox::kernel::layout::SlotRect,
        members: &[usize],
        focused_plugin: Option<usize>,
    ) {
        let sizes: Vec<_> = members.iter().map(|i| self.host.plugins[*i].size).collect();
        let rects = thurbox::kernel::layout::divide_slot(slot.rect, Axis::Vertical, &sizes, 0);
        for (nth, &index) in members.iter().enumerate() {
            let Some(&rect) = rects.get(nth) else {
                continue;
            };
            if rect.width == 0 || rect.height == 0 {
                continue;
            }
            self.draw_plugin(frame, index, rect, focused_plugin == Some(index));
        }
    }

    /// Step 2b: floats, above the arrangement.
    ///
    /// A plugin only floats on the frames it returns a float node, so a modal
    /// opens and closes with no separate channel for the kernel to keep in sync.
    pub(crate) fn draw_floats(&mut self, frame: &mut Frame, area: Rect) {
        self.grabbed = None;
        self.drawn_floats.clear();
        // Copied out: the loop body mutates `self`, which a borrow of the
        // host's index list would forbid — and a float list is a handful of
        // entries.
        for index in self.host.floating().to_vec() {
            let probe = RenderContext {
                width: area.width,
                height: area.height,
                focused: true,
                elapsed: self.started.elapsed().as_secs_f64(),
                frame: self.frames,
            };
            let Ok(rendered) = self.host.render(index, probe) else {
                continue;
            };
            let Some(float) = rendered.float else {
                // Not floating this frame: a closed modal draws nothing at all.
                continue;
            };
            let rect = Self::float_rect(area, float);
            // Dim what is beneath, so the float reads as above rather than
            // merely drawn later.
            frame.render_widget(ratatui::widgets::Clear, rect);
            let mut hits = Vec::new();
            paint::render_recording(frame, rect, &rendered.node, &self.terminals, &mut hits);
            // Recorded after every pane, so a float's targets win the overlap for
            // the same reason its cells do. Its whole rect goes in first — a
            // click that misses every button still lands on the modal rather than
            // on the pane it covers.
            self.push_targets(index, Some(rect), hits);
            // The topmost float takes input; later plugins win, matching the
            // order they are painted in.
            self.grabbed = Some(index);
            // Settled like a pane, by comparing what it drew — not marked changed
            // for simply being open. Unconditional, an open float held `dirty` set
            // forever, so the whole interface rebuilt every Lua tree at the frame
            // cap for as long as the creation wizard was up. A float that really
            // does animate still repaints: the 250 ms floor paints it, its tree
            // differs, and that marks the change.
            let unchanged = self.last_floats.get(&index).is_some_and(|(last, node)| {
                // Pointer identity first: a pure-cache hit hands back the same
                // `Rc`, which settles the question without walking the tree.
                *last == rect
                    && (std::rc::Rc::ptr_eq(node, &rendered.node) || **node == *rendered.node)
            });
            if !unchanged {
                self.changed_this_frame = true;
                // Stored only on change: on the unchanged branch the held tree
                // is already equal, and re-storing was a second tree-sized
                // clone per float per frame (now a refcount bump either way).
                self.last_floats
                    .insert(index, (rect, std::rc::Rc::clone(&rendered.node)));
            }
            self.drawn_floats.insert(index);
        }
    }

    pub(crate) fn draw_plugin(
        &mut self,
        frame: &mut Frame,
        index: usize,
        rect: Rect,
        focused: bool,
    ) {
        let ctx = RenderContext {
            width: rect.width,
            height: rect.height,
            focused,
            elapsed: self.started.elapsed().as_secs_f64(),
            frame: self.frames,
        };
        Counters::bump(&self.perf.renders);
        let rendered = match self.host.render(index, ctx) {
            Ok(rendered) => rendered,
            Err(e) => {
                paint::render_error(frame, rect, &e.plugin, &e.message);
                Counters::bump(&self.perf.failures);
                self.errors.push(e);
                // The pane's own rect, though it drew no rows to record. A
                // press matching no target at all falls through to
                // `begin_selection`, so without this a throwing plugin costs
                // not just its pane but the pointer over it — clicks there
                // silently paint a text selection across the error panel
                // instead of focusing it. Isolation is the rule everywhere
                // else here; this is its mouse half.
                let fallback = self.host.plugins[index].focusable.then_some(rect);
                self.push_targets(index, fallback, Vec::new());
                return;
            }
        };
        // A plugin that floats is drawn above the arrangement instead, so it
        // takes no room in its slot.
        if rendered.float.is_some() {
            return;
        }
        if focused {
            // The session, for everything that is about sessions — the focus
            // label, the info the bands show, `Ctrl+O`.
            self.focused_session = rendered.node.first_session_surface().map(str::to_string);
            // And whatever this pane is actually showing, which is where raw
            // input goes. `input = "session"` never meant "the selected
            // session"; it meant "what this pane is showing", and a plugin's own
            // program is now one of the things that can be.
            self.focused_surface = rendered.node.first_live_surface().map(str::to_string);
        }

        // Decoration is the rare case, so the undecorated tree rides the
        // render's own `Rc` — a decorator pays one deep clone, everyone else
        // pays a refcount bump.
        let node = match self.decorate_tree(index, &rendered.node, ctx) {
            Some(decorated) => std::rc::Rc::new(decorated),
            None => std::rc::Rc::clone(&rendered.node),
        };
        if self.last_trees.len() <= index {
            self.last_trees.resize(index + 1, None);
        }
        // Stamped unconditionally, before the tree comparison can short-circuit
        // it: skipping the stamp on a frame whose tree changed would make the
        // *next* frame see output that had already been painted.
        let surface_moved = self.surface_moved(&node);
        // Pointer identity first: a pure-cache hit hands back the same `Rc`,
        // which settles the question without a per-node walk.
        let tree_unchanged = self.last_trees[index]
            .as_ref()
            .is_some_and(|last| std::rc::Rc::ptr_eq(last, &node) || **last == *node);
        let unchanged = tree_unchanged && !surface_moved;
        if !unchanged {
            self.changed_this_frame = true;
        }
        if !tree_unchanged {
            // On the unchanged branch the held tree is already equal, and
            // re-storing it was a second tree-sized clone per pane per frame.
            self.last_trees[index] = Some(std::rc::Rc::clone(&node));
        }
        let mut hits = Vec::new();
        paint::render_recording(frame, rect, &node, &self.terminals, &mut hits);
        // The pane's own rect is only a target when focus can rest on it. A
        // footer click must reach the pill it landed on and nothing else — v1
        // likewise records no `FocusPane` for panes that cannot hold focus.
        let fallback = self.host.plugins[index].focusable.then_some(rect);
        self.push_targets(index, fallback, hits);
    }

    /// Let every decorator of this plugin's slot restyle its tree.
    ///
    /// A decorator that fails costs its decoration, not the pane — so the
    /// original is what gets drawn. `None` means no decorator claims the slot,
    /// which is nearly every pane on nearly every frame: the caller keeps the
    /// shared tree instead of paying a clone for a restyle that never runs.
    pub(crate) fn decorate_tree(
        &mut self,
        index: usize,
        node: &thurbox::kernel::node::Node,
        ctx: RenderContext,
    ) -> Option<thurbox::kernel::node::Node> {
        let slot = &self.host.plugins[index].slot;
        let decorators = self.host.decorators_of(slot);
        if decorators.is_empty() {
            return None;
        }
        // Copied out: `decorate` and the error sink both need `self` again.
        let decorators = decorators.to_vec();
        let mut node = node.clone();
        for decorator in decorators {
            match self.host.decorate(decorator, &node, ctx) {
                Ok(decorated) => node = decorated,
                Err(e) => self.errors.push(e),
            }
        }
        Some(node)
    }

    /// Render one plugin, painting an error panel in ITS OWN rect on failure.
    ///
    /// This is the isolation rule made concrete: a plugin that throws costs its
    /// own pane and nothing else. Its neighbours keep drawing, and its state
    /// survives for when the file is fixed.
    /// Centre a rect taking `width_pct` x `height_pct` of `area`.
    pub(crate) fn float_rect(area: Rect, float: thurbox::kernel::host::Float) -> Rect {
        // Cells when the plugin knows them, else a share of the screen. A modal
        // whose height follows its content — v1's pickers, all of them — can only
        // say so in cells; clamping keeps an over-ambitious one on screen.
        let width = clamp_span(
            float
                .cols
                .unwrap_or_else(|| (f64::from(area.width) * float.width_pct / 100.0) as u16),
            4,
            area.width,
        );
        let height = clamp_span(
            float
                .rows
                .unwrap_or_else(|| (f64::from(area.height) * float.height_pct / 100.0) as u16),
            3,
            area.height,
        );
        Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        }
    }

    /// Repaint cells that fell back to terminal-default colours with the active
    /// theme's background and primary text.
    ///
    /// v1 `App::repaint_theme_background`. Without it a theme only tints what a
    /// pane explicitly styled, and every gap between panes keeps the user's own
    /// terminal background — so 30 of the 36 presets rendered as a patchwork.
    ///
    /// Themes whose `app_bg` is `Reset` (the ANSI-based Default preset) skip it
    /// deliberately, so they keep honouring the terminal palette.
    pub(crate) fn repaint_theme_background(&self, frame: &mut Frame) {
        let palette = &self.themes.active().palette;
        if palette.app_bg == ratatui::style::Color::Reset {
            return;
        }
        // The buffer's storage is its area exactly, row-major, so walking it
        // directly visits the same cells the coordinate loop did — minus a
        // bounds-checked `cell_mut` per cell, on a pass that touches every
        // cell of every painted frame (the same cut `normalize_ambiguous_width`
        // took).
        for cell in frame.buffer_mut().content.iter_mut() {
            if cell.bg == ratatui::style::Color::Reset {
                cell.bg = palette.app_bg;
            }
            if cell.fg == ratatui::style::Color::Reset {
                cell.fg = palette.text_primary;
            }
        }
    }

    /// Draw one chrome band into the rect the arrangement gave it.
    ///
    /// Every value is read from state the kernel already holds — the version, the
    /// theme, the counts, the focused surface, the message — which is what makes
    /// "a band cannot be broken by a plugin" true rather than aspirational.
    pub(crate) fn render_band(&mut self, frame: &mut Frame, rect: Rect, band: Band) {
        let snapshot = self.snapshots.current();
        let selected = self.host.shared_string("selected");
        let session = selected
            .as_deref()
            .and_then(|id| snapshot.session(id))
            .map(|row| row.name.clone());
        // The VIEW if the focused pane is showing one, else the pane itself —
        // `focused_session` carries `<id>#shell` while the shell tab is up, and
        // the centre pane paints before the footer band, so this is already
        // resolved by the time the band draws.
        let focused_plugin = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.plugins.get(*index))
            .map(|plugin| plugin.name.clone())
            .unwrap_or_default();
        let focus_label = bands::focus_label(self.focused_session.as_deref(), &focused_plugin);
        // Work in flight outranks a toast while it runs: creation phases
        // routinely outlast the retention window, which is why they are progress
        // rather than a message. `first_running` rather than the whole list,
        // because progress outranking a message makes a *failed* entry hide the
        // error explaining it — see [`CommandBus::first_running`].
        let progress = self
            .commands
            .first_running()
            .map(|item| match &item.subject {
                Some(subject) => format!("{} {subject}…", item.kind),
                None => format!("{}…", item.kind),
            });
        let message = self
            .status
            .as_ref()
            .filter(|(_, _, at)| at.elapsed() < STATUS_TTL)
            .map(|(text, level, _)| (text.as_str(), *level));

        let state = BandState {
            version: env!("THURBOX_VERSION"),
            theme_label: &self.themes.active().display_name,
            update_available: self.updates.available(),
            session: session.as_deref(),
            session_count: snapshot.sessions.len(),
            automation_count: snapshot.automations.len(),
            focus_label: &focus_label,
            message,
            progress: progress.as_deref(),
            hovered: self.hovered.as_ref(),
            registry: &self.registry,
            themes: &self.themes,
        };
        if !bands::occupies(band, &state) {
            // A band that occupied the last frame and does not occupy this one
            // HAS changed the frame — the message band finishing is the case —
            // so forgetting it is itself the signal.
            if self.last_bands.remove(&band).is_some() {
                self.changed_this_frame = true;
            }
            return;
        }
        let hits = bands::render(frame, rect, band, &state);
        self.band_targets.extend(hits);
        // A band is chrome the kernel paints directly, so there is no tree to
        // diff the way a plugin's is — and marking it changed for having been
        // *drawn* is not the same question. It held `dirty` set after every
        // single frame, which defeated the demand-driven redraw entirely: the
        // loop never settled to the 250ms floor and repainted at the frame cap
        // forever, on an idle screen with no sessions at all.
        //
        // The cells it just painted are the honest analog of a plugin's tree —
        // exact, and immune to a new `BandState` field being forgotten here.
        let painted = read_cells(frame, rect);
        let entry = (rect, painted);
        if self.last_bands.get(&band) != Some(&entry) {
            self.changed_this_frame = true;
            self.last_bands.insert(band, entry);
        }
    }

    /// `area` minus the rows the chrome bands were placed on.
    ///
    /// Bands are only ever full-width single rows at the top or the bottom, so
    /// this is a shrink rather than a general subtraction: a band placed between
    /// two panes would not be excluded, and nothing places one there.
    pub(crate) fn content_area(
        &self,
        area: Rect,
        placed: &[thurbox::kernel::layout::SlotRect],
    ) -> Rect {
        let mut top = area.y;
        let mut bottom = area.y.saturating_add(area.height);
        for slot in placed {
            if Band::from_slot(&slot.slot).is_none() {
                continue;
            }
            let rect = slot.rect;
            if rect.y <= top {
                top = top.max(rect.y.saturating_add(rect.height));
            } else if rect.y.saturating_add(rect.height) >= bottom {
                bottom = bottom.min(rect.y);
            }
        }
        Rect {
            x: area.x,
            y: top,
            width: area.width,
            height: bottom.saturating_sub(top),
        }
    }

    /// Rows the message band needs: one while there is a live message or work in
    /// flight, none otherwise.
    pub(crate) fn status_rows(&self) -> u16 {
        let live_message = self
            .status
            .as_ref()
            .is_some_and(|(_, _, at)| at.elapsed() < STATUS_TTL);
        u16::from(live_message || self.commands.has_inflight())
    }

    /// Whether this tree's session surface has printed since it was last
    /// painted.
    ///
    /// A surface's cells live OUTSIDE the tree, so tree equality cannot tell
    /// whether it changed. Its own output stamp can: a pane whose agent has said
    /// nothing since the last paint is as settled as a pane of text, which is
    /// what lets a screen with a live terminal on it idle at the redraw floor
    /// instead of repainting at the frame cap forever. v1 gates the same way, off
    /// the same atomic (`detect_output_redraw`).
    pub(crate) fn surface_moved(&mut self, node: &thurbox::kernel::node::Node) -> bool {
        let Some(surface) = node.first_session_surface() else {
            return false;
        };
        let stamp = self.terminals.output_stamp(surface);
        let previous = self.last_output_painted.get(surface).copied();
        if let Some(stamp) = stamp {
            self.last_output_painted.insert(surface.to_string(), stamp);
        }
        // An unattached surface has no stamp and nothing to show, so it is
        // settled rather than perpetually new.
        stamp.is_some() && previous != stamp
    }

    /// Record one plugin's hitboxes, its own rect first.
    ///
    /// Order is the whole contract: the pane fallback goes in before the tree
    /// so that scanning in reverse finds an identified node first and the
    /// fallback only when nothing inside matched. v1 spells the same rule the
    /// other way round — specific targets recorded first, first match wins.
    pub(crate) fn push_targets(
        &mut self,
        plugin: usize,
        pane: Option<Rect>,
        hits: Vec<paint::Hit>,
    ) {
        if let Some(rect) = pane {
            self.click_targets.push(ClickTarget {
                plugin,
                rect,
                identity: Identity::default(),
            });
        }
        self.click_targets
            .extend(hits.into_iter().map(|hit| ClickTarget {
                plugin,
                rect: hit.rect,
                identity: hit.identity,
            }));
    }
}
