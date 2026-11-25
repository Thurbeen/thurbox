use std::cell::Cell;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;

use super::Terminals;
use crate::kernel::paint::SurfaceProvider;

#[derive(Default)]
pub(super) struct Painted {
    pub(super) size: Cell<(u16, u16)>,
    pub(super) rect: Cell<Rect>,
    pub(super) shown_at: Cell<Option<std::time::Instant>>,
    pub(super) top: Cell<u16>,
}

impl Painted {
    pub(super) fn on_screen(&self) -> bool {
        let rect = self.rect.get();
        rect.width > 0 && rect.height > 0
    }
}

impl Terminals {
    pub fn cursor_on<'a>(&'a self, input: Option<&'a str>) -> CursorOn<'a> {
        CursorOn {
            terminals: self,
            input,
        }
    }

    pub(super) fn paint_program(
        &self,
        frame: &mut Frame,
        area: Rect,
        surface: &str,
        cursor: bool,
    ) -> crate::kernel::paint::ProgramPaint {
        let Some(key) = self.program_key(surface).cloned() else {
            return crate::kernel::paint::ProgramPaint::NotStarted;
        };
        let Some(slot) = self.programs.get(&key) else {
            return crate::kernel::paint::ProgramPaint::NotStarted;
        };
        if slot.pane.has_exited() {
            return crate::kernel::paint::ProgramPaint::Exited(slot.program.clone());
        }

        // Recorded so a click and a wheel can be resolved against this rect, the
        // same way a session surface's is.
        slot.painted.rect.set(area);
        // Matched to the rect on change only: a program told its size every frame
        // is a program sent a SIGWINCH every frame.
        let wanted = (area.height, area.width);
        if slot.painted.size.get() != wanted && slot.pane.resize(area.height, area.width) {
            slot.painted.size.set(wanted);
        }
        slot.pane.retake_size();
        let Ok(parser) = slot.pane.parser.lock() else {
            return crate::kernel::paint::ProgramPaint::NotStarted;
        };
        let top = grid_top(parser.screen(), area);
        slot.painted.top.set(top);
        let view = FromRow {
            screen: parser.screen(),
            top,
        };
        frame.render_widget(screen_widget(&view, cursor), area);
        let (rows, cols) = parser.screen().size();
        let size = (rows.get(), cols.get());
        if size != wanted && slot.pane.sized_elsewhere() {
            paint_sized_elsewhere(frame, area, size);
        }
        crate::kernel::paint::ProgramPaint::Painted
    }

    pub(super) fn paint_session(
        &self,
        frame: &mut Frame,
        area: Rect,
        session: &str,
        scroll: u16,
        cursor: bool,
    ) -> bool {
        // A session's shell is addressed as `<id>#shell`, so it is a second
        // surface over the same primitive rather than a second node kind — and
        // ONE path paints either of them, parameterised by which pane the name
        // asked for. It was two, and the two disagreed: each recorded its rect
        // and its size in the same pair of cells, so whichever painted second
        // resized both panes to its own rect and the pane painted first spent
        // the next frame undoing it (#1220).
        let Some(pane) = self.pane(session) else {
            return false;
        };
        let Some(parser) = pane.parser() else {
            return false;
        };
        let painted = pane.painted();

        // The pane must match the rect it is painted into, or its program wraps
        // at the wrong width. The memo is this surface's own, so the comparison
        // keeps a tmux round-trip off every frame without ever reading a size
        // that belongs to the other pane.
        painted.rect.set(area);
        painted.shown_at.set(Some(std::time::Instant::now()));
        let wanted = (area.height, area.width);
        if painted.size.get() != wanted && pane.resize(area.height, area.width) {
            painted.size.set(wanted);
        }
        pane.retake_size();

        // Restore is asynchronous. Until its ordered snapshot lands, paint
        // blank rather than either the dormant grid or a stale first frame
        // (#1242). The reader moves the output generation when it installs it.
        if !pane.is_resident() {
            pane.live.session.restore_pane(pane.shell);
            if !pane.is_resident() {
                frame.render_widget(ratatui::widgets::Clear, area);
                return true;
            }
        }

        let Ok(mut parser) = parser.lock() else {
            return false;
        };
        parser.screen_mut().set_scrollback(usize::from(scroll));
        super::links::clear_uncovered(frame, area, parser.screen());
        let top = grid_top(parser.screen(), area);
        painted.top.set(top);
        let view = FromRow {
            screen: parser.screen(),
            top,
        };
        frame.render_widget(screen_widget(&view, cursor), area);
        let (rows, cols) = parser.screen().size();
        let size = (rows.get(), cols.get());
        if size != wanted && pane.sized_elsewhere() {
            paint_sized_elsewhere(frame, area, size);
        }
        true
    }
}

