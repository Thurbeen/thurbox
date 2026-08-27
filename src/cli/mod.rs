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

#[cfg(test)]
mod tests;

pub mod action;
pub mod automations;
pub mod config;
pub mod editor;
pub mod extensions;
pub mod identity;
pub mod messages;
pub mod notify;
pub mod output;
pub mod perf;
pub mod plugins;
pub mod sessions;
pub mod tasks;
pub mod update;
pub mod version;

use output::{CommandOutput, Format};

/// Thurbox CLI — manage sessions, scheduled commands, and more.
///
/// `version` is spelled out rather than left bare: clap's implicit form reads
/// `CARGO_PKG_VERSION`, which is the static `0.0.0-dev` marker this project
/// never bumps, so `--version` reported a dev build on every release while the
/// `version` subcommand was right. Both now call the one
/// `version_check::current_version`.
#[derive(Parser, Debug)]
#[command(
    name = "thurbox-cli",
    version = crate::agent::version_check::current_version(),
    about
)]
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
        Command::Update(args) => Ok(update::run(args)),
        Command::Notify(args) => Ok(notify::run(args)),
        Command::Perf => perf::run(db),
        // The only command that needs no database: a plugin is a file.
        Command::Plugin { action } => plugins::run(action),
    }?;

    println!("{}", format.render(&output));
    // A command can render normally yet still request a non-zero exit (e.g.
    // `config validate` on an invalid file).
    match output.failure {
        Some(msg) => Err(msg),
        None => Ok(()),
    }
}
