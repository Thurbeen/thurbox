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
//!
//! `notify send` delivers one notification of the caller's own, from a script
//! or an Exec automation, with an optional dedupe key.

use std::time::Duration;

use clap::{Args, Subcommand};
use serde_json::json;

use crate::notifications::{self, DeliveryBackend, Notification};
use crate::session::settings::{NotificationBackend, Settings};

use super::output::{kv, CommandOutput};
use super::CommandError;

/// `notify` arguments.
#[derive(Args, Debug)]
pub struct NotifyArgs {
    /// Fire a sample notification to confirm delivery works end-to-end.
    #[arg(long)]
    pub test: bool,
    #[command(subcommand)]
    pub action: Option<Action>,
}

/// Run the `notify` command.
pub fn run(args: NotifyArgs, settings: &Settings) -> Result<CommandOutput, CommandError> {
    match args.action {
        Some(Action::Send(send_args)) => send(send_args, settings),
        None => Ok(diagnose(args.test, settings)),
    }
}

/// `notify` subcommands.
#[derive(Subcommand, Debug)]
pub enum Action {
    /// Deliver one notification now, e.g. from a script or an Exec
    /// automation. Exits 1 when delivery fails.
    Send(SendArgs),
}

/// `notify send` arguments.
#[derive(Args, Debug)]
pub struct SendArgs {
    #[arg(long)]
    pub title: String,
    #[arg(long)]
    pub body: String,
    /// Drop a repeat of this key inside `--min-interval`, across processes.
    #[arg(long, requires = "min_interval")]
    pub key: Option<String>,
    /// Seconds a delivered `--key` holds off its repeats; at most a year.
    #[arg(
        long,
        requires = "key",
        value_parser = clap::value_parser!(u64).range(1..=MAX_MIN_INTERVAL_SECS)
    )]
    pub min_interval: Option<u64>,
}

/// The longest `--min-interval`: a year.
const MAX_MIN_INTERVAL_SECS: u64 = 365 * 24 * 60 * 60;

/// How long a send waits for the backend. The toast and macOS paths wait on a
/// child process, and one that never returns must not hold up the script or
/// the automation that sent it.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// What became of a send.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sent {
    Delivered,
    /// `disabled` or `deduped`.
    Suppressed(&'static str),
    Failed(String),
}

impl Sent {
    fn to_json(&self) -> serde_json::Value {
        match self {
            Sent::Delivered => json!({ "delivered": true, "suppressed": null, "error": null }),
            Sent::Suppressed(why) => {
                json!({ "delivered": false, "suppressed": why, "error": null })
            }
            Sent::Failed(e) => json!({ "delivered": false, "suppressed": null, "error": e }),
        }
    }
}

/// Deliver one notification, unless notifications are off or its key was
/// delivered inside its interval.
///
/// The key is held in `notify-dedupe.json` in the data directory, under the
/// file lock every process takes, before delivery: two processes racing on
/// one key deliver once. A delivery that fails gives back the hold it took,
/// and only that one, so the retry is not deduped against a notification
/// nobody saw, and a newer hold of the same key survives a late failure.
fn send(args: SendArgs, s: &Settings) -> Result<CommandOutput, CommandError> {
    if !s.features.notifications || s.notifications.backend == NotificationBackend::Off {
        return Ok(CommandOutput::new(
            Sent::Suppressed("disabled").to_json(),
            "Notification suppressed (disabled).",
        ));
    }
    let hold = match (&args.key, args.min_interval) {
        (Some(key), Some(secs)) => {
            let now = now_ms();
            let expires = now.saturating_add(secs.saturating_mul(1000));
            if !dedupe::take(key, expires, now)? {
                return Ok(CommandOutput::new(
                    Sent::Suppressed("deduped").to_json(),
                    format!("Notification suppressed: {key} was sent within its interval."),
                ));
            }
            Some((key.clone(), expires))
        }
        _ => None,
    };
    let n = Notification {
        session_id: String::new(),
        title: args.title,
        body: args.body,
        sound: s.notifications.sound,
    };
    let sent = match deliver(n, notifications::detect_backend(s.notifications.backend)) {
        Ok(()) => Sent::Delivered,
        Err(e) => {
            if let Some((key, expires)) = &hold {
                if let Err(e) = dedupe::give_back(key, *expires) {
                    tracing::warn!("give back notification key {key}: {e}");
                }
            }
            Sent::Failed(e)
        }
    };
    Ok(match &sent {
        Sent::Failed(e) => {
            let msg = format!("Notification failed: {e}");
            CommandOutput::failed(sent.to_json(), msg.clone(), msg)
        }
        _ => CommandOutput::new(sent.to_json(), "Notification delivered."),
    })
}

/// Deliver on a thread of its own and wait at most [`DELIVERY_TIMEOUT`]. A
/// backend that hangs is left behind; the process ends without it.
fn deliver(n: Notification, backend: DeliveryBackend) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(notifications::send_blocking(&n, backend));
    });
    rx.recv_timeout(DELIVERY_TIMEOUT).unwrap_or_else(|_| {
        Err(format!(
            "delivery timed out after {}s",
            DELIVERY_TIMEOUT.as_secs()
        ))
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// The dedupe keys: key → when its hold ends (unix millis), in
/// `notify-dedupe.json`, rewritten whole under the file lock.
mod dedupe {
    use std::collections::BTreeMap;

    type Holds = BTreeMap<String, u64>;

    fn edit<T>(change: impl FnOnce(&mut Holds) -> T) -> Result<T, String> {
        let path =
            crate::paths::notify_dedupe_file().ok_or("could not resolve the data directory")?;
        crate::paths::with_file_lock(&path, || {
            let mut holds: Holds = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            let answer = change(&mut holds);
            let text = serde_json::to_string_pretty(&holds).map_err(|e| e.to_string())?;
            crate::paths::write_atomically(&path, text).map_err(|e| e.to_string())?;
            Ok(answer)
        })
        .map_err(|e| format!("dedupe key lock: {e}"))?
    }

    /// Hold `key` until `expires`, unless a hold of it is still running at
    /// `now`. Whether this call took it.
    pub(super) fn take(key: &str, expires: u64, now: u64) -> Result<bool, String> {
        edit(|holds| {
            holds.retain(|_, until| *until > now);
            if holds.contains_key(key) {
                return false;
            }
            holds.insert(key.to_string(), expires);
            true
        })
    }

    /// Release the hold of `key` that ends at `expires`, if it is still that
    /// one.
    pub(super) fn give_back(key: &str, expires: u64) -> Result<(), String> {
        edit(|holds| {
            if holds.get(key) == Some(&expires) {
                holds.remove(key);
            }
        })
    }
}

/// The report `notify` with no subcommand prints. Reads the settings (for the
/// configured backend) and probes the host.
fn diagnose(test: bool, s: &Settings) -> CommandOutput {
    let feature_on = s.features.notifications;
    let configured = s.notifications.backend;
    let backend = notifications::detect_backend(configured);

    if test {
        return run_test(feature_on, backend, s.notifications.sound);
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

fn run_test(feature_on: bool, backend: DeliveryBackend, sound: bool) -> CommandOutput {
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
        session_id: String::new(),
        title: "thurbox · test".into(),
        body: "Notifications are working. This is a test from `thurbox-cli notify --test`.".into(),
        sound,
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
        // The all-default config (feature on, backend auto); host probing runs
        // against the real test host, so we only assert the shape, not the
        // concrete backend.
        let out = diagnose(false, &Settings::default());
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
