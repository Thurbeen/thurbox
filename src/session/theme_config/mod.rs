//! Theme palettes for the Thurbox TUI.
//!
//! A `ThemePalette` is the runtime, swappable palette. Widgets read the active
//! palette via `crate::kernel::theme`; users pick one via the theme
//! picker modal or by setting `active_theme` in the SQLite metadata table.

mod palettes;

use palettes::*;

use ratatui::style::Color;

/// Colour values + glyph preferences for the entire TUI.
///
/// Palettes are never persisted directly — only the preset *name* is stored
/// in the SQLite `metadata.active_theme` row. The runtime palette is
/// reconstructed from `ThemePreset::palette()` on load.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemePalette {
    pub accent: Color,
    pub accent_bright: Color,

    pub status_working: Color,
    pub status_blocked: Color,
    pub status_done: Color,
    pub status_idle: Color,
    pub status_error: Color,
    /// Colour of the "unreachable" status glyph (a remote host that is down /
    /// offline). Muted grey by default so a placeholder row reads as inert, not
    /// urgent.
    pub status_unreachable: Color,

    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_muted: Color,

    pub border_focused: Color,
    pub border_unfocused: Color,

    pub role_name: Color,
    pub branch_name: Color,
    pub search_bar: Color,

    pub keybind_hint: Color,
    pub tool_allowed: Color,
    pub tool_disallowed: Color,

    pub danger: Color,

    pub selection_bg: Color,
    pub selection_fg: Color,

    pub modal_dim_bg: Color,
    pub modal_bg: Color,
    pub modal_border: Color,

    pub inverted_fg: Color,

    /// Code-review diff colours: foreground for added / removed lines and a
    /// subtle full-row background tint for each (GitHub-style). Used by the
    /// native review view (`ui::code_review`).
    pub diff_added: Color,
    pub diff_removed: Color,
    pub diff_added_bg: Color,
    pub diff_removed_bg: Color,

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
    /// Doom — dark hellish palette inspired by Doom Eternal with bright red and neon green.
    Doom,
    /// Nord — cool arctic blue palette from the Nord project.
    Nord,
    /// Dracula — iconic dark palette with purple, pink, and cyan accents.
    Dracula,
    /// One Dark — Atom's signature dark palette.
    OneDark,
    /// Rosé Pine Moon — soft, muted dark palette with iris and rose accents.
    RosePineMoon,
    /// Everforest — comfortable green-tinted dark palette.
    Everforest,
    /// Kanagawa — muted, ink-wash dark palette inspired by Hokusai.
    Kanagawa,
    /// Solarized Dark — Ethan Schoonover's classic dark palette.
    SolarizedDark,
    /// Monokai — the high-saturation classic from Sublime Text.
    Monokai,
    /// Ayu Dark — deep navy with warm orange accents.
    AyuDark,
    /// Ayu Mirage — Ayu's mid-tone slate variant.
    AyuMirage,
    /// Material — the Material Theme "Darker" palette.
    Material,
    /// Rosé Pine — the main (dark) Rosé Pine variant.
    RosePine,
    /// Oxocarbon — IBM Carbon-derived dark palette.
    Oxocarbon,
    /// GitHub Dark — GitHub's own dark interface palette.
    GithubDark,
    /// Nightfox — the Nightfox Neovim colourscheme.
    Nightfox,
    /// Sonokai — a vivid Monokai Pro descendant.
    Sonokai,
    /// Melange — warm, low-saturation dark palette.
    Melange,
    /// Zenburn — the low-contrast classic.
    Zenburn,
    /// Iceberg — cool bluish dark palette.
    Iceberg,
    /// Vesper — near-monochrome dark palette with amber accents.
    Vesper,
    /// Synthwave — neon retrowave palette on deep purple.
    Synthwave,
    /// Nightfly — dark navy palette with bright accents.
    Nightfly,
    /// Tomorrow Night — the Tomorrow theme's dark variant.
    TomorrowNight,
    /// Ayu Light — Ayu's cream-background light variant.
    AyuLight,
    /// One Light — Atom's light counterpart to One Dark.
    OneLight,
    /// Rosé Pine Dawn — Rosé Pine's light variant.
    RosePineDawn,
    /// GitHub Light — GitHub's own light interface palette.
    GithubLight,
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
            Self::Doom => "doom",
            Self::Nord => "nord",
            Self::Dracula => "dracula",
            Self::OneDark => "one-dark",
            Self::RosePineMoon => "rose-pine-moon",
            Self::Everforest => "everforest",
            Self::Kanagawa => "kanagawa",
            Self::SolarizedDark => "solarized-dark",
            Self::Monokai => "monokai",
            Self::AyuDark => "ayu-dark",
            Self::AyuMirage => "ayu-mirage",
            Self::Material => "material",
            Self::RosePine => "rose-pine",
            Self::Oxocarbon => "oxocarbon",
            Self::GithubDark => "github-dark",
            Self::Nightfox => "nightfox",
            Self::Sonokai => "sonokai",
            Self::Melange => "melange",
            Self::Zenburn => "zenburn",
            Self::Iceberg => "iceberg",
            Self::Vesper => "vesper",
            Self::Synthwave => "synthwave",
            Self::Nightfly => "nightfly",
            Self::TomorrowNight => "tomorrow-night",
            Self::AyuLight => "ayu-light",
            Self::OneLight => "one-light",
            Self::RosePineDawn => "rose-pine-dawn",
            Self::GithubLight => "github-light",
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
            Self::Doom => "Doom",
            Self::Nord => "Nord",
            Self::Dracula => "Dracula",
            Self::OneDark => "One Dark",
            Self::RosePineMoon => "Rosé Pine Moon",
            Self::Everforest => "Everforest",
            Self::Kanagawa => "Kanagawa",
            Self::SolarizedDark => "Solarized Dark",
            Self::Monokai => "Monokai",
            Self::AyuDark => "Ayu Dark",
            Self::AyuMirage => "Ayu Mirage",
            Self::Material => "Material",
            Self::RosePine => "Rosé Pine",
            Self::Oxocarbon => "Oxocarbon",
            Self::GithubDark => "GitHub Dark",
            Self::Nightfox => "Nightfox",
            Self::Sonokai => "Sonokai",
            Self::Melange => "Melange",
            Self::Zenburn => "Zenburn",
            Self::Iceberg => "Iceberg",
            Self::Vesper => "Vesper",
            Self::Synthwave => "Synthwave",
            Self::Nightfly => "Nightfly",
            Self::TomorrowNight => "Tomorrow Night",
            Self::AyuLight => "Ayu Light",
            Self::OneLight => "One Light",
            Self::RosePineDawn => "Rosé Pine Dawn",
            Self::GithubLight => "GitHub Light",
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
            "doom" => Some(Self::Doom),
            "nord" => Some(Self::Nord),
            "dracula" => Some(Self::Dracula),
            "one-dark" => Some(Self::OneDark),
            "rose-pine-moon" => Some(Self::RosePineMoon),
            "everforest" => Some(Self::Everforest),
            "kanagawa" => Some(Self::Kanagawa),
            "solarized-dark" => Some(Self::SolarizedDark),
            "monokai" => Some(Self::Monokai),
            "ayu-dark" => Some(Self::AyuDark),
            "ayu-mirage" => Some(Self::AyuMirage),
            "material" => Some(Self::Material),
            "rose-pine" => Some(Self::RosePine),
            "oxocarbon" => Some(Self::Oxocarbon),
            "github-dark" => Some(Self::GithubDark),
            "nightfox" => Some(Self::Nightfox),
            "sonokai" => Some(Self::Sonokai),
            "melange" => Some(Self::Melange),
            "zenburn" => Some(Self::Zenburn),
            "iceberg" => Some(Self::Iceberg),
            "vesper" => Some(Self::Vesper),
            "synthwave" => Some(Self::Synthwave),
            "nightfly" => Some(Self::Nightfly),
            "tomorrow-night" => Some(Self::TomorrowNight),
            "ayu-light" => Some(Self::AyuLight),
            "one-light" => Some(Self::OneLight),
            "rose-pine-dawn" => Some(Self::RosePineDawn),
            "github-light" => Some(Self::GithubLight),
            _ => None,
        }
    }

    pub fn all() -> &'static [ThemePreset] {
        // Dark presets first, then the light ones grouped at the end so the
        // picker shows all dark themes together before the light section.
        &[
            Self::Default,
            Self::CatppuccinMocha,
            Self::TokyoNight,
            Self::GruvboxDark,
            Self::Doom,
            Self::Nord,
            Self::Dracula,
            Self::OneDark,
            Self::RosePineMoon,
            Self::Everforest,
            Self::Kanagawa,
            Self::SolarizedDark,
            Self::Monokai,
            Self::AyuDark,
            Self::AyuMirage,
            Self::Material,
            Self::RosePine,
            Self::Oxocarbon,
            Self::GithubDark,
            Self::Nightfox,
            Self::Sonokai,
            Self::Melange,
            Self::Zenburn,
            Self::Iceberg,
            Self::Vesper,
            Self::Synthwave,
            Self::Nightfly,
            Self::TomorrowNight,
            Self::CatppuccinLatte,
            Self::TokyoNightDay,
            Self::GruvboxLight,
            Self::SolarizedLight,
            Self::AyuLight,
            Self::OneLight,
            Self::RosePineDawn,
            Self::GithubLight,
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
            Self::Doom => doom_palette(),
            Self::Nord => nord_palette(),
            Self::Dracula => dracula_palette(),
            Self::OneDark => one_dark_palette(),
            Self::RosePineMoon => rose_pine_moon_palette(),
            Self::Everforest => everforest_palette(),
            Self::Kanagawa => kanagawa_palette(),
            Self::SolarizedDark => solarized_dark_palette(),
            Self::Monokai => monokai_palette(),
            Self::AyuDark => ayu_dark_palette(),
            Self::AyuMirage => ayu_mirage_palette(),
            Self::Material => material_palette(),
            Self::RosePine => rose_pine_palette(),
            Self::Oxocarbon => oxocarbon_palette(),
            Self::GithubDark => github_dark_palette(),
            Self::Nightfox => nightfox_palette(),
            Self::Sonokai => sonokai_palette(),
            Self::Melange => melange_palette(),
            Self::Zenburn => zenburn_palette(),
            Self::Iceberg => iceberg_palette(),
            Self::Vesper => vesper_palette(),
            Self::Synthwave => synthwave_palette(),
            Self::Nightfly => nightfly_palette(),
            Self::TomorrowNight => tomorrow_night_palette(),
            Self::AyuLight => ayu_light_palette(),
            Self::OneLight => one_light_palette(),
            Self::RosePineDawn => rose_pine_dawn_palette(),
            Self::GithubLight => github_light_palette(),
        }
    }

    /// Whether this preset is intended for terminals with a light background.
    /// Used by the picker to group dark and light themes.
    pub fn is_light(self) -> bool {
        matches!(
            self,
            Self::CatppuccinLatte
                | Self::TokyoNightDay
                | Self::GruvboxLight
                | Self::SolarizedLight
                | Self::AyuLight
                | Self::OneLight
                | Self::RosePineDawn
                | Self::GithubLight
        )
    }
}

