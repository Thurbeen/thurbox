//! Config introspection subcommands: `validate` and `show`.
//!
//! `validate` strictly parses every config file and fails (exit 1) when any
//! is invalid — usable as a dotfiles CI check. `show` prints the *effective*
//! resolved configuration and where each value came from.
//!
//! The agent module's loaders are reached via fully-qualified paths (no
//! `use crate::agent`) to keep the cli module free of an `agent` import —
//! see tests/architecture_rules.rs::cli_module_isolation.

use clap::Subcommand;
use serde_json::{json, Value};

use crate::storage::Database;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Parse every config file strictly; non-zero exit when any is invalid.
    Validate,
    /// Print the effective configuration and where each value came from.
    Show,
}

pub fn run(action: Action, db: &Database) -> Result<Value, String> {
    match action {
        Action::Validate => validate(),
        Action::Show => show(db),
    }
}

/// One file's validation outcome.
fn file_report(path: Option<std::path::PathBuf>, problems: Vec<String>, exists: bool) -> Value {
    json!({
        "path": path.map(|p| p.display().to_string()),
        "exists": exists,
        "valid": problems.is_empty(),
        "problems": problems,
    })
}

/// Validate one TOML config file against `T`. Absent files are valid
/// (defaults/seeding apply). Unknown fields don't fail the *load* at startup,
/// but they do fail *validation* — they are typos or stale keys either way.
fn validate_toml<T: serde::de::DeserializeOwned>(
    path: Option<std::path::PathBuf>,
    label: &str,
) -> (Value, bool) {
    let Some(path) = path else {
        return (
            file_report(None, vec!["could not resolve path".into()], false),
            false,
        );
    };
    if !path.exists() {
        return (file_report(Some(path), Vec::new(), false), true);
    }
    let problems = match std::fs::read_to_string(&path) {
        Ok(contents) => {
            match crate::agent::agent_config::parse_toml_reporting_unknown::<T>(&contents, label) {
                Ok((_, warnings)) => warnings,
                Err(e) => vec![e.to_string()],
            }
        }
        Err(e) => vec![format!("read failed: {e}")],
    };
    let ok = problems.is_empty();
    (file_report(Some(path), problems, true), ok)
}

fn validate_keybindings() -> (Value, bool) {
    let path = crate::paths::keybindings_file();
    match crate::storage::keybindings::load_keybindings_json() {
        Ok(Some(jsonbody)) => {
            let problems = match crate::session::KeyBindings::from_json_with_warnings(&jsonbody) {
                Ok((_, warnings)) => warnings,
                Err(e) => vec![e],
            };
            let ok = problems.is_empty();
            (file_report(path, problems, true), ok)
        }
        Ok(None) => (file_report(path, Vec::new(), false), true),
        Err(e) => (file_report(path, vec![e], true), false),
    }
}

fn validate() -> Result<Value, String> {
    let (agents, agents_ok) = validate_toml::<crate::session::AgentRegistry>(
        crate::agent::agent_config::agents_config_path(),
        "agents.toml",
    );
    let (hosts, hosts_ok) = validate_toml::<crate::session::HostRegistry>(
        crate::agent::host_config::hosts_config_path(),
        "hosts.toml",
    );
    let (settings, settings_ok) = validate_toml::<crate::session::settings::Settings>(
        crate::agent::settings_config::settings_config_path(),
        "settings.toml",
    );
    let (themes, themes_ok) = validate_toml::<crate::session::theme_config::ThemesFile>(
        crate::agent::themes_config::themes_config_path(),
        "themes.toml",
    );
    let (keybindings, kb_ok) = validate_keybindings();

    let failed: Vec<&str> = [
        ("agents.toml", agents_ok),
        ("hosts.toml", hosts_ok),
        ("settings.toml", settings_ok),
        ("themes.toml", themes_ok),
        ("keybindings.json", kb_ok),
    ]
    .iter()
    .filter(|(_, ok)| !ok)
    .map(|(name, _)| *name)
    .collect();

    let report = json!({
        "valid": failed.is_empty(),
        "agents_toml": agents,
        "hosts_toml": hosts,
        "settings_toml": settings,
        "themes_toml": themes,
        "keybindings_json": keybindings,
    });
    if failed.is_empty() {
        Ok(report)
    } else {
        // Print the full report on stdout (like the Ok path would), then fail
        // with a one-line error so the process exits non-zero — usable as a
        // dotfiles CI gate.
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string())
        );
        Err(format!("config invalid: {}", failed.join(", ")))
    }
}

