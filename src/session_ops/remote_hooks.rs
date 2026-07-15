//! Remote provisioning of per-agent hook configs, so **every** agent — not
//! just claude — reports hooks-driven session status from a remote (SSH/WSL)
//! host.
//!
//! Locally the built-in hooks extension wires most agents through files in the
//! agent's *own* config dir (`~/.codex/hooks.json`, the opencode plugin, …).
//! Those files never travel with the launch args, so before this module a
//! remote codex/opencode/… session was silently Idle-only. At spawn time this
//! module ships the same payloads to the host — with their commands rewritten
//! to the tmux pane-option form (`builtin_hooks::rewrite_hook_signals_for_target`)
//! so the local TUI receives state over its control-mode subscription — using
//! the same safety rules as the local installer: `requires_dir` probe (skip
//! when the agent isn't installed there), deep-merge-not-clobber for shared
//! config files, managed-marker guard for standalone files, and
//! compare-before-write idempotency.
//!
//! **Best-effort by contract**: a down host, a permission error, or a
//! malformed remote file degrades to a warning (surfaced on the session as
//! `hook_wiring`) — it never fails the spawn.
//!
//! **Remote cleanup is deliberately out of scope** (thurbox never uninstalls
//! anything from a host — same policy as remote worktrees). The shipped
//! entries carry two prune markers (`thurbox-cli session signal` pre-rewrite,
//! `@thurbox_state` post-rewrite), so a future remote prune needs no schema
//! knowledge.
//!
//! Besides provisioning (the write side), this module also owns the
//! **headless status poll** (`poll_remote_hook_states`) — the read side that
//! keeps remote hook states flowing while no TUI is attached.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::session::{HostDef, SessionId};

use super::builtin_hooks;
use super::extensions::{HOOK_SIGNAL_MARKER, MANAGED_MARKER};

/// How an asset lands on the host — mirrors the local installer's
/// `config_merges` vs `external_files` split.
enum RemoteAssetKind {
    /// Deep-merge into a shared JSON config the agent (and its user) own.
    MergeJson,
    /// Write a standalone thurbox-managed file (refused if a user-owned file
    /// — one without [`MANAGED_MARKER`] — already sits there).
    WriteFile,
}

/// One agent's hook payload and where it lives on a POSIX host.
struct RemoteHookAsset {
    kind: RemoteAssetKind,
    /// Destination, `~`-anchored (expanded against the *remote* home).
    remote_path: &'static str,
    /// Agent-installed guard: skip silently when this dir is absent.
    requires_dir: &'static str,
    payload: &'static str,
}

/// The config-dir hook asset for `agent`, or `None` when the agent has no
/// remote provisioning to do — claude (hooks travel via `--settings`) and
/// aider (a literal arg) are handled by `adapt_agent_args_for_remote`
/// instead. Kept in sync with `extensions/hooks/extension.toml` (guarded by a
/// test against the embedded manifest).
fn remote_asset_for(agent: &str) -> Option<RemoteHookAsset> {
    match agent {
        "codex" => Some(RemoteHookAsset {
            kind: RemoteAssetKind::MergeJson,
            remote_path: "~/.codex/hooks.json",
            requires_dir: "~/.codex",
            payload: builtin_hooks::CODEX_HOOKS,
        }),
        "antigravity" => Some(RemoteHookAsset {
            kind: RemoteAssetKind::MergeJson,
            remote_path: "~/.gemini/settings.json",
            requires_dir: "~/.gemini",
            payload: builtin_hooks::ANTIGRAVITY_HOOKS,
        }),
        "opencode" => Some(RemoteHookAsset {
            kind: RemoteAssetKind::WriteFile,
            remote_path: "~/.config/opencode/plugin/thurbox-status.js",
            requires_dir: "~/.config/opencode",
            payload: builtin_hooks::OPENCODE_PLUGIN,
        }),
        "vibe" => Some(RemoteHookAsset {
            kind: RemoteAssetKind::WriteFile,
            remote_path: "~/.vibe/hooks.toml",
            requires_dir: "~/.vibe",
            payload: builtin_hooks::VIBE_HOOKS,
        }),
        "copilot" => Some(RemoteHookAsset {
            kind: RemoteAssetKind::WriteFile,
            remote_path: "~/.copilot/hooks/thurbox-status.json",
            requires_dir: "~/.copilot",
            payload: builtin_hooks::COPILOT_HOOKS,
        }),
        _ => None,
    }
}

