use clap::Args;
use serde_json::json;

use super::output::{kv, CommandOutput};

#[derive(Args, Debug)]
pub struct UpdateArgs {
    #[arg(
        help = "Update even if up to date or on a dev build (re-download + replace). Never installs another major version"
    )]
    #[arg(long)]
    pub force: bool,
}

pub fn run(
    args: UpdateArgs,
    enabled: bool,
    fleets: &[crate::fleet_link::FleetLink],
) -> CommandOutput {
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
            let fleet = crate::kernel::snapshot::fleet::stale_managers(fleets, &to);
            let mut fields = vec![
                ("from", from.clone()),
                ("to", to.clone()),
                ("updated", "true".to_string()),
                ("note", "restart thurbox to apply".to_string()),
            ];
            let mut summary = format!("Updated {from} → {to}. Restart thurbox to apply.");
            if !fleet.is_empty() {
                fields.push(("fleet", fleet.clone()));
                summary.push(' ');
                summary.push_str(&fleet);
            }
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
                    "fleet": fleet,
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
        let out = run(UpdateArgs { force: false }, false, &[]);
        assert_eq!(out["update_enabled"], false);
        assert!(out.human.contains("disabled"), "got: {}", out.human);
        assert!(
            out.failure.is_none(),
            "the hint is informational, not an error"
        );
    }
}
