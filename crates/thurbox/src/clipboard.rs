use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use base64::Engine as _;

use crate::session::settings::ClipboardProvider;

pub const OSC52_MAX_BYTES: usize = 74_994;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyRoute {
    Native,
    Osc52,
    Both,
}

impl CopyRoute {
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

#[derive(Debug)]
pub enum CopyError {
    TooLarge { bytes: usize },
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

pub fn osc52_sequence(text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

pub fn app_copy_text(target: &[u8], data: &[u8]) -> Option<String> {
    if !target.contains(&b'c') || data.is_empty() || data.len() > OSC52_MAX_BYTES.div_ceil(3) * 4 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    if bytes.is_empty() || bytes.len() > OSC52_MAX_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

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

pub fn paste(native: Option<&mut arboard::Clipboard>) -> Option<String> {
    native.and_then(|cb| cb.get_text().ok())
}

pub const PASTE_UNAVAILABLE_HINT: &str =
    "No local clipboard — use your terminal's paste (Ctrl+Shift+V)";

#[derive(Default)]
pub struct ImageProbe {
    channel: Option<(Sender<Verdict>, Receiver<Verdict>)>,
    in_flight: bool,
}

const POWERSHELL: &str = "powershell.exe";
const POWERSHELL_FALLBACK: &str = "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";

fn powershell_candidates() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = crate::paths::resolve_on_path(POWERSHELL)
        .into_iter()
        .collect();
    found.push(PathBuf::from(POWERSHELL_FALLBACK));
    found
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Image,
    NotImage,
    Unknown,
}

static POWERSHELL_ABSENT_ROUNDS: AtomicUsize = AtomicUsize::new(0);

const ABSENT_ROUNDS_BEFORE_LATCH: usize = 2;

fn powershell_is_absent() -> bool {
    POWERSHELL_ABSENT_ROUNDS.load(Ordering::Relaxed) >= ABSENT_ROUNDS_BEFORE_LATCH
}

fn is_absence(errors: &[std::io::ErrorKind]) -> bool {
    !errors.is_empty() && errors.iter().all(|k| *k == std::io::ErrorKind::NotFound)
}

fn note_round(errors: &[std::io::ErrorKind]) -> bool {
    if is_absence(errors) {
        POWERSHELL_ABSENT_ROUNDS.fetch_add(1, Ordering::Relaxed);
    } else {
        POWERSHELL_ABSENT_ROUNDS.store(0, Ordering::Relaxed);
    }
    powershell_is_absent()
}

#[cfg(test)]
fn forget_powershell_absence() {
    POWERSHELL_ABSENT_ROUNDS.store(0, Ordering::Relaxed);
}

const CLIPBOARD_KIND: &str = "Add-Type -AssemblyName System.Windows.Forms; \
     $c = [System.Windows.Forms.Clipboard]; \
     if ($c::ContainsImage() -and -not $c::ContainsText()) { 'image' } else { 'other' }";

const IMAGE_ANSWER: &str = "image";
const OTHER_ANSWER: &str = "other";

impl ImageProbe {
    pub fn applies() -> bool {
        cfg!(unix) && !powershell_is_absent() && crate::paths::current_wsl_distro().is_some()
    }

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

    pub fn poll(&mut self) -> Option<Verdict> {
        let (_, rx) = self.channel.as_ref()?;
        let answer = rx.try_recv().ok()?;
        self.in_flight = false;
        Some(answer)
    }
}

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

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

const PROBE_POLL: Duration = Duration::from_millis(25);

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

    #[test]
    fn only_a_wsl_distro_asks_windows() {
        use crate::paths::with_wsl_distro;

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

    #[test]
    fn only_a_machine_without_powershell_stops_being_asked() {
        use crate::paths::with_wsl_distro;
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

    #[test]
    fn oversized_text_without_a_native_clipboard_is_still_an_error() {
        let huge = "x".repeat(OSC52_MAX_BYTES + 1);
        let err = copy(&huge, None, ClipboardProvider::Auto).unwrap_err();
        assert!(matches!(err, CopyError::TooLarge { .. }));
    }

    #[test]
    fn an_app_copy_is_clipboard_targeted_utf8_text_that_fits_one_osc52() {
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        assert_eq!(
            app_copy_text(b"c", b64("é漢".as_bytes()).as_bytes()).as_deref(),
            Some("é漢")
        );
        assert_eq!(
            app_copy_text(b"pc", b64(b"x").as_bytes()).as_deref(),
            Some("x")
        );
        assert_eq!(app_copy_text(b"p", b64(b"x").as_bytes()), None);
        assert_eq!(app_copy_text(b"", b64(b"x").as_bytes()), None);
        assert_eq!(app_copy_text(b"c", b""), None);
        assert_eq!(app_copy_text(b"c", b"abc"), None);
        assert_eq!(app_copy_text(b"c", b64(b"\xff\xfe").as_bytes()), None);

        let fits = "a".repeat(OSC52_MAX_BYTES);
        assert_eq!(
            app_copy_text(b"c", b64(fits.as_bytes()).as_bytes()),
            Some(fits)
        );
        let over = "a".repeat(OSC52_MAX_BYTES + 1);
        assert_eq!(app_copy_text(b"c", b64(over.as_bytes()).as_bytes()), None);
    }
}
