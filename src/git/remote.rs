//! Running things on somebody else's machine, and reading what came back.
//!
//! Two halves. The first launches a command through the host's transport (ssh
//! or `wsl.exe`) and, for a **Windows** host — which has no POSIX shell — sends
//! PowerShell as UTF-16LE base64, because ssh space-joins its args for a default
//! sshd shell that is commonly PowerShell and would expand `$…` inside them.
//!
//! The second is about *error text*, and it is the half that is easy to skip and
//! then rediscover: OpenSSH ≥ 10 prints a three-line post-quantum advisory on
//! stderr, first, so it used to be the whole reported error; and PowerShell
//! wraps stderr in a `#< CLIXML` envelope that has to be decoded to reach the
//! message inside it. `reportable_stderr` is the one place both are handled.
//!
//! **Read the psmux divergences subsection of ADR-13 before changing anything
//! here** — each workaround has non-obvious quoting and tokenizing constraints,
//! and delivery is probed by `scripts/dev/e2e/windows-vm.sh test`.

use super::*;

/// Cache of resolved host `$HOME` directories, keyed by the host's backend name
/// (`ssh:<name>` / `wsl:<name>`), so we only pay one round-trip per host. A WSL
/// host has no `destination`, so the backend name — unique per host — is the
/// stable key for both kinds. Entries live for the process lifetime:
/// `hosts.toml` is read once at startup, so repointing a host requires a
/// restart anyway — the cache can never be staler than the config it derives
/// from.
pub(super) fn remote_home_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve the home directory on a host (over SSH or inside a WSL distro),
/// caching the result.
///
/// A **native-Windows** host has no `$HOME`, and asking anyway is worse than
/// failing: `echo $HOME` under `cmd`/PowerShell prints the string `$HOME` and
/// exits 0, so the bogus value was accepted and every `~`-relative path became
/// a literal `$HOME/…`. Such a host is routed to
/// [`remote_home_windows`] (`%USERPROFILE%`) instead — the same cache, since a
/// host is one platform or the other and never both.
pub(crate) fn remote_home(host: &HostDef) -> Result<String> {
    if host.is_windows() {
        return remote_home_windows(host);
    }
    let key = host.backend_name();
    if let Ok(guard) = remote_home_cache().lock() {
        if let Some(home) = guard.get(&key) {
            return Ok(home.clone());
        }
    }
    let mut cmd = host_launcher(host);
    // Pass `$HOME` literally; the host login shell expands it.
    cmd.arg("echo").arg("$HOME");
    let output = cmd
        .stderr(Stdio::piped())
        .output()
        .context("failed to resolve host $HOME")?;
    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("`echo $HOME` on {key} failed: {stderr}");
    }
    let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if home.is_empty() {
        anyhow::bail!("$HOME resolved empty for {key}");
    }
    if let Ok(mut guard) = remote_home_cache().lock() {
        guard.insert(key, home.clone());
    }
    Ok(home)
}

/// Resolve `%USERPROFILE%` on a **native-Windows** SSH host (a `psmux` host),
/// normalized to forward slashes (`C:/Users/me`) and cached like
/// [`remote_home`] (same key space — a host is either POSIX or Windows, never
/// both). Runs `powershell -NoProfile -Command Write-Output $env:USERPROFILE`,
/// which works whether the host's default sshd shell is `cmd` or PowerShell
/// (`cmd` passes `$env:…` through untouched for PowerShell to evaluate).
pub(crate) fn remote_home_windows(host: &HostDef) -> Result<String> {
    let key = host.backend_name();
    if let Ok(guard) = remote_home_cache().lock() {
        if let Some(home) = guard.get(&key) {
            return Ok(home.clone());
        }
    }
    let mut cmd = host_launcher(host);
    cmd.args(["powershell", "-NoProfile", "-Command"])
        .arg("Write-Output $env:USERPROFILE");
    let output = cmd
        .stderr(Stdio::piped())
        .output()
        .context("failed to resolve host %USERPROFILE%")?;
    if !output.status.success() {
        let stderr = reportable_stderr(&output.stderr);
        anyhow::bail!("resolving %USERPROFILE% on {key} failed: {stderr}");
    }
    let home = String::from_utf8_lossy(&output.stdout)
        .trim()
        .replace('\\', "/");
    if home.is_empty() {
        anyhow::bail!("%USERPROFILE% resolved empty for {key}");
    }
    if let Ok(mut guard) = remote_home_cache().lock() {
        guard.insert(key, home.clone());
    }
    Ok(home)
}

