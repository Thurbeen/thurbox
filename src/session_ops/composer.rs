//! Whether an agent's input line is empty, read off its pane.
//!
//! `session send` types into a pane the operator may be typing into too. Text
//! typed onto their half-written line merges with it ("c" + "fleet
//! reconciler: …" reached a lead as one prompt), and an Enter the agent drops
//! leaves the text sitting unsent while the caller is told it went. Both are
//! answered by one reading: the text left of the cursor on its row.
//!
//! Empty means nothing but prompt chrome is left of the cursor — a prompt glyph
//! (`❯`, `›`, `>`, `→`, a shell's `$`/`%`/`#`) or box drawing — so the dimmed
//! placeholder an idle agent draws to the cursor's *right* does not count as
//! input.

use std::time::{Duration, Instant};

/// What the input line holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Composer {
    Empty,
    Holding,
    /// The backend could not say where the cursor is.
    Unknown,
}

/// Glyphs an input line's prompt ends with, across the agents thurbox runs and
/// a plain shell.
const PROMPT_GLYPHS: &[char] = &['❯', '›', '>', '→', '»', '$', '%', '#'];

/// How often [`wait_for`] looks again.
const POLL: Duration = Duration::from_millis(100);

/// Classify the text left of the cursor.
pub fn classify(before_cursor: &str) -> Composer {
    let typed = before_cursor
        .trim_end_matches(|c: char| c.is_whitespace() || ('\u{2500}'..='\u{259f}').contains(&c));
    if typed.is_empty() || typed.ends_with(PROMPT_GLYPHS) {
        Composer::Empty
    } else {
        Composer::Holding
    }
}

/// Read `pane`'s input line once. A backend that cannot answer is `Unknown`.
pub fn read(backend: &dyn crate::backend::SessionBackend, pane: &str) -> Composer {
    match backend.text_before_cursor(pane) {
        Ok(Some(text)) => classify(&text),
        Ok(None) | Err(_) => Composer::Unknown,
    }
}

/// Read until the line is `Empty` or `within` has passed, and return the last
/// reading. `Unknown` returns at once: waiting will not make it known.
pub fn wait_for_empty(
    backend: &dyn crate::backend::SessionBackend,
    pane: &str,
    within: Duration,
) -> Composer {
    let deadline = Instant::now() + within;
    loop {
        let now = read(backend, pane);
        if now != Composer::Holding || Instant::now() >= deadline {
            return now;
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_prompt_is_empty_whatever_its_glyph() {
        for line in ["", "  ", "❯ ", "› ", "│ > ", "→ ", "user@box:~$ ", "┃  "] {
            assert_eq!(classify(line), Composer::Empty, "{line:?}");
        }
    }

    #[test]
    fn anything_typed_after_the_prompt_is_holding() {
        for line in [
            "❯ c",
            "› fix the",
            "│ > hello │",
            "$ ls",
            "  continued text",
        ] {
            assert_eq!(classify(line), Composer::Holding, "{line:?}");
        }
    }
}