/// Human-readable reason hooks-driven status will be degraded/absent for a
/// session (probe failed, user-owned file refused, copy failed, …). `None` =
/// healthy, or nothing to provision. Informational only — provisioning never
/// fails a spawn.
pub(crate) type HookDegradation = Option<String>;

/// Outcome of one uncached provisioning pass.
enum ProvisionOutcome {
    /// The payload is verified present on the host (written now, or already
    /// up to date) — cacheable for the process lifetime.
    Provisioned,
    /// The agent isn't installed on the host (guard dir absent): nothing to
    /// wire *yet*. Deliberately **not** cached — installing the agent on the
    /// host later is picked up by the next spawn, at the cost of one cheap
    /// probe per spawn.
    NotInstalled,
    /// Provisioning failed; the reason is surfaced as the session's
    /// hook-wiring degradation. Not cached (retried on the next spawn).
    Degraded(String),
}

/// Provisioning bookkeeping, keyed by `(backend_name, agent)`.
/// Process-lifetime, like `git`'s remote-home cache: `hosts.toml` is read once
/// at startup. Only [`ProvisionOutcome::Provisioned`] lands in `provisioned`,
/// so repeat spawns of the same agent on the same host skip the ssh
/// round-trips while failures and not-installed skips are re-tried.
/// `in_flight` guards the read-merge-write per key **without** holding the
/// lock across the ssh round-trips (a slow or down host must not stall an
/// unrelated host's spawn): a concurrent spawn of the same key skips
/// provisioning entirely — the first pass either succeeds, or the next spawn
/// retries.
#[derive(Default)]
struct ProvisionCache {
    provisioned: HashSet<(String, String)>,
    in_flight: HashSet<(String, String)>,
}

fn provisioned_cache() -> &'static Mutex<ProvisionCache> {
    static CACHE: OnceLock<Mutex<ProvisionCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ProvisionCache::default()))
}

/// Ensure `agent`'s hook config exists (rewritten for the host) on `host`,
/// returning the degradation reason when it can't. Called from the spawn
/// worker paths — this performs ssh round-trips, so it must never run on the
/// UI thread.
pub(crate) fn provision_agent_hooks_on_host(
    host: &HostDef,
    agent: &str,
    hooks_enabled: bool,
) -> HookDegradation {
    if !hooks_enabled {
        return None;
    }
    let asset = remote_asset_for(agent)?;
    // Windows/psmux hosts are deferred: these payloads' hook commands run
    // through `sh`, and each agent's Windows config dir / hook shell differs.
    if host.mux() == "psmux" {
        return Some(format!(
            "{agent} hooks not provisioned on psmux host '{}'",
            host.name
        ));
    }

    let key = (host.backend_name(), agent.to_string());
    {
        let Ok(mut cache) = provisioned_cache().lock() else {
            // A poisoned lock means a prior provisioning pass panicked —
            // report it rather than claiming healthy wiring.
            return Some("hook provisioning unavailable (cache lock poisoned)".to_string());
        };
        if cache.provisioned.contains(&key) {
            return None;
        }
        if !cache.in_flight.insert(key.clone()) {
            // Another spawn is mid-provision for this exact key; don't
            // interleave its read-merge-write (and don't wait out its ssh
            // round-trips either — see `ProvisionCache`).
            return None;
        }
    }
    let outcome = provision_uncached(host, &asset);
    if let Ok(mut cache) = provisioned_cache().lock() {
        cache.in_flight.remove(&key);
        if matches!(outcome, ProvisionOutcome::Provisioned) {
            cache.provisioned.insert(key);
        }
    }
    match outcome {
        ProvisionOutcome::Provisioned | ProvisionOutcome::NotInstalled => None,
        ProvisionOutcome::Degraded(reason) => {
            tracing::warn!(
                "remote hook provisioning degraded on host '{}': {reason}",
                host.name
            );
            Some(reason)
        }
    }
}

