//! Rebuilding a paste on Windows, where the console delivers no `Event::Paste`.
//!
//! crossterm's Windows backend never emits `Event::Paste` — its
//! `EnableBracketedPaste` is documented as unsupported — so a paste arrives as a
//! stream of ordinary key events, and a multi-line one submitted the prompt a
//! line at a time. This turns that stream back into one paste before it is
//! dispatched.
//!
//! The design rests on what the console actually delivers, measured against this
//! environment's `ssh` → ConPTY path with a key dumper rather than assumed:
//!
//! * **The bracketed-paste markers are gone.** The ConPTY strips `ESC[200~` /
//!   `ESC[201~` before the app sees them; a paste is plain `Char`/`Enter`
//!   events with no framing at all. There is nothing to match on.
//! * **Editing keys arrive whole.** An arrow key, `Delete`, `Ctrl+Delete`, a
//!   function key each arrive as their own `KeyEvent` (`KeyCode::Left`, …), not
//!   as a run of `ESC` `[` `…` character events.
//!
//! Two rules, both deliberately small:
//!
//! * **Grouping is by timing.** A plain character joins a run when it arrives
//!   within [`MACHINE_GAP`] of the last one; **everything else passes straight
//!   through the instant it arrives** — an arrow key, `Delete`, `Esc`, any
//!   `Ctrl`/`Alt` chord is never held, never matched against a marker, never at
//!   risk of being swallowed. The earlier attempts carried a marker/`Esc`/
//!   VT-sequence state machine that fired on none of these inputs yet stood
//!   between every one of them and the agent; it is gone.
//! * **A run is a paste only if it carries an interior newline.** Sending
//!   newlines to the agent one keystroke at a time is the sole thing this path
//!   exists to prevent, so it is the sole thing it acts on. A newline-free run
//!   — fast typing, quick `j`/`k`, two keys a slow frame batched — is emitted
//!   as the keys it was, because the clock times when a key is *read*, not when
//!   it was pressed, and under load ordinary input bunches up. Coalescing that
//!   on length alone is what announced a phantom "pasted 2 characters" mid-type.
//!
//! The outbound half of the journey is `agent::control_mode`'s `PsmuxPaste`,
//! which carries the reassembled paste to a psmux pane in one piece (ADR-13);
//! the rationale for both halves is ADR-4.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

/// What the key stream resolved to, in the order it must be dispatched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Input {
    Key(KeyEvent),
    Paste(String),
}

/// Two keys closer together than this were machine-fed, not typed. A pasted
/// stream runs orders of magnitude under it; a person's fastest two keys stay
/// well above it. It is also the most the first key of any run is delayed —
/// under one frame, so typing does not feel it.
const MACHINE_GAP: Duration = Duration::from_millis(10);

/// Reassembles a paste from a Windows key stream. Inert everywhere else, where
/// crossterm delivers `Event::Paste` on its own.
pub(crate) struct PasteBurst {
    active: bool,
    /// Plain-character keys gathered so far as a possible paste.
    run: Vec<KeyEvent>,
    /// When the last key arrived, for the gap that tells paste from typing.
    last: Option<Instant>,
}

impl PasteBurst {
    /// The coalescer this platform needs: real on Windows, inert elsewhere.
    pub(crate) fn for_platform() -> Self {
        Self::new(cfg!(windows))
    }

    fn new(active: bool) -> Self {
        Self {
            active,
            run: Vec::new(),
            last: None,
        }
    }

    /// How long the loop should wait for the next key before flushing the
    /// batch: [`MACHINE_GAP`] while a run is open (its next key may still be on
    /// the way), nothing otherwise — so an idle interface polls exactly as it
    /// did.
    pub(crate) fn drain_timeout(&self) -> Duration {
        if self.active && !self.run.is_empty() {
            MACHINE_GAP
        } else {
            Duration::ZERO
        }
    }

    /// Feed one key press, with the instant it arrived.
    pub(crate) fn push(&mut self, key: KeyEvent, at: Instant) -> Vec<Input> {
        if !self.active {
            return vec![Input::Key(key)];
        }
        let gap = self.last.map(|last| at.saturating_duration_since(last));
        self.last = Some(at);

        // Anything that is not a plain character cannot be part of a paste:
        // end the run and pass the key on immediately, in order. This is the
        // line that keeps arrows, Delete, Esc and every chord working —
        // nothing here can hold one back.
        if paste_char(key).is_none() {
            let mut out = self.flush();
            out.push(Input::Key(key));
            return out;
        }

        // A character that arrived slowly ends the previous run before starting
        // its own; one that arrived fast continues the burst.
        let mut out = Vec::new();
        if gap.map_or(true, |gap| gap >= MACHINE_GAP) {
            out = self.flush();
        }
        self.run.push(key);
        out
    }

