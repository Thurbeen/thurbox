use std::error::Error;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::{DefaultTerminal, Frame};

use thurbox::kernel::bands::{Band, BandState, Level};
use thurbox::kernel::host::{ModError, RenderContext, Rendered};
use thurbox::kernel::layout::{resolve, SlotMode};
use thurbox::kernel::node::{Axis, Identity};
use thurbox::kernel::perf::Counters;
use thurbox::kernel::snapshot::FleetState;
use thurbox::kernel::{bands, paint};

use super::{
    clamp_span, error_area, hud_area, mod_hud_area, read_cells, render_hud, render_mod_hud,
};
use crate::{
    App, ClickTarget, FAILURE_HOLD, FORCE_REDRAW_INTERVAL, KEEP_FRAME_WHILE_TYPING,
    MIN_FRAME_INTERVAL, OUTPUT_FRAME_INTERVAL, QUIESCENT_AFTER, STATUS_TTL,
};

impl App {
    pub(crate) fn paint_if_due(
        &mut self,
        terminal: &mut DefaultTerminal,
    ) -> Result<(), Box<dyn Error>> {
        let since_paint = self.last_paint.elapsed();
        let floor = if self.echo_due.is_some() {
            Duration::ZERO
        } else if self.input_dirty {
            MIN_FRAME_INTERVAL
        } else {
            OUTPUT_FRAME_INTERVAL
        };
        let held = self.echo_due.is_none()
            && self
                .echo
                .front()
                .is_some_and(|echo| Instant::now() < echo.hold_until());
        let due = self.dirty && since_paint >= floor && !held;
        if !due && since_paint < FORCE_REDRAW_INTERVAL {
            Counters::bump(&self.perf.skipped);
            return Ok(());
        }
        if let Some(surface) = self.echo_due.take() {
            Counters::bump(&self.perf.echoes);
            if self.paint_echo_frame(terminal, &surface)? {
                Counters::bump(&self.perf.echo_frames);
                return Ok(());
            }
        }
        let timing = self.perf_timing_active();
        if timing {
            self.host
                .set_idle(self.last_activity.elapsed() >= QUIESCENT_AFTER);
        }
        let republish_start = timing.then(Instant::now);
        self.republish();
        if let Some(start) = republish_start {
            self.timings.republish.record(start.elapsed());
        }
        let draw_start = timing.then(Instant::now);
        let placed_before = self.last_placed.clone();
        let painted = terminal.draw(|frame| self.draw(frame))?;
        self.host.end_files_frame();
        if let Some(start) = draw_start {
            let took = start.elapsed();
            self.timings.frame.record(took);
            self.host.note_frame(took);
        }
        let typing = self
            .last_keystroke
            .is_some_and(|at| at.elapsed() < KEEP_FRAME_WHILE_TYPING);
        let reusable = typing && self.last_placed == placed_before && !self.covered();
        match &mut self.last_frame {
            Some(kept) if reusable && kept.area == painted.buffer.area => {
                kept.content.clone_from_slice(&painted.buffer.content);
            }
            _ if reusable => self.last_frame = Some(painted.buffer.clone()),
            _ => self.last_frame = None,
        }
        self.paint_outer_hyperlinks(painted.buffer);
        self.place_modal_caret(terminal)?;
        self.last_paint = Instant::now();
        self.input_dirty = false;
        self.frames += 1;
        Counters::bump(&self.perf.frames);
        self.log_first_frame();
        if std::mem::take(&mut self.copy_after_paint) {
            self.copy_on_select();
        }
        Ok(())
    }

    fn covered(&self) -> bool {
        self.modals.is_open()
            || !self.drawn_floats.is_empty()
            || self.selection.is_some()
            || self.hud
            || !self.errors.is_empty()
            || self.host.error.is_some()
            || self.layout_error.is_some()
            || self.floor.is_some()
    }

