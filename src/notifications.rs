//! OS desktop notifications for the "agent needs you" event.
//!
//! Sends a notification when a session transitions into an attention-needing
//! state (`Blocked` always, plus the finished `Done` edge when opted in via
//! `also_on_waiting`). Delivery picks a
//! **backend** at startup ([`detect_backend`]):
//!
//! - **dbus** (`org.freedesktop.Notifications`) on a normal Linux desktop.
//!   Click-to-focus works: the dbus action callback writes a
//!   `pending_focus_session_id` into the SQLite `metadata` table, which the
//!   running TUI picks up next tick via its normal external-state poll.
//! - **Windows toast** under WSL when no dbus notification daemon is reachable.
//!   We shell out to `powershell.exe` and raise a WinRT toast, so notifications
//!   appear in the Windows Action Center. Click-to-focus is *not* supported on
//!   this path (a Windows toast can't call back into the WSL process), so the
//!   banner is informational — the user alt-tabs back themselves.
//! - **macOS** native banner via `terminal-notifier` if installed (own
//!   bundle + icon, looks like a real app), falling back to `osascript`'s
//!   `display notification` (always available, attributed to Apple's
//!   Script Editor). Both paths are informational; the
//!   `UNUserNotificationCenter` click API needs a signed app bundle,
//!   which thurbox is not. `notify-rust`'s macOS backend
//!   (`mac-notification-sys`) is intentionally not used: it rides the
//!   deprecated `NSUserNotificationCenter` API, which silently no-ops on
//!   macOS 12+ for unbundled CLIs.
//!
//! A previous silent-failure bug motivated this: under WSL the dbus path errors
//! on connect, but the only signal was a `warn!` to the logfile — the user saw
//! nothing. We now (a) auto-detect the working backend, (b) record the last
//! delivery error so `thurbox-cli notify` can surface it, and (c) treat "no
//! backend at all" as a reported condition rather than a silent drop.
//!
//! Notify-rust on Linux blocks on dbus, and `powershell.exe` is slow to spawn;
//! dispatch therefore runs on a dedicated background thread fed by an mpsc
//! channel, so the UI tick is never stalled by a slow/missing notification
//! daemon.
//!
//! Architecture: leaf module. Depends only on `session` (the `SessionId` /
//! `NotificationBackend` types) and `paths` (for the DB path the click callback
//! writes to). Never reaches into `app` / `agent` / `ui`.

#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;

use tracing::{debug, warn};

use crate::session::settings::NotificationBackend;
use crate::session::{SessionId, PENDING_FOCUS_SESSION_ID_KEY};

/// The resolved delivery mechanism, decided once at startup from the
/// configured [`NotificationBackend`] plus host probing. This is the concrete
/// transport, with no `Auto` ambiguity left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryBackend {
    /// freedesktop dbus (`notify-rust` → `org.freedesktop.Notifications`).
    Dbus,
    /// WSL → Windows toast via `powershell.exe`.
    WindowsToast,
    /// macOS native banner.
    Macos,
    /// No working delivery path (e.g. `backend = "off"`, or WSL without
    /// `powershell.exe`, or an unsupported OS). Notifications are dropped, but
    /// the reason is reported via [`last_error`].
    None,
}

impl DeliveryBackend {
    /// Short, user-facing label for the diagnostic output.
    pub fn label(self) -> &'static str {
        match self {
            DeliveryBackend::Dbus => "dbus (org.freedesktop.Notifications)",
            DeliveryBackend::WindowsToast => "windows-toast (powershell.exe)",
            DeliveryBackend::Macos => "macos (native banner)",
            DeliveryBackend::None => "none (delivery disabled)",
        }
    }

    /// Whether the backend can actually deliver a notification.
    pub fn is_deliverable(self) -> bool {
        !matches!(self, DeliveryBackend::None)
    }

    /// Whether clicking the notification focuses the session in the TUI.
    /// Linux dbus wires this up natively (action callback); macOS does too
    /// when `terminal-notifier` is in PATH (its `-execute` flag shells back
    /// into `thurbox-cli session focus <id>`). The `osascript` fallback
    /// can't, so on macOS this flips to false when only the fallback path
    /// is available.
    pub fn supports_click_to_focus(self) -> bool {
        match self {
            DeliveryBackend::Dbus => true,
            #[cfg(target_os = "macos")]
            DeliveryBackend::Macos => terminal_notifier_available(),
            _ => false,
        }
    }
}

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

