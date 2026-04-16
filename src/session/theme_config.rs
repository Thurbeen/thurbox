//! Theme palettes for the Thurbox TUI.
//!
//! A `ThemePalette` is the runtime, swappable counterpart to the static colour
//! constants that used to live in `src/ui/theme.rs`. Widgets read the active
//! palette via `crate::ui::theme::current()`; users pick one via the theme
//! picker modal or by setting `active_theme` in the SQLite metadata table.

use ratatui::style::Color;

/// Colour values + glyph preferences for the entire TUI.
///
/// Field names match the former `Theme::*` constants one-to-one so the
/// existing widget code translates cleanly to function calls
/// (`Theme::accent()` etc.).
///
/// Palettes are never persisted directly — only the preset *name* is stored
/// in the SQLite `metadata.active_theme` row. The runtime palette is
/// reconstructed from `ThemePreset::palette()` on load.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemePalette {
    pub accent: Color,
    pub accent_bright: Color,

    pub status_busy: Color,
    pub status_waiting: Color,
    pub status_idle: Color,
    pub status_error: Color,

    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_muted: Color,

    pub border_focused: Color,
    pub border_unfocused: Color,

    pub role_name: Color,
    pub admin_badge: Color,
    pub branch_name: Color,
    pub search_bar: Color,

    pub keybind_hint: Color,
    pub tool_allowed: Color,
    pub tool_disallowed: Color,

    pub admin_border: Color,
    pub danger: Color,

    pub selection_bg: Color,
    pub selection_fg: Color,

    pub modal_dim_bg: Color,
    pub modal_bg: Color,
    pub modal_border: Color,

    pub inverted_fg: Color,

    /// Background colour painted under the entire app (chrome + PTY cells
    /// that don't set their own bg). Use `Color::Reset` to keep the
    /// terminal's native background — required for the `Default` preset
    /// which uses ANSI named colours that adapt to user terminal themes.
    pub app_bg: Color,

    /// When true, widgets that have nerd-font glyph alternatives use them.
    /// Defaults to `false` to stay readable on terminals without nerd fonts.
    pub nerd_font_enabled: bool,
}

impl Default for ThemePalette {
    fn default() -> Self {
        ThemePreset::Default.palette()
    }
}

/// Built-in theme presets. The string names are the values stored in the
/// SQLite `metadata.active_theme` row and accepted by the MCP `set_theme`
/// tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePreset {
    /// The original cyan-accented palette that shipped first.
    Default,
    /// Catppuccin Mocha — warm dark palette with mauve accents.
    CatppuccinMocha,
    /// Tokyo Night — cool blue-purple palette popular in Neovim configs.
    TokyoNight,
    /// Gruvbox Dark — earthy, high-contrast retro palette.
    GruvboxDark,
    /// Catppuccin Latte — light counterpart to Mocha.
    CatppuccinLatte,
    /// Tokyo Night Day — light counterpart to Tokyo Night.
    TokyoNightDay,
    /// Gruvbox Light — earthy retro palette on a cream background.
    GruvboxLight,
    /// Solarized Light — Ethan Schoonover's classic light palette.
    SolarizedLight,
}

