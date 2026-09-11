//! Clipboard writes that survive SSH.
//!
//! Two transports, and by default both are used (see [`ClipboardProvider`] for
//! the config knob that forces one):
//!
//! 1. **Native** ([`arboard`]) — talks to the local display server. Verifiable:
//!    it reports real success or a real error. Unavailable the moment thurbox
//!    runs anywhere but the machine holding the clipboard.
//! 2. **OSC 52** — an escape sequence the *terminal emulator* interprets, so it
//!    reaches the clipboard of whoever is looking at the screen no matter how
//!    many SSH hops are in between. Fire-and-forget: a terminal that doesn't
//!    implement it discards the sequence, and we can never tell.
//!
//! ## Why `auto` writes BOTH, and still does not check for SSH
//!
//! Sniffing `$SSH_TTY` to pick a transport is a trap, and the ecosystem has
//! already walked out of it. Neovim shipped exactly that in 0.10 and **removed
//! it as a breaking change** in 0.11 (PR #31730); nothing modern branches on
//! SSH for a clipboard *write*. thurbox has an extra reason to distrust the
//! env: the tmux server daemonizes with the environment of its **first**
//! client, so panes routinely carry stale or missing `SSH_*`.
//!
//! What that reasoning got wrong here was the next step: "*trying* the local
//! clipboard answers the question directly". It answers it only where a native
//! clipboard is **absent when nobody is at the machine** — X11 and Wayland,
//! where a headless SSH session has no display and `arboard` fails, so the
//! fallback to OSC 52 ran and copy worked. Windows has no such property: the
//! clipboard of a session nobody is looking at accepts writes and reports
//! success. So a copy from a Windows host over SSH landed in that host's
//! clipboard, reported success, and never reached the person who pressed the
//! key.
//!
//! Hence `auto` writes to **both**: native for the local case (verifiable, and
//! what a clipboard manager sees), OSC 52 for whoever is actually looking at
//! the screen. No SSH check, no platform branch, no way for one transport's
//! success to hide the other's necessity — and either one succeeding is a
//! successful copy. `native` and `osc52` still force a single transport for
//! anyone who wants one.
//!
//! ## Why we don't probe for OSC 52 support either
//!
//! Terminfo `Ms` and the `XTGETTCAP` query both false-negative constantly (only
//! foot/kitty/WezTerm answer; iTerm2 supports OSC 52 but reports that it does
//! not), and the query itself can corrupt output on older terminals. A false
//! negative silently disables copy — strictly worse than emitting a sequence a
//! terminal harmlessly ignores.
//!
//! Reading the clipboard over OSC 52 is not attempted at all: terminals disable
//! it by default as an exfiltration risk, and probing for it is actively
//! harmful (a read that times out stalled Neovim for >10 s on Windows
//! Terminal). Paste over SSH is the terminal's own `Ctrl+Shift+V`, which
//! arrives as an ordinary bracketed paste.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

use base64::Engine as _;

use crate::session::settings::ClipboardProvider;

/// Practical ceiling on the text of one OSC 52 sequence.
///
/// The convention is a 100,000-byte total sequence; base64 costs 4 bytes per 3,
/// and the `ESC ] 52 ; c ; … BEL` framing costs 8, leaving ~74,994 bytes of
/// payload. Oversized writes are not truncated by tmux — `input_input` sets
/// `INPUT_DISCARD` and drops the **whole** sequence — so exceeding this is
/// total silent loss, and worth an explicit error instead.
pub const OSC52_MAX_BYTES: usize = 74_994;

/// Which transport actually took the copy — surfaced in the status toast so a
/// silently-ignored OSC 52 is diagnosable rather than mysterious.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyRoute {
    /// The local display server accepted it (verified).
    Native,
    /// An OSC 52 sequence was written to the terminal (unverifiable).
    Osc52,
    /// Both: the local clipboard took it AND the sequence went out, which is
    /// what `auto` does so that neither the person at the machine nor the
    /// person at the far end of an SSH hop is the one who misses out.
    Both,
}

impl CopyRoute {
    /// Suffix for the "Copied" toast. The native path is silent because it is
    /// the unremarkable case; OSC 52 is named so that a user whose terminal
    /// drops it can tell which path ran.
    pub fn toast_suffix(self) -> &'static str {
        match self {
            // The unremarkable cases: something verifiable took it.
            CopyRoute::Native | CopyRoute::Both => "",
            // Named, so a user whose terminal drops OSC 52 can tell that this
            // was the only path that ran.
            CopyRoute::Osc52 => " (OSC 52)",
        }
    }
}