/// A selectable theme — a built-in preset or a user-defined custom theme —
/// flattened to what the picker and the apply path need.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeEntry {
    /// Stable identifier persisted in `metadata.active_theme`.
    pub name: String,
    /// Human-readable label shown in the picker and status bar.
    pub display_name: String,
    pub palette: ThemePalette,
    pub is_light: bool,
}

impl ThemeEntry {
    pub fn from_preset(preset: ThemePreset) -> Self {
        Self {
            name: preset.as_str().to_string(),
            display_name: preset.display_name().to_string(),
            palette: preset.palette(),
            is_light: preset.is_light(),
        }
    }
}

/// A user-defined theme from `themes.toml`: a base preset plus per-colour
/// overrides. Colours accept anything ratatui parses — `#rrggbb`, ANSI names
/// (`red`, `lightcyan`), indexed (`14`), or `reset`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CustomThemeDef {
    /// Stable identifier (what `metadata.active_theme` stores). Must not
    /// collide with a built-in preset name.
    pub name: String,
    /// Picker label; defaults to `name`.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Built-in preset the palette starts from; defaults to `default`.
    #[serde(default)]
    pub base: Option<String>,
    /// Whether the theme targets light terminals; defaults to the base's.
    #[serde(default)]
    pub light: Option<bool>,
    /// Nerd-font glyph opt-in; defaults to the base's.
    #[serde(default)]
    pub nerd_font: Option<bool>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub accent_bright: Option<String>,
    #[serde(default)]
    pub status_working: Option<String>,
    #[serde(default)]
    pub status_blocked: Option<String>,
    #[serde(default)]
    pub status_done: Option<String>,
    #[serde(default)]
    pub status_idle: Option<String>,
    #[serde(default)]
    pub status_error: Option<String>,
    #[serde(default)]
    pub status_unreachable: Option<String>,
    #[serde(default)]
    pub text_primary: Option<String>,
    #[serde(default)]
    pub text_secondary: Option<String>,
    #[serde(default)]
    pub text_muted: Option<String>,
    #[serde(default)]
    pub border_focused: Option<String>,
    #[serde(default)]
    pub border_unfocused: Option<String>,
    #[serde(default)]
    pub role_name: Option<String>,
    #[serde(default)]
    pub branch_name: Option<String>,
    #[serde(default)]
    pub search_bar: Option<String>,
    #[serde(default)]
    pub keybind_hint: Option<String>,
    #[serde(default)]
    pub tool_allowed: Option<String>,
    #[serde(default)]
    pub tool_disallowed: Option<String>,
    #[serde(default)]
    pub danger: Option<String>,
    #[serde(default)]
    pub selection_bg: Option<String>,
    #[serde(default)]
    pub selection_fg: Option<String>,
    #[serde(default)]
    pub modal_dim_bg: Option<String>,
    #[serde(default)]
    pub modal_bg: Option<String>,
    #[serde(default)]
    pub modal_border: Option<String>,
    #[serde(default)]
    pub inverted_fg: Option<String>,
    #[serde(default)]
    pub diff_added: Option<String>,
    #[serde(default)]
    pub diff_removed: Option<String>,
    #[serde(default)]
    pub diff_added_bg: Option<String>,
    #[serde(default)]
    pub diff_removed_bg: Option<String>,
    #[serde(default)]
    pub app_bg: Option<String>,
}

