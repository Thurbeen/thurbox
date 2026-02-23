use crate::session::SessionConfig;

/// Abstraction over different coding agent CLIs (Claude, opencode, etc.).
///
/// Each provider knows how to build CLI arguments from a `SessionConfig`.
/// `Session` delegates command/arg construction to the provider, keeping
/// session lifecycle code agent-agnostic.
pub trait AgentProvider: Send + Sync {
    /// CLI command name (e.g., "claude", "opencode").
    fn command(&self) -> &str;

    /// Build CLI arguments from session config.
    fn build_args(&self, config: &SessionConfig) -> Vec<String>;
}
