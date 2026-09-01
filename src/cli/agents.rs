//! `thurbox-cli agent` — read the registry a session's launch is resolved from.
//!
//! Read-only, and one verb: what thurbox would run to start a registered agent.
//!
//! It exists because the status hooks that make `state` and `watch` work are
//! *arguments*. The `hooks` extension installs them by appending to an agent's
//! `args` in `agents.toml` (`--settings <hooks>.json` for claude), so they only
//! reach the process when thurbox builds the command line. A driver that
//! launches the agent itself — through `session create --command`, or by typing
//! into a shell session — had no way to obtain them, so its sessions reported
//! nothing and `watch` never mentioned them. That is exactly the integrator
//! `session signal`'s help invites in ("a driver that launches its own agent
//! there"), and this is the missing half of the invitation.

use clap::Subcommand;
use serde_json::json;

use crate::cli::output::CommandOutput;
use crate::storage::Database;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Print how thurbox would launch a registered agent: `command`, `args`,
    /// `env`.
    ///
    /// Pass the args through and the agent's status hooks fire, so the session
    /// reports `state` and appears in `watch` — which is the whole reason to
    /// ask. Without `--session` the environment names this thurbox instance
    /// (config dir, data dir, multiplexer socket) and nothing more; with it,
    /// the agent's conversation id is pinned to that session's and the
    /// `THURBOX_*` identity its `session signal` needs is included.
    ///
    /// This is a fresh launch, never a resume: continuing an existing
    /// conversation is `session start` / `session restart`.
    LaunchArgs {
        /// Agent name from `agents.toml` (e.g. `claude`, `codex`).
        agent: String,
        /// Resolve for this session — name, UUID, or unique id prefix.
        #[arg(long)]
        session: Option<String>,
    },
}

pub fn run(action: Action, db: &Database) -> Result<CommandOutput, String> {
    match action {
        Action::LaunchArgs { agent, session } => {
            let session = session
                .as_deref()
                .map(|reference| super::sessions::resolve(db, reference))
                .transpose()?;
            let plan = crate::session_ops::agent_launch_plan(db, &agent, session.as_ref())?;
            let mut human = format!("{} {}", plan.command, plan.args.join(" "));
            for (key, value) in &plan.env {
                human.push_str(&format!("\n  {key}={value}"));
            }
            if !plan.hooks_enabled {
                human.push_str(
                    "\n  ! the hooks extension is not active, so these args carry no status \
                     wiring",
                );
            }
            if let Some(note) = &plan.degraded {
                human.push_str(&format!("\n  ! {note}"));
            }
            // The registry's own account of what this agent could report, so an
            // empty `args` is readable: an uninstrumented agent has none to
            // give, an instrumented one with the extension off has them
            // uninstalled, and the two look identical in the args alone.
            let coverage =
                crate::session::coverage_for(&crate::agent::agent_config::load_or_seed(), &agent);
            Ok(CommandOutput::new(
                json!({
                    "agent": plan.agent,
                    "command": plan.command,
                    "args": plan.args,
                    "env": plan.env,
                    "session_id": session.as_ref().map(|s| s.id.to_string()),
                    "hooks_enabled": plan.hooks_enabled,
                    "hook_coverage": crate::session::Coverage::of(coverage.map(|(c, _)| c)).as_str(),
                    "hook_states_reportable": coverage.map(|(c, _)| c.states).unwrap_or(&[]),
                    "degraded": plan.degraded,
                }),
                human,
            )
            .help([
                "thurbox-cli agent launch-args <name> --session <ref>   with that session's identity",
                "thurbox-cli session create --name x --repo-path . --command <command> --arg <arg>   run it as a session",
                "thurbox-cli session doctor <ref>   whether its reports are arriving",
            ]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `agents.toml` carrying the shape the hooks extension leaves behind:
    /// the status wiring is an appended argument, which is the thing a driver
    /// cannot otherwise obtain.
    const AGENTS_TOML: &str = r#"
config_version = 1
default = "claude"

[[agents]]
name = "claude"
command = "claude"
args = ["--settings", "/opt/hooks/claude.json"]
new_session_args = ["--session-id", "{id}"]

[[agents]]
name = "shell"
command = "bash"
args = ["-i"]
"#;

    fn isolated(dir: &std::path::Path) -> crate::paths::TestPathGuard {
        let guard = crate::paths::TestPathGuard::new(dir);
        let path = crate::agent::agent_config::agents_config_path().expect("agents path");
        std::fs::create_dir_all(path.parent().expect("config dir")).expect("mkdir");
        std::fs::write(&path, AGENTS_TOML).expect("write agents.toml");
        guard
    }

    #[test]
    fn launch_args_reports_the_hook_wiring() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = isolated(temp.path());
        let db = Database::open_in_memory().unwrap();

        let out = run(
            Action::LaunchArgs {
                agent: "claude".into(),
                session: None,
            },
            &db,
        )
        .expect("launch-args");

        assert_eq!(out["command"], json!("claude"));
        assert_eq!(out["args"], json!(["--settings", "/opt/hooks/claude.json"]));
        // No session, so no identity to lend — and saying nothing is right:
        // an empty THURBOX_SESSION would report for no session at all.
        assert!(out["env"]["THURBOX_SESSION"].is_null());
        assert!(out["session_id"].is_null());
    }

    #[test]
    fn an_unknown_agent_names_the_ones_there_are() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = isolated(temp.path());
        let db = Database::open_in_memory().unwrap();

        let err = run(
            Action::LaunchArgs {
                agent: "nope".into(),
                session: None,
            },
            &db,
        )
        .expect_err("unknown agent");
        assert!(err.contains("claude"), "got {err}");
        assert!(err.contains("shell"), "got {err}");
    }
}
