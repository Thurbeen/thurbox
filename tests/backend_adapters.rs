//! Each adapter serves its own multiplexer on any
//! machine, rather than one adapter that becomes the other when the binary's
//! name or the build OS says so.

use thurbox::backend::psmux::PsmuxBackend;
use thurbox::backend::rmux::RmuxBackend;
use thurbox::backend::tmux::TmuxBackend;
use thurbox::backend::SessionBackend;
use thurbox::session::{HostDef, Multiplexer, Platform, Route};

#[path = "support/backend_contract.rs"]
mod backend_contract;

#[path = "support/tmux_server.rs"]
mod tmux_server;

fn host(name: &str, platform: Platform, preferred: Option<&str>) -> HostDef {
    HostDef {
        name: name.into(),
        destination: format!("user@{name}"),
        platform: Some(platform),
        multiplexer: preferred.map(str::to_string),
        ..Default::default()
    }
}

/// Each adapter serves the route of its own multiplexer — on this machine
/// whatever OS it is, and on a host whatever the host prefers.
#[test]
fn each_adapter_serves_its_own_multiplexer_wherever_it_runs() {
    assert_eq!(
        TmuxBackend::local().name(),
        Route::local(Some(Multiplexer::Tmux)).format()
    );
    assert_eq!(
        PsmuxBackend::local().name(),
        Route::local(Some(Multiplexer::Psmux)).format()
    );
    assert_eq!(
        RmuxBackend::local().name(),
        Route::local(Some(Multiplexer::Rmux)).format()
    );
    for host in [
        host("linux", Platform::Posix, None),
        host("windows", Platform::Windows, Some("psmux")),
        host("moved", Platform::Posix, Some("rmux")),
    ] {
        assert_eq!(
            TmuxBackend::for_host(&host).name(),
            host.route(Some(Multiplexer::Tmux)).format()
        );
        assert_eq!(
            PsmuxBackend::for_host(&host).name(),
            host.route(Some(Multiplexer::Psmux)).format()
        );
        assert_eq!(
            RmuxBackend::for_host(&host).name(),
            host.route(Some(Multiplexer::Rmux)).format()
        );
    }
}

/// What a multiplexer can report is its adapter's answer: tmux announces a
/// window's close and answers a snapshot in step with the pane's output, psmux
/// does neither (ADR-13) — on any OS, since neither is a fact about one.
#[test]
fn what_each_multiplexer_can_report_is_its_own_adapters_answer() {
    let windows = host("windows", Platform::Windows, None);
    let linux = host("linux", Platform::Posix, Some("psmux"));
    let tmux: [Box<dyn SessionBackend>; 3] = [
        Box::new(TmuxBackend::local()),
        Box::new(TmuxBackend::for_host(&windows)),
        Box::new(TmuxBackend::for_host(&linux)),
    ];
    for backend in &tmux {
        assert!(!backend.needs_liveness_poll(), "{}", backend.name());
        assert!(backend.supports_snapshots(), "{}", backend.name());
    }
    let psmux: [Box<dyn SessionBackend>; 3] = [
        Box::new(PsmuxBackend::local()),
        Box::new(PsmuxBackend::for_host(&windows)),
        Box::new(PsmuxBackend::for_host(&linux)),
    ];
    for backend in &psmux {
        assert!(backend.needs_liveness_poll(), "{}", backend.name());
        assert!(!backend.supports_snapshots(), "{}", backend.name());
    }
}

/// Each adapter runs its own binary: fake multiplexers on
/// `PATH` record which one each adapter asked for its version.
#[cfg(unix)]
#[test]
fn each_adapter_runs_its_own_binary() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("argv");
    for binary in ["tmux", "psmux", "rmux"] {
        let path = dir.path().join(binary);
        let banner = if binary == "rmux" {
            "rmux 0.10.0"
        } else {
            "tmux 3.4"
        };
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"{binary} $*\" >> '{}'\necho '{banner}'\n",
                log.display()
            ),
        )
        .expect("write a fake multiplexer");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    // nextest runs each test in a process of its own.
    std::env::set_var("PATH", dir.path());

    TmuxBackend::local()
        .check_available()
        .expect("tmux answers");
    PsmuxBackend::local()
        .check_available()
        .expect("psmux answers");
    RmuxBackend::local()
        .check_available()
        .expect("rmux answers");

    let ran = std::fs::read_to_string(&log).expect("the fakes ran");
    let binaries: Vec<&str> = ran
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(binaries, ["tmux", "psmux", "rmux"], "argv seen: {ran}");
}

