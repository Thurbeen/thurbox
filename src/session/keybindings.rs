//! User-customizable keybindings.
//!
//! Each action the TUI exposes maps to one or more `KeyChord`s and carries a
//! [`KeyContext`]. **Global** actions (quit, new session, copy/paste, …) are
//! active everywhere; **scoped** actions (file viewer / session list nav,
//! terminal scroll) fire only while their pane is focused — so single-letter
//! keys like `j`/`k` can be rebound per-pane without stealing them from the
//! terminal, which forwards everything to the PTY. Defaults reproduce the
//! table in `CLAUDE.md`; users override via the F1 editor or by hand-editing
//! `~/.config/thurbox/keybindings.json`.
//!
//! A few stateful keys remain literal in `key_handlers.rs` and are *not*
//! rebindable: modal-internal selectors (j/k/Enter/Esc), the automations/tasks
//! panes, and the file-viewer search sub-mode.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyModifiers};
use serde::{Deserialize, Serialize};

/// Every user-rebindable action.
///
/// Each action has a [`KeyContext`] (see [`Action::context`]). **Global**
/// actions are active everywhere; **scoped** actions only fire while their
/// pane is focused — which is why single-letter keys (`j`/`k`/`h`/`l`) can be
/// rebound for the file viewer / session list without stealing them from the
/// terminal (which forwards everything to the PTY).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    // ── Global ──────────────────────────────────────────────────────────
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
    FocusTasks,
    GlobalSearch,
    /// Copy the active mouse selection (falls through to terminal SIGINT when
    /// there is no selection).
    Copy,
    /// Paste the clipboard into the focused text input / terminal.
    Paste,
    // ── Session list (scoped) ───────────────────────────────────────────
    SessionListNext,
    SessionListPrev,
    SessionListOpen,
    // ── File viewer (scoped) ────────────────────────────────────────────
    FileViewerDown,
    FileViewerUp,
    FileViewerCollapse,
    FileViewerExpand,
    FileViewerSearch,
    FileViewerNextMatch,
    FileViewerPrevMatch,
    // ── Terminal (scoped) ───────────────────────────────────────────────
    TerminalScrollUp,
    TerminalScrollDown,
    TerminalPageUp,
    TerminalPageDown,
}

