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

/// Initialize a git repo at `dir` with one committed file, leaving an
/// uncommitted edit when `dirty`. Used by the hard-delete tests to give a
/// session a worktree whose state `git::worktree_stats` can read.
fn init_git_repo(dir: &Path, dirty: bool) {
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
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

#[test]
fn repo_picker_browse_view_renders() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    // Stable, name-sorted fixture so the snapshot is deterministic.
    std::fs::create_dir_all(projects.join("alpha").join(".git")).unwrap();
    std::fs::create_dir_all(projects.join("beta")).unwrap();
    let mut h = Harness::snapshot();
    // Seed a bookmark inside `projects` so the browser opens there (hermetic).
    h.app
        .db
        .upsert_repo_bookmark(&projects.join("alpha"))
        .unwrap();
    h.render();
    h.ctrl('n');
    assert_eq!(
        repo_picker(&h).focus,
        modals::RepoPickerFocus::Browse,
        "the picker opens directly in the browser"
    );
    // The left pane title shows an absolute tempdir path; redact it so the
    // snapshot is stable across machines/runs.
    let dir = projects.display().to_string();
    let rendered = h.render().replace(&dir, "<dir>");
    insta::assert_snapshot!(rendered);
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

/// Build a `projects/` dir holding two fake git repos (`.git` subdir),
/// one plain dir, and one hidden dir. Returns `(tmp, projects_path)`; keep
/// `tmp` alive for the tree to exist on disk.
fn browse_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("projects");
    for repo in ["repo_a", "repo_b"] {
        std::fs::create_dir_all(projects.join(repo).join(".git")).unwrap();
    }
    std::fs::create_dir_all(projects.join("plain_dir")).unwrap();
    std::fs::create_dir_all(projects.join(".hidden_dir")).unwrap();
    (tmp, projects)
}

/// Open the two-pane repo picker seeded with one bookmark inside `projects` so
/// the browser's start dir resolves into the fixture (hermetic; avoids `$HOME`).
/// The picker lands directly in the browser at `projects`.
fn open_picker_in_fixture(h: &mut Harness, projects: &Path) {
    h.app
        .db
        .upsert_repo_bookmark(&projects.join("repo_a"))
        .unwrap();
    h.render();
    h.ctrl('n');
}

/// The repo picker's modal state, or panic.
fn repo_picker(h: &Harness) -> &modals::RepoPickerModal {
    match &h.app.modal {
        modals::Modal::RepoPicker(rp) => rp,
        other => panic!("expected repo picker, got {other:?}"),
    }
}

#[test]
fn repo_picker_opens_in_browser() {
    let (_tmp, projects) = browse_fixture();
    let mut h = Harness::standard(0);
    open_picker_in_fixture(&mut h, &projects);

    let rp = repo_picker(&h);
    assert_eq!(rp.focus, modals::RepoPickerFocus::Browse);
    assert_eq!(rp.browse_dir, projects);
    let names: Vec<&str> = rp.browse_entries.iter().map(|e| e.name.as_str()).collect();
    // A `..` row precedes the (sorted, hidden-excluded) children. The seeded
    // bookmark `repo_a` is a child here, so its favorite copy is deduped.
    assert_eq!(names, vec!["..", "plain_dir", "repo_a", "repo_b"]);
    assert_eq!(
        rp.browse_selected().map(|e| e.name.as_str()),
        Some("repo_a"),
        "cursor starts on the most-recent repo (the seeded bookmark)"
    );
    let repo_b = rp
        .browse_entries
        .iter()
        .find(|e| e.name == "repo_b")
        .unwrap();
    assert!(repo_b.is_repo, "git repos flagged");
}

#[test]
fn repo_picker_browse_descend_and_ascend() {
    let (_tmp, projects) = browse_fixture();
    let mut h = Harness::standard(0);
    open_picker_in_fixture(&mut h, &projects);

    // Cursor starts on the most-recent repo (repo_a); open it with `l`
    // (navigate into it — `Enter` would pick+confirm instead).
    assert_eq!(
        repo_picker(&h).browse_selected().map(|e| e.name.as_str()),
        Some("repo_a")
    );
    h.key(KeyCode::Char('l'), KeyModifiers::NONE);
    assert_eq!(repo_picker(&h).browse_dir, projects.join("repo_a"));

    // Ascend with Backspace — cursor restored onto repo_a.
    h.key(KeyCode::Backspace, KeyModifiers::NONE);
    let rp = repo_picker(&h);
    assert_eq!(rp.browse_dir, projects);
    assert_eq!(
        rp.browse_selected().map(|e| e.name.as_str()),
        Some("repo_a"),
        "ascending restores the cursor onto the directory we came from"
    );
}

