//! `thurbox-cli doctor` — whether this machine has what a session needs
//! *before* anyone tries to create one.
//!
//! It reads; it never installs. Each `fail`/`warn` carries the one thing to do,
//! and where the answer depends on a distribution it names the package and
//! links the project's own install page rather than guessing an invocation.

use serde_json::{json, Value};

use crate::cli::output::CommandOutput;
use crate::cli::CommandError;

/// Whether a check found a problem, and how bad.
///
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }

    fn mark(self) -> &'static str {
        match self {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        }
    }
}

/// A single thing checked, and what was found.
struct Finding {
    /// Short stable key, so a script branches on the problem rather than
    /// parsing the sentence.
    key: String,
    level: Level,
    detail: String,
    /// The Fleet the check is of, when it is one Fleet's.
    fleet: Option<String>,
}

impl Finding {
    fn new(key: &str, level: Level, detail: String) -> Self {
        Self {
            key: key.into(),
            level,
            detail,
            fleet: None,
        }
    }
}

/// Report on this machine's readiness to run a session, and on each Fleet
/// `settings.toml` lists.
///
/// Exits non-zero only when something that must work does not: the Fleet the
/// interface uses, or, when a Fleet runs on this machine, the multiplexer. A
/// listed Fleet the interface does not use yet only warns. The agents are
/// Fleet's harnesses, reported with Fleet.
pub fn run(settings: &crate::session::settings::Settings) -> Result<CommandOutput, CommandError> {
    let listed = settings.fleets();
    let (in_force, _) = settings.fleet_in_force();
    let mut findings = Vec::new();
    let mut fleets = Vec::new();
    let mut fleet = Value::Null;
    for (name, def) in &listed {
        let facts = FleetFacts::read(&crate::fleet_link::FleetLink::of(name, def));
        let used = *name == in_force;
        findings.extend(facts.findings(used));
        let document = facts.document();
        if used {
            fleet = document.clone();
        }
        let mut entry = document;
        entry["transport"] = json!(def.transport);
        entry["in_force"] = json!(used);
        fleets.push(entry);
    }
    if listed.len() > 1 {
        findings.push(Finding::new(
            "fleets",
            Level::Warn,
            format!(
                "{} Fleets are listed; the interface uses {in_force}, the first by name, until it \
                 follows several",
                listed.len()
            ),
        ));
    }
    // Sessions of a Fleet on another machine run in that machine's
    // multiplexer: this one needs none unless a Fleet runs here.
    if listed.iter().any(|(_, def)| def.transport.is_local()) {
        findings.push(multiplexer_finding());
    }

    let verdict = findings.iter().map(|f| f.level).max().unwrap_or(Level::Ok);
    let human = render(&findings, verdict);
    let mut json = document(&findings, verdict);
    json["fleet"] = fleet;
    json["fleets"] = Value::Array(fleets);

    Ok(match verdict {
        Level::Fail => CommandOutput::failed(
            json,
            human,
            "this machine is missing something a session needs — see the FAIL checks above"
                .to_string(),
        ),
        _ => CommandOutput::new(json, human),
    })
}

/// Whether the multiplexer every local session's window is made of is there.
///
/// The one check that fails the report: without it nothing can be created at
/// all, so an `Unknown` here is treated as a failure too — the only way the
/// lookup answers that for a bare name is a `PATH` it could not read.
fn multiplexer_finding() -> Finding {
    let mux = crate::backend::preflight::local_multiplexer();
    match crate::backend::preflight::look_up(mux) {
        crate::backend::preflight::Presence::Present => Finding::new(
            "multiplexer",
            Level::Ok,
            format!("{mux} is installed{}", version_suffix(mux)),
        ),
        _ => Finding::new(
            "multiplexer",
            Level::Fail,
            crate::backend::preflight::Dependency::LocalMultiplexer.missing_summary(),
        ),
    }
}