    fn paint_echo_frame(
        &mut self,
        terminal: &mut DefaultTerminal,
        surface: &str,
    ) -> Result<bool, Box<dyn Error>> {
        if self.covered() || self.terminals.program_key(surface).is_some() {
            return Ok(false);
        }
        let Some(rect) = self.terminals.last_rect(surface) else {
            return Ok(false);
        };
        let shown = self
            .last_trees
            .iter()
            .flatten()
            .find_map(|tree| tree.session_surface(surface));
        let Some((0, None)) = shown else {
            return Ok(false);
        };
        let size = terminal.size()?;
        let Some(kept) = self
            .last_frame
            .as_ref()
            .filter(|kept| kept.area == Rect::new(0, 0, size.width, size.height))
        else {
            return Ok(false);
        };
        let painted = terminal.draw(|frame| {
            frame.buffer_mut().content.clone_from_slice(&kept.content);
            let _ = thurbox::kernel::paint::SurfaceProvider::render_session(
                &self.terminals,
                frame,
                rect,
                surface,
                0,
            );
            paint::normalize_ambiguous_width(frame.buffer_mut());
            self.repaint_theme_background(frame);
        })?;
        if let Some(kept) = self.last_frame.as_mut() {
            kept.content.clone_from_slice(&painted.buffer.content);
        }
        self.paint_outer_hyperlinks(painted.buffer);
        self.last_paint = Instant::now();
        self.frames += 1;
        Counters::bump(&self.perf.frames);
        Ok(true)
    }

    // A ratatui `Frame`'s cursor can be set but never unset, so the caret is fixed after the frame.
    fn place_modal_caret(&mut self, terminal: &mut DefaultTerminal) -> Result<(), Box<dyn Error>> {
        if !self.modals.is_open() {
            return Ok(());
        }
        match self.modals.caret() {
            Some(position) => {
                terminal.show_cursor()?;
                terminal.set_cursor_position(position)?;
            }
            None => terminal.hide_cursor()?,
        }
        Ok(())
    }

    fn log_first_frame(&mut self) {
        if self.first_frame_logged {
            return;
        }
        self.first_frame_logged = true;
        self.startup.first_frame_ms = self.process_start.elapsed().as_millis() as u64;
        if self.perf_log {
            let s = &self.startup;
            tracing::info!(
                config_init_ms = s.config_init_ms,
                snapshot_open_ms = s.snapshot_open_ms,
                theme_activate_ms = s.theme_activate_ms,
                ui_build_ms = s.ui_build_ms,
                first_frame_ms = s.first_frame_ms,
                "startup"
            );
        }
    }

