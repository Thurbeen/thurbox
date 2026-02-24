use ratatui::layout::{Position, Rect};

/// A position on screen (absolute coordinates, 0-indexed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermPos {
    pub row: usize,
    pub col: usize,
}

/// Rectangular bounds of the pane a selection is confined to.
///
/// Wraps a `Rect` and adds selection-specific helpers (clamping, hit-testing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneBounds(Rect);

impl PaneBounds {
    pub fn from_rect(r: Rect) -> Self {
        Self(r)
    }

    pub fn rect(&self) -> Rect {
        self.0
    }

    /// Clamp a screen-absolute position to within this pane.
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

/// Active text selection state.
#[derive(Debug, Clone)]
pub struct Selection {
    /// Where the mouse button was first pressed.
    pub anchor: TermPos,
    /// Current drag position (updated on MouseDrag, finalized on MouseUp).
    pub cursor: TermPos,
    /// Whether the selection is still being dragged (vs finalized).
    pub dragging: bool,
    /// The pane this selection is confined to.
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

    /// Returns (start, end) in reading order (top-left to bottom-right).
    pub fn ordered(&self) -> (TermPos, TermPos) {
        if self.anchor.row < self.cursor.row
            || (self.anchor.row == self.cursor.row && self.anchor.col <= self.cursor.col)
        {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    /// Check whether a given (row, col) falls within the selection.
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

    /// Iterate over (row, col_start, col_end_exclusive) spans in the selection,
    /// clamped to pane bounds.
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

/// Apply a style to all buffer cells within the selection.
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

/// Extract selected text from a ratatui frame buffer.
///
/// Reads cell symbols within the selection, clamped to pane bounds.
/// Trailing whitespace is trimmed per line, lines joined with `\n`.
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
        // First row: col >= 5
        assert!(sel.contains(1, 5));
        assert!(sel.contains(1, 80));
        assert!(!sel.contains(1, 4));
        // Middle row: all cols
        assert!(sel.contains(2, 0));
        assert!(sel.contains(2, 999));
        // Last row: col <= 10
        assert!(sel.contains(3, 0));
        assert!(sel.contains(3, 10));
        assert!(!sel.contains(3, 11));
        // Outside rows
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

    /// Fill a buffer region with known text for extraction tests.
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
        // Row 1: starts at sel start col (15), ends at pane right (60)
        // Row 2: full pane width (10..60)
        // Row 3: pane left (10) to sel end col+1 (21)
        assert_eq!(spans, vec![(1, 15, 60), (2, 10, 60), (3, 10, 21)]);
    }

    #[test]
    fn row_spans_clamped_by_pane_height() {
        let pane = PaneBounds::from_rect(Rect::new(0, 0, 80, 3));
        let sel = make_sel((1, 0), (5, 10), pane);
        let spans: Vec<_> = sel.row_spans().collect();
        // Pane height 3 means rows 0..3, so rows 1 and 2 are included
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

        // Cells 2..5 should have the style, others should not
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

        // Pane starts at col 10, width 15 (cols 10..25), simulating inner area
        let pane = PaneBounds::from_rect(Rect::new(10, 1, 15, 3));
        // Select from (1,10) to (2,22) — within pane bounds
        let sel = make_sel((1, 10), (2, 22), pane);
        let text = extract_text_from_buffer(&buf, &sel);
        // Row 1: cols 10..25 (full pane width) = "content here..."
        // Row 2: cols 10..23 = "more content!"
        assert_eq!(text, "content here...\nmore content!");
    }

    #[test]
    fn pane_bounds_clamp() {
        let pane = PaneBounds::from_rect(Rect::new(10, 5, 20, 10));
        assert_eq!(pane.clamp(15, 8), (15, 8)); // inside
        assert_eq!(pane.clamp(5, 3), (10, 5)); // top-left
        assert_eq!(pane.clamp(50, 20), (29, 14)); // bottom-right
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
