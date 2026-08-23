//! The install half of the extension machinery: fetching a manifest, laying
//! down its payload (files, symlinks, external files, agent patches, config
//! merges), and the update / reinstall / uninstall flows that refresh or
//! reverse it. The runtime half (ensure/activate/heal) is `lifecycle`.

use std::path::Path;

use crate::session::extension_def::HOME_TOKEN;
use crate::session::ExtensionDef;
use crate::storage::Database;

use super::fs::{
    ensure_safe_relative, guard_removable_dir, is_user_modified, make_symlink,
    remove_dir_all_resilient, safe_join, set_executable,
};
use super::lifecycle::{activate_extension, deactivate_extension, DeactivateReport, EnsureReport};

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

    // Home precedence: `--home` > a manifest-pinned `home` > the derived default
    // under the config dir (`<extensions_dir>/<name>`). Official manifests omit
    // `home`, so they land in the derived default rather than the user's `$HOME`.
    let home_raw = home_override
        .map(str::to_string)
        .or_else(|| def.home.clone())
        .or_else(|| {
            crate::agent::extension_config::default_home(&def.name)
                .map(|p| p.to_string_lossy().into_owned())
        })
        .ok_or_else(|| {
            format!(
                "extension '{}': cannot resolve a default home (config dir unavailable); \
                 pass --home <dir>",
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
        .resolved_for_home(&home_str, crate::paths::home_dir().as_deref())
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
/// merged entries on uninstall — robust across payload schema changes. The
/// remote provisioning (`remote_hooks`) prunes on it too, paired with the
/// rewritten form's [`crate::session::REMOTE_HOOK_STATE_OPTION`] marker.
pub(crate) const HOOK_SIGNAL_MARKER: &str = "thurbox-cli session signal";

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