/// Why a copy failed. Carries enough detail for an actionable message.
#[derive(Debug)]
pub enum CopyError {
    /// Text exceeds what one OSC 52 sequence can carry.
    TooLarge { bytes: usize },
    /// Every permitted transport failed (or none was permitted).
    NoTransport { detail: String },
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CopyError::TooLarge { bytes } => write!(
                f,
                "Selection too large to copy ({bytes} bytes; limit {OSC52_MAX_BYTES})"
            ),
            CopyError::NoTransport { detail } => write!(f, "Clipboard write failed: {detail}"),
        }
    }
}

/// Encode `text` as a complete OSC 52 clipboard-set sequence.
///
/// Terminated with **BEL** (`\a`) rather than ST (`ESC \`): both are legal, but
/// BEL is the more widely tolerated of the two across older terminals and
/// multiplexers. (crossterm's own `CopyToClipboard` hardcodes ST, which is why
/// this is spelled out here rather than delegated to it.)
///
/// Emitted raw, *not* wrapped in tmux's DCS passthrough: the raw form is
/// handled by tmux's own OSC 52 handler, which also keeps its paste buffer in
/// sync, and needs only `set-clipboard on` (which thurbox sets on its own
/// server — see `TmuxBackend::apply_clipboard_config`). The DCS form would
/// instead require `allow-passthrough`, which is off by default.
pub fn osc52_sequence(text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

/// Write an OSC 52 sequence for `text` to the controlling terminal.
///
/// Targets `/dev/tty` rather than stdout. This is the subtle part: a
/// multiplexer *intercepts* OSC 52 arriving on a child's stdout and does not
/// necessarily forward it, so writing to the tty directly is what makes copy
/// work from inside tmux/Zellij (the same reason lazygit and Neovim do it).
/// Falls back to stdout where `/dev/tty` cannot be opened (Windows, or a
/// detached process).
fn write_osc52(text: &str) -> std::io::Result<()> {
    let seq = osc52_sequence(text);

    #[cfg(unix)]
    {
        if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            tty.write_all(seq.as_bytes())?;
            return tty.flush();
        }
    }

    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes())?;
    out.flush()
}

/// Copy `text`, returning which transport carried it.
///
/// `native` is the app-lifetime [`arboard`] handle (kept alive to dodge the
/// Linux "dropped too quickly" problem); `None` means construction failed at
/// startup, which is the normal state on a headless/SSH host.
pub fn copy(
    text: &str,
    native: Option<&mut arboard::Clipboard>,
    provider: ClipboardProvider,
) -> Result<CopyRoute, CopyError> {
    if provider == ClipboardProvider::None {
        return Err(CopyError::NoTransport {
            detail: "clipboard disabled by config ([clipboard] provider = \"none\")".into(),
        });
    }

    let mut detail = String::new();

    // Native when permitted: the only transport that can confirm it worked, and
    // the one a local clipboard manager sees. Its success is no longer the end
    // of the story under `auto` — see the module docs for why a Windows host
    // says yes to a clipboard nobody is looking at.
    let mut native_ok = false;
    if provider != ClipboardProvider::Osc52 {
        match native {
            Some(cb) => match cb.set_text(text) {
                Ok(()) => native_ok = true,
                Err(e) => detail = format!("native: {e}"),
            },
            None => detail = "native: unavailable".into(),
        }
        if provider == ClipboardProvider::Native {
            return native_ok
                .then_some(CopyRoute::Native)
                .ok_or(CopyError::NoTransport { detail });
        }
    }

    // OSC 52 carries the payload whole or not at all, so refuse oversized text
    // rather than let tmux discard it silently. Text that big is still a
    // successful copy when the local clipboard took it — only the far end
    // misses out, and saying so beats reporting a failure that did not happen.
    if text.len() > OSC52_MAX_BYTES {
        return native_ok
            .then_some(CopyRoute::Native)
            .ok_or(CopyError::TooLarge { bytes: text.len() });
    }

    match write_osc52(text) {
        Ok(()) if native_ok => Ok(CopyRoute::Both),
        Ok(()) => Ok(CopyRoute::Osc52),
        Err(e) if native_ok => {
            tracing::warn!("copied natively, but the OSC 52 write failed: {e}");
            Ok(CopyRoute::Native)
        }
        Err(e) => {
            if !detail.is_empty() {
                detail.push_str("; ");
            }
            detail.push_str(&format!("osc52: {e}"));
            Err(CopyError::NoTransport { detail })
        }
    }
}

/// Read the clipboard, if a local one is reachable.
///
/// `None` means "no local clipboard" — the SSH case — and is not an error worth
/// reporting as a failure: the caller should point the user at their terminal's
/// native paste instead. There is deliberately no OSC 52 read fallback (see the
/// module docs).
pub fn paste(native: Option<&mut arboard::Clipboard>) -> Option<String> {
    native.and_then(|cb| cb.get_text().ok())
}

