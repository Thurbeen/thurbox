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

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Parse every config file strictly; non-zero exit when any is invalid.
    Validate,
    /// Print the effective configuration and where each value came from.
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
                // Exit non-zero so this is usable as a dotfiles CI gate.
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
    validate_file(path, |contents| {
        crate::config::file::parse_toml_reporting_unknown::<T>(contents, label)
            .map(|(_, warnings)| warnings)
            .map_err(|e| e.to_string())
    })
}

/// Validate one config file with `parse`, whose `Ok` holds the problems a
/// loadable file still has.
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

/// Validate `ui.json` — the file the interface actually reads for rebindings,
/// settings and band order.
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

/// Validate `plugin-state.json` — trust grants and the disabled set.
fn validate_plugin_state() -> (Value, bool) {
    let path = crate::kernel::registry::decisions_file();
    let exists = path.as_ref().is_some_and(|p| p.exists());
    let problems = crate::kernel::registry::validate_decisions();
    let ok = problems.is_empty();
    (file_report(path, problems, exists), ok)
}

/// Validate every config file. Returns the full report plus the list of files
/// that failed (empty = all valid).
fn validate() -> (Value, Vec<String>) {
    let (agents, agents_ok) = validate_file(
        crate::agent::agent_config::agents_config_path(),
        |contents| {
            crate::agent::agent_config::parse_for_reload(contents).map(|(_, warnings)| warnings)
        },
    );
    // Through the loader's own parse, so an entry the load refuses — a host
    // name holding `:` — fails validation rather than vanishing silently.
    let (hosts, hosts_ok) = validate_file(crate::config::hosts::hosts_config_path(), |c| {
        crate::config::hosts::parse_hosts(c).map(|(_, warnings)| warnings)
    });
    let (settings, settings_ok) = validate_toml::<crate::session::settings::Settings>(
        crate::config::settings::settings_config_path(),
        "settings.toml",
    );
    let (themes, themes_ok) = validate_toml::<crate::theme::ThemesFile>(
        crate::config::themes::themes_config_path(),
        "themes.toml",
    );
    let (ui_json, ui_ok) = validate_ui_json();
    let (plugin_state, plugin_state_ok) = validate_plugin_state();

    let failed: Vec<String> = [
        ("agents.toml", agents_ok),
        ("hosts.toml", hosts_ok),
        ("settings.toml", settings_ok),
        ("themes.toml", themes_ok),
        ("ui.json", ui_ok),
        ("plugin-state.json", plugin_state_ok),
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
        "ui_json": ui_json,
        "plugin_state_json": plugin_state,
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
        ("ui.json", "ui_json"),
        ("plugin-state.json", "plugin_state_json"),
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

    sections.push(format!(
        "Sessions\n{}",
        output::kv(&[("tmux socket", output::dash(report["tmux_socket"].as_str()))])
    ));

    sections.join("\n\n")
}

fn show(settings: &crate::session::settings::Settings) -> Value {
    let agents = crate::agent::agent_config::load_or_seed();
    // The *effective* host set: configured SSH/WSL hosts plus auto-discovered
    // WSL distros — matching what `--host` and the TUI picker actually offer
    // (`config validate` stays file-only).
    let hosts = crate::config::hosts::load_all();
    let (custom_themes, _) = crate::config::themes::load_or_seed_with_warnings();
    let state = crate::config::interface_state::load();
    let (editor, editor_source) = resolve_editor(state.editor_command);

    json!({
        "paths": {
            "agents_toml": crate::agent::agent_config::agents_config_path()
                .map(|p| p.display().to_string()),
            "hosts_toml": crate::config::hosts::hosts_config_path()
                .map(|p| p.display().to_string()),
            "settings_toml": crate::config::settings::settings_config_path()
                .map(|p| p.display().to_string()),
            "themes_toml": crate::config::themes::themes_config_path()
                .map(|p| p.display().to_string()),
            // The interface is config like the rest of it, and it was the one
            // thing this command could not tell you the location of — which is
            // exactly the question a dev build invites, since every path here
            // swaps `thurbox` for `thurbox-dev`.
            "ui_dir": crate::kernel::bundled::user_ui_dir()
                .map(|p| p.display().to_string()),
            "ui_json": crate::kernel::registry::overrides_file()
                .map(|p| p.display().to_string()),
            "plugin_state_json": crate::kernel::registry::decisions_file()
                .map(|p| p.display().to_string()),
            "interface_json": crate::config::file::path("interface.json")
                .map(|p| p.display().to_string()),
        },
        // Not a path, but the same question: which instance is this? A
        // relocated data dir gets a socket of its own, so the name is resolved
        // rather than constant and worth reporting where the dirs are.
        "tmux_socket": crate::backend::instance::local_socket_name(),
        "agents": { "default": agents.default_name(), "names": agents.names() },
        "hosts": { "names": hosts.names() },
        "settings": settings,
        "editor": { "command": editor, "source": editor_source, "mode": state.editor_mode.as_db_value() },
        "theme": state.theme,
        "custom_themes": custom_themes.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
    })
}

/// Resolve the effective editor command + its source, mirroring the TUI's
/// Ctrl+O chain: `interface.json` → $VISUAL → $EDITOR → unset.
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

/// A non-empty environment variable value, or `None` when unset/empty.
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
    fn validate_fails_on_an_unknown_agents_toml_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let path = crate::agent::agent_config::agents_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[[agents]]\nname = \"ok\"\ncommand = \"x\"\n\
             [[agents]]\nname = \"coder\"\ncommand = \"x\"\n\
             setup = [\"scripts/role.sh {rol}\"]\nvars = { role = \"coder\" }\n",
        )
        .unwrap();

        let (report, failed) = validate();
        assert!(failed.iter().any(|f| f == "agents.toml"), "got: {failed:?}");
        assert!(
            report.to_string().contains("unknown placeholder {rol}"),
            "the message names the placeholder: {report}"
        );
    }

    #[test]
    fn validate_checks_the_interface_overrides_file() {
        // `ui.json` is the one the interface actually reads on every launch, and it
        // was the one `validate` did not look at.
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
    fn validate_checks_the_plugin_state_store() {
        // Trust is granted from this file, so a broken one is reported rather
        // than read as "nothing trusted" in silence.
        let tmp = tempfile::tempdir().unwrap();
        let _g = TestPathGuard::new(tmp.path());
        let path = crate::kernel::registry::decisions_file().expect("a store path");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ this is not json").unwrap();

        let (report, failed) = validate();
        assert!(
            failed.iter().any(|f| f == "plugin-state.json"),
            "{failed:?}"
        );
        assert_eq!(report["plugin_state_json"]["exists"], true, "{report}");
    }

    /// `serde_ignored` reports nested unknown keys, so a typo inside
    /// `[features]` must fail strict validation just like a top-level one.
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
        assert_eq!(v["settings"]["features"]["tasks"], json!(true));
        assert_eq!(v["agents"]["default"], json!("claude"));
    }
}
