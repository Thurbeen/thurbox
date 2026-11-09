//! Repository-only gate and release tooling. The installed binary has no tooling dependency.
mod gate;
mod release;
mod workflow;

use clap::{Parser, Subcommand};
use fleet_exec::{Outcome, Spec};
use std::path::PathBuf;
use std::time::Duration;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Parser)]
#[command(about = "Fleet repository tooling. Example: cargo xtask check")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, Subcommand)]
enum Command {
    /// Run the same gate as CI. Example: cargo xtask check rust-test
    Check {
        #[arg(long)]
        fix: bool,
        #[arg(long)]
        list: bool,
        areas: Vec<String>,
    },
    /// Validate every workflow in this checkout, without running it.
    Workflow,
    /// Require success from every declared CI job ($NEEDS is GitHub JSON).
    Verdict,
    /// Validate an actual nextest JSON listing (also used by rust-test).
    Suites { listing: PathBuf },
    /// Plan and package releases without a Python interpreter.
    Release {
        #[command(subcommand)]
        command: release::Command,
    },
}
fn main() -> std::process::ExitCode {
    match run(Cli::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fleet-dev: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
fn run(cli: Cli) -> Result {
    match cli.command {
        Command::Check { fix, list, areas } => gate::check(&areas, fix, list),
        Command::Workflow => workflow::check(),
        Command::Verdict => workflow::verdict(),
        Command::Suites { listing } => {
            gate::suites(&serde_json::from_slice(&std::fs::read(listing)?)?)
        }
        Command::Release { command } => release::run(command),
    }
}
/// All external programs use the runtime's tree-bounded process seam. A hung
/// compiler or a fixture child cannot outlive a timed-out gate.
fn exec(program: &str, args: &[&str], env: &[(&str, &str)]) -> Result<String> {
    let mut spec = Spec::new(program, Duration::from_secs(1800)).args(args.iter().copied());
    spec.env = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    match fleet_exec::run(&spec) {
        Outcome::Exited {
            code: Some(0),
            stdout,
            stderr,
        } => {
            eprint!("{}", String::from_utf8_lossy(&stderr));
            Ok(String::from_utf8(stdout)?)
        }
        Outcome::Exited {
            code,
            stdout,
            stderr,
        } => Err(format!(
            "{program} {}: exit {code:?}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        )
        .into()),
        o => Err(format!("{program}: {}", o.failure()).into()),
    }
}
