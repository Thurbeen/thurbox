//! Command-line interface dispatcher for the `thurbox-cli` binary.
//!
//! Each subcommand mirrors a tool exposed by the MCP server. Output is JSON
//! matching the MCP tool result shape. Pass `--pretty` to pretty-print.
//!
//! The CLI is intentionally thin: it parses arguments, calls into
//! `storage::Database`, `session_ops`, or the tmux helpers in
//! `agent::tmux`, and prints the result. No TUI, no event loop.

use clap::{Parser, Subcommand};

use crate::storage::Database;

pub mod containerfiles;
pub mod editor;
pub mod mcp_servers;
pub mod roles;
pub mod scheduled;
pub mod sessions;
pub mod skills;
pub mod vm_images;
pub mod vms;

/// Thurbox CLI — manage roles, sessions, scheduled commands, and more.
#[derive(Parser, Debug)]
#[command(name = "thurbox-cli", version, about)]
pub struct Cli {
    /// Pretty-print JSON output.
    #[arg(long, global = true)]
    pub pretty: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage global roles.
    Role {
        #[command(subcommand)]
        action: roles::Action,
    },
    /// Manage global MCP servers.
    #[command(name = "mcp-server")]
    McpServer {
        #[command(subcommand)]
        action: mcp_servers::Action,
    },
    /// Get/set the editor command (Ctrl+O in the TUI).
    Editor {
        #[command(subcommand)]
        action: editor::Action,
    },
    /// Manage sessions.
    Session {
        #[command(subcommand)]
        action: sessions::Action,
    },
    /// Manage scheduled commands.
    Schedule {
        #[command(subcommand)]
        action: scheduled::Action,
    },
    /// Manage VMs (read-only).
    Vm {
        #[command(subcommand)]
        action: vms::Action,
    },
    /// Manage cached VM images.
    #[command(name = "vm-image")]
    VmImage {
        #[command(subcommand)]
        action: vm_images::Action,
    },
    /// Manage Containerfile templates.
    Containerfile {
        #[command(subcommand)]
        action: containerfiles::Action,
    },
    /// Manage the skill registry (references to on-disk skill directories).
    Skill {
        #[command(subcommand)]
        action: skills::Action,
    },
}

/// Run a parsed CLI invocation against `db` and write JSON to stdout.
pub fn run(cli: Cli, db: &Database) -> Result<(), String> {
    let value = match cli.command {
        Command::Role { action } => roles::run(action, db),
        Command::McpServer { action } => mcp_servers::run(action, db),
        Command::Editor { action } => editor::run(action, db),
        Command::Session { action } => sessions::run(action, db),
        Command::Schedule { action } => scheduled::run(action, db),
        Command::Vm { action } => vms::run(action, db),
        Command::VmImage { action } => vm_images::run(action),
        Command::Containerfile { action } => containerfiles::run(action),
        Command::Skill { action } => skills::run(action, db),
    }?;

    let text = if cli.pretty {
        serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    } else {
        serde_json::to_string(&value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    };
    println!("{text}");
    Ok(())
}

// ── Shared helpers ─────────────────────────────────────────────────

/// Read a file's contents as a UTF-8 string, returning a CLI-style error.
pub(crate) fn read_file(path: &std::path::Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))
}

/// Parse a JSON document that may be either a bare array or an object
/// with a single `wrapper_key` field whose value is an array. Returns the
/// array items. Used by `role set` / `mcp-server set` CLI commands.
pub(crate) fn parse_array_or_wrapper(
    content: &str,
    wrapper_key: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("Failed to parse JSON: {e}"))?;
    let err = || format!("JSON must be an array or {{\"{wrapper_key}\":[...]}}");
    match value {
        serde_json::Value::Array(a) => Ok(a),
        serde_json::Value::Object(mut o) => match o.remove(wrapper_key) {
            Some(serde_json::Value::Array(a)) => Ok(a),
            _ => Err(err()),
        },
        _ => Err(err()),
    }
}

