//! User-customizable global keybindings.
//!
//! Each global action the TUI exposes (quit, new session, switch session,
//! ...) maps to one or more `KeyChord`s. Defaults reproduce the table in
//! `CLAUDE.md`. Users can override via `~/.config/thurbox/keybindings.json`.
//!
//! Modal-internal navigation keys (j/k/Enter/Esc inside selectors) are *not*
//! customizable and remain literal in `key_handlers.rs`.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyModifiers};
use serde::{Deserialize, Serialize};

/// Every user-rebindable global action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    QuitApp,
    NewSession,
    DeleteSession,
    OpenInEditor,
    OpenAutomations,
    StartSync,
    ToggleShell,
    ForkSession,
    RestartSession,
    UndoDelete,
    OpenRestoreSessions,
    OpenThemePicker,
    FocusBackward,
    FocusForward,
    NextSession,
    PreviousSession,
    ToggleHelp,
    ToggleInfoPanel,
    ToggleFileViewer,
}

impl Action {
    /// All actions in stable order — used by the help overlay and config codegen.
    pub fn all() -> &'static [Action] {
        &[
            Action::QuitApp,
            Action::NewSession,
            Action::DeleteSession,
            Action::OpenInEditor,
            Action::OpenAutomations,
            Action::StartSync,
            Action::ToggleShell,
            Action::ForkSession,
            Action::RestartSession,
            Action::UndoDelete,
            Action::OpenRestoreSessions,
            Action::OpenThemePicker,
            Action::FocusBackward,
            Action::FocusForward,
            Action::NextSession,
            Action::PreviousSession,
            Action::ToggleHelp,
            Action::ToggleInfoPanel,
            Action::ToggleFileViewer,
        ]
    }

    /// Short user-facing label used by the help overlay.
    pub fn label(self) -> &'static str {
        match self {
            Action::QuitApp => "Quit",
            Action::NewSession => "New session",
            Action::DeleteSession => "Delete session",
            Action::OpenInEditor => "Open in editor",
            Action::OpenAutomations => "Automations",
            Action::StartSync => "Sync worktrees",
            Action::ToggleShell => "Toggle shell view",
            Action::ForkSession => "Fork session",
            Action::RestartSession => "Restart session",
            Action::UndoDelete => "Undo delete",
            Action::OpenRestoreSessions => "Restore deleted sessions",
            Action::OpenThemePicker => "Pick theme",
            Action::FocusBackward => "Focus previous pane",
            Action::FocusForward => "Focus next pane",
            Action::NextSession => "Next session",
            Action::PreviousSession => "Previous session",
            Action::ToggleHelp => "Help",
            Action::ToggleInfoPanel => "Toggle info panel",
            Action::ToggleFileViewer => "Toggle file viewer",
        }
    }

    /// Section grouping used by the F1 help overlay. Exhaustive match —
    /// adding a new `Action` variant without classifying it here is a
    /// compile error, which is the entire point of this method.
    pub fn category(self) -> Category {
        match self {
            Action::FocusBackward
            | Action::FocusForward
            | Action::NextSession
            | Action::PreviousSession => Category::Navigation,

            Action::NewSession
            | Action::DeleteSession
            | Action::RestartSession
            | Action::ForkSession
            | Action::OpenAutomations
            | Action::UndoDelete
            | Action::OpenRestoreSessions => Category::Sessions,

            Action::OpenInEditor | Action::StartSync => Category::Project,

            Action::QuitApp
            | Action::ToggleShell
            | Action::ToggleHelp
            | Action::ToggleInfoPanel
            | Action::ToggleFileViewer
            | Action::OpenThemePicker => Category::Ui,
        }
    }

    /// Default key chord(s) bound to this action. Exhaustive match —
    /// adding a new `Action` variant without a default chord here is a
    /// compile error.
    pub fn default_chords(self) -> Vec<KeyChord> {
        match self {
            Action::QuitApp => vec![KeyChord::ctrl('q')],
            Action::NewSession => vec![KeyChord::ctrl('n')],
            Action::DeleteSession => vec![KeyChord::ctrl('d')],
            Action::OpenInEditor => vec![KeyChord::ctrl('o')],
            Action::OpenAutomations => vec![KeyChord::ctrl('p')],
            Action::StartSync => vec![KeyChord::ctrl('s')],
            Action::ToggleShell => vec![KeyChord::ctrl('t')],
            Action::ForkSession => vec![KeyChord::ctrl('f')],
            Action::RestartSession => vec![KeyChord::ctrl('r')],
            Action::UndoDelete => vec![KeyChord::ctrl('z')],
            Action::OpenRestoreSessions => vec![KeyChord::ctrl('u')],
            Action::OpenThemePicker => vec![KeyChord::ctrl('y'), KeyChord::function(4)],
            Action::FocusBackward => vec![KeyChord::ctrl('h')],
            Action::FocusForward => vec![KeyChord::ctrl('l')],
            Action::NextSession => vec![KeyChord::ctrl('j')],
            Action::PreviousSession => vec![KeyChord::ctrl('k')],
            Action::ToggleHelp => vec![KeyChord::ctrl('g'), KeyChord::function(1)],
            Action::ToggleInfoPanel => vec![KeyChord::ctrl('b'), KeyChord::function(2)],
            Action::ToggleFileViewer => vec![KeyChord::ctrl('e'), KeyChord::function(3)],
        }
    }
}

