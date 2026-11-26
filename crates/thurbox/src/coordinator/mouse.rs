use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use thurbox::kernel::host::{Click, Scroll};
use thurbox::kernel::node::{ClickVerb, Identity};
use thurbox::kernel::selection::{PaneBounds, Selection, TermPos};

use thurbox::session::settings::ClipboardProvider;

use super::{key_event_from_chord, open_url};
use crate::{App, ClickTarget, PointerGrab};

impl App {
    pub(crate) fn on_mouse(&mut self, mouse: MouseEvent) {
        if let MouseEventKind::Down(_) = mouse.kind {
            self.click_train.begin();
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Some emulators drop the release when focus is lost mid-drag.
                self.pty_pointer = None;
                self.copy_after_paint = false;
                self.on_click(mouse.column, mouse.row, mouse.modifiers)
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.on_context_click(mouse.column, mouse.row)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(surface) = self.pty_pointer.clone() {
                    self.terminals
                        .forward_motion(&surface, mouse.column, mouse.row);
                } else if self.modals.on_drag(mouse.row) {
                    self.dirty = true;
                } else if !self.drag_held(mouse.column, mouse.row) {
                    self.drag_selection(mouse.column, mouse.row);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(surface) = self.pty_pointer.take() {
                    self.terminals
                        .forward_release(&surface, mouse.column, mouse.row);
                }
                self.pointer_grab = None;
                if let Some(selection) = &mut self.selection {
                    selection.dragging = false;
                    if selection.anchor == selection.cursor {
                        self.selection = None;
                    }
                }
            }
            MouseEventKind::ScrollUp => self.on_scroll(mouse.column, mouse.row, true),
            MouseEventKind::ScrollDown => self.on_scroll(mouse.column, mouse.row, false),
            MouseEventKind::Moved => {
                if !self.modals.is_open() && self.grabbed.is_none() {
                    self.terminals.forward_move(mouse.column, mouse.row);
                }
                self.hover(mouse.column, mouse.row);
            }
            _ => {}
        }
    }

    pub(crate) fn grid_selection_text(&self, selection: &Selection) -> Option<String> {
        self.surface_at(selection.pane.rect().x, selection.pane.rect().y)
            .filter(|(_, rect)| *rect == selection.pane.rect())
            .and_then(|(session, rect)| {
                self.terminals
                    .selected_text(&session, selection, (rect.x, rect.y))
            })
    }

    pub(crate) fn refresh_selection_text(&mut self) -> bool {
        let next = self
            .selection
            .clone()
            .filter(|selection| selection.anchor != selection.cursor)
            .map(|selection| match self.grid_selection_text(&selection) {
                Some(text) => (!text.trim().is_empty()).then_some(text),
                None => self.selected_text.clone(),
            })
            .unwrap_or(None);
        let changed = next != self.selected_text;
        self.selected_text = next;
        changed
    }

    pub(crate) fn on_scroll(&mut self, x: u16, y: u16, up: bool) {
        self.selection = None;
        let code = if up { KeyCode::Up } else { KeyCode::Down };
        let key = KeyEvent::new(code, KeyModifiers::NONE);

        if self.modals.is_open() {
            if self.modals.on_wheel(up) {
                self.dirty = true;
            } else if !self.modals.captures_everything()
                && self.wheel_notch.opens(Instant::now(), up)
            {
                self.dispatch_modal_key(&key);
            }
            return;
        }

        if let Some(index) = self.grabbed {
            if self.wheel_notch.opens(Instant::now(), up) {
                self.dispatch_key_to(index, &key);
                self.dirty = true;
            }
            return;
        }

        if self.terminals.forward_wheel(x, y, up) {
            self.dirty = true;
            return;
        }

        if let Some(target) = self.target_at(x, y) {
            let scroll = Scroll {
                up,
                x: x.saturating_sub(target.rect.x),
                y: y.saturating_sub(target.rect.y),
            };
            match self.host.on_scroll(target.mod_index, &scroll) {
                Ok(true) => {
                    self.dirty = true;
                    return;
                }
                Ok(false) => {}
                Err(e) => self.errors.push(e),
            }
            if self.wheel_notch.opens(Instant::now(), up) {
                self.dispatch_key_to(target.mod_index, &key);
                self.dirty = true;
            }
        }
    }

