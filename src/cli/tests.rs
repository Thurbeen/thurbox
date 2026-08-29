//! `cli`'s own tests — argument parsing and output-format selection over the
//! dispatcher, kept in a sibling file (the `git/tests.rs` pattern) because the
//! suite had grown to four times the module it tests.

use super::*;
use clap::Parser;

/// The parsed subcommand. [`Cli::command`] is an `Option` because a bare
/// `thurbox-cli` prints the home view rather than a usage dump (AXI principle
/// 8); every test in this file passes one, so unwrapping is the assertion.
fn subcommand(cli: Cli) -> Command {
    cli.command.expect("these tests always parse a subcommand")
}

#[test]
fn parse_extra_repos_splits_base_on_last_at() {
    // A path containing '@' must keep it: only the suffix after the LAST
    // '@' is the base branch.
    let extra = parse_extra_repos(
        &[
            "/srv/repo@main".to_string(),
            "/srv/user@host/repo@develop".to_string(),
            "/srv/plain".to_string(),
            "@onlybase".to_string(),
        ],
        &["/reference".to_string()],
    );

    assert_eq!(extra.len(), 5);

    assert_eq!(extra[0].repo_path, std::path::PathBuf::from("/srv/repo"));
    assert_eq!(extra[0].base_branch.as_deref(), Some("main"));
    assert!(extra[0].worktree);

    // Path itself contains '@' — only the last '@' splits off the base.
    assert_eq!(
        extra[1].repo_path,
        std::path::PathBuf::from("/srv/user@host/repo")
    );
    assert_eq!(extra[1].base_branch.as_deref(), Some("develop"));

    assert_eq!(extra[2].repo_path, std::path::PathBuf::from("/srv/plain"));
    assert_eq!(extra[2].base_branch, None);

    // Empty path before '@' — the guard rejects the split, token taken verbatim.
    assert_eq!(extra[3].repo_path, std::path::PathBuf::from("@onlybase"));
    assert_eq!(extra[3].base_branch, None);

    assert_eq!(extra[4].repo_path, std::path::PathBuf::from("/reference"));
    assert_eq!(extra[4].base_branch, None);
    assert!(!extra[4].worktree);
}

#[test]
fn pretty_flag_is_global() {
    let cli = Cli::try_parse_from(["thurbox-cli", "session", "list", "--pretty"]).unwrap();
    assert!(cli.pretty);
    assert!(matches!(
        subcommand(cli),
        Command::Session {
            action: sessions::Action::List {
                parent: None,
                deleted: false
            }
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
    assert!(Cli::try_parse_from(["thurbox-cli", "session", "create"]).is_err());

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
    } = subcommand(cli)
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
    } = subcommand(cli)
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
    } = subcommand(cli)
    else {
        panic!("expected Session::Send");
    };
    assert_eq!(uuid, "0f4dec1e-9d4b-4c4f-9d05-3a3a3a3a3a3a");
    assert_eq!(text, "hello");

    // The original collision-triggering invocation: global `--text` flag set.
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
    } = subcommand(cli)
    else {
        panic!("expected Session::Send");
    };
    assert_eq!(text, "hello");
}

#[test]
fn parse_session_focus_takes_uuid() {
    let cli = Cli::try_parse_from([
        "thurbox-cli",
        "session",
        "focus",
        "0f4dec1e-9d4b-4c4f-9d05-3a3a3a3a3a3a",
    ])
    .unwrap();
    let Command::Session {
        action: sessions::Action::Focus { uuid },
    } = subcommand(cli)
    else {
        panic!("expected Session::Focus");
    };
    assert_eq!(uuid, "0f4dec1e-9d4b-4c4f-9d05-3a3a3a3a3a3a");

    assert!(Cli::try_parse_from(["thurbox-cli", "session", "focus"]).is_err());
}

