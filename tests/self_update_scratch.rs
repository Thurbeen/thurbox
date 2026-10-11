#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime};

struct UpdateProcess {
    child: Child,
    stopped: bool,
}

impl UpdateProcess {
    fn stop(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if !self.stopped {
            // The leader is kept unreaped until the group is stopped, so its
            // pid cannot be reused for a group this test does not own.
            let result = unsafe { libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            self.stopped = true;
        }
        self.child.wait()
    }
}

impl Drop for UpdateProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn updater(root: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    cmd.args(["update", "--force"])
        .env("THURBOX_CONFIG_DIR", root.join("config"))
        .env("THURBOX_DATA_DIR", root.join("data"))
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env_remove("THURBOX_SOCKET")
        .env_remove("THURBOX_SOCKET_FOR")
        .env("PATH", root.join("bin"));
    cmd
}

#[test]
fn interrupted_cli_update_recovers_only_abandoned_scratch() {
    let root = tempfile::tempdir().unwrap();
    for directory in ["config", "data", "home", "bin"] {
        std::fs::create_dir(root.path().join(directory)).unwrap();
    }
    let live = root
        .path()
        .join("data")
        .join(format!(".update-{}-23", std::process::id()));
    std::fs::create_dir(&live).unwrap();
    std::fs::write(live.join("keep"), b"live update").unwrap();
    std::fs::File::open(&live)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(7200)),
        )
        .unwrap();
    for (name, script) in [
        (
            "curl",
            r#"#!/bin/sh
output=''
while [ "$#" -gt 0 ]; do
    if [ "$1" = '-o' ]; then
        shift
        output="$1"
    fi
    shift
done
if [ -z "$output" ]; then
    printf '%s' '{"tag_name":"v1.2.3"}'
    exit 0
fi
printf '%s' 'partial checksum' > "$output"
if [ "$THURBOX_UPDATE_TEST_HOLD" = '1' ]; then
    /bin/sleep 15
fi
exit 1
"#,
        ),
        ("wget", "#!/bin/sh\nexit 1\n"),
    ] {
        let path = root.path().join("bin").join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut running = UpdateProcess {
        child: updater(root.path())
            .env("THURBOX_UPDATE_TEST_HOLD", "1")
            .process_group(0)
            .spawn()
            .unwrap(),
        stopped: false,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let abandoned: PathBuf = loop {
        if let Some(path) = std::fs::read_dir(root.path().join("data"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                std::fs::read(path.join("thurbox-v1.2.3-checksums.txt"))
                    .is_ok_and(|bytes| bytes == b"partial checksum")
            })
        {
            break path;
        }
        assert!(
            Instant::now() < deadline,
            "update never reached its download"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!running.stop().unwrap().success());
    assert!(abandoned.exists(), "termination must bypass scratch Drop");
    std::fs::File::open(&abandoned)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(7200)),
        )
        .unwrap();
    let next = updater(root.path())
        .env("THURBOX_UPDATE_TEST_HOLD", "0")
        .output()
        .unwrap();
    assert!(
        !next.status.success(),
        "fake download must stop before installation"
    );
    assert!(String::from_utf8_lossy(&next.stdout).contains("failed to download"));
    assert!(
        !abandoned.exists(),
        "next CLI update must recover killed scratch"
    );
    assert_eq!(std::fs::read(live.join("keep")).unwrap(), b"live update");
    assert_eq!(
        std::fs::read_dir(root.path().join("data"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".update-"))
            .count(),
        1
    );
}
