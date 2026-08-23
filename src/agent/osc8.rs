//! OSC 8 hyperlink capture from the agent's byte stream.
//!
//! `vt100` has no notion of a hyperlink: it drops the escape and keeps only the
//! printed label, so the URL would be lost. The sequence is a *pair* — an
//! opening `OSC 8 ; <params> ; <uri>`, the label's glyphs, then a closing
//! `OSC 8 ; ; ` with no URI — and both halves arrive at
//! [`vt100::Callbacks::unhandled_osc`] with the live screen. The label is
//! therefore read straight off the screen between the cursor position at the
//! open and the one at the close (the escape arrives *after* its label
//! printed), and stored in the session's [`HyperlinkTable`] for a `Ctrl+Click`
//! to resolve.

use crate::session::hyperlink::HyperlinkTable;

/// An OSC 8 run whose closing sequence has not arrived yet: its target plus the
/// cursor position where its label started printing.
#[derive(Clone, Debug)]
pub(crate) struct PendingHyperlink {
    url: String,
    row: u16,
    col: u16,
    /// See [`scroll_witness`].
    witness: String,
}

/// The URI an `OSC 8 ; <params> ; <uri>` sequence carries. `fields` are the OSC
/// fields after the `8`: the first holds the link params (`id=…`) and
/// everything after it is the URI — rejoined because `vte` splits fields on
/// `;`, which a URI may legitimately contain. Empty (or absent) means the
/// sequence closes the open run.
pub(crate) fn uri_from_fields(fields: &[&[u8]]) -> String {
    let uri = fields
        .iter()
        .skip(1)
        .map(|field| String::from_utf8_lossy(field))
        .collect::<Vec<_>>()
        .join(";");
    uri.trim().to_string()
}

/// Handle one OSC 8 sequence. `uri` empty closes the open run; a non-empty one
/// closes it too (an unterminated run is still worth keeping) and opens the
/// next.
pub(crate) fn handle(
    screen: &vt100::Screen,
    pending: &mut Option<PendingHyperlink>,
    table: &mut HyperlinkTable,
    uri: &str,
) {
    if let Some(open) = pending.take() {
        close_run(screen, table, open);
    }
    if !uri.is_empty() {
        let (row, col) = screen.cursor_position();
        *pending = Some(PendingHyperlink {
            url: uri.to_string(),
            row,
            col,
            witness: scroll_witness(screen, row, col),
        });
    }
}

/// Record the label `open` printed, one entry per screen row it covers (a run
/// long enough to wrap spans several).
fn close_run(screen: &vt100::Screen, table: &mut HyperlinkTable, open: PendingHyperlink) {
    let (end_row, end_col) = screen.cursor_position();
    // The label is no longer where it printed — the screen scrolled under the
    // run, or was cleared — so there is nothing to anchor to.
    if end_row < open.row || scroll_witness(screen, open.row, open.col) != open.witness {
        return;
    }
    let (_, cols) = screen.size();
    for row in open.row..=end_row {
        let from = if row == open.row { open.col } else { 0 };
        // The cursor sits on the cell the *next* glyph goes to, so `end_col` is
        // exclusive. A label that fills the row to its edge leaves the cursor
        // parked on the last column (pending wrap) and loses that one cell:
        // the click target ends up a cell short, never pointing at the wrong
        // run.
        let to = if row == end_row { end_col } else { cols };
        let label = row_glyphs(screen, row, from, to);
        table.record(&open.url, label.trim_end(), from as usize);
        // A run reaches the next row only by wrapping. Anything else moved the
        // cursor down on its own (a newline inside the label), which leaves the
        // rows below holding text this run never printed — so stop rather than
        // claim them.
        if row < end_row && !screen.row_wrapped(row) {
            break;
        }
    }
}

/// The screen text a run's label cannot touch: the glyphs before it on its own
/// row, plus the whole row above. A label only ever prints forward from its
/// start, so that region holds still for the run's lifetime — unless the label
/// wraps past the bottom row (or carries a newline) and scrolls the screen,
/// which shifts other content into it. Comparing the witness at the close is
/// how a run learns its recorded start no longer points at its own label; it is
/// then dropped rather than anchored to whatever text now sits there.
fn scroll_witness(screen: &vt100::Screen, row: u16, col: u16) -> String {
    let mut witness = row_glyphs(screen, row, 0, col);
    if row > 0 {
        let (_, cols) = screen.size();
        witness.push('\n');
        witness.push_str(&row_glyphs(screen, row - 1, 0, cols));
    }
    witness
}

