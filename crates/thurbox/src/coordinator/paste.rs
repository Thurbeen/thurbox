use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Input {
    Key(KeyEvent),
    Paste(String),
}

const MACHINE_GAP: Duration = Duration::from_millis(10);

pub(crate) struct PasteBurst {
    active: bool,
    run: Vec<KeyEvent>,
    last: Option<Instant>,
}

impl PasteBurst {
    pub(crate) fn for_platform() -> Self {
        Self::new(cfg!(windows))
    }

    pub(crate) fn new(active: bool) -> Self {
        Self {
            active,
            run: Vec::new(),
            last: None,
        }
    }

    pub(crate) fn drain_timeout(&self) -> Duration {
        if self.active && !self.run.is_empty() {
            MACHINE_GAP
        } else {
            Duration::ZERO
        }
    }

    pub(crate) fn push(&mut self, key: KeyEvent, at: Instant) -> Vec<Input> {
        if !self.active {
            return vec![Input::Key(key)];
        }
        if key.kind == KeyEventKind::Release {
            if self.run.is_empty() {
                return vec![Input::Key(key)];
            }
            self.run.push(key);
            return Vec::new();
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
        if gap.is_none_or(|gap| gap >= MACHINE_GAP) {
            out = self.flush();
        }
        self.run.push(key);
        out
    }

    pub(crate) fn flush(&mut self) -> Vec<Input> {
        let presses = self
            .run
            .iter()
            .filter(|key| key.kind != KeyEventKind::Release)
            .count();
        if presses >= 2 {
            let text: String = self.run.iter().filter_map(|key| paste_char(*key)).collect();
            if text.trim_end_matches(['\r', '\n']).contains(['\r', '\n']) {
                let mut out = vec![Input::Paste(text)];
                out.extend(
                    self.run
                        .drain(..)
                        .filter(|key| key.kind == KeyEventKind::Release)
                        .map(Input::Key),
                );
                return out;
            }
        }
        self.run.drain(..).map(Input::Key).collect()
    }
}

fn paste_char(key: KeyEvent) -> Option<char> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
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

    fn release(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Release)
    }

    #[test]
    fn releases_inside_a_paste_follow_it_in_order() {
        let mut feed = Feed::new(true);
        let mut out = Vec::new();
        for key in events("a\rb") {
            feed.now += Duration::from_millis(1);
            out.extend(feed.burst.push(key, feed.now));
            out.extend(
                feed.burst
                    .push(release(key.code, KeyModifiers::NONE), feed.now),
            );
        }
        out.extend(feed.settle());
        let none = KeyModifiers::NONE;
        assert_eq!(
            out,
            vec![
                Input::Paste("a\rb".into()),
                Input::Key(release(KeyCode::Char('a'), none)),
                Input::Key(release(KeyCode::Enter, none)),
                Input::Key(release(KeyCode::Char('b'), none)),
            ]
        );
        // With no run open there is nothing to wait behind.
        let x = release(KeyCode::Char('x'), none);
        assert_eq!(feed.burst.push(x, feed.now), vec![Input::Key(x)]);
    }

    #[test]
    fn a_release_from_before_the_run_survives_its_paste() {
        let held = release(KeyCode::Char(' '), KeyModifiers::CONTROL);
        let mut feed = Feed::new(true);
        let mut out = Vec::new();
        for (at, key) in events("a\rb").into_iter().enumerate() {
            feed.now += Duration::from_millis(1);
            out.extend(feed.burst.push(key, feed.now));
            if at == 0 {
                out.extend(feed.burst.push(held, feed.now));
            }
        }
        out.extend(feed.settle());
        assert_eq!(out, vec![Input::Paste("a\rb".into()), Input::Key(held)]);
    }

    #[test]
    fn a_release_from_before_the_run_keeps_its_place_behind_typing() {
        let held = release(KeyCode::Char(' '), KeyModifiers::CONTROL);
        let mut feed = Feed::new(true);
        let mut out = feed.burst.push(ch('j'), feed.now);
        out.extend(feed.burst.push(held, feed.now));
        out.extend(feed.settle());
        assert_eq!(out, vec![Input::Key(ch('j')), Input::Key(held)]);
    }

    #[test]
    fn inert_off_windows_passes_every_key_through() {
        let mut feed = Feed::new(false);
        let mut out = feed.pasted("a\rb");
        out.extend(feed.settle());
        assert_eq!(out, as_keys("a\rb"));
    }

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

    #[test]
    fn a_batched_typed_line_still_submits() {
        let mut feed = Feed::new(true);
        let mut out = feed.pasted("yes\r");
        out.extend(feed.settle());
        assert_eq!(out, as_keys("yes\r"));
    }

    #[test]
    fn a_pasted_single_line_submits() {
        let mut feed = Feed::new(true);
        let mut out = feed.pasted("ls -la\r");
        out.extend(feed.settle());
        assert_eq!(out, as_keys("ls -la\r"));
    }

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

    #[test]
    fn a_chord_is_never_paste_text() {
        let mut feed = Feed::new(true);
        let chord = KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL);
        let out = feed.feed(vec![chord], Duration::from_millis(1));
        assert_eq!(out, vec![Input::Key(chord)]);
    }

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
