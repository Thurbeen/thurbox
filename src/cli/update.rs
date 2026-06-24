//! `thurbox-cli update` — download, verify, and replace the installed binaries
//! with the latest GitHub release.
//!
//! Gated behind the opt-in `[features] auto_update` flag (off by default, since
//! it makes a network call and replaces files on disk). When the flag is off,
//! `update` prints a one-line hint on how to enable it instead of reaching the
//! network. `--force` re-downloads and replaces even when up to date or on a
//! development build.

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
    let current = crate::agent::version_check::current_version();

    if !settings::global().features.auto_update {
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
        // `settings::init` is only ever called by the binaries, so in the test
        // process `settings::global()` is the all-default config — auto_update
        // is off — and `update` degrades to the enable hint with no network or
        // disk touch.
        let out = run(UpdateArgs { force: false });
        assert_eq!(out["update_enabled"], false);
        assert!(out.human.contains("disabled"), "got: {}", out.human);
        assert!(
            out.failure.is_none(),
            "the hint is informational, not an error"
        );
    }
}
