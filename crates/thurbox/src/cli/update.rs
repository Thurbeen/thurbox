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
///
/// `enabled` is `[features] auto_update` as the process loaded it.
pub fn run(args: UpdateArgs, enabled: bool) -> CommandOutput {
    let current = crate::update::version_check::current_version();

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

    match crate::update::self_update::perform_update(args.force) {
        Ok(crate::update::self_update::UpdateOutcome::Updated {
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
            let mut summary = format!("Updated {from} → {to}. Restart thurbox to apply.");
            if let Some(next) = &next_major {
                let note = next_major_note(next);
                fields.push(("next major", note.clone()));
                summary.push(' ');
                summary.push_str(&note);
            }
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
        Ok(crate::update::self_update::UpdateOutcome::UpToDate {
            current,
            latest,
            next_major,
        }) => {
            let mut summary = "Up to date — running the latest release.".to_string();
            if let Some(next) = &next_major {
                summary.push(' ');
                summary.push_str(&next_major_note(next));
            }
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
        Ok(crate::update::self_update::UpdateOutcome::SkippedDevBuild { current }) => {
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
        crate::update::version_check::MIGRATION_NOTES_URL
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_when_flag_disabled_prints_enable_hint() {
        // The flag is on by default for 1.0, so exercise the disabled path
        // directly, with the flag passed in — no network or
        // disk touch.
        let out = run(UpdateArgs { force: false }, false);
        assert_eq!(out["update_enabled"], false);
        assert!(out.human.contains("disabled"), "got: {}", out.human);
        assert!(
            out.failure.is_none(),
            "the hint is informational, not an error"
        );
    }
}