fn have(binary: &str) -> bool {
    std::process::Command::new(binary)
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(unix)]
#[test]
fn reassigned_pane_ids_do_not_claim_duplicate_unstamped_names() {
    use std::os::unix::fs::PermissionsExt;
    use thurbox::backend::contract::{Located, Owner};

    let Some(tmux) = thurbox::paths::resolve_on_path("tmux") else {
        eprintln!("skipping: tmux is not installed");
        return;
    };
    let server = tmux_server::TmuxServer::pin("thurbox-reassigned-pane-test");
    std::env::remove_var("TMUX");
    let create = |command: &str, name: &str| {
        let output = server.tmux(&[
            command,
            "-d",
            "-s",
            "thurbox-dev",
            "-n",
            name,
            "-P",
            "-F",
            "#{pane_id}",
            "cat",
        ]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("pane id")
            .trim()
            .to_string()
    };
    let old_agent = create("new-session", "tb-stale");
    let shell = server.tmux(&[
        "new-window",
        "-d",
        "-t",
        "thurbox-dev",
        "-n",
        "tbs-stale",
        "-P",
        "-F",
        "#{pane_id}",
        "cat",
    ]);
    assert!(shell.status.success());
    let old_shell = String::from_utf8(shell.stdout)
        .expect("shell id")
        .trim()
        .to_string();
    server.reap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let reassigned_agent = create("new-session", "tb-stale");
    let shell = server.tmux(&[
        "new-window",
        "-d",
        "-t",
        "thurbox-dev",
        "-n",
        "tbs-stale",
        "-P",
        "-F",
        "#{pane_id}",
        "cat",
    ]);
    assert!(shell.status.success());
    let reassigned_shell = String::from_utf8(shell.stdout)
        .expect("shell id")
        .trim()
        .to_string();
    assert_eq!(
        reassigned_agent, old_agent,
        "server must reissue the stale agent id"
    );
    assert_eq!(
        reassigned_shell, old_shell,
        "server must reissue the stale shell id"
    );
    for _ in 0..3 {
        for name in ["tb-stale", "tbs-stale"] {
            assert!(server
                .tmux(&["new-window", "-d", "-t", "thurbox-dev", "-n", name, "cat"])
                .status
                .success());
        }
    }

    // Real unstamped windows exercise the psmux adapter's ownership rules;
    // forwarding changes only the executable name, never the listing replies.
    let bin = tempfile::tempdir().expect("shim directory");
    let shim = bin.path().join("psmux");
    std::fs::write(&shim, "#!/bin/sh\nexec \"$IDENTITY_REAL_TMUX\" \"$@\"\n").expect("shim");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::env::set_var("IDENTITY_REAL_TMUX", tmux);
    let path = std::env::var_os("PATH").expect("PATH");
    let paths = std::iter::once(bin.path().to_path_buf()).chain(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(paths).expect("shim PATH"));
    let backend = PsmuxBackend::local();
    let windows = backend.discover().expect("real windows");
    for name in ["tb-stale", "tbs-stale"] {
        assert_eq!(
            windows.iter().filter(|window| window.name == name).count(),
            4
        );
    }
    let located = backend
        .locate(
            Owner::new("11111111-1111-4111-8111-111111111111", "stale")
                .remembering(&old_agent, &old_shell),
        )
        .expect("locate after server restart");
    assert_eq!(
        located.agent,
        Located::Unknown,
        "a reassigned id does not prove agent ownership"
    );
    assert_eq!(
        located.shell,
        Located::Unknown,
        "a reassigned id does not prove shell ownership"
    );
}

#[cfg(unix)]
#[test]
fn local_psmux_namesakes_stay_unknown_with_a_remembered_pane() {
    use std::os::unix::fs::PermissionsExt;
    use thurbox::backend::contract::{Located, Owner};

    let dir = tempfile::tempdir().expect("tempdir");
    let mux = dir.path().join("psmux");
    std::fs::write(
        &mux,
        r#"#!/bin/sh
case "$3" in
list-windows) printf '%s\n' '%1|tb-demo|0||' '%2|tb-demo|0||' ;;
list-panes) printf '%s\n' '%1|tb-demo' '%2|tb-demo' '%3|tb-other' ;;
*) exit 1 ;;
esac
"#,
    )
    .expect("write fake psmux");
    std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::env::set_var("PATH", dir.path());

    let backend = PsmuxBackend::local();
    let owner = Owner::new("11111111-1111-4111-8111-111111111111", "demo");
    assert_eq!(
        backend.locate(owner).expect("listing").agent,
        Located::Unknown
    );
    for (remembered, expected) in [
        ("%1", Located::Unknown),
        ("%2", Located::Unknown),
        ("%3", Located::Unknown),
        ("%9", Located::Unknown),
    ] {
        assert_eq!(
            backend
                .locate(Owner {
                    agent_pane: remembered,
                    ..owner
                })
                .expect("listing")
                .agent,
            expected,
            "remembered {remembered}"
        );
    }
}

