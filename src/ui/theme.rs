//! Runtime-swappable theme accessors for the Thurbox UI.
//!
//! Widgets read colours via `Theme::accent()` etc. — function calls that
//! resolve through a global `OnceLock<RwLock<ThemePalette>>`. Switching a
//! theme is a single `Theme::set_active(palette)` call; the next render
//! frame picks it up.
//!
//! Initialise the active theme exactly once at startup
//! (see `Theme::ensure_initialized`). Tests that paint frames must call
//! `ensure_initialized()` first or use `Theme::set_active(...)` explicitly.

use std::sync::{OnceLock, RwLock};

use ratatui::style::{Color, Modifier, Style};

use crate::session::{ThemePalette, ThemePreset};

static ACTIVE_THEME: OnceLock<RwLock<ThemePalette>> = OnceLock::new();

/// Snapshot of the currently active palette.
///
/// Cheap (one `RwLock` read + a `Clone`) so widgets can call this freely
/// inside their render functions.
pub fn current() -> ThemePalette {
    ensure_initialized();
    ACTIVE_THEME.get().unwrap().read().unwrap().clone()
}

/// Replace the active palette in place. The next render reflects the change.
pub fn set_active(palette: ThemePalette) {
    let cell = ACTIVE_THEME.get_or_init(|| RwLock::new(palette.clone()));
    *cell.write().unwrap() = palette;
}

/// Idempotent: install the default palette if none has been set yet.
///
/// Safe to call from any render path; prevents the first widget that fires
/// during a test from panicking on an uninitialised `OnceLock`.
pub fn ensure_initialized() {
    ACTIVE_THEME.get_or_init(|| RwLock::new(ThemePalette::default()));
}

/// Convenience: load a built-in preset by name and activate it. Falls back
/// to `Default` for unknown names. Returns the preset that was actually
/// applied so callers can persist it back to storage.
pub fn apply_preset_by_name(name: &str) -> ThemePreset {
    let preset = ThemePreset::from_str(name).unwrap_or(ThemePreset::Default);
    set_active(preset.palette());
    preset
}

/// Centralised colour and style accessors for the Thurbox UI.
///
/// All widget files reference `Theme::accent()` etc. instead of hard-coding
/// colours, so swapping themes only takes one `set_active()` call.
pub struct Theme;

impl Theme {
    // ── Accent ──────────────────────────────────────────────────────────────

    pub fn accent() -> Color {
        current().accent
    }

    pub fn accent_bright() -> Color {
        current().accent_bright
    }

    // ── Status colours ──────────────────────────────────────────────────────

    pub fn status_busy() -> Color {
        current().status_busy
    }
    pub fn status_waiting() -> Color {
        current().status_waiting
    }
    pub fn status_idle() -> Color {
        current().status_idle
    }
    pub fn status_error() -> Color {
        current().status_error
    }

    // ── Text hierarchy ──────────────────────────────────────────────────────

    pub fn text_primary() -> Color {
        current().text_primary
    }
    pub fn text_secondary() -> Color {
        current().text_secondary
    }
    pub fn text_muted() -> Color {
        current().text_muted
    }

    // ── Borders ─────────────────────────────────────────────────────────────

    pub fn border_focused() -> Color {
        current().border_focused
    }
    pub fn border_unfocused() -> Color {
        current().border_unfocused
    }

    // ── Domain-specific ─────────────────────────────────────────────────────

    pub fn role_name() -> Color {
        current().role_name
    }
    pub fn admin_badge() -> Color {
        current().admin_badge
    }
    pub fn branch_name() -> Color {
        current().branch_name
    }
    pub fn search_bar() -> Color {
        current().search_bar
    }

    // ── Keybind hints / tool permissions ────────────────────────────────────

    pub fn keybind_hint() -> Color {
        current().keybind_hint
    }
    pub fn tool_allowed() -> Color {
        current().tool_allowed
    }
    pub fn tool_disallowed() -> Color {
        current().tool_disallowed
    }

    // ── Admin / danger ──────────────────────────────────────────────────────

    pub fn admin_border() -> Color {
        current().admin_border
    }
    pub fn danger() -> Color {
        current().danger
    }

