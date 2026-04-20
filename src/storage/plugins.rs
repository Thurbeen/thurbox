//! SQLite CRUD for plugin registry entries.
//!
//! The `plugins` table holds two kinds of rows:
//!
//! 1. **User-registered plugins** — plugins installed outside the admin dir,
//!    whose path lives wherever the user keeps them.
//! 2. **Shadow rows for disk-source plugins** — created lazily by
//!    `set_plugin_enabled` when a user disables an auto-discovered plugin, so
//!    the `enabled=false` flag survives restarts without moving files.
//!
//! `list_effective_plugins` merges disk-source plugins with the table, mirroring
//! the pattern in `storage/skills.rs:100-110`: registered rows win collisions.

use std::path::PathBuf;

use rusqlite::params;

use crate::session::{McpServerConfig, PluginConfig, PluginManifest, RoleConfig, SkillConfig};
use crate::sync::current_time_millis;

use super::Database;

/// Where a resolved plugin entry came from in the merged view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginSource {
    Disk,
    Registered,
}

/// Outcome of [`Database::uninstall_plugin_with_disk`].
#[derive(Debug, Clone)]
pub struct UninstallSummary {
    pub name: String,
    pub removed_disk: bool,
    pub removed_registry: bool,
    pub disk_path: PathBuf,
}

fn row_to_plugin_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<PluginConfig> {
    let name: String = row.get(0)?;
    let path: String = row.get(1)?;
    let version: String = row.get(2)?;
    let enabled: i64 = row.get(3)?;
    Ok(PluginConfig {
        name,
        path: PathBuf::from(path),
        version,
        enabled: enabled != 0,
    })
}

impl Database {
    /// List all registered plugins (rows in the `plugins` table), sorted by name.
    pub fn list_global_plugins(&self) -> rusqlite::Result<Vec<PluginConfig>> {
        let mut stmt = self.conn.prepare(
            "SELECT plugin_name, path, version, enabled \
             FROM plugins ORDER BY plugin_name",
        )?;
        let plugins = stmt
            .query_map([], row_to_plugin_config)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(plugins)
    }

