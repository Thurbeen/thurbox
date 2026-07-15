//! Account-level usage / rate-limit fetching, per agent **and per host**.
//!
//! Mirrors what an agent's `/usage` (Claude) or `/status` (Codex) command
//! shows — the rolling rate-limit windows (e.g. 5-hour and weekly) with their
//! used-percentage and reset time. This data is **account-global**, but the
//! *account* is determined by the credentials on the machine the agent runs
//! on: a session on `ssh:devbox` may be logged into a different Claude account
//! than a local session. So every fetch is scoped to a host (`None` = local):
//! credentials are read where the agent process lives — locally, over the SSH
//! launcher, or inside the WSL distro — and the vendor API is then called
//! *locally* with that token (the token, not the network path, selects the
//! account; this also avoids requiring `curl` on the host). Codex has no
//! separable credential file/API pair, so its `app-server` subprocess runs on
//! the host itself over the same launcher (stdio JSON-RPC is transport-neutral).
//!
//! Agent-neutral at the type level ([`crate::session::AgentUsage`] is a plain
//! list of named windows); the per-agent fetch logic lives here. Everything is
//! best-effort — any missing credential, absent CLI, unreachable host, or
//! network error yields an [`AgentUsage`] with a human `note` (naming the host
//! when off-local) and no windows.
//!
//! Known limitation: a *remote* `CLAUDE_CONFIG_DIR` override is honored on
//! POSIX hosts (the read script lets the host shell expand it) but not on a
//! Windows (psmux) host, and per-session env overrides baked into a wrapper
//! script are invisible here.
//!
//! No HTTP client dependency: authenticated requests shell out to `curl`
//! (config piped over stdin so the bearer token never appears in argv).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::session::{AgentUsage, HostDef, UsageWindow};
use crate::shell::{posix_quote, ssh_command, wsl_command};

/// Agents we know how to fetch usage for. Others are skipped entirely (no
/// process spawned, no panel section shown).
const SUPPORTED: &[&str] = &["claude", "codex", "antigravity"];

/// Whether [`fetch`] can return real usage for this agent name.
pub fn is_supported(agent: &str) -> bool {
    SUPPORTED.contains(&agent)
}

/// Fetch account usage for `agent` as seen from `host` (`None` = the local
/// machine). Best-effort; never errors — unavailability is reported via
/// [`AgentUsage::note`] with no windows.
pub async fn fetch(agent: &str, host: Option<&HostDef>) -> AgentUsage {
    match agent {
        "claude" => claude(host).await,
        "codex" => codex(host).await,
        "antigravity" => antigravity(host).await,
        other => AgentUsage {
            note: Some(format!("usage not supported for '{other}'")),
            ..Default::default()
        },
    }
}

// ─────────────────────────── shared helpers ───────────────────────────

/// `" on <host>"` suffix for notes, empty for local fetches.
fn at_host(host: Option<&HostDef>) -> String {
    host.map(|h| format!(" on {}", h.name)).unwrap_or_default()
}

/// Command that reads a small credentials file on `host`. Pure builder
/// (returns a std [`std::process::Command`]) so quoting is unit-testable.
///
/// - POSIX hosts (SSH + tmux, or a WSL distro) run `sh -c <posix_script>`,
///   letting the *host* shell expand `$HOME` / `$CLAUDE_CONFIG_DIR`. Quoting
///   mirrors `git::host_shell_c`: ssh space-joins its trailing args into one
///   string the remote login shell re-splits (so the script is POSIX-quoted),
///   while `wsl.exe --exec` passes argv verbatim (so it must NOT be quoted).
/// - A psmux host is native Windows with no `sh`. `type <rel-path>` works
///   under both default OpenSSH shells (a cmd builtin; a PowerShell alias for
///   `Get-Content`) and resolves relative to `%USERPROFILE%`, the OpenSSH
///   default cwd — no env expansion needed, so no quoting hazards.
fn remote_read_command(
    host: &HostDef,
    posix_script: &str,
    windows_args: &[&str],
) -> std::process::Command {
    if host.is_wsl() {
        let mut c = wsl_command(&host.distro_name());
        c.arg("-e").arg("sh").arg("-c").arg(posix_script);
        c
    } else if host.mux() == "psmux" {
        let mut c = ssh_command(&host.destination, &host.ssh_opts);
        c.args(windows_args);
        c
    } else {
        let mut c = ssh_command(&host.destination, &host.ssh_opts);
        c.arg(posix_quote("sh"))
            .arg(posix_quote("-c"))
            .arg(posix_quote(posix_script));
        c
    }
}

/// Run a [`remote_read_command`] and return its stdout as text. `None` on
/// spawn failure, timeout, or non-zero exit (host down, file missing…).
async fn remote_read_text(
    host: &HostDef,
    posix_script: &str,
    windows_args: &[&str],
) -> Option<String> {
    let mut cmd =
        tokio::process::Command::from(remote_read_command(host, posix_script, windows_args));
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let out = tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (!text.trim().is_empty()).then_some(text)
}

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

/// Reject tokens that could break the curl-config syntax (quote/newline
/// injection from a tampered credentials file).
fn token_is_clean(token: &str) -> bool {
    !token.contains('"') && !token.contains('\n') && !token.contains('\r')
}