    pub(crate) fn hover(&mut self, x: u16, y: u16) {
        if self.modals.is_open() {
            let moved = self.modals.on_hover(x, y);
            let dropped = self.hovered.take().is_some();
            if dropped {
                self.note_published_change();
            }
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
            self.note_published_change();
            self.dirty = true;
        }
    }

    pub(crate) fn on_click(&mut self, x: u16, y: u16, modifiers: KeyModifiers) {
        if self.modals.is_open() {
            let message = self.with_modal_world(|modals, world| modals.on_click(x, y, world));
            if let Some(message) = message {
                self.toast(message);
            }
            self.dirty = true;
            return;
        }

        if let Some(hit) = self.band_target_at(x, y) {
            let missed = self.grabbed;
            match hit.identity.click_verb() {
                Some(ClickVerb::Action(action)) => {
                    if let Some(float) = missed {
                        self.dispatch_outside(float, x, y);
                    }
                    self.run_clicked_action(&action, self.focus);
                    self.dirty = true;
                    return;
                }
                Some(ClickVerb::Url(url)) => {
                    if let Some(float) = missed {
                        self.dispatch_outside(float, x, y);
                    }
                    self.open_or_copy_link(&url);
                    self.dirty = true;
                    return;
                }
                _ => {}
            }
        }

        let target = match float_grab(self.grabbed, self.target_at(x, y)) {
            Grab::Free(target) => target,
            Grab::Held(target) => {
                self.dispatch_click(target, x, y);
                return;
            }
            Grab::Outside(float) => {
                self.dispatch_outside(float, x, y);
                return;
            }
        };

        if modifiers.contains(KeyModifiers::CONTROL) {
            self.selection = None;
            self.open_clicked_link(x, y);
            return;
        }

        if let Some(target) = target {
            if self.dispatch_click(target, x, y) {
                return;
            }
        }
        if let Some(surface) = self.terminals.forward_press(x, y) {
            self.selection = None;
            self.pty_pointer = Some(surface);
            return;
        }
        self.begin_selection(x, y);
    }

    pub(crate) fn on_context_click(&mut self, x: u16, y: u16) {
        if self.modals.is_open() {
            return;
        }

        match float_grab(self.grabbed, self.target_at(x, y)) {
            Grab::Free(Some(target)) | Grab::Held(target) => {
                self.dispatch_context(target, x, y);
            }
            Grab::Free(None) => {}
            Grab::Outside(float) => self.dispatch_outside(float, x, y),
        }
    }

    fn dispatch_context(&mut self, target: ClickTarget, x: u16, y: u16) {
        let click = self.click_at(&target, x, y, false, 1);
        match self.host.on_context(target.mod_index, &click) {
            Ok(handled) => {
                if handled {
                    self.dirty = true;
                }
            }
            Err(e) => self.errors.push(e),
        }
    }

    fn dispatch_outside(&mut self, float: usize, x: u16, y: u16) {
        let click = Click {
            screen_x: x,
            screen_y: y,
            clicks: 1,
            ..Click::default()
        };
        match self.host.on_outside(float, &click) {
            Ok(handled) => {
                if handled {
                    self.dirty = true;
                }
            }
            Err(e) => self.errors.push(e),
        }
    }

