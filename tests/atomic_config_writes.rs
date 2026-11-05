//! A write that fails partway leaves a user's config file as it was (#1634).
//!
//! The failure is real, not mocked: the CLI runs under `RLIMIT_FSIZE` with
//! `SIGXFSZ` ignored, so any write past the limit stops there with `EFBIG`. A
//! file rewritten in place is truncated before its new contents fail to fit;
//! one staged beside its destination and renamed over it is not.
#![cfg(target_os = "linux")]

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Far larger than the database and logs the CLI writes before the agents
/// step, so only a rewrite of `agents.toml` reaches the limit.
const PADDING: usize = 1 << 20;

struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for sub in ["home", "config", "data", "extension"] {
            std::fs::create_dir_all(root.path().join(sub)).unwrap();
        }
        std::fs::write(
            root.path().join("config/settings.toml"),
            "[features]\nautomations = false\nversion_check = false\nauto_update = false\n",
        )
        .unwrap();
        Self { root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.path().join(p)
    }

    /// `thurbox-cli` that cannot write any file past `limit` bytes.
    fn run_limited(&self, args: &[&str], limit: u64) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.arg("--json")
            .args(args)
            .env("HOME", self.path("home"))
            .env("THURBOX_CONFIG_DIR", self.path("config"))
            .env("THURBOX_DATA_DIR", self.path("data"))
            .env(
                "THURBOX_SOCKET",
                format!("thurbox-atomic-{}", std::process::id()),
            )
            .env_remove("THURBOX_SOCKET_FOR")
            .env_remove("THURBOX_SESSION")
            .env_remove("THURBOX_SESSION_ID");
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_PREFIX",
            "GIT_NAMESPACE",
        ] {
            cmd.env_remove(var);
        }
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                let cap = libc::rlimit {
                    rlim_cur: limit,
                    rlim_max: limit,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &cap) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // An ignored signal stays ignored across exec, so the write
                // past the limit returns EFBIG instead of killing the CLI.
                libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
                Ok(())
            });
        }
        cmd.output().unwrap()
    }
}

fn padded_agents_toml() -> String {
    let mut text = String::from("default = 'shell'\n[[agents]]\nname = 'shell'\ncommand = 'sh'\n");
    while text.len() < PADDING {
        text.push_str("# a comment the user wrote, kept across every edit thurbox makes\n");
    }
    text
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn a_failed_rewrite_of_agents_toml_leaves_the_original_unchanged() {
    let env = Env::new();
    let agents = env.path("config/agents.toml");
    let original = padded_agents_toml();
    std::fs::write(&agents, &original).unwrap();
    std::fs::write(
        env.path("extension/extension.toml"),
        "name = 'grows-agents'\n[[agents]]\nname = 'grows-agents-worker'\ncommand = 'sh'\n",
    )
    .unwrap();

    let source = env.path("extension");
    let out = env.run_limited(
        &["extension", "install", source.to_str().unwrap(), "--force"],
        (original.len() / 2) as u64,
    );

    assert!(
        !out.status.success(),
        "the limit must stop the write: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let after = read(&agents);
    assert!(
        after == original,
        "agents.toml changed by a failed write: {} bytes, was {}",
        after.len(),
        original.len()
    );
    let strays: Vec<_> = std::fs::read_dir(env.path("config"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("saving-"))
        .collect();
    assert!(strays.is_empty(), "staged files left behind: {strays:?}");
}