fn render(findings: &[Finding], verdict: Level) -> String {
    let mut human = format!("This machine — {}\n", verdict.as_str().to_uppercase());
    for f in findings {
        let key = match &f.fleet {
            Some(fleet) => format!("{} ({fleet})", f.key),
            None => f.key.clone(),
        };
        human.push_str(&format!("  {}  {:<20} {}\n", f.level.mark(), key, f.detail));
    }
    let dirs = crate::paths::path_dirs();
    human.push_str(&format!("\nPATH searched ({}):\n", dirs.len()));
    for dir in &dirs {
        human.push_str(&format!("  {}\n", dir.display()));
    }
    human
}

/// The same report as one JSON document, for a script that branches on `key`
/// and `level` rather than parsing the sentences.
fn document(findings: &[Finding], verdict: Level) -> Value {
    json!({
        "verdict": verdict.as_str(),
        "multiplexer": crate::backend::preflight::local_multiplexer(),
        "path": crate::paths::path_dirs()
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        "checks": findings
            .iter()
            .map(|f| json!({
                "key": f.key,
                "level": f.level.as_str(),
                "detail": f.detail,
                "fleet": f.fleet,
            }))
            .collect::<Vec<Value>>(),
    })
}

/// ` (3.4)` when the multiplexer answers `-V`, empty when it does not.
///
/// Best-effort and never fatal: a version that cannot be read says nothing
/// about whether the binary works, and thurbox's own requirement (tmux >= 3.2)
/// is stated by the install advice rather than enforced here.
fn version_suffix(mux: &str) -> String {
    let Ok(out) = std::process::Command::new(mux).arg("-V").output() else {
        return String::new();
    };
    if !out.status.success() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    match text.split_whitespace().last() {
        Some(version) => format!(" ({version})"),
        None => String::new(),
    }
}

/// The API version thurbox writes its Tasks in: a Fleet that does not speak
/// it is refused (M6).
const TASK_API: &str = "fleet/v1alpha1";

