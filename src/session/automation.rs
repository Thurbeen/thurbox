//! Automations — named, schedulable agent runs.
//!
//! An automation fires on a schedule (one-shot or recurring cron) and, when it
//! fires, either pastes a prompt into an existing session (`Send`) or spawns a
//! new session — optionally on a fresh git worktree — and prompts it (`Spawn`).
//!
//! This module is pure data + schedule math (no local crate imports), matching
//! the architecture rule for `session`. Persistence lives in
//! `storage::automations`; dispatch lives in the `app` tick loop.

use std::path::PathBuf;
use std::str::FromStr;

use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};

use super::SessionId;

/// An additional repository attached to a multi-repo `Spawn`, beyond the
/// primary `repo_path`.
///
/// Each extra repo either gets its **own isolated worktree** — on the session's
/// shared `worktree_branch`, off its own `base_branch` — so its changes stay
/// isolated and PR-able per repo (the flow default), or is attached **as-is** as
/// an additional directory in the multi-repo symlink workspace (no new branch).
/// The whole list is persisted as JSON in the `action_extra_repos` column, so an
/// empty list (the common single-repo case) stores `NULL` and is byte-identical
/// to the pre-multi-repo behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraRepo {
    /// Absolute path to the repository (worktree mode) or directory (dir mode).
    pub repo_path: PathBuf,
    /// `true` = create a worktree on the session's shared branch off
    /// `base_branch`; `false` = attach the directory as-is (no branch).
    pub worktree: bool,
    /// Base branch for the worktree when `worktree` is `true`. `None` falls back
    /// to the primary repo's base (`base_branch` on the `Spawn`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
}

/// A persisted automation definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Automation {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub schedule: AutomationSchedule,
    /// IANA timezone name (e.g. `"Europe/Zurich"`). `None` = system local time.
    pub timezone: Option<String>,
    pub action: AutomationAction,
    /// Text sent to the target session when the automation fires.
    pub prompt: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub last_run_at: Option<u64>,
    /// Next fire time (unix millis). `None` once a one-shot has fired or a
    /// schedule can no longer produce an occurrence.
    pub next_run_at: Option<u64>,
}

/// When an automation fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomationSchedule {
    /// Fire exactly once at the given unix-millisecond timestamp.
    Once { at: u64 },
    /// Fire on a recurring cron schedule. Stored in standard 5-field form
    /// (`min hour dom month dow`); 6-field (with leading seconds) is also
    /// accepted for power users.
    Cron { expr: String },
}

impl AutomationSchedule {
    /// Storage discriminant (`schedule_kind` column).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Once { .. } => "once",
            Self::Cron { .. } => "cron",
        }
    }

    /// Storage payload (`schedule_spec` column).
    pub fn spec(&self) -> String {
        match self {
            Self::Once { at } => at.to_string(),
            Self::Cron { expr } => expr.clone(),
        }
    }

    /// Reconstruct from the stored `(schedule_kind, schedule_spec)` columns.
    pub fn from_parts(kind: &str, spec: &str) -> Option<Self> {
        match kind {
            "once" => spec.parse().ok().map(|at| Self::Once { at }),
            "cron" => Some(Self::Cron {
                expr: spec.to_string(),
            }),
            _ => None,
        }
    }

    /// Compute the next fire time strictly after `now_millis`, evaluated in the
    /// given IANA `timezone` (or system local when `None`). Returns `None` when
    /// the schedule has no future occurrence (a past one-shot, or an
    /// unparsable cron expression).
    pub fn next_after(&self, now_millis: u64, timezone: Option<&str>) -> Option<u64> {
        match self {
            Self::Once { at } => (*at > now_millis).then_some(*at),
            Self::Cron { expr } => {
                let schedule = cron::Schedule::from_str(&normalize_cron(expr)).ok()?;
                let now_utc = Utc.timestamp_millis_opt(now_millis as i64).single()?;
                let next_millis = match timezone.and_then(|tz| chrono_tz::Tz::from_str(tz).ok()) {
                    Some(tz) => schedule
                        .after(&now_utc.with_timezone(&tz))
                        .next()?
                        .timestamp_millis(),
                    None => schedule
                        .after(&now_utc.with_timezone(&chrono::Local))
                        .next()?
                        .timestamp_millis(),
                };
                u64::try_from(next_millis).ok()
            }
        }
    }
}

