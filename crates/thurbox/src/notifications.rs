use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::session::settings::NotificationBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryBackend {
    Dbus,
    WindowsToast,
    Macos,
    None,
}

impl DeliveryBackend {
    pub fn label(self) -> &'static str {
        match self {
            DeliveryBackend::Dbus => "dbus (org.freedesktop.Notifications)",
            DeliveryBackend::WindowsToast => "windows-toast (powershell.exe)",
            DeliveryBackend::Macos => "macos (native banner)",
            DeliveryBackend::None => "none (delivery disabled)",
        }
    }

    pub fn is_deliverable(self) -> bool {
        !matches!(self, DeliveryBackend::None)
    }

    pub fn supports_click_to_focus(self) -> bool {
        match self {
            DeliveryBackend::Dbus => true,
            #[cfg(target_os = "macos")]
            DeliveryBackend::Macos => terminal_notifier_available(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Notification {
    pub session_id: String,
    pub title: String,
    pub body: String,
    pub sound: bool,
}

#[derive(Clone)]
pub struct NotificationSender {
    tx: Sender<Notification>,
}

impl NotificationSender {
    #[cfg(test)]
    pub(crate) fn to(tx: Sender<Notification>) -> Self {
        Self { tx }
    }

    pub fn send(&self, n: Notification) {
        if let Err(e) = self.tx.send(n) {
            debug!("notification channel closed: {e}");
        }
    }

    #[cfg(test)]
    pub fn __test_with_sender(tx: Sender<Notification>) -> Self {
        Self { tx }
    }
}

static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn record_error(msg: impl Into<String>) {
    let msg = msg.into();
    warn!("notification dispatch failed: {msg}");
    if let Ok(mut slot) = LAST_ERROR.lock() {
        *slot = Some(msg);
    }
}

pub fn last_error() -> Option<String> {
    LAST_ERROR.lock().ok().and_then(|s| s.clone())
}

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

pub fn send_blocking(n: &Notification, backend: DeliveryBackend) -> Result<(), String> {
    if !backend.is_deliverable() {
        return Err(format!("no deliverable backend ({})", backend.label()));
    }
    dispatch_one(n, backend).map_err(|e| e.to_string())
}

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
    let session_id = n.session_id.clone();
    // wait_for_action blocks until the user clicks, dismisses, or the
    // notification times out — run it on its own short-lived thread so the
    // dispatcher can move on to the next queued notification.
    thread::Builder::new()
        .name("thurbox-notification-wait".into())
        .spawn(move || {
            handle.wait_for_action(|action| match action {
                "default" | "open" => {
                    write_focus_request(session_id);
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

#[cfg(target_os = "macos")]
fn terminal_notifier_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| crate::paths::which_on_path("terminal-notifier"))
}

#[cfg(target_os = "macos")]
fn dispatch_macos_terminal_notifier(n: &Notification) -> Result<(), Box<dyn std::error::Error>> {
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
    // `thurbox-cli ui action session.focus --session <id>`, which asks the
    // running interface to focus it. We resolve
    // an *absolute* `thurbox-cli` path from the running binary's directory
    // — the click handler runs under launchd's environment, which has a
    // very sparse PATH, so a bare `thurbox-cli` would often not resolve.
    // If we can't locate the CLI binary the click is simply non-interactive
    // (the banner still shows).
    if let Some(focus_cmd) = thurbox_cli_focus_command(&n.session_id) {
        cmd.arg("-execute").arg(focus_cmd);
    }
    let status = run_notifier(&mut cmd, NOTIFIER_TIMEOUT)?.status;
    if !status.success() {
        return Err(format!("terminal-notifier exited with {status}").into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn thurbox_cli_focus_command(session_id: &str) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let parent = exe.parent()?;
    let cli = parent.join("thurbox-cli");
    if !cli.exists() {
        return None;
    }
    Some(format!(
        "{} ui action session.focus --session {}",
        sh_single_quote(&cli.to_string_lossy()),
        sh_single_quote(session_id)
    ))
}

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
    let status = run_notifier(
        Command::new("osascript")
            .arg("-e")
            .arg(script)
            .arg("--")
            .arg(&n.body)
            .arg(&n.title),
        NOTIFIER_TIMEOUT,
    )?
    .status;
    if !status.success() {
        return Err(format!("osascript exited with {status}").into());
    }
    Ok(())
}

const NOTIFIER_TIMEOUT: Duration = Duration::from_secs(8);

fn run_notifier(cmd: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    use std::io::Read;

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("timed out after {timeout:?}"),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    };
    let stderr = stderr
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    Ok(Output {
        status,
        stdout: Vec::new(),
        stderr,
    })
}

fn dispatch_windows_toast(n: &Notification) -> Result<(), Box<dyn std::error::Error>> {
    let script = build_powershell_toast_script(&n.title, &n.body, n.sound);
    let output = run_notifier(
        Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]),
        NOTIFIER_TIMEOUT,
    )
    .map_err(|e| match e.kind() {
        std::io::ErrorKind::TimedOut => format!("powershell.exe toast {e}"),
        _ => format!("could not run powershell.exe (WSL interop?): {e}"),
    })?;

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

pub(crate) fn powershell_escape(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    let quoted = crate::shell::powershell_quote(&cleaned);
    quoted[1..quoted.len() - 1].to_string()
}

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

pub fn detect_backend(configured: NotificationBackend) -> DeliveryBackend {
    resolve_backend(configured, probe_host())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostProbe {
    pub is_linux: bool,
    pub is_macos: bool,
    pub is_windows: bool,
    pub has_dbus_service: bool,
    pub has_powershell: bool,
}

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
        NotificationBackend::Auto => resolve_auto(probe),
    }
}

fn resolve_auto(probe: HostProbe) -> DeliveryBackend {
    let toast_or_none = |available: bool| match available {
        true => DeliveryBackend::WindowsToast,
        false => DeliveryBackend::None,
    };
    if probe.is_macos {
        return DeliveryBackend::Macos;
    }
    if probe.is_windows {
        // Native Windows: `powershell.exe` ships with the OS, so the WinRT
        // toast path is always available. (Click-to-focus is not wired on
        // Windows — the banner shows but is non-interactive.)
        return toast_or_none(probe.has_powershell);
    }
    if !probe.is_linux {
        return DeliveryBackend::None;
    }
    // Prefer dbus when a real notification daemon answers. The common WSL case
    // has no daemon, so fall back to a Windows toast whenever interop
    // (`powershell.exe`) is available — this also covers the unusual non-WSL
    // Linux host with powershell on PATH, still better than dropping silently.
    // Without either we have nothing (reported via `last_error`).
    match probe.has_dbus_service {
        true => DeliveryBackend::Dbus,
        false => toast_or_none(probe.has_powershell),
    }
}

pub fn probe_host() -> HostProbe {
    HostProbe {
        is_linux: cfg!(target_os = "linux"),
        is_macos: cfg!(target_os = "macos"),
        is_windows: cfg!(target_os = "windows"),
        has_dbus_service: detect_dbus_service(),
        has_powershell: detect_powershell(),
    }
}

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

fn detect_powershell() -> bool {
    crate::paths::which_on_path("powershell.exe")
}

static FOCUS_REQUEST: Mutex<Option<String>> = Mutex::new(None);

#[cfg(target_os = "linux")]
fn write_focus_request(session_id: String) {
    if session_id.is_empty() {
        return;
    }
    if let Ok(mut slot) = FOCUS_REQUEST.lock() {
        *slot = Some(session_id);
    }
}

pub fn take_focus_request() -> Option<String> {
    FOCUS_REQUEST.lock().ok()?.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_notifier_that_hangs_is_given_up_on_and_stopped() {
        let dir = tempfile::tempdir().expect("dir");
        let pid = dir.path().join("pid");
        let started = Instant::now();
        let error = run_notifier(
            Command::new("sh")
                .arg("-c")
                .arg(format!("echo $$ > '{}'; exec sleep 120", pid.display())),
            Duration::from_millis(300),
        )
        .expect_err("it never exits");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let left_running = std::fs::read_to_string(&pid).is_ok_and(|pid| {
            Command::new("kill")
                .arg(pid.trim())
                .status()
                .is_ok_and(|status| status.success())
        });
        assert!(!left_running, "the hung notifier outlived the send");
    }

    fn probe(is_linux: bool, is_macos: bool, has_dbus: bool, has_ps: bool) -> HostProbe {
        HostProbe {
            is_linux,
            is_macos,
            is_windows: false,
            has_dbus_service: has_dbus,
            has_powershell: has_ps,
        }
    }

    fn probe_windows() -> HostProbe {
        HostProbe {
            is_linux: false,
            is_macos: false,
            is_windows: true,
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
        sender.send(Notification {
            session_id: String::new(),
            title: "t".into(),
            body: "b".into(),
            sound: false,
        });
    }

    #[test]
    fn auto_picks_dbus_on_normal_linux_desktop() {
        let p = probe(true, false, true, false);
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, p),
            DeliveryBackend::Dbus
        );
    }

    #[test]
    fn auto_falls_back_to_windows_toast_under_wsl_without_dbus() {
        // The exact WSL failure case: linux + WSL marker + no dbus service,
        // but powershell.exe is reachable.
        let p = probe(true, false, false, true);
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, p),
            DeliveryBackend::WindowsToast
        );
    }

    #[test]
    fn auto_under_wsl_without_powershell_reports_none() {
        let p = probe(true, false, false, false);
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, p),
            DeliveryBackend::None
        );
    }

    #[test]
    fn auto_prefers_dbus_even_under_wsl_if_a_daemon_answers() {
        // A WSL setup that *does* run a notification daemon should still use it.
        let p = probe(true, false, true, true);
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, p),
            DeliveryBackend::Dbus
        );
    }

    #[test]
    fn auto_picks_macos_banner() {
        let p = probe(false, true, false, false);
        assert_eq!(
            resolve_backend(NotificationBackend::Auto, p),
            DeliveryBackend::Macos
        );
    }

    #[test]
    fn off_always_disables() {
        let p = probe(true, false, true, true);
        assert_eq!(
            resolve_backend(NotificationBackend::Off, p),
            DeliveryBackend::None
        );
    }

    #[test]
    fn forced_dbus_requires_linux() {
        assert_eq!(
            resolve_backend(NotificationBackend::Dbus, probe(true, false, false, false)),
            DeliveryBackend::Dbus
        );
        assert_eq!(
            resolve_backend(NotificationBackend::Dbus, probe(false, true, false, false)),
            DeliveryBackend::None
        );
    }

    #[test]
    fn forced_windows_requires_powershell() {
        assert_eq!(
            resolve_backend(
                NotificationBackend::Windows,
                probe(true, false, false, true)
            ),
            DeliveryBackend::WindowsToast
        );
        assert_eq!(
            resolve_backend(
                NotificationBackend::Windows,
                probe(true, false, false, false)
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
