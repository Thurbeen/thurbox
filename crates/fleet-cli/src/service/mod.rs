//! `fleet service`: the one user service that runs `fleet up` for a store,
//! under this system's own supervisor: a systemd user unit on Linux, a
//! launchd agent on macOS, a Task Scheduler task on Windows. Fleet writes
//! the service's file, which names its store, so Fleet finds the service of
//! a store without a record of its own.

#[cfg(unix)]
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fleet_exec::{Outcome, Spec};
use fleet_store::FileStore;
use fleet_store::manager::ManagerState;

#[cfg(target_os = "macos")]
mod launchd;
#[cfg(windows)]
mod schtasks;
#[cfg(target_os = "linux")]
mod systemd;
#[cfg(any(target_os = "macos", windows))]
mod xml;

#[cfg(target_os = "macos")]
use launchd as host;
#[cfg(windows)]
use schtasks as host;
#[cfg(target_os = "linux")]
use systemd as host;

pub const DEFAULT_NAME: &str = "fleet";
#[cfg(any(target_os = "linux", windows))]
const STORE_KEY: &str = "X-Fleet-Store=";
const QUICK_BOUND: Duration = Duration::from_secs(30);

pub const KIND: &str = host::KIND;

pub struct Unit {
    pub name: String,
    pub path: PathBuf,
}

pub struct Install<'a> {
    pub name: &'a str,
    pub exe: &'a Path,
    pub config: &'a Path,
    pub data_dir: &'a Path,
    #[cfg(unix)]
    pub path_env: OsString,
    #[cfg(unix)]
    pub stop_timeout: Duration,
    pub now: bool,
}

pub enum Refusal {
    Invalid(String),
    Taken(Unit),
    Foreign { unit: String, store: String },
    NotInstalled,
    Supervisor(String),
    StillRunning(String),
    Stopped(String),
    Io(String),
}

struct State {
    running: bool,
    failed: Option<String>,
}

