//! Two thurbox instances on one tmux server must not fight over a pane's size.
//!
//! Each instance matches a pane to the rect it paints it into, and both used to
//! do it unconditionally: whichever painted last — including a toast appearing
//! and taking a row — resized the shared window, the agent re-wrapped, and the
//! other instance went on parsing the agent's output into a grid of its own,
//! different size. Measured with two instances of 100×30 and 160×45 on one
//! server: twelve SIGWINCHes in twelve seconds of alternating rect changes, the
//! agent bouncing between 26×73 and 41×118.
//!
//! What must hold instead (`tmux_compat::Server::resize`, `docs/ARCHITECTURE.md`):
//!
//! - an instance painting a different rect does not move a pane another
//!   instance is sizing;
//! - every instance's grid is the pane's real size, so the one that is not
//!   sizing renders the same screen rather than a re-wrapped one;
//! - input is what hands the size over: the instance typed into takes it;
//! - and so is focus: a pane gaining the focus in an instance takes its size
//!   before anything is typed, while a pane that merely keeps the focus does
//!   not take it back, or two instances focused on it would trade it forever;
//! - an instance left alone sizes freely again, with no flap on the way.
//!
//! Two `TmuxBackend`s in one process are two control-mode clients, which is
//! exactly what two thurbox instances are to the tmux server. Skipped when tmux
//! is absent.

#![cfg(unix)]

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use thurbox::backend::pane::ProgramPane;
use thurbox::backend::tmux::TmuxBackend;
use thurbox::backend::SessionBackend;
use thurbox::kernel::paint::SurfaceProvider;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::terminal::Terminals;
use thurbox::session::SessionState;

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-shared-size-e2e";

/// For a loaded machine starting a server; the notifications themselves arrive
/// within milliseconds.
const DEADLINE: Duration = Duration::from_secs(10);

/// Long enough for a resize that WAS going to land to have landed: a stable
/// size is asserted by watching it not change for this long.
const SETTLE: Duration = Duration::from_millis(400);

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The pane's size as tmux has it, `(rows, cols)`.
fn pane_size(server: &TmuxServer, pane: &str) -> (u16, u16) {
    let out = server.tmux(&[
        "display-message",
        "-p",
        "-t",
        pane,
        "#{pane_height} #{pane_width}",
    ]);
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text.split_whitespace().map(|n| n.parse::<u16>().unwrap());
    (it.next().unwrap(), it.next().unwrap())
}

fn grid_size(pane: &ProgramPane) -> (u16, u16) {
    pane.parser.lock().unwrap().screen().size()
}

