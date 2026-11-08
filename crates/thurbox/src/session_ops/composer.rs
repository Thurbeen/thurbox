//! Whether an agent's input line is empty, read off its pane.
//!
//! `session send` types into a pane the operator may be typing into too. Text
//! typed onto their half-written line merges with it ("c" + "fleet
//! reconciler: …" reached a lead as one prompt), and an Enter the agent drops
//! leaves the text sitting unsent while the caller is told it went. Both are
//! answered by one reading: the text left of the cursor on its row.
//!
//! Empty means nothing but prompt chrome is left of the cursor: whitespace, box
//! drawing, and at most one prompt glyph (`❯`, `›`, `>`, `→`, `$`, …). The
//! dimmed placeholder an idle agent draws to the cursor's *right* does not
//! count. Anything else is typed text — including the wrapped end of a long
//! line that happens to finish in `%`, which has no prompt on its row at all.
//!
//! A pane whose foreground is an interactive shell also reads its own prompt as
//! empty: one word ending in a prompt glyph (`bash-5.3$`, `user@box:~$`,
//! `host%`) or PowerShell's `PS C:\…>`. Only there, because in an agent's
//! composer the same row is as likely the wrapped end of a long line.
//!
//! What this cannot see: a cursor moved back into a draft (Home, or the start
//! of a draft's second line) reads as the bare prompt it is sitting after. And
//! a shell's wrapped line that is one word ending in a glyph reads as a prompt.

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

/// How often [`wait_while`] looks again.
const POLL: Duration = Duration::from_millis(100);

/// `text` without the whitespace and box drawing around it.
fn unframed(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{2500}'..='\u{259f}').contains(&c))
}

/// Glyphs a shell's own prompt ends with.
const SHELL_PROMPT_GLYPHS: &[char] = &['$', '%', '#', '>'];

/// Whether `line` is a shell prompt with nothing typed after it: one word
/// ending in a glyph, or PowerShell's `PS <path>>`, whose path may hold spaces
/// but never a `>`.
fn is_shell_prompt(line: &str) -> bool {
    let one_word =
        !line.contains(char::is_whitespace) && line.ends_with(|c| SHELL_PROMPT_GLYPHS.contains(&c));
    let powershell = line
        .strip_prefix("PS ")
        .and_then(|rest| rest.strip_suffix('>'))
        .is_some_and(|path| !path.contains('>'));
    one_word || powershell
}

/// Classify the text left of the cursor; `shell` when the pane's foreground is
/// an interactive shell, whose own prompt then reads as empty too.
pub fn classify(before_cursor: &str, shell: bool) -> Composer {
    let line = unframed(before_cursor);
    if line.is_empty() || is_bare_prompt(line) || (shell && is_shell_prompt(line)) {
        Composer::Empty
    } else {
        Composer::Holding
    }
}

/// Whether the text left of the cursor is a drawn, empty composer: one prompt
/// glyph and nothing typed. Stricter than [`classify`]'s `Empty`, which also
/// takes a blank line — what a pane shows while its agent is still starting.
pub fn shows_prompt(before_cursor: &str) -> bool {
    is_bare_prompt(unframed(before_cursor))
}

fn is_bare_prompt(line: &str) -> bool {
    let mut chars = line.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(c), None) if PROMPT_GLYPHS.contains(&c)
    )
}

/// Whether the text left of the cursor still ends with what was sent — the
/// line was typed and is sitting there unsubmitted, rather than replaced by
/// something else (a dialog, a collapsed paste) that an Enter must not answer.
pub fn ends_with_sent(before_cursor: &str, sent: &str) -> bool {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<Vec<_>>();
    let left = squash(before_cursor);
    let last = sent.lines().rev().find(|l| !l.trim().is_empty());
    let sent = squash(last.unwrap_or(sent));
    // The row may hold only the wrapped end of a long line, so compare no more
    // than it can show.
    let n = sent.len().min(left.len()).min(12);
    n > 0 && left.ends_with(&sent[sent.len() - n..])
}

/// Read `pane`'s input line once: the text left of the cursor, or `None` when
/// the backend cannot answer.
pub fn read_line(backend: &dyn crate::backend::SessionBackend, pane: &str) -> Option<String> {
    backend.text_before_cursor(pane).ok().flatten()
}

/// Read `pane`'s input line once. A backend that cannot answer is `Unknown`.
pub fn read(backend: &dyn crate::backend::SessionBackend, pane: &str, shell: bool) -> Composer {
    read_line(backend, pane).map_or(Composer::Unknown, |text| classify(&text, shell))
}

