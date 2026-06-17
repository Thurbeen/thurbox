//! In-process acceptance ("end-to-end") tests for the thurbox TUI.
//!
//! Where the focused unit tests in [`super::tests`] poke individual methods,
//! these drive a *real* [`App`] the way `main.rs`'s loop does — feeding
//! `update(AppMessage)` events and rendering `view(Frame)` to a headless
//! ratatui [`TestBackend`]. (The loop's third step, `tick()`, is deliberately
//! skipped: it spawns Tokio tasks and so needs a runtime these synchronous
//! tests don't have; nothing under test here depends on it.) No TTY, tmux
//! server, or agent process is involved:
//!
//! * sessions are inert [`Session::stub`]s on a no-op [`FakeBackend`],
//! * the database is `Database::open_in_memory()`,
//! * every config/data path is redirected to a throwaway tempdir via
//!   [`crate::paths::TestPathGuard`], so the suite never touches the
//!   developer's real `~/.config/thurbox`.
//!
//! Stable, deterministic screens (the empty welcome state, the keybindings
//! help overlay, the theme picker) are pinned with `insta` snapshots so a UI
//! change surfaces as a reviewable diff (`cargo insta review` /
//! `INSTA_UPDATE=always cargo test`). Flows whose output depends on live
//! metrics or wall-clock time are asserted on `App` *state* instead (modal
//! kind, selection index, panel visibility, quit flag) to stay robust.

use std::path::Path;
use std::sync::Arc;

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use super::*;
use crate::agent::AgentProvider;

/// Wide layout (≥120 cols) used by the behavioral tests — exercises the full
/// multi-panel TUI the way a real terminal would.
const STD_COLS: u16 = 120;
const STD_ROWS: u16 = 40;

/// Smaller, sessionless size for the pinned snapshot screens, kept compact so
/// the `.snap` files stay readable.
const SNAP_COLS: u16 = 100;
const SNAP_ROWS: u16 = 30;

/// Backend stand-in for the harness. Inert by default: `spawn`/`adopt` error,
/// so a test proves no accidental spawn while the session still has a real
/// vt100 parser (the session list draws). With `spawnable = true` they succeed,
/// returning an inert EOF reader + sink writer, so the spawn-dependent App flows
/// (restart, shell pane) run for real — those wire Tokio I/O tasks, so such
/// tests must be `#[tokio::test]`.
struct FakeBackend {
    spawnable: bool,
}

impl FakeBackend {
    /// Inert: spawning/adopting fails.
    fn stub() -> Self {
        Self { spawnable: false }
    }

    /// Spawnable: `spawn`/`adopt` succeed with no-op I/O.
    fn spawnable() -> Self {
        Self { spawnable: true }
    }
}