/// Wait until `check` holds, and fail with `what` if it never does.
async fn until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Every size the pane takes over `SETTLE`, so a flap that comes and goes
/// between two samples is still seen.
async fn sizes_over_settle(server: &TmuxServer, pane: &str) -> Vec<(u16, u16)> {
    let mut seen = Vec::new();
    let end = Instant::now() + SETTLE;
    while Instant::now() < end {
        let size = pane_size(server, pane);
        if seen.last() != Some(&size) {
            seen.push(size);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    seen
}

fn start(server: &TmuxServer) -> bool {
    let started = server.tmux(&["new-session", "-d", "-s", "thurbox", "-x", "80", "-y", "24"]);
    if !started.status.success() {
        eprintln!(
            "skipping: tmux would not start a server: {}",
            String::from_utf8_lossy(&started.stderr).trim()
        );
    }
    started.status.success()
}

fn instance() -> Arc<dyn SessionBackend> {
    let backend = Arc::new(TmuxBackend::local());
    backend
        .ensure_ready()
        .unwrap_or_else(|e| panic!("tmux control mode would not start: {e:#}"));
    backend
}

/// An agent-like program: long-running, and it wraps at whatever it is told.
fn wrapper_args() -> Vec<String> {
    vec![
        "-c".to_string(),
        "while :; do stty size; sleep 0.2; done".to_string(),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn two_instances_painting_different_rects_leave_the_pane_alone() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());
    if !start(&server) {
        return;
    }

    // Instance A starts the program in its 24×80 rect.
    let a_backend = instance();
    let a = ProgramPane::spawn(
        Arc::clone(&a_backend),
        "tbp-shared-size",
        "sh",
        &wrapper_args(),
        Some(dir.path()),
        &Default::default(),
        24,
        80,
    )
    .expect("spawn in A");
    let id = a.backend_id().to_string();
    until("A's size to reach the pane", || {
        pane_size(&server, &id) == (24, 80)
    })
    .await;

    // Instance B, in a bigger terminal, attaches to the same pane and paints it
    // into a 40×120 rect.
    let b_backend = instance();
    let b = ProgramPane::adopt(Arc::clone(&b_backend), &id, "sh", 40, 120).expect("adopt in B");
    assert!(b.resize(40, 120), "B's resize could not be sent");

    let seen = sizes_over_settle(&server, &id).await;
    assert_eq!(
        seen,
        vec![(24, 80)],
        "B painting its own rect moved the pane A is sizing"
    );
    until("B's grid to be the pane's real size", || {
        grid_size(&b) == (24, 80)
    })
    .await;

    // A's rect changes by a row — a toast coming or going. A is the sizer, so
    // the pane follows A, and B's grid follows the pane.
    assert!(a.resize(25, 80));
    until("A's new rect to reach the pane", || {
        pane_size(&server, &id) == (25, 80)
    })
    .await;
    until("B's grid to follow", || grid_size(&b) == (25, 80)).await;

    // B's rect changes too: still not B's to size.
    assert!(b.resize(41, 120));
    let seen = sizes_over_settle(&server, &id).await;
    assert_eq!(seen, vec![(25, 80)], "B's rect change flapped the pane");

    // Typing into B hands it the size.
    b.send_input(b"\n".to_vec()).expect("input to B");
    until("B's claim to reach the pane", || {
        pane_size(&server, &id) == (41, 120)
    })
    .await;
    until("A's grid to follow the claim", || {
        grid_size(&a) == (41, 120)
    })
    .await;
    assert_eq!(grid_size(&b), (41, 120));

    // And now it is A that cannot move it by painting.
    assert!(a.resize(26, 80));
    let seen = sizes_over_settle(&server, &id).await;
    assert_eq!(seen, vec![(41, 120)], "A took the size back without input");

    // B goes away. A is alone, so the size is A's again: it takes it back on
    // its own — A's rect has not changed, so nothing else would ask — once,
    // with nothing in between. `retake_size` is what the render path calls for
    // every pane it paints, so calling it here is a frame going by.
    // Heard over a format subscription, which tmux re-evaluates once a second.
    until("A to hear that B sizes the pane", || a.sized_elsewhere()).await;
    drop(b);
    b_backend.shutdown();
    drop(b_backend);
    until("the lone instance to take its size back", || {
        a.retake_size();
        pane_size(&server, &id) == (26, 80)
    })
    .await;
    let seen = sizes_over_settle(&server, &id).await;
    assert_eq!(seen, vec![(26, 80)], "the handover flapped");
    until("A's grid to follow its own size", || {
        grid_size(&a) == (26, 80)
    })
    .await;
    assert!(!a.sized_elsewhere());

    // And from here on it resizes freely, as a lone instance always did.
    assert!(a.resize(27, 80));
    until("the lone instance to size the pane", || {
        pane_size(&server, &id) == (27, 80)
    })
    .await;
    a.kill();
}

/// One instance alone is today's behaviour, exactly: every rect it paints is the
/// pane's size, and its grid is that size.
#[tokio::test(flavor = "multi_thread")]
async fn a_lone_instance_still_resizes_to_every_rect() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let server = TmuxServer::pin(SOCKET);
    thurbox::paths::set_test_dir(dir.path());
    if !start(&server) {
        return;
    }

    let backend = instance();
    let pane = ProgramPane::spawn(
        Arc::clone(&backend),
        "tbp-lone-size",
        "sh",
        &wrapper_args(),
        Some(dir.path()),
        &Default::default(),
        24,
        80,
    )
    .expect("spawn");
    let id = pane.backend_id().to_string();
    for (rows, cols) in [(30, 100), (20, 60), (45, 160)] {
        assert!(pane.resize(rows, cols));
        until("the rect to reach the pane", || {
            pane_size(&server, &id) == (rows, cols)
        })
        .await;
        until("the grid to match", || grid_size(&pane) == (rows, cols)).await;
    }
    pane.kill();
}

/// `backend::tmux_compat::server::TMUX_SESSION` in a test build — see
/// `tests/attach_by_name.rs`. A session row is attached by finding its pane
/// there.
const INTERFACE_SESSION: &str = "thurbox-dev";

const ID: &str = "33333333-3333-3333-3333-333333333333";

fn snapshot(pane: &str) -> Snapshot {
    Snapshot {
        sessions: vec![SessionRow {
            id: ID.into(),
            name: "shared".into(),
            agent: "claude".into(),
            status: SessionState::Idle,
            cwd: None,
            repo: None,
            repos: Vec::new(),
            branch: None,
            base_branch: None,
            backend: "local-tmux".into(),
            backend_id: Some(pane.into()),
            remote_host: None,
            agent_session_id: None,
            parent_id: None,
            display_order: None,
            worktree_count: 0,
            git: None,
            stopped: false,
            hook_state: None,
            reports_as: None,
            detected_agent: None,
            shell_backend_id: None,
            member_dirs: Vec::new(),
        }],
        ..Snapshot::default()
    }
}

/// One iteration of the interface's loop with the session in a `rows`×`cols`
/// pane: the paint, with the keys going to the session or not, and then the
/// step after it that is told where they went.
fn frame(terminals: &mut Terminals, (rows, cols): (u16, u16), focused: bool) {
    let input = focused.then_some(ID);
    let mut term = Terminal::new(TestBackend::new(cols, rows)).expect("terminal");
    term.draw(|frame| {
        terminals
            .cursor_on(input)
            .render_session(frame, Rect::new(0, 0, cols, rows), ID, 0);
    })
    .expect("draw");
    terminals.focus(input);
}

/// The interface's grid for the session: the pane's size as it last heard it.
fn session_grid(terminals: &Terminals) -> (u16, u16) {
    terminals
        .search_sources(&[ID.to_string()])
        .into_iter()
        .find(|source| !source.shell)
        .expect("the agent pane is a search source")
        .parser
        .lock()
        .expect("parser")
        .screen()
        .size()
}

/// The server, with the session the interface finds its windows in.
fn start_interface_session(server: &TmuxServer, (rows, cols): (u16, u16)) -> bool {
    let started = server.tmux(&[
        "new-session",
        "-d",
        "-s",
        INTERFACE_SESSION,
        "-x",
        &cols.to_string(),
        "-y",
        &rows.to_string(),
        "-n",
        "idle",
        "sh",
    ]);
    if !started.status.success() {
        eprintln!(
            "skipping: tmux would not start a server: {}",
            String::from_utf8_lossy(&started.stderr).trim()
        );
    }
    started.status.success()
}

/// A window for the session row, named the way the interface looks it up
/// (`tb-<name>`); its pane id.
fn session_window(server: &TmuxServer) -> String {
    let out = server.tmux(&[
        "new-window",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        INTERFACE_SESSION,
        "-n",
        "tb-shared",
        "sh -c 'exec sleep 100000'",
    ]);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Sync the interface to a row naming `pane` until it holds that pane.
async fn attach(terminals: &mut Terminals, pane: &str, (rows, cols): (u16, u16)) {
    let snap = snapshot(pane);
    let deadline = Instant::now() + DEADLINE;
    while terminals.backend_handle(ID).map(|(_, id)| id).as_deref() != Some(pane) {
        assert!(
            Instant::now() < deadline,
            "never attached {pane}: {}",
            terminals.failure(ID).unwrap_or_default()
        );
        terminals.sync(&snap, rows, cols);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Focus hands the size over the way input does, so a session focused after
/// another instance sized it is not left at that size until the first
/// keystroke. Driven through the interface's own terminals — the paint and the
/// focus step the loop runs — against a second instance on the same server.
#[tokio::test(flavor = "multi_thread")]
async fn focusing_a_pane_another_instance_sizes_takes_its_size_before_any_keystroke() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(SOCKET);
    let here_rect = (24, 80);
    let there_rect = (40, 120);
    if !start_interface_session(&server, here_rect) {
        return;
    }
    let id = session_window(&server);

    // This instance shows the session without the focus, and sizes the pane.
    let mut here = Terminals::with_registry(Arc::new(thurbox::backend::wiring::configured().0));
    attach(&mut here, &id, here_rect).await;
    frame(&mut here, here_rect, false);
    until("this instance's rect to reach the pane", || {
        pane_size(&server, &id) == here_rect
    })
    .await;

    // Another instance, in a bigger terminal, is typed into: the pane is its.
    let there_backend = instance();
    let there = ProgramPane::adopt(
        Arc::clone(&there_backend),
        &id,
        "sh",
        there_rect.0,
        there_rect.1,
    )
    .expect("adopt in the other instance");
    assert!(there.resize(there_rect.0, there_rect.1));
    there.send_input(Vec::new()).expect("input there");
    until("the other instance's claim to reach the pane", || {
        pane_size(&server, &id) == there_rect
    })
    .await;
    until("this instance to hear the pane's new size", || {
        frame(&mut here, here_rect, false);
        session_grid(&here) == there_rect
    })
    .await;

    // Painted here without the focus, it stays the other instance's.
    frame(&mut here, here_rect, false);
    assert_eq!(
        sizes_over_settle(&server, &id).await,
        vec![there_rect],
        "an unfocused paint took the size"
    );

    // Focused here, and nothing typed: the pane is this instance's size.
    until("focus to size the pane to this instance's rect", || {
        frame(&mut here, here_rect, true);
        pane_size(&server, &id) == here_rect
    })
    .await;
    until("this instance's grid to follow", || {
        frame(&mut here, here_rect, true);
        session_grid(&here) == here_rect
    })
    .await;

    // The other instance is typed into while this one keeps the focus. Keeping
    // it is not gaining it, so the pane stays where that input put it. Typed
    // once it has heard the pane moved: a claim compares the pane's size as it
    // last heard it, and one that still has its own size sends nothing.
    until("the other instance to hear the pane moved", || {
        grid_size(&there) == here_rect
    })
    .await;
    there.send_input(Vec::new()).expect("input there");
    until("the other instance's claim to reach the pane", || {
        pane_size(&server, &id) == there_rect
    })
    .await;
    until("this instance to hear it", || {
        frame(&mut here, here_rect, true);
        session_grid(&here) == there_rect
    })
    .await;
    frame(&mut here, here_rect, true);
    assert_eq!(
        sizes_over_settle(&server, &id).await,
        vec![there_rect],
        "a pane that kept the focus took the size back"
    );

    // The focus leaves and comes back: that is gaining it again.
    frame(&mut here, here_rect, false);
    frame(&mut here, here_rect, true);
    until("refocusing to size the pane again", || {
        pane_size(&server, &id) == here_rect
    })
    .await;

    drop(there);
    there_backend.shutdown();
}

/// A pane replaced under a session that keeps the focus — a restart — is a pane
/// that never had the focus here, so it takes this instance's size the way a
/// newly focused one does, even though the focused surface's name never moved.
#[tokio::test(flavor = "multi_thread")]
async fn a_focused_session_whose_pane_is_replaced_takes_the_new_panes_size() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(SOCKET);
    let here_rect = (24, 80);
    let there_rect = (40, 120);
    if !start_interface_session(&server, here_rect) {
        return;
    }
    let first = session_window(&server);
    let mut here = Terminals::with_registry(Arc::new(thurbox::backend::wiring::configured().0));
    attach(&mut here, &first, here_rect).await;
    frame(&mut here, here_rect, true);
    until("this instance's rect to reach the pane", || {
        pane_size(&server, &first) == here_rect
    })
    .await;

    // The session restarts: its window goes and a new one takes its place, and
    // another instance sizes the new pane before this one attaches to it.
    // `forget` is the restart telling the interface, as the coordinator does.
    server.tmux(&["kill-window", "-t", &first]);
    here.forget(ID);
    let second = session_window(&server);
    let there_backend = instance();
    let there = ProgramPane::adopt(
        Arc::clone(&there_backend),
        &second,
        "sh",
        there_rect.0,
        there_rect.1,
    )
    .expect("adopt in the other instance");
    assert!(there.resize(there_rect.0, there_rect.1));
    there.send_input(Vec::new()).expect("input there");
    until("the other instance's claim to reach the new pane", || {
        pane_size(&server, &second) == there_rect
    })
    .await;

    // This instance picks up the new pane with the session focused throughout.
    attach(&mut here, &second, here_rect).await;
    until("the new pane to take this instance's size", || {
        frame(&mut here, here_rect, true);
        pane_size(&server, &second) == here_rect
    })
    .await;

    drop(there);
    there_backend.shutdown();
}
