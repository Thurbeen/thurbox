//! Command-line interface dispatcher for the `thurbox-cli` binary.
//!
//! Output is human-readable in a terminal and TOON down a pipe, because what
//! is usually on the other end of that pipe is an agent. Force a format with
//! `--json` (compact), `--pretty` (indented JSON), `--toon`, or `--text`. See
//! [`output::Format`] for the precedence and [`toon`] for the format itself.
//!
//! The CLI is intentionally thin: it parses arguments, calls into
//! `storage::Database`, `session_ops`, or the tmux helpers in
//! `agent::tmux`, and prints the result. No TUI, no event loop.
//!
//! It is also an **AXI** (`axi/1.0-2026-07`, <https://axi.md>) — an interface
//! shaped for an agent rather than for a person at a keyboard. Four of that
//! spec's rules are structural and live here rather than in any one
//! subcommand: output is TOON down a pipe (principle 1), running the binary
//! with no subcommand prints live state instead of a usage dump
//! ([`home::run`], principle 8), every result can carry `help[N]:` next steps
//! ([`output::AgentView`], principle 9), and errors are structured on stdout
//! with the exit code saying which kind they are — 0 success, 1 failure, 2
//! usage ([`error_output`], principle 6). The rest are per-command and marked
//! where they are met.

use clap::{Parser, Subcommand};

use crate::storage::Database;

#[cfg(test)]
mod tests;

pub mod action;
pub mod automations;
pub mod config;
pub mod editor;
pub mod extensions;
pub mod home;
pub mod identity;
pub mod messages;
pub mod notify;
pub mod output;
pub mod perf;
pub mod plugins;
pub mod session_doctor;
pub mod sessions;
pub mod tasks;
pub mod toon;
pub mod update;
pub mod version;

use output::{CommandOutput, Format, FormatFlags};

/// Drive thurbox's sessions, tasks, automations and interface without the TUI.
///
/// Run with no subcommand for the current state of this machine's sessions.
// `version` is spelled out rather than left bare: clap's implicit form reads
// `CARGO_PKG_VERSION`, which is the static `0.0.0-dev` marker this project
// never bumps, so `--version` reported a dev build on every release while the
// `version` subcommand was right. Both now call the one
// `version_check::current_version`.
#[derive(Parser, Debug)]
#[command(
    name = "thurbox-cli",
    version = crate::agent::version_check::current_version(),
    about,
    after_help = EXAMPLES
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
    // `id = "text_format"` disambiguates from subcommand positional args also
    // named `text` (e.g. `sessions::Action::Send`). Without the explicit id,
    // clap registers two args with id "text" (this bool flag and the String
    // positional), and `get_one::<String>("text")` at parse time panics with
    // a TypeId downcast mismatch.
    #[arg(long, global = true, id = "text_format")]
    pub text: bool,

    /// The operation to run. Absent means the home view — AXI principle 8 asks
    /// a bare invocation for live state, not a usage manual, so this is
    /// `Option` rather than required.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Worked examples for `--help`. AXI principle 10 asks every help surface for
/// two or three of them; a list of subcommand names alone leaves an agent to
/// guess the shape of an invocation.
const EXAMPLES: &str = "\
Examples:
  thurbox-cli                                  live state: sessions, inbox, tasks
  thurbox-cli session list                     every session, with status and branch
  thurbox-cli session create --name fix-ci --repo-path . --worktree-branch fix/ci
  thurbox-cli session capture <id> --lines 50  what an agent's pane is showing
  thurbox-cli message send --to <id> --kind result --body 'done'
  thurbox-cli session list --json | jq         full records for a script

Output is human-readable in a terminal and TOON when piped; --json restores the
full JSON record on any command.";

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
    /// Download, verify, and replace the installed binaries with the latest release.
    Update(update::UpdateArgs),
    /// Diagnose OS desktop notifications; `--test` fires a sample.
    Notify(notify::NotifyArgs),
    /// Print the perf snapshot a running TUI publishes (THURBOX_PERF_LOG or
    /// the perf HUD must be active in that TUI).
    Perf,
    /// Interface plugins: where they live, start one, check it loads.
    Plugin {
        #[command(subcommand)]
        action: plugins::Action,
    },
}

