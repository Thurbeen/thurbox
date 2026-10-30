//! `thurbox-cli doctor` — whether this machine has what a session needs
//! *before* anyone tries to create one.
//!
//! The companion to `session doctor`, and deliberately the same shape: that one
//! asks whether an existing session's status hooks are wired, this one asks
//! whether the binaries a session is made of are installed at all. Splitting
//! them rather than growing one command follows what they are asked about — one
//! takes a session id, the other cannot, because on a fresh machine there are
//! no sessions to name.
//!
//! It reads; it never installs. Each `fail`/`warn` carries the one thing to do,
//! and where the answer depends on a distribution it names the package and
//! links the project's own install page rather than guessing an invocation.

use serde_json::{json, Value};

use crate::cli::output::CommandOutput;
use crate::cli::CommandError;

/// Whether a check found a problem, and how bad.
///
/// The same three words `session doctor` uses, so a script that already
/// branches on one branches on the other.
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
/// Exits non-zero only when something that must work does not: no multiplexer,
/// or no registered agent that resolves anywhere. A partially-installed
/// registry is a `warn` and exits 0 — having `claude` but not `aider` is an
/// ordinary machine, not breakage.
pub fn run() -> Result<CommandOutput, CommandError> {
    let mut findings = vec![multiplexer_finding()];
    findings.extend(agent_findings());
    findings.extend(host_findings());

    let verdict = findings.iter().map(|f| f.level).max().unwrap_or(Level::Ok);
    let human = render(&findings, verdict);
    let json = document(&findings, verdict);

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
    let mux = crate::agent::preflight::local_multiplexer();
    match crate::agent::preflight::look_up(mux) {
        crate::agent::preflight::Presence::Present => Finding {
            key: "multiplexer".into(),
            level: Level::Ok,
            detail: format!("{mux} is installed{}", version_suffix(mux)),
        },
        _ => Finding {
            key: "multiplexer".into(),
            level: Level::Fail,
            detail: crate::agent::preflight::Dependency::LocalMultiplexer.missing_summary(),
        },
    }
}

/// One row per registered agent, plus the verdict on the registry as a whole.
fn agent_findings() -> Vec<Finding> {
    let registry = crate::agent::agent_config::load_or_seed();
    let mut findings: Vec<Finding> = registry.agents.iter().map(agent_finding).collect();

    // Every individual miss above is a warning; all of them together is not. A
    // machine where no registered agent resolves can still create a session,
    // and every one of them will open a pane that exits. An agent whose
    // presence is *unknown* counts as resolving: it may well launch, and
    // failing the report over a machine nothing looked at is the conflation
    // `Presence` exists to prevent.
    let missing = findings.iter().filter(|f| f.level == Level::Warn).count();
    if !registry.agents.is_empty() && missing == registry.agents.len() {
        findings.push(Finding {
            key: "agents".into(),
            level: Level::Fail,
            detail: format!(
                "none of the {} registered agents resolves on PATH — a session created now \
                 would open a pane that exits immediately",
                registry.agents.len()
            ),
        });
    }
    findings
}

/// What one agent's `command` resolves to.
///
/// Three answers, not two. A relative `command` is launched from the session's
/// own directory, so this machine's answer about it would be about the wrong
/// directory — reported as such rather than guessed.
fn agent_finding(agent: &crate::session::AgentDef) -> Finding {
    let key = format!("agent:{}", agent.name);
    match crate::agent::preflight::look_up(&agent.command) {
        crate::agent::preflight::Presence::Present => Finding {
            key,
            level: Level::Ok,
            detail: format!("{} runs `{}`", agent.name, agent.command),
        },
        crate::agent::preflight::Presence::Unknown => Finding {
            key,
            level: Level::Ok,
            detail: format!(
                "{} runs `{}`, which is resolved from the session's own directory — not \
                 something this machine can answer",
                agent.name, agent.command
            ),
        },
        // The short form: the search path is printed once below, and repeating
        // it on every row of a ten-agent registry buries the name that differs
        // between them.
        crate::agent::preflight::Presence::Missing => Finding {
            key,
            level: Level::Warn,
            detail: crate::agent::preflight::Dependency::Agent {
                name: &agent.name,
                command: &agent.command,
            }
            .missing_summary(),
        },
    }
}

