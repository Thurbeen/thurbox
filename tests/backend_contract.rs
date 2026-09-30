//! The session-backend contract, run against the tmux adapter and against the
//! in-memory fake the routing tests register in its place. A fake that passes
//! only its own tests would prove nothing about routing; one that passes the
//! adapter's is a stand-in for it.

use thurbox::backend::identity::WindowIndex;
use thurbox::backend::tmux::TmuxBackend;
use thurbox::backend::{BackendLiveness, SessionBackend, WindowRole};
use thurbox::session::{Multiplexer, Route};

#[path = "support/tmux_server.rs"]
mod tmux_server;

#[path = "support/recording_backend.rs"]
mod recording_backend;

#[path = "support/backend_contract.rs"]
mod backend_contract;

use recording_backend::RecordingBackend;
use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-backend-contract";

fn have_tmux() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn the_recording_backend_keeps_the_contract() {
    let fake = RecordingBackend::new(&Route::local(Some(Multiplexer::Rmux)));
    backend_contract::suite(&*fake);
    backend_contract::lifecycle(&*fake);
}

#[test]
fn the_tmux_backend_keeps_the_contract() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(SOCKET);
    let backend = TmuxBackend::new();
    backend_contract::suite(&backend);
    backend.shutdown();

    // Headless, on a backend nothing attached to: a teardown or a restart
    // from `thurbox-cli` opens no control client on the server it acts on.
    let headless = TmuxBackend::new();
    backend_contract::lifecycle(&headless);
    let clients = server.tmux(&["list-clients", "-F", "#{client_name}"]);
    assert_eq!(
        String::from_utf8_lossy(&clients.stdout).trim(),
        "",
        "the headless lifecycle attached a client"
    );
}

/// An unreachable machine answers nothing, and a fake that answered "empty"
/// instead would make every teardown through it look finished.
#[test]
fn an_unreachable_fake_answers_nothing_rather_than_nothing_there() {
    let fake = RecordingBackend::new(&Route::local(Some(Multiplexer::Rmux)));
    let pane = fake.open("tb-far", "row", WindowRole::Agent);
    fake.set_reachable(false);
    assert!(fake.discover().is_err(), "a listing that did not happen");
    assert!(fake.kill(&pane).is_err(), "a kill that did not happen");
    assert!(fake.ensure_ready().is_err());
    fake.set_reachable(true);
    assert_eq!(
        fake.windows().len(),
        1,
        "nothing was killed while unreachable"
    );
}

/// Two unstamped windows of one name are ambiguous, and ambiguity never
/// authorises a relaunch — the fake lists them the way a multiplexer would.
#[test]
fn an_ambiguous_fake_listing_never_permits_a_relaunch() {
    let fake = RecordingBackend::new(&Route::local(Some(Multiplexer::Rmux)));
    fake.open("tb-twin", "", WindowRole::Agent);
    fake.open("tb-twin", "", WindowRole::Agent);
    let index = WindowIndex::from_listing(fake.discover().unwrap());
    let liveness = index.agent_liveness("some-row", "twin");
    assert_eq!(liveness, BackendLiveness::Unknown);
    assert!(!liveness.permits_relaunch());
}

/// One session, one window per role: a second stamp for the same row retires
/// the older window, as the tmux adapter's sweep does (ADR-25).
#[test]
fn a_stamp_two_windows_carry_is_kept_by_the_newer() {
    let fake = RecordingBackend::new(&Route::local(Some(Multiplexer::Rmux)));
    let old = fake.open("tb-x", "", WindowRole::Agent);
    let new = fake.open("tb-x", "", WindowRole::Agent);
    fake.stamp_window(&old, "row", WindowRole::Agent).unwrap();
    fake.stamp_window(&new, "row", WindowRole::Agent).unwrap();
    let panes: Vec<String> = fake.windows_of("row").into_iter().map(|w| w.pane).collect();
    assert_eq!(panes, vec![new]);
}

/// An attached backend whose server stops answering reports that, rather than
/// an empty server: a sweep reading "nothing there" clears its backoff and
/// asks again every pass, and a relaunch reading it launches a second agent.
#[cfg(unix)]
#[test]
fn an_attached_tmux_backend_does_not_read_an_unanswered_listing_as_empty() {
    use std::os::unix::fs::PermissionsExt;
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin("thurbox-backend-contract-unanswered");
    let backend = TmuxBackend::new();
    backend.ensure_ready().expect("attach");
    let socket = server
        .tmpdir()
        .join(format!("tmux-{}", uid()))
        .join(server.socket());
    assert!(socket.exists(), "no socket at {}", socket.display());
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let listing = backend.discover();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    backend.shutdown();
    assert!(
        listing.is_err(),
        "a server nobody could ask was read as holding nothing: {listing:?}",
        listing = listing.map(|l| l.len())
    );
}

#[cfg(unix)]
fn uid() -> String {
    String::from_utf8(
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .expect("id -u")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_string()
}

/// A kill takes the window the pane is in, not only the pane: a session
/// window someone split must not keep running a process in its other pane
/// after a stop, restart or force delete — attached or not.
#[test]
fn killing_a_split_window_takes_the_whole_window() {
    use thurbox::backend::{Owner, WindowSpec};
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin("thurbox-backend-contract-split");
    let env = std::collections::HashMap::new();
    let args = ["300".to_string()];
    for attached in [false, true] {
        let backend = TmuxBackend::new();
        let name = format!("split-{attached}");
        let pane = backend
            .create_window(&WindowSpec {
                owner: Owner::new("00000000-0000-4000-8000-0000000000aa", &name),
                role: WindowRole::Agent,
                command: "sleep",
                args: &args,
                cwd: None,
                env: &env,
            })
            .expect("create_window");
        let split = server.tmux(&["split-window", "-d", "-t", &pane, "sleep", "300"]);
        assert!(split.status.success(), "split-window failed");
        if attached {
            backend.ensure_ready().expect("attach");
        }
        backend.kill(&pane).expect("kill");
        let windows = server.tmux(&["list-windows", "-a", "-F", "#{window_name}"]);
        let windows = String::from_utf8_lossy(&windows.stdout);
        assert!(
            !windows.lines().any(|w| w == format!("tb-{name}")),
            "attached={attached}: the split window survived its kill: {windows}"
        );
        backend.shutdown();
    }
}