/// What an automation does when it fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomationAction {
    /// Paste the prompt into an existing running session.
    Send { session_id: SessionId },
    /// Spawn a new session (optionally on a fresh worktree) and prompt it.
    ///
    /// A single-repo spawn leaves `extra_repos` empty (the common case). When it
    /// is non-empty the session spans multiple repos: the primary plus each
    /// extra, launched in a per-session symlink workspace. See [`ExtraRepo`].
    Spawn {
        repo_path: PathBuf,
        /// `None` = run in the repo root; `Some` = create/use a worktree branch.
        worktree_branch: Option<String>,
        /// Base branch for a new worktree (default `main`).
        base_branch: Option<String>,
        /// Agent name; `None` = registry default.
        agent: Option<String>,
        /// Additional repositories spanned by this session (empty = single-repo).
        extra_repos: Vec<ExtraRepo>,
    },
}

impl AutomationAction {
    /// Storage discriminant (`action_kind` column).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Send { .. } => "send",
            Self::Spawn { .. } => "spawn",
        }
    }
}

/// Outcome of a single automation fire, kept for history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationRunStatus {
    Success,
    Error,
    Skipped,
}

impl AutomationRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }

    /// Parse a status stored in the database, defaulting unknown values to
    /// `Error`.
    pub fn from_db(s: &str) -> Self {
        match s {
            "success" => Self::Success,
            "skipped" => Self::Skipped,
            _ => Self::Error,
        }
    }
}

/// A recorded automation fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationRun {
    pub id: i64,
    pub automation_id: i64,
    pub started_at: u64,
    pub status: AutomationRunStatus,
    /// Free-text detail: error message, skip reason, human summary.
    pub detail: String,
    /// Session this run sent to / spawned, when one exists. `None` on
    /// pre-v28 rows (the TUI falls back to parsing `detail` for those).
    pub related_session_id: Option<super::SessionId>,
}

/// A scheduling preset offered in the CLI/TUI. Each compiles to a cron string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulePreset {
    Hourly,
    Daily,
    Weekdays,
    Weekly,
}

impl SchedulePreset {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "hourly" => Some(Self::Hourly),
            "daily" => Some(Self::Daily),
            "weekdays" => Some(Self::Weekdays),
            "weekly" => Some(Self::Weekly),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hourly => "hourly",
            Self::Daily => "daily",
            Self::Weekdays => "weekdays",
            Self::Weekly => "weekly",
        }
    }
}

/// Compile a preset + `HH:MM` time into a standard 5-field cron expression.
/// `hour`/`minute` come from the time (defaults 0:0). `weekly` fires on the
/// given `dow` (0 = Sunday), defaulting to Monday.
pub fn preset_to_cron(preset: SchedulePreset, hour: u32, minute: u32, dow: u32) -> String {
    match preset {
        SchedulePreset::Hourly => format!("{minute} * * * *"),
        SchedulePreset::Daily => format!("{minute} {hour} * * *"),
        SchedulePreset::Weekdays => format!("{minute} {hour} * * 1-5"),
        SchedulePreset::Weekly => format!("{minute} {hour} * * {dow}"),
    }
}