#[test]
fn repo_picker_add_to_basket_and_submit() {
    let (_tmp, projects) = browse_fixture();
    let mut h = Harness::standard(0);
    open_picker_in_fixture(&mut h, &projects);

    // Cursor starts on repo_a; move to repo_b and add it to the basket with `a`
    // (which adds without confirming, so the basket can hold several).
    h.key(KeyCode::Char('j'), KeyModifiers::NONE); // repo_b
    assert_eq!(
        repo_picker(&h).browse_selected().map(|e| e.name.as_str()),
        Some("repo_b")
    );
    h.key(KeyCode::Char('a'), KeyModifiers::NONE);

    let repo_b = projects.join("repo_b");
    let rp = repo_picker(&h);
    assert_eq!(rp.basket.len(), 1, "repo_b is in the basket");
    assert_eq!(rp.basket[0].path, repo_b);
    assert!(
        !rp.basket[0].worktree,
        "added as a plain (non-worktree) repo"
    );

    let persisted = h.app.db.list_repo_bookmarks().unwrap();
    assert!(
        persisted.iter().any(|b| b.repo_path == repo_b),
        "added repo persisted for recency"
    );

    // Ctrl+Enter (the Done button) confirms from the browser; the wizard
    // advances past the repo step (no worktree → session-name / agent step).
    h.key(KeyCode::Enter, KeyModifiers::CONTROL);
    assert!(
        !matches!(h.app.modal, modals::Modal::RepoPicker(_)),
        "Ctrl+Enter submits the repo picker and advances the wizard"
    );
}

#[test]
fn repo_picker_enter_on_repo_picks_and_confirms() {
    let (_tmp, projects) = browse_fixture();
    let mut h = Harness::standard(0);
    open_picker_in_fixture(&mut h, &projects);

    // Cursor starts on the most-recent repo (repo_a) — a single `Enter` adds it
    // and confirms the picker (the keyboard fast path).
    assert_eq!(
        repo_picker(&h).browse_selected().map(|e| e.name.as_str()),
        Some("repo_a")
    );
    h.key(KeyCode::Enter, KeyModifiers::NONE);

    assert!(
        !matches!(h.app.modal, modals::Modal::RepoPicker(_)),
        "Enter on a repo confirms and advances the wizard"
    );
    let cwd = h
        .app
        .new_session
        .spawn_config
        .as_ref()
        .and_then(|c| c.cwd.clone());
    assert_eq!(
        cwd,
        Some(projects.join("repo_a")),
        "the picked repo became the new session's cwd"
    );
}

#[test]
fn repo_picker_space_adds_and_advances() {
    let (_tmp, projects) = browse_fixture();
    let mut h = Harness::standard(0);
    open_picker_in_fixture(&mut h, &projects);

    // Cursor starts on repo_a; Space adds it and advances to repo_b.
    h.key(KeyCode::Char(' '), KeyModifiers::NONE);
    let rp = repo_picker(&h);
    assert_eq!(rp.basket.len(), 1);
    assert_eq!(rp.basket[0].name, "repo_a");
    assert_eq!(
        rp.browse_selected().map(|e| e.name.as_str()),
        Some("repo_b"),
        "Space advances the cursor for rapid multi-add"
    );
}