/// Process-wide record of the last delivery failure, so the diagnostic
/// (`thurbox-cli notify`) and a future TUI warning can surface what would
/// otherwise be a silent drop. `None` = no failure recorded yet.
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn record_error(msg: impl Into<String>) {
    let msg = msg.into();
    warn!("notification dispatch failed: {msg}");
    if let Ok(mut slot) = LAST_ERROR.lock() {
        *slot = Some(msg);
    }
}

/// The most recent delivery error, if any. Used by the CLI diagnostic.
pub fn last_error() -> Option<String> {
    LAST_ERROR.lock().ok().and_then(|s| s.clone())
}

/// Start the background dispatcher thread for the given backend. Returns a
/// sender for the UI tick to push notifications into. The thread reads its DB
/// path lazily on each click (we only write a single row, no persistent
/// connection needed) so it is safe to spawn before the database file exists.
///
/// Only one dispatcher per process: re-entry returns the same sender and the
/// backend chosen on the first call wins.
pub fn start(backend: DeliveryBackend) -> NotificationSender {
    static SHARED: OnceLock<NotificationSender> = OnceLock::new();
    SHARED
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Notification>();
            thread::Builder::new()
                .name("thurbox-notifications".into())
                .spawn(move || dispatch_loop(rx, backend))
                .expect("spawn notification dispatcher thread");
            NotificationSender { tx }
        })
        .clone()
}

fn dispatch_loop(rx: Receiver<Notification>, backend: DeliveryBackend) {
    while let Ok(n) = rx.recv() {
        if let Err(e) = dispatch_one(&n, backend) {
            record_error(e.to_string());
        }
    }
    debug!("notification dispatcher exiting (all senders dropped)");
}

/// Synchronously deliver a single notification over `backend`, blocking until
/// the OS call returns. Used by the `thurbox-cli notify --test` diagnostic,
/// which is a short-lived process with no dispatcher thread — it must complete
/// the delivery before exiting. Returns an error string on failure (the dbus
/// path's click-wait thread is still spawned but the process may exit before a
/// click; that's fine for a one-shot test).
pub fn send_blocking(n: &Notification, backend: DeliveryBackend) -> Result<(), String> {
    if !backend.is_deliverable() {
        return Err(format!("no deliverable backend ({})", backend.label()));
    }
    dispatch_one(n, backend).map_err(|e| e.to_string())
}

/// Dispatch one notification over the resolved backend.
fn dispatch_one(
    n: &Notification,
    backend: DeliveryBackend,
) -> Result<(), Box<dyn std::error::Error>> {
    match backend {
        #[cfg(target_os = "linux")]
        DeliveryBackend::Dbus => dispatch_dbus(n),
        DeliveryBackend::WindowsToast => dispatch_windows_toast(n),
        #[cfg(target_os = "macos")]
        DeliveryBackend::Macos => dispatch_macos(n),
        DeliveryBackend::None => Ok(()),
        // A backend that isn't compiled for this target (e.g. Dbus on macOS):
        // nothing to do. detect_backend never returns these combinations.
        #[allow(unreachable_patterns)]
        _ => Ok(()),
    }
}

#[cfg(target_os = "linux")]
fn dispatch_dbus(n: &Notification) -> Result<(), Box<dyn std::error::Error>> {
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
fn dispatch_macos(n: &Notification) -> Result<(), Box<dyn std::error::Error>> {
    // notify-rust's macOS backend (`mac-notification-sys`) uses the
    // deprecated `NSUserNotificationCenter` API, which silently no-ops on
    // macOS 12+ for unbundled CLIs — the cause of "thurbox notifications
    // don't fire on Mac". Two viable replacements that need no `.app`
    // bundle / no code signing:
    //
    //   1. `terminal-notifier` — a small signed helper distributed via
    //      Homebrew. Banner attribution + icon belong to the helper (a
    //      bell-on-terminal mark), so it looks like a normal app
    //      notification. Opt-in: the user must `brew install terminal-notifier`.
    //   2. `osascript` (`display notification`) — built into every macOS
    //      release, so always available. Banner attribution is "Script
    //      Editor" (Apple's Script Editor agent owns the notification
    //      permission `display notification` rides on).
    //
    // Try (1) and fall back to (2). The first attempt also acts as a
    // failure escape hatch: if `terminal-notifier` is in PATH but broken
    // we still land on the `osascript` path.
    //
    // No action callbacks on either path: those need `UNUserNotificationCenter`
    // (a signed `.app`), which thurbox is not.
    if terminal_notifier_available() {
        match dispatch_macos_terminal_notifier(n) {
            Ok(()) => return Ok(()),
            Err(e) => debug!("terminal-notifier failed, falling back to osascript: {e}"),
        }
    }
    dispatch_macos_osascript(n)
}

/// Cached check for `terminal-notifier` in `PATH`. We probe once per
/// process (it won't appear mid-session) and reuse the answer so every
/// notification doesn't pay a spawn.
#[cfg(target_os = "macos")]
fn terminal_notifier_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| which_on_path("terminal-notifier"))
}

