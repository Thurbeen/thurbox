//! Shipping a file to a real native-Windows SSH host — the path that
//! provisions the host's `thurbox-cli` for session sharing.
//!
//! Needs a machine, so it is opt-in: `scripts/dev/e2e/real-host.sh <host>
//! transfer-test` runs it with `THURBOX_E2E_WINDOWS_HOST` set to an ssh
//! destination whose sshd runs Windows PowerShell (the default shell the
//! Windows VM harness configures, and a common one on real machines).
//!
//! The payload is larger than 4 KiB on purpose. Windows PowerShell 5.1's console
//! host consumes redirected stdin itself, so a script reading
//! `[Console]::OpenStandardInput()` got the first 4096 bytes and then waited
//! forever: the copy never returned, its PowerShell stayed behind holding the
//! half-written archive open, and every later attempt failed on "being used by
//! another process" — the host was never shareable again.

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use thurbox::session::{HostDef, Platform};

/// Long enough for a megabyte over a slow link, short enough that a stalled
/// copy fails the test instead of hanging the suite.
const COPY_BUDGET: Duration = Duration::from_secs(120);

fn host(destination: &str) -> HostDef {
    HostDef {
        name: "e2e-windows".into(),
        destination: destination.into(),
        platform: Some(Platform::Windows),
        multiplexer: Some("psmux".into()),
        ..Default::default()
    }
}

/// Run a PowerShell script on the host and return its stdout, the same way
/// thurbox addresses a Windows host: UTF-16LE base64 via `-EncodedCommand`.
fn powershell(destination: &str, script: &str) -> String {
    use base64::Engine as _;
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let out = Command::new("ssh")
        .args(["-o", "BatchMode=yes", destination])
        .args(["powershell", "-NoProfile", "-EncodedCommand", &encoded])
        .stdin(Stdio::null())
        .output()
        .expect("run ssh");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
#[ignore = "needs THURBOX_E2E_WINDOWS_HOST: run via scripts/dev/e2e/real-host.sh <host> transfer-test"]
fn a_binary_larger_than_a_pipe_buffer_reaches_a_windows_host_intact() {
    let destination = std::env::var("THURBOX_E2E_WINDOWS_HOST")
        .expect("THURBOX_E2E_WINDOWS_HOST names the ssh destination");
    let dir = powershell(&destination, "[IO.Path]::GetTempPath()")
        .trim()
        .replace('\\', "/");
    assert!(!dir.is_empty(), "the host answered no temp directory");
    let remote = format!(
        "{}/thurbox-e2e-transfer-{}/payload.bin",
        dir.trim_end_matches('/'),
        std::process::id()
    );

    // Every byte value, many times over: a transport that treats the payload as
    // text (line endings, a code page, a NUL) shows up as a different hash.
    let payload: Vec<u8> = (0..1024 * 1024).map(|i: u32| (i * 7 % 256) as u8).collect();
    let expected: String = Sha256::digest(&payload)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();

    let (tx, rx) = mpsc::channel();
    let (h, p, r) = (host(&destination), payload.clone(), remote.clone());
    std::thread::spawn(move || {
        let _ = tx.send(thurbox::git::copy_stream_to_remote_windows(&h, &p, &r));
    });
    let shipped = rx
        .recv_timeout(COPY_BUDGET)
        .unwrap_or_else(|_| panic!("the copy did not finish within {COPY_BUDGET:?}"));
    shipped.expect("the copy reported a failure");

    let quoted = format!("'{}'", remote.replace('\'', "''"));
    let hash = powershell(
        &destination,
        &format!(
            "(Get-FileHash -Algorithm SHA256 -LiteralPath {quoted}).Hash; \
             Remove-Item -Recurse -Force -LiteralPath (Split-Path {quoted})"
        ),
    );
    assert_eq!(
        hash.lines().map(str::trim).find(|l| l.len() == 64),
        Some(expected.as_str()),
        "the file on the host is not the payload that was sent"
    );
}
