//! Minimal markdown → ratatui renderer for the task description preview.
//!
//! Parses CommonMark with [`pulldown_cmark`] and maps the event stream to
//! styled [`Line`]s: ATX headings (bold + accent), `**bold**`/`*italic*`
//! inline emphasis, inline + fenced code (dimmed), and bullet/ordered lists.
//! Block elements are separated by a blank line; the caller is expected to
//! word-wrap (e.g. via `Paragraph::wrap`), so this module emits logical lines
//! and does not wrap itself.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::theme::Theme;

/// Render markdown `src` into styled lines. Returns an empty vec for empty
/// input (callers typically skip rendering then).
pub fn render_markdown(src: &str) -> Vec<Line<'static>> {
    let mut r = Renderer::default();
    for ev in Parser::new_ext(src, Options::empty()) {
        r.event(ev);
    }
    r.finish()
}

#[derive(Default)]
struct Renderer {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    bold: u32,
    italic: u32,
    heading: bool,
    in_code_block: bool,
    /// Blockquote nesting depth; quoted lines get a `▏ ` gutter + dim italic.
    blockquote: u32,
    /// One entry per open list; `Some(n)` is the next ordered number, `None` is
    /// an unordered list.
    list_stack: Vec<Option<u64>>,
    /// List-item marker (with indent) to emit before the item's first text.
    pending_prefix: Option<String>,
    /// Destination of the link currently being rendered, appended (dimmed) when
    /// the link closes.
    pending_link_url: Option<String>,
}

impl Renderer {
    fn event(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => self.current.push(Span::styled(
                format!("`{t}`"),
                Style::default().fg(Theme::text_secondary()),
            )),
            Event::SoftBreak | Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.flush_line();
                self.lines.push(Line::from(Span::styled(
                    "─".repeat(24),
                    Style::default().fg(Theme::text_secondary()),
                )));
                self.push_blank();
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { level, .. } => {
                self.heading = true;
                // Prefix the heading with its `#`-depth markers for hierarchy.
                let depth = heading_depth(level);
                self.current.push(Span::styled(
                    format!("{} ", "#".repeat(depth)),
                    heading_style(),
                ));
            }
            Tag::Strong => self.bold += 1,
            Tag::Emphasis => self.italic += 1,
            Tag::Link { dest_url, .. } => self.pending_link_url = Some(dest_url.to_string()),
            Tag::BlockQuote(_) => {
                self.flush_line();
                self.blockquote += 1;
            }
            Tag::CodeBlock(_) => {
                self.flush_line();
                self.in_code_block = true;
            }
            Tag::List(start) => self.list_stack.push(start),
            Tag::Item => {
                let depth = self.list_stack.len().saturating_sub(1);
                let indent = "  ".repeat(depth);
                let marker = match self.list_stack.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                self.pending_prefix = Some(format!("{indent}{marker}"));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.heading = false;
                self.flush_line();
                self.push_blank();
            }
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Link => {
                if let Some(url) = self.pending_link_url.take() {
                    self.current.push(Span::styled(
                        format!(" ({url})"),
                        Style::default().fg(Theme::text_secondary()),
                    ));
                }
            }
            TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.blockquote = self.blockquote.saturating_sub(1);
                if self.blockquote == 0 {
                    self.push_blank();
                }
            }
            TagEnd::CodeBlock => {
                self.flush_line();
                self.in_code_block = false;
                self.push_blank();
            }
            TagEnd::Paragraph => {
                self.flush_line();
                // Inside a list, keep items tight (no blank separator).
                if self.list_stack.is_empty() {
                    self.push_blank();
                }
            }
            TagEnd::Item => self.flush_line(),
            TagEnd::List(_) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.push_blank();
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, t: &str) {
        if self.in_code_block {
            let mut parts = t.split('\n').peekable();
            while let Some(part) = parts.next() {
                if !part.is_empty() {
                    self.current.push(Span::styled(
                        part.to_string(),
                        Style::default().fg(Theme::text_secondary()),
                    ));
                }
                if parts.peek().is_some() {
                    self.flush_line();
                }
            }
            return;
        }
        if let Some(prefix) = self.pending_prefix.take() {
            self.current.push(Span::styled(prefix, Theme::label()));
        }
        self.current
            .push(Span::styled(t.to_string(), self.current_style()));
    }

    fn current_style(&self) -> Style {
        // Heading text shares the marker's accent + bold styling so the whole
        // line reads as a heading, not just the `#` prefix.
        let mut s = if self.heading {
            heading_style()
        } else if self.blockquote > 0 {
            // Quoted text reads as a muted aside.
            Style::default()
                .fg(Theme::text_secondary())
                .add_modifier(Modifier::ITALIC)
        } else {
            Style::default().fg(Theme::text_primary())
        };
        if self.bold > 0 {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            s = s.add_modifier(Modifier::ITALIC);
        }
        s
    }

    /// Push the in-progress spans as a line and reset (a no-op produces a blank
    /// line, which is how soft/hard breaks render). Quoted lines get a `▏`
    /// gutter prepended.
    fn flush_line(&mut self) {
        let mut spans = std::mem::take(&mut self.current);
        if self.blockquote > 0 && !spans.is_empty() {
            spans.insert(
                0,
                Span::styled("▏ ", Style::default().fg(Theme::text_secondary())),
            );
        }
        self.lines.push(Line::from(spans));
    }

    /// Append a single blank separator line, collapsing consecutive blanks.
    fn push_blank(&mut self) {
        if self
            .lines
            .last()
            .is_some_and(|l| l.spans.is_empty() || l.width() == 0)
        {
            return;
        }
        self.lines.push(Line::default());
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if !self.current.is_empty() {
            self.flush_line();
        }
        while self
            .lines
            .last()
            .is_some_and(|l| l.spans.is_empty() || l.width() == 0)
        {
            self.lines.pop();
        }
        self.lines
    }
}