/// One row per configured host about the launcher, and for a host that
/// shares sessions a second about the `thurbox-cli` there.
///
/// The multiplexer and agents on a host are not probed here: `session doctor`
/// reports on a session once one exists there. The CLI is, because sharing
/// rests on it and nothing else says when it cannot work — the mirror skips the
/// host and logs why at debug level, and the host's sessions simply never
/// appear. That costs one round trip per shareable host, bounded by the ssh
/// connect timeout.
fn host_findings() -> Vec<Finding> {
    let (hosts, _warnings) = crate::agent::host_config::cached_registry();
    let mut findings = Vec::new();
    for host in &hosts.hosts {
        let launcher = if host.is_wsl() { "wsl.exe" } else { "ssh" };
        let present = crate::agent::preflight::look_up(launcher)
            == crate::agent::preflight::Presence::Present;
        findings.push(Finding {
            key: format!("host:{}", host.name),
            level: if present { Level::Ok } else { Level::Fail },
            detail: if present {
                format!(
                    "{} is reached with {launcher}, which is installed (the multiplexer and \
                     agents on {} are not probed from here)",
                    host.name, host.name
                )
            } else {
                crate::agent::preflight::Dependency::Launcher(launcher).missing_summary()
            },
        });
        if present && host.shareable() {
            findings.push(sharing_finding(
                &host.name,
                launcher,
                &crate::session_ops::host_cli::probe(host),
            ));
        }
    }
    findings
}

/// What a host's `thurbox-cli` probe means for sharing its sessions.
///
/// Never a `fail`: a host being down or behind is not this machine missing
/// something. A probe never provisions — doctor reads, it never installs — so
/// a host that needs a new CLI names the command that installs one and reports
/// why it could not.
fn sharing_finding(
    name: &str,
    launcher: &str,
    probe: &Result<
        Option<crate::session_ops::host_cli::CliInfo>,
        crate::session_ops::host_cli::ProbeFailure,
    >,
) -> Finding {
    let sync = format!("`thurbox-cli session sync --host {name}`");
    let (level, detail) = match probe {
        Ok(Some(cli)) => match crate::session_ops::host_cli::compatible(cli) {
            Ok(()) => (
                Level::Ok,
                format!(
                    "{name} shares sessions through thurbox-cli {} at {}",
                    cli.version, cli.path
                ),
            ),
            Err(mismatch) => (
                Level::Warn,
                format!(
                    "{name}'s thurbox-cli {} at {} {mismatch}, so its sessions are not shared \
                     until a matching one is installed there — {sync} installs it, or says why \
                     it cannot",
                    cli.version, cli.path
                ),
            ),
        },
        Ok(None) => (
            Level::Warn,
            format!(
                "{name} has no thurbox-cli, so its sessions are not shared until one is \
                 installed there — {sync} installs it, or says why it cannot"
            ),
        ),
        Err(e) if e.fault == crate::session_ops::host_cli::ProbeFault::Refused => (
            Level::Warn,
            format!(
                "{name} would not start its thurbox-cli ({}), so its sessions are not shared \
                 until its endpoint policy allows it — replacing the binary would not help",
                first_line(&e.message)
            ),
        ),
        Err(e) if e.fault == crate::session_ops::host_cli::ProbeFault::Broken => (
            Level::Warn,
            format!(
                "{name}'s thurbox-cli does not work ({}), so its sessions are not shared until it \
                 is replaced — {sync} replaces it, or says why it cannot",
                first_line(&e.message)
            ),
        ),
        Err(e) => (
            Level::Warn,
            format!(
                "{name} did not answer the thurbox-cli probe over {launcher}, so its sessions are \
                 not shared: {}",
                first_line(&e.message)
            ),
        ),
    };
    Finding {
        key: format!("sharing:{name}"),
        level,
        detail,
    }
}

fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or_default()
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
    human.push_str("\nWiring of an existing session: thurbox-cli session doctor");
    human
}

