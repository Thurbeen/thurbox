#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use thurbox::backend::WindowLifecycle;

use thurbox::backend::tmux::TmuxBackend;
use thurbox::backend::tmux_compat::server::TMUX_SESSION;
use thurbox::backend::tmux_compat::transport::{SocketDir, TmuxTransport};
use thurbox::backend::SessionBackend;
use thurbox::session::{HostDef, Platform};
use thurbox::shell::HostLauncher;

#[path = "../../runtime-mux/tests/support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|out| out.status.success())
}

#[derive(Default)]
struct ReapByPid(Vec<u32>);

impl Drop for ReapByPid {
    fn drop(&mut self) {
        for pid in &self.0 {
            // SAFETY: plain signals to a pid this test started — `SIGCONT`
            // after, so a server a test stopped gets to act on the `SIGTERM`.
            unsafe {
                libc::kill(*pid as libc::pid_t, libc::SIGTERM);
                libc::kill(*pid as libc::pid_t, libc::SIGCONT);
            }
        }
    }
}

struct KillDefaultServer(String);

impl Drop for KillDefaultServer {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .env_remove("TMUX_TMPDIR")
            .args(["-L", &self.0, "kill-server"])
            .output();
        // A killed server can leave its socket file; the directory is shared.
        let _ = std::fs::remove_file(socket_path(Path::new("/tmp"), &self.0));
    }
}

struct KillServerAt(PathBuf);

impl Drop for KillServerAt {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&self.0)
            .arg("kill-server")
            .output();
    }
}

const DEST: &str = "e2e@sockrec.invalid";

fn stand_in_ssh() -> tempfile::TempDir {
    let bin = tempfile::tempdir().expect("tempdir");
    let ssh = bin.path().join("ssh");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nwhile [ \"$1\" != '{DEST}' ]; do shift; done; shift; exec sh -c \"$*\"\n"
        ),
    )
    .expect("ssh stand-in");
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![bin.path().to_path_buf()];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).expect("PATH"));
    bin
}

fn host_backend(socket: &str, session: &str) -> TmuxBackend {
    let host = HostDef {
        name: "sockrec".into(),
        destination: DEST.into(),
        socket: Some(socket.into()),
        session: Some(session.into()),
        share_sessions: false,
        ..Default::default()
    };
    TmuxBackend::on_host(
        &host,
        HostLauncher::Ssh {
            destination: DEST.into(),
            ssh_opts: Vec::new(),
        },
        Platform::Posix,
        &thurbox::backend::mux_settings(&Default::default()),
    )
}

fn socket_path(dir: &Path, socket: &str) -> PathBuf {
    // SAFETY: `getuid` cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    dir.join(format!("tmux-{uid}")).join(socket)
}

fn servers(socket: &str) -> Vec<u32> {
    let out = Command::new("ps")
        .args(["-axo", "pid=,args="])
        .output()
        .expect("ps");
    let needle = format!("-L {socket} new-session");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.contains(&needle))
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect()
}

fn answering_pid(server: &TmuxServer) -> Option<u32> {
    let out = server.tmux(&["display-message", "-p", "#{pid}"]);
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
        .flatten()
}

fn start_with_window(server: &TmuxServer, session: &str) -> u32 {
    let out = server.tmux(&[
        "new-session",
        "-d",
        "-s",
        session,
        "-n",
        "tb-survivor",
        "sleep",
        "300",
    ]);
    assert!(
        out.status.success(),
        "new-session: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    answering_pid(server).expect("the server answers")
}

fn window_names(backend: &dyn SessionBackend) -> Vec<String> {
    backend
        .discover()
        .expect("discover")
        .into_iter()
        .map(|w| w.name)
        .collect()
}

#[test]
fn a_server_whose_socket_was_removed_is_reattached_not_replaced() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(&format!("tbx-sockrec-local-{}", std::process::id()));
    let mut reap = ReapByPid::default();
    let pid = start_with_window(&server, TMUX_SESSION);
    reap.0.push(pid);

    std::fs::remove_file(socket_path(server.tmpdir(), server.socket())).expect("remove socket");
    assert!(answering_pid(&server).is_none(), "the socket is gone");

    let backend = TmuxBackend::local(&thurbox::backend::mux_settings(&Default::default()));
    assert!(
        window_names(&backend).contains(&"tb-survivor".to_string()),
        "discovery reads the original server's windows"
    );
    backend.ensure_ready().expect("attach");
    assert_eq!(answering_pid(&server), Some(pid), "the original answers");
    assert_eq!(
        servers(server.socket()),
        vec![pid],
        "and no second server runs"
    );
    backend.shutdown();
}

#[test]
fn a_hosts_server_whose_socket_was_removed_is_reattached_not_replaced() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(&format!("tbx-sockrec-ssh-{}", std::process::id()));
    let mut reap = ReapByPid::default();
    let _bin = stand_in_ssh();
    let pid = start_with_window(&server, "tbx-sockrec");
    reap.0.push(pid);
    std::fs::remove_file(socket_path(server.tmpdir(), server.socket())).expect("remove socket");

    let backend = host_backend(server.socket(), "tbx-sockrec");
    assert!(
        window_names(&backend).contains(&"tb-survivor".to_string()),
        "discovery over ssh reads the original server's windows"
    );
    backend.ensure_ready().expect("attach over ssh");
    assert_eq!(answering_pid(&server), Some(pid), "the original answers");
    assert_eq!(
        servers(server.socket()),
        vec![pid],
        "and no second server runs"
    );
    backend.shutdown();
}

