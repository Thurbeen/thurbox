//! The opencode status plugin against a real opencode, end to end.
//!
//! `hook_turn_sequence` holds the plugin to each loader's contract under Node;
//! this runs the real thing. A V1-shaped plugin, as an earlier thurbox
//! installed it, is planted where opencode reads it; `thurbox-cli automation
//! tick` — the headless half of the install/update path — must replace it;
//! then a real `opencode run` must load it without a "failed to load plugin"
//! and report the turn through `thurbox-cli session signal` (on 2.x, from the
//! `--standalone` server a session's own TUI spawns).
//!
//! Opt-in, because it needs an opencode binary and a model reachable over the
//! network: set `THURBOX_E2E_OPENCODE` to the binary (1.x or 2.x — both must
//! pass) and optionally `THURBOX_E2E_OPENCODE_MODEL` (default: one of
//! opencode's free models). Everything opencode and thurbox read or write goes
//! to a temporary `HOME`, and the `thurbox-cli` the plugin calls is a stub that
//! records the state word — a `#!/bin/sh` script, so this is unix-only.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_MODEL: &str = "opencode/deepseek-v4.1-flash-free";

/// The plugin as thurbox installed it before opencode 2: a named export of a
/// V1 plugin function, which 2.x refuses. Carries the managed marker, as the
/// installed copy did, so the installer may overwrite it.
const V1_PLUGIN: &str = r#"// Managed by thurbox `extension install` (the built-in "hooks" extension).
export const ThurboxStatus = async ({ $ }) => {
  const signal = async (state) => {
    await $`thurbox-cli session signal --state ${state}`.quiet().nothrow();
  };
  return {
    "chat.message": async () => {
      await signal("working");
    },
    event: async ({ event }) => {
      if (event?.type === "session.idle") await signal("done");
    },
  };
};
"#;

fn opencode() -> Option<PathBuf> {
    std::env::var_os("THURBOX_E2E_OPENCODE").map(PathBuf::from)
}

/// Every variable that decides where opencode and thurbox keep their files,
/// pointed into `home`; the stub `thurbox-cli` first on `PATH`.
fn sandboxed(cmd: &mut Command, home: &Path, bin: &Path) {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(bin.to_path_buf()).chain(std::env::split_paths(&path)),
    )
    .expect("join PATH");
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("THURBOX_CONFIG_DIR", home.join("thurbox-config"))
        .env("THURBOX_DATA_DIR", home.join("thurbox-data"))
        .env("THURBOX_SESSION", "opencode-e2e")
        .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
        .env("PATH", path)
        .env_remove("THURBOX_SOCKET")
        .env_remove("THURBOX_SOCKET_FOR");
}

#[test]
fn the_installed_plugin_loads_and_reports_a_turn_in_a_real_opencode() {
    let Some(opencode) = opencode() else {
        eprintln!("skipping: THURBOX_E2E_OPENCODE is not set");
        return;
    };
    let model =
        std::env::var("THURBOX_E2E_OPENCODE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let bin = dir.path().join("bin");
    let project = dir.path().join("project");
    let log = dir.path().join("states");
    let plugin = home.join(".config/opencode/plugin/thurbox-status.js");
    for d in [plugin.parent().expect("plugin dir"), &bin, &project] {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    std::fs::write(&plugin, V1_PLUGIN).expect("plant the V1 plugin");

    let stub = bin.join("thurbox-cli");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$4\" >> \"{}\"\n",
            log.display()
        ),
    )
    .expect("write stub");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }

    // The real install path, not the stub: it is what upgrades the file.
    let mut tick = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    tick.args(["automation", "tick"]);
    sandboxed(&mut tick, &home, &bin);
    let out = tick.output().expect("run automation tick");
    assert!(
        out.status.success(),
        "automation tick failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let installed = std::fs::read_to_string(&plugin).expect("read installed plugin");
    assert_eq!(
        installed,
        include_str!("../extensions/hooks/opencode-status.js"),
        "the install path left the V1 plugin in place"
    );

    // 2.x reports only from a server of the session's own (see the manifest);
    // 1.x has no such flag and refuses it.
    let version = Command::new(&opencode)
        .arg("--version")
        .output()
        .expect("opencode --version");
    let v2 = String::from_utf8_lossy(&version.stdout).contains("v2.");
    let mut run = Command::new(&opencode);
    run.current_dir(&project).arg("run");
    if v2 {
        run.arg("--standalone");
    }
    run.args([
        "--print-logs",
        "-m",
        &model,
        "Reply with the single word: hi",
    ]);
    sandboxed(&mut run, &home, &bin);
    let out = run.output().expect("run opencode");
    let logs = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "opencode run failed:\n{logs}");
    assert!(
        !logs
            .lines()
            .any(|l| l.contains("failed to load plugin") && l.contains("thurbox-status")),
        "opencode refused the plugin:\n{logs}"
    );
    let states = std::fs::read_to_string(&log).unwrap_or_default();
    let states: Vec<&str> = states.lines().collect();
    assert!(
        states.contains(&"working"),
        "the turn never reported working: {states:?}\n{logs}"
    );
    assert_eq!(
        states.last(),
        Some(&"done"),
        "the turn never reported done: {states:?}\n{logs}"
    );
}
