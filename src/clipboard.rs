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

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

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
    channel: Option<(Sender<Verdict>, Receiver<Verdict>)>,
    /// Whether a question is out. At most one at a time, because key
    /// auto-repeat holds `Ctrl+V` down far faster than the ~0.42 s answer and
    /// a process per repeat is a machine brought to its knees by a held key.
    in_flight: bool,
}

/// How PowerShell is found. `powershell.exe` is on `PATH` inside a distro
/// through WSL interop; the absolute path is the fallback for a `PATH` that
/// interop did not reach, and is the one Claude Code itself falls back to.
const POWERSHELL: &str = "powershell.exe";
const POWERSHELL_FALLBACK: &str = "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";

/// Where PowerShell is looked for, in the order it is tried.
///
/// The `PATH` search goes through [`crate::paths::resolve_on_path`] rather than
/// through `Command::new`'s own: the OS search treats an **empty** entry in
/// `PATH` (a stray leading, trailing or doubled `:`) as the current directory,
/// so a file named `powershell.exe` sitting in whatever repository thurbox was
/// launched from would answer this question on every `Ctrl+V`. `resolve_on_path`
/// keeps absolute entries only, which is the rule #1100 landed for agent
/// spawns; there is no reason for this path to hold a weaker one.
fn powershell_candidates() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = crate::paths::resolve_on_path(POWERSHELL)
        .into_iter()
        .collect();
    found.push(PathBuf::from(POWERSHELL_FALLBACK));
    found
}

/// What Windows said about its clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A picture and nothing else — the press belongs to the agent, which can
    /// fetch it.
    Image,
    /// Something thurbox can paste itself, or nothing at all.
    NotImage,
    /// Windows could not be asked. Deliberately **not** folded into
    /// [`Verdict::NotImage`]: WSL interop wedges intermittently (this machine
    /// logs `UtilAcceptVsock:273: accept4 failed 110` roughly once in ten cold
    /// calls), and reading that as "no image" pastes the stale X-clipboard text
    /// this whole path exists to stop. An unanswerable question is given to the
    /// agent, which asks Windows itself and may get further.
    Unknown,
}

/// Rounds that found no `powershell.exe` anywhere, in a row.
///
/// Reset by any answer at all, so this counts a *standing* absence rather than
/// a total: a machine that answered once has interop, whatever happens next.
static POWERSHELL_ABSENT_ROUNDS: AtomicUsize = AtomicUsize::new(0);

/// How many such rounds stop the question being asked at all.
///
/// Two rather than one because the first can be lost to a race nobody controls:
/// interop mounts `/mnt/c` and publishes `powershell.exe` on `PATH` a moment
/// after a distro starts, and a paste in that moment would otherwise silence
/// the probe for the rest of the session.
const ABSENT_ROUNDS_BEFORE_LATCH: usize = 2;

/// Whether this machine has been shown to have no PowerShell to ask.
///
/// The cost this saves is the reason the gate exists at all: on a box with no
/// interop every paste otherwise spawns nothing, waits for nothing and still
/// walks the candidate list before answering [`Verdict::Unknown`] — and the
/// press reaches the agent a round trip later than it needed to.
fn powershell_is_absent() -> bool {
    POWERSHELL_ABSENT_ROUNDS.load(Ordering::Relaxed) >= ABSENT_ROUNDS_BEFORE_LATCH
}

/// What one round of candidates says about the machine, as opposed to about
/// the clipboard.
///
/// Only [`std::io::ErrorKind::NotFound`] means "there is no PowerShell here".
/// `PermissionDenied`, an exec format error, a policy that refuses the spawn —
/// those are a machine that **has** one and would not run it this time, and
/// latching on them would turn a transient or administrative failure into a
/// permanent one. They stay [`Verdict::Unknown`] with the question still asked
/// next time.
fn is_absence(errors: &[std::io::ErrorKind]) -> bool {
    !errors.is_empty() && errors.iter().all(|k| *k == std::io::ErrorKind::NotFound)
}

/// Records what a round found, and reports whether the question is now latched
/// off. A round that reached PowerShell at all clears the count.
fn note_round(errors: &[std::io::ErrorKind]) -> bool {
    if is_absence(errors) {
        POWERSHELL_ABSENT_ROUNDS.fetch_add(1, Ordering::Relaxed);
    } else {
        POWERSHELL_ABSENT_ROUNDS.store(0, Ordering::Relaxed);
    }
    powershell_is_absent()
}