pub(super) fn paint_sized_elsewhere(frame: &mut Frame, area: Rect, (rows, cols): (u16, u16)) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let text =
        format!(" {cols}\u{d7}{rows} \u{b7} sized by another thurbox \u{b7} type here to resize ");
    let width = u16::try_from(text.chars().count())
        .unwrap_or(u16::MAX)
        .min(area.width);
    let row = Rect {
        x: area.right() - width,
        y: area.bottom() - 1,
        width,
        height: 1,
    };
    let style = Style::default().add_modifier(Modifier::DIM | Modifier::REVERSED);
    frame.render_widget(ratatui::widgets::Paragraph::new(text).style(style), row);
}

pub(super) fn grid_top(screen: &vt100::Screen, area: Rect) -> u16 {
    screen.size().0.get().saturating_sub(area.height)
}

pub(super) fn screen_widget<'a>(
    view: &'a FromRow<'a>,
    cursor: bool,
) -> PseudoTerminal<'a, FromRow<'a>> {
    PseudoTerminal::new(view)
        .style(Style::default())
        .cursor(tui_term::widget::Cursor::default().visibility(cursor))
}

pub(super) struct FromRow<'a> {
    pub(super) screen: &'a vt100::Screen,
    pub(super) top: u16,
}

impl tui_term::widget::Screen for FromRow<'_> {
    type C = vt100::Cell;

    fn cell(&self, row: u16, col: u16) -> Option<&Self::C> {
        tui_term::widget::Screen::cell(self.screen, row.checked_add(self.top)?, col)
    }

    fn hide_cursor(&self) -> bool {
        let (row, _) = tui_term::widget::Screen::cursor_position(self.screen);
        tui_term::widget::Screen::hide_cursor(self.screen) || row < self.top
    }

    fn cursor_position(&self) -> (u16, u16) {
        let (row, col) = tui_term::widget::Screen::cursor_position(self.screen);
        (row.saturating_sub(self.top), col)
    }
}

impl SurfaceProvider for Terminals {
    fn render_program(
        &self,
        frame: &mut Frame,
        area: Rect,
        surface: &str,
    ) -> crate::kernel::paint::ProgramPaint {
        self.paint_program(frame, area, surface, true)
    }

    fn render_session(&self, frame: &mut Frame, area: Rect, session: &str, scroll: u16) -> bool {
        self.paint_session(frame, area, session, scroll, true)
    }
}

pub struct CursorOn<'a> {
    terminals: &'a Terminals,
    input: Option<&'a str>,
}

impl SurfaceProvider for CursorOn<'_> {
    fn render_program(
        &self,
        frame: &mut Frame,
        area: Rect,
        surface: &str,
    ) -> crate::kernel::paint::ProgramPaint {
        let cursor = self.input == Some(surface);
        self.terminals.paint_program(frame, area, surface, cursor)
    }

    fn render_session(&self, frame: &mut Frame, area: Rect, session: &str, scroll: u16) -> bool {
        let cursor = self.input == Some(session);
        self.terminals
            .paint_session(frame, area, session, scroll, cursor)
    }
}
