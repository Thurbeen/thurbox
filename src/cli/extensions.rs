//! Extension activate/deactivate subcommands for `thurbox-cli`.
//!
//! Extensions declare the sessions/automations they need in an `extension.toml`
//! manifest under `~/.config/thurbox/extensions/`. `activate` (re)creates those
//! resources and marks the extension active so thurbox self-heals them if
//! deleted; `deactivate` tears them down and is the real off-switch. See
//! [`crate::session_ops::extensions`].

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::output::{self, CommandOutput};
use crate::session::ExtensionDef;
use crate::storage::Database;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Install an extension from a name, URL, or local directory: fetch it, lay
    /// down its files, register its agents, and activate it. Idempotent.
    Install {
        /// What to install: a bare name (`flow`, from the official source), an
        /// `http(s)://` base URL, or a path to a local extension directory.
        target: String,
        /// Override the install home directory (default: the manifest's `home`).
        #[arg(long)]
        home: Option<String>,
        /// Re-write even `if_absent` seed files (e.g. reset `repos.md`).
        #[arg(long)]
        force: bool,
    },
    /// Uninstall an extension: tear down its session/automation, remove its
    /// agents from agents.toml, and delete its manifest (the reverse of install).
    Uninstall {
        /// Extension name.
        name: String,
        /// Also delete the install home directory (payload + any data under it).
        #[arg(long)]
        purge: bool,
    },
    /// List installed extensions and whether each is active/healthy.
    List,
    /// List the official extensions available to install (name + description +
    /// whether already installed), optionally filtered by a query.
    #[command(alias = "search")]
    Available {
        /// Filter by a substring matched against name or description.
        query: Option<String>,
    },
    /// Update installed extensions by re-fetching from their recorded source (a
    /// bare name re-resolves to the running binary's release tag, so the matching
    /// version is pulled). With no name, updates **all** installed extensions.
    /// Preserves user-edited files unless --force.
    Update {
        /// Extension name. Omit to update every installed extension.
        name: Option<String>,
        /// Update every installed extension (the default when no name is given).
        #[arg(long)]
        all: bool,
        /// Also overwrite user-edited `substitute` files and `if_absent` seeds.
        #[arg(long)]
        force: bool,
    },
    /// Clean-slate reinstall: uninstall the extension (tear down its resources,
    /// remove its agents + manifest) then install it fresh from its recorded
    /// source, overwriting user-edited seed/`substitute` files. Heavier than
    /// `update --force`, which only refreshes payload files in place.
    Reinstall {
        /// Extension name.
        name: String,
        /// Also delete + recreate the install home directory (full reset,
        /// discarding any user data under it).
        #[arg(long)]
        purge: bool,
    },
    /// Activate an extension: (re)create its sessions/automations and mark it
    /// active so thurbox self-heals them. Idempotent.
    Activate {
        /// Extension name (matches `<name>.toml` in the extensions dir).
        name: String,
    },
    /// Deactivate an extension: tear down its resources and stop self-healing.
    Deactivate {
        /// Extension name.
        name: String,
        /// Also tear down each session's tmux window + worktrees (not just a
        /// soft delete).
        #[arg(long)]
        force: bool,
        /// Also remove the extension's manifest from the discovery dir, so it no
        /// longer appears in `extension list`. Leaves the extension's own files
        /// (e.g. its home dir) alone — use the extension's uninstall for those.
        #[arg(long)]
        purge: bool,
    },
    /// Show one extension's resource health (or all when no name is given).
    Status {
        /// Extension name; omit to report on every installed extension.
        name: Option<String>,
    },
}

