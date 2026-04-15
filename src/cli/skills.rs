//! Skill registry CRUD subcommands.
//!
//! Skills are references to on-disk skill directories (each containing a
//! `SKILL.md`). Thurbox never creates, edits, or deletes skill files —
//! these commands only manage the registry rows in SQLite.

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::Value;

use crate::session::SkillConfig;
use crate::storage::Database;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// List all registered skills.
    List,
    /// Register (or update) a single skill reference.
    Register {
        /// Skill name (unique; 1-64 chars, no slashes or leading dot).
        #[arg(long)]
        name: String,
        /// Absolute path to the skill directory (must contain SKILL.md).
        #[arg(long)]
        path: PathBuf,
    },
    /// Unregister a skill (disk files are not touched).
    Unregister {
        /// Skill name to remove from the registry.
        name: String,
    },
    /// Atomically replace the full skill registry from a JSON file.
    ///
    /// The file must contain `[{"name": ..., "path": ...}, ...]` or
    /// `{"skills": [...]}`. Each `path` must be absolute, exist, and
    /// contain a SKILL.md file.
    Set {
        /// Path to a JSON file with the desired skill list.
        #[arg(long)]
        file: PathBuf,
    },
}

pub fn run(action: Action, db: &Database) -> Result<Value, String> {
    match action {
        Action::List => {
            let skills = db
                .list_global_skills()
                .map_err(|e| format!("list_global_skills: {e}"))?;
            Ok(serde_json::to_value(skills).map_err(|e| e.to_string())?)
        }
        Action::Register { name, path } => {
            super::validate_safe_name(&name)?;
            crate::paths::validate_skill_path(&path)?;
            let skill = SkillConfig { name, path };
            db.upsert_global_skill(&skill)
                .map_err(|e| format!("upsert_global_skill: {e}"))?;
            Ok(serde_json::to_value(&skill).map_err(|e| e.to_string())?)
        }
        Action::Unregister { name } => {
            let removed = db
                .delete_global_skill(&name)
                .map_err(|e| format!("delete_global_skill: {e}"))?;
            if !removed {
                return Err(format!("Skill not registered: {name}"));
            }
            Ok(serde_json::json!({ "deleted": true, "name": name }))
        }
        Action::Set { file } => {
            let content = super::read_file(&file)?;
            let arr = super::parse_array_or_wrapper(&content, "skills")?;
            let mut skills: Vec<SkillConfig> = Vec::with_capacity(arr.len());
            for v in arr {
                let name = v
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("skill.name required")?
                    .to_string();
                let path = v
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("skill.path required")?;
                super::validate_safe_name(&name)?;
                let path = PathBuf::from(path);
                crate::paths::validate_skill_path(&path)
                    .map_err(|e| format!("skill '{name}': {e}"))?;
                skills.push(SkillConfig { name, path });
            }
            db.replace_global_skills(&skills)
                .map_err(|e| format!("replace_global_skills: {e}"))?;
            Ok(serde_json::to_value(&skills).map_err(|e| e.to_string())?)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn make_skill_dir(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        dir
    }

    #[test]
    fn list_is_empty_by_default() {
        let v = run(Action::List, &db()).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 0);
    }

    #[test]
    fn register_then_list_then_unregister() {
        let tmp = tempfile::tempdir().unwrap();
        let path = make_skill_dir(tmp.path(), "my-skill");
        let db = db();

        run(
            Action::Register {
                name: "my-skill".into(),
                path: path.clone(),
            },
            &db,
        )
        .unwrap();

        let v = run(Action::List, &db).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"].as_str(), Some("my-skill"));

        run(
            Action::Unregister {
                name: "my-skill".into(),
            },
            &db,
        )
        .unwrap();
        assert_eq!(run(Action::List, &db).unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn register_rejects_missing_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let err = run(
            Action::Register {
                name: "x".into(),
                path: empty,
            },
            &db(),
        )
        .unwrap_err();
        assert!(err.contains("SKILL.md"), "got: {err}");
    }

    #[test]
    fn register_rejects_relative_path() {
        let err = run(
            Action::Register {
                name: "x".into(),
                path: PathBuf::from("relative/path"),
            },
            &db(),
        )
        .unwrap_err();
        assert!(err.contains("absolute"), "got: {err}");
    }

    #[test]
    fn unregister_unknown_fails() {
        let err = run(
            Action::Unregister {
                name: "nope".into(),
            },
            &db(),
        )
        .unwrap_err();
        assert!(err.contains("not registered"), "got: {err}");
    }
}
