//! OSC 8 hyperlink runs captured from an agent's output.
//!
//! Agent CLIs render a markdown link as an OSC 8 hyperlink: the escape carries
//! the target and only the *label* is printed (`Github`, never
//! `https://github.com`). The plain-text scan in [`crate::ui::links`] therefore
//! finds nothing on such a row and a `Ctrl+Click` has no target at all — the
//! URL exists only in the escape sequence, which `vt100` discards. The parser
//! callbacks record each run here instead (see `crate::agent::osc8`), and a
//! click resolves against this table.
//!
//! A run is keyed by its **label text + start column**, not by screen row: the
//! row a link sits on moves every time the agent's transcript scrolls, while
//! the printed glyphs and the column they start at survive scrolling and
//! redraws. Resolution re-checks that the label is still on screen at that
//! column, so a row whose content has since been overwritten resolves to
//! nothing rather than to a stale URL.

use std::collections::{HashSet, VecDeque};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Runs kept per session, oldest dropped first. A long transcript prints far
/// more links than a click will ever target; the cap keeps the table bounded
/// without needing to know when a run scrolls out of the scrollback.
const MAX_RUNS: usize = 512;

/// Newest runs scanned by [`HyperlinkTable::visible_runs`]. That runs once per
/// painted frame, so the scan is bounded; a run older than this is many
/// screenfuls back and cannot be on screen.
const VISIBLE_SCAN_LIMIT: usize = 128;

/// One OSC 8 hyperlink run, on one screen row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HyperlinkRun {
    /// Target URL the escape carried.
    pub url: String,
    /// Glyphs printed while the run was open (a run that wrapped contributes
    /// one entry per row).
    pub label: String,
    /// Screen column the label starts at.
    pub col: usize,
}

impl HyperlinkRun {
    /// Cells the label occupies.
    fn width(&self) -> usize {
        UnicodeWidthStr::width(self.label.as_str())
    }

    /// Whether display column `col` falls on the label.
    fn covers(&self, col: usize) -> bool {
        col >= self.col && col < self.col + self.width()
    }

    /// Whether `row_text` still prints this label where it was captured — the
    /// check that keeps a scrolled-away or repainted row from resolving to a
    /// stale target.
    fn is_printed_in(&self, row_text: &str) -> bool {
        label_at_column(row_text, self.col, &self.label)
    }
}

/// Per-session table of captured hyperlink runs.
#[derive(Clone, Debug, Default)]
pub struct HyperlinkTable {
    runs: VecDeque<HyperlinkRun>,
}

impl HyperlinkTable {
    /// Record a run's label.
    ///
    /// An agent repaints its transcript constantly, re-emitting the same runs:
    /// a label already captured at that column is refreshed in place (its target
    /// may have changed) rather than appended, so a redraw neither grows the
    /// table nor allocates.
    pub fn record(&mut self, url: &str, label: &str, col: usize) {
        if url.is_empty() || label.is_empty() {
            return;
        }
        if let Some(run) = self
            .runs
            .iter_mut()
            .find(|run| run.col == col && run.label == label)
        {
            if run.url != url {
                run.url.clear();
                run.url.push_str(url);
            }
            return;
        }
        self.runs.push_back(HyperlinkRun {
            url: url.to_string(),
            label: label.to_string(),
            col,
        });
        while self.runs.len() > MAX_RUNS {
            self.runs.pop_front();
        }
    }

    /// Target of the hyperlink under display column `col` of `row_text`, if
    /// any. `row_text` is one screen row as
    /// [`crate::ui::links::extract_screen_rows`] renders it (one char per
    /// glyph), so columns compare directly against a click's cell column.
    ///
    /// Newest run first, so a redrawn row resolves against what it prints now.
    /// A label still has to be present at its recorded column for the run to
    /// match — the guard against a row that has since scrolled other text into
    /// that spot.
    pub fn resolve(&self, row_text: &str, col: usize) -> Option<&str> {
        self.runs
            .iter()
            .rev()
            .find(|run| run.covers(col) && run.is_printed_in(row_text))
            .map(|run| run.url.as_str())
    }

