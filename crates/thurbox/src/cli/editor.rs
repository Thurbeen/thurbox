//! Editor command get/set subcommands, over `interface.json`
//! ([`crate::config::interface_state`]).

use clap::Subcommand;
use serde_json::json;

use crate::cli::output::CommandOutput;
use crate::session::settings::EditorMode;

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

pub fn run(action: Action) -> Result<CommandOutput, String> {
    let write = |change: &dyn Fn(&mut crate::config::interface_state::InterfaceState)| {
        crate::config::interface_state::update(|state| change(state))
            .map_err(|e| format!("write interface.json: {e}"))
    };
    match action {
        Action::Get => {
            let cmd = crate::config::interface_state::load().editor_command;
            let human = match &cmd {
                Some(c) => format!("Editor: {c}"),
                None => "Editor: (not set)".to_string(),
            };
            Ok(CommandOutput::new(json!({ "command": cmd }), human))
        }
        Action::Set { command } => {
            let stored = (!command.is_empty()).then_some(command);
            write(&|state| state.editor_command = stored.clone())?;
            let human = match &stored {
                None => "Editor cleared.".to_string(),
                Some(command) => format!("Editor set to: {command}"),
            };
            Ok(CommandOutput::new(json!({ "command": stored }), human))
        }
        Action::Mode { mode: None } => {
            let current = crate::config::interface_state::load().editor_mode;
            Ok(CommandOutput::new(
                json!({ "mode": current.as_db_value() }),
                format!("Editor mode: {}", current.as_db_value()),
            ))
        }
        Action::Mode { mode: Some(value) } => {
            let parsed = EditorMode::parse_stored(Some(&value));
            write(&|state| state.editor_mode = parsed)?;
            Ok(CommandOutput::new(
                json!({ "mode": parsed.as_db_value() }),
                format!("Editor mode set to: {}", parsed.as_db_value()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated() -> (tempfile::TempDir, crate::paths::TestPathGuard) {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::paths::TestPathGuard::new(dir.path());
        (dir, guard)
    }

    #[test]
    fn set_then_get_roundtrips_and_empty_clears() {
        let _env = isolated();
        assert!(run(Action::Get).unwrap()["command"].is_null());
        let v = run(Action::Set {
            command: "code --wait".into(),
        })
        .unwrap();
        assert_eq!(v["command"].as_str(), Some("code --wait"));
        assert_eq!(
            run(Action::Get).unwrap()["command"].as_str(),
            Some("code --wait")
        );
        run(Action::Set {
            command: String::new(),
        })
        .unwrap();
        assert!(run(Action::Get).unwrap()["command"].is_null());
    }

    #[test]
    fn mode_normalizes_aliases_and_unknown_values_fall_back_to_auto() {
        let _env = isolated();
        let mode = || run(Action::Mode { mode: None }).unwrap()["mode"].clone();
        assert_eq!(mode(), "auto");
        for (given, stored) in [
            ("TTY", "terminal"),
            ("detached", "gui"),
            ("nonsense", "auto"),
        ] {
            run(Action::Mode {
                mode: Some(given.into()),
            })
            .unwrap();
            assert_eq!(mode(), stored, "{given}");
        }
    }
}