    /// Nothing more is coming within the window, so decide what the run was.
    ///
    /// The signal is a **line break with content after it**, and only that.
    /// Delivering newlines to the agent one keystroke at a time is the sole
    /// thing this path exists to prevent — it is what submits a prompt
    /// mid-paste — so a run carrying an interior newline is the paste, handed
    /// over whole.
    ///
    /// Everything else is emitted as the keys it was: fast typing, quick
    /// `j`/`k` navigation, or two keys an event queue batched during a slow
    /// frame all read as one rapid run, and calling any of them a paste is what
    /// produced a phantom "pasted 2 characters" while someone was only typing.
    /// A run that merely *ends* at a newline is a typed line submitted with
    /// `Enter`; it must still submit, so it too stays keys. A newline-free
    /// paste loses nothing by arriving as keystrokes — the text lands in the
    /// prompt identically.
    pub(crate) fn flush(&mut self) -> Vec<Input> {
        if self.run.len() >= 2 {
            let text: String = self.run.iter().filter_map(|key| paste_char(*key)).collect();
            if text.trim_end_matches(['\r', '\n']).contains(['\r', '\n']) {
                self.run.clear();
                return vec![Input::Paste(text)];
            }
        }
        self.run.drain(..).map(Input::Key).collect()
    }
}

