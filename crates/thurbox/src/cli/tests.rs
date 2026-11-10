//! `cli`'s own tests — argument parsing and output-format selection over the
//! dispatcher, kept in a sibling file (the `git/tests.rs` pattern).

use super::*;
use clap::Parser;

fn subcommand(cli: Cli) -> Command {
    cli.command
}

#[test]
fn the_output_flags_are_global_and_parse_after_a_subcommand() {
    let cli = Cli::parse_from(["thurbox-cli", "config", "show", "--pretty", "--full"]);
    assert!(cli.pretty);
    assert!(cli.full);
    let cli = Cli::parse_from(["thurbox-cli", "--text", "doctor", "--fields", "name,id"]);
    assert!(cli.text);
    assert_eq!(cli.fields.as_deref(), Some("name,id"));
}

#[test]
fn the_engine_commands_are_gone() {
    for noun in [
        "session",
        "task",
        "message",
        "extension",
        "watch",
        "runtime",
        "agent",
    ] {
        assert!(
            Cli::try_parse_from(["thurbox-cli", noun, "list"]).is_err(),
            "`{noun}` is Fleet's now"
        );
    }
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
fn version_flag_reports_the_injected_version_not_the_dev_marker() {
    let rendered = Cli::try_parse_from(["thurbox-cli", "--version"])
        .unwrap_err()
        .to_string();
    let expected = crate::update::version_check::current_version();
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
        command_attr.contains("version = crate::update::version_check::current_version()"),
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
