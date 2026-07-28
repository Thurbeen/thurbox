//! Loading and seeding of the custom-themes config file.
//!
//! `~/.config/thurbox/themes.toml` defines user themes as a base preset plus
//! per-colour overrides (see [`crate::session::theme_config::CustomThemeDef`]).
//! Seeded with a fully commented-out example, so a fresh install ships only
//! the built-in presets. Resolution problems (bad colours, name collisions)
//! degrade to warnings, never to a startup failure.

use std::path::PathBuf;

use crate::session::theme_config::{ThemeEntry, ThemesFile};
use crate::session::ThemePreset;

/// Seed contents for `themes.toml` on first run.
pub const SEED_THEMES_TOML: &str = r##"# Thurbox custom themes  —  ~/.config/thurbox/themes.toml
#
# Each [[themes]] entry defines a theme offered in the Ctrl+Y picker alongside
# the built-in presets. Start from a built-in `base` and override only the
# colours you want; everything else inherits from the base.
#
# Colours accept anything ratatui parses: "#rrggbb", ANSI names ("red",
# "lightcyan"), indexed ("14"), or "reset" (terminal default — useful for
# app_bg).
#
# Dark bases: default, catppuccin-mocha, tokyo-night, gruvbox-dark, doom,
# nord, dracula, one-dark, rose-pine-moon, everforest, kanagawa,
# solarized-dark, monokai, ayu-dark, ayu-mirage, material, rose-pine,
# oxocarbon, github-dark, nightfox, sonokai, melange, zenburn, iceberg,
# vesper, synthwave, nightfly, tomorrow-night.
# Light bases: catppuccin-latte, tokyo-night-day, gruvbox-light,
# solarized-light, ayu-light, one-light, rose-pine-dawn, github-light.
#
# Overridable colour keys: accent, accent_bright, status_working,
# status_blocked, status_done, status_idle, status_error, status_unreachable,
# text_primary, text_secondary, text_muted,
# border_focused, border_unfocused, role_name, branch_name, search_bar,
# keybind_hint, tool_allowed, tool_disallowed, danger, selection_bg,
# selection_fg, modal_dim_bg, modal_bg, modal_border, inverted_fg,
# diff_added, diff_removed, diff_added_bg, diff_removed_bg, app_bg.
# Plus: display_name (string), light (bool), nerd_font (bool).
#
# Unknown keys are reported on startup (and fail `thurbox-cli config
# validate`) but don't break the load.

config_version = 1

# ──────────────────────────────────────────────────────────────────────────
# Minimal tweak — restyle a built-in by overriding one colour (uncomment)
# ──────────────────────────────────────────────────────────────────────────
#
# Inherits everything from catppuccin-mocha; only the accent changes. Setting
# app_bg = "reset" keeps your terminal's own background (nice for transparency).
#
# [[themes]]
# name = "my-mocha"
# display_name = "My Mocha"
# base = "catppuccin-mocha"
# accent = "#fab387"
# app_bg = "reset"
#
# ──────────────────────────────────────────────────────────────────────────
# Fuller theme — light base + several overrides + nerd-font glyphs
# ──────────────────────────────────────────────────────────────────────────
#
# Mix hex, ANSI names, and indexed colours freely. `light = true` tunes a few
# contrast-sensitive defaults; `nerd_font = true` opts into nerd-font icons.
#
# [[themes]]
# name = "solar-soft"
# display_name = "Solar Soft"
# base = "solarized-light"
# light = true
# nerd_font = true
# accent = "#268bd2"
# accent_bright = "#2aa198"
# status_working = "yellow"
# status_error = "#dc322f"
# selection_bg = "14"
"##;

/// Path to the custom-themes file: `~/.config/thurbox/themes.toml`.
pub fn themes_config_path() -> Option<PathBuf> {
    crate::paths::config_file().map(|p| p.with_file_name("themes.toml"))
}

/// Load and resolve the custom themes, seeding the file when absent.
/// Collisions with built-in preset names and duplicate custom names are
/// skipped with a warning (the persisted `active_theme` must stay
/// unambiguous).
pub fn load_or_seed_with_warnings() -> (Vec<ThemeEntry>, Vec<String>) {
    let Some(path) = themes_config_path() else {
        return (
            Vec::new(),
            vec!["Could not resolve themes.toml path; no custom themes".into()],
        );
    };

    if !path.exists() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return (
                    Vec::new(),
                    vec![format!("Failed to create config dir for themes.toml: {e}")],
                );
            }
        }
        if let Err(e) = std::fs::write(&path, SEED_THEMES_TOML) {
            return (Vec::new(), vec![format!("Failed to seed themes.toml: {e}")]);
        }
        tracing::info!(path = %path.display(), "Seeded themes.toml (no custom themes)");
        return (Vec::new(), Vec::new());
    }

    let file = match std::fs::read_to_string(&path) {
        Ok(contents) => {
            match super::agent_config::parse_toml_reporting_unknown::<ThemesFile>(
                &contents,
                "themes.toml",
            ) {
                Ok((file, warnings)) => return_resolved(file, warnings),
                Err(e) => {
                    return (
                        Vec::new(),
                        vec![format!(
                            "themes.toml: {}; no custom themes",
                            super::agent_config::compact_toml_error(&e.to_string())
                        )],
                    )
                }
            }
        }
        Err(e) => return (Vec::new(), vec![format!("Failed to read themes.toml: {e}")]),
    };
    file
}

