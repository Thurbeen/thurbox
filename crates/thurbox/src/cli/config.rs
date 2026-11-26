use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::output::{self, CommandOutput};

#[derive(Subcommand, Debug)]
pub enum Action {
    #[command(about = "Parse every config file strictly; non-zero exit when any is invalid")]
    Validate,
    #[command(about = "Print the effective configuration and where each value came from")]
    Show,
}

pub fn run(
    action: Action,
    settings: &crate::session::settings::Settings,
) -> Result<CommandOutput, String> {
    match action {
        Action::Validate => {
            let (report, failed) = validate();
            let human = render_validate(&report, &failed);
            if failed.is_empty() {
                Ok(CommandOutput::new(report, human))
            } else {
                Ok(CommandOutput::failed(
                    report,
                    human,
                    format!("config invalid: {}", failed.join(", ")),
                ))
            }
        }
        Action::Show => {
            let report = show(settings);
            let human = render_show(&report);
            Ok(CommandOutput::new(report, human))
        }
    }
}

fn file_report(path: Option<std::path::PathBuf>, problems: Vec<String>, exists: bool) -> Value {
    json!({
        "path": path.map(|p| p.display().to_string()),
        "exists": exists,
        "valid": problems.is_empty(),
        "problems": problems,
    })
}

fn validate_toml<T: serde::de::DeserializeOwned>(
    path: Option<std::path::PathBuf>,
    label: &str,
) -> (Value, bool) {
    validate_file(path, |contents| {
        crate::config::file::parse_toml_reporting_unknown::<T>(contents, label)
            .map(|(_, warnings)| warnings)
            .map_err(|e| e.to_string())
    })
}

fn validate_file(
    path: Option<std::path::PathBuf>,
    parse: impl Fn(&str) -> Result<Vec<String>, String>,
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
        Ok(contents) => parse(&contents).unwrap_or_else(|e| vec![e]),
        Err(e) => vec![format!("read failed: {e}")],
    };
    let ok = problems.is_empty();
    (file_report(Some(path), problems, true), ok)
}

fn validate_ui_json() -> (Value, bool) {
    let path = crate::kernel::registry::overrides_file();
    let problems = crate::kernel::registry::validate_overrides();
    let ok = problems.is_empty();
    (
        json!({
            "path": path.map(|p| p.display().to_string()),
            "ok": ok,
            "problems": problems,
        }),
        ok,
    )
}

fn validate_mod_state() -> (Value, bool) {
    let path = crate::kernel::registry::decisions_file();
    let exists = path.as_ref().is_some_and(|p| p.exists());
    let problems = crate::kernel::registry::validate_decisions();
    let ok = problems.is_empty();
    (file_report(path, problems, exists), ok)
}

fn validate() -> (Value, Vec<String>) {
    let (settings, settings_ok) = validate_file(
        crate::config::settings::settings_config_path(),
        crate::config::settings::unknown_keys,
    );
    let (themes, themes_ok) = validate_toml::<crate::theme::ThemesFile>(
        crate::config::themes::themes_config_path(),
        "themes.toml",
    );
    let (ui_json, ui_ok) = validate_ui_json();
    let (mod_state, mod_state_ok) = validate_mod_state();

    let failed: Vec<String> = [
        ("settings.toml", settings_ok),
        ("themes.toml", themes_ok),
        ("ui.json", ui_ok),
        ("mod-state.json", mod_state_ok),
    ]
    .iter()
    .filter(|(_, ok)| !ok)
    .map(|(name, _)| (*name).to_string())
    .collect();

    let report = json!({
        "valid": failed.is_empty(),
        "settings_toml": settings,
        "themes_toml": themes,
        "ui_json": ui_json,
        "mod_state_json": mod_state.clone(),
        "plugin_state_json": mod_state,
    });
    (report, failed)
}

