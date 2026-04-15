//! Editor command get/set subcommands.

use clap::Subcommand;
use serde_json::Value;

use crate::storage::Database;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Print the configured editor command (or null).
    Get,
    /// Set the editor command (pass empty string to clear).
    Set {
        /// Command line template. The worktree path is appended as a final arg.
        command: String,
    },
}

pub fn run(action: Action, db: &Database) -> Result<Value, String> {
    match action {
        Action::Get => {
            let cmd = db
                .get_editor_command()
                .map_err(|e| format!("get_editor_command: {e}"))?;
            Ok(serde_json::json!({ "command": cmd }))
        }
        Action::Set { command } => {
            db.set_editor_command(&command)
                .map_err(|e| format!("set_editor_command: {e}"))?;
            Ok(serde_json::json!({
                "command": if command.is_empty() { None } else { Some(command) }
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn get_returns_null_when_unset() {
        let db = db();
        let v = run(Action::Get, &db).unwrap();
        assert!(v.get("command").unwrap().is_null());
    }

    #[test]
    fn set_then_get_roundtrips() {
        let db = db();
        let v = run(
            Action::Set {
                command: "code --wait".into(),
            },
            &db,
        )
        .unwrap();
        assert_eq!(v["command"].as_str(), Some("code --wait"));
        let v = run(Action::Get, &db).unwrap();
        assert_eq!(v["command"].as_str(), Some("code --wait"));
    }

    #[test]
    fn set_empty_clears() {
        let db = db();
        run(
            Action::Set {
                command: "vim".into(),
            },
            &db,
        )
        .unwrap();
        let v = run(
            Action::Set {
                command: String::new(),
            },
            &db,
        )
        .unwrap();
        assert!(v["command"].is_null());
    }
}
