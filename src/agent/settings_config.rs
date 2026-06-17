//! Loading and seeding of the settings config file.
//!
//! `~/.config/thurbox/settings.toml` holds the user-tunable scalars and
//! feature flags (see [`crate::session::settings::Settings`]). On first run the file is seeded
//! fully commented-out, so a fresh install runs on the built-in defaults. A
//! malformed file degrades to the defaults with a startup warning.

use std::path::PathBuf;

use crate::session::settings::Settings;

/// Seed contents for `settings.toml` on first run: every knob documented with
/// its default, all commented out.
pub const SEED_SETTINGS_TOML: &str = r#"# Thurbox settings  —  ~/.config/thurbox/settings.toml
#
# Scalar tuning knobs. Every entry below is commented out and shows its
# default; uncomment to change. Read once at startup.
#
# Unknown keys are reported on startup (and fail `thurbox-cli config
# validate`) but don't break the load.

config_version = 1

# Scrollback lines kept per session terminal.
# scrollback_lines = 1000

# Terminal width (columns) below which only the terminal pane renders.
# two_panel_min_cols = 80

# Terminal width (columns) at which the optional third column
# (info panel / tasks / file viewer) becomes available.
# three_panel_min_cols = 120

# Days of audit-log history kept (pruned on startup).
# audit_retention_days = 90

# Feature flags: turn whole TUI features off. All default to true.
# Disabling `automations` also stops the TUI firing schedules and arming
# the tmux heartbeat on startup; explicit `thurbox-cli automation`
# commands (and an already-armed heartbeat window) keep working. Data is
# never touched, so re-enabling a flag is lossless.
# [features]
# tasks = true            # F5/Ctrl+W tasks panel
# automations = true      # automations pane, Ctrl+P, schedule firing
# file_viewer = true      # F3 file viewer column
# global_search = true    # Ctrl+/ search strip
# info_panel = true       # F2 info panel
# shell_pane = true       # Ctrl+T per-session shell
# mouse = true            # mouse capture: clicks, wheel, drag-select, hover
# notifications = true    # OS desktop notifications when a session needs attention
# soft_delete = true      # Ctrl+D soft-deletes (Ctrl+Z undo); false = hard delete after a prompt
#
# `version_check` and `auto_update` are the two flags that default to FALSE:
# both reach the network (GitHub) on startup. `version_check` only *notifies*
# (TUI header "update available" badge + `thurbox-cli version --check`);
# `auto_update` goes further and silently downloads, verifies, and replaces the
# installed binaries when a newer release exists (the new version applies on the
# next launch). `thurbox-cli update` does the same on demand.
# version_check = false   # GitHub update check (TUI badge + `version --check`)
# auto_update = false     # silently download+verify+replace binaries on startup

# OS desktop notifications. Linux gets click-to-focus (clicking the banner
# selects the session in the running TUI); macOS shows a passive banner only.
# Under WSL (no dbus notification daemon) the `auto` backend delivers a Windows
# toast via powershell.exe instead — click-to-focus is unavailable on that path.
# Run `thurbox-cli notify` to see the detected backend, or `--test` to fire a
# sample. The dispatcher only starts when [features] notifications = true.
# [notifications]
# also_on_waiting = false       # also fire on Busy → Waiting (no explicit OSC bell)
# suppress_for_active = true    # don't notify if you're already viewing that session
# sound = true                  # play the OS default notification sound
# min_interval_secs = 5         # per-session dedup floor (seconds)
# backend = "auto"              # delivery backend: auto | dbus | windows | off

# ──────────────────────────────────────────────────────────────────────────
# Common recipes (uncomment the lines under the recipe you want)
# ──────────────────────────────────────────────────────────────────────────
#
# Keep more terminal history (e.g. long build logs):
# scrollback_lines = 10000
#
# Minimal / focused TUI — turn off panels you don't use (frees key chords too):
# [features]
# tasks = false
# automations = false
# file_viewer = false
# global_search = false
#
# Get notified the moment a session needs you, even on quiet agents, and never
# for the one you're already watching:
# [features]
# notifications = true
# [notifications]
# also_on_waiting = true        # also fire on the timing-only Busy → Waiting edge
# suppress_for_active = false   # also notify the focused session
# min_interval_secs = 30        # at most one notification / 30 s per session
#
# Show an "update available" badge in the TUI header (makes a network call to
# GitHub on startup):
# [features]
# version_check = true
#
# Keep thurbox up to date automatically — silently download+verify+replace the
# binaries on startup when a newer release exists (restart to apply):
# [features]
# auto_update = true
"#;

/// Path to the settings file: `~/.config/thurbox/settings.toml`.
pub fn settings_config_path() -> Option<PathBuf> {
    crate::paths::config_file().map(|p| p.with_file_name("settings.toml"))
}