/// Parse a `--trigger`/editor trigger value into a schedule.
///
/// Accepts: `cron:"<expr>"`, `at:<unix_millis>`, or a preset name
/// (`hourly`/`daily`/`weekdays`/`weekly`) combined with an optional `HH:MM`
/// `time` and `weekday` (0 = Sun, for `weekly`; default Monday).
pub fn parse_trigger(
    trigger: &str,
    time: Option<&str>,
    weekday: Option<u32>,
) -> Result<AutomationSchedule, String> {
    let trigger = trigger.trim();
    if let Some(expr) = trigger.strip_prefix("cron:") {
        let expr = expr.trim().trim_matches('"').to_string();
        if expr.is_empty() {
            return Err("cron expression must not be empty".into());
        }
        return Ok(AutomationSchedule::Cron { expr });
    }
    if let Some(at) = trigger.strip_prefix("at:") {
        let at: u64 = at
            .trim()
            .parse()
            .map_err(|_| format!("invalid `at:` timestamp: {at}"))?;
        return Ok(AutomationSchedule::Once { at });
    }
    let preset = SchedulePreset::parse(trigger).ok_or_else(|| {
        format!("unknown trigger `{trigger}` (use hourly|daily|weekdays|weekly|cron:…|at:…)")
    })?;
    let (hour, minute) = match time {
        Some(t) => parse_hhmm(t).ok_or_else(|| format!("invalid time `{t}` (expected HH:MM)"))?,
        None => (0, 0),
    };
    let dow = weekday.unwrap_or(1).min(6);
    Ok(AutomationSchedule::Cron {
        expr: preset_to_cron(preset, hour, minute, dow),
    })
}

/// Parse a relative duration like `30m`, `2h`, `1h30m`, `45s`, or `1d` into
/// milliseconds. Units: `s`, `m`, `h`, `d`. Whitespace between tokens is
/// allowed. Returns `None` if empty, malformed, or a number lacks a unit.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    let mut any = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
        } else if !ch.is_whitespace() {
            total = total.checked_add(duration_token_ms(&num, ch)?)?;
            num.clear();
            any = true;
        }
    }
    // A trailing number with no unit is invalid.
    if !num.is_empty() {
        return None;
    }
    any.then_some(total)
}

/// Convert one `<digits><unit>` token into milliseconds. `digits` is the number
/// accumulated so far (must be non-empty) and `unit` the unit char (`s`/`m`/
/// `h`/`d`, case-insensitive). Returns `None` on a missing number, unknown
/// unit, or arithmetic overflow.
fn duration_token_ms(digits: &str, unit: char) -> Option<u64> {
    if digits.is_empty() {
        return None; // unit with no preceding number
    }
    let n: u64 = digits.parse().ok()?;
    let unit_ms: u64 = match unit.to_ascii_lowercase() {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return None,
    };
    n.checked_mul(unit_ms)
}

/// Parse an `HH:MM` string into `(hour, minute)`. Returns `None` if malformed
/// or out of range.
pub fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.split_once(':')?;
    let hour: u32 = h.parse().ok()?;
    let minute: u32 = m.parse().ok()?;
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// Normalize a standard Unix cron expression to the form the `cron` crate
/// expects:
///
/// 1. A 5-field expression gets a `0` seconds field prepended (the crate is
///    seconds-leading: `sec min hour dom month dow`).
/// 2. The day-of-week field is translated from Unix numbering (0–6, 0 = Sunday,
///    7 = Sunday) to the crate's numbering (1–7, 1 = Sunday). Named days and
///    `*` pass through untouched.
fn normalize_cron(expr: &str) -> String {
    let mut fields: Vec<String> = expr.split_whitespace().map(String::from).collect();
    if fields.len() == 5 {
        fields.insert(0, "0".to_string());
    }
    // Day-of-week is the sixth field (index 5) in the seconds-leading form.
    if let Some(dow) = fields.get_mut(5) {
        *dow = remap_dow_numbers(dow);
    }
    fields.join(" ")
}

