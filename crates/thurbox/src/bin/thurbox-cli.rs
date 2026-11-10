//! Thurbox CLI binary — scriptable access to thurbox's own files and to a
//! running interface. Sessions are Fleet's: `fleet` is their command line.
//!
//! This entrypoint owns the parts of the AXI contract (`axi/1.0-2026-07`) that
//! are about the *process* rather than about any one command: a failure that
//! left nothing to print is a structured document on **stdout**, and the exit
//! code says which kind of failure it was — `0` success, `1` the command ran
//! and failed, `2` the invocation was wrong, `3` the reference matched more
//! than one running interface. An agent reads one stream and one status;
//! splitting the answer across stdout and stderr costs it a retry to find out
//! what happened.
//!
//! The exit code is the *only* half of that contract a caller may invert. An
//! `error` key on stdout implies a non-zero exit; the converse does not hold,
//! because a command whose report *is* the answer (`config validate`) prints
//! that report and asks for a non-zero exit anyway. Gate on the document, not
//! on `$?`.

use clap::Parser;
use thurbox::cli::{self, output::Format, output::FormatFlags, Cli, EXIT_ERROR, EXIT_USAGE};

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

    // Loaded once and handed to everything that reads one. Warnings go to the
    // WARN-level stderr logger; `config validate` is the loud path.
    let (settings, _) = thurbox::config::settings::load_or_seed_with_warnings();

    match cli::run(cli, &settings) {
        Ok(cli::Outcome::Ok) => {}
        // The report is already on stdout and is the answer; only the verdict
        // is left to carry, and it carries as an exit code.
        Ok(cli::Outcome::Failed { message }) => {
            eprintln!("{message}");
            std::process::exit(EXIT_ERROR);
        }
        // A runtime failure, not a bad invocation: clap already answered those
        // with its own wording and exit 2.
        Err(e) => {
            println!(
                "{}",
                cli::error_output(
                    &e,
                    "the command ran and failed; the message says what went wrong",
                    "thurbox-cli --help",
                    format
                )
            );
            std::process::exit(e.exit_code)
        }
    }
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

    // Required argument names follow the opening line. Keep the problem's
    // whole paragraph, without folding usage and hints into the error itself.
    let rendered = e.render().to_string();
    let message = rendered
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    let message = if message.is_empty() {
        "invalid arguments".to_string()
    } else {
        message.trim_start_matches("error: ").to_string()
    };

    println!(
        "{}",
        cli::error_output(
            &cli::CommandError::with_code(message, EXIT_USAGE),
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