/// `themes.toml` document shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ThemesFile {
    /// Config-format version, for future migrations. Currently `1`.
    #[serde(default)]
    pub config_version: Option<u32>,
    #[serde(default)]
    pub themes: Vec<CustomThemeDef>,
}

/// Apply one colour override, recording a warning instead of failing when the
/// value doesn't parse (the base colour stays in effect).
fn apply_color(
    warnings: &mut Vec<String>,
    theme: &str,
    field: &str,
    value: &Option<String>,
    slot: &mut Color,
) {
    let Some(raw) = value else { return };
    match raw.parse::<Color>() {
        Ok(color) => *slot = color,
        Err(_) => warnings.push(format!(
            "theme \"{theme}\": invalid colour \"{raw}\" for {field} (kept base colour)"
        )),
    }
}

impl CustomThemeDef {
    /// Number of per-colour override fields wired into [`Self::resolve`]'s
    /// `fields` array. Tied to the array's length at compile time; a test
    /// (`override_array_covers_every_color_field`) asserts it matches the
    /// struct's colour-field count, so a forgotten row can't silently make a
    /// colour un-overridable.
    const COLOR_OVERRIDE_COUNT: usize = 31;

    /// Materialise the theme: base preset palette + overrides. Unparsable
    /// colours and an unknown base degrade to warnings, never to a hard
    /// failure — a half-styled theme beats no theme.
    pub fn resolve(&self) -> (ThemeEntry, Vec<String>) {
        let mut warnings = Vec::new();
        let base = match self.base.as_deref() {
            None => ThemePreset::Default,
            Some(name) => ThemePreset::from_str(name).unwrap_or_else(|| {
                warnings.push(format!(
                    "theme \"{}\": unknown base \"{name}\" (using default)",
                    self.name
                ));
                ThemePreset::Default
            }),
        };
        let mut palette = base.palette();
        if let Some(nerd) = self.nerd_font {
            palette.nerd_font_enabled = nerd;
        }

        let fields: [(&str, &Option<String>, &mut Color); Self::COLOR_OVERRIDE_COUNT] = [
            ("accent", &self.accent, &mut palette.accent),
            (
                "accent_bright",
                &self.accent_bright,
                &mut palette.accent_bright,
            ),
            (
                "status_working",
                &self.status_working,
                &mut palette.status_working,
            ),
            (
                "status_blocked",
                &self.status_blocked,
                &mut palette.status_blocked,
            ),
            ("status_done", &self.status_done, &mut palette.status_done),
            ("status_idle", &self.status_idle, &mut palette.status_idle),
            (
                "status_error",
                &self.status_error,
                &mut palette.status_error,
            ),
            (
                "status_unreachable",
                &self.status_unreachable,
                &mut palette.status_unreachable,
            ),
            (
                "text_primary",
                &self.text_primary,
                &mut palette.text_primary,
            ),
            (
                "text_secondary",
                &self.text_secondary,
                &mut palette.text_secondary,
            ),
            ("text_muted", &self.text_muted, &mut palette.text_muted),
            (
                "border_focused",
                &self.border_focused,
                &mut palette.border_focused,
            ),
            (
                "border_unfocused",
                &self.border_unfocused,
                &mut palette.border_unfocused,
            ),
            ("role_name", &self.role_name, &mut palette.role_name),
            ("branch_name", &self.branch_name, &mut palette.branch_name),
            ("search_bar", &self.search_bar, &mut palette.search_bar),
            (
                "keybind_hint",
                &self.keybind_hint,
                &mut palette.keybind_hint,
            ),
            (
                "tool_allowed",
                &self.tool_allowed,
                &mut palette.tool_allowed,
            ),
            (
                "tool_disallowed",
                &self.tool_disallowed,
                &mut palette.tool_disallowed,
            ),
            ("danger", &self.danger, &mut palette.danger),
            (
                "selection_bg",
                &self.selection_bg,
                &mut palette.selection_bg,
            ),
            (
                "selection_fg",
                &self.selection_fg,
                &mut palette.selection_fg,
            ),
            (
                "modal_dim_bg",
                &self.modal_dim_bg,
                &mut palette.modal_dim_bg,
            ),
            ("modal_bg", &self.modal_bg, &mut palette.modal_bg),
            (
                "modal_border",
                &self.modal_border,
                &mut palette.modal_border,
            ),
            ("inverted_fg", &self.inverted_fg, &mut palette.inverted_fg),
            ("diff_added", &self.diff_added, &mut palette.diff_added),
            (
                "diff_removed",
                &self.diff_removed,
                &mut palette.diff_removed,
            ),
            (
                "diff_added_bg",
                &self.diff_added_bg,
                &mut palette.diff_added_bg,
            ),
            (
                "diff_removed_bg",
                &self.diff_removed_bg,
                &mut palette.diff_removed_bg,
            ),
            ("app_bg", &self.app_bg, &mut palette.app_bg),
        ];
        for (field, value, slot) in fields {
            apply_color(&mut warnings, &self.name, field, value, slot);
        }

        let entry = ThemeEntry {
            name: self.name.clone(),
            display_name: self
                .display_name
                .clone()
                .unwrap_or_else(|| self.name.clone()),
            palette,
            is_light: self.light.unwrap_or_else(|| base.is_light()),
        };
        (entry, warnings)
    }
}

