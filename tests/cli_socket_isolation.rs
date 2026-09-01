//! An instance relocated by `THURBOX_DATA_DIR` runs on its own tmux socket.
//!
//! Driven through the real `thurbox-cli` binary, reading the `tmux_socket`
//! field integrators read (`version --json`) — the socket a build *reports* is
//! the only observable form of "which server do my sessions live on", and the
//! contract is that it always names the one actually in use.
//!
//! No test here starts a multiplexer: the resolution is what is under test, and
//! spawning a session would put windows on somebody's server, which is the very
//! thing this change exists to prevent.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

/// One throwaway environment: real `HOME`/XDG kept out, so the "default
/// instance" of these tests is the temp one rather than the operator's.
struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        Self { root }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    /// The data dir this environment's *default* instance resolves to — the
    /// XDG default, which a relocation has to differ from to count as one.
    fn default_data_dir(&self) -> PathBuf {
        self.path("data").join(thurbox::paths::app_dir_name())
    }

    /// `thurbox-cli version --json`, with `THURBOX_*` set from `vars`.
    fn version(&self, vars: &[(&str, &OsStr)]) -> Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.arg("version").arg("--json");
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
        cmd.env("XDG_CONFIG_HOME", self.path("config"));
        cmd.env("XDG_DATA_HOME", self.path("data"));
        cmd.env("APPDATA", self.path("config"));
        cmd.env("LOCALAPPDATA", self.path("data"));
        // Inherited from the developer's own session, and the subject of the
        // test — never leave them to chance. Scrubbed as a namespace rather
        // than a list: `cargo test` runs inside a live thurbox session on any
        // developer machine, and a list has to be extended every time thurbox
        // injects one more var. It was not. `THURBOX_SOCKET_FOR` — the tag that
        // tells an inherited socket from an operator's own — leaked in and made
        // `an_explicit_socket_still_wins` fail, because the ambient tag named a
        // data dir that was not the test's and the override was correctly ruled
        // inherited.
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("THURBOX_") {
                cmd.env_remove(&key);
            }
        }
        for (key, value) in vars {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("run thurbox-cli version");
        assert!(
            out.status.success(),
            "thurbox-cli version failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("version --json is JSON")
    }

    /// The reported socket for an instance configured by `vars`.
    fn socket(&self, vars: &[(&str, &OsStr)]) -> String {
        self.version(vars)["tmux_socket"]
            .as_str()
            .expect("version --json reports tmux_socket")
            .to_string()
    }
}

#[test]
fn a_default_instance_keeps_its_socket() {
    let env = Env::new();
    let plain = env.socket(&[]);
    assert!(!plain.is_empty(), "a socket is always reported");

    // thurbox injects THURBOX_DATA_DIR into every session it spawns, pointing
    // at the dir it resolved itself. A `thurbox-cli` inside such a session —
    // an agent's status hook — must still land on the operator's server.
    let restated = env.socket(&[("THURBOX_DATA_DIR", env.default_data_dir().as_os_str())]);
    assert_eq!(
        restated, plain,
        "restating the default data dir is not a relocation"
    );
}

#[test]
fn a_relocated_instance_reports_a_socket_of_its_own() {
    let env = Env::new();
    let default = env.socket(&[]);
    let lab = env.path("lab");
    let other = env.path("other");

    let report = env.version(&[("THURBOX_DATA_DIR", lab.as_os_str())]);
    let lab_socket = report["tmux_socket"].as_str().unwrap().to_string();
    assert_eq!(
        report["data_dir"].as_str(),
        Some(lab.to_string_lossy().as_ref()),
        "the data dir moved"
    );
    assert_ne!(lab_socket, default, "and so did the socket");
    assert_ne!(
        env.socket(&[("THURBOX_DATA_DIR", other.as_os_str())]),
        lab_socket,
        "two relocated instances do not share a server"
    );
    assert_eq!(
        env.socket(&[("THURBOX_DATA_DIR", lab.as_os_str())]),
        lab_socket,
        "and one finds its own again on the next run"
    );
}

#[test]
fn an_explicit_socket_still_wins() {
    let env = Env::new();
    let lab = env.path("lab");
    assert_eq!(
        env.socket(&[
            ("THURBOX_DATA_DIR", lab.as_os_str()),
            ("THURBOX_SOCKET", OsStr::new("thurbox-named-lab")),
        ]),
        "thurbox-named-lab",
        "THURBOX_SOCKET names the server outright, relocated or not"
    );
}

/// The socket thurbox injects into a pane belongs to the instance that spawned
/// it. A child that *relocates itself* out of that instance must not keep it.
///
/// `session_ops::thurbox_env_overrides` puts `THURBOX_DATA_DIR` and
/// `THURBOX_SOCKET` on every pane together, so they arrive consistent. The
/// hazard is the next step: a sandbox, a test harness or an agent that exports
/// a *different* `THURBOX_DATA_DIR` inside that pane inherits a socket naming
/// the operator's server, and the derivation that exists to prevent exactly
/// this never runs. Isolating the database while silently sharing the tmux
/// server is worse than not isolating at all — it looks contained and is not.
#[test]
fn an_inherited_socket_loses_to_a_relocation() {
    let env = Env::new();
    let default_data = env.default_data_dir();
    let default_socket = env.socket(&[]);
    let lab = env.path("lab");

    // Exactly what a pane carries: the pair, agreeing with each other.
    let inherited: [(&str, &OsStr); 3] = [
        ("THURBOX_DATA_DIR", lab.as_os_str()),
        ("THURBOX_SOCKET", OsStr::new(default_socket.as_str())),
        ("THURBOX_SOCKET_FOR", default_data.as_os_str()),
    ];
    assert_eq!(
        env.socket(&inherited),
        env.socket(&[("THURBOX_DATA_DIR", lab.as_os_str())]),
        "a socket inherited from another instance does not survive a relocation"
    );

    // The same inherited pair, with the data dir left alone: nothing was
    // relocated, so the injected socket is still the right answer.
    assert_eq!(
        env.socket(&[
            ("THURBOX_DATA_DIR", default_data.as_os_str()),
            ("THURBOX_SOCKET", OsStr::new(default_socket.as_str())),
            ("THURBOX_SOCKET_FOR", default_data.as_os_str()),
        ]),
        default_socket,
        "an agent's hook still finds the server its session lives on"
    );
}
