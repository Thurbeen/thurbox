//! Loading and seeding of the scalar-settings config file.
//!
//! `~/.config/thurbox/settings.toml` holds the few user-tunable scalars (see
//! [`crate::session::settings::Settings`]). On first run the file is seeded
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
        ] {
            assert!(
                SEED_SETTINGS_TOML.contains(field),
                "settings.toml seed must document '{field}'"
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
}
