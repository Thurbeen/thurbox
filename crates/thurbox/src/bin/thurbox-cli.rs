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

    let (settings, _) = thurbox::config::settings::load_or_seed_with_warnings();

    match cli::run(cli, &settings) {
        Ok(cli::Outcome::Ok) => {}
        Ok(cli::Outcome::Failed { message }) => {
            eprintln!("{message}");
            std::process::exit(EXIT_ERROR);
        }
        // clap has already answered a bad invocation itself, with exit 2.
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

    // clap renders required argument names on the lines after the opening one.
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