fn default_palette() -> ThemePalette {
    ThemePalette {
        accent: Color::Cyan,
        accent_bright: Color::LightCyan,

        status_working: Color::Yellow,
        status_blocked: Color::Red,
        status_done: Color::LightBlue,
        status_idle: Color::Green,
        status_error: Color::Red,
        status_unreachable: Color::DarkGray,

        text_primary: Color::White,
        text_secondary: Color::Gray,
        text_muted: Color::DarkGray,

        border_focused: Color::Cyan,
        border_unfocused: Color::Gray,

        role_name: Color::Magenta,
        branch_name: Color::Green,
        search_bar: Color::Blue,

        keybind_hint: Color::Yellow,
        tool_allowed: Color::Green,
        tool_disallowed: Color::Red,

        danger: Color::Red,

        selection_bg: Color::Indexed(24),
        selection_fg: Color::White,

        modal_dim_bg: Color::Indexed(235),
        modal_bg: Color::Indexed(236),
        modal_border: Color::Cyan,

        inverted_fg: Color::Black,

        // Named-colour preset: green/red fg with dim Indexed backgrounds that
        // read on a dark terminal (the Default preset keeps app_bg = Reset).
        diff_added: Color::Green,
        diff_removed: Color::Red,
        diff_added_bg: Color::Indexed(22),
        diff_removed_bg: Color::Indexed(52),

        app_bg: Color::Reset,

        nerd_font_enabled: false,
    }
}