pub fn run(action: Action, db: &Database) -> Result<CommandOutput, String> {
    match action {
        Action::Install {
            target,
            home,
            force,
        } => {
            let report =
                crate::session_ops::install_extension(db, &target, home.as_deref(), force)?;
            // Arm the heartbeat so the extension's automations fire headlessly.
            arm_heartbeat();
            Ok(CommandOutput::from_summary(install_report_to_json(&report)))
        }
        Action::Uninstall { name, purge } => {
            let report = crate::session_ops::uninstall_extension(db, &name, purge)?;
            Ok(CommandOutput::from_summary(json!({
                "ok": true,
                "summary": format!("Uninstalled extension '{}'", report.name),
                "uninstalled": report.name,
                "sessions_deleted": report.deactivate.sessions_deleted,
                "automations_deleted": report.deactivate.automations_deleted,
                "agents_removed": report.agents_removed,
                "manifest_removed": report.manifest_removed,
                "home_removed": report.home_removed,
            })))
        }
        Action::List => {
            let defs = crate::agent::extension_config::list_manifests();
            let mut healths = Vec::with_capacity(defs.len());
            for def in &defs {
                healths.push(crate::session_ops::extension_health(db, def)?);
            }
            let json = Value::Array(healths.iter().map(health_to_json).collect());
            Ok(CommandOutput::new(json, render_extension_list(&healths)))
        }
        Action::Available { query } => Ok(available_output(query.as_deref())),
        Action::Update { name, all, force } => match name {
            // `--all` alongside a name is contradictory; otherwise a bare name
            // updates one and *no* name updates them all (no flag required).
            Some(_) if all => Err("pass either a name or --all, not both".to_string()),
            Some(name) => {
                let report = crate::session_ops::update_extension(db, &name, force)?;
                // Arm the heartbeat so the refreshed automations keep firing headlessly.
                arm_heartbeat();
                Ok(CommandOutput::from_summary(update_report_to_json(&report)))
            }
            None => {
                let results = crate::session_ops::update_all_extensions(db, force);
                arm_heartbeat();
                let total = results.len();
                let mut changed = 0;
                let mut failed = 0;
                let out: Vec<Value> = results
                    .into_iter()
                    .map(|(name, result)| match result {
                        Ok(report) => {
                            if report.changed {
                                changed += 1;
                            }
                            update_report_to_json(&report)
                        }
                        Err(e) => {
                            failed += 1;
                            json!({ "name": name, "ok": false, "error": e })
                        }
                    })
                    .collect();
                let summary = if total == 0 {
                    "No extensions installed".to_string()
                } else {
                    format!("Updated {total} extension(s): {changed} changed, {failed} failed")
                };
                Ok(CommandOutput::from_summary(
                    json!({ "ok": failed == 0, "summary": summary, "extensions": out }),
                ))
            }
        },
        Action::Reinstall { name, purge } => {
            let report = crate::session_ops::reinstall_extension(db, &name, purge)?;
            arm_heartbeat();
            let version = report.install.version.as_deref().unwrap_or("?");
            Ok(CommandOutput::from_summary(json!({
                "ok": true,
                "summary": format!(
                    "Reinstalled '{}' {} → {}",
                    report.name, version, report.install.home
                ),
                "reinstalled": report.name,
                "home_removed": report.uninstall.home_removed,
                "home": report.install.home,
                "version": report.install.version,
                "files_written": report.install.files_written,
                "agents_added": report.install.agents_added,
                "sessions_created": report.install.ensure.sessions_created,
                "automations_created": report.install.ensure.automations_created,
            })))
        }
        Action::Activate { name } => {
            let def = load_manifest(&name)?;
            let report = crate::session_ops::activate_extension(db, &def)?;
            // A `Send` automation only fires while something ticks it. Arm the
            // heartbeat keeper so the extension works headlessly (TUI closed),
            // matching how `automation create` arms it.
            arm_heartbeat();
            Ok(CommandOutput::from_summary(json!({
                "ok": true,
                "summary": format!(
                    "Activated '{}' ({} session(s), {} automation(s))",
                    def.name,
                    report.sessions_created.len(),
                    report.automations_created.len(),
                ),
                "activated": def.name,
                "sessions_created": report.sessions_created,
                "automations_created": report.automations_created,
                "health": health_to_json(&crate::session_ops::extension_health(db, &def)?),
            })))
        }
        Action::Deactivate { name, force, purge } => {
            // Tear down whatever the manifest declares. If the manifest is gone
            // we can't know the resources, but still clear the active-set entry.
            let report = match crate::agent::extension_config::load_manifest(&name) {
                Some(def) => crate::session_ops::deactivate_extension(db, &def, force)?,
                None => {
                    let was_active = db
                        .remove_active_extension(&name)
                        .map_err(|e| format!("remove_active_extension: {e}"))?;
                    crate::session_ops::DeactivateReport {
                        was_active,
                        ..Default::default()
                    }
                }
            };
            let manifest_removed = if purge {
                crate::agent::extension_config::remove_manifest_file(&name)?
            } else {
                false
            };
            let summary = if report.was_active {
                format!("Deactivated '{name}'")
            } else {
                format!("'{name}' was not active")
            };
            Ok(CommandOutput::from_summary(json!({
                "ok": true,
                "summary": summary,
                "deactivated": name,
                "was_active": report.was_active,
                "sessions_deleted": report.sessions_deleted,
                "automations_deleted": report.automations_deleted,
                "manifest_removed": manifest_removed,
            })))
        }
        Action::Status { name } => match name {
            Some(name) => {
                let def = load_manifest(&name)?;
                let health = crate::session_ops::extension_health(db, &def)?;
                Ok(CommandOutput::new(
                    health_to_json(&health),
                    render_extension_detail(&health),
                ))
            }
            None => run(Action::List, db),
        },
    }
}

