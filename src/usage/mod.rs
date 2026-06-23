//! Account-level usage / rate-limit fetching, per agent.
//!
//! Mirrors what an agent's `/usage` (Claude) or `/status` (Codex) command
//! shows — the rolling rate-limit windows (e.g. 5-hour and weekly) with their
//! used-percentage and reset time. This data is **account-global**, fetched
//! directly from the vendor using the user's existing local credentials, the
//! same way Claude Code and Orca obtain it. Agent-neutral at the type level
//! ([`crate::session::AgentUsage`] is a plain list of named windows); the
//! per-agent fetch logic lives here.
//!
//! No HTTP client dependency: authenticated requests shell out to `curl`
//! (config piped over stdin so the bearer token never appears in argv), and
//! Codex is queried via its `app-server` JSON-RPC subprocess. Everything is
//! best-effort — any missing credential, absent CLI, or network error yields
//! an [`AgentUsage`] with a human `note` and no windows.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::session::{AgentUsage, UsageWindow};

/// Agents we know how to fetch usage for. Others are skipped entirely (no
/// process spawned, no panel section shown).
const SUPPORTED: &[&str] = &["claude", "codex", "antigravity"];

/// Whether [`fetch`] can return real usage for this agent name.
pub fn is_supported(agent: &str) -> bool {
    SUPPORTED.contains(&agent)
}

/// Fetch account usage for `agent`. Best-effort; never errors — unavailability
/// is reported via [`AgentUsage::note`] with no windows.
pub async fn fetch(agent: &str) -> AgentUsage {
    match agent {
        "claude" => claude().await,
        "codex" => codex().await,
        "antigravity" => antigravity().await,
        other => AgentUsage {
            note: Some(format!("usage not supported for '{other}'")),
            ..Default::default()
        },
    }
}

// ─────────────────────────── shared helpers ───────────────────────────

/// Run `curl` with a config supplied on stdin (keeps tokens out of argv) and
/// parse stdout as JSON. Returns `None` on spawn/timeout/non-JSON.
async fn curl_json(config: String) -> Option<serde_json::Value> {
    let mut child = tokio::process::Command::new("curl")
        .arg("-sS")
        .arg("--config")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(config.as_bytes()).await;
        let _ = stdin.shutdown().await; // close so curl proceeds
    }
    let out = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Read a JSON file (used for vendor credential files).
fn read_json_file(path: &PathBuf) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Parse an RFC 3339 timestamp to Unix epoch seconds.
fn rfc3339_epoch(s: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp().max(0) as u64)
}

/// Clamp a vendor "percent used" value to 0–100.
fn clamp_pct(v: f64) -> f32 {
    v.clamp(0.0, 100.0) as f32
}

// ─────────────────────────────── Claude ───────────────────────────────

const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLAUDE_OAUTH_BETA: &str = "oauth-2025-04-20";
const CLAUDE_USER_AGENT: &str = "claude-code/2.1.0";

/// Locate Claude's credentials file (`$CLAUDE_CONFIG_DIR/.credentials.json`,
/// else `~/.claude/.credentials.json`).
fn claude_credentials_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        let p = PathBuf::from(dir).join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }
    let p = crate::paths::home_dir()?
        .join(".claude")
        .join(".credentials.json");
    p.exists().then_some(p)
}

/// Read the subscription OAuth access token and plan tier from disk.
fn claude_token() -> Option<(String, Option<String>)> {
    let v = read_json_file(&claude_credentials_path()?)?;
    let token = v
        .pointer("/claudeAiOauth/accessToken")?
        .as_str()?
        .to_string();
    // Reject anything that could break the curl config syntax.
    if token.contains('"') || token.contains('\n') {
        return None;
    }
    let plan = v
        .pointer("/claudeAiOauth/subscriptionType")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    Some((token, plan))
}

async fn claude() -> AgentUsage {
    let Some((token, plan)) = claude_token() else {
        return AgentUsage {
            note: Some("not logged in (no subscription token)".into()),
            ..Default::default()
        };
    };
    let config = format!(
        "url = \"{CLAUDE_USAGE_URL}\"\n\
         header = \"Authorization: Bearer {token}\"\n\
         header = \"anthropic-beta: {CLAUDE_OAUTH_BETA}\"\n\
         header = \"User-Agent: {CLAUDE_USER_AGENT}\"\n\
         max-time = 15\n"
    );
    match curl_json(config).await {
        Some(v) => parse_claude_usage(&v, plan),
        None => AgentUsage {
            plan,
            note: Some("usage unavailable (API error or API-key account)".into()),
            ..Default::default()
        },
    }
}

