use std::path::PathBuf;

use rusqlite::params;

use crate::session::SkillConfig;
use crate::sync::current_time_millis;

use super::Database;

/// Where a resolved skill came from in the merged view.
///
/// The TUI and MCP both need to show end-users which skills are admin-shipped
/// defaults vs. user-registered references. Kept small and serde-friendly so
/// it can ride along with `SkillConfig` in JSON responses without forcing
/// `SkillConfig` itself to carry a source field.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    /// Found on disk under `~/.local/share/thurbox/admin/skills/`.
    Disk,
    /// Registered in the SQLite `skills` table.
    Registered,
    /// Contributed by an enabled plugin's `[[contributes.skills]]` row. Carries
    /// the plugin name so the TUI can show "from `<plugin>`".
    Plugin(String),
}

/// Parse a row with columns (skill_name, path) into SkillConfig.
fn row_to_skill_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillConfig> {
    let name: String = row.get(0)?;
    let path: String = row.get(1)?;

    Ok(SkillConfig {
        name,
        path: PathBuf::from(path),
    })
}

impl Database {
    /// List all global skills.
    pub fn list_global_skills(&self) -> rusqlite::Result<Vec<SkillConfig>> {
        let mut stmt = self.conn.prepare(
            "SELECT skill_name, path \
             FROM skills ORDER BY skill_name",
        )?;

        let skills = stmt
            .query_map([], row_to_skill_config)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(skills)
    }

    /// Atomically replace all global skills (delete existing + insert new).
    pub fn replace_global_skills(&self, skills: &[SkillConfig]) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;

        self.conn.execute("DELETE FROM skills", [])?;

        for skill in skills {
            self.conn.execute(
                "INSERT INTO skills \
                 (skill_name, path, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![skill.name, skill.path.to_string_lossy().as_ref(), now, now,],
            )?;
        }

