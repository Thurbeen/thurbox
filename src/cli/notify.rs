//! `thurbox-cli notify` — diagnose OS desktop notifications.
//!
//! The default (no flags) prints which delivery backend thurbox would use on
//! this host, whether it can actually deliver, whether click-to-focus is
//! supported, and the last recorded delivery error (if any). This exists
//! because the WSL failure mode used to be entirely silent — the dbus path
//! errored and only logged a `warn!` to the file logger.
//!
//! With `--test`, it fires a sample notification *synchronously* so the user
//! can confirm end-to-end that a banner/toast actually appears.

use clap::Args;
use serde_json::json;

use crate::notifications::{self, DeliveryBackend, Notification};
use crate::session::settings;
use crate::session::SessionId;

use super::output::{kv, CommandOutput};

/// `notify` subcommand arguments.
#[derive(Args, Debug)]
pub struct NotifyArgs {
    /// Fire a sample notification to confirm delivery works end-to-end.
    #[arg(long)]
    pub test: bool,
}

/// Run the `notify` command. Takes no database — it reads the process-wide
/// settings (for the configured backend) and probes the host.
pub fn run(args: NotifyArgs) -> CommandOutput {
    let s = settings::global();
    let feature_on = s.features.notifications;
    let configured = s.notifications.backend;
    let backend = notifications::detect_backend(configured);

    if args.test {
        return run_test(feature_on, backend);
    }

    let last_error = notifications::last_error();
    let mut pairs = vec![
        ("feature", on_off(feature_on)),
        ("configured", format!("{configured:?}").to_lowercase()),
        ("backend", backend.label().to_string()),
        ("deliverable", yes_no(backend.is_deliverable())),
        ("click-to-focus", yes_no(backend.supports_click_to_focus())),
    ];
    if let Some(err) = &last_error {
        pairs.push(("last_error", err.clone()));
    }

    // Lead with the most actionable line, then the detail block.
    let headline = diagnose_headline(feature_on, backend);
    let human = format!("{headline}\n\n{}", kv(&pairs));

    CommandOutput::new(
        json!({
            "feature_enabled": feature_on,
            "configured_backend": format!("{configured:?}").to_lowercase(),
            "resolved_backend": backend.label(),
            "deliverable": backend.is_deliverable(),
            "click_to_focus": backend.supports_click_to_focus(),
            "last_error": last_error,
            "summary": headline,
        }),
        human,
    )
}

fn run_test(feature_on: bool, backend: DeliveryBackend) -> CommandOutput {
    if !feature_on {
        let msg = "Notifications are disabled ([features] notifications = false); \
                   nothing to test. Enable the feature first.";
        return CommandOutput::new(
            json!({
                "feature_enabled": false,
                "tested": false,
                "summary": msg,
            }),
            msg,
        );
    }

    let n = Notification {
        session_id: SessionId::default(),
        title: "thurbox · test".into(),
        body: "Notifications are working. This is a test from `thurbox-cli notify --test`.".into(),
        sound: settings::global().notifications.sound,
    };

    match notifications::send_blocking(&n, backend) {
        Ok(()) => {
            let msg = format!(
                "Sent a test notification via {}. If you don't see it, check your OS \
                 notification settings.",
                backend.label()
            );
            CommandOutput::new(
                json!({
                    "feature_enabled": true,
                    "tested": true,
                    "delivered": true,
                    "resolved_backend": backend.label(),
                    "summary": msg,
                }),
                msg,
            )
        }
        Err(e) => {
            let msg = format!("Test notification failed via {}: {e}", backend.label());
            CommandOutput::failed(
                json!({
                    "feature_enabled": true,
                    "tested": true,
                    "delivered": false,
                    "resolved_backend": backend.label(),
                    "error": e,
                    "summary": msg,
                }),
                msg.clone(),
                msg,
            )
        }
    }
}

/// One actionable summary line tailored to the resolved state.
fn diagnose_headline(feature_on: bool, backend: DeliveryBackend) -> String {
    if !feature_on {
        return "Notifications are OFF ([features] notifications = false).".into();
    }
    if !backend.is_deliverable() {
        return format!(
            "Notifications are enabled but have no working backend ({}). On WSL, ensure \
             powershell.exe is on PATH; otherwise install a notification daemon or set \
             [notifications] backend.",
            backend.label()
        );
    }
    format!(
        "Notifications enabled — delivering via {}. Run `thurbox-cli notify --test` to confirm.",
        backend.label()
    )
}

fn on_off(b: bool) -> String {
    if b {
        "on".into()
    } else {
        "off".into()
    }
}

fn yes_no(b: bool) -> String {
    if b {
        "yes".into()
    } else {
        "no".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_default_reports_backend_fields() {
        // In the test process settings::global() is the all-default config
        // (feature on, backend auto); host probing runs against the real test
        // host, so we only assert the shape, not the concrete backend.
        let out = run(NotifyArgs { test: false });
        assert!(out["feature_enabled"].is_boolean());
        assert!(out["resolved_backend"].is_string());
        assert!(out["deliverable"].is_boolean());
        assert!(out["summary"].is_string());
        assert!(out.failure.is_none(), "the report itself never fails");
    }

    #[test]
    fn headline_calls_out_disabled_feature() {
        let h = diagnose_headline(false, DeliveryBackend::Dbus);
        assert!(h.contains("OFF"), "got: {h}");
    }

    #[test]
    fn headline_calls_out_no_backend() {
        let h = diagnose_headline(true, DeliveryBackend::None);
        assert!(h.contains("no working backend"), "got: {h}");
        assert!(h.contains("WSL"), "should hint at the WSL case: {h}");
    }

    #[test]
    fn headline_confirms_a_working_backend() {
        let h = diagnose_headline(true, DeliveryBackend::Dbus);
        assert!(h.contains("delivering via"), "got: {h}");
    }
}
