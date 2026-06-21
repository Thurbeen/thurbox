//! Headless extension activation, deactivation, and self-healing.
//!
//! An extension manifest ([`ExtensionDef`]) declares the sessions/automations an
//! opt-in extension needs. These helpers make that declaration real and keep it
//! real:
//!
//! - [`ensure_extension`] idempotently (re)creates any missing declared
//!   resources — the self-heal primitive run at TUI startup and on every
//!   headless `automation tick`.
//! - [`activate_extension`] = `ensure` + record the extension in the active set
//!   (SQLite `metadata`), so self-heal will resurrect its resources if deleted.
//! - [`deactivate_extension`] tears the resources down and clears the active-set
//!   entry, so self-heal stops resurrecting it. This is the real off-switch.
//!
//! Deleting an extension's session/automation by hand (TUI `Ctrl+D`, `clean`,
//! `thurbox-cli session/automation delete`) is therefore a no-op while the
//! extension is active: the next ensure pass recreates it. `deactivate` is how a
//! user turns an extension off for good.
//!
//! This module reaches no `agent::` symbols — spawn/delete go through the
//! sibling `session_ops` helpers and everything else is `storage`/`session`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::session::automation::parse_trigger;
use crate::session::extension_def::HOME_TOKEN;
use crate::session::{AutomationAction, ExtensionAutomation, ExtensionDef, SessionId};
use crate::storage::automations::NewAutomation;
use crate::storage::Database;
use crate::sync::current_time_millis;

/// What [`ensure_extension`] actually created this pass (empty = everything was
/// already present). Lets callers toast only on a real (re)creation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnsureReport {
    /// Names of sessions newly spawned this pass.
    pub sessions_created: Vec<String>,
    /// Names of automations newly created this pass.
    pub automations_created: Vec<String>,
    /// Names of existing `Send` automations whose target session id was stale
    /// and got re-linked to the session's current id this pass.
    pub automations_relinked: Vec<String>,
}

impl EnsureReport {
    /// Whether anything was (re)created or repaired.
    pub fn created_anything(&self) -> bool {
        !self.sessions_created.is_empty()
            || !self.automations_created.is_empty()
            || !self.automations_relinked.is_empty()
    }
}

/// What [`deactivate_extension`] tore down.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeactivateReport {
    pub sessions_deleted: Vec<String>,
    pub automations_deleted: Vec<String>,
    /// Whether the extension was in the active set before this call.
    pub was_active: bool,
}

/// Per-resource presence snapshot for `extension status` / `extension list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionHealth {
    pub name: String,
    /// The extension's own one-line description (`description` in its manifest).
    pub description: Option<String>,
    pub active: bool,
    /// `(session_name, present)` for each declared session.
    pub sessions: Vec<(String, bool)>,
    /// `(automation_name, present)` for each declared automation.
    pub automations: Vec<(String, bool)>,
    /// The extension's own declared version (`version` in its manifest), if any.
    pub version: Option<String>,
    /// The thurbox version that installed it (`installed_with`), if recorded.
    pub installed_with: Option<String>,
    /// The running binary's version (the staleness reference point).
    pub current_binary: String,
    /// `true` when the binary upgraded since install — `extension update` would
    /// refresh it. Always `false` on a dev build.
    pub stale: bool,
    /// A compatibility warning when the binary is older than the extension's
    /// declared `min_thurbox_version`, else `None`.
    pub compat_warning: Option<String>,
}

impl ExtensionHealth {
    /// Healthy = active and every declared resource currently exists.
    pub fn is_healthy(&self) -> bool {
        self.active
            && self.sessions.iter().all(|(_, p)| *p)
            && self.automations.iter().all(|(_, p)| *p)
    }
}

/// What [`install_extension`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallReport {
    pub name: String,
    /// Resolved (absolute) home directory the payload landed in.
    pub home: String,
    pub files_written: Vec<String>,
    /// Files skipped because they already existed and are `if_absent`.
    pub files_skipped: Vec<String>,
    pub symlinks_created: Vec<String>,
    /// Symlinks skipped because a regular file already occupies the link path.
    pub symlinks_skipped: Vec<String>,
    /// External files written into agents' own config dirs (hook plugins).
    pub external_files_written: Vec<String>,
    /// External files skipped (`if_absent`/user-modified/`requires_dir` absent).
    pub external_files_skipped: Vec<String>,
    pub agents_added: Vec<String>,
    /// Existing agents whose `args` were extended with a hook patch.
    pub agents_patched: Vec<String>,
    /// Config files an extension JSON-merged hook entries into (e.g.
    /// `~/.gemini/settings.json`).
    pub config_merges_applied: Vec<String>,
    /// Config merges skipped because their `requires_dir` was absent (the agent
    /// isn't installed) or the merge was already present (no-op).
    pub config_merges_skipped: Vec<String>,
    /// The activate result (sessions/automations created).
    pub ensure: EnsureReport,
    /// The newly-installed extension's declared `version` (if any).
    pub version: Option<String>,
    /// The version that was installed before this run (for `update` to report a
    /// `0.9.0 → 1.0.0` move). `None` on a first install.
    pub previous_version: Option<String>,
    /// A compatibility warning if the running binary is older than the
    /// extension's declared `min_thurbox_version`.
    pub compat_warning: Option<String>,
}

/// Install an extension end-to-end from a `target` (a bare name resolved against
/// the official source, a `http(s)://` base, or a local directory): fetch the
/// manifest, lay down its payload files + symlinks under the home dir, register
/// its agents in `agents.toml`, write the (home-resolved) manifest to the
/// discovery dir, then activate it (ensure session/automation + self-heal).
///
/// Idempotent: re-running refreshes payload files (except `if_absent` ones,
/// unless `force`), adds only missing agents, and reuses existing
/// sessions/automations.
pub fn install_extension(
    db: &Database,
    target: &str,
    home_override: Option<&str>,
    force: bool,
) -> Result<InstallReport, String> {
    // Agent-layer helpers are reached fully-qualified (no `use crate::agent`) per
    // the session_ops → agent path-only architecture rule.
    let source = crate::agent::extension_config::resolve_source(target);
    let (def, warnings) = load_manifest_for_install(target, &source)?;
    for w in &warnings {
        tracing::warn!("{w}");
    }

    // Record the previously-installed version (if any) before we overwrite the
    // discovery manifest, so an install-over-existing / update can report a move.
    let previous_version =
        crate::agent::extension_config::load_manifest(&def.name).and_then(|prev| prev.version);

    let home_raw = home_override
        .map(str::to_string)
        .or_else(|| def.home.clone())
        .ok_or_else(|| {
            format!(
                "extension '{}' has no `home` in its manifest; pass --home <dir>",
                def.name
            )
        })?;
    let home = crate::agent::extension_config::expand_tilde(&home_raw);
    let home_str = home.to_string_lossy().to_string();

    let current = crate::agent::extension_config::binary_version();
    let mut report = InstallReport {
        name: def.name.clone(),
        home: home_str.clone(),
        version: def.version.clone(),
        previous_version,
        compat_warning: def.compat_warning(current),
        ..Default::default()
    };
    if let Some(w) = &report.compat_warning {
        tracing::warn!("{w}");
    }

    // 1. Payload files.
    for f in &def.files {
        install_payload_file(&source, f, &home, &home_str, force, &mut report)?;
    }

    // 2. Symlinks (never clobber a regular file the user owns).
    for s in &def.symlinks {
        install_symlink(s, &home, &mut report)?;
    }

    // 3. Agents → agents.toml (idempotent).
    report.agents_added = crate::agent::extension_config::ensure_agents_registered(&def.agents)?;

    // 3b. External files (hook plugins) into agents' own config dirs. These take
    //     absolute / `~` paths, so `{home}` is resolved first.
    let resolved_externals: Vec<crate::session::ExternalFile> = def
        .external_files
        .iter()
        .map(|f| {
            let mut f = f.clone();
            f.path = f.path.replace(HOME_TOKEN, &home_str);
            if let Some(req) = &f.requires_dir {
                f.requires_dir = Some(req.replace(HOME_TOKEN, &home_str));
            }
            f
        })
        .collect();
    for f in &resolved_externals {
        install_external_file(&source, f, &home_str, force, &mut report)?;
    }

    // 3c. Hook-arg patches into existing agents (reversible).
    let resolved_patches: Vec<crate::session::AgentPatch> = def
        .agent_patches
        .iter()
        .map(|p| {
            let mut p = p.clone();
            for a in &mut p.append_args {
                *a = a.replace(HOME_TOKEN, &home_str);
            }
            p
        })
        .collect();
    report.agents_patched = crate::agent::extension_config::apply_agent_patches(&resolved_patches)?;

    // 3d. JSON merges into agents' own config files (reversible, non-clobbering).
    let resolved_merges: Vec<crate::session::ConfigMerge> = def
        .config_merges
        .iter()
        .map(|m| {
            let mut m = m.clone();
            m.path = m.path.replace(HOME_TOKEN, &home_str);
            if let Some(req) = &m.requires_dir {
                m.requires_dir = Some(req.replace(HOME_TOKEN, &home_str));
            }
            m
        })
        .collect();
    for m in &resolved_merges {
        install_config_merge(&source, m, &mut report)?;
    }

    // 4. Persist the home-resolved manifest (stamped with install provenance —
    //    which binary installed it + where from) to the discovery dir, then
    //    activate. `target` is recorded verbatim so `update` re-fetches the same
    //    source (a bare name re-resolves against the *current* binary's tag).
    let resolved = def
        .resolved_for_home(&home_str)
        .with_provenance(current, target);
    crate::agent::extension_config::write_manifest(&resolved)?;
    report.ensure = activate_extension(db, &resolved)?;

    Ok(report)
}

