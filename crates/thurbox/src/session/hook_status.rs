//! A session's state: the one vocabulary every surface prints.
//!
//! Fleet owns a session's state (ADR-41): the snapshot maps a Task's
//! `status.sessionState` onto [`SessionState`]
//! (`kernel::snapshot::fleet`). The words an agent's own hooks report are
//! part of the vocabulary, so a plugin can tell them from what Fleet infers.

/// Every state a session can be in.
///
/// Five are an agent's own hook words (`working`, `blocked`, `background`,
/// `done`, `idle`); the rest are inferred, and each is spelled apart from
/// `idle` so that "nothing reported" never reads as "the agent says it is at
/// rest".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionState {
    /// A turn is running (hook).
    Working,
    /// The agent is waiting on input or approval (hook).
    Blocked,
    /// The turn is at rest and sub-work it started is still running (hook):
    /// claude's background subagents. Not `done`, because the work is not, and
    /// not `working`, because nothing is happening in the pane — a driver that
    /// reads it as finished tears the work down early.
    Background,
    /// A turn finished and nobody has looked at it yet (hook).
    Done,
    /// At rest: acknowledged, never active, or a stored word nothing knows.
    Idle,
    /// Paused: the Task stands, its agent does not run.
    Stopped,
    /// The session is lost or gone: nothing runs, and no report can come.
    Failed,
    /// A remote session whose host cannot be reached.
    Unreachable,
    /// The agent runs, and no hook says what it is doing.
    Running,
    /// This session's agent is wired to report nothing, so its silence means
    /// nothing. Spelling it `idle` would launder "we cannot know" into "the
    /// agent says it is at rest".
    Uncovered,
    /// The agent *can* report and has not yet.
    Unreported,
}

impl SessionState {
    /// Every word, in the order a reader scans them — so a surface that counts
    /// or documents the vocabulary enumerates it rather than restating it.
    pub const ALL: &[SessionState] = &[
        SessionState::Working,
        SessionState::Blocked,
        SessionState::Background,
        SessionState::Done,
        SessionState::Idle,
        SessionState::Stopped,
        SessionState::Failed,
        SessionState::Unreachable,
        SessionState::Running,
        SessionState::Uncovered,
        SessionState::Unreported,
    ];

    /// The stable lowercase word every surface prints.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Working => "working",
            SessionState::Blocked => "blocked",
            SessionState::Background => "background",
            SessionState::Done => "done",
            SessionState::Idle => "idle",
            SessionState::Stopped => "stopped",
            SessionState::Failed => "failed",
            SessionState::Unreachable => "unreachable",
            SessionState::Running => "running",
            SessionState::Uncovered => "uncovered",
            SessionState::Unreported => "unreported",
        }
    }

    /// The state an agent's own hook word names — `None` for any other word.
    pub fn from_hook_state(word: &str) -> Option<Self> {
        match word {
            "working" => Some(SessionState::Working),
            "blocked" => Some(SessionState::Blocked),
            "background" => Some(SessionState::Background),
            "done" => Some(SessionState::Done),
            "idle" => Some(SessionState::Idle),
            _ => None,
        }
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How long a pane may print nothing and still count as printing: a TUI agent
/// animates its in-progress line while a turn runs, so output within this
/// window separates a turn in flight from a prompt waiting for input.
pub const WORKING_QUIET_MS: u64 = 10_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_its_own_word() {
        let words: std::collections::HashSet<&str> =
            SessionState::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(words.len(), SessionState::ALL.len());
        for word in ["working", "blocked", "background", "done", "idle"] {
            assert_eq!(
                SessionState::from_hook_state(word).map(SessionState::as_str),
                Some(word)
            );
        }
        assert_eq!(SessionState::from_hook_state("running"), None);
    }
}
