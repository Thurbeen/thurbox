//! Remote provisioning of per-agent hook configs, so **every** agent — not
//! just claude — reports hooks-driven session status from a remote (SSH/WSL)
//! host.
//!
//! Locally the built-in hooks extension wires most agents through files in the
//! agent's *own* config dir (`~/.codex/hooks.json`, the opencode plugin, …).
//! Those files never travel with the launch args, so before this module a
//! remote codex/opencode/… session was silently Idle-only. At spawn time this
//! module ships the same payloads to the host — with their commands rewritten
//! to the command the row's backend reports state through
//! (`builtin_hooks::rewrite_hook_signals`) so the local interface reads it back
//! through that backend — using
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
//! the backend's hook command post-rewrite), so a future remote prune needs no
//! schema knowledge.
//!
//! **Accounts.** An agent whose config dir follows a variable (codex's
//! `CODEX_HOME`) names it in its asset path as `{env:CODEX_HOME:-~/.codex}`,
//! the token the manifest spells the same way. A launch resolves it against the
//! session's saved env, so an account selected by an agent's `env` (issue
//! #1526) gets its hooks in its own dir: on a host through the pass above, and
//! on this machine through `provision_account_hooks_locally`, which lands the
//! payload under the installer's own rules. Neither is ever uninstalled.
//!
//! Besides provisioning (the write side), this module also owns the
//! **headless status poll** (`poll_hook_states`) — the read side that keeps
//! every route's hook states flowing while no interface is attached.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::session::background_work::ReportFrom;
use crate::session::{HostDef, SessionId};

use super::builtin_hooks;
use super::extensions::{HOOK_SIGNAL_MARKER, MANAGED_MARKER};

/// How an asset lands on the host — mirrors the local installer's
/// `config_merges` vs `external_files` split.
enum RemoteAssetKind {
    /// Deep-merge into a shared JSON config the agent (and its user) own.
    MergeJson,
    /// The same, for an agent whose shared config is TOML (kimi).
    MergeToml,
    /// Write a standalone thurbox-managed file (refused if a user-owned file
    /// — one without [`MANAGED_MARKER`] — already sits there).
    WriteFile,
}

/// One agent's hook payload and where it lives on a POSIX host.
struct RemoteHookAsset {
    kind: RemoteAssetKind,
    /// Destination, `~`-anchored (expanded against the *remote* home), and
    /// maybe carrying an `{env:NAME:-default}` token for the session's env.
    remote_path: &'static str,
    /// Agent-installed guard: skip silently when this dir is absent.
    requires_dir: &'static str,
    payload: &'static str,
}

/// An asset's two locations with their `{env:…}` tokens read from `env`.
struct Placed<'a> {
    asset: &'a RemoteHookAsset,
    path: String,
    requires_dir: String,
}

