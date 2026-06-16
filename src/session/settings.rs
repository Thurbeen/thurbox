//! User-tunable settings (`~/.config/thurbox/settings.toml`): scalar knobs
//! plus the `[features]` whole-feature switches.
//!
//! Pure data + parsing, per the `session/` architecture rule; the file IO and
//! seeding live in `crate::agent::settings_config`. Only knobs a user
//! plausibly wants to change are exposed — timing/buffer internals stay
//! hardcoded. The loaded value is published process-wide via [`init`] /
//! [`global`] because the consumers span modules that must not know about each
//! other (terminal wiring, layout, storage retention).

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Settings loaded from `settings.toml`. Every field has a default, so
/// an absent file (the common case) behaves exactly like before the file
/// existed. Unknown keys are tolerated but reported: the loader names every
/// unrecognized key in a startup warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Config-format version, for future migrations. Currently `1`.
    #[serde(default)]
    pub config_version: Option<u32>,
    /// Scrollback lines kept per session terminal (vt100 parser history).
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,
    /// Terminal width (columns) below which only the terminal pane renders.
    #[serde(default = "default_two_panel_min_cols")]
    pub two_panel_min_cols: u16,
    /// Terminal width (columns) at which the optional third column (info /
    /// tasks / file viewer) becomes available.
    #[serde(default = "default_three_panel_min_cols")]
    pub three_panel_min_cols: u16,
    /// Days of audit-log history kept (pruned on startup).
    #[serde(default = "default_audit_retention_days")]
    pub audit_retention_days: u64,
    /// Per-feature on/off switches (`[features]` table). Absent table = all
    /// enabled.
    #[serde(default)]
    pub features: FeatureFlags,
}

/// Whole-feature switches (`[features]` in settings.toml). Each flag hides the
/// feature's UI and blocks its keybinding; disabling `automations` also stops
/// the TUI firing schedules and arming the tmux heartbeat. Data and
/// `thurbox-cli` surfaces stay fully functional regardless, so re-enabling a
/// flag is lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFlags {
    /// Tasks panel (F5/Ctrl+W) and task search results.
    #[serde(default = "default_true")]
    pub tasks: bool,
    /// Automations pane, Ctrl+P editor, TUI schedule firing, heartbeat arming.
    #[serde(default = "default_true")]
    pub automations: bool,
    /// File viewer column (F3) and file search results.
    #[serde(default = "default_true")]
    pub file_viewer: bool,
    /// Global search strip (Ctrl+/).
    #[serde(default = "default_true")]
    pub global_search: bool,
    /// Info panel column (F2).
    #[serde(default = "default_true")]
    pub info_panel: bool,
    /// Per-session shell pane toggle (Ctrl+T).
    #[serde(default = "default_true")]
    pub shell_pane: bool,
    /// Mouse support: terminal mouse capture plus all click/scroll/hover
    /// handling (click-to-select, drag selection, Ctrl+Click URLs,
    /// scrollbars). Disable to keep the terminal's native mouse behavior
    /// (e.g. its own text selection).
    #[serde(default = "default_true")]
    pub mouse: bool,
}

fn default_true() -> bool {
    true
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            tasks: true,
            automations: true,
            file_viewer: true,
            global_search: true,
            info_panel: true,
            shell_pane: true,
            mouse: true,
        }
    }
}

fn default_scrollback_lines() -> usize {
    1000
}
fn default_two_panel_min_cols() -> u16 {
    80
}
fn default_three_panel_min_cols() -> u16 {
    120
}
fn default_audit_retention_days() -> u64 {
    90
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            config_version: None,
            scrollback_lines: default_scrollback_lines(),
            two_panel_min_cols: default_two_panel_min_cols(),
            three_panel_min_cols: default_three_panel_min_cols(),
            audit_retention_days: default_audit_retention_days(),
            features: FeatureFlags::default(),
        }
    }
}

static GLOBAL: OnceLock<Settings> = OnceLock::new();

/// Publish the loaded settings process-wide. Call once at startup, before the
/// first [`global`] read; later calls are ignored (first writer wins).
pub fn init(settings: Settings) {
    let _ = GLOBAL.set(settings);
}

/// The process-wide settings; defaults when [`init`] was never called (tests,
/// or library use outside the binaries).
pub fn global() -> &'static Settings {
    GLOBAL.get_or_init(Settings::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_document_yields_defaults() {
        let s: Settings = toml::from_str("").unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.scrollback_lines, 1000);
        assert_eq!(s.two_panel_min_cols, 80);
        assert_eq!(s.three_panel_min_cols, 120);
        assert_eq!(s.audit_retention_days, 90);
    }

    #[test]
    fn partial_override_keeps_other_defaults() {
        let s: Settings = toml::from_str("scrollback_lines = 5000").unwrap();
        assert_eq!(s.scrollback_lines, 5000);
        assert_eq!(s.audit_retention_days, 90);
    }

    #[test]
    fn type_mismatch_is_rejected() {
        let err = toml::from_str::<Settings>("scrollback_lines = \"many\"").unwrap_err();
        assert!(err.to_string().contains("scrollback_lines"));
    }

    #[test]
    fn absent_features_table_enables_everything() {
        let s: Settings = toml::from_str("").unwrap();
        assert_eq!(s.features, FeatureFlags::default());
        assert!(s.features.tasks && s.features.automations);
    }

    #[test]
    fn empty_features_table_enables_everything() {
        let s: Settings = toml::from_str("[features]").unwrap();
        assert_eq!(s.features, FeatureFlags::default());
    }

    #[test]
    fn partial_features_override_keeps_other_flags_enabled() {
        let s: Settings = toml::from_str("[features]\ntasks = false").unwrap();
        assert!(!s.features.tasks);
        assert!(s.features.automations);
        assert!(s.features.file_viewer);
        assert!(s.features.global_search);
        assert!(s.features.info_panel);
        assert!(s.features.shell_pane);
        assert!(s.features.mouse);
    }

    #[test]
    fn mouse_feature_flag_parses() {
        let s: Settings = toml::from_str("[features]\nmouse = false").unwrap();
        assert!(!s.features.mouse);
        assert!(s.features.tasks, "untouched flags stay enabled");
    }

    #[test]
    fn feature_flag_type_mismatch_is_rejected() {
        let err = toml::from_str::<Settings>("[features]\ntasks = \"no\"").unwrap_err();
        assert!(err.to_string().contains("tasks"));
    }

    #[test]
    fn global_defaults_when_uninitialized() {
        // Note: other tests may have init()'d already; both paths are default.
        assert_eq!(global().two_panel_min_cols, 80);
    }
}