/// The remote workspace directory for a session id on `host`, mirroring the
/// local layout (`<thurbox data root>/workspaces/<sanitized id>`). Base:
/// `<worktrees_dir>/..`, or `$HOME/.local/share/thurbox`. Sanitizes the id
/// with the same shared helper as the local builder
/// (`workspace::workspace_dir`) — including its empty-id rejection: an empty
/// segment would make the `rm -rf` in ensure/remove target the workspaces
/// *root*, wiping every session's workspace on the host.
pub(crate) fn remote_workspace_dir(host: &HostDef, id: &str) -> Result<String> {
    let base = match &host.worktrees_dir {
        Some(dir) => Path::new(dir)
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| dir.clone()),
        None => format!("{}/.local/share/thurbox", remote_home(host)?),
    };
    let segment = crate::paths::sanitize_workspace_segment(id);
    anyhow::ensure!(!segment.is_empty(), "empty workspace id");
    Ok(format!("{base}/workspaces/{segment}"))
}

/// Build a per-session **remote** symlink workspace on `host`: a directory
/// holding one symlink per member (`<label> -> <remote member path>`), so a
/// multi-repo remote session launches somewhere every repo is a visible subdir.
///
/// This is the remote analogue of [`crate::workspace::ensure_workspace`], run
/// over the host launcher (`ssh`/`wsl.exe`). All paths are POSIX (`/`-joined):
/// the `sh -c` script assumes a POSIX host, so on a Windows (`psmux`) SSH host
/// it fails and callers fall back to the primary cwd (a multi-repo session
/// there loses its workspace, with a logged error). Returns the remote
/// workspace directory path.
pub fn ensure_remote_workspace(
    host: &HostDef,
    id: &str,
    members: &[(String, PathBuf)],
) -> Result<PathBuf> {
    let ws = remote_workspace_dir(host, id)?;

    // Fresh dir, then one symlink per member, de-duplicating names with a `-2`,
    // `-3`, … suffix (matching the local builder).
    let mut script = format!("rm -rf {ws} && mkdir -p {ws}", ws = posix_quote(&ws));
    let mut used: HashSet<String> = HashSet::new();
    for (label, target) in members {
        let name = crate::paths::unique_link_name(label, &mut used);
        let link = format!("{ws}/{name}");
        script.push_str(&format!(
            " && ln -s {target} {link}",
            target = posix_quote(&target.to_string_lossy()),
            link = posix_quote(&link),
        ));
    }

    run_host_script(host, &script, "workspace build")?;
    Ok(PathBuf::from(ws))
}

/// Remove the remote symlink workspace for a session id on `host` — the
/// teardown counterpart of [`ensure_remote_workspace`], the remote analogue of
/// [`crate::workspace::remove_workspace`]. Only the workspace dir (symlinks) is
/// removed; the repos the links point at are untouched, and a missing
/// workspace is not an error (`rm -rf` on a missing path succeeds).
pub fn remove_remote_workspace(host: &HostDef, id: &str) -> Result<()> {
    let ws = remote_workspace_dir(host, id)?;
    run_host_script(
        host,
        &format!("rm -rf {}", posix_quote(&ws)),
        "workspace removal",
    )
}