/// The uncached provisioning pass: probe → rewrite → merge/write →
/// compare-before-copy.
fn provision_uncached(host: &HostDef, asset: &RemoteHookAsset) -> ProvisionOutcome {
    use ProvisionOutcome::{Degraded, NotInstalled, Provisioned};

    match crate::git::remote_dir_exists(host, asset.requires_dir) {
        // Agent not installed on the host — nothing to wire, not a degradation
        // (the pane would have no hooks locally either).
        Ok(false) => return NotInstalled,
        Ok(true) => {}
        Err(e) => {
            return Degraded(format!(
                "cannot probe {} on host: {e:#}",
                asset.requires_dir
            ))
        }
    }

    // POSIX remotes only (psmux is deferred above), so the tmux form is fixed.
    let rewritten = builtin_hooks::rewrite_hook_signals_for_remote(asset.payload);

    let existing = match crate::git::read_remote_file(host, asset.remote_path) {
        Ok(existing) => existing,
        Err(e) => return Degraded(format!("cannot read {} on host: {e:#}", asset.remote_path)),
    };

    let to_write = match asset.kind {
        RemoteAssetKind::MergeJson => {
            match merged_remote_doc(existing.as_deref().unwrap_or(""), &rewritten) {
                Ok(Some(merged)) => merged,
                // Already up to date — record success without a write.
                Ok(None) => return Provisioned,
                Err(e) => {
                    return Degraded(format!(
                        "cannot merge into {} on host: {e}",
                        asset.remote_path
                    ))
                }
            }
        }
        RemoteAssetKind::WriteFile => match existing {
            Some(content) if content == rewritten => return Provisioned,
            // A pre-existing file without the managed marker belongs to the
            // remote user — never clobber it (same rule as the local
            // installer's `is_user_modified`).
            Some(content) if !content.contains(MANAGED_MARKER) => {
                return Degraded(format!(
                    "{} on host is user-owned (no managed marker)",
                    asset.remote_path
                ))
            }
            _ => rewritten,
        },
    };

    let dest = match crate::git::expand_remote_tilde(host, asset.remote_path) {
        Ok(dest) => dest,
        Err(e) => {
            return Degraded(format!(
                "cannot resolve {} on host: {e:#}",
                asset.remote_path
            ))
        }
    };
    match crate::git::copy_bytes_to_remote(host, to_write.as_bytes(), &dest) {
        Ok(()) => Provisioned,
        Err(e) => Degraded(format!("cannot write {dest} on host: {e:#}")),
    }
}

/// Pure merge core (unit-testable without ssh): deep-merge the rewritten
/// `payload` into the remote file's `existing` JSON (empty/blank = `{}`),
/// **prune-then-merge** so a payload upgrade replaces our old entries instead
/// of accumulating next to them. Returns the pretty-serialized doc to write,
/// or `None` when the file is already up to date. A malformed existing doc is
/// an `Err` — never clobber config we can't parse.
fn merged_remote_doc(existing: &str, payload: &str) -> Result<Option<String>, String> {
    let before: serde_json::Value = if existing.trim().is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(existing)
            .map_err(|e| format!("existing file is not valid JSON: {e}"))?
    };
    let to_merge: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| format!("payload is not valid JSON: {e}"))?;

    let mut doc = before.clone();
    // Prune both command forms: the pre-rewrite local marker (a stale entry
    // from an older thurbox that shipped the un-rewritten payload) and the
    // rewritten pane-option marker (our own previous version).
    crate::agent::json_merge::prune_marked(&mut doc, HOOK_SIGNAL_MARKER);
    crate::agent::json_merge::prune_marked(&mut doc, crate::session::REMOTE_HOOK_STATE_OPTION);
    crate::agent::json_merge::merge(&mut doc, &to_merge);

    if doc == before {
        return Ok(None);
    }
    serde_json::to_string_pretty(&doc)
        .map(Some)
        .map_err(|e| format!("serialize merged doc: {e}"))
}

/// The hook states a headless poll may write — the same allow-list the TUI's
/// drain applies (`App::drain_remote_hook_events`): the polled value is
/// remote-host-controlled free text, so it is matched, never interpolated.
const VALID_POLL_STATES: [&str; 4] = ["working", "blocked", "done", "idle"];

/// Join one host's polled `(pane_id, state)` pairs against that backend's
/// sessions, returning the `(session, state)` writes that change anything.
/// Pure — the ssh/DB plumbing lives in [`poll_remote_hook_states`].
///
/// Comparing against the **stored** state is the resurrection guard: an
/// acknowledged `done` still stores `done` (`seen_at` is a separate column),
/// so a steady-state re-report compares equal and is dropped — the headless
/// equivalent of the TUI drain's dedup against its cache.
fn remote_status_updates(
    polled: &[(String, String)],
    sessions: &[(SessionId, &str, Option<&str>)],
) -> Vec<(SessionId, String)> {
    let states: HashMap<&str, &str> = polled
        .iter()
        .map(|(pane, state)| (pane.as_str(), state.as_str()))
        .collect();
    sessions
        .iter()
        .filter_map(|(id, backend_id, stored)| {
            let state = states.get(backend_id)?;
            if !VALID_POLL_STATES.contains(state) {
                return None;
            }
            (*stored != Some(*state)).then(|| (*id, state.to_string()))
        })
        .collect()
}

