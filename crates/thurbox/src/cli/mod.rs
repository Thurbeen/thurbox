//! Command-line interface dispatcher for the `thurbox-cli` binary.
//!
//! Output is human-readable in a terminal and TOON down a pipe, because what
//! is usually on the other end of that pipe is an agent. Force a format with
//! `--json` (compact), `--pretty` (indented JSON), `--toon`, or `--text`. See
//! [`output::Format`] for the precedence and [`toon`] for the format itself.
//!
//! The CLI is intentionally thin: it parses arguments, reads or writes
//! thurbox's own files, and prints the result. Sessions are Fleet's: `fleet`
//! is their command line (ADR-41). It hosts no TUI or event loop; `ui` sends
//! bounded requests to a separately running interface.
//!
//! It is also an **AXI** (`axi/1.0-2026-07`, <https://axi.md>) — an interface
//! shaped for an agent rather than for a person at a keyboard. Three of that
//! spec's rules are structural and live here rather than in any one
//! subcommand: output is TOON down a pipe (principle 1), every result can
//! carry `help[N]:` next steps ([`output::AgentView`], principle 9), and errors
//! are structured on stdout with the exit code saying which kind they are —
//! [`EXIT_ERROR`], [`EXIT_USAGE`], [`EXIT_AMBIGUOUS`] ([`error_output`],
//! principle 6). The rest are per-command and marked where they are met.
//!
//! One consequence of principle 6 is a rule the entrypoint enforces: **stdout
//! carries exactly one document per invocation**. A command that renders a
//! report and *then* asks for a non-zero exit (`config validate`) has already
//! written it, so its failure comes back as [`Outcome::Failed`] and the
//! sentence explaining the exit goes to stderr — see [`Outcome`].

use clap::{Parser, Subcommand};

use crate::session::settings::Settings;

#[cfg(test)]
mod tests;

pub mod config;
pub mod doctor;
pub mod editor;
pub mod notify;
pub mod output;
pub mod perf;
pub mod plugins;
pub mod toon;
pub mod ui;
pub mod update;
pub mod version;

use output::{CommandOutput, Format, FormatFlags};

/// The command ran and failed.
pub const EXIT_ERROR: i32 = 1;
/// The invocation was wrong: an unknown flag, a missing argument, a bad value.
pub const EXIT_USAGE: i32 = 2;
/// A reference matched more than one candidate: several running interfaces
/// answer `ui` and none was chosen.
///
/// Its own code because the answer is different in kind: "none" is something
/// a driver reconciles, while "several" is something only an operator can
/// settle. AXI principle 6 asks the two to exit differently.
pub const EXIT_AMBIGUOUS: i32 = 3;

/// A failure that left nothing on stdout, and the exit code it deserves.
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
    /// A failure with an exit code of its own.
    pub fn with_code(message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            message: message.into(),
            exit_code,
        }
    }

    /// The failure's kind, as the structured error names it.
    pub fn kind(&self) -> &'static str {
        match self.exit_code {
            EXIT_USAGE => "usage",
            EXIT_AMBIGUOUS => "ambiguous",
            _ => "other",
        }
    }
}

// Deref to the message so `err.contains("…")` at a call site — overwhelmingly a
// test asserting what the failure says — reads the sentence rather than the
// struct, exactly as `CommandOutput` derefs to its JSON.
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

/// thurbox's interface files and a running interface, without the TUI.
/// Sessions are Fleet's: drive them with `fleet`.
// `version` is spelled out rather than left bare: clap's implicit form reads
// `CARGO_PKG_VERSION`, which is the static `0.0.0-dev` marker this project
// never bumps, so `--version` reported a dev build on every release while the
// `version` subcommand was right. Both now call the one
// `version_check::current_version`.
#[derive(Parser, Debug)]
#[command(
    name = "thurbox-cli",
    version = crate::update::version_check::current_version(),
    about,
    after_help = EXAMPLES,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Output JSON — every field, the format scripts parse.
    #[arg(long, global = true)]
    pub json: bool,

    /// Pretty-print JSON output (implies --json).
    #[arg(long, global = true)]
    pub pretty: bool,

    /// Output TOON, the agent format (the default when piped).
    #[arg(long, global = true)]
    pub toon: bool,

    /// Columns for a list view: a comma-separated set, or `all`.
    ///
    /// A list defaults to the three or four fields that let you decide what to
    /// do next (AXI principle 2). This asks for a different set by name —
    /// `--fields name,cwd,base_branch` — without going all the way to `--json`.
    #[arg(long, global = true, value_name = "LIST")]
    pub fields: Option<String>,

    /// Do not shorten long text fields (AXI principle 3's escape hatch).
    #[arg(long, global = true)]
    pub full: bool,

    /// Force human-readable output even when piped.
    #[arg(long, global = true, id = "text_format")]
    pub text: bool,

    /// The operation to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Worked examples for `--help`. AXI principle 10 asks every help surface for
