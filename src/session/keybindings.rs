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
    /// Toggle the native code-review view for the active session.
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
    GlobalSearch,
    /// Open the Settings panel (view/edit settings.toml in the TUI).
    OpenSettings,
    /// Toggle the perf HUD overlay (live counters + frame/tick timing).
    TogglePerfHud,
    /// Copy the active mouse selection. With no selection it copies the current
    /// status-bar message instead — except in a focused terminal, where it falls
    /// through to SIGINT (the status row stays click-to-copy there).
    Copy,
    /// Paste the clipboard into the focused text input / terminal.
    Paste,
    // ── Session list (scoped) ───────────────────────────────────────────
    SessionListNext,
    SessionListPrev,
    SessionListOpen,
    /// Move the selected session one row down (manual reordering).
    SessionListMoveDown,
    /// Move the selected session one row up (manual reordering).
    SessionListMoveUp,
    /// Sort sessions alphabetically by name within each repo group, preserving
    /// group order and parent/child nesting (children sort among siblings).
    SessionListSortAlphabetically,
    // ── Automations pane (scoped) ───────────────────────────────────────
    AutomationsNew,
    AutomationsNext,
    AutomationsPrev,
    /// Open the central-pane editor for the selected automation.
    AutomationsOpen,
    /// Toggle the selected automation enabled/disabled.
    AutomationsToggle,
    /// Run the selected automation now.
    AutomationsRun,
    AutomationsDelete,
    // ── Tasks pane (scoped) ─────────────────────────────────────────────
    TasksNew,
    TasksNext,
    TasksPrev,
    /// Open the central-pane editor for the selected task.
    TasksOpen,
    /// Cycle the selected task's status (Todo → InProgress → Done).
    TasksCycleStatus,
    /// Open the trigger-time action picker (Send → session / Spawn new).
    TasksRun,
    /// Open the selected task's related session.
    TasksOpenRelated,
    TasksDelete,
    TasksPreviewDown,
    TasksPreviewUp,
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
    Automations,
    Tasks,
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

    /// The focus scope in which this action is active. Exhaustive match —
    /// adding a new `Action` variant without classifying it here is a compile
    /// error, which is the entire point of this method. Drives both the
    /// context-aware [`KeyBindings::lookup_in`] and the conflict rules in
    /// [`KeyBindings::rebind`].
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
            // Everything else is a global action, active in every context.
            _ => KeyContext::Global,
        }
    }

    /// Whether this action should **defer to the agent CLI** when a session
    /// terminal is focused, instead of running as a thurbox command.
    ///
    /// thurbox's global chords share the `Ctrl+<letter>` namespace with the
    /// readline / shell line-editing chords users have in muscle memory
    /// (`Ctrl+A` = start-of-line, `Ctrl+E` = end-of-line, `Ctrl+W` =
    /// delete-word, `Ctrl+U` = kill-line, `Ctrl+R` = reverse-search, `Ctrl+D`
    /// = EOF, …). For the actions below we let those keystrokes pass through to
    /// the PTY while the terminal is focused, so the inner agent CLI behaves
    /// normally; the thurbox command stays reachable from the session list (and
    /// via its `F`-key alternate, where one exists). The deferral is gated on
    /// the *bound chord* still being a bare `Ctrl+<letter>` (applied in
    /// `App::handle_key`), so rebinding an action to a non-conflicting key keeps
    /// it working in the terminal.
    ///
    /// Navigation / app-control chords (`Ctrl+H/J/K/L` focus + session nav,
    /// `Ctrl+Q` quit, `Ctrl+N` new, …) are deliberately **not** deferred: they
    /// are the keyboard escape route out of the terminal, so they must keep
    /// working there even though some collide with readline.
    ///
    /// `Ctrl+T` (`ToggleShell`) is a deliberate exception that is **not** in
    /// this list even though it shadows readline's transpose-chars: transpose is
    /// rarely used and the convenient shell toggle wins. It also carries an `F8`
    /// alternate, so this is a considered choice — do not "fix" it by adding it
    /// here.
    ///
    /// `Ctrl+X` (`ToggleReview`) **is** in the list: it is the emacs/readline
    /// *prefix* key (`C-x C-e` edits the command line, `C-x C-s` saves, …), so
    /// deferring it keeps that prefix working for the inner agent/shell. `F7` is
    /// its in-terminal alternate.
    pub fn terminal_passthrough(self) -> bool {
        matches!(
            self,
            Action::ToggleInfoPanel         // Ctrl+B — backward char   (F2 alt)
                | Action::DeleteSession     // Ctrl+D — EOF / delete char
                | Action::ToggleFileViewer  // Ctrl+E — end of line      (F3 alt)
                | Action::ForkSession       // Ctrl+F — forward char
                | Action::OpenInEditor      // Ctrl+O — operate-and-get-next
                | Action::OpenAutomations   // Ctrl+P — previous history
                | Action::RestartSession    // Ctrl+R — reverse search
                | Action::StartSync         // Ctrl+S — forward search / XOFF
                | Action::OpenRestoreSessions // Ctrl+U — kill line
                | Action::FocusTasks        // Ctrl+W — delete word      (F5 alt)
                | Action::ToggleReview // Ctrl+X — emacs prefix (C-x …)  (F7 alt)
        )
    }

    /// Default key chord(s) bound to this action for the platform we were
    /// compiled for. `cfg!(target_os)` is decided at exactly this one
    /// callsite; everything else goes through [`Action::default_chords_for`]
    /// so Linux CI can test both platform sets.
    pub fn default_chords(self) -> Vec<KeyChord> {
        self.default_chords_for(cfg!(target_os = "macos"))
    }

    /// Default key chord(s) bound to this action. Exhaustive match —
    /// adding a new `Action` variant without a default chord here is a
    /// compile error.
    ///
    /// With `macos` set, a few Cmd alternates are **appended** after the
    /// cross-platform chords (the primaries — and so the rendered hints —
    /// are identical on every platform). The Cmd set is deliberately tiny:
    /// macOS terminals claim most of the Cmd namespace at the GUI level
    /// (Cmd+Q/W/N/T/C/V/F, Cmd+K clears, Cmd+H hides, Cmd+digits switch
    /// tabs), and only kitty-protocol terminals deliver Cmd at all — so we
    /// add one coherent pattern (Cmd mirrors the Ctrl primary, Shift
    /// reverses) on letters no major terminal claims.
    pub fn default_chords_for(self, macos: bool) -> Vec<KeyChord> {
        let mut chords = match self {
            Action::QuitApp => vec![KeyChord::ctrl('q')],
            Action::NewSession => vec![KeyChord::ctrl('n')],
            Action::DeleteSession => vec![KeyChord::ctrl('d')],
            Action::OpenInEditor => vec![KeyChord::ctrl('o')],
            Action::OpenAutomations => vec![KeyChord::ctrl('p')],
            Action::StartSync => vec![KeyChord::ctrl('s')],
            // Ctrl+T primary, F8 alternate (the only panel toggle that lacked
            // one; F1–F7 are taken). Stays a shell toggle in the terminal — see
            // the `terminal_passthrough` exception note.
            Action::ToggleShell => vec![KeyChord::ctrl('t'), KeyChord::function(8)],
            // Ctrl+X primary, F7 alternate — consistent with the other panel
            // toggles. Ctrl+X is the emacs/readline *prefix* key (`C-x C-e`,
            // `C-x C-s`, …), so it is in `terminal_passthrough`: it forwards to
            // the agent in a focused terminal/shell pane (emacs `C-x` unaffected)
            // and toggles the review only from non-terminal panes. F7 is the
            // in-terminal escape hatch. Fully rebindable.
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
            // Ctrl+/ — the near-universal "search" chord. Terminals encode it
            // inconsistently: kitty-protocol ones deliver `Ctrl+/`, while legacy
            // ones send the raw 0x1F byte that crossterm decodes as `Ctrl+7` /
            // `Ctrl+_`, so all three are bound (the first is the displayed
            // hint). None is a bare Ctrl+<letter>, so it never defers to the PTY
            // — search opens from the terminal too. Fully rebindable.
            Action::GlobalSearch => {
                vec![
                    KeyChord::ctrl('/'),
                    KeyChord::ctrl('7'),
                    KeyChord::ctrl('_'),
                ]
            }
            // Ctrl+, — the near-universal "preferences/settings" chord. Not a
            // bare Ctrl+<letter>, so it never defers to the PTY (the panel opens
            // from a focused terminal too). F6 is a discoverable alternate
            // (F1–F5 are already bound). Fully rebindable.
            Action::OpenSettings => vec![KeyChord::ctrl(','), KeyChord::function(6)],
            // F12 only — a diagnostic surface, not worth spending a scarce
            // Ctrl+<letter> on. F-keys dispatch from every pane (a focused
            // terminal included), which is exactly what a HUD toggle needs.
            Action::TogglePerfHud => vec![KeyChord::function(12)],
            Action::Copy => vec![KeyChord::ctrl('c')],
            Action::Paste => vec![KeyChord::ctrl('v')],
            // Scoped single-letter / arrow nav. These only fire while their
            // pane is focused, so they don't collide with the terminal.
            Action::SessionListNext => vec![KeyChord::plain('j'), KeyChord::key(KeyCode::Down)],
            Action::SessionListPrev => vec![KeyChord::plain('k'), KeyChord::key(KeyCode::Up)],
            Action::SessionListOpen => vec![KeyChord::key(KeyCode::Enter)],
            // Shift+J / Shift+K — normalized like FileViewerPrevMatch's Shift+N.
            Action::SessionListMoveDown => {
                vec![KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('J'))]
            }
            Action::SessionListMoveUp => {
                vec![KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('K'))]
            }
            // Shift+S — normalized like SessionListMoveDown/Up's Shift+J/K.
            Action::SessionListSortAlphabetically => {
                vec![KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('S'))]
            }
            // Automations pane (scoped) — same letters as the session list,
            // safe because the context lookup keeps them apart.
            Action::AutomationsNew => vec![KeyChord::plain('n')],
            Action::AutomationsNext => vec![KeyChord::plain('j'), KeyChord::key(KeyCode::Down)],
            Action::AutomationsPrev => vec![KeyChord::plain('k'), KeyChord::key(KeyCode::Up)],
            Action::AutomationsOpen => vec![KeyChord::key(KeyCode::Enter), KeyChord::plain('e')],
            Action::AutomationsToggle => vec![KeyChord::plain(' ')],
            Action::AutomationsRun => vec![KeyChord::plain('r')],
            Action::AutomationsDelete => vec![KeyChord::plain('d')],
            // Tasks pane (scoped).
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
            // Shift+N — normalized so it round-trips through display/parse.
            Action::FileViewerPrevMatch => {
                vec![KeyChord::normalized(KeyModifiers::NONE, KeyCode::Char('N'))]
            }
            Action::TerminalScrollUp => vec![KeyChord::shift(KeyCode::Up)],
            Action::TerminalScrollDown => vec![KeyChord::shift(KeyCode::Down)],
            // Alt+Page fallbacks: Terminal.app/iTerm2 intercept Shift+PageUp/
            // PageDown for their own scrollback, and Mac laptops reach PageUp
            // only via Fn — Alt+PageUp (Fn+Option+Up) is unclaimed everywhere.
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
                // Cmd+K is "clear buffer" in Terminal.app/iTerm2/kitty/Ghostty
                // — Shift reverses the pair instead.
                Action::PreviousSession => chords.push(KeyChord::cmd_shift('j')),
                Action::FocusForward => chords.push(KeyChord::cmd('l')),
                // Cmd+H is OS-level Hide — Shift-reverse again.
                Action::FocusBackward => chords.push(KeyChord::cmd_shift('l')),
                _ => {}
            }
        }
        chords
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
                ToggleReview,
                ToggleHelp,
                ToggleInfoPanel,
                ToggleFileViewer,
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

    /// An `Alt`+key chord (e.g. `Alt+PageUp`).
    pub fn alt(code: KeyCode) -> Self {
        Self::normalized(KeyModifiers::ALT, code)
    }

    /// A `Cmd`+char chord (macOS Command key — crossterm's SUPER modifier).
    /// Only deliverable by kitty-keyboard-protocol terminals.
    pub fn cmd(c: char) -> Self {
        Self::normalized(KeyModifiers::SUPER, KeyCode::Char(c))
    }

    /// A `Cmd+Shift`+char chord.
    pub fn cmd_shift(c: char) -> Self {
        Self::normalized(KeyModifiers::SUPER | KeyModifiers::SHIFT, KeyCode::Char(c))
    }

    /// Build a chord, normalizing the Shift+letter encoding ambiguity:
    /// terminals deliver e.g. Shift+n as `Char('N')` (sometimes with the SHIFT
    /// modifier, sometimes without), and `KeyChord::parse` lowercases letters.
    /// We canonicalize every uppercase `Char` to `Shift` + the lowercase letter
    /// so capture, lookup, and the JSON round-trip all agree. Modifiers are
    /// also masked to the supported set (Ctrl/Alt/Shift/Super): under the kitty
    /// keyboard protocol terminals may report extra bits (HYPER, META,
    /// KEYPAD, …) that would otherwise make lookups silently fail.
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
        if self.mods.contains(KeyModifiers::SUPER) {
            parts.push("cmd");
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

    /// A compact one-token label for tight UI (footer pills, central-pane tabs):
    /// `ctrl+<letter>` as `^X`, a bare F-key as `F7`, else the full
    /// [`display`](Self::display) notation.
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

    /// Parse `"ctrl+n"`, `"f1"`, `"shift+pageup"`, `"cmd+j"`. Case-insensitive.
    /// `cmd`/`super`/`command`/`win` all mean the SUPER modifier (`cmd` is the
    /// canonical display form).
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
                "cmd" | "super" | "command" | "win" => mods |= KeyModifiers::SUPER,
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
            // "f1".."f12" — but NOT a bare "f", which is the letter key (the
            // `[1..].parse()` on "" used to fail and reject "ctrl+f" entirely).
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

/// Two actions' active scopes overlap (and so their chords would collide) when
/// either is global, or they share the same scope. Distinct scoped contexts
/// (e.g. session list vs file viewer) never collide — they are never focused
/// at the same time.
pub fn contexts_overlap(a: KeyContext, b: KeyContext) -> bool {
    a == KeyContext::Global || b == KeyContext::Global || a == b
}

/// The compact shortcut hint for an action's bound `chords`, preferring a bare
/// **F-key** alternate over the primary chord — the F-key fits a tight footer
/// and dispatches even from a focused terminal (where a `Ctrl+<letter>` is
/// passed through to the agent CLI). Falls back to the first chord's
/// [`compact`](KeyChord::compact) form; `None` when there is no binding. Shared
/// by the footer pills and the central-pane tab strip so they never drift.
pub fn compact_shortcut(chords: &[KeyChord]) -> Option<String> {
    chords
        .iter()
        .find(|c| matches!(c.code, KeyCode::F(_)) && c.mods.is_empty())
        .or_else(|| chords.first())
        .map(KeyChord::compact)
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
    ///
    /// When a hand-edited config binds one chord to several overlapping actions
    /// the match is resolved **deterministically**: a context-scoped action
    /// wins over a global one (more specific intent), ties broken by the
    /// earliest position in [`Action::all`]. Iterating the `HashMap` and
    /// returning the first hit would otherwise pick nondeterministically.
    pub fn lookup_in(
        &self,
        context: KeyContext,
        code: KeyCode,
        mods: KeyModifiers,
    ) -> Option<Action> {
        let target = KeyChord::normalized(mods, code);
        // Lower sort key wins: scoped (0) before global (1), then earliest in
        // `Action::all()`.
        let sort_key = |action: &Action| -> (u8, usize) {
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
                    Some(b) => sort_key(action) < sort_key(&b),
                };
                if wins {
                    best = Some(*action);
                }
            }
        }
        best
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
    fn chord_parse_bare_f_is_the_letter_not_a_function_key() {
        // Regression: "f" used to enter the F-key branch and fail the parse,
        // silently dropping bindings like "ctrl+f" from keybindings.json.
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
        // The readline / shell line-editing chords defer to the PTY when the
        // terminal is focused…
        for action in [
            Action::ToggleInfoPanel,     // Ctrl+B
            Action::DeleteSession,       // Ctrl+D
            Action::ToggleFileViewer,    // Ctrl+E
            Action::ForkSession,         // Ctrl+F
            Action::OpenInEditor,        // Ctrl+O
            Action::OpenAutomations,     // Ctrl+P
            Action::RestartSession,      // Ctrl+R
            Action::StartSync,           // Ctrl+S
            Action::OpenRestoreSessions, // Ctrl+U
            Action::FocusTasks,          // Ctrl+W
            Action::ToggleReview,        // Ctrl+X — emacs prefix
        ] {
            assert!(
                action.terminal_passthrough(),
                "{action:?} should defer to the PTY in the terminal"
            );
        }

        // …but the keyboard escape route (focus / session nav) and quit must
        // keep working in the terminal, so they never defer. Global search's
        // default (Ctrl+/) isn't a readline editing chord, so it doesn't defer
        // either — it opens from the terminal directly.
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
    fn toggle_shell_has_dual_ctrl_and_f8_chord() {
        // Ctrl+T is the only panel toggle that historically lacked an F-key
        // alternate; F8 was the first free function key.
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
        // Ctrl+X primary (in terminal_passthrough — the emacs prefix key), F7
        // alternate. Mirrors the other panel toggles.
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
        // Ctrl+A is unbound by default (it is a readline editing chord, not a
        // thurbox action), so it is the neutral "free chord" for fixtures.
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
    fn from_json_falls_back_for_missing_actions() {
        let json = r#"{ "QuitApp": ["ctrl+a"] }"#;
        let kb = KeyBindings::from_json(json).unwrap();
        assert_eq!(kb.chord_for(Action::QuitApp), Some(&KeyChord::ctrl('a')));
        // Unmodified actions retain defaults.
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
        // Two global actions on the same chord: one nondeterministically wins.
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
        // The listed variants must equal Action::all().len(). If you add
        // a variant, update both `Action::all()` and the match above.
        const EXPECTED: usize = 60;
        assert_eq!(Action::all().len(), EXPECTED);
        for a in Action::all() {
            classify(*a);
        }
    }

    #[test]
    fn rebind_replaces_all_chords() {
        let mut kb = KeyBindings::default();
        // ctrl+a is free by default, so the rebind steals from no one.
        let chord = KeyChord::ctrl('a');
        assert_eq!(kb.rebind(Action::ToggleHelp, chord), None);
        assert_eq!(kb.chords_for(Action::ToggleHelp), &[chord]);
        assert_eq!(
            kb.lookup(KeyCode::Char('a'), KeyModifiers::CONTROL),
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
        // The same `j` drives the automations and tasks panes, each scoped to
        // its own context — no collision with the session list / file viewer.
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
        // A bare letter unique to one pane resolves only there.
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
    fn lookup_in_resolves_overlapping_bindings_deterministically() {
        // A hand-edited config can bind one chord to several actions whose
        // scopes overlap (e.g. a global and a session-list action). The lookup
        // must pick a stable winner regardless of HashMap iteration order.
        let mut kb = KeyBindings::default();
        // Ctrl+A is unbound by default, so it has no other claimants to muddy
        // the overlap test (Ctrl+X is now ToggleReview's default).
        let chord = KeyChord::ctrl('a');
        // Bind ctrl+a to a global action and a session-list action directly in
        // the map, bypassing `rebind`'s conflict resolution.
        kb.map.insert(Action::ToggleHelp, vec![chord]); // global
        kb.map.insert(Action::SessionListNext, vec![chord]); // scoped

        // In the session list both match; the scoped action wins over global.
        assert_eq!(
            kb.lookup_in(
                KeyContext::SessionList,
                KeyCode::Char('a'),
                KeyModifiers::CONTROL
            ),
            Some(Action::SessionListNext)
        );
        // Outside its scope only the global action is eligible.
        assert_eq!(
            kb.lookup_in(
                KeyContext::Terminal,
                KeyCode::Char('a'),
                KeyModifiers::CONTROL
            ),
            Some(Action::ToggleHelp)
        );
        // Repeated lookups never flip (no nondeterminism).
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
    fn cmd_chord_parse_aliases_and_canonical_display() {
        for s in ["cmd+j", "super+j", "command+j", "win+j"] {
            assert_eq!(KeyChord::parse(s), Some(KeyChord::cmd('j')), "{s}");
        }
        assert_eq!(KeyChord::cmd('j').display(), "cmd+j");
        assert_eq!(KeyChord::cmd_shift('j').display(), "shift+cmd+j");
        // Both forms round-trip through display/parse.
        for chord in [KeyChord::cmd('j'), KeyChord::cmd_shift('j')] {
            assert_eq!(KeyChord::parse(&chord.display()), Some(chord));
        }
    }

    #[test]
    fn normalized_masks_unsupported_modifier_bits() {
        // Kitty-protocol terminals can report HYPER/META/KEYPAD bits the app
        // never binds — they must not make lookups fail.
        let chord = KeyChord::normalized(
            KeyModifiers::CONTROL | KeyModifiers::HYPER | KeyModifiers::META,
            KeyCode::Char('q'),
        );
        assert_eq!(chord, KeyChord::ctrl('q'));
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
        // The Linux chord list must be a prefix of the macOS one for every
        // action: primaries (and so rendered hints) identical on both
        // platforms, Cmd alternates strictly appended.
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
        // cmd+j belongs to the global NextSession; a scoped action grabbing it
        // steals it like any other chord.
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
        // Shift+PageUp/PageDown is intercepted by Terminal.app/iTerm2
        // scrollback, so the Alt variants must resolve too — on every platform.
        let kb = KeyBindings::default();
        for (code, action) in [
            (KeyCode::PageUp, Action::TerminalPageUp),
            (KeyCode::PageDown, Action::TerminalPageDown),
        ] {
            for mods in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
                assert_eq!(
                    kb.lookup_in(KeyContext::Terminal, code, mods),
                    Some(action),
                    "{action:?} via {mods:?}"
                );
            }
        }
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

    #[test]
    fn compact_renders_ctrl_fkey_and_fallback() {
        assert_eq!(KeyChord::ctrl('x').compact(), "^X");
        assert_eq!(KeyChord::function(7).compact(), "F7");
        // Non-ctrl, non-F-key falls back to the full display notation.
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
        // ToggleReview is bound to [Ctrl+X, F7]; the hint must surface F7 (it
        // dispatches from a focused terminal where Ctrl+X is passed through).
        assert_eq!(
            compact_shortcut(KeyBindings::default().chords_for(Action::ToggleReview)),
            Some("F7".to_string())
        );
        // QuitApp has no F-key, so it falls back to the primary chord's caret.
        assert_eq!(
            compact_shortcut(KeyBindings::default().chords_for(Action::QuitApp)),
            Some("^Q".to_string())
        );
        // No binding → no hint.
        assert_eq!(compact_shortcut(&[]), None);
    }
}
