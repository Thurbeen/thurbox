//! An automation's schedule: a one-shot or a recurring cron trigger, and the
//! math that finds its next fire time. The automation itself is
//! `config::automations`'s.
//!
//! Pure data + schedule math (no local crate imports), matching the
//! architecture rule for `session`.

use std::str::FromStr;

use chrono::{TimeZone, Utc};

/// How long an automation's run may take when it names no deadline.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

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
    /// The schedule as a `trigger` value: `cron:<expr>` or `at:<unix_millis>`.
    ///
    /// The spelling [`parse_trigger`] reads, so an editor can hand a stored
    /// schedule back unchanged.
    pub fn trigger(&self) -> String {
        match self {
            Self::Once { at } => format!("at:{at}"),
            Self::Cron { expr } => format!("cron:{expr}"),
        }
    }

    /// Why this schedule cannot be evaluated, when it cannot.
    ///
    /// [`Self::next_after`] answers `None` both for a schedule with no future
    /// occurrence and for a cron expression that does not parse; a writer has to
    /// tell the two apart to say what is wrong.
    pub fn check(&self) -> Result<(), String> {
        match self {
            Self::Once { .. } => Ok(()),
            Self::Cron { expr } => cron::Schedule::from_str(&normalize_cron(expr))
                .map(|_| ())
                .map_err(|e| format!("invalid cron expression `{expr}`: {e}")),
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
/// `time` and `weekday` (0..=6 = Sun..Sat, or 7 = Sun, for `weekly`; default
/// Monday).
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
    // Unix cron day-of-week: 0 and 7 both mean Sunday. Accept the full 0..=7
    // range (normalize_cron remaps it for the `cron` crate) rather than
    // clamping 7 down to 6 (Saturday), which silently broke Sunday.
    let dow = match weekday {
        Some(d) if d > 7 => {
            return Err(format!(
                "invalid weekday `{d}` (use 0=Sun..6=Sat, or 7=Sun)"
            ))
        }
        Some(d) => d,
        None => 1,
    };
    Ok(AutomationSchedule::Cron {
        expr: preset_to_cron(preset, hour, minute, dow),
    })
}

/// Refuse a timezone [`AutomationSchedule::next_after`] would silently ignore.
pub fn check_timezone(timezone: &str) -> Result<(), String> {
    chrono_tz::Tz::from_str(timezone).map(|_| ()).map_err(|_| {
        format!("unknown timezone `{timezone}` (use an IANA name such as Europe/Zurich)")
    })
}

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
    fn parse_trigger_weekly_handles_sunday_both_ways() {
        // Sunday is 0 or 7 in Unix cron; neither must clamp to Saturday (6).
        for sun in [0, 7] {
            let sched = parse_trigger("weekly", Some("09:00"), Some(sun)).unwrap();
            let AutomationSchedule::Cron { expr } = sched else {
                panic!("expected cron schedule");
            };
            assert_eq!(expr, format!("0 9 * * {sun}"));
            // ...and it actually lands on a Sunday: from Saturday 2024-01-06 the
            // next fire is Sunday 2024-01-07 09:00 UTC.
            let sat = MON_2024 + 5 * 86_400_000;
            let next = AutomationSchedule::Cron { expr }
                .next_after(sat, Some("UTC"))
                .unwrap();
            assert_eq!(next, MON_2024 + 6 * 86_400_000 + 9 * 3_600_000);
        }
    }

    #[test]
    fn parse_trigger_rejects_out_of_range_weekday() {
        let err = parse_trigger("weekly", None, Some(8)).unwrap_err();
        assert!(err.contains("weekday"), "got {err}");
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
