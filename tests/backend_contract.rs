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
    backend_contract::pane_io(&*fake);
    backend_contract::shutdown_is_final(&*fake);
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
    backend_contract::pane_io(&headless);
    let clients = server.tmux(&["list-clients", "-F", "#{client_name}"]);
    assert_eq!(
        String::from_utf8_lossy(&clients.stdout).trim(),
        "",
        "the headless lifecycle attached a client"
    );
    let quitting = TmuxBackend::new();
    backend_contract::shutdown_is_final(&quitting);
    let clients = server.tmux(&["list-clients", "-F", "#{client_name}"]);
    assert_eq!(
        String::from_utf8_lossy(&clients.stdout).trim(),
        "",
        "a shut-down backend left a client attached"
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

/// A psmux host stamps nothing, so where two windows share a session's name a
/// teardown can go only by the pane the row remembers — and it must find that
/// pane's *window*, whichever of its panes is selected, and never a window of
/// another name the id was reissued to after a restart.
///
/// The host is a stand-in: `ssh` runs its remote command here and `psmux`
/// is this machine's tmux, so the adapter's psmux path runs for real against
/// a private server.
#[cfg(unix)]
#[test]
fn a_psmux_hosts_remembered_pane_finds_its_own_window_and_no_other() {
    use std::os::unix::fs::PermissionsExt;
    use thurbox::backend::{Located, Owner};
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin("thurbox-backend-contract-psmux");
    let bin = tempfile::tempdir().expect("tempdir");
    let script = |name: &str, body: &str| {
        let path = bin.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    };
    const DEST: &str = "e2e@psmux.invalid";
    script(
        "ssh",
        &format!("while [ \"$1\" != '{DEST}' ]; do shift; done; shift; exec sh -c \"$*\""),
    );
    script("psmux", "exec tmux \"$@\"");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![bin.path().to_path_buf()];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).expect("PATH"));

    let pane = |args: &[&str]| {
        let out = server.tmux(args);
        assert!(out.status.success(), "tmux {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let own = pane(&[
        "new-session",
        "-d",
        "-s",
        "probe",
        "-n",
        "tb-x",
        "-P",
        "-F",
        "#{pane_id}",
        "sleep",
        "300",
    ]);
    let namesake = pane(&[
        "new-window",
        "-d",
        "-t",
        "probe",
        "-n",
        "tb-x",
        "-P",
        "-F",
        "#{pane_id}",
        "sleep",
        "300",
    ]);
    let other = pane(&[
        "new-window",
        "-d",
        "-t",
        "probe",
        "-n",
        "tb-other",
        "-P",
        "-F",
        "#{pane_id}",
        "sleep",
        "300",
    ]);
    // Split, and the new pane selected: the listing now reports it, not the
    // pane the row remembers.
    pane(&[
        "split-window",
        "-t",
        &own,
        "-P",
        "-F",
        "#{pane_id}",
        "sleep",
        "300",
    ]);

    let host = thurbox::session::HostDef {
        name: "psmuxhost".into(),
        destination: DEST.into(),
        multiplexer: Some("psmux".into()),
        share_sessions: false,
        socket: Some(server.socket().to_string()),
        session: Some("probe".into()),
        ..Default::default()
    };
    let backend = TmuxBackend::for_route(&host, Multiplexer::Psmux);
    let row = "00000000-0000-4000-8000-0000000000bb";

    // An id reissued to a window of another name is not this row's.
    let reissued = backend
        .locate(Owner::new(row, "x").remembering(&other, ""))
        .expect("locate");
    assert_eq!(reissued.agent, Located::Unknown, "claimed another window");

    // The remembered pane, split away from, is still this row's window.
    let placed = backend
        .locate(Owner::new(row, "x").remembering(&own, ""))
        .expect("locate");
    assert_eq!(placed.agent, Located::At(own.clone()));
    backend.kill(&own).expect("kill");
    let panes = pane(&["list-panes", "-s", "-t", "probe", "-F", "#{pane_id}"]);
    let panes: Vec<&str> = panes.lines().collect();
    assert!(
        !panes.contains(&own.as_str()),
        "the row's window survived: {panes:?}"
    );
    assert!(
        panes.contains(&namesake.as_str()),
        "the namesake was killed"
    );
    assert!(
        panes.contains(&other.as_str()),
        "the other window was killed"
    );
}

/// Input the tmux adapter must refuse or defer, on a real server: a pane whose
/// program exited keeps its frame (`remain-on-exit`) and `send-keys` would
/// exit 0 into it, and a deferred prompt runs on the pane's own server after
/// the caller has gone.
#[test]
fn the_tmux_backend_refuses_an_exited_pane_and_delivers_a_deferred_prompt() {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    use thurbox::backend::{Owner, WindowSpec};

    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin(SOCKET);
    let backend = TmuxBackend::new();
    let env = HashMap::new();
    let open = |id: &'static str, name: &'static str, command: &'static str| {
        backend
            .create_window(&WindowSpec {
                owner: Owner::new(id, name),
                role: WindowRole::Agent,
                command,
                args: &[],
                cwd: None,
                env: &env,
            })
            .expect("create_window")
    };
    let until = |what: &str, done: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "{what} never happened");
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    let exited = open("00000000-0000-4000-8000-0000000000e1", "exits", "true");
    until("the program's exit", &|| {
        backend
            .pane_state(&exited)
            .is_ok_and(|s| s.dead == Some(true))
    });
    assert!(
        backend.send_text(&exited, "into a corpse", true).is_err(),
        "a send into an exited pane reported success"
    );

    let live = open("00000000-0000-4000-8000-0000000000e2", "later", "cat");
    backend
        .send_text_after(&live, "deferred 'quoted' line", Duration::from_secs(1))
        .expect("schedule");
    until("the deferred prompt", &|| {
        backend
            .capture(&live, 20, false)
            .is_ok_and(|screen| screen.contains("deferred 'quoted' line"))
    });
    backend.kill(&exited).expect("kill");
    backend.kill(&live).expect("kill");
}