#[test]
fn parse_session_signal_accepts_state_and_rejects_garbage() {
    let cli =
        Cli::try_parse_from(["thurbox-cli", "session", "signal", "--state", "blocked"]).unwrap();
    let Command::Session {
        action: sessions::Action::Signal { state, session },
    } = subcommand(cli)
    else {
        panic!("expected Session::Signal");
    };
    assert_eq!(state, "blocked");
    assert_eq!(session, None);

    // The value_parser allow-list rejects unknown states.
    assert!(
        Cli::try_parse_from(["thurbox-cli", "session", "signal", "--state", "exploded",]).is_err()
    );
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
        action: sessions::Action::List {
            parent,
            deleted: false,
        },
    } = subcommand(cli)
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
        subcommand(cli),
        Command::Editor {
            action: editor::Action::Get
        }
    ));
    let cli = Cli::try_parse_from(["thurbox-cli", "editor", "set", "code --wait"]).unwrap();
    let Command::Editor {
        action: editor::Action::Set { command },
    } = subcommand(cli)
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
        subcommand(cli),
        Command::Automation {
            action: automations::Action::Create { .. }
        }
    ));
}

#[test]
fn automation_alias_auto_parses() {
    let cli = Cli::try_parse_from(["thurbox-cli", "auto", "list"]).unwrap();
    assert!(matches!(
        subcommand(cli),
        Command::Automation {
            action: automations::Action::List
        }
    ));
}

#[test]
fn automation_tick_parses() {
    let cli = Cli::try_parse_from(["thurbox-cli", "automation", "tick"]).unwrap();
    assert!(matches!(
        subcommand(cli),
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
    let cli = Cli::try_parse_from(["thurbox-cli", "task", "create", "--title", "Fix bug"]).unwrap();
    let Command::Task {
        action:
            tasks::Action::Create {
                title,
                session,
                repo,
                ..
            },
    } = subcommand(cli)
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
    } = subcommand(cli)
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
    } = subcommand(cli)
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
        subcommand(cli),
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
    } = subcommand(cli)
    else {
        panic!("expected Extension::Install");
    };
    assert_eq!(target, "flow");
    assert_eq!(home.as_deref(), Some("/home/me/flow"));
    assert!(force);
}

#[test]
fn parse_extension_uninstall() {
    let cli =
        Cli::try_parse_from(["thurbox-cli", "extension", "uninstall", "flow", "--purge"]).unwrap();
    let Command::Extension {
        action: extensions::Action::Uninstall { name, purge },
    } = subcommand(cli)
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
    } = subcommand(cli)
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
    } = subcommand(cli)
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
    } = subcommand(cli)
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
    } = subcommand(all_cli)
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
    } = subcommand(cli)
    else {
        panic!("expected Extension::Update");
    };
    assert!(name.is_none());
    assert!(!all);
    assert!(!force);
}

#[test]
fn parse_extension_reinstall() {
    let cli =
        Cli::try_parse_from(["thurbox-cli", "extension", "reinstall", "flow", "--purge"]).unwrap();
    let Command::Extension {
        action: extensions::Action::Reinstall { name, purge },
    } = subcommand(cli)
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
    } = subcommand(cli)
    else {
        panic!("expected Extension::Available");
    };
    assert!(query.is_none());

    let cli = Cli::try_parse_from(["thurbox-cli", "ext", "search", "deps"]).unwrap();
    let Command::Extension {
        action: extensions::Action::Available { query },
    } = subcommand(cli)
    else {
        panic!("expected Extension::Available via search alias");
    };
    assert_eq!(query.as_deref(), Some("deps"));
}

#[test]
fn extension_alias_ext_parses() {
    let cli = Cli::try_parse_from(["thurbox-cli", "ext", "list"]).unwrap();
    assert!(matches!(
        subcommand(cli),
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
    } = subcommand(cli)
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
    } = subcommand(cli)
    else {
        panic!("expected Message::Inbox");
    };
    assert_eq!(for_session.as_deref(), Some("flow"));
    assert!(claim);
    assert!(!all);
}

#[test]
fn message_alias_msg_parses() {
    let cli =
        Cli::try_parse_from(["thurbox-cli", "msg", "prune", "--older-than-days", "30"]).unwrap();
    let Command::Message {
        action: messages::Action::Prune {
            older_than_days, ..
        },
    } = subcommand(cli)
    else {
        panic!("expected Message::Prune via msg alias");
    };
    assert_eq!(older_than_days, Some(30));
}

#[test]
fn parse_version_with_and_without_check() {
    let cli = Cli::try_parse_from(["thurbox-cli", "version"]).unwrap();
    let Command::Version(args) = subcommand(cli) else {
        panic!("expected Version");
    };
    assert!(!args.check);

    let cli = Cli::try_parse_from(["thurbox-cli", "version", "--check"]).unwrap();
    let Command::Version(args) = subcommand(cli) else {
        panic!("expected Version");
    };
    assert!(args.check);
}

