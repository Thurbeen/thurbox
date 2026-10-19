//! The session-backend contract, run against the tmux adapter and against the
//! in-memory fake the routing tests register in its place. A fake that passes
//! only its own tests would prove nothing about routing; one that passes the
//! adapter's is a stand-in for it.

use thurbox::backend::identity::WindowIndex;
use thurbox::backend::rmux::Rmux;
use thurbox::backend::tmux::TmuxBackend;
use thurbox::backend::tmux_compat::server::TmuxCompatible;
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
    backend_contract::status(&*fake);
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
    backend_contract::status(&headless);
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

#[test]
fn the_registered_rmux_backend_keeps_the_contract() {
    if std::process::Command::new("rmux")
        .arg("-V")
        .output()
        .map_or(true, |out| {
            !out.status.success()
                || Rmux::check_banner(&String::from_utf8_lossy(&out.stdout), "test").is_err()
        })
    {
        eprintln!("skipping: RMUX 0.10.0 or newer is not installed");
        return;
    }
    let socket = format!("thurbox-rmux-contract-{}", std::process::id());
    let _server = TmuxServer::pin(&socket);
    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::process::Command::new("rmux")
                .args(["-L", &self.0, "kill-server"])
                .output();
        }
    }
    let _cleanup = Cleanup(socket);
    let registry = thurbox::backend::wiring::local_only();
    let route = Route::local(Some(Multiplexer::Rmux));
    let backend = registry.get(&route).expect("rmux route must be registered");
    backend_contract::suite(backend.as_ref());
    backend_contract::lifecycle(backend.as_ref());
    backend_contract::pane_io(backend.as_ref());
    backend_contract::status(backend.as_ref());
    backend.shutdown();
}

#[cfg(unix)]
fn missing_window_pid<M: TmuxCompatible>() {
    use thurbox::backend::tmux_compat::{server::Server, transport::TmuxTransport};
    use thurbox::backend::{Owner, PaneState, WindowSpec};

    let binary = M::MULTIPLEXER.name();
    if std::process::Command::new(binary)
        .arg("-V")
        .output()
        .map_or(true, |out| {
            !out.status.success()
                || M::check_banner(&String::from_utf8_lossy(&out.stdout), "test").is_err()
        })
    {
        eprintln!("skipping: {binary} is not installed");
        return;
    }
    let socket = format!("thurbox-missing-pid-{binary}-{}", std::process::id());
    let _server = TmuxServer::pin(&socket);
    struct Cleanup<'a>(&'a str, &'a str);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = std::process::Command::new(self.0)
                .args(["-L", self.1, "kill-server"])
                .output();
        }
    }
    let _cleanup = Cleanup(binary, &socket);
    let backend = Server::<M>::with_transport(
        TmuxTransport::local(binary),
        &socket,
        "identity-probe",
        format!("local:{binary}"),
    );
    let pane = backend
        .create_window(&WindowSpec {
            owner: Owner::new("00000000-0000-4000-8000-0000000000ab", "live"),
            role: WindowRole::Agent,
            command: "sleep",
            args: &["300".into()],
            cwd: None,
            env: &Default::default(),
        })
        .expect("create live pane");
    for attached in [false, true] {
        if attached {
            backend.ensure_ready().expect("attach");
        }
        let pid = backend.pane_pid(&pane).expect("live pid");
        assert!(pid.is_some());
        assert_eq!(
            backend
                .pane_pid("identity-probe:=tb-live")
                .expect("live window pid"),
            pid
        );
        let missing = "identity-probe:=tb-missing";
        assert_eq!(
            backend.pane_state(missing).unwrap_or_default(),
            PaneState::default()
        );
        assert_eq!(
            backend.pane_pid(missing).expect("missing window pid"),
            None,
            "{binary}, attached={attached}: a missing window borrowed the live pane's pid"
        );
    }
    backend.shutdown();
}

#[cfg(unix)]
#[test]
fn a_missing_tmux_window_never_borrows_a_live_panes_pid() {
    missing_window_pid::<thurbox::backend::tmux::Tmux>();
}