/// Clears what [`note_round`] counted. Tests only: the latch is deliberately
/// for the life of the process, and nothing in production wants it back.
#[cfg(test)]
fn forget_powershell_absence() {
    POWERSHELL_ABSENT_ROUNDS.store(0, Ordering::Relaxed);
}

/// Asks only what *kind* of thing is there — deliberately not for the image
/// itself: not carrying pixels over the boundary keeps the call cheap.
///
/// The answer is a word on stdout rather than an exit code, because an exit
/// code cannot say "I could not ask". PowerShell exits non-zero for a wedged
/// interop, a missing assembly and a clipboard without a picture alike, and
/// only one of those three means "paste the text".
///
/// `ContainsText` is the tie-break, and it is what makes an ordinary copy
/// survive: Excel, Word, Outlook and browsers all put a **bitmap alongside the
/// text** on a normal copy, so `ContainsImage` alone is true for copying a
/// spreadsheet row — and the press would be handed to the agent, which would
/// fetch a picture of the row the person meant to paste as text. Only a
/// clipboard that carries a picture and no text is an image paste.
const CLIPBOARD_KIND: &str = "Add-Type -AssemblyName System.Windows.Forms; \
     $c = [System.Windows.Forms.Clipboard]; \
     if ($c::ContainsImage() -and -not $c::ContainsText()) { 'image' } else { 'other' }";

/// What [`CLIPBOARD_KIND`] prints for a clipboard holding only a picture.
const IMAGE_ANSWER: &str = "image";
/// And for everything else. Named so an answer that is neither — a PowerShell
/// banner, a profile's stray output, an error — is read as "could not ask"
/// rather than as one of the two real answers.
const OTHER_ANSWER: &str = "other";

impl ImageProbe {
    /// Whether this machine is one where the question even arises.
    ///
    /// Only inside a WSL distro: everywhere else the local clipboard *is* the
    /// one being copied into, so arboard's answer is the whole truth and no
    /// subprocess is worth spawning.
    ///
    /// And only while a PowerShell to ask still looks reachable — see
    /// `powershell_is_absent`. A distro without interop would otherwise pay
    /// the gate's whole cost for a question that cannot be answered, on every
    /// paste, for as long as the session lasts.
    pub fn applies() -> bool {
        cfg!(unix)
            && !powershell_is_absent()
            && crate::session::host_def::current_wsl_distro().is_some()
    }

    /// Ask Windows, on a thread. The answer arrives at a later [`Self::poll`].
    ///
    /// `true` when a question was actually put, `false` when one was already
    /// out (see `in_flight`).
    ///
    /// The answer describes the clipboard **as of this call**, so it may only
    /// be applied to presses already made: the caller keeps the presses and
    /// re-asks for any that arrived after — see `poll_image_probe`. This end of
    /// the wire only knows what the clipboard holds, never who is waiting.
    pub fn ask(&mut self) -> bool {
        if self.in_flight {
            return false;
        }
        self.in_flight = true;
        let tx = self.channel.get_or_insert_with(channel).0.clone();
        std::thread::spawn(move || {
            let _ = tx.send(windows_clipboard_has_image());
        });
        true
    }

    /// The answer to one earlier [`Self::ask`], if one has come back.
    ///
    /// Taking it is what frees the next question, so a probe nobody polls is a
    /// probe nobody re-asks.
    pub fn poll(&mut self) -> Option<Verdict> {
        let (_, rx) = self.channel.as_ref()?;
        let answer = rx.try_recv().ok()?;
        self.in_flight = false;
        Some(answer)
    }
}