    pub(crate) fn draw(&mut self, frame: &mut Frame) {
        self.errors.clear();
        self.focused_session = None;
        self.focused_surface = None;
        self.focused_typing = false;
        self.changed_this_frame = false;
        self.click_targets.clear();
        self.band_targets.clear();
        self.terminals.forget_rects();
        let area = frame.area();
        self.last_area = area;
        let mod_report = self.hud.then(|| self.host.mod_report());

        let region = match self.host.arrangement(area.width, area.height) {
            Ok(region) => {
                self.layout_error = None;
                region
            }
            Err(e) => {
                self.layout_error = Some(e);
                std::rc::Rc::new(thurbox::kernel::layout::Region {
                    slot: Some("center".to_string()),
                    ..Default::default()
                })
            }
        };
        let placed = resolve(&region, area);
        let reflowed = self.last_placed != placed;
        if reflowed {
            self.last_placed = placed.clone();
            self.changed_this_frame = true;
            // A reprinted cell loses the hyperlink the terminal attached to it.
            self.last_link_paints.clear();
        }
        self.visible_slots = placed.iter().map(|s| s.slot.clone()).collect();
        self.apply_pending_focus();
        let focusable_now = self.host.focusable();
        if focusable_now
            .get(self.focus)
            .is_some_and(|index| !self.can_focus_mod(*index))
        {
            self.cycle_focus(1);
        }

        let focusable = self.host.focusable();
        let focused_mod = focusable.get(self.focus).copied();

        self.draw_slots(frame, &placed, focused_mod);

        self.draw_floats(frame, area);
        self.diffs_shown = self.host.take_diffs_read();

        if self.modals.is_open() {
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

        if self.hud {
            let hud = hud_area(area);
            render_hud(frame, hud, &self.perf.read(), &self.timings);
            if let Some(report) = &mod_report {
                render_mod_hud(frame, mod_hud_area(area, hud, report.rows.len()), report);
            }
            self.changed_this_frame = true;
        }

        if let Some(selection) = self
            .selection
            .clone()
            .filter(|selection| selection.anchor != selection.cursor)
        {
            let style = self.themes.selection_style();
            thurbox::kernel::selection::highlight_buffer(frame.buffer_mut(), &selection, style);

            let from_grid = self.grid_selection_text(&selection);
            let text = from_grid.unwrap_or_else(|| {
                thurbox::kernel::selection::extract_text_from_buffer(frame.buffer_mut(), &selection)
            });
            self.selected_text = (!text.trim().is_empty()).then_some(text);
        } else {
            self.selected_text = None;
        }

        if self
            .status
            .as_ref()
            .is_some_and(|(_, _, at)| at.elapsed() >= STATUS_TTL)
        {
            self.status = None;
            self.changed_this_frame = true;
        }
        if self
            .startup_notice_due
            .is_some_and(|due| Instant::now() >= due)
        {
            let active_error = self.status.as_ref().and_then(|(_, level, at)| {
                (*level == Level::Error && at.elapsed() < STATUS_TTL).then_some(*at + STATUS_TTL)
            });
            if let Some(until) = active_error {
                self.startup_notice_due = Some(until);
            } else {
                if let Some(notice) = self.startup_notices.pop_front() {
                    self.toast(notice);
                }
                self.startup_notice_due =
                    (!self.startup_notices.is_empty()).then(|| Instant::now() + STATUS_TTL);
                self.changed_this_frame = true;
            }
        }

        self.dirty = self.changed_this_frame;

        if let Some(error) = self
            .host
            .error
            .clone()
            .or_else(|| self.layout_error.clone())
            .or_else(|| self.floor.clone())
            .filter(|_| !self.modals.is_open())
        {
            paint::render_error(frame, error_area(area), "reload failed", &error);
        }

        paint::normalize_ambiguous_width(frame.buffer_mut());
        self.repaint_theme_background(frame);
        if reflowed {
            paint::force_full_repaint(frame.buffer_mut());
        }
    }

    pub(crate) fn draw_slots(
        &mut self,
        frame: &mut Frame,
        placed: &[thurbox::kernel::layout::SlotRect],
        focused_mod: Option<usize>,
    ) {
        for slot in placed {
            if let Some(band) = Band::from_slot(&slot.slot) {
                self.render_band(frame, slot.rect, band);
                continue;
            }
            let members = self.host.in_slot(&slot.slot).to_vec();
            if members.is_empty() {
                continue;
            }
            match self.host.slot_mode(&slot.slot) {
                SlotMode::Switch => self.draw_switch_slot(frame, slot, &members, focused_mod),
                SlotMode::Stack => self.draw_stack_slot(frame, slot, &members, focused_mod),
            }
        }
    }

    pub(crate) fn draw_switch_slot(
        &mut self,
        frame: &mut Frame,
        slot: &thurbox::kernel::layout::SlotRect,
        members: &[usize],
        focused_mod: Option<usize>,
    ) {
        let visible = members
            .iter()
            .position(|index| focused_mod == Some(*index))
            .unwrap_or_else(|| {
                self.slot_selection
                    .get(&slot.slot)
                    .copied()
                    .unwrap_or(0)
                    .min(members.len().saturating_sub(1))
            });
        self.slot_selection.insert(slot.slot.clone(), visible);
        if let Some(&index) = members.get(visible) {
            self.draw_mod(frame, index, slot.rect, focused_mod == Some(index));
        }
    }

    pub(crate) fn draw_stack_slot(
        &mut self,
        frame: &mut Frame,
        slot: &thurbox::kernel::layout::SlotRect,
        members: &[usize],
        focused_mod: Option<usize>,
    ) {
        let sizes: Vec<_> = members.iter().map(|i| self.host.mods[*i].size).collect();
        let rects = thurbox::kernel::layout::divide_slot(slot.rect, Axis::Vertical, &sizes, 0);
        for (nth, &index) in members.iter().enumerate() {
            let Some(&rect) = rects.get(nth) else {
                continue;
            };
            if rect.width == 0 || rect.height == 0 {
                continue;
            }
            self.draw_mod(frame, index, rect, focused_mod == Some(index));
        }
    }

    pub(crate) fn draw_floats(&mut self, frame: &mut Frame, area: Rect) {
        self.grabbed = None;
        self.drawn_floats.clear();
        for index in self.host.floating().to_vec() {
            let probe = RenderContext {
                width: area.width,
                height: area.height,
                focused: true,
                elapsed: self.started.elapsed().as_secs_f64(),
                frame: self.frames,
            };
            let rendered = self.host.render(index, probe);
            let Ok(rendered) = self.hold_failure(index, rendered) else {
                continue;
            };
            let Some(float) = rendered.float else {
                continue;
            };
            let rect = Self::float_rect(area, float);
            frame.render_widget(ratatui::widgets::Clear, rect);
            let mut hits = Vec::new();
            let surfaces = self.terminals.cursor_on(None);
            paint::render_recording(frame, rect, &rendered.node, &surfaces, &mut hits);
            self.push_targets(index, Some(rect), hits);
            self.grabbed = Some(index);
            let unchanged = self.last_floats.get(&index).is_some_and(|(last, node)| {
                *last == rect
                    && (std::rc::Rc::ptr_eq(node, &rendered.node) || **node == *rendered.node)
            });
            if !unchanged {
                self.changed_this_frame = true;
                self.last_floats
                    .insert(index, (rect, std::rc::Rc::clone(&rendered.node)));
            }
            self.drawn_floats.insert(index);
        }
    }

    pub(crate) fn draw_mod(&mut self, frame: &mut Frame, index: usize, rect: Rect, focused: bool) {
        let ctx = RenderContext {
            width: rect.width,
            height: rect.height,
            focused,
            elapsed: self.started.elapsed().as_secs_f64(),
            frame: self.frames,
        };
        Counters::bump(&self.perf.renders);
        let rendered = self.host.render(index, ctx);
        if rendered.is_err() {
            Counters::bump(&self.perf.failures);
        }
        let rendered = match self.hold_failure(index, rendered) {
            Ok(rendered) => rendered,
            Err(e) => {
                paint::render_error(frame, rect, &e.owner, &e.message);
                self.errors.push(e);
                let fallback = self.host.mods[index].focusable.then_some(rect);
                self.push_targets(index, fallback, Vec::new());
                return;
            }
        };
        if rendered.float.is_some() {
            return;
        }
        if focused {
            self.focused_session = rendered.node.first_session_surface().map(str::to_string);
            self.focused_surface = rendered.node.first_live_surface().map(str::to_string);
            self.focused_typing = rendered.node.has_focused_input();
        }

        let node = match self.decorate_tree(index, &rendered.node, ctx) {
            Some(decorated) => std::rc::Rc::new(decorated),
            None => std::rc::Rc::clone(&rendered.node),
        };
        if self.last_trees.len() <= index {
            self.last_trees.resize(index + 1, None);
        }
        let surface_moved = self.surface_moved(&node);
        let tree_unchanged = self.last_trees[index]
            .as_ref()
            .is_some_and(|last| std::rc::Rc::ptr_eq(last, &node) || **last == *node);
        let unchanged = tree_unchanged && !surface_moved;
        if !unchanged {
            self.changed_this_frame = true;
        }
        if !tree_unchanged {
            self.last_trees[index] = Some(std::rc::Rc::clone(&node));
        }
        let mut hits = Vec::new();
        let input = if focused {
            node.first_live_surface()
        } else {
            None
        };
        let surfaces = self.terminals.cursor_on(input);
        paint::render_recording(frame, rect, &node, &surfaces, &mut hits);
        let fallback = self.host.mods[index].focusable.then_some(rect);
        self.push_targets(index, fallback, hits);
    }

    pub(crate) fn hold_failure(
        &mut self,
        index: usize,
        rendered: Result<Rendered, ModError>,
    ) -> Result<Rendered, ModError> {
        match rendered {
            Err(e) => {
                let started = self
                    .failing
                    .insert(index, (e.clone(), Instant::now()))
                    .is_none();
                if started {
                    self.changed_this_frame = true;
                    if self
                        .host
                        .mods
                        .get(index)
                        .is_some_and(|loaded| loaded.floats)
                    {
                        let first = e.message.lines().next().unwrap_or_default().trim();
                        self.report(format!("{} failed: {first}", e.owner), Level::Error);
                    }
                }
                Err(e)
            }
            Ok(rendered) => match self.failing.get(&index) {
                Some((e, at)) if at.elapsed() < FAILURE_HOLD => Err(e.clone()),
                Some(_) => {
                    self.failing.remove(&index);
                    self.changed_this_frame = true;
                    Ok(rendered)
                }
                None => Ok(rendered),
            },
        }
    }

    pub(crate) fn decorate_tree(
        &mut self,
        index: usize,
        node: &thurbox::kernel::node::Node,
        ctx: RenderContext,
    ) -> Option<thurbox::kernel::node::Node> {
        let slot = &self.host.mods[index].slot;
        let decorators = self.host.decorators_of(slot);
        if decorators.is_empty() {
            return None;
        }
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

    pub(crate) fn float_rect(area: Rect, float: thurbox::kernel::host::Float) -> Rect {
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
        let (x, y) = match float.at {
            None => (
                area.x + (area.width - width) / 2,
                area.y + (area.height - height) / 2,
            ),
            Some((x, y)) => (
                anchor_span(x, width, area.x, area.width),
                anchor_span(y, height, area.y, area.height),
            ),
        };
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub(crate) fn repaint_theme_background(&self, frame: &mut Frame) {
        // Under NO_COLOR crossterm still emits an empty SetColors, which resets attributes.
        if crossterm::style::Colored::ansi_color_disabled_memoized() {
            for cell in &mut frame.buffer_mut().content {
                cell.fg = ratatui::style::Color::Reset;
                cell.bg = ratatui::style::Color::Reset;
            }
            return;
        }
        let palette = &self.themes.active().palette;
        if palette.app_bg == ratatui::style::Color::Reset {
            return;
        }
        for cell in frame.buffer_mut().content.iter_mut() {
            if cell.bg == ratatui::style::Color::Reset {
                cell.bg = palette.app_bg;
            }
            if cell.fg == ratatui::style::Color::Reset {
                cell.fg = palette.text_primary;
            }
        }
    }

    pub(crate) fn render_band(&mut self, frame: &mut Frame, rect: Rect, band: Band) {
        let snapshot = self.snapshots.current();
        let selected = self.host.shared_string("selected");
        let session = selected
            .as_deref()
            .and_then(|id| snapshot.session(id))
            .map(|row| row.name.clone());
        let focused_mod = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.mods.get(*index))
            .map(|loaded| loaded.name.clone())
            .unwrap_or_default();
        let focus_label = bands::focus_label(self.focused_session.as_deref(), &focused_mod);
        let progress = self
            .commands
            .first_running()
            .map(|item| match &item.subject {
                Some(subject) => format!("{} {subject}…", item.kind),
                None => format!("{}…", item.kind),
            });
        let fleet_down_notice = self.fleet_down_notice();
        let message = self
            .status
            .as_ref()
            .filter(|(_, _, at)| at.elapsed() < STATUS_TTL)
            .map(|(text, level, _)| (text.as_str(), *level))
            .or(fleet_down_notice.as_deref().map(|why| (why, Level::Error)));

        let state = BandState {
            version: env!("THURBOX_VERSION"),
            theme_label: &self.themes.active().display_name,
            update_available: self.updates.available(),
            session: session.as_deref(),
            session_count: snapshot.sessions.len(),
            focus_label: &focus_label,
            message,
            progress: progress.as_deref(),
            hovered: self.hovered.as_ref(),
            registry: &self.registry,
            themes: &self.themes,
        };
        if !bands::occupies(band, &state) {
            if self.last_bands.remove(&band).is_some() {
                self.changed_this_frame = true;
            }
            return;
        }
        let hits = bands::render(frame, rect, band, &state);
        self.band_targets.extend(hits);
        let painted = read_cells(frame, rect);
        let entry = (rect, painted);
        if self.last_bands.get(&band) != Some(&entry) {
            self.changed_this_frame = true;
            self.last_bands.insert(band, entry);
        }
    }

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

    pub(crate) fn status_rows(&self) -> u16 {
        let live_message = self
            .status
            .as_ref()
            .is_some_and(|(_, _, at)| at.elapsed() < STATUS_TTL);
        u16::from(
            live_message || self.commands.has_inflight() || self.fleet_down_notice().is_some(),
        )
    }

    pub(crate) fn fleet_down_notice(&self) -> Option<String> {
        self.snapshots
            .current()
            .fleets
            .iter()
            .find(|fleet| fleet.state == FleetState::Down && !fleet.remote)
            .and_then(|fleet| fleet.why.clone())
    }

    pub(crate) fn surface_moved(&mut self, node: &thurbox::kernel::node::Node) -> bool {
        let Some(surface) = node.first_session_surface() else {
            return false;
        };
        let stamp = self.terminals.output_stamp(surface);
        let previous = self.last_output_painted.get(surface).copied();
        if let Some(stamp) = stamp {
            self.last_output_painted.insert(surface.to_string(), stamp);
        }
        stamp.is_some() && previous != stamp
    }

    pub(crate) fn push_targets(
        &mut self,
        mod_index: usize,
        pane: Option<Rect>,
        hits: Vec<paint::Hit>,
    ) {
        if let Some(rect) = pane {
            self.click_targets.push(ClickTarget {
                mod_index,
                rect,
                identity: Identity::default(),
            });
        }
        self.click_targets
            .extend(hits.into_iter().map(|hit| ClickTarget {
                mod_index,
                rect: hit.rect,
                identity: hit.identity,
            }));
    }
}

fn anchor_span(at: u16, span: u16, start: u16, len: u16) -> u16 {
    let end = start.saturating_add(len);
    let at = at.clamp(start, end.saturating_sub(1).max(start));
    let origin = if at.saturating_add(span) <= end {
        at
    } else {
        (at + 1).saturating_sub(span)
    };
    origin.clamp(start, end.saturating_sub(span).max(start))
}
