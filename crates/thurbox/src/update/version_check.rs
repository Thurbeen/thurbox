//! Update check: is a newer thurbox release available?
//!
//! This is the side-effect half of the version-deployment indicator (gated
//! behind `[features] version_check`, on by default — see
//! [`crate::session::settings::FeatureFlags`]). It surfaces two ways:
//!
//! - **TUI** — a small "⬆ vX.Y.Z available" badge in the header. The draw loop
//!   only ever reads a cached result ([`read_cached_status`]); the network
//!   refresh ([`refresh_cache`]) runs off the render path.
//! - **CLI** — `thurbox-cli version --check` fetches fresh and reports.
//!
//! **A binary only ever updates within its own major line** ([`RELEASE_LINE`]).
//! The check reads GitHub's release *list* and picks the newest release on the
//! line, not `releases/latest`: once 3.x is "Latest", a 2.x maintenance release
//! is published with `make_latest: false` and only the list shows it. A newer
//! major is reported as [`Releases::next_major`] and never installed — see
//! `docs/thurbox/releasing.md`, "v2 and v3".
//!
//! The pure decision ([`decide_update`]) is built on the existing
//! [`compare_versions`] / [`is_dev_version`] helpers, so dev builds
//! (`0.0.0-dev`) never report an update. The GitHub fetch shells out to
//! `curl`/`wget` (no new crate dependency — same dependency-free approach as
//! `scripts/install.sh` and the extension installer).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::session::extension_def::{compare_versions, is_dev_version, major_version};

/// The major version this source tree releases. Auto-update and the update
/// check only ever consider releases on this line.
///
/// It is a constant rather than the running version's major so a development
/// build (`0.0.0-dev`, no line of its own) still knows which line it belongs to
/// for `thurbox-cli update --force`. `scripts/ci/release-line.sh` reads it
/// and refuses to cut a release on any other major, which is what makes crossing
/// a major a source change somebody reviews rather than a commit footer.
pub const RELEASE_LINE: u64 = 2;

/// Where a user on this line reads what moving to the next major involves.
pub const MIGRATION_NOTES_URL: &str =
    "https://github.com/Thurbeen/thurbox/blob/main/docs/thurbox/releasing.md#v2-and-v3";

/// GitHub's release list for the thurbox repo, newest first. One page: a line
/// with no release among the hundred newest has nothing to offer anyway.
pub(crate) const RELEASES_URL: &str =
    "https://api.github.com/repos/Thurbeen/thurbox/releases?per_page=100";

/// How long a cached check stays fresh. The TUI reads the cache on startup and
/// only refreshes over the network once it is older than this, so a normal
/// launch never hits the network.
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// A confirmed available update: `latest` is strictly newer than `current`.
///
/// The *existence* of this value is itself the "update available" signal —
/// [`decide_update`] only ever returns `Some` when a newer release exists, so
/// there is no redundant boolean flag to keep in sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// The running binary's version (e.g. `0.113.0`).
    pub current: String,
    /// The latest published release (e.g. `0.114.0`, "v" stripped), newer than
    /// `current`.
    pub latest: String,
}

/// What the release list offers this binary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Releases {
    /// The newest published release on [`RELEASE_LINE`] (no leading `v`).
    pub line: Option<String>,
    /// The newest published release on a later major, when there is one. It is
    /// only ever reported, with [`MIGRATION_NOTES_URL`].
    pub next_major: Option<String>,
}

/// On-disk cache of the last successful check.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedCheck {
    /// Newest release on the line (normalized, no leading `v`). Builds before
    /// the line rule wrote GitHub's "Latest" here, which can be another major,
    /// so a reader checks the major rather than trusting the field.
    latest: String,
    /// Unix seconds when the check ran.
    checked_at: u64,
}

/// Decide whether `latest` is a newer release than `current`.
///
/// Returns `None` — i.e. "no badge / nothing to report" — when:
/// - `current` is a dev build (its version doesn't order against tags), or
/// - `latest` is empty / not parseable as newer, or
/// - `latest` is not strictly greater than `current`.
///
/// Otherwise returns a populated [`UpdateStatus`]. Pure: no IO, fully
/// unit-testable.
pub fn decide_update(current: &str, latest: &str) -> Option<UpdateStatus> {
    let current = current.trim();
    let latest = latest.trim().trim_start_matches('v');
    if current.is_empty() || latest.is_empty() {
        return None;
    }
    // Dev builds (0.0.0-dev) intentionally never flag an update.
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

/// The running binary's version (thin wrapper over the shared
/// [`binary_version`](crate::agent::extension_config::binary_version) helper, so
/// callers in this feature have a single import).
pub fn current_version() -> &'static str {
    crate::agent::extension_config::binary_version()
}

