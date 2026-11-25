#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct TmuxServer {
    socket: String,
    tmpdir: tempfile::TempDir,
    core_dumps: bool,
}

impl TmuxServer {
    pub fn pin(socket: &str) -> Self {
        let server = Self::private(socket);
        std::env::set_var("TMUX_TMPDIR", server.tmpdir());
        std::env::set_var(runtime_mux::instance::SOCKET_OVERRIDE_ENV, &server.socket);
        // Cleared, not merely overridden: thurbox tags an injected socket with
        // the data dir it belongs to, so a suite run inside a thurbox pane
        // inherits a tag naming the operator's instance. `socket_for` then
        // reads the override above as inherited and derives a socket from this
        // test's own data dir — a server no `kill-server` here names.
        std::env::remove_var(runtime_mux::instance::SOCKET_OWNER_ENV);
        server
    }

    pub fn private(socket: &str) -> Self {
        // An AF_UNIX path is limited to ~104 bytes and tmux spends ten of them
        // on its own `tmux-<uid>/` level, so this sits next to the runtime dir
        // under a short prefix rather than nested under a `TMPDIR` whose length
        // is not ours to choose. macOS has no runtime dir and a `TMPDIR` of
        // about fifty bytes, so there it is `/tmp`.
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(short_temp_dir);
        // Created exclusively, so two guards in one process — or two runs on
        // one machine — can never be handed the same directory and reap each
        // other's server.
        let tmpdir = tempfile::Builder::new()
            .prefix("tbx-")
            .tempdir_in(base)
            .expect("socket dir");
        Self {
            socket: socket.to_string(),
            tmpdir,
            core_dumps: true,
        }
    }

    pub fn crashable(socket: &str) -> Self {
        let mut server = Self::private(socket);
        server.core_dumps = false;
        server
    }

    pub fn socket(&self) -> &str {
        &self.socket
    }

    pub fn tmpdir(&self) -> &Path {
        self.tmpdir.path()
    }

    pub fn scope<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        cmd.env("TMUX_TMPDIR", self.tmpdir());
        cmd.env(runtime_mux::instance::SOCKET_OVERRIDE_ENV, &self.socket);
        cmd.env_remove(runtime_mux::instance::SOCKET_OWNER_ENV)
    }

    pub fn tmux(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new("tmux");
        #[cfg(unix)]
        if !self.core_dumps {
            use std::os::unix::process::CommandExt;
            // SAFETY: `setrlimit` is async-signal-safe, which is all
            // `pre_exec` asks.
            unsafe {
                cmd.pre_exec(|| {
                    let none = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    if libc::setrlimit(libc::RLIMIT_CORE, &none) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        cmd.env("TMUX_TMPDIR", self.tmpdir())
            // A suite run inside a tmux pane inherits `TMUX`, and a command
            // that reads it resolves against the operator's server rather than
            // this one however the socket is spelled.
            .env_remove("TMUX")
            .args(["-L", &self.socket])
            .args(args)
            .output()
            .expect("run tmux")
    }

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
        walk(self.tmpdir(), &mut found);
        found.sort();
        found
    }

    pub fn alive(&self) -> Vec<String> {
        self.sockets()
            .into_iter()
            .filter(|socket| {
                Command::new("tmux")
                    .env("TMUX_TMPDIR", self.tmpdir())
                    .args(["-L", socket, "list-sessions"])
                    .output()
                    .is_ok_and(|out| out.status.success())
            })
            .collect()
    }

    pub fn reap(&self) {
        // The pinned name first, then whatever else turned up: a server whose
        // socket file was already unlinked is unreachable either way, but one
        // that started on a derived name still has its file here.
        for socket in std::iter::once(self.socket.clone()).chain(self.sockets()) {
            let _ = Command::new("tmux")
                .env("TMUX_TMPDIR", self.tmpdir())
                .args(["-L", &socket, "kill-server"])
                .output();
        }
    }
}

pub fn short_temp_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    }
}

pub fn system_path() -> String {
    let mut dirs = vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")];
    let tmux_dir = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .find(|dir| dir.join("tmux").is_file());
    if let Some(dir) = tmux_dir.filter(|dir| !dirs.contains(dir)) {
        dirs.push(dir);
    }
    std::env::join_paths(dirs)
        .expect("PATH entries")
        .into_string()
        .expect("a UTF-8 PATH")
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        // The socket file has to still be there when the kill goes out, or
        // there is nothing left to connect to and the server runs forever.
        // This body is what guarantees that: it runs to completion before
        // `tmpdir` — and with it the socket — is dropped.
        self.reap();
    }
}
