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
}

/// Report on this machine's readiness to run a session.
///
/// Exits non-zero only when something that must work does not: no Fleet, or
/// no multiplexer. The agents are Fleet's harnesses, reported with Fleet.
pub fn run(fleet_program: &str) -> Result<CommandOutput, CommandError> {
    let fleet = FleetFacts::read(fleet_program);
    let mut findings = fleet.findings();
    findings.push(multiplexer_finding());

    let verdict = findings.iter().map(|f| f.level).max().unwrap_or(Level::Ok);
    let human = render(&findings, verdict);
    let mut json = document(&findings, verdict);
    json["fleet"] = fleet.document();

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
        crate::backend::preflight::Presence::Present => Finding {
            key: "multiplexer".into(),
            level: Level::Ok,
            detail: format!("{mux} is installed{}", version_suffix(mux)),
        },
        _ => Finding {
            key: "multiplexer".into(),
            level: Level::Fail,
            detail: crate::backend::preflight::Dependency::LocalMultiplexer.missing_summary(),
        },
    }
}

fn render(findings: &[Finding], verdict: Level) -> String {
    let mut human = format!("This machine — {}\n", verdict.as_str().to_uppercase());
    for f in findings {
        human.push_str(&format!(
            "  {}  {:<20} {}\n",
            f.level.mark(),
            f.key,
            f.detail
        ));
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

/// What Fleet, which runs every session (ADR-41), answers about itself on
/// this machine: the program, its version and harness profiles, and the config
/// and data directory it reads. thurbox links no Fleet crate, so this is
/// Fleet's command line and its documented config rule.
pub(super) struct FleetFacts {
    pub(super) program: String,
    /// `fleet version --json`, or why it could not be read.
    pub(super) version: Result<serde_json::Value, String>,
    /// The config file Fleet reads, and what named it.
    pub(super) config: Option<(std::path::PathBuf, &'static str)>,
    /// `fleet runtime hosts --json`: the hosts a session can run on, or why
    /// they could not be read.
    pub(super) hosts: Result<serde_json::Value, String>,
}

impl FleetFacts {
    pub(super) fn read(program: &str) -> Self {
        Self::read_through(&crate::fleet_link::FleetLink::local(program))
    }

    /// [`Self::read`], of the Fleet `link` reaches.
    fn read_through(link: &crate::fleet_link::FleetLink) -> Self {
        let program = link.program();
        let version = bounded_output(link, &["version", "--json"], PROBE_TIMEOUT).and_then(|out| {
            if out.status.success() {
                serde_json::from_slice(&out.stdout)
                    .map_err(|e| format!("{program} version --json answered no JSON: {e}"))
            } else {
                Err(format!(
                    "{program} version --json: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        });
        let hosts =
            bounded_output(link, &["runtime", "hosts", "--json"], PROBE_TIMEOUT).and_then(|out| {
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
            });
        Self {
            program: program.to_string(),
            version,
            config: fleet_config_path(),
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

    /// The Fleet data directory: the config's `[data] dir`, relative to the
    /// config's own directory, else Fleet's per-user default.
    pub(super) fn data_dir(&self) -> Option<std::path::PathBuf> {
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

    fn findings(&self) -> Vec<Finding> {
        let mut findings = vec![match &self.version {
            Ok(v) => Finding {
                key: "fleet".into(),
                level: Level::Ok,
                detail: format!(
                    "{} {} runs every session; harnesses: {}",
                    self.program,
                    v["version"].as_str().unwrap_or("?"),
                    self.harnesses().join(", ")
                ),
            },
            Err(why) => Finding {
                key: "fleet".into(),
                level: Level::Fail,
                detail: format!(
                    "{why} — Fleet runs every session; install it, or set [fleet] program in \
                     settings.toml"
                ),
            },
        }];
        findings.push(match &self.config {
            Some((path, from)) if path.is_file() => Finding {
                key: "fleet-config".into(),
                level: Level::Ok,
                detail: format!("{} ({from})", path.display()),
            },
            Some((path, from)) => Finding {
                key: "fleet-config".into(),
                level: Level::Warn,
                detail: format!(
                    "{} ({from}) does not exist — run `{} init`",
                    path.display(),
                    self.program
                ),
            },
            None => Finding {
                key: "fleet-config".into(),
                level: Level::Warn,
                detail: "no Fleet config path: set FLEET_CONFIG".into(),
            },
        });
        findings.push(match &self.hosts {
            Ok(answer) => Finding {
                key: "fleet-hosts".into(),
                level: Level::Ok,
                detail: match answer["hosts"].as_array() {
                    Some(hosts) => hosts
                        .iter()
                        .filter_map(|h| h.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    // `null`: one runtime serves any host.
                    None => "any host (one runtime serves them all)".into(),
                },
            },
            Err(why) => Finding {
                key: "fleet-hosts".into(),
                level: Level::Warn,
                detail: why.clone(),
            },
        });
        let mux = "runtime-mux";
        findings.push(match crate::backend::preflight::look_up(mux) {
            crate::backend::preflight::Presence::Missing => Finding {
                key: mux.into(),
                level: Level::Warn,
                detail: format!(
                    "{mux} is not on PATH — Fleet's local runtime runs it unless its \
                         config names another program"
                ),
            },
            _ => Finding {
                key: mux.into(),
                level: Level::Ok,
                detail: format!("{mux} is installed"),
            },
        });
        findings
    }

    /// The facts as one JSON object, for `doctor`, `config show` and
    /// `version --json`.
    pub(super) fn document(&self) -> Value {
        json!({
            "program": self.program,
            "version": self.version.as_ref().ok().and_then(|v| v["version"].as_str()),
            "harnesses": self.harnesses(),
            "config": self.config.as_ref().map(|(path, _)| path.display().to_string()),
            "config_from": self.config.as_ref().map(|(_, from)| *from),
            "data_dir": self.data_dir().map(|d| d.display().to_string()),
            "error": self.version.as_ref().err(),
        })
    }
}

/// How long `fleet version --json` may take: a wrapper that stalls must not
/// hang `doctor`, `config show` or `version`.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
        let facts =
            crate::paths::with_path(remote.path(), || FleetFacts::read_through(&remote.link));
        assert_eq!(facts.version.expect("version")["version"], "9.9.9");
        assert_eq!(facts.hosts.expect("hosts")["hosts"], json!(["local"]));
        assert!(remote.called("version --json"), "{:?}", remote.calls());
        assert!(
            remote.called("runtime hosts --json"),
            "{:?}",
            remote.calls()
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
            "#!/bin/sh\ncase \"$1\" in\n  runtime) echo '{\"hosts\":[\"local\",\"devbox\"]}' ;;\n  *) echo '{\"version\":\"9.9.9\",\"harnesses\":[]}' ;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).unwrap();
        let findings = FleetFacts::read(&fleet.to_string_lossy()).findings();
        let hosts = findings
            .iter()
            .find(|f| f.key == "fleet-hosts")
            .expect("a fleet-hosts row");
        assert_eq!(hosts.level, Level::Ok, "{}", hosts.detail);
        assert!(hosts.detail.contains("local, devbox"), "{}", hosts.detail);
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
            "#!/bin/sh\necho '{\"version\":\"9.9.9\",\"harnesses\":[\"claude-code\",\"codex\"]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[data]\ndir = \"store\"\n").unwrap();
        std::env::set_var("FLEET_CONFIG", &config);

        let facts = FleetFacts::read(&fleet.to_string_lossy());
        let findings = facts.findings();
        assert_eq!(findings[0].level, Level::Ok, "{}", findings[0].detail);
        assert!(
            findings[0].detail.contains("9.9.9"),
            "{}",
            findings[0].detail
        );
        assert!(findings[0].detail.contains("claude-code, codex"));
        assert_eq!(findings[1].level, Level::Ok, "{}", findings[1].detail);
        assert!(findings[1].detail.contains("FLEET_CONFIG"));
        assert_eq!(
            facts.document()["data_dir"],
            json!(dir.path().join("store"))
        );

        let gone = FleetFacts::read("definitely-not-a-fleet-xyz");
        assert_eq!(gone.findings()[0].level, Level::Fail);

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