fn render_validate(report: &Value, failed: &[String]) -> String {
    let files = [
        ("settings.toml", "settings_toml"),
        ("themes.toml", "themes_toml"),
        ("ui.json", "ui_json"),
        ("mod-state.json", "mod_state_json"),
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

fn push_validate_file_lines(lines: &mut Vec<String>, label: &str, entry: &Value) {
    let exists = entry["exists"].as_bool().unwrap_or(false);
    let valid = entry["valid"].as_bool().unwrap_or(false);
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

fn render_show(report: &Value) -> String {
    let mut sections: Vec<String> = Vec::new();

    if let Some(paths) = report["paths"].as_object() {
        let pairs: Vec<(&str, String)> = paths
            .iter()
            .filter(|(k, _)| k.as_str() != "plugin_state_json")
            .map(|(k, v)| (k.as_str(), output::dash(v.as_str())))
            .collect();
        sections.push(format!("Paths\n{}", output::kv(&pairs)));
    }

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

    let fleet = &report["fleet"];
    sections.push(format!(
        "Fleet\n{}",
        output::kv(&[
            ("name", output::dash(fleet["name"].as_str())),
            ("program", output::dash(fleet["program"].as_str())),
            ("version", output::dash(fleet["version"].as_str())),
            ("config", output::dash(fleet["config"].as_str())),
            ("data dir", output::dash(fleet["data_dir"].as_str())),
        ])
    ));

    sections.push(format!(
        "Sessions\n{}",
        output::kv(&[("tmux socket", output::dash(report["tmux_socket"].as_str()))])
    ));

    sections.join("\n\n")
}

fn show(settings: &crate::session::settings::Settings) -> Value {
    let (custom_themes, _) = crate::config::themes::load_or_seed_with_warnings();
    let state = crate::config::interface_state::load();
    let (editor, editor_source) = resolve_editor(state.editor_command);

    json!({
        "paths": {
            "settings_toml": crate::config::settings::settings_config_path()
                .map(|p| p.display().to_string()),
            "themes_toml": crate::config::themes::themes_config_path()
                .map(|p| p.display().to_string()),
            "ui_dir": crate::kernel::bundled::user_ui_dir()
                .map(|p| p.display().to_string()),
            "ui_json": crate::kernel::registry::overrides_file()
                .map(|p| p.display().to_string()),
            "mod_state_json": crate::kernel::registry::decisions_file()
                .map(|p| p.display().to_string()),
            "plugin_state_json": crate::kernel::registry::decisions_file()
                .map(|p| p.display().to_string()),
            "interface_json": crate::config::file::path("interface.json")
                .map(|p| p.display().to_string()),
        },
        "tmux_socket": crate::backend::instance::local_socket_name(),
        "settings": settings,
        "fleet": super::doctor::FleetFacts::read(&crate::fleet_link::FleetLink::in_force(settings)).document(),
        "editor": { "command": editor, "source": editor_source, "mode": state.editor_mode.as_db_value() },
        "theme": state.theme,
        "custom_themes": custom_themes.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
    })
}

fn resolve_editor(stored: Option<String>) -> (Option<String>, &'static str) {
    if let Some(cmd) = stored.filter(|c| !c.is_empty()) {
        return (Some(cmd), "interface.json (editor_command)");
    }
    if let Some(v) = nonempty_env("VISUAL") {
        return (Some(v), "$VISUAL");
    }
    if let Some(v) = nonempty_env("EDITOR") {
        return (Some(v), "$EDITOR");
    }
    (None, "unset")
}

fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
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
    fn validate_checks_the_interface_overrides_file() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let path = crate::kernel::registry::overrides_file().expect("a ui.json path");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ this is not json").unwrap();

        let (report, failed) = validate();
        assert!(
            failed.iter().any(|f| f == "ui.json"),
            "a malformed ui.json is a real problem: {failed:?}"
        );
        assert!(
            !report["ui_json"]["problems"]
                .as_array()
                .expect("problems is a list")
                .is_empty(),
            "and it says what is wrong: {report}"
        );
    }

    #[test]
    fn validate_checks_the_mod_state_store() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let path = crate::kernel::registry::decisions_file().expect("a store path");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ this is not json").unwrap();

        let (report, failed) = validate();
        assert!(failed.iter().any(|f| f == "mod-state.json"), "{failed:?}");
        assert_eq!(report["mod_state_json"]["exists"], true, "{report}");
        assert_eq!(report["plugin_state_json"], report["mod_state_json"]);
    }

    #[test]
    fn validate_fails_on_unknown_feature_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let path = crate::config::settings::settings_config_path().unwrap();
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
        crate::config::interface_state::update(|s| s.editor_command = Some("code --wait".into()))
            .unwrap();

        let v = show(&Default::default());
        assert_eq!(v["editor"]["command"], json!("code --wait"));
        assert_eq!(
            v["editor"]["source"],
            json!("interface.json (editor_command)")
        );
        assert!(v["settings"]["scrollback_lines"].is_number());
        assert_eq!(v["settings"]["features"]["mouse"], json!(true));
        assert_eq!(
            v["paths"]["plugin_state_json"],
            v["paths"]["mod_state_json"]
        );
    }
}
