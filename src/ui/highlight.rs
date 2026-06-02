//! Shared fuzzy-match highlight rendering.
//!
//! Builds ratatui [`Span`]s that emphasise the characters a fuzzy query matched
//! (accent colour + bold + underline) against a base style, leaving the rest of
//! the string in the base style. Used by the session list, tasks panel, and
//! automations pane so search highlighting looks identical everywhere.

use ratatui::{
    style::{Modifier, Style},
    text::Span,
};

use super::theme::Theme;

/// A contiguous byte range of `text` and whether it is a matched (highlighted)
/// run. The two public span builders share this single segmentation pass, so
/// borrowed and owned outputs can never drift apart.
struct Segment {
    start: usize,
    end: usize,
    highlighted: bool,
}

/// Base style for a list row before fuzzy-match highlighting is layered on top.
///
/// The precedence — `selected` > `dimmed` (a row filtered out by the active
/// search) > `normal` — is shared by the session list, tasks panel, and
/// automations pane so the three lists feel identical. `normal` is the
/// caller-specific resting style (e.g. status colour, or enabled/disabled).
pub(crate) fn row_base_style(selected: bool, dimmed: bool, normal: Style) -> Style {
    if selected {
        Theme::selected_item()
    } else if dimmed {
        Style::default()
            .fg(Theme::text_muted())
            .add_modifier(Modifier::DIM)
    } else {
        normal
    }
}

/// Style applied to matched characters: accent foreground + bold + underline.
fn highlight_style(base_style: Style) -> Style {
    base_style
        .fg(Theme::accent())
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

/// Split `text` into highlighted/plain runs at the matched byte `positions`
/// (UTF-8 offsets, as returned by [`crate::fuzzy::fuzzy_match`]). Out-of-range
/// positions stop the scan; the trailing remainder is emitted as a plain run.
fn segments(text: &str, positions: &[usize]) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut last_end = 0;
    for &byte_pos in positions {
        if byte_pos > text.len() {
            break;
        }
        if let Some(ch) = text[byte_pos..].chars().next() {
            let char_len = ch.len_utf8();
            if byte_pos > last_end {
                segments.push(Segment {
                    start: last_end,
                    end: byte_pos,
                    highlighted: false,
                });
            }
            segments.push(Segment {
                start: byte_pos,
                end: byte_pos + char_len,
                highlighted: true,
            });
            last_end = byte_pos + char_len;
        }
    }
    if last_end < text.len() {
        segments.push(Segment {
            start: last_end,
            end: text.len(),
            highlighted: false,
        });
    }
    segments
}

/// Build spans for `text` with the bytes at `positions` highlighted over
/// `base_style`. `positions` are UTF-8 byte offsets (as returned by
/// [`crate::fuzzy::fuzzy_match`]).
pub(crate) fn highlighted_spans<'a>(
    text: &'a str,
    positions: &[usize],
    base_style: Style,
) -> Vec<Span<'a>> {
    let highlighted = highlight_style(base_style);
    segments(text, positions)
        .into_iter()
        .map(|seg| {
            let style = if seg.highlighted {
                highlighted
            } else {
                base_style
            };
            Span::styled(&text[seg.start..seg.end], style)
        })
        .collect()
}

/// Append `text` to `out`, highlighting `match_positions` when present,
/// otherwise pushing the whole string in `style`.
pub(crate) fn append_highlighted<'a>(
    out: &mut Vec<Span<'a>>,
    text: &'a str,
    match_positions: Option<&[usize]>,
    style: Style,
) {
    match match_positions {
        Some(positions) if !positions.is_empty() => {
            out.extend(highlighted_spans(text, positions, style));
        }
        _ => out.push(Span::styled(text, style)),
    }
}

/// Owned-span variant of [`highlighted_spans`]: returns `Span<'static>` so the
/// result can outlive a local `String` (e.g. a per-row truncated label built
/// inside a render closure).
pub(crate) fn highlighted_spans_owned(
    text: &str,
    positions: &[usize],
    base_style: Style,
) -> Vec<Span<'static>> {
    let highlighted = highlight_style(base_style);
    segments(text, positions)
        .into_iter()
        .map(|seg| {
            let style = if seg.highlighted {
                highlighted
            } else {
                base_style
            };
            Span::styled(text[seg.start..seg.end].to_string(), style)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(spans: &[Span]) -> Vec<String> {
        spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn no_positions_is_single_span() {
        let spans = highlighted_spans("hello", &[], Style::default());
        assert_eq!(texts(&spans), vec!["hello"]);
    }

    #[test]
    fn splits_around_matched_bytes() {
        // Match 'f' (0) and 'b' (4) in "foo-bar".
        let spans = highlighted_spans("foo-bar", &[0, 4], Style::default());
        assert_eq!(texts(&spans), vec!["f", "oo-", "b", "ar"]);
    }

    #[test]
    fn owned_variant_matches_borrowed() {
        let owned = highlighted_spans_owned("foo-bar", &[0, 4], Style::default());
        assert_eq!(texts(&owned), vec!["f", "oo-", "b", "ar"]);
        // Empty positions → one span.
        let owned = highlighted_spans_owned("hi", &[], Style::default());
        assert_eq!(texts(&owned), vec!["hi"]);
    }

    #[test]
    fn out_of_range_positions_are_ignored() {
        let spans = highlighted_spans("ab", &[99], Style::default());
        assert_eq!(texts(&spans), vec!["ab"]);
        // The owned variant ignores them the same way.
        let owned = highlighted_spans_owned("ab", &[99], Style::default());
        assert_eq!(texts(&owned), vec!["ab"]);
    }

    #[test]
    fn row_base_style_precedence_selected_over_dimmed_over_normal() {
        let normal = Style::default().fg(ratatui::style::Color::Green);
        // Selected wins regardless of dimmed.
        assert_eq!(row_base_style(true, true, normal), Theme::selected_item());
        assert_eq!(row_base_style(true, false, normal), Theme::selected_item());
        // Dimmed (but not selected) is the muted+DIM style, not `normal`.
        let dimmed = row_base_style(false, true, normal);
        assert_eq!(dimmed.fg, Some(Theme::text_muted()));
        assert!(dimmed.add_modifier.contains(Modifier::DIM));
        // Otherwise the caller's resting style passes through unchanged.
        assert_eq!(row_base_style(false, false, normal), normal);
    }
}