/// Validate that `name` is a safe single-segment filename (no slashes,
/// dot-prefix, or `..`) — used by VM-image and Containerfile commands.
pub(crate) fn validate_safe_name(name: &str) -> Result<(), String> {
    crate::paths::validate_safe_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_role_list() {
        let cli = Cli::try_parse_from(["thurbox-cli", "role", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Role {
                action: roles::Action::List
            }
        ));
        assert!(!cli.pretty);
    }

    #[test]
    fn pretty_flag_is_global() {
        let cli = Cli::try_parse_from(["thurbox-cli", "session", "list", "--pretty"]).unwrap();
        assert!(cli.pretty);
        assert!(matches!(
            cli.command,
            Command::Session {
                action: sessions::Action::List
            }
        ));
    }

    #[test]
    fn parse_session_create_requires_name_and_repo() {
        // Missing required args fails.
        assert!(Cli::try_parse_from(["thurbox-cli", "session", "create"]).is_err());

        // Full happy path.
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "session",
            "create",
            "--name",
            "demo",
            "--repo-path",
            "/tmp/repo",
            "--worktree-branch",
            "feat/x",
            "--mcp-server",
            "fs",
            "--mcp-server",
            "github",
            "--skill",
            "review",
        ])
        .unwrap();
        let Command::Session {
            action:
                sessions::Action::Create {
                    name,
                    repo_path,
                    mcp_servers,
                    skills,
                    worktree_branch,
                    ..
                },
        } = cli.command
        else {
            panic!("expected Session::Create");
        };
        assert_eq!(name, "demo");
        assert_eq!(repo_path.to_string_lossy(), "/tmp/repo");
        assert_eq!(worktree_branch.as_deref(), Some("feat/x"));
        assert_eq!(mcp_servers, vec!["fs", "github"]);
        assert_eq!(skills, vec!["review"]);
    }

    #[test]
    fn parse_editor_set_and_get() {
        let cli = Cli::try_parse_from(["thurbox-cli", "editor", "get"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Editor {
                action: editor::Action::Get
            }
        ));
        let cli = Cli::try_parse_from(["thurbox-cli", "editor", "set", "code --wait"]).unwrap();
        let Command::Editor {
            action: editor::Action::Set { command },
        } = cli.command
        else {
            panic!("expected Editor::Set");
        };
        assert_eq!(command, "code --wait");
    }

    #[test]
    fn parse_schedule_create_requires_args() {
        assert!(
            Cli::try_parse_from(["thurbox-cli", "schedule", "create"]).is_err(),
            "missing required args should fail"
        );
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "schedule",
            "create",
            "--session",
            "00000000-0000-0000-0000-000000000000",
            "--at",
            "9999999999999",
            "--command",
            "ls",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Schedule {
                action: scheduled::Action::Create { .. }
            }
        ));
    }

    #[test]
    fn parse_vm_image_download() {
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "vm-image",
            "download",
            "https://example.com/img.qcow2",
            "--filename",
            "img.qcow2",
        ])
        .unwrap();
        let Command::VmImage {
            action:
                vm_images::Action::Download {
                    url,
                    filename: Some(f),
                },
        } = cli.command
        else {
            panic!("expected VmImage::Download with filename");
        };
        assert_eq!(url, "https://example.com/img.qcow2");
        assert_eq!(f, "img.qcow2");
    }

    #[test]
    fn parse_containerfile_set_with_support_files() {
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "containerfile",
            "set",
            "mytpl",
            "--file",
            "/tmp/Containerfile",
            "--support-file",
            "entrypoint.sh=/tmp/entry.sh",
        ])
        .unwrap();
        let Command::Containerfile {
            action:
                containerfiles::Action::Set {
                    name,
                    file,
                    support,
                },
        } = cli.command
        else {
            panic!("expected Containerfile::Set");
        };
        assert_eq!(name, "mytpl");
        assert_eq!(file.to_string_lossy(), "/tmp/Containerfile");
        assert_eq!(support.files, vec!["entrypoint.sh=/tmp/entry.sh"]);
    }

    #[test]
    fn validate_safe_name_basics() {
        assert!(validate_safe_name("ok-name").is_ok());
        assert!(validate_safe_name("").is_err());
        assert!(validate_safe_name(".dot").is_err());
        assert!(validate_safe_name("a/b").is_err());
        assert!(validate_safe_name(&"x".repeat(65)).is_err());
    }
}