    pub(crate) fn dispatch_click(&mut self, target: ClickTarget, x: u16, y: u16) -> bool {
        self.focus_mod(target.mod_index);

        match target.identity.click_verb() {
            Some(ClickVerb::Action(action)) => {
                self.run_clicked_action(&action, target.mod_index);
                true
            }
            Some(ClickVerb::Key(chord)) => {
                if let Some(key) = key_event_from_chord(&chord) {
                    self.on_key(&key);
                }
                true
            }
            Some(ClickVerb::Focus(mod_name)) => {
                if let Some(index) = self.host.index_of(&mod_name) {
                    self.focus_mod(index);
                }
                true
            }
            Some(ClickVerb::Url(url)) => {
                self.open_or_copy_link(&url);
                true
            }
            None => {
                if target.identity.is_drag_handle() {
                    self.selection = None;
                    self.pointer_grab = Some(PointerGrab {
                        mod_index: target.mod_index,
                        rect: target.rect,
                        identity: target.identity.clone(),
                    });
                }
                let clicks =
                    self.click_train
                        .count(Instant::now(), target.mod_index, &target.identity);
                let click = self.click_at(&target, x, y, false, clicks);
                match self.host.on_click(target.mod_index, &click) {
                    Ok(handled) => handled,
                    Err(e) => {
                        self.errors.push(e);
                        false
                    }
                }
            }
        }
    }

    pub(crate) fn run_clicked_action(&mut self, action: &str, _clicked: usize) {
        self.run_clicked_action_with_args(action, _clicked, &[]);
    }

    pub(crate) fn run_clicked_action_with_args(
        &mut self,
        action: &str,
        _clicked: usize,
        args: &[(&str, &str)],
    ) {
        let Some(descriptor) = self
            .registry
            .action_catalog()
            .into_iter()
            .find(|entry| entry.name == action)
        else {
            self.toast(format!("no catalog action named {action:?}"));
            return;
        };
        if descriptor.owner == "kernel" {
            self.run_kernel_action(action);
            return;
        }
        let Some(owner) = self.host.index_of(&descriptor.owner) else {
            return;
        };
        if let Err(e) = self.host.on_action_with_args(owner, action, args) {
            self.errors.push(e);
        }
    }

    pub(crate) fn target_at(&self, x: u16, y: u16) -> Option<ClickTarget> {
        let position = ratatui::layout::Position::new(x, y);
        self.click_targets
            .iter()
            .rev()
            .find(|target| target.rect.contains(position))
            .cloned()
    }