/// Build the additional-repo list for a multi-repo `Spawn` from the repeatable
/// `--add-repo`/`--add-dir` flags shared by `session create` and `task create`.
///
/// Each `--add-repo` token is `PATH` or `PATH@BASE` — the repo gets its own
/// isolated worktree on the spawn's shared `--worktree`/`--worktree-branch`,
/// off `BASE` (falling back to the primary's base when omitted). Each
/// `--add-dir` token is attached as-is (no worktree). The base is split on the
/// last `@`, so paths without `@` (the norm) are taken verbatim.
pub(crate) fn parse_extra_repos(
    add_repo: &[String],
    add_dir: &[String],
) -> Vec<crate::session::ExtraRepo> {
    use crate::session::ExtraRepo;
    let mut extra: Vec<ExtraRepo> = Vec::new();
    for tok in add_repo {
        let (path, base) = match tok.rsplit_once('@') {
            Some((p, b)) if !p.is_empty() && !b.is_empty() => (p.to_string(), Some(b.to_string())),
            _ => (tok.clone(), None),
        };
        extra.push(ExtraRepo {
            repo_path: std::path::PathBuf::from(path),
            worktree: true,
            base_branch: base,
        });
    }
    for dir in add_dir {
        extra.push(ExtraRepo {
            repo_path: std::path::PathBuf::from(dir),
            worktree: false,
            base_branch: None,
        });
    }
    extra
}

/// Run a parsed CLI invocation against `db`, rendering the result in the
/// resolved [`Format`] (human by default, JSON when piped or forced).
pub fn run(cli: Cli, db: &Database) -> Result<(), String> {
    // A peer probing this machine looks for its CLI under the data dir; keep
    // that pointer true (a readlink when it already is).
    crate::session_ops::host_cli::advertise_running_cli();
    let format = Format::resolve(FormatFlags {
        json: cli.json,
        pretty: cli.pretty,
        text: cli.text,
        toon: cli.toon,
    });
    let mut output: CommandOutput = match cli.command {
        // No subcommand: live state, not a usage dump (AXI principle 8).
        None => home::run(db),
        Some(command) => dispatch(command, db),
    }?;
    if cli.full {
        output.agent.max_text = None;
    }
    if let Some(spec) = &cli.fields {
        output.agent.fields = parse_fields(spec);
    }

    println!("{}", format.render(&output));
    // A command can render normally yet still request a non-zero exit (e.g.
    // `config validate` on an invalid file).
    match output.failure {
        Some(msg) => Err(msg),
        None => Ok(()),
    }
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
fn dispatch(command: Command, db: &Database) -> Result<CommandOutput, String> {
    match command {
        Command::Editor { action } => editor::run(action, db),
        Command::Session { action } => sessions::run(action, db),
        Command::Automation { action } => automations::run(action, db),
        Command::Task { action } => tasks::run(action, db),
        Command::Message { action } => messages::run(action, db),
        Command::Config { action } => config::run(action, db),
        Command::Extension { action } => extensions::run(action, db),
        Command::Version(args) => Ok(version::run(args)),
        Command::Update(args) => Ok(update::run(args)),
        Command::Notify(args) => Ok(notify::run(args)),
        Command::Perf => perf::run(db),
        // The only command that needs no database: a plugin is a file.
        Command::Plugin { action } => plugins::run(action),
    }
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
pub fn error_output(message: &str, suggestion: &str, next: &str, format: Format) -> String {
    let out = CommandOutput::new(
        serde_json::json!({ "error": message, "suggestion": suggestion }),
        format!("error: {message}\n  {suggestion}"),
    )
    .help([next]);
    format.render(&out)
}