#[test]
fn a_server_in_the_old_directory_is_adopted_until_it_exits() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    // The guard's directory stands for tmux's default (`TMUX_TMPDIR`, which
    // an earlier build left alone); `new/` inside it is the one a build
    // choosing its own directory would use.
    let server = TmuxServer::pin(&format!("tbx-sockrec-move-{}", std::process::id()));
    let mut reap = ReapByPid::default();
    let new_dir = server.tmpdir().join("new");
    std::fs::create_dir(&new_dir).expect("mkdir");
    let pid = start_with_window(&server, TMUX_SESSION);
    reap.0.push(pid);

    let backend = || {
        TmuxBackend::with_transport(
            TmuxTransport::local("tmux")
                .with_socket_dir(SocketDir::Dir(new_dir.display().to_string())),
            server.socket(),
            TMUX_SESSION,
            "local:tmux",
            thurbox::session::settings::default_scrollback_lines(),
        )
    };
    let adopted = backend();
    assert!(
        window_names(&adopted).contains(&"tb-survivor".to_string()),
        "the old directory's server is read"
    );
    adopted.ensure_ready().expect("attach");
    assert_eq!(servers(server.socket()), vec![pid], "no second server runs");
    assert!(
        !socket_path(&new_dir, server.socket()).exists(),
        "nothing was started in the new directory"
    );
    adopted.shutdown();

    // The old server exits on its own terms; the next start is a clean one.
    assert!(server.tmux(&["kill-server"]).status.success());
    let started = backend();
    started.ensure_ready().expect("start");
    assert!(
        socket_path(&new_dir, server.socket()).exists(),
        "a clean start puts the socket in the new directory"
    );
    reap.0.extend(servers(server.socket()));
    started.shutdown();
}

#[test]
fn a_socket_path_with_a_space_in_it_is_found_whole() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(&format!("tbx sockrec space-{}", std::process::id()));
    let mut reap = ReapByPid::default();
    let pid = start_with_window(&server, TMUX_SESSION);
    reap.0.push(pid);
    std::fs::remove_file(socket_path(server.tmpdir(), server.socket())).expect("remove socket");

    let backend = TmuxBackend::local(&thurbox::backend::mux_settings(&Default::default()));
    assert!(
        window_names(&backend).contains(&"tb-survivor".to_string()),
        "discovery reads the original server's windows"
    );
    assert_eq!(answering_pid(&server), Some(pid), "the original answers");
    backend.shutdown();
}

#[test]
fn a_server_that_will_not_answer_is_not_replaced() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(&format!("tbx-sockrec-stop-{}", std::process::id()));
    let mut reap = ReapByPid::default();
    let pid = start_with_window(&server, TMUX_SESSION);
    reap.0.push(pid);
    // SAFETY: a plain signal to the server this test started.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGSTOP) };
    std::fs::remove_file(socket_path(server.tmpdir(), server.socket())).expect("remove socket");

    let backend = TmuxBackend::local(&thurbox::backend::mux_settings(&Default::default()));
    let listed = backend.discover().map(|windows| windows.len());
    assert!(
        listed.is_err(),
        "discovery must not read it as empty: {listed:?}"
    );
    let ready = backend.ensure_ready();
    assert!(ready.is_err(), "nothing may start beside it");
    assert_eq!(
        servers(server.socket()),
        vec![pid],
        "and no second server runs"
    );
    backend.shutdown();
}

#[test]
fn a_host_whose_data_dir_is_too_deep_starts_its_server_in_the_default() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(&format!("tbx-sockrec-deep-{}", std::process::id()));
    let socket = format!("tbx-sockrec-deep-{}", std::process::id());
    let _kill = KillDefaultServer(socket.clone());
    let _bin = stand_in_ssh();
    let deep = server.tmpdir().join("d".repeat(120));
    std::env::set_var("XDG_DATA_HOME", &deep);
    std::env::remove_var("TMUX_TMPDIR");

    let backend = host_backend(&socket, "tbx-sockrec");
    backend.ensure_ready().expect("start over ssh");
    assert!(
        socket_path(Path::new("/tmp"), &socket).exists(),
        "the server is in tmux's default directory"
    );
    assert!(!deep.exists(), "nothing was made under the deep data dir");
    assert!(
        backend.discover().is_ok(),
        "and later commands reach it there"
    );
    backend.shutdown();
}

#[test]
fn a_hosts_socket_name_with_a_quote_in_it_starts_in_its_data_dir() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(&format!("tbx-sockrec-quote-{}", std::process::id()));
    let socket = format!("tbx-sockrec-\"q-{}", std::process::id());
    let _bin = stand_in_ssh();
    let data = server.tmpdir().join("xdg");
    std::env::set_var("XDG_DATA_HOME", &data);
    std::env::remove_var("TMUX_TMPDIR");

    let at = socket_path(
        &data.join(thurbox::paths::app_dir_name()).join("tmux"),
        &socket,
    );
    let _kill = KillServerAt(at.clone());

    let backend = host_backend(&socket, "tbx-sockrec");
    backend.ensure_ready().expect("start over ssh");
    assert!(at.exists(), "the server is in the host's data dir");
    assert!(
        backend.discover().is_ok(),
        "and later commands reach it there"
    );
    backend.shutdown();
}
