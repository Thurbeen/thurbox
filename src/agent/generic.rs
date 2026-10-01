//! Data-driven [`AgentProvider`] built from an [`AgentDef`].
//!
//! Replaces the former hard-coded `ClaudeProvider`: any agent described in
//! `agents.toml` is launched through this single provider, which maps the
//! session's resume/fork/session ids onto the definition's argument templates.

use crate::session::{AgentDef, SessionConfig};

use super::provider::AgentProvider;

/// An [`AgentProvider`] backed by a declarative [`AgentDef`].
pub struct GenericProvider {
    def: AgentDef,
}

impl GenericProvider {
    /// Wrap an agent definition.
    pub fn new(def: AgentDef) -> Self {
        Self { def }
    }

    /// The underlying definition.
    pub fn def(&self) -> &AgentDef {
        &self.def
    }
}

impl AgentProvider for GenericProvider {
    fn command(&self) -> &str {
        &self.def.command
    }

    fn build_args(&self, config: &SessionConfig) -> Vec<String> {
        self.def.build_args(
            config.resume_session_id.as_deref(),
            config.fork_session_id.as_deref(),
            config.agent_session_id.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::AgentDef;

    /// Claude's arg groups, as `agents.toml` declares them: the provider is
    /// tested against a definition, not against the registry that loads one.
    fn claude() -> AgentDef {
        AgentDef {
            name: "claude".into(),
            command: "claude".into(),
            resume_args: vec!["--resume".into(), "{id}".into()],
            new_session_args: vec!["--session-id".into(), "{id}".into()],
            ..AgentDef::default()
        }
    }

    #[test]
    fn claude_provider_builds_resume_without_model() {
        let provider = GenericProvider::new(claude());
        assert_eq!(provider.command(), "claude");

        let config = SessionConfig {
            resume_session_id: Some("abc-123".into()),
            ..SessionConfig::default()
        };
        let args = provider.build_args(&config);
        assert_eq!(args, vec!["--resume", "abc-123"]);
        assert!(!args.iter().any(|a| a == "--model"));
    }

    #[test]
    fn fresh_session_pins_id_no_model() {
        let provider = GenericProvider::new(claude());

        let config = SessionConfig {
            agent_session_id: Some("new-id".into()),
            ..SessionConfig::default()
        };
        let args = provider.build_args(&config);
        assert_eq!(args, vec!["--session-id", "new-id"]);
    }
}