/// Load the settings, seeding the config file with commented-out defaults
/// when it is absent. Any read/parse error degrades gracefully to the
/// defaults; warnings are returned for the TUI status bar and logged by
/// headless callers.
pub fn load_or_seed_with_warnings() -> (Settings, Vec<String>) {
    let Some(path) = settings_config_path() else {
        return (
            Settings::default(),
            vec!["Could not resolve settings.toml path; using defaults".into()],
        );
    };

    if !path.exists() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return (
                    Settings::default(),
                    vec![format!(
                        "Failed to create config dir for settings.toml: {e}"
                    )],
                );
            }
        }
        if let Err(e) = std::fs::write(&path, SEED_SETTINGS_TOML) {
            return (
                Settings::default(),
                vec![format!("Failed to seed settings.toml: {e}")],
            );
        }
        tracing::info!(path = %path.display(), "Seeded settings.toml (defaults)");
        return (Settings::default(), Vec::new());
    }

    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            match super::agent_config::parse_toml_reporting_unknown::<Settings>(
                &contents,
                "settings.toml",
            ) {
                Ok((settings, warnings)) => (settings, warnings),
                Err(e) => (
                    Settings::default(),
                    vec![format!(
                        "settings.toml: {}; using defaults",
                        super::agent_config::compact_toml_error(&e.to_string())
                    )],
                ),
            }
        }
        Err(e) => (
            Settings::default(),
            vec![format!("Failed to read settings.toml: {e}")],
        ),
    }
}

/// Set a boolean key on a `toml_edit` table.
fn set_table_bool(table: &mut toml_edit::Table, key: &str, v: bool) {
    table[key] = toml_edit::value(v);
}