/// What a Fleet, which manages sessions (ADR-41), answers about itself: the
/// program, its version and harness profiles, and the config and data
/// directory it reads. thurbox links no Fleet crate, so this is Fleet's
/// command line and its documented config rule.
pub(super) struct FleetFacts {
    /// The Fleet's name in `settings.toml`.
    pub(super) name: String,
    pub(super) program: String,
    /// Whether the Fleet runs on this machine: only then are its config,
    /// data directory and `runtime-mux` this machine's to check.
    local: bool,
    /// Why the machine the Fleet runs on could not be reached: ssh's own
    /// failure, exit 255.
    pub(super) unreachable: Option<String>,
    /// `fleet version --json`, or why it could not be read.
    pub(super) version: Result<serde_json::Value, String>,
    /// The config file Fleet reads, and what named it.
    pub(super) config: Option<(std::path::PathBuf, &'static str)>,
    /// `fleet runtime hosts --json`: the places a session can run in, or why
    /// they could not be read.
    pub(super) hosts: Result<serde_json::Value, String>,
}

impl FleetFacts {
    pub(super) fn read(link: &crate::fleet_link::FleetLink) -> Self {
        let program = link.program();
        // ssh's connect bound is inside the probe's: a slow handshake is not
        // a Fleet that stalls.
        let timeout = if link.is_local() {
            PROBE_TIMEOUT
        } else {
            REMOTE_PROBE_TIMEOUT
        };
        let answer = bounded_output(link, &["version", "--json"], timeout);
        let unreachable = match &answer {
            Ok(out) if link.is_ssh() && out.status.code() == Some(255) => {
                Some(String::from_utf8_lossy(&out.stderr).trim().to_string())
            }
            _ => None,
        };
        let version = match &unreachable {
            Some(why) => Err(format!("unreachable: {why}")),
            None => answer.and_then(|out| {
                if out.status.success() {
                    serde_json::from_slice(&out.stdout)
                        .map_err(|e| format!("{program} version --json answered no JSON: {e}"))
                } else {
                    Err(format!(
                        "{program} version --json: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ))
                }
            }),
        };
        // A machine that cannot be reached is asked once.
        let hosts = match &unreachable {
            Some(why) => Err(format!("unreachable: {why}")),
            None => {
                bounded_output(link, &["runtime", "hosts", "--json"], timeout).and_then(|out| {
                    if out.status.success() {
                        serde_json::from_slice(&out.stdout).map_err(|e| {
                            format!("{program} runtime hosts --json answered no JSON: {e}")
                        })
                    } else {
                        Err(format!(
                            "{program} runtime hosts --json: {}",
                            String::from_utf8_lossy(&out.stderr).trim()
                        ))
                    }
                })
            }
        };
        let config = match link.config() {
            Some(path) => Some((std::path::PathBuf::from(path), "settings.toml")),
            None if link.is_local() => fleet_config_path(),
            None => None,
        };
        Self {
            name: link.name().to_string(),
            program: program.to_string(),
            local: link.is_local(),
            unreachable,
            version,
            config,
            hosts,
        }
    }

    fn harnesses(&self) -> Vec<String> {
        self.version
            .as_ref()
            .ok()
            .and_then(|v| v["harnesses"].as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|h| h.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The Fleet data directory, for a Fleet on this machine: the config's
    /// `[data] dir`, relative to the config's own directory, else Fleet's
    /// per-user default.
    pub(super) fn data_dir(&self) -> Option<std::path::PathBuf> {
        if !self.local {
            return None;
        }
        let configured = self.config.as_ref().and_then(|(path, _)| {
            let text = std::fs::read_to_string(path).ok()?;
            let doc: toml::Table = toml::from_str(&text).ok()?;
            let dir = doc.get("data")?.get("dir")?.as_str()?.to_string();
            Some(path.parent()?.join(dir))
        });
        configured.or_else(|| {
            if cfg!(windows) {
                return std::env::var_os("LOCALAPPDATA")
                    .filter(|d| !d.is_empty())
                    .map(|d| std::path::PathBuf::from(d).join("fleet"));
            }
            std::env::var_os("XDG_DATA_HOME")
                .filter(|d| !d.is_empty())
                .map(|d| std::path::PathBuf::from(d).join("fleet"))
                .or_else(|| {
                    std::env::var_os("HOME")
                        .map(|h| std::path::PathBuf::from(h).join(".local/share/fleet"))
                })
        })
    }

    /// Why thurbox cannot write this Fleet's Tasks, when its answer lacks
    /// [`TASK_API`] among its `apiVersions`.
    fn incompatible(&self) -> Option<String> {
        let version = self.version.as_ref().ok()?;
        let apis: Vec<&str> = version["apiVersions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str())
            .collect();
        (!apis.contains(&TASK_API)).then(|| {
            format!(
                "{} {} speaks {}; thurbox writes {TASK_API} — update Fleet",
                self.program,
                version["version"].as_str().unwrap_or("?"),
                if apis.is_empty() {
                    "no API version".to_string()
                } else {
                    apis.join(", ")
                }
            )
        })
    }

    /// The checks of this Fleet. `in_force`: the interface uses it, so a
    /// Fleet that does not answer fails the report; any other only warns.
    fn findings(&self, in_force: bool) -> Vec<Finding> {
        let broken = if in_force { Level::Fail } else { Level::Warn };
        let name = &self.name;
        let mut findings = vec![match (&self.version, self.incompatible()) {
            (Ok(_), Some(why)) => Finding::new("fleet", broken, why),
            (Ok(v), None) => Finding::new(
                "fleet",
                Level::Ok,
                format!(
                    "{} {} manages sessions{}; harnesses: {}",
                    self.program,
                    v["version"].as_str().unwrap_or("?"),
                    if in_force { " (in use)" } else { "" },
                    self.harnesses().join(", ")
                ),
            ),
            (Err(why), _) if self.unreachable.is_some() => Finding::new(
                "fleet",
                broken,
                format!("{why} — the machine of [fleets.{name}] cannot be reached"),
            ),
            (Err(why), _) => Finding::new(
                "fleet",
                broken,
                format!(
                    "{why} — install Fleet there, or set [fleets.{name}] program in settings.toml"
                ),
            ),
        }];
        if self.unreachable.is_none() {
            findings.push(match &self.hosts {
                Ok(answer) => Finding::new(
                    "fleet-hosts",
                    Level::Ok,
                    match answer["hosts"].as_array() {
                        Some(hosts) => hosts
                            .iter()
                            .filter_map(|h| h.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        // `null`: one runtime serves any place.
                        None => "any place (one runtime serves them all)".into(),
                    },
                ),
                Err(why) => Finding::new("fleet-hosts", Level::Warn, why.clone()),
            });
        }
        if self.local {
            findings.extend(self.local_findings());
        } else {
            if let Some((path, from)) = &self.config {
                findings.push(Finding::new(
                    "fleet-config",
                    Level::Ok,
                    format!("{} ({from})", path.display()),
                ));
            }
            findings.extend(self.manager_finding(broken));
        }
        for finding in &mut findings {
            finding.fleet = Some(name.clone());
        }
        findings
    }

    /// Whether the manager of a Fleet on another machine runs, from the
    /// `manager` of its `fleet version --json`. thurbox never starts that
    /// manager (M4): a user systemd unit there runs `fleet up`. A Fleet that
    /// did not answer has no such row: its `fleet` row says why.
    fn manager_finding(&self, broken: Level) -> Option<Finding> {
        let manager = &self.version.as_ref().ok()?["manager"];
        Some(match manager["held"].as_bool() {
            Some(true) => Finding::new(
                "fleet-manager",
                Level::Ok,
                format!(
                    "manager running ({})",
                    manager["holder"].as_str().unwrap_or("?")
                ),
            ),
            Some(false) => Finding::new(
                "fleet-manager",
                broken,
                "manager not running — see the systemd unit in docs/where-sessions-run.md".into(),
            ),
            None => Finding::new(
                "fleet-manager",
                Level::Warn,
                format!(
                    "whether the manager runs is unknown: {}",
                    manager["why"]
                        .as_str()
                        .unwrap_or("this Fleet does not report its manager — update Fleet")
                ),
            ),
        })
    }

    /// The checks only a Fleet on this machine has: its config file and the
    /// runtime it runs here.
    fn local_findings(&self) -> Vec<Finding> {
        let config = match &self.config {
            Some((path, from)) if path.is_file() => Finding::new(
                "fleet-config",
                Level::Ok,
                format!("{} ({from})", path.display()),
            ),
            Some((path, from)) => Finding::new(
                "fleet-config",
                Level::Warn,
                format!(
                    "{} ({from}) does not exist — run `{} init`",
                    path.display(),
                    self.program
                ),
            ),
            None => Finding::new(
                "fleet-config",
                Level::Warn,
                "no Fleet config path: set FLEET_CONFIG".into(),
            ),
        };
        let mux = "runtime-mux";
        let runtime = match crate::backend::preflight::look_up(mux) {
            crate::backend::preflight::Presence::Missing => Finding::new(
                mux,
                Level::Warn,
                format!(
                    "{mux} is not on PATH — Fleet's local runtime runs it unless its config \
                     names another program"
                ),
            ),
            _ => Finding::new(mux, Level::Ok, format!("{mux} is installed")),
        };
        vec![config, runtime]
    }

    /// The facts as one JSON object, for `doctor`, `config show` and
    /// `version --json`.
    pub(super) fn document(&self) -> Value {
        json!({
            "name": self.name,
            "program": self.program,
            "version": self.version.as_ref().ok().and_then(|v| v["version"].as_str()),
            "harnesses": self.harnesses(),
            "config": self.config.as_ref().map(|(path, _)| path.display().to_string()),
            "config_from": self.config.as_ref().map(|(_, from)| *from),
            "data_dir": self.data_dir().map(|d| d.display().to_string()),
            "unreachable": self.unreachable,
            "error": self.version.as_ref().err(),
        })
    }
}

/// How long `fleet version --json` may take: a wrapper that stalls must not
/// hang `doctor`, `config show` or `version`.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// [`PROBE_TIMEOUT`] for a Fleet on another machine: ssh's own
/// `ConnectTimeout` (5 s) comes first, and a first connection also sets up
/// the shared master.
const REMOTE_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// `fleet args`'s output, or why there is none: it could not start, or it
/// ran past `timeout` and was stopped.
fn bounded_output(
    link: &crate::fleet_link::FleetLink,
    args: &[&str],
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    use std::process::Stdio;
    let program = link.program();
    let mut child = link
        .command(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {program}: {e}"))?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| format!("{program}: {e}"))
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} {} did not answer within {}s",
                    args.join(" "),
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(e) => return Err(format!("{program}: {e}")),
        }
    }
}

/// The config file Fleet reads, by its documented rule (docs/fleet): the file
/// `FLEET_CONFIG` names, else the native per-user one.
fn fleet_config_path() -> Option<(std::path::PathBuf, &'static str)> {
    if let Some(path) = std::env::var_os("FLEET_CONFIG").filter(|p| !p.is_empty()) {
        return Some((std::path::PathBuf::from(path), "FLEET_CONFIG"));
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA")
            .filter(|d| !d.is_empty())?
            .into()
    } else if let Some(d) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        std::path::PathBuf::from(d)
    } else {
        std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config")
    };
    Some((base.join("fleet/config.toml"), "the per-user file"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// `doctor`'s probes reach Fleet through its link.
    #[test]
    fn doctor_probes_fleet_through_its_link() {
        let remote = crate::fleet_link::testing::Remote::new(
            "case \"$1\" in\n\
             runtime) echo '{\"hosts\":[\"local\"]}' ;;\n\
             *) echo '{\"version\":\"9.9.9\",\"harnesses\":[\"claude\"]}' ;;\n\
             esac",
        );
        let facts = crate::paths::with_path(remote.path(), || FleetFacts::read(&remote.link));
        assert_eq!(facts.version.expect("version")["version"], "9.9.9");
        assert_eq!(facts.hosts.expect("hosts")["hosts"], json!(["local"]));
        assert!(remote.called("version --json"), "{:?}", remote.calls());
        assert!(
            remote.called("runtime hosts --json"),
            "{:?}",
            remote.calls()
        );
    }

    /// A remote Fleet too old to report its manager is a warning to update
    /// it, never "not running".
    #[test]
    fn doctor_warns_when_a_remote_fleet_does_not_report_its_manager() {
        let remote = crate::fleet_link::testing::Remote::new(
            "case \"$1\" in\n\
             runtime) echo '{\"hosts\":[\"local\"]}' ;;\n\
             *) echo '{\"version\":\"9.9.9\",\"apiVersions\":[\"fleet/v1alpha1\"],\"harnesses\":[]}' ;;\n\
             esac",
        );
        let facts = crate::paths::with_path(remote.path(), || FleetFacts::read(&remote.link));
        let findings = facts.findings(true);
        let manager = findings
            .iter()
            .find(|f| f.key == "fleet-manager")
            .expect("a fleet-manager row");
        assert_eq!(manager.level, Level::Warn, "{}", manager.detail);
        assert!(
            manager
                .detail
                .contains("does not report its manager — update Fleet"),
            "{}",
            manager.detail
        );
    }

    /// `doctor` lists the hosts Fleet defines, which are where a session can
    /// run, instead of thurbox's own `hosts.toml`.
    #[cfg(unix)]
    #[test]
    fn doctor_reports_fleets_hosts() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fleet = dir.path().join("fleet");
        std::fs::write(
            &fleet,
            "#!/bin/sh\ncase \"$1\" in\n  runtime) echo '{\"hosts\":[\"local\",\"devbox\"]}' ;;\n  *) echo '{\"version\":\"9.9.9\",\"apiVersions\":[\"fleet/v1alpha1\"],\"harnesses\":[]}' ;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).unwrap();
        let findings = FleetFacts::read(&crate::fleet_link::FleetLink::local(
            fleet.to_string_lossy(),
        ))
        .findings(true);
        let hosts = findings
            .iter()
            .find(|f| f.key == "fleet-hosts")
            .expect("a fleet-hosts row");
        assert_eq!(hosts.level, Level::Ok, "{}", hosts.detail);
        assert!(hosts.detail.contains("local, devbox"), "{}", hosts.detail);
    }

    /// A Fleet whose `apiVersions` lack the one thurbox writes its Tasks in
    /// is refused by name (M6).
    #[test]
    fn doctor_refuses_a_fleet_that_does_not_speak_the_task_api() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fleet = dir.path().join("fleet");
        std::fs::write(
            &fleet,
            "#!/bin/sh\necho '{\"version\":\"9.0.0\",\"apiVersions\":[\"fleet/v2\"],\"harnesses\":[]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).unwrap();
        let facts = FleetFacts::read(&crate::fleet_link::FleetLink::local(
            fleet.to_string_lossy(),
        ));
        let findings = facts.findings(true);
        assert_eq!(findings[0].level, Level::Fail, "{}", findings[0].detail);
        assert!(
            findings[0]
                .detail
                .contains("speaks fleet/v2; thurbox writes fleet/v1alpha1"),
            "{}",
            findings[0].detail
        );
    }

    /// `doctor` names the Fleet that runs every session, its harnesses and
    /// the config it reads.
    #[cfg(unix)]
    #[test]
    fn doctor_reports_the_fleet_in_force() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fleet = dir.path().join("fleet");
        std::fs::write(
            &fleet,
            "#!/bin/sh\necho '{\"version\":\"9.9.9\",\"apiVersions\":[\"fleet/v1alpha1\"],\"harnesses\":[\"claude-code\",\"codex\"]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[data]\ndir = \"store\"\n").unwrap();
        std::env::set_var("FLEET_CONFIG", &config);

        let facts = FleetFacts::read(&crate::fleet_link::FleetLink::local(
            fleet.to_string_lossy(),
        ));
        let findings = facts.findings(true);
        assert_eq!(findings[0].level, Level::Ok, "{}", findings[0].detail);
        assert!(
            findings[0].detail.contains("9.9.9"),
            "{}",
            findings[0].detail
        );
        assert!(findings[0].detail.contains("claude-code, codex"));
        let config = findings
            .iter()
            .find(|f| f.key == "fleet-config")
            .expect("a fleet-config row");
        assert_eq!(config.level, Level::Ok, "{}", config.detail);
        assert!(config.detail.contains("FLEET_CONFIG"));
        assert_eq!(
            facts.document()["data_dir"],
            json!(dir.path().join("store"))
        );

        let gone = FleetFacts::read(&crate::fleet_link::FleetLink::local(
            "definitely-not-a-fleet-xyz",
        ));
        assert_eq!(gone.findings(true)[0].level, Level::Fail);
        // A listed Fleet the interface does not use only warns.
        assert_eq!(gone.findings(false)[0].level, Level::Warn);

        // A Fleet that never answers is stopped and reported, not waited on.
        let stuck = dir.path().join("stuck");
        std::fs::write(&stuck, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let out = bounded_output(
            &crate::fleet_link::FleetLink::local(stuck.to_string_lossy()),
            &["version"],
            std::time::Duration::from_millis(300),
        );
        assert!(out.unwrap_err().contains("did not answer"));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