/// One PowerShell round trip, and the only place the three verdicts are told
/// apart. Every way the question could not be put — spawn, deadline, an answer
/// that is neither word — is [`Verdict::Unknown`], never `NotImage`.
fn windows_clipboard_has_image() -> Verdict {
    let ask = |exe: &Path| -> std::io::Result<Verdict> {
        let mut child = Command::new(exe)
            .args(["-NoProfile", "-NonInteractive", "-Sta", "-Command"])
            .arg(CLIPBOARD_KIND)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        // Past the spawn, a failure is a failure of *this* attempt: the fallback
        // below is for a `powershell.exe` that could not be found, and running a
        // second one because the first wedged would be two wedged processes
        // instead of one — with the person waiting through both deadlines.
        if !wait_bounded(&mut child, PROBE_TIMEOUT) {
            return Ok(Verdict::Unknown);
        }
        let mut answer = String::new();
        if let Some(mut out) = child.stdout.take() {
            // A word, read after the child is gone: the pipe holds it, and
            // nothing this small can fill the buffer and deadlock the wait.
            let _ = out.read_to_string(&mut answer);
        }
        Ok(match answer.trim() {
            IMAGE_ANSWER => Verdict::Image,
            OTHER_ANSWER => Verdict::NotImage,
            other => {
                tracing::debug!("the Windows clipboard probe answered {other:?}");
                Verdict::Unknown
            }
        })
    };
    let mut failures = Vec::new();
    for exe in powershell_candidates() {
        match ask(&exe) {
            Ok(verdict) => {
                note_round(&[]);
                return verdict;
            }
            Err(e) => {
                tracing::debug!("{} could not be run ({e})", exe.display());
                failures.push(e.kind());
            }
        }
    }
    if note_round(&failures) {
        tracing::info!(
            "no powershell.exe on this machine after {ABSENT_ROUNDS_BEFORE_LATCH} tries; \
             pastes go straight to the agent from here on"
        );
    }
    Verdict::Unknown
}

/// How long the probe waits for Windows before giving up on it.
///
/// The round trip is ~0.42 s measured, so this is not a budget anyone reaches
/// by being slow — it is there because WSL interop can wedge outright (this
/// machine has logged `UtilAcceptVsock:273: accept4 failed 110`), and a wedged
/// question with no deadline is a `powershell.exe` and a thread that never end,
/// one per press, for as long as the interface runs.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the wait looks. Next to a 0.42 s round trip this is noise, and it
/// costs nothing while the probe is out: the thread is asleep.
const PROBE_POLL: Duration = Duration::from_millis(25);

