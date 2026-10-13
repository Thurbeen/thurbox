//! A tmux server whose socket is gone is found again, never replaced.
//!
//! tmux's default socket directory is `/tmp`, which macOS sweeps; a socket's
//! access time is not bumped by `connect()`, so a server in daily use loses its
//! socket after a few days. The server keeps running, unreachable, and a
//! `new-session` on the same name used to start a second one beside it — one
//! Mac held 93 processes and 3.1 GB that way, 13 of its agents duplicates of
//! sessions resumed into the new server. Each test here starts a *real* server
//! (skipped when tmux is absent), takes its socket away or puts it where an
//! earlier build would have, and asserts that thurbox reaches the original:
//! its windows are there, and no second server process exists.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use thurbox::backend::tmux::TmuxBackend;
use thurbox::backend::tmux_compat::server::TMUX_SESSION;
use thurbox::backend::tmux_compat::transport::{SocketDir, TmuxTransport};
use thurbox::backend::SessionBackend;
use thurbox::session::{HostDef, Platform};
use thurbox::shell::HostLauncher;

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Kills the servers it is handed when dropped. [`TmuxServer`] reaps by
/// connecting, which is exactly what a server with no socket refuses — so a
/// test that takes a socket away holds one of these too, or a failure in it
/// would leak the very orphan it is about.
#[derive(Default)]
struct ReapByPid(Vec<u32>);

impl Drop for ReapByPid {
    fn drop(&mut self) {
        for pid in &self.0 {
            // SAFETY: a plain signal to a pid this test started.
            unsafe { libc::kill(*pid as libc::pid_t, libc::SIGTERM) };
        }
    }
}

/// `<dir>/tmux-<uid>/<socket>`, where tmux puts a socket for `dir`.
fn socket_path(dir: &Path, socket: &str) -> PathBuf {
    // SAFETY: `getuid` cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    dir.join(format!("tmux-{uid}")).join(socket)
}

/// Every running tmux server process for `socket`: a process whose command
/// line started one (`new-session`) on that name. A server keeps the argv of
/// the client that started it, which is what makes a socketless one findable.
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

/// The pid of the server answering on `server`'s socket.
fn answering_pid(server: &TmuxServer) -> Option<u32> {
    let out = server.tmux(&["display-message", "-p", "#{pid}"]);
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
        .flatten()
}

/// Start `server` with one window, `tb-survivor`, in `session`, and return
/// the server's pid.
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

/// The incident: the socket goes from under a running server, and the next
/// thurbox finds its windows rather than starting a server of its own.
#[test]
fn a_server_whose_socket_was_removed_is_reattached_not_replaced() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin("tbx-sockrec-local");
    let mut reap = ReapByPid::default();
    let pid = start_with_window(&server, TMUX_SESSION);
    reap.0.push(pid);

    std::fs::remove_file(socket_path(server.tmpdir(), server.socket())).expect("remove socket");
    assert!(answering_pid(&server).is_none(), "the socket is gone");

    let backend = TmuxBackend::local();
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

/// The same over the ssh transport: the host is a stand-in whose `ssh` runs
/// the remote command here, so the lookup and the `SIGUSR1` run through the
/// host's shell exactly as they would on one.
#[test]
fn a_hosts_server_whose_socket_was_removed_is_reattached_not_replaced() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin("tbx-sockrec-ssh");
    let mut reap = ReapByPid::default();
    let bin = tempfile::tempdir().expect("tempdir");
    const DEST: &str = "e2e@sockrec.invalid";
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

    let host = HostDef {
        name: "sockrec".into(),
        destination: DEST.into(),
        socket: Some(server.socket().into()),
        session: Some("tbx-sockrec".into()),
        share_sessions: false,
        ..Default::default()
    };
    let pid = start_with_window(&server, "tbx-sockrec");
    reap.0.push(pid);
    std::fs::remove_file(socket_path(server.tmpdir(), server.socket())).expect("remove socket");

    let backend = TmuxBackend::on_host(
        &host,
        HostLauncher::Ssh {
            destination: DEST.into(),
            ssh_opts: Vec::new(),
        },
        Platform::Posix,
    );
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

/// A thurbox that keeps its socket somewhere new still finds a server an
/// earlier build started in tmux's default directory, and adopts it rather
/// than starting a second; once that server is gone, the next one starts in
/// the new directory.
#[test]
fn a_server_in_the_old_directory_is_adopted_until_it_exits() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    // The guard's directory stands for tmux's default (`TMUX_TMPDIR`, which
    // an earlier build left alone); `new/` inside it is the one a build
    // choosing its own directory would use.
    let server = TmuxServer::pin("tbx-sockrec-move");
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
