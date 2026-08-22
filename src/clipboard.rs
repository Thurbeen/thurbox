//! Clipboard writes that survive SSH.
//!
//! Two transports, tried in order (see [`ClipboardProvider`] for the config
//! knob that overrides the order):
//!
//! 1. **Native** ([`arboard`]) — talks to the local display server. Verifiable:
//!    it reports real success or a real error. Unavailable the moment thurbox
//!    runs anywhere but the machine holding the clipboard.
//! 2. **OSC 52** — an escape sequence the *terminal emulator* interprets, so it
//!    reaches the clipboard of whoever is looking at the screen no matter how
//!    many SSH hops are in between. Fire-and-forget: a terminal that doesn't
//!    implement it discards the sequence, and we can never tell.
//!
//! ## Why the order is "native, then OSC 52" and not a check for SSH
//!
//! The obvious design — sniff `$SSH_TTY` and pick a transport — is a trap, and
//! the ecosystem has already walked out of it. Neovim shipped exactly that in
//! 0.10 and **removed it as a breaking change** in 0.11 (PR #31730): the SSH
//! check is only ever a proxy for "no local clipboard here", and *trying* the
//! local clipboard answers that question directly and correctly. Helix and
//! gitui order their providers the same way; nothing modern branches on SSH for
//! a clipboard *write*.
//!
//! thurbox has an extra reason to distrust the env: the tmux server daemonizes
//! with the environment of its **first** client, so panes routinely carry stale
//! or missing `SSH_*` from whenever the server happened to start.
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
}

impl CopyRoute {
    /// Suffix for the "Copied" toast. The native path is silent because it is
    /// the unremarkable case; OSC 52 is named so that a user whose terminal
    /// drops it can tell which path ran.
    pub fn toast_suffix(self) -> &'static str {
        match self {
            CopyRoute::Native => "",
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

    // Native first when permitted: it is the only transport that can confirm it
    // worked, so preferring it keeps the common local case verifiable.
    if provider != ClipboardProvider::Osc52 {
        match native {
            Some(cb) => match cb.set_text(text) {
                Ok(()) => return Ok(CopyRoute::Native),
                Err(e) => detail = format!("native: {e}"),
            },
            None => detail = "native: unavailable".into(),
        }
        if provider == ClipboardProvider::Native {
            return Err(CopyError::NoTransport { detail });
        }
    }

    // OSC 52 carries the payload whole or not at all, so refuse oversized text
    // rather than let tmux discard it silently.
    if text.len() > OSC52_MAX_BYTES {
        return Err(CopyError::TooLarge { bytes: text.len() });
    }

    match write_osc52(text) {
        Ok(()) => Ok(CopyRoute::Osc52),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
