//! What the CLI-backed adapters share: one bounded call through
//! fleet-exec, the HTTP head a `--include` answer carries, the forge's
//! rate-limit advice, a budget for one observation made of several calls,
//! and the forge's timestamps as Unix milliseconds.

use std::ffi::OsString;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fleet_api::Millis;
use fleet_api::forge::Uncertainty;
use fleet_exec::{Outcome, Spec, run};

/// The most calls one observation of a change makes. The bound reported for
/// the port is this many worst-case calls; the budget below runs out first.
pub const MAX_CALLS_PER_OBSERVATION: u32 = 6;

/// One HTTP answer as `gh api --include` and `glab api --include` print it:
/// the status line and headers, then the body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    /// 0 when no status line was printed.
    pub status: u16,
    /// Lowercased names.
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub exit: Option<i32>,
    pub stderr: String,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// How long the forge asked to wait, from `Retry-After` (seconds) or a
    /// rate-limit reset time (Unix seconds), in milliseconds.
    pub fn retry_after(&self) -> Option<Millis> {
        if let Some(secs) = self
            .header("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            return Some(secs.saturating_mul(1000).max(1000));
        }
        for name in ["x-ratelimit-reset", "ratelimit-reset"] {
            if let Some(reset) = self.header(name).and_then(|v| v.trim().parse::<u64>().ok()) {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                return Some(reset.saturating_sub(now).saturating_mul(1000).max(1000));
            }
        }
        None
    }

    /// The forge said to slow down: a 429, or a 403 whose body says so.
    pub fn throttled(&self) -> bool {
        self.status == 429
            || (self.status == 403
                && (self.header("retry-after").is_some()
                    || self.header("x-ratelimit-remaining") == Some("0")
                    || self.body.to_ascii_lowercase().contains("rate limit")))
    }
}

/// Split the `--include` output into its head and body.
pub fn parse_reply(stdout: &str) -> (u16, Vec<(String, String)>, String) {
    if !stdout.starts_with("HTTP/") {
        return (0, Vec::new(), stdout.to_string());
    }
    let (head, body) = match stdout.find("\r\n\r\n") {
        Some(i) => (&stdout[..i], &stdout[i + 4..]),
        None => match stdout.find("\n\n") {
            Some(i) => (&stdout[..i], &stdout[i + 2..]),
            None => (stdout, ""),
        },
    };
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    (status, headers, body.to_string())
}

/// One bounded call. The process tree is killed past `timeout`.
pub fn call(
    program: &OsString,
    args: &[String],
    timeout: Duration,
) -> Result<Reply, (Uncertainty, String)> {
    let what = args.first().cloned().unwrap_or_default();
    let mut spec = Spec::new(program, timeout).args(args);
    // No prompts, no colour, no update nag: the output is parsed.
    spec.env.extend(
        [
            ("GH_PROMPT_DISABLED", "1"),
            ("GH_NO_UPDATE_NOTIFIER", "1"),
            ("GLAB_CHECK_UPDATE", "false"),
            ("AZURE_EXTENSION_USE_DYNAMIC_INSTALL", "no"),
            ("AZURE_CORE_ONLY_SHOW_ERRORS", "1"),
            ("NO_COLOR", "1"),
            ("NO_PROMPT", "1"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string())),
    );
    match run(&spec) {
        Outcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            let stdout = String::from_utf8_lossy(&stdout).into_owned();
            let stderr = first_line(&stderr);
            if code == Some(4) || stderr.contains("auth login") || stderr.contains("not logged in")
            {
                return Err((
                    Uncertainty::NotLoggedIn,
                    format!("{what}: the CLI has no login for this host: {stderr}"),
                ));
            }
            let (status, headers, body) = parse_reply(&stdout);
            Ok(Reply {
                status,
                headers,
                body,
                exit: code,
                stderr,
            })
        }
        Outcome::TimedOut { .. } => Err((
            Uncertainty::Timeout,
            format!("{what}: timed out; its process tree was killed"),
        )),
        Outcome::NotStarted(why) => Err((
            Uncertainty::Unavailable,
            format!("{what}: the CLI could not start: {why}"),
        )),
    }
}

fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(200)
        .collect()
}

/// The time one observation may spend over all its calls: `call_timeout`
/// times [`MAX_CALLS_PER_OBSERVATION`], and no more calls than that either.
#[derive(Debug)]
pub struct Budget {
    deadline: Instant,
    calls_left: u32,
    call_timeout: Duration,
}

impl Budget {
    pub fn new(call_timeout: Duration) -> Budget {
        Budget {
            deadline: Instant::now() + call_timeout * MAX_CALLS_PER_OBSERVATION,
            calls_left: MAX_CALLS_PER_OBSERVATION,
            call_timeout,
        }
    }

    /// The timeout of the next call, or `None` when the budget is spent.
    pub fn next_timeout(&mut self) -> Option<Duration> {
        if self.calls_left == 0 {
            return None;
        }
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        self.calls_left -= 1;
        Some(left.min(self.call_timeout))
    }
}

