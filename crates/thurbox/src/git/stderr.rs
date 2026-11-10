//! The reportable part of what a failed `git` printed on stderr.

/// The reportable part of a command's stderr, with ssh's advisories removed.
///
/// A `git clone`/`git fetch` runs over ssh through `GIT_SSH_COMMAND`, and
/// OpenSSH ≥ 10 prints a three-line block on **stderr**, each line marked with
/// its own `**`, for every connection whose key exchange is not post-quantum —
/// which is every connection to a server still on an older OpenSSH:
///
/// ```text
/// ** WARNING: connection is not using a post-quantum key exchange algorithm.
/// ** This session may be vulnerable to "store now, decrypt later" attacks.
/// ** The server may need to be upgraded. See https://openssh.com/pq.html
/// ```
///
/// It is informational and the command still runs, but it is *first* in the
/// buffer, so reporting stderr verbatim made every failure read as "connection
/// is not using a post-quantum key…" and buried the real cause. Suppressing it
/// at the source is not an option, because `LogLevel=ERROR` would equally hide
/// `Permission denied` and `WarnWeakCrypto=no` is fatal on the older clients
/// that never warn anyway.
pub(crate) fn reportable_stderr(stderr: &[u8]) -> String {
    /// OpenSSH's marker for an advisory it prints without failing.
    const SSH_ADVISORY_PREFIX: &str = "**";

    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| !line.trim_start().starts_with(SSH_ADVISORY_PREFIX))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}