fn valid_name(name: &str, suffix: &str, extra: &str) -> Result<String, Refusal> {
    let base = name.strip_suffix(suffix).unwrap_or(name);
    let allowed = |c: char| c.is_ascii_alphanumeric() || "-_.".contains(c) || extra.contains(c);
    if base.is_empty() || !base.chars().all(allowed) {
        return Err(Refusal::Invalid(format!(
            "--name {name:?}: a service name holds letters, digits and - _ . {extra} only"
        )));
    }
    Ok(base.to_string())
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
    let Some(dir) = host::unit_dir() else {
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
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(host::unit_of_file)
        else {
            continue;
        };
        if path.is_file()
            && host::unit_store(&path).is_some_and(|s| same_store(Path::new(&s), data_dir))
        {
            found.push(Unit { name, path });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found.into_iter().next())
}

pub fn target(name: Option<&str>, data_dir: &Path) -> Result<Unit, Refusal> {
    let unit = match name {
        Some(name) => {
            let name = host::unit_name(name)?;
            let dir = host::unit_dir().ok_or(Refusal::NotInstalled)?;
            Unit {
                path: dir.join(host::file_of_unit(&name)),
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

fn printable(path: &Path, what: &str) -> Result<String, Refusal> {
    let text = path
        .to_str()
        .ok_or_else(|| Refusal::Invalid(format!("{what} {}: not UTF-8", path.display())))?;
    if text.chars().any(char::is_control) {
        return Err(Refusal::Invalid(format!(
            "{what} {text:?}: a service file cannot name a path with a control character"
        )));
    }
    Ok(text.to_string())
}

#[cfg(unix)]
fn search_path(install: &Install, separator: &str) -> Result<String, Refusal> {
    let mut path_env = install
        .exe
        .parent()
        .map(|d| d.as_os_str().to_os_string())
        .unwrap_or_default();
    if !install.path_env.is_empty() {
        path_env.push(separator);
        path_env.push(&install.path_env);
    }
    printable(Path::new(&path_env), "PATH")
}

pub fn install(install: &Install) -> Result<Unit, Refusal> {
    let name = host::unit_name(install.name)?;
    if let Some(other) = store_unit(install.data_dir).map_err(Refusal::Io)?
        && other.name != name
    {
        return Err(Refusal::Taken(other));
    }
    let dir = host::unit_dir().ok_or_else(|| Refusal::Io("no home directory".into()))?;
    let bytes = host::render(install, &name)?;
    std::fs::create_dir_all(&dir).map_err(|e| Refusal::Io(format!("{}: {e}", dir.display())))?;
    let path = dir.join(host::file_of_unit(&name));
    if let Some(store) =
        host::unit_store(&path).filter(|s| !same_store(Path::new(s), install.data_dir))
    {
        return Err(Refusal::Foreign { unit: name, store });
    }
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| Refusal::Io(format!("{}: {e}", path.display())))?;
    let unit = Unit { name, path };
    host::register(&unit, install.now)?;
    Ok(unit)
}

pub fn uninstall(unit: &Unit, stop_bound: Duration) -> Result<(), Refusal> {
    host::deregister(unit, stop_bound)?;
    std::fs::remove_file(&unit.path)
        .map_err(|e| Refusal::Io(format!("{}: {e}", unit.path.display())))?;
    host::forget(unit)
}

pub fn stable_exe(exe: &Path) -> PathBuf {
    let release_home = || {
        let bin = exe.parent()?;
        let releases = bin.parent()?.parent()?;
        (bin.file_name()? == "bin" && releases.file_name()? == "releases")
            .then(|| releases.parent())
            .flatten()
    };
    release_home()
        .map(|home| {
            home.join("current/bin")
                .join(exe.file_name().unwrap_or_default())
        })
        .filter(|current| current.is_file())
        .unwrap_or_else(|| exe.to_path_buf())
}

pub enum Restart {
    Serving,
    NotRunning,
}

pub fn restart_expecting(
    unit: &Unit,
    data_dir: &Path,
    version: &str,
    bound: Duration,
) -> Result<Restart, Refusal> {
    let name = unit.name.as_str();
    if !host::state(unit)?.running {
        return Ok(Restart::NotRunning);
    }
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let deadline = Instant::now() + bound;
    host::try_restart(unit)?;
    let mut seen = "no manager record".to_string();
    loop {
        if let Ok(Some(record)) = FileStore::peek_manager(data_dir) {
            if record.started_at >= since
                && record.state == ManagerState::Serving
                && record.version == version
            {
                return Ok(Restart::Serving);
            }
            seen = format!(
                "the last manager record names {} ({:?})",
                record.version, record.state
            );
        }
        if let Some(why) = host::state(unit)?.failed {
            return Err(Refusal::Stopped(format!(
                "{name} stopped before it served {version}: {why}"
            )));
        }
        if Instant::now() >= deadline {
            return Err(Refusal::StillRunning(format!(
                "{name} did not serve {version} within {}s; {seen}",
                bound.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

pub enum Verb {
    Start,
    Stop,
    Restart,
}

pub fn drive(unit: &Unit, verb: Verb, stop_bound: Duration) -> Result<(), Refusal> {
    host::drive(unit, verb, stop_bound)
}

/// The supervisor's own report on the service, and an exit code: 0 while
/// it runs, 3 when it does not.
pub fn status(unit: &Unit) -> Result<(String, i32), Refusal> {
    host::status(unit)
}

struct Tool {
    program: &'static str,
    lead: &'static [&'static str],
}

impl Tool {
    fn spec(&self, args: &[&str], bound: Duration) -> Spec {
        let mut spec = Spec::new(self.program, bound).args(self.lead).args(args);
        // Windows looks a bare program up in System32 before the parent's
        // PATH, and in a PATH set on the child first: setting it keeps the
        // lookup to PATH, as on Unix.
        if let Some(path) = std::env::var_os("PATH").and_then(|p| p.into_string().ok()) {
            spec.env.push(("PATH".into(), path));
        }
        spec
    }

    fn run(&self, args: &[&str], bound: Duration) -> Outcome {
        fleet_exec::run(&self.spec(args, bound))
    }

    fn text(&self, args: &[&str], bound: Duration) -> Result<String, Refusal> {
        let outcome = self.run(args, bound);
        outcome.success_text().ok_or_else(|| match outcome {
            Outcome::TimedOut { .. } => Refusal::StillRunning(format!(
                "{} {} did not answer within {}s",
                self.name(),
                args.join(" "),
                bound.as_secs()
            )),
            other => self.refusal(&other),
        })
    }

    fn name(&self) -> String {
        std::iter::once(self.program)
            .chain(self.lead.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn refusal(&self, outcome: &Outcome) -> Refusal {
        let stderr = match outcome {
            Outcome::Exited { stderr, .. } => String::from_utf8_lossy(stderr).into_owned(),
            _ => String::new(),
        };
        let why = match stderr.trim() {
            "" => outcome.failure(),
            text => format!("{}: {text}", outcome.failure()),
        };
        Refusal::Supervisor(format!("{} {why}", self.name()))
    }

    fn report(&self, args: &[&str]) -> Result<(String, i32), Refusal> {
        match self.run(args, QUICK_BOUND) {
            Outcome::Exited {
                code: Some(code),
                stdout,
                ..
            } => Ok((String::from_utf8_lossy(&stdout).into_owned(), code)),
            other => Err(self.refusal(&other)),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_unit_runs_the_current_release_not_the_one_it_was_installed_from() {
        let root = std::env::temp_dir().join(format!("fleet-stable-exe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let release = root.join("lib/thurbox/releases/1.2.3/bin");
        std::fs::create_dir_all(&release).unwrap_or_else(|e| panic!("{e}"));
        let exe = release.join("fleet");
        std::fs::write(&exe, "").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(stable_exe(&exe), exe);

        std::os::unix::fs::symlink("releases/1.2.3", root.join("lib/thurbox/current"))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(stable_exe(&exe), root.join("lib/thurbox/current/bin/fleet"));
        assert_eq!(
            stable_exe(Path::new("/usr/bin/fleet")),
            Path::new("/usr/bin/fleet")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
