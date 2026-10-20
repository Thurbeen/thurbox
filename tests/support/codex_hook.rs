//! A disposable pane-local Codex stand-in that stays alive while its hook runs.
use std::path::Path;
use std::process::Command;

pub fn command(home: &Path) -> Command {
    command_with_mode(home, true)
}

pub fn command_with_mode(home: &Path, no_daemon: bool) -> Command {
    use std::os::unix::fs::PermissionsExt;
    let path = home.join("hook-process");
    std::fs::create_dir_all(&path).unwrap();
    let script = path.join("codex");
    std::fs::write(
        &script,
        "#!/bin/sh\n\"$HOOK_CLI\" session bind-codex --json\nstatus=$?\nexit $status\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cmd = Command::new(script);
    if no_daemon {
        cmd.arg("--no-daemon");
    }
    cmd.env("HOOK_CLI", env!("CARGO_BIN_EXE_thurbox-cli"));
    cmd
}

#[allow(dead_code)]
pub fn nested_command(home: &Path) -> Command {
    use std::os::unix::fs::PermissionsExt;
    let inner = command(home).get_program().to_owned();
    let dir = home.join("outer");
    std::fs::create_dir_all(&dir).unwrap();
    let outer = dir.join("codex");
    std::fs::write(
        &outer,
        "#!/bin/sh\n\"$INNER_CODEX\" --no-daemon\nstatus=$?\nexit $status\n",
    )
    .unwrap();
    std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = Command::new(outer);
    command
        .arg("--no-daemon")
        .env("INNER_CODEX", inner)
        .env("HOOK_CLI", env!("CARGO_BIN_EXE_thurbox-cli"));
    command
}

#[allow(dead_code)]
pub fn node_launcher_command(home: &Path) -> Command {
    let outer = nested_command(home);
    let inner = command(home).get_program().to_owned();
    let mut command = Command::new("bash");
    command
        .args(["-c", "exec -a node sh \"$1\" --no-daemon", "launcher"])
        .arg(outer.get_program())
        .env("INNER_CODEX", inner)
        .env("HOOK_CLI", env!("CARGO_BIN_EXE_thurbox-cli"));
    command
}
