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
    let mut findings = Vec::new();

    let mux = crate::agent::preflight::local_multiplexer();
    findings.push(match crate::agent::preflight::look_up(mux) {
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
    });

    let registry = crate::agent::agent_config::load_or_seed();
    let mut resolved = 0usize;
    for agent in &registry.agents {
        let present = crate::agent::preflight::look_up(&agent.command)
            == crate::agent::preflight::Presence::Present;
        resolved += usize::from(present);
        findings.push(Finding {
            key: format!("agent:{}", agent.name),
            level: if present { Level::Ok } else { Level::Warn },
            detail: if present {
                format!("{} runs `{}`", agent.name, agent.command)
            } else {
                // The short form: the search path is printed once below, and
                // repeating it on every row of a ten-agent registry buries the
                // name that differs between them.
                crate::agent::preflight::Dependency::Agent {
                    name: &agent.name,
                    command: &agent.command,
                }
                .missing_summary()
            },
        });
    }
    if !registry.agents.is_empty() && resolved == 0 {
        // Every individual miss above is a warning; all of them together is
        // not. A machine where no registered agent resolves can create a
        // session, and every one of them will open a pane that exits.
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

    // A remote host's own binaries are on the host and are not probed here: the
    // answer would be a round trip per host, and `session doctor` is what
    // reports on a session once one exists there. What *is* checkable from here
    // is the launcher that would carry the request.
    let (hosts, _warnings) = crate::agent::host_config::cached_registry();
    for host in &hosts.hosts {
        let launcher = if host.backend_name().starts_with("wsl:") {
            "wsl.exe"
        } else {
            "ssh"
        };
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
    }

    let verdict = findings.iter().map(|f| f.level).max().unwrap_or(Level::Ok);

    let mut human = format!("This machine — {}\n", verdict.as_str().to_uppercase());
    for f in &findings {
        human.push_str(&format!(
            "  {}  {:<20} {}\n",
            f.level.mark(),
            f.key,
            f.detail
        ));
    }
    human.push_str(&format!(
        "\nPATH searched ({}):\n",
        crate::paths::path_dirs().len()
    ));
    for dir in crate::paths::path_dirs() {
        human.push_str(&format!("  {}\n", dir.display()));
    }
    human.push_str("\nWiring of an existing session: thurbox-cli session doctor");

    let json = json!({
        "verdict": verdict.as_str(),
        "multiplexer": mux,
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
    });

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