impl RemoteHookAsset {
    fn placed(&self, env: &BTreeMap<String, String>) -> Placed<'_> {
        let resolve = |p: &str| crate::paths::resolve_env_default(p, |name| env.get(name).cloned());
        Placed {
            asset: self,
            path: resolve(self.remote_path),
            requires_dir: resolve(self.requires_dir),
        }
    }
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
            remote_path: "{env:CODEX_HOME:-~/.codex}/hooks.json",
            requires_dir: "{env:CODEX_HOME:-~/.codex}",
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
        "pi" => Some(RemoteHookAsset {
            kind: RemoteAssetKind::WriteFile,
            remote_path: "~/.pi/agent/extensions/thurbox-status.ts",
            requires_dir: "~/.pi/agent",
            payload: builtin_hooks::PI_STATUS,
        }),
        "omp" => Some(RemoteHookAsset {
            kind: RemoteAssetKind::WriteFile,
            remote_path: "~/.omp/agent/extensions/thurbox-status.ts",
            requires_dir: "~/.omp/agent",
            payload: builtin_hooks::OMP_STATUS,
        }),
        "grok" => Some(RemoteHookAsset {
            kind: RemoteAssetKind::WriteFile,
            remote_path: "~/.grok/hooks/thurbox-status.json",
            requires_dir: "~/.grok",
            payload: builtin_hooks::GROK_HOOKS,
        }),
        "kimi" => Some(RemoteHookAsset {
            kind: RemoteAssetKind::MergeToml,
            remote_path: "~/.kimi-code/config.toml",
            requires_dir: "~/.kimi-code",
            payload: builtin_hooks::KIMI_HOOKS,
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

/// Provisioning bookkeeping, keyed by [`ProvisionKey`].
/// Process-lifetime, like `git`'s remote-home cache: `hosts.toml` is read once
/// at startup. Only [`ProvisionOutcome::Provisioned`] lands in `provisioned`,
/// so repeat spawns of the same agent on the same host skip the ssh
/// round-trips while failures and not-installed skips are re-tried.
/// `provisioned` holds, per host and agent, the one hook command last shipped:
/// the file on the host carries one, so a launch on another route of the host
/// replaces what an earlier one is still remembered as having shipped.
/// `in_flight` makes the read-merge-write exclusive per host and agent — the
/// file it rewrites, whichever command a launch ships — **without** holding
/// the lock across the ssh round-trips (a slow or down host must not stall an
/// unrelated host's spawn): a concurrent spawn for the same file waits for the
/// holder (bounded), then reads the cache or retries the pass itself.
#[derive(Default)]
struct ProvisionCache {
    provisioned: HashMap<HookFile, String>,
    in_flight: HashSet<HookFile>,
}

impl ProvisionCache {
    fn has_shipped(&self, key: &ProvisionKey) -> bool {
        self.provisioned
            .get(&hook_file(key))
            .is_some_and(|shipped| *shipped == key.2)
    }

    fn shipped(&mut self, key: &ProvisionKey) {
        self.provisioned.insert(hook_file(key), key.2.clone());
    }
}

fn provisioned_cache() -> &'static Mutex<ProvisionCache> {
    static CACHE: OnceLock<Mutex<ProvisionCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ProvisionCache::default()))
}

/// Lock the cache, recovering from poison: every mutation under this lock is
/// a single `insert`/`remove`/`contains` (no multi-step invariant a panic
/// could tear), so a poisoned mutex — e.g. [`InFlightGuard`]'s own release
/// during an unwind — carries consistent data and is safe to keep using.
fn cache_lock() -> std::sync::MutexGuard<'static, ProvisionCache> {
    provisioned_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `(host, hook file path, hook command)`: what was shipped carries one
/// backend's command, so a launch on another route of the same host is not
/// done yet. The path, not the agent, because two accounts of one agent are
/// two files.
type ProvisionKey = (String, String, String);

fn provision_key(host: &HostDef, path: &str, signal: &str) -> ProvisionKey {
    (host.backend_name(), path.to_string(), signal.to_string())
}

/// `(host, hook file path)`: the one hook file a key's launch rewrites.
type HookFile = (String, String);

fn hook_file((host, path, _): &ProvisionKey) -> HookFile {
    (host.clone(), path.clone())
}

/// Removes its key from `in_flight` on drop — **including on unwind**, so a
/// panic inside the provisioning pass can never leak the key and permanently
/// (and silently) disable provisioning for that key.
struct InFlightGuard {
    key: HookFile,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        cache_lock().in_flight.remove(&self.key);
    }
}