#[cfg(target_os = "macos")]
fn dispatch_macos_terminal_notifier(n: &Notification) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::{Command, Stdio};

    // `-group <id>` collapses repeat notifications for the same session
    // into a single banner instead of stacking — the per-session dedup
    // floor already bounds frequency, but if a long-running session fires
    // across the floor we still want the latest one to replace the prior.
    let mut cmd = Command::new("terminal-notifier");
    cmd.arg("-title")
        .arg(&n.title)
        .arg("-message")
        .arg(&n.body)
        .arg("-group")
        .arg(format!("thurbox-{}", n.session_id));
    if n.sound {
        cmd.arg("-sound").arg("default");
    }
    // Click-to-focus: `terminal-notifier -execute "<cmd>"` runs the command
    // via `/bin/sh -c` when the user clicks the banner. We point it at
    // `thurbox-cli session focus <id>`, which writes the same
    // `pending_focus_session_id` metadata row the Linux dbus path writes;
    // the TUI's external-state poll then switches active_index. We resolve
    // an *absolute* `thurbox-cli` path from the running binary's directory
    // — the click handler runs under launchd's environment, which has a
    // very sparse PATH, so a bare `thurbox-cli` would often not resolve.
    // If we can't locate the CLI binary the click is simply non-interactive
    // (the banner still shows).
    if let Some(focus_cmd) = thurbox_cli_focus_command(n.session_id) {
        cmd.arg("-execute").arg(focus_cmd);
    }
    let status = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status()?;
    if !status.success() {
        return Err(format!("terminal-notifier exited with {status}").into());
    }
    Ok(())
}

/// Build the shell command `terminal-notifier -execute` should run on click:
/// `<thurbox-cli> session focus <session-id>`, with the thurbox-cli path
/// quoted for `/bin/sh`. Returns `None` if we can't find a sibling
/// `thurbox-cli` binary next to the running executable — the path probe
/// is cached, the resolution is not, because `current_exe()` is cheap.
#[cfg(target_os = "macos")]
fn thurbox_cli_focus_command(session_id: SessionId) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let parent = exe.parent()?;
    let cli = parent.join("thurbox-cli");
    if !cli.exists() {
        return None;
    }
    // SessionId is a UUID (hex + dashes), shell-safe without quoting.
    Some(format!(
        "{} session focus {}",
        sh_single_quote(&cli.to_string_lossy()),
        session_id
    ))
}

/// POSIX single-quote a string so it survives `/bin/sh -c "<cmd>"`. The
/// terminal-notifier `-execute` argument is interpreted by `sh`, and a user
/// `HOME` containing a space or apostrophe would otherwise break the
/// command. The trick: close the quote, emit an escaped quote, re-open
/// (`'\''`).
#[cfg(target_os = "macos")]
fn sh_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(target_os = "macos")]
fn dispatch_macos_osascript(n: &Notification) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Command;

    // Arguments are passed via AppleScript's `argv` (rather than spliced
    // into the script text) so the body/title can contain any characters —
    // quotes, backslashes, newlines — with zero escaping.
    let script = if n.sound {
        "on run argv\n\
         display notification (item 1 of argv) with title (item 2 of argv) \
         sound name \"default\"\n\
         end run"
    } else {
        "on run argv\n\
         display notification (item 1 of argv) with title (item 2 of argv)\n\
         end run"
    };
    let status = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .arg("--")
        .arg(&n.body)
        .arg(&n.title)
        .status()?;
    if !status.success() {
        return Err(format!("osascript exited with {status}").into());
    }
    Ok(())
}