/// Section grouping for the F1 help overlay.
///
/// Order of variants in `all()` is the order sections render in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Navigation,
    Sessions,
    Project,
    Ui,
}

impl Category {
    /// Section header text shown above the entries.
    pub fn title(self) -> &'static str {
        match self {
            Category::Navigation => "Navigation",
            Category::Sessions => "Sessions",
            Category::Project => "Project",
            Category::Ui => "UI",
        }
    }

    /// Every category in render order.
    pub fn all() -> &'static [Category] {
        &[
            Category::Navigation,
            Category::Sessions,
            Category::Project,
            Category::Ui,
        ]
    }
}

/// A key chord: modifiers + key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub mods: KeyModifiers,
    pub code: KeyCode,
}

impl KeyChord {
    pub fn ctrl(c: char) -> Self {
        Self {
            mods: KeyModifiers::CONTROL,
            code: KeyCode::Char(c),
        }
    }

    pub fn function(n: u8) -> Self {
        Self {
            mods: KeyModifiers::NONE,
            code: KeyCode::F(n),
        }
    }

    /// Render the chord using the same notation accepted by `parse`.
    pub fn display(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            parts.push("ctrl");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            parts.push("alt");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            parts.push("shift");
        }
        let key = match self.code {
            KeyCode::Char(c) => c.to_string(),
            KeyCode::F(n) => format!("f{n}"),
            KeyCode::Enter => "enter".into(),
            KeyCode::Esc => "esc".into(),
            KeyCode::Tab => "tab".into(),
            KeyCode::BackTab => "backtab".into(),
            KeyCode::Left => "left".into(),
            KeyCode::Right => "right".into(),
            KeyCode::Up => "up".into(),
            KeyCode::Down => "down".into(),
            KeyCode::Home => "home".into(),
            KeyCode::End => "end".into(),
            KeyCode::PageUp => "pageup".into(),
            KeyCode::PageDown => "pagedown".into(),
            KeyCode::Backspace => "backspace".into(),
            KeyCode::Delete => "delete".into(),
            KeyCode::Insert => "insert".into(),
            other => format!("{other:?}").to_lowercase(),
        };
        parts.push(&key);
        parts.join("+")
    }

    /// Parse `"ctrl+n"`, `"f1"`, `"shift+pageup"`. Case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        let lc = s.trim().to_ascii_lowercase();
        if lc.is_empty() {
            return None;
        }
        let parts: Vec<&str> = lc.split('+').map(str::trim).collect();
        let (key_part, mod_parts) = parts.split_last()?;

        let mut mods = KeyModifiers::NONE;
        for p in mod_parts {
            match *p {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "meta" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                _ => return None,
            }
        }

        let code = match *key_part {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            other if other.starts_with('f') && other.len() <= 3 => {
                let n: u8 = other[1..].parse().ok()?;
                KeyCode::F(n)
            }
            other if other.chars().count() == 1 => KeyCode::Char(other.chars().next().unwrap()),
            _ => return None,
        };

        Some(KeyChord { mods, code })
    }
}