#[test]
fn repo_picker_basket_worktree_toggle_and_remove() {
    let (_tmp, projects) = browse_fixture();
    let mut h = Harness::standard(0);
    open_picker_in_fixture(&mut h, &projects);

    // Cursor starts on repo_a — add it, switch to the basket, toggle worktree,
    // then remove it.
    h.key(KeyCode::Char('a'), KeyModifiers::NONE);
    h.key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(repo_picker(&h).focus, modals::RepoPickerFocus::Basket);

    h.key(KeyCode::Char('w'), KeyModifiers::NONE);
    assert!(repo_picker(&h).basket[0].worktree, "w toggles worktree");

    h.key(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(
        repo_picker(&h).basket.is_empty(),
        "x removes from the basket"
    );
}

#[test]
fn repo_picker_adds_non_repo_dir_as_attached() {
    let (_tmp, projects) = browse_fixture();
    let mut h = Harness::standard(0);
    open_picker_in_fixture(&mut h, &projects);

    // Move up onto plain_dir (a non-git directory) and `a` adds it as an
    // attached (`--add-dir`) entry that can't be put in worktree mode.
    h.key(KeyCode::Char('k'), KeyModifiers::NONE); // repo_a -> plain_dir
    assert_eq!(
        repo_picker(&h).browse_selected().map(|e| e.name.as_str()),
        Some("plain_dir")
    );
    h.key(KeyCode::Char('a'), KeyModifiers::NONE);
    let rp = repo_picker(&h);
    assert_eq!(rp.basket.len(), 1, "a plain directory is addable");
    assert_eq!(rp.basket[0].path, projects.join("plain_dir"));
    assert!(!rp.basket[0].is_repo);

    // Worktree mode is rejected for a plain dir.
    h.key(KeyCode::Tab, KeyModifiers::NONE);
    h.key(KeyCode::Char('w'), KeyModifiers::NONE);
    assert!(!repo_picker(&h).basket[0].worktree);
}

#[test]
fn repo_picker_filter_narrows_browser() {
    let (_tmp, projects) = browse_fixture();
    let mut h = Harness::standard(0);
    open_picker_in_fixture(&mut h, &projects);

    // `/` opens the filter; typing "repo_b" narrows to one entry.
    h.key(KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(repo_picker(&h).filter_active);
    for c in "repo_b".chars() {
        h.key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    let rp = repo_picker(&h);
    assert_eq!(rp.browse_filtered.len(), 1);
    assert_eq!(
        rp.browse_selected().map(|e| e.name.as_str()),
        Some("repo_b")
    );
}

#[test]
fn repo_picker_browse_toggle_hidden() {
    let (_tmp, projects) = browse_fixture();
    let mut h = Harness::standard(0);
    open_picker_in_fixture(&mut h, &projects);
    // `..` + plain_dir + repo_a + repo_b (hidden excluded).
    assert_eq!(repo_picker(&h).browse_entries.len(), 4);

    h.key(KeyCode::Char('.'), KeyModifiers::NONE);
    let rp = repo_picker(&h);
    assert!(rp.show_hidden);
    assert!(
        rp.browse_entries.iter().any(|e| e.name == ".hidden_dir"),
        "toggling hidden reveals dotfiles"
    );
}

#[test]
fn repo_picker_favorites_pinned_and_addable() {
    // A bookmark outside the browsed directory shows as a pinned ★ favorite.
    let (_tmp, projects) = browse_fixture();
    let other = _tmp.path().join("elsewhere");
    std::fs::create_dir_all(other.join("widget").join(".git")).unwrap();
    let widget = other.join("widget");

    let mut h = Harness::standard(0);
    h.app.db.upsert_repo_bookmark(&widget).unwrap();
    h.render();
    h.ctrl('n');

    // Navigate to `projects` (which doesn't contain `widget`) via the go-to
    // input, so the favorite stays pinned rather than deduped as a child.
    h.key(KeyCode::Char('g'), KeyModifiers::NONE);
    for c in projects.to_str().unwrap().chars() {
        h.key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    h.key(KeyCode::Enter, KeyModifiers::NONE);

    let rp = repo_picker(&h);
    assert_eq!(rp.browse_dir, projects);
    let fav = &rp.browse_entries[0];
    assert_eq!(fav.kind, modals::BrowseKind::Favorite);
    assert_eq!(fav.name, "widget");

    // Cursor starts on the first child (plain_dir); move up past `..` onto the
    // pinned favorite and add it with `a` without leaving the directory.
    h.key(KeyCode::Char('k'), KeyModifiers::NONE); // onto `..`
    h.key(KeyCode::Char('k'), KeyModifiers::NONE); // onto the favorite
    assert_eq!(
        repo_picker(&h).browse_selected().map(|e| e.name.as_str()),
        Some("widget")
    );
    h.key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(repo_picker(&h).basket[0].path, widget);
    assert_eq!(
        repo_picker(&h).browse_dir,
        projects,
        "stayed in the directory"
    );
}

#[test]
fn repo_picker_forget_favorite_deletes_bookmark() {
    let (_tmp, projects) = browse_fixture();
    let other = _tmp.path().join("elsewhere");
    std::fs::create_dir_all(other.join("widget").join(".git")).unwrap();
    let widget = other.join("widget");

    let mut h = Harness::standard(0);
    h.app.db.upsert_repo_bookmark(&widget).unwrap();
    h.render();
    h.ctrl('n');
    // Go to `projects` so `widget` shows as a pinned favorite (not deduped).
    h.key(KeyCode::Char('g'), KeyModifiers::NONE);
    for c in projects.to_str().unwrap().chars() {
        h.key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    h.key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        repo_picker(&h).browse_entries[0].kind,
        modals::BrowseKind::Favorite
    );

    // Move onto the favorite and forget it with `d`.
    h.key(KeyCode::Char('k'), KeyModifiers::NONE); // onto `..`
    h.key(KeyCode::Char('k'), KeyModifiers::NONE); // onto the favorite
    h.key(KeyCode::Char('d'), KeyModifiers::NONE);

    let rp = repo_picker(&h);
    assert!(rp.favorites.is_empty(), "the favorite row is dropped");
    assert!(
        !rp.browse_entries
            .iter()
            .any(|e| e.kind == modals::BrowseKind::Favorite),
        "no favorite rows remain"
    );
    assert!(
        h.app.db.list_repo_bookmarks().unwrap().is_empty(),
        "the persisted bookmark is deleted"
    );
}

#[test]
fn repo_picker_remote_typed_path_adds_to_basket() {
    let mut h = Harness::standard(0);
    h.app.new_session.backend = Some("ssh:devbox".to_string());
    h.app.open_repo_picker();
    assert_eq!(repo_picker(&h).focus, modals::RepoPickerFocus::PathInput);

    // Type a remote path and commit it — it lands in the basket as a repo
    // (worktree-able) and focus moves to the basket.
    for c in "/srv/app".chars() {
        h.key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    h.key(KeyCode::Enter, KeyModifiers::NONE);

    let rp = repo_picker(&h);
    assert_eq!(rp.basket.len(), 1);
    assert_eq!(rp.basket[0].path, std::path::PathBuf::from("/srv/app"));
    assert!(rp.basket[0].is_repo);
    assert_eq!(rp.focus, modals::RepoPickerFocus::Basket);
}

#[test]
fn repo_picker_tab_cycles_through_all_panes() {
    let (_tmp, projects) = browse_fixture();
    let mut h = Harness::standard(0);
    open_picker_in_fixture(&mut h, &projects);
    assert_eq!(repo_picker(&h).focus, modals::RepoPickerFocus::Browse);

    // Tab cycles Browse → Basket → Go-to-path → Browse, so the path input is
    // reachable without the `g` shortcut.
    h.key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(repo_picker(&h).focus, modals::RepoPickerFocus::Basket);
    h.key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(repo_picker(&h).focus, modals::RepoPickerFocus::PathInput);
    h.key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(repo_picker(&h).focus, modals::RepoPickerFocus::Browse);

    // Shift+Tab cycles the other way.
    h.key(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(repo_picker(&h).focus, modals::RepoPickerFocus::PathInput);
}

#[test]
fn repo_picker_remote_opens_path_input() {
    let mut h = Harness::standard(0);
    h.app.new_session.backend = Some("ssh:devbox".to_string());
    h.app.open_repo_picker();
    // A remote target has no local filesystem to browse — it opens straight
    // into the path text-input with no browser entries.
    let rp = repo_picker(&h);
    assert_eq!(rp.focus, modals::RepoPickerFocus::PathInput);
    assert!(rp.browse_entries.is_empty());
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

// ── Performance counters: deterministic render-path proxies ───────────────────
//
// These assert on `App::perf_counters()` — wall-clock-free counts — so they
// gate the redraw-throttling and per-frame caching optimizations without timing
// flakiness. The acceptance harness drives `view()` directly (it skips
// `tick()`), so only the render-path counters are exercised here; the
// tick-driven counters (`status_refreshes`) and the redraw-skip accounting live
// in the `#[tokio::test]` units in `super::tests`.

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
            target: crate::app::code_review::ReviewTarget::Working,
            commits: Vec::new(),
            host: None,
            target_picker: None,
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
