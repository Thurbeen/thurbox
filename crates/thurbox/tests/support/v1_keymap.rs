use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyModifiers};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    QuitApp,
    NewSession,
    DeleteSession,
    OpenInEditor,
    OpenAutomations,
    StartSync,
    ToggleShell,
    ToggleReview,
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
    ToggleSessionList,
    GlobalSearch,
    OpenSettings,
    TogglePerfHud,
    Copy,
    Paste,
    SessionListNext,
    SessionListPrev,
    SessionListOpen,
    SessionListMoveDown,
    SessionListMoveUp,
    SessionListSortAlphabetically,
    AutomationsNew,
    AutomationsNext,
    AutomationsPrev,
    AutomationsOpen,
    AutomationsToggle,
    AutomationsRun,
    AutomationsDelete,
    TasksNew,
    TasksNext,
    TasksPrev,
    TasksOpen,
    TasksCycleStatus,
    TasksRun,
    TasksOpenRelated,
    TasksDelete,
    TasksPreviewDown,
    TasksPreviewUp,
    FileViewerDown,
    FileViewerUp,
    FileViewerCollapse,
    FileViewerExpand,
    FileViewerSearch,
    FileViewerNextMatch,
    FileViewerPrevMatch,
    TerminalScrollUp,
    TerminalScrollDown,
    TerminalPageUp,
    TerminalPageDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyContext {
    Global,
    SessionList,
    Automations,
    Tasks,
    FileViewer,
    Terminal,
}

