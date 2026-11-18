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
    /// The command or step that fixes a check that is not ok.
    fix: Option<String>,
}

impl Finding {
    fn new(key: &str, level: Level, detail: String) -> Self {
        Self {
            key: key.into(),
            level,
            detail,
            fleet: None,
            fix: None,
        }
    }

    /// The same finding, with the one thing to do about it.
    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
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
        findings.push(
            Finding::new(
                "fleets",
                Level::Warn,
                format!(
                "{} Fleets are listed; the interface uses {in_force}, the first by name, until it \
                 follows several",
                listed.len()
            ),
            )
            .fix("keep only the Fleets you use under [fleets] in settings.toml"),
        );
    }
    findings.extend(retired_hosts_finding(
        crate::config::file::path("hosts.toml").as_deref(),
    ));
    findings.extend(retired_automations_finding(
        crate::config::file::path("automations.json").as_deref(),
    ));
    findings.extend(heartbeat_finding(&LocalServer::of_this_instance()));
    findings.extend(automation_timer_finding(
        automation_timer_state().as_deref(),
    ));
    // Sessions of a Fleet on another machine run in that machine's
    // multiplexer: this one needs none unless a Fleet runs here.
    if listed.iter().any(|(_, def)| def.transport.is_local()) {
        findings.push(multiplexer_finding());
    }
    findings.push(telemetry_finding(otel_env::describe(
        &settings.telemetry,
        true,
    )));

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

/// A warning for a `hosts.toml` an earlier thurbox read, at `path`: nothing
/// reads it any more, and saying so is all this does. `None` when there is
/// no such file.
fn retired_hosts_finding(path: Option<&std::path::Path>) -> Option<Finding> {
    let path = path.filter(|path| path.is_file())?;
    Some(
        Finding::new(
            "hosts-toml",
            Level::Warn,
            "hosts.toml is no longer read; list remote Fleets under [fleets] in settings.toml \
             (see docs/where-sessions-run.md)"
                .into(),
        )
        .fix(format!(
            "list each machine's Fleet under [fleets] in settings.toml, then delete {}",
            path.display()
        )),
    )
}

/// A warning for an `automations.json` an earlier thurbox fired: nothing
/// reads it any more, and saying so is all this does. `None` when there is
/// no such file.
fn retired_automations_finding(path: Option<&std::path::Path>) -> Option<Finding> {
    let path = path.filter(|path| path.is_file())?;
    Some(
        Finding::new(
            "automations-json",
            Level::Warn,
            "automations.json is no longer read; thurbox schedules nothing of its own \
             (see docs/thurbox/features.md, Scheduling work)"
                .into(),
        )
        .fix(format!(
            "move each schedule to a Fleet CronTask (`fleet apply -f crontask.json`; see \
             docs/fleet/resources/cron-task.md), then delete {}",
            path.display()
        )),
    )
}

/// The window an earlier thurbox kept on its tmux server to fire automations.
const HEARTBEAT_WINDOW: &str = "automation-heartbeat";

/// The systemd user timer an earlier thurbox shipped for the same purpose.
const AUTOMATION_TIMER: &str = "thurbox-automations.timer";

/// This instance's local tmux server, as a `tmux` command names it: the
/// socket, and the socket directory when thurbox chooses it rather than an
/// inherited `TMUX_TMPDIR`.
struct LocalServer {
    socket: String,
    tmpdir: Option<String>,
}

impl LocalServer {
    fn of_this_instance() -> Self {
        let inherited = std::env::var_os("TMUX_TMPDIR").is_some_and(|d| !d.is_empty());
        LocalServer {
            socket: crate::backend::instance::local_socket_name(),
            tmpdir: (!inherited)
                .then(crate::backend::instance::local_socket_dir_report)
                .flatten(),
        }
    }

    /// `tmux …` against this server, as a person would paste it: every word
    /// shell-quoted, so a data directory with a space stays one word.
    fn command_line(&self, args: &[&str]) -> String {
        let q = crate::shell::posix_quote;
        let env = self
            .tmpdir
            .as_ref()
            .map(|d| format!("TMUX_TMPDIR={} ", q(d)))
            .unwrap_or_default();
        let words: Vec<String> = ["tmux", "-L", &self.socket]
            .iter()
            .chain(args)
            .map(|w| q(w))
            .collect();
        format!("{env}{}", words.join(" "))
    }