/// The reportable part of a command's stderr: the transport's own advisories
/// and serialization removed, so what is left is what actually went wrong.
///
/// Applied to local git too, not only a remote host: a `git clone`/`git fetch`
/// runs over ssh through `GIT_SSH_COMMAND`, so it carries the same advisory.
///
/// OpenSSH ≥ 10 prints a three-line block on **stderr**, each line marked with
/// its own `**`, for every connection whose key exchange is not post-quantum —
/// which is every connection to a server still on an older OpenSSH, Windows
/// hosts very much included:
///
/// ```text
/// ** WARNING: connection is not using a post-quantum key exchange algorithm.
/// ** This session may be vulnerable to "store now, decrypt later" attacks.
/// ** The server may need to be upgraded. See https://openssh.com/pq.html
/// ```
///
/// It is informational and the command still runs, but it is *first* in the
/// buffer, so reporting stderr verbatim made every remote failure read as
/// "connection is not using a post-quantum key…" and buried the real cause
/// below the fold. Suppressing it at the source is not an option, because
/// `LogLevel=ERROR` would equally hide `Permission denied` and
/// `WarnWeakCrypto=no` is fatal on the older clients that never warn anyway.
///
/// Returns **empty** when nothing reportable was there — only advisories, or a
/// CLIXML envelope with no error in it. The caller then reports the exit status
/// rather than pasting the noise back in, which would recreate the very
/// confusion this removes.
pub(super) fn reportable_stderr(stderr: &[u8]) -> String {
    /// OpenSSH's marker for an advisory it prints without failing.
    const SSH_ADVISORY_PREFIX: &str = "**";

    let raw = String::from_utf8_lossy(stderr);
    let without_advisories = raw
        .lines()
        .filter(|line| !line.trim_start().starts_with(SSH_ADVISORY_PREFIX))
        .collect::<Vec<_>>()
        .join("\n");
    decode_clixml(&without_advisories).unwrap_or_else(|| without_advisories.trim().to_string())
}

/// Recover the human text from PowerShell's CLIXML stderr serialization.
///
/// `powershell.exe` does not write error records as text when its stderr is
/// redirected (which it always is here) — it writes a `#< CLIXML` document
/// whose messages sit in `<S S="Error">` nodes with CRLF encoded as
/// `_x000D__x000A_`. Reported verbatim that is unreadable, so a Windows host's
/// real failures arrived as `#< CLIXML <Objs Version=…` — the same class of
/// defect as the post-quantum advisory: a message that names nothing.
///
/// The envelope is removed whole — the `#< CLIXML` marker and every `<Objs …>`
/// document — with each document replaced by the text of its error records.
/// Removing only the records would leave the scaffolding behind, and a document
/// often carries no error at all (PowerShell emits a `progress` record for
/// "Preparing modules for first use"), so the useful line ends up buried in
/// XML either way. Text outside a document is a native command's own stderr and
/// is kept verbatim.
///
/// `None` when the input is not CLIXML, so the POSIX path is untouched;
/// `Some("")` when it was CLIXML carrying nothing to say, so the caller falls
/// back to the exit status instead of the markup. Scanned rather than parsed as
/// XML: this is one fixed shape from one producer, and a diagnostic string is
/// not worth an XML dependency.
pub(super) fn decode_clixml(stderr: &str) -> Option<String> {
    /// The line PowerShell opens the serialization with.
    const CLIXML_MARKER: &str = "#< CLIXML";
    const DOCUMENT_OPEN: &str = "<Objs";
    const DOCUMENT_CLOSE: &str = "</Objs>";

    if !stderr.contains(CLIXML_MARKER) {
        return None;
    }
    let body = strip_lines_equal_to(stderr, CLIXML_MARKER);

    let mut out = String::new();
    let mut rest = body.as_str();
    while let Some(start) = rest.find(DOCUMENT_OPEN) {
        out.push_str(&rest[..start]);
        let from_open = &rest[start..];
        // A truncated document is consumed whole rather than half-emitted.
        let (document, tail) = match from_open.find(DOCUMENT_CLOSE) {
            Some(end) => from_open.split_at(end + DOCUMENT_CLOSE.len()),
            None => (from_open, ""),
        };
        out.push_str(&clixml_error_text(document));
        rest = tail;
    }
    out.push_str(rest);

    Some(join_non_blank_lines(&out))
}