/// Fetch and parse an extension manifest for [`install_extension`], turning a
/// failed bare-name fetch (almost always a typo or an unknown extension) into
/// discovery guidance.
fn load_manifest_for_install(
    target: &str,
    source: &crate::agent::extension_config::ExtensionSource,
) -> Result<(ExtensionDef, Vec<String>), String> {
    match crate::agent::extension_config::load_manifest_from_source(source) {
        Ok(v) => Ok(v),
        Err(e) if crate::agent::extension_config::is_bare_name(target) => Err(
            crate::agent::extension_config::unknown_extension_help(target, &e),
        ),
        Err(e) => Err(e),
    }
}

/// Lay down one payload file under the home dir, honouring `if_absent` /
/// `substitute` skip rules and the path-traversal guard, recording the outcome
/// in `report`.
fn install_payload_file(
    source: &crate::agent::extension_config::ExtensionSource,
    f: &crate::session::extension_def::ExtensionFile,
    home: &Path,
    home_str: &str,
    force: bool,
    report: &mut InstallReport,
) -> Result<(), String> {
    // Reject absolute / `..` destinations and sources so a manifest can't
    // write or read outside the home / source dir (path-traversal guard).
    let dest = safe_join(home, &f.path)?;
    ensure_safe_relative(f.source_path())?;
    if f.if_absent && dest.exists() && !force {
        report.files_skipped.push(f.path.clone());
        return Ok(());
    }
    // Don't clobber a `substitute` file (e.g. .claude/settings.json) the
    // user has edited: we only overwrite ours, identified by the installer
    // marker we write into it. `--force` overrides.
    if f.substitute && !force && is_user_modified(&dest) {
        report.files_skipped.push(f.path.clone());
        return Ok(());
    }
    let mut content = crate::agent::extension_config::fetch_file(source, f.source_path())?;
    if f.substitute {
        content = content.replace(HOME_TOKEN, home_str);
    }
    // Skip the write when nothing changed: `ensure` re-runs this on every TUI
    // startup and every 60 s heartbeat tick, so a no-op rewrite each time is
    // wasted disk churn.
    if file_has_content(&dest, &content) {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&dest, content).map_err(|e| format!("write {}: {e}", dest.display()))?;
    if f.executable {
        set_executable(&dest)?;
    }
    report.files_written.push(f.path.clone());
    Ok(())
}

/// Whether `dest` already holds exactly `content` (so a rewrite would be a no-op).
fn file_has_content(dest: &Path, content: &str) -> bool {
    std::fs::read_to_string(dest).is_ok_and(|existing| existing == content)
}

/// Write one external file into an agent's own config dir (absolute / `~`
/// path). Unlike [`install_payload_file`] this deliberately escapes the home
/// dir, so it never uses the relative-path guard. Skips when `requires_dir` is
/// absent (the agent isn't installed), when `if_absent` and the file exists, or
/// when a user has edited our managed file (no marker) — unless `force`.
fn install_external_file(
    source: &crate::agent::extension_config::ExtensionSource,
    f: &crate::session::ExternalFile,
    home_str: &str,
    force: bool,
    report: &mut InstallReport,
) -> Result<(), String> {
    if let Some(req) = &f.requires_dir {
        if !crate::agent::extension_config::expand_tilde(req).is_dir() {
            report.external_files_skipped.push(f.path.clone());
            return Ok(());
        }
    }
    let dest = crate::agent::extension_config::expand_tilde(&f.path);
    if f.if_absent && dest.exists() && !force {
        report.external_files_skipped.push(f.path.clone());
        return Ok(());
    }
    // Never clobber a file a user has edited (one lacking our managed marker).
    if !force && dest.exists() && is_user_modified(&dest) {
        report.external_files_skipped.push(f.path.clone());
        return Ok(());
    }
    let mut content = crate::agent::extension_config::fetch_file(source, f.source_path())?;
    if f.substitute {
        content = content.replace(HOME_TOKEN, home_str);
    }
    // Skip the write when unchanged (re-run every startup + heartbeat tick).
    if file_has_content(&dest, &content) {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&dest, content).map_err(|e| format!("write {}: {e}", dest.display()))?;
    if f.executable {
        set_executable(&dest)?;
    }
    report.external_files_written.push(f.path.clone());
    Ok(())
}

/// Marker present in every hook command we ship (`thurbox-cli session signal
/// …`). [`crate::agent::json_merge::prune_marked`] uses it to remove exactly our
/// merged entries on uninstall — robust across payload schema changes.
const HOOK_SIGNAL_MARKER: &str = "thurbox-cli session signal";

/// Read the JSON config at `path` (or `{}` when absent), parsed. A malformed
/// file is an error rather than a silent overwrite — we never clobber config we
/// can't safely round-trip.
fn read_json_or_empty(path: &Path) -> Result<serde_json::Value, String> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(serde_json::json!({})),
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("parse {}: {e}", path.display())),
        Err(_) => Ok(serde_json::json!({})),
    }
}

/// Write `value` as pretty JSON to `path` only when it differs from the current
/// contents (the merge runs every startup + heartbeat tick, so a no-op write
/// would be churn). Returns whether it wrote.
fn write_json_if_changed(path: &Path, value: &serde_json::Value) -> Result<bool, String> {
    let content = serde_json::to_string_pretty(value)
        .map_err(|e| format!("serialize {}: {e}", path.display()))?;
    if file_has_content(path, &content) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(true)
}

/// Deep-merge an extension's shipped JSON into an agent's *own* config file
/// (`~/.gemini/settings.json`, …) in place — reversibly and without clobbering
/// the user's other settings. Skips when `requires_dir` is absent (the agent
/// isn't installed) or when the merge is already present (no-op write).
fn install_config_merge(
    source: &crate::agent::extension_config::ExtensionSource,
    m: &crate::session::ConfigMerge,
    report: &mut InstallReport,
) -> Result<(), String> {
    if let Some(req) = &m.requires_dir {
        if !crate::agent::extension_config::expand_tilde(req).is_dir() {
            report.config_merges_skipped.push(m.path.clone());
            return Ok(());
        }
    }
    let dest = crate::agent::extension_config::expand_tilde(&m.path);
    let to_merge: serde_json::Value = serde_json::from_str(
        &crate::agent::extension_config::fetch_file(source, m.source_path())?,
    )
    .map_err(|e| format!("parse merge source {}: {e}", m.source_path()))?;
    // A user's malformed target must NOT abort the whole install: this runs every
    // startup + heartbeat tick, so one broken file would degrade every agent's
    // wiring. Soft-skip it (mirroring the `requires_dir` guard) and carry on.
    let mut doc = match read_json_or_empty(&dest) {
        Ok(doc) => doc,
        Err(e) => {
            tracing::warn!("skipping config merge into {}: {e}", dest.display());
            report.config_merges_skipped.push(m.path.clone());
            return Ok(());
        }
    };
    crate::agent::json_merge::merge(&mut doc, &to_merge);
    if write_json_if_changed(&dest, &doc)? {
        report.config_merges_applied.push(m.path.clone());
    } else {
        report.config_merges_skipped.push(m.path.clone());
    }
    Ok(())
}

/// Reverse an [`install_config_merge`]: prune our marked hook entries out of the
/// agent's config file, leaving the user's own settings intact. A missing file
/// is a no-op. Returns whether the path was touched.
fn revert_config_merge(m: &crate::session::ConfigMerge) -> Result<bool, String> {
    let dest = crate::agent::extension_config::expand_tilde(&m.path);
    if !dest.exists() {
        return Ok(false);
    }
    // A malformed target can't be safely pruned; leave it rather than abort the
    // rest of the uninstall (consistent with the install soft-skip).
    let mut doc = match read_json_or_empty(&dest) {
        Ok(doc) => doc,
        Err(e) => {
            tracing::warn!("skipping config-merge revert in {}: {e}", dest.display());
            return Ok(false);
        }
    };
    crate::agent::json_merge::prune_marked(&mut doc, HOOK_SIGNAL_MARKER);
    write_json_if_changed(&dest, &doc)
}