/// Render the installed-extension list as an aligned table.
fn render_extension_list(healths: &[crate::session_ops::ExtensionHealth]) -> String {
    if healths.is_empty() {
        return "No extensions installed.".to_string();
    }
    let rows: Vec<Vec<String>> = healths
        .iter()
        .map(|h| {
            vec![
                h.name.clone(),
                if h.active { "active" } else { "inactive" }.to_string(),
                if h.is_healthy() { "ok" } else { "degraded" }.to_string(),
                if h.stale { "yes" } else { "no" }.to_string(),
                output::dash(h.version.as_deref()),
            ]
        })
        .collect();
    output::table(&["NAME", "STATE", "HEALTH", "STALE", "VERSION"], &rows)
}

/// Render a single extension's health as a key/value block + resource lines.
fn render_extension_detail(h: &crate::session_ops::ExtensionHealth) -> String {
    let mut pairs: Vec<(&str, String)> = vec![
        ("name", h.name.clone()),
        ("status", health_summary(h)),
        ("active", h.active.to_string()),
        ("healthy", h.is_healthy().to_string()),
        ("version", output::dash(h.version.as_deref())),
        ("stale", h.stale.to_string()),
    ];
    if let Some(w) = &h.compat_warning {
        pairs.push(("compat_warning", w.clone()));
    }
    let mut block = output::kv(&pairs);
    let resources = |label: &str, items: &[(String, bool)]| -> String {
        if items.is_empty() {
            return String::new();
        }
        let listed = items
            .iter()
            .map(|(n, present)| format!("{} {n}", if *present { "✓" } else { "✗" }))
            .collect::<Vec<_>>()
            .join(", ");
        format!("\n{label}:  {listed}")
    };
    block.push_str(&resources("sessions", &h.sessions));
    block.push_str(&resources("automations", &h.automations));
    block
}

/// Load a manifest by name or error with guidance.
fn load_manifest(name: &str) -> Result<ExtensionDef, String> {
    crate::agent::extension_config::load_manifest(name).ok_or_else(|| {
        format!(
            "No extension manifest '{name}' found. Install the extension first \
             (it writes ~/.config/thurbox/extensions/{name}.toml)."
        )
    })
}

/// Best-effort: ensure the tmux heartbeat keeper runs so the extension's
/// automations fire even when no TUI is attached. Non-fatal on failure.
fn arm_heartbeat() {
    let cli = crate::agent::tmux::resolve_cli_binary();
    if let Err(e) = crate::agent::tmux::ensure_automation_heartbeat(&cli) {
        eprintln!("warning: failed to arm automation heartbeat: {e}");
    }
}

/// Build the `extension available` result: every official extension with its
/// description, whether it's already installed, and the install command.
fn available_to_json(query: Option<&str>) -> Value {
    let q = query.map(|s| s.trim().to_lowercase());
    let installed: std::collections::HashSet<String> =
        crate::agent::extension_config::list_manifests()
            .into_iter()
            .map(|d| d.name)
            .collect();
    let mut out = Vec::new();
    for ext in crate::agent::extension_config::OFFICIAL_EXTENSIONS {
        if let Some(q) = &q {
            if !ext.name.to_lowercase().contains(q) && !ext.description.to_lowercase().contains(q) {
                continue;
            }
        }
        out.push(json!({
            "name": ext.name,
            "description": ext.description,
            "installed": installed.contains(ext.name),
            "install_command": format!("thurbox-cli extension install {}", ext.name),
        }));
    }
    json!({
        "ok": true,
        "summary": format!("{} official extension(s) available", out.len()),
        "available": out,
    })
}

/// `extension available`: the JSON above plus a human table (NAME / INSTALLED /
/// DESCRIPTION) built from the same `available` entries.
fn available_output(query: Option<&str>) -> CommandOutput {
    let json = available_to_json(query);
    let entries = json.get("available").and_then(Value::as_array);
    let human = match entries {
        Some(list) if !list.is_empty() => {
            let rows: Vec<Vec<String>> = list
                .iter()
                .map(|e| {
                    vec![
                        e["name"].as_str().unwrap_or("").to_string(),
                        if e["installed"].as_bool().unwrap_or(false) {
                            "yes"
                        } else {
                            "no"
                        }
                        .to_string(),
                        e["description"].as_str().unwrap_or("").to_string(),
                    ]
                })
                .collect();
            output::table(&["NAME", "INSTALLED", "DESCRIPTION"], &rows)
        }
        _ => "No matching extensions.".to_string(),
    };
    CommandOutput::new(json, human)
}

