#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionState {
    Working,
    Blocked,
    Background,
    Done,
    Idle,
    Stopped,
    Failed,
    Unreachable,
    Running,
    Uncovered,
    Unreported,
}

impl SessionState {
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
