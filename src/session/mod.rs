use std::fmt;
use std::path::PathBuf;

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(Uuid);

impl Default for SessionId {
    fn default() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Idle,
    Error,
}

impl SessionStatus {
    pub fn icon(self) -> &'static str {
        match self {
            Self::Running => "●",
            Self::Idle => "○",
            Self::Error => "✗",
        }
    }
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => write!(f, "Running"),
            Self::Idle => write!(f, "Idle"),
            Self::Error => write!(f, "Error"),
        }
    }
}

pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub status: SessionStatus,
}

impl SessionInfo {
    pub fn new(name: String) -> Self {
        Self {
            id: SessionId::default(),
            name,
            status: SessionStatus::Running,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    pub resume_session_id: Option<String>,
    pub cwd: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_display_is_uuid_format() {
        let id = SessionId::default();
        let display = id.to_string();
        // UUID v4 format: 8-4-4-4-12 hex chars
        assert_eq!(display.len(), 36);
        assert_eq!(display.chars().filter(|&c| c == '-').count(), 4);
    }

    #[test]
    fn session_id_default_is_unique() {
        let id1 = SessionId::default();
        let id2 = SessionId::default();
        assert_ne!(id1, id2);
    }

    #[test]
    fn session_status_display() {
        assert_eq!(SessionStatus::Running.to_string(), "Running");
        assert_eq!(SessionStatus::Idle.to_string(), "Idle");
        assert_eq!(SessionStatus::Error.to_string(), "Error");
    }

    #[test]
    fn session_status_icon() {
        assert_eq!(SessionStatus::Running.icon(), "●");
        assert_eq!(SessionStatus::Idle.icon(), "○");
        assert_eq!(SessionStatus::Error.icon(), "✗");
    }

    #[test]
    fn session_info_new_starts_running() {
        let info = SessionInfo::new("Test".to_string());
        assert_eq!(info.name, "Test");
        assert_eq!(info.status, SessionStatus::Running);
    }
}