/// Pure parser for the `oauth/usage` response:
/// `{ "five_hour": {utilization, resets_at}, "seven_day": {...} }`.
/// `utilization` is a percent (0–100), matching what `/usage` renders.
fn parse_claude_usage(v: &serde_json::Value, plan: Option<String>) -> AgentUsage {
    let window = |key: &str, label: &str| -> Option<UsageWindow> {
        let w = v.get(key)?;
        let util = w.get("utilization").and_then(|x| x.as_f64())?;
        let resets_at = w
            .get("resets_at")
            .and_then(|x| x.as_str())
            .and_then(rfc3339_epoch);
        Some(UsageWindow {
            label: label.to_string(),
            used_percent: clamp_pct(util),
            resets_at,
        })
    };
    let windows: Vec<UsageWindow> = [window("five_hour", "5h"), window("seven_day", "Week")]
        .into_iter()
        .flatten()
        .collect();
    let note = windows.is_empty().then(|| "no rate-limit data".to_string());
    AgentUsage {
        windows,
        plan,
        note,
    }
}

// ─────────────────────────────── Codex ────────────────────────────────

/// Query Codex's `app-server` over JSON-RPC for rate limits. Best-effort:
/// requires a logged-in `codex` CLI on PATH. Newline-delimited JSON-RPC with a
/// hard timeout; any framing/auth issue simply yields a note.
async fn codex() -> AgentUsage {
    match tokio::time::timeout(Duration::from_secs(15), codex_app_server()).await {
        Ok(Some(v)) => parse_codex_ratelimits(&v),
        _ => AgentUsage {
            note: Some("usage unavailable (codex not logged in?)".into()),
            ..Default::default()
        },
    }
}

