use crate::agent::provider::AgentProvider;
use crate::session::SessionConfig;

/// Default permission mode passed to the Claude CLI when no explicit mode is configured.
const DEFAULT_PERMISSION_MODE: &str = "default";

/// Agent provider for the Claude Code CLI.
pub struct ClaudeProvider;

impl AgentProvider for ClaudeProvider {
    fn command(&self) -> &str {
        "claude"
    }

    fn build_args(&self, config: &SessionConfig) -> Vec<String> {
        build_claude_args(config)
    }
}

/// Build the CLI argument list from a SessionConfig.
///
/// This is extracted as a pure function for testability.
fn build_claude_args(config: &SessionConfig) -> Vec<String> {
    let mut args = Vec::new();

    if let Some(ref fork_id) = config.fork_session_id {
        args.push("--resume".to_string());
        args.push(fork_id.clone());
        args.push("--fork-session".to_string());
    } else if let Some(ref session_id) = config.resume_session_id {
        args.push("--resume".to_string());
        args.push(session_id.clone());
    } else if let Some(ref session_id) = config.agent_session_id {
        args.push("--session-id".to_string());
        args.push(session_id.clone());
    }

    // Role permission flags — default to "default" when no mode is configured.
    let mode = config
        .permissions
        .permission_mode
        .as_deref()
        .unwrap_or(DEFAULT_PERMISSION_MODE);
    args.push("--permission-mode".to_string());
    args.push(mode.to_string());
    if !config.permissions.allowed_tools.is_empty() {
        args.push("--allowed-tools".to_string());
        args.push(config.permissions.allowed_tools.join(" "));
    }
    if !config.permissions.disallowed_tools.is_empty() {
        args.push("--disallowed-tools".to_string());
        args.push(config.permissions.disallowed_tools.join(" "));
    }
    if let Some(ref tools) = config.permissions.tools {
        args.push("--tools".to_string());
        args.push(tools.clone());
    }
    if let Some(ref prompt) = config.permissions.append_system_prompt {
        args.push("--append-system-prompt".to_string());
        args.push(prompt.clone());
    }

    for dir in &config.additional_dirs {
        args.push("--add-dir".to_string());
        args.push(dir.display().to_string());
    }

    args
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::session::RolePermissions;

    #[test]
    fn build_args_empty_config() {
        let config = SessionConfig::default();
        let args = build_claude_args(&config);
        assert_eq!(args, vec!["--permission-mode", "default"]);
    }

    #[test]
    fn build_args_no_permissions() {
        let config = SessionConfig {
            agent_session_id: Some("abc-123".to_string()),
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec!["--session-id", "abc-123", "--permission-mode", "default"]
        );
    }

    #[test]
    fn build_args_resume_takes_precedence() {
        let config = SessionConfig {
            resume_session_id: Some("resume-id".to_string()),
            agent_session_id: Some("session-id".to_string()),
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec!["--resume", "resume-id", "--permission-mode", "default"]
        );
    }

    #[test]
    fn build_args_fork_session() {
        let config = SessionConfig {
            fork_session_id: Some("parent-id".to_string()),
            agent_session_id: Some("new-id".to_string()),
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec![
                "--resume",
                "parent-id",
                "--fork-session",
                "--permission-mode",
                "default"
            ]
        );
    }

    #[test]
    fn build_args_fork_takes_precedence_over_resume() {
        let config = SessionConfig {
            fork_session_id: Some("fork-id".to_string()),
            resume_session_id: Some("resume-id".to_string()),
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec![
                "--resume",
                "fork-id",
                "--fork-session",
                "--permission-mode",
                "default"
            ]
        );
    }

    #[test]
    fn build_args_with_permission_mode() {
        let config = SessionConfig {
            permissions: RolePermissions {
                permission_mode: Some("plan".to_string()),
                ..RolePermissions::default()
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(args, vec!["--permission-mode", "plan"]);
    }

    #[test]
    fn build_args_with_allowed_tools() {
        let config = SessionConfig {
            permissions: RolePermissions {
                allowed_tools: vec!["Read".to_string(), "Bash(git:*)".to_string()],
                ..RolePermissions::default()
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec![
                "--permission-mode",
                "default",
                "--allowed-tools",
                "Read Bash(git:*)"
            ]
        );
    }

    #[test]
    fn build_args_with_disallowed_tools() {
        let config = SessionConfig {
            permissions: RolePermissions {
                disallowed_tools: vec!["Edit".to_string()],
                ..RolePermissions::default()
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec!["--permission-mode", "default", "--disallowed-tools", "Edit"]
        );
    }

    #[test]
    fn build_args_with_tools_empty_string() {
        let config = SessionConfig {
            permissions: RolePermissions {
                tools: Some(String::new()),
                ..RolePermissions::default()
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(args, vec!["--permission-mode", "default", "--tools", ""]);
    }

    #[test]
    fn build_args_with_system_prompt() {
        let config = SessionConfig {
            permissions: RolePermissions {
                append_system_prompt: Some("Be careful".to_string()),
                ..RolePermissions::default()
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec![
                "--permission-mode",
                "default",
                "--append-system-prompt",
                "Be careful"
            ]
        );
    }

    #[test]
    fn build_args_with_additional_dirs() {
        let config = SessionConfig {
            additional_dirs: vec![
                PathBuf::from("/home/user/repo2"),
                PathBuf::from("/home/user/repo3"),
            ],
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec![
                "--permission-mode",
                "default",
                "--add-dir",
                "/home/user/repo2",
                "--add-dir",
                "/home/user/repo3",
            ]
        );
    }

    #[test]
    fn build_args_all_fields() {
        let config = SessionConfig {
            agent_session_id: Some("id-1".to_string()),
            additional_dirs: vec![PathBuf::from("/extra")],
            permissions: RolePermissions {
                permission_mode: Some("plan".to_string()),
                allowed_tools: vec!["Read".to_string()],
                disallowed_tools: vec!["Edit".to_string()],
                tools: Some("default".to_string()),
                append_system_prompt: Some("Focus".to_string()),
                ..RolePermissions::default()
            },
            ..SessionConfig::default()
        };
        let args = build_claude_args(&config);
        assert_eq!(
            args,
            vec![
                "--session-id",
                "id-1",
                "--permission-mode",
                "plan",
                "--allowed-tools",
                "Read",
                "--disallowed-tools",
                "Edit",
                "--tools",
                "default",
                "--append-system-prompt",
                "Focus",
                "--add-dir",
                "/extra",
            ]
        );
    }

    #[test]
    fn provider_command_is_claude() {
        let provider = ClaudeProvider;
        assert_eq!(provider.command(), "claude");
    }

    #[test]
    fn provider_delegates_to_build_claude_args() {
        let provider = ClaudeProvider;
        let config = SessionConfig::default();
        assert_eq!(provider.build_args(&config), build_claude_args(&config));
    }
}
