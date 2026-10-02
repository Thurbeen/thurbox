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