/// Glyphs printed in `row` between cell columns `from`..`to`. Wide-glyph
/// continuation cells are skipped, so a captured label compares equal to the
/// row strings `kernel::terminal::links::extract_screen_rows` builds when a
/// click or a repaint resolves it.
///
/// The two readers are deliberately separate: `agent` may not depend on
/// `kernel` (see the module rules in `tests/architecture_rules.rs`), and the
/// type they share these strings through — `session::hyperlink` — is
/// vt100-free pure data. Keep them in step rather than merging them.
fn row_glyphs(screen: &vt100::Screen, row: u16, from: u16, to: u16) -> String {
    let mut out = String::new();
    for col in from..to {
        match screen.cell(row, col) {
            Some(cell) if cell.is_wide_continuation() => continue,
            Some(cell) => out.push(cell.contents().chars().next().unwrap_or(' ')),
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a parser the way [`crate::agent::TermSignals`] does, so these
    /// tests exercise the real escape → table path.
    #[derive(Default)]
    struct Capture {
        pending: Option<PendingHyperlink>,
        table: HyperlinkTable,
    }

    impl vt100::Callbacks for Capture {
        fn unhandled_osc(&mut self, screen: &mut vt100::Screen, params: &[&[u8]]) {
            if let [b"8", fields @ ..] = params {
                let uri = uri_from_fields(fields);
                handle(screen, &mut self.pending, &mut self.table, &uri);
            }
        }
    }

    fn feed(rows: u16, cols: u16, bytes: &[u8]) -> vt100::Parser<Capture> {
        let mut parser = vt100::Parser::new_with_callbacks(rows, cols, 0, Capture::default());
        parser.process(bytes);
        parser
    }

    fn screen_row(parser: &vt100::Parser<Capture>, row: u16) -> String {
        let (_, cols) = parser.screen().size();
        row_glyphs(parser.screen(), row, 0, cols)
    }

    #[test]
    fn captures_a_bel_terminated_run() {
        let parser = feed(
            4,
            40,
            b"see \x1b]8;;https://github.com\x07Github\x1b]8;;\x07 done",
        );
        let row = screen_row(&parser, 0);
        assert_eq!(row.trim_end(), "see Github done");
        let table = &parser.callbacks().table;
        assert_eq!(table.resolve(&row, 4), Some("https://github.com"));
        assert_eq!(table.resolve(&row, 9), Some("https://github.com"));
        // Only the label's own cells are clickable.
        assert_eq!(table.resolve(&row, 3), None);
        assert_eq!(table.resolve(&row, 10), None);
    }

    #[test]
    fn captures_an_st_terminated_run_with_params_and_a_uri_semicolon() {
        // `id=…` link params must not be mistaken for the URI, and a URI
        // carrying its own `;` arrives split across several OSC fields.
        let parser = feed(
            4,
            40,
            b"\x1b]8;id=1;https://example.com/a;b=c\x1b\\Docs\x1b]8;;\x1b\\",
        );
        let row = screen_row(&parser, 0);
        assert_eq!(
            parser.callbacks().table.resolve(&row, 0),
            Some("https://example.com/a;b=c")
        );
    }

    #[test]
    fn captures_each_row_of_a_wrapped_run() {
        // The label runs past the right edge, so it lands on two rows and each
        // gets its own entry.
        let parser = feed(
            4,
            10,
            b"ab\x1b]8;;https://example.com\x07cdefghijkl\x1b]8;;\x07",
        );
        let table = &parser.callbacks().table;
        let first = screen_row(&parser, 0);
        let second = screen_row(&parser, 1);
        assert_eq!(table.resolve(&first, 2), Some("https://example.com"));
        assert_eq!(table.resolve(&first, 9), Some("https://example.com"));
        assert_eq!(table.resolve(&second, 0), Some("https://example.com"));
        assert_eq!(table.resolve(&second, 1), Some("https://example.com"));
    }

    #[test]
    fn wide_glyphs_before_a_label_shift_its_column() {
        let parser = feed(
            2,
            40,
            "漢字 \x1b]8;;https://example.com\x07Github\x1b]8;;\x07".as_bytes(),
        );
        let row = screen_row(&parser, 0);
        let table = &parser.callbacks().table;
        assert_eq!(table.resolve(&row, 5), Some("https://example.com"));
        assert_eq!(table.resolve(&row, 4), None);
    }

    #[test]
    fn an_unclosed_run_is_flushed_by_the_next_one() {
        let parser = feed(
            2,
            40,
            b"\x1b]8;;https://a.example\x07one \x1b]8;;https://b.example\x07two\x1b]8;;\x07",
        );
        let row = screen_row(&parser, 0);
        let table = &parser.callbacks().table;
        assert_eq!(table.resolve(&row, 0), Some("https://a.example"));
        assert_eq!(table.resolve(&row, 4), Some("https://b.example"));
    }

    #[test]
    fn a_label_carrying_a_newline_claims_only_the_row_it_printed_on() {
        let parser = feed(
            4,
            10,
            b"\x1b]8;;https://example.com\x07hi\r\nmore\x1b]8;;\x07",
        );
        let table = &parser.callbacks().table;
        assert_eq!(
            table.resolve(&screen_row(&parser, 0), 0),
            Some("https://example.com")
        );
        // The cursor moved down on its own, so "more" is not part of the run.
        assert_eq!(table.resolve(&screen_row(&parser, 1), 0), None);
    }

    #[test]
    fn a_run_that_printed_nothing_records_nothing() {
        let parser = feed(2, 40, b"\x1b]8;;https://example.com\x07\x1b]8;;\x07");
        assert_eq!(parser.callbacks().table.runs().count(), 0);
    }

    #[test]
    fn a_run_the_screen_scrolled_under_is_dropped() {
        // Opening on the last row and printing past it scrolls the label above
        // its recorded row, leaving nothing to anchor to.
        let parser = feed(
            2,
            10,
            b"\r\n\x1b]8;;https://example.com\x07link\r\nmore\x1b]8;;\x07",
        );
        assert_eq!(parser.callbacks().table.runs().count(), 0);
    }
}