/// The focus scope in which an [`Action`] is active. `Global` actions fire in
/// any context; scoped actions fire only while their pane is focused, so the
/// same chord can mean different things in different panes (and stays free for
/// the terminal to forward to the PTY).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyContext {
    Global,
    SessionList,
    FileViewer,
    Terminal,
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
            Action::FocusTasks,
            Action::GlobalSearch,
            Action::Copy,
            Action::Paste,
            Action::SessionListNext,
            Action::SessionListPrev,
            Action::SessionListOpen,
            Action::FileViewerDown,
            Action::FileViewerUp,
            Action::FileViewerCollapse,
            Action::FileViewerExpand,
            Action::FileViewerSearch,
            Action::FileViewerNextMatch,
            Action::FileViewerPrevMatch,
            Action::TerminalScrollUp,
            Action::TerminalScrollDown,
            Action::TerminalPageUp,
            Action::TerminalPageDown,
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
            Action::FocusTasks => "Tasks",
            Action::GlobalSearch => "Global search",
            Action::Copy => "Copy selection",
            Action::Paste => "Paste",
            Action::SessionListNext => "Next item",
            Action::SessionListPrev => "Previous item",
            Action::SessionListOpen => "Focus terminal",
            Action::FileViewerDown => "Move down",
            Action::FileViewerUp => "Move up",
            Action::FileViewerCollapse => "Collapse / parent",
            Action::FileViewerExpand => "Expand / open file",
            Action::FileViewerSearch => "Start search",
            Action::FileViewerNextMatch => "Next match",
            Action::FileViewerPrevMatch => "Previous match",
            Action::TerminalScrollUp => "Scroll up one line",
            Action::TerminalScrollDown => "Scroll down one line",
            Action::TerminalPageUp => "Scroll up half page",
            Action::TerminalPageDown => "Scroll down half page",
        }
    }

    /// The focus scope in which this action is active. Exhaustive match —
    /// adding a new `Action` variant without classifying it here is a compile
    /// error, which is the entire point of this method. Drives both the
    /// context-aware [`KeyBindings::lookup_in`] and the conflict rules in
    /// [`KeyBindings::rebind`].
    pub fn context(self) -> KeyContext {
        match self {
            Action::SessionListNext | Action::SessionListPrev | Action::SessionListOpen => {
                KeyContext::SessionList
            }
            Action::FileViewerDown
            | Action::FileViewerUp
            | Action::FileViewerCollapse
            | Action::FileViewerExpand
            | Action::FileViewerSearch
            | Action::FileViewerNextMatch
            | Action::FileViewerPrevMatch => KeyContext::FileViewer,
            Action::TerminalScrollUp
            | Action::TerminalScrollDown
            | Action::TerminalPageUp
            | Action::TerminalPageDown => KeyContext::Terminal,
            // Everything else is a global action, active in every context.
            _ => KeyContext::Global,
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
            Action::FocusTasks => vec![KeyChord::ctrl('w'), KeyChord::function(5)],
            // Ctrl+A ("search All") — encodes identically on every terminal,
            // so it's a reliable, fully-rebindable opener.
            Action::GlobalSearch => vec![KeyChord::ctrl('a')],
            Action::Copy => vec![KeyChord::ctrl('c')],
            Action::Paste => vec![KeyChord::ctrl('v')],
            // Scoped single-letter / arrow nav. These only fire while their
            // pane is focused, so they don't collide with the terminal.
            Action::SessionListNext => vec![KeyChord::plain('j'), KeyChord::key(KeyCode::Down)],
            Action::SessionListPrev => vec![KeyChord::plain('k'), KeyChord::key(KeyCode::Up)],
            Action::SessionListOpen => vec![KeyChord::key(KeyCode::Enter)],
            Action::FileViewerDown => vec![KeyChord::plain('j'), KeyChord::key(KeyCode::Down)],
            Action::FileViewerUp => vec![KeyChord::plain('k'), KeyChord::key(KeyCode::Up)],
            Action::FileViewerCollapse => vec![KeyChord::plain('h'), KeyChord::key(KeyCode::Left)],
            Action::FileViewerExpand => vec![
                KeyChord::plain('l'),
                KeyChord::key(KeyCode::Right),
                KeyChord::key(KeyCode::Enter),
            ],
            Action::FileViewerSearch => vec![KeyChord::plain('/')],
            Action::FileViewerNextMatch => vec![KeyChord::plain('n')],
            // Shift+N — normalized so it round-trips through display/parse.
            Action::FileViewerPrevMatch => {
                vec![KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('N'))]
            }
            Action::TerminalScrollUp => vec![KeyChord::shift(KeyCode::Up)],
            Action::TerminalScrollDown => vec![KeyChord::shift(KeyCode::Down)],
            Action::TerminalPageUp => vec![KeyChord::shift(KeyCode::PageUp)],
            Action::TerminalPageDown => vec![KeyChord::shift(KeyCode::PageDown)],
        }
    }

    /// Rebindable actions in F1 help render order — the flattened
    /// [`help_sections`]. The interactive help editor indexes its selection
    /// into this list, so it must match the render order in
    /// `render_help_overlay`.
    pub fn rebindable_in_order() -> Vec<Action> {
        help_sections()
            .into_iter()
            .flat_map(|(_, actions)| actions)
            .collect()
    }
}