impl Action {
    pub fn all() -> &'static [Action] {
        &[
            Action::QuitApp,
            Action::NewSession,
            Action::DeleteSession,
            Action::OpenInEditor,
            Action::OpenAutomations,
            Action::StartSync,
            Action::ToggleShell,
            Action::ToggleReview,
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
            Action::ToggleSessionList,
            Action::GlobalSearch,
            Action::OpenSettings,
            Action::TogglePerfHud,
            Action::Copy,
            Action::Paste,
            Action::SessionListNext,
            Action::SessionListPrev,
            Action::SessionListOpen,
            Action::SessionListMoveDown,
            Action::SessionListMoveUp,
            Action::SessionListSortAlphabetically,
            Action::AutomationsNew,
            Action::AutomationsNext,
            Action::AutomationsPrev,
            Action::AutomationsOpen,
            Action::AutomationsToggle,
            Action::AutomationsRun,
            Action::AutomationsDelete,
            Action::TasksNew,
            Action::TasksNext,
            Action::TasksPrev,
            Action::TasksOpen,
            Action::TasksCycleStatus,
            Action::TasksRun,
            Action::TasksOpenRelated,
            Action::TasksDelete,
            Action::TasksPreviewDown,
            Action::TasksPreviewUp,
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

    pub fn label(self) -> &'static str {
        match self {
            Action::QuitApp => "Quit",
            Action::NewSession => "New session",
            Action::DeleteSession => "Delete session",
            Action::OpenInEditor => "Open in editor",
            Action::OpenAutomations => "Automations",
            Action::StartSync => "Sync worktrees",
            Action::ToggleShell => "Toggle shell view",
            Action::ToggleReview => "Toggle code review",
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
            Action::ToggleSessionList => "Toggle session list",
            Action::GlobalSearch => "Global search",
            Action::OpenSettings => "Settings",
            Action::TogglePerfHud => "Toggle perf HUD",
            Action::Copy => "Copy selection / status",
            Action::Paste => "Paste",
            Action::SessionListNext => "Next item",
            Action::SessionListPrev => "Previous item",
            Action::SessionListOpen => "Focus terminal",
            Action::SessionListMoveDown => "Move session down",
            Action::SessionListMoveUp => "Move session up",
            Action::SessionListSortAlphabetically => "Sort sessions A→Z",
            Action::AutomationsNew => "New automation",
            Action::AutomationsNext => "Next item",
            Action::AutomationsPrev => "Previous item",
            Action::AutomationsOpen => "Edit automation",
            Action::AutomationsToggle => "Toggle enabled",
            Action::AutomationsRun => "Run now",
            Action::AutomationsDelete => "Delete automation",
            Action::TasksNew => "New task",
            Action::TasksNext => "Next item",
            Action::TasksPrev => "Previous item",
            Action::TasksOpen => "Edit task",
            Action::TasksCycleStatus => "Cycle status",
            Action::TasksRun => "Run (Send / Spawn)",
            Action::TasksOpenRelated => "Open related session",
            Action::TasksDelete => "Delete task",
            Action::TasksPreviewDown => "Scroll preview down",
            Action::TasksPreviewUp => "Scroll preview up",
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

    pub fn context(self) -> KeyContext {
        match self {
            Action::SessionListNext
            | Action::SessionListPrev
            | Action::SessionListOpen
            | Action::SessionListMoveDown
            | Action::SessionListMoveUp
            | Action::SessionListSortAlphabetically => KeyContext::SessionList,
            Action::AutomationsNew
            | Action::AutomationsNext
            | Action::AutomationsPrev
            | Action::AutomationsOpen
            | Action::AutomationsToggle
            | Action::AutomationsRun
            | Action::AutomationsDelete => KeyContext::Automations,
            Action::TasksNew
            | Action::TasksNext
            | Action::TasksPrev
            | Action::TasksOpen
            | Action::TasksCycleStatus
            | Action::TasksRun
            | Action::TasksOpenRelated
            | Action::TasksDelete
            | Action::TasksPreviewDown
            | Action::TasksPreviewUp => KeyContext::Tasks,
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
            _ => KeyContext::Global,
        }
    }

    pub fn terminal_passthrough(self) -> bool {
        // Each chord here is a readline editing key, so it defers to the PTY in a focused terminal.
        matches!(
            self,
            Action::ToggleInfoPanel
                | Action::DeleteSession
                | Action::ToggleFileViewer
                | Action::ForkSession
                | Action::OpenInEditor
                | Action::OpenAutomations
                | Action::RestartSession
                | Action::StartSync
                | Action::OpenRestoreSessions
                | Action::FocusTasks
                | Action::ToggleReview
        )
    }

    pub fn default_chords(self) -> Vec<KeyChord> {
        self.default_chords_for(cfg!(target_os = "macos"))
    }

    pub fn default_chords_for(self, macos: bool) -> Vec<KeyChord> {
        let mut chords = match self {
            Action::QuitApp => vec![KeyChord::ctrl('q')],
            Action::NewSession => vec![KeyChord::ctrl('n')],
            Action::DeleteSession => vec![KeyChord::ctrl('d')],
            Action::OpenInEditor => vec![KeyChord::ctrl('o')],
            Action::OpenAutomations => vec![KeyChord::ctrl('p')],
            Action::StartSync => vec![KeyChord::ctrl('s')],
            Action::ToggleShell => vec![KeyChord::ctrl('t'), KeyChord::function(8)],
            // Ctrl+X is the emacs/readline prefix key, so it defers to the PTY; F7 works there.
            Action::ToggleReview => vec![KeyChord::ctrl('x'), KeyChord::function(7)],
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
            Action::ToggleSessionList => vec![KeyChord::function(9)],
            // kitty terminals send Ctrl+/; legacy ones send 0x1F, read by crossterm as Ctrl+7 or Ctrl+_.
            Action::GlobalSearch => {
                vec![
                    KeyChord::ctrl('/'),
                    KeyChord::ctrl('7'),
                    KeyChord::ctrl('_'),
                ]
            }
            Action::OpenSettings => vec![KeyChord::ctrl(','), KeyChord::function(6)],
            Action::TogglePerfHud => vec![KeyChord::function(12)],
            Action::Copy => vec![KeyChord::ctrl('c')],
            Action::Paste => vec![KeyChord::ctrl('v')],
            Action::SessionListNext => vec![KeyChord::plain('j'), KeyChord::key(KeyCode::Down)],
            Action::SessionListPrev => vec![KeyChord::plain('k'), KeyChord::key(KeyCode::Up)],
            Action::SessionListOpen => vec![KeyChord::key(KeyCode::Enter)],
            Action::SessionListMoveDown => {
                vec![KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('J'))]
            }
            Action::SessionListMoveUp => {
                vec![KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('K'))]
            }
            Action::SessionListSortAlphabetically => {
                vec![KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('S'))]
            }
            Action::AutomationsNew => vec![KeyChord::plain('n')],
            Action::AutomationsNext => vec![KeyChord::plain('j'), KeyChord::key(KeyCode::Down)],
            Action::AutomationsPrev => vec![KeyChord::plain('k'), KeyChord::key(KeyCode::Up)],
            Action::AutomationsOpen => vec![KeyChord::key(KeyCode::Enter), KeyChord::plain('e')],
            Action::AutomationsToggle => vec![KeyChord::plain(' ')],
            Action::AutomationsRun => vec![KeyChord::plain('r')],
            Action::AutomationsDelete => vec![KeyChord::plain('d')],
            Action::TasksNew => vec![KeyChord::plain('n')],
            Action::TasksNext => vec![KeyChord::plain('j'), KeyChord::key(KeyCode::Down)],
            Action::TasksPrev => vec![KeyChord::plain('k'), KeyChord::key(KeyCode::Up)],
            Action::TasksOpen => vec![KeyChord::key(KeyCode::Enter), KeyChord::plain('e')],
            Action::TasksCycleStatus => vec![KeyChord::plain(' ')],
            Action::TasksRun => vec![KeyChord::plain('r')],
            Action::TasksOpenRelated => vec![KeyChord::plain('o')],
            Action::TasksDelete => vec![KeyChord::plain('d')],
            Action::TasksPreviewDown => vec![KeyChord::key(KeyCode::PageDown)],
            Action::TasksPreviewUp => vec![KeyChord::key(KeyCode::PageUp)],
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
            Action::FileViewerPrevMatch => {
                vec![KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('N'))]
            }
            Action::TerminalScrollUp => vec![KeyChord::shift(KeyCode::Up)],
            Action::TerminalScrollDown => vec![KeyChord::shift(KeyCode::Down)],
            // Terminal.app and iTerm2 take Shift+PageUp/PageDown for scrollback; Alt+Page is free.
            Action::TerminalPageUp => {
                vec![
                    KeyChord::shift(KeyCode::PageUp),
                    KeyChord::alt(KeyCode::PageUp),
                ]
            }
            Action::TerminalPageDown => {
                vec![
                    KeyChord::shift(KeyCode::PageDown),
                    KeyChord::alt(KeyCode::PageDown),
                ]
            }
        };
        if macos {
            match self {
                Action::NextSession => chords.push(KeyChord::cmd('j')),
                // Cmd+K clears the buffer in Terminal.app, iTerm2, kitty and Ghostty.
                Action::PreviousSession => chords.push(KeyChord::cmd_shift('j')),
                Action::FocusForward => chords.push(KeyChord::cmd('l')),
                // Cmd+H is macOS's Hide.
                Action::FocusBackward => chords.push(KeyChord::cmd_shift('l')),
                _ => {}
            }
        }
        chords
    }

    pub fn rebindable_in_order() -> Vec<Action> {
        help_sections()
            .into_iter()
            .flat_map(|(_, actions)| actions)
            .collect()
    }
}

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
                ToggleReview,
                ToggleHelp,
                ToggleInfoPanel,
                ToggleFileViewer,
                ToggleSessionList,
                OpenThemePicker,
                OpenSettings,
                GlobalSearch,
                TogglePerfHud,
            ],
        ),
        ("Clipboard", vec![Copy, Paste]),
        (
            "Session list (when focused)",
            vec![
                SessionListNext,
                SessionListPrev,
                SessionListOpen,
                SessionListMoveDown,
                SessionListMoveUp,
                SessionListSortAlphabetically,
            ],
        ),
        (
            "Automations (when focused)",
            vec![
                AutomationsNew,
                AutomationsNext,
                AutomationsPrev,
                AutomationsOpen,
                AutomationsToggle,
                AutomationsRun,
                AutomationsDelete,
            ],
        ),
        (
            "Tasks (when focused)",
            vec![
                TasksNew,
                TasksNext,
                TasksPrev,
                TasksOpen,
                TasksCycleStatus,
                TasksRun,
                TasksOpenRelated,
                TasksDelete,
                TasksPreviewDown,
                TasksPreviewUp,
            ],
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

    pub fn plain(c: char) -> Self {
        Self::normalized(KeyModifiers::NONE, KeyCode::Char(c))
    }

    pub fn key(code: KeyCode) -> Self {
        Self::normalized(KeyModifiers::NONE, code)
    }

    pub fn shift(code: KeyCode) -> Self {
        Self::normalized(KeyModifiers::SHIFT, code)
    }

    pub fn alt(code: KeyCode) -> Self {
        Self::normalized(KeyModifiers::ALT, code)
    }

    // Cmd is crossterm's SUPER, and only kitty-keyboard-protocol terminals deliver it.
    pub fn cmd(c: char) -> Self {
        Self::normalized(KeyModifiers::SUPER, KeyCode::Char(c))
    }

    pub fn cmd_shift(c: char) -> Self {
        Self::normalized(KeyModifiers::SUPER | KeyModifiers::SHIFT, KeyCode::Char(c))
    }

    // Terminals send Shift+letter as an uppercase char, with or without SHIFT; kitty adds extra bits.
    pub fn normalized(mods: KeyModifiers, code: KeyCode) -> Self {
        let mods = mods
            & (KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SHIFT
                | KeyModifiers::SUPER);
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
        if self.mods.contains(KeyModifiers::SUPER) {
            parts.push("cmd");
        }
        let key = match self.code {
            KeyCode::Char(' ') => "space".into(),
            KeyCode::Char('+') => "plus".into(),
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

    pub fn compact(&self) -> String {
        if self.mods == KeyModifiers::CONTROL {
            if let KeyCode::Char(c) = self.code {
                return format!("^{}", c.to_ascii_uppercase());
            }
        }
        if let KeyCode::F(n) = self.code {
            if self.mods.is_empty() {
                return format!("F{n}");
            }
        }
        self.display()
    }

    pub fn parse(s: &str) -> Option<Self> {
        let lc = s.to_ascii_lowercase();
        let parts: Vec<&str> = lc.split('+').collect();
        let (key_raw, mod_parts) = parts.split_last()?;
        let key_part = if *key_raw == " " {
            "space"
        } else {
            key_raw.trim()
        };
        if key_part.is_empty() {
            return None;
        }

        let mut mods = KeyModifiers::NONE;
        for p in mod_parts.iter().map(|p| p.trim()) {
            match p {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "meta" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                "cmd" | "super" | "command" | "win" => mods |= KeyModifiers::SUPER,
                _ => return None,
            }
        }

        let code = match key_part {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "space" => KeyCode::Char(' '),
            "plus" => KeyCode::Char('+'),
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
            other
                if other.len() >= 2
                    && other.len() <= 3
                    && other.starts_with('f')
                    && other[1..].bytes().all(|b| b.is_ascii_digit()) =>
            {
                let n: u8 = other[1..].parse().ok()?;
                KeyCode::F(n)
            }
            other if other.chars().count() == 1 => KeyCode::Char(other.chars().next().unwrap()),
            _ => return None,
        };

        Some(KeyChord::normalized(mods, code))
    }
}

pub fn contexts_overlap(a: KeyContext, b: KeyContext) -> bool {
    a == KeyContext::Global || b == KeyContext::Global || a == b
}

pub fn compact_shortcut(chords: &[KeyChord]) -> Option<String> {
    chords
        .iter()
        .find(|c| matches!(c.code, KeyCode::F(_)) && c.mods.is_empty())
        .or_else(|| chords.first())
        .map(KeyChord::compact)
}

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
    pub fn chord_for(&self, action: Action) -> Option<&KeyChord> {
        self.map.get(&action).and_then(|v| v.first())
    }

    pub fn chords_for(&self, action: Action) -> &[KeyChord] {
        self.map.get(&action).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn lookup(&self, code: KeyCode, mods: KeyModifiers) -> Option<Action> {
        self.lookup_in(KeyContext::Global, code, mods)
    }

    pub fn lookup_in(
        &self,
        context: KeyContext,
        code: KeyCode,
        mods: KeyModifiers,
    ) -> Option<Action> {
        let target = KeyChord::normalized(mods, code);
        let scoped_first_then_declaration_order = |action: &Action| -> (u8, usize) {
            let is_global = (action.context() == KeyContext::Global) as u8;
            let idx = Action::all()
                .iter()
                .position(|a| a == action)
                .unwrap_or(usize::MAX);
            (is_global, idx)
        };
        let mut best: Option<Action> = None;
        for (action, chords) in &self.map {
            let ctx = action.context();
            if ctx != KeyContext::Global && ctx != context {
                continue;
            }
            if chords
                .iter()
                .any(|c| c.code == target.code && c.mods == target.mods)
            {
                let wins = match best {
                    None => true,
                    Some(b) => {
                        scoped_first_then_declaration_order(action)
                            < scoped_first_then_declaration_order(&b)
                    }
                };
                if wins {
                    best = Some(*action);
                }
            }
        }
        best
    }

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

    pub fn reset(&mut self, action: Action) {
        self.map.insert(action, action.default_chords());
    }

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

    pub fn from_json(json: &str) -> Result<Self, String> {
        Self::from_json_with_warnings(json).map(|(bindings, _)| bindings)
    }

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
        let cases = [
            "ctrl+n",
            "f1",
            "shift+pageup",
            "alt+enter",
            "q",
            "space",
            "ctrl+space",
            "plus",
        ];
        for c in cases {
            let chord = KeyChord::parse(c).expect(c);
            assert_eq!(chord.display(), c);
        }
    }

    #[test]
    fn space_is_named_rather_than_written_literally() {
        assert_eq!(KeyChord::plain(' ').display(), "space");
        assert_eq!(KeyChord::plain(' ').compact(), "space");
        assert_eq!(KeyChord::parse("space"), Some(KeyChord::plain(' ')));
        assert_eq!(KeyChord::parse("SPACE"), Some(KeyChord::plain(' ')));
    }

    #[test]
    fn a_literal_space_chord_still_parses() {
        assert_eq!(
            KeyChord::parse(" "),
            Some(KeyChord::plain(' ')),
            "releases up to 1.8.6 wrote a literal space, and those files must keep loading"
        );
        assert_eq!(
            KeyChord::parse("ctrl+ "),
            Some(KeyChord::normalized(
                KeyModifiers::CONTROL,
                KeyCode::Char(' ')
            ))
        );
        assert_eq!(
            KeyChord::parse(""),
            None,
            "splitting before trimming must not turn blank input into a key"
        );
        assert_eq!(KeyChord::parse("   "), None);
        assert_eq!(KeyChord::parse("ctrl+"), None);
    }

    #[test]
    fn plus_is_named_rather_than_written_literally() {
        assert_eq!(KeyChord::plain('+').display(), "plus");
        assert_eq!(KeyChord::parse("plus"), Some(KeyChord::plain('+')));
        assert_eq!(
            KeyChord::parse("ctrl+plus"),
            Some(KeyChord::normalized(
                KeyModifiers::CONTROL,
                KeyCode::Char('+')
            ))
        );
    }

    #[test]
    fn chord_parse_bare_f_is_the_letter_not_a_function_key() {
        assert_eq!(
            KeyChord::parse("ctrl+f"),
            Some(KeyChord::ctrl('f')),
            "ctrl+f must parse as the letter key"
        );
        assert_eq!(
            KeyChord::parse("f"),
            Some(KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('f')))
        );
        assert_eq!(
            KeyChord::parse("f12"),
            Some(KeyChord::normalized(KeyModifiers::NONE, KeyCode::F(12)))
        );
        assert_eq!(KeyChord::parse("fx"), None, "non-digit suffix is invalid");
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
    fn terminal_passthrough_covers_readline_chords_not_navigation() {
        for action in [
            Action::ToggleInfoPanel,
            Action::DeleteSession,
            Action::ToggleFileViewer,
            Action::ForkSession,
            Action::OpenInEditor,
            Action::OpenAutomations,
            Action::RestartSession,
            Action::StartSync,
            Action::OpenRestoreSessions,
            Action::FocusTasks,
            Action::ToggleReview,
        ] {
            assert!(
                action.terminal_passthrough(),
                "{action:?} should defer to the PTY in the terminal"
            );
        }

        for action in [
            Action::QuitApp,
            Action::NewSession,
            Action::FocusBackward,
            Action::FocusForward,
            Action::NextSession,
            Action::PreviousSession,
            Action::ToggleShell,
            Action::UndoDelete,
            Action::Copy,
            Action::Paste,
            Action::GlobalSearch,
        ] {
            assert!(
                !action.terminal_passthrough(),
                "{action:?} must stay active in the terminal"
            );
        }
    }

    #[test]
    fn function_key_actions_have_dual_ctrl_chord() {
        let kb = KeyBindings::default();
        for (action, ctrl, f) in [
            (Action::ToggleHelp, 'g', 1u8),
            (Action::ToggleInfoPanel, 'b', 2),
            (Action::ToggleFileViewer, 'e', 3),
        ] {
            assert_eq!(
                kb.lookup(KeyCode::Char(ctrl), KeyModifiers::CONTROL),
                Some(action),
                "{action:?} needs a Ctrl chord: F-keys are unreliable over some terminals and recorders"
            );
            assert_eq!(kb.lookup(KeyCode::F(f), KeyModifiers::NONE), Some(action));
        }
    }

    #[test]
    fn toggle_session_list_has_f9_chord() {
        let kb = KeyBindings::default();
        assert_eq!(
            kb.lookup(KeyCode::F(9), KeyModifiers::NONE),
            Some(Action::ToggleSessionList)
        );
        assert_eq!(
            kb.chords_for(Action::ToggleSessionList),
            &[KeyChord::function(9)]
        );
    }

    #[test]
    fn toggle_shell_has_dual_ctrl_and_f8_chord() {
        let kb = KeyBindings::default();
        assert_eq!(
            kb.lookup(KeyCode::Char('t'), KeyModifiers::CONTROL),
            Some(Action::ToggleShell)
        );
        assert_eq!(
            kb.lookup(KeyCode::F(8), KeyModifiers::NONE),
            Some(Action::ToggleShell)
        );
    }

    #[test]
    fn toggle_review_has_dual_ctrl_x_and_f7_chord() {
        let kb = KeyBindings::default();
        assert_eq!(
            kb.lookup(KeyCode::Char('x'), KeyModifiers::CONTROL),
            Some(Action::ToggleReview)
        );
        assert_eq!(
            kb.lookup(KeyCode::F(7), KeyModifiers::NONE),
            Some(Action::ToggleReview)
        );
        assert!(Action::ToggleReview.terminal_passthrough());
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
        assert_eq!(kb.lookup(KeyCode::Char('a'), KeyModifiers::CONTROL), None);
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
    fn settings_panel_has_dual_chord() {
        let kb = KeyBindings::default();
        assert_eq!(
            kb.lookup(KeyCode::Char(','), KeyModifiers::CONTROL),
            Some(Action::OpenSettings)
        );
        assert_eq!(
            kb.lookup(KeyCode::F(6), KeyModifiers::NONE),
            Some(Action::OpenSettings)
        );
        assert_eq!(Action::OpenSettings.context(), KeyContext::Global);
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
    fn saving_the_defaults_reloads_without_a_single_warning() {
        let json = KeyBindings::default().to_json().unwrap();
        let (_, warnings) = KeyBindings::from_json_with_warnings(&json).unwrap();
        assert!(
            warnings.is_empty(),
            "the F1 editor saves the whole map, so a default that cannot round-trip warns on \
             every launch, and json_round_trip cannot see it: {warnings:?}"
        );
    }

    #[test]
    fn from_json_falls_back_for_missing_actions() {
        let json = r#"{ "QuitApp": ["ctrl+a"] }"#;
        let kb = KeyBindings::from_json(json).unwrap();
        assert_eq!(kb.chord_for(Action::QuitApp), Some(&KeyChord::ctrl('a')));
        assert_eq!(kb.chord_for(Action::NewSession), Some(&KeyChord::ctrl('n')));
    }

    #[test]
    fn from_json_ignores_unknown_actions_and_invalid_chords() {
        let json = r#"{ "QuitApp": ["nonsense", "ctrl+a"], "BogusAction": ["ctrl+y"] }"#;
        let kb = KeyBindings::from_json(json).unwrap();
        assert_eq!(kb.chord_for(Action::QuitApp), Some(&KeyChord::ctrl('a')));
    }

    #[test]
    fn from_json_with_warnings_reports_skipped_entries() {
        let json = r#"{ "QuitApp": ["nonsense", "ctrl+a"], "BogusAction": ["ctrl+y"] }"#;
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
        let json = r#"{ "QuitApp": ["ctrl+a"], "NewSession": ["ctrl+a"] }"#;
        let (_, warnings) = KeyBindings::from_json_with_warnings(json).unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("ctrl+a") && w.contains("bound to both")),
            "conflict must be reported: {warnings:?}"
        );
    }

    #[test]
    fn from_json_with_warnings_is_quiet_for_valid_input() {
        let json = r#"{ "QuitApp": ["ctrl+a"] }"#;
        let (kb, warnings) = KeyBindings::from_json_with_warnings(json).unwrap();
        assert!(warnings.is_empty(), "got: {warnings:?}");
        assert_eq!(kb.chord_for(Action::QuitApp), Some(&KeyChord::ctrl('a')));
    }

    #[test]
    fn every_action_has_default_chord_and_context() {
        let kb = KeyBindings::default();
        for action in Action::all() {
            assert!(
                !kb.chords_for(*action).is_empty(),
                "Action::{action:?} has no default chord binding"
            );
            let _ = action.context();
        }
    }

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
                Action::ToggleReview => 0,
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
                Action::ToggleSessionList => 0,
                Action::GlobalSearch => 0,
                Action::OpenSettings => 0,
                Action::TogglePerfHud => 0,
                Action::Copy => 0,
                Action::Paste => 0,
                Action::SessionListNext => 0,
                Action::SessionListPrev => 0,
                Action::SessionListOpen => 0,
                Action::SessionListMoveDown => 0,
                Action::SessionListMoveUp => 0,
                Action::SessionListSortAlphabetically => 0,
                Action::AutomationsNew => 0,
                Action::AutomationsNext => 0,
                Action::AutomationsPrev => 0,
                Action::AutomationsOpen => 0,
                Action::AutomationsToggle => 0,
                Action::AutomationsRun => 0,
                Action::AutomationsDelete => 0,
                Action::TasksNew => 0,
                Action::TasksNext => 0,
                Action::TasksPrev => 0,
                Action::TasksOpen => 0,
                Action::TasksCycleStatus => 0,
                Action::TasksRun => 0,
                Action::TasksOpenRelated => 0,
                Action::TasksDelete => 0,
                Action::TasksPreviewDown => 0,
                Action::TasksPreviewUp => 0,
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
        const VARIANTS_IN_CLASSIFY: usize = 61;
        assert_eq!(
            Action::all().len(),
            VARIANTS_IN_CLASSIFY,
            "a new variant goes into both Action::all() and classify"
        );
        for a in Action::all() {
            classify(*a);
        }
    }

    #[test]
    fn rebind_replaces_all_chords() {
        let mut kb = KeyBindings::default();
        let chord = KeyChord::ctrl('a');
        assert_eq!(kb.rebind(Action::ToggleHelp, chord), None);
        assert_eq!(kb.chords_for(Action::ToggleHelp), &[chord]);
        assert_eq!(
            kb.lookup(KeyCode::Char('a'), KeyModifiers::CONTROL),
            Some(Action::ToggleHelp)
        );
        assert_eq!(kb.lookup(KeyCode::F(1), KeyModifiers::NONE), None);
    }

    #[test]
    fn rebind_steals_chord_from_other_action() {
        let mut kb = KeyBindings::default();
        let chord = KeyChord::ctrl('q');
        assert_eq!(kb.rebind(Action::NewSession, chord), Some(Action::QuitApp));
        assert_eq!(
            kb.lookup(KeyCode::Char('q'), KeyModifiers::CONTROL),
            Some(Action::NewSession)
        );
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
        assert_eq!(
            kb.lookup_in(
                KeyContext::Automations,
                KeyCode::Char('j'),
                KeyModifiers::NONE
            ),
            Some(Action::AutomationsNext)
        );
        assert_eq!(
            kb.lookup_in(KeyContext::Tasks, KeyCode::Char('j'), KeyModifiers::NONE),
            Some(Action::TasksNext)
        );
        assert_eq!(
            kb.lookup_in(KeyContext::Tasks, KeyCode::Char('o'), KeyModifiers::NONE),
            Some(Action::TasksOpenRelated)
        );
        assert_eq!(
            kb.lookup_in(
                KeyContext::SessionList,
                KeyCode::Char('o'),
                KeyModifiers::NONE
            ),
            None
        );
        assert_eq!(
            kb.lookup_in(KeyContext::Terminal, KeyCode::Char('j'), KeyModifiers::NONE),
            None
        );
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
    fn lookup_in_resolves_overlapping_bindings_deterministically() {
        let mut kb = KeyBindings::default();
        let chord = KeyChord::ctrl('a');
        kb.map.insert(Action::ToggleHelp, vec![chord]);
        kb.map.insert(Action::SessionListNext, vec![chord]);

        assert_eq!(
            kb.lookup_in(
                KeyContext::SessionList,
                KeyCode::Char('a'),
                KeyModifiers::CONTROL
            ),
            Some(Action::SessionListNext)
        );
        assert_eq!(
            kb.lookup_in(
                KeyContext::Terminal,
                KeyCode::Char('a'),
                KeyModifiers::CONTROL
            ),
            Some(Action::ToggleHelp)
        );
        for _ in 0..50 {
            assert_eq!(
                kb.lookup_in(
                    KeyContext::SessionList,
                    KeyCode::Char('a'),
                    KeyModifiers::CONTROL
                ),
                Some(Action::SessionListNext)
            );
        }
    }

    #[test]
    fn same_chord_reused_across_distinct_scopes_without_conflict() {
        let mut kb = KeyBindings::default();
        assert_eq!(
            kb.rebind(Action::FileViewerDown, KeyChord::plain('j')),
            None,
            "distinct scopes must not collide"
        );
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
        assert_eq!(
            kb.rebind(Action::FileViewerUp, KeyChord::plain('j')),
            Some(Action::FileViewerDown)
        );
    }

    #[test]
    fn global_chord_conflicts_with_every_scope() {
        let mut kb = KeyBindings::default();
        assert_eq!(
            kb.rebind(Action::FileViewerDown, KeyChord::ctrl('q')),
            Some(Action::QuitApp)
        );
    }

    #[test]
    fn cmd_chord_parse_aliases_and_canonical_display() {
        for s in ["cmd+j", "super+j", "command+j", "win+j"] {
            assert_eq!(KeyChord::parse(s), Some(KeyChord::cmd('j')), "{s}");
        }
        assert_eq!(KeyChord::cmd('j').display(), "cmd+j");
        assert_eq!(KeyChord::cmd_shift('j').display(), "shift+cmd+j");
        for chord in [KeyChord::cmd('j'), KeyChord::cmd_shift('j')] {
            assert_eq!(KeyChord::parse(&chord.display()), Some(chord));
        }
    }

    #[test]
    fn normalized_masks_unsupported_modifier_bits() {
        let chord = KeyChord::normalized(
            KeyModifiers::CONTROL | KeyModifiers::HYPER | KeyModifiers::META,
            KeyCode::Char('q'),
        );
        assert_eq!(
            chord,
            KeyChord::ctrl('q'),
            "kitty-protocol terminals report HYPER/META bits the app never binds"
        );
        let kb = KeyBindings::default();
        assert_eq!(
            kb.lookup(
                KeyCode::Char('q'),
                KeyModifiers::CONTROL | KeyModifiers::HYPER
            ),
            Some(Action::QuitApp)
        );
    }

    #[test]
    fn macos_defaults_are_additive_superset() {
        for action in Action::all() {
            let linux = action.default_chords_for(false);
            let macos = action.default_chords_for(true);
            assert!(
                macos.len() >= linux.len() && macos[..linux.len()] == linux[..],
                "{action:?}: {linux:?} is not a prefix of {macos:?}"
            );
        }
    }

    #[test]
    fn macos_default_set_has_no_conflicts() {
        let map = Action::all()
            .iter()
            .map(|a| (*a, a.default_chords_for(true)))
            .collect();
        let kb = KeyBindings { map };
        let warnings = kb.conflict_warnings();
        assert!(warnings.is_empty(), "macOS defaults conflict: {warnings:?}");
    }

    #[test]
    fn rebind_steals_cmd_chord_across_overlapping_contexts() {
        let map = Action::all()
            .iter()
            .map(|a| (*a, a.default_chords_for(true)))
            .collect();
        let mut kb = KeyBindings { map };
        assert_eq!(
            kb.rebind(Action::FileViewerDown, KeyChord::cmd('j')),
            Some(Action::NextSession)
        );
        assert!(!kb
            .chords_for(Action::NextSession)
            .contains(&KeyChord::cmd('j')));
    }

    #[test]
    fn cmd_chord_json_round_trip() {
        let mut kb = KeyBindings::default();
        kb.rebind(Action::NextSession, KeyChord::cmd('j'));
        let parsed = KeyBindings::from_json(&kb.to_json().unwrap()).unwrap();
        assert_eq!(
            parsed.chords_for(Action::NextSession),
            &[KeyChord::cmd('j')]
        );
    }

    #[test]
    fn terminal_paging_has_alt_fallback() {
        let kb = KeyBindings::default();
        for (code, action) in [
            (KeyCode::PageUp, Action::TerminalPageUp),
            (KeyCode::PageDown, Action::TerminalPageDown),
        ] {
            for mods in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
                assert_eq!(
                    kb.lookup_in(KeyContext::Terminal, code, mods),
                    Some(action),
                    "{action:?} via {mods:?}: Terminal.app and iTerm2 take Shift+Page for scrollback"
                );
            }
        }
    }

    #[test]
    fn normalized_shift_letter_round_trips() {
        let chord = KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('N'));
        assert_eq!(chord.mods, KeyModifiers::SHIFT);
        assert_eq!(chord.code, KeyCode::Char('n'));
        assert_eq!(KeyChord::parse(&chord.display()), Some(chord));
        let kb = KeyBindings::default();
        for mods in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            assert_eq!(
                kb.lookup_in(KeyContext::FileViewer, KeyCode::Char('N'), mods),
                Some(Action::FileViewerPrevMatch),
                "a terminal may send Char('N') with or without SHIFT: {mods:?}"
            );
        }
    }

    #[test]
    fn compact_renders_ctrl_fkey_and_fallback() {
        assert_eq!(KeyChord::ctrl('x').compact(), "^X");
        assert_eq!(KeyChord::function(7).compact(), "F7");
        assert_eq!(
            KeyChord {
                mods: KeyModifiers::NONE,
                code: KeyCode::Enter,
            }
            .compact(),
            "enter"
        );
    }

    #[test]
    fn compact_shortcut_prefers_the_f_key_alternate() {
        assert_eq!(
            compact_shortcut(KeyBindings::default().chords_for(Action::ToggleReview)),
            Some("F7".to_string())
        );
        assert_eq!(
            compact_shortcut(KeyBindings::default().chords_for(Action::QuitApp)),
            Some("^Q".to_string())
        );
        assert_eq!(compact_shortcut(&[]), None);
    }
}
