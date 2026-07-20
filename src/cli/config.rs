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

use crate::cli::output::{self, CommandOutput};
use crate::storage::Database;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Parse every config file strictly; non-zero exit when any is invalid.
    Validate,
    /// Print the effective configuration and where each value came from.
    Show,
}

pub fn run(action: Action, db: &Database) -> Result<CommandOutput, String> {
    match action {
        Action::Validate => {
            let (report, failed) = validate();
            let human = render_validate(&report, &failed);
            if failed.is_empty() {
                Ok(CommandOutput::new(report, human))
            } else {
                // Exit non-zero so this is usable as a dotfiles CI gate.
                Ok(CommandOutput::failed(
                    report,
                    human,
                    format!("config invalid: {}", failed.join(", ")),
                ))
            }
        }
        Action::Show => {
            let report = show(db)?;
            let human = render_show(&report);
            Ok(CommandOutput::new(report, human))
        }
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

/// Validate every config file. Returns the full report plus the list of files
/// that failed (empty = all valid).
fn validate() -> (Value, Vec<String>) {
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

    let failed: Vec<String> = [
        ("agents.toml", agents_ok),
        ("hosts.toml", hosts_ok),
        ("settings.toml", settings_ok),
        ("themes.toml", themes_ok),
        ("keybindings.json", kb_ok),
    ]
    .iter()
    .filter(|(_, ok)| !ok)
    .map(|(name, _)| (*name).to_string())
    .collect();

    let report = json!({
        "valid": failed.is_empty(),
        "agents_toml": agents,
        "hosts_toml": hosts,
        "settings_toml": settings,
        "themes_toml": themes,
        "keybindings_json": keybindings,
    });
    (report, failed)
}

/// Render `config validate` as a per-file status list.
fn render_validate(report: &Value, failed: &[String]) -> String {
    let files = [
        ("agents.toml", "agents_toml"),
        ("hosts.toml", "hosts_toml"),
        ("settings.toml", "settings_toml"),
        ("themes.toml", "themes_toml"),
        ("keybindings.json", "keybindings_json"),
    ];
    let mut lines = Vec::new();
    for (label, key) in files {
        push_validate_file_lines(&mut lines, label, &report[key]);
    }
    if failed.is_empty() {
        lines.push("All config files valid.".to_string());
    } else {
        lines.push(format!("Invalid: {}", failed.join(", ")));
    }
    lines.join("\n")
}

/// Append one config file's status line (and any problem lines) to `lines`.
fn push_validate_file_lines(lines: &mut Vec<String>, label: &str, entry: &Value) {
    let exists = entry["exists"].as_bool().unwrap_or(false);
    let valid = entry["valid"].as_bool().unwrap_or(false);
    // Absent files are valid (defaults/seeding apply), so flag them apart.
    let (mark, status) = match (exists, valid) {
        (false, _) => ("·", "absent"),
        (true, true) => ("✓", "ok"),
        (true, false) => ("✗", "invalid"),
    };
    lines.push(format!("{mark} {label}  {status}"));
    if let Some(problems) = entry["problems"].as_array() {
        for p in problems {
            if let Some(p) = p.as_str() {
                lines.push(format!("    - {p}"));
            }
        }
    }
}

/// Render `config show` as grouped key/value blocks.
fn render_show(report: &Value) -> String {
    let mut sections: Vec<String> = Vec::new();

    if let Some(paths) = report["paths"].as_object() {
        let pairs: Vec<(&str, String)> = paths
            .iter()
            .map(|(k, v)| (k.as_str(), output::dash(v.as_str())))
            .collect();
        sections.push(format!("Paths\n{}", output::kv(&pairs)));
    }

    let agents = &report["agents"];
    let names = agents["names"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    sections.push(format!(
        "Agents\n{}",
        output::kv(&[
            ("default", output::dash(agents["default"].as_str())),
            ("names", names),
        ])
    ));

    let editor = &report["editor"];
    sections.push(format!(
        "Editor\n{}",
        output::kv(&[
            ("command", output::dash(editor["command"].as_str())),
            ("source", output::dash(editor["source"].as_str())),
        ])
    ));

    sections.push(format!(
        "Theme\n{}",
        output::kv(&[("active", output::dash(report["theme"].as_str()))])
    ));

    sections.join("\n\n")
}

fn show(db: &Database) -> Result<Value, String> {
    let agents = crate::agent::agent_config::load_or_seed();
    // The *effective* host set: configured SSH/WSL hosts plus auto-discovered
    // WSL distros — matching what `--host` and the TUI picker actually offer
    // (`config validate` stays file-only).
    let hosts = crate::agent::host_config::load_all();
    let settings = crate::session::settings::global();
    let (custom_themes, _) = crate::agent::themes_config::load_or_seed_with_warnings();

    // Editor resolution mirrors the TUI's Ctrl+O chain: DB → $VISUAL → $EDITOR.
    let (editor, editor_source) = resolve_editor(db);
    let editor_mode = db.get_editor_mode().unwrap_or_default();
    let overridden_actions = overridden_action_names();

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
        "editor": { "command": editor, "source": editor_source, "mode": editor_mode.as_db_value() },
        "theme": db.get_active_theme().ok().flatten(),
        "custom_themes": custom_themes.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
    }))
}

/// Resolve the effective editor command + its source, mirroring the TUI's
/// Ctrl+O chain: DB (editor_command) → $VISUAL → $EDITOR → unset.
fn resolve_editor(db: &Database) -> (Option<String>, &'static str) {
    let db_editor = db.get_editor_command().ok().flatten();
    if let Some(cmd) = db_editor.filter(|c| !c.is_empty()) {
        return (Some(cmd), "database (editor_command)");
    }
    if let Some(v) = nonempty_env("VISUAL") {
        return (Some(v), "$VISUAL");
    }
    if let Some(v) = nonempty_env("EDITOR") {
        return (Some(v), "$EDITOR");
    }
    (None, "unset")
}

/// A non-empty environment variable value, or `None` when unset/empty.
fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// The sorted action names overridden in keybindings.json (empty when none).
fn overridden_action_names() -> Vec<String> {
    match crate::storage::keybindings::load_keybindings_json() {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::TestPathGuard;

    #[test]
    fn validate_passes_on_fresh_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let (v, failed) = validate();
        assert!(failed.is_empty(), "got failures: {failed:?}");
        assert_eq!(v["valid"], json!(true));
    }

    #[test]
    fn validate_fails_with_exit_error_on_malformed_agents_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let path = crate::agent::agent_config::agents_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not toml {{{").unwrap();

        let (_, failed) = validate();
        assert!(failed.iter().any(|f| f == "agents.toml"), "got: {failed:?}");
    }

    #[test]
    fn validate_reports_keybinding_conflicts() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        crate::storage::keybindings::save_keybindings_json(
            r#"{ "QuitApp": ["ctrl+a"], "NewSession": ["ctrl+a"] }"#,
        )
        .unwrap();

        let (_, failed) = validate();
        assert!(
            failed.iter().any(|f| f == "keybindings.json"),
            "got: {failed:?}"
        );
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

        let (_, failed) = validate();
        assert!(
            failed.iter().any(|f| f == "settings.toml"),
            "got: {failed:?}"
        );
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