impl ThemePreset {
    /// Stable identifier used for storage / MCP / config files.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::TokyoNight => "tokyo-night",
            Self::GruvboxDark => "gruvbox-dark",
            Self::CatppuccinLatte => "catppuccin-latte",
            Self::TokyoNightDay => "tokyo-night-day",
            Self::GruvboxLight => "gruvbox-light",
            Self::SolarizedLight => "solarized-light",
        }
    }

    /// Human-readable label shown in pickers and the status bar.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::TokyoNight => "Tokyo Night",
            Self::GruvboxDark => "Gruvbox Dark",
            Self::CatppuccinLatte => "Catppuccin Latte",
            Self::TokyoNightDay => "Tokyo Night Day",
            Self::GruvboxLight => "Gruvbox Light",
            Self::SolarizedLight => "Solarized Light",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "default" => Some(Self::Default),
            "catppuccin-mocha" => Some(Self::CatppuccinMocha),
            "tokyo-night" => Some(Self::TokyoNight),
            "gruvbox-dark" => Some(Self::GruvboxDark),
            "catppuccin-latte" => Some(Self::CatppuccinLatte),
            "tokyo-night-day" => Some(Self::TokyoNightDay),
            "gruvbox-light" => Some(Self::GruvboxLight),
            "solarized-light" => Some(Self::SolarizedLight),
            _ => None,
        }
    }

    pub fn all() -> &'static [ThemePreset] {
        &[
            Self::Default,
            Self::CatppuccinMocha,
            Self::TokyoNight,
            Self::GruvboxDark,
            Self::CatppuccinLatte,
            Self::TokyoNightDay,
            Self::GruvboxLight,
            Self::SolarizedLight,
        ]
    }

    /// Materialise this preset's colours.
    pub fn palette(self) -> ThemePalette {
        match self {
            Self::Default => default_palette(),
            Self::CatppuccinMocha => catppuccin_mocha_palette(),
            Self::TokyoNight => tokyo_night_palette(),
            Self::GruvboxDark => gruvbox_dark_palette(),
            Self::CatppuccinLatte => catppuccin_latte_palette(),
            Self::TokyoNightDay => tokyo_night_day_palette(),
            Self::GruvboxLight => gruvbox_light_palette(),
            Self::SolarizedLight => solarized_light_palette(),
        }
    }

    /// Whether this preset is intended for terminals with a light background.
    /// Used by the picker to group dark and light themes.
    pub fn is_light(self) -> bool {
        matches!(
            self,
            Self::CatppuccinLatte | Self::TokyoNightDay | Self::GruvboxLight | Self::SolarizedLight
        )
    }
}

fn default_palette() -> ThemePalette {
    ThemePalette {
        accent: Color::Cyan,
        accent_bright: Color::LightCyan,

        status_busy: Color::Green,
        status_waiting: Color::Yellow,
        status_idle: Color::DarkGray,
        status_error: Color::Red,

        text_primary: Color::White,
        text_secondary: Color::Gray,
        text_muted: Color::DarkGray,

        border_focused: Color::Cyan,
        border_unfocused: Color::Gray,

        role_name: Color::Magenta,
        admin_badge: Color::Yellow,
        branch_name: Color::Green,
        search_bar: Color::Blue,

        keybind_hint: Color::Yellow,
        tool_allowed: Color::Green,
        tool_disallowed: Color::Red,

        admin_border: Color::Yellow,
        danger: Color::Red,

        selection_bg: Color::Indexed(24),
        selection_fg: Color::White,

        modal_dim_bg: Color::Indexed(235),
        modal_bg: Color::Indexed(236),
        modal_border: Color::Cyan,

        inverted_fg: Color::Black,

        app_bg: Color::Reset,

        nerd_font_enabled: false,
    }
}

fn catppuccin_mocha_palette() -> ThemePalette {
    let mauve = Color::Rgb(0xCB, 0xA6, 0xF7);
    let pink = Color::Rgb(0xF5, 0xC2, 0xE7);
    let green = Color::Rgb(0xA6, 0xE3, 0xA1);
    let yellow = Color::Rgb(0xF9, 0xE2, 0xAF);
    let red = Color::Rgb(0xF3, 0x8B, 0xA8);
    let blue = Color::Rgb(0x89, 0xB4, 0xFA);
    let teal = Color::Rgb(0x94, 0xE2, 0xD5);
    let text = Color::Rgb(0xCD, 0xD6, 0xF4);
    let subtext = Color::Rgb(0xA6, 0xAD, 0xC8);
    let overlay = Color::Rgb(0x6C, 0x70, 0x86);
    let surface0 = Color::Rgb(0x31, 0x32, 0x44);
    let surface1 = Color::Rgb(0x45, 0x47, 0x5A);
    let base = Color::Rgb(0x1E, 0x1E, 0x2E);

    ThemePalette {
        accent: mauve,
        accent_bright: pink,

        status_busy: green,
        status_waiting: yellow,
        status_idle: overlay,
        status_error: red,

        text_primary: text,
        text_secondary: subtext,
        text_muted: overlay,

        border_focused: mauve,
        border_unfocused: surface1,

        role_name: pink,
        admin_badge: yellow,
        branch_name: green,
        search_bar: blue,

        keybind_hint: yellow,
        tool_allowed: green,
        tool_disallowed: red,

        admin_border: yellow,
        danger: red,

        selection_bg: surface1,
        selection_fg: text,

        modal_dim_bg: base,
        modal_bg: surface0,
        modal_border: teal,

        inverted_fg: base,

        app_bg: base,

        nerd_font_enabled: false,
    }
}

