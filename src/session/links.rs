//! Detecting links in terminal output.
//!
//! Pure: a vt100 screen in, positions and URLs out. It lives in `session`
//! rather than beside a renderer because **both** halves of the crate need it —
//! v1's mouse layer and the v2 kernel, which cannot import `ui`. Duplicating it
//! would have meant two definitions of what counts as a URL.
//!
//! Positions are display-width based, not byte- or char-based, so they map
//! directly onto vt100 cell columns even on rows containing wide (CJK/emoji)
//! glyphs, which each span two cells.

use unicode_width::UnicodeWidthStr;

pub struct DetectedLink {
    pub row: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub url: String,
}

/// URL schemes we linkify, matching the old `(?:https?|file)://` prefix.
const SCHEMES: [&str; 3] = ["https://", "http://", "file://"];

/// A URL run stops at whitespace or any of these terminators (the old
/// character class `[^\s<>"'\x60)\]]`).
fn is_url_terminator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | ')' | ']')
}

/// Leftmost, non-overlapping scan for `scheme://…` runs — the hand-rolled
/// equivalent of the former URL regex. Returns each match's byte offset in
/// `row` and the matched slice, so callers keep using byte-based slicing for
/// display-width column math.
fn find_url_runs(row: &str) -> Vec<(usize, &str)> {
    let bytes = row.as_bytes();
    let mut matches = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &row[i..];
        if let Some(scheme) = SCHEMES.iter().find(|s| rest.starts_with(**s)) {
            let run_len = rest[scheme.len()..]
                .find(is_url_terminator)
                .map(|end| scheme.len() + end)
                .unwrap_or(rest.len());
            matches.push((i, &row[i..i + run_len]));
            i += run_len;
        } else {
            // Advance one full char so byte offsets stay UTF-8 aligned.
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    matches
}

/// Extract visible rows from a vt100 screen, one string per row.
///
/// Emits one char per *glyph*, not per cell: a wide (CJK/emoji) glyph occupies
/// two cells, where vt100 stores the glyph in the lead cell and marks the next
/// cell as a continuation. Those continuation cells are skipped so the string
/// holds exactly the printed characters, letting [`detect_urls`] recover screen
/// cell columns via display width rather than a per-cell placeholder.
pub fn extract_screen_rows(screen: &vt100::Screen) -> Vec<String> {
    let (rows, cols) = screen.size();
    (0..rows)
        .map(|row| {
            let mut line = String::new();
            for col in 0..cols {
                match screen.cell(row, col) {
                    Some(c) if c.is_wide_continuation() => continue,
                    Some(c) => line.push(c.contents().chars().next().unwrap_or(' ')),
                    None => line.push(' '),
                }
            }
            line
        })
        .collect()
}

/// Detect URLs in screen rows, stripping trailing punctuation.
///
/// Positions are display-width based (not byte- or char-based) so they map
/// directly to vt100 screen cell columns even on rows containing wide
/// (CJK/emoji) glyphs, which each span two cells.
pub fn detect_urls(screen_rows: &[String]) -> Vec<DetectedLink> {
    let mut links = Vec::new();
    for (row_idx, row) in screen_rows.iter().enumerate() {
        for (start, matched) in find_url_runs(row) {
            let mut url = matched;
            while url.ends_with(['.', ',', ';', ':', ')', ']']) {
                url = &url[..url.len() - 1];
            }
            // Skip bare scheme-only matches like "http://" with no host
            if url.len() > "https://".len() {
                // Convert the match's byte offset to cell columns using display
                // width: a wide glyph before the URL shifts it two cells, not one.
                let start_col = UnicodeWidthStr::width(&row[..start]);
                let end_col = start_col + UnicodeWidthStr::width(url);
                links.push(DetectedLink {
                    row: row_idx,
                    start_col,
                    end_col,
                    url: url.to_string(),
                });
            }
        }
    }
    links
}

/// Find the URL at a given screen position, if any.
pub fn url_at_position(links: &[DetectedLink], row: usize, col: usize) -> Option<&str> {
    links
        .iter()
        .find(|link| link.row == row && col >= link.start_col && col < link.end_col)
        .map(|link| link.url.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_https_url() {
        let rows = vec!["Visit https://example.com for info".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com");
        assert_eq!(links[0].row, 0);
        assert_eq!(links[0].start_col, 6);
        assert_eq!(links[0].end_col, 25);
    }

    #[test]
    fn detect_http_url() {
        let rows = vec!["http://example.org/path?q=1".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "http://example.org/path?q=1");
    }

    #[test]
    fn strip_trailing_punctuation() {
        let rows = vec!["See https://example.com/page.".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com/page");
    }

    #[test]
    fn strip_trailing_paren() {
        let rows = vec!["(https://example.com)".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com");
    }

    #[test]
    fn multiple_urls_on_one_row() {
        let rows = vec!["https://a.com and https://b.com here".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].url, "https://a.com");
        assert_eq!(links[1].url, "https://b.com");
    }

    #[test]
    fn no_urls() {
        let rows = vec!["no links here".to_string()];
        let links = detect_urls(&rows);
        assert!(links.is_empty());
    }

    #[test]
    fn url_at_position_hit() {
        let rows = vec!["Visit https://example.com for info".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(url_at_position(&links, 0, 6), Some("https://example.com"));
        // last char of URL (end_col is exclusive)
        assert_eq!(url_at_position(&links, 0, 24), Some("https://example.com"));
    }

    #[test]
    fn url_at_position_miss() {
        let rows = vec!["Visit https://example.com for info".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(url_at_position(&links, 0, 0), None);
        assert_eq!(url_at_position(&links, 0, 25), None);
        assert_eq!(url_at_position(&links, 1, 10), None);
    }

    #[test]
    fn file_url_detected() {
        let rows = vec!["file:///home/user/doc.txt".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "file:///home/user/doc.txt");
    }

    #[test]
    fn preserves_path_components() {
        let rows = vec!["https://example.com/a/b/c?x=1&y=2#frag".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com/a/b/c?x=1&y=2#frag");
    }

    #[test]
    fn empty_rows() {
        let links = detect_urls(&[]);
        assert!(links.is_empty());
    }

    #[test]
    fn url_at_position_empty_links() {
        assert_eq!(url_at_position(&[], 0, 0), None);
    }

    #[test]
    fn strip_multiple_trailing_punctuation() {
        let rows = vec!["https://example.com);;".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com");
    }

    #[test]
    fn url_at_start_of_row() {
        let rows = vec!["https://example.com".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].start_col, 0);
    }

    #[test]
    fn urls_across_multiple_rows() {
        let rows = vec![
            "first https://a.com here".to_string(),
            "no url line".to_string(),
            "last https://b.com end".to_string(),
        ];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].row, 0);
        assert_eq!(links[0].url, "https://a.com");
        assert_eq!(links[1].row, 2);
        assert_eq!(links[1].url, "https://b.com");
    }

    #[test]
    fn bare_scheme_rejected() {
        let rows = vec!["see https:// here".to_string()];
        let links = detect_urls(&rows);
        assert!(links.is_empty());
    }

    #[test]
    fn strip_trailing_colon_and_bracket() {
        let rows = vec!["[https://example.com/path]:".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com/path");
    }

    #[test]
    fn non_ascii_before_url_uses_char_offsets() {
        // '•' is 3 bytes in UTF-8 but 1 screen cell — byte offset would be 4, char offset is 2
        let rows = vec!["• https://example.com".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].start_col, 2);
        assert_eq!(links[0].end_col, 21);
        assert_eq!(url_at_position(&links, 0, 2), Some("https://example.com"));
        assert_eq!(url_at_position(&links, 0, 1), None);
    }

    #[test]
    fn wide_chars_before_url_use_display_width() {
        // Each CJK glyph spans two cells; the URL therefore starts at column 5
        // (漢=2 + 字=2 + space=1), not column 3 as a per-char count would give.
        let rows = vec!["漢字 https://example.com".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com");
        assert_eq!(links[0].start_col, 5);
        assert_eq!(links[0].end_col, 5 + "https://example.com".len());
        assert_eq!(url_at_position(&links, 0, 5), Some("https://example.com"));
        // The trailing cell of 字 (column 4) is not part of the URL.
        assert_eq!(url_at_position(&links, 0, 4), None);
    }

    #[test]
    fn emoji_before_url_uses_display_width() {
        // 🚀 is one char but two cells wide; URL starts at column 3 (🚀=2 + space).
        let rows = vec!["🚀 https://example.com".to_string()];
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].start_col, 3);
        assert_eq!(url_at_position(&links, 0, 3), Some("https://example.com"));
    }

    #[test]
    fn wide_chars_through_parser_match_cell_columns() {
        // End-to-end over the real screen path: extract_screen_rows must agree
        // with the cell columns that a mouse click reports.
        let mut parser = vt100::Parser::new(1, 40, 0);
        parser.process("漢字 https://example.com".as_bytes());
        let rows = extract_screen_rows(parser.screen());
        let links = detect_urls(&rows);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].start_col, 5);
        assert_eq!(url_at_position(&links, 0, 5), Some("https://example.com"));
        assert_eq!(url_at_position(&links, 0, 4), None);
    }

    #[test]
    fn extract_screen_rows_from_parser() {
        let mut parser = vt100::Parser::new(2, 10, 0);
        parser.process(b"hello");
        let rows = extract_screen_rows(parser.screen());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], "hello     ");
        assert_eq!(rows[1], "          ");
    }
}