    /// Whether nothing has been captured — the state of a session whose agent
    /// never printed a link, checked to keep the per-frame
    /// [`Self::visible_runs`] pass off the render path entirely.
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// Captured runs, oldest first (tests / diagnostics).
    pub fn runs(&self) -> impl Iterator<Item = &HyperlinkRun> {
        self.runs.iter()
    }

    /// Every run currently on screen, so the renderer can hand them to the
    /// outer terminal (see [`osc8_open`]). `rows` is one screenful as
    /// [`crate::ui::links::extract_screen_rows`] renders it.
    ///
    /// Newest first, at most one run per starting cell — the same
    /// "what does this cell print *now*" rule [`Self::resolve`] applies, so a
    /// repainted row can't show a link that has scrolled away. Only the newest
    /// `VISIBLE_SCAN_LIMIT` runs are considered: this drives a per-frame pass,
    /// and anything older is many screenfuls back.
    pub fn visible_runs<'a>(&'a self, rows: &[String]) -> Vec<VisibleRun<'a>> {
        let mut claimed: HashSet<(usize, usize)> = HashSet::new();
        let mut visible = Vec::new();
        for run in self.runs.iter().rev().take(VISIBLE_SCAN_LIMIT) {
            for (row, text) in rows.iter().enumerate() {
                if run.is_printed_in(text) && claimed.insert((row, run.col)) {
                    visible.push(VisibleRun {
                        row,
                        col: run.col,
                        label: &run.label,
                        url: &run.url,
                    });
                }
            }
        }
        visible
    }
}

/// A run found on the screen as currently drawn.
#[derive(Debug, PartialEq, Eq)]
pub struct VisibleRun<'a> {
    pub row: usize,
    pub col: usize,
    pub label: &'a str,
    pub url: &'a str,
}

/// Opening half of an OSC 8 hyperlink: everything printed until
/// [`OSC8_CLOSE`] becomes a link to `url` in the terminal thurbox itself runs
/// in.
///
/// Control characters are stripped. The URL is **agent-controlled text being
/// written back to the user's own terminal**, and an embedded `ESC` would end
/// the sequence early and let the rest be interpreted as escapes of its own.
pub fn osc8_open(url: &str) -> String {
    let safe: String = url.chars().filter(|c| !c.is_control()).collect();
    format!("\x1b]8;;{safe}\x07")
}

/// Closing half of an OSC 8 hyperlink (see [`osc8_open`]).
pub const OSC8_CLOSE: &str = "\x1b]8;;\x07";