        Ok(())
    }

    /// Insert or update a single global skill.
    pub fn upsert_global_skill(&self, skill: &SkillConfig) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        self.conn.execute(
            "INSERT INTO skills \
             (skill_name, path, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(skill_name) DO UPDATE SET \
                 path = excluded.path, \
                 updated_at = excluded.updated_at",
            params![skill.name, skill.path.to_string_lossy().as_ref(), now, now,],
        )?;
        Ok(())
    }

    /// Delete a single global skill by name. Returns true if it existed.
    pub fn delete_global_skill(&self, name: &str) -> rusqlite::Result<bool> {
        let count = self
            .conn
            .execute("DELETE FROM skills WHERE skill_name = ?1", params![name])?;
        Ok(count > 0)
    }

    /// List the effective skill set — disk-source + registered — merged by name.
    ///
    /// Collision rule: **registered entries win over disk-source entries** with
    /// the same name. Rationale: disk-source skills are admin-shipped defaults
    /// that users are free to override by registering a different path. The
    /// returned vector is deduped by name and sorted alphabetically. Each
    /// entry carries its origin so callers can surface "disk" vs "registered"
    /// in UI.
    pub fn list_effective_skills(&self) -> rusqlite::Result<Vec<(SkillConfig, SkillSource)>> {
        let mut by_name: std::collections::BTreeMap<String, (SkillConfig, SkillSource)> =
            std::collections::BTreeMap::new();
        // Precedence (later inserts win): Disk → Plugin → Registered.
        for skill in crate::paths::list_disk_skills() {
            by_name.insert(skill.name.clone(), (skill, SkillSource::Disk));
        }
        for (skill, plugin_name) in self.plugin_contributed_skills()? {
            by_name.insert(
                skill.name.clone(),
                (skill, SkillSource::Plugin(plugin_name)),
            );
        }
        for skill in self.list_global_skills()? {
            by_name.insert(skill.name.clone(), (skill, SkillSource::Registered));
        }
        Ok(by_name.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_global_skills_empty() {
        let db = Database::open_in_memory().unwrap();
        let skills = db.list_global_skills().unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn replace_and_list_global_skills() {
        let db = Database::open_in_memory().unwrap();
        let skills = vec![
            SkillConfig {
                name: "domain-cli".to_string(),
                path: PathBuf::from("/home/user/.claude/skills/domain-cli"),
            },
            SkillConfig {
                name: "m01-ownership".to_string(),
                path: PathBuf::from("/home/user/.claude/skills/m01-ownership"),
            },
        ];

        db.replace_global_skills(&skills).unwrap();
        let loaded = db.list_global_skills().unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "domain-cli");
        assert_eq!(loaded[1].name, "m01-ownership");
        assert_eq!(
            loaded[1].path,
            PathBuf::from("/home/user/.claude/skills/m01-ownership")
        );
    }

    #[test]
    fn replace_global_skills_overwrites() {
        let db = Database::open_in_memory().unwrap();
        db.replace_global_skills(&[SkillConfig {
            name: "old".to_string(),
            path: PathBuf::from("/old/path"),
        }])
        .unwrap();

        db.replace_global_skills(&[SkillConfig {
            name: "new".to_string(),
            path: PathBuf::from("/new/path"),
        }])
        .unwrap();

        let loaded = db.list_global_skills().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "new");
    }

    #[test]
    fn upsert_global_skill_insert_and_update() {
        let db = Database::open_in_memory().unwrap();
        let skill = SkillConfig {
            name: "test-skill".to_string(),
            path: PathBuf::from("/path/v1"),
        };
        db.upsert_global_skill(&skill).unwrap();

        let loaded = db.list_global_skills().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].path, PathBuf::from("/path/v1"));

        let updated = SkillConfig {
            name: "test-skill".to_string(),
            path: PathBuf::from("/path/v2"),
        };
        db.upsert_global_skill(&updated).unwrap();

        let loaded = db.list_global_skills().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].path, PathBuf::from("/path/v2"));
    }

    fn seed_disk_skill(dir: &std::path::Path, name: &str) -> PathBuf {
        let skill = dir.join(name);
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\n").unwrap();
        skill
    }

    #[test]
    fn list_effective_skills_merges_disk_and_registry() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let disk_dir = crate::paths::skills_directory().unwrap();
        seed_disk_skill(&disk_dir, "publish");
        seed_disk_skill(&disk_dir, "orchestrate");

        let db = Database::open_in_memory().unwrap();
        db.upsert_global_skill(&SkillConfig {
            name: "custom".into(),
            path: PathBuf::from("/some/where"),
        })
        .unwrap();

        let effective = db.list_effective_skills().unwrap();
        let names: Vec<&str> = effective.iter().map(|(s, _)| s.name.as_str()).collect();
        assert_eq!(names, vec!["custom", "orchestrate", "publish"]);
        assert_eq!(effective[0].1, SkillSource::Registered);
        assert_eq!(effective[1].1, SkillSource::Disk);
        assert_eq!(effective[2].1, SkillSource::Disk);
    }

    #[test]
    fn list_effective_skills_registry_wins_on_collision() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let disk_dir = crate::paths::skills_directory().unwrap();
        let disk_path = seed_disk_skill(&disk_dir, "publish");

        let db = Database::open_in_memory().unwrap();
        db.upsert_global_skill(&SkillConfig {
            name: "publish".into(),
            path: PathBuf::from("/user/override/publish"),
        })
        .unwrap();

        let effective = db.list_effective_skills().unwrap();
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0].0.name, "publish");
        assert_eq!(
            effective[0].0.path,
            PathBuf::from("/user/override/publish"),
            "registered entry must shadow disk-source skill of the same name"
        );
        assert_eq!(effective[0].1, SkillSource::Registered);
        // Disk entry is still on disk — the registry just hides it.
        assert!(disk_path.exists());
    }

    fn seed_plugin_with_skill(
        dir: &std::path::Path,
        plugin_name: &str,
        skill_name: &str,
    ) -> PathBuf {
        let p = dir.join(plugin_name);
        let skill_dir = p.join("skills").join(skill_name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "---\n").unwrap();
        std::fs::write(
            p.join("thurbox-plugin.toml"),
            format!(
                "name = \"{plugin_name}\"\nversion = \"0.1.0\"\nthurbox_plugin_api = 1\n\
                 [[contributes.skills]]\nname = \"{skill_name}\"\npath = \"skills/{skill_name}\"\n"
            ),
        )
        .unwrap();
        p
    }

    #[test]
    fn list_effective_skills_precedence_registered_beats_plugin_beats_disk() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let disk_dir = crate::paths::skills_directory().unwrap();
        let disk_path = seed_disk_skill(&disk_dir, "common");
        seed_disk_skill(&disk_dir, "disk-only");

        let plugin_dir = crate::paths::plugins_directory().unwrap();
        let plugin_path = seed_plugin_with_skill(&plugin_dir, "bundle", "common");
        seed_plugin_with_skill(&plugin_dir, "bundle2", "plugin-only");

        let db = Database::open_in_memory().unwrap();
        db.upsert_global_skill(&SkillConfig {
            name: "common".into(),
            path: PathBuf::from("/user/override"),
        })
        .unwrap();

        let effective = db.list_effective_skills().unwrap();
        let by_name: std::collections::HashMap<_, _> = effective
            .iter()
            .map(|(s, src)| (s.name.as_str(), (s.path.clone(), src.clone())))
            .collect();

        assert_eq!(
            by_name["common"].1,
            SkillSource::Registered,
            "registered must beat both plugin and disk"
        );
        assert_eq!(by_name["common"].0, PathBuf::from("/user/override"));
        assert_eq!(
            by_name["plugin-only"].1,
            SkillSource::Plugin("bundle2".into())
        );
        assert_eq!(by_name["disk-only"].1, SkillSource::Disk);
        // Disk + plugin files still on disk; registry just hides the colliding one.
        assert!(disk_path.exists() && plugin_path.exists());
    }

    #[test]
    fn delete_global_skill() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_global_skill(&SkillConfig {
            name: "to-delete".to_string(),
            path: PathBuf::from("/path"),
        })
        .unwrap();

        assert!(db.delete_global_skill("to-delete").unwrap());
        assert!(!db.delete_global_skill("to-delete").unwrap());
        assert!(db.list_global_skills().unwrap().is_empty());
    }
}