/// Create one symlink under the home dir, replacing an existing symlink but
/// never clobbering a regular file the user owns, recording the outcome in
/// `report`.
fn install_symlink(
    s: &crate::session::extension_def::ExtensionSymlink,
    home: &Path,
    report: &mut InstallReport,
) -> Result<(), String> {
    // Validate both ends before touching the filesystem, so a bad target
    // can't leave a removed symlink behind.
    let link = safe_join(home, &s.link)?;
    ensure_safe_relative(&s.target)?;
    match std::fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() => {
            std::fs::remove_file(&link)
                .map_err(|e| format!("replace symlink {}: {e}", link.display()))?;
        }
        Ok(_) => {
            report.symlinks_skipped.push(s.link.clone());
            return Ok(());
        }
        Err(_) => {}
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    make_symlink(&s.target, &link)?;
    report.symlinks_created.push(s.link.clone());
    Ok(())
}

/// What [`update_extension`] did to one extension.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateReport {
    pub name: String,
    /// `true` when the declared `version` changed (a real upgrade/downgrade).
    pub changed: bool,
    /// The underlying re-install report (files refreshed, version move, …).
    pub install: InstallReport,
}

/// Re-install an already-installed extension from its **recorded source**,
/// refreshing its payload + manifest to match the running binary. This is the
/// mechanism that keeps extensions in sync after a thurbox upgrade: a bare-name
/// source re-resolves against the new binary's release tag, so the matching
/// extension version is fetched.
///
/// User-edited `substitute` files and `if_absent` seed files are preserved
/// (same rules as install; pass `force` to overwrite them). Errors if the
/// extension isn't installed or its manifest recorded no source.
pub fn update_extension(db: &Database, name: &str, force: bool) -> Result<UpdateReport, String> {
    let installed = crate::agent::extension_config::load_manifest(name)
        .ok_or_else(|| format!("extension '{name}' is not installed (no manifest found)"))?;
    let source = installed.source.clone().ok_or_else(|| {
        format!(
            "extension '{name}' has no recorded install source (installed by an older thurbox); \
             reinstall it with `thurbox-cli extension install {name}`"
        )
    })?;
    // Keep it in its existing home, regardless of what the new manifest defaults to.
    let home = installed.home.clone();
    let install = install_extension(db, &source, home.as_deref(), force)?;
    let changed = install.previous_version != install.version;
    Ok(UpdateReport {
        name: name.to_string(),
        changed,
        install,
    })
}

/// Update every installed extension (see [`update_extension`]), returning a
/// per-extension result so one failure doesn't abort the rest. The names come
/// from the discovery dir, in sorted order.
pub fn update_all_extensions(
    db: &Database,
    force: bool,
) -> Vec<(String, Result<UpdateReport, String>)> {
    crate::agent::extension_config::list_manifests()
        .into_iter()
        .map(|def| {
            let name = def.name.clone();
            let result = update_extension(db, &name, force);
            (name, result)
        })
        .collect()
}

/// What [`reinstall_extension`] did: the uninstall teardown followed by a fresh
/// install from the recorded source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReinstallReport {
    pub name: String,
    pub uninstall: UninstallReport,
    pub install: InstallReport,
}

/// Clean-slate reinstall: fully [`uninstall_extension`] the extension (tearing
/// down its session/automation, removing its agents, deleting its manifest, and
/// — with `purge_home` — its home dir), then re-[`install_extension`] from the
/// **recorded source** with `force` so even user-edited seed/`substitute` files
/// are rewritten.
///
/// This is the heavier hammer than `update --force`: `update` refreshes payload
/// files in place but never removes now-stale agents or runtime resources, while
/// `reinstall` removes everything first and lays it down fresh. The extension's
/// home is preserved unless `purge_home`. Errors if the extension isn't
/// installed or its manifest recorded no source (older installs — uninstall +
/// install by hand instead).
pub fn reinstall_extension(
    db: &Database,
    name: &str,
    purge_home: bool,
) -> Result<ReinstallReport, String> {
    let installed = crate::agent::extension_config::load_manifest(name)
        .ok_or_else(|| format!("extension '{name}' is not installed (no manifest found)"))?;
    let source = installed.source.clone().ok_or_else(|| {
        format!(
            "extension '{name}' has no recorded install source (installed by an older thurbox); \
             reinstall it by hand: `thurbox-cli extension uninstall {name}` then \
             `thurbox-cli extension install {name}`"
        )
    })?;
    // Keep the extension in its existing home unless the caller purges it.
    let home = installed.home.clone();

    let uninstall = uninstall_extension(db, name, purge_home)?;
    let install = install_extension(db, &source, home.as_deref(), true)?;
    Ok(ReinstallReport {
        name: name.to_string(),
        uninstall,
        install,
    })
}

/// What [`uninstall_extension`] removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UninstallReport {
    pub name: String,
    /// The teardown of runtime resources (session/automation) + active set.
    pub deactivate: DeactivateReport,
    pub agents_removed: Vec<String>,
    /// Existing agents whose hook-patch args were removed.
    pub agents_unpatched: Vec<String>,
    /// External files (hook plugins) removed from agents' config dirs.
    pub external_files_removed: Vec<String>,
    /// Config files our JSON-merged hook entries were pruned out of.
    pub config_merges_reverted: Vec<String>,
    pub manifest_removed: bool,
    /// The home dir, if it was removed (`purge_home`).
    pub home_removed: Option<String>,
}

/// Fully reverse an install: tear down the session/automation (force), remove
/// the extension's agents from `agents.toml`, and delete the discovery manifest.
/// With `purge_home`, also delete the install home directory (the payload +
/// any user data under it). The inverse of [`install_extension`].
pub fn uninstall_extension(
    db: &Database,
    name: &str,
    purge_home: bool,
) -> Result<UninstallReport, String> {
    let def = crate::agent::extension_config::load_manifest(name)
        .ok_or_else(|| format!("extension '{name}' is not installed (no manifest found)"))?;

    let mut report = UninstallReport {
        name: name.to_string(),
        ..Default::default()
    };

    // Tear down runtime resources + clear the active set (force kills tmux/worktrees).
    report.deactivate = deactivate_extension(db, &def, true)?;

    // Remove the agents this extension registered.
    let agent_names: Vec<String> = def.agents.iter().map(|a| a.name.clone()).collect();
    report.agents_removed = crate::agent::extension_config::remove_agents_from_toml(&agent_names)?;

    // Reverse hook-arg patches on existing agents (manifest stores resolved args).
    report.agents_unpatched =
        crate::agent::extension_config::remove_agent_patches(&def.agent_patches)?;

    // Remove external hook files we still own (those carrying our managed marker).
    for f in &def.external_files {
        let dest = crate::agent::extension_config::expand_tilde(&f.path);
        if dest.is_file() && !is_user_modified(&dest) && std::fs::remove_file(&dest).is_ok() {
            report.external_files_removed.push(f.path.clone());
        }
    }

    // Prune our merged hook entries out of agents' own config files, leaving the
    // user's other settings intact.
    for m in &def.config_merges {
        if revert_config_merge(m)? {
            report.config_merges_reverted.push(m.path.clone());
        }
    }

    // Optionally delete the install home (payload + user data).
    if purge_home {
        if let Some(home) = &def.home {
            let path = crate::agent::extension_config::expand_tilde(home);
            guard_removable_dir(&path)?;
            if path.is_dir() {
                remove_dir_all_resilient(&path)
                    .map_err(|e| format!("remove {}: {e}", path.display()))?;
                report.home_removed = Some(path.to_string_lossy().into_owned());
            }
        }
    }

    // Drop the discovery manifest last, so a failure above leaves it recoverable.
    report.manifest_removed = crate::agent::extension_config::remove_manifest_file(name)?;

    Ok(report)
}

/// Refuse to recursively delete obviously-dangerous paths (root, `$HOME`
/// itself, or a shallow path) — a guard before `remove_dir_all` on a
/// manifest-supplied home.
/// Remove a directory tree. On Windows a just-written payload file can be held
/// transiently by the search indexer / antivirus, so `remove_dir_all` fails with
/// `ERROR_SHARING_VIOLATION` (os error 32); retry with a short backoff until the
/// handle is released. Unix removes in one shot.
fn remove_dir_all_resilient(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let mut last: std::io::Result<()> = Ok(());
        // Rides out a *transient* hold on a just-written payload file by the
        // search indexer / antivirus (ERROR_SHARING_VIOLATION), retrying ~1.4s.
        // It does NOT help when the dir is held persistently — e.g. psmux's
        // server-level handle on a deleted session's pane cwd, which only
        // `kill-server` frees (a documented Windows limitation).
        for attempt in 0..5u64 {
            match std::fs::remove_dir_all(path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Err(e);
                    std::thread::sleep(std::time::Duration::from_millis(100 * (attempt + 1)));
                }
            }
        }
        last
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_dir_all(path)
    }
}