#[cfg(unix)]
#[test]
fn a_missing_rmux_window_never_borrows_a_live_panes_pid() {
    missing_window_pid::<Rmux>();
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

/// A psmux host stamps nothing, so remembered pane ids cannot prove ownership
/// among duplicate names, even when a split changes the selected pane. A
/// restarted server can reissue an id to another same-name window.
///
/// The host is a stand-in: `ssh` runs its remote command here and `psmux`
/// is this machine's tmux, so the adapter's psmux path runs for real against
/// a private server. Its login shell is this machine's `sh`, so the entry
/// says it is POSIX: a Windows one's commands are quoted for `cmd.exe` and
/// PowerShell, which `sh` cannot run.
#[cfg(unix)]
#[test]
fn a_psmux_hosts_duplicate_names_stay_unknown_even_with_a_remembered_pane() {
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
        platform: Some(thurbox::session::Platform::Posix),
        share_sessions: false,
        socket: Some(server.socket().to_string()),
        session: Some("probe".into()),
        ..Default::default()
    };
    let backend = thurbox::backend::psmux::PsmuxBackend::for_host(&host);
    let row = "00000000-0000-4000-8000-0000000000bb";

    for remembered in [&own, &namesake, &other] {
        let placed = backend
            .locate(Owner::new(row, "x").remembering(remembered, ""))
            .expect("locate");
        assert_eq!(placed.agent, Located::Unknown, "claimed {remembered}");
    }
    let panes = pane(&["list-panes", "-s", "-t", "probe", "-F", "#{pane_id}"]);
    let panes: Vec<&str> = panes.lines().collect();
    for original in [&own, &namesake, &other] {
        assert!(
            panes.contains(&original.as_str()),
            "window removed: {panes:?}"
        );
    }
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

/// Every command written down a control connection is answered, and the reader
/// hands each answer to the next command in line — so a line written with no
/// place of its own (a keystroke) has its answer taken by the command after it.
/// Over ssh that read a window listing as empty: the host held no windows, and
/// every session on it read `Unreachable` while the link was fine.
#[cfg(unix)]
#[test]
fn a_keystroke_never_answers_the_next_command() {
    use std::collections::HashMap;
    use std::io::Write;

    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin(SOCKET);
    let backend = TmuxBackend::new();
    backend.ensure_ready().expect("the backend readies");
    let pane = backend
        .spawn("tb-replies", "cat", &[], None, &HashMap::new(), 24, 80)
        .expect("spawn")
        .backend_id;
    let mut adopted = backend
        .adopt(&pane, 24, 80, Some(Vec::new()))
        .expect("adopt");

    let mut wrong = Vec::new();
    for n in 0..50 {
        adopted.input.write_all(b"x").expect("type");
        adopted.input.flush().expect("flush");
        // A round trip of its own on the control connection, answered with
        // the pane's pid — or, handed the keystroke's empty reply, with none.
        if !matches!(backend.pane_pid(&pane), Ok(Some(_))) {
            wrong.push(n);
        }
    }
    backend.shutdown();
    assert!(
        wrong.is_empty(),
        "{} of 50 commands right after a keystroke were not answered with the pane typed \
         into (attempts {wrong:?})",
        wrong.len()
    );
}

/// A pane the multiplexer reports gone is never adopted: an agent that exits
/// at spawn leaves an id behind, and adopting it would show the session as a
/// quiet live one with nothing in it (#1433).
#[cfg(unix)]
#[test]
fn a_pane_reported_gone_is_never_adopted() {
    use std::collections::HashMap;

    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin(SOCKET);
    let backend = TmuxBackend::new();
    backend.ensure_ready().expect("the backend readies");
    let pane = backend
        .spawn("tb-gone", "cat", &[], None, &HashMap::new(), 24, 80)
        .expect("spawn")
        .backend_id;
    backend.kill(&pane).expect("kill");

    let adopted = backend.adopt(&pane, 24, 80, None);
    backend.shutdown();
    assert!(
        adopted.is_err(),
        "a pane the server reported gone was adopted"
    );
}
