//! Extension activate/deactivate subcommands for `thurbox-cli`.
//!
//! Extensions declare the sessions/automations they need in an `extension.toml`
//! manifest under `~/.config/thurbox/extensions/`. `activate` (re)creates those
//! resources and marks the extension active so thurbox self-heals them if
//! deleted; `deactivate` tears them down and is the real off-switch. See
//! [`crate::session_ops::extensions`].

use clap::Subcommand;
use serde_json::{json, Value};

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
    /// Update an installed extension by re-fetching it from its recorded source
    /// (a bare name re-resolves to the running binary's release tag, so the
    /// matching version is pulled). Preserves user-edited files unless --force.
    Update {
        /// Extension name. Omit and pass --all to update every installed one.
        name: Option<String>,
        /// Update every installed extension instead of a single named one.
        #[arg(long)]
        all: bool,
        /// Also overwrite user-edited `substitute` files and `if_absent` seeds.
        #[arg(long)]
        force: bool,
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

pub fn run(action: Action, db: &Database) -> Result<Value, String> {
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
            Ok(install_report_to_json(&report))
        }
        Action::Uninstall { name, purge } => {
            let report = crate::session_ops::uninstall_extension(db, &name, purge)?;
            Ok(json!({
                "uninstalled": report.name,
                "sessions_deleted": report.deactivate.sessions_deleted,
                "automations_deleted": report.deactivate.automations_deleted,
                "agents_removed": report.agents_removed,
                "manifest_removed": report.manifest_removed,
                "home_removed": report.home_removed,
            }))
        }
        Action::List => {
            let defs = crate::agent::extension_config::list_manifests();
            let mut out = Vec::with_capacity(defs.len());
            for def in &defs {
                out.push(health_to_json(&crate::session_ops::extension_health(
                    db, def,
                )?));
            }
            Ok(Value::Array(out))
        }
        Action::Update { name, all, force } => match (name, all) {
            (Some(_), true) => Err("pass either a name or --all, not both".to_string()),
            (None, false) => {
                Err("specify an extension name, or --all to update every installed one".to_string())
            }
            (Some(name), false) => {
                let report = crate::session_ops::update_extension(db, &name, force)?;
                // Arm the heartbeat so the refreshed automations keep firing headlessly.
                arm_heartbeat();
                Ok(update_report_to_json(&report))
            }
            (None, true) => {
                let results = crate::session_ops::update_all_extensions(db, force);
                arm_heartbeat();
                let out: Vec<Value> = results
                    .into_iter()
                    .map(|(name, result)| match result {
                        Ok(report) => update_report_to_json(&report),
                        Err(e) => json!({ "name": name, "error": e }),
                    })
                    .collect();
                Ok(Value::Array(out))
            }
        },
        Action::Activate { name } => {
            let def = load_manifest(&name)?;
            let report = crate::session_ops::activate_extension(db, &def)?;
            // A `Send` automation only fires while something ticks it. Arm the
            // heartbeat keeper so the extension works headlessly (TUI closed),
            // matching how `automation create` arms it.
            arm_heartbeat();
            Ok(json!({
                "activated": def.name,
                "sessions_created": report.sessions_created,
                "automations_created": report.automations_created,
                "health": health_to_json(&crate::session_ops::extension_health(db, &def)?),
            }))
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
            Ok(json!({
                "deactivated": name,
                "was_active": report.was_active,
                "sessions_deleted": report.sessions_deleted,
                "automations_deleted": report.automations_deleted,
                "manifest_removed": manifest_removed,
            }))
        }
        Action::Status { name } => match name {
            Some(name) => {
                let def = load_manifest(&name)?;
                Ok(health_to_json(&crate::session_ops::extension_health(
                    db, &def,
                )?))
            }
            None => run(Action::List, db),
        },
    }
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

fn health_to_json(h: &crate::session_ops::ExtensionHealth) -> Value {
    json!({
        "name": h.name,
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

fn install_report_to_json(report: &crate::session_ops::InstallReport) -> Value {
    json!({
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
    json!({
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