#[test]
fn parse_update_with_and_without_force() {
    let cli = Cli::try_parse_from(["thurbox-cli", "update"]).unwrap();
    let Command::Update(args) = subcommand(cli) else {
        panic!("expected Update");
    };
    assert!(!args.force);

    let cli = Cli::try_parse_from(["thurbox-cli", "update", "--force"]).unwrap();
    let Command::Update(args) = subcommand(cli) else {
        panic!("expected Update");
    };
    assert!(args.force);
}

#[test]
fn parse_notify_with_and_without_test() {
    let cli = Cli::try_parse_from(["thurbox-cli", "notify"]).unwrap();
    let Command::Notify(args) = subcommand(cli) else {
        panic!("expected Notify");
    };
    assert!(!args.test);

    let cli = Cli::try_parse_from(["thurbox-cli", "notify", "--test"]).unwrap();
    let Command::Notify(args) = subcommand(cli) else {
        panic!("expected Notify");
    };
    assert!(args.test);
}

#[test]
fn task_run_parses() {
    let cli = Cli::try_parse_from(["thurbox-cli", "task", "run", "7"]).unwrap();
    assert!(matches!(
        subcommand(cli),
        Command::Task {
            action: tasks::Action::Run { id: 7 }
        }
    ));
}

/// `--version` must report the build-time-injected version, not clap's default.
///
/// Two assertions because neither alone bites everywhere. The behavioural one
/// is the contract but is **vacuous in a dev build**: clap's implicit `version`
/// reads `CARGO_PKG_VERSION`, which equals `THURBOX_VERSION` exactly when no
/// release version was injected — which is every local and CI test run. So the
/// source check is the one that actually catches a regression here, in the
/// style of `kernel::updates`' single-installer guard.
#[test]
fn version_flag_reports_the_injected_version_not_the_dev_marker() {
    let rendered = Cli::try_parse_from(["thurbox-cli", "--version"])
        .unwrap_err()
        .to_string();
    let expected = crate::agent::version_check::current_version();
    assert_eq!(
        rendered.trim(),
        format!("thurbox-cli {expected}"),
        "--version must print the version the `version` subcommand reports"
    );

    // The bare `version,` attribute is the bug: it silently reads
    // CARGO_PKG_VERSION, which this project pins to the `0.0.0-dev` marker and
    // never bumps, so every release shipped a `--version` claiming a dev build.
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli/mod.rs"),
    )
    .expect("read src/cli/mod.rs");
    let command_attr = {
        let start = src.find("#[command(").expect("the Cli command attribute");
        &src[start..src[start..].find(")]").expect("attribute end") + start]
    };
    assert!(
        command_attr.contains("version = crate::agent::version_check::current_version()"),
        "the Cli #[command(..)] must name the injected version explicitly; \
         a bare `version` falls back to CARGO_PKG_VERSION. Got: {command_attr:?}"
    );
}

#[test]
fn fields_flag_names_the_columns_a_list_shows() {
    assert_eq!(parse_fields("name,agent"), vec!["name", "agent"]);
    // Spaces around a comma are how a person writes it, so accept them.
    assert_eq!(parse_fields("name, agent "), vec!["name", "agent"]);
    // A trailing comma must not become a column with no name.
    assert_eq!(parse_fields("name,"), vec!["name"]);
}

#[test]
fn fields_all_clears_the_projection() {
    // Empty is the renderer's own spelling of "no projection", so `all` needs
    // no second branch downstream.
    assert!(parse_fields("all").is_empty());
    assert!(parse_fields("ALL").is_empty());
}

#[test]
fn a_bare_invocation_parses_with_no_subcommand() {
    // AXI principle 8: `thurbox-cli` on its own is the home view, so the parse
    // must succeed rather than fail with a usage error.
    let cli = Cli::parse_from(["thurbox-cli"]);
    assert!(cli.command.is_none());
}

#[test]
fn the_output_flags_are_global_and_parse_after_a_subcommand() {
    let cli = Cli::parse_from(["thurbox-cli", "session", "list", "--toon", "--full"]);
    assert!(cli.toon);
    assert!(cli.full);
    let cli = Cli::parse_from(["thurbox-cli", "session", "list", "--fields", "name,id"]);
    assert_eq!(cli.fields.as_deref(), Some("name,id"));
}