#[cfg(windows)]
#[test]
fn native_psmux_namesakes_stay_unknown_with_a_remembered_pane() {
    use std::process::Command;
    use std::time::{Duration, Instant};
    use thurbox::backend::contract::{Located, Owner};

    if !have("psmux") {
        eprintln!("skipping: psmux is not installed");
        return;
    }
    const SESSION: &str = "thurbox-dev";
    let socket = format!("thurbox-identity-{}", uuid::Uuid::new_v4().simple());
    eprintln!("isolated identity socket: {socket}");
    let _pinned = tmux_server::TmuxServer::pin(&socket);
    struct Reap(String);
    impl Drop for Reap {
        fn drop(&mut self) {
            for session in [SESSION, "__warm__"] {
                let _ = Command::new("psmux")
                    .args(["-L", &self.0, "kill-session", "-t", session])
                    .output();
            }
            let _ = Command::new("psmux")
                .args(["-L", &self.0, "kill-server"])
                .output();
        }
    }
    let _reap = Reap(socket.clone());
    let run = |args: &[&str]| {
        Command::new("psmux")
            .args(["-L", &socket])
            .args(args)
            .output()
            .expect("run psmux")
    };
    run(&[
        "new-session",
        "-d",
        "-s",
        SESSION,
        "-n",
        "tb-demo",
        "powershell -NoProfile -Command Start-Sleep 120",
    ]);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !run(&["list-windows", "-t", SESSION]).status.success() {
        assert!(Instant::now() < deadline, "isolated session did not start");
        std::thread::sleep(Duration::from_millis(100));
    }
    for _ in 0..3 {
        let created = run(&[
            "new-window",
            "-t",
            SESSION,
            "-n",
            "tb-demo",
            "powershell -NoProfile -Command Start-Sleep 120",
        ]);
        assert!(created.status.success(), "create duplicate window");
    }

    let backend = PsmuxBackend::local();
    let panes: Vec<_> = backend
        .discover()
        .expect("discover native windows")
        .into_iter()
        .filter(|window| window.name == "tb-demo")
        .map(|window| window.backend_id)
        .collect();
    assert_eq!(panes.len(), 4, "four isolated namesakes must be listed");
    let exact = run(&["capture-pane", "-p", "-t", "thurbox-dev:=tb-demo"]);
    eprintln!(
        "exact-name capture: status {:?}, stdout {:?}, stderr {:?}",
        exact.status.code(),
        String::from_utf8_lossy(&exact.stdout),
        String::from_utf8_lossy(&exact.stderr)
    );
    let owner = Owner::new("11111111-1111-4111-8111-111111111111", "demo");
    assert_eq!(
        backend.locate(owner).expect("locate").agent,
        Located::Unknown
    );
    for pane in &panes {
        assert_eq!(
            backend
                .locate(owner.remembering(pane, ""))
                .expect("locate")
                .agent,
            Located::Unknown
        );
    }
    assert_eq!(
        backend
            .locate(owner.remembering("%999", ""))
            .expect("locate")
            .agent,
        Located::Unknown
    );
    let target = format!("{SESSION}:.{}", panes[1]);
    assert!(run(&["rename-window", "-t", &target, "tb-demo-extra"])
        .status
        .success());
    for pane in &panes[2..] {
        let target = format!("{SESSION}:.{pane}");
        assert!(run(&["rename-window", "-t", &target, "unrelated"])
            .status
            .success());
    }
    assert_eq!(
        backend.locate(owner).expect("unique name").agent,
        Located::At(panes[0].clone())
    );
    assert_eq!(
        backend
            .locate(Owner::new(owner.session_id, "demo-extra"))
            .expect("prefix namesake")
            .agent,
        Located::At(panes[1].clone())
    );
}

#[cfg(unix)]
#[test]
fn psmux_listing_rejects_status_zero_error_output() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let mux = dir.path().join("psmux");
    std::fs::write(
        &mux,
        r#"#!/bin/sh
case "$3" in
capture-pane) printf '%s\n' 'ERROR: agent message' ;;
*) printf '%s\n' "ERROR: can't find window: =tb-demo" ;;
esac
"#,
    )
    .expect("write fake psmux");
    std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::env::set_var("PATH", dir.path());
    assert!(PsmuxBackend::local().discover().is_err());
    assert_eq!(
        PsmuxBackend::local()
            .capture("%1", 20, false)
            .expect("agent output"),
        "ERROR: agent message\n"
    );
}