// ─────────────────────────────── Claude ───────────────────────────────

const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLAUDE_OAUTH_BETA: &str = "oauth-2025-04-20";
const CLAUDE_USER_AGENT: &str = "claude-code/2.1.0";

/// POSIX read of Claude's credentials, honoring a host-side
/// `CLAUDE_CONFIG_DIR` and falling back to the macOS Keychain (where Claude
/// Code stores the same JSON blob when the file is absent).
const CLAUDE_POSIX_READ: &str = "cat \"${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json\" \
     2>/dev/null || security find-generic-password -s 'Claude Code-credentials' -w 2>/dev/null";

/// Windows (psmux host) read — see [`remote_read_command`] for why `type`.
const CLAUDE_WINDOWS_READ: &[&str] = &["type", ".claude\\.credentials.json"];

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

/// Extract the subscription OAuth access token and plan tier from the
/// credentials JSON text (same shape on disk and in the macOS Keychain).
fn parse_claude_credentials(text: &str) -> Option<(String, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let token = v
        .pointer("/claudeAiOauth/accessToken")?
        .as_str()?
        .to_string();
    if !token_is_clean(&token) {
        return None;
    }
    let plan = v
        .pointer("/claudeAiOauth/subscriptionType")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    Some((token, plan))
}

/// Read Claude's local credentials: the on-disk file, else the macOS Keychain
/// (where Claude Code stores them when no file exists). The keychain attempt
/// is harmless off-macOS — `security` isn't on PATH, so spawn fails fast.
async fn claude_local_credentials() -> Option<String> {
    if let Some(path) = claude_credentials_path() {
        return std::fs::read_to_string(path).ok();
    }
    let out = tokio::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    (out.status.success()).then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn claude(host: Option<&HostDef>) -> AgentUsage {
    let creds = match host {
        None => claude_local_credentials().await,
        Some(h) => remote_read_text(h, CLAUDE_POSIX_READ, CLAUDE_WINDOWS_READ).await,
    };
    let Some((token, plan)) = creds.as_deref().and_then(parse_claude_credentials) else {
        return AgentUsage {
            note: Some(format!(
                "not logged in{} (no subscription token)",
                at_host(host)
            )),
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

/// Codex's `app-server` argv. Every token is whitespace-free, so it survives
/// ssh's space-join and `wsl.exe`'s token forwarding without quoting.
const CODEX_SERVER_ARGS: &[&str] = &["-s", "read-only", "-a", "untrusted", "app-server"];

/// Build the `codex app-server` command, locally or on `host` (the subprocess
/// runs *on the host* — its stdio JSON-RPC pipes straight through ssh/wsl, and
/// its credentials are wherever that codex is logged in). Pure for testing.
fn codex_server_command(host: Option<&HostDef>) -> std::process::Command {
    match host {
        None => {
            let mut c = std::process::Command::new("codex");
            c.args(CODEX_SERVER_ARGS);
            c
        }
        Some(h) if h.is_wsl() => {
            let mut c = wsl_command(&h.distro_name());
            c.arg("codex").args(CODEX_SERVER_ARGS);
            c
        }
        // Plain tokens work for a POSIX *and* a Windows (psmux) SSH host: the
        // remote shell resolves `codex`/`codex.exe` from PATH either way.
        Some(h) => {
            let mut c = ssh_command(&h.destination, &h.ssh_opts);
            c.arg("codex").args(CODEX_SERVER_ARGS);
            c
        }
    }
}

/// Query Codex's `app-server` over JSON-RPC for rate limits. Best-effort:
/// requires a logged-in `codex` CLI on the target's PATH. Newline-delimited
/// JSON-RPC with a hard timeout; any framing/auth issue simply yields a note.
async fn codex(host: Option<&HostDef>) -> AgentUsage {
    match tokio::time::timeout(Duration::from_secs(15), codex_app_server(host)).await {
        Ok(Some(v)) => parse_codex_ratelimits(&v),
        _ => AgentUsage {
            note: Some(format!(
                "usage unavailable (codex not logged in{}?)",
                at_host(host)
            )),
            ..Default::default()
        },
    }
}

async fn codex_app_server(host: Option<&HostDef>) -> Option<serde_json::Value> {
    let mut child = tokio::process::Command::from(codex_server_command(host))
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

const ANTIGRAVITY_POSIX_READ: &str = "cat \"$HOME/.gemini/oauth_creds.json\" 2>/dev/null";
const ANTIGRAVITY_WINDOWS_READ: &[&str] = &["type", ".gemini\\oauth_creds.json"];

/// Extract the Google OAuth access token from the credentials JSON text.
fn parse_antigravity_credentials(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let token = v.get("access_token").and_then(|x| x.as_str())?.to_string();
    token_is_clean(&token).then_some(token)
}

async fn antigravity(host: Option<&HostDef>) -> AgentUsage {
    let creds = match host {
        None => crate::paths::home_dir()
            .map(|h| h.join(".gemini").join("oauth_creds.json"))
            .and_then(|p| std::fs::read_to_string(p).ok()),
        Some(h) => remote_read_text(h, ANTIGRAVITY_POSIX_READ, ANTIGRAVITY_WINDOWS_READ).await,
    };
    let Some(token) = creds.as_deref().and_then(parse_antigravity_credentials) else {
        return not_logged_in_google(host);
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

fn not_logged_in_google(host: Option<&HostDef>) -> AgentUsage {
    AgentUsage {
        note: Some(format!(
            "not logged in{} (no Google OAuth creds)",
            at_host(host)
        )),
        ..Default::default()
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

    fn ssh_host(name: &str) -> HostDef {
        HostDef {
            name: name.into(),
            destination: format!("me@{name}"),
            ..Default::default()
        }
    }

    fn psmux_host(name: &str) -> HostDef {
        HostDef {
            multiplexer: Some("psmux".into()),
            ..ssh_host(name)
        }
    }

    fn args_of(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn supported_set() {
        assert!(is_supported("claude"));
        assert!(is_supported("codex"));
        assert!(is_supported("antigravity"));
        assert!(!is_supported("aider"));
        assert!(!is_supported("shell"));
    }

    #[test]
    fn at_host_labels_remote_only() {
        assert_eq!(at_host(None), "");
        assert_eq!(at_host(Some(&ssh_host("devbox"))), " on devbox");
    }

    // ── remote credential-read command construction ──

    #[test]
    fn remote_read_ssh_posix_quotes_script() {
        let cmd = remote_read_command(&ssh_host("devbox"), "cat \"$HOME/x\"", &["type", "x"]);
        assert_eq!(cmd.get_program(), "ssh");
        let args = args_of(&cmd);
        assert_eq!(args.last().unwrap(), &posix_quote("cat \"$HOME/x\""));
        assert!(args.contains(&"me@devbox".to_string()));
        // `sh -c` framing, each token quoted for the remote login shell.
        let tail = &args[args.len() - 3..];
        assert_eq!(tail[0], "sh");
        assert_eq!(tail[1], "-c");
    }

    #[test]
    fn remote_read_wsl_passes_script_unquoted_via_exec() {
        let cmd = remote_read_command(&HostDef::wsl("Ubuntu"), "cat \"$HOME/x\"", &["type", "x"]);
        assert_eq!(cmd.get_program(), "wsl.exe");
        let args = args_of(&cmd);
        // `--exec sh -c <script>`: argv reaches the in-distro sh verbatim.
        assert!(args.contains(&"-e".to_string()));
        assert_eq!(args.last().unwrap(), "cat \"$HOME/x\"");
    }

    #[test]
    fn remote_read_psmux_uses_windows_args() {
        let cmd = remote_read_command(
            &psmux_host("winbox"),
            "cat \"$HOME/x\"",
            CLAUDE_WINDOWS_READ,
        );
        assert_eq!(cmd.get_program(), "ssh");
        let args = args_of(&cmd);
        assert_eq!(args.last().unwrap(), ".claude\\.credentials.json");
        assert_eq!(args[args.len() - 2], "type");
        assert!(!args.contains(&"sh".to_string()));
    }

    // ── codex app-server command construction ──

    #[test]
    fn codex_command_local_runs_codex_directly() {
        let cmd = codex_server_command(None);
        assert_eq!(cmd.get_program(), "codex");
        assert_eq!(args_of(&cmd), CODEX_SERVER_ARGS);
    }

    #[test]
    fn codex_command_remote_runs_on_host() {
        let cmd = codex_server_command(Some(&ssh_host("devbox")));
        assert_eq!(cmd.get_program(), "ssh");
        let args = args_of(&cmd);
        assert!(args.contains(&"codex".to_string()));
        assert_eq!(args.last().unwrap(), "app-server");

        let cmd = codex_server_command(Some(&HostDef::wsl("Ubuntu")));
        assert_eq!(cmd.get_program(), "wsl.exe");
        let args = args_of(&cmd);
        assert!(args.contains(&"codex".to_string()));
        assert_eq!(args.last().unwrap(), "app-server");
    }

    // ── credential parsing ──

    #[test]
    fn claude_credentials_parse_token_and_plan() {
        let text =
            r#"{ "claudeAiOauth": { "accessToken": "tok-123", "subscriptionType": "max" } }"#;
        let (token, plan) = parse_claude_credentials(text).unwrap();
        assert_eq!(token, "tok-123");
        assert_eq!(plan.as_deref(), Some("max"));
    }

    #[test]
    fn claude_credentials_reject_injection_and_garbage() {
        let inj = r#"{ "claudeAiOauth": { "accessToken": "a\"\nb" } }"#;
        assert!(parse_claude_credentials(inj).is_none());
        assert!(parse_claude_credentials("not json").is_none());
        assert!(parse_claude_credentials("{}").is_none());
    }

    #[test]
    fn antigravity_credentials_parse_token() {
        assert_eq!(
            parse_antigravity_credentials(r#"{ "access_token": "ya29.x" }"#).as_deref(),
            Some("ya29.x")
        );
        assert!(parse_antigravity_credentials("{}").is_none());
    }

    // ── vendor response parsing ──

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
