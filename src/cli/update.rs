//! `thurbox-cli update` — download, verify, and replace the installed binaries
//! with the latest GitHub release.
//!
//! Gated behind the `[features] auto_update` flag (on by default for 1.0,
//! since thurbox now keeps itself current — it makes a network call and
//! replaces files on disk). When the flag is off, `update` prints a one-line
//! hint on how to enable it instead of reaching the network. `--force`
//! re-downloads and replaces even when up to date or on a development build.

use clap::Args;
use serde_json::json;

use crate::session::settings;

use super::output::{kv, CommandOutput};

/// `update` subcommand arguments.
#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Update even if up to date or on a dev build (re-download + replace).
    #[arg(long)]
    pub force: bool,
}

/// Run the `update` command. Takes no database — it only reads the compiled-in
/// version, the network, and the install directory.
pub fn run(args: UpdateArgs) -> CommandOutput {
    run_with(args, settings::global().features.auto_update)
}

/// Same as [`run`] but with the `auto_update` flag passed explicitly, so the
/// disabled path is testable without depending on the process-global default
/// (the flag is on by default for 1.0).
fn run_with(args: UpdateArgs, enabled: bool) -> CommandOutput {
    let current = crate::agent::version_check::current_version();

    if !enabled {
        let hint = "update is disabled. Enable it by setting \
                    `[features] auto_update = true` in settings.toml.";
        return CommandOutput::new(
            json!({
                "version": current,
                "update_enabled": false,
                "summary": hint,
            }),
            format!("thurbox {current}\n{hint}"),
        );
    }

    match crate::agent::self_update::perform_update(args.force) {
        Ok(crate::agent::self_update::UpdateOutcome::Updated { from, to }) => {
            let human = kv(&[
                ("from", from.clone()),
                ("to", to.clone()),
                ("updated", "true".to_string()),
                ("note", "restart thurbox to apply".to_string()),
            ]);
            CommandOutput::new(
                json!({
                    "version": from,
                    "latest": to,
                    "updated": true,
                    "update_enabled": true,
                    "summary": format!("Updated {from} → {to}. Restart thurbox to apply."),
                }),
                human,
            )
        }
        Ok(crate::agent::self_update::UpdateOutcome::UpToDate { current, latest }) => {
            CommandOutput::new(
                json!({
                    "version": current,
                    "latest": latest,
                    "updated": false,
                    "update_enabled": true,
                    "summary": "Up to date — running the latest release.",
                }),
                format!(
                "thurbox {current} (latest: {latest})\nUp to date — running the latest release."
            ),
            )
        }
        Ok(crate::agent::self_update::UpdateOutcome::SkippedDevBuild { current }) => {
            CommandOutput::new(
                json!({
                    "version": current,
                    "updated": false,
                    "update_enabled": true,
                    "summary": "Development build — skipped. Use --force to update anyway.",
                }),
                format!(
                "thurbox {current}\nDevelopment build — skipped (use --force to update anyway)."
            ),
            )
        }
        Err(e) => CommandOutput::failed(
            json!({
                "version": current,
                "updated": false,
                "update_enabled": true,
                "error": e,
            }),
            format!("thurbox {current}\nUpdate failed: {e}"),
            format!("update failed: {e}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_when_flag_disabled_prints_enable_hint() {
        // The flag is on by default for 1.0, so exercise the disabled path
        // directly via `run_with` — no process-global settings, no network or
        // disk touch.
        let out = run_with(UpdateArgs { force: false }, false);
        assert_eq!(out["update_enabled"], false);
        assert!(out.human.contains("disabled"), "got: {}", out.human);
        assert!(
            out.failure.is_none(),
            "the hint is informational, not an error"
        );
    }
}