/// How a same-key waiter polls the in-flight holder, and for how long. The
/// cap only trips if the holder is wedged past every ssh timeout — the waiter
/// then degrades with a note instead of spinning forever.
const IN_FLIGHT_WAIT_STEP: std::time::Duration = std::time::Duration::from_millis(200);
const IN_FLIGHT_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// Ensure `agent`'s hook config exists (rewritten for the host) on `host`,
/// returning the degradation reason when it can't. Called from the spawn
/// worker paths — this performs ssh round-trips, so it must never run on the
/// UI thread.
pub(crate) fn provision_agent_hooks_on_host(
    host: &HostDef,
    signal: Option<&str>,
    agent: &str,
    env: &BTreeMap<String, String>,
    hooks_enabled: bool,
) -> HookDegradation {
    if !hooks_enabled {
        return None;
    }
    let asset = remote_asset_for(agent)?;
    let placed = asset.placed(env);
    // Status is the backend's to carry. One with no channel gets no hooks:
    // they would have nothing to report through, and the session reads as
    // reporting nothing rather than as idle.
    let Some(signal) = signal else {
        return Some(format!(
            "{agent} hooks not provisioned on host '{}': its backend reports no hook status",
            host.name
        ));
    };
    // Windows hosts are deferred: these payloads' hook commands run through
    // `sh`, and each agent's Windows config dir / hook shell differs. A
    // property of the payloads and the host's shell, whatever multiplexer it
    // runs.
    if crate::shell::HostShell::for_host(host) != crate::shell::HostShell::Posix {
        return Some(format!(
            "{agent} hooks not provisioned on Windows host '{}'",
            host.name
        ));
    }

    // Keyed on the expanded file, so `~/.codex` and an account spelled
    // `/home/u/.codex` share one entry and one in-flight guard. The home is
    // already cached by the launch that resolved `{home}`.
    let dest = match crate::git::expand_remote_tilde(host, &placed.path) {
        Ok(dest) => dest,
        Err(e) => {
            return Some(format!(
                "{agent} hooks not provisioned on host '{}': cannot resolve {}: {e:#}",
                host.name, placed.path
            ))
        }
    };
    let key = provision_key(host, &dest, signal);
    // Claim the key, or wait for the concurrent spawn that holds it: skipping
    // would report this session healthy while its agent may boot before the
    // holder's file lands (or after the holder *fails*). Waiting is bounded —
    // the holder's ssh calls are ConnectTimeout/ServerAlive-bounded and the
    // guard releases the key even on panic — and runs on a spawn worker by
    // contract, never the UI thread. If the holder succeeded we return
    // healthy from the cache; if it failed, we retry the pass ourselves.
    let mut waited = std::time::Duration::ZERO;
    let _guard = loop {
        {
            let mut cache = cache_lock();
            if cache.has_shipped(&key) {
                return None;
            }
            if cache.in_flight.insert(hook_file(&key)) {
                break InFlightGuard {
                    key: hook_file(&key),
                };
            }
        }
        if waited >= IN_FLIGHT_WAIT_MAX {
            return Some(format!(
                "{agent} hook provisioning still in flight on host '{}' (concurrent spawn)",
                host.name
            ));
        }
        std::thread::sleep(IN_FLIGHT_WAIT_STEP);
        waited += IN_FLIGHT_WAIT_STEP;
    };
    let outcome = provision_uncached(host, signal, &placed, &dest);
    if matches!(outcome, ProvisionOutcome::Provisioned) {
        cache_lock().shipped(&key);
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
///
/// The guard-dir probe and the existing-file read are **one** ssh round trip
/// ([`crate::git::probe_remote_dir_and_file`]): made separately they were two
/// serial connections per `(host, agent)` before the copy even started, on the
/// spawn path.
fn provision_uncached(
    host: &HostDef,
    signal: &str,
    placed: &Placed<'_>,
    dest: &str,
) -> ProvisionOutcome {
    use ProvisionOutcome::{Degraded, NotInstalled, Provisioned};

    let asset = placed.asset;
    let probed = crate::git::probe_remote_dir_and_file(host, &placed.requires_dir, &placed.path);
    let existing = match probed {
        // Agent not installed on the host — nothing to wire, not a degradation
        // (the pane would have no hooks locally either).
        Ok(crate::git::DirFileProbe::NoDir) => return NotInstalled,
        Ok(crate::git::DirFileProbe::NoFile) => None,
        Ok(crate::git::DirFileProbe::File(content)) => Some(content),
        Ok(crate::git::DirFileProbe::NotFile) => {
            return Degraded(format!(
                "{} on host exists but is not a regular file",
                placed.path
            ))
        }
        Err(e) => {
            return Degraded(format!(
                "cannot probe {} on host: {e:#}",
                placed.requires_dir
            ))
        }
    };

    let rewritten = builtin_hooks::rewrite_hook_signals(asset.payload, signal);

    let to_write = match asset.kind {
        RemoteAssetKind::MergeJson | RemoteAssetKind::MergeToml => {
            let existing = existing.as_deref().unwrap_or("");
            let merged = match asset.kind {
                RemoteAssetKind::MergeToml => merged_remote_toml_doc(existing, &rewritten),
                _ => merged_remote_doc(existing, &rewritten, signal),
            };
            match merged {
                Ok(Some(merged)) => merged,
                // Already up to date — record success without a write.
                Ok(None) => return Provisioned,
                Err(e) => {
                    return Degraded(format!("cannot merge into {} on host: {e}", placed.path))
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
                    placed.path
                ))
            }
            _ => rewritten,
        },
    };

    match crate::git::copy_bytes_to_remote(host, to_write.as_bytes(), dest) {
        Ok(()) => Provisioned,
        Err(e) => Degraded(format!("cannot write {dest} on host: {e:#}")),
    }
}

/// Land `agent`'s hook payload in the config dir a local session's saved
/// `env` moves it to — an account (issue #1526). `None` when the env moves
/// nothing, or moves it where the installer already serves (this process's own
/// env): the extension install owns that copy, and removes it on uninstall.
/// Same best-effort contract as the remote pass: a failure is a note, never a
/// failed launch.
pub(crate) fn provision_account_hooks_locally(
    agent: &str,
    env: &BTreeMap<String, String>,
    hooks_enabled: bool,
) -> HookDegradation {
    if !hooks_enabled {
        return None;
    }
    let asset = remote_asset_for(agent)?;
    let names = crate::paths::env_token_names(asset.remote_path);
    if !names
        .iter()
        .any(|name| env.get(*name).is_some_and(|v| !v.is_empty()))
    {
        return None;
    }
    let placed = asset.placed(env);
    let dest = crate::paths::expand_tilde(&placed.path);
    let installed = crate::paths::expand_tilde(&crate::paths::resolve_env_default(
        asset.remote_path,
        |name| std::env::var(name).ok(),
    ));
    if dest == installed {
        return None;
    }
    let landing = match asset.kind {
        RemoteAssetKind::MergeJson => {
            super::extensions::Landing::Merge(crate::session::ConfigMergeFormat::Json)
        }
        RemoteAssetKind::MergeToml => {
            super::extensions::Landing::Merge(crate::session::ConfigMergeFormat::Toml)
        }
        RemoteAssetKind::WriteFile => super::extensions::Landing::File,
    };
    match super::extensions::land_payload(
        landing,
        &crate::paths::expand_tilde(&placed.requires_dir),
        &dest,
        asset.payload,
    ) {
        Ok(_) => None,
        Err(e) => {
            let reason = format!("{agent} hooks not provisioned in {}: {e}", dest.display());
            tracing::warn!("{reason}");
            Some(reason)
        }
    }
}

/// Pure merge core (unit-testable without ssh): deep-merge the rewritten
/// `payload` into the remote file's `existing` JSON (empty/blank = `{}`),
/// **prune-then-merge** so a payload upgrade replaces our old entries instead
/// of accumulating next to them. Returns the pretty-serialized doc to write,
/// or `None` when the file is already up to date. A malformed existing doc is
/// an `Err` — never clobber config we can't parse.
fn merged_remote_doc(
    existing: &str,
    payload: &str,
    signal: &str,
) -> Result<Option<String>, crate::session_ops::OpError> {
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
    // backend's hook command (our own previous version). The ownership stamp
    // catches what calls neither — codex's `SessionStart` group that prints
    // the idle-claim instruction — which would otherwise be merged in again
    // beside itself on every provision.
    crate::agent::json_merge::prune_marked(&mut doc, HOOK_SIGNAL_MARKER);
    crate::agent::json_merge::prune_marked(&mut doc, signal);
    crate::agent::json_merge::prune_marked(&mut doc, MANAGED_MARKER);
    crate::agent::json_merge::merge(&mut doc, &to_merge);

    if doc == before {
        return Ok(None);
    }
    Ok(serde_json::to_string_pretty(&doc)
        .map(Some)
        .map_err(|e| format!("serialize merged doc: {e}"))?)
}

/// [`merged_remote_doc`] for a TOML target (kimi's `~/.kimi-code/config.toml`):
/// same prune-then-merge contract, `toml_edit` so the remote user's comments and
/// key order survive.
fn merged_remote_toml_doc(
    existing: &str,
    payload: &str,
) -> Result<Option<String>, crate::session_ops::OpError> {
    let before: toml_edit::DocumentMut = if existing.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        existing
            .parse()
            .map_err(|e| format!("existing file is not valid TOML: {e}"))?
    };
    let to_merge: toml_edit::DocumentMut = payload
        .parse()
        .map_err(|e| format!("payload is not valid TOML: {e}"))?;

    let mut doc = before.clone();
    // One marker covers both command forms here, unlike the JSON sibling: the
    // ownership comment is on the entry, and the remote rewrite only touches the
    // command inside it, so a stale un-rewritten entry is recognised as ours
    // just the same.
    crate::agent::toml_merge::prune_owned(&mut doc, MANAGED_MARKER);
    crate::agent::toml_merge::merge(&mut doc, &to_merge);

    let after = doc.to_string();
    if after == before.to_string() {
        return Ok(None);
    }
    Ok(Some(after))
}