/// Whether `pane`'s foreground is an interactive shell, for [`classify`]. A
/// backend that cannot say is not one: the strict rule only ever waits.
pub fn is_shell(backend: &dyn crate::backend::SessionBackend, pane: &str) -> bool {
    backend.pane_state(pane).is_ok_and(|s| {
        crate::session::is_interactive_shell(
            s.foreground_process.as_deref(),
            s.foreground_command.as_deref(),
        )
    })
}

/// Read until the line is no longer `from` or `within` has passed, and return
/// the last reading. `Unknown` returns at once: waiting will not make it known.
pub fn wait_while(
    backend: &dyn crate::backend::SessionBackend,
    pane: &str,
    shell: bool,
    from: Composer,
    within: Duration,
) -> Composer {
    let deadline = Instant::now() + within;
    loop {
        let now = read(backend, pane, shell);
        if now != from || Instant::now() >= deadline {
            return now;
        }
        std::thread::sleep(POLL);
    }
}

/// Read until the line ends with `sent` — the agent has drawn what was typed —
/// or `within` has passed. Whether it did.
pub fn wait_for_sent(
    backend: &dyn crate::backend::SessionBackend,
    pane: &str,
    sent: &str,
    within: Duration,
) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if read_line(backend, pane).is_some_and(|l| ends_with_sent(&l, sent)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_prompt_is_empty_whatever_its_glyph() {
        for line in ["", "  ", "❯ ", "› ", "│ > ", "→ ", "$ ", "┃  "] {
            assert_eq!(classify(line, false), Composer::Empty, "{line:?}");
        }
    }

    #[test]
    fn only_a_drawn_prompt_shows_one() {
        for line in ["❯ ", "› ", "│ > ", "→ "] {
            assert!(shows_prompt(line), "{line:?}");
        }
        for line in ["", "  ", "┃  ", "Starting", "› fix it"] {
            assert!(!shows_prompt(line), "{line:?}");
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
            assert_eq!(classify(line, false), Composer::Holding, "{line:?}");
        }
    }

    #[test]
    fn typed_text_ending_in_a_prompt_glyph_is_still_holding() {
        for line in [
            "❯ fix issue #",
            "› it's at 50%",
            "❯ a -> b>",
            "user@box:~$ echo #",
            // The wrapped end of a long line: no prompt on the row at all.
            "xxxxxxxx coverage fell to 50%",
            "see issue #",
            "a -> b>",
            // A shell prompt with more than its glyph: in an agent's pane,
            // indistinguishable from a wrapped row of text.
            "user@box:~$ ",
        ] {
            assert_eq!(classify(line, false), Composer::Holding, "{line:?}");
        }
    }

    #[test]
    fn a_shell_reads_its_own_prompt_as_empty_and_anything_after_it_as_typed() {
        for line in [
            "bash-5.3$ ",
            "bash-3.2$",
            "user@box:~$ ",
            "host% ",
            "root@box:/# ",
            "C:\\Users\\me>",
            "PS C:\\Users\\me> ",
            "PS C:\\Program Files> ",
            "$ ",
        ] {
            assert_eq!(classify(line, true), Composer::Empty, "{line:?}");
        }
        for line in [
            "bash-5.3$ ec",
            "user@box:~$ echo #",
            "xxxxxxxx coverage fell to 50%",
            "PS C:\\> Get-Item a>",
            "PS C:\\> dir",
        ] {
            assert_eq!(classify(line, true), Composer::Holding, "{line:?}");
        }
    }

    #[test]
    fn the_sent_text_is_recognised_at_the_end_of_the_line_and_nothing_else() {
        assert!(ends_with_sent("❯ run the tests", "run the tests"));
        assert!(ends_with_sent("❯ c run the tests", "run the tests"));
        // Only the wrapped tail of a long line is on the cursor's row.
        assert!(ends_with_sent("ests", "run the tests"));
        assert!(ends_with_sent("❯ second line", "first line\nsecond line"));
        assert!(ends_with_sent("❯ fix the build", "fix the build\n\n"));
        assert!(!ends_with_sent(
            "❯ [Pasted text #1 +4 lines]",
            "run the tests"
        ));
        assert!(!ends_with_sent(" Do you want to proceed?", "run the tests"));
        assert!(!ends_with_sent("❯ ", "run the tests"));
    }
}