fn guard_removable_dir(path: &Path) -> Result<(), String> {
    let depth = path
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count();
    if depth < 2 {
        return Err(format!(
            "refusing to remove '{}' (too shallow); remove it by hand",
            path.display()
        ));
    }
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    if let Some(home) = std::env::var_os(home_var) {
        if path == Path::new(&home) {
            return Err("refusing to remove the user's home directory".into());
        }
    }
    Ok(())
}

/// Validate that a manifest-supplied path is a safe relative path (no absolute
/// root, no `..` components) — the path-traversal guard for install payloads.
fn ensure_safe_relative(rel: &str) -> Result<(), String> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(format!(
            "manifest path '{rel}' must be relative, not absolute"
        ));
    }
    for c in p.components() {
        match c {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            _ => {
                return Err(format!(
                    "manifest path '{rel}' must not contain '..' or a root component"
                ))
            }
        }
    }
    Ok(())
}

/// [`ensure_safe_relative`] + join under `home`.
fn safe_join(home: &Path, rel: &str) -> Result<PathBuf, String> {
    ensure_safe_relative(rel)?;
    Ok(home.join(rel))
}

/// Marker an installer-managed `substitute` file carries (in the template
/// content) so reinstall can overwrite *its own* file but not one the user has
/// edited (or whose marker they removed).
const MANAGED_MARKER: &str = "thurbox `extension install`";