/// The message shown when [`paste`] finds no local clipboard. Names the
/// terminal's own paste chord, which delivers the text as a bracketed paste the
/// loop's paste handler already routes correctly.
pub const PASTE_UNAVAILABLE_HINT: &str =
    "No local clipboard — use your terminal's paste (Ctrl+Shift+V)";

/// Whether the **Windows** clipboard holds an image, asked off the event loop.
///
/// ## Why Windows has to be asked at all
///
/// Inside WSL the X clipboard is not the clipboard the person is copying into.
/// WSLg bridges **text only**: copy a screenshot in Windows and the X side is
/// not updated at all — it still hands out whatever text was copied before
/// (measured on WSLg, Ubuntu: `Clipboard::get_text` returned an IP address
/// copied minutes earlier while Windows held a 1594x535 PNG). So `Ctrl+V` on a
/// copied image does not paste nothing, it pastes something *stale*, which is
/// worse. arboard cannot tell us either: thurbox builds it with
/// `default-features = false`, which is the build without `get_image`.
///
/// ## Why the answer is worth waiting for
///
/// thurbox cannot paste an image — but the agent in the pane can fetch one
/// itself when it sees the paste chord (Claude Code shells out to
/// `xclip`/`wl-paste`, and under WSL to this same PowerShell). So the only
/// thing the question decides is who handles the press, and getting it wrong
/// silently corrupts a prompt.
///
/// ## Why it is a worker
///
/// Spawning `powershell.exe` costs ~0.42 s (measured, three runs: 0.42/0.41/
/// 0.43). That is far too long to hold the event loop for, and it is paid on
/// *every* paste, not just image ones. So this is the seventh instance of the
/// worker pattern: ask, keep drawing, act when the answer arrives.
#[derive(Default)]
pub struct ImageProbe {
    channel: Option<(Sender<bool>, Receiver<bool>)>,
}

/// How PowerShell is found. `powershell.exe` is on `PATH` inside a distro
/// through WSL interop; the absolute path is the fallback for a `PATH` that
/// interop did not reach, and is the one Claude Code itself falls back to.
const POWERSHELL: &str = "powershell.exe";
const POWERSHELL_FALLBACK: &str =
    "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";

/// Asks only whether an image is there — deliberately not for the image. An
/// exit code is all the caller needs, and not carrying pixels over the boundary
/// keeps the call cheap.
const CONTAINS_IMAGE: &str = "Add-Type -AssemblyName System.Windows.Forms;      if (-not [System.Windows.Forms.Clipboard]::ContainsImage()) { exit 1 }";

impl ImageProbe {
    /// Whether this machine is one where the question even arises.
    ///
    /// Only inside a WSL distro: everywhere else the local clipboard *is* the
    /// one being copied into, so arboard's answer is the whole truth and no
    /// subprocess is worth spawning.
    pub fn applies() -> bool {
        cfg!(unix) && crate::session::host_def::current_wsl_distro().is_some()
    }

    /// Ask Windows, on a thread. The answer arrives at a later [`Self::poll`].
    ///
    /// Repeated presses each ask: two pastes are two pastes, and the answers
    /// come back in the order the threads finish, which for a question this
    /// small is the order they were asked.
    pub fn ask(&mut self) {
        let tx = self.channel.get_or_insert_with(channel).0.clone();
        std::thread::spawn(move || {
            let _ = tx.send(windows_clipboard_has_image());
        });
    }

    /// The answer to one earlier [`Self::ask`], if one has come back.
    pub fn poll(&mut self) -> Option<bool> {
        let (_, rx) = self.channel.as_ref()?;
        rx.try_recv().ok()
    }
}