fn tokyo_night_palette() -> ThemePalette {
    let blue = Color::Rgb(0x7A, 0xA2, 0xF7);
    let cyan = Color::Rgb(0x7D, 0xCF, 0xFF);
    let purple = Color::Rgb(0xBB, 0x9A, 0xF7);
    let green = Color::Rgb(0x9E, 0xCE, 0x6A);
    let yellow = Color::Rgb(0xE0, 0xAF, 0x68);
    let orange = Color::Rgb(0xFF, 0x9E, 0x64);
    let red = Color::Rgb(0xF7, 0x76, 0x8E);
    let magenta = Color::Rgb(0xC0, 0xCA, 0xF5);
    let text = Color::Rgb(0xC0, 0xCA, 0xF5);
    let subtext = Color::Rgb(0x9A, 0xA5, 0xCE);
    let muted = Color::Rgb(0x56, 0x5F, 0x89);
    let bg = Color::Rgb(0x1A, 0x1B, 0x26);
    let bg_dark = Color::Rgb(0x16, 0x16, 0x1E);
    let bg_highlight = Color::Rgb(0x29, 0x2E, 0x42);

    ThemePalette {
        accent: blue,
        accent_bright: cyan,

        status_busy: green,
        status_waiting: yellow,
        status_idle: muted,
        status_error: red,

        text_primary: text,
        text_secondary: subtext,
        text_muted: muted,

        border_focused: blue,
        border_unfocused: bg_highlight,

        role_name: purple,
        admin_badge: orange,
        branch_name: green,
        search_bar: cyan,

        keybind_hint: yellow,
        tool_allowed: green,
        tool_disallowed: red,

        admin_border: orange,
        danger: red,

        selection_bg: bg_highlight,
        selection_fg: magenta,

        modal_dim_bg: bg_dark,
        modal_bg: bg,
        modal_border: blue,

        inverted_fg: bg,

        app_bg: bg,

        nerd_font_enabled: false,
    }
}

fn gruvbox_dark_palette() -> ThemePalette {
    let yellow = Color::Rgb(0xFA, 0xBD, 0x2F);
    let orange = Color::Rgb(0xFE, 0x80, 0x19);
    let red = Color::Rgb(0xFB, 0x49, 0x34);
    let green = Color::Rgb(0xB8, 0xBB, 0x26);
    let aqua = Color::Rgb(0x8E, 0xC0, 0x7C);
    let blue = Color::Rgb(0x83, 0xA5, 0x98);
    let purple = Color::Rgb(0xD3, 0x86, 0x9B);
    let fg = Color::Rgb(0xEB, 0xDB, 0xB2);
    let fg2 = Color::Rgb(0xD5, 0xC4, 0xA1);
    let gray = Color::Rgb(0x92, 0x83, 0x74);
    let bg0 = Color::Rgb(0x28, 0x28, 0x28);
    let bg1 = Color::Rgb(0x3C, 0x38, 0x36);
    let bg_hard = Color::Rgb(0x1D, 0x20, 0x21);

    ThemePalette {
        accent: yellow,
        accent_bright: orange,

        status_busy: green,
        status_waiting: yellow,
        status_idle: gray,
        status_error: red,

        text_primary: fg,
        text_secondary: fg2,
        text_muted: gray,

        border_focused: yellow,
        border_unfocused: bg1,

        role_name: purple,
        admin_badge: orange,
        branch_name: aqua,
        search_bar: blue,

        keybind_hint: orange,
        tool_allowed: green,
        tool_disallowed: red,

        admin_border: orange,
        danger: red,

        selection_bg: bg1,
        selection_fg: fg,

        modal_dim_bg: bg_hard,
        modal_bg: bg0,
        modal_border: yellow,

        inverted_fg: bg0,

        app_bg: bg0,

        nerd_font_enabled: false,
    }
}