/// Whether `dest` is a `substitute` file the user has taken ownership of: it
/// exists but no longer carries the managed marker. A missing file (fresh
/// install) or one still carrying the marker is ours to (over)write.
fn is_user_modified(dest: &Path) -> bool {
    match std::fs::read_to_string(dest) {
        Ok(content) => !content.contains(MANAGED_MARKER),
        Err(_) => false,
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn make_symlink(target: &str, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link)
        .map_err(|e| format!("symlink {} -> {target}: {e}", link.display()))
}

#[cfg(windows)]
fn make_symlink(target: &str, link: &Path) -> Result<(), String> {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    // `target` is relative to the link's parent; resolve it to choose the right
    // symlink flavour (Windows distinguishes file vs directory symlinks).
    let resolved = link
        .parent()
        .map(|p| p.join(target))
        .unwrap_or_else(|| std::path::PathBuf::from(target));
    let is_dir = resolved.is_dir();
    let primary = if is_dir {
        symlink_dir(target, link)
    } else {
        symlink_file(target, link)
    };
    if let Err(err) = primary {
        // Symlink creation needs privilege (admin or Developer Mode). Fall back
        // to a privilege-free equivalent that keeps the payload reachable at
        // `link`: an NTFS junction for directories, a hard link for files.
        let recovered = if is_dir {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(&resolved)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        } else {
            std::fs::hard_link(&resolved, link).is_ok()
        };
        if !recovered {
            return Err(format!("symlink {} -> {target}: {err}", link.display()));
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn make_symlink(_target: &str, _link: &Path) -> Result<(), String> {
    Err("symlinks are not supported on this platform".into())
}

/// Idempotently ensure every resource a manifest declares exists. Existing
/// sessions/automations are matched by name and reused; only the missing ones
/// are created. An existing `Send` automation whose stored target id no longer
/// matches its session's current id is re-linked (a recreated session would
/// otherwise orphan it). Safe to call repeatedly (this is the self-heal
/// primitive).
pub fn ensure_extension(db: &Database, def: &ExtensionDef) -> Result<EnsureReport, String> {
    let mut report = EnsureReport::default();
    let mut session_ids: HashMap<String, SessionId> = HashMap::new();

    for sess in &def.sessions {
        let existing = db
            .list_active_sessions()
            .map_err(|e| format!("list_active_sessions: {e}"))?
            .into_iter()
            .find(|row| row.name == sess.name);
        let id = match existing {
            Some(row) => row.id,
            None => {
                let result = super::spawn_session_headless(
                    db,
                    super::SpawnRequest {
                        name: sess.name.clone(),
                        repo_path: sess.repo_path.clone(),
                        worktree_branch: None,
                        base_branch: None,
                        agent: Some(sess.agent.clone()),
                        agent_session_id: None,
                        host: None,
                        parent_session_id: None,
                        task_id: None,
                        extra_repos: Vec::new(),
                    },
                )?;
                report.sessions_created.push(sess.name.clone());
                result.session_id
            }
        };
        session_ids.insert(sess.name.clone(), id);
    }

    for auto in &def.automations {
        let target = *session_ids.get(&auto.session_ref).ok_or_else(|| {
            format!(
                "automation '{}' references unknown session '{}'",
                auto.name, auto.session_ref
            )
        })?;
        ensure_automation(db, auto, target, &mut report)?;
    }

    Ok(report)
}

/// Ensure a single declared automation exists and points at `target` (its
/// session's current id). Matched by name: a missing one is created; an
/// existing one with a stale `Send` target is re-linked (see [`ensure_extension`]
/// for why a recreated session orphans the old id).
fn ensure_automation(
    db: &Database,
    auto: &ExtensionAutomation,
    target: SessionId,
    report: &mut EnsureReport,
) -> Result<(), String> {
    let existing = db
        .list_automations()
        .map_err(|e| format!("list_automations: {e}"))?
        .into_iter()
        .find(|row| row.name == auto.name);
    if let Some(mut row) = existing {
        if matches!(&row.action, AutomationAction::Send { session_id } if *session_id != target) {
            row.action = AutomationAction::Send { session_id: target };
            db.update_automation(&row)
                .map_err(|e| format!("update_automation: {e}"))?;
            report.automations_relinked.push(auto.name.clone());
        }
        return Ok(());
    }
    let schedule = parse_trigger(&auto.trigger, None, None)?;
    let next_run_at = schedule.next_after(current_time_millis(), None);
    let new = NewAutomation {
        name: auto.name.clone(),
        enabled: true,
        schedule,
        timezone: None,
        action: AutomationAction::Send { session_id: target },
        prompt: auto.prompt.clone(),
        next_run_at,
    };
    db.create_automation(&new)
        .map_err(|e| format!("create_automation: {e}"))?;
    report.automations_created.push(auto.name.clone());
    Ok(())
}

/// Activate an extension: ensure its resources exist, then record it in the
/// active set so self-heal keeps them alive. Idempotent.
///
/// Note: this does NOT arm the tmux automation heartbeat — the CLI layer does
/// that (it owns the `agent::tmux` dependency). A `Send` automation only fires
/// while something ticks it (TUI tick loop, or the heartbeat keeper window).
pub fn activate_extension(db: &Database, def: &ExtensionDef) -> Result<EnsureReport, String> {
    let report = ensure_extension(db, def)?;
    db.add_active_extension(&def.name)
        .map_err(|e| format!("add_active_extension: {e}"))?;
    Ok(report)
}

/// Deactivate an extension: delete its declared automations and sessions, then
/// drop it from the active set so self-heal won't resurrect it. `force` also
/// tears down each session's tmux window/worktrees (otherwise a soft delete).
/// Idempotent — missing resources are simply skipped.
pub fn deactivate_extension(
    db: &Database,
    def: &ExtensionDef,
    force: bool,
) -> Result<DeactivateReport, String> {
    let mut report = DeactivateReport::default();

    for auto in &def.automations {
        let id = db
            .list_automations()
            .map_err(|e| format!("list_automations: {e}"))?
            .into_iter()
            .find(|row| row.name == auto.name)
            .map(|row| row.id);
        if let Some(id) = id {
            db.delete_automation(id)
                .map_err(|e| format!("delete_automation: {e}"))?;
            report.automations_deleted.push(auto.name.clone());
        }
    }

    for sess in &def.sessions {
        let id = db
            .list_active_sessions()
            .map_err(|e| format!("list_active_sessions: {e}"))?
            .into_iter()
            .find(|row| row.name == sess.name)
            .map(|row| row.id);
        if let Some(id) = id {
            super::delete_session_headless(db, id, force)?;
            report.sessions_deleted.push(sess.name.clone());
        }
    }

    report.was_active = db
        .remove_active_extension(&def.name)
        .map_err(|e| format!("remove_active_extension: {e}"))?;

    Ok(report)
}

/// Re-ensure every active extension's declared resources, returning user-facing
/// messages for anything that was recreated, has a missing manifest, or failed.
/// This is the self-heal entry point shared by TUI startup and the headless
/// `automation tick`. Never errors out: per-extension problems become messages,
/// so one bad extension can't block the others (or, in tick, the firing pass).
///
/// The active set is read from SQLite `metadata`; `activate_extension` /
/// `deactivate_extension` (i.e. `thurbox-cli extension …`) manage membership.
pub fn heal_active_extensions(db: &Database) -> Vec<String> {
    let active = db.get_active_extensions().unwrap_or_default();
    let mut messages = Vec::new();
    for name in active {
        heal_one_extension(db, &name, &mut messages);
    }
    messages
}

/// Self-heal a single active extension: surface a missing-manifest error, a
/// compat/staleness nudge, and re-ensure its declared resources, appending any
/// user-facing messages to `messages`.
fn heal_one_extension(db: &Database, name: &str, messages: &mut Vec<String>) {
    // Fully-qualified agent reference (no `use`) per the session_ops →
    // agent path-only architecture rule.
    let Some(def) = crate::agent::extension_config::load_manifest(name) else {
        messages.push(format!(
            "extension '{name}' is active but its manifest is missing; reinstall it \
             or run `thurbox-cli extension deactivate {name}`"
        ));
        return;
    };
    // Nudge once-per-pass when the binary upgraded since install, or when the
    // extension wants a newer thurbox than this one. Both clear after the
    // user acts (`extension update` / a thurbox upgrade), so they don't
    // persist as noise.
    let current = crate::agent::extension_config::binary_version();
    if let Some(w) = heal_compat_message(&def, name, current) {
        messages.push(w);
    }
    match ensure_extension(db, &def) {
        Ok(report) if report.created_anything() => {
            messages.push(heal_recreated_message(&report, name));
        }
        Ok(_) => {}
        Err(e) => messages.push(format!("extension '{name}' self-heal failed: {e}")),
    }
}

/// A compatibility warning (binary too old) or a staleness nudge (extension
/// installed under an older binary), or `None` when neither applies.
fn heal_compat_message(def: &ExtensionDef, name: &str, current: &str) -> Option<String> {
    if let Some(w) = def.compat_warning(current) {
        Some(w)
    } else if def.is_stale(current) {
        Some(format!(
            "extension '{name}' was installed under thurbox {} but this binary is {current}; \
             run `thurbox-cli extension update {name}` to refresh it",
            def.installed_with.as_deref().unwrap_or("an older version")
        ))
    } else {
        None
    }
}

/// Human-readable "Repaired …" message describing what a self-heal pass
/// re-created or re-linked for an extension.
fn heal_recreated_message(report: &EnsureReport, name: &str) -> String {
    let mut parts = Vec::new();
    if !report.sessions_created.is_empty() {
        parts.push(format!("session(s) {}", report.sessions_created.join(", ")));
    }
    if !report.automations_created.is_empty() {
        parts.push(format!(
            "automation(s) {}",
            report.automations_created.join(", ")
        ));
    }
    if !report.automations_relinked.is_empty() {
        parts.push(format!(
            "re-linked automation(s) {}",
            report.automations_relinked.join(", ")
        ));
    }
    format!(
        "Repaired {} for managed extension '{name}' \
         (`thurbox-cli extension deactivate {name}` to turn it off)",
        parts.join(" + ")
    )
}

/// Snapshot which of a manifest's declared resources currently exist and whether
/// the extension is in the active set.
pub fn extension_health(db: &Database, def: &ExtensionDef) -> Result<ExtensionHealth, String> {
    let session_names: Vec<String> = db
        .list_active_sessions()
        .map_err(|e| format!("list_active_sessions: {e}"))?
        .into_iter()
        .map(|row| row.name)
        .collect();
    let automation_names: Vec<String> = db
        .list_automations()
        .map_err(|e| format!("list_automations: {e}"))?
        .into_iter()
        .map(|row| row.name)
        .collect();
    let active = db
        .get_active_extensions()
        .map_err(|e| format!("get_active_extensions: {e}"))?
        .iter()
        .any(|n| n == &def.name);

    let current = crate::agent::extension_config::binary_version();
    Ok(ExtensionHealth {
        name: def.name.clone(),
        description: def.description.clone(),
        active,
        sessions: def
            .sessions
            .iter()
            .map(|s| (s.name.clone(), session_names.contains(&s.name)))
            .collect(),
        automations: def
            .automations
            .iter()
            .map(|a| (a.name.clone(), automation_names.contains(&a.name)))
            .collect(),
        version: def.version.clone(),
        installed_with: def.installed_with.clone(),
        current_binary: current.to_string(),
        stale: def.is_stale(current),
        compat_warning: def.compat_warning(current),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ExtensionAutomation, ExtensionSession};
    use crate::sync::SharedSession;

    fn insert_session(db: &Database, name: &str) -> SessionId {
        let id = SessionId::default();
        let shared = SharedSession {
            id,
            name: name.into(),
            agent: "flow".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: Some(uuid::Uuid::new_v4().to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&shared).unwrap();
        id
    }

    /// A manifest whose single session already exists, so ensure never has to
    /// spawn (which would need tmux) — only the automation is created.
    fn flow_def() -> ExtensionDef {
        ExtensionDef {
            name: "flow".into(),
            description: None,
            config_version: Some(1),
            version: None,
            min_thurbox_version: None,
            installed_with: None,
            source: None,
            home: None,
            agents: Vec::new(),
            files: Vec::new(),
            external_files: Vec::new(),
            agent_patches: Vec::new(),
            config_merges: Vec::new(),
            symlinks: Vec::new(),
            sessions: vec![ExtensionSession {
                name: "flow".into(),
                agent: "flow".into(),
                repo_path: "/tmp/flow".into(),
            }],
            automations: vec![ExtensionAutomation {
                name: "flow-tick".into(),
                trigger: "cron:*/5 * * * *".into(),
                session_ref: "flow".into(),
                prompt: "tick".into(),
            }],
        }
    }

    #[test]
    fn ensure_reuses_existing_session_and_creates_automation() {
        let db = Database::open_in_memory().unwrap();
        insert_session(&db, "flow");

        let report = ensure_extension(&db, &flow_def()).unwrap();
        assert!(report.sessions_created.is_empty(), "session was reused");
        assert_eq!(report.automations_created, ["flow-tick"]);

        let autos = db.list_automations().unwrap();
        assert_eq!(autos.len(), 1);
        assert_eq!(autos[0].name, "flow-tick");
        assert!(matches!(autos[0].action, AutomationAction::Send { .. }));
    }

    #[test]
    fn ensure_is_idempotent() {
        let db = Database::open_in_memory().unwrap();
        insert_session(&db, "flow");
        let def = flow_def();

        ensure_extension(&db, &def).unwrap();
        let second = ensure_extension(&db, &def).unwrap();
        assert!(!second.created_anything(), "second pass creates nothing");
        assert_eq!(db.list_automations().unwrap().len(), 1);
    }

    #[test]
    fn ensure_relinks_stale_send_target_after_session_recreated() {
        let db = Database::open_in_memory().unwrap();
        let old_id = insert_session(&db, "flow");
        let def = flow_def();

        // First pass binds the automation to the original session id.
        ensure_extension(&db, &def).unwrap();
        let auto = &db.list_automations().unwrap()[0];
        assert_eq!(auto.action, AutomationAction::Send { session_id: old_id });

        // The session is recreated under the same name with a fresh id (the
        // shape that orphaned the automation: soft-delete + new row).
        db.soft_delete_session(old_id).unwrap();
        let new_id = insert_session(&db, "flow");
        assert_ne!(old_id, new_id);

        // Self-heal re-links the existing automation to the live id rather than
        // leaving it pointing at the dead one.
        let report = ensure_extension(&db, &def).unwrap();
        assert!(report.automations_created.is_empty(), "no new automation");
        assert_eq!(report.automations_relinked, ["flow-tick"]);
        assert!(report.created_anything(), "a relink counts as repair");

        let auto = &db.list_automations().unwrap()[0];
        assert_eq!(auto.action, AutomationAction::Send { session_id: new_id });

        // A subsequent pass is a no-op now that the link is correct.
        let again = ensure_extension(&db, &def).unwrap();
        assert!(!again.created_anything(), "relink is idempotent");
    }

    #[test]
    fn unknown_session_ref_errors() {
        let db = Database::open_in_memory().unwrap();
        insert_session(&db, "flow");
        let mut def = flow_def();
        def.automations[0].session_ref = "ghost".into();
        let err = ensure_extension(&db, &def).unwrap_err();
        assert!(err.contains("ghost"), "got: {err}");
    }

    #[test]
    fn activate_records_active_set() {
        let db = Database::open_in_memory().unwrap();
        insert_session(&db, "flow");
        activate_extension(&db, &flow_def()).unwrap();
        assert_eq!(db.get_active_extensions().unwrap(), ["flow"]);
    }

    #[test]
    fn deactivate_tears_down_and_clears_active_set() {
        let db = Database::open_in_memory().unwrap();
        insert_session(&db, "flow");
        let def = flow_def();
        activate_extension(&db, &def).unwrap();

        let report = deactivate_extension(&db, &def, false).unwrap();
        assert!(report.was_active);
        assert_eq!(report.automations_deleted, ["flow-tick"]);
        assert_eq!(report.sessions_deleted, ["flow"]);
        assert!(db.list_automations().unwrap().is_empty());
        assert!(
            db.get_active_extensions().unwrap().is_empty(),
            "self-heal must not resurrect a deactivated extension"
        );
    }

    #[test]
    fn deactivate_is_idempotent() {
        let db = Database::open_in_memory().unwrap();
        let def = flow_def();
        // Nothing exists / not active — deactivate is a clean no-op.
        let report = deactivate_extension(&db, &def, false).unwrap();
        assert!(!report.was_active);
        assert!(report.automations_deleted.is_empty());
        assert!(report.sessions_deleted.is_empty());
    }

    #[test]
    fn install_lays_files_registers_agents_and_activates() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        // Pre-create the session so activate reuses it (no tmux spawn in tests).
        insert_session(&db, "flow");

        // A local source dir with a manifest + payload.
        let src = tempfile::TempDir::new().unwrap();
        let home = temp.path().join("flowhome");
        std::fs::write(
            src.path().join("extension.toml"),
            format!(
                r#"name = "flow"
home = '{}'

[[agents]]
name = "flow"
command = "claude"
args = ["--model", "haiku"]

[[files]]
path = "FLOW.md"

[[files]]
path = "scripts/do.sh"
executable = true

[[files]]
path = "repos.md"
if_absent = true

[[files]]
path = ".claude/settings.json"
source = "settings.tmpl"
substitute = true

[[symlinks]]
link = "CLAUDE.md"
target = "FLOW.md"

[[sessions]]
name = "flow"
agent = "flow"
repo_path = "{{home}}"

[[automations]]
name = "flow-tick"
trigger = "cron:*/5 * * * *"
session_ref = "flow"
prompt = "tick"
"#,
                home.display()
            ),
        )
        .unwrap();
        std::fs::write(src.path().join("FLOW.md"), "spec").unwrap();
        std::fs::create_dir_all(src.path().join("scripts")).unwrap();
        std::fs::write(src.path().join("scripts/do.sh"), "#!/bin/sh\n").unwrap();
        std::fs::write(src.path().join("repos.md"), "seed table").unwrap();
        std::fs::write(src.path().join("settings.tmpl"), "perm {home}/x").unwrap();

        let target = src.path().to_string_lossy().to_string();
        let report = install_extension(&db, &target, None, false).unwrap();

        // Files laid down under home.
        assert!(home.join("FLOW.md").exists());
        assert!(home.join("scripts/do.sh").exists());
        assert!(home.join("repos.md").exists());
        // {home} substituted in the settings template.
        let settings = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        assert_eq!(settings, format!("perm {}/x", home.display()));
        // Symlink created.
        assert!(std::fs::symlink_metadata(home.join("CLAUDE.md"))
            .unwrap()
            .file_type()
            .is_symlink());
        // executable bit set.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(home.join("scripts/do.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o100, 0o100, "do.sh should be executable");
        }

        // Agent registered, manifest written + active, automation created.
        assert_eq!(report.agents_added, ["flow"]);
        let reg = crate::agent::agent_config::load_or_seed();
        assert_eq!(reg.get("flow").unwrap().args, ["--model", "haiku"]);
        assert_eq!(db.get_active_extensions().unwrap(), ["flow"]);
        let stored = crate::agent::extension_config::load_manifest("flow").unwrap();
        // repo_path was resolved from {home} to the absolute home.
        assert_eq!(stored.sessions[0].repo_path, home);
        // Install provenance is stamped into the discovery manifest.
        assert_eq!(
            stored.installed_with.as_deref(),
            Some(crate::agent::extension_config::binary_version())
        );
        assert_eq!(stored.source.as_deref(), Some(target.as_str()));
        assert_eq!(report.ensure.automations_created, ["flow-tick"]);

        // Re-install is idempotent: repos.md kept (if_absent), no new agents.
        std::fs::write(home.join("repos.md"), "user edited").unwrap();
        let again = install_extension(&db, &target, None, false).unwrap();
        assert!(again.files_skipped.contains(&"repos.md".to_string()));
        assert_eq!(
            std::fs::read_to_string(home.join("repos.md")).unwrap(),
            "user edited",
            "if_absent file must not be clobbered on reinstall"
        );
        assert!(again.agents_added.is_empty());
    }

    #[test]
    fn install_applies_agent_patches_and_external_files() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();

        // An out-of-home dir that exists (the "agent is installed" case) and a
        // plugin destination under it — kept inside the tempdir so the test
        // never touches the real home.
        let plugin_dir = temp.path().join("opencode");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let plugin_dest = plugin_dir.join("plugin/status.js");
        let home = temp.path().join("hookshome");

        let src = tempfile::TempDir::new().unwrap();
        std::fs::write(
            src.path().join("extension.toml"),
            format!(
                r#"name = "hooks"
home = '{home}'

[[agent_patches]]
name = "claude"
append_args = ["--settings", "{{home}}/claude.json"]

[[external_files]]
path = '{plugin}'
source = "status.js"
requires_dir = '{plugin_dir}'
"#,
                home = home.display(),
                plugin = plugin_dest.display(),
                plugin_dir = plugin_dir.display(),
            ),
        )
        .unwrap();
        // The plugin payload carries the managed marker so uninstall can remove it.
        std::fs::write(
            src.path().join("status.js"),
            "// thurbox `extension install` managed\n",
        )
        .unwrap();

        let target = src.path().to_string_lossy().to_string();
        let report = install_extension(&db, &target, None, false).unwrap();

        // The built-in claude agent's args gained the --settings flag, resolved.
        assert_eq!(report.agents_patched, ["claude"]);
        let reg = crate::agent::agent_config::load_or_seed();
        let args = &reg.get("claude").unwrap().args;
        let expected = format!("{}/claude.json", home.display());
        assert!(
            args.windows(2)
                .any(|w| w == ["--settings".to_string(), expected.clone()]),
            "claude args carry the resolved --settings: {args:?}"
        );

        // The external plugin file landed in the out-of-home config dir.
        assert!(plugin_dest.is_file());
        assert!(report
            .external_files_written
            .iter()
            .any(|p| p == &plugin_dest.to_string_lossy()));

        // Uninstall reverses both: the patch is removed and the plugin deleted.
        let un = uninstall_extension(&db, "hooks", false).unwrap();
        assert_eq!(un.agents_unpatched, ["claude"]);
        assert!(!plugin_dest.exists(), "managed plugin removed on uninstall");
        let reg = crate::agent::agent_config::load_or_seed();
        assert!(!reg
            .get("claude")
            .unwrap()
            .args
            .contains(&"--settings".to_string()));
    }

    #[test]
    fn config_merge_installs_and_reverts_without_clobbering_user_config() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();

        // The agent's config dir (gates the merge) + its shared settings file,
        // seeded with the user's own settings incl. a pre-existing empty map.
        let agent_dir = temp.path().join("dotgemini");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let settings = agent_dir.join("settings.json");
        std::fs::write(
            &settings,
            r#"{"theme":"dark","mcpServers":{},"hooks":{"BeforeTool":[{"command":"user"}]}}"#,
        )
        .unwrap();
        let home = temp.path().join("hookshome");

        let src = tempfile::TempDir::new().unwrap();
        std::fs::write(
            src.path().join("extension.toml"),
            format!(
                r#"name = "hooks"
home = '{home}'

[[config_merges]]
path = '{settings}'
source = "gemini-hooks.json"
requires_dir = '{agent_dir}'
"#,
                home = home.display(),
                settings = settings.display(),
                agent_dir = agent_dir.display(),
            ),
        )
        .unwrap();
        std::fs::write(
            src.path().join("gemini-hooks.json"),
            r#"{"hooks":{"BeforeTool":[{"hooks":[{"type":"command","command":"thurbox-cli session signal --state working || true"}]}],"AfterAgent":[{"hooks":[{"type":"command","command":"thurbox-cli session signal --state done || true"}]}]}}"#,
        )
        .unwrap();

        let target = src.path().to_string_lossy().to_string();
        let report = install_extension(&db, &target, None, false).unwrap();
        assert_eq!(report.config_merges_applied, [settings.to_string_lossy()]);

        let merged: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        // User settings preserved (incl. the empty map) ...
        assert_eq!(merged["theme"], serde_json::json!("dark"));
        assert_eq!(merged["mcpServers"], serde_json::json!({}));
        // ... the user's own BeforeTool hook survived, ours was unioned in ...
        assert_eq!(merged["hooks"]["BeforeTool"].as_array().unwrap().len(), 2);
        assert_eq!(
            merged["hooks"]["BeforeTool"][0],
            serde_json::json!({"command": "user"})
        );
        // ... and our AfterAgent hook was added.
        assert!(merged["hooks"]["AfterAgent"].is_array());

        // A re-install is a no-op write (skipped, not re-applied).
        let again = install_extension(&db, &target, None, false).unwrap();
        assert!(again.config_merges_applied.is_empty());
        assert_eq!(again.config_merges_skipped, [settings.to_string_lossy()]);

        // Uninstall prunes exactly our entries; the user's config is restored.
        let un = uninstall_extension(&db, "hooks", false).unwrap();
        assert_eq!(un.config_merges_reverted, [settings.to_string_lossy()]);
        let restored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(
            restored,
            serde_json::json!({"theme":"dark","mcpServers":{},"hooks":{"BeforeTool":[{"command":"user"}]}})
        );
    }

    #[test]
    fn config_merge_skipped_when_requires_dir_absent() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();

        // requires_dir points at a path that does NOT exist (agent not installed).
        let missing_dir = temp.path().join("not-installed");
        let settings = missing_dir.join("settings.json");
        let home = temp.path().join("hookshome");

        let src = tempfile::TempDir::new().unwrap();
        std::fs::write(
            src.path().join("extension.toml"),
            format!(
                r#"name = "hooks"
home = '{home}'

[[config_merges]]
path = '{settings}'
source = "gemini-hooks.json"
requires_dir = '{missing}'
"#,
                home = home.display(),
                settings = settings.display(),
                missing = missing_dir.display(),
            ),
        )
        .unwrap();
        std::fs::write(src.path().join("gemini-hooks.json"), "{}").unwrap();

        let target = src.path().to_string_lossy().to_string();
        let report = install_extension(&db, &target, None, false).unwrap();
        assert_eq!(report.config_merges_skipped, [settings.to_string_lossy()]);
        assert!(report.config_merges_applied.is_empty());
        assert!(
            !settings.exists(),
            "no file created when the agent is absent"
        );
    }

    #[test]
    fn config_merge_soft_skips_a_malformed_user_target() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();

        // The agent is installed (dir exists) but the user's settings file is
        // broken JSON. Install must NOT abort — it runs every startup + tick.
        let agent_dir = temp.path().join("dotgemini");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let settings = agent_dir.join("settings.json");
        std::fs::write(&settings, "{ this is not valid json").unwrap();
        let home = temp.path().join("hookshome");

        let src = tempfile::TempDir::new().unwrap();
        std::fs::write(
            src.path().join("extension.toml"),
            format!(
                r#"name = "hooks"
home = '{home}'

[[config_merges]]
path = '{settings}'
source = "gemini-hooks.json"
requires_dir = '{agent_dir}'
"#,
                home = home.display(),
                settings = settings.display(),
                agent_dir = agent_dir.display(),
            ),
        )
        .unwrap();
        std::fs::write(src.path().join("gemini-hooks.json"), r#"{"hooks":{}}"#).unwrap();

        let target = src.path().to_string_lossy().to_string();
        // Install succeeds despite the broken target; the merge is soft-skipped...
        let report = install_extension(&db, &target, None, false).unwrap();
        assert_eq!(report.config_merges_skipped, [settings.to_string_lossy()]);
        assert!(report.config_merges_applied.is_empty());
        // ...and the user's (broken) file is left exactly as-is, not overwritten.
        assert_eq!(
            std::fs::read_to_string(&settings).unwrap(),
            "{ this is not valid json"
        );
    }

    #[test]
    fn config_merge_revert_soft_skips_a_malformed_target() {
        // If the user corrupts settings.json AFTER install, uninstall must not
        // abort on the unparseable file — it leaves it and reverts nothing.
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();

        let agent_dir = temp.path().join("dotgemini");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let settings = agent_dir.join("settings.json");
        std::fs::write(&settings, "{}").unwrap();
        let home = temp.path().join("hookshome");

        let src = tempfile::TempDir::new().unwrap();
        std::fs::write(
            src.path().join("extension.toml"),
            format!(
                r#"name = "hooks"
home = '{home}'

[[config_merges]]
path = '{settings}'
source = "gemini-hooks.json"
requires_dir = '{agent_dir}'
"#,
                home = home.display(),
                settings = settings.display(),
                agent_dir = agent_dir.display(),
            ),
        )
        .unwrap();
        std::fs::write(
            src.path().join("gemini-hooks.json"),
            r#"{"hooks":{"AfterAgent":[{"hooks":[{"type":"command","command":"thurbox-cli session signal --state done || true"}]}]}}"#,
        )
        .unwrap();

        let target = src.path().to_string_lossy().to_string();
        install_extension(&db, &target, None, false).unwrap();
        // The user corrupts the file after install.
        std::fs::write(&settings, "}{ broken").unwrap();

        // Uninstall succeeds, reverts nothing, leaves the broken file untouched.
        let un = uninstall_extension(&db, "hooks", false).unwrap();
        assert!(un.config_merges_reverted.is_empty());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "}{ broken");
    }

    #[test]
    fn external_file_skipped_when_requires_dir_absent() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();

        let missing_dir = temp.path().join("not-installed");
        let dest = missing_dir.join("plugin/status.js");
        let home = temp.path().join("h");
        let src = tempfile::TempDir::new().unwrap();
        std::fs::write(
            src.path().join("extension.toml"),
            format!(
                r#"name = "hooks"
home = '{home}'

[[external_files]]
path = '{dest}'
source = "status.js"
requires_dir = '{req}'
"#,
                home = home.display(),
                dest = dest.display(),
                req = missing_dir.display(),
            ),
        )
        .unwrap();
        std::fs::write(
            src.path().join("status.js"),
            "// thurbox `extension install`\n",
        )
        .unwrap();

        let report = install_extension(&db, &src.path().to_string_lossy(), None, false).unwrap();
        assert!(!dest.exists(), "skipped because requires_dir is absent");
        assert!(report
            .external_files_skipped
            .iter()
            .any(|p| p == &dest.to_string_lossy()));
    }

    #[test]
    fn ensure_safe_relative_rejects_traversal_and_absolute() {
        assert!(ensure_safe_relative("FLOW.md").is_ok());
        assert!(ensure_safe_relative("scripts/do.sh").is_ok());
        assert!(ensure_safe_relative("./a/b").is_ok());
        assert!(ensure_safe_relative("/etc/passwd").is_err());
        assert!(ensure_safe_relative("../escape").is_err());
        assert!(ensure_safe_relative("a/../../b").is_err());
    }

    #[test]
    fn install_rejects_path_traversal_in_manifest() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();

        let src = tempfile::TempDir::new().unwrap();
        std::fs::write(
            src.path().join("extension.toml"),
            format!(
                "name = \"evil\"\nhome = '{}'\n[[files]]\npath = \"../../pwned\"\n",
                temp.path().join("h").display()
            ),
        )
        .unwrap();
        std::fs::write(src.path().join("../../pwned"), "x").ok();

        let target = src.path().to_string_lossy().to_string();
        let err = install_extension(&db, &target, None, false).unwrap_err();
        assert!(err.contains("must not contain '..'"), "got: {err}");
    }

    #[test]
    fn install_skips_user_modified_substitute_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        insert_session(&db, "flow");

        let src = tempfile::TempDir::new().unwrap();
        let home = temp.path().join("h");
        std::fs::write(
            src.path().join("extension.toml"),
            format!(
                "name = \"flow\"\nhome = '{}'\n[[files]]\npath = \"settings.json\"\nsubstitute = true\n[[sessions]]\nname = \"flow\"\nagent = \"flow\"\nrepo_path = \"{{home}}\"\n",
                home.display()
            ),
        )
        .unwrap();
        // Template carries the managed marker so a fresh install owns it.
        std::fs::write(
            src.path().join("settings.json"),
            "thurbox `extension install` managed {home}",
        )
        .unwrap();
        let target = src.path().to_string_lossy().to_string();

        // First install writes it.
        let r1 = install_extension(&db, &target, None, false).unwrap();
        assert!(r1.files_written.contains(&"settings.json".to_string()));

        // User edits it (drops the marker) → reinstall must not clobber it.
        std::fs::write(home.join("settings.json"), "MY CUSTOM PERMS").unwrap();
        let r2 = install_extension(&db, &target, None, false).unwrap();
        assert!(r2.files_skipped.contains(&"settings.json".to_string()));
        assert_eq!(
            std::fs::read_to_string(home.join("settings.json")).unwrap(),
            "MY CUSTOM PERMS"
        );

        // --force overrides and rewrites from the template.
        let r3 = install_extension(&db, &target, None, true).unwrap();
        assert!(r3.files_written.contains(&"settings.json".to_string()));
    }

    #[test]
    // Only ext test that force-deletes then re-spawns a session, so on Windows
    // `install#2` spawns a real psmux pane with `cwd = flowhome`. psmux holds an
    // OS handle to that working dir and only releases it on `kill-server`
    // (`kill-window`/`respawn-pane`/waiting do NOT release it — verified directly
    // in the Windows VM), so `remove_dir_all(flowhome)` hits os error 32. This is
    // an upstream psmux limitation, not a thurbox bug; `force_teardown`'s
    // pane-reap + `remove_dir_all_resilient` are partial mitigations but cannot
    // free a *server*-held handle without killing the shared server.
    #[cfg_attr(windows, ignore = "psmux leaks the pane cwd handle until kill-server")]
    fn uninstall_reverses_install() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        insert_session(&db, "flow");

        let src = tempfile::TempDir::new().unwrap();
        let home = temp.path().join("flowhome");
        std::fs::write(
            src.path().join("extension.toml"),
            format!(
                r#"name = "flow"
home = '{}'
[[agents]]
name = "flow"
command = "claude"
[[files]]
path = "FLOW.md"
[[sessions]]
name = "flow"
agent = "flow"
repo_path = "{{home}}"
[[automations]]
name = "flow-tick"
trigger = "cron:*/5 * * * *"
session_ref = "flow"
prompt = "tick"
"#,
                home.display()
            ),
        )
        .unwrap();
        std::fs::write(src.path().join("FLOW.md"), "spec").unwrap();
        let target = src.path().to_string_lossy().to_string();

        install_extension(&db, &target, None, false).unwrap();
        assert!(home.join("FLOW.md").exists());
        assert!(crate::agent::agent_config::load_or_seed()
            .get("flow")
            .is_some());
        assert_eq!(db.get_active_extensions().unwrap(), ["flow"]);

        // Uninstall without --purge keeps the home dir but removes everything else.
        let report = uninstall_extension(&db, "flow", false).unwrap();
        assert_eq!(report.agents_removed, ["flow"]);
        assert!(report.manifest_removed);
        assert!(report.home_removed.is_none());
        assert!(crate::agent::agent_config::load_or_seed()
            .get("flow")
            .is_none());
        assert!(db.get_active_extensions().unwrap().is_empty());
        assert!(crate::agent::extension_config::load_manifest("flow").is_none());
        assert!(home.join("FLOW.md").exists(), "home kept without --purge");

        // Reinstall, then uninstall --purge removes the home dir too.
        install_extension(&db, &target, None, false).unwrap();
        let report = uninstall_extension(&db, "flow", true).unwrap();
        assert_eq!(
            report.home_removed.as_deref(),
            Some(home.to_string_lossy().as_ref())
        );
        assert!(!home.exists(), "home removed with --purge");
    }

    #[test]
    fn update_refetches_from_recorded_source_and_reports_version_move() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        insert_session(&db, "flow");

        let src = tempfile::TempDir::new().unwrap();
        let home = temp.path().join("flowhome");
        let manifest = |version: &str| {
            format!(
                "name = \"flow\"\nversion = \"{version}\"\nhome = '{}'\n[[files]]\npath = \"FLOW.md\"\n[[sessions]]\nname = \"flow\"\nagent = \"flow\"\nrepo_path = \"{{home}}\"\n",
                home.display()
            )
        };
        std::fs::write(src.path().join("extension.toml"), manifest("1.0.0")).unwrap();
        std::fs::write(src.path().join("FLOW.md"), "v1 spec").unwrap();
        let target = src.path().to_string_lossy().to_string();

        install_extension(&db, &target, None, false).unwrap();
        let stored = crate::agent::extension_config::load_manifest("flow").unwrap();
        assert_eq!(stored.version.as_deref(), Some("1.0.0"));
        assert_eq!(stored.source.as_deref(), Some(target.as_str()));

        // Author publishes a new version at the same source; update pulls it.
        std::fs::write(src.path().join("extension.toml"), manifest("2.0.0")).unwrap();
        std::fs::write(src.path().join("FLOW.md"), "v2 spec").unwrap();

        let report = update_extension(&db, "flow", false).unwrap();
        assert!(report.changed, "version moved 1.0.0 -> 2.0.0");
        assert_eq!(report.install.previous_version.as_deref(), Some("1.0.0"));
        assert_eq!(report.install.version.as_deref(), Some("2.0.0"));
        assert_eq!(
            std::fs::read_to_string(home.join("FLOW.md")).unwrap(),
            "v2 spec"
        );
        assert_eq!(
            crate::agent::extension_config::load_manifest("flow")
                .unwrap()
                .version
                .as_deref(),
            Some("2.0.0")
        );

        // A no-op update (same source, unchanged) reports changed = false.
        let again = update_extension(&db, "flow", false).unwrap();
        assert!(!again.changed);
    }

    #[test]
    fn update_errors_when_no_recorded_source() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        // A manifest installed by an older thurbox carries no `source`.
        crate::agent::extension_config::write_manifest(&ExtensionDef {
            name: "legacy".into(),
            ..Default::default()
        })
        .unwrap();
        let err = update_extension(&db, "legacy", false).unwrap_err();
        assert!(err.contains("no recorded install source"), "got: {err}");
    }

    #[test]
    // Real-spawns a session (ensure_extension → spawn), so it needs a live
    // multiplexer. The GH windows-latest runner has no psmux installed; this
    // real-spawn path is covered by the dockur VM suite instead.
    #[cfg_attr(
        windows,
        ignore = "needs a multiplexer; not installed on the GH windows runner"
    )]
    fn reinstall_tears_down_then_installs_fresh() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        insert_session(&db, "flow");

        let src = tempfile::TempDir::new().unwrap();
        let home = temp.path().join("flowhome");
        std::fs::write(
            src.path().join("extension.toml"),
            format!(
                "name = \"flow\"\nversion = \"1.0.0\"\nhome = '{}'\n[[files]]\npath = \"seed.md\"\nif_absent = true\n[[sessions]]\nname = \"flow\"\nagent = \"flow\"\nrepo_path = \"{{home}}\"\n",
                home.display()
            ),
        )
        .unwrap();
        std::fs::write(src.path().join("seed.md"), "pristine seed").unwrap();
        let target = src.path().to_string_lossy().to_string();

        install_extension(&db, &target, None, false).unwrap();
        // User edits the if_absent seed — update without --force would keep it.
        std::fs::write(home.join("seed.md"), "user edit").unwrap();

        let report = reinstall_extension(&db, "flow", false).unwrap();
        assert_eq!(report.name, "flow");
        assert!(report.uninstall.manifest_removed);
        assert_eq!(report.install.version.as_deref(), Some("1.0.0"));
        // Reinstall forces even the if_absent seed back to pristine.
        assert_eq!(
            std::fs::read_to_string(home.join("seed.md")).unwrap(),
            "pristine seed"
        );
        // The extension is installed + active again afterwards.
        assert!(crate::agent::extension_config::load_manifest("flow").is_some());
    }

    #[test]
    fn reinstall_errors_when_no_recorded_source() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        crate::agent::extension_config::write_manifest(&ExtensionDef {
            name: "legacy".into(),
            ..Default::default()
        })
        .unwrap();
        let err = reinstall_extension(&db, "legacy", false).unwrap_err();
        assert!(err.contains("no recorded install source"), "got: {err}");
    }

    #[test]
    fn guard_refuses_shallow_dirs() {
        assert!(guard_removable_dir(Path::new("/x")).is_err());
        assert!(guard_removable_dir(Path::new("/home/me/flow")).is_ok());
    }

    #[test]
    fn health_reports_presence_and_active_flag() {
        let db = Database::open_in_memory().unwrap();
        let def = flow_def();

        let before = extension_health(&db, &def).unwrap();
        assert!(!before.active);
        assert_eq!(before.sessions, [("flow".to_string(), false)]);
        assert_eq!(before.automations, [("flow-tick".to_string(), false)]);
        assert!(!before.is_healthy());

        insert_session(&db, "flow");
        activate_extension(&db, &def).unwrap();
        let after = extension_health(&db, &def).unwrap();
        assert!(after.active);
        assert!(after.is_healthy());
    }
}