/// The F1 help overlay's editable sections, in render order: a section title
/// and the actions shown under it. Global actions come first (grouped by
/// theme), then the scoped panes (`… (when focused)`). This is the single
/// source of truth for both [`Action::rebindable_in_order`] and the overlay
/// renderer, so the editor's selection index and the rendered rows never drift.
pub fn help_sections() -> Vec<(&'static str, Vec<Action>)> {
    use Action::*;
    vec![
        (
            "Navigation",
            vec![FocusBackward, FocusForward, NextSession, PreviousSession],
        ),
        (
            "Sessions",
            vec![
                NewSession,
                DeleteSession,
                RestartSession,
                ForkSession,
                OpenAutomations,
                FocusTasks,
                UndoDelete,
                OpenRestoreSessions,
            ],
        ),
        ("Project", vec![OpenInEditor, StartSync]),
        (
            "UI",
            vec![
                QuitApp,
                ToggleShell,
                ToggleHelp,
                ToggleInfoPanel,
                ToggleFileViewer,
                OpenThemePicker,
                GlobalSearch,
            ],
        ),
        ("Clipboard", vec![Copy, Paste]),
        (
            "Session list (when focused)",
            vec![SessionListNext, SessionListPrev, SessionListOpen],
        ),
        (
            "File viewer (when focused)",
            vec![
                FileViewerDown,
                FileViewerUp,
                FileViewerCollapse,
                FileViewerExpand,
                FileViewerSearch,
                FileViewerNextMatch,
                FileViewerPrevMatch,
            ],
        ),
        (
            "Terminal (when focused)",
            vec![
                TerminalScrollUp,
                TerminalScrollDown,
                TerminalPageUp,
                TerminalPageDown,
            ],
        ),
    ]
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

    /// A plain (unmodified) character key, e.g. `j` or `/`.
    pub fn plain(c: char) -> Self {
        Self::normalized(KeyModifiers::NONE, KeyCode::Char(c))
    }

    /// A bare key code with no modifiers (e.g. `Enter`, `Down`).
    pub fn key(code: KeyCode) -> Self {
        Self::normalized(KeyModifiers::NONE, code)
    }

    /// A `Shift`+key chord (e.g. `Shift+Up`).
    pub fn shift(code: KeyCode) -> Self {
        Self::normalized(KeyModifiers::SHIFT, code)
    }

    /// Build a chord, normalizing the Shift+letter encoding ambiguity:
    /// terminals deliver e.g. Shift+n as `Char('N')` (sometimes with the SHIFT
    /// modifier, sometimes without), and `KeyChord::parse` lowercases letters.
    /// We canonicalize every uppercase `Char` to `Shift` + the lowercase letter
    /// so capture, lookup, and the JSON round-trip all agree.
    pub fn normalized(mods: KeyModifiers, code: KeyCode) -> Self {
        if let KeyCode::Char(c) = code {
            if c.is_ascii_uppercase() {
                return Self {
                    mods: mods | KeyModifiers::SHIFT,
                    code: KeyCode::Char(c.to_ascii_lowercase()),
                };
            }
        }
        Self { mods, code }
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

        Some(KeyChord::normalized(mods, code))
    }
}