fn catppuccin_latte_palette() -> ThemePalette {
    let mauve = Color::Rgb(0x88, 0x39, 0xEF);
    let pink = Color::Rgb(0xEA, 0x76, 0xCB);
    let green = Color::Rgb(0x40, 0xA0, 0x2B);
    let yellow = Color::Rgb(0xDF, 0x8E, 0x1D);
    let red = Color::Rgb(0xD2, 0x0F, 0x39);
    let blue = Color::Rgb(0x1E, 0x66, 0xF5);
    let teal = Color::Rgb(0x17, 0x92, 0x99);
    let text = Color::Rgb(0x4C, 0x4F, 0x69);
    let subtext = Color::Rgb(0x6C, 0x6F, 0x85);
    let overlay = Color::Rgb(0x9C, 0xA0, 0xB0);
    let surface0 = Color::Rgb(0xCC, 0xD0, 0xDA);
    let surface1 = Color::Rgb(0xBC, 0xC0, 0xCC);
    let base = Color::Rgb(0xEF, 0xF1, 0xF5);
    let crust = Color::Rgb(0xDC, 0xE0, 0xE8);

    ThemePalette {
        accent: mauve,
        accent_bright: pink,

        status_busy: green,
        status_waiting: yellow,
        status_idle: overlay,
        status_error: red,

        text_primary: text,
        text_secondary: subtext,
        text_muted: overlay,

        border_focused: mauve,
        border_unfocused: surface1,

        role_name: pink,
        admin_badge: yellow,
        branch_name: green,
        search_bar: blue,

        keybind_hint: yellow,
        tool_allowed: green,
        tool_disallowed: red,

        admin_border: yellow,
        danger: red,

        selection_bg: surface0,
        selection_fg: text,

        modal_dim_bg: crust,
        modal_bg: base,
        modal_border: teal,

        inverted_fg: base,

        app_bg: base,

        nerd_font_enabled: false,
    }
}

fn tokyo_night_day_palette() -> ThemePalette {
    let blue = Color::Rgb(0x2E, 0x7D, 0xE9);
    let cyan = Color::Rgb(0x00, 0x71, 0x97);
    let purple = Color::Rgb(0x98, 0x54, 0xF1);
    let magenta = Color::Rgb(0x78, 0x47, 0xBD);
    let green = Color::Rgb(0x58, 0x75, 0x39);
    let yellow = Color::Rgb(0x8C, 0x6C, 0x3E);
    let orange = Color::Rgb(0xB1, 0x5C, 0x00);
    let red = Color::Rgb(0xF5, 0x2A, 0x65);
    let text = Color::Rgb(0x37, 0x60, 0xBF);
    let subtext = Color::Rgb(0x6A, 0x6E, 0x8A);
    let muted = Color::Rgb(0x84, 0x8C, 0xB5);
    let bg = Color::Rgb(0xE1, 0xE2, 0xE7);
    let bg_dark = Color::Rgb(0xD0, 0xD5, 0xE1);
    let bg_highlight = Color::Rgb(0xC4, 0xC8, 0xDA);

    ThemePalette {
        accent: blue,
        accent_bright: cyan,

        status_busy: green,
        status_waiting: yellow,
        status_idle: muted,
        status_error: red,

        text_primary: text,
        text_secondary: subtext,
        text_muted: muted,

        border_focused: blue,
        border_unfocused: bg_highlight,

        role_name: purple,
        admin_badge: orange,
        branch_name: green,
        search_bar: cyan,

        keybind_hint: orange,
        tool_allowed: green,
        tool_disallowed: red,

        admin_border: orange,
        danger: red,

        selection_bg: bg_highlight,
        selection_fg: magenta,

        modal_dim_bg: bg_dark,
        modal_bg: bg,
        modal_border: blue,

        inverted_fg: bg,

        app_bg: bg,

        nerd_font_enabled: false,
    }
}

