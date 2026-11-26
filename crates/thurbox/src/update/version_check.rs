use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::agent::fetch::{compare_versions, is_dev_version, major_version};

pub const RELEASE_LINE: u64 = 3;

pub const MIGRATION_NOTES_URL: &str =
    "https://github.com/Thurbeen/thurbox/blob/main/docs/thurbox/releasing.md#v2-and-v3";

pub(crate) const RELEASES_URL: &str =
    "https://api.github.com/repos/Thurbeen/thurbox/releases?per_page=100";

const CACHE_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Releases {
    pub line: Option<String>,
    pub next_major: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedCheck {
    latest: String,
    checked_at: u64,
}

pub fn decide_update(current: &str, latest: &str) -> Option<UpdateStatus> {
    let current = current.trim();
    let latest = latest.trim().trim_start_matches('v');
    if current.is_empty() || latest.is_empty() {
        return None;
    }
    if is_dev_version(current) {
        return None;
    }
    if compare_versions(latest, current) == std::cmp::Ordering::Greater {
        Some(UpdateStatus {
            current: current.to_string(),
            latest: latest.to_string(),
        })
    } else {
        None
    }
}

pub fn current_version() -> &'static str {
    crate::agent::fetch::binary_version()
}

fn cache_path() -> Option<PathBuf> {
    crate::paths::log_directory().map(|d| d.join("version-check.json"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_cache() -> Option<CachedCheck> {
    let path = cache_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_cache(latest: &str) {
    let Some(path) = cache_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let entry = CachedCheck {
        latest: latest.trim_start_matches('v').to_string(),
        checked_at: now_secs(),
    };
    if let Ok(json) = serde_json::to_string(&entry) {
        let _ = std::fs::write(&path, json);
    }
}

pub fn cache_is_stale() -> bool {
    match read_cache() {
        Some(c) => now_secs().saturating_sub(c.checked_at) >= CACHE_TTL_SECS,
        None => true,
    }
}

pub fn read_cached_status() -> Option<UpdateStatus> {
    cached_status(current_version(), &read_cache()?.latest)
}

fn cached_status(current: &str, cached_latest: &str) -> Option<UpdateStatus> {
    if major_version(cached_latest) != Some(RELEASE_LINE) {
        return None;
    }
    decide_update(current, cached_latest)
}

fn parse_releases(body: &str) -> Option<Releases> {
    let list: Vec<serde_json::Value> = serde_json::from_str(body).ok()?;
    let mut releases = Releases::default();
    for release in &list {
        let flag = |k: &str| release.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        if flag("draft") || flag("prerelease") {
            continue;
        }
        let Some(tag) = release.get("tag_name").and_then(|v| v.as_str()) else {
            continue;
        };
        let version = tag.trim().trim_start_matches('v');
        let slot = match major_version(version) {
            Some(m) if m == RELEASE_LINE => &mut releases.line,
            Some(m) if m > RELEASE_LINE => &mut releases.next_major,
            _ => continue,
        };
        let newer = slot
            .as_deref()
            .is_none_or(|have| compare_versions(version, have) == std::cmp::Ordering::Greater);
        if newer {
            *slot = Some(version.to_string());
        }
    }
    Some(releases)
}

pub fn fetch_releases(url: &str) -> Result<Releases, String> {
    let body = crate::agent::fetch::http_get(url)?;
    parse_releases(&body).ok_or_else(|| "could not parse the GitHub release list".to_string())
}

pub fn refresh_cache() -> Result<(Releases, Option<UpdateStatus>), String> {
    let releases = fetch_releases(RELEASES_URL)?;
    let line = releases.line.clone().unwrap_or_default();
    write_cache(&line);
    let status = decide_update(current_version(), &line);
    Ok((releases, status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_release_is_reported() {
        let s = decide_update("0.113.0", "0.114.0").expect("update");
        assert_eq!(s.current, "0.113.0");
        assert_eq!(s.latest, "0.114.0");
    }

    #[test]
    fn strips_leading_v_on_latest() {
        let s = decide_update("0.113.0", "v0.114.0").expect("update");
        assert_eq!(s.latest, "0.114.0");
    }

    #[test]
    fn same_or_older_release_reports_nothing() {
        assert!(decide_update("0.114.0", "0.114.0").is_none());
        assert!(decide_update("0.114.0", "0.113.0").is_none());
    }

    #[test]
    fn dev_build_never_reports_update() {
        assert!(decide_update("0.0.0-dev", "9.9.9").is_none());
        assert!(decide_update("0.0.0", "9.9.9").is_none());
    }

    #[test]
    fn empty_inputs_report_nothing() {
        assert!(decide_update("", "1.0.0").is_none());
        assert!(decide_update("1.0.0", "").is_none());
        assert!(decide_update("1.0.0", "   ").is_none());
    }

    fn release(tag: &str) -> serde_json::Value {
        serde_json::json!({"tag_name": tag, "draft": false, "prerelease": false})
    }

    #[test]
    fn the_line_and_the_next_major_are_picked_apart() {
        let body = serde_json::json!([
            release("v4.1.0"),
            release("v3.0.2"),
            release("v4.0.0"),
            release("v3.0.1"),
            release("v2.84.0"),
        ])
        .to_string();
        let r = parse_releases(&body).unwrap();
        assert_eq!(r.line.as_deref(), Some("3.0.2"));
        assert_eq!(r.next_major.as_deref(), Some("4.1.0"));
    }

    #[test]
    fn the_newest_on_the_line_wins_whatever_the_list_order() {
        let body = serde_json::json!([release("v3.9.1"), release("v3.10.0")]).to_string();
        let r = parse_releases(&body).unwrap();
        assert_eq!(r.line.as_deref(), Some("3.10.0"));
        assert_eq!(r.next_major, None);
    }

    #[test]
    fn drafts_and_prereleases_are_not_offered() {
        let body = serde_json::json!([
            {"tag_name": "v3.99.0", "draft": true, "prerelease": false},
            {"tag_name": "v4.0.0-rc1", "draft": false, "prerelease": true},
            release("v3.0.1"),
        ])
        .to_string();
        let r = parse_releases(&body).unwrap();
        assert_eq!(r.line.as_deref(), Some("3.0.1"));
        assert_eq!(r.next_major, None);
    }

    #[test]
    fn a_body_that_is_not_a_list_is_an_error() {
        assert!(parse_releases(r#"{"message":"Not Found"}"#).is_none());
        assert!(parse_releases("not json").is_none());
        assert_eq!(parse_releases("[]"), Some(Releases::default()));
    }

    #[test]
    fn a_cache_from_before_the_line_rule_never_offers_another_major() {
        assert_eq!(cached_status("3.0.1", "4.0.0"), None);
        assert_eq!(
            cached_status("3.0.1", "3.0.2").map(|s| s.latest),
            Some("3.0.2".to_string())
        );
    }

    #[test]
    fn cache_round_trips_and_drives_status() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());

        assert!(cache_is_stale());
        assert!(read_cached_status().is_none());

        write_cache("v9.9.9");
        assert!(!cache_is_stale(), "just-written cache is fresh");
        let cached = read_cache().expect("cache present");
        assert_eq!(cached.latest, "9.9.9", "leading v stripped on write");
    }

    #[test]
    fn cache_older_than_ttl_is_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());

        let path = cache_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let entry = CachedCheck {
            latest: "9.9.9".into(),
            checked_at: 0,
        };
        std::fs::write(&path, serde_json::to_string(&entry).unwrap()).unwrap();

        assert!(cache_is_stale(), "an epoch-stamped cache is past the TTL");
    }
}