/// Deliver a Windows toast from WSL by invoking `powershell.exe` with a small
/// WinRT script. This is the only path that crosses the WSL → Windows
/// boundary; it needs WSL interop enabled (the default) and `powershell.exe`
/// on `PATH` (also the default). Click-to-focus is not wired up here.
fn dispatch_windows_toast(n: &Notification) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Command;

    let script = build_powershell_toast_script(&n.title, &n.body, n.sound);
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|e| format!("could not run powershell.exe (WSL interop?): {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "powershell.exe toast failed ({}): {}",
            output.status,
            stderr.trim()
        )
        .into());
    }
    Ok(())
}

/// Single-quote a string for safe interpolation into a PowerShell literal.
/// PowerShell escapes a literal single quote by doubling it (`''`). We also
/// strip control characters that could break the one-liner; the body has
/// already been length-bounded upstream.
pub(crate) fn powershell_escape(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect::<String>()
        .replace('\'', "''")
}

/// Build the PowerShell script that raises a WinRT toast. Title/body are
/// single-quote-escaped and embedded as XML text nodes (the WinRT API takes
/// raw text, so XML-special characters are passed as-is by `CreateTextNode`).
pub(crate) fn build_powershell_toast_script(title: &str, body: &str, sound: bool) -> String {
    let title = powershell_escape(title);
    let body = powershell_escape(body);
    // ToastText02 = one bold heading line + one wrapped body line.
    // A generic AppUserModelID ("Windows Terminal") lets the toast show
    // without registering a Start-menu shortcut.
    let audio = if sound {
        String::new()
    } else {
        // Suppress the default toast sound by appending a silent <audio> node.
        "$audio = $template.CreateElement('audio'); \
         $audio.SetAttribute('silent','true'); \
         $template.DocumentElement.AppendChild($audio) | Out-Null; "
            .to_string()
    };
    format!(
        "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; \
         $template = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
         $texts = $template.GetElementsByTagName('text'); \
         $texts.Item(0).AppendChild($template.CreateTextNode('{title}')) | Out-Null; \
         $texts.Item(1).AppendChild($template.CreateTextNode('{body}')) | Out-Null; \
         {audio}\
         $toast = [Windows.UI.Notifications.ToastNotification]::new($template); \
         $notifier = [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Windows Terminal'); \
         $notifier.Show($toast)"
    )
}

/// Decide the concrete delivery backend for this host from the configured
/// preference. `Auto` probes the environment; the explicit variants force a
/// path (or disable). Probing is delegated to [`probe_host`] so the decision
/// table is pure and unit-testable.
pub fn detect_backend(configured: NotificationBackend) -> DeliveryBackend {
    resolve_backend(configured, probe_host())
}

/// Observable facts about the host that drive backend selection. Kept as a
/// plain struct so `resolve_backend` is a pure function of (config, facts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostProbe {
    /// Compiled for `target_os = "linux"`.
    pub is_linux: bool,
    /// Compiled for `target_os = "macos"`.
    pub is_macos: bool,
    /// Compiled for `target_os = "windows"` (native Windows, not WSL).
    pub is_windows: bool,
    /// Running under WSL (Microsoft kernel marker in `/proc/version`).
    pub is_wsl: bool,
    /// A reachable freedesktop notification service on the session bus.
    pub has_dbus_service: bool,
    /// `powershell.exe` resolves on `PATH` (WSL interop available).
    pub has_powershell: bool,
}