/// The worst case of one observation: every call at its worst.
pub fn operation_bound(call_timeout: Duration) -> Millis {
    let one = fleet_exec::worst_case(call_timeout) * MAX_CALLS_PER_OBSERVATION;
    u64::try_from(one.as_millis()).unwrap_or(u64::MAX)
}

/// An RFC 3339 timestamp (`2026-10-24T12:34:56Z`, with optional fraction and
/// offset) as Unix milliseconds. `None` for anything else.
pub fn iso_millis(s: &str) -> Option<Millis> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', 't', ' '])?;
    let mut d = date.split('-');
    let (y, m, day): (i64, u32, u32) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    if d.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let (clock, offset) = match rest.find(['Z', 'z', '+']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => match rest.rfind('-') {
            Some(i) if i > 0 => (&rest[..i], &rest[i..]),
            _ => (rest, "Z"),
        },
    };
    let (hms, frac) = clock.split_once('.').unwrap_or((clock, ""));
    let mut t = hms.split(':');
    let (h, mi, sec): (u32, u32, u32) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
    );
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let millis: i64 = if frac.is_empty() {
        0
    } else {
        let digits: String = frac.chars().take(3).collect();
        if !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        format!("{digits:0<3}").parse().ok()?
    };
    let offset_secs: i64 = match offset {
        "Z" | "z" => 0,
        o => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let (oh, om) = o[1..].split_once(':').unwrap_or((&o[1..], "0"));
            let (oh, om): (i64, i64) = (oh.parse().ok()?, om.parse().ok()?);
            sign * (oh * 3600 + om * 60)
        }
    };
    let days = days_from_civil(y, m, day);
    let secs =
        days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(sec) - offset_secs;
    u64::try_from(secs * 1000 + millis).ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Percent-encode a project path for a URL segment (`group/sub` as
/// `group%2Fsub`), leaving unreserved characters alone.
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `scheme://host/<project>/(pull|pulls|merge_requests|changes)/<id>`, with
/// an optional `/-` before the word, as GitLab writes it. Each adapter then
/// checks the shape its forge uses.
pub fn parse_change_url(url: &str) -> Option<(String, String, String)> {
    let rest = url.trim().split_once("://")?.1;
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = rest.split_once('/')?;
    let host = host.trim().to_ascii_lowercase();
    let path = path.trim_matches('/');
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let id = segments.pop()?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let word = segments.pop()?;
    if !matches!(word, "pull" | "pulls" | "merge_requests" | "changes") {
        return None;
    }
    if segments.last() == Some(&"-") {
        segments.pop();
    }
    if segments.is_empty() || host.is_empty() {
        return None;
    }
    Some((host, segments.join("/"), id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_change_url_shape_is_read() {
        for url in [
            "https://forge.test/acme/widgets/pull/7",
            "https://forge.test/acme/widgets/pulls/7/",
            "https://forge.test/acme/widgets/-/merge_requests/7#note_1",
            "https://forge.test/acme/widgets/changes/7?tab=files",
        ] {
            assert_eq!(
                parse_change_url(url),
                Some(("forge.test".into(), "acme/widgets".into(), "7".into())),
                "{url}"
            );
        }
        assert_eq!(parse_change_url("https://forge.test/acme/widgets"), None);
        assert_eq!(parse_change_url("https://forge.test/pull/7"), None);
    }

    #[test]
    fn timestamps_become_unix_milliseconds() {
        assert_eq!(iso_millis("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_millis("2026-10-24T12:00:00Z"), Some(1_792_843_200_000));
        assert_eq!(
            iso_millis("2026-10-24T12:00:00.250Z"),
            Some(1_792_843_200_250)
        );
        assert_eq!(
            iso_millis("2026-10-24T14:00:00+02:00"),
            Some(1_792_843_200_000)
        );
        assert_eq!(
            iso_millis("2026-10-24T10:00:00-02:00"),
            Some(1_792_843_200_000)
        );
        assert_eq!(iso_millis("not a time"), None);
    }

    #[test]
    fn an_included_head_is_split_from_the_body() {
        let (status, headers, body) = parse_reply(
            "HTTP/2.0 403 Forbidden\r\nRetry-After: 30\r\nX-Other: a:b\r\n\r\n{\"message\":\"slow down\"}",
        );
        assert_eq!(status, 403);
        assert_eq!(headers[0], ("retry-after".to_string(), "30".to_string()));
        assert_eq!(headers[1].1, "a:b");
        assert_eq!(body, "{\"message\":\"slow down\"}");
        let (status, _, body) = parse_reply("{\"data\":1}");
        assert_eq!((status, body.as_str()), (0, "{\"data\":1}"));
    }

    #[test]
    fn retry_after_prefers_the_explicit_header() {
        let r = Reply {
            status: 429,
            headers: vec![("retry-after".into(), "45".into())],
            ..Reply::default()
        };
        assert_eq!(r.retry_after(), Some(45_000));
        assert!(r.throttled());
    }

    #[test]
    fn segments_are_percent_encoded() {
        assert_eq!(encode_segment("group/sub.project"), "group%2Fsub.project");
    }
}
