//! Thurbox CLI binary — scriptable access to the same state the TUI shows.
//! Every subcommand works without the TUI running.
//!
//! This entrypoint owns the parts of the AXI contract (`axi/1.0-2026-07`) that
//! are about the *process* rather than about any one command: every failure is
//! a structured document on **stdout**, and the exit code says which kind of
//! failure it was — `0` success, `1` the command ran and failed, `2` the
//! invocation was wrong. An agent reads one stream and one status; splitting
//! the answer across stdout and stderr costs it a retry to find out what
//! happened.

use clap::Parser;
use thurbox::cli::{self, output::Format, output::FormatFlags, Cli};

/// The command ran and failed.
const EXIT_ERROR: i32 = 1;
/// The invocation was wrong: an unknown flag, a missing argument, a bad value.
const EXIT_USAGE: i32 = 2;

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => exit_from_clap(&e),
    };

    let format = Format::resolve(FormatFlags {
        json: cli.json,
        pretty: cli.pretty,
        text: cli.text,
        toon: cli.toon,
    });

    // Publish settings before Database::open (audit pruning reads retention).
    // Warnings go to the WARN-level stderr logger; `config validate` is the
    // loud path.
    let (settings, _) = thurbox::agent::settings_config::load_or_seed_with_warnings();
    thurbox::session::settings::init(settings);

    let Some(db_path) = thurbox::paths::database_file() else {
        fail(
            "cannot resolve the database path",
            "set HOME (or THURBOX_DATA_DIR) and run the command again",
            "thurbox-cli config show",
            format,
        );
    };

    let db = match thurbox::storage::Database::open(&db_path) {
        Ok(db) => db,
        Err(e) => fail(
            &format!("cannot open the database at {}: {e}", db_path.display()),
            "check the file is readable and not held by another process, then retry",
            "thurbox-cli config show",
            format,
        ),
    };

    if let Err(e) = cli::run(cli, &db) {
        fail(
            &e,
            "check the arguments against this command's usage",
            "thurbox-cli <command> --help",
            format,
        );
    }
}

/// Print a structured failure on stdout and exit [`EXIT_ERROR`].
fn fail(message: &str, suggestion: &str, next: &str, format: Format) -> ! {
    println!("{}", cli::error_output(message, suggestion, next, format));
    std::process::exit(EXIT_ERROR)
}

/// Turn a clap outcome into an exit.
///
/// `--help` and `--version` are successful requests for information, so they
/// keep clap's own rendering and exit 0. Everything else is a usage error: it
/// goes to stdout in the resolved format and exits [`EXIT_USAGE`]. The format
/// has to be read off the raw arguments, because the parse that would have
/// produced the flags is the one that just failed.
fn exit_from_clap(e: &clap::Error) -> ! {
    use clap::error::ErrorKind;
    if matches!(
        e.kind(),
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        print!("{e}");
        std::process::exit(0)
    }

    // clap renders the problem, the usage line and its own hint as one block.
    // The first line is the problem; the rest is the suggestion, which is what
    // an agent needs to fix the call.
    let rendered = e.render().to_string();
    let message = rendered
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("invalid arguments")
        .trim_start_matches("error: ")
        .to_string();

    println!(
        "{}",
        cli::error_output(
            &message,
            "this binary's commands and flags are listed by --help",
            "thurbox-cli --help",
            format_from_raw_args(),
        )
    );
    std::process::exit(EXIT_USAGE)
}

/// Resolve the output format from unparsed `argv`, for the failure paths that
/// have no parsed [`Cli`] to read it from.
fn format_from_raw_args() -> Format {
    let args: Vec<String> = std::env::args().collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    Format::resolve(FormatFlags {
        json: has("--json"),
        pretty: has("--pretty"),
        text: has("--text"),
        toon: has("--toon"),
    })
}
