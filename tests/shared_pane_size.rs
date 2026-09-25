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
//! What must hold instead (`TmuxBackend::resize`, `docs/ARCHITECTURE.md`):
//!
//! - an instance painting a different rect does not move a pane another
//!   instance is sizing;
//! - every instance's grid is the pane's real size, so the one that is not
//!   sizing renders the same screen rather than a re-wrapped one;
//! - input is what hands the size over: the instance typed into takes it;
//! - an instance left alone sizes freely again, with no flap on the way.
//!
//! Two `TmuxBackend`s in one process are two control-mode clients, which is
//! exactly what two thurbox instances are to the tmux server. Skipped when tmux
//! is absent.

#![cfg(unix)]

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use thurbox::agent::backend::{ProgramPane, SessionBackend};
use thurbox::agent::tmux::TmuxBackend;

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
    until("B's grid to follow its own claim", || {
        grid_size(&b) == (41, 120)
    })
    .await;

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

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_retake_is_retried_after_the_backend_recovers() {
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

    let a_backend = instance();
    let a = ProgramPane::spawn(
        Arc::clone(&a_backend),
        "tbp-retry-released-size",
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

    let b_backend = instance();
    let b = ProgramPane::adopt(Arc::clone(&b_backend), &id, "sh", 40, 120).expect("adopt in B");
    assert!(b.resize(40, 120));
    b.send_input(b"\n".to_vec()).expect("input to B");
    until("B's claim to reach the pane", || {
        pane_size(&server, &id) == (40, 120)
    })
    .await;
    until("A to hear that B sizes the pane", || a.sized_elsewhere()).await;

    assert!(a.resize(26, 80));
    drop(b);
    b_backend.shutdown();
    drop(b_backend);
    until("A to hear that B released the pane", || {
        !a.sized_elsewhere()
    })
    .await;

    // Lose the control connection for the frame that first sees the release,
    // then restore it. The next frame must retry the retake even though this
    // instance's rect has not changed.
    a_backend.shutdown();
    a.retake_size();
    assert_eq!(pane_size(&server, &id), (40, 120));
    a_backend.ensure_ready().expect("restore A's control mode");
    until("the recovered instance to retry its retake", || {
        a.retake_size();
        pane_size(&server, &id) == (26, 80)
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
