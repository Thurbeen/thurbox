//! `thurbox-cli notify` — diagnose OS desktop notifications, and send one.
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
//! `notify send` is the same delivery for a script, a hook or an Exec
//! automation (issue #1519): it honours both off switches and the configured
//! backend, never touches a session's state, and with `--key` drops a repeat
//! inside `--min-interval`, across processes, through a lease in
//! `event_cursors`. The failing-run alert of `automation tick` is built on
//! `send` (issue #1508).

use std::time::Duration;

use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::notifications::{self, DeliveryBackend, Notification};
use crate::session::settings::{self, NotificationBackend};
use crate::session::SessionId;
use crate::storage::event_cursors::LockSubject;
use crate::storage::Database;
use crate::sync::current_time_millis;

use super::output::{kv, CommandOutput};
use super::CommandError;

/// `notify` subcommand arguments.
#[derive(Args, Debug)]
pub struct NotifyArgs {
    #[command(subcommand)]
    pub action: Option<Action>,
    /// Fire a sample notification to confirm delivery works end-to-end.
    #[arg(long)]
    pub test: bool,
}

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Send a desktop notification through the configured backend. Exits 0
    /// with `suppressed` when notifications are off or the key is deduped.
    Send(SendArgs),
}

#[derive(Args, Debug)]
pub struct SendArgs {
    #[arg(long)]
    pub title: String,
    #[arg(long)]
    pub body: String,
    /// The session the notification is about (id, key or name). Clicking it
    /// focuses that session where the backend supports it.
    #[arg(long)]
    pub session: Option<String>,
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

/// The longest `--min-interval`: a year, far inside what a lease expiry in
/// milliseconds can hold.
const MAX_MIN_INTERVAL_SECS: u64 = 365 * 24 * 60 * 60;

/// How long a send waits for the backend. The toast and macOS paths wait on a
/// child process, and one that never returns must not hold up the tick that
/// sends an alert, nor every scheduled run behind it.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// One notification [`send`] is asked to deliver.
pub(crate) struct Message {
    pub title: String,
    pub body: String,
    pub session: Option<SessionId>,
    /// The dedupe key and how long a delivery of it holds off its repeats.
    pub dedupe: Option<(String, u64)>,
}

/// What became of a [`Message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sent {
    Delivered,
    /// `disabled` or `deduped`.
    Suppressed(&'static str),
    Failed(String),
}

impl Sent {
    pub fn to_json(&self) -> Value {
        match self {
            Sent::Delivered => json!({ "delivered": true, "suppressed": null, "error": null }),
            Sent::Suppressed(why) => {
                json!({ "delivered": false, "suppressed": why, "error": null })
            }
            Sent::Failed(e) => json!({ "delivered": false, "suppressed": null, "error": e }),
        }
    }
}

/// The subject a dedupe key is leased under.
fn dedupe_subject(key: &str) -> LockSubject {
    LockSubject::Notification(key.to_string())
}

/// Deliver `m` synchronously, unless notifications are off or its key was
/// delivered inside its interval. Changes no session state.
///
/// The key is leased before delivery, so two processes racing on one key
/// deliver once; a delivery that fails gives the lease back, so the retry is
/// not deduped against a notification nobody saw.
/// Whether either off switch is set.
fn disabled() -> bool {
    let s = settings::global();
    !s.features.notifications || s.notifications.backend == NotificationBackend::Off
}

pub(crate) fn send(db: &Database, m: &Message) -> Sent {
    if disabled() {
        return Sent::Suppressed("disabled");
    }
    let s = settings::global();
    let leased = match &m.dedupe {
        Some((key, secs)) => {
            let now = current_time_millis();
            if let Err(e) = db.prune_notification_leases(now) {
                tracing::debug!("prune notification leases: {e}");
            }
            let subject = dedupe_subject(key);
            let expires = now.saturating_add(secs.saturating_mul(1000));
            match db.take_lease(&subject, expires, now) {
                Ok(true) => Some((subject, expires)),
                Ok(false) => return Sent::Suppressed("deduped"),
                Err(e) => return Sent::Failed(format!("dedupe lease: {e}")),
            }
        }
        None => None,
    };
    let n = Notification {
        session_id: m.session.unwrap_or_default(),
        title: m.title.clone(),
        body: m.body.clone(),
        sound: s.notifications.sound,
    };
    match deliver(n, notifications::detect_backend(s.notifications.backend)) {
        Ok(()) => Sent::Delivered,
        Err(e) => {
            // Only the hold this send took: past its interval another send
            // may have taken the key, and that hold is not ours to give back.
            if let Some((subject, expires)) = leased {
                if let Err(e) = db.release_lease(&subject, expires) {
                    tracing::warn!("give back notification lease: {e}");
                }
            }
            Sent::Failed(e)
        }
    }
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

/// Forget `key`'s last delivery, so its next send is not deduped.
pub(crate) fn forget(db: &Database, key: &str) -> rusqlite::Result<()> {
    db.drop_lease(&dedupe_subject(key))
}

/// Run the `notify` command.
pub fn run(args: NotifyArgs, db: &Database) -> Result<CommandOutput, CommandError> {
    match args.action {
        Some(Action::Send(send_args)) => run_send(db, send_args),
        None => Ok(diagnose(args.test)),
    }
}

fn run_send(db: &Database, args: SendArgs) -> Result<CommandOutput, CommandError> {
    // Before the session is looked up: with notifications off a hook naming a
    // session that is already gone still exits 0.
    if disabled() {
        let mut json = Sent::Suppressed("disabled").to_json();
        json["session_id"] = Value::Null;
        return Ok(CommandOutput::new(
            json,
            "Notification suppressed (disabled).",
        ));
    }
    let session = match &args.session {
        Some(reference) => Some(super::session_ref::resolve(db, reference)?.id),
        None => None,
    };
    let sent = send(
        db,
        &Message {
            title: args.title,
            body: args.body,
            session,
            dedupe: args.key.zip(args.min_interval),
        },
    );
    let mut json = sent.to_json();
    json["session_id"] = session.map_or(Value::Null, |id| Value::String(id.to_string()));
    Ok(match sent {
        Sent::Delivered => CommandOutput::new(json, "Notification delivered."),
        Sent::Suppressed(why) => {
            CommandOutput::new(json, format!("Notification suppressed ({why})."))
        }
        Sent::Failed(e) => {
            let msg = format!("Notification failed: {e}");
            CommandOutput::failed(json, msg.clone(), msg)
        }
    })
}

/// The report `notify` with no subcommand prints. Reads the process-wide
/// settings (for the configured backend) and probes the host.
fn diagnose(test: bool) -> CommandOutput {
    let s = settings::global();
    let feature_on = s.features.notifications;
    let configured = s.notifications.backend;
    let backend = notifications::detect_backend(configured);

    if test {
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
        let out = diagnose(false);
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