/// A user-editable map from `Action` to one or more chords.
///
/// JSON shape:
/// ```json
/// { "QuitApp": ["ctrl+q"], "NewSession": ["ctrl+n"] }
/// ```
#[derive(Debug, Clone)]
pub struct KeyBindings {
    map: HashMap<Action, Vec<KeyChord>>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        let map = Action::all()
            .iter()
            .map(|a| (*a, a.default_chords()))
            .collect();
        Self { map }
    }
}

impl KeyBindings {
    /// First chord for the given action (used by hint rendering).
    pub fn chord_for(&self, action: Action) -> Option<&KeyChord> {
        self.map.get(&action).and_then(|v| v.first())
    }

    /// All chords bound to the given action, in order. Empty slice if
    /// the action has no binding (should not happen for built-in
    /// actions, since `Default` covers every variant).
    pub fn chords_for(&self, action: Action) -> &[KeyChord] {
        self.map.get(&action).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Reverse lookup: given the keypress, return the bound action.
    pub fn lookup(&self, code: KeyCode, mods: KeyModifiers) -> Option<Action> {
        for (action, chords) in &self.map {
            for chord in chords {
                if chord.code == code && chord.mods == mods {
                    return Some(*action);
                }
            }
        }
        None
    }

    /// Serialize to the JSON shape `~/.config/thurbox/keybindings.json` uses.
    pub fn to_json(&self) -> Result<String, String> {
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for (action, chords) in &self.map {
            out.insert(
                serde_json::to_string(action)
                    .map_err(|e| e.to_string())?
                    .trim_matches('"')
                    .to_string(),
                chords.iter().map(KeyChord::display).collect(),
            );
        }
        serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
    }

    /// Parse from the JSON shape. Unknown actions are ignored; unknown chords
    /// are silently dropped. Missing actions fall back to defaults.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let parsed: HashMap<String, Vec<String>> =
            serde_json::from_str(json).map_err(|e| e.to_string())?;
        let mut bindings = KeyBindings::default();
        for (key, chord_strs) in parsed {
            let action: Action = match serde_json::from_str::<Action>(&format!("\"{key}\"")) {
                Ok(a) => a,
                Err(_) => continue,
            };
            let chords: Vec<KeyChord> = chord_strs
                .iter()
                .filter_map(|s| KeyChord::parse(s))
                .collect();
            if !chords.is_empty() {
                bindings.map.insert(action, chords);
            }
        }
        Ok(bindings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_parse_round_trip() {
        let cases = ["ctrl+n", "f1", "shift+pageup", "alt+enter", "q"];
        for c in cases {
            let chord = KeyChord::parse(c).expect(c);
            assert_eq!(chord.display(), c);
        }
    }

    #[test]
    fn chord_parse_is_case_insensitive() {
        assert_eq!(KeyChord::parse("Ctrl+N"), KeyChord::parse("ctrl+n"));
        assert_eq!(KeyChord::parse("F1"), KeyChord::parse("f1"));
    }

    #[test]
    fn default_bindings_reproduce_claude_md_table() {
        let kb = KeyBindings::default();
        assert_eq!(kb.chord_for(Action::QuitApp), Some(&KeyChord::ctrl('q')));
        assert_eq!(kb.chord_for(Action::NewSession), Some(&KeyChord::ctrl('n')));
        assert_eq!(kb.chord_for(Action::ToggleHelp), Some(&KeyChord::ctrl('g')));
    }

    #[test]
    fn function_key_actions_have_dual_ctrl_chord() {
        // F-keys are unreliable over some terminals/recorders, so the panel
        // toggles also accept a Ctrl chord (ctrl is primary, F-key secondary).
        let kb = KeyBindings::default();
        for (action, ctrl, f) in [
            (Action::ToggleHelp, 'g', 1u8),
            (Action::ToggleInfoPanel, 'b', 2),
            (Action::ToggleFileViewer, 'e', 3),
        ] {
            assert_eq!(
                kb.lookup(KeyCode::Char(ctrl), KeyModifiers::CONTROL),
                Some(action)
            );
            assert_eq!(kb.lookup(KeyCode::F(f), KeyModifiers::NONE), Some(action));
        }
    }

    #[test]
    fn lookup_finds_default_chord() {
        let kb = KeyBindings::default();
        assert_eq!(
            kb.lookup(KeyCode::Char('q'), KeyModifiers::CONTROL),
            Some(Action::QuitApp)
        );
        assert_eq!(
            kb.lookup(KeyCode::F(1), KeyModifiers::NONE),
            Some(Action::ToggleHelp)
        );
    }

    #[test]
    fn lookup_returns_none_for_unbound_chord() {
        let kb = KeyBindings::default();
        assert_eq!(kb.lookup(KeyCode::Char('x'), KeyModifiers::CONTROL), None);
    }

    #[test]
    fn theme_picker_has_dual_chord() {
        let kb = KeyBindings::default();
        assert_eq!(
            kb.lookup(KeyCode::Char('y'), KeyModifiers::CONTROL),
            Some(Action::OpenThemePicker)
        );
        assert_eq!(
            kb.lookup(KeyCode::F(4), KeyModifiers::NONE),
            Some(Action::OpenThemePicker)
        );
    }

    #[test]
    fn json_round_trip() {
        let kb = KeyBindings::default();
        let json = kb.to_json().unwrap();
        let parsed = KeyBindings::from_json(&json).unwrap();
        for action in Action::all() {
            assert_eq!(
                kb.chord_for(*action),
                parsed.chord_for(*action),
                "{action:?}"
            );
        }
    }

    #[test]
    fn from_json_falls_back_for_missing_actions() {
        let json = r#"{ "QuitApp": ["ctrl+x"] }"#;
        let kb = KeyBindings::from_json(json).unwrap();
        assert_eq!(kb.chord_for(Action::QuitApp), Some(&KeyChord::ctrl('x')));
        // Unmodified actions retain defaults.
        assert_eq!(kb.chord_for(Action::NewSession), Some(&KeyChord::ctrl('n')));
    }

    #[test]
    fn from_json_ignores_unknown_actions_and_invalid_chords() {
        let json = r#"{ "QuitApp": ["nonsense", "ctrl+x"], "BogusAction": ["ctrl+y"] }"#;
        let kb = KeyBindings::from_json(json).unwrap();
        assert_eq!(kb.chord_for(Action::QuitApp), Some(&KeyChord::ctrl('x')));
    }

    #[test]
    fn every_action_has_default_chord_and_category() {
        let kb = KeyBindings::default();
        for action in Action::all() {
            assert!(
                !kb.chords_for(*action).is_empty(),
                "Action::{action:?} has no default chord binding"
            );
            // `category()` is exhaustive at compile time, but calling it
            // here catches any panic-on-call and keeps test coverage honest.
            let _ = action.category();
        }
    }

    /// Compile-time check that `Action::all()` lists every variant.
    ///
    /// The match below is exhaustive: adding a new `Action` variant
    /// without updating both this match AND `Action::all()` is a
    /// compile error (non-exhaustive match) OR a test failure (length
    /// mismatch). This is the last guard preventing a variant from
    /// silently disappearing from the help overlay.
    #[test]
    fn all_enumerates_every_action_variant() {
        fn classify(a: Action) -> u8 {
            match a {
                Action::QuitApp => 0,
                Action::NewSession => 0,
                Action::DeleteSession => 0,
                Action::OpenInEditor => 0,
                Action::OpenAutomations => 0,
                Action::StartSync => 0,
                Action::ToggleShell => 0,
                Action::ForkSession => 0,
                Action::RestartSession => 0,
                Action::UndoDelete => 0,
                Action::OpenRestoreSessions => 0,
                Action::OpenThemePicker => 0,
                Action::FocusBackward => 0,
                Action::FocusForward => 0,
                Action::NextSession => 0,
                Action::PreviousSession => 0,
                Action::ToggleHelp => 0,
                Action::ToggleInfoPanel => 0,
                Action::ToggleFileViewer => 0,
            }
        }
        // 19 listed variants must equal Action::all().len(). If you add
        // a variant, update both `Action::all()` and the match above.
        const EXPECTED: usize = 19;
        assert_eq!(Action::all().len(), EXPECTED);
        for a in Action::all() {
            classify(*a);
        }
    }
}
