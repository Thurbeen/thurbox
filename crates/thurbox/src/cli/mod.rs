use clap::{Parser, Subcommand};

use crate::session::settings::Settings;

#[cfg(test)]
mod tests;

pub mod config;
pub mod doctor;
pub mod editor;
pub mod migrate;
pub mod mods;
pub mod notify;
pub mod output;
pub mod perf;
pub mod toon;
pub mod ui;
pub mod update;
pub mod version;

use output::{CommandOutput, Format, FormatFlags};

pub const EXIT_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_AMBIGUOUS: i32 = 3;

#[derive(Debug)]
pub struct CommandError {
    pub message: String,
    pub exit_code: i32,
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::with_code(message, EXIT_ERROR)
    }
}

impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

impl CommandError {
    pub fn with_code(message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            message: message.into(),
            exit_code,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self.exit_code {
            EXIT_USAGE => "usage",
            EXIT_AMBIGUOUS => "ambiguous",
            _ => "other",
        }
    }
}

impl std::ops::Deref for CommandError {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

// clap's bare `version` reads CARGO_PKG_VERSION, which stays `0.0.0-dev` here.
#[derive(Parser, Debug)]
#[command(
    name = "thurbox-cli",
    version = crate::update::version_check::current_version(),
    about,
    after_help = EXAMPLES,
    arg_required_else_help = true
)]
pub struct Cli {
    #[arg(help = "Output JSON — every field, the format scripts parse")]
    #[arg(long, global = true)]
    pub json: bool,

    #[arg(help = "Pretty-print JSON output (implies --json)")]
    #[arg(long, global = true)]
    pub pretty: bool,

    #[arg(help = "Output TOON, the agent format (the default when piped)")]
    #[arg(long, global = true)]
    pub toon: bool,

    #[arg(
        help = "Columns for a list view: a comma-separated set, or `all`",
        long_help = "Columns for a list view: a comma-separated set, or `all`.\n\nA list defaults to the three or four fields that let you decide what to do next (AXI principle 2). This asks for a different set by name — `--fields name,cwd,base_branch` — without going all the way to `--json`."
    )]
    #[arg(long, global = true, value_name = "LIST")]
    pub fields: Option<String>,

    #[arg(help = "Do not shorten long text fields (AXI principle 3's escape hatch)")]
    #[arg(long, global = true)]
    pub full: bool,

    #[arg(help = "Force human-readable output even when piped")]
    #[arg(long, global = true, id = "text_format")]
    pub text: bool,

    #[command(subcommand)]
    pub command: Command,
}

const EXAMPLES: &str = "\
Examples:
  thurbox-cli ui instances --json              running local interface IDs
  thurbox-cli config show                      the effective configuration
  thurbox-cli editor set 'code --wait'         what Ctrl+O opens
  thurbox-cli doctor                           is the multiplexer/agent installed?
  fleet get tasks                              the sessions, which are Fleet's

Output is human-readable in a terminal and TOON when piped; --json restores the
full JSON record on any command.";

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(about = "Get/set the editor command (Ctrl+O in the TUI)")]
    Editor {
        #[command(subcommand)]
        action: editor::Action,
    },
    #[command(about = "Validate or inspect the config files")]
    Config {
        #[command(subcommand)]
        action: config::Action,
    },
    #[command(about = "Print the version; `--check` queries GitHub for a newer release")]
    Version(version::VersionArgs),
    #[command(
        about = "Download, verify, and replace the installed binaries with the latest release"
    )]
    Update(update::UpdateArgs),
    #[command(about = "Diagnose OS desktop notifications; `--test` fires a sample")]
    Notify(notify::NotifyArgs),
    #[command(
        about = "Print the perf snapshot a running TUI publishes (THURBOX_PERF_LOG or the perf HUD must be active in that TUI); `--mods` for the per-pane table"
    )]
    Perf(perf::PerfArgs),
    #[command(about = "Interface mods: where they live, start one, check it loads")]
    Mod {
        #[command(subcommand)]
        action: mods::Action,
    },
    #[command(hide = true)]
    Plugin {
        #[command(subcommand)]
        action: mods::Action,
    },
    #[command(about = "Control a running local interface instance")]
    Ui {
        #[arg(help = "Select a particular interface instance")]
        #[arg(long)]
        instance: Option<String>,
        #[command(subcommand)]
        action: ui::Action,
    },
    #[command(about = "Describe CLI commands and the selected interface's live actions")]
    Schema {
        #[arg(help = "Select a particular running interface")]
        #[arg(long)]
        instance: Option<String>,
    },
    #[command(
        about = "Whether this machine has what a session needs: Fleet, with its harnesses and the hosts it defines, and the multiplexer"
    )]
    Doctor,
    #[command(
        about = "Turn thurbox 2.x state into Fleet Tasks and CronTasks, and print the places and harness profiles to add to Fleet's config"
    )]
    Migrate(migrate::Args),
}