fn return_resolved(file: ThemesFile, mut warnings: Vec<String>) -> (Vec<ThemeEntry>, Vec<String>) {
    let mut entries: Vec<ThemeEntry> = Vec::new();
    for def in &file.themes {
        if def.name.is_empty() {
            warnings.push("themes.toml: a theme without a name was skipped".into());
            continue;
        }
        if ThemePreset::from_str(&def.name).is_some() {
            warnings.push(format!(
                "theme \"{}\" shadows a built-in preset name (skipped)",
                def.name
            ));
            continue;
        }
        if entries.iter().any(|t| t.name == def.name) {
            warnings.push(format!("duplicate theme name \"{}\" (skipped)", def.name));
            continue;
        }
        let (entry, theme_warnings) = def.resolve();
        warnings.extend(theme_warnings);
        entries.push(entry);
    }
    (entries, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_toml_parses_to_no_themes() {
        let file: ThemesFile = toml::from_str(SEED_THEMES_TOML).unwrap();
        assert!(file.themes.is_empty());
        assert_eq!(file.config_version, Some(1));
    }

    /// Seed ships a minimal one-override example plus a fuller multi-override
    /// one, both commented (the no-themes test above proves they don't load).
    #[test]
    fn seed_toml_documents_minimal_and_full_examples() {
        for marker in ["Minimal tweak", "Fuller theme", "my-mocha", "solar-soft"] {
            assert!(
                SEED_THEMES_TOML.contains(marker),
                "themes.toml seed must include the '{marker}' example"
            );
        }
    }

    /// The seeded `themes.toml` is the primary documentation users see, so it
    /// must mention every overridable palette key. Guards against adding a
    /// `ThemePalette` colour without documenting it here.
    #[test]
    fn seed_toml_documents_every_palette_key() {
        for field in [
            "accent",
            "accent_bright",
            "status_working",
            "status_blocked",
            "status_done",
            "status_idle",
            "status_error",
            "status_unreachable",
            "text_primary",
            "text_secondary",
            "text_muted",
            "border_focused",
            "border_unfocused",
            "role_name",
            "branch_name",
            "search_bar",
            "keybind_hint",
            "tool_allowed",
            "tool_disallowed",
            "danger",
            "selection_bg",
            "selection_fg",
            "modal_dim_bg",
            "modal_bg",
            "modal_border",
            "inverted_fg",
            "diff_added",
            "diff_removed",
            "diff_added_bg",
            "diff_removed_bg",
            "app_bg",
            "display_name",
            "light",
            "nerd_font",
        ] {
            assert!(
                SEED_THEMES_TOML.contains(field),
                "themes.toml seed must document '{field}'"
            );
        }
    }

    #[test]
    fn load_or_seed_writes_file_when_absent() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = themes_config_path().unwrap();
        assert!(!path.exists());

        let (themes, warnings) = load_or_seed_with_warnings();
        assert!(themes.is_empty());
        assert!(warnings.is_empty(), "got: {warnings:?}");
        assert!(path.exists(), "themes.toml should have been seeded");
    }

    #[test]
    fn load_resolves_custom_theme_with_base_and_overrides() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = themes_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[[themes]]\nname = \"mine\"\nbase = \"catppuccin-mocha\"\naccent = \"#ff00ff\"\n",
        )
        .unwrap();

        let (themes, warnings) = load_or_seed_with_warnings();
        assert!(warnings.is_empty(), "got: {warnings:?}");
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].name, "mine");
        assert_eq!(
            themes[0].palette.accent,
            ratatui::style::Color::Rgb(0xff, 0x00, 0xff)
        );
        assert_eq!(
            themes[0].palette.text_primary,
            ThemePreset::CatppuccinMocha.palette().text_primary
        );
        assert!(!themes[0].is_light);
    }

    #[test]
    fn load_skips_builtin_shadow_and_warns_on_bad_colour() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = themes_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            concat!(
                "[[themes]]\nname = \"doom\"\n",
                "[[themes]]\nname = \"ok\"\naccent = \"not-a-colour\"\n",
            ),
        )
        .unwrap();

        let (themes, warnings) = load_or_seed_with_warnings();
        assert_eq!(themes.len(), 1, "shadowing entry must be skipped");
        assert_eq!(themes[0].name, "ok");
        assert!(warnings.iter().any(|w| w.contains("shadows a built-in")));
        assert!(warnings.iter().any(|w| w.contains("not-a-colour")));
        assert_eq!(
            themes[0].palette.accent,
            ThemePreset::Default.palette().accent
        );
    }
}