impl SessionBackend for FakeBackend {
    fn name(&self) -> &str {
        "fake"
    }
    fn check_available(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn ensure_ready(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn spawn(
        &self,
        _: &str,
        _: &str,
        _: &[String],
        _: Option<&Path>,
        _: &std::collections::HashMap<String, String>,
        _: u16,
        _: u16,
    ) -> anyhow::Result<crate::agent::backend::SpawnedSession> {
        anyhow::ensure!(self.spawnable, "inert fake backend does not spawn");
        Ok(crate::agent::backend::SpawnedSession {
            backend_id: "fake:0".into(),
            output: Box::new(std::io::empty()),
            input: Box::new(std::io::sink()),
        })
    }
    fn adopt(
        &self,
        _: &str,
        _: u16,
        _: u16,
    ) -> anyhow::Result<crate::agent::backend::AdoptedSession> {
        anyhow::ensure!(self.spawnable, "inert fake backend does not adopt");
        Ok(crate::agent::backend::AdoptedSession {
            output: Box::new(std::io::empty()),
            input: Box::new(std::io::sink()),
        })
    }
    fn discover(&self) -> anyhow::Result<Vec<crate::agent::backend::DiscoveredSession>> {
        Ok(vec![])
    }
    fn resize(&self, _: &str, _: u16, _: u16) -> anyhow::Result<()> {
        Ok(())
    }
    fn is_dead(&self, _: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    fn kill(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn detach(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn pane_pid(&self, _: &str) -> anyhow::Result<Option<u32>> {
        Ok(None)
    }
}

/// A driveable TUI under test: a real [`App`] paired with a headless terminal,
/// plus the tempdir + path guard that keep it hermetic for the harness's life.
struct Harness {
    app: App,
    terminal: Terminal<TestBackend>,
    // Held for their `Drop` side effects (restore XDG paths / delete tempdir);
    // ordering matters — the guard resets path resolution before the dir goes.
    _guard: crate::paths::TestPathGuard,
    _tmp: tempfile::TempDir,
}

impl Harness {
    /// Build an `App` of `cols`×`rows` seeded with `session_count` stub
    /// sessions on the inert [`FakeBackend`].
    fn new(cols: u16, rows: u16, session_count: usize) -> Self {
        Self::with_backend(cols, rows, session_count, Arc::new(FakeBackend::stub()))
    }

    /// As [`Harness::new`], but on a caller-supplied backend — the seam that
    /// lets spawn-dependent flows run against a spawnable [`FakeBackend`].
    fn with_backend(
        cols: u16,
        rows: u16,
        session_count: usize,
        backend: Arc<dyn SessionBackend>,
    ) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let guard = crate::paths::TestPathGuard::new(tmp.path());

        let provider: Arc<dyn AgentProvider> = Arc::new(GenericProvider::new(
            crate::agent::agent_config::builtin_registry()
                .default_agent()
                .unwrap()
                .clone(),
        ));

        let mut app = App::new(
            rows,
            cols,
            BackendRegistry::new(Arc::clone(&backend)),
            crate::agent::agent_config::builtin_registry(),
            Database::open_in_memory().unwrap(),
        );
        for i in 0..session_count {
            app.sessions
                .push(Session::stub(&format!("session-{i}"), &backend, &provider));
        }
        if session_count > 0 {
            app.active_index = 0;
        }

        let terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        Self {
            app,
            terminal,
            _guard: guard,
            _tmp: tmp,
        }
    }

    /// Standard wide harness ([`STD_COLS`]×[`STD_ROWS`]) seeded with
    /// `session_count` stub sessions — the default for behavioral tests.
    fn standard(session_count: usize) -> Self {
        Self::new(STD_COLS, STD_ROWS, session_count)
    }

    /// Snapshot-sized, sessionless harness for the pinned-screen tests.
    fn snapshot() -> Self {
        Self::new(SNAP_COLS, SNAP_ROWS, 0)
    }

    /// Wide harness on a spawnable [`FakeBackend`], with each session given a
    /// resumable `agent_session_id` so spawn-dependent flows (restart) aren't
    /// no-ops. Must be driven from a `#[tokio::test]`: the spawn path wires up
    /// Tokio I/O tasks and needs a runtime.
    fn spawnable(session_count: usize) -> Self {
        let mut h = Self::with_backend(
            STD_COLS,
            STD_ROWS,
            session_count,
            Arc::new(FakeBackend::spawnable()),
        );
        for (i, session) in h.app.sessions.iter_mut().enumerate() {
            session.info.agent_session_id = Some(format!("agent-{i}"));
        }
        h
    }

    /// Feed one key event, exactly as the real event loop converts a crossterm
    /// `KeyPress` into an [`AppMessage`].
    fn key(&mut self, code: KeyCode, mods: KeyModifiers) -> &mut Self {
        self.app.update(AppMessage::KeyPress(code, mods));
        self
    }

    /// A `Ctrl+<c>` chord (the form most global thurbox bindings take).
    fn ctrl(&mut self, c: char) -> &mut Self {
        self.key(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// A bare function key (`F1`…`F5`).
    fn func(&mut self, n: u8) -> &mut Self {
        self.key(KeyCode::F(n), KeyModifiers::NONE)
    }

    /// A `Shift+<letter>` chord (e.g. session reordering). Terminals deliver
    /// these as an uppercase char; `KeyChord::normalized` canonicalizes the
    /// encoding, so the uppercase-char + SHIFT form resolves the same binding.
    fn shift(&mut self, c: char) -> &mut Self {
        self.key(KeyCode::Char(c.to_ascii_uppercase()), KeyModifiers::SHIFT)
    }

    /// Draw the current state to the headless backend and return the visible
    /// glyphs as newline-separated rows (one string per terminal line), the
    /// shape both `insta` snapshots and substring assertions read.
    fn render(&mut self) -> String {
        let app = &mut self.app;
        self.terminal.draw(|f| app.view(f)).unwrap();
        let buffer = self.terminal.backend().buffer();
        let area = *buffer.area();
        let mut out = String::new();
        for y in 0..area.height {
            let mut line = String::new();
            for x in 0..area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            // Drop trailing blanks so snapshots aren't a wall of spaces.
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }
}

// ── Snapshot tests: stable, deterministic screens ────────────────────────────

#[test]
fn empty_welcome_screen_renders() {
    let mut h = Harness::snapshot();
    insta::assert_snapshot!(h.render());
}

#[test]
fn help_overlay_lists_keybindings() {
    let mut h = Harness::snapshot();
    h.func(1); // F1 → ToggleHelp
    assert!(
        matches!(h.app.modal, modals::Modal::Help(_)),
        "F1 should open the help modal"
    );
    insta::assert_snapshot!(h.render());
}

#[test]
fn theme_picker_lists_palettes() {
    let mut h = Harness::snapshot();
    h.ctrl('y'); // Ctrl+Y → OpenThemePicker
    assert!(
        matches!(h.app.modal, modals::Modal::ThemePicker(_)),
        "Ctrl+Y should open the theme picker"
    );
    insta::assert_snapshot!(h.render());
}

// ── Behavioral tests: drive keys, assert on App state ────────────────────────

#[test]
fn ctrl_n_opens_repo_picker() {
    let mut h = Harness::standard(0);
    h.render(); // first frame
    h.ctrl('n'); // Ctrl+N → NewSession
    assert!(
        matches!(h.app.modal, modals::Modal::RepoPicker(_)),
        "Ctrl+N should open the repo picker (no hosts configured)"
    );
    // Esc closes it again.
    h.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!h.app.modal.is_open(), "Esc should dismiss the modal");
}

#[test]
fn ctrl_j_and_k_cycle_session_selection() {
    let mut h = Harness::standard(3);
    assert_eq!(h.app.active_index, 0);

    h.ctrl('j'); // NextSession
    assert_eq!(h.app.active_index, 1, "Ctrl+J moves to the next session");
    h.ctrl('j');
    assert_eq!(h.app.active_index, 2);

    h.ctrl('k'); // PreviousSession
    assert_eq!(h.app.active_index, 1, "Ctrl+K moves back up");
}

#[test]
fn ctrl_w_toggles_tasks_panel() {
    let mut h = Harness::standard(0);
    assert!(!h.app.show_tasks_panel);

    h.ctrl('w'); // FocusTasks
    assert!(h.app.show_tasks_panel, "Ctrl+W reveals the tasks panel");
    h.ctrl('w');
    assert!(!h.app.show_tasks_panel, "Ctrl+W again hides it");
}

#[test]
fn f5_toggles_tasks_panel_like_ctrl_w() {
    // F5 is the documented alternate chord for FocusTasks (Ctrl+W); both must
    // drive the same toggle.
    let mut h = Harness::standard(0);
    assert!(!h.app.show_tasks_panel);

    h.func(5);
    assert!(h.app.show_tasks_panel, "F5 reveals the tasks panel");
    h.func(5);
    assert!(!h.app.show_tasks_panel, "F5 again hides it");
}

#[test]
fn ctrl_slash_opens_global_search_strip() {
    let mut h = Harness::standard(2);
    assert!(!h.app.global_search.active);

    h.ctrl('/'); // GlobalSearch
    assert!(h.app.global_search.active, "Ctrl+/ opens the search strip");

    // The strip captures typing before global keybindings, so a plain letter
    // edits the query rather than triggering a binding.
    h.key(KeyCode::Char('s'), KeyModifiers::NONE);
    assert_eq!(h.app.global_search.query.value(), "s");

    // Esc restores the prior state.
    h.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!h.app.global_search.active, "Esc closes the search strip");
}

#[test]
fn ctrl_q_requests_quit() {
    let mut h = Harness::standard(1);
    assert!(!h.app.should_quit());
    h.ctrl('q'); // QuitApp
    assert!(h.app.should_quit(), "Ctrl+Q should request shutdown");
}

#[test]
fn session_list_renders_seeded_sessions() {
    // Not a snapshot (status dots/metrics drift); assert the names appear.
    let mut h = Harness::standard(2);
    let frame = h.render();
    assert!(
        frame.contains("session-0"),
        "first session name should render"
    );
    assert!(
        frame.contains("session-1"),
        "second session name should render"
    );
}

// ── Side panels: file viewer, info panel ─────────────────────────────────────

#[test]
fn file_viewer_toggles_via_f3_and_ctrl_e() {
    // F3 and Ctrl+E are the two default chords for ToggleFileViewer.
    let mut h = Harness::standard(1);
    assert!(!h.app.show_file_viewer);

    h.func(3);
    assert!(h.app.show_file_viewer, "F3 reveals the file viewer");
    h.func(3);
    assert!(!h.app.show_file_viewer, "F3 again hides it");

    h.ctrl('e');
    assert!(
        h.app.show_file_viewer,
        "Ctrl+E also reveals the file viewer"
    );
    h.ctrl('e');
    assert!(!h.app.show_file_viewer, "Ctrl+E again hides it");
}

#[test]
fn info_panel_toggles_via_f2_and_ctrl_b() {
    let mut h = Harness::standard(1);
    let initial = h.app.show_info_panel;

    h.func(2);
    assert_ne!(h.app.show_info_panel, initial, "F2 toggles the info panel");
    h.ctrl('b');
    assert_eq!(
        h.app.show_info_panel, initial,
        "Ctrl+B toggles it back (same action, alternate chord)"
    );
}

// ── Modals: automations list, restore deleted sessions ───────────────────────

#[test]
fn automations_list_modal_empty() {
    let mut h = Harness::snapshot();
    h.ctrl('p'); // Ctrl+P → OpenAutomations
    assert!(
        matches!(h.app.modal, modals::Modal::AutomationsList(_)),
        "Ctrl+P opens the automations list modal"
    );
    insta::assert_snapshot!(h.render());
}

#[test]
fn restore_sessions_modal_empty() {
    let mut h = Harness::snapshot();
    h.ctrl('u'); // Ctrl+U → OpenRestoreSessions
    assert!(
        matches!(h.app.modal, modals::Modal::RestoreSessions(_)),
        "Ctrl+U opens the restore-deleted-sessions modal"
    );
    insta::assert_snapshot!(h.render());
}

// ── Delete + undo ────────────────────────────────────────────────────────────

#[test]
fn ctrl_d_soft_deletes_and_ctrl_z_undoes() {
    let mut h = Harness::standard(2);
    assert_eq!(h.app.sessions.len(), 2);

    h.ctrl('d'); // DeleteSession (soft, with a 10s undo window)
    assert_eq!(
        h.app.sessions.len(),
        1,
        "delete removes the session from the list"
    );
    assert!(
        h.app.pending_delete.is_some(),
        "a pending delete is held for undo"
    );

    h.ctrl('z'); // UndoDelete
    assert_eq!(h.app.sessions.len(), 2, "undo restores the session");
    assert!(
        h.app.pending_delete.is_none(),
        "the undo consumes the pending delete"
    );
}

#[test]
fn ctrl_d_hard_delete_confirms_when_soft_delete_disabled() {
    let mut h = Harness::standard(2);
    h.app.features.soft_delete = false;

    // Ctrl+D now opens a confirmation prompt instead of deleting immediately.
    h.ctrl('d');
    assert!(
        matches!(h.app.modal, modals::Modal::ConfirmDelete(_)),
        "Ctrl+D opens the hard-delete confirmation when soft_delete is off"
    );
    assert_eq!(
        h.app.sessions.len(),
        2,
        "nothing is deleted before confirmation"
    );

    // Esc cancels, leaving the session untouched.
    h.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!h.app.modal.is_open(), "Esc closes the confirmation");
    assert_eq!(h.app.sessions.len(), 2, "cancel leaves the session intact");

    // Re-open and confirm with Enter → the session is torn down, no undo.
    h.ctrl('d');
    h.key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(!h.app.modal.is_open(), "confirm closes the confirmation");
    assert_eq!(h.app.sessions.len(), 1, "confirm removes the session");
    assert!(
        h.app.pending_delete.is_none(),
        "a hard delete offers no Ctrl+Z undo"
    );
}

#[test]
fn hard_delete_confirmation_accepts_y_and_n_keys() {
    let mut h = Harness::standard(2);
    h.app.features.soft_delete = false;

    // 'n' cancels, like Esc.
    h.ctrl('d');
    h.key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(!h.app.modal.is_open(), "'n' closes the confirmation");
    assert_eq!(h.app.sessions.len(), 2, "'n' cancels the delete");

    // 'y' confirms, like Enter.
    h.ctrl('d');
    h.key(KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(!h.app.modal.is_open(), "'y' closes the confirmation");
    assert_eq!(h.app.sessions.len(), 1, "'y' confirms the delete");
}

// ── Pane focus cycling ───────────────────────────────────────────────────────

#[test]
fn focus_cycles_between_session_list_and_terminal() {
    // With no side panels shown, the session ring is [SessionList, Terminal].
    let mut h = Harness::standard(1);
    assert!(
        matches!(h.app.focus, InputFocus::SessionList),
        "focus starts on the session list"
    );

    h.ctrl('l'); // FocusForward
    assert!(
        matches!(h.app.focus, InputFocus::Terminal),
        "Ctrl+L moves to the terminal"
    );
    h.ctrl('l');
    assert!(
        matches!(h.app.focus, InputFocus::SessionList),
        "Ctrl+L wraps back to the session list"
    );
    h.ctrl('h'); // FocusBackward
    assert!(
        matches!(h.app.focus, InputFocus::Terminal),
        "Ctrl+H steps backward to the terminal"
    );
}

#[test]
fn focus_ring_includes_file_viewer_when_shown() {
    let mut h = Harness::standard(1);
    h.func(3); // show the file viewer
    assert!(h.app.show_file_viewer);

    // Cycling forward from the session list must reach the file viewer.
    let mut saw_file_viewer = false;
    for _ in 0..4 {
        h.ctrl('l');
        if matches!(h.app.focus, InputFocus::FileViewer) {
            saw_file_viewer = true;
            break;
        }
    }
    assert!(
        saw_file_viewer,
        "the focus ring visits the file viewer while it is shown"
    );
}

// ── Manual session ordering ──────────────────────────────────────────────────

#[test]
fn shift_j_reorders_sessions() {
    let mut h = Harness::standard(2);
    let before = h.app.render_order_indices();
    assert_eq!(
        before,
        vec![0, 1],
        "initial render order is insertion order"
    );

    h.shift('j'); // SessionListMoveDown — move the selected (first) row down
    let after = h.app.render_order_indices();
    assert_eq!(
        after,
        vec![1, 0],
        "Shift+J swaps the first session past the second"
    );

    h.shift('k'); // SessionListMoveUp — move it back
    assert_eq!(
        h.app.render_order_indices(),
        vec![0, 1],
        "Shift+K restores the original order"
    );
}

// ── Tasks: panel focus + new-task editor ─────────────────────────────────────

#[test]
fn tasks_panel_new_task_opens_editor() {
    let mut h = Harness::standard(0);
    h.ctrl('w'); // FocusTasks → panel shown and focused
    assert!(h.app.show_tasks_panel);
    assert!(matches!(h.app.focus, InputFocus::TaskList));

    h.key(KeyCode::Char('n'), KeyModifiers::NONE); // new task
    assert!(
        matches!(h.app.focus, InputFocus::TaskEditor),
        "'n' opens the central-pane task editor"
    );
    assert!(
        h.app.task_ui.task_editor.is_some(),
        "a fresh task editor is in flight"
    );
}

// ── Fork ─────────────────────────────────────────────────────────────────────

#[test]
fn ctrl_f_fork_opens_session_name_prompt() {
    // Fork pre-fills the session-name modal with "<name>-fork" before spawning,
    // so it is observable without a real backend.
    let mut h = Harness::standard(1);
    h.ctrl('f'); // ForkSession
    assert!(
        matches!(h.app.modal, modals::Modal::SessionName(_)),
        "Ctrl+F opens the session-name prompt for the fork"
    );
}

// ── Help editor: capture mode ────────────────────────────────────────────────

#[test]
fn help_editor_enters_capture_mode() {
    let mut h = Harness::standard(0);
    h.func(1); // F1 → help
    match h.app.modal {
        modals::Modal::Help(ref help) => assert!(!help.capturing, "starts in navigation mode"),
        ref other => panic!("expected help modal, got {other:?}"),
    }

    h.key(KeyCode::Enter, KeyModifiers::NONE); // begin capturing a new chord
    match h.app.modal {
        modals::Modal::Help(ref help) => {
            assert!(
                help.capturing,
                "Enter starts capture mode for the selected action"
            )
        }
        ref other => panic!("expected help modal, got {other:?}"),
    }
}

// ── Behavioral effects: assert the action actually changed state ──────────────

#[test]
fn theme_picker_selection_applies_and_persists() {
    let mut h = Harness::standard(0);
    let entries = crate::ui::theme::all_theme_entries();
    let default_name = h.app.active_theme.name.clone();

    h.ctrl('y'); // open the picker (opens on the active theme, index 0)
    h.key(KeyCode::Char('j'), KeyModifiers::NONE); // move to the next palette
    h.key(KeyCode::Enter, KeyModifiers::NONE); // confirm

    assert!(!h.app.modal.is_open(), "confirming closes the picker");
    assert_eq!(
        h.app.active_theme.name, entries[1].name,
        "the second palette becomes active"
    );
    assert_ne!(
        h.app.active_theme.name, default_name,
        "the theme actually changed"
    );
    assert_eq!(
        h.app.db.get_active_theme().ok().flatten().as_deref(),
        Some(entries[1].name.as_str()),
        "the choice is persisted to the database"
    );
}

#[test]
fn help_editor_capture_rebinds_the_selected_action() {
    // The help editor opens with the first rebindable action selected; capturing
    // a fresh chord must reassign exactly that action.
    let action = crate::session::Action::rebindable_in_order()[0];
    let new_chord = crate::session::KeyChord::ctrl('x');

    let mut h = Harness::standard(0);
    h.func(1); // F1 → help
    h.key(KeyCode::Enter, KeyModifiers::NONE); // begin capture
    h.ctrl('x'); // the captured chord

    assert_eq!(
        h.app.keybindings.chord_for(action),
        Some(&new_chord),
        "the selected action is rebound to the captured chord"
    );
    match h.app.modal {
        modals::Modal::Help(ref help) => {
            assert!(!help.capturing, "capture ends after one chord")
        }
        ref other => panic!("expected help modal, got {other:?}"),
    }
}

#[test]
fn task_editor_creates_task_and_space_cycles_status() {
    let mut h = Harness::standard(0);
    h.ctrl('w'); // focus the tasks panel
    h.key(KeyCode::Char('n'), KeyModifiers::NONE); // new-task editor

    for ch in "Demo task".chars() {
        h.key(KeyCode::Char(ch), KeyModifiers::NONE); // type the title
    }
    h.ctrl('s'); // save from any field

    assert!(
        matches!(h.app.focus, InputFocus::TaskList),
        "saving returns to the panel"
    );
    assert_eq!(h.app.task_ui.cached_tasks.len(), 1, "the task is persisted");
    let task = &h.app.task_ui.cached_tasks[0];
    assert_eq!(task.title, "Demo task");
    assert_eq!(
        task.status,
        crate::session::TaskStatus::Todo,
        "new tasks start as Todo"
    );

    h.key(KeyCode::Char(' '), KeyModifiers::NONE); // cycle status
    assert_eq!(
        h.app.task_ui.cached_tasks[0].status,
        crate::session::TaskStatus::InProgress,
        "Space advances Todo → InProgress"
    );
}

#[test]
fn global_search_returns_results_for_a_session_query() {
    let mut h = Harness::standard(2); // session-0, session-1
    h.ctrl('/'); // open the search strip
    for ch in "session-1".chars() {
        h.key(KeyCode::Char(ch), KeyModifiers::NONE);
    }
    assert_eq!(h.app.global_search.query.value(), "session-1");
    assert!(
        !h.app.global_search.results.is_empty(),
        "a matching session name yields at least one result"
    );
}

// ── Spawn-dependent flows (fake backend, real Tokio I/O wiring) ───────────────

#[tokio::test]
async fn ctrl_r_restarts_session_on_spawnable_backend() {
    // Restart kills + respawns through the backend and rewires I/O; the fake
    // backend makes that succeed without a real tmux/PTY.
    let mut h = Harness::spawnable(1);
    h.ctrl('r'); // RestartSession

    let msg = h
        .app
        .status_message
        .as_ref()
        .expect("restart reports a status toast");
    assert!(
        matches!(msg.level, StatusLevel::Info),
        "restart succeeds (not an error toast): {:?}",
        msg.text
    );
    assert!(
        msg.text.contains("restart"),
        "the toast names the restart: {:?}",
        msg.text
    );
}

#[tokio::test]
async fn ctrl_t_opens_shell_pane_on_spawnable_backend() {
    // Ctrl+T lazily spawns a shell pane via the backend and flips the session's
    // terminal view to the shell.
    let mut h = Harness::spawnable(1);
    let id = h.app.sessions[0].info.id;

    h.ctrl('t'); // ToggleShell

    assert!(
        h.app.status_message.is_none()
            || !matches!(
                h.app.status_message.as_ref().unwrap().level,
                StatusLevel::Error
            ),
        "opening the shell pane does not error"
    );
    assert!(
        h.app.sessions[0].shell_pane.is_some(),
        "a shell pane was spawned for the session"
    );
    assert_eq!(
        h.app.session_terminal_views.get(&id),
        Some(&TerminalView::Shell),
        "the active session now shows its shell view"
    );
}