/// Whether `row_text` prints `label` starting exactly at display column `col`.
fn label_at_column(row_text: &str, col: usize, label: &str) -> bool {
    let mut width = 0;
    for (offset, ch) in row_text.char_indices() {
        if width == col {
            return row_text[offset..].starts_with(label);
        }
        if width > col {
            // `col` lands inside a wide glyph — no label can start there.
            return false;
        }
        width += UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_label_under_click() {
        let mut table = HyperlinkTable::default();
        table.record("https://github.com", "Github", 4);
        let row = "see Github done";
        // Every cell of the label resolves; the cells around it do not.
        assert_eq!(table.resolve(row, 4), Some("https://github.com"));
        assert_eq!(table.resolve(row, 9), Some("https://github.com"));
        assert_eq!(table.resolve(row, 3), None);
        assert_eq!(table.resolve(row, 10), None);
    }

    #[test]
    fn ignores_row_whose_content_moved_on() {
        let mut table = HyperlinkTable::default();
        table.record("https://github.com", "Github", 4);
        // Same column, different text now printed there (the row scrolled and
        // was reused): no link, rather than the stale one.
        assert_eq!(table.resolve("see Gitlab done", 4), None);
        // Right label, wrong column.
        assert_eq!(table.resolve("Github done", 0), None);
    }

    #[test]
    fn newest_run_wins_at_a_column() {
        let mut table = HyperlinkTable::default();
        table.record("https://a.example", "docs", 0);
        table.record("https://b.example", "docs", 0);
        assert_eq!(table.resolve("docs", 0), Some("https://b.example"));
        // The replaced entry is gone, not shadowed.
        assert_eq!(table.runs().count(), 1);
    }

    #[test]
    fn same_label_at_two_columns_is_kept_twice() {
        let mut table = HyperlinkTable::default();
        table.record("https://a.example", "docs", 0);
        table.record("https://b.example", "docs", 10);
        let row = "docs      docs";
        assert_eq!(table.resolve(row, 0), Some("https://a.example"));
        assert_eq!(table.resolve(row, 10), Some("https://b.example"));
    }

    #[test]
    fn wide_glyphs_before_a_label_shift_its_column() {
        let mut table = HyperlinkTable::default();
        // 漢字 spans four cells, so the label starts at column 5.
        table.record("https://example.com", "Github", 5);
        let row = "漢字 Github";
        assert_eq!(table.resolve(row, 5), Some("https://example.com"));
        // The trailing cell of 字 is not part of the label.
        assert_eq!(table.resolve(row, 4), None);
        // A column landing inside a wide glyph never starts a label.
        assert_eq!(table.resolve(row, 1), None);
    }

    #[test]
    fn empty_url_or_label_is_not_recorded() {
        let mut table = HyperlinkTable::default();
        table.record("", "Github", 0);
        table.record("https://example.com", "", 0);
        assert_eq!(table.runs().count(), 0);
    }

    #[test]
    fn table_is_bounded() {
        let mut table = HyperlinkTable::default();
        for i in 0..MAX_RUNS + 50 {
            table.record(&format!("https://example.com/{i}"), "link", i);
        }
        assert_eq!(table.runs().count(), MAX_RUNS);
        // The oldest runs were dropped, the newest kept.
        assert!(table.resolve("link", 0).is_none());
        let last = MAX_RUNS + 49;
        let row = " ".repeat(last) + "link";
        assert_eq!(
            table.resolve(&row, last),
            Some(format!("https://example.com/{last}").as_str())
        );
    }

    #[test]
    fn visible_runs_finds_what_the_screen_prints_now() {
        let mut table = HyperlinkTable::default();
        table.record("https://github.com", "Github", 4);
        table.record("https://gone.example", "Vanished", 0);
        let rows = vec![
            "nothing here".to_string(),
            "see Github done".to_string(),
            String::new(),
        ];
        let visible = table.visible_runs(&rows);
        assert_eq!(
            visible,
            vec![VisibleRun {
                row: 1,
                col: 4,
                label: "Github",
                url: "https://github.com",
            }],
            "a run whose label is no longer on screen is not reported"
        );
    }

    #[test]
    fn visible_runs_reports_the_newest_run_per_cell() {
        let mut table = HyperlinkTable::default();
        table.record("https://old.example", "docs", 0);
        table.record("https://new.example", "docs", 0);
        let visible = table.visible_runs(&["docs".to_string()]);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].url, "https://new.example");
    }

    #[test]
    fn osc8_open_strips_control_characters() {
        assert_eq!(
            osc8_open("https://example.com"),
            "\x1b]8;;https://example.com\x07"
        );
        // An agent-supplied URL must not be able to end the sequence early and
        // inject escapes into the user's terminal.
        assert_eq!(
            osc8_open("https://x.example\x1b[31m\x07\n"),
            "\x1b]8;;https://x.example[31m\x07"
        );
    }

    #[test]
    fn empty_table_resolves_nothing() {
        assert_eq!(HyperlinkTable::default().resolve("Github", 0), None);
    }
}