/// Two actions' active scopes overlap (and so their chords would collide) when
/// either is global, or they share the same scope. Distinct scoped contexts
/// (e.g. session list vs file viewer) never collide — they are never focused
/// at the same time.
pub fn contexts_overlap(a: KeyContext, b: KeyContext) -> bool {
    a == KeyContext::Global || b == KeyContext::Global || a == b
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

    /// Reverse lookup restricted to **global** actions (active in every
    /// context). Used by the early clipboard routing and the global dispatch.
    pub fn lookup(&self, code: KeyCode, mods: KeyModifiers) -> Option<Action> {
        self.lookup_in(KeyContext::Global, code, mods)
    }

    /// Context-aware reverse lookup: match a chord against actions that are
    /// active in `context` — i.e. global actions plus those scoped to
    /// `context`. The keypress is normalized first so Shift+letter encodings
    /// match regardless of how the terminal delivered them.
    pub fn lookup_in(
        &self,
        context: KeyContext,
        code: KeyCode,
        mods: KeyModifiers,
    ) -> Option<Action> {
        let target = KeyChord::normalized(mods, code);
        for (action, chords) in &self.map {
            let ctx = action.context();
            if ctx != KeyContext::Global && ctx != context {
                continue;
            }
            if chords
                .iter()
                .any(|c| c.code == target.code && c.mods == target.mods)
            {
                return Some(*action);
            }
        }
        None
    }

    /// Replace all chords for `action` with the single `chord`. If the chord
    /// was already bound to a *conflicting* action (one whose context overlaps
    /// — see [`contexts_overlap`]), unbind it there and return that action so
    /// the caller can report the reassignment. Bindings in non-overlapping
    /// scopes are left untouched, so e.g. `j` can drive both the session list
    /// and the file viewer.
    pub fn rebind(&mut self, action: Action, chord: KeyChord) -> Option<Action> {
        let chord = KeyChord::normalized(chord.mods, chord.code);
        let stolen = self.map.iter().find_map(|(a, chords)| {
            if *a != action
                && contexts_overlap(a.context(), action.context())
                && chords
                    .iter()
                    .any(|c| c.code == chord.code && c.mods == chord.mods)
            {
                Some(*a)
            } else {
                None
            }
        });
        if let Some(other) = stolen {
            if let Some(v) = self.map.get_mut(&other) {
                v.retain(|c| !(c.code == chord.code && c.mods == chord.mods));
            }
        }
        self.map.insert(action, vec![chord]);
        stolen
    }

    /// Restore `action`'s chords to its compiled-in defaults.
    pub fn reset(&mut self, action: Action) {
        self.map.insert(action, action.default_chords());
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
        Self::from_json_with_warnings(json).map(|(bindings, _)| bindings)
    }

    /// Parse from the JSON shape, reporting everything that would otherwise be
    /// silently skipped: unknown action names, unparsable chord strings, and
    /// chords bound to more than one action in overlapping contexts (lookup
    /// order over a HashMap is arbitrary, so a conflict means one of the two
    /// actions nondeterministically wins). Missing actions fall back to
    /// defaults; the parse itself only fails on malformed JSON.
    pub fn from_json_with_warnings(json: &str) -> Result<(Self, Vec<String>), String> {
        let parsed: HashMap<String, Vec<String>> =
            serde_json::from_str(json).map_err(|e| e.to_string())?;
        let mut warnings = Vec::new();
        let mut bindings = KeyBindings::default();
        for (key, chord_strs) in parsed {
            let action: Action = match serde_json::from_str::<Action>(&format!("\"{key}\"")) {
                Ok(a) => a,
                Err(_) => {
                    warnings.push(format!("unknown action \"{key}\""));
                    continue;
                }
            };
            let mut chords: Vec<KeyChord> = Vec::new();
            for s in &chord_strs {
                match KeyChord::parse(s) {
                    Some(chord) => chords.push(chord),
                    None => warnings.push(format!("invalid chord \"{s}\" for {key}")),
                }
            }
            if !chords.is_empty() {
                bindings.map.insert(action, chords);
            }
        }
        warnings.extend(bindings.conflict_warnings());
        Ok((bindings, warnings))
    }

    /// Chords bound to more than one action whose contexts overlap. The F1
    /// editor prevents these by stealing chords; a hand-edited file can still
    /// introduce them.
    fn conflict_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut entries: Vec<(&Action, &KeyChord)> = self
            .map
            .iter()
            .flat_map(|(action, chords)| chords.iter().map(move |c| (action, c)))
            .collect();
        entries.sort_by_key(|(a, _)| a.label());
        for (i, (action_a, chord)) in entries.iter().enumerate() {
            for (action_b, other) in &entries[i + 1..] {
                if chord == other && contexts_overlap(action_a.context(), action_b.context()) {
                    warnings.push(format!(
                        "chord \"{}\" is bound to both {} and {} (one will be ignored)",
                        chord.display(),
                        action_a.label(),
                        action_b.label(),
                    ));
                }
            }
        }
        warnings
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
    fn from_json_with_warnings_reports_skipped_entries() {
        let json = r#"{ "QuitApp": ["nonsense", "ctrl+x"], "BogusAction": ["ctrl+y"] }"#;
        let (_, warnings) = KeyBindings::from_json_with_warnings(json).unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("BogusAction")),
            "unknown action must be reported: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("nonsense")),
            "invalid chord must be reported: {warnings:?}"
        );
    }

    #[test]
    fn from_json_with_warnings_reports_chord_conflicts() {
        // Two global actions on the same chord: one nondeterministically wins.
        let json = r#"{ "QuitApp": ["ctrl+x"], "NewSession": ["ctrl+x"] }"#;
        let (_, warnings) = KeyBindings::from_json_with_warnings(json).unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("ctrl+x") && w.contains("bound to both")),
            "conflict must be reported: {warnings:?}"
        );
    }

    #[test]
    fn from_json_with_warnings_is_quiet_for_valid_input() {
        let json = r#"{ "QuitApp": ["ctrl+x"] }"#;
        let (kb, warnings) = KeyBindings::from_json_with_warnings(json).unwrap();
        assert!(warnings.is_empty(), "got: {warnings:?}");
        assert_eq!(kb.chord_for(Action::QuitApp), Some(&KeyChord::ctrl('x')));
    }

    #[test]
    fn every_action_has_default_chord_and_context() {
        let kb = KeyBindings::default();
        for action in Action::all() {
            assert!(
                !kb.chords_for(*action).is_empty(),
                "Action::{action:?} has no default chord binding"
            );
            // `context()` is exhaustive at compile time; calling it here keeps
            // coverage honest.
            let _ = action.context();
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
                Action::FocusTasks => 0,
                Action::GlobalSearch => 0,
                Action::Copy => 0,
                Action::Paste => 0,
                Action::SessionListNext => 0,
                Action::SessionListPrev => 0,
                Action::SessionListOpen => 0,
                Action::FileViewerDown => 0,
                Action::FileViewerUp => 0,
                Action::FileViewerCollapse => 0,
                Action::FileViewerExpand => 0,
                Action::FileViewerSearch => 0,
                Action::FileViewerNextMatch => 0,
                Action::FileViewerPrevMatch => 0,
                Action::TerminalScrollUp => 0,
                Action::TerminalScrollDown => 0,
                Action::TerminalPageUp => 0,
                Action::TerminalPageDown => 0,
            }
        }
        // 37 listed variants must equal Action::all().len(). If you add
        // a variant, update both `Action::all()` and the match above.
        const EXPECTED: usize = 37;
        assert_eq!(Action::all().len(), EXPECTED);
        for a in Action::all() {
            classify(*a);
        }
    }

    #[test]
    fn rebind_replaces_all_chords() {
        let mut kb = KeyBindings::default();
        let chord = KeyChord::ctrl('x');
        assert_eq!(kb.rebind(Action::ToggleHelp, chord), None);
        assert_eq!(kb.chords_for(Action::ToggleHelp), &[chord]);
        assert_eq!(
            kb.lookup(KeyCode::Char('x'), KeyModifiers::CONTROL),
            Some(Action::ToggleHelp)
        );
        // The old dual F-key fallback is gone after a single-chord rebind.
        assert_eq!(kb.lookup(KeyCode::F(1), KeyModifiers::NONE), None);
    }

    #[test]
    fn rebind_steals_chord_from_other_action() {
        let mut kb = KeyBindings::default();
        // ctrl+q is QuitApp's default; reassign it to NewSession.
        let chord = KeyChord::ctrl('q');
        assert_eq!(kb.rebind(Action::NewSession, chord), Some(Action::QuitApp));
        assert_eq!(
            kb.lookup(KeyCode::Char('q'), KeyModifiers::CONTROL),
            Some(Action::NewSession)
        );
        // QuitApp no longer owns ctrl+q.
        assert!(!kb.chords_for(Action::QuitApp).contains(&chord));
    }

    #[test]
    fn rebind_to_json_roundtrip() {
        let mut kb = KeyBindings::default();
        let chord = KeyChord::ctrl('x');
        kb.rebind(Action::QuitApp, chord);
        let parsed = KeyBindings::from_json(&kb.to_json().unwrap()).unwrap();
        assert_eq!(parsed.chords_for(Action::QuitApp), &[chord]);
    }

    #[test]
    fn reset_restores_default_chords() {
        let mut kb = KeyBindings::default();
        kb.rebind(Action::OpenThemePicker, KeyChord::ctrl('x'));
        kb.reset(Action::OpenThemePicker);
        assert_eq!(
            kb.chords_for(Action::OpenThemePicker),
            Action::OpenThemePicker.default_chords().as_slice()
        );
        // The dual F-key fallback is restored.
        assert_eq!(
            kb.lookup(KeyCode::F(4), KeyModifiers::NONE),
            Some(Action::OpenThemePicker)
        );
    }

    #[test]
    fn rebindable_in_order_is_permutation_of_all() {
        let ordered = Action::rebindable_in_order();
        assert_eq!(ordered.len(), Action::all().len());
        for action in Action::all() {
            assert!(ordered.contains(action), "{action:?} missing from order");
        }
    }

    #[test]
    fn lookup_in_scopes_to_context() {
        let kb = KeyBindings::default();
        // `j` is a scoped action — only resolves in its own pane.
        assert_eq!(
            kb.lookup_in(
                KeyContext::FileViewer,
                KeyCode::Char('j'),
                KeyModifiers::NONE
            ),
            Some(Action::FileViewerDown)
        );
        assert_eq!(
            kb.lookup_in(
                KeyContext::SessionList,
                KeyCode::Char('j'),
                KeyModifiers::NONE
            ),
            Some(Action::SessionListNext)
        );
        // The terminal never resolves `j` — it forwards it to the PTY.
        assert_eq!(
            kb.lookup_in(KeyContext::Terminal, KeyCode::Char('j'), KeyModifiers::NONE),
            None
        );
        // Global actions resolve in every context.
        assert_eq!(
            kb.lookup_in(
                KeyContext::Terminal,
                KeyCode::Char('q'),
                KeyModifiers::CONTROL
            ),
            Some(Action::QuitApp)
        );
    }

    #[test]
    fn same_chord_reused_across_distinct_scopes_without_conflict() {
        let mut kb = KeyBindings::default();
        // `j` is bound in both file viewer and session list by default — no steal.
        assert_eq!(
            kb.rebind(Action::FileViewerDown, KeyChord::plain('j')),
            None,
            "distinct scopes must not collide"
        );
        // Both still resolve in their own context.
        assert_eq!(
            kb.lookup_in(
                KeyContext::SessionList,
                KeyCode::Char('j'),
                KeyModifiers::NONE
            ),
            Some(Action::SessionListNext)
        );
        assert_eq!(
            kb.lookup_in(
                KeyContext::FileViewer,
                KeyCode::Char('j'),
                KeyModifiers::NONE
            ),
            Some(Action::FileViewerDown)
        );
    }

    #[test]
    fn rebind_steals_within_same_scope() {
        let mut kb = KeyBindings::default();
        // Bind FileViewerUp to `j` — already FileViewerDown's chord (same scope).
        assert_eq!(
            kb.rebind(Action::FileViewerUp, KeyChord::plain('j')),
            Some(Action::FileViewerDown)
        );
    }

    #[test]
    fn global_chord_conflicts_with_every_scope() {
        let mut kb = KeyBindings::default();
        // A scoped action grabbing a global chord steals it from the global action.
        assert_eq!(
            kb.rebind(Action::FileViewerDown, KeyChord::ctrl('q')),
            Some(Action::QuitApp)
        );
    }

    #[test]
    fn normalized_shift_letter_round_trips() {
        // Shift+N normalizes to {SHIFT, 'n'} and survives display/parse.
        let chord = KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('N'));
        assert_eq!(chord.mods, KeyModifiers::SHIFT);
        assert_eq!(chord.code, KeyCode::Char('n'));
        assert_eq!(KeyChord::parse(&chord.display()), Some(chord));
        // Lookup matches whether the terminal delivers Char('N') with or
        // without the SHIFT modifier.
        let kb = KeyBindings::default();
        for mods in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            assert_eq!(
                kb.lookup_in(KeyContext::FileViewer, KeyCode::Char('N'), mods),
                Some(Action::FileViewerPrevMatch)
            );
        }
    }
}
