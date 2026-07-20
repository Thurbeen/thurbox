//! Editor command get/set subcommands.

use clap::Subcommand;
use serde_json::json;

use crate::cli::output::CommandOutput;
use crate::session::settings::EditorMode;
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
    /// Print or set how `Ctrl+O` launches the editor (`auto`/`terminal`/`gui`).
    /// `auto` (default) detects terminal vs GUI editors from the command name
    /// and gives terminal editors (vim, nano, ttt, …) a real TTY. `terminal`
    /// forces the TTY path for everything; `gui` forces the detached spawn.
    /// With no argument, prints the current mode.
    Mode {
        /// `auto`, `terminal`, or `gui`. Omit to print the current value.
        mode: Option<String>,
    },
}

pub fn run(action: Action, db: &Database) -> Result<CommandOutput, String> {
    match action {
        Action::Get => {
            let cmd = db
                .get_editor_command()
                .map_err(|e| format!("get_editor_command: {e}"))?;
            let human = match &cmd {
                Some(c) if !c.is_empty() => format!("Editor: {c}"),
                _ => "Editor: (not set)".to_string(),
            };
            Ok(CommandOutput::new(json!({ "command": cmd }), human))
        }
        Action::Set { command } => {
            db.set_editor_command(&command)
                .map_err(|e| format!("set_editor_command: {e}"))?;
            let human = if command.is_empty() {
                "Editor cleared.".to_string()
            } else {
                format!("Editor set to: {command}")
            };
            Ok(CommandOutput::new(
                json!({
                    "command": if command.is_empty() { None } else { Some(command) }
                }),
                human,
            ))
        }
        Action::Mode { mode } => match mode {
            None => {
                let current = db
                    .get_editor_mode()
                    .map_err(|e| format!("get_editor_mode: {e}"))?;
                Ok(CommandOutput::new(
                    json!({ "mode": current.as_db_value() }),
                    format!("Editor mode: {}", current.as_db_value()),
                ))
            }
            Some(value) => {
                let parsed = EditorMode::parse_stored(Some(&value));
                db.set_editor_mode(parsed)
                    .map_err(|e| format!("set_editor_mode: {e}"))?;
                Ok(CommandOutput::new(
                    json!({ "mode": parsed.as_db_value() }),
                    format!("Editor mode set to: {}", parsed.as_db_value()),
                ))
            }
        },
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

    #[test]
    fn mode_prints_auto_when_unset() {
        let db = db();
        let v = run(Action::Mode { mode: None }, &db).unwrap();
        assert_eq!(v["mode"].as_str(), Some("auto"));
    }

    #[test]
    fn mode_set_then_get_roundtrips() {
        let db = db();
        let v = run(
            Action::Mode {
                mode: Some("terminal".into()),
            },
            &db,
        )
        .unwrap();
        assert_eq!(v["mode"].as_str(), Some("terminal"));
        let v = run(Action::Mode { mode: None }, &db).unwrap();
        assert_eq!(v["mode"].as_str(), Some("terminal"));
    }

    #[test]
    fn mode_normalizes_aliases_and_case() {
        let db = db();
        // "tty" → terminal, case-insensitive.
        run(
            Action::Mode {
                mode: Some("TTY".into()),
            },
            &db,
        )
        .unwrap();
        let v = run(Action::Mode { mode: None }, &db).unwrap();
        assert_eq!(v["mode"].as_str(), Some("terminal"));
        // "detached" → gui.
        run(
            Action::Mode {
                mode: Some("detached".into()),
            },
            &db,
        )
        .unwrap();
        let v = run(Action::Mode { mode: None }, &db).unwrap();
        assert_eq!(v["mode"].as_str(), Some("gui"));
    }

    #[test]
    fn mode_setting_auto_clears_the_row() {
        let db = db();
        run(
            Action::Mode {
                mode: Some("terminal".into()),
            },
            &db,
        )
        .unwrap();
        run(
            Action::Mode {
                mode: Some("auto".into()),
            },
            &db,
        )
        .unwrap();
        // `auto` is the default, so the row is cleared.
        assert_eq!(
            db.get_editor_mode().unwrap(),
            crate::session::settings::EditorMode::Auto
        );
    }

    #[test]
    fn mode_unknown_value_falls_back_to_auto() {
        let db = db();
        let v = run(
            Action::Mode {
                mode: Some("nonsense".into()),
            },
            &db,
        )
        .unwrap();
        assert_eq!(v["mode"].as_str(), Some("auto"));
    }
}
