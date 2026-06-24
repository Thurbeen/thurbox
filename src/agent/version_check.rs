//! Update check: is a newer thurbox release available?
//!
//! This is the side-effect half of the **opt-in** version-deployment indicator
//! (gated behind `[features] version_check`, off by default — see
//! [`crate::session::settings::FeatureFlags`]). It surfaces two ways:
//!
//! - **TUI** — a small "⬆ vX.Y.Z available" badge in the header. The draw loop
//!   only ever reads a cached result ([`read_cached_status`]); the network
//!   refresh ([`refresh_cache`]) runs off the render path.
//! - **CLI** — `thurbox-cli version --check` fetches fresh and reports.
//!
//! The pure decision ([`decide_update`]) is built on the existing
//! [`compare_versions`] / [`is_dev_version`] helpers, so dev builds
//! (`0.0.0-dev`) never report an update. The GitHub fetch shells out to
//! `curl`/`wget` (no new crate dependency — same dependency-free approach as
//! `scripts/install.sh` and the extension installer).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::session::extension_def::{compare_versions, is_dev_version};

/// GitHub "latest release" endpoint for the thurbox repo (same repo + API as
/// `scripts/install.sh`).
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/Thurbeen/thurbox/releases/latest";

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

/// On-disk cache of the last successful check.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedCheck {
    /// Latest release tag observed (normalized, no leading `v`).
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
    let cached = read_cache()?;
    decide_update(current_version(), &cached.latest)
}

/// Parse the latest release tag out of the GitHub `releases/latest` JSON. Kept
/// separate from the fetch so it can be unit-tested against a fixed body. Uses
/// the same field (`tag_name`) the install script scrapes.
fn parse_tag_name(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let tag = value.get("tag_name")?.as_str()?.trim();
    if tag.is_empty() {
        None
    } else {
        Some(tag.trim_start_matches('v').to_string())
    }
}

/// Fetch the latest release tag from GitHub (network). Shells out to
/// `curl`/`wget`; returns the normalized version (no leading `v`).
pub fn fetch_latest_release() -> Result<String, String> {
    let body = crate::agent::extension_config::http_get(LATEST_RELEASE_URL)?;
    parse_tag_name(&body)
        .ok_or_else(|| "could not parse latest release tag from GitHub response".to_string())
}

/// Fetch the latest release over the network and refresh the on-disk cache.
/// Returns the fetched latest version plus the update decision (so callers can
/// report "up to date" with the actual latest version, not just `None`). The
/// cache is updated regardless, so the TUI badge reflects the newest release.
pub fn refresh_cache() -> Result<(String, Option<UpdateStatus>), String> {
    let latest = fetch_latest_release()?;
    write_cache(&latest);
    let status = decide_update(current_version(), &latest);
    Ok((latest, status))
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

    #[test]
    fn parses_tag_name_from_release_json() {
        let body = r#"{"tag_name":"v0.120.0","name":"v0.120.0","draft":false}"#;
        assert_eq!(parse_tag_name(body).as_deref(), Some("0.120.0"));
    }

    #[test]
    fn parse_tag_name_handles_missing_or_blank() {
        assert!(parse_tag_name(r#"{"message":"Not Found"}"#).is_none());
        assert!(parse_tag_name(r#"{"tag_name":""}"#).is_none());
        assert!(parse_tag_name("not json").is_none());
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
