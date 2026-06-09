//! User-tunable scalar settings (`~/.config/thurbox/settings.toml`).
//!
//! Pure data + parsing, per the `session/` architecture rule; the file IO and
//! seeding live in `crate::agent::settings_config`. Only knobs a user
//! plausibly wants to change are exposed — timing/buffer internals stay
//! hardcoded. The loaded value is published process-wide via [`init`] /
//! [`global`] because the consumers span modules that must not know about each
//! other (terminal wiring, layout, storage retention).

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Scalar settings loaded from `settings.toml`. Every field has a default, so
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
    fn global_defaults_when_uninitialized() {
        // Note: other tests may have init()'d already; both paths are default.
        assert_eq!(global().two_panel_min_cols, 80);
    }
}