/// Headless counterpart of the TUI's live status channels: poll each remote
/// host that has **live sessions in the DB** and write changed pane states
/// into the same hook columns `session signal` uses. Returns the number of
/// states written.
///
/// The subscription/poller channels live on the TUI's persistent control-mode
/// connection and die with it, so with the TUI closed a remote session's
/// status froze at its last pushed value (local sessions are immune —
/// `session signal` writes SQLite directly). Called from the headless
/// `automation tick` (the 60 s tmux heartbeat keeper): coarse latency is fine
/// there — no human is watching a dot; the consumers are `session list
/// --json` readers — while the TUI stays the sub-second channel when open.
///
/// Best-effort everywhere: an unreachable host (bounded by the ssh
/// `ConnectTimeout`) or a host with no running server is skipped for the
/// cycle. Hosts from `hosts.toml` with **no** live remote sessions are never
/// contacted, and psmux hosts stay excluded behind
/// [`crate::session::psmux_hook_rewrite_supported`] (nothing sets the pane
/// option there yet).
pub(crate) fn poll_remote_hook_states(db: &crate::storage::Database) -> usize {
    let Ok(sessions) = db.list_active_sessions() else {
        return 0;
    };
    let mut by_backend: HashMap<String, Vec<&crate::sync::SharedSession>> = HashMap::new();
    for s in &sessions {
        if crate::session::is_remote_backend(&s.backend_type) {
            by_backend
                .entry(s.backend_type.clone())
                .or_default()
                .push(s);
        }
    }
    if by_backend.is_empty() {
        return 0;
    }
    let hook_rows = db.load_hook_states().unwrap_or_default();
    let hosts = crate::agent::host_config::load_all();
    let mut written = 0;
    for (backend_name, group) in by_backend {
        let Some(host) = hosts.get_by_backend(&backend_name) else {
            continue;
        };
        if host.mux() == "psmux" && !crate::session::psmux_hook_rewrite_supported() {
            continue;
        }
        let polled = match crate::agent::tmux::list_remote_hook_states(host) {
            Ok(polled) => polled,
            Err(e) => {
                tracing::debug!("remote status poll skipped for host '{}': {e:#}", host.name);
                continue;
            }
        };
        let rows: Vec<(SessionId, &str, Option<&str>)> = group
            .iter()
            .map(|s| {
                (
                    s.id,
                    s.backend_id.as_str(),
                    hook_rows.get(&s.id).and_then(|r| r.state.as_deref()),
                )
            })
            .collect();
        for (id, state) in remote_status_updates(&polled, &rows) {
            match db.set_hook_state(id, &state) {
                Ok(()) => written += 1,
                Err(e) => tracing::debug!("remote status poll write failed for {id}: {e}"),
            }
        }
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_status_updates_writes_changes_only() {
        let a = SessionId::default();
        let b = SessionId::default();
        let polled = vec![
            ("%1".to_string(), "working".to_string()),
            ("%2".to_string(), "done".to_string()),
            ("%9".to_string(), "blocked".to_string()), // no matching session
        ];
        let sessions = [
            (a, "%1", None),         // first report → write
            (b, "%2", Some("done")), // unchanged (incl. an acknowledged done) → silent
        ];
        assert_eq!(
            remote_status_updates(&polled, &sessions),
            vec![(a, "working".to_string())]
        );
        // A change writes; a pane whose option is unset stays untouched.
        let sessions = [(a, "%1", Some("working")), (b, "%3", Some("done"))];
        let polled = vec![("%1".to_string(), "done".to_string())];
        assert_eq!(
            remote_status_updates(&polled, &sessions),
            vec![(a, "done".to_string())]
        );
    }

    #[test]
    fn remote_status_updates_allowlists_states() {
        // The polled value is remote-controlled text — anything outside the
        // states `session signal` accepts is dropped, never written.
        let a = SessionId::default();
        let polled = vec![
            ("%1".to_string(), "pwned; DROP TABLE".to_string()),
            ("%1".to_string(), "Working".to_string()), // case-sensitive
        ];
        assert!(remote_status_updates(&polled, &[(a, "%1", None)]).is_empty());
    }

    /// Every config-dir wiring in the embedded manifest must have a matching
    /// remote asset (same destination, guard dir, and payload), so the local
    /// and remote installs can never drift apart.
    #[test]
    fn remote_assets_stay_in_sync_with_embedded_manifest() {
        let def: crate::session::ExtensionDef =
            toml::from_str(builtin_hooks::MANIFEST).expect("manifest parses");

        let agent_for_path = |path: &str| -> &'static str {
            if path.contains(".codex") {
                "codex"
            } else if path.contains(".gemini") {
                "antigravity"
            } else if path.contains("opencode") {
                "opencode"
            } else if path.contains(".vibe") {
                "vibe"
            } else if path.contains(".copilot") {
                "copilot"
            } else {
                panic!("unknown config-dir wiring path in manifest: {path}")
            }
        };

        let mut covered = 0;
        for m in &def.config_merges {
            let agent = agent_for_path(&m.path);
            let asset = remote_asset_for(agent).expect("merge agent has a remote asset");
            assert!(matches!(asset.kind, RemoteAssetKind::MergeJson), "{agent}");
            assert_eq!(asset.remote_path, m.path, "{agent} destination drifted");
            assert_eq!(
                Some(asset.requires_dir),
                m.requires_dir.as_deref(),
                "{agent} requires_dir drifted"
            );
            covered += 1;
        }
        for f in &def.external_files {
            let agent = agent_for_path(&f.path);
            let asset = remote_asset_for(agent).expect("file agent has a remote asset");
            assert!(matches!(asset.kind, RemoteAssetKind::WriteFile), "{agent}");
            assert_eq!(asset.remote_path, f.path, "{agent} destination drifted");
            assert_eq!(
                Some(asset.requires_dir),
                f.requires_dir.as_deref(),
                "{agent} requires_dir drifted"
            );
            covered += 1;
        }
        // Every table entry is reachable from the manifest (no orphan assets).
        assert_eq!(covered, 5, "manifest wiring count changed — sync the table");
        // claude/aider stay arg-handled.
        assert!(remote_asset_for("claude").is_none());
        assert!(remote_asset_for("aider").is_none());
    }

    #[test]
    fn merged_doc_into_empty_writes_rewritten_payload() {
        let payload = builtin_hooks::rewrite_hook_signals_for_remote(builtin_hooks::CODEX_HOOKS);
        let merged = merged_remote_doc("", &payload)
            .expect("merges")
            .expect("writes");
        assert!(merged.contains("tmux set-option -p @thurbox_state"));
        assert!(!merged.contains("thurbox-cli"));
        // Idempotent: merging into the just-written doc is a no-op.
        assert_eq!(merged_remote_doc(&merged, &payload).unwrap(), None);
    }

    #[test]
    fn merged_doc_preserves_user_entries_and_replaces_stale_thurbox_ones() {
        // The remote file carries a user hook plus a stale *un-rewritten*
        // thurbox entry (an older thurbox shipped the local command form).
        let existing = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [{ "type": "command", "command": "echo user-hook" }] },
                    { "hooks": [{ "type": "command",
                        "command": "thurbox-cli session signal --state idle || true" }] }
                ]
            },
            "userSetting": true
        })
        .to_string();
        let payload = builtin_hooks::rewrite_hook_signals_for_remote(builtin_hooks::CODEX_HOOKS);
        let merged = merged_remote_doc(&existing, &payload)
            .expect("merges")
            .expect("writes");
        // User content survives; the stale local-form entry is replaced by the
        // rewritten one, not accumulated next to it.
        assert!(merged.contains("echo user-hook"));
        assert!(merged.contains("\"userSetting\": true"));
        assert!(!merged.contains("thurbox-cli"));
        assert!(merged.contains("tmux set-option -p @thurbox_state idle"));
    }

    #[test]
    fn merged_doc_refuses_malformed_existing() {
        let payload = builtin_hooks::rewrite_hook_signals_for_remote(builtin_hooks::CODEX_HOOKS);
        assert!(merged_remote_doc("{not json", &payload).is_err());
    }

    #[test]
    fn psmux_host_is_deferred_with_a_degradation_note() {
        let host = HostDef {
            name: "winbox".into(),
            destination: "user@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let degraded = provision_agent_hooks_on_host(&host, "codex", true);
        assert!(degraded.is_some_and(|d| d.contains("psmux")));
    }

    #[test]
    fn opted_out_and_assetless_agents_are_noops() {
        let host = HostDef {
            name: "devbox".into(),
            destination: "user@devbox".into(),
            ..Default::default()
        };
        // Hooks opted out → no-op even for a covered agent (no ssh attempted;
        // the host doesn't exist).
        assert!(provision_agent_hooks_on_host(&host, "codex", false).is_none());
        // claude/aider are arg-handled → no-op.
        assert!(provision_agent_hooks_on_host(&host, "claude", true).is_none());
    }
}