    /// Every window on the server as `session:window`. `None` when there is
    /// no server or tmux could not be asked: nothing to report either way.
    fn windows(&self) -> Option<String> {
        let mut cmd = std::process::Command::new("tmux");
        cmd.args(["-L", &self.socket, "list-windows", "-a", "-F"])
            .arg("#{session_name}:#{window_name}")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(dir) = &self.tmpdir {
            cmd.env("TMUX_TMPDIR", dir);
        }
        let out = cmd.output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// A warning for a keeper window an earlier thurbox armed: it runs the gone
/// `thurbox-cli automation tick` every minute until it is killed. `None`
/// when the server has none.
fn heartbeat_finding(server: &LocalServer) -> Option<Finding> {
    heartbeat_in(server, server.windows().as_deref())
}

fn heartbeat_in(server: &LocalServer, windows: Option<&str>) -> Option<Finding> {
    let target = windows?.lines().find(|line| {
        line.rsplit_once(':')
            .is_some_and(|(_, w)| w == HEARTBEAT_WINDOW)
    })?;
    Some(
        Finding::new(
            "automation-heartbeat",
            Level::Warn,
            format!(
                "the tmux window {target} an earlier thurbox armed still runs \
                 `thurbox-cli automation tick`, a command this thurbox no longer has"
            ),
        )
        .fix(server.command_line(&["kill-window", "-t", target])),
    )
}

/// What `systemctl --user is-enabled` answers for the old timer, or `None`
/// when it could not be asked. Only Linux had the timer.
fn automation_timer_state() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let out = std::process::Command::new("systemctl")
        .args(["--user", "is-enabled", AUTOMATION_TIMER])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A warning for the old systemd timer while it is enabled: it runs the
/// gone `thurbox-cli automation tick` every minute.
fn automation_timer_finding(state: Option<&str>) -> Option<Finding> {
    (state? == "enabled").then(|| {
        Finding::new(
            "automation-timer",
            Level::Warn,
            format!(
                "{AUTOMATION_TIMER} is enabled and runs `thurbox-cli automation tick`, a \
                 command this thurbox no longer has"
            ),
        )
        .fix(format!(
            "systemctl --user disable --now {AUTOMATION_TIMER}, then remove its unit files \
             from ~/.config/systemd/user"
        ))
    })
}

fn telemetry_finding(described: Result<String, String>) -> Finding {
    match described {
        Ok(detail) => Finding::new(
            "telemetry",
            Level::Ok,
            format!("interface export: {detail}"),
        ),
        Err(why) => Finding::new(
            "telemetry",
            Level::Warn,
            format!("the interface exports nothing: {why}"),
        )
        .fix("fix [telemetry] in settings.toml or the OTEL_* variables"),
    }
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
        )
        .fix(crate::backend::preflight::Dependency::LocalMultiplexer.fix()),
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
        if let Some(fix) = &f.fix {
            human.push_str(&format!("        {:<20} fix: {fix}\n", ""));
        }
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
                "fix": f.fix,
            }))
            .collect::<Vec<Value>>(),
    })
}

/// The one-line install, which installs Fleet and runtime-mux with thurbox.
fn installer() -> &'static str {
    if cfg!(windows) {
        "irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1 | iex"
    } else {
        "curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh"
    }
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
    /// The command that starts its manager ([`crate::fleet_link::FleetLink::start_manager`]).
    start_manager: String,
    restart_manager: String,
    /// `fleet runtime hosts --json`: the places a session can run in, or why
    /// they could not be read.
    pub(super) hosts: Result<serde_json::Value, String>,
}

