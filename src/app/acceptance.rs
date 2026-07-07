//! In-process acceptance ("end-to-end") tests for the thurbox TUI.
//!
//! Where the focused unit tests in [`super::tests`] poke individual methods,
//! these drive a *real* [`App`] the way `main.rs`'s loop does — feeding
//! `update(AppMessage)` events, running the loop's deterministic tick half
//! ([`App::tick_core`] via [`Harness::tick`]; the excluded `tick_background`
//! spawns Tokio tasks that shell out), and rendering `view(Frame)` to a
//! headless ratatui [`TestBackend`]. No TTY, tmux server, or agent process is
//! involved:
//!
//! * sessions are inert [`Session::stub`]s on a no-op [`FakeBackend`],
//! * the database is `Database::open_in_memory()`,
//! * every config/data path is redirected to a throwaway tempdir via
//!   [`crate::paths::TestPathGuard`], so the suite never touches the
//!   developer's real `~/.config/thurbox`,
//! * agent output is injected per session with [`Harness::feed_output`]
//!   (through the same vt100 parser + `TermSignals` path the PTY reader uses),
//! * wall-clock-gated behavior (timeouts, debounces, the redraw floor) is
//!   fast-forwarded with [`Harness::advance`] (see [`clock`]) — never slept.
//!
//! Stable, deterministic screens (the empty welcome state, the keybindings
//! help overlay, the theme picker) are pinned with `insta` snapshots so a UI
//! change surfaces as a reviewable diff (`cargo insta review` /
//! `INSTA_UPDATE=always cargo test`). Flows whose output depends on live
//! metrics or wall-clock time are asserted on `App` *state* instead (modal
//! kind, selection index, panel visibility, quit flag) to stay robust.
//!
//! Finally, [`monkey_random_events_uphold_invariants`] fuzzes the whole
//! surface: thousands of seeded pseudo-random events (keys, chords, mouse,
//! ticks, clock jumps, resizes, injected output) with [`assert_invariants`]
//! checked after every step — the regression net for "weird TUI behavior"
//! that no directed test anticipated.

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

/// Initialize a git repo at `dir` with one committed file, leaving an
/// uncommitted edit when `dirty`. Used by the hard-delete tests to give a
/// session a worktree whose state `git::worktree_stats` can read.
fn init_git_repo(dir: &Path, dirty: bool) {
    let git = |args: &[&str]| {
        // `git_program` scrubs inherited `GIT_*` vars so this stays hermetic even
        // when the suite runs under the project's pre-commit hook.
        let ok = crate::git::git_program()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "thurbox-test"]);
    std::fs::write(dir.join("f.txt"), "hello\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    if dirty {
        std::fs::write(dir.join("f.txt"), "changed\n").unwrap();
    }
}

/// Backend stand-in for the harness. Inert by default: `spawn`/`adopt` error,
/// so a test proves no accidental spawn while the session still has a real
/// vt100 parser (the session list draws). With `spawnable = true` they succeed,
/// returning an inert EOF reader + sink writer, so the spawn-dependent App flows
/// (restart, shell pane) run for real — those wire Tokio I/O tasks, so such
/// tests must be `#[tokio::test]`.
struct FakeBackend {
    spawnable: bool,
    /// Pushable remote-hook status events, drained by
    /// [`SessionBackend::take_hook_state_events`] — lets a test drive the
    /// remote-session status path without a control-mode connection.
    hook_events: std::sync::Mutex<Vec<(String, String)>>,
}