/// The character a key contributes to pasted text, or `None` for a key that is
/// not text — which ends a run and passes straight through.
///
/// `Enter` becomes CR because that is what a terminal's own bracketed paste puts
/// between two pasted lines, so an agent sees the same bytes on either platform.
/// A modifier other than SHIFT means a chord, which is never paste text.
fn paste_char(key: KeyEvent) -> Option<char> {
    let plain = (key.modifiers - KeyModifiers::SHIFT).is_empty();
    match key.code {
        KeyCode::Char(ch) if plain => Some(ch),
        KeyCode::Enter if plain => Some('\r'),
        KeyCode::Tab if plain => Some('\t'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn events(text: &str) -> Vec<KeyEvent> {
        text.chars()
            .map(|c| match c {
                '\r' => key(KeyCode::Enter),
                '\t' => key(KeyCode::Tab),
                c => ch(c),
            })
            .collect()
    }

    fn as_keys(text: &str) -> Vec<Input> {
        events(text).into_iter().map(Input::Key).collect()
    }

    /// Drives a clock: every key lands `gap` after the one before it, which is
    /// the whole of what separates a paste from typing.
    struct Feed {
        burst: PasteBurst,
        now: Instant,
    }

    impl Feed {
        fn new(active: bool) -> Self {
            Self {
                burst: PasteBurst::new(active),
                now: Instant::now(),
            }
        }

        fn typed(&mut self, text: &str) -> Vec<Input> {
            self.feed(events(text), Duration::from_millis(80))
        }

        fn pasted(&mut self, text: &str) -> Vec<Input> {
            self.feed(events(text), Duration::from_millis(1))
        }

        fn feed(&mut self, keys: Vec<KeyEvent>, gap: Duration) -> Vec<Input> {
            let mut out = Vec::new();
            for key in keys {
                self.now += gap;
                out.extend(self.burst.push(key, self.now));
            }
            out
        }

        fn settle(&mut self) -> Vec<Input> {
            self.burst.flush()
        }
    }

    #[test]
    fn inert_off_windows_passes_every_key_through() {
        let mut feed = Feed::new(false);
        let mut out = feed.pasted("a\rb");
        out.extend(feed.settle());
        assert_eq!(out, as_keys("a\rb"));
    }

    /// The bug, and the shape it has to arrive in: a pasted block is ONE paste,
    /// so an agent renders it as a paste rather than as typing.
    #[test]
    fn a_pasted_block_is_one_paste() {
        let mut feed = Feed::new(true);
        let mut out = feed.pasted("Bonjour\r\rIl y a plusieurs lignes\rdans ce texte");
        out.extend(feed.settle());
        assert_eq!(
            out,
            vec![Input::Paste(
                "Bonjour\r\rIl y a plusieurs lignes\rdans ce texte".to_string()
            )]
        );
    }

    /// No line may submit while a paste is still arriving — the "Bonjour went
    /// alone" failure. Delivered one key at a time, nothing queued behind.
    #[test]
    fn no_line_submits_mid_paste() {
        let mut feed = Feed::new(true);
        let mut out = feed.pasted("Bonjour\r\rIl y a\rdans ce texte\rpour un exemple");
        out.extend(feed.settle());
        assert!(
            !out.contains(&Input::Key(key(KeyCode::Enter))),
            "an Enter reached the agent mid-paste: {out:?}"
        );
        assert_eq!(out.len(), 1, "should be exactly one paste: {out:?}");
    }

    #[test]
    fn typing_stays_keys_and_still_submits() {
        let mut feed = Feed::new(true);
        let mut out = feed.typed("yes\r");
        out.extend(feed.settle());
        assert_eq!(out, as_keys("yes\r"));
    }

    /// A slow frame can batch a whole typed line; the trailing newline is what
    /// keeps it submitting rather than becoming a paste.
    #[test]
    fn a_batched_typed_line_still_submits() {
        let mut feed = Feed::new(true);
        let mut out = feed.pasted("yes\r");
        out.extend(feed.settle());
        assert_eq!(out, as_keys("yes\r"));
    }

    /// A pasted single command is let through so it runs — what pasting one is
    /// for.
    #[test]
    fn a_pasted_single_line_submits() {
        let mut feed = Feed::new(true);
        let mut out = feed.pasted("ls -la\r");
        out.extend(feed.settle());
        assert_eq!(out, as_keys("ls -la\r"));
    }

    /// A newline-free burst is not a paste, however fast it arrived — this is
    /// the reported bug, where two quick keys raised a phantom "pasted 2
    /// characters" while someone was only typing or navigating. It goes out as
    /// the keys it was; the text lands in the prompt just the same.
    #[test]
    fn a_newline_free_burst_stays_keys() {
        let mut feed = Feed::new(true);
        let mut out = feed.pasted("jk");
        out.extend(feed.settle());
        assert_eq!(out, as_keys("jk"));

        let mut feed = Feed::new(true);
        let mut out = feed.pasted("some pasted words");
        out.extend(feed.settle());
        assert_eq!(out, as_keys("some pasted words"));
    }

    /// The regression this rewrite is meant to make impossible: an arrow key,
    /// `Delete`, `Ctrl+Delete`, `Esc` all pass straight through, in order, with
    /// nothing held and no wait left behind.
    #[test]
    fn editing_keys_pass_straight_through() {
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Delete,
            KeyCode::Home,
            KeyCode::Esc,
            KeyCode::Backspace,
        ] {
            let mut feed = Feed::new(true);
            let out = feed.feed(vec![key(code)], Duration::from_millis(1));
            assert_eq!(
                out,
                vec![Input::Key(key(code))],
                "{code:?} was not passed on"
            );
            assert_eq!(feed.burst.drain_timeout(), Duration::ZERO);
        }
    }

    /// `Ctrl+Delete` is a chord: the modifier makes it non-text, so it passes
    /// through even though `Delete` alone already does.
    #[test]
    fn a_chord_is_never_paste_text() {
        let mut feed = Feed::new(true);
        let chord = KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL);
        let out = feed.feed(vec![chord], Duration::from_millis(1));
        assert_eq!(out, vec![Input::Key(chord)]);
    }

    /// An editing key in the MIDDLE of a fast burst ends whatever preceded it
    /// and goes out in order — a run is never silently extended across a cursor
    /// move. With no newlines here, the surrounding text is keys, not a paste.
    #[test]
    fn an_editing_key_ends_a_run_in_place() {
        let mut feed = Feed::new(true);
        let mut out = feed.pasted("ab");
        out.extend(feed.feed(vec![key(KeyCode::Left)], Duration::from_millis(1)));
        out.extend(feed.pasted("cd"));
        out.extend(feed.settle());
        assert_eq!(
            out,
            vec![
                Input::Key(ch('a')),
                Input::Key(ch('b')),
                Input::Key(key(KeyCode::Left)),
                Input::Key(ch('c')),
                Input::Key(ch('d')),
            ]
        );
    }

    /// A multi-line paste interrupted by a cursor move still yields a paste for
    /// the part that carries the newline.
    #[test]
    fn a_multiline_run_before_an_editing_key_is_a_paste() {
        let mut feed = Feed::new(true);
        let mut out = feed.pasted("one\rtwo");
        out.extend(feed.feed(vec![key(KeyCode::Left)], Duration::from_millis(1)));
        out.extend(feed.settle());
        assert_eq!(
            out,
            vec![
                Input::Paste("one\rtwo".to_string()),
                Input::Key(key(KeyCode::Left)),
            ]
        );
    }

    #[test]
    fn a_lone_character_is_not_held_past_its_window() {
        let mut feed = Feed::new(true);
        let out = feed.feed(vec![ch('a')], Duration::from_millis(1));
        assert!(out.is_empty(), "held to see if a burst follows");
        assert_eq!(feed.burst.drain_timeout(), MACHINE_GAP);
        assert_eq!(feed.settle(), as_keys("a"));
    }

    /// Two pastes far enough apart in time are two pastes, not one.
    #[test]
    fn a_gap_separates_two_pastes() {
        let mut feed = Feed::new(true);
        let mut out = feed.pasted("first line\rsecond");
        // A human pause, then another paste.
        feed.now += Duration::from_millis(500);
        out.extend(feed.pasted("third line\rfourth"));
        out.extend(feed.settle());
        assert_eq!(
            out,
            vec![
                Input::Paste("first line\rsecond".to_string()),
                Input::Paste("third line\rfourth".to_string()),
            ]
        );
    }
}