impl FleetFacts {
    pub(super) fn read(link: &crate::fleet_link::FleetLink) -> Self {
        let program = link.program();
        // ssh's connect bound is inside the probe's: a slow handshake is not
        // a Fleet that stalls.
        let timeout = link.probe_timeout();
        let answer = link.bounded_output(&["version", "--json"], timeout);
        let unreachable = answer.as_ref().ok().and_then(|out| link.unreachable(out));
        let version = match &unreachable {
            Some(why) => Err(why.clone()),
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
            Some(why) => Err(why.clone()),
            None => link
                .bounded_output(&["runtime", "hosts", "--json"], timeout)
                .and_then(|out| {
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
                }),
        };
        let config = link.local_config();
        let no_manager = serde_json::Value::Null;
        let manager = version
            .as_ref()
            .map_or(&no_manager, |v: &serde_json::Value| &v["manager"]);
        let start_manager = link.start_manager(manager);
        let restart_manager = link.restart_manager(manager);
        Self {
            name: link.name().to_string(),
            program: program.to_string(),
            local: link.is_local(),
            unreachable,
            version,
            config,
            hosts,
            start_manager,
            restart_manager,
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

    /// Why thurbox cannot write this Fleet's Tasks: the shared rule,
    /// [`crate::fleet_link::incompatible`].
    fn incompatible(&self) -> Option<String> {
        crate::fleet_link::incompatible(self.version.as_ref().ok()?)
    }

    /// The checks of this Fleet. `in_force`: the interface uses it, so a
    /// Fleet that does not answer fails the report; any other only warns.
    fn findings(&self, in_force: bool) -> Vec<Finding> {
        let broken = if in_force { Level::Fail } else { Level::Warn };
        let name = &self.name;
        let mut findings = vec![match (&self.version, self.incompatible()) {
            (Ok(_), Some(why)) => Finding::new("fleet", broken, why).fix(if self.local {
                format!("update Fleet: {}", installer())
            } else {
                "update Fleet on that machine".into()
            }),
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
            (Err(why), _) if self.unreachable.is_some() => {
                Finding::new("fleet", broken, why.clone()).fix(format!(
                    "check the transport of [fleets.{name}] in settings.toml, then `thurbox-cli \
                     doctor`"
                ))
            }
            (Err(why), _) if self.local => Finding::new("fleet", broken, why.clone()).fix(format!(
                "install Fleet with thurbox: {}, or set [fleets.{name}] program in settings.toml",
                installer()
            )),
            (Err(why), _) => Finding::new("fleet", broken, why.clone()).fix(format!(
                "install Fleet on that machine, or set [fleets.{name}] program in settings.toml"
            )),
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
                Err(why) => Finding::new("fleet-hosts", Level::Warn, why.clone()).fix(format!(
                    "`{} doctor` names the runtime that does not answer",
                    self.program
                )),
            });
        }
        if self.local {
            findings.extend(self.local_findings());
            if self.config.as_ref().is_some_and(|(path, _)| path.is_file()) {
                findings.extend(self.manager_finding(broken));
            }
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

    /// Whether the Fleet's manager runs, from the `manager` of its `fleet
    /// version --json`. thurbox never starts a manager: a user systemd unit
    /// runs `fleet up`, and the row names the command that starts it. A
    /// Fleet that did not answer has no such row: its `fleet` row says why.
    fn manager_finding(&self, broken: Level) -> Option<Finding> {
        let version = self.version.as_ref().ok()?;
        let manager = &version["manager"];
        Some(match manager["held"].as_bool() {
            Some(true) => {
                let running = format!(
                    "manager running ({})",
                    manager["holder"].as_str().unwrap_or("?")
                );
                match (
                    manager["version"].as_str(),
                    version["cli"]["version"].as_str(),
                ) {
                    (Some(runs), Some(installed)) if runs != installed => Finding::new(
                        "fleet-manager",
                        Level::Warn,
                        format!("{running}; manager runs {runs}, installed {installed}"),
                    )
                    .fix(if self.local {
                        self.restart_manager.clone()
                    } else {
                        "restart the manager on that machine".to_string()
                    }),
                    _ => Finding::new("fleet-manager", Level::Ok, running),
                }
            }
            Some(false) if self.local => {
                Finding::new("fleet-manager", broken, "Fleet not running".into())
                    .fix(self.start_manager.clone())
            }
            Some(false) => Finding::new("fleet-manager", broken, "manager not running".into()).fix(
                match crate::fleet_link::supervisor_unit(manager) {
                    Some(unit) if manager["supervisor"]["kind"] == "systemd" => {
                        format!("systemctl --user start {unit} on that machine")
                    }
                    Some(_) => "fleet service start on that machine".to_string(),
                    None => "fleet service install --now on that machine".to_string(),
                },
            ),
            None => Finding::new(
                "fleet-manager",
                Level::Warn,
                format!(
                    "whether the manager runs is unknown: {}",
                    manager["why"]
                        .as_str()
                        .unwrap_or("this Fleet does not report its manager")
                ),
            )
            .fix(if self.local {
                "update Fleet"
            } else {
                "update Fleet on that machine"
            }),
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
                format!("{} ({from}) does not exist", path.display()),
            )
            .fix(match *from {
                "settings.toml" => crate::fleet_link::command_line(&[
                    &self.program,
                    "--config",
                    &path.to_string_lossy(),
                    "init",
                ]),
                _ => crate::fleet_link::command_line(&[&self.program, "init"]),
            }),
            None => Finding::new("fleet-config", Level::Warn, "no Fleet config path".into())
                .fix("set FLEET_CONFIG to the config file, then `fleet init`"),
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
            )
            .fix(format!("install it with thurbox: {}", installer())),
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A `hosts.toml` left by an earlier thurbox is reported as unread, with
    /// the fix; none at all reports nothing.
    #[test]
    fn a_left_over_hosts_toml_is_a_warning_with_its_fix() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("hosts.toml");
        assert!(
            retired_hosts_finding(Some(&path)).is_none(),
            "no file, no finding"
        );
        std::fs::write(&path, "[[hosts]]\nname = \"devbox\"\n").expect("hosts.toml");
        let finding = retired_hosts_finding(Some(&path)).expect("a finding");
        assert_eq!(finding.key, "hosts-toml");
        assert_eq!(finding.level, Level::Warn);
        assert_eq!(
            finding.detail,
            "hosts.toml is no longer read; list remote Fleets under [fleets] in settings.toml \
             (see docs/where-sessions-run.md)"
        );
        let fix = finding.fix.expect("a fix");
        assert!(fix.contains(&path.display().to_string()), "{fix}");
    }

    /// An `automations.json` left by an earlier thurbox is reported as
    /// unread, with the fix; none at all reports nothing.
    #[test]
    fn a_left_over_automations_json_is_a_warning_with_its_fix() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("automations.json");
        assert!(
            retired_automations_finding(Some(&path)).is_none(),
            "no file, no finding"
        );
        std::fs::write(&path, "{\"automations\":[]}").expect("automations.json");
        let finding = retired_automations_finding(Some(&path)).expect("a finding");
        assert_eq!(finding.key, "automations-json");
        assert_eq!(finding.level, Level::Warn);
        assert!(
            finding
                .detail
                .starts_with("automations.json is no longer read"),
            "{}",
            finding.detail
        );
        let fix = finding.fix.expect("a fix");
        assert!(fix.contains(&path.display().to_string()), "{fix}");
        // Fleet's CronTask is where a schedule goes now.
        assert!(fix.contains("CronTask"), "{fix}");
        assert!(fix.contains("docs/fleet/resources/cron-task.md"), "{fix}");
    }

    /// The keeper window is found on any session and named with the command
    /// that kills it; a server without one, or none at all, reports nothing.
    #[test]
    fn a_left_over_heartbeat_window_is_a_warning_with_its_fix() {
        let server = LocalServer {
            socket: "thurbox".into(),
            tmpdir: Some("/data/tmux".into()),
        };
        assert!(heartbeat_in(&server, None).is_none(), "no server");
        assert!(heartbeat_in(&server, Some("thurbox:tb-a\n")).is_none());
        let finding = heartbeat_in(
            &server,
            Some("thurbox:tb-a\nthurbox:automation-heartbeat\n"),
        )
        .expect("a finding");
        assert_eq!(finding.key, "automation-heartbeat");
        assert_eq!(finding.level, Level::Warn);
        assert_eq!(
            finding.fix.as_deref(),
            Some("TMUX_TMPDIR=/data/tmux tmux -L thurbox kill-window -t thurbox:automation-heartbeat")
        );
    }

    /// A path with a space stays one word in the printed command.
    #[test]
    fn the_heartbeat_fix_quotes_its_paths() {
        let server = LocalServer {
            socket: "thurbox".into(),
            tmpdir: Some("/home/me/My Data/tmux".into()),
        };
        let finding =
            heartbeat_in(&server, Some("my session:automation-heartbeat\n")).expect("a finding");
        assert_eq!(
            finding.fix.as_deref(),
            Some(
                "TMUX_TMPDIR='/home/me/My Data/tmux' tmux -L thurbox kill-window -t \
                 'my session:automation-heartbeat'"
            )
        );
    }

    /// Only an enabled timer is reported, with the command that stops it.
    #[test]
    fn an_enabled_automation_timer_is_a_warning_with_its_fix() {
        for quiet in [None, Some("disabled"), Some("not-found"), Some("")] {
            assert!(automation_timer_finding(quiet).is_none(), "{quiet:?}");
        }
        let finding = automation_timer_finding(Some("enabled")).expect("a finding");
        assert_eq!(finding.key, "automation-timer");
        assert!(finding.fix.is_some_and(
            |f| f.contains("systemctl --user disable --now thurbox-automations.timer")
        ),);
    }

    /// A Fleet's facts with every field given, for the cases below.
    fn facts(
        local: bool,
        unreachable: Option<&str>,
        version: Result<Value, &str>,
        config: Option<&str>,
        hosts: Result<Value, &str>,
    ) -> FleetFacts {
        FleetFacts {
            name: "local".into(),
            program: "fleet".into(),
            local,
            unreachable: unreachable.map(str::to_string),
            version: version.map_err(str::to_string),
            config: config.map(|p| (std::path::PathBuf::from(p), "the per-user file")),
            hosts: hosts.map_err(str::to_string),
            start_manager: "fleet up".into(),
            restart_manager: "systemctl --user restart fleet.service".into(),
        }
    }

    /// Every check that is not ok carries the command or step that fixes it,
    /// in the JSON and on its own line of the report.
    #[test]
    fn every_failing_check_names_its_fix() {
        let empty = tempfile::tempdir().expect("an empty PATH dir");
        let fine = json!({"version": "9.9.9", "apiVersions": ["fleet/v1alpha1"], "harnesses": []});
        let cases = [
            facts(
                true,
                None,
                Err("fleet: not found"),
                None,
                Err("fleet: not found"),
            ),
            facts(
                true,
                None,
                Ok(json!({"version": "0.1.0"})),
                Some("/nowhere/config.toml"),
                Ok(json!({"hosts": []})),
            ),
            facts(
                true,
                None,
                Ok(fine.clone()),
                Some("/nowhere/config.toml"),
                Err("no config"),
            ),
            facts(
                false,
                Some("ssh: connect refused"),
                Err("ssh: connect refused"),
                None,
                Err("x"),
            ),
            facts(
                false,
                None,
                Ok(
                    json!({"version": "9.9.9", "apiVersions": ["fleet/v1alpha1"], "manager": {"held": false}}),
                ),
                None,
                Ok(json!({"hosts": []})),
            ),
            facts(false, None, Ok(fine), None, Ok(json!({"hosts": []}))),
        ];
        let mut findings = crate::paths::with_path(empty.path().as_os_str(), || {
            let mut all: Vec<Finding> = cases.iter().flat_map(|f| f.findings(true)).collect();
            all.push(multiplexer_finding());
            all
        });
        findings.retain(|f| f.level != Level::Ok);
        assert!(
            findings.len() >= 6,
            "the cases fail several checks: {}",
            findings.len()
        );
        for f in &findings {
            let fix = f.fix.as_deref().unwrap_or("");
            assert!(
                !fix.trim().is_empty(),
                "{} ({}) has no fix: {}",
                f.key,
                f.level.as_str(),
                f.detail
            );
        }
        let report = render(&findings, Level::Fail);
        assert!(report.contains("fix:"), "{report}");
        let json = document(&findings, Level::Fail);
        assert!(json["checks"][0]["fix"].is_string(), "{json}");
    }

    #[test]
    fn doctor_names_the_command_that_starts_a_local_fleets_manager() {
        let config = tempfile::NamedTempFile::new().expect("config");
        let down = json!({"version": "9.9.9", "apiVersions": ["fleet/v1alpha1"], "manager": {"held": false}});
        let findings = facts(
            true,
            None,
            Ok(down),
            config.path().to_str(),
            Ok(json!({"hosts": []})),
        )
        .findings(true);
        let manager = findings
            .iter()
            .find(|f| f.key == "fleet-manager")
            .expect("a fleet-manager row");
        assert_eq!(manager.level, Level::Fail, "{}", manager.detail);
        assert_eq!(manager.detail, "Fleet not running");
        assert_eq!(manager.fix.as_deref(), Some("fleet up"));
    }

    #[test]
    fn doctor_warns_when_the_manager_runs_another_version_than_the_installed_fleet() {
        let config = tempfile::NamedTempFile::new().expect("config");
        let answer = |manager: &str| {
            json!({"version": "2.0.0", "cli": {"version": "2.0.0"}, "apiVersions": ["fleet/v1alpha1"],
                "manager": {"held": true, "holder": "1@b", "version": manager}})
        };
        let row = |manager: &str| {
            facts(
                true,
                None,
                Ok(answer(manager)),
                config.path().to_str(),
                Ok(json!({"hosts": []})),
            )
            .findings(true)
            .into_iter()
            .find(|f| f.key == "fleet-manager")
            .expect("a fleet-manager row")
        };
        let drifted = row("1.9.0");
        assert_eq!(drifted.level, Level::Warn, "{}", drifted.detail);
        assert!(
            drifted
                .detail
                .contains("manager runs 1.9.0, installed 2.0.0"),
            "{}",
            drifted.detail
        );
        assert!(drifted.fix.is_some_and(|f| !f.trim().is_empty()));
        let same = row("2.0.0");
        assert_eq!(same.level, Level::Ok, "{}", same.detail);
        assert!(!same.detail.contains("installed"), "{}", same.detail);
    }

    #[test]
    fn doctor_names_fleet_init_for_a_local_fleet_with_no_config() {
        let findings = facts(
            true,
            None,
            Ok(json!({"version": "9.9.9", "apiVersions": ["fleet/v1alpha1"], "manager": {"held": false}})),
            Some("/nowhere/config.toml"),
            Ok(json!({"hosts": []})),
        )
        .findings(true);
        let config = findings
            .iter()
            .find(|f| f.key == "fleet-config")
            .expect("a fleet-config row");
        assert_eq!(config.fix.as_deref(), Some("fleet init"));
        assert!(
            findings.iter().all(|f| f.key != "fleet-manager"),
            "no manager to start before the config exists"
        );
    }

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
            manager.detail.contains("does not report its manager"),
            "{}",
            manager.detail
        );
        assert_eq!(manager.fix.as_deref(), Some("update Fleet on that machine"));
    }

    /// `doctor` lists the hosts Fleet defines, which are where a session can
    /// run.
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

    /// The interface's export is reported from its own table, with where
    /// each value comes from; a table against the rules warns.
    #[test]
    fn doctor_reports_the_interfaces_telemetry_table() {
        let table: otel_env::TelemetryConfig =
            toml::from_str("endpoint = \"http://127.0.0.1:9\"\nlogs = true\n").unwrap();
        let ok = telemetry_finding(Ok(otel_env::describe(&table, true).unwrap()));
        assert_eq!(ok.level, Level::Ok, "{}", ok.detail);
        assert!(ok.detail.contains("(config file)"), "{}", ok.detail);
        let grpc: otel_env::TelemetryConfig = toml::from_str("protocol = \"grpc\"\n").unwrap();
        let warn = telemetry_finding(otel_env::describe(&grpc, true));
        assert_eq!(warn.level, Level::Warn);
        assert!(
            warn.detail.contains("telemetry.protocol"),
            "{}",
            warn.detail
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
        let out = crate::fleet_link::FleetLink::local(stuck.to_string_lossy())
            .bounded_output(&["version"], std::time::Duration::from_millis(300));
        assert!(out.unwrap_err().contains("did not answer"));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