    // ── Selection ───────────────────────────────────────────────────────────

    pub fn selection_bg() -> Color {
        current().selection_bg
    }
    pub fn selection_fg() -> Color {
        current().selection_fg
    }

    // ── Modal ───────────────────────────────────────────────────────────────

    pub fn modal_dim_bg() -> Color {
        current().modal_dim_bg
    }
    pub fn modal_bg() -> Color {
        current().modal_bg
    }
    pub fn modal_border() -> Color {
        current().modal_border
    }

    // ── Background colours ──────────────────────────────────────────────────

    pub fn inverted_fg() -> Color {
        current().inverted_fg
    }

    /// Background colour for the entire app surface (chrome + PTY cells
    /// without their own bg). `Color::Reset` keeps the terminal's native
    /// background.
    pub fn app_bg() -> Color {
        current().app_bg
    }

    /// Whether nerd-font glyphs should be used for icons.
    pub fn nerd_font_enabled() -> bool {
        current().nerd_font_enabled
    }

    // ── Composite styles ────────────────────────────────────────────────────

    /// Style for a focused panel/modal title: bold black on accent background.
    pub fn focused_title() -> Style {
        Style::default()
            .fg(Self::inverted_fg())
            .bg(Self::accent())
            .add_modifier(Modifier::BOLD)
    }

    /// Style for an unfocused panel title: dimmed secondary text.
    pub fn unfocused_title() -> Style {
        Style::default().fg(Self::border_unfocused())
    }

    /// Style for section headers (e.g. info panel sections, help overlay).
    pub fn section_header() -> Style {
        Style::default()
            .fg(Self::accent())
            .add_modifier(Modifier::BOLD)
    }

    /// Style for labels in info panels and status displays.
    pub fn label() -> Style {
        Style::default().fg(Self::text_muted())
    }

    /// Style for keybind hint keys in modal footers.
    pub fn keybind() -> Style {
        Style::default().fg(Self::keybind_hint())
    }

    /// Style for keybind descriptions in modal footers.
    pub fn keybind_desc() -> Style {
        Style::default().fg(Self::text_muted())
    }

    /// Style for selected/active list items.
    pub fn selected_item() -> Style {
        Style::default()
            .fg(Self::accent())
            .add_modifier(Modifier::BOLD)
    }

    /// Style for normal (unselected) list items.
    pub fn normal_item() -> Style {
        Style::default().fg(Self::text_primary())
    }

    /// Style for admin section title: yellow bold.
    pub fn admin_title() -> Style {
        Style::default()
            .fg(Self::admin_border())
            .add_modifier(Modifier::BOLD)
    }

    /// Style for a focused admin panel title: bold black on yellow badge.
    pub fn admin_focused_title() -> Style {
        Style::default()
            .fg(Self::INVERTED_FG)
            .bg(Self::ADMIN_BORDER)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for modal titles: bold black on accent background (same as focused panels).
    pub fn modal_title() -> Style {
        Self::focused_title()
    }

    /// Style for danger modal titles: bold white on red background.
    pub fn modal_title_danger() -> Style {
        Style::default()
            .fg(Self::text_primary())
            .bg(Self::danger())
            .add_modifier(Modifier::BOLD)
    }

    /// Style for the block cursor in text fields.
    pub fn cursor() -> Style {
        Style::default()
            .fg(Self::inverted_fg())
            .bg(Self::text_primary())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_initialized_is_idempotent() {
        ensure_initialized();
        ensure_initialized();
        // Just confirm reading the palette doesn't panic and returns the
        // default accent on a fresh init.
        assert_eq!(Theme::accent(), ThemePalette::default().accent);
    }

    #[test]
    fn set_active_switches_palette() {
        set_active(ThemePreset::CatppuccinMocha.palette());
        assert_eq!(
            Theme::accent(),
            ThemePreset::CatppuccinMocha.palette().accent
        );

        // Restore default for any later test that depends on it.
        set_active(ThemePalette::default());
    }

    #[test]
    fn apply_preset_falls_back_to_default_for_unknown_name() {
        let applied = apply_preset_by_name("does-not-exist");
        assert_eq!(applied, ThemePreset::Default);
    }
}
