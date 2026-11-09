//! fleet-exec's promises, proven with real processes on the OS that runs
//! the test: the child is this test binary itself, re-entered in a role
//! named by `FLEET_EXEC_ROLE`, so the same test runs natively on Linux,
//! macOS and Windows with no shell and no helper program.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use fleet_exec::{Outcome, Spec, run};

/// Not a test of its own: the body each child role runs.
#[test]
fn child_role() {
    let Ok(role) = std::env::var("FLEET_EXEC_ROLE") else {
        return;
    };
    let out = PathBuf::from(std::env::var("FLEET_EXEC_OUT").expect("out"));
    match role.as_str() {
        "hang-with-grandchild" => {
            // Left running on purpose: it is what the parent's kill must reach.
            #[allow(clippy::zombie_processes)]
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "child_role", "--nocapture"])
                .env("FLEET_EXEC_ROLE", "sleep")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            fs::write(&out, child.id().to_string()).unwrap();
            sleep(Duration::from_secs(600));
        }
        "sleep" => sleep(Duration::from_secs(600)),
        "echo" => {
            let mut input = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).unwrap();
            let token = std::env::var("FLEET_EXEC_TOKEN").unwrap_or_default();
            let path_kept = std::env::var_os("PATH").is_some();
            let argv: Vec<String> = std::env::args().collect();
            fs::write(&out, format!("{input}|{token}|{path_kept}")).unwrap();
            fs::write(out.with_extension("argv"), argv.join("\n")).unwrap();
            print!("printed");
        }
        other => panic!("unknown role {other}"),
    }
}

fn child(role: &str, out: &Path, timeout: Duration) -> Spec {
    let mut s = Spec::new(std::env::current_exe().unwrap(), timeout).args([
        "--exact",
        "child_role",
        "--nocapture",
        "--test-threads",
        "1",
    ]);
    s.env.push(("FLEET_EXEC_ROLE".into(), role.into()));
    s.env
        .push(("FLEET_EXEC_OUT".into(), out.to_string_lossy().into_owned()));
    s
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("exec")
        .join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        Err(_) => Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success()),
    }
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
}

#[test]
fn a_child_past_its_bound_is_killed_with_its_whole_tree() {
    let dir = scratch("tree");
    let out = dir.join("pid");
    let started = Instant::now();
    let o = run(&child("hang-with-grandchild", &out, Duration::from_secs(3)));
    assert!(matches!(o, Outcome::TimedOut { .. }), "{o:?}");
    assert!(started.elapsed() < Duration::from_secs(30), "bounded");
    let pid: u32 = fs::read_to_string(&out).unwrap().trim().parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) && Instant::now() < deadline {
        sleep(Duration::from_millis(50));
    }
    assert!(!alive(pid), "grandchild {pid} outlived the kill");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn stdin_and_environment_reach_the_child_and_never_its_command_line() {
    let dir = scratch("echo");
    let out = dir.join("echo");
    // A value for the environment only, and an inherited variable (PATH)
    // the parent removes.
    let mut spec = child("echo", &out, Duration::from_secs(30)).stdin("from-stdin");
    spec.env
        .push(("FLEET_EXEC_TOKEN".into(), "tok-value".into()));
    spec.env_remove.push("PATH".into());
    let shown = format!("{spec:?}");
    assert!(!shown.contains("tok-value"), "{shown}");
    let o = run(&spec);
    let text = o.success_text().unwrap_or_else(|| panic!("{o:?}"));
    assert!(text.contains("printed"), "{text}");
    assert_eq!(
        fs::read_to_string(&out).unwrap(),
        "from-stdin|tok-value|false"
    );
    // The child's own argv, as it saw it.
    let argv = fs::read_to_string(out.with_extension("argv")).unwrap();
    assert!(
        !argv.contains("tok-value"),
        "the value reached argv: {argv}"
    );
    let _ = fs::remove_dir_all(dir);
}
