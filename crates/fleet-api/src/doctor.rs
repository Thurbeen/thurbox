//! `fleet doctor`: what each check found, classified as ok, info, warn or
//! fail, with the one command or step that fixes it. The CLI observes; this
//! module decides. Only a fail makes the command exit non-zero.
//!
//! The checks are in docs/fleet/contracts.md, "The command line".

use serde::Serialize;

use crate::harness::Profiles;
use crate::integration::{Ledger, State};

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
                "fleet integration uninstall {agent}, then fleet integration install {agent}; when uninstall cannot find the block's boundary lines, remove the block by hand first"
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

/// Each ledger entry no profile installs any more: the profile was removed,
/// or lost its `[install]` table. Its files stay on disk until uninstalled,
/// and the ledger lists exactly what to remove.
pub fn left_behind(ledger: &Ledger, profiles: &Profiles) -> Vec<Check> {
    ledger
        .integrations
        .iter()
        .filter(|(agent, _)| {
            !profiles
                .list()
                .iter()
                .any(|p| &p.name == *agent && p.install.is_some())
        })
        .map(|(agent, entry)| Check {
            area: "integration",
            name: agent.clone(),
            level: Level::Warn,
            detail: format!(
                "left behind: no harness profile installs it; its files are in {}",
                entry.dir
            ),
            hint: Some(format!("fleet integration uninstall {agent}")),
        })
        .collect()
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

/// The `[runtime]` table: the fake, which Fleet opens only with its `dir`.
/// Any other kind is refused when a command opens the runtime.
pub fn fake_runtime(kind: &str, has_dir: bool) -> Check {
    let (level, detail, hint) = match (kind, has_dir) {
        ("fake", true) => (Level::Ok, "the fake runtime serves every host".into(), None),
        ("fake", false) => (
            Level::Fail,
            "runtime.dir is required for the fake runtime".into(),
            Some("set dir in [runtime]".into()),
        ),
        (other, _) => (
            Level::Fail,
            format!("unknown runtime kind {other:?}"),
            Some("[runtime] is the fake; a real runtime is a program under [runtimes]".into()),
        ),
    };
    Check {
        area: "runtime",
        name: "fake".into(),
        level,
        detail,
        hint,
    }
}

/// No runtime in the config: no session can start.
pub fn no_runtime() -> Check {
    Check {
        area: "runtime",
        name: "none".into(),
        level: Level::Fail,
        detail: "no runtime configured".into(),
        hint: Some("run fleet init, or add [runtimes.mux] to the config".into()),
    }
}

/// The host `local` names no place, for `why`: a warning, since a config
/// of other places alone is whole.
pub fn no_local_place(why: &str) -> Check {
    Check {
        area: "runtime",
        name: "local".into(),
        level: Level::Warn,
        detail: why.to_string(),
        hint: Some("a Task names one of the places `fleet runtime places` lists".into()),
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
        assert!(fake_runtime("fake", true).level == Level::Ok);
        assert!(fake_runtime("fake", false).level == Level::Fail);
        assert!(fake_runtime("other", true).level == Level::Fail);
        assert_eq!(integration("kimi", State::Current).level, Level::Ok);
        assert_eq!(integration("kimi", State::NotInstalled).level, Level::Info);
    }

    #[test]
    fn an_entry_no_profile_installs_is_left_behind() {
        let entry = |dir: &str| crate::integration::Entry {
            version: 1,
            dir: dir.into(),
            files: Vec::new(),
            blocks: Vec::new(),
            dirs: Vec::new(),
        };
        let mut ledger = Ledger::default();
        ledger
            .integrations
            .insert("gone".into(), entry("/home/u/.gone"));
        let installs = Profiles::bundled()
            .list()
            .iter()
            .find(|p| p.install.is_some())
            .expect("a bundled profile with [install]")
            .name
            .clone();
        ledger
            .integrations
            .insert(installs, entry("/home/u/.agent"));
        let checks = left_behind(&ledger, &Profiles::bundled());
        assert_eq!(checks.len(), 1, "{checks:?}");
        assert_eq!(checks[0].name, "gone");
        assert_eq!(checks[0].level, Level::Warn);
        assert!(checks[0].detail.contains("/home/u/.gone"));
        assert_eq!(
            checks[0].hint.as_deref(),
            Some("fleet integration uninstall gone")
        );
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