/// Write `settings` back to `settings.toml`, **preserving comments and
/// layout**.
///
/// The existing file (or, when absent, the documented [`SEED_SETTINGS_TOML`])
/// is parsed into a `toml_edit::DocumentMut` and each value is set in place, so
/// the surrounding documentation survives a round-trip. A malformed file falls
/// back to the seed text rather than blocking the save.
///
/// Note: the seed ships every knob as a *commented* `# key = …` line, which
/// `toml_edit` cannot see. The first save therefore **adds real, uncommented
/// keys** (below the documentation comments, which remain as reference); from
/// then on those keys are edited in place.
pub fn save_settings(settings: &Settings) -> std::io::Result<()> {
    use toml_edit::{value, DocumentMut};

    let Some(path) = settings_config_path() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not resolve settings.toml path",
        ));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SEED_SETTINGS_TOML.to_string(),
        Err(e) => return Err(e),
    };

    // A malformed file shouldn't block saving from the panel: fall back to the
    // seed document (its comments are still useful) rather than erroring out.
    let mut doc = contents
        .parse::<DocumentMut>()
        .or_else(|_| SEED_SETTINGS_TOML.parse::<DocumentMut>())
        .unwrap_or_default();

    // Top-level scalars (cast to i64 — TOML's only integer type).
    doc["config_version"] = value(i64::from(settings.config_version.unwrap_or(1)));
    doc["scrollback_lines"] = value(settings.scrollback_lines as i64);
    doc["two_panel_min_cols"] = value(i64::from(settings.two_panel_min_cols));
    doc["three_panel_min_cols"] = value(i64::from(settings.three_panel_min_cols));
    doc["audit_retention_days"] = value(settings.audit_retention_days as i64);

    // [features] table — create if missing.
    if !doc.contains_key("features") {
        doc["features"] = toml_edit::table();
    }
    if let Some(features) = doc["features"].as_table_mut() {
        let f = &settings.features;
        set_table_bool(features, "tasks", f.tasks);
        set_table_bool(features, "automations", f.automations);
        set_table_bool(features, "file_viewer", f.file_viewer);
        set_table_bool(features, "global_search", f.global_search);
        set_table_bool(features, "info_panel", f.info_panel);
        set_table_bool(features, "shell_pane", f.shell_pane);
        set_table_bool(features, "mouse", f.mouse);
        set_table_bool(features, "notifications", f.notifications);
        set_table_bool(features, "soft_delete", f.soft_delete);
        set_table_bool(features, "version_check", f.version_check);
        set_table_bool(features, "auto_update", f.auto_update);
    }

    // [notifications] table — create if missing.
    if !doc.contains_key("notifications") {
        doc["notifications"] = toml_edit::table();
    }
    if let Some(notifications) = doc["notifications"].as_table_mut() {
        let n = &settings.notifications;
        set_table_bool(notifications, "also_on_waiting", n.also_on_waiting);
        set_table_bool(notifications, "suppress_for_active", n.suppress_for_active);
        set_table_bool(notifications, "sound", n.sound);
        notifications["min_interval_secs"] = value(n.min_interval_secs as i64);
    }

    std::fs::write(&path, doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_toml_parses_to_defaults() {
        let s: Settings = toml::from_str(SEED_SETTINGS_TOML).unwrap();
        assert_eq!(
            Settings {
                config_version: None,
                ..s.clone()
            },
            Settings::default()
        );
        assert_eq!(s.config_version, Some(1));
    }

    /// The seeded `settings.toml` is the primary documentation users see, so
    /// it must mention every field.
    #[test]
    fn seed_toml_documents_every_field() {
        for field in [
            "scrollback_lines",
            "two_panel_min_cols",
            "three_panel_min_cols",
            "audit_retention_days",
            "[features]",
            "tasks",
            "automations",
            "file_viewer",
            "global_search",
            "info_panel",
            "shell_pane",
            "notifications",
            "[notifications]",
            "also_on_waiting",
            "suppress_for_active",
            "sound",
            "min_interval_secs",
            "backend",
        ] {
            assert!(
                SEED_SETTINGS_TOML.contains(field),
                "settings.toml seed must document '{field}'"
            );
        }
    }

    /// The seed carries a "common recipes" block of copy-pasteable settings,
    /// kept commented so `seed_toml_parses_to_defaults` still holds.
    #[test]
    fn seed_documents_common_recipes() {
        for marker in [
            "Common recipes",
            "scrollback_lines = 10000",
            "version_check = true",
            "auto_update = true",
        ] {
            assert!(
                SEED_SETTINGS_TOML.contains(marker),
                "settings.toml seed must include recipe '{marker}'"
            );
        }
    }

    #[test]
    fn load_or_seed_writes_file_when_absent() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = settings_config_path().unwrap();
        assert!(!path.exists());

        let (s, warnings) = load_or_seed_with_warnings();
        assert!(warnings.is_empty(), "got: {warnings:?}");
        assert_eq!(s, Settings::default());
        assert!(path.exists(), "settings.toml should have been seeded");
    }

    #[test]
    fn load_or_seed_falls_back_on_malformed_file_with_warning() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = settings_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "scrollback_lines = \"many\"").unwrap();

        let (s, warnings) = load_or_seed_with_warnings();
        assert_eq!(s, Settings::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("settings.toml"));
    }

    #[test]
    fn load_or_seed_reads_overrides() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = settings_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "scrollback_lines = 4000\naudit_retention_days = 7\n").unwrap();

        let (s, warnings) = load_or_seed_with_warnings();
        assert!(warnings.is_empty());
        assert_eq!(s.scrollback_lines, 4000);
        assert_eq!(s.audit_retention_days, 7);
        assert_eq!(s.two_panel_min_cols, 80);
    }

    #[test]
    fn save_settings_round_trips() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        // Seed the documented file first, then save mutated settings.
        let (mut s, _) = load_or_seed_with_warnings();
        s.scrollback_lines = 4000;
        s.audit_retention_days = 7;
        s.features.tasks = false;
        s.features.version_check = true;
        s.features.auto_update = true;
        s.notifications.min_interval_secs = 30;
        s.notifications.suppress_for_active = false;

        save_settings(&s).unwrap();

        let (reloaded, warnings) = load_or_seed_with_warnings();
        assert!(warnings.is_empty(), "got: {warnings:?}");
        // save_settings always stamps config_version = 1 (a migration marker);
        // every other field must round-trip exactly.
        assert_eq!(reloaded.config_version, Some(1));
        assert_eq!(
            Settings {
                config_version: None,
                ..reloaded
            },
            Settings {
                config_version: None,
                ..s
            }
        );
    }

    #[test]
    fn save_settings_preserves_comments() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let (s, _) = load_or_seed_with_warnings();

        save_settings(&s).unwrap();

        let raw = std::fs::read_to_string(settings_config_path().unwrap()).unwrap();
        assert!(raw.contains("# Thurbox settings"));
        assert!(raw.contains("Common recipes"));
    }

    #[test]
    fn save_settings_writes_when_file_absent() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let path = settings_config_path().unwrap();
        assert!(!path.exists());

        save_settings(&Settings::default()).unwrap();

        assert!(path.exists());
        let (reloaded, warnings) = load_or_seed_with_warnings();
        assert!(warnings.is_empty(), "got: {warnings:?}");
        assert_eq!(
            Settings {
                config_version: None,
                ..reloaded
            },
            Settings::default()
        );
    }

    #[test]
    fn save_settings_recovers_from_malformed_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let path = settings_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "garbage = ").unwrap();

        save_settings(&Settings::default()).unwrap();

        // The file must now parse back cleanly.
        let (reloaded, warnings) = load_or_seed_with_warnings();
        assert!(warnings.is_empty(), "got: {warnings:?}");
        assert_eq!(
            Settings {
                config_version: None,
                ..reloaded
            },
            Settings::default()
        );
    }

    #[test]
    fn load_or_seed_reads_feature_overrides() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = settings_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[features]\nautomations = false\n").unwrap();

        let (s, warnings) = load_or_seed_with_warnings();
        assert!(warnings.is_empty(), "got: {warnings:?}");
        assert!(!s.features.automations);
        assert!(s.features.tasks, "untouched flags stay enabled");
    }
}