/// Whether `child` exited successfully within `timeout`, killing it if not.
///
/// A child that overran, was killed, or became unreadable is `false`, which the
/// caller reads as [`Verdict::Unknown`]: the deadline exists to stop a wedged
/// interop from leaking processes, not to decide what is on the clipboard.
fn wait_bounded(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() >= deadline => {
                tracing::warn!(
                    "the Windows clipboard probe did not answer within {timeout:?}; \
                     what the clipboard holds stays unknown and the press goes to the agent"
                );
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(PROBE_POLL),
            Err(e) => {
                tracing::debug!("could not wait for the Windows clipboard probe: {e}");
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe never runs a `powershell.exe` the working directory can reach.
    ///
    /// An empty entry in `PATH` — a stray leading, trailing or doubled `:` —
    /// means "here" to the OS search, and a relative entry is resolved against
    /// wherever thurbox was launched from, so a file of that name in a
    /// repository would answer every `Ctrl+V` on a WSL box. #1100 closed the
    /// same hole for agent spawns; this is that rule holding on the clipboard
    /// path. Planted where a relative `PATH` entry really does find it, since
    /// a lookup that cannot see the file proves nothing about the rule.
    #[test]
    #[cfg(unix)]
    fn the_probe_looks_only_at_absolute_path_entries() {
        use std::os::unix::fs::PermissionsExt;

        // Relative to the working directory a unit test runs in — the package
        // root — and inside the build directory, which is not tracked.
        let relative = std::path::Path::new("target").join("tbx-powershell-probe");
        std::fs::create_dir_all(&relative).unwrap();
        let planted = relative.join(POWERSHELL);
        std::fs::write(&planted, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            planted.exists(),
            "the plant has to be findable to prove anything"
        );

        let reachable = format!(":{}", relative.display());
        let candidates = crate::paths::with_path(&reachable, powershell_candidates);
        let _ = std::fs::remove_dir_all(&relative);

        assert_eq!(
            candidates,
            vec![PathBuf::from(POWERSHELL_FALLBACK)],
            "a PATH entry the working directory resolves reached the probe"
        );
    }

    /// And the interop PowerShell an absolute entry names is still what is
    /// tried before the hard-coded path.
    #[test]
    #[cfg(unix)]
    fn an_absolute_path_entry_is_what_the_probe_prefers() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let planted = dir.path().join(POWERSHELL);
        std::fs::write(&planted, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o700)).unwrap();

        let candidates = crate::paths::with_path(dir.path(), powershell_candidates);

        assert_eq!(
            candidates.first().map(PathBuf::as_path),
            Some(planted.as_path())
        );
    }

    /// The probe fires inside a WSL distro and nowhere else.
    ///
    /// The gate is the whole cost control: every paste on a machine that asks
    /// pays ~0.42 s for a `powershell.exe` that a plain Linux or macOS box has
    /// no reason to run. A gate that answered "yes" everywhere would be a
    /// third-of-a-second added to every `Ctrl+V` on every platform.
    #[test]
    fn only_a_wsl_distro_asks_windows() {
        use crate::session::host_def::with_wsl_distro;

        forget_powershell_absence();

        assert_eq!(
            with_wsl_distro(Some("Ubuntu"), ImageProbe::applies),
            cfg!(unix),
            "inside a distro the X clipboard is not the one being copied into, \
             so Windows has to be asked — and a native Windows build asks nobody"
        );
        assert!(
            !with_wsl_distro(None, ImageProbe::applies),
            "off WSL the local clipboard is the whole truth — asking would only \
             cost every paste a subprocess"
        );
    }

    /// A machine with no interop stops being asked — and only that machine.
    ///
    /// The three cases are one test because the latch is one counter and what
    /// matters is which outcomes move it: absence twice latches, anything that
    /// reached PowerShell clears it, and a refusal to run is not an absence.
    ///
    /// Process-global state, so this relies on the suite's runner (nextest)
    /// giving each test its own process, the way the `PATH`-mutating tests
    /// above already do.
    #[test]
    fn only_a_machine_without_powershell_stops_being_asked() {
        use crate::session::host_def::with_wsl_distro;
        use std::io::ErrorKind;

        forget_powershell_absence();
        assert!(
            !note_round(&[ErrorKind::NotFound]),
            "one round is not proof"
        );
        assert!(
            note_round(&[ErrorKind::NotFound]),
            "two rounds without a powershell.exe anywhere are"
        );
        if cfg!(unix) {
            assert!(
                !with_wsl_distro(Some("Ubuntu"), ImageProbe::applies),
                "a latched machine must stop paying ~0.42 s for a question \
                 nothing can answer"
            );
        }

        forget_powershell_absence();
        assert!(
            !note_round(&[ErrorKind::PermissionDenied, ErrorKind::PermissionDenied]),
            "a PowerShell that would not run is a machine that has one"
        );
        assert!(
            !note_round(&[ErrorKind::PermissionDenied, ErrorKind::PermissionDenied]),
            "and no number of refusals makes it missing"
        );

        forget_powershell_absence();
        note_round(&[ErrorKind::NotFound]);
        note_round(&[]);
        assert!(
            !note_round(&[ErrorKind::NotFound]),
            "an answer in between means the count starts again, not resumes"
        );
    }

    /// The PowerShell round trip reports what PowerShell itself reports.
    ///
    /// Run against the real Windows clipboard, because what is being tested is
    /// the wiring — the argument list, `-Sta` (without it `Add-Type` throws and
    /// every answer becomes "other"), the `PATH`/interop fallback, and reading
    /// a word off stdout rather than a status. A helper returning a constant
    /// would pass every test that stubbed this out.
    ///
    /// The oracle is asked for both halves of the question, because the probe's
    /// answer is a tie-break between them: a clipboard carrying a picture *and*
    /// text is an ordinary rich copy (Excel, Word, a browser) and must paste as
    /// text.
    ///
    /// Skipped where it cannot discriminate — off WSL, with no PowerShell —
    /// and, importantly, **not** skipped merely because the oracle was unhappy:
    /// a wedged interop exits non-zero with nothing on stdout, which the first
    /// version of this test read as "no image on the clipboard" and passed on.
    /// A skip says which of the two it was. Nothing here writes to the
    /// clipboard: a test suite that destroys what you copied is worse than a
    /// test that skips.
    #[test]
    fn the_windows_probe_agrees_with_powershell() {
        if !ImageProbe::applies() {
            eprintln!("skipping: not inside a WSL distro");
            return;
        }
        let oracle = Command::new(POWERSHELL)
            .args(["-NoProfile", "-NonInteractive", "-Sta", "-Command"])
            .arg(
                "Add-Type -AssemblyName System.Windows.Forms; \
                 $c = [System.Windows.Forms.Clipboard]; \
                 \"$($c::ContainsImage()) $($c::ContainsText())\"",
            )
            .output();
        let Ok(oracle) = oracle else {
            eprintln!("skipping: no powershell.exe on PATH");
            return;
        };
        let said = String::from_utf8_lossy(&oracle.stdout)
            .trim()
            .to_lowercase();
        let words: Vec<&str> = said.split_whitespace().collect();
        let (Some(&image), Some(&text), true) =
            (words.first(), words.get(1), oracle.status.success())
        else {
            eprintln!(
                "skipping: PowerShell could not be asked (status {:?}, said {said:?}) — \
                 which is NOT the same as the clipboard holding no image",
                oracle.status.code()
            );
            return;
        };
        let expected = if image == "true" && text != "true" {
            Verdict::Image
        } else {
            Verdict::NotImage
        };
        assert_eq!(
            windows_clipboard_has_image(),
            expected,
            "PowerShell reports ContainsImage={image} ContainsText={text} and the \
             probe disagreed"
        );
    }

    /// A probe that never answers is killed, not waited on.
    ///
    /// WSL interop wedges (`UtilAcceptVsock:273: accept4 failed 110` on this
    /// machine): the child is started and simply never returns. Without a
    /// deadline the worker thread and its `powershell.exe` live for the rest of
    /// the session — and because the probe is only re-asked once its answer is
    /// taken, the paste chord would stop being answered at all from then on.
    ///
    /// Driven with a real child that really hangs, because the claim is about
    /// the waiting: a helper asked to time out against nothing proves nothing.
    #[cfg(unix)]
    #[test]
    fn a_probe_that_never_answers_is_killed_rather_than_waited_on() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a hanging child");

        let started = Instant::now();
        let answered = wait_bounded(&mut child, Duration::from_millis(200));
        let waited = started.elapsed();

        assert!(!answered, "a child that never exited was read as an answer");
        assert!(
            waited < Duration::from_secs(5),
            "the wait ran past its deadline: {waited:?}"
        );
        // Killed, not merely abandoned: an abandoned one holds the interop pipe.
        assert!(
            matches!(child.try_wait(), Ok(Some(_))),
            "the child outlived the wait that gave up on it"
        );
    }

    /// A press made while a question is out does not ask a second one.
    ///
    /// Key auto-repeat holds `Ctrl+V` down at tens of presses a second, and each
    /// one used to spawn its own `powershell.exe` — ~0.42 s of cold start each,
    /// all asking the same question about a clipboard that cannot have changed
    /// in between. One question, one answer, however many presses are waiting
    /// on it.
    ///
    /// Asserted by counting answers rather than processes: a second question
    /// would deliver a second `true`/`false` down the same channel, so a probe
    /// that answers exactly once is a probe that asked exactly once. Runs
    /// anywhere — off WSL the command cannot start and the failure is the
    /// answer, which is the same one answer.
    #[test]
    fn one_question_is_asked_at_a_time() {
        let mut probe = ImageProbe::default();
        assert!(probe.ask(), "the first press put no question at all");
        assert!(
            !probe.ask(),
            "a press made while a question was out started a second one; the \
             caller reads this as its press being covered by an answer that was \
             asked for before it happened"
        );

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut first = None;
        while first.is_none() && Instant::now() < deadline {
            first = probe.poll();
            if first.is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert!(
            first.is_some(),
            "the probe never answered at all, so this asserts nothing about a \
             second one"
        );

        // A second question was asked at the same moment as the first, so its
        // answer would already be queued behind the one just taken.
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            probe.poll(),
            None,
            "a press made while a question was out asked Windows again"
        );

        // And taking the answer frees the next question: the press that was
        // refused above is asked for now, against the clipboard as it is now.
        assert!(
            probe.ask(),
            "no question could be put after the previous answer was taken; a \
             press waiting on a fresh one would wait for ever"
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
