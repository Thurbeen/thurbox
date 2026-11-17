//! An update replaces `fleet` on disk, and a running `fleet up` keeps the
//! program it started from. The update restarts it on the new one, against
//! the same store: its Tasks, their inboxes and their sessions stay.
//!
//! The real `thurbox-cli update` and the real `fleet`, on Fleet's fake
//! runtime: `thurbox-cli` runs from an install directory of its own, and a
//! stand-in `curl` serves a release whose archive ships the same `fleet`.
#![cfg(target_os = "linux")]

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BOUND: Duration = Duration::from_secs(60);
const RELEASE: &str = "3.99.0";

fn fleet_bin() -> Option<PathBuf> {
    let found = std::env::var_os("FLEET_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            Path::new(env!("CARGO_BIN_EXE_thurbox-cli"))
                .parent()
                .map(|dir| dir.join("fleet"))
        })
        .filter(|p| p.is_file());
    if found.is_none() {
        assert!(
            std::env::var_os("CI").is_none(),
            "no fleet binary in CI, where the tests that drive Fleet must run"
        );
        eprintln!("skipping: no fleet binary; build it with `cargo build -p fleet-cli`");
    }
    found
}

fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, script).expect("script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + BOUND;
    while !done() {
        assert!(Instant::now() < deadline, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// An install of thurbox with its `fleet`, an isolated Fleet store on the
/// fake runtime, and a release feed offering [`RELEASE`].
struct Install {
    root: tempfile::TempDir,
    manager: Option<Child>,
}

impl Install {
    fn new(fleet: &Path) -> Install {
        let root = tempfile::Builder::new()
            .prefix("update-fleet-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .expect("root");
        let install = Install {
            root,
            manager: None,
        };
        for dir in [
            "install",
            "feed",
            "stage",
            "fakebin",
            "home",
            "tmp",
            "thurbox-config",
            "thurbox-data",
        ] {
            std::fs::create_dir_all(install.path(dir)).expect("dir");
        }
        let cli = install.path("install/thurbox-cli");
        if std::fs::hard_link(env!("CARGO_BIN_EXE_thurbox-cli"), &cli).is_err() {
            std::fs::copy(env!("CARGO_BIN_EXE_thurbox-cli"), &cli).expect("thurbox-cli");
        }
        std::fs::copy(fleet, install.path("install/fleet")).expect("fleet");
        std::fs::write(
            install.config(),
            format!(
                "version = 1\n\n[data]\ndir = '{}'\n\n[runtime]\nkind = \"fake\"\ndir = '{}'\n",
                install.data().display(),
                install.path("fake").display()
            ),
        )
        .expect("fleet config");
        install.publish(fleet);
        install
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn config(&self) -> PathBuf {
        self.path("fleet.toml")
    }

    fn data(&self) -> PathBuf {
        self.path("fleet-data")
    }

    fn fleet(&self) -> PathBuf {
        self.path("install/fleet")
    }

    /// The release archive, its checksums, and a `curl` that serves them by
    /// file name: the list when asked for no file.
    fn publish(&self, fleet: &Path) {
        let stage = self.path("stage");
        for name in ["thurbox", "thurbox-cli", "runtime-mux", "workspace-git"] {
            executable(&stage.join(name), "#!/bin/sh\nexit 0\n");
        }
        std::fs::copy(fleet, stage.join("fleet")).expect("fleet");
        let target = thurbox::update::self_update::current_target().expect("target");
        let archive = thurbox::update::self_update::archive_name(RELEASE, target);
        let packed = Command::new("tar")
            .arg("czf")
            .arg(self.path("feed").join(&archive))
            .arg("-C")
            .arg(&stage)
            .args([
                "thurbox",
                "thurbox-cli",
                "fleet",
                "runtime-mux",
                "workspace-git",
            ])
            .status()
            .expect("tar");
        assert!(packed.success(), "tar the release archive");
        let digest = Command::new("sha256sum")
            .arg(self.path("feed").join(&archive))
            .output()
            .expect("sha256sum");
        let digest = String::from_utf8_lossy(&digest.stdout);
        let digest = digest.split_whitespace().next().expect("a digest");
        std::fs::write(
            self.path("feed")
                .join(format!("thurbox-{RELEASE}-sha256sums.txt")),
            format!("{digest}  {archive}\n"),
        )
        .expect("checksums");
        executable(
            &self.path("fakebin/curl"),
            &format!(
                "#!/bin/sh\n\
                 out=''\n\
                 url=''\n\
                 while [ $# -gt 0 ]; do\n\
                 case \"$1\" in -o) out=$2; shift 2 ;; *) url=$1; shift ;; esac\n\
                 done\n\
                 if [ -z \"$out\" ]; then\n\
                 printf '%s' '{list}'\n\
                 exit 0\n\
                 fi\n\
                 exec cp \"{feed}/${{url##*/}}\" \"$out\"\n",
                list = json!([{"tag_name": format!("v{RELEASE}")}]),
                feed = self.path("feed").display(),
            ),
        );
    }

    fn env(&self, command: &mut Command) {
        command
            .env_clear()
            .env("HOME", self.path("home"))
            .env("TMPDIR", self.path("tmp"))
            .env("THURBOX_CONFIG_DIR", self.path("thurbox-config"))
            .env("THURBOX_DATA_DIR", self.path("thurbox-data"))
            .env("FLEET_CONFIG", self.config())
            .env(
                "PATH",
                format!(
                    "{}:{}:/usr/bin:/bin",
                    self.path("fakebin").display(),
                    self.path("install").display()
                ),
            );
    }

    fn fleet_run(&self, args: &[&str], input: Option<&str>) -> std::process::Output {
        use std::io::Write;
        let mut command = Command::new(self.fleet());
        self.env(&mut command);
        let mut child = command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("fleet");
        let mut stdin = child.stdin.take().expect("stdin");
        stdin
            .write_all(input.unwrap_or_default().as_bytes())
            .expect("stdin");
        drop(stdin);
        let out = child.wait_with_output().expect("fleet");
        assert!(
            out.status.success(),
            "fleet {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn task(&self, name: &str) -> Value {
        serde_json::from_slice(&self.fleet_run(&["get", "task", name], None).stdout).expect("JSON")
    }

    /// The pid of the `fleet up` holding the lease, if one does.
    fn holder(&self) -> Option<u32> {
        let out = self.fleet_run(&["version", "--json"], None);
        let version: Value = serde_json::from_slice(&out.stdout).expect("JSON");
        if version["manager"]["held"] != json!(true) {
            return None;
        }
        version["manager"]["holder"]
            .as_str()?
            .strip_prefix("fleet-up-")?
            .parse()
            .ok()
    }

    /// `fleet up` from the installed `fleet`, as thurbox or a person starts it.
    fn start_manager(&mut self) -> u32 {
        let mut command = Command::new(self.fleet());
        self.env(&mut command);
        let child = command
            .arg("up")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("fleet up");
        let pid = child.id();
        self.manager = Some(child);
        until("the manager holds the lease", || self.holder() == Some(pid));
        pid
    }

    fn update(&self) -> std::process::Output {
        let mut command = Command::new(self.path("install/thurbox-cli"));
        self.env(&mut command);
        command
            .args(["update", "--force"])
            .stdin(Stdio::null())
            .output()
            .expect("thurbox-cli update")
    }

    /// Every file the fake runtime keeps its sessions in, as one text.
    fn runtime_state(&self) -> String {
        std::fs::read_dir(self.path("fake"))
            .into_iter()
            .flatten()
            .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path()).ok())
            .collect()
    }

    /// The `fleet up` processes running the program now installed.
    fn managers_on_installed_fleet(&self) -> Vec<u32> {
        let installed = self.fleet();
        std::fs::read_dir("/proc")
            .expect("/proc")
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| {
                std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| exe == installed)
                    && std::fs::read(format!("/proc/{pid}/cmdline"))
                        .is_ok_and(|cmdline| cmdline.split(|b| *b == 0).any(|arg| arg == b"up"))
            })
            .collect()
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        if let Some(pid) = self.holder() {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
        if let Some(mut child) = self.manager.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn alpha() -> Value {
    json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Task",
        "metadata": {"name": "alpha"},
        "spec": {
            "harness": "claude-code",
            "host": "local",
            "dispatch": true,
            "workspace": ["file:///work/alpha"],
        }
    })
}

fn exe_of(pid: u32) -> String {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .map(|exe| exe.display().to_string())
        .unwrap_or_else(|e| format!("<{e}>"))
}

#[test]
fn an_update_restarts_the_running_fleet_up_on_the_new_fleet() {
    let Some(bin) = fleet_bin() else { return };
    let mut install = Install::new(&bin);
    install.fleet_run(&["apply", "-f", "-"], Some(&alpha().to_string()));
    let old = install.start_manager();
    until("the old manager gives alpha its session", || {
        install.task("alpha")["status"]["session"]["key"].is_string()
    });
    install.fleet_run(&["send", "alpha", "kept across the update"], None);
    let before = install.task("alpha");

    let updated = install.update();
    assert!(
        updated.status.success(),
        "update: {}\n{}",
        String::from_utf8_lossy(&updated.stdout),
        String::from_utf8_lossy(&updated.stderr)
    );

    let holder = install.holder();
    assert!(
        holder.is_some_and(|pid| pid != old),
        "the running fleet up is still the old binary, or is left stopped: \
         lease holder {holder:?} (old {old}, running {})",
        exe_of(old)
    );
    let new = holder.unwrap();
    assert_eq!(
        exe_of(new),
        install.fleet().display().to_string(),
        "the new manager runs the installed fleet"
    );
    let old_status = install
        .manager
        .as_mut()
        .expect("the old manager")
        .try_wait()
        .expect("wait");
    assert_eq!(
        old_status.map(|s| s.code()),
        Some(Some(0)),
        "the old manager stopped gracefully"
    );
    assert_eq!(
        install.managers_on_installed_fleet(),
        [new],
        "one engine, not two"
    );

    let after = install.task("alpha");
    assert_eq!(
        after["status"]["session"]["key"], before["status"]["session"]["key"],
        "{after:#}"
    );
    assert_eq!(after["metadata"]["uid"], before["metadata"]["uid"]);
    until("the inbox message is delivered to the same session", || {
        install.runtime_state().contains("kept across the update")
    });
    assert!(
        !install.runtime_state().contains("\"gone\":true"),
        "{}",
        install.runtime_state()
    );
}

#[test]
fn an_update_with_no_fleet_up_running_starts_none() {
    let Some(bin) = fleet_bin() else { return };
    let install = Install::new(&bin);
    install.fleet_run(&["apply", "-f", "-"], Some(&alpha().to_string()));

    let updated = install.update();
    assert!(
        updated.status.success(),
        "update: {}\n{}",
        String::from_utf8_lossy(&updated.stdout),
        String::from_utf8_lossy(&updated.stderr)
    );
    assert_eq!(install.holder(), None);
    assert!(install.managers_on_installed_fleet().is_empty());
    assert!(install.task("alpha")["metadata"]["uid"].is_string());
}