fn gruvbox_light_palette() -> ThemePalette {
    let yellow = Color::Rgb(0xB5, 0x76, 0x14);
    let orange = Color::Rgb(0xAF, 0x3A, 0x03);
    let red = Color::Rgb(0x9D, 0x00, 0x06);
    let green = Color::Rgb(0x79, 0x74, 0x0E);
    let aqua = Color::Rgb(0x42, 0x7B, 0x58);
    let blue = Color::Rgb(0x07, 0x66, 0x78);
    let purple = Color::Rgb(0x8F, 0x3F, 0x71);
    let fg = Color::Rgb(0x3C, 0x38, 0x36);
    let fg2 = Color::Rgb(0x50, 0x49, 0x45);
    let gray = Color::Rgb(0x7C, 0x6F, 0x64);
    let bg0 = Color::Rgb(0xFB, 0xF1, 0xC7);
    let bg1 = Color::Rgb(0xEB, 0xDB, 0xB2);
    let bg_soft = Color::Rgb(0xF2, 0xE5, 0xBC);

    ThemePalette {
        accent: orange,
        accent_bright: yellow,

        status_busy: green,
        status_waiting: yellow,
        status_idle: gray,
        status_error: red,

        text_primary: fg,
        text_secondary: fg2,
        text_muted: gray,

        border_focused: orange,
        border_unfocused: bg1,

        role_name: purple,
        admin_badge: yellow,
        branch_name: aqua,
        search_bar: blue,

        keybind_hint: yellow,
        tool_allowed: green,
        tool_disallowed: red,

        admin_border: yellow,
        danger: red,

        selection_bg: bg1,
        selection_fg: fg,

        modal_dim_bg: bg_soft,
        modal_bg: bg0,
        modal_border: orange,

        inverted_fg: bg0,

        app_bg: bg0,

        nerd_font_enabled: false,
    }
}

fn solarized_light_palette() -> ThemePalette {
    let yellow = Color::Rgb(0xB5, 0x89, 0x00);
    let orange = Color::Rgb(0xCB, 0x4B, 0x16);
    let red = Color::Rgb(0xDC, 0x32, 0x2F);
    let magenta = Color::Rgb(0xD3, 0x36, 0x82);
    let violet = Color::Rgb(0x6C, 0x71, 0xC4);
    let blue = Color::Rgb(0x26, 0x8B, 0xD2);
    let cyan = Color::Rgb(0x2A, 0xA1, 0x98);
    let green = Color::Rgb(0x85, 0x99, 0x00);
    let base00 = Color::Rgb(0x65, 0x7B, 0x83);
    let base01 = Color::Rgb(0x58, 0x6E, 0x75);
    let base1 = Color::Rgb(0x93, 0xA1, 0xA1);
    let base2 = Color::Rgb(0xEE, 0xE8, 0xD5);
    let base3 = Color::Rgb(0xFD, 0xF6, 0xE3);

    ThemePalette {
        accent: blue,
        accent_bright: cyan,

        status_busy: green,
        status_waiting: yellow,
        status_idle: base1,
        status_error: red,

        text_primary: base01,
        text_secondary: base00,
        text_muted: base1,

        border_focused: blue,
        border_unfocused: base2,

        role_name: magenta,
        admin_badge: orange,
        branch_name: green,
        search_bar: violet,

        keybind_hint: orange,
        tool_allowed: green,
        tool_disallowed: red,

        admin_border: orange,
        danger: red,

        selection_bg: base2,
        selection_fg: base01,

        modal_dim_bg: base2,
        modal_bg: base3,
        modal_border: blue,

        inverted_fg: base3,

        app_bg: base3,

        nerd_font_enabled: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_presets_round_trip_through_string() {
        for preset in ThemePreset::all() {
            let id = preset.as_str();
            assert_eq!(ThemePreset::from_str(id), Some(*preset));
        }
    }

    #[test]
    fn unknown_preset_returns_none() {
        assert_eq!(ThemePreset::from_str("nope"), None);
    }

    #[test]
    fn default_palette_matches_default_preset() {
        assert_eq!(ThemePalette::default(), ThemePreset::Default.palette());
    }

    #[test]
    fn presets_have_distinct_accents() {
        let mut accents = std::collections::HashSet::new();
        for preset in ThemePreset::all() {
            // Different presets should pick different accent colours so the
            // theme picker preview swatch shows visible variation.
            assert!(
                accents.insert(format!("{:?}", preset.palette().accent)),
                "preset {preset:?} duplicates an existing accent colour",
            );
        }
    }

    #[test]
    fn display_names_are_human_readable() {
        assert_eq!(
            ThemePreset::CatppuccinMocha.display_name(),
            "Catppuccin Mocha"
        );
        assert_eq!(ThemePreset::TokyoNight.display_name(), "Tokyo Night");
    }
}