/// Map every integer in a day-of-week field from Unix numbering (0–6/7,
/// 0 = Sunday) to the `cron` crate's numbering (1–7, 1 = Sunday): `n -> n % 7 + 1`.
/// Separators (`*`, `-`, `,`, `/`) and named days pass through unchanged.
fn remap_dow_numbers(field: &str) -> String {
    let mut out = String::new();
    let mut digits = String::new();
    let flush = |digits: &mut String, out: &mut String| {
        if !digits.is_empty() {
            if let Ok(n) = digits.parse::<u32>() {
                out.push_str(&(n % 7 + 1).to_string());
            } else {
                out.push_str(digits);
            }
            digits.clear();
        }
    };
    for ch in field.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            flush(&mut digits, &mut out);
            out.push(ch);
        }
    }
    flush(&mut digits, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2024-01-01 00:00:00 UTC (a Monday) in millis.
    const MON_2024: u64 = 1_704_067_200_000;

    #[test]
    fn once_in_future_returns_at() {
        let s = AutomationSchedule::Once { at: 1000 };
        assert_eq!(s.next_after(500, None), Some(1000));
    }

    #[test]
    fn once_in_past_returns_none() {
        let s = AutomationSchedule::Once { at: 500 };
        assert_eq!(s.next_after(1000, None), None);
    }

    #[test]
    fn cron_daily_next_is_after_now_in_utc() {
        // 09:00 every day.
        let s = AutomationSchedule::Cron {
            expr: "0 9 * * *".to_string(),
        };
        let next = s.next_after(MON_2024, Some("UTC")).unwrap();
        // Same day 09:00 UTC = base + 9h.
        assert_eq!(next, MON_2024 + 9 * 3_600_000);
    }

    #[test]
    fn cron_weekdays_skips_to_weekday() {
        // Saturday 2024-01-06 00:00 UTC.
        let sat = MON_2024 + 5 * 86_400_000;
        let s = AutomationSchedule::Cron {
            expr: "0 9 * * 1-5".to_string(),
        };
        let next = s.next_after(sat, Some("UTC")).unwrap();
        // Next weekday fire = Monday 2024-01-08 09:00 UTC.
        let expected = MON_2024 + 7 * 86_400_000 + 9 * 3_600_000;
        assert_eq!(next, expected);
    }

    #[test]
    fn cron_invalid_returns_none() {
        let s = AutomationSchedule::Cron {
            expr: "not a cron".to_string(),
        };
        assert_eq!(s.next_after(MON_2024, Some("UTC")), None);
    }

    #[test]
    fn schedule_round_trips_through_parts() {
        for s in [
            AutomationSchedule::Once { at: 42 },
            AutomationSchedule::Cron {
                expr: "0 9 * * 1-5".to_string(),
            },
        ] {
            let back = AutomationSchedule::from_parts(s.kind(), &s.spec()).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn preset_compiles_to_expected_cron() {
        assert_eq!(
            preset_to_cron(SchedulePreset::Hourly, 9, 30, 1),
            "30 * * * *"
        );
        assert_eq!(preset_to_cron(SchedulePreset::Daily, 9, 0, 1), "0 9 * * *");
        assert_eq!(
            preset_to_cron(SchedulePreset::Weekdays, 9, 0, 1),
            "0 9 * * 1-5"
        );
        assert_eq!(preset_to_cron(SchedulePreset::Weekly, 9, 0, 0), "0 9 * * 0");
    }

    #[test]
    fn parse_duration_valid_and_invalid() {
        assert_eq!(parse_duration("30m"), Some(1_800_000));
        assert_eq!(parse_duration("2h"), Some(7_200_000));
        assert_eq!(parse_duration("1h30m"), Some(5_400_000));
        assert_eq!(parse_duration("1d"), Some(86_400_000));
        assert_eq!(parse_duration("45s"), Some(45_000));
        assert_eq!(parse_duration("1h 30m"), Some(5_400_000));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("30"), None); // no unit
        assert_eq!(parse_duration("m"), None); // no number
        assert_eq!(parse_duration("30x"), None); // bad unit
    }

    #[test]
    fn parse_hhmm_valid_and_invalid() {
        assert_eq!(parse_hhmm("09:30"), Some((9, 30)));
        assert_eq!(parse_hhmm("23:59"), Some((23, 59)));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("9"), None);
        assert_eq!(parse_hhmm("bad"), None);
    }

    #[test]
    fn normalize_cron_prepends_seconds_and_remaps_dow() {
        // 5-field: prepend seconds, remap dow 1-5 (Mon-Fri Unix) -> 2-6 (crate).
        assert_eq!(normalize_cron("0 9 * * 1-5"), "0 0 9 * * 2-6");
        // 6-field: only remap dow.
        assert_eq!(normalize_cron("0 0 9 * * 1-5"), "0 0 9 * * 2-6");
        // Sunday 0 -> 1; star untouched.
        assert_eq!(normalize_cron("0 9 * * 0"), "0 0 9 * * 1");
        assert_eq!(normalize_cron("30 * * * *"), "0 30 * * * *");
    }
}