async fn codex_app_server() -> Option<serde_json::Value> {
    let mut child = tokio::process::Command::new("codex")
        .args(["-s", "read-only", "-a", "untrusted", "app-server"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;

    // initialize → initialized → account/rateLimits/read (newline-delimited).
    let msgs = [
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"thurbox\",\"version\":\"0\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"initialized\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"account/rateLimits/read\",\"params\":{}}\n",
    ];
    for m in msgs {
        if stdin.write_all(m.as_bytes()).await.is_err() {
            break;
        }
    }
    let _ = stdin.flush().await;

    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(stdout).lines();
    let mut result = None;
    while let Ok(Some(line)) = reader.next_line().await {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            if v.get("id").and_then(|x| x.as_i64()) == Some(2) {
                result = v.get("result").cloned();
                break;
            }
        }
    }
    let _ = child.kill().await;
    result
}

/// Parse `{ "rateLimits": { "primary": {usedPercent, resetsAt}, "secondary": {...} } }`.
/// `resetsAt` is Unix seconds.
fn parse_codex_ratelimits(v: &serde_json::Value) -> AgentUsage {
    let root = v.pointer("/rateLimits").unwrap_or(v);
    let window = |key: &str, label: &str| -> Option<UsageWindow> {
        let w = root.get(key)?;
        let used = w.get("usedPercent").and_then(|x| x.as_f64())?;
        let resets_at = w
            .get("resetsAt")
            .and_then(|x| x.as_u64())
            .or_else(|| w.get("resetsAt").and_then(|x| x.as_f64()).map(|f| f as u64));
        Some(UsageWindow {
            label: label.to_string(),
            used_percent: clamp_pct(used),
            resets_at,
        })
    };
    let windows: Vec<UsageWindow> = [window("primary", "5h"), window("secondary", "Week")]
        .into_iter()
        .flatten()
        .collect();
    let note = windows.is_empty().then(|| "no rate-limit data".to_string());
    AgentUsage {
        windows,
        plan: None,
        note,
    }
}

// ──────────────────────────── Antigravity ─────────────────────────────
// antigravity (the `agy` CLI) is the Gemini CLI successor and shares the same
// Google OAuth creds (~/.gemini/oauth_creds.json) + Code Assist quota backend.

const ANTIGRAVITY_QUOTA_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";

fn antigravity_token() -> Option<String> {
    let v = read_json_file(
        &crate::paths::home_dir()?
            .join(".gemini")
            .join("oauth_creds.json"),
    )?;
    let token = v.get("access_token").and_then(|x| x.as_str())?.to_string();
    (!token.contains('"') && !token.contains('\n')).then_some(token)
}

async fn antigravity() -> AgentUsage {
    let Some(token) = antigravity_token() else {
        return AgentUsage {
            note: Some("not logged in (no Google OAuth creds)".into()),
            ..Default::default()
        };
    };
    let config = format!(
        "url = \"{ANTIGRAVITY_QUOTA_URL}\"\n\
         request = \"POST\"\n\
         header = \"Authorization: Bearer {token}\"\n\
         header = \"Content-Type: application/json\"\n\
         data = \"{{}}\"\n\
         max-time = 15\n"
    );
    match curl_json(config).await {
        Some(v) => parse_antigravity_quota(&v),
        None => AgentUsage {
            note: Some("usage unavailable (API error)".into()),
            ..Default::default()
        },
    }
}

/// Parse `{ "buckets": [ {modelId, remainingFraction, resetTime}, ... ] }`.
/// Surfaces the most-constrained bucket (lowest remaining) as a window.
fn parse_antigravity_quota(v: &serde_json::Value) -> AgentUsage {
    let buckets = v.get("buckets").and_then(|b| b.as_array());
    let mut best: Option<UsageWindow> = None;
    if let Some(buckets) = buckets {
        for b in buckets {
            let Some(remaining) = b.get("remainingFraction").and_then(|x| x.as_f64()) else {
                continue;
            };
            let used = clamp_pct((1.0 - remaining) * 100.0);
            let label = b
                .get("modelId")
                .and_then(|x| x.as_str())
                .unwrap_or("Quota")
                .to_string();
            let resets_at = b
                .get("resetTime")
                .and_then(|x| x.as_str())
                .and_then(rfc3339_epoch);
            let candidate = UsageWindow {
                label,
                used_percent: used,
                resets_at,
            };
            if best
                .as_ref()
                .map(|w| candidate.used_percent > w.used_percent)
                .unwrap_or(true)
            {
                best = Some(candidate);
            }
        }
    }
    match best {
        Some(w) => AgentUsage {
            windows: vec![w],
            ..Default::default()
        },
        None => AgentUsage {
            note: Some("no quota data".into()),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_set() {
        assert!(is_supported("claude"));
        assert!(is_supported("codex"));
        assert!(is_supported("antigravity"));
        assert!(!is_supported("aider"));
        assert!(!is_supported("shell"));
    }

    #[test]
    fn claude_usage_parses_windows_and_resets() {
        let json = serde_json::json!({
            "five_hour": { "utilization": 42.0, "resets_at": "2026-06-01T15:00:00Z" },
            "seven_day": { "utilization": 18.5, "resets_at": "2026-06-05T00:00:00Z" }
        });
        let u = parse_claude_usage(&json, Some("max".into()));
        assert_eq!(u.plan.as_deref(), Some("max"));
        assert_eq!(u.windows.len(), 2);
        assert_eq!(u.windows[0].label, "5h");
        assert!((u.windows[0].used_percent - 42.0).abs() < 0.01);
        assert!(u.windows[0].resets_at.is_some());
        assert_eq!(u.windows[1].label, "Week");
        assert!((u.windows[1].used_percent - 18.5).abs() < 0.01);
        assert!(u.note.is_none());
    }

    #[test]
    fn claude_usage_clamps_and_handles_missing() {
        let json = serde_json::json!({ "five_hour": { "utilization": 150.0 } });
        let u = parse_claude_usage(&json, None);
        assert_eq!(u.windows.len(), 1);
        assert!((u.windows[0].used_percent - 100.0).abs() < 0.01);
        assert!(u.windows[0].resets_at.is_none());
    }

    #[test]
    fn claude_usage_empty_gives_note() {
        let u = parse_claude_usage(&serde_json::json!({}), None);
        assert!(u.windows.is_empty());
        assert!(u.note.is_some());
    }

    #[test]
    fn codex_ratelimits_parses() {
        let json = serde_json::json!({
            "rateLimits": {
                "primary": { "usedPercent": 30, "resetsAt": 1_780_000_000u64 },
                "secondary": { "usedPercent": 12 }
            }
        });
        let u = parse_codex_ratelimits(&json);
        assert_eq!(u.windows.len(), 2);
        assert_eq!(u.windows[0].label, "5h");
        assert!((u.windows[0].used_percent - 30.0).abs() < 0.01);
        assert_eq!(u.windows[0].resets_at, Some(1_780_000_000));
        assert_eq!(u.windows[1].resets_at, None);
    }

    #[test]
    fn antigravity_quota_picks_most_constrained() {
        let json = serde_json::json!({
            "buckets": [
                { "modelId": "gemini-pro", "remainingFraction": 0.9, "resetTime": "2026-06-02T00:00:00Z" },
                { "modelId": "gemini-flash", "remainingFraction": 0.25 }
            ]
        });
        let u = parse_antigravity_quota(&json);
        assert_eq!(u.windows.len(), 1);
        assert_eq!(u.windows[0].label, "gemini-flash");
        assert!((u.windows[0].used_percent - 75.0).abs() < 0.01);
    }

    #[test]
    fn antigravity_quota_empty_gives_note() {
        let u = parse_antigravity_quota(&serde_json::json!({ "buckets": [] }));
        assert!(u.windows.is_empty());
        assert!(u.note.is_some());
    }
}
