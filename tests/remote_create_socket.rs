//! `session create --json` on a host reports the socket the session lives on.
//!
//! The document's `tmux_socket` is what a caller hands `tmux -L` to reach the
//! pane it was just given, so for a remote session it is the host's socket
//! (ADR-12): the host's `socket` in `hosts.toml`, else what its own CLI
//! reported. This instance's own socket names a server on another machine.
//!
//! The host is this machine behind a stand-in `ssh` that runs the command it
//! is handed, so the create really happens and the pane really exists — on a
//! socket the test pinned apart from the local one. POSIX-only for the stand-in.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const LOCAL_SOCKET: &str = "tbx-local-create";
const HOST_SOCKET: &str = "tbx-host-create";

const AGENTS_TOML: &str =
    "default = \"shell\"\n\n[[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = []\n";

/// Drops the ssh options and the destination, then runs the rest here — the
/// words joined and re-split by a shell, which is what a host's login shell
/// does with them.
const SSH_STAND_IN: &str = "#!/bin/sh\n\
     while [ \"$#\" -gt 0 ]; do\n\
     \x20 case \"$1\" in\n\
     \x20   -o) shift; shift ;;\n\
     \x20   -*) shift ;;\n\
     \x20   *) break ;;\n\
     \x20 esac\n\
     done\n\
     [ \"$#\" -gt 0 ] && shift\n\
     [ \"$#\" -eq 0 ] && exit 0\n\
     eval \"exec $*\"\n";

#[test]
fn a_remote_create_reports_the_hosts_socket() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let path = |sub: &str| -> PathBuf { root.path().join(sub) };
    for sub in ["home", "config", "data", "bin", "repo"] {
        std::fs::create_dir_all(path(sub)).expect("mkdir");
    }
    std::fs::write(path("config/agents.toml"), AGENTS_TOML).expect("agents.toml");
    std::fs::write(
        path("config/hosts.toml"),
        format!(
            "[[hosts]]\nname = \"box\"\ndestination = \"e2e@box.invalid\"\n\
             socket = \"{HOST_SOCKET}\"\nshare_sessions = false\n"
        ),
    )
    .expect("hosts.toml");
    let ssh = path("bin/ssh");
    std::fs::write(&ssh, SSH_STAND_IN).expect("ssh stand-in");
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).expect("chmod");

    // Both servers start in this guard's directory, so it reaps the host's as
    // well as the local one.
    let server = TmuxServer::private(LOCAL_SOCKET);
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let search_path =
        std::env::join_paths(std::iter::once(path("bin")).chain(std::env::split_paths(&inherited)))
            .expect("PATH");
    let create = |extra: &[&str]| -> Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args([
            "--json", "session", "create", "--name", "afar", "--host", "box",
        ])
        .args(extra)
        .arg("--repo-path")
        .arg(path("repo"))
        .env("PATH", &search_path)
        .env("HOME", path("home"))
        .env("XDG_DATA_HOME", path("home/xdg-data"))
        .env("XDG_CONFIG_HOME", path("home/xdg-config"))
        .env("THURBOX_CONFIG_DIR", path("config"))
        .env("THURBOX_DATA_DIR", path("data"))
        .env_remove("THURBOX_SESSION")
        .env_remove("THURBOX_SESSION_ID")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
        server.scope(&mut cmd);
        let out = cmd.output().expect("run thurbox-cli");
        let report: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "create printed no JSON ({e}): {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        assert!(out.status.success(), "create failed: {report}");
        report
    };

    let report = create(&[]);
    assert_eq!(report["backend_type"], "ssh:box:tmux", "{report}");
    assert_eq!(
        report["tmux_socket"], HOST_SOCKET,
        "a remote session's socket is the host's: {report}"
    );
    // The adopt answer is the same document, so it names the same server.
    let adopted = create(&["--on-existing", "adopt"]);
    assert_eq!(adopted["created"], false, "{adopted}");
    assert_eq!(adopted["tmux_socket"], HOST_SOCKET, "{adopted}");

    // And it is the socket the pane is really on.
    let pane = report["backend_id"].as_str().expect("backend_id");
    let on_host = Command::new("tmux")
        .env("TMUX_TMPDIR", server.tmpdir())
        .env_remove("TMUX")
        .args([
            "-L",
            HOST_SOCKET,
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_id}",
        ])
        .output()
        .expect("run tmux");
    assert_eq!(String::from_utf8_lossy(&on_host.stdout).trim(), pane);
}