/// The same report as one JSON document, for a script that branches on `key`
/// and `level` rather than parsing the sentences.
fn document(findings: &[Finding], verdict: Level) -> Value {
    json!({
        "verdict": verdict.as_str(),
        "multiplexer": crate::agent::preflight::local_multiplexer(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::AgentDef;

    /// Before the `Presence::Unknown` fix, `look_up` answered a relative
    /// `command` as `Missing` (checked against thurbox's own directory, not
    /// the session's), so this row read as a `warn` telling the user to
    /// install a CLI that was already there. Reproduces with `left: Warn`.
    #[test]
    fn a_relative_agent_command_is_reported_ok_not_a_warning() {
        let agent = AgentDef {
            name: "custom".into(),
            command: "./bin/agent".into(),
            ..Default::default()
        };
        let finding = agent_finding(&agent);
        assert_eq!(finding.level, Level::Ok, "{}", finding.detail);
        assert!(
            finding
                .detail
                .contains("resolved from the session's own directory"),
            "{}",
            finding.detail
        );
    }

    #[test]
    fn a_missing_agent_command_is_still_a_warning() {
        let agent = AgentDef {
            name: "custom".into(),
            command: "definitely-not-a-real-binary-xyz".into(),
            ..Default::default()
        };
        let finding = agent_finding(&agent);
        assert_eq!(finding.level, Level::Warn, "{}", finding.detail);
    }

    fn cli(schema: u32) -> crate::session_ops::host_cli::CliInfo {
        crate::session_ops::host_cli::CliInfo {
            path: "thurbox-cli".into(),
            version: crate::agent::version_check::current_version().into(),
            tmux_socket: None,
            data_dir: None,
            tmux_tmpdir: None,
            schema_version: Some(schema),
            multiplexer_choice: true,
            soft_delete_choice: true,
            session_revision: true,
            deleted_changes_since: true,
            create_parity: true,
            agent_args: false,
            telemetry_attrs: true,
            purged_tombstones: false,
            lifecycle_preconditions: false,
        }
    }

    /// A host whose CLI is behind used to read `ok` here — doctor looked only
    /// at the local `ssh` — while every attempt to share with it failed and
    /// said so nowhere but a debug log.
    #[test]
    fn a_host_whose_cli_cannot_share_is_a_warning_naming_the_repair() {
        let behind = sharing_finding("box", "ssh", &Ok(Some(cli(1))));
        assert_eq!(behind.level, Level::Warn, "{}", behind.detail);
        assert_eq!(behind.key, "sharing:box");
        assert!(
            behind
                .detail
                .contains("`thurbox-cli session sync --host box`"),
            "{}",
            behind.detail
        );

        let current = sharing_finding("box", "ssh", &Ok(Some(cli(crate::storage::SCHEMA_VERSION))));
        assert_eq!(current.level, Level::Ok, "{}", current.detail);
    }

    /// A host that answered with a CLI that crashes is not a host that did
    /// not answer: the repair is a reinstall, which `session sync` performs.
    #[test]
    fn a_host_with_a_broken_cli_is_not_reported_as_silent() {
        let failure = crate::session_ops::host_cli::ProbeFailure {
            message: "thurbox-cli died on signal 11 (SIGSEGV)".into(),
            fault: crate::session_ops::host_cli::ProbeFault::Broken,
        };
        let finding = sharing_finding("box", "ssh", &Err(failure));
        assert_eq!(finding.level, Level::Warn);
        assert!(
            !finding.detail.contains("did not answer"),
            "{}",
            finding.detail
        );
        assert!(finding.detail.contains("SIGSEGV"), "{}", finding.detail);
        assert!(
            finding
                .detail
                .contains("`thurbox-cli session sync --host box`"),
            "{}",
            finding.detail
        );
    }

    /// A host whose endpoint refused to start the CLI is sent to that policy,
    /// not to `session sync`, which would only re-provision a binary the
    /// policy refuses again (#1576).
    #[test]
    fn a_host_that_refused_to_start_its_cli_points_at_the_policy() {
        let failure = crate::session_ops::host_cli::ProbeFailure {
            message: "thurbox-cli at C:/x/thurbox-cli.exe could not be started".into(),
            fault: crate::session_ops::host_cli::ProbeFault::Refused,
        };
        let finding = sharing_finding("box", "ssh", &Err(failure));
        assert_eq!(finding.level, Level::Warn);
        assert!(
            finding.detail.contains("endpoint policy") && !finding.detail.contains("session sync"),
            "{}",
            finding.detail
        );
    }

    #[test]
    fn a_host_that_did_not_answer_says_why_on_one_line() {
        let failure = crate::session_ops::host_cli::ProbeFailure {
            message: "ssh: connect to host box port 22: Connection refused\nsecond line".into(),
            fault: crate::session_ops::host_cli::ProbeFault::Unanswered,
        };
        let finding = sharing_finding("box", "ssh", &Err(failure));
        assert_eq!(finding.level, Level::Warn);
        assert!(finding.detail.contains("over ssh"), "{}", finding.detail);
        assert!(
            finding.detail.ends_with("Connection refused"),
            "{}",
            finding.detail
        );
    }
}
