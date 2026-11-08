/// A failure callers can act on without interpreting its message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum OpError {
    NotFound(String),
    Ambiguous(String),
    Usage(String),
    /// The transport failed before a command could be answered.
    Unreached(String),
    /// The command may have run; retrying a write requires reconciliation.
    Undetermined(String),
    Unreachable(String),
    Refused(String),
    Conflict(String),
    Timeout(String),
    Gone(String),
    Other(String),
}

impl OpError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::Ambiguous(_) => "ambiguous",
            Self::Usage(_) => "usage",
            Self::Unreached(_) => "unreached",
            Self::Undetermined(_) => "undetermined",
            Self::Unreachable(_) => "unreachable",
            Self::Refused(_) => "refused",
            Self::Conflict(_) => "conflict",
            Self::Timeout(_) => "timeout",
            Self::Gone(_) => "gone",
            Self::Other(_) => "other",
        }
    }

    /// Read a peer's additive error tag. Unknown and legacy tags stay opaque.
    pub fn from_kind(kind: &str, message: String) -> Self {
        match kind {
            "not_found" => Self::NotFound(message),
            "ambiguous" => Self::Ambiguous(message),
            "usage" => Self::Usage(message),
            "unreached" => Self::Unreached(message),
            "undetermined" => Self::Undetermined(message),
            "unreachable" => Self::Unreachable(message),
            "refused" => Self::Refused(message),
            "conflict" => Self::Conflict(message),
            "timeout" => Self::Timeout(message),
            "gone" => Self::Gone(message),
            _ => Self::Other(message),
        }
    }

    /// Add context without erasing the classification.
    pub fn context(self, context: impl std::fmt::Display) -> Self {
        Self::from_kind(self.kind(), format!("{context}: {self}"))
    }
}

impl std::ops::Deref for OpError {
    type Target = str;
    fn deref(&self) -> &str {
        match self {
            Self::NotFound(m)
            | Self::Ambiguous(m)
            | Self::Usage(m)
            | Self::Unreached(m)
            | Self::Undetermined(m)
            | Self::Unreachable(m)
            | Self::Refused(m)
            | Self::Conflict(m)
            | Self::Timeout(m)
            | Self::Gone(m)
            | Self::Other(m) => m,
        }
    }
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self)
    }
}
impl std::error::Error for OpError {}
impl From<String> for OpError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}
impl From<&str> for OpError {
    fn from(message: &str) -> Self {
        Self::Other(message.to_string())
    }
}