/// One PowerShell round trip. `false` for "no image", and also for every way
/// the question could not be put: a machine that cannot answer is one whose
/// clipboard thurbox pastes as text, which is what it did before this existed.
fn windows_clipboard_has_image() -> bool {
    let ask = |exe: &str| {
        Command::new(exe)
            .args(["-NoProfile", "-NonInteractive", "-Sta", "-Command"])
            .arg(CONTAINS_IMAGE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    };
    match ask(POWERSHELL) {
        Ok(status) => status.success(),
        Err(e) => {
            tracing::debug!("{POWERSHELL} is not on PATH ({e}); trying the interop path");
            ask(POWERSHELL_FALLBACK).is_ok_and(|status| status.success())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe fires inside a WSL distro and nowhere else.
    ///
    /// The gate is the whole cost control: every paste on a machine that asks
    /// pays ~0.42 s for a `powershell.exe` that a plain Linux or macOS box has
    /// no reason to run. A gate that answered "yes" everywhere would be a
    /// third-of-a-second added to every `Ctrl+V` on every platform.
    #[test]
    fn only_a_wsl_distro_asks_windows() {
        use crate::session::host_def::with_wsl_distro;

        assert!(
            with_wsl_distro(Some("Ubuntu"), ImageProbe::applies),
            "inside a distro the X clipboard is not the one being copied into, \
             so Windows has to be asked"
        );
        assert!(
            !with_wsl_distro(None, ImageProbe::applies),
            "off WSL the local clipboard is the whole truth — asking would only \
             cost every paste a subprocess"
        );
    }

    /// The PowerShell round trip reports what PowerShell itself reports.
    ///
    /// Run against the real Windows clipboard, because what is being tested is
    /// the wiring — the argument list, `-Sta` (without it `Add-Type` throws and
    /// every answer becomes "no image"), the `PATH`/interop fallback, and
    /// reading the answer off the *exit code* rather than stdout. A helper
    /// returning `false` would pass every test that stubbed this out.
    ///
    /// Skipped where it cannot discriminate: off WSL, with no PowerShell, or
    /// with no image on the clipboard — in that last case both a working probe
    /// and a broken one say "no". Nothing here writes to the clipboard: a test
    /// suite that destroys what you copied is worse than a test that skips.
    #[test]
    fn the_windows_probe_agrees_with_powershell() {
        if !ImageProbe::applies() {
            eprintln!("skipping: not inside a WSL distro");
            return;
        }
        let oracle = Command::new(POWERSHELL)
            .args(["-NoProfile", "-NonInteractive", "-Sta", "-Command"])
            .arg("Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.Clipboard]::ContainsImage()")
            .output();
        let Ok(oracle) = oracle else {
            eprintln!("skipping: no powershell.exe on PATH");
            return;
        };
        if !String::from_utf8_lossy(&oracle.stdout).trim().eq_ignore_ascii_case("true") {
            eprintln!("skipping: no image on the Windows clipboard to recognise");
            return;
        }
        assert!(
            windows_clipboard_has_image(),
            "PowerShell says the clipboard holds an image and the probe did not"
        );
    }

    #[test]
    fn osc52_sequence_is_bel_terminated_base64() {
        // "foo" -> Zm9v; BEL terminator, clipboard ('c') selection.
        assert_eq!(osc52_sequence("foo"), "\x1b]52;c;Zm9v\x07");
    }

    #[test]
    fn osc52_sequence_encodes_multibyte_and_newlines() {
        let seq = osc52_sequence("a\né");
        assert!(seq.starts_with("\x1b]52;c;"));
        assert!(seq.ends_with('\x07'));
        let b64 = seq
            .trim_start_matches("\x1b]52;c;")
            .trim_end_matches('\x07');
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "a\né");
    }

    #[test]
    fn provider_none_refuses_every_transport() {
        let err = copy("x", None, ClipboardProvider::None).unwrap_err();
        assert!(matches!(err, CopyError::NoTransport { .. }));
    }

    #[test]
    fn provider_native_does_not_fall_through_to_osc52() {
        // No native handle available → error rather than an OSC 52 write.
        let err = copy("x", None, ClipboardProvider::Native).unwrap_err();
        match err {
            CopyError::NoTransport { detail } => assert!(detail.contains("native")),
            other => panic!("expected NoTransport, got {other:?}"),
        }
    }

    #[test]
    fn oversized_text_is_refused_before_writing() {
        let big = "a".repeat(OSC52_MAX_BYTES + 1);
        let err = copy(&big, None, ClipboardProvider::Osc52).unwrap_err();
        match err {
            CopyError::TooLarge { bytes } => assert_eq!(bytes, OSC52_MAX_BYTES + 1),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn toast_suffix_names_only_the_unverifiable_route() {
        assert_eq!(CopyRoute::Native.toast_suffix(), "");
        assert!(CopyRoute::Osc52.toast_suffix().contains("OSC 52"));
        // Both is the ordinary `auto` outcome, and something verifiable took
        // it: naming a transport there would be noise on every copy.
        assert_eq!(CopyRoute::Both.toast_suffix(), "");
    }

    /// Oversized text is only a failure when nothing else carried it. The
    /// far end misses out; the local clipboard still has it, and reporting a
    /// failure that did not happen is worse than saying so.
    #[test]
    fn oversized_text_without_a_native_clipboard_is_still_an_error() {
        let huge = "x".repeat(OSC52_MAX_BYTES + 1);
        let err = copy(&huge, None, ClipboardProvider::Auto).unwrap_err();
        assert!(matches!(err, CopyError::TooLarge { .. }));
    }
}
