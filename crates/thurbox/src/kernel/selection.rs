use ratatui::layout::{Position, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermPos {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneBounds(Rect);

impl PaneBounds {
    pub fn from_rect(r: Rect) -> Self {
        Self(r)
    }

    pub fn rect(&self) -> Rect {
        self.0
    }

    pub fn content_at(pane: Rect, x: u16, y: u16) -> Option<Self> {
        let inner = Rect {
            x: pane.x.saturating_add(1),
            y: pane.y.saturating_add(1),
            width: pane.width.saturating_sub(2),
            height: pane.height.saturating_sub(2),
        };
        if inner.width == 0 || inner.height == 0 {
            return None;
        }
        inner
            .contains(Position::new(x, y))
            .then(|| Self::from_rect(inner))
    }

    pub fn clamp(&self, x: u16, y: u16) -> (u16, u16) {
        let r = self.0;
        let cx = x.max(r.x).min(r.x + r.width.saturating_sub(1));
        let cy = y.max(r.y).min(r.y + r.height.saturating_sub(1));
        (cx, cy)
    }

    pub fn contains(&self, x: u16, y: u16) -> bool {
        self.0.contains(Position::new(x, y))
    }
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub anchor: TermPos,
    pub cursor: TermPos,
    pub dragging: bool,
    pub pane: PaneBounds,
}

impl Selection {
    pub fn new(pos: TermPos, pane: PaneBounds) -> Self {
        Self {
            anchor: pos,
            cursor: pos,
            dragging: true,
            pane,
        }
    }

    pub fn ordered(&self) -> (TermPos, TermPos) {
        if self.anchor.row < self.cursor.row
            || (self.anchor.row == self.cursor.row && self.anchor.col <= self.cursor.col)
        {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        let (start, end) = self.ordered();
        if row < start.row || row > end.row {
            return false;
        }
        if start.row == end.row {
            col >= start.col && col <= end.col
        } else if row == start.row {
            col >= start.col
        } else if row == end.row {
            col <= end.col
        } else {
            true
        }
    }

    fn row_spans(&self) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        let (start, end) = self.ordered();
        let pane = self.pane.rect();
        (start.row..=end.row)
            .take_while(move |&row| (row as u16) < pane.y + pane.height)
            .map(move |row| {
                let col_start = if row == start.row {
                    start.col
                } else {
                    pane.x as usize
                };
                let col_end = if row == end.row {
                    end.col + 1
                } else {
                    (pane.x + pane.width) as usize
                };
                (row, col_start, col_end)
            })
    }
}

pub fn highlight_buffer(
    buf: &mut ratatui::buffer::Buffer,
    selection: &Selection,
    style: ratatui::style::Style,
) {
    let buf_right = buf.area.x + buf.area.width;
    for (row, col_start, col_end) in selection.row_spans() {
        for col in col_start..col_end {
            if col as u16 >= buf_right {
                break;
            }
            if let Some(cell) = buf.cell_mut(Position::new(col as u16, row as u16)) {
                cell.set_style(style);
            }
        }
    }
}

pub fn extract_text_from_screen(
    screen: &vt100::Screen,
    selection: &Selection,
    pane_origin: (u16, u16),
) -> String {
    extract_text_from_rows(screen, selection, pane_origin, 0)
}

pub fn extract_text_from_rows(
    screen: &vt100::Screen,
    selection: &Selection,
    pane_origin: (u16, u16),
    top: u16,
) -> String {
    let (grid_rows, grid_cols) = screen.size();
    let (grid_rows, grid_cols) = (grid_rows.get(), grid_cols.get());
    let (ox, oy) = pane_origin;

    let mut lines: Vec<String> = Vec::new();
    let mut continues_previous = false;

    for (row, col_start, col_end) in selection.row_spans() {
        let Some(grid_row) = (row as u16)
            .checked_sub(oy)
            .map(|r| r.saturating_add(top))
            .filter(|r| *r < grid_rows)
        else {
            break;
        };

        let lo = (col_start as u16).saturating_sub(ox);
        let hi = (col_end as u16).saturating_sub(ox).min(grid_cols);

        let mut text = String::new();
        for col in lo..hi {
            if let Some(cell) = screen.cell(grid_row, col) {
                // vt100 stores a wide character's right half as an empty continuation cell.
                if cell.is_wide_continuation() {
                    continue;
                }
                if cell.has_contents() {
                    text.push_str(cell.contents());
                } else {
                    text.push(' ');
                }
            }
        }

        match lines.last_mut() {
            Some(last) if continues_previous => last.push_str(&text),
            _ => lines.push(text),
        }

        continues_previous = screen.row_wrapped(grid_row) && hi >= grid_cols;
    }

    while lines.last().is_some_and(|l| l.trim_end().is_empty()) {
        lines.pop();
    }

    lines
        .iter()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn extract_text_from_buffer(buf: &ratatui::buffer::Buffer, selection: &Selection) -> String {
    let buf_right = buf.area.x + buf.area.width;
    let mut lines = Vec::new();

    for (row, col_start, col_end) in selection.row_spans() {
        let mut line = String::new();
        for col in col_start..col_end {
            if col as u16 >= buf_right {
                break;
            }
            if let Some(cell) = buf.cell(Position::new(col as u16, row as u16)) {
                line.push_str(cell.symbol());
            }
        }
        lines.push(line.trim_end().to_string());
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pane() -> PaneBounds {
        PaneBounds::from_rect(Rect::new(0, 0, 200, 50))
    }

    #[test]
    fn selection_ordered_forward() {
        let sel = Selection {
            anchor: TermPos { row: 0, col: 5 },
            cursor: TermPos { row: 2, col: 10 },
            dragging: false,
            pane: test_pane(),
        };
        let (start, end) = sel.ordered();
        assert_eq!(start, TermPos { row: 0, col: 5 });
        assert_eq!(end, TermPos { row: 2, col: 10 });
    }

    #[test]
    fn selection_ordered_backward() {
        let sel = Selection {
            anchor: TermPos { row: 2, col: 10 },
            cursor: TermPos { row: 0, col: 5 },
            dragging: false,
            pane: test_pane(),
        };
        let (start, end) = sel.ordered();
        assert_eq!(start, TermPos { row: 0, col: 5 });
        assert_eq!(end, TermPos { row: 2, col: 10 });
    }

    #[test]
    fn selection_contains_single_line() {
        let sel = Selection {
            anchor: TermPos { row: 1, col: 3 },
            cursor: TermPos { row: 1, col: 8 },
            dragging: false,
            pane: test_pane(),
        };
        assert!(sel.contains(1, 5));
        assert!(sel.contains(1, 3));
        assert!(sel.contains(1, 8));
        assert!(!sel.contains(1, 2));
        assert!(!sel.contains(1, 9));
        assert!(!sel.contains(0, 5));
        assert!(!sel.contains(2, 5));
    }

    #[test]
    fn selection_contains_multi_line() {
        let sel = Selection {
            anchor: TermPos { row: 1, col: 5 },
            cursor: TermPos { row: 3, col: 10 },
            dragging: false,
            pane: test_pane(),
        };
        assert!(sel.contains(1, 5));
        assert!(sel.contains(1, 80));
        assert!(!sel.contains(1, 4));
        assert!(sel.contains(2, 0));
        assert!(sel.contains(2, 999));
        assert!(sel.contains(3, 0));
        assert!(sel.contains(3, 10));
        assert!(!sel.contains(3, 11));
        assert!(!sel.contains(0, 5));
        assert!(!sel.contains(4, 5));
    }

    fn make_sel(anchor: (usize, usize), cursor: (usize, usize), pane: PaneBounds) -> Selection {
        Selection {
            anchor: TermPos {
                row: anchor.0,
                col: anchor.1,
            },
            cursor: TermPos {
                row: cursor.0,
                col: cursor.1,
            },
            dragging: false,
            pane,
        }
    }

    fn fill_buffer(buf: &mut ratatui::buffer::Buffer, rows: &[&str]) {
        for (row_idx, text) in rows.iter().enumerate() {
            for (col_idx, ch) in text.chars().enumerate() {
                let pos = Position::new(col_idx as u16, row_idx as u16);
                if let Some(cell) = buf.cell_mut(pos) {
                    cell.set_symbol(&ch.to_string());
                }
            }
        }
    }

    #[test]
    fn row_spans_single_row() {
        let pane = PaneBounds::from_rect(Rect::new(0, 0, 80, 24));
        let sel = make_sel((2, 5), (2, 10), pane);
        let spans: Vec<_> = sel.row_spans().collect();
        assert_eq!(spans, vec![(2, 5, 11)]);
    }

    #[test]
    fn row_spans_multi_row() {
        let pane = PaneBounds::from_rect(Rect::new(10, 0, 50, 10));
        let sel = make_sel((1, 15), (3, 20), pane);
        let spans: Vec<_> = sel.row_spans().collect();
        assert_eq!(spans, vec![(1, 15, 60), (2, 10, 60), (3, 10, 21)]);
    }

    #[test]
    fn row_spans_clamped_by_pane_height() {
        let pane = PaneBounds::from_rect(Rect::new(0, 0, 80, 3));
        let sel = make_sel((1, 0), (5, 10), pane);
        let spans: Vec<_> = sel.row_spans().collect();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, 1);
        assert_eq!(spans[1].0, 2);
    }

    #[test]
    fn extract_from_buffer_single_line() {
        let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 20, 5));
        fill_buffer(&mut buf, &["hello world"]);

        let pane = PaneBounds::from_rect(Rect::new(0, 0, 20, 5));
        let sel = make_sel((0, 0), (0, 4), pane);
        assert_eq!(extract_text_from_buffer(&buf, &sel), "hello");
    }

    #[test]
    fn extract_from_buffer_multi_line() {
        let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 20, 5));
        fill_buffer(&mut buf, &["first line", "second line", "third line"]);

        let pane = PaneBounds::from_rect(Rect::new(0, 0, 20, 5));
        let sel = make_sel((0, 6), (2, 4), pane);
        let text = extract_text_from_buffer(&buf, &sel);
        assert_eq!(text, "line\nsecond line\nthird");
    }

    #[test]
    fn extract_trims_trailing_whitespace() {
        let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 20, 5));
        fill_buffer(&mut buf, &["  hello   "]);

        let pane = PaneBounds::from_rect(Rect::new(0, 0, 20, 5));
        let sel = make_sel((0, 0), (0, 9), pane);
        assert_eq!(extract_text_from_buffer(&buf, &sel), "  hello");
    }

    #[test]
    fn highlight_buffer_applies_style() {
        use ratatui::style::{Color, Style};

        let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 10, 3));
        fill_buffer(&mut buf, &["0123456789", "abcdefghij"]);

        let pane = PaneBounds::from_rect(Rect::new(0, 0, 10, 3));
        let sel = make_sel((0, 2), (0, 4), pane);
        let style = Style::default().fg(Color::Red);

        highlight_buffer(&mut buf, &sel, style);

        assert_eq!(buf.cell(Position::new(1, 0)).unwrap().fg, Color::Reset);
        assert_eq!(buf.cell(Position::new(2, 0)).unwrap().fg, Color::Red);
        assert_eq!(buf.cell(Position::new(4, 0)).unwrap().fg, Color::Red);
        assert_eq!(buf.cell(Position::new(5, 0)).unwrap().fg, Color::Reset);
    }

    #[test]
    fn extract_respects_pane_offset() {
        let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 30, 5));
        fill_buffer(
            &mut buf,
            &[
                "......header area.........",
                "|borders||content here....",
                "|borders||more content!...",
            ],
        );

        let pane = PaneBounds::from_rect(Rect::new(10, 1, 15, 3));
        let sel = make_sel((1, 10), (2, 22), pane);
        let text = extract_text_from_buffer(&buf, &sel);
        assert_eq!(text, "content here...\nmore content!");
    }

    fn screen_with(rows: u16, cols: u16, scrollback: usize, input: &str) -> vt100::Parser {
        let mut p = vt100::Parser::new(
            rows.try_into().unwrap(),
            cols.try_into().unwrap(),
            scrollback,
        );
        p.process(input.as_bytes());
        p
    }

    #[test]
    fn screen_extract_reads_grid_at_pane_origin() {
        let p = screen_with(5, 20, 0, "hello world");
        let pane = PaneBounds::from_rect(Rect::new(10, 3, 20, 5));
        let sel = make_sel((3, 10), (3, 14), pane);
        assert_eq!(extract_text_from_screen(p.screen(), &sel, (10, 3)), "hello");
    }

    #[test]
    fn screen_extract_joins_soft_wrapped_rows() {
        let url = "https://example.com/a/b";
        let p = screen_with(5, 10, 0, url);
        let pane = PaneBounds::from_rect(Rect::new(0, 0, 10, 5));
        let sel = make_sel((0, 0), (2, 9), pane);
        assert_eq!(extract_text_from_screen(p.screen(), &sel, (0, 0)), url);
    }

    #[test]
    fn screen_extract_keeps_hard_newlines_between_logical_lines() {
        let p = screen_with(5, 20, 0, "first\r\nsecond");
        let pane = PaneBounds::from_rect(Rect::new(0, 0, 20, 5));
        let sel = make_sel((0, 0), (1, 19), pane);
        assert_eq!(
            extract_text_from_screen(p.screen(), &sel, (0, 0)),
            "first\nsecond"
        );
    }

    #[test]
    fn screen_extract_follows_scrollback() {
        let input: String = (0..10).map(|i| format!("line{i}\r\n")).collect();
        let mut p = screen_with(3, 20, 20, &input);
        let pane = PaneBounds::from_rect(Rect::new(0, 0, 20, 3));
        let sel = make_sel((0, 0), (0, 19), pane);

        let bottom = extract_text_from_screen(p.screen(), &sel, (0, 0));

        p.screen_mut().set_scrollback(5);
        let scrolled = extract_text_from_screen(p.screen(), &sel, (0, 0));

        assert_ne!(bottom, scrolled);
        assert_eq!(scrolled.trim(), "line3");
    }

    #[test]
    fn screen_extract_skips_rows_outside_the_grid() {
        let p = screen_with(2, 10, 0, "ab");
        let pane = PaneBounds::from_rect(Rect::new(0, 0, 10, 8));
        let sel = make_sel((0, 0), (7, 9), pane);
        assert_eq!(extract_text_from_screen(p.screen(), &sel, (0, 0)), "ab");
    }

    #[test]
    fn screen_extract_preserves_interior_blanks() {
        let p = screen_with(2, 20, 0, "a    b");
        let pane = PaneBounds::from_rect(Rect::new(0, 0, 20, 2));
        let sel = make_sel((0, 0), (0, 19), pane);
        assert_eq!(extract_text_from_screen(p.screen(), &sel, (0, 0)), "a    b");
    }

    #[test]
    fn screen_extract_copies_wide_characters_byte_exact() {
        let text = "zq漢字 a  e\u{301}-end";
        let p = screen_with(2, 30, 0, text);
        let pane = PaneBounds::from_rect(Rect::new(0, 0, 30, 2));
        let sel = make_sel((0, 0), (0, 29), pane);
        assert_eq!(extract_text_from_screen(p.screen(), &sel, (0, 0)), text);
    }

    #[test]
    fn pane_bounds_clamp() {
        let pane = PaneBounds::from_rect(Rect::new(10, 5, 20, 10));
        assert_eq!(pane.clamp(15, 8), (15, 8));
        assert_eq!(pane.clamp(5, 3), (10, 5));
        assert_eq!(pane.clamp(50, 20), (29, 14));
    }

    #[test]
    fn pane_bounds_contains() {
        let pane = PaneBounds::from_rect(Rect::new(10, 5, 20, 10));
        assert!(pane.contains(10, 5));
        assert!(pane.contains(29, 14));
        assert!(!pane.contains(9, 5));
        assert!(!pane.contains(30, 5));
        assert!(!pane.contains(10, 4));
        assert!(!pane.contains(10, 15));
    }
}