/// Compact intermediate representation used to construct the seven RGB-based
/// palettes without restating the 27-field `ThemePalette` literal in each one.
///
/// `build()` derives the symmetric fields (status_idle = text_muted,
/// border_focused = accent, inverted_fg = app_bg) so each preset only specifies
/// the colours that actually vary between themes.
struct PaletteSlots {
    accent: Color,
    accent_bright: Color,
    green: Color,
    yellow: Color,
    red: Color,
    blue: Color,
    text_primary: Color,
    text_secondary: Color,
    text_muted: Color,
    border_unfocused: Color,
    role_name: Color,
    branch_name: Color,
    search_bar: Color,
    keybind_hint: Color,
    selection_bg: Color,
    selection_fg: Color,
    modal_dim_bg: Color,
    modal_bg: Color,
    modal_border: Color,
    base_bg: Color,
}

impl PaletteSlots {
    fn build(self) -> ThemePalette {
        ThemePalette {
            accent: self.accent,
            accent_bright: self.accent_bright,
            status_working: self.yellow,
            status_blocked: self.red,
            status_done: self.blue,
            status_idle: self.green,
            status_error: self.red,
            // Muted grey: an unreachable placeholder is inert, not urgent.
            status_unreachable: self.text_muted,
            text_primary: self.text_primary,
            text_secondary: self.text_secondary,
            text_muted: self.text_muted,
            border_focused: self.accent,
            border_unfocused: self.border_unfocused,
            role_name: self.role_name,
            branch_name: self.branch_name,
            search_bar: self.search_bar,
            keybind_hint: self.keybind_hint,
            tool_allowed: self.green,
            tool_disallowed: self.red,
            danger: self.red,
            selection_bg: self.selection_bg,
            selection_fg: self.selection_fg,
            modal_dim_bg: self.modal_dim_bg,
            modal_bg: self.modal_bg,
            modal_border: self.modal_border,
            inverted_fg: self.base_bg,
            // Diff fg = the theme's green/red; bg = that hue blended ~82% toward
            // the app background for a subtle full-row tint that works on both
            // dark and light presets (these all use RGB base colours).
            diff_added: self.green,
            diff_removed: self.red,
            diff_added_bg: blend_rgb(self.green, self.base_bg, 0.82),
            diff_removed_bg: blend_rgb(self.red, self.base_bg, 0.82),
            app_bg: self.base_bg,
            nerd_font_enabled: false,
        }
    }
}

