use std::sync::OnceLock;

use regex::Regex;

pub struct DetectedLink {
    pub row: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub url: String,
}

fn url_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?:https?|file)://[^\s<>"'\x60)\]]+"#).unwrap())
}

/// Extract visible rows from a vt100 screen, one string per row.
pub fn extract_screen_rows(screen: &vt100::Screen) -> Vec<String> {
    let (rows, cols) = screen.size();
    (0..rows)
        .map(|row| {
            (0..cols)
                .map(|col| {
                    screen
                        .cell(row, col)
                        .and_then(|c| c.contents().chars().next())
                        .unwrap_or(' ')
                })
                .collect()
        })
        .collect()
}

/// Detect URLs in screen rows, stripping trailing punctuation.
///
/// Positions are character-based (not byte-based) so they map directly
/// to vt100 screen cell columns.
pub fn detect_urls(screen_rows: &[String]) -> Vec<DetectedLink> {
    let re = url_regex();
    let mut links = Vec::new();
    for (row_idx, row) in screen_rows.iter().enumerate() {
        for m in re.find_iter(row) {
            let mut url = m.as_str();
            while url.ends_with(['.', ',', ';', ':', ')', ']']) {
                url = &url[..url.len() - 1];
            }
            // Skip bare scheme-only matches like "http://" with no host
            if url.len() > "https://".len() {
                // Convert byte offsets to character offsets: regex returns byte
                // positions, but we need cell positions (1 char = 1 cell).
                let start_col = row[..m.start()].chars().count();
                let end_col = start_col + url.chars().count();
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
        assert_eq!(links[0].start_col, 2); // char position, not byte position
        assert_eq!(links[0].end_col, 21);
        assert_eq!(url_at_position(&links, 0, 2), Some("https://example.com"));
        assert_eq!(url_at_position(&links, 0, 1), None);
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
