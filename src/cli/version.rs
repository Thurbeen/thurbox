//! `thurbox-cli version [--check]` — print the running version and, with
//! `--check`, query GitHub for the latest release.
//!
//! `--check` is gated behind the `[features] version_check` flag (on by
//! default for 1.0, since thurbox now keeps itself current). When the flag is
//! off, `--check` prints a one-line hint on how to enable it instead of
//! reaching the network.
//! A successful check also refreshes the on-disk cache the TUI badge reads.

use clap::Args;
use serde_json::json;

use crate::session::settings;

use super::output::{kv, CommandOutput};

/// `version` subcommand arguments.
#[derive(Args, Debug)]
pub struct VersionArgs {
    /// Check GitHub for a newer release (requires `[features] version_check`).
    #[arg(long)]
    pub check: bool,
}

/// Run the `version` command. Takes no database — it only reads the compiled-in
/// version and (with `--check`) the network.
pub fn run(args: VersionArgs) -> CommandOutput {
    run_with(args, settings::global().features.version_check)
}

/// Same as [`run`] but with the `version_check` flag passed explicitly, so the
/// disabled path is testable without depending on the process-global default
/// (the flag is on by default for 1.0).
fn run_with(args: VersionArgs, enabled: bool) -> CommandOutput {
    let current = crate::agent::version_check::current_version();

    if !args.check {
        return CommandOutput::new(json!({ "version": current }), format!("thurbox {current}"));
    }

    if !enabled {
        let hint = "version --check is disabled. Enable it by setting \
                    `[features] version_check = true` in settings.toml.";
        return CommandOutput::new(
            json!({
                "version": current,
                "check_enabled": false,
                "summary": hint,
            }),
            format!("thurbox {current}\n{hint}"),
        );
    }

    match crate::agent::version_check::refresh_cache() {
        Ok((_, Some(status))) => {
            let human = kv(&[
                ("current", current.to_string()),
                ("latest", status.latest.clone()),
                ("update", "available".to_string()),
                (
                    "upgrade",
                    "curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh"
                        .to_string(),
                ),
            ]);
            CommandOutput::new(
                json!({
                    "version": current,
                    "latest": status.latest,
                    "update_available": true,
                    "check_enabled": true,
                    "summary": format!("Update available: {current} → {}", status.latest),
                }),
                human,
            )
        }
        Ok((latest, None)) => CommandOutput::new(
            json!({
                "version": current,
                "latest": latest,
                "update_available": false,
                "check_enabled": true,
                "summary": "Up to date — running the latest release.",
            }),
            format!(
                "thurbox {current} (latest: {latest})\nUp to date — running the latest release."
            ),
        ),
        Err(e) => CommandOutput::failed(
            json!({
                "version": current,
                "check_enabled": true,
                "update_available": null,
                "error": e,
            }),
            format!("thurbox {current}\nUpdate check failed: {e}"),
            format!("update check failed: {e}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_without_check_prints_current_version() {
        let out = run(VersionArgs { check: false });
        assert!(out["version"].is_string(), "version field present");
        assert!(out.human.starts_with("thurbox "), "got: {}", out.human);
        assert!(out.failure.is_none(), "plain version never fails");
    }

    #[test]
    fn version_check_when_flag_disabled_prints_enable_hint() {
        // The flag is on by default for 1.0, so exercise the disabled path
        // directly via `run_with` — no process-global settings, no network.
        let out = run_with(VersionArgs { check: true }, false);
        assert_eq!(out["check_enabled"], false);
        assert!(out.human.contains("disabled"), "got: {}", out.human);
        assert!(
            out.failure.is_none(),
            "the hint is informational, not an error"
        );
    }
}
