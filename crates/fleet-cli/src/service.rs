//! `fleet service`: the one systemd user unit that runs `fleet up` for a
//! store. Fleet writes the unit and drives it through `systemctl --user`.
//! The unit names its store in `X-Fleet-Store`, a key systemd keeps and
//! ignores, so Fleet finds the unit of a store without a record of its own.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleet_exec::{Outcome, Spec};

pub const DEFAULT_NAME: &str = "fleet";
const STORE_KEY: &str = "X-Fleet-Store=";
const QUICK_BOUND: Duration = Duration::from_secs(30);

pub struct Unit {
    pub name: String,
    pub path: PathBuf,
}

pub struct Install<'a> {
    pub name: &'a str,
    pub exe: &'a Path,
    pub config: &'a Path,
    pub data_dir: &'a Path,
    pub path_env: OsString,
    pub stop_timeout: Duration,
    pub now: bool,
}

pub enum Refusal {
    Unsupported,
    Invalid(String),
    Taken(Unit),
    Foreign { unit: String, store: String },
    NotInstalled,
    Systemctl(String),
    StillRunning(String),
    Io(String),
}

pub fn unit_name(name: &str) -> Result<String, Refusal> {
    let base = name.strip_suffix(".service").unwrap_or(name);
    let allowed = |c: char| c.is_ascii_alphanumeric() || "-_.@:".contains(c);
    if base.is_empty() || !base.chars().all(allowed) {
        return Err(Refusal::Invalid(format!(
            "--name {name:?}: a unit name holds letters, digits and - _ . @ : only"
        )));
    }
    Ok(format!("{base}.service"))
}

fn unit_dir() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("systemd/user"))
}

fn unit_store(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(STORE_KEY))
        .map(str::to_string)
}

fn same_store(a: &Path, b: &Path) -> bool {
    let resolve = |p: &Path| {
        std::fs::canonicalize(p)
            .or_else(|_| std::path::absolute(p))
            .unwrap_or_else(|_| p.to_path_buf())
    };
    resolve(a) == resolve(b)
}

pub fn store_unit(data_dir: &Path) -> Result<Option<Unit>, String> {
    if !cfg!(target_os = "linux") {
        return Ok(None);
    }
    let Some(dir) = unit_dir() else {
        return Ok(None);
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut found: Vec<Unit> = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".service") || !path.is_file() {
            continue;
        }
        if unit_store(&path).is_some_and(|s| same_store(Path::new(&s), data_dir)) {
            found.push(Unit {
                name: name.to_string(),
                path,
            });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found.into_iter().next())
}

pub fn target(name: Option<&str>, data_dir: &Path) -> Result<Unit, Refusal> {
    supported()?;
    let unit = match name {
        Some(name) => {
            let name = unit_name(name)?;
            let dir = unit_dir().ok_or(Refusal::NotInstalled)?;
            Unit {
                path: dir.join(&name),
                name,
            }
        }
        None => store_unit(data_dir)
            .map_err(Refusal::Io)?
            .ok_or(Refusal::NotInstalled)?,
    };
    if unit.path.is_file() {
        Ok(unit)
    } else {
        Err(Refusal::NotInstalled)
    }
}

fn quoted(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

fn printable(path: &Path, what: &str) -> Result<String, Refusal> {
    let text = path
        .to_str()
        .ok_or_else(|| Refusal::Invalid(format!("{what} {}: not UTF-8", path.display())))?;
    if text.chars().any(char::is_control) {
        return Err(Refusal::Invalid(format!(
            "{what} {text:?}: a unit file cannot name a path with a control character"
        )));
    }
    Ok(text.to_string())
}

fn render(install: &Install) -> Result<String, Refusal> {
    let exe = printable(install.exe, "fleet")?;
    let config = printable(install.config, "config")?;
    let store = printable(install.data_dir, "data directory")?;
    let mut path_env = install
        .exe
        .parent()
        .map(|d| d.as_os_str().to_os_string())
        .unwrap_or_default();
    if !install.path_env.is_empty() {
        path_env.push(":");
        path_env.push(&install.path_env);
    }
    let path_env = printable(Path::new(&path_env), "PATH")?;
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
        name = unit_name(install.name)?,
        fleet_config = quoted(&format!("FLEET_CONFIG={config}")),
        path = quoted(&format!("PATH={path_env}")),
    ))
}