/// The contract's pane I/O and shutdown, against a real psmux where one is
/// installed. Not the stamp-reading half: psmux keeps `@` options in one
/// server-wide map (ADR-13), so it resolves windows by name and declares that
/// by stamping nothing.
#[test]
fn the_psmux_backend_keeps_the_contract_where_psmux_is_installed() {
    if !have("psmux") {
        eprintln!("skipping: psmux is not installed");
        return;
    }
    // psmux has no socket directory, so the pinned socket's name is all that
    // keeps this off the operator's own server — and a tmux reap does not
    // reach a psmux server, so it is killed by name too.
    const SOCKET: &str = "thurbox-psmux-contract";
    let _pinned = tmux_server::TmuxServer::pin(SOCKET);
    struct Reap;
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = std::process::Command::new("psmux")
                .args(["-L", SOCKET, "kill-server"])
                .output();
        }
    }
    let _reap = Reap;
    let backend = PsmuxBackend::local();
    backend_contract::pane_io(&backend);
    backend_contract::shutdown_is_final(&PsmuxBackend::local());
}

/// A psmux window with no stored pane id is the recovery shape of a row that
/// survived a server or application restart. Resolve it from the live listing:
/// psmux 3.3.8 does not implement tmux's `=name` exact-match target, reports
/// that lookup error on stdout with exit status 0, and permits duplicate window
/// names. A name target can therefore be neither the recovered pane nor proof
/// that capture succeeded.
#[test]
fn psmux_recovery_requires_one_unique_live_pane() {
    use thurbox::backend::{Located, Owner, WindowRole, WindowSpec};

    if !have("psmux") {
        eprintln!("skipping: psmux is not installed");
        return;
    }

    let socket = format!("thurbox-psmux-recovery-{}", std::process::id());
    let _pinned = tmux_server::TmuxServer::pin(&socket);
    struct Reap(String);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = std::process::Command::new("psmux")
                .args(["-L", &self.0, "kill-server"])
                .output();
        }
    }
    let _reap = Reap(socket.clone());
    let backend = PsmuxBackend::local();
    let env = std::collections::HashMap::new();
    let args = [
        "-NoProfile".to_string(),
        "-Command".to_string(),
        "Write-Output psmux-recovery-marker; Start-Sleep -Seconds 300".to_string(),
    ];
    let owner = Owner::new("11111111-1111-4111-8111-111111111111", "recovery");
    backend
        .create_window(&WindowSpec {
            owner,
            role: WindowRole::Agent,
            command: "powershell",
            args: &args,
            cwd: None,
            env: &env,
        })
        .expect("create the recovery window");
    let pane = backend
        .discover()
        .expect("list the recovery window")
        .into_iter()
        .find(|window| window.name == "tb-recovery")
        .expect("the created window is listed")
        .backend_id;

    assert_eq!(
        backend
            .locate(owner)
            .expect("list the recovery window")
            .agent,
        Located::At(pane.clone()),
        "recovery must return the pane psmux actually listed"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let captured = loop {
        let captured = backend.capture(&pane, 20, false).expect("capture the pane");
        if captured.contains("psmux-recovery-marker") || std::time::Instant::now() >= deadline {
            break captured;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(
        captured.contains("psmux-recovery-marker"),
        "the recovered pane must be reachable: {captured:?}"
    );
    let exact = std::process::Command::new("psmux")
        .args([
            "-L",
            &socket,
            "capture-pane",
            "-p",
            "-t",
            "thurbox-dev:=tb-recovery",
        ])
        .output()
        .expect("probe psmux's exact-name target");
    assert!(exact.status.success(), "psmux hides the lookup failure");
    assert!(
        exact.stderr.is_empty()
            && String::from_utf8_lossy(&exact.stdout).contains("can't find window"),
        "the hidden psmux error must be reproduced: stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&exact.stdout),
        String::from_utf8_lossy(&exact.stderr)
    );

    backend
        .create_window(&WindowSpec {
            owner: Owner::new("22222222-2222-4222-8222-222222222222", "recovery"),
            role: WindowRole::Agent,
            command: "powershell",
            args: &args,
            cwd: None,
            env: &env,
        })
        .expect("create the indistinguishable namesake");
    assert_eq!(
        backend.locate(owner).expect("list both namesakes").agent,
        Located::Unknown,
        "duplicate unstamped namesakes must never select one agent"
    );
}