/// The decoded text of every error record in one CLIXML document.
pub(super) fn clixml_error_text(document: &str) -> String {
    const RECORD_OPEN: &str = "<S S=\"Error\">";
    const RECORD_CLOSE: &str = "</S>";

    let mut text = String::new();
    let mut rest = document;
    while let Some(open) = rest.find(RECORD_OPEN) {
        rest = &rest[open + RECORD_OPEN.len()..];
        let Some(close) = rest.find(RECORD_CLOSE) else {
            break;
        };
        text.push_str(&rest[..close]);
        rest = &rest[close..];
    }
    unescape_clixml(&text)
}

/// Decode CLIXML's `_xNNNN_` character escapes and the five XML entities.
///
/// An `_x` that does not open a well-formed escape is literal text, so it is
/// passed through rather than guessed at.
pub(super) fn unescape_clixml(text: &str) -> String {
    /// What follows the opening `_x`: four hex digits and a closing `_`.
    const ESCAPE_TAIL_LEN: usize = 5;

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("_x") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        match decode_hex_escape(after) {
            Some(ch) => {
                out.push(ch);
                rest = &after[ESCAPE_TAIL_LEN..];
            }
            None => {
                out.push_str("_x");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    // `&amp;` last, so an escaped ampersand cannot re-open one of the others.
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The character `NNNN_` names, when `after` opens one — four hex digits then a
/// closing underscore. `None` for anything else.
pub(super) fn decode_hex_escape(after: &str) -> Option<char> {
    let hex = after.get(..4)?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) || after.as_bytes().get(4) != Some(&b'_') {
        return None;
    }
    char::from_u32(u32::from_str_radix(hex, 16).ok()?)
}

/// `text` without the lines that are exactly `marker`.
pub(super) fn strip_lines_equal_to(text: &str, marker: &str) -> String {
    text.lines()
        .filter(|line| line.trim() != marker)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The non-blank lines of `text`, de-duplicated and joined.
///
/// CLIXML error records each end in an encoded CRLF, so decoding one yields
/// trailing blanks; consecutive duplicates come from a record repeated across
/// the envelope's streams.
pub(super) fn join_non_blank_lines(text: &str) -> String {
    let mut lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines.dedup();
    lines.join("\n").trim().to_string()
}

/// Map a finished remote command to `Ok(stdout)`, or an error carrying the
/// remote stderr when it exited non-zero. Shared by every remote helper below
/// so their failures read uniformly.
pub(super) fn remote_output_or_stderr(
    output: std::process::Output,
    action: &str,
) -> Result<Vec<u8>> {
    if output.status.success() {
        return Ok(output.stdout);
    }
    let detail = reportable_stderr(&output.stderr);
    // A command that failed silently still has to say *something*; its exit
    // code is more use than a blank after the colon.
    if detail.is_empty() {
        anyhow::bail!(
            "remote {action} failed: {}",
            describe_exit(output.status.code())
        );
    }
    anyhow::bail!("remote {action} failed: {detail}")
}

/// Describe a remote command's exit code, for when it printed nothing to explain
/// itself. `None` is "terminated without one" — a signal on unix.
///
/// `255` is worth naming because it is not the command's: ssh reserves it for
/// its own failures, so it means the host was never reached rather than that
/// the command ran and failed. Phrased without naming ssh — the same helper
/// serves the `wsl.exe` transport, which has no such convention.
///
/// Takes the code rather than the `ExitStatus` it came from: the status is only
/// ever consulted for it, and the extra coupling made this untestable without
/// `ExitStatusExt`, whose constructor is per-platform — so the test compiled on
/// unix and broke the Windows build.
pub(super) fn describe_exit(code: Option<i32>) -> String {
    /// ssh's reserved code for "the connection itself failed".
    const SSH_TRANSPORT_FAILURE: i32 = 255;

    match code {
        Some(SSH_TRANSPORT_FAILURE) => {
            "exit 255, no error output — the host was likely never reached".to_string()
        }
        Some(code) => format!("exit {code}, no error output"),
        None => "terminated without an exit code".to_string(),
    }
}

/// Run `script` on `host` via [`host_shell_c`], discarding stdout. See
/// [`remote_output_or_stderr`] for the failure shape.
pub(super) fn run_host_script(host: &HostDef, script: &str, action: &str) -> Result<()> {
    let output = host_shell_c(host, script)
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to run remote {action}"))?;
    remote_output_or_stderr(output, action).map(|_| ())
}

/// Build a `<launcher> sh -c <script>` [`Command`] for a host, correct for each
/// transport's argument handling.
///
/// The two launchers disagree on how trailing args reach the in-host shell:
/// - **`wsl.exe`** gets `--exec` (`-e`), which bypasses `wsl.exe`'s own
///   command-line processing entirely: argv reaches the in-distro process
///   verbatim, so the multi-statement `script` travels as one **unquoted** arg
///   and `sh -c` parses the `posix_quote`d paths inside it exactly like the
///   ssh path. Without `-e`, `wsl.exe` mangles the script — it substitutes
///   `$…` even inside a preserved argument (single quotes don't protect it),
///   and pre-quoting the script makes the in-distro shell treat the quoted
///   blob as one command word ("not found").
/// - **`ssh`** space-joins its trailing args into one string the remote login
///   shell re-splits, so the `script` must be POSIX-quoted to survive as a
///   single `sh -c` argument (mirroring [`git_command`]).
pub(crate) fn host_shell_c(host: &HostDef, script: &str) -> Command {
    let mut cmd = host_launcher(host);
    if host.is_wsl() {
        cmd.arg("-e").arg("sh").arg("-c").arg(script);
    } else {
        cmd.arg(posix_quote("sh"))
            .arg(posix_quote("-c"))
            .arg(posix_quote(script));
    }
    cmd
}

/// Build a `<launcher> powershell -NoProfile -EncodedCommand <base64>`
/// [`Command`] — [`host_shell_c`] for a **native-Windows** host, which has no
/// `sh` at all (`sh -c …` there fails with PowerShell's
/// `CommandNotFoundException`, which is what made the repo picker unable to
/// list a directory on a Windows host).
///
/// `-EncodedCommand` rather than `-Command` because the script must cross two
/// shells that both rewrite it. ssh space-joins its trailing args into one
/// string that the host's **default sshd shell** parses — and that shell is
/// commonly PowerShell itself, which expands `$…` inside double quotes: a
/// probe reading `$PSVersionTable` came back with the *outer* shell's expansion
/// (`System.Collections.Hashtable`) substituted into it. Base64 is
/// `[A-Za-z0-9+/=]` only, so neither `cmd` nor PowerShell finds anything to
/// interpret and the script arrives byte-exact.
///
/// The payload is **UTF-16LE**, which is what `-EncodedCommand` decodes.
pub(crate) fn host_powershell_c(host: &HostDef, script: &str) -> Command {
    use base64::Engine as _;

    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&utf16);
    let mut cmd = host_launcher(host);
    cmd.args(["powershell", "-NoProfile", "-EncodedCommand"])
        .arg(encoded);
    cmd
}

/// Run a probe on `host` in whichever dialect the host speaks: `posix` through
/// [`host_shell_c`], or `windows` (PowerShell) through [`host_powershell_c`].
///
/// The one dispatch point for the platform, so a probe cannot be POSIX-only by
/// omission — and each pair of scripts is written to emit the **same line
/// protocol**, which is why every parser below stays transport-neutral and
/// untouched by the Windows path.
pub(super) fn host_probe(host: &HostDef, posix: &str, windows: &str) -> Command {
    if host.is_windows() {
        host_powershell_c(host, windows)
    } else {
        host_shell_c(host, posix)
    }
}

/// Quote `s` as a PowerShell single-quoted literal. Only `'` is special inside
/// one (doubled to escape); `\` and `$` are literal, which is what makes it the
/// right quote for a Windows path.
pub(super) fn powershell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Write `bytes` to `remote_path` on `host`, creating the parent directory.
/// Streams the bytes over the host launcher's stdin into `cat > <path>`, so it
/// is transport-neutral (ssh/wsl) and needs no `scp`/`\\wsl$` share. Used to
/// materialize thurbox-managed agent config (e.g. the hooks `--settings
/// claude.json`, with its commands rewritten for the host) on the remote so
/// the agent — launched with a `--settings <path>` that thurbox generated
/// against the *local* config dir — finds the file at that path there too.
pub fn copy_bytes_to_remote(host: &HostDef, bytes: &[u8], remote_path: &str) -> Result<()> {
    use std::io::Write;

    let parent = Path::new(remote_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string());
    // `mkdir -p <dir> && cat > <file>`: stdin is the file body. Both transports
    // pass stdin straight through to the in-host shell.
    let script = format!(
        "mkdir -p {dir} && cat > {file}",
        dir = posix_quote(&parent),
        file = posix_quote(remote_path),
    );

    let mut child = host_shell_c(host, &script)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn remote file-copy")?;
    child
        .stdin
        .take()
        .context("remote file-copy stdin unavailable")?
        .write_all(bytes)
        .context("failed to stream file to remote")?;
    let output = child
        .wait_with_output()
        .context("failed to wait on remote file-copy")?;
    remote_output_or_stderr(output, "file-copy").map(|_| ())
}

/// [`copy_bytes_to_remote`] for a **native-Windows** SSH host: `cat > file`
/// doesn't exist there, so the payload travels base64-encoded inside a
/// PowerShell one-liner (`[IO.File]::WriteAllBytes` + `New-Item -Force` for
/// the parent dir). Bounded by the Windows command-line limit — thurbox's hook
/// payloads are 1–4 KB, well within it; anything larger errors instead of
/// truncating. `remote_path` must be `/`-separated (PowerShell accepts it).
pub(crate) fn copy_bytes_to_remote_windows(
    host: &HostDef,
    bytes: &[u8],
    remote_path: &str,
) -> Result<()> {
    use base64::Engine as _;

    let parent = Path::new(remote_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    anyhow::ensure!(
        !remote_path.contains('\'') && !parent.contains('\''),
        "remote path {remote_path} contains a quote"
    );
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let script = format!(
        "New-Item -ItemType Directory -Force -Path '{parent}' | Out-Null; \
         [IO.File]::WriteAllBytes('{remote_path}', [Convert]::FromBase64String('{b64}'))"
    );
    anyhow::ensure!(
        script.len() <= WINDOWS_COMMAND_BUDGET,
        "payload too large for a Windows command line ({} bytes)",
        bytes.len()
    );
    let mut cmd = host_launcher(host);
    // Double-quoted so the host's default shell (cmd or PowerShell) hands the
    // script to `powershell -Command` as one argument; the payload/path are
    // single-quoted inside, and base64 text never contains quotes.
    cmd.args(["powershell", "-NoProfile", "-Command"])
        .arg(format!("\"{script}\""));
    let output = cmd
        .stderr(Stdio::piped())
        .output()
        .context("failed to spawn windows remote file-copy")?;
    remote_output_or_stderr(output, "windows file-copy").map(|_| ())
}

/// Expand a leading `~` in a remote path against the host's `$HOME`. Remote
/// paths are never expanded against the *local* home (they're different
/// filesystems), so this is the only `~` handling a remote path gets. A path
/// with no `~` prefix passes through unchanged.
pub fn expand_remote_tilde(host: &HostDef, path: &str) -> Result<String> {
    if path == "~" {
        return remote_home(host);
    }
    match path.strip_prefix("~/") {
        Some(rest) => Ok(format!("{}/{}", remote_home(host)?, rest)),
        None => Ok(path.to_string()),
    }
}

/// Whether `dir` exists as a directory on `host` (a leading `~` expands against
/// the remote home). `Ok(false)` means the probe *ran* and answered no — a
/// transport failure is an `Err`, distinguished by the exit code: the script
/// answers only 0 / [`REMOTE_PROBE_NEGATIVE`] itself.
pub(crate) fn remote_dir_exists(host: &HostDef, dir: &str) -> Result<bool> {
    let dir = expand_remote_tilde(host, dir)?;
    let script = format!(
        "if test -d {}; then exit 0; else exit {REMOTE_PROBE_NEGATIVE}; fi",
        posix_quote(&dir)
    );
    let output = host_shell_c(host, &script)
        .stderr(Stdio::piped())
        .output()
        .context("failed to run remote dir probe")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(REMOTE_PROBE_NEGATIVE) => Ok(false),
        _ => {
            let stderr = reportable_stderr(&output.stderr);
            anyhow::bail!("remote dir probe failed: {stderr}")
        }
    }
}

/// Read a regular file on `host` (a leading `~` expands against the remote
/// home). `Ok(None)` = the path doesn't exist; an existing-but-not-regular
/// path or a transport failure is an `Err` (see [`remote_dir_exists`] for the
/// exit-code discipline).
pub(crate) fn read_remote_file(host: &HostDef, path: &str) -> Result<Option<String>> {
    let path = expand_remote_tilde(host, path)?;
    let quoted = posix_quote(&path);
    let script = format!(
        "if test -f {quoted}; then cat {quoted}; elif test -e {quoted}; then \
         exit {REMOTE_PROBE_NOT_FILE}; else exit {REMOTE_PROBE_NEGATIVE}; fi"
    );
    let output = host_shell_c(host, &script)
        .stderr(Stdio::piped())
        .output()
        .context("failed to run remote file read")?;
    match output.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned())),
        Some(REMOTE_PROBE_NEGATIVE) => Ok(None),
        Some(REMOTE_PROBE_NOT_FILE) => {
            anyhow::bail!("remote path {path} exists but is not a regular file")
        }
        // Anything else includes `cat`'s own failure (e.g. permission denied,
        // exit 1) — surface its stderr rather than misreporting the file type.
        _ => {
            let stderr = reportable_stderr(&output.stderr);
            anyhow::bail!("remote file read failed: {stderr}")
        }
    }
}

/// Sentinel exit code remote probe scripts use for their "negative" answer, so
/// it can't be confused with a transport failure (ssh's 255, a launch error,
/// or the probed command's own 1).
pub(super) const REMOTE_PROBE_NEGATIVE: i32 = 3;

/// Sentinel for "the path exists but is not a regular file" — distinct from
/// [`REMOTE_PROBE_NEGATIVE`] *and* from the probed command's own exit 1 (e.g.
/// `cat` failing on a permission-denied file must not be misreported as a
/// file-type problem).
pub(super) const REMOTE_PROBE_NOT_FILE: i32 = 4;

/// The longest command line safely below Windows `cmd.exe`'s ~8191-char limit
/// (the sshd default shell may be `cmd`), leaving headroom for the PowerShell
/// wrapper around the base64 payload.
pub(super) const WINDOWS_COMMAND_BUDGET: usize = 7_500;
