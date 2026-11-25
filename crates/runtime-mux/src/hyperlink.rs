use std::collections::{HashSet, VecDeque};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_RUNS: usize = 512;

const VISIBLE_SCAN_LIMIT: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HyperlinkRun {
    pub url: String,
    pub label: String,
    pub col: usize,
}

impl HyperlinkRun {
    fn width(&self) -> usize {
        UnicodeWidthStr::width(self.label.as_str())
    }

    fn covers(&self, col: usize) -> bool {
        col >= self.col && col < self.col + self.width()
    }

    fn is_printed_in(&self, row_text: &str) -> bool {
        label_at_column(row_text, self.col, &self.label)
    }
}

#[derive(Clone, Debug, Default)]
pub struct HyperlinkTable {
    runs: VecDeque<HyperlinkRun>,
}

impl HyperlinkTable {
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

    pub fn resolve(&self, row_text: &str, col: usize) -> Option<&str> {
        self.runs
            .iter()
            .rev()
            .find(|run| run.covers(col) && run.is_printed_in(row_text))
            .map(|run| run.url.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    pub fn runs(&self) -> impl Iterator<Item = &HyperlinkRun> {
        self.runs.iter()
    }

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

#[derive(Debug, PartialEq, Eq)]
pub struct VisibleRun<'a> {
    pub row: usize,
    pub col: usize,
    pub label: &'a str,
    pub url: &'a str,
}

pub fn osc8_open(url: &str) -> String {
    let safe: String = url.chars().filter(|c| !c.is_control()).collect();
    format!("\x1b]8;;{safe}\x07")
}

pub const OSC8_CLOSE: &str = "\x1b]8;;\x07";

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