/// Pure backend-selection table. Separated from probing so every branch is
/// testable without a real host.
pub fn resolve_backend(configured: NotificationBackend, probe: HostProbe) -> DeliveryBackend {
    match configured {
        NotificationBackend::Off => DeliveryBackend::None,
        NotificationBackend::Dbus => {
            if probe.is_linux {
                DeliveryBackend::Dbus
            } else {
                DeliveryBackend::None
            }
        }
        NotificationBackend::Windows => {
            if probe.has_powershell {
                DeliveryBackend::WindowsToast
            } else {
                DeliveryBackend::None
            }
        }
        NotificationBackend::Auto => {
            if probe.is_macos {
                DeliveryBackend::Macos
            } else if probe.is_windows {
                // Native Windows: `powershell.exe` ships with the OS, so the
                // WinRT toast path is always available. (Click-to-focus is not
                // wired on Windows — the banner shows but is non-interactive.)
                if probe.has_powershell {
                    DeliveryBackend::WindowsToast
                } else {
                    DeliveryBackend::None
                }
            } else if probe.is_linux {
                // Prefer dbus when a real notification daemon answers. The
                // common WSL case has no daemon, so fall back to a Windows
                // toast whenever interop (`powershell.exe`) is available —
                // this also covers the unusual non-WSL Linux host with
                // powershell on PATH, still better than dropping silently.
                // Without either we have nothing (reported via `last_error`).
                if probe.has_dbus_service {
                    DeliveryBackend::Dbus
                } else if probe.has_powershell {
                    DeliveryBackend::WindowsToast
                } else {
                    DeliveryBackend::None
                }
            } else {
                DeliveryBackend::None
            }
        }
    }
}

/// Gather host facts for [`detect_backend`]. The `cfg!` flags are
/// compile-time; the dynamic probes (WSL marker, dbus service, powershell)
/// are best-effort and never panic.
pub fn probe_host() -> HostProbe {
    HostProbe {
        is_linux: cfg!(target_os = "linux"),
        is_macos: cfg!(target_os = "macos"),
        is_windows: cfg!(target_os = "windows"),
        is_wsl: detect_wsl(),
        has_dbus_service: detect_dbus_service(),
        has_powershell: detect_powershell(),
    }
}

/// WSL is detected by the Microsoft marker the WSL kernel writes into
/// `/proc/version` (e.g. "...microsoft-standard-WSL2..."). Pure check exposed
/// for tests via [`proc_version_is_wsl`].
fn detect_wsl() -> bool {
    std::fs::read_to_string("/proc/version")
        .map(|v| proc_version_is_wsl(&v))
        .unwrap_or(false)
}

/// Pure WSL test over a `/proc/version` string.
pub(crate) fn proc_version_is_wsl(proc_version: &str) -> bool {
    let lower = proc_version.to_ascii_lowercase();
    lower.contains("microsoft") || lower.contains("wsl")
}