/// two or three of them; a list of subcommand names alone leaves an agent to
/// guess the shape of an invocation.
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
    /// Get/set the editor command (Ctrl+O in the TUI).
    Editor {
        #[command(subcommand)]
        action: editor::Action,
    },
    /// Validate or inspect the config files.
    Config {
        #[command(subcommand)]
        action: config::Action,
    },
    /// Print the version; `--check` queries GitHub for a newer release.
    Version(version::VersionArgs),
    /// Download, verify, and replace the installed binaries with the latest release.
    Update(update::UpdateArgs),
    /// Diagnose OS desktop notifications; `--test` fires a sample.
    Notify(notify::NotifyArgs),
    /// Print the perf snapshot a running TUI publishes (THURBOX_PERF_LOG or
    /// the perf HUD must be active in that TUI); `--plugins` for the per-pane
    /// table.
    Perf(perf::PerfArgs),
    /// Interface plugins: where they live, start one, check it loads.
    Plugin {
        #[command(subcommand)]
        action: plugins::Action,
    },
    /// Control a running local interface instance.
    Ui {
        /// Select a particular interface instance.
        #[arg(long)]
        instance: Option<String>,
        #[command(subcommand)]
        action: ui::Action,
    },
    /// Describe CLI commands and the selected interface's live actions.
    Schema {
        /// Select a particular running interface.
        #[arg(long)]
        instance: Option<String>,
    },
    /// Whether this machine has what a session needs: the multiplexer, each
    /// registered agent's command, and the launcher for every configured host.
    Doctor,
}

/// What a completed invocation asks the process to do next.
///
/// The distinction that matters is *whether stdout has already been written*.
/// A command that could not run at all produced nothing, so the caller is free
/// to print the structured error document ([`error_output`]). A command that
/// rendered its answer and then asked for a non-zero exit has already put the
/// one document on stdout that a machine consumer will parse — printing a
/// second one there turns `session doctor --json` into two JSON values and
/// breaks every single-document parser reading it.
pub enum Outcome {
    /// Rendered; exit 0.
    Ok,
    /// Rendered, and the command asks for a non-zero exit. Nothing more may go
    /// to stdout; the message is a diagnostic for stderr.
    ///
    Failed { message: String },
}

/// Run a parsed CLI invocation, rendering the result in the
/// resolved [`Format`] (human in a terminal, TOON down a pipe, or whatever
/// `--json`/`--pretty`/`--toon`/`--text` forced).
///
/// `Err` means nothing was printed. A command that rendered normally and still
/// wants a non-zero exit comes back as [`Outcome::Failed`].
/// `settings` are the ones this process loaded: every value a command reads
/// from `settings.toml` comes from here, never from process state.
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
    // A command can render normally yet still request a non-zero exit (e.g.
    // `config validate` on an invalid file).
    // Its report is the answer and is already on stdout, so the failure travels
    // as an outcome rather than an `Err` the caller would print again.
    Ok(match output.failure {
        Some(message) => Outcome::Failed { message },
        None => Outcome::Ok,
    })
}

/// Read a `--fields` value into the column set a list view should show.
///
/// `all` yields an empty set, which is what the renderer already takes to mean
/// "no projection — show the record as it is", so the escape hatch needs no
/// second code path. Blank entries are dropped so a trailing comma is not a
/// column named nothing.
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

/// Route one subcommand to the module that owns it.
fn dispatch(command: Command, settings: &Settings) -> Result<CommandOutput, CommandError> {
    Ok(match command {
        Command::Editor { action } => editor::run(action)?,
        Command::Config { action } => config::run(action, settings)?,
        Command::Version(args) => version::run(args, settings.features.version_check),
        Command::Update(args) => update::run(args, settings.features.auto_update),
        Command::Notify(args) => notify::run(args, settings),
        Command::Perf(args) => perf::run(args.plugins)?,
        Command::Plugin { action } => plugins::run(action)?,
        Command::Ui { instance, action } => ui::run(instance, action)?,
        Command::Schema { instance } => ui::schema(instance)?,
        // Reads the machine: what is installed is not something thurbox
        // recorded.
        Command::Doctor => doctor::run()?,
    })
}

/// Render a failure the way AXI principle 6 asks for: a structured document on
/// **stdout**, not a bare line on stderr.
///
/// An agent reads one stream. A message on stderr is one it has to be told to
/// capture, and half the time the capture is dropped — so the error becomes an
/// empty stdout and an exit code, which is indistinguishable from a command
/// that produced nothing. `suggestion` says what to do about it in prose and
/// `next` is that same advice as something runnable, which is the difference
/// between an error an agent can act on and one it can only report.
pub fn error_output(error: &CommandError, suggestion: &str, next: &str, format: Format) -> String {
    let out = CommandOutput::new(
        serde_json::json!({ "error": error.message, "error_kind": error.kind(), "suggestion": suggestion }),
        format!("error: {}\n  {suggestion}", error.message),
    )
    .help([next]);
    format.render(&out)
}