/// Path to the cache file: `~/.local/share/thurbox/version-check.json`.
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

/// Whether the cache is missing or older than the freshness window
/// (`CACHE_TTL_SECS`, 24 h).
pub fn cache_is_stale() -> bool {
    match read_cache() {
        Some(c) => now_secs().saturating_sub(c.checked_at) >= CACHE_TTL_SECS,
        None => true,
    }
}

/// Read the cached check and compare it against the running binary, without
/// any network IO. Returns `Some` only when a newer release is cached (so the
/// caller can render a badge); `None` otherwise. Always safe to call from the
/// TUI draw loop.
pub fn read_cached_status() -> Option<UpdateStatus> {
    cached_status(current_version(), &read_cache()?.latest)
}

/// [`read_cached_status`]'s decision over an explicit running version.
fn cached_status(current: &str, cached_latest: &str) -> Option<UpdateStatus> {
    if major_version(cached_latest) != Some(RELEASE_LINE) {
        return None;
    }
    decide_update(current, cached_latest)
}

/// Pick the newest release on the line, and the newest beyond it, out of the
/// GitHub release-list JSON. Drafts and pre-releases are not releases a user
/// is offered. Kept apart from the fetch so it is tested against fixed bodies.
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

/// Fetch the release list from `url` (network; `curl`/`wget`).
pub fn fetch_releases(url: &str) -> Result<Releases, String> {
    let body = crate::agent::extension_config::http_get(url)?;
    parse_releases(&body).ok_or_else(|| "could not parse the GitHub release list".to_string())
}

/// Fetch the release list over the network and refresh the on-disk cache.
/// Returns what the list offers plus the update decision (so callers can report
/// "up to date" with the actual newest release, not just `None`).
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
            release("v3.1.0"),
            release("v2.84.2"),
            release("v3.0.0"),
            release("v2.84.1"),
            release("v1.8.7"),
        ])
        .to_string();
        let r = parse_releases(&body).unwrap();
        assert_eq!(r.line.as_deref(), Some("2.84.2"));
        assert_eq!(r.next_major.as_deref(), Some("3.1.0"));
    }

    #[test]
    fn the_newest_on_the_line_wins_whatever_the_list_order() {
        // GitHub orders by creation date, so a 2.x hotfix of an older minor can
        // come first; the version, not the position, decides.
        let body = serde_json::json!([release("v2.9.1"), release("v2.10.0")]).to_string();
        let r = parse_releases(&body).unwrap();
        assert_eq!(r.line.as_deref(), Some("2.10.0"));
        assert_eq!(r.next_major, None);
    }

    #[test]
    fn drafts_and_prereleases_are_not_offered() {
        let body = serde_json::json!([
            {"tag_name": "v2.99.0", "draft": true, "prerelease": false},
            {"tag_name": "v3.0.0-rc1", "draft": false, "prerelease": true},
            release("v2.84.1"),
        ])
        .to_string();
        let r = parse_releases(&body).unwrap();
        assert_eq!(r.line.as_deref(), Some("2.84.1"));
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
        // Builds before this one cached GitHub's "Latest" — a 3.x tag once v3
        // ships. Reading it must not put a v3 badge on a 2.x install.
        assert_eq!(cached_status("2.84.1", "3.0.0"), None);
        assert_eq!(
            cached_status("2.84.1", "2.84.2").map(|s| s.latest),
            Some("2.84.2".to_string())
        );
    }

    #[test]
    fn cache_round_trips_and_drives_status() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());

        // No cache yet: stale, no status.
        assert!(cache_is_stale());
        assert!(read_cached_status().is_none());

        // A freshly written newer release is fresh and (for a non-dev current
        // version) drives a status. We can't override the compiled-in current
        // version, so assert the cache file itself round-trips.
        write_cache("v9.9.9");
        assert!(!cache_is_stale(), "just-written cache is fresh");
        let cached = read_cache().expect("cache present");
        assert_eq!(cached.latest, "9.9.9", "leading v stripped on write");
    }

    #[test]
    fn cache_older_than_ttl_is_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());

        // Write a cache stamped at the epoch — far older than the TTL window.
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