/// Linear-blend `a` toward `b` by `t` (0.0 = all `a`, 1.0 = all `b`). Only
/// meaningful for `Color::Rgb`; if either side isn't RGB it returns `a`
/// unchanged (the `Default` preset, which is named-colour, sets diff bgs
/// explicitly instead).
fn blend_rgb(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let mix = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
            Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
        }
        _ => a,
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

    #[test]
    fn all_enumerates_every_preset_variant() {
        // Exhaustive match: adding a variant fails to compile until it's listed
        // here, and the EXPECTED guard ensures `all()` was also updated. Mirrors
        // keybindings.rs's `all_enumerates_every_action_variant`.
        fn classify(p: ThemePreset) -> u8 {
            match p {
                ThemePreset::Default => 0,
                ThemePreset::CatppuccinMocha => 0,
                ThemePreset::TokyoNight => 0,
                ThemePreset::GruvboxDark => 0,
                ThemePreset::CatppuccinLatte => 0,
                ThemePreset::TokyoNightDay => 0,
                ThemePreset::GruvboxLight => 0,
                ThemePreset::SolarizedLight => 0,
                ThemePreset::Doom => 0,
                ThemePreset::Nord => 0,
                ThemePreset::Dracula => 0,
                ThemePreset::OneDark => 0,
                ThemePreset::RosePineMoon => 0,
                ThemePreset::Everforest => 0,
                ThemePreset::Kanagawa => 0,
                ThemePreset::SolarizedDark => 0,
                ThemePreset::Monokai => 0,
                ThemePreset::AyuDark => 0,
                ThemePreset::AyuMirage => 0,
                ThemePreset::Material => 0,
                ThemePreset::RosePine => 0,
                ThemePreset::Oxocarbon => 0,
                ThemePreset::GithubDark => 0,
                ThemePreset::Nightfox => 0,
                ThemePreset::Sonokai => 0,
                ThemePreset::Melange => 0,
                ThemePreset::Zenburn => 0,
                ThemePreset::Iceberg => 0,
                ThemePreset::Vesper => 0,
                ThemePreset::Synthwave => 0,
                ThemePreset::Nightfly => 0,
                ThemePreset::TomorrowNight => 0,
                ThemePreset::AyuLight => 0,
                ThemePreset::OneLight => 0,
                ThemePreset::RosePineDawn => 0,
                ThemePreset::GithubLight => 0,
            }
        }
        const EXPECTED: usize = 36;
        assert_eq!(ThemePreset::all().len(), EXPECTED);
        for p in ThemePreset::all() {
            classify(*p);
        }
    }

    #[test]
    fn dark_presets_precede_light_presets_in_all() {
        // The picker renders presets in `all()` order; dark themes are grouped
        // first and the light ones at the end. Guard the invariant so a new
        // preset inserted in the wrong place can't scatter the light section.
        let mut seen_light = false;
        for preset in ThemePreset::all() {
            if preset.is_light() {
                seen_light = true;
            } else {
                assert!(
                    !seen_light,
                    "dark preset {preset:?} appears after a light preset in all()",
                );
            }
        }
        assert!(seen_light, "expected at least one light preset in all()");
    }

    #[test]
    fn override_array_covers_every_color_field() {
        // Count the struct's colour-override fields (total serialized fields
        // minus the five non-colour meta fields) and assert it equals the
        // `fields` array length. A new `Option<String>` colour added to the
        // struct but not to `resolve()`'s array would be silently
        // un-overridable; this catches the omission.
        let json = serde_json::to_value(CustomThemeDef::default()).unwrap();
        let total = json.as_object().unwrap().len();
        const META_FIELDS: usize = 5; // name, display_name, base, light, nerd_font
        assert_eq!(total - META_FIELDS, CustomThemeDef::COLOR_OVERRIDE_COUNT);
    }

    #[test]
    fn every_preset_defines_review_diff_colours() {
        // The native code-review view relies on these, so no preset may leave
        // them unset (the build()-derived presets get them from green/red).
        for preset in ThemePreset::all() {
            let p = preset.palette();
            assert_ne!(p.diff_added, p.diff_removed, "{preset:?}");
            // The RGB presets blend the bg toward the base; it must differ from
            // the plain fg so the tint reads as a background.
            if let Color::Rgb(..) = p.diff_added {
                assert_ne!(p.diff_added, p.diff_added_bg, "{preset:?}");
            }
        }
    }

    #[test]
    fn blend_rgb_mixes_and_passes_through_named() {
        let mid = blend_rgb(Color::Rgb(0, 0, 0), Color::Rgb(100, 100, 100), 0.5);
        assert_eq!(mid, Color::Rgb(50, 50, 50));
        // Non-RGB inputs return the first colour unchanged.
        assert_eq!(
            blend_rgb(Color::Green, Color::Rgb(0, 0, 0), 0.5),
            Color::Green
        );
    }

    #[test]
    fn custom_theme_overrides_diff_colour() {
        let def = CustomThemeDef {
            name: "x".into(),
            diff_added: Some("#00ff00".into()),
            ..Default::default()
        };
        let (entry, warnings) = def.resolve();
        assert!(warnings.is_empty());
        assert_eq!(entry.palette.diff_added, Color::Rgb(0, 0xff, 0));
    }

    #[test]
    fn every_preset_distinguishes_its_semantic_slots() {
        // Cross-preset coherence: a copy-paste slip while adding a palette
        // (leaving accent == accent_bright, or a status trio collapsed onto one
        // colour) makes the UI read as broken rather than themed. Assert the
        // pairs that must stay visually distinct in *every* preset.
        for preset in ThemePreset::all() {
            let p = preset.palette();
            assert_ne!(
                p.accent, p.accent_bright,
                "{preset:?}: accent and accent_bright must differ"
            );
            assert_ne!(
                p.status_working, p.status_blocked,
                "{preset:?}: working and blocked must be tellable apart"
            );
            assert_ne!(
                p.status_done, p.status_idle,
                "{preset:?}: done and idle are the done-vs-seen pair"
            );
            assert_ne!(
                p.text_primary, p.text_muted,
                "{preset:?}: primary and muted text must differ"
            );
            // The palette must actually contrast with its own background.
            assert_ne!(
                p.text_primary, p.app_bg,
                "{preset:?}: primary text is invisible on the app background"
            );
            assert_ne!(
                p.selection_fg, p.selection_bg,
                "{preset:?}: selected text is invisible on the selection band"
            );
        }
    }

    #[test]
    fn light_presets_are_light_and_dark_presets_are_dark() {
        // `is_light` drives the picker's Dark/Light sections, so it must agree
        // with the palette's actual background luminance — a mislabeled preset
        // would sort into the wrong section and break the ordering invariant
        // `dark_presets_precede_light_presets_in_all` relies on.
        fn luminance(c: Color) -> Option<f32> {
            match c {
                // Rec. 601 luma, good enough to separate light from dark bg.
                Color::Rgb(r, g, b) => Some(0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32),
                _ => None,
            }
        }
        for preset in ThemePreset::all() {
            let Some(luma) = luminance(preset.palette().app_bg) else {
                continue; // the Default preset keeps app_bg = Reset
            };
            if preset.is_light() {
                assert!(
                    luma > 128.0,
                    "{preset:?} is marked light but its background is dark (luma {luma:.0})"
                );
            } else {
                assert!(
                    luma < 128.0,
                    "{preset:?} is marked dark but its background is light (luma {luma:.0})"
                );
            }
        }
    }
}