/// Probe for a working freedesktop notification service. We deliberately do a
/// cheap socket-existence check rather than a full dbus round-trip: the common
/// failure (WSL) is that `DBUS_SESSION_BUS_ADDRESS` points at a socket path
/// that does not exist, so even opening the bus fails. A present socket is a
/// good-enough signal to attempt the dbus path (a misbehaving daemon then
/// surfaces via `last_error`, no longer silent).
#[cfg(target_os = "linux")]
fn detect_dbus_service() -> bool {
    // Honour an explicit address first.
    if let Ok(addr) = std::env::var("DBUS_SESSION_BUS_ADDRESS") {
        if let Some(path) = addr
            .split(',')
            .find_map(|kv| kv.strip_prefix("unix:path="))
            .or_else(|| addr.strip_prefix("unix:path="))
        {
            return std::path::Path::new(path).exists();
        }
        // Non-unix transport (tcp, etc.) — assume reachable; let dbus try.
        if !addr.is_empty() {
            return true;
        }
    }
    // Fall back to the conventional per-user bus socket location.
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        return std::path::Path::new(&runtime).join("bus").exists();
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn detect_dbus_service() -> bool {
    false
}

/// `powershell.exe` on `PATH` (WSL interop). Cheap PATH scan; no process spawn.
fn detect_powershell() -> bool {
    which_on_path("powershell.exe")
}

/// Minimal `PATH` lookup (avoids pulling in a `which` crate for one probe).
fn which_on_path(exe: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(exe).exists())
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

    fn probe(
        is_linux: bool,
        is_macos: bool,
        is_wsl: bool,
        has_dbus: bool,
        has_ps: bool,
    ) -> HostProbe {
        HostProbe {
            is_linux,
            is_macos,
            is_windows: false,
            is_wsl,
            has_dbus_service: has_dbus,
            has_powershell: has_ps,
        }
    }

    /// A native-Windows host facts struct (no WSL, no dbus, powershell present).
    fn probe_windows() -> HostProbe {
        HostProbe {
            is_linux: false,
            is_macos: false,
            is_windows: true,
            is_wsl: false,
            has_dbus_service: false,
            has_powershell: true,
        }
    }

    #[test]
    fn auto_on_native_windows_uses_toast() {
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, probe_windows()),
            DeliveryBackend::WindowsToast
        );
    }

    #[test]
    fn auto_on_windows_without_powershell_is_none() {
        let mut p = probe_windows();
        p.has_powershell = false;
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, p),
            DeliveryBackend::None
        );
    }

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

    #[test]
    fn proc_version_wsl_marker() {
        assert!(proc_version_is_wsl(
            "Linux version 6.6.0-microsoft-standard-WSL2 (gcc ...)"
        ));
        assert!(proc_version_is_wsl("... WSL ..."));
        assert!(!proc_version_is_wsl(
            "Linux version 6.6.0-arch1-1 (linux@archlinux) ..."
        ));
    }

    #[test]
    fn auto_picks_dbus_on_normal_linux_desktop() {
        let p = probe(true, false, false, true, false);
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, p),
            DeliveryBackend::Dbus
        );
    }

    #[test]
    fn auto_falls_back_to_windows_toast_under_wsl_without_dbus() {
        // The exact WSL failure case: linux + WSL marker + no dbus service,
        // but powershell.exe is reachable.
        let p = probe(true, false, true, false, true);
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, p),
            DeliveryBackend::WindowsToast
        );
    }

    #[test]
    fn auto_under_wsl_without_powershell_reports_none() {
        let p = probe(true, false, true, false, false);
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, p),
            DeliveryBackend::None
        );
    }

    #[test]
    fn auto_prefers_dbus_even_under_wsl_if_a_daemon_answers() {
        // A WSL setup that *does* run a notification daemon should still use it.
        let p = probe(true, false, true, true, true);
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, p),
            DeliveryBackend::Dbus
        );
    }

    #[test]
    fn auto_picks_macos_banner() {
        let p = probe(false, true, false, false, false);
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, p),
            DeliveryBackend::Macos
        );
    }

    #[test]
    fn off_always_disables() {
        let p = probe(true, false, false, true, true);
        assert_eq!(
            resolve_backend(NotificationBackend::Off, p),
            DeliveryBackend::None
        );
    }

    #[test]
    fn forced_dbus_requires_linux() {
        assert_eq!(
            resolve_backend(
                NotificationBackend::Dbus,
                probe(true, false, false, false, false)
            ),
            DeliveryBackend::Dbus
        );
        assert_eq!(
            resolve_backend(
                NotificationBackend::Dbus,
                probe(false, true, false, false, false)
            ),
            DeliveryBackend::None
        );
    }

    #[test]
    fn forced_windows_requires_powershell() {
        assert_eq!(
            resolve_backend(
                NotificationBackend::Windows,
                probe(true, false, true, false, true)
            ),
            DeliveryBackend::WindowsToast
        );
        assert_eq!(
            resolve_backend(
                NotificationBackend::Windows,
                probe(true, false, true, false, false)
            ),
            DeliveryBackend::None
        );
    }

    #[test]
    fn backend_capability_helpers() {
        assert!(DeliveryBackend::Dbus.supports_click_to_focus());
        assert!(!DeliveryBackend::WindowsToast.supports_click_to_focus());
        assert!(DeliveryBackend::WindowsToast.is_deliverable());
        assert!(!DeliveryBackend::None.is_deliverable());
    }

    #[test]
    fn powershell_escape_doubles_single_quotes_and_strips_controls() {
        assert_eq!(powershell_escape("it's fine"), "it''s fine");
        // Newlines collapse to spaces; other control chars are dropped.
        assert_eq!(powershell_escape("a\nb\tc"), "a bc");
        assert_eq!(powershell_escape("plain"), "plain");
    }

    #[test]
    fn toast_script_embeds_escaped_title_and_body() {
        let script = build_powershell_toast_script("my-feat · claude", "can't proceed", true);
        assert!(script.contains("my-feat · claude"));
        // The single quote in the body is doubled for PowerShell.
        assert!(script.contains("can''t proceed"));
        assert!(script.contains("ToastNotificationManager"));
        // Sound on → no silent audio node.
        assert!(!script.contains("silent"));
    }

    #[test]
    fn toast_script_suppresses_sound_when_off() {
        let script = build_powershell_toast_script("t", "b", false);
        assert!(script.contains("silent"));
    }

    #[test]
    fn last_error_round_trips() {
        record_error("boom");
        assert_eq!(last_error().as_deref(), Some("boom"));
    }
}
