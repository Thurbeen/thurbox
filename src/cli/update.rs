//! `thurbox-cli update` — download, verify, and replace the installed binaries
//! with the newest GitHub release of this binary's major line.
//!
//! Gated behind the `[features] auto_update` flag (on by default for 1.0,
//! since thurbox now keeps itself current — it makes a network call and
//! replaces files on disk). When the flag is off, `update` prints a one-line
//! hint on how to enable it instead of reaching the network. `--force`
//! re-downloads and replaces even when up to date or on a development build,
//! and still never installs another major.

use clap::Args;
use serde_json::json;

use crate::session::settings;

use super::output::{kv, CommandOutput};

/// `update` subcommand arguments.
#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Update even if up to date or on a dev build (re-download + replace). Never
    /// installs another major version.
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
        Ok(crate::agent::self_update::UpdateOutcome::Updated {
            from,
            to,
            next_major,
        }) => {
            let mut fields = vec![
                ("from", from.clone()),
                ("to", to.clone()),
                ("updated", "true".to_string()),
                ("note", "restart thurbox to apply".to_string()),
            ];
            if let Some(next) = &next_major {
                fields.push(("next major", next_major_note(next)));
            }
            let summary = updated_summary(&from, &to, next_major.as_deref());
            CommandOutput::new(
                json!({
                    "version": from,
                    "latest": to,
                    "next_major": next_major,
                    "updated": true,
                    "update_enabled": true,
                    "summary": summary,
                }),
                kv(&fields),
            )
        }
        Ok(crate::agent::self_update::UpdateOutcome::UpToDate {
            current,
            latest,
            next_major,
        }) => {
            let summary = up_to_date_summary(next_major.as_deref());
            CommandOutput::new(
                json!({
                    "version": current,
                    "latest": latest,
                    "next_major": next_major,
                    "updated": false,
                    "update_enabled": true,
                    "summary": summary,
                }),
                format!("thurbox {current} (latest: {latest})\n{summary}"),
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

/// What a user on this line is told about a newer major: that it exists, that
/// no update command takes it, and where moving is explained.
pub(super) fn next_major_note(next: &str) -> String {
    format!(
        "v{next} is a new major release. thurbox never moves to another major \
         by itself, and `update` will not install it; moving is a reinstall — \
         see {}",
        crate::agent::version_check::MIGRATION_NOTES_URL
    )
}

fn updated_summary(from: &str, to: &str, next_major: Option<&str>) -> String {
    with_next_major(
        format!("Updated {from} → {to}. Restart thurbox to apply."),
        next_major,
    )
}

fn up_to_date_summary(next_major: Option<&str>) -> String {
    with_next_major(
        "Up to date — running the latest release.".to_string(),
        next_major,
    )
}

pub(super) fn with_next_major(mut summary: String, next_major: Option<&str>) -> String {
    if let Some(next) = next_major {
        summary.push(' ');
        summary.push_str(&next_major_note(next));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_summary_names_the_next_major_after_one_space() {
        assert_eq!(
            updated_summary("2.84.1", "2.84.2", Some("3.0.0")),
            format!(
                "Updated 2.84.1 → 2.84.2. Restart thurbox to apply. {}",
                next_major_note("3.0.0")
            )
        );
        assert_eq!(
            up_to_date_summary(Some("3.0.0")),
            format!(
                "Up to date — running the latest release. {}",
                next_major_note("3.0.0")
            )
        );
        assert_eq!(
            up_to_date_summary(None),
            "Up to date — running the latest release."
        );
    }

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
