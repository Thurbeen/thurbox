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
//! * sessions are inert [`Session::stub`]s on a no-op [`StubBackend`],
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

/// Inert [`SessionBackend`] for the harness — every method is a no-op or an
/// error. Sessions rendered on top of it have a real vt100 parser (so the
/// session list draws) but never spawn or talk to tmux.
#[derive(Default)]
struct StubBackend;

impl SessionBackend for StubBackend {
    fn name(&self) -> &str {
        "stub"
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
        anyhow::bail!("stub backend does not spawn")
    }
    fn adopt(
        &self,
        _: &str,
        _: u16,
        _: u16,
    ) -> anyhow::Result<crate::agent::backend::AdoptedSession> {
        anyhow::bail!("stub backend does not adopt")
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
    /// sessions, wired to an in-memory DB and a headless `TestBackend`.
    fn new(cols: u16, rows: u16, session_count: usize) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let guard = crate::paths::TestPathGuard::new(tmp.path());

        let backend: Arc<dyn SessionBackend> = Arc::new(StubBackend);
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
fn ctrl_a_opens_global_search_strip() {
    let mut h = Harness::standard(2);
    assert!(!h.app.global_search.active);

    h.ctrl('a'); // GlobalSearch
    assert!(h.app.global_search.active, "Ctrl+A opens the search strip");

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
