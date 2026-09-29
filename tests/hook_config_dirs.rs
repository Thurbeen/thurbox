//! The status hooks land in the config dir each agent actually reads.
//!
//! An agent whose config dir was moved by its own environment variable
//! (`CODEX_HOME`, `PI_CODING_AGENT_DIR`, `COPILOT_HOME`) never looks in the
//! default one. The built-in hooks extension used to write only there — or,
//! when the default dir did not exist, skip the agent as not installed — so a
//! relocated agent reported nothing, silently.
//!
//! Run through the real binary with its own `HOME`, so no developer config is
//! ever touched.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;

fn tick(root: &Path, env: &[(&str, &Path)]) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    cmd.args(["automation", "tick", "--json"])
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("home/.config"))
        .env("XDG_DATA_HOME", root.join("home/.local/share"));
    // Scrubbed as a namespace: `cargo test` runs inside a live thurbox session
    // on a developer machine, and an inherited var would point this at it.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("THURBOX_") {
            cmd.env_remove(&key);
        }
    }
    for var in ["CODEX_HOME", "PI_CODING_AGENT_DIR", "COPILOT_HOME"] {
        cmd.env_remove(var);
    }
    cmd.env("THURBOX_CONFIG_DIR", root.join("config"))
        .env("THURBOX_DATA_DIR", root.join("data"))
        .env("THURBOX_SOCKET", "thurbox-hook-config-dirs")
        .env("TMUX_TMPDIR", root.join("tmux"));
    for (key, value) in env {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("run thurbox-cli automation tick");
    assert!(
        out.status.success(),
        "tick failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn hooks_follow_an_agents_relocated_config_dir() {
    let root = tempfile::tempdir().expect("tempdir");
    for sub in ["home", "config", "data", "tmux"] {
        std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
    }
    let codex = root.path().join("elsewhere/codex");
    let pi = root.path().join("elsewhere/pi-agent");
    let copilot = root.path().join("elsewhere/copilot");
    for dir in [&codex, &pi, &copilot] {
        std::fs::create_dir_all(dir).expect("mkdir");
    }

    tick(
        root.path(),
        &[
            ("CODEX_HOME", &codex),
            ("PI_CODING_AGENT_DIR", &pi),
            ("COPILOT_HOME", &copilot),
        ],
    );

    for wired in [
        codex.join("hooks.json"),
        pi.join("extensions/thurbox-status.ts"),
        copilot.join("hooks/thurbox-status.json"),
    ] {
        assert!(
            wired.is_file(),
            "{} was not written, so that agent reports nothing",
            wired.display()
        );
    }
    // And nothing was created where the relocated agents no longer look.
    for default in [".codex", ".pi", ".copilot"] {
        assert!(
            !root.path().join("home").join(default).exists(),
            "~/{default} was created for an agent that reads elsewhere"
        );
    }
}