fn health_to_json(h: &crate::session_ops::ExtensionHealth) -> Value {
    json!({
        "name": h.name,
        "description": h.description,
        "summary": health_summary(h),
        "active": h.active,
        "healthy": h.is_healthy(),
        "version": h.version,
        "installed_with": h.installed_with,
        "current_binary": h.current_binary,
        "stale": h.stale,
        "compat_warning": h.compat_warning,
        "sessions": h.sessions.iter()
            .map(|(n, present)| json!({ "name": n, "present": present }))
            .collect::<Vec<_>>(),
        "automations": h.automations.iter()
            .map(|(n, present)| json!({ "name": n, "present": present }))
            .collect::<Vec<_>>(),
    })
}

/// A one-line human status for an extension's health (active/inactive, stale,
/// missing resources), for the `summary` field of `list`/`status`.
fn health_summary(h: &crate::session_ops::ExtensionHealth) -> String {
    if !h.active {
        return format!("'{}' is installed but not active", h.name);
    }
    if !h.is_healthy() {
        return format!(
            "'{}' is active but missing resources — run self-heal",
            h.name
        );
    }
    if h.stale {
        return format!(
            "'{}' is active but stale — run `thurbox-cli extension update {}`",
            h.name, h.name
        );
    }
    format!("'{}' is active and healthy", h.name)
}

fn install_report_to_json(report: &crate::session_ops::InstallReport) -> Value {
    let version = report.version.as_deref().unwrap_or("?");
    let summary = format!(
        "Installed '{}' {} → {} ({} file(s), {} session(s), {} automation(s))",
        report.name,
        version,
        report.home,
        report.files_written.len(),
        report.ensure.sessions_created.len(),
        report.ensure.automations_created.len(),
    );
    json!({
        "ok": true,
        "summary": summary,
        "installed": report.name,
        "home": report.home,
        "version": report.version,
        "previous_version": report.previous_version,
        "compat_warning": report.compat_warning,
        "files_written": report.files_written,
        "files_skipped": report.files_skipped,
        "symlinks_created": report.symlinks_created,
        "symlinks_skipped": report.symlinks_skipped,
        "agents_added": report.agents_added,
        "sessions_created": report.ensure.sessions_created,
        "automations_created": report.ensure.automations_created,
    })
}

fn update_report_to_json(report: &crate::session_ops::UpdateReport) -> Value {
    let summary = if report.changed {
        format!(
            "Updated '{}' {} → {}",
            report.name,
            report.install.previous_version.as_deref().unwrap_or("?"),
            report.install.version.as_deref().unwrap_or("?"),
        )
    } else {
        format!(
            "'{}' already up to date ({})",
            report.name,
            report.install.version.as_deref().unwrap_or("?"),
        )
    };
    json!({
        "ok": true,
        "summary": summary,
        "updated": report.name,
        "changed": report.changed,
        "previous_version": report.install.previous_version,
        "version": report.install.version,
        "compat_warning": report.install.compat_warning,
        "files_written": report.install.files_written,
        "files_skipped": report.install.files_skipped,
        "home": report.install.home,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_lists_official_extensions_with_install_commands() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let out = available_to_json(None);
        assert_eq!(out["ok"], true);
        let list = out["available"].as_array().unwrap();
        // Every official extension shows up, none installed in a fresh dir.
        let names: Vec<&str> = list.iter().map(|e| e["name"].as_str().unwrap()).collect();
        for ext in crate::agent::extension_config::OFFICIAL_EXTENSIONS {
            assert!(names.contains(&ext.name), "missing {}", ext.name);
        }
        let flow = list.iter().find(|e| e["name"] == "flow").unwrap();
        assert_eq!(flow["installed"], false);
        assert_eq!(
            flow["install_command"],
            "thurbox-cli extension install flow"
        );
    }

    #[test]
    fn available_query_filters_by_name_or_description() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        // Matches the renovate description ("dependencies").
        let out = available_to_json(Some("dependencies"));
        let list = out["available"].as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], "renovate");

        // A query matching nothing yields an empty list (not an error).
        let out = available_to_json(Some("nonexistent-xyz"));
        assert!(out["available"].as_array().unwrap().is_empty());
    }
}