pub enum Outcome {
    Ok,
    Failed { message: String },
}

pub fn run(cli: Cli, settings: &Settings) -> Result<Outcome, CommandError> {
    let format = Format::resolve(FormatFlags {
        json: cli.json,
        pretty: cli.pretty,
        text: cli.text,
        toon: cli.toon,
    });
    if let Command::Ui {
        instance,
        action: ui::Action::Watch { since, once: false },
    } = cli.command
    {
        ui::stream(instance, since)?;
        return Ok(Outcome::Ok);
    }

    let mut output = dispatch(cli.command, settings)?;
    if cli.full {
        output.agent.max_text = None;
    }
    if let Some(spec) = &cli.fields {
        output
            .select_fields(parse_fields(spec))
            .map_err(|e| CommandError::with_code(e, EXIT_USAGE))?;
    }

    println!("{}", format.render(&output));
    Ok(match output.failure {
        Some(message) => Outcome::Failed { message },
        None => Outcome::Ok,
    })
}

fn parse_fields(spec: &str) -> Vec<String> {
    if spec.eq_ignore_ascii_case("all") {
        return Vec::new();
    }
    spec.split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_string)
        .collect()
}

const DEPRECATED_PLUGIN_VERB: &str =
    "thurbox-cli: `thurbox-cli plugin` is deprecated; use `thurbox-cli mod`";

fn migrate_mods() -> Result<(), CommandError> {
    let migration = crate::kernel::mod_migration::migrate_in_force();
    let Some(notice) = migration.notice() else {
        return Ok(());
    };
    eprintln!("thurbox-cli: {notice}");
    match migration.errors.is_empty() {
        true => Ok(()),
        false => Err(notice.into()),
    }
}

fn dispatch(command: Command, settings: &Settings) -> Result<CommandOutput, CommandError> {
    Ok(match command {
        Command::Editor { action } => editor::run(action)?,
        Command::Config { action } => {
            migrate_mods()?;
            config::run(action, settings)?
        }
        Command::Version(args) => version::run(
            args,
            settings.features.version_check,
            &crate::fleet_link::FleetLink::in_force(settings),
        ),
        Command::Update(args) => update::run(
            args,
            settings.features.auto_update,
            crate::fleet_link::Fleets::of(settings).all(),
        ),
        Command::Notify(args) => notify::run(args, settings)?,
        Command::Perf(args) => {
            if args.plugins {
                eprintln!("{}", perf::DEPRECATED_PLUGINS_FLAG);
            }
            perf::run(args.mods || args.plugins)?
        }
        Command::Mod { action } => {
            migrate_mods()?;
            mods::run(action)?
        }
        Command::Plugin { action } => {
            eprintln!("{DEPRECATED_PLUGIN_VERB}");
            migrate_mods()?;
            mods::run(action)?
        }
        Command::Ui { instance, action } => ui::run(instance, action)?,
        Command::Schema { instance } => ui::schema(instance)?,
        Command::Doctor => doctor::run(settings)?,
        Command::Migrate(args) => migrate::run(args, settings)?,
    })
}

pub fn error_output(error: &CommandError, suggestion: &str, next: &str, format: Format) -> String {
    let out = CommandOutput::new(
        serde_json::json!({ "error": error.message, "error_kind": error.kind(), "suggestion": suggestion }),
        format!("error: {}\n  {suggestion}", error.message),
    )
    .help([next]);
    format.render(&out)
}
