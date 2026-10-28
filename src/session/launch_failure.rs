//! A launch that died before its agent ever said anything.
//!
//! `remain-on-exit` keeps a dead pane's frame, and a session whose agent never
//! fired a hook reads `unreported` — "can report, has not yet" — for as long as
//! the row stands. For a pane whose program has already exited that word is a
//! promise nothing will keep: a driver waiting for the first report waits
//! forever (a Fleet task waited twelve hours on a wrapper that had printed
//! `error: role 'debugger' not found` and exited 1).
//!
//! So a pane found dead with no lifecycle evidence from its agent is recorded
//! as a [`LaunchFailure`], once, and every surface reads the session as
//! [`SessionState::Failed`](super::SessionState::Failed) from then on. It is
//! stored as session meta under [`LAUNCH_FAILURE_META`] and cleared by the next
//! launch (restart, `session start`).
//!
//! The record is a contract: `session get`/`list --json` publish it as
//! `launch_failure` with exactly the fields of [`LaunchFailure`], and
//! `docs/ORCHESTRATION.md` documents them for drivers.

use serde::{Deserialize, Serialize};

use super::agent_def::SETUP_FAILURE_MARKER;

/// The session meta key the record is stored under — inside the reserved
/// `thurbox.` namespace, so no driver can forge or clear one.
pub const LAUNCH_FAILURE_META: &str = "thurbox.launch_failure";

/// How many of the dead pane's last lines a record keeps.
pub const LAUNCH_FAILURE_TAIL_LINES: u32 = 20;

/// Which half of the launch died.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchFailureReason {
    /// A step of the agent's `setup` (agents.toml) exited non-zero, so the
    /// agent was never started.
    Setup,
    /// The agent's process — or a wrapper standing in for it — exited before
    /// any hook reported from it.
    Exited,
}

impl LaunchFailureReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Exited => "exited",
        }
    }

    /// Which half died, from the pane's last lines: the setup script prints
    /// [`SETUP_FAILURE_MARKER`] on its way out, and nothing else does.
    pub fn from_output(output: Option<&str>) -> Self {
        match output {
            Some(text) if text.contains(SETUP_FAILURE_MARKER) => Self::Setup,
            _ => Self::Exited,
        }
    }
}

/// One recorded launch failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchFailure {
    pub reason: LaunchFailureReason,
    /// The pane program's exit status; `None` when the multiplexer could not
    /// say (killed by a signal, or a multiplexer without `pane_dead_status`).
    pub exit_code: Option<i64>,
    /// The pane's last [`LAUNCH_FAILURE_TAIL_LINES`] lines, trailing blank
    /// lines trimmed; `None` when the screen could not be read or was empty.
    pub output: Option<String>,
    /// The pane that died. `remain-on-exit` keeps it on screen until the
    /// session is relaunched, so `thurbox-cli session capture` reads more of
    /// it than [`Self::output`] holds. Also what keeps the record honest: a
    /// relaunch puts the row on another pane, and a record naming a pane the
    /// row no longer points at describes a launch that is over.
    pub pane: String,
    /// Epoch ms the failure was recorded.
    pub at: i64,
}

impl LaunchFailure {
    /// A failure observed now, classified from its output.
    pub fn observed(pane: &str, exit_code: Option<i64>, output: Option<String>, at: i64) -> Self {
        let output = output
            .map(|text| text.trim_end().to_string())
            .filter(|text| !text.is_empty());
        Self {
            reason: LaunchFailureReason::from_output(output.as_deref()),
            exit_code,
            output,
            pane: pane.to_string(),
            at,
        }
    }

    /// The stored meta value. Unreadable text is no record, which leaves the
    /// session reading as it would have without one.
    pub fn parse(stored: &str) -> Option<Self> {
        serde_json::from_str(stored).ok()
    }

    pub fn to_meta(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setup_marker_decides_the_reason() {
        let setup = LaunchFailure::observed(
            "%1",
            Some(3),
            Some(format!(
                "error: role 'debugger' not found\n\n{SETUP_FAILURE_MARKER}2 failed (exit 3)\n\n"
            )),
            1,
        );
        assert_eq!(setup.reason, LaunchFailureReason::Setup);
        assert!(setup.output.as_deref().unwrap().ends_with("(exit 3)"));

        let exited = LaunchFailure::observed(
            "%1",
            Some(1),
            Some("error: role 'debugger' not found".into()),
            1,
        );
        assert_eq!(exited.reason, LaunchFailureReason::Exited);
        assert_eq!(
            LaunchFailure::observed("%1", None, Some("  \n".into()), 1).output,
            None
        );
    }

    #[test]
    fn the_meta_value_round_trips_with_its_documented_fields() {
        let failure = LaunchFailure::observed("%7", Some(1), Some("boom".into()), 42);
        let stored = failure.to_meta();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stored).unwrap(),
            serde_json::json!({
                "reason": "exited",
                "exit_code": 1,
                "output": "boom",
                "pane": "%7",
                "at": 42
            })
        );
        assert_eq!(LaunchFailure::parse(&stored), Some(failure));
        assert_eq!(LaunchFailure::parse("not json"), None);
    }
}