    pub(crate) fn band_target_at(&self, x: u16, y: u16) -> Option<thurbox::kernel::bands::Hit> {
        let position = ratatui::layout::Position::new(x, y);
        self.band_targets
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(position))
            .cloned()
    }

    pub(crate) fn surface_at(&self, x: u16, y: u16) -> Option<(String, Rect)> {
        let position = ratatui::layout::Position::new(x, y);
        self.snapshots
            .current()
            .sessions
            .iter()
            .flat_map(|row| {
                [
                    row.id.clone(),
                    thurbox::kernel::terminal::shell_surface(&row.id),
                ]
            })
            .filter_map(|surface| {
                let rect = self.terminals.last_rect(&surface)?;
                Some((surface, rect))
            })
            .find(|(_, rect)| rect.contains(position))
    }

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

    fn click_at(&self, target: &ClickTarget, x: u16, y: u16, dragging: bool, clicks: u8) -> Click {
        Click {
            id: target.identity.id.clone(),
            classes: target.identity.classes.clone(),
            role: target.identity.role.clone(),
            x: x.saturating_sub(target.rect.x),
            y: y.saturating_sub(target.rect.y),
            w: target.rect.width,
            h: target.rect.height,
            screen_x: x,
            screen_y: y,
            dragging,
            clicks,
        }
    }

    pub(crate) fn drag_held(&mut self, x: u16, y: u16) -> bool {
        let Some(grab) = self.pointer_grab.clone() else {
            return false;
        };
        let target = ClickTarget {
            mod_index: grab.mod_index,
            rect: grab.rect,
            identity: grab.identity,
        };
        let bounds = PaneBounds::from_rect(grab.rect);
        let (x, y) = bounds.clamp(x, y);
        let click = self.click_at(&target, x, y, true, 1);
        match self.host.on_click(grab.mod_index, &click) {
            Ok(_) => {}
            Err(e) => self.errors.push(e),
        }
        self.dirty = true;
        true
    }

    pub(crate) fn begin_selection(&mut self, x: u16, y: u16) {
        let rect = match self.surface_at(x, y) {
            Some((_, rect)) => Some(rect),
            None => self.pane_inner_at(x, y),
        };
        let Some(rect) = rect else {
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

    pub(crate) fn copy_on_select(&mut self) {
        let settings = self.config.in_force().clipboard;
        if !settings.copy_on_select || settings.provider == ClipboardProvider::None {
            return;
        }
        let Some(text) = self.selected_text.clone().filter(|t| !t.trim().is_empty()) else {
            return;
        };
        let message = copy_message(
            &text,
            thurbox::clipboard::copy(&text, self.clipboard.as_mut(), settings.provider),
        );
        self.toast(message);
    }

    pub(crate) fn copy_selection(&mut self) {
        let message = match self.selected_text.clone() {
            Some(text) if !text.trim().is_empty() => {
                let outcome = thurbox::clipboard::copy(
                    &text,
                    self.clipboard.as_mut(),
                    self.config.in_force().clipboard.provider,
                );
                copy_message(&text, outcome)
            }
            _ => "nothing to copy".to_string(),
        };
        self.toast(message);
        self.selection = None;
    }

    pub(crate) fn open_clicked_link(&mut self, x: u16, y: u16) {
        if let Some(url) = self.clicked_node_url(x, y) {
            self.open_or_copy_link(&url);
            return;
        }
        let Some((session, rect)) = self.surface_at(x, y) else {
            return;
        };
        let row = usize::from(y.saturating_sub(rect.y) + self.terminals.last_top(&session));
        let col = usize::from(x.saturating_sub(rect.x));
        if let Some(url) = self.terminals.url_at(&session, row, col) {
            self.open_or_copy_link(&url);
        }
    }

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

    pub(crate) fn open_or_copy_link(&mut self, url: &str) {
        let opened = open_url(url);
        let message = match opened {
            Ok(()) => format!("Opening {url}"),
            Err(reason) => {
                let outcome = thurbox::clipboard::copy(
                    url,
                    self.clipboard.as_mut(),
                    self.config.in_force().clipboard.provider,
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

    pub(crate) fn paint_outer_hyperlinks(&mut self, buf: &ratatui::buffer::Buffer) {
        let mut paints = Vec::new();
        for row in &self.snapshots.current().sessions {
            for surface in [
                row.id.clone(),
                thurbox::kernel::terminal::shell_surface(&row.id),
            ] {
                let scanned = self.links.get(&surface).map_or(&[][..], Vec::as_slice);
                paints.extend(self.terminals.hyperlink_paints(&surface, buf, scanned));
            }
        }
        let panes = self
            .click_targets
            .iter()
            .map(|target| (Some(target.mod_index), target.rect, &target.identity));
        let bands = self
            .band_targets
            .iter()
            .map(|hit| (None, hit.rect, &hit.identity));
        for (mod_index, rect, identity) in panes.chain(bands) {
            let Some(ClickVerb::Url(url)) = identity.click_verb() else {
                continue;
            };
            if self.link_paint_obscured(mod_index, rect) {
                continue;
            }
            paints.extend(thurbox::kernel::terminal::drawn_link_paints(
                buf, rect, &url,
            ));
        }
        // OSC 8 binds a URL to the cells, so an unchanged frame owes the terminal nothing.
        if paints != self.last_link_paints {
            if !paints.is_empty() {
                let _ = thurbox::kernel::terminal::paint_hyperlinks(&paints);
            }
            self.last_link_paints = paints;
        }
    }

    pub(crate) fn link_paint_obscured(&self, mod_index: Option<usize>, rect: Rect) -> bool {
        if self.modals.is_open() {
            return true;
        }
        self.drawn_floats.iter().any(|index| {
            Some(*index) != mod_index
                && self
                    .last_floats
                    .get(index)
                    .is_some_and(|(float, _)| float.intersects(rect))
        })
    }
}

enum Grab {
    Free(Option<ClickTarget>),
    Held(ClickTarget),
    Outside(usize),
}

fn float_grab(grabbed: Option<usize>, target: Option<ClickTarget>) -> Grab {
    match grabbed {
        None => Grab::Free(target),
        Some(float) => match target.filter(|target| target.mod_index == float) {
            Some(target) => Grab::Held(target),
            None => Grab::Outside(float),
        },
    }
}

// ghostty, kitty and xterm send one wheel report per scroll line, back to back: three per detent.
const WHEEL_NOTCH: Duration = Duration::from_millis(20);

#[derive(Default)]
pub(crate) struct WheelNotch {
    opened: Option<(Instant, bool)>,
}

impl WheelNotch {
    fn opens(&mut self, now: Instant, up: bool) -> bool {
        if let Some((at, direction)) = self.opened {
            if direction == up && now.duration_since(at) < WHEEL_NOTCH {
                return false;
            }
        }
        self.opened = Some((now, up));
        true
    }
}

// Under the 500 ms desktop default of macOS and Windows.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

struct Press {
    at: Instant,
    mod_index: usize,
    id: String,
    clicks: u8,
}

#[derive(Default)]
pub(crate) struct ClickTrain {
    previous: Option<Press>,
    last: Option<Press>,
}

impl ClickTrain {
    pub(crate) fn begin(&mut self) {
        self.previous = self.last.take();
    }

    fn count(&mut self, now: Instant, mod_index: usize, identity: &Identity) -> u8 {
        let Some(id) = identity.id.clone() else {
            return 1;
        };
        let repeats = self.previous.as_ref().is_some_and(|press| {
            press.clicks == 1
                && press.mod_index == mod_index
                && press.id == id
                && now.duration_since(press.at) < DOUBLE_CLICK
        });
        let clicks = if repeats { 2 } else { 1 };
        self.last = Some(Press {
            at: now,
            mod_index,
            id,
            clicks,
        });
        clicks
    }
}

fn copy_message(
    text: &str,
    outcome: Result<thurbox::clipboard::CopyRoute, thurbox::clipboard::CopyError>,
) -> String {
    match outcome {
        Ok(route) => format!(
            "copied {} line(s){}",
            text.lines().count(),
            route.toast_suffix()
        ),
        Err(e) => format!("copy failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn painted_by(mod_index: usize) -> Option<ClickTarget> {
        Some(ClickTarget {
            mod_index,
            rect: Rect::new(0, 0, 10, 1),
            identity: Identity::default(),
        })
    }

    fn mod_of(target: &Option<ClickTarget>) -> Option<usize> {
        target.as_ref().map(|target| target.mod_index)
    }

    #[test]
    fn a_float_holds_every_press_and_hands_on_only_its_own() {
        match float_grab(None, painted_by(3)) {
            Grab::Free(target) => assert_eq!(mod_of(&target), Some(3)),
            _ => panic!("no float is up, so nothing holds the press"),
        }
        match float_grab(Some(7), painted_by(7)) {
            Grab::Held(target) => assert_eq!(target.mod_index, 7),
            _ => panic!("a press on the float is the float's"),
        }
        match float_grab(Some(7), painted_by(3)) {
            Grab::Outside(float) => assert_eq!(float, 7, "the miss is told to the float"),
            _ => panic!("a press outside the float is swallowed, not freed"),
        }
        match float_grab(Some(7), None) {
            Grab::Outside(float) => assert_eq!(float, 7),
            _ => panic!("a press on nothing is still swallowed"),
        }
    }

    #[test]
    fn a_burst_of_reports_is_one_notch() {
        let start = Instant::now();
        let mut notch = WheelNotch::default();
        let mut steps = 0;
        for offset in [0, 1, 2] {
            if notch.opens(start + Duration::from_micros(offset * 200), false) {
                steps += 1;
            }
        }
        assert_eq!(steps, 1, "one detent must move the selection once");
    }

    #[test]
    fn a_held_spin_keeps_stepping() {
        let start = Instant::now();
        let mut notch = WheelNotch::default();
        let mut steps = 0;
        for tick in 0..40 {
            if notch.opens(start + Duration::from_millis(tick * 5), false) {
                steps += 1;
            }
        }
        assert_eq!(steps, 10, "a spin must keep moving, at one step per notch");
    }

    #[test]
    fn reversing_steps_immediately() {
        let now = Instant::now();
        let mut notch = WheelNotch::default();
        assert!(notch.opens(now, false));
        assert!(notch.opens(now, true), "a direction change is a new notch");
    }

    #[test]
    fn separated_notches_both_step() {
        let start = Instant::now();
        let mut notch = WheelNotch::default();
        assert!(notch.opens(start, false));
        assert!(notch.opens(start + WHEEL_NOTCH, false));
    }

    fn row(id: &str) -> Identity {
        Identity {
            id: Some(id.into()),
            classes: vec!["row".into()],
            role: Some("row".into()),
        }
    }

    fn press(train: &mut ClickTrain, at: Instant, mod_index: usize, identity: &Identity) -> u8 {
        train.begin();
        train.count(at, mod_index, identity)
    }

    #[test]
    fn a_second_press_on_the_same_node_within_the_window_is_a_double_click() {
        let start = Instant::now();
        let mut train = ClickTrain::default();
        assert_eq!(press(&mut train, start, 1, &row("a")), 1);
        assert_eq!(
            press(&mut train, start + Duration::from_millis(100), 1, &row("a")),
            2
        );
    }

    #[test]
    fn a_row_that_gained_a_class_between_two_presses_still_doubles() {
        let start = Instant::now();
        let mut train = ClickTrain::default();
        let mut selected = row("a");
        selected.classes.push("selected".into());
        assert_eq!(press(&mut train, start, 1, &row("a")), 1);
        assert_eq!(
            press(&mut train, start + Duration::from_millis(100), 1, &selected),
            2
        );
    }

    #[test]
    fn a_press_after_the_window_starts_over() {
        let start = Instant::now();
        let mut train = ClickTrain::default();
        assert_eq!(press(&mut train, start, 1, &row("a")), 1);
        assert_eq!(press(&mut train, start + DOUBLE_CLICK, 1, &row("a")), 1);
    }

    #[test]
    fn a_press_on_another_node_starts_over() {
        let start = Instant::now();
        let mut train = ClickTrain::default();
        assert_eq!(press(&mut train, start, 1, &row("a")), 1);
        assert_eq!(
            press(&mut train, start + Duration::from_millis(100), 1, &row("b")),
            1
        );
        assert_eq!(
            press(&mut train, start + Duration::from_millis(200), 1, &row("b")),
            2
        );
    }

    #[test]
    fn a_press_that_reaches_no_node_between_two_presses_starts_over() {
        let start = Instant::now();
        let mut train = ClickTrain::default();
        assert_eq!(press(&mut train, start, 1, &row("a")), 1);
        train.begin();
        assert_eq!(
            press(&mut train, start + Duration::from_millis(200), 1, &row("a")),
            1
        );
    }

    #[test]
    fn the_same_id_in_another_pane_starts_over() {
        let start = Instant::now();
        let mut train = ClickTrain::default();
        assert_eq!(press(&mut train, start, 1, &row("a")), 1);
        assert_eq!(
            press(&mut train, start + Duration::from_millis(100), 2, &row("a")),
            1
        );
    }

    #[test]
    fn a_third_quick_press_is_a_new_single() {
        let start = Instant::now();
        let mut train = ClickTrain::default();
        assert_eq!(press(&mut train, start, 1, &row("a")), 1);
        assert_eq!(
            press(&mut train, start + Duration::from_millis(100), 1, &row("a")),
            2
        );
        assert_eq!(
            press(&mut train, start + Duration::from_millis(200), 1, &row("a")),
            1
        );
    }

    #[test]
    fn a_node_without_an_id_never_doubles() {
        let start = Instant::now();
        let mut train = ClickTrain::default();
        let bare = Identity {
            id: None,
            classes: vec!["row".into()],
            role: None,
        };
        assert_eq!(press(&mut train, start, 1, &bare), 1);
        assert_eq!(
            press(&mut train, start + Duration::from_millis(100), 1, &bare),
            1
        );
    }
}
