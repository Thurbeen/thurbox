//! A host that never answers must not hold the heartbeat's tick (#1431).
//!
//! The host here is reached through the real `ssh` client, with a proxy that
//! accepts the connection and then says nothing, and `ConnectTimeout=0` in the
//! host's own `ssh_opts` so ssh's connect deadline cannot rescue it. That is
//! the transport wedged mid-handshake: ssh waits for a server banner that never
//! comes, exactly as a degraded `wsl.exe` waits on a service that never
//! answers. Before the fix the delegation probe waited with it, for as long as
//! the transport cared to hang.
//!
//! A file of its own because the host-routing suites stand in for ssh with
//! shell scripts and so run on POSIX only; this one drives the real client and
//! runs on Windows too, where the wedge was seen.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long the tick may take before it counts as wedged: the probe deadline
/// and the startup around it, with room for a loaded CI runner.
const TICK_BUDGET: Duration = Duration::from_secs(90);

/// Outlives [`TICK_BUDGET`], so the proxy is still silent when the deadline
/// passes, and then exits on its own — it is ssh's child, not ours to reap.
#[cfg(unix)]
const SILENT_PROXY: &str = "sleep 120";
#[cfg(windows)]
const SILENT_PROXY: &str = "powershell -NoProfile -Command Start-Sleep 120";

#[test]
fn a_host_that_never_answers_does_not_wedge_the_automation_tick() {
    // With its suffix: on Windows the file on PATH is `ssh.exe`, and a bare
    // `ssh` finds nothing there, so the test would skip where it matters most.
    let ssh = format!("ssh{}", std::env::consts::EXE_SUFFIX);
    if thurbox::paths::resolve_on_path(&ssh).is_none() {
        eprintln!("skipping: no ssh client on PATH");
        return;
    }
    let root = tempfile::TempDir::new().expect("tempdir");
    for sub in ["home", "config", "data"] {
        std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
    }
    std::fs::write(
        root.path().join("config/hosts.toml"),
        format!(
            "[[hosts]]\nname = 'silent'\ndestination = 'silent.invalid'\n\
             ssh_opts = ['-o', 'ConnectTimeout=0', '-o', 'ProxyCommand={SILENT_PROXY}']\n"
        ),
    )
    .expect("write hosts.toml");

    let mut child = cli(root.path(), &["automation", "tick", "--json"])
        .spawn()
        .expect("run thurbox-cli");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for thurbox-cli") {
            break status;
        }
        if started.elapsed() > TICK_BUDGET {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "`automation tick` was still blocked on the silent host after {}s",
                TICK_BUDGET.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    assert!(status.success(), "tick failed: {stdout}");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("tick JSON");
    let synced = report.to_string();
    assert!(
        synced.contains("silent") && synced.contains("did not answer within"),
        "the silent host should be reported as timed out: {stdout}"
    );
}

fn cli(root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    cmd.args(args)
        .env("HOME", root.join("home"))
        .env("USERPROFILE", root.join("home"))
        .env("THURBOX_CONFIG_DIR", root.join("config"))
        .env("THURBOX_DATA_DIR", root.join("data"))
        .env_remove("THURBOX_SOCKET")
        .env_remove("THURBOX_SOCKET_FOR")
        .env_remove("THURBOX_SESSION")
        .env_remove("THURBOX_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    cmd
}
