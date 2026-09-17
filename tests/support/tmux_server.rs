//! A tmux server a harness cannot forget to reap.
//!
//! Every e2e harness here needs the same three things — a pinned socket name,
//! the inherited owner tag cleared, and a `TMUX_TMPDIR` of its own — and then
//! needs the server killed again afterwards. Spelling the teardown as a
//! `cleanup()` called at each exit point made correctness rest on *counting*:
//! `tests/spawn_command_resolution.rs` had eleven of those calls and
//! `tests/create_e2e.rs` more, and a panic between the first `tmux` call and
//! the last one reached none of them. A nextest timeout reached none of them
//! either. The servers that escaped that way were unreachable rather than
//! merely untidy — the socket file lives in the harness's tempdir, so it went
//! away with the test and left a server nothing could connect to, agent
//! processes and all, until the machine was rebooted (issue #1175).
//!
//! [`TmuxServer`] makes the reap structural instead: hold one for as long as
//! the server should live and its `Drop` kills it, whether the test returned,
//! asserted or panicked.
//!
//! Two things it owns deliberately:
//!
//! - **The socket directory.** It is the guard's, not the harness's, so the
//!   kill cannot race the directory's removal: `Drop::drop` runs to completion
//!   before any field of the struct is dropped, so the server is already dead
//!   by the time the directory goes. A harness that kept `TMUX_TMPDIR` in its
//!   own tempdir was one field reordering away from killing a server whose
//!   socket had just been deleted.
//! - **Every socket in that directory**, not just the pinned name. A run that
//!   landed on a name it did not choose — the failure
//!   `tests/tmux_server_leak.rs` exists to catch — started a server too, and
//!   reaping only the pinned name would leak exactly the thing under test.
//!
//! What it cannot cover is `SIGKILL`, which runs no destructor:
//! `scripts/dev/reap-tmux-servers.sh` (`just reap-tmux`) is the sweep for
//! that, and `docs/DEVELOPMENT.md` says when to run it.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A pinned tmux socket, its private socket directory, and the promise that
/// both are gone when this value is.
pub struct TmuxServer {
    socket: String,
    /// `TMUX_TMPDIR`. A plain `PathBuf` rather than a `tempfile::TempDir`
    /// because the removal has to happen *after* the kill, which is what
    /// [`Drop::drop`] spells out.
    tmpdir: PathBuf,
}

impl TmuxServer {
    /// A server scoped to this process: `TMUX_TMPDIR`, the socket override and
    /// the cleared owner tag are all set process-wide.
    ///
    /// That is safe under nextest, which gives every test a process of its own
    /// — which is also why a harness holding two of these at once would have
    /// them fight over the same three variables. Hold one.
    pub fn pin(socket: &str) -> Self {
        let server = Self::private(socket);
        std::env::set_var("TMUX_TMPDIR", &server.tmpdir);
        std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, &server.socket);
        // Cleared, not merely overridden: thurbox tags an injected socket with
        // the data dir it belongs to, so a suite run inside a thurbox pane
        // inherits a tag naming the operator's instance. `socket_for` then
        // reads the override above as inherited and derives a socket from this
        // test's own data dir — a server no `kill-server` here names.
        std::env::remove_var(thurbox::agent::tmux::SOCKET_OWNER_ENV);
        server
    }

    /// The same server, leaving this process's environment alone — for a
    /// harness that scopes each child command with [`Self::scope`] instead.
    pub fn private(socket: &str) -> Self {
        // Per guard, not per process: a harness that builds two must not have
        // the second take the first's socket directory out from under it.
        static NTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // An AF_UNIX path is limited to ~104 bytes and tmux spends ten of them
        // on its own `tmux-<uid>/` level, so this is a short directory next to
        // the runtime dir rather than one nested under a tempdir whose prefix
        // is not ours to keep short.
        let tmpdir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("tbx-{}-{nth}", std::process::id()));
        std::fs::create_dir_all(&tmpdir).expect("mkdir socket dir");
        Self {
            socket: socket.to_string(),
            tmpdir,
        }
    }

    /// The pinned socket name.
    pub fn socket(&self) -> &str {
        &self.socket
    }

    /// The private `TMUX_TMPDIR` this server's sockets live in.
    pub fn tmpdir(&self) -> &Path {
        &self.tmpdir
    }

    /// Point `cmd` at this server the way a harness must: pinned socket,
    /// cleared owner tag, private socket directory.
    pub fn scope<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        cmd.env("TMUX_TMPDIR", &self.tmpdir);
        cmd.env(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, &self.socket);
        cmd.env_remove(thurbox::agent::tmux::SOCKET_OWNER_ENV)
    }

    /// `tmux <args>` on this server.
    pub fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .env("TMUX_TMPDIR", &self.tmpdir)
            // A suite run inside a tmux pane inherits `TMUX`, and a command
            // that reads it resolves against the operator's server rather than
            // this one however the socket is spelled.
            .env_remove("TMUX")
            .args(["-L", &self.socket])
            .args(args)
            .output()
            .expect("run tmux")
    }

    /// Every socket *file* under this server's directory. tmux nests them one
    /// level down (`tmux-<uid>/<name>`), so this walks rather than lists.
    ///
    /// A file, not a server: tmux never unlinks a socket, so one outlives the
    /// server that made it. [`Self::alive`] is the liveness question.
    pub fn sockets(&self) -> Vec<String> {
        fn walk(dir: &Path, found: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                } else {
                    found.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.tmpdir, &mut found);
        found.sort();
        found
    }

    /// The sockets here a tmux server still answers on.
    pub fn alive(&self) -> Vec<String> {
        self.sockets()
            .into_iter()
            .filter(|socket| {
                Command::new("tmux")
                    .env("TMUX_TMPDIR", &self.tmpdir)
                    .args(["-L", socket, "list-sessions"])
                    .output()
                    .is_ok_and(|out| out.status.success())
            })
            .collect()
    }

    /// Kill every server that answers in this directory. Idempotent, and the
    /// same thing [`Drop`] does — a test that wants the server gone at a
    /// particular moment can say so without giving up the guard.
    pub fn reap(&self) {
        // The pinned name first, then whatever else turned up: a server whose
        // socket file was already unlinked is unreachable either way, but one
        // that started on a derived name still has its file here.
        for socket in std::iter::once(self.socket.clone()).chain(self.sockets()) {
            let _ = Command::new("tmux")
                .env("TMUX_TMPDIR", &self.tmpdir)
                .args(["-L", &socket, "kill-server"])
                .output();
        }
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        // Order, spelled out rather than left to field declaration order: the
        // socket file has to still be there when the kill goes out, or there
        // is nothing left to connect to and the server runs forever.
        self.reap();
        let _ = std::fs::remove_dir_all(&self.tmpdir);
    }
}
