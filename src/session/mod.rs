use std::fmt;

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
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
            id: SessionId::new(),
            name,
            status: SessionStatus::Running,
        }
    }

    pub fn status_icon(&self) -> &'static str {
        match self.status {
            SessionStatus::Running => "●",
            SessionStatus::Idle => "○",
            SessionStatus::Error => "✗",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    pub resume_session_id: Option<String>,
}
