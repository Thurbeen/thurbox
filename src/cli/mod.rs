//! Command-line interface dispatcher for the `thurbox-cli` binary.
//!
//! Output is human-readable by default and switches to JSON automatically when
//! stdout is a pipe (so `thurbox-cli … | jq` keeps working). Force a format with
//! `--json` (compact), `--pretty` (indented JSON), or `--text` (human). See
//! [`output::Format`].
//!
//! The CLI is intentionally thin: it parses arguments, calls into
//! `storage::Database`, `session_ops`, or the tmux helpers in
//! `agent::tmux`, and prints the result. No TUI, no event loop.

use clap::{Parser, Subcommand};

use crate::storage::Database;

pub mod automations;
pub mod config;
pub mod editor;
pub mod extensions;
pub mod messages;
pub mod output;
pub mod sessions;
pub mod tasks;
pub mod version;

use output::{CommandOutput, Format};

/// Thurbox CLI — manage sessions, scheduled commands, and more.
#[derive(Parser, Debug)]
#[command(name = "thurbox-cli", version, about)]
pub struct Cli {
    /// Output JSON instead of the human-readable default.
    #[arg(long, global = true)]
    pub json: bool,

    /// Pretty-print JSON output (implies --json).
    #[arg(long, global = true)]
    pub pretty: bool,

    /// Force human-readable output even when piped.
    ///
    /// `id = "text_format"` disambiguates from subcommand positional args also
    /// named `text` (e.g. `sessions::Action::Send`). Without the explicit id,
    /// clap registers two args with id "text" (this bool flag and the String
    /// positional), and `get_one::<String>("text")` at parse time panics with
    /// a TypeId downcast mismatch.
    #[arg(long, global = true, id = "text_format")]
    pub text: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
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
    /// Manage automations (scheduled agent runs).
    #[command(alias = "auto")]
    Automation {
        #[command(subcommand)]
        action: automations::Action,
    },
    /// Manage tasks (todo list).
    #[command(alias = "todo")]
    Task {
        #[command(subcommand)]
        action: tasks::Action,
    },
    /// Send/read inter-session messages (the mailbox queue).
    #[command(alias = "msg")]
    Message {
        #[command(subcommand)]
        action: messages::Action,
    },
    /// Validate or inspect the config files.
    Config {
        #[command(subcommand)]
        action: config::Action,
    },
    /// Activate/deactivate opt-in extensions (e.g. flow).
    #[command(alias = "ext")]
    Extension {
        #[command(subcommand)]
        action: extensions::Action,
    },
    /// Print the version; `--check` queries GitHub for a newer release.
    Version(version::VersionArgs),
}

