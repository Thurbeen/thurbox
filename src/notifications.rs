//! OS desktop notifications for the "agent needs you" event.
//!
//! Sends a notification when a session transitions into an attention-needing
//! state (`Attention` always, plus `Waiting` when opted in). Click handling is
//! Linux-only: the dbus action callback writes a `pending_focus_session_id`
//! into the SQLite `metadata` table, which the running TUI picks up next tick
//! via its normal external-state poll. On macOS the notification is purely
//! informational — clicking it does nothing (the modern `UNUserNotificationCenter`
//! API requires a signed app bundle, which thurbox is not).
//!
//! Notify-rust on Linux blocks on dbus; dispatch therefore runs on a dedicated
//! background thread fed by an mpsc channel, so the UI tick is never stalled
//! by a slow/missing notification daemon.
//!
//! Architecture: leaf module. Depends only on `session` (the `SessionId`
//! type) and `paths` (for the DB path the click callback writes to). Never
//! reaches into `app` / `agent` / `ui`.

#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::thread;

use tracing::{debug, warn};

use crate::session::{SessionId, PENDING_FOCUS_SESSION_ID_KEY};

/// One notification to fire. Cheap to clone/move across the channel.
#[derive(Debug, Clone)]
pub struct Notification {
    /// Stable session identifier — also the value written into the
    /// `pending_focus_session_id` metadata key when the user clicks.
    pub session_id: SessionId,
    /// Title shown by the OS (e.g. `"my-feature · claude"`).
    pub title: String,
    /// Body text (e.g. the agent's last OSC notification message, or a
    /// generic "waiting for input").
    pub body: String,
    /// Play the OS default notification sound.
    pub sound: bool,
}

/// Sends notifications to the background dispatcher thread. Cloned freely;
/// dropping all senders shuts the thread down on the next recv.
#[derive(Clone)]
pub struct NotificationSender {
    tx: Sender<Notification>,
}

impl NotificationSender {
    /// Best-effort send. A full channel or a dead receiver drops the
    /// notification silently (notifications are advisory; we never block
    /// the UI tick on them).
    pub fn send(&self, n: Notification) {
        if let Err(e) = self.tx.send(n) {
            debug!("notification channel closed: {e}");
        }
    }

    /// Test helper: build a sender around a caller-owned channel so tests
    /// can construct `NotificationState` without spawning the dispatcher.
    /// Not exported outside the crate.
    #[cfg(test)]
    pub fn __test_with_sender(tx: Sender<Notification>) -> Self {
        Self { tx }
    }
}

/// Start the background dispatcher thread. Returns a sender for the UI tick
/// to push notifications into. The thread reads its DB path lazily on each
/// click (we only write a single row, no persistent connection needed) so it
/// is safe to spawn before the database file exists.
///
/// Only one dispatcher per process: re-entry returns the same sender.
pub fn start() -> NotificationSender {
    static SHARED: OnceLock<NotificationSender> = OnceLock::new();
    SHARED
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Notification>();
            thread::Builder::new()
                .name("thurbox-notifications".into())
                .spawn(move || dispatch_loop(rx))
                .expect("spawn notification dispatcher thread");
            NotificationSender { tx }
        })
        .clone()
}

fn dispatch_loop(rx: Receiver<Notification>) {
    while let Ok(n) = rx.recv() {
        if let Err(e) = dispatch_one(n) {
            warn!("notification dispatch failed: {e}");
        }
    }
    debug!("notification dispatcher exiting (all senders dropped)");
}

#[cfg(target_os = "linux")]
fn dispatch_one(n: Notification) -> Result<(), Box<dyn std::error::Error>> {
    use notify_rust::{Hint, Notification as NotifRust};

    let mut notif = NotifRust::new();
    notif
        .summary(&n.title)
        .body(&n.body)
        .appname("thurbox")
        .hint(Hint::Category("im.received".into()))
        // The "default" action fires when the user clicks the banner body
        // itself, distinct from the explicit "open" button — both route the
        // same way below.
        .action("default", "Open")
        .action("open", "Open session");
    if !n.sound {
        notif.hint(Hint::SuppressSound(true));
    }

    let handle = notif.show()?;
    let session_id = n.session_id;
    // wait_for_action blocks until the user clicks, dismisses, or the
    // notification times out — run it on its own short-lived thread so the
    // dispatcher can move on to the next queued notification.
    thread::Builder::new()
        .name("thurbox-notification-wait".into())
        .spawn(move || {
            handle.wait_for_action(|action| match action {
                "default" | "open" => {
                    if let Err(e) = write_focus_request(session_id) {
                        warn!("failed to record click-to-focus request: {e}");
                    }
                }
                _ => {}
            });
        })?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn dispatch_one(n: Notification) -> Result<(), Box<dyn std::error::Error>> {
    use notify_rust::Notification as NotifRust;

    // macOS path: no action callbacks (UNUserNotificationCenter requires a
    // signed app bundle). The notification is informational; the user
    // navigates back to thurbox manually.
    let mut notif = NotifRust::new();
    notif.summary(&n.title).body(&n.body).appname("thurbox");
    if !n.sound {
        notif.sound_name("");
    }
    notif.show()?;
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn dispatch_one(_n: Notification) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

/// SQLite `metadata` key the click handler writes to. The TUI's
/// `poll_external_changes` reads + clears it on the next tick. The constant
/// is defined in `session::` so both writer (here) and reader
/// (`storage::settings`) point at the same value without crossing module
/// boundaries.
pub const FOCUS_REQUEST_KEY: &str = PENDING_FOCUS_SESSION_ID_KEY;

/// Resolve the DB path the same way the rest of thurbox does. Returned as a
/// PathBuf so the click handler can open a short-lived connection without
/// holding any global state.
#[cfg(target_os = "linux")]
fn db_path() -> Option<PathBuf> {
    crate::paths::database_file()
}

/// Click handler: write the focus request straight to the SQLite metadata
/// table from the wait-for-action thread. We open a fresh connection per
/// click because clicks are rare and the dispatcher thread has no DB handle
/// of its own.
#[cfg(target_os = "linux")]
fn write_focus_request(session_id: SessionId) -> Result<(), Box<dyn std::error::Error>> {
    let path = db_path().ok_or("could not resolve thurbox DB path")?;
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![FOCUS_REQUEST_KEY, session_id.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_send_does_not_panic_when_receiver_dropped() {
        let (tx, rx) = mpsc::channel::<Notification>();
        drop(rx);
        let sender = NotificationSender { tx };
        // No receiver — silently dropped, no panic.
        sender.send(Notification {
            session_id: SessionId::default(),
            title: "t".into(),
            body: "b".into(),
            sound: false,
        });
    }

    #[test]
    fn focus_request_key_is_stable() {
        // Documenting the contract: the metadata key is part of the
        // cross-process protocol with the TUI poll loop. Changing it is a
        // breaking change for any in-flight clicks across an upgrade.
        assert_eq!(FOCUS_REQUEST_KEY, "pending_focus_session_id");
    }
}
