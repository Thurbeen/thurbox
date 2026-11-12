//! `fleet doctor`: what each check found, classified as ok, info, warn or
//! fail, with the one command or step that fixes it. The CLI observes; this
//! module decides. Only a fail makes the command exit non-zero.
//!
//! The checks are in docs/fleet/contracts.md, "The command line".

use serde::Serialize;

use crate::integration::State;

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    /// Worth knowing, nothing is broken: an agent the user did not install.
    Info,
    /// Works now, but needs a step: an outdated or modified integration.
    Warn,
    /// Something Fleet needs does not work.
    Fail,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

/// One check's finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// `integration`, `harness` or `runtime`.
    pub area: &'static str,
    /// The agent, harness or host checked.
    pub name: String,
    pub level: Level,
    pub detail: String,
    /// The fix, when one is needed.
    pub hint: Option<String>,
}

/// An installed integration's state: outdated and modified are warnings.
pub fn integration(agent: &str, state: State) -> Check {
    let (level, hint) = match state {
        State::Current | State::NotNeeded | State::Unsupported => (Level::Ok, None),
        State::UnsupportedPlatform => (Level::Info, None),
        State::AgentMissing => (
            Level::Info,
            Some(format!(
                "start {agent} once, then run fleet integration install {agent}"
            )),
        ),
        State::NotInstalled => (
            Level::Info,
            Some(format!("fleet integration install {agent}")),
        ),
        State::Outdated => (
            Level::Warn,
            Some(format!("fleet integration install {agent}")),
        ),
        State::Modified => (
            Level::Warn,
            Some(format!(
                "fleet integration uninstall {agent}, then fleet integration install {agent}"
            )),
        ),
    };
    Check {
        area: "integration",
        name: agent.to_string(),
        level,
        detail: state.as_str().to_string(),
        hint,
    }
}

/// A ledger that cannot be read: no integration's state is known.
pub fn integration_unknown(agent: &str, why: &str) -> Check {
    Check {
        area: "integration",
        name: agent.to_string(),
        level: Level::Fail,
        detail: why.to_string(),
        hint: Some("check the data directory's permissions and integrations.json".into()),
    }
}

/// A harness's program, where it was found on PATH. A missing one is
/// information only: a user installs the agents they run.
pub fn harness(name: &str, program: &str, found: Option<&str>) -> Check {
    match found {
        Some(path) => Check {
            area: "harness",
            name: name.to_string(),
            level: Level::Ok,
            detail: path.to_string(),
            hint: None,
        },
        None => Check {
            area: "harness",
            name: name.to_string(),
            level: Level::Info,
            detail: format!("{program} is not on PATH"),
            hint: Some(format!("install {program} to run {name} sessions")),
        },
    }
}

/// A host's runtime program and whether its `manifest` answered as Fleet
/// speaks it.
pub fn runtime(host: &str, runtime: &str, answer: Result<(), String>) -> Check {
    match answer {
        Ok(()) => Check {
            area: "runtime",
            name: host.to_string(),
            level: Level::Ok,
            detail: format!("runtime {runtime} answers"),
            hint: None,
        },
        Err(why) => Check {
            area: "runtime",
            name: host.to_string(),
            level: Level::Fail,
            detail: why,
            hint: Some(format!(
                "install the program [runtimes.{runtime}] names, or fix it; fleet runtime check -- <program> tests it"
            )),
        },
    }
}

/// The fake runtime: nothing to reach.
pub fn fake_runtime() -> Check {
    Check {
        area: "runtime",
        name: "fake".into(),
        level: Level::Ok,
        detail: "the fake runtime serves every host".into(),
        hint: None,
    }
}

/// No runtime in the config: no session can start.
pub fn no_runtime() -> Check {
    Check {
        area: "runtime",
        name: "none".into(),
        level: Level::Fail,
        detail: "no runtime configured".into(),
        hint: Some("run fleet init, or add [runtimes.mux] and [hosts.local] to the config".into()),
    }
}

/// How many checks failed.
pub fn failed(checks: &[Check]) -> usize {
    checks.iter().filter(|c| c.level == Level::Fail).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outdated_and_modified_warn_and_name_their_fix() {
        let outdated = integration("kimi", State::Outdated);
        assert_eq!(outdated.level, Level::Warn);
        assert_eq!(
            outdated.hint.as_deref(),
            Some("fleet integration install kimi")
        );
        let modified = integration("kimi", State::Modified);
        assert_eq!(modified.level, Level::Warn);
        assert!(modified.hint.unwrap().contains("uninstall kimi"));
        assert_eq!(integration("kimi", State::Current).level, Level::Ok);
        assert_eq!(integration("kimi", State::NotInstalled).level, Level::Info);
    }

    #[test]
    fn a_missing_harness_is_information_and_a_dead_runtime_fails() {
        assert_eq!(harness("codex", "codex", None).level, Level::Info);
        assert_eq!(
            harness("codex", "codex", Some("/usr/bin/codex")).level,
            Level::Ok
        );
        let checks = [
            harness("codex", "codex", None),
            integration("kimi", State::Outdated),
            runtime("local", "mux", Err("not found".into())),
        ];
        assert_eq!(failed(&checks), 1);
    }
}