fn heading_depth(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn heading_style() -> Style {
    Style::default()
        .fg(Theme::border_focused())
        .add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Concatenate a line's span contents for assertion convenience.
    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn renders_heading_bold_list_and_code() {
        let src = "# Title\n\nSome **bold** text\n\n- one\n- two\n\n```\ncode\n```";
        let lines = render_markdown(src);
        let texts: Vec<String> = lines.iter().map(line_text).collect();

        // Heading carries its marker and the whole line — marker *and* text —
        // is bold + accent (regression: the title body must be styled too).
        assert_eq!(texts[0], "# Title");
        assert!(lines[0]
            .spans
            .iter()
            .all(|s| s.style.add_modifier.contains(Modifier::BOLD)
                && s.style.fg == Some(Theme::border_focused())));

        // Bold span exists somewhere in the paragraph.
        assert!(lines.iter().any(|l| l.spans.iter().any(
            |s| s.content.as_ref() == "bold" && s.style.add_modifier.contains(Modifier::BOLD)
        )));

        // Bullets are rendered with a marker.
        assert!(texts.iter().any(|t| t == "• one"));
        assert!(texts.iter().any(|t| t == "• two"));
        // Code-block content is preserved.
        assert!(texts.iter().any(|t| t == "code"));
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(render_markdown("").is_empty());
    }

    #[test]
    fn renders_blockquote_with_gutter() {
        let lines = render_markdown("> quoted line");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("▏ ") && t.contains("quoted line")),
            "blockquote should get a gutter marker, got {texts:?}"
        );
    }

    #[test]
    fn renders_link_text_and_url() {
        let lines = render_markdown("see [docs](https://example.com)");
        let joined: String = lines.iter().map(line_text).collect();
        assert!(joined.contains("docs"), "link text kept");
        assert!(joined.contains("https://example.com"), "url appended");
    }

    #[test]
    fn renders_thematic_break() {
        let lines = render_markdown("a\n\n---\n\nb");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.chars().all(|c| c == '─') && !t.is_empty()),
            "a horizontal rule renders as a divider, got {texts:?}"
        );
    }
}
