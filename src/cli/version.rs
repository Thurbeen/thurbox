//! `thurbox-cli version [--check]` — print the running version and, with
//! `--check`, query GitHub for the newest release of this binary's major line
//! (and report, never offer, a newer major).
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
        // The extra facts are what a *peer* thurbox reads when it probes this
        // machine's CLI before delegating to it (`session_ops::host_cli`): the
        // tmux server this build's sessions live on, where its data lives, and
        // whether the two databases speak the same schema.
        return CommandOutput::new(
            json!({
                "version": current,
                "tmux_socket": crate::backend::instance::local_socket_name(),
                "tmux_tmpdir": crate::backend::instance::local_socket_dir_report(),
                "data_dir": crate::paths::database_file()
                    .and_then(|p| p.parent().map(|d| d.display().to_string())),
                "schema_version": crate::storage::SCHEMA_VERSION,
                "multiplexer_choice": true,
                "soft_delete_choice": true,
                "session_revision": true,
                "session_wait": true,
                "session_state_hooks": true,
                "session_pre_launch": true,
                "deleted_since": true,
                "deleted_changes_since": true,
                "create_parity": true,
                "agent_args": true,
                "telemetry_attrs": true,
                "purged_tombstones": true,
                "lifecycle_preconditions": true,
                "session_restart_fresh": true,
            }),
            format!("thurbox {current}"),
        );
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
        Ok((releases, status)) => {
            let latest = releases.line.unwrap_or_else(|| current.to_string());
            let next_major = releases.next_major;
            let summary = check_summary(current, &latest, status.is_some(), next_major.as_deref());
            let mut fields = vec![("current", current.to_string()), ("latest", latest.clone())];
            if status.is_some() {
                fields.push(("update", "available".to_string()));
                fields.push(("upgrade", "thurbox-cli update".to_string()));
            }
            if let Some(next) = &next_major {
                fields.push((
                    "next major",
                    format!("v{next} (not installed automatically)"),
                ));
            }
            CommandOutput::new(
                json!({
                    "version": current,
                    "latest": latest,
                    "update_available": status.is_some(),
                    "next_major": next_major,
                    "check_enabled": true,
                    "summary": summary,
                }),
                format!("{}\n{summary}", kv(&fields)),
            )
        }
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

fn check_summary(current: &str, latest: &str, available: bool, next_major: Option<&str>) -> String {
    let summary = if available {
        format!("Update available: {current} → {latest}.")
    } else {
        "Up to date — running the latest release.".to_string()
    };
    super::update::with_next_major(summary, next_major)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_summary_names_the_next_major_after_one_space() {
        let note = super::super::update::next_major_note("3.0.0");
        assert_eq!(
            check_summary("2.84.1", "2.84.1", false, Some("3.0.0")),
            format!("Up to date — running the latest release. {note}")
        );
        assert_eq!(
            check_summary("2.84.1", "2.84.2", true, Some("3.0.0")),
            format!("Update available: 2.84.1 → 2.84.2. {note}")
        );
        assert_eq!(
            check_summary("2.84.1", "2.84.2", true, None),
            "Update available: 2.84.1 → 2.84.2."
        );
    }

    #[test]
    fn version_without_check_prints_current_version() {
        let out = run(VersionArgs { check: false });
        assert!(out["version"].is_string(), "version field present");
        assert_eq!(out["multiplexer_choice"], true);
        assert_eq!(out["soft_delete_choice"], true);
        assert_eq!(out["session_revision"], true);
        assert_eq!(out["deleted_since"], true);
        assert_eq!(out["deleted_changes_since"], true);
        assert_eq!(out["create_parity"], true);
        assert_eq!(out["telemetry_attrs"], true);
        assert_eq!(out["session_restart_fresh"], true);
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