impl FakeBackend {
    /// Inert: spawning/adopting fails.
    fn stub() -> Self {
        Self {
            spawnable: false,
            hook_events: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Spawnable: `spawn`/`adopt` succeed with no-op I/O.
    fn spawnable() -> Self {
        Self {
            spawnable: true,
            hook_events: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Queue a `(pane_id, state)` event for the next drain.
    fn push_hook_event(&self, pane_id: &str, state: &str) {
        self.hook_events
            .lock()
            .unwrap()
            .push((pane_id.to_string(), state.to_string()));
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
    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.hook_events.lock().unwrap())
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

    /// Point the active session at a freshly-created git repo (clean, or
    /// `dirty` with one uncommitted change) so a `soft_delete`-off delete sees —
    /// or doesn't see — work at risk. Returns the backing `TempDir`, which the
    /// caller must keep alive for the repo to exist on disk.
    fn set_active_git_cwd(&mut self, dirty: bool) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        init_git_repo(repo.path(), dirty);
        let idx = self.app.active_index;
        self.app.sessions[idx].info.cwd = Some(repo.path().to_path_buf());
        repo
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

    /// Click the open Settings panel's row for `field`. Renders first so this
    /// frame's `ModalField` hitboxes exist, locates the one carrying `field`'s
    /// `ORDER` index, and dispatches a click at its left edge.
    fn click_settings_field(&mut self, field: modals::SettingsField) -> &mut Self {
        self.render();
        let index = modals::SettingsField::ORDER
            .iter()
            .position(|f| *f == field)
            .expect("field in ORDER");
        let rect = self
            .app
            .click_targets
            .iter()
            .find_map(|t| match t.action {
                ClickAction::ModalField(i) if i == index => Some(t.rect),
                _ => None,
            })
            .expect("settings field hitbox recorded");
        self.app.update(AppMessage::MouseClick {
            x: rect.x + 1,
            y: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        self
    }

    /// Run one deterministic tick — the third step of `main.rs`'s loop, minus
    /// its background half ([`App::tick_core`]; the excluded
    /// `tick_background` spawns Tokio tasks that shell out / hit the network).
    /// This drives everything tick-dependent hermetically: status derivation,
    /// timer expiry, the global-search debounce, automation firing, and the
    /// external-change poll.
    fn tick(&mut self) -> &mut Self {
        self.app.tick_core();
        self
    }

    /// Fast-forward the app's clock by `d` (see [`clock`]). Timers, debounces
    /// and retry windows age deterministically — the next [`Self::tick`] (or
    /// `should_redraw` check) observes the elapsed time without real waiting.
    fn advance(&mut self, d: std::time::Duration) -> &mut Self {
        clock::advance(d);
        self
    }

    /// Feed raw agent-output bytes to session `idx`, exactly as its PTY reader
    /// loop would — the seam for testing everything downstream of output:
    /// terminal rendering, the output-change redraw detector, OSC
    /// title/bell/notification signals, and buffer-content search.
    fn feed_output(&mut self, idx: usize, bytes: &[u8]) -> &mut Self {
        self.app.sessions[idx].feed_output_for_test(bytes);
        self
    }

    /// Resize both the app (as the real loop would on a terminal resize) and
    /// the headless backend, so subsequent renders draw at the new size.
    fn resize(&mut self, cols: u16, rows: u16) -> &mut Self {
        self.app.update(AppMessage::Resize(cols, rows));
        self.terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        self
    }

    /// Render, then click the central-pane tab strip's cell for `tab` (returns
    /// false when no such tab was rendered, e.g. its feature is off).
    fn click_central_tab(&mut self, tab: CentralTab) -> bool {
        self.render();
        let rect = self.app.click_targets.iter().find_map(|t| match t.action {
            ClickAction::CentralTab(found) if found == tab => Some(t.rect),
            _ => None,
        });
        let Some(rect) = rect else {
            return false;
        };
        self.app.update(AppMessage::MouseClick {
            x: rect.x + 1,
            y: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        true
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
    h.render();
    h.ctrl('n'); // Ctrl+N → NewSession
    assert!(
        matches!(h.app.modal, modals::Modal::RepoPicker(_)),
        "Ctrl+N should open the repo picker (no hosts configured)"
    );
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
fn settings_panel_opens_and_closes() {
    let mut h = Harness::standard(1);
    h.ctrl(','); // OpenSettings
    assert!(
        matches!(h.app.modal, modals::Modal::Settings(_)),
        "Ctrl+, should open the settings panel"
    );

    // The panel shows section headers, the selected field's description, and
    // the restart marker on restart-required rows.
    let screen = h.render();
    assert!(screen.contains("FEATURES"), "section header renders");
    assert!(
        screen.contains("Tasks panel"),
        "selected field's description renders in the footer"
    );
    assert!(screen.contains('⟳'), "restart marker renders");

    h.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!h.app.modal.is_open(), "Esc closes the settings panel");
}

#[test]
fn settings_panel_live_toggle_applies_on_save() {
    let mut h = Harness::standard(1);
    assert!(h.app.features.tasks, "tasks default on");

    h.ctrl(','); // OpenSettings — starts on the `tasks` field
    h.key(KeyCode::Char(' '), KeyModifiers::NONE); // toggle tasks off in the draft
    assert!(h.app.features.tasks, "draft edits don't apply until save");

    h.ctrl('s'); // Save
    assert!(!h.app.modal.is_open(), "save closes the panel");
    assert!(
        !h.app.features.tasks,
        "a live feature flag applies immediately on save"
    );
}

#[test]
fn settings_panel_click_toggles_boolean_field() {
    let mut h = Harness::standard(1);
    assert!(h.app.features.mouse, "mouse on by default");
    assert!(h.app.features.info_panel, "info_panel default on");

    h.ctrl(','); // OpenSettings — opens on the `tasks` field
                 // Click a *different* field than the one focused on open, so the click must
                 // both select the row and toggle its boolean.
    h.click_settings_field(modals::SettingsField::FeatInfoPanel);

    let modals::Modal::Settings(s) = &h.app.modal else {
        panic!("settings panel still open after the click");
    };
    assert_eq!(
        s.field,
        modals::SettingsField::FeatInfoPanel,
        "the click selected the clicked row"
    );
    assert!(
        !s.draft.features.info_panel,
        "the click also toggled the boolean off in the draft"
    );
    assert!(
        h.app.features.info_panel,
        "draft edits don't apply until save"
    );
}

#[test]
fn settings_panel_click_does_not_change_scalar() {
    let mut h = Harness::standard(1);
    h.ctrl(','); // OpenSettings
    h.render();
    let before = match &h.app.modal {
        modals::Modal::Settings(s) => s.draft.scrollback_lines,
        _ => unreachable!(),
    };

    h.click_settings_field(modals::SettingsField::ScrollbackLines);

    let modals::Modal::Settings(s) = &h.app.modal else {
        panic!("settings panel still open");
    };
    assert_eq!(
        s.field,
        modals::SettingsField::ScrollbackLines,
        "the click selected the scalar row"
    );
    assert_eq!(
        s.draft.scrollback_lines, before,
        "a click never steps a scalar value — only selects it"
    );
}

#[test]
fn settings_panel_esc_discards() {
    let mut h = Harness::standard(1);
    assert!(h.app.features.tasks);

    h.ctrl(','); // OpenSettings
    h.key(KeyCode::Char(' '), KeyModifiers::NONE); // toggle in the draft
    h.key(KeyCode::Esc, KeyModifiers::NONE); // discard

    assert!(
        h.app.features.tasks,
        "Esc discards the draft — no live preview applied"
    );
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

#[test]
fn force_deleted_restore_confirms_then_best_effort_restores() {
    let mut h = Harness::standard(1);

    // Persist the stub session, then force-delete it — the soft-deleted +
    // force-deleted DB row a best-effort recovery acts on (no worktrees → no
    // on-disk teardown needed).
    let id = h.app.sessions[0].info.id;
    let shared = h.app.session_to_shared(&h.app.sessions[0]);
    h.app.db.upsert_session(&shared).unwrap();
    crate::session_ops::delete_session_headless(&h.app.db, id, true).unwrap();
    assert!(
        h.app.db.get_deleted_session_by_id(id).unwrap().is_some(),
        "row is soft-deleted + force-deleted"
    );

    // Ctrl+U lists it; Enter on a force-deleted row opens the confirm prompt
    // rather than restoring immediately.
    h.ctrl('u');
    assert!(matches!(h.app.modal, modals::Modal::RestoreSessions(_)));
    h.key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        matches!(h.app.modal, modals::Modal::ConfirmRestore(_)),
        "Enter on a force-deleted row asks for confirmation"
    );
    assert!(
        h.app.db.get_deleted_session_by_id(id).unwrap().is_some(),
        "nothing restored before confirmation"
    );

    // Confirm → `restore_session` clears `deleted_at` + `force_deleted`, so the
    // row leaves the deleted list and is an active session again.
    h.key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(!h.app.modal.is_open(), "confirm closes the prompt");
    assert!(
        h.app.db.get_deleted_session_by_id(id).unwrap().is_none(),
        "the session is no longer in the deleted list"
    );
    assert!(
        h.app.db.get_session_by_id(id).unwrap().is_some(),
        "the row is an active session again"
    );
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
    // The active session has uncommitted work, so a hard delete must confirm.
    let _repo = h.set_active_git_cwd(true);

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
    let _repo = h.set_active_git_cwd(true);

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

#[test]
fn ctrl_d_hard_deletes_clean_session_without_confirmation() {
    let mut h = Harness::standard(2);
    h.app.features.soft_delete = false;
    // A clean git worktree has no work at risk → delete straight away.
    let _repo = h.set_active_git_cwd(false);

    h.ctrl('d');
    assert!(
        !h.app.modal.is_open(),
        "a clean session is hard-deleted without a confirmation prompt"
    );
    assert_eq!(h.app.sessions.len(), 1, "the clean session is removed");
    assert!(
        h.app.pending_delete.is_none(),
        "a hard delete offers no Ctrl+Z undo"
    );
}

#[test]
fn ctrl_d_confirms_dirty_session_and_lists_risk() {
    let mut h = Harness::standard(2);
    h.app.features.soft_delete = false;
    let _repo = h.set_active_git_cwd(true);

    h.ctrl('d');
    let modals::Modal::ConfirmDelete(ref cd) = h.app.modal else {
        panic!("a dirty session opens the hard-delete confirmation");
    };
    assert!(
        cd.risk.dirty && cd.risk.files_changed > 0,
        "the risk reflects the uncommitted change: {:?}",
        cd.risk
    );
    assert!(!cd.risk.unknown, "a local git worktree is inspectable");
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

// ── Code review: focusable changed-files pane ────────────────────────────────

/// Open a synthetic review with `n` files on the active session and focus the
/// diff pane, without needing a real git worktree.
fn open_review(h: &mut Harness, n: usize) {
    let sid = h.app.active_session_id().unwrap();
    h.app
        .code_reviews
        .insert(sid, super::code_review::CodeReviewState::for_test(sid, n));
    h.app.focus = InputFocus::CodeReview;
}

#[test]
fn review_files_pane_joins_focus_ring_and_replaces_file_viewer() {
    let mut h = Harness::standard(1);
    h.func(3); // show the file viewer too — the review must still take the column
    open_review(&mut h, 3);

    // Cycling forward from the diff reaches the changed-files pane, never the
    // plain file viewer while a review owns the column.
    let mut saw_review_files = false;
    for _ in 0..4 {
        h.ctrl('l');
        assert!(
            !matches!(h.app.focus, InputFocus::FileViewer),
            "the file viewer is not a ring stop while a review is open"
        );
        if matches!(h.app.focus, InputFocus::ReviewFiles) {
            saw_review_files = true;
            break;
        }
    }
    assert!(
        saw_review_files,
        "the focus ring visits the changed-files pane"
    );
}

#[test]
fn review_files_pane_navigates_and_opens_into_diff() {
    let mut h = Harness::standard(1);
    open_review(&mut h, 3);
    h.app.focus = InputFocus::ReviewFiles;

    // The diff starts on the first file.
    assert_eq!(h.app.active_review().unwrap().current_file(), Some(0));

    // `j` walks to the next file (the diff follows).
    h.key(KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(h.app.active_review().unwrap().current_file(), Some(1));
    h.key(KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(h.app.active_review().unwrap().current_file(), Some(0));

    // `G` jumps to the last file.
    h.key(KeyCode::Char('G'), KeyModifiers::SHIFT);
    assert_eq!(h.app.active_review().unwrap().current_file(), Some(2));

    // `r` marks the current file reviewed.
    h.key(KeyCode::Char('r'), KeyModifiers::NONE);
    assert!(!h.app.active_review().unwrap().reviewed_files.is_empty());

    // `Enter` drops focus into the diff at the selected file.
    h.key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(h.app.focus, InputFocus::CodeReview));
}

#[test]
fn review_files_pane_demoted_to_terminal_when_review_closes() {
    let mut h = Harness::standard(1);
    open_review(&mut h, 2);
    h.app.focus = InputFocus::ReviewFiles;

    // Esc from the changed-files pane closes the review and drops focus back to
    // the terminal (no review owns the central pane anymore).
    h.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(h.app.active_review().is_none());
    assert!(matches!(h.app.focus, InputFocus::Terminal));
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
fn theme_picker_cancel_restores_previewed_theme() {
    // The picker live-previews by mutating the global palette as the selection
    // moves; cancelling (`Esc`) must undo that preview, leaving the original
    // theme active and unpersisted.
    let mut h = Harness::standard(0);
    let entries = crate::ui::theme::all_theme_entries();
    let original_name = h.app.active_theme.name.clone();
    let original_palette = crate::ui::theme::current();

    h.ctrl('y'); // open the picker (opens on the active theme, index 0)
    h.key(KeyCode::Char('j'), KeyModifiers::NONE); // preview the next palette
    assert_eq!(
        crate::ui::theme::current(),
        entries[1].palette,
        "navigating previews the highlighted palette globally"
    );

    h.key(KeyCode::Esc, KeyModifiers::NONE); // cancel

    assert!(!h.app.modal.is_open(), "Esc closes the picker");
    assert_eq!(
        crate::ui::theme::current(),
        original_palette,
        "cancelling restores the palette active when the picker opened"
    );
    assert_eq!(
        h.app.active_theme.name, original_name,
        "the active theme is unchanged after cancel"
    );
    assert_eq!(
        h.app.db.get_active_theme().ok().flatten(),
        None,
        "cancelling persists nothing to the database"
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
        h.key(KeyCode::Char(ch), KeyModifiers::NONE);
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

#[test]
fn ctrl_c_in_a_focused_terminal_preserves_sigint_over_status_copy() {
    // Ctrl+C copies the status message only in non-PTY panes. In a focused
    // terminal it must still fall through to SIGINT — so the status-copy path is
    // skipped and the shown message is left untouched (deterministic: no
    // clipboard call). The status row stays reachable by mouse click there.
    let mut h = Harness::standard(1);
    h.app.focus = InputFocus::Terminal;
    h.app.set_error("boom");

    h.ctrl('c'); // Copy — no selection, terminal-focused → SIGINT, not a copy

    let msg = h.app.status_message.as_ref().expect("message still shown");
    assert_eq!(
        msg.text, "boom",
        "terminal Ctrl+C leaves the status message intact (SIGINT, no copy)"
    );
}

#[test]
fn ctrl_c_outside_a_terminal_copies_the_status_message() {
    // In a non-PTY pane Ctrl+C (no selection) runs the status-copy path. Every
    // branch of `copy_status_to_clipboard` overwrites the toast (copied /
    // unavailable / write-error), so the original message no longer stands —
    // deterministic regardless of whether a clipboard exists on the runner.
    let mut h = Harness::standard(1);
    h.app.focus = InputFocus::SessionList;
    h.app.set_error("boom");

    h.ctrl('c'); // Copy — no selection, non-terminal → copy the status message

    let msg = h
        .app
        .status_message
        .as_ref()
        .expect("a toast is still shown");
    assert_ne!(
        msg.text, "boom",
        "the copy path fired and replaced the original message with its result"
    );
}

#[test]
fn status_message_row_records_a_click_to_copy_target() {
    // The status row is click-to-copy: whenever a message is shown, its rect is
    // registered as a `CopyStatus` hitbox so a mouse click pulls the text out.
    let mut h = Harness::standard(1);
    h.app.set_info("something worth copying");
    h.render();

    let has_target = h
        .app
        .click_targets
        .iter()
        .any(|t| matches!(t.action, ClickAction::CopyStatus));
    assert!(
        has_target,
        "a shown status message registers a CopyStatus click target"
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
async fn ctrl_r_restart_preserves_thurbox_identity_env() {
    // `Session::restart` replaces the session env wholesale, so the restart path
    // must re-inject the `THURBOX_*` identity vars — otherwise the restarted
    // agent loses its identity and the metrics/status hooks break.
    let mut h = Harness::spawnable(1);
    let session_id = h.app.sessions[0].info.id;
    let agent_session_id = h.app.sessions[0]
        .info
        .agent_session_id
        .clone()
        .expect("spawnable sessions have an agent_session_id");

    h.ctrl('r'); // RestartSession

    let env = h.app.sessions[0].env();
    assert_eq!(
        env.get("THURBOX_SESSION"),
        Some(&session_id.to_string()),
        "the thurbox session key survives the restart"
    );
    assert_eq!(
        env.get("THURBOX_SESSION_ID"),
        Some(&agent_session_id),
        "the agent conversation id survives the restart"
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

#[tokio::test]
async fn central_tab_strip_switches_agent_and_shell_views() {
    // The top-border tab strip is mouse-driven: clicking Shell flips to the
    // shell view (spawning the pane), clicking Agent flips back.
    let mut h = Harness::spawnable(1);
    let id = h.app.sessions[0].info.id;
    assert_eq!(h.app.active_central_tab(), CentralTab::Agent);

    assert!(h.click_central_tab(CentralTab::Shell), "Shell tab rendered");
    assert!(
        h.app.sessions[0].shell_pane.is_some(),
        "clicking Shell spawned the shell pane"
    );
    assert_eq!(
        h.app.session_terminal_views.get(&id),
        Some(&TerminalView::Shell),
        "clicking Shell selects the shell view"
    );
    assert_eq!(h.app.active_central_tab(), CentralTab::Shell);

    assert!(h.click_central_tab(CentralTab::Agent), "Agent tab rendered");
    assert_eq!(
        h.app.session_terminal_views.get(&id),
        Some(&TerminalView::Claude),
        "clicking Agent returns to the agent view"
    );
    assert_eq!(h.app.active_central_tab(), CentralTab::Agent);
}

#[tokio::test]
async fn f8_leaves_open_review_for_the_shell() {
    // With a review overlaying the central pane, F8 (ToggleShell) must reach the
    // global binding (the review's key capture lets it fall through) and land on
    // the shell — not silently flip the hidden terminal view behind the review.
    let mut h = Harness::spawnable(1);
    let sid = h.app.active_session_id().unwrap();
    h.app
        .code_reviews
        .insert(sid, super::code_review::CodeReviewState::for_test(sid, 2));
    h.app.focus = InputFocus::CodeReview;
    assert_eq!(h.app.active_central_tab(), CentralTab::Review);

    h.func(8); // F8 = ToggleShell

    assert!(
        h.app.active_review().is_none(),
        "F8 closes the open review instead of being swallowed"
    );
    assert_eq!(
        h.app.active_central_tab(),
        CentralTab::Shell,
        "F8 lands on the shell view"
    );
    assert_eq!(
        h.app.focus,
        InputFocus::Terminal,
        "focus moves out of the (now closed) review to the terminal"
    );
    assert!(
        h.app.sessions[0].shell_pane.is_some(),
        "the shell pane was spawned"
    );
}

#[tokio::test]
async fn central_tab_strip_renders_labels_and_shortcuts() {
    // The strip paints Agent/Shell/Review with each toggle's shortcut hint in
    // the pane's top border (Agent has no dedicated key, so no hint).
    let mut h = Harness::spawnable(1);
    let screen = h.render();
    // The tab strip is the pane border row carrying the Review toggle hint `F7`
    // (anchored on it to avoid the "…Agent Orchestrator" header banner). The
    // F-key form is shown, not `^X`, since a focused terminal passes Ctrl chords
    // through to the agent.
    let top = screen.lines().find(|l| l.contains("F7")).unwrap_or("");
    for needle in ["Agent", "Shell", "F8", "Review", "F7"] {
        assert!(
            top.contains(needle),
            "central tab strip missing {needle:?}: {top:?}"
        );
    }
}

#[tokio::test]
async fn central_tab_strip_omits_feature_gated_tabs() {
    // Shell/Review tabs are gated by their feature flags. With both off, only
    // the Agent pill would remain — a tab strip you can't switch away from — so
    // the whole strip is suppressed rather than advertising a lone dead tab.
    let mut h = Harness::spawnable(1);
    let collect_tabs = |app: &super::App| -> Vec<CentralTab> {
        app.click_targets
            .iter()
            .filter_map(|t| match t.action {
                ClickAction::CentralTab(tab) => Some(tab),
                _ => None,
            })
            .collect()
    };

    // Only one alternate view gated off → the strip stays (Agent + the other).
    h.app.features.shell_pane = false;
    h.app.features.code_review = true;
    h.render();
    assert_eq!(
        collect_tabs(&h.app),
        vec![CentralTab::Agent, CentralTab::Review],
        "Review survives when only Shell is gated off"
    );

    // Both alternate views gated off → no tab strip at all.
    h.app.features.shell_pane = false;
    h.app.features.code_review = false;
    h.render();
    assert!(
        collect_tabs(&h.app).is_empty(),
        "the lone Agent tab is dropped when Shell and Review are both off"
    );
}

/// The F2 info panel lists upcoming automation runs. When the `automations`
/// feature is off the TUI never fires those schedules (and the pane is hidden),
/// so the info panel must not surface them either — even though the cache is
/// still loaded from the DB.
#[tokio::test]
async fn info_panel_hides_automations_when_feature_off() {
    use crate::session::{Automation, AutomationAction, AutomationSchedule};
    let mut h = Harness::spawnable(1);
    h.app.show_info_panel = true;
    let far_future = crate::sync::current_time_millis() + 3_600_000;
    h.app.automation_ui.cached_automations = vec![Automation {
        id: 1,
        name: "infopanelnightly".into(),
        enabled: true,
        schedule: AutomationSchedule::Once { at: 0 },
        timezone: None,
        action: AutomationAction::Send {
            session_id: SessionId::default(),
        },
        prompt: "p".into(),
        created_at: 0,
        updated_at: 0,
        last_run_at: None,
        next_run_at: Some(far_future),
    }];

    // Feature on: the info panel's automations section lists it.
    h.app.features.automations = true;
    assert!(
        h.render().contains("infopanelnightly"),
        "info panel surfaces the upcoming automation when the feature is on"
    );

    // Feature off: the pane is hidden *and* the info-panel section is dropped,
    // so the automation appears nowhere.
    h.app.features.automations = false;
    assert!(
        !h.render().contains("infopanelnightly"),
        "info panel must not surface automations when the feature is off"
    );
}

// ── Performance counters: deterministic render-path proxies ───────────────────
//
// These assert on `App::perf_counters()` — wall-clock-free counts — so they
// gate the redraw-throttling and per-frame caching optimizations without timing
// flakiness. The acceptance harness drives `view()` directly (it skips
// `tick()`), so only the render-path counters are exercised here; the
// tick-driven counters (`status_refreshes`) and the redraw-skip accounting live
// in the `#[tokio::test]` units in `super::tests`.

#[test]
fn perf_hud_toggles_with_f12_and_activates_timing() {
    let mut h = Harness::standard(1);
    assert!(!h.app.perf_timing_active(), "timing is off by default");
    h.key(KeyCode::F(12), KeyModifiers::NONE);
    assert!(h.app.show_perf_hud, "F12 opens the perf HUD");
    assert!(
        h.app.perf_timing_active(),
        "an open HUD switches timing collection on"
    );
    h.render(); // the overlay renders without disturbing the panes
    h.key(KeyCode::F(12), KeyModifiers::NONE);
    assert!(!h.app.show_perf_hud, "F12 closes it again");
}

#[test]
fn perf_hud_feature_flag_disables_toggle_and_closes_overlay() {
    let mut h = Harness::standard(1);
    h.key(KeyCode::F(12), KeyModifiers::NONE);
    assert!(h.app.show_perf_hud);
    // Disabling the live flag tears the overlay down and blocks the chord.
    let mut settings = crate::session::settings::Settings::default();
    settings.features.perf_hud = false;
    h.app.apply_live_settings(&settings);
    assert!(!h.app.show_perf_hud, "disabling the flag closes the HUD");
    h.key(KeyCode::F(12), KeyModifiers::NONE);
    assert!(!h.app.show_perf_hud, "the chord toasts instead of toggling");
}

#[test]
fn perf_render_counter_tracks_painted_frames() {
    let mut h = Harness::standard(2);
    assert_eq!(h.app.perf_counters().frames_rendered, 0);
    h.render();
    h.render();
    h.render();
    assert_eq!(
        h.app.perf_counters().frames_rendered,
        3,
        "each view() paint bumps frames_rendered exactly once"
    );
}

#[test]
fn perf_terminal_render_locks_parser_once_per_frame() {
    // With an active session, the central pane locks its vt100 parser once per
    // painted frame (the O(1) scrollback read rides along, so it is not tracked
    // separately). Redraw throttling, not caching, bounds how often this runs.
    let mut h = Harness::standard(1);
    h.render();
    h.render();
    assert_eq!(
        h.app.perf_counters().parser_locks_render,
        2,
        "one parser lock per terminal frame"
    );
}

#[test]
fn perf_session_order_cached_across_idle_frames() {
    // The session-list ordering is status-independent, so once built it is
    // reused across frames whose grouping/nesting inputs didn't change. Three
    // paints with no session mutation must rebuild the order exactly once.
    let mut h = Harness::standard(3);
    h.render();
    h.render();
    h.render();
    assert_eq!(
        h.app.perf_counters().ordered_sessions_rebuilds,
        1,
        "the session order is cached: only the first frame rebuilds it"
    );
}

#[test]
fn perf_session_order_rebuilds_when_sessions_change() {
    // Adding a session changes the order signature, so the cache is invalidated
    // and the order rebuilt — exactly once for the change.
    let mut h = Harness::standard(2);
    h.render(); // builds the order (rebuild #1)
    h.render(); // cache hit, no rebuild
    assert_eq!(h.app.perf_counters().ordered_sessions_rebuilds, 1);

    // Mutate the session set, then repaint.
    let backend: Arc<dyn SessionBackend> = Arc::new(FakeBackend::stub());
    let provider: Arc<dyn AgentProvider> = Arc::new(GenericProvider::new(
        crate::agent::agent_config::builtin_registry()
            .default_agent()
            .unwrap()
            .clone(),
    ));
    h.app
        .sessions
        .push(Session::stub("session-new", &backend, &provider));
    h.render(); // signature changed → rebuild #2
    h.render(); // cache hit again
    assert_eq!(
        h.app.perf_counters().ordered_sessions_rebuilds,
        2,
        "a session-set change invalidates the cache exactly once"
    );
}

#[test]
fn perf_status_change_keeps_order_cache() {
    // The order is status-independent (ADR-P3): a session changing status must
    // NOT invalidate the cache — only grouping/ordering/nesting inputs do. This
    // pins the signature's field set; adding `status` to it would fail here.
    let mut h = Harness::standard(2);
    h.render(); // rebuild #1
    h.render(); // cache hit
    assert_eq!(h.app.perf_counters().ordered_sessions_rebuilds, 1);

    h.app.sessions[0].info.status = SessionStatus::Blocked;
    h.render(); // status changed, but order inputs did not → still a cache hit
    assert_eq!(
        h.app.perf_counters().ordered_sessions_rebuilds,
        1,
        "a status change must not rebuild the (status-independent) order"
    );
}

// ── Redraw throttling: the dirty-flag decision the render loop gates on ───────

#[test]
fn perf_first_frame_is_always_dirty() {
    // `needs_redraw` starts true so the very first loop iteration paints (the
    // smoke test and a real launch both rely on this).
    let h = Harness::standard(1);
    assert!(h.app.should_redraw(), "a freshly built App must paint once");
}

#[test]
fn perf_clean_state_skips_redraw() {
    // After a paint with nothing changed, the loop skips the (expensive) draw.
    let mut h = Harness::standard(1);
    h.app.mark_redrawn();
    assert!(
        !h.app.should_redraw(),
        "no input/output/forced-floor → no redraw"
    );
}

#[test]
fn perf_input_requests_redraw() {
    // Any key event re-dirties the UI so keypress-to-screen stays immediate.
    let mut h = Harness::standard(1);
    h.app.mark_redrawn();
    assert!(!h.app.should_redraw());
    h.ctrl('j'); // NextSession — goes through update()
    assert!(
        h.app.should_redraw(),
        "input must mark the UI dirty for the next frame"
    );
}

#[test]
fn perf_no_new_output_does_not_request_redraw() {
    // The lock-free output detector must not false-positive: with no reader
    // thread producing output, a second poll sees an unchanged signature and
    // leaves the UI clean.
    let mut h = Harness::standard(2);
    h.app.detect_output_redraw(); // prime the output-generation baseline
    h.app.mark_redrawn(); // clear any dirty from the first observation
    h.app.detect_output_redraw(); // no new output
    assert!(
        !h.app.should_redraw(),
        "unchanged output signature must not trigger a redraw"
    );
}

#[test]
fn perf_idle_iterations_skip_the_paint() {
    // Mimic the render loop's gate over several idle iterations (well within the
    // forced-redraw floor): the first paints, the rest are skipped.
    let mut h = Harness::standard(2);
    h.app.detect_output_redraw(); // prime output baseline
    let mut requested = 0u64;
    let mut skipped = 0u64;
    for _ in 0..5 {
        if h.app.should_redraw() {
            h.app.mark_redrawn();
            requested += 1;
        } else {
            h.app.note_redraw_skipped();
            skipped += 1;
        }
        h.app.detect_output_redraw(); // no new output between iterations
    }
    assert_eq!(requested, 1, "only the initial dirty frame paints");
    assert_eq!(skipped, 4, "idle iterations skip the expensive draw");
    assert_eq!(h.app.perf_counters().redraws_skipped, 4);
}

/// Disabling a live feature flag at runtime tears down whatever panel/view it
/// had left open (otherwise the panel keeps rendering with its tab/footer
/// affordance gone). Covers the `apply_live_settings` → `enforce_feature_visibility`
/// path the settings panel and config-reload both run.
#[tokio::test]
async fn disabling_a_live_feature_tears_down_its_open_surfaces() {
    let mut h = Harness::spawnable(1);
    let sid = h.app.sessions[0].info.id;

    // Open every live-gated surface, and park focus on the file viewer.
    h.app.show_info_panel = true;
    h.app.show_file_viewer = true;
    h.app.show_tasks_panel = true;
    h.app
        .session_terminal_views
        .insert(sid, TerminalView::Shell);
    open_minimal_review(&mut h);
    h.app.focus = InputFocus::FileViewer;

    // Flip the live UI feature flags off and re-apply (as the settings panel does).
    let mut settings = crate::session::settings::Settings::default();
    settings.features.info_panel = false;
    settings.features.file_viewer = false;
    settings.features.tasks = false;
    settings.features.shell_pane = false;
    settings.features.code_review = false;
    h.app.apply_live_settings(&settings);

    assert!(!h.app.show_info_panel, "info panel hidden");
    assert!(!h.app.show_file_viewer, "file viewer hidden");
    assert!(!h.app.show_tasks_panel, "tasks panel hidden");
    assert_eq!(
        h.app.session_terminal_views.get(&sid).copied(),
        Some(TerminalView::Claude),
        "shell view reverted to the agent view"
    );
    assert!(h.app.code_reviews.is_empty(), "open review closed");
    assert!(
        matches!(h.app.focus, InputFocus::SessionList),
        "focus moved off the now-hidden file viewer"
    );
}

/// `dispatch_action` partitions `Action` across several sub-dispatchers whose
/// final arm (`dispatch_scoped_pane_action`) is `unreachable!()`. A new `Action`
/// variant that isn't wired into any dispatcher would therefore panic at runtime
/// instead of failing to compile — this exercises every variant through the real
/// dispatch path so an unrouted action fails the suite loudly. A fresh harness
/// per action keeps the routing decision independent of accumulated side effects.
#[tokio::test]
async fn every_action_is_routed_by_dispatch_action() {
    for &action in crate::session::Action::all() {
        let mut h = Harness::standard(1);
        // The assertion is simply that this does not hit the `unreachable!()` in
        // `dispatch_scoped_pane_action` (or otherwise panic).
        let _ = h.app.dispatch_action(action);
    }
}

/// Install a minimal open+focused review on the harness (no git worktree
/// needed), for testing the view's key fall-through behavior.
fn open_minimal_review(h: &mut Harness) {
    use std::collections::HashSet;
    let sid = h.app.sessions[0].info.id;
    h.app.code_reviews.insert(
        sid,
        crate::app::code_review::CodeReviewState {
            session_id: sid,
            repos: Vec::new(),
            multi: false,
            files: Vec::new(),
            comments: Vec::new(),
            reviewed_files: HashSet::new(),
            reviewed_hunks: HashSet::new(),
            fold_override: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            compose: None,
            side_by_side: false,
            click_side: None,
            h_scroll: 0,
            wrap: false,
            target: crate::app::code_review::ReviewTarget::Working,
            commits: Vec::new(),
            host: None,
            target_picker: None,
            search: None,
        },
    );
    h.app.focus = InputFocus::CodeReview;
}

/// The review pane toggles shut on its own key, like every other pane: with a
/// review open and focused, pressing the bound chord (F7) again closes it and
/// moves focus away. Regression for the key being swallowed by the review's
/// own capture handler.
#[test]
fn review_toggle_key_closes_open_review() {
    let mut h = Harness::new(STD_COLS, STD_ROWS, 1);
    open_minimal_review(&mut h);

    h.key(KeyCode::F(7), KeyModifiers::NONE);

    assert!(
        h.app.active_review().is_none(),
        "pressing the review toggle again closes the open review"
    );
    assert_ne!(
        h.app.focus,
        InputFocus::CodeReview,
        "focus leaves the review when it closes"
    );
}

/// `/` opens find-in-diff (file-viewer pattern): typing jumps to the first
/// match, `Tab` commits, `n`/`N` step matches relative to the cursor, and `Esc`
/// clears the search before it closes the review.
#[test]
fn review_search_flow_finds_navigates_and_clears() {
    let mut h = Harness::new(STD_COLS, STD_ROWS, 1);
    let sid = h.app.sessions[0].info.id;
    // Two files: src/f0.rs, src/f1.rs (each one added line).
    h.app.code_reviews.insert(
        sid,
        crate::app::code_review::CodeReviewState::for_test(sid, 2),
    );
    h.app.focus = InputFocus::CodeReview;

    // `/` enters the search sub-mode (capturing keys).
    h.key(KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(h
        .app
        .active_review()
        .and_then(|cr| cr.search.as_ref())
        .is_some_and(|s| s.editing));

    // Type ".rs" → matches both file headers; selection jumps to the first.
    for c in ['.', 'r', 's'] {
        h.key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    let (first, second) = {
        let cr = h.app.active_review().unwrap();
        let s = cr.search.as_ref().unwrap();
        assert_eq!(s.matches.len(), 2, "both file headers match '.rs'");
        assert_eq!(cr.selected, s.matches[0], "jumps to the first match");
        (s.matches[0], s.matches[1])
    };

    // Enter (while typing) steps to the next match without leaving the input.
    h.key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(h.app.active_review().unwrap().selected, second);
    assert!(
        h.app
            .active_review()
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .editing
    );

    // Tab commits: still open, no longer editing.
    h.key(KeyCode::Tab, KeyModifiers::NONE);
    assert!(h
        .app
        .active_review()
        .and_then(|cr| cr.search.as_ref())
        .is_some_and(|s| !s.editing));

    // `n`/`N` step matches relative to the cursor (wrapping).
    h.key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert_eq!(
        h.app.active_review().unwrap().selected,
        first,
        "n from the last match wraps to the first"
    );
    h.key(KeyCode::Char('N'), KeyModifiers::NONE);
    assert_eq!(
        h.app.active_review().unwrap().selected,
        second,
        "N from the first match wraps to the last"
    );

    // Esc clears the search but keeps the review open; a second Esc closes it.
    h.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(h.app.active_review().is_some());
    assert!(h.app.active_review().unwrap().search.is_none());
    h.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(h.app.active_review().is_none());
}

/// Insert a review whose first diff line is `width` chars wide, so horizontal
/// scroll / wrap have something to act on. Returns the session id.
#[cfg(test)]
fn open_review_with_long_line(h: &mut Harness, width: usize) -> crate::session::SessionId {
    let sid = h.app.sessions[0].info.id;
    let mut cr = crate::app::code_review::CodeReviewState::for_test(sid, 1);
    cr.files[0].hunks[0].lines[0].text = "a".repeat(width);
    cr.rebuild_rows();
    h.app.code_reviews.insert(sid, cr);
    h.app.focus = InputFocus::CodeReview;
    sid
}

/// `Left`/`Right` (and `h`/`l`) scroll the diff body horizontally, clamped to
/// the longest line; `w` toggles wrap and resets the offset; scroll is a no-op
/// while wrapped.
#[test]
fn review_horizontal_scroll_and_wrap_toggle() {
    let mut h = Harness::new(STD_COLS, STD_ROWS, 1);
    open_review_with_long_line(&mut h, 300);

    // Right scrolls the body (step 8); Left scrolls back and clamps at 0.
    h.key(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(h.app.active_review().unwrap().h_scroll, 8);
    h.key(KeyCode::Char('l'), KeyModifiers::NONE);
    assert_eq!(
        h.app.active_review().unwrap().h_scroll,
        16,
        "`l` also scrolls right"
    );
    for _ in 0..10 {
        h.key(KeyCode::Left, KeyModifiers::NONE);
    }
    assert_eq!(
        h.app.active_review().unwrap().h_scroll,
        0,
        "Left clamps at 0"
    );

    // A big jump clamps to the longest line (max_line_width - 1 = 299).
    for _ in 0..100 {
        h.key(KeyCode::Right, KeyModifiers::NONE);
    }
    assert_eq!(
        h.app.active_review().unwrap().h_scroll,
        299,
        "scroll clamps to the widest line"
    );

    // `w` turns on wrap and resets the horizontal offset; while wrapped, scroll
    // is a no-op.
    h.key(KeyCode::Char('w'), KeyModifiers::NONE);
    {
        let cr = h.app.active_review().unwrap();
        assert!(cr.wrap, "`w` enables wrap");
        assert_eq!(cr.h_scroll, 0, "enabling wrap resets h_scroll");
    }
    h.key(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(
        h.app.active_review().unwrap().h_scroll,
        0,
        "horizontal scroll is a no-op while wrapped"
    );

    // `w` again turns wrap off.
    h.key(KeyCode::Char('w'), KeyModifiers::NONE);
    assert!(!h.app.active_review().unwrap().wrap);
}

/// Entering side-by-side pins the horizontal offset to 0 (h-scroll is
/// unified-only) and scroll stays a no-op there.
#[test]
fn review_side_by_side_disables_horizontal_scroll() {
    let mut h = Harness::new(STD_COLS, STD_ROWS, 1);
    open_review_with_long_line(&mut h, 300);

    h.key(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(h.app.active_review().unwrap().h_scroll, 8);

    // `v` → side-by-side resets the offset.
    h.key(KeyCode::Char('v'), KeyModifiers::NONE);
    {
        let cr = h.app.active_review().unwrap();
        assert!(cr.side_by_side);
        assert_eq!(cr.h_scroll, 0, "side-by-side resets h_scroll");
    }
    h.key(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(
        h.app.active_review().unwrap().h_scroll,
        0,
        "no horizontal scroll in side-by-side"
    );
}

/// A review is per-session like the shell view: switching to another session
/// hides it (and demotes the central focus), and switching back shows it again
/// — the state is preserved, not torn down.
#[test]
fn review_persists_per_session_across_switches() {
    let mut h = Harness::new(STD_COLS, STD_ROWS, 2);
    h.app.active_index = 0;
    open_minimal_review(&mut h); // review open + focused on session 0
    h.render();
    assert!(h.app.active_review().is_some());
    assert_eq!(h.app.focus, InputFocus::CodeReview);

    // Switch to session 1 (no review): it's hidden and focus drops off the
    // review (synced on render).
    h.app.active_index = 1;
    h.render();
    assert!(
        h.app.active_review().is_none(),
        "the other session has no review"
    );
    assert_ne!(
        h.app.focus,
        InputFocus::CodeReview,
        "focus leaves the review when its session isn't active"
    );

    // Switch back to session 0: the review is still there and re-focused.
    h.app.active_index = 0;
    h.render();
    assert!(
        h.app.active_review().is_some(),
        "session 0's review is preserved across the round-trip"
    );
    assert_eq!(
        h.app.focus,
        InputFocus::CodeReview,
        "returning to the review session re-focuses it"
    );
}

/// Hovering a code-review footer button brightens its fill to `accent_bright`,
/// exactly like the global footer and modal buttons. Regression: review footer
/// buttons (recorded as `ClickAction::ReviewButton`) were left out of the hover
/// highlight, so they never lit up under the pointer.
#[test]
fn hovering_review_footer_button_brightens_it() {
    let mut h = Harness::new(STD_COLS, STD_ROWS, 1);
    open_minimal_review(&mut h);
    h.render();
    let r = h
        .app
        .click_targets
        .iter()
        .find(|t| matches!(t.action, ClickAction::ReviewButton(_)))
        .map(|t| t.rect)
        .expect("review footer buttons recorded");
    h.app.update(AppMessage::MouseMove { x: r.x, y: r.y });
    h.render();
    let buf = h.terminal.backend().buffer();
    assert_eq!(
        buf[(r.x, r.y)].bg,
        crate::ui::theme::Theme::accent_bright(),
        "hovered review footer button should brighten to accent_bright"
    );
}

/// Clicking a diff row in the main review pane focuses it (regression: the
/// `ReviewRow` click selected the row but never set `InputFocus::CodeReview`,
/// so a click while another pane was focused left focus elsewhere — the
/// whole-pane `FocusPane` fallback is recorded after the row targets and never
/// wins on a row hit).
#[test]
fn clicking_review_row_focuses_the_pane() {
    let mut h = Harness::new(STD_COLS, STD_ROWS, 1);
    let sid = h.app.sessions[0].info.id;
    h.app.code_reviews.insert(
        sid,
        crate::app::code_review::CodeReviewState::for_test(sid, 2),
    );
    h.app.focus = InputFocus::CodeReview;
    h.render();
    // Move focus off the review, as if the session list were active.
    h.app.focus = InputFocus::SessionList;
    let r = h
        .app
        .click_targets
        .iter()
        .find(|t| matches!(t.action, ClickAction::ReviewRow(_)))
        .map(|t| t.rect)
        .expect("review diff rows recorded as click targets");
    h.app.update(AppMessage::MouseClick {
        x: r.x,
        y: r.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        h.app.focus,
        InputFocus::CodeReview,
        "clicking a diff row focuses the review pane"
    );
}

/// A click in the paired side-by-side layout records which column (old | new)
/// it hit, so a follow-up comment attaches to that side; a later column-less
/// select (scrollbar drag) clears it.
#[test]
fn side_by_side_click_records_column_side() {
    use crate::session::review::Side;
    let mut h = Harness::new(STD_COLS, STD_ROWS, 1);
    let sid = h.app.sessions[0].info.id;
    let mut cr = crate::app::code_review::CodeReviewState::for_test(sid, 1);
    cr.side_by_side = true;
    cr.rebuild_rows();
    h.app.code_reviews.insert(sid, cr);
    h.app.focus = InputFocus::CodeReview;
    h.render();
    let r = h
        .app
        .click_targets
        .iter()
        .find(|t| matches!(t.action, ClickAction::ReviewRow(_)))
        .map(|t| t.rect)
        .expect("review diff rows recorded as click targets");

    // Right column → the New side is recorded for that row.
    h.app.update(AppMessage::MouseClick {
        x: r.x + r.width - 1,
        y: r.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        matches!(
            h.app.active_review().unwrap().click_side,
            Some((_, Side::New))
        ),
        "a right-column click records the New side"
    );

    // Left column → the Old side.
    h.app.update(AppMessage::MouseClick {
        x: r.x,
        y: r.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        matches!(
            h.app.active_review().unwrap().click_side,
            Some((_, Side::Old))
        ),
        "a left-column click records the Old side"
    );

    // A column-less select (scrollbar drag / cr_select_row) clears it.
    let sel = h.app.active_review().unwrap().selected;
    h.app.cr_select_row(sel);
    assert!(
        h.app.active_review().unwrap().click_side.is_none(),
        "a column-less select clears the recorded click side"
    );
}

/// The review-target picker is mouse-driven too: clicking one of its entries
/// switches the diff to that target, mirroring the keyboard Enter path.
#[test]
fn clicking_review_target_entry_switches_target() {
    use crate::app::code_review::ReviewTarget;
    let mut h = Harness::new(STD_COLS, STD_ROWS, 1);
    let sid = h.app.sessions[0].info.id;
    h.app.code_reviews.insert(
        sid,
        crate::app::code_review::CodeReviewState::for_test(sid, 2),
    );
    h.app.focus = InputFocus::CodeReview;
    // The picker opens on Branch (the for_test default); it offers Working +
    // Branch entries.
    h.app.cr_open_target_picker();
    assert_eq!(h.app.active_review().unwrap().target, ReviewTarget::Branch);
    h.render();

    // Find the "Working" entry's hitbox (index 0) and click it.
    let r = h
        .app
        .click_targets
        .iter()
        .find(|t| matches!(t.action, ClickAction::ReviewTarget(0)))
        .map(|t| t.rect)
        .expect("target-picker entries recorded as click targets");
    h.app.update(AppMessage::MouseClick {
        x: r.x,
        y: r.y,
        modifiers: KeyModifiers::NONE,
    });

    let cr = h.app.active_review().unwrap();
    assert_eq!(
        cr.target,
        ReviewTarget::Working,
        "clicking a target-picker entry switches the diff to that target"
    );
    assert!(
        cr.target_picker.is_none(),
        "selecting a target closes the picker"
    );
}

/// Global overlay/panel toggles fall through the review's key capture so they
/// stay reachable while a review is open (regression: the capture handler
/// swallowed them). The review itself stays open.
#[test]
fn info_panel_toggles_while_review_is_open() {
    let mut h = Harness::new(STD_COLS, STD_ROWS, 1);
    open_minimal_review(&mut h);
    assert!(!h.app.show_info_panel);

    h.key(KeyCode::F(2), KeyModifiers::NONE);

    assert!(
        h.app.show_info_panel,
        "F2 toggles the info panel even while the review is focused"
    );
    assert!(
        h.app.active_review().is_some(),
        "toggling the info panel leaves the review open"
    );
}

// ── Remote-hook status events (control-mode subscription → hook columns) ──────

/// The backend queued a remote-hook event for a session's pane: one refresh
/// drains it into the hook columns and the derived status reflects it — the
/// remote analogue of a local `thurbox-cli session signal`.
#[test]
fn remote_hook_event_drives_session_status() {
    let backend = Arc::new(FakeBackend::stub());
    let mut h = Harness::with_backend(STD_COLS, STD_ROWS, 2, backend.clone());
    h.app.sessions[0].info.backend_id = Some("%5".into());
    h.app.sessions[1].info.backend_id = Some("%9".into());
    h.app.save_state(); // hook columns update persisted rows

    backend.push_hook_event("%5", "working");
    h.app.refresh_session_statuses();

    // Stub sessions have fresh output, so `working` isn't quiescence-demoted.
    assert_eq!(h.app.sessions[0].info.status, SessionStatus::Working);
    assert_eq!(
        h.app.sessions[1].info.status,
        SessionStatus::Idle,
        "the other session is untouched"
    );
    let rows = h.app.db.load_hook_states().unwrap();
    assert_eq!(
        rows.get(&h.app.sessions[0].info.id)
            .and_then(|r| r.state.as_deref()),
        Some("working"),
        "the event is persisted through set_hook_state"
    );
}

/// Events that don't resolve to a session never touch the DB (an unknown pane
/// is *parked* for the adoption retry — see
/// `remote_hook_event_parked_until_session_adopted` — not applied), and a
/// non-allow-listed state (the value is remote-controlled free text) is
/// dropped outright.
#[test]
fn remote_hook_event_ignores_unmatched_and_invalid() {
    let backend = Arc::new(FakeBackend::stub());
    let mut h = Harness::with_backend(STD_COLS, STD_ROWS, 1, backend.clone());
    h.app.sessions[0].info.backend_id = Some("%5".into());
    h.app.save_state();

    backend.push_hook_event("%99", "working"); // no such pane
    backend.push_hook_event("%5", "rm -rf /"); // not an allowed state
    h.app.refresh_session_statuses();

    assert_eq!(h.app.sessions[0].info.status, SessionStatus::Idle);
    assert!(
        h.app
            .db
            .load_hook_states()
            .unwrap()
            .values()
            .all(|r| r.state.is_none()),
        "neither event may reach the hook columns"
    );
}

/// An event for a pane no session claims *yet* is parked and re-applied once
/// the session appears: the subscription's initial catch-up report lands while
/// the background restore is still adopting that host's windows, and dropping
/// it would lose e.g. a `done` set while the TUI was closed.
#[test]
fn remote_hook_event_parked_until_session_adopted() {
    let backend = Arc::new(FakeBackend::stub());
    let mut h = Harness::with_backend(STD_COLS, STD_ROWS, 1, backend.clone());
    let id = h.app.sessions[0].info.id;

    backend.push_hook_event("%5", "working");
    h.app.refresh_session_statuses(); // no session owns %5 yet → parked

    h.app.sessions[0].info.backend_id = Some("%5".into());
    h.app.save_state();
    h.app.refresh_session_statuses(); // nothing new pushed — the parked event applies

    let rows = h.app.db.load_hook_states().unwrap();
    assert_eq!(
        rows.get(&id).and_then(|r| r.state.as_deref()),
        Some("working"),
        "the pre-adoption event must survive to the adopting tick"
    );
}

/// Two transitions for one pane in a single drained batch (`working` then
/// `done`, e.g. queued while the main thread stalled) must both land: deduping
/// the second against the stale pre-batch cache would swallow the `done` and
/// leave the session spinning on `working`.
#[test]
fn remote_hook_batch_applies_both_transitions() {
    let backend = Arc::new(FakeBackend::stub());
    let mut h = Harness::with_backend(STD_COLS, STD_ROWS, 1, backend.clone());
    h.app.sessions[0].info.backend_id = Some("%5".into());
    h.app.save_state();
    let id = h.app.sessions[0].info.id;

    // A previous turn ended `done`, absorbed into the cache.
    backend.push_hook_event("%5", "done");
    h.app.refresh_session_statuses();

    backend.push_hook_event("%5", "working");
    backend.push_hook_event("%5", "done");
    h.app.refresh_session_statuses();

    let rows = h.app.db.load_hook_states().unwrap();
    assert_eq!(
        rows.get(&id).and_then(|r| r.state.as_deref()),
        Some("done"),
        "the batch's final transition must not be swallowed by the stale cache"
    );
}

/// A re-report of the current state (the subscription re-sends the pane
/// option's value on reconnect/TUI restart) must not re-stamp `state_at` —
/// otherwise an already-acknowledged `done` resurrects as unseen and re-fires
/// its OS notification on every restart.
#[test]
fn remote_hook_event_dedupes_repeated_state() {
    let backend = Arc::new(FakeBackend::stub());
    let mut h = Harness::with_backend(STD_COLS, STD_ROWS, 1, backend.clone());
    h.app.sessions[0].info.backend_id = Some("%5".into());
    h.app.save_state();
    let id = h.app.sessions[0].info.id;

    backend.push_hook_event("%5", "done");
    h.app.refresh_session_statuses();
    let first_at = h.app.db.load_hook_states().unwrap()[&id].state_at;
    assert!(first_at.is_some());

    std::thread::sleep(std::time::Duration::from_millis(5));
    backend.push_hook_event("%5", "done");
    h.app.refresh_session_statuses();
    let second_at = h.app.db.load_hook_states().unwrap()[&id].state_at;
    assert_eq!(
        first_at, second_at,
        "an identical re-report must not re-stamp state_at"
    );
}

// ── Tick-driven behavior: timers, debounce, redraw floor ─────────────────────
//
// These drive `App::tick_core` (the deterministic half of the event loop's
// tick) with the clock fast-forwarded via `Harness::advance`, so every
// wall-clock-gated behavior is asserted without sleeping.

#[test]
fn status_message_expires_after_timeout_via_tick() {
    let mut h = Harness::standard(1);
    h.app.set_status(StatusLevel::Info, "transient note");
    assert!(h.app.status_message.is_some());

    h.tick();
    assert!(
        h.app.status_message.is_some(),
        "a fresh message survives a tick"
    );

    h.advance(STATUS_MESSAGE_TIMEOUT).tick();
    assert!(
        h.app.status_message.is_none(),
        "the tick clears an expired status message"
    );
}

#[test]
fn pending_delete_finalizes_after_undo_window() {
    let mut h = Harness::standard(2);
    h.ctrl('d'); // DeleteSession (soft) — starts the undo window
    assert!(h.app.pending_delete.is_some());

    // Inside the window the delete stays pending (undoable).
    h.advance(UNDO_TIMEOUT - std::time::Duration::from_secs(1))
        .tick();
    assert!(
        h.app.pending_delete.is_some(),
        "still undoable inside the window"
    );

    h.advance(std::time::Duration::from_secs(2)).tick();
    assert!(
        h.app.pending_delete.is_none(),
        "the expired window finalizes the delete"
    );

    h.ctrl('z'); // UndoDelete — too late now
    assert_eq!(
        h.app.sessions.len(),
        1,
        "a finalized delete can no longer be undone"
    );
}

#[test]
fn global_search_content_scan_waits_for_debounce() {
    let mut h = Harness::standard(2);
    h.feed_output(1, b"a unique zebra-crossing appears\r\n");

    h.ctrl('/'); // GlobalSearch
    for c in "zebra".chars() {
        h.key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    let content_hit = |app: &App| {
        app.global_search
            .results
            .iter()
            .any(|r| r.kind == search::SearchKind::Session && r.snippet.is_some())
    };

    h.tick();
    assert!(
        !content_hit(&h.app),
        "the expensive buffer scan is debounced — no content hit immediately"
    );
    assert!(h.app.global_search.content_dirty, "a scan is pending");

    h.advance(std::time::Duration::from_millis(
        search::CONTENT_DEBOUNCE_MS + 10,
    ))
    .tick();
    assert!(
        content_hit(&h.app),
        "once the query settles, the tick scans session buffers"
    );
    assert!(!h.app.global_search.content_dirty);
}

#[test]
fn forced_redraw_floor_repaints_after_interval() {
    let mut h = Harness::standard(1);
    h.app.mark_redrawn();
    assert!(
        !h.app.should_redraw(),
        "clean state right after a paint — no redraw needed"
    );

    h.advance(FORCE_REDRAW_INTERVAL);
    assert!(
        h.app.should_redraw(),
        "the forced-redraw floor repaints time-driven UI"
    );
}

// ── Regressions the monkey test originally caught ────────────────────────────

#[test]
fn global_search_on_short_terminal_does_not_panic_session_resize() {
    // The search strip + footer can eat a short terminal's entire height,
    // producing a zero-row content area. `Session::resize` must clamp before
    // vt100's `set_size` (which underflows on 0) — this panicked pre-clamp.
    let mut h = Harness::new(30, 8, 1);
    h.render();
    h.ctrl('/'); // GlobalSearch — resizes sessions to the shrunken content area
    h.render();
    assert!(h.app.global_search.active);
}

#[test]
fn narrow_resize_rescues_task_editor_focus() {
    // Shrinking below 120 cols hides the tasks panel; focus must leave the
    // *editor* too, or it keeps capturing every key for an invisible surface.
    let mut h = Harness::standard(1);
    h.ctrl('w'); // FocusTasks
    h.key(KeyCode::Char('n'), KeyModifiers::NONE); // new task → TaskEditor
    assert!(matches!(h.app.focus, InputFocus::TaskEditor));

    h.resize(100, 40);
    assert!(!h.app.show_tasks_panel, "narrow layout hides the panel");
    assert!(
        matches!(h.app.focus, InputFocus::SessionList),
        "focus is rescued off the hidden panel's editor"
    );
    h.render();
}

// ── Injected agent output: the PTY seam ──────────────────────────────────────

#[test]
fn injected_output_marks_redraw_and_renders() {
    let mut h = Harness::standard(1);
    // Sync the output-change detector, then settle to a clean state.
    h.app.detect_output_redraw();
    h.app.mark_redrawn();
    h.app.detect_output_redraw();
    assert!(!h.app.should_redraw(), "no new output ⇒ no repaint");

    h.feed_output(0, b"MARKER-7f3a output line\r\n");
    h.app.detect_output_redraw();
    assert!(h.app.should_redraw(), "new output marks the UI dirty");

    let screen = h.render();
    assert!(
        screen.contains("MARKER-7f3a"),
        "injected output reaches the rendered terminal pane:\n{screen}"
    );
}

#[test]
fn osc_title_and_bell_reach_the_session() {
    let mut h = Harness::standard(1);

    h.feed_output(0, b"\x1b]0;Reticulating splines\x07");
    assert_eq!(
        h.app.sessions[0].agent_title().as_deref(),
        Some("Reticulating splines"),
        "an OSC 0 title lands in the session's activity text"
    );

    assert!(!h.app.sessions[0].needs_attention());
    h.feed_output(0, b"\x07");
    assert!(
        h.app.sessions[0].needs_attention(),
        "a BEL raises the attention flag"
    );
}

#[test]
fn split_utf8_output_chunks_render_intact() {
    // The reader loop protects vt100 from mid-codepoint chunks with a carry
    // buffer; `feed_output` bypasses the reader, so this documents that a test
    // feeding whole-codepoint chunks renders multi-byte text correctly (the
    // carry logic itself is unit-tested via `utf8_ready_prefix_len`).
    let mut h = Harness::standard(1);
    h.feed_output(0, "boîte — ünïcode ✓\r\n".as_bytes());
    let screen = h.render();
    assert!(
        screen.contains("boîte — ünïcode ✓"),
        "multi-byte output renders intact:\n{screen}"
    );
}

// ── Invariant tripwires + deterministic monkey test ──────────────────────────

/// Structural invariants that must hold after *any* event, in any order. The
/// monkey test checks these after every step; when a "weird TUI behavior" is
/// reduced to a rule ("focus never rests on a hidden pane"), add it here and
/// the monkey hunts for a sequence that breaks it.
fn assert_invariants(app: &App, ctx: &str) {
    assert!(
        app.sessions.is_empty() || app.active_index < app.sessions.len(),
        "[{ctx}] active_index {} out of bounds ({} sessions)",
        app.active_index,
        app.sessions.len()
    );
    assert!(
        app.task_ui.filtered_task_indices.is_empty()
            || app.task_ui.task_panel_index < app.task_ui.filtered_task_indices.len(),
        "[{ctx}] task panel selection out of bounds"
    );
    assert!(
        app.automation_ui.cached_automations.is_empty()
            || app.automation_ui.automation_panel_index
                < app.automation_ui.cached_automations.len(),
        "[{ctx}] automation pane selection out of bounds"
    );

    // Panel visibility never outlives its feature flag.
    assert!(
        !app.show_tasks_panel || app.features.tasks,
        "[{ctx}] tasks panel shown with the feature disabled"
    );
    assert!(
        !app.show_file_viewer || app.features.file_viewer,
        "[{ctx}] file viewer shown with the feature disabled"
    );

    // Focus only ever rests on a surface that exists.
    match app.focus {
        InputFocus::TaskList | InputFocus::TaskEditor => assert!(
            app.features.tasks && app.show_tasks_panel,
            "[{ctx}] focus {:?} but the tasks panel is hidden",
            app.focus
        ),
        InputFocus::FileViewer => assert!(
            app.show_file_viewer,
            "[{ctx}] focus on a hidden file viewer"
        ),
        InputFocus::GlobalSearch => assert!(
            app.global_search.active,
            "[{ctx}] focus on a closed search strip"
        ),
        InputFocus::CodeReview | InputFocus::ReviewFiles => {
            assert!(
                app.features.code_review,
                "[{ctx}] review focus with the feature disabled"
            );
            assert!(
                app.active_review().is_some(),
                "[{ctx}] focus {:?} but the active session has no open review",
                app.focus
            );
        }
        InputFocus::Automations
        | InputFocus::AutomationEditor
        | InputFocus::AutomationRunHistory => assert!(
            app.features.automations,
            "[{ctx}] automations focus with the feature disabled"
        ),
        InputFocus::SessionList | InputFocus::Terminal => {}
    }

    if app.global_search.active {
        assert!(
            app.features.global_search,
            "[{ctx}] search strip active with the feature disabled"
        );
    }
}

/// Deterministic pseudo-random stream (an LCG — no dev-dependency, and a
/// failing seed reproduces exactly).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// `Ctrl` chords the monkey may press. Excluded on purpose:
/// `n` (repo picker `Enter` can reach real-git branch listing on the dev
/// machine), `o` (spawns `$EDITOR`), `v` (reads the system clipboard),
/// `q` (quit is a terminal state with nothing to fuzz behind it).
const MONKEY_CTRL: &[char] = &[
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'j', 'k', 'l', 'p', 'r', 's', 't', 'u', 'w', 'x', 'y',
    'z', '/', ',',
];

/// Plain (unmodified) keys the monkey may press: pane-scoped letters, digits,
/// and the structural navigation/editing keys.
const MONKEY_KEYS: &[KeyCode] = &[
    KeyCode::Char('j'),
    KeyCode::Char('k'),
    KeyCode::Char('h'),
    KeyCode::Char('l'),
    KeyCode::Char('g'),
    KeyCode::Char('r'),
    KeyCode::Char('d'),
    KeyCode::Char('e'),
    KeyCode::Char('n'),
    KeyCode::Char('s'),
    KeyCode::Char('w'),
    KeyCode::Char('y'),
    KeyCode::Char('/'),
    KeyCode::Char(' '),
    KeyCode::Char('1'),
    KeyCode::Char('9'),
    KeyCode::Esc,
    KeyCode::Enter,
    KeyCode::Tab,
    KeyCode::Backspace,
    KeyCode::Up,
    KeyCode::Down,
    KeyCode::Left,
    KeyCode::Right,
    KeyCode::PageUp,
    KeyCode::PageDown,
    KeyCode::Home,
    KeyCode::End,
];

/// Realistic agent-output chunks: plain text, SGR colour, OSC title, BEL,
/// OSC 9 notification, unicode, screen clears, and alt-screen flips.
const MONKEY_OUTPUT: &[&[u8]] = &[
    b"compiling foo v0.1.0\r\n",
    b"\x1b[31merror\x1b[0m: something\r\n",
    b"\x1b]0;Agent thinking\x07",
    b"\x07",
    b"\x1b]9;needs input\x07",
    "héllo wörld — \u{2714}\r\n".as_bytes(),
    b"\x1b[2J\x1b[H",
    b"\x1b[?1049h",
    b"\x1b[?1049l",
];

/// Monkey test: drive a real `App` with thousands of pseudo-random events —
/// keys, chords, ticks, clock jumps, resizes, mouse, and injected agent
/// output — rendering after every step and asserting [`assert_invariants`].
/// This is the net for "weird TUI behavior": any panic (in update *or* view)
/// or invariant violation fails with the seed + step for exact replay.
/// Tokio flavor: spawn-adjacent flows (fork/restart on the inert backend) may
/// touch the runtime before erroring.
#[tokio::test]
async fn monkey_random_events_uphold_invariants() {
    for seed in [0xDEADBEEFu64, 42, 20260707] {
        let mut rng = Rng(seed);
        let mut h = Harness::standard(3);
        h.render();

        for step in 0..2500 {
            let ctx = format!("seed {seed:#x} step {step}");
            match rng.below(100) {
                // Plain keys: letters, digits, and structural keys.
                0..=39 => {
                    let code = MONKEY_KEYS[rng.below(MONKEY_KEYS.len())];
                    h.key(code, KeyModifiers::NONE);
                }
                // Ctrl chords (thurbox's global namespace).
                40..=59 => {
                    let c = MONKEY_CTRL[rng.below(MONKEY_CTRL.len())];
                    h.ctrl(c);
                }
                // Shift chords (reorder/sort) and F-keys.
                60..=69 => {
                    if rng.below(2) == 0 {
                        let c = ['j', 'k', 's', 'd'][rng.below(4)];
                        h.shift(c);
                    } else {
                        h.func((rng.below(8) + 1) as u8);
                    }
                }
                // Deterministic tick, sometimes after a clock jump.
                70..=79 => {
                    if rng.below(2) == 0 {
                        let ms = [50, 200, 1_000, 5_000, 11_000][rng.below(5)];
                        h.advance(std::time::Duration::from_millis(ms));
                    }
                    h.tick();
                }
                // Mouse: click / scroll / move at a random point.
                80..=89 => {
                    let size = *h.terminal.backend().buffer().area();
                    let x = (rng.below(size.width.max(1) as usize)) as u16;
                    let y = (rng.below(size.height.max(1) as usize)) as u16;
                    let msg = match rng.below(4) {
                        0 => AppMessage::MouseClick {
                            x,
                            y,
                            modifiers: KeyModifiers::NONE,
                        },
                        1 => AppMessage::MouseScrollUp { x, y },
                        2 => AppMessage::MouseScrollDown { x, y },
                        _ => AppMessage::MouseMove { x, y },
                    };
                    h.app.update(msg);
                }
                // Agent output into a random session.
                90..=94 => {
                    if !h.app.sessions.is_empty() {
                        let idx = rng.below(h.app.sessions.len());
                        let chunk = MONKEY_OUTPUT[rng.below(MONKEY_OUTPUT.len())];
                        h.feed_output(idx, chunk);
                        h.app.detect_output_redraw();
                    }
                }
                // Resize, including below the 80/120 layout breakpoints.
                _ => {
                    let cols = (20 + rng.below(160)) as u16;
                    let rows = (8 + rng.below(43)) as u16;
                    h.resize(cols, rows);
                }
            }

            // Render every step: a draw panic (layout overflow, index OOB in a
            // widget) is as much a bug as an update panic.
            h.render();
            assert_invariants(&h.app, &ctx);
        }
    }
}
