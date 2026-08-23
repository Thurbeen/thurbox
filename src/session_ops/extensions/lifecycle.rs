//! The runtime half of the extension machinery: idempotently ensuring the
//! sessions/automations a manifest declares, activation/deactivation (the
//! active set self-heal reads), the self-heal pass itself, and the health
//! snapshot `extension status` renders. The payload half is `install`.

use std::collections::HashMap;

use crate::session::automation::parse_trigger;
use crate::session::{Automation, AutomationAction, ExtensionAutomation, ExtensionDef, SessionId};
use crate::storage::automations::NewAutomation;
use crate::storage::Database;
use crate::sync::current_time_millis;

use super::install::update_extension;

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

/// Idempotently ensure every resource a manifest declares exists. Existing
/// sessions/automations are matched by name and reused; only the missing ones
/// are created. An existing `Send` automation whose stored target id no longer
/// matches its session's current id is re-linked (a recreated session would
/// otherwise orphan it). Safe to call repeatedly (this is the self-heal
/// primitive).
pub fn ensure_extension(db: &Database, def: &ExtensionDef) -> Result<EnsureReport, String> {
    let mut report = EnsureReport::default();
    let mut session_ids: HashMap<String, SessionId> = HashMap::new();

    // Snapshot existing rows once instead of re-listing per declared resource:
    // self-heal runs this on every heartbeat tick. Declared names are unique, so
    // a pre-loop snapshot is correct for the existence lookups below.
    let existing_sessions: HashMap<String, SessionId> = db
        .list_active_sessions()
        .map_err(|e| format!("list_active_sessions: {e}"))?
        .into_iter()
        .map(|row| (row.name, row.id))
        .collect();
    let existing_automations: HashMap<String, Automation> = db
        .list_automations()
        .map_err(|e| format!("list_automations: {e}"))?
        .into_iter()
        .map(|row| (row.name.clone(), row))
        .collect();

    for sess in &def.sessions {
        let id = match existing_sessions.get(&sess.name) {
            Some(id) => *id,
            None => {
                let result = crate::session_ops::spawn_session_headless(
                    db,
                    crate::session_ops::SpawnRequest {
                        name: sess.name.clone(),
                        repo_path: sess.repo_path.clone(),
                        agent: Some(sess.agent.clone()),
                        ..Default::default()
                    },
                )?;
                report.sessions_created.push(sess.name.clone());
                result.session_id
            }
        };
        session_ids.insert(sess.name.clone(), id);
    }

    for auto in &def.automations {
        auto.validate()?;
        // An exec automation has no session; a send one resolves its target.
        let target = if auto.command.is_some() {
            None
        } else {
            let session_ref = auto.session_ref.as_deref().ok_or_else(|| {
                format!(
                    "automation '{}' has neither a command nor a session_ref",
                    auto.name
                )
            })?;
            Some(*session_ids.get(session_ref).ok_or_else(|| {
                format!(
                    "automation '{}' references unknown session '{session_ref}'",
                    auto.name
                )
            })?)
        };
        ensure_automation(
            db,
            auto,
            target,
            existing_automations.get(&auto.name),
            &mut report,
        )?;
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
    target: Option<SessionId>,
    existing: Option<&Automation>,
    report: &mut EnsureReport,
) -> Result<(), String> {
    // Desired action: a command wins (exec); otherwise send to the target.
    let action = match (&auto.command, target) {
        (Some(command), _) => AutomationAction::Exec {
            command: command.clone(),
        },
        (None, Some(session_id)) => AutomationAction::Send { session_id },
        (None, None) => {
            return Err(format!(
                "automation '{}' has neither a command nor a session_ref",
                auto.name
            ))
        }
    };
    if let Some(row) = existing {
        // Re-link a send automation whose target session was recreated (a new id).
        if let (AutomationAction::Send { session_id }, Some(t)) = (&row.action, target) {
            if *session_id != t {
                let mut row = row.clone();
                row.action = AutomationAction::Send { session_id: t };
                db.update_automation(&row)
                    .map_err(|e| format!("update_automation: {e}"))?;
                report.automations_relinked.push(auto.name.clone());
            }
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
        action,
        prompt: auto.prompt.clone().unwrap_or_default(),
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

    // Snapshot existing rows once rather than re-listing per declared resource.
    let automation_ids: HashMap<String, i64> = db
        .list_automations()
        .map_err(|e| format!("list_automations: {e}"))?
        .into_iter()
        .map(|row| (row.name, row.id))
        .collect();
    for auto in &def.automations {
        if let Some(&id) = automation_ids.get(&auto.name) {
            db.delete_automation(id)
                .map_err(|e| format!("delete_automation: {e}"))?;
            report.automations_deleted.push(auto.name.clone());
        }
    }

    let session_ids: HashMap<String, SessionId> = db
        .list_active_sessions()
        .map_err(|e| format!("list_active_sessions: {e}"))?
        .into_iter()
        .map(|row| (row.name, row.id))
        .collect();
    for sess in &def.sessions {
        if let Some(&id) = session_ids.get(&sess.name) {
            crate::session_ops::delete_session_headless(db, id, force)?;
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
    // Handle binary-vs-extension version drift once per pass (warn / auto-update /
    // nudge). A successful auto-update re-activates the extension, so it already
    // re-ensured its resources — skip the trailing ensure (which would run against
    // the now-stale `def`).
    let current = crate::agent::extension_config::binary_version();
    let auto_update = crate::session::settings::global().features.auto_update;
    if heal_version_drift(db, &def, name, current, auto_update, messages) {
        return;
    }
    match ensure_extension(db, &def) {
        Ok(report) if report.created_anything() => {
            messages.push(heal_recreated_message(&report, name));
        }
        Ok(_) => {}
        Err(e) => messages.push(format!("extension '{name}' self-heal failed: {e}")),
    }
}

/// Reconcile a binary-vs-extension version mismatch during self-heal, appending
/// any user-facing message. Returns `true` when it auto-updated the extension
/// (the caller should then skip its own `ensure_extension`, since the update
/// already re-activated it). `current` / `auto_update` are passed in (not read
/// from the globals) so the branches are unit-testable without a real release
/// build or a write-once settings init.
///
/// The branches are mutually exclusive:
/// - binary *older* than the extension wants (`compat_warning`) → only warn; an
///   update can't fix it (the matching extension version targets a newer binary);
/// - installed under an older binary (`is_stale`) → auto-update in place when
///   `auto_update` is on (mirroring the binary self-update), else nudge the user
///   to run `extension update` by hand;
/// - otherwise → nothing. Dev builds never go stale, so they fall here.
pub(super) fn heal_version_drift(
    db: &Database,
    def: &ExtensionDef,
    name: &str,
    current: &str,
    auto_update: bool,
    messages: &mut Vec<String>,
) -> bool {
    if let Some(w) = def.compat_warning(current) {
        messages.push(w);
        return false;
    }
    if !def.is_stale(current) {
        return false;
    }
    if auto_update {
        match update_extension(db, name, false) {
            Ok(report) => {
                if report.changed {
                    messages.push(format!(
                        "Auto-updated extension '{name}' to v{}",
                        report.install.version.as_deref().unwrap_or("?")
                    ));
                }
                return true;
            }
            // Fall back to the manual nudge so the user can still act; the caller
            // then re-ensures resources with the (unchanged) current def.
            Err(e) => tracing::warn!("auto-update of extension '{name}' failed: {e}"),
        }
    }
    messages.push(stale_extension_nudge(def, name, current));
    false
}

/// The "installed under an older binary — run `extension update`" nudge, shown
/// by self-heal when an extension is stale and auto-update is off (or failed).
fn stale_extension_nudge(def: &ExtensionDef, name: &str, current: &str) -> String {
    format!(
        "extension '{name}' was installed under thurbox {} but this binary is {current}; \
         run `thurbox-cli extension update {name}` to refresh it",
        def.installed_with.as_deref().unwrap_or("an older version")
    )
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