    /// Insert or update a single registered plugin.
    pub fn upsert_global_plugin(&self, plugin: &PluginConfig) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        self.conn.execute(
            "INSERT INTO plugins \
             (plugin_name, path, version, enabled, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
             ON CONFLICT(plugin_name) DO UPDATE SET \
                 path = excluded.path, \
                 version = excluded.version, \
                 enabled = excluded.enabled, \
                 updated_at = excluded.updated_at",
            params![
                plugin.name,
                plugin.path.to_string_lossy().as_ref(),
                plugin.version,
                plugin.enabled as i64,
                now,
            ],
        )?;
        Ok(())
    }

    /// Atomically replace all registered plugins.
    pub fn replace_global_plugins(&self, plugins: &[PluginConfig]) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        self.conn.execute("DELETE FROM plugins", [])?;
        for plugin in plugins {
            self.conn.execute(
                "INSERT INTO plugins \
                 (plugin_name, path, version, enabled, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![
                    plugin.name,
                    plugin.path.to_string_lossy().as_ref(),
                    plugin.version,
                    plugin.enabled as i64,
                    now,
                ],
            )?;
        }
        Ok(())
    }

    /// Delete a single registered plugin. Returns true if it existed.
    pub fn delete_global_plugin(&self, name: &str) -> rusqlite::Result<bool> {
        let count = self
            .conn
            .execute("DELETE FROM plugins WHERE plugin_name = ?1", params![name])?;
        Ok(count > 0)
    }

    /// Uninstall a plugin: removes the directory under
    /// `~/.local/share/thurbox/admin/plugins/` (when applicable) AND deletes
    /// the registry row (cascades `plugin_settings`). For plugins registered
    /// outside the admin dir, only the registry row is removed.
    ///
    /// Shared by the MCP `uninstall_plugin` tool and the TUI Plugins-tab
    /// uninstall affordance so both surfaces apply identical rules.
    pub fn uninstall_plugin_with_disk(&self, name: &str) -> Result<UninstallSummary, String> {
        let registered = self
            .list_global_plugins()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|p| p.name == name);
        let plugins_dir =
            crate::paths::plugins_directory().ok_or("Could not resolve plugins directory")?;
        let disk_path = registered
            .as_ref()
            .map(|p| p.path.clone())
            .unwrap_or_else(|| plugins_dir.join(name));

        let mut removed_disk = false;
        if disk_path.starts_with(&plugins_dir) && disk_path.exists() {
            std::fs::remove_dir_all(&disk_path)
                .map_err(|e| format!("Failed to remove plugin directory: {e}"))?;
            removed_disk = true;
        }
        let removed_registry = self.delete_global_plugin(name).map_err(|e| e.to_string())?;

        if !removed_disk && !removed_registry {
            return Err(format!("Plugin not found: {name}"));
        }

        Ok(UninstallSummary {
            name: name.to_string(),
            removed_disk,
            removed_registry,
            disk_path,
        })
    }

    /// Set the `enabled` flag for a plugin by name.
    ///
    /// If the plugin isn't in the registry yet (disk-source only), creates a
    /// shadow row pointing at the disk-source path so the flag persists. The
    /// shadow row's path is resolved from `list_disk_plugins()`. Returns an
    /// error if the plugin can't be found on disk or in the registry.
    pub fn set_plugin_enabled(&self, name: &str, enabled: bool) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let updated = self.conn.execute(
            "UPDATE plugins SET enabled = ?1, updated_at = ?2 WHERE plugin_name = ?3",
            params![enabled as i64, now, name],
        )?;
        if updated > 0 {
            return Ok(());
        }
        // Shadow a disk-source plugin.
        let disk = crate::paths::list_disk_plugins()
            .into_iter()
            .find(|p| p.name == name);
        let Some(disk_plugin) = disk else {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        };
        self.upsert_global_plugin(&PluginConfig {
            name: disk_plugin.name,
            path: disk_plugin.path,
            version: disk_plugin.version,
            enabled,
        })?;
        Ok(())
    }

    /// List the effective plugin set — disk-source + registered, merged by name.
    ///
    /// Collision rule: **registered entries win over disk-source entries** with
    /// the same name (same semantics as `list_effective_skills`). The returned
    /// vector is deduped by name, sorted alphabetically, and each entry carries
    /// its origin.
    pub fn list_effective_plugins(&self) -> rusqlite::Result<Vec<(PluginConfig, PluginSource)>> {
        let mut by_name: std::collections::BTreeMap<String, (PluginConfig, PluginSource)> =
            std::collections::BTreeMap::new();
        for plugin in crate::paths::list_disk_plugins() {
            by_name.insert(plugin.name.clone(), (plugin, PluginSource::Disk));
        }
        for plugin in self.list_global_plugins()? {
            by_name.insert(plugin.name.clone(), (plugin, PluginSource::Registered));
        }
        Ok(by_name.into_values().collect())
    }

    /// All skill contributions from currently-enabled plugins, paired with the
    /// plugin's name so callers can attribute the contribution.
    ///
    /// A plugin whose manifest fails to parse contributes nothing (its error
    /// surfaces separately via `list_effective_plugins`). A single contribution
    /// row whose target directory is missing is silently skipped — admin-shipped
    /// content shouldn't crash the TUI on a typo.
    pub fn plugin_contributed_skills(&self) -> rusqlite::Result<Vec<(SkillConfig, String)>> {
        let mut out = Vec::new();
        for (plugin, _src) in self.list_effective_plugins()? {
            if !plugin.enabled {
                continue;
            }
            let Ok(manifest) = PluginManifest::load(&plugin.path) else {
                continue;
            };
            for c in manifest.contributes.skills {
                out.push((
                    SkillConfig {
                        name: c.name,
                        path: plugin.path.join(c.path),
                    },
                    plugin.name.clone(),
                ));
            }
        }
        Ok(out)
    }

    /// All role contributions from currently-enabled plugins. Role contribution
    /// `path` points at a TOML file; a parse failure on one file skips that
    /// contribution but doesn't disable the plugin.
    pub fn plugin_contributed_roles(&self) -> rusqlite::Result<Vec<(RoleConfig, String)>> {
        let mut out = Vec::new();
        for (plugin, _src) in self.list_effective_plugins()? {
            if !plugin.enabled {
                continue;
            }
            let Ok(manifest) = PluginManifest::load(&plugin.path) else {
                continue;
            };
            for c in manifest.contributes.roles {
                let path = plugin.path.join(&c.path);
                let Ok(raw) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(mut role) = toml::from_str::<RoleConfig>(&raw) else {
                    continue;
                };
                role.name = c.name;
                out.push((role, plugin.name.clone()));
            }
        }
        Ok(out)
    }

    /// All MCP server contributions from currently-enabled plugins.
    pub fn plugin_contributed_mcp_servers(
        &self,
    ) -> rusqlite::Result<Vec<(McpServerConfig, String)>> {
        let mut out = Vec::new();
        for (plugin, _src) in self.list_effective_plugins()? {
            if !plugin.enabled {
                continue;
            }
            let Ok(manifest) = PluginManifest::load(&plugin.path) else {
                continue;
            };
            for c in manifest.contributes.mcp_servers {
                let path = plugin.path.join(&c.path);
                let Ok(raw) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(mut server) = toml::from_str::<McpServerConfig>(&raw) else {
                    continue;
                };
                server.name = c.name;
                out.push((server, plugin.name.clone()));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> PluginConfig {
        PluginConfig {
            name: name.into(),
            path: PathBuf::from(format!("/tmp/{name}")),
            version: "0.1.0".into(),
            enabled: true,
        }
    }

    #[test]
    fn list_global_plugins_empty() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.list_global_plugins().unwrap().is_empty());
    }

    #[test]
    fn upsert_and_list_preserves_order_and_updates_fields() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_global_plugin(&sample("bravo")).unwrap();
        db.upsert_global_plugin(&sample("alpha")).unwrap();

        let plugins = db.list_global_plugins().unwrap();
        let names: Vec<&str> = plugins.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "bravo"]);

        let mut updated = sample("alpha");
        updated.version = "2.0.0".into();
        updated.enabled = false;
        db.upsert_global_plugin(&updated).unwrap();

        let reread = db.list_global_plugins().unwrap();
        assert_eq!(reread[0].version, "2.0.0");
        assert!(!reread[0].enabled);
    }

    #[test]
    fn replace_global_plugins_is_atomic() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_global_plugin(&sample("old")).unwrap();
        db.replace_global_plugins(&[sample("new")]).unwrap();
        let plugins = db.list_global_plugins().unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "new");
    }

    #[test]
    fn delete_global_plugin_returns_existence() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_global_plugin(&sample("one")).unwrap();
        assert!(db.delete_global_plugin("one").unwrap());
        assert!(!db.delete_global_plugin("one").unwrap());
    }

    fn seed_disk_plugin(dir: &std::path::Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("thurbox-plugin.toml"),
            format!("name = \"{name}\"\nversion = \"0.1.0\"\nthurbox_plugin_api = 1\n"),
        )
        .unwrap();
        p
    }

    #[test]
    fn list_effective_plugins_merges_disk_and_registry() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let disk_dir = crate::paths::plugins_directory().unwrap();
        seed_disk_plugin(&disk_dir, "ondisk");

        let db = Database::open_in_memory().unwrap();
        db.upsert_global_plugin(&sample("registered")).unwrap();

        let effective = db.list_effective_plugins().unwrap();
        let names: Vec<&str> = effective.iter().map(|(p, _)| p.name.as_str()).collect();
        assert_eq!(names, vec!["ondisk", "registered"]);
        assert_eq!(effective[0].1, PluginSource::Disk);
        assert_eq!(effective[1].1, PluginSource::Registered);
    }

    #[test]
    fn list_effective_plugins_registry_wins_on_collision() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let disk_dir = crate::paths::plugins_directory().unwrap();
        seed_disk_plugin(&disk_dir, "publish");

        let db = Database::open_in_memory().unwrap();
        db.upsert_global_plugin(&PluginConfig {
            name: "publish".into(),
            path: PathBuf::from("/user/override"),
            version: "9.9.9".into(),
            enabled: false,
        })
        .unwrap();

        let effective = db.list_effective_plugins().unwrap();
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0].1, PluginSource::Registered);
        assert_eq!(effective[0].0.path, PathBuf::from("/user/override"));
        assert!(!effective[0].0.enabled);
    }

    #[test]
    fn set_plugin_enabled_shadows_disk_only_plugin() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let disk_dir = crate::paths::plugins_directory().unwrap();
        seed_disk_plugin(&disk_dir, "onlyondisk");

        let db = Database::open_in_memory().unwrap();
        assert!(db.list_global_plugins().unwrap().is_empty());

        db.set_plugin_enabled("onlyondisk", false).unwrap();
        let reg = db.list_global_plugins().unwrap();
        assert_eq!(reg.len(), 1, "should have shadow row now");
        assert!(!reg[0].enabled);
    }

    #[test]
    fn set_plugin_enabled_errors_when_neither_disk_nor_registry() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        assert!(db.set_plugin_enabled("ghost", true).is_err());
    }

    /// Builds a disk plugin directory with a manifest declaring one skill,
    /// one role, and one mcp server contribution. Files are pre-created so
    /// callers can verify resolution.
    fn seed_full_plugin(dir: &std::path::Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::create_dir_all(p.join("skills/publish")).unwrap();
        std::fs::create_dir_all(p.join("roles")).unwrap();
        std::fs::create_dir_all(p.join("mcp")).unwrap();
        std::fs::write(p.join("skills/publish/SKILL.md"), "---\n").unwrap();
        std::fs::write(
            p.join("roles/reviewer.toml"),
            "name = \"reviewer\"\ndescription = \"from plugin\"\n",
        )
        .unwrap();
        std::fs::write(
            p.join("mcp/gh.toml"),
            "name = \"gh\"\ncommand = \"gh\"\nargs = [\"mcp\"]\n",
        )
        .unwrap();
        std::fs::write(
            p.join("thurbox-plugin.toml"),
            format!(
                "name = \"{name}\"\nversion = \"0.1.0\"\nthurbox_plugin_api = 1\n\
                 [[contributes.skills]]\nname = \"publish\"\npath = \"skills/publish\"\n\
                 [[contributes.roles]]\nname = \"reviewer\"\npath = \"roles/reviewer.toml\"\n\
                 [[contributes.mcp_servers]]\nname = \"gh\"\npath = \"mcp/gh.toml\"\n"
            ),
        )
        .unwrap();
        p
    }

    #[test]
    fn plugin_contributed_skills_resolves_relative_paths() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let dir = crate::paths::plugins_directory().unwrap();
        let plugin_dir = seed_full_plugin(&dir, "bundle");

        let db = Database::open_in_memory().unwrap();
        let skills = db.plugin_contributed_skills().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].0.name, "publish");
        assert_eq!(skills[0].0.path, plugin_dir.join("skills/publish"));
        assert_eq!(skills[0].1, "bundle");
    }

    #[test]
    fn plugin_contributed_roles_loads_toml_and_overrides_name() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let dir = crate::paths::plugins_directory().unwrap();
        seed_full_plugin(&dir, "bundle");

        let db = Database::open_in_memory().unwrap();
        let roles = db.plugin_contributed_roles().unwrap();
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].0.name, "reviewer");
        assert_eq!(roles[0].0.description, "from plugin");
    }

    #[test]
    fn plugin_contributed_mcp_servers_loads_toml() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let dir = crate::paths::plugins_directory().unwrap();
        seed_full_plugin(&dir, "bundle");

        let db = Database::open_in_memory().unwrap();
        let servers = db.plugin_contributed_mcp_servers().unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].0.command, "gh");
    }

    #[test]
    fn disabled_plugin_contributes_nothing() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let dir = crate::paths::plugins_directory().unwrap();
        seed_full_plugin(&dir, "bundle");

        let db = Database::open_in_memory().unwrap();
        db.set_plugin_enabled("bundle", false).unwrap();
        assert!(db.plugin_contributed_skills().unwrap().is_empty());
        assert!(db.plugin_contributed_roles().unwrap().is_empty());
        assert!(db.plugin_contributed_mcp_servers().unwrap().is_empty());
    }
}
