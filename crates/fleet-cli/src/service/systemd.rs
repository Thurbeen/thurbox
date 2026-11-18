//! A systemd user unit, driven through `systemctl --user`. The unit names
//! its store in `X-Fleet-Store`, a key systemd keeps and ignores.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Install, QUICK_BOUND, Refusal, STORE_KEY, State, Tool, Unit, Verb};

pub const KIND: &str = "systemd";
const SYSTEMCTL: Tool = Tool {
    program: "systemctl",
    lead: &["--user"],
};

pub fn unit_name(name: &str) -> Result<String, Refusal> {
    super::valid_name(name, ".service", "@:").map(|base| format!("{base}.service"))
}

pub fn file_of_unit(unit: &str) -> String {
    unit.to_string()
}

pub fn unit_of_file(file: &str) -> Option<String> {
    file.ends_with(".service").then(|| file.to_string())
}

pub fn unit_dir() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("systemd/user"))
}

pub fn unit_store(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(STORE_KEY))
        .map(str::to_string)
}

fn quoted(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

pub fn render(install: &Install, name: &str) -> Result<Vec<u8>, Refusal> {
    let exe = super::printable(install.exe, "fleet")?;
    let config = super::printable(install.config, "config")?;
    let store = super::printable(install.data_dir, "data directory")?;
    let path_env = super::search_path(install, ":")?;
    let exec = quoted(&exe).replace('$', "$$");
    let stop = install.stop_timeout.as_millis().div_ceil(1000).max(1);
    Ok(format!(
        "# Written by `fleet service install`: it rewrites this file.\n\
         # Put local changes in a drop-in: systemctl --user edit {name}\n\
         [Unit]\n\
         Description=Fleet manager (fleet up)\n\
         {STORE_KEY}{store}\n\
         \n\
         [Service]\n\
         Type=notify\n\
         Environment={fleet_config}\n\
         Environment={path}\n\
         ExecStart={exec} --config ${{FLEET_CONFIG}} up\n\
         Restart=on-failure\n\
         RestartSec=2s\n\
         RestartSteps=6\n\
         RestartMaxDelaySec=2min\n\
         RestartPreventExitStatus=78\n\
         TimeoutStartSec=infinity\n\
         TimeoutStopSec={stop}s\n\
         KillMode=process\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        fleet_config = quoted(&format!("FLEET_CONFIG={config}")),
        path = quoted(&format!("PATH={path_env}")),
    )
    .into_bytes())
}

pub fn register(unit: &Unit, now: bool) -> Result<(), Refusal> {
    SYSTEMCTL.text(&["daemon-reload"], QUICK_BOUND)?;
    if now {
        SYSTEMCTL.text(&["enable", "--now", "--no-block", &unit.name], QUICK_BOUND)?;
    } else {
        SYSTEMCTL.text(&["enable", &unit.name], QUICK_BOUND)?;
    }
    Ok(())
}

pub fn deregister(unit: &Unit, stop_bound: Duration) -> Result<(), Refusal> {
    SYSTEMCTL
        .text(&["disable", "--now", &unit.name], stop_bound)
        .map(drop)
}

pub fn forget(_unit: &Unit) -> Result<(), Refusal> {
    SYSTEMCTL.text(&["daemon-reload"], QUICK_BOUND).map(drop)
}

pub fn state(unit: &Unit) -> Result<State, Refusal> {
    let text = SYSTEMCTL.text(
        &[
            "show",
            "-p",
            "ActiveState",
            "-p",
            "Result",
            "-p",
            "ExecMainStatus",
            &unit.name,
        ],
        QUICK_BOUND,
    )?;
    let value = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or_default()
            .to_string()
    };
    let active = value("ActiveState");
    Ok(State {
        running: matches!(active.as_str(), "active" | "activating" | "reloading"),
        failed: (active == "failed")
            .then(|| format!("{}, status {}", value("Result"), value("ExecMainStatus"))),
    })
}

pub fn try_restart(unit: &Unit) -> Result<(), Refusal> {
    SYSTEMCTL
        .text(&["try-restart", "--no-block", &unit.name], QUICK_BOUND)
        .map(drop)
}

pub fn drive(unit: &Unit, verb: Verb, stop_bound: Duration) -> Result<(), Refusal> {
    let name = unit.name.as_str();
    match verb {
        Verb::Start => SYSTEMCTL.text(&["start", "--no-block", name], QUICK_BOUND),
        Verb::Stop => SYSTEMCTL.text(&["stop", name], stop_bound),
        Verb::Restart => SYSTEMCTL.text(&["restart", "--no-block", name], QUICK_BOUND),
    }
    .map(drop)
}

/// `systemctl --user status`: its text, and its exit code (0 active, 3
/// inactive).
pub fn status(unit: &Unit) -> Result<(String, i32), Refusal> {
    SYSTEMCTL.report(&["status", &unit.name, "--no-pager"])
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn a_path_with_quotes_percent_and_dollar_stays_one_word() {
        let unit = render(
            &Install {
                name: "fleet",
                exe: Path::new("/opt/a \"b\" 100%/$x/fleet"),
                config: Path::new("/c/%h.toml"),
                data_dir: Path::new("/d"),
                path_env: OsString::from("/usr/bin"),
                stop_timeout: Duration::from_millis(30_500),
                now: false,
            },
            "fleet.service",
        )
        .map(|b| String::from_utf8(b).unwrap_or_default())
        .unwrap_or_else(|_| panic!("render"));
        assert!(unit.contains("ExecStart=\"/opt/a \\\"b\\\" 100%%/$$x/fleet\" --config"));
        assert!(unit.contains("Environment=\"FLEET_CONFIG=/c/%%h.toml\"\n"));
        assert!(unit.contains("Environment=\"PATH=/opt/a \\\"b\\\" 100%%/$x:/usr/bin\"\n"));
        assert!(unit.contains("TimeoutStopSec=31s\n"));
    }

    #[test]
    fn a_unit_name_takes_its_suffix_once_and_no_path() {
        assert_eq!(unit_name("fleet").ok(), Some("fleet.service".into()));
        assert_eq!(unit_name("a@b.service").ok(), Some("a@b.service".into()));
        assert!(unit_name("../x").is_err());
        assert!(unit_name("").is_err());
    }
}