/// Run a parsed CLI invocation against `db`, rendering the result in the
/// resolved [`Format`] (human by default, JSON when piped or forced).
pub fn run(cli: Cli, db: &Database) -> Result<(), String> {
    let format = Format::resolve(cli.json, cli.text, cli.pretty);
    let output: CommandOutput = match cli.command {
        Command::Editor { action } => editor::run(action, db),
        Command::Session { action } => sessions::run(action, db),
        Command::Automation { action } => automations::run(action, db),
        Command::Task { action } => tasks::run(action, db),
        Command::Message { action } => messages::run(action, db),
        Command::Config { action } => config::run(action, db),
        Command::Extension { action } => extensions::run(action, db),
        Command::Version(args) => Ok(version::run(args)),
    }?;

    println!("{}", format.render(&output));
    // A command can render normally yet still request a non-zero exit (e.g.
    // `config validate` on an invalid file).
    match output.failure {
        Some(msg) => Err(msg),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn pretty_flag_is_global() {
        let cli = Cli::try_parse_from(["thurbox-cli", "session", "list", "--pretty"]).unwrap();
        assert!(cli.pretty);
        assert!(matches!(
            cli.command,
            Command::Session {
                action: sessions::Action::List { parent: None }
            }
        ));
    }

    #[test]
    fn json_and_text_flags_are_global() {
        let cli = Cli::try_parse_from(["thurbox-cli", "task", "list", "--json"]).unwrap();
        assert!(cli.json);
        assert!(!cli.text);
        let cli = Cli::try_parse_from(["thurbox-cli", "--text", "task", "list"]).unwrap();
        assert!(cli.text);
        assert!(!cli.json);
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
            "--agent",
            "codex",
        ])
        .unwrap();
        let Command::Session {
            action:
                sessions::Action::Create {
                    name,
                    repo_path,
                    agent,
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
        assert_eq!(agent.as_deref(), Some("codex"));
    }

    #[test]
    fn parse_session_create_accepts_parent() {
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "session",
            "create",
            "--name",
            "worker",
            "--repo-path",
            "/tmp/repo",
            "--parent",
            "0f4dec1e-9d4b-4c4f-9d05-3a3a3a3a3a3a",
        ])
        .unwrap();
        let Command::Session {
            action: sessions::Action::Create { parent, .. },
        } = cli.command
        else {
            panic!("expected Session::Create");
        };
        assert_eq!(
            parent.as_deref(),
            Some("0f4dec1e-9d4b-4c4f-9d05-3a3a3a3a3a3a")
        );
    }

    #[test]
    fn parse_session_send_disambiguates_global_text_flag() {
        // Regression: the global `--text` flag (bool) and the `text: String`
        // positional in `sessions::Action::Send` both default to clap arg id
        // "text", which causes a TypeId-mismatch panic at parse time when
        // constructing Send. The global flag uses `id = "text_format"` to
        // disambiguate; this test fails-to-compile-or-panics if either side
        // regresses back to the colliding id.
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "session",
            "send",
            "0f4dec1e-9d4b-4c4f-9d05-3a3a3a3a3a3a",
            "hello",
        ])
        .unwrap();
        let Command::Session {
            action: sessions::Action::Send { uuid, text },
        } = cli.command
        else {
            panic!("expected Session::Send");
        };
        assert_eq!(uuid, "0f4dec1e-9d4b-4c4f-9d05-3a3a3a3a3a3a");
        assert_eq!(text, "hello");

        // Same subcommand, with the global `--text` flag set — the original
        // collision-triggering invocation. Must still parse and set both.
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "--text",
            "session",
            "send",
            "0f4dec1e-9d4b-4c4f-9d05-3a3a3a3a3a3a",
            "hello",
        ])
        .unwrap();
        assert!(cli.text);
        let Command::Session {
            action: sessions::Action::Send { text, .. },
        } = cli.command
        else {
            panic!("expected Session::Send");
        };
        assert_eq!(text, "hello");
    }

    #[test]
    fn parse_session_list_accepts_parent_filter() {
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "session",
            "list",
            "--parent",
            "0f4dec1e-9d4b-4c4f-9d05-3a3a3a3a3a3a",
        ])
        .unwrap();
        let Command::Session {
            action: sessions::Action::List { parent },
        } = cli.command
        else {
            panic!("expected Session::List");
        };
        assert_eq!(
            parent.as_deref(),
            Some("0f4dec1e-9d4b-4c4f-9d05-3a3a3a3a3a3a")
        );
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
    fn parse_automation_create_requires_args() {
        assert!(
            Cli::try_parse_from(["thurbox-cli", "automation", "create"]).is_err(),
            "missing required args should fail"
        );
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "automation",
            "create",
            "--name",
            "nightly",
            "--trigger",
            "weekdays",
            "--time",
            "09:00",
            "--session",
            "00000000-0000-0000-0000-000000000000",
            "--prompt",
            "triage",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Automation {
                action: automations::Action::Create { .. }
            }
        ));
    }

    #[test]
    fn automation_alias_auto_parses() {
        let cli = Cli::try_parse_from(["thurbox-cli", "auto", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Automation {
                action: automations::Action::List
            }
        ));
    }

    #[test]
    fn automation_tick_parses() {
        let cli = Cli::try_parse_from(["thurbox-cli", "automation", "tick"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Automation {
                action: automations::Action::Tick
            }
        ));
    }

    #[test]
    fn parse_task_create_requires_title() {
        assert!(
            Cli::try_parse_from(["thurbox-cli", "task", "create"]).is_err(),
            "missing --title should fail"
        );
        // A plain local todo needs only a title.
        let cli =
            Cli::try_parse_from(["thurbox-cli", "task", "create", "--title", "Fix bug"]).unwrap();
        let Command::Task {
            action:
                tasks::Action::Create {
                    title,
                    session,
                    repo,
                    ..
                },
        } = cli.command
        else {
            panic!("expected Task::Create");
        };
        assert_eq!(title, "Fix bug");
        assert!(session.is_none());
        assert!(repo.is_none());
    }

    #[test]
    fn parse_task_create_accepts_description() {
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "task",
            "create",
            "--title",
            "Doc me",
            "--description",
            "# Notes\n- item",
        ])
        .unwrap();
        let Command::Task {
            action: tasks::Action::Create { description, .. },
        } = cli.command
        else {
            panic!("expected Task::Create");
        };
        assert_eq!(description.as_deref(), Some("# Notes\n- item"));
    }

    #[test]
    fn parse_task_edit_accepts_description() {
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "task",
            "edit",
            "3",
            "--description",
            "updated",
        ])
        .unwrap();
        let Command::Task {
            action: tasks::Action::Edit {
                id, description, ..
            },
        } = cli.command
        else {
            panic!("expected Task::Edit");
        };
        assert_eq!(id, 3);
        assert_eq!(description.as_deref(), Some("updated"));
    }

    #[test]
    fn task_alias_todo_parses() {
        let cli = Cli::try_parse_from(["thurbox-cli", "todo", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Task {
                action: tasks::Action::List
            }
        ));
    }

    #[test]
    fn parse_extension_install() {
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "extension",
            "install",
            "flow",
            "--home",
            "/home/me/flow",
            "--force",
        ])
        .unwrap();
        let Command::Extension {
            action:
                extensions::Action::Install {
                    target,
                    home,
                    force,
                },
        } = cli.command
        else {
            panic!("expected Extension::Install");
        };
        assert_eq!(target, "flow");
        assert_eq!(home.as_deref(), Some("/home/me/flow"));
        assert!(force);
    }

    #[test]
    fn parse_extension_uninstall() {
        let cli = Cli::try_parse_from(["thurbox-cli", "extension", "uninstall", "flow", "--purge"])
            .unwrap();
        let Command::Extension {
            action: extensions::Action::Uninstall { name, purge },
        } = cli.command
        else {
            panic!("expected Extension::Uninstall");
        };
        assert_eq!(name, "flow");
        assert!(purge);
    }

    #[test]
    fn parse_extension_activate() {
        let cli = Cli::try_parse_from(["thurbox-cli", "extension", "activate", "flow"]).unwrap();
        let Command::Extension {
            action: extensions::Action::Activate { name },
        } = cli.command
        else {
            panic!("expected Extension::Activate");
        };
        assert_eq!(name, "flow");
    }

    #[test]
    fn parse_extension_deactivate_with_flags() {
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "extension",
            "deactivate",
            "flow",
            "--force",
            "--purge",
        ])
        .unwrap();
        let Command::Extension {
            action: extensions::Action::Deactivate { name, force, purge },
        } = cli.command
        else {
            panic!("expected Extension::Deactivate");
        };
        assert_eq!(name, "flow");
        assert!(force);
        assert!(purge);
    }

    #[test]
    fn parse_extension_update() {
        let cli = Cli::try_parse_from(["thurbox-cli", "extension", "update", "flow"]).unwrap();
        let Command::Extension {
            action: extensions::Action::Update { name, all, force },
        } = cli.command
        else {
            panic!("expected Extension::Update");
        };
        assert_eq!(name.as_deref(), Some("flow"));
        assert!(!all);
        assert!(!force);

        let all_cli =
            Cli::try_parse_from(["thurbox-cli", "ext", "update", "--all", "--force"]).unwrap();
        let Command::Extension {
            action: extensions::Action::Update { name, all, force },
        } = all_cli.command
        else {
            panic!("expected Extension::Update");
        };
        assert!(name.is_none());
        assert!(all);
        assert!(force);
    }

    #[test]
    fn parse_extension_update_no_name_means_all() {
        // No name and no --all is now valid: it updates every installed extension.
        let cli = Cli::try_parse_from(["thurbox-cli", "extension", "update"]).unwrap();
        let Command::Extension {
            action: extensions::Action::Update { name, all, force },
        } = cli.command
        else {
            panic!("expected Extension::Update");
        };
        assert!(name.is_none());
        assert!(!all);
        assert!(!force);
    }

    #[test]
    fn parse_extension_reinstall() {
        let cli = Cli::try_parse_from(["thurbox-cli", "extension", "reinstall", "flow", "--purge"])
            .unwrap();
        let Command::Extension {
            action: extensions::Action::Reinstall { name, purge },
        } = cli.command
        else {
            panic!("expected Extension::Reinstall");
        };
        assert_eq!(name, "flow");
        assert!(purge);
    }

    #[test]
    fn parse_extension_available_and_search_alias() {
        let cli = Cli::try_parse_from(["thurbox-cli", "extension", "available"]).unwrap();
        let Command::Extension {
            action: extensions::Action::Available { query },
        } = cli.command
        else {
            panic!("expected Extension::Available");
        };
        assert!(query.is_none());

        // `search` is an alias and accepts a filter query.
        let cli = Cli::try_parse_from(["thurbox-cli", "ext", "search", "deps"]).unwrap();
        let Command::Extension {
            action: extensions::Action::Available { query },
        } = cli.command
        else {
            panic!("expected Extension::Available via search alias");
        };
        assert_eq!(query.as_deref(), Some("deps"));
    }

    #[test]
    fn extension_alias_ext_parses() {
        let cli = Cli::try_parse_from(["thurbox-cli", "ext", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Extension {
                action: extensions::Action::List
            }
        ));
    }

    #[test]
    fn parse_message_send_requires_to_kind_body() {
        assert!(
            Cli::try_parse_from(["thurbox-cli", "message", "send", "--to", "flow"]).is_err(),
            "missing --kind/--body should fail"
        );
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "message",
            "send",
            "--to",
            "flow",
            "--kind",
            "questions",
            "--body",
            "q1?",
            "--task",
            "5",
            "--no-wake",
        ])
        .unwrap();
        let Command::Message {
            action:
                messages::Action::Send {
                    to,
                    kind,
                    body,
                    task,
                    no_wake,
                    ..
                },
        } = cli.command
        else {
            panic!("expected Message::Send");
        };
        assert_eq!(to, "flow");
        assert_eq!(kind, "questions");
        assert_eq!(body, "q1?");
        assert_eq!(task, Some(5));
        assert!(no_wake);
    }

    #[test]
    fn parse_message_inbox_claim() {
        let cli = Cli::try_parse_from([
            "thurbox-cli",
            "message",
            "inbox",
            "--for",
            "flow",
            "--claim",
        ])
        .unwrap();
        let Command::Message {
            action:
                messages::Action::Inbox {
                    for_session,
                    claim,
                    all,
                    ..
                },
        } = cli.command
        else {
            panic!("expected Message::Inbox");
        };
        assert_eq!(for_session.as_deref(), Some("flow"));
        assert!(claim);
        assert!(!all);
    }

    #[test]
    fn message_alias_msg_parses() {
        let cli = Cli::try_parse_from(["thurbox-cli", "msg", "prune", "--older-than-days", "30"])
            .unwrap();
        let Command::Message {
            action: messages::Action::Prune {
                older_than_days, ..
            },
        } = cli.command
        else {
            panic!("expected Message::Prune via msg alias");
        };
        assert_eq!(older_than_days, Some(30));
    }

    #[test]
    fn parse_version_with_and_without_check() {
        let cli = Cli::try_parse_from(["thurbox-cli", "version"]).unwrap();
        let Command::Version(args) = cli.command else {
            panic!("expected Version");
        };
        assert!(!args.check);

        let cli = Cli::try_parse_from(["thurbox-cli", "version", "--check"]).unwrap();
        let Command::Version(args) = cli.command else {
            panic!("expected Version");
        };
        assert!(args.check);
    }

    #[test]
    fn task_run_parses() {
        let cli = Cli::try_parse_from(["thurbox-cli", "task", "run", "7"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Task {
                action: tasks::Action::Run { id: 7 }
            }
        ));
    }
}
