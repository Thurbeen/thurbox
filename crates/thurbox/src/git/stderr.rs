pub(crate) fn reportable_stderr(stderr: &[u8]) -> String {
    const SSH_ADVISORY_PREFIX: &str = "**";

    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| !line.trim_start().starts_with(SSH_ADVISORY_PREFIX))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

pub(crate) fn reportable_stderr_line(stderr: &[u8]) -> String {
    reportable_stderr(stderr)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}