fn show(db: &Database) -> Result<Value, String> {
    let agents = crate::agent::agent_config::load_or_seed();
    let hosts = crate::agent::host_config::load_or_seed();
    let settings = crate::session::settings::global();
    let (custom_themes, _) = crate::agent::themes_config::load_or_seed_with_warnings();

    // Editor resolution mirrors the TUI's Ctrl+O chain: DB → $VISUAL → $EDITOR.
    let db_editor = db.get_editor_command().ok().flatten();
    let (editor, editor_source) = match db_editor {
        Some(cmd) if !cmd.is_empty() => (Some(cmd), "database (editor_command)"),
        _ => match std::env::var("VISUAL") {
            Ok(v) if !v.is_empty() => (Some(v), "$VISUAL"),
            _ => match std::env::var("EDITOR") {
                Ok(v) if !v.is_empty() => (Some(v), "$EDITOR"),
                _ => (None, "unset"),
            },
        },
    };

    let overridden_actions: Vec<String> = match crate::storage::keybindings::load_keybindings_json()
    {
        Ok(Some(jsonbody)) => {
            serde_json::from_str::<std::collections::HashMap<String, Vec<String>>>(&jsonbody)
                .map(|m| {
                    let mut keys: Vec<String> = m.into_keys().collect();
                    keys.sort();
                    keys
                })
                .unwrap_or_default()
        }
        _ => Vec::new(),
    };

    Ok(json!({
        "paths": {
            "agents_toml": crate::agent::agent_config::agents_config_path()
                .map(|p| p.display().to_string()),
            "hosts_toml": crate::agent::host_config::hosts_config_path()
                .map(|p| p.display().to_string()),
            "settings_toml": crate::agent::settings_config::settings_config_path()
                .map(|p| p.display().to_string()),
            "themes_toml": crate::agent::themes_config::themes_config_path()
                .map(|p| p.display().to_string()),
            "keybindings_json": crate::paths::keybindings_file()
                .map(|p| p.display().to_string()),
            "database": crate::paths::database_file().map(|p| p.display().to_string()),
        },
        "agents": { "default": agents.default_name(), "names": agents.names() },
        "hosts": { "names": hosts.names() },
        "settings": settings,
        "keybindings": { "overridden_actions": overridden_actions },
        "editor": { "command": editor, "source": editor_source },
        "theme": db.get_active_theme().ok().flatten(),
        "custom_themes": custom_themes.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::TestPathGuard;

    #[test]
    fn validate_passes_on_fresh_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let v = validate().unwrap();
        assert_eq!(v["valid"], json!(true));
    }

    #[test]
    fn validate_fails_with_exit_error_on_malformed_agents_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let path = crate::agent::agent_config::agents_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not toml {{{").unwrap();

        let err = validate().unwrap_err();
        assert!(err.contains("agents.toml"), "got: {err}");
    }

    #[test]
    fn validate_reports_keybinding_conflicts() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        crate::storage::keybindings::save_keybindings_json(
            r#"{ "QuitApp": ["ctrl+x"], "NewSession": ["ctrl+x"] }"#,
        )
        .unwrap();

        let err = validate().unwrap_err();
        assert!(err.contains("keybindings.json"), "got: {err}");
    }

    /// `serde_ignored` reports nested unknown keys, so a typo inside
    /// `[features]` must fail strict validation just like a top-level one.
    #[test]
    fn validate_fails_on_unknown_feature_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let path = crate::agent::settings_config::settings_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[features]\nbogus = true\n").unwrap();

        let err = validate().unwrap_err();
        assert!(err.contains("settings.toml"), "got: {err}");
    }

    #[test]
    fn show_reports_effective_settings_and_editor_source() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let db = Database::open_in_memory().unwrap();
        db.set_editor_command("code --wait").unwrap();

        let v = show(&db).unwrap();
        assert_eq!(v["editor"]["command"], json!("code --wait"));
        assert_eq!(v["editor"]["source"], json!("database (editor_command)"));
        assert!(v["settings"]["scrollback_lines"].is_number());
        assert_eq!(v["settings"]["features"]["tasks"], json!(true));
        assert_eq!(v["agents"]["default"], json!("claude"));
    }
}