pub fn install(install: &Install) -> Result<Unit, Refusal> {
    supported()?;
    let name = unit_name(install.name)?;
    if let Some(other) = store_unit(install.data_dir).map_err(Refusal::Io)?
        && other.name != name
    {
        return Err(Refusal::Taken(other));
    }
    let dir = unit_dir().ok_or_else(|| Refusal::Io("no HOME or XDG_CONFIG_HOME".into()))?;
    let text = render(install)?;
    std::fs::create_dir_all(&dir).map_err(|e| Refusal::Io(format!("{}: {e}", dir.display())))?;
    let path = dir.join(&name);
    if let Some(store) = unit_store(&path).filter(|s| !same_store(Path::new(s), install.data_dir)) {
        return Err(Refusal::Foreign { unit: name, store });
    }
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| Refusal::Io(format!("{}: {e}", path.display())))?;
    systemctl(&["daemon-reload"], QUICK_BOUND)?;
    if install.now {
        systemctl(&["enable", "--now", "--no-block", &name], QUICK_BOUND)?;
    } else {
        systemctl(&["enable", &name], QUICK_BOUND)?;
    }
    Ok(Unit { name, path })
}

pub fn uninstall(unit: &Unit, stop_bound: Duration) -> Result<(), Refusal> {
    supported()?;
    systemctl(&["disable", "--now", &unit.name], stop_bound)?;
    std::fs::remove_file(&unit.path)
        .map_err(|e| Refusal::Io(format!("{}: {e}", unit.path.display())))?;
    systemctl(&["daemon-reload"], QUICK_BOUND).map(drop)
}

pub enum Verb {
    Start,
    Stop,
    Restart,
}

pub fn drive(unit: &Unit, verb: Verb, stop_bound: Duration) -> Result<(), Refusal> {
    supported()?;
    let name = unit.name.as_str();
    match verb {
        Verb::Start => systemctl(&["start", "--no-block", name], QUICK_BOUND),
        Verb::Stop => systemctl(&["stop", name], stop_bound),
        Verb::Restart => systemctl(&["restart", "--no-block", name], QUICK_BOUND),
    }
    .map(drop)
}

/// `systemctl --user status`: its text, and its exit code (0 active, 3
/// inactive).
pub fn status(unit: &Unit) -> Result<(String, i32), Refusal> {
    supported()?;
    match run(&["status", &unit.name, "--no-pager"], QUICK_BOUND) {
        Outcome::Exited {
            code: Some(code),
            stdout,
            ..
        } => Ok((String::from_utf8_lossy(&stdout).into_owned(), code)),
        other => Err(refusal(&other)),
    }
}

fn supported() -> Result<(), Refusal> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        Err(Refusal::Unsupported)
    }
}

fn run(args: &[&str], bound: Duration) -> Outcome {
    fleet_exec::run(&Spec::new("systemctl", bound).arg("--user").args(args))
}

fn systemctl(args: &[&str], bound: Duration) -> Result<String, Refusal> {
    let outcome = run(args, bound);
    outcome.success_text().ok_or_else(|| match outcome {
        Outcome::TimedOut { .. } => Refusal::StillRunning(format!(
            "systemctl --user {} did not answer within {}s",
            args.join(" "),
            bound.as_secs()
        )),
        other => refusal(&other),
    })
}

fn refusal(outcome: &Outcome) -> Refusal {
    let stderr = match outcome {
        Outcome::Exited { stderr, .. } => String::from_utf8_lossy(stderr).into_owned(),
        _ => String::new(),
    };
    let why = match stderr.trim() {
        "" => outcome.failure(),
        text => format!("{}: {text}", outcome.failure()),
    };
    Refusal::Systemctl(why)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_with_quotes_percent_and_dollar_stays_one_word() {
        let unit = render(&Install {
            name: "fleet",
            exe: Path::new("/opt/a \"b\" 100%/$x/fleet"),
            config: Path::new("/c/%h.toml"),
            data_dir: Path::new("/d"),
            path_env: OsString::from("/usr/bin"),
            stop_timeout: Duration::from_millis(30_500),
            now: false,
        })
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