/// One row a poll may write to: its id, its pane, the state it holds and
/// whether it is parked.
type PollRow<'a> = (SessionId, &'a str, Option<&'a str>, bool);

/// Join one backend's polled `(pane_id, state)` pairs against the sessions on
/// its route, returning the `(session, state)` writes that change anything —
/// judged by [`super::signals::echo_changes`], as the interface's subscription
/// judges them. Pure — the backend/DB plumbing lives in [`poll_hook_states`].
fn hook_status_updates(
    polled: &[(String, String)],
    sessions: &[PollRow<'_>],
) -> Vec<(SessionId, String)> {
    let states: HashMap<&str, &str> = polled
        .iter()
        .map(|(pane, state)| (pane.as_str(), state.as_str()))
        .collect();
    sessions
        .iter()
        .filter_map(|&(id, backend_id, held, stopped)| {
            let state = states.get(backend_id)?;
            super::signals::echo_changes(held, stopped, state).then(|| (id, state.to_string()))
        })
        .collect()
}

/// Headless counterpart of the interface's live status channels: ask the
/// backend serving each route that has **live sessions in the DB** for its
/// panes' hook states ([`crate::backend::StatusChannel::hook_states`]), and
/// write the changed ones into the same hook columns `session signal` uses.
/// Returns the number of states written.
///
/// The live channels ride an attached interface's connections and die with
/// it, so with the interface closed a session whose hooks report through its
/// backend — every remote one, and a local one a peer created here — froze at
/// its last pushed value. Called from the headless `automation tick` (the
/// heartbeat): coarse latency is fine there — no human is watching a dot; the
/// consumers are `session list --json` readers — while the interface stays the
/// sub-second channel when open.
///
/// Rows are grouped by the route they settle to, so a pane id is only ever
/// matched against the backend that issued it: two servers both have a `%0`.
/// Best-effort everywhere, and never a guess: a route no backend here serves,
/// a backend with no status channel, and one that could not be asked are all
/// skipped for the cycle — the state held stays held, and nothing is ever
/// inferred idle. Routes with no live sessions are never asked.
pub(crate) fn poll_hook_states(
    db: &crate::storage::Database,
    backends: &crate::backend::BackendRegistry,
) -> usize {
    let Ok(sessions) = db.list_active_sessions() else {
        return 0;
    };
    let (hosts, _warnings) = crate::config::hosts::cached_registry();
    // By the server a row's route settles to, not by its spelling: a legacy
    // `ssh:box` row and a new `ssh:box:tmux` one are one listing.
    let mut by_route: HashMap<crate::session::Route, Vec<&crate::session::SharedSession>> =
        HashMap::new();
    for s in &sessions {
        match crate::session::Route::parse(&s.backend_type) {
            Ok(route) => by_route.entry(hosts.qualify(&route)).or_default().push(s),
            Err(e) => tracing::debug!("status poll skipped '{}': {e}", s.backend_type),
        }
    }
    if by_route.is_empty() {
        return 0;
    }
    let hook_rows = db.load_hook_states().unwrap_or_default();
    let stopped = db.load_stopped_sessions().unwrap_or_default();
    let mut written = 0;
    for (route, group) in by_route {
        if route.host().is_some() && hosts.host_of(&route).is_none() {
            tracing::debug!("status poll skipped for {route}: its host is not in hosts.toml");
            continue;
        }
        let Some(backend) = backends.get(&route) else {
            tracing::debug!("status poll skipped for {route}: no backend here serves it");
            continue;
        };
        let polled = match backend.hook_states() {
            Ok(polled) => polled,
            Err(e) => {
                tracing::debug!("status poll skipped for {route}: {e:#}");
                continue;
            }
        };
        let rows: Vec<PollRow<'_>> = group
            .iter()
            .map(|s| {
                (
                    s.id,
                    s.backend_id.as_str(),
                    hook_rows.get(&s.id).and_then(|r| r.state.as_deref()),
                    stopped.contains(&s.id),
                )
            })
            .collect();
        for (id, state) in hook_status_updates(&polled, &rows) {
            // An echo of what `session signal` published, in the order its
            // writers published rather than stored: `signals::report` refuses
            // a late `background` for work that has all ended (issue #1510).
            match super::signals::report(db, id, &state, ReportFrom::Echo, None) {
                Ok(reported) => {
                    written +=
                        usize::from(matches!(reported, super::signals::Reported::Stored { .. }))
                }
                Err(e) => tracing::debug!("status poll write failed for {id}: {e}"),
            }
        }
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tmux-protocol backend's hook command, as one answers it.
    const SIGNAL: &str = "tmux set-option -p @thurbox_state ";
    const OPENCODE_PATH: &str = "/home/u/.config/opencode/plugin/thurbox-status.js";

    fn no_env() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// An account moves a host's hook file, and the cache keys on the file:
    /// the default account's shipped copy says nothing about another's.
    #[test]
    fn an_accounts_hook_file_is_its_own_on_a_host() {
        let host = HostDef {
            name: "account-host".into(),
            destination: "user@account-host.invalid".into(),
            ..Default::default()
        };
        crate::git::seed_remote_home(&host, "/home/u");
        let asset = remote_asset_for("codex").unwrap();
        let work = BTreeMap::from([("CODEX_HOME".to_string(), "/home/u/.codex-work".to_string())]);
        assert_eq!(asset.placed(&work).path, "/home/u/.codex-work/hooks.json");
        assert_eq!(asset.placed(&work).requires_dir, "/home/u/.codex-work");
        assert_eq!(asset.placed(&no_env()).path, "~/.codex/hooks.json");
        cache_lock().shipped(&provision_key(&host, "/home/u/.codex/hooks.json", SIGNAL));
        assert!(
            provision_agent_hooks_on_host(&host, Some(SIGNAL), "codex", &no_env(), true).is_none()
        );
        assert!(
            provision_agent_hooks_on_host(&host, Some(SIGNAL), "codex", &work, true).is_some(),
            "the account's file was taken as provisioned because the default one was"
        );
        let same = BTreeMap::from([("CODEX_HOME".to_string(), "/home/u/.codex".to_string())]);
        assert!(
            provision_agent_hooks_on_host(&host, Some(SIGNAL), "codex", &same, true).is_none(),
            "two spellings of the default account's file were provisioned as two files"
        );
    }

    #[test]
    fn in_flight_guard_releases_on_unwind() {
        // A panic inside the provisioning pass must not leak the in-flight
        // key (which would silently disable provisioning for the process
        // lifetime while reporting healthy).
        let key = ("test-guard-backend".to_string(), "codex".to_string());
        assert!(cache_lock().in_flight.insert(key.clone()));
        let k = key.clone();
        let unwound = std::panic::catch_unwind(move || {
            let _guard = InFlightGuard { key: k };
            panic!("simulated provisioning panic");
        });
        assert!(unwound.is_err());
        assert!(
            !cache_lock().in_flight.contains(&key),
            "guard must release the key on unwind"
        );
    }

    #[test]
    fn hook_status_updates_writes_changes_only() {
        let a = SessionId::default();
        let b = SessionId::default();
        let polled = vec![
            ("%1".to_string(), "working".to_string()),
            ("%2".to_string(), "done".to_string()),
            ("%9".to_string(), "blocked".to_string()), // no matching session
        ];
        let sessions = [
            (a, "%1", None, false),         // first report → write
            (b, "%2", Some("done"), false), // unchanged (incl. an acknowledged done) → silent
        ];
        assert_eq!(
            hook_status_updates(&polled, &sessions),
            vec![(a, "working".to_string())]
        );
        // A change writes; a pane whose option is unset stays untouched.
        let sessions = [
            (a, "%1", Some("working"), false),
            (b, "%3", Some("done"), false),
        ];
        let polled = vec![("%1".to_string(), "done".to_string())];
        assert_eq!(
            hook_status_updates(&polled, &sessions),
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
        assert!(hook_status_updates(&polled, &[(a, "%1", None, false)]).is_empty());
    }

    #[test]
    fn a_parked_session_takes_no_polled_state() {
        // `session stop` killed the pane; the backend may still report it.
        let a = SessionId::default();
        let polled = vec![("%1".to_string(), "working".to_string())];
        assert!(hook_status_updates(&polled, &[(a, "%1", Some("done"), true)]).is_empty());
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
            } else if path.contains(".omp") {
                "omp"
            } else if path.contains(".pi") {
                "pi"
            } else if path.contains(".grok") {
                "grok"
            } else if path.contains(".kimi-code") {
                "kimi"
            } else {
                panic!("unknown config-dir wiring path in manifest: {path}")
            }
        };

        let mut covered = 0;
        for m in &def.config_merges {
            let agent = agent_for_path(&m.path);
            let asset = remote_asset_for(agent).expect("merge agent has a remote asset");
            let expected_toml = m.format == crate::session::ConfigMergeFormat::Toml;
            assert_eq!(
                matches!(asset.kind, RemoteAssetKind::MergeToml),
                expected_toml,
                "{agent}: local and remote merge formats disagree"
            );
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
        assert_eq!(covered, 9, "manifest wiring count changed — sync the table");
        // claude/aider stay arg-handled.
        assert!(remote_asset_for("claude").is_none());
        assert!(remote_asset_for("aider").is_none());
    }

    #[test]
    fn merged_doc_into_empty_writes_rewritten_payload() {
        let payload = builtin_hooks::rewrite_hook_signals(builtin_hooks::CODEX_HOOKS, SIGNAL);
        let merged = merged_remote_doc("", &payload, SIGNAL)
            .expect("merges")
            .expect("writes");
        assert!(merged.contains("tmux set-option -p @thurbox_state"));
        assert!(!merged.contains("thurbox-cli"));
        // Idempotent: merging into the just-written doc is a no-op.
        assert_eq!(merged_remote_doc(&merged, &payload, SIGNAL).unwrap(), None);
    }

    #[test]
    fn merged_toml_doc_preserves_the_remote_users_config_and_replaces_stale_entries() {
        let payload = builtin_hooks::rewrite_hook_signals(builtin_hooks::KIMI_HOOKS, SIGNAL);
        // The host's file carries the remote user's own settings and hook, plus
        // a *stale* thurbox entry an older thurbox shipped in the un-rewritten
        // local command form. Ownership is the comment, so that entry is
        // recognised as ours whichever command form it holds — which is why one
        // prune call replaces the two the JSON sibling needs.
        let existing = "# theirs\nmodel = \"kimi-code/k3\"\n\n\
                        [[hooks]]\nevent = \"Stop\"\n\
                        command = \"notify-send hi; thurbox-cli session signal --state done\"\n\n\
                        # managed by thurbox `extension install`\n\
                        [[hooks]]\nevent = \"Retired\"\n\
                        command = \"thurbox-cli session signal --state done || true\"\n";
        let merged = merged_remote_toml_doc(existing, &payload)
            .expect("merges")
            .expect("writes");

        // The remote user's config and their own hook survive untouched — even
        // though that hook calls `thurbox-cli session signal` itself.
        assert!(merged.contains("# theirs"));
        assert!(merged.contains("notify-send hi; thurbox-cli session signal --state done"));
        // The stale entry is gone rather than sitting beside the new one, and
        // ours now reports through the pane option.
        assert!(
            !merged.contains("Retired"),
            "stale entry replaced: {merged}"
        );
        assert!(merged.contains("tmux set-option -p @thurbox_state done"));

        // Every command we ship reports remotely; the only one still naming the
        // local CLI is the user's own.
        let doc: toml::Value = toml::from_str(&merged).expect("merged doc is valid TOML");
        for hook in doc["hooks"].as_array().expect("[[hooks]]") {
            let command = hook["command"].as_str().expect("hook has a command");
            assert!(
                !command.contains("thurbox-cli") || command.starts_with("notify-send hi"),
                "a shipped command still calls the local CLI: {command}"
            );
        }

        // Idempotent: merging into the just-written doc is a no-op.
        assert_eq!(merged_remote_toml_doc(&merged, &payload).unwrap(), None);
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
        let payload = builtin_hooks::rewrite_hook_signals(builtin_hooks::CODEX_HOOKS, SIGNAL);
        let merged = merged_remote_doc(&existing, &payload, SIGNAL)
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
        let payload = builtin_hooks::rewrite_hook_signals(builtin_hooks::CODEX_HOOKS, SIGNAL);
        assert!(merged_remote_doc("{not json", &payload, SIGNAL).is_err());
    }

    #[test]
    fn a_windows_host_is_deferred_with_a_degradation_note() {
        let host = HostDef {
            name: "winbox".into(),
            destination: "user@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let degraded = provision_agent_hooks_on_host(&host, Some(SIGNAL), "codex", &no_env(), true);
        assert!(degraded.is_some_and(|d| d.contains("Windows host")));
    }

    /// What was shipped carries one backend's hook command, so a launch on
    /// another route of the same host — another command — is not already done.
    #[test]
    fn a_cached_provisioning_counts_only_for_the_command_it_shipped() {
        let host = HostDef {
            name: "cache-key-host".into(),
            destination: "user@cache-key-host.invalid".into(),
            ..Default::default()
        };
        crate::git::seed_remote_home(&host, "/home/u");
        cache_lock().shipped(&provision_key(&host, "/home/u/.codex/hooks.json", SIGNAL));
        assert!(
            provision_agent_hooks_on_host(&host, Some(SIGNAL), "codex", &no_env(), true).is_none(),
            "the command it shipped is cached"
        );
        assert!(
            provision_agent_hooks_on_host(
                &host,
                Some("other-mux signal "),
                "codex",
                &no_env(),
                true
            )
            .is_some(),
            "another route's command was taken as already shipped"
        );
    }

    /// An agent's hook file on a host holds one command. Once a launch on
    /// another route of the host has shipped its own, the earlier one is no
    /// longer what the file says, and a launch on that route ships it again
    /// rather than boot an agent that reports through the other backend.
    #[test]
    fn a_cached_provisioning_is_forgotten_once_another_command_replaces_it() {
        let host = HostDef {
            name: "cache-replaced-host".into(),
            destination: "user@cache-replaced-host.invalid".into(),
            ..Default::default()
        };
        crate::git::seed_remote_home(&host, "/home/u");
        for signal in [SIGNAL, "other-mux signal "] {
            cache_lock().shipped(&provision_key(&host, OPENCODE_PATH, signal));
        }
        assert!(
            provision_agent_hooks_on_host(&host, Some(SIGNAL), "opencode", &no_env(), true)
                .is_some(),
            "the file now carries the other route's command, and was taken as this one's"
        );
    }

    /// Two routes' launches write the same file on the host, so one waits for
    /// the other whatever command each ships: interleaved, the cache could
    /// name the command that lost the race.
    #[test]
    fn provisioning_is_exclusive_per_hook_file_not_per_command() {
        let host = HostDef {
            name: "exclusive-file-host".into(),
            destination: "user@exclusive-file-host.invalid".into(),
            ..Default::default()
        };
        crate::git::seed_remote_home(&host, "/home/u");
        let held = hook_file(&provision_key(&host, OPENCODE_PATH, SIGNAL));
        assert!(cache_lock().in_flight.insert(held.clone()));
        let guard = InFlightGuard { key: held };
        let other = {
            let host = host.clone();
            std::thread::spawn(move || {
                provision_agent_hooks_on_host(
                    &host,
                    Some("other-mux signal "),
                    "opencode",
                    &no_env(),
                    true,
                )
            })
        };
        std::thread::sleep(IN_FLIGHT_WAIT_STEP * 3);
        assert!(
            !other.is_finished(),
            "a launch shipping another command went ahead while the file was held"
        );
        drop(guard);
        assert!(other.join().unwrap().is_some());
    }

    #[test]
    fn a_route_with_no_status_channel_is_provisioned_nothing() {
        let host = HostDef {
            name: "devbox".into(),
            destination: "user@devbox.invalid".into(),
            ..Default::default()
        };
        let degraded = provision_agent_hooks_on_host(&host, None, "codex", &no_env(), true);
        assert!(degraded.is_some_and(|d| d.contains("reports no hook status")));
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
        assert!(
            provision_agent_hooks_on_host(&host, Some(SIGNAL), "codex", &no_env(), false).is_none()
        );
        // claude/aider are arg-handled → no-op.
        assert!(
            provision_agent_hooks_on_host(&host, Some(SIGNAL), "claude", &no_env(), true).is_none()
        );
    }
}
