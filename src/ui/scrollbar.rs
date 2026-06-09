//! Shared scrollbar geometry + rendering for every scrollable pane.
//!
//! A single [`ScrollbarGeom`] describes one rendered scrollbar: the track rect
//! it occupies, the total scrollable `content_len`, and the visible `viewport`.
//! Renderers build it (via [`render_into`]) and the app records each one per
//! frame so mouse handlers can hit-test the track and map a click/drag `y` back
//! to a scroll position ([`ScrollbarGeom::position_for_y`]). Keeping the render
//! and the hit-test driven by the same struct guarantees they never drift.
//!
//! This module is pure geometry/rendering — it has no knowledge of *which*
//! pane a scrollbar drives (that mapping lives in `app`).

use ratatui::{
    layout::{Position, Rect},
    style::Style,
    widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use super::theme::Theme;

/// The geometry + metrics of one rendered scrollbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarGeom {
    /// The rect the scrollbar track occupies (its rightmost column carries the
    /// thumb). Mouse hit-testing is against this rect.
    pub track: Rect,
    /// Total number of scrollable units (rows of content).
    pub content_len: usize,
    /// Number of visible units (rows that fit in the viewport).
    pub viewport: usize,
}

impl ScrollbarGeom {
    /// Whether a screen-absolute `(x, y)` falls inside the track.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        self.track.contains(Position::new(x, y))
    }

    /// Map a mouse `y` inside the track to a content position in
    /// `0..=content_len-1`. Clamps `y` to the track, and is safe for degenerate
    /// tracks (`height <= 1`) and empty content.
    pub fn position_for_y(&self, y: u16) -> usize {
        if self.content_len == 0 {
            return 0;
        }
        let max = self.content_len - 1;
        if self.track.height <= 1 {
            // A one-row (or zero-row) track can't express intermediate
            // positions; snap to the end so a click still does something sane.
            return max;
        }
        let top = self.track.y;
        let bottom = self.track.y + self.track.height - 1;
        let y = y.clamp(top, bottom);
        let rel = (y - top) as f64 / (self.track.height - 1) as f64;
        (rel * max as f64).round() as usize
    }
}

/// Split `area` to make room for a right-edge scrollbar when the content
/// overflows the viewport. Returns the (possibly narrowed) content rect plus the
/// one-column track rect to hand to [`render_into`] — or `None` for the track
/// when everything fits or the area is too narrow to spare a column.
///
/// Centralizes the "scrollbar lives in the rightmost column" convention so every
/// pane reserves space and positions the bar identically.
pub fn reserve_track(area: Rect, content_len: usize, viewport: usize) -> (Rect, Option<Rect>) {
    if content_len <= viewport || area.width <= 1 {
        return (area, None);
    }
    let content = Rect {
        width: area.width - 1,
        ..area
    };
    let track = Rect {
        x: area.x + area.width - 1,
        width: 1,
        ..area
    };
    (content, Some(track))
}

/// Render a vertical scrollbar into `track` and return its geometry. Callers
/// decide *whether* there's overflow (via [`reserve_track`]) before calling;
/// this just draws the bar and hands back the geometry to record as a drag
/// target. Returns `None` only for a zero-height track.
///
/// `position` is the current scroll position in the same `0..content_len` space
/// that [`ScrollbarGeom::position_for_y`] maps back to.
pub fn render_into(
    frame: &mut Frame,
    track: Rect,
    content_len: usize,
    viewport: usize,
    position: usize,
) -> Option<ScrollbarGeom> {
    if track.height == 0 {
        return None;
    }

    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .thumb_style(Style::default().fg(Theme::accent()))
        .track_style(Style::default().fg(Theme::text_muted()));

    let mut state = ScrollbarState::new(content_len)
        .position(position)
        .viewport_content_length(viewport);
    frame.render_stateful_widget(scrollbar, track, &mut state);

    Some(ScrollbarGeom {
        track,
        content_len,
        viewport,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(y: u16, height: u16, content_len: usize) -> ScrollbarGeom {
        ScrollbarGeom {
            track: Rect::new(10, y, 1, height),
            content_len,
            viewport: 5,
        }
    }

    #[test]
    fn position_for_y_endpoints() {
        let g = geom(5, 10, 100);
        // Top of the track maps to 0, bottom maps to the last index.
        assert_eq!(g.position_for_y(5), 0);
        assert_eq!(g.position_for_y(14), 99);
    }

    #[test]
    fn position_for_y_midpoint() {
        let g = geom(0, 11, 101);
        // Exact middle row → exact middle position.
        assert_eq!(g.position_for_y(5), 50);
    }

    #[test]
    fn position_for_y_clamps_out_of_range() {
        let g = geom(5, 10, 100);
        // Above the track clamps to 0, below clamps to the max.
        assert_eq!(g.position_for_y(0), 0);
        assert_eq!(g.position_for_y(255), 99);
    }

    #[test]
    fn position_for_y_degenerate_track() {
        assert_eq!(geom(5, 1, 100).position_for_y(5), 99);
        assert_eq!(geom(5, 0, 100).position_for_y(5), 99);
    }

    #[test]
    fn position_for_y_empty_content() {
        assert_eq!(geom(5, 10, 0).position_for_y(8), 0);
    }

    #[test]
    fn contains_matches_track() {
        let g = geom(5, 10, 100);
        assert!(g.contains(10, 5));
        assert!(g.contains(10, 14));
        assert!(!g.contains(9, 5));
        assert!(!g.contains(10, 15));
    }

    #[test]
    fn reserve_track_splits_when_overflowing() {
        let area = Rect::new(4, 2, 20, 10);
        let (content, track) = reserve_track(area, 100, 10);
        // Content loses its rightmost column; the track claims it.
        assert_eq!(content, Rect::new(4, 2, 19, 10));
        assert_eq!(track, Some(Rect::new(23, 2, 1, 10)));
    }

    #[test]
    fn reserve_track_keeps_full_area_when_fitting() {
        let area = Rect::new(4, 2, 20, 10);
        // content_len <= viewport → no scrollbar, full content area.
        assert_eq!(reserve_track(area, 10, 10), (area, None));
        assert_eq!(reserve_track(area, 5, 10), (area, None));
    }

    #[test]
    fn reserve_track_skips_when_too_narrow() {
        let area = Rect::new(4, 2, 1, 10);
        // A one-column area can't spare a column for the bar.
        assert_eq!(reserve_track(area, 100, 10), (area, None));
    }
}
