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
///
/// `enabled` is `[features] version_check` as the process loaded it, and
/// `fleet` the Fleet in force, which it reports beside the version.
pub fn run(
    args: VersionArgs,
    enabled: bool,
    fleet: &crate::fleet_link::FleetLink,
) -> CommandOutput {
    let current = crate::update::version_check::current_version();

    if !args.check {
        // Where this build's interface state lives and which tmux socket it
        // names: what tells a sandbox apart from the real instance.
        return CommandOutput::new(
            json!({
                "version": current,
                "tmux_socket": crate::backend::instance::local_socket_name(),
                "tmux_tmpdir": crate::backend::instance::local_socket_dir_report(),
                "data_dir": crate::paths::data_directory().map(|d| d.display().to_string()),
                "fleet": super::doctor::FleetFacts::read(fleet).document(),
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

    match crate::update::version_check::refresh_cache() {
        Ok((releases, status)) => {
            let latest = releases.line.unwrap_or_else(|| current.to_string());
            let next_major = releases.next_major;
            let mut summary = match &status {
                Some(_) => format!("Update available: {current} → {latest}"),
                None => "Up to date — running the latest release.".to_string(),
            };
            if let Some(next) = &next_major {
                summary.push_str(". ");
                summary.push_str(&super::update::next_major_note(next));
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_without_check_prints_current_version() {
        let out = run(
            VersionArgs { check: false },
            true,
            &crate::fleet_link::FleetLink::local("fleet"),
        );
        assert!(out["version"].is_string(), "version field present");
        assert!(out["tmux_socket"].is_string(), "the socket in force");
        assert!(out.human.starts_with("thurbox "), "got: {}", out.human);
        assert!(out.failure.is_none(), "plain version never fails");
    }

    #[test]
    fn version_check_when_flag_disabled_prints_enable_hint() {
        // The flag is on by default for 1.0, so exercise the disabled path
        // directly, with the flag passed in — no network.
        let out = run(
            VersionArgs { check: true },
            false,
            &crate::fleet_link::FleetLink::local("fleet"),
        );
        assert_eq!(out["check_enabled"], false);
        assert!(out.human.contains("disabled"), "got: {}", out.human);
        assert!(
            out.failure.is_none(),
            "the hint is informational, not an error"
        );
    }
}
