use std::path::PathBuf;

use rusqlite::params;

use crate::session::SkillConfig;
use crate::sync::current_time_millis;

use super::Database;

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
