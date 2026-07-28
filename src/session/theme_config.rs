//! Theme palettes for the Thurbox TUI.
//!
//! A `ThemePalette` is the runtime, swappable palette. Widgets read the active
//! palette via `crate::ui::theme::current()`; users pick one via the theme
//! picker modal or by setting `active_theme` in the SQLite metadata table.

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
    PaletteSlots {
        accent: mauve,
        accent_bright: pink,
        green,
        yellow,
        red,
        blue,
        text_primary: text,
        text_secondary: subtext,
        text_muted: overlay,
        border_unfocused: surface1,
        role_name: pink,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: surface1,
        selection_fg: text,
        modal_dim_bg: base,
        modal_bg: surface0,
        modal_border: teal,
        base_bg: base,
    }
    .build()
}

fn tokyo_night_palette() -> ThemePalette {
    let blue = Color::Rgb(0x7A, 0xA2, 0xF7);
    let cyan = Color::Rgb(0x7D, 0xCF, 0xFF);
    let purple = Color::Rgb(0xBB, 0x9A, 0xF7);
    let green = Color::Rgb(0x9E, 0xCE, 0x6A);
    let yellow = Color::Rgb(0xE0, 0xAF, 0x68);
    let red = Color::Rgb(0xF7, 0x76, 0x8E);
    let magenta = Color::Rgb(0xC0, 0xCA, 0xF5);
    let text = Color::Rgb(0xC0, 0xCA, 0xF5);
    let subtext = Color::Rgb(0x9A, 0xA5, 0xCE);
    let muted = Color::Rgb(0x56, 0x5F, 0x89);
    let bg = Color::Rgb(0x1A, 0x1B, 0x26);
    let bg_dark = Color::Rgb(0x16, 0x16, 0x1E);
    let bg_highlight = Color::Rgb(0x29, 0x2E, 0x42);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: text,
        text_secondary: subtext,
        text_muted: muted,
        border_unfocused: bg_highlight,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: bg_highlight,
        selection_fg: magenta,
        modal_dim_bg: bg_dark,
        modal_bg: bg,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
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
    PaletteSlots {
        accent: yellow,
        accent_bright: orange,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg2,
        text_muted: gray,
        border_unfocused: bg1,
        role_name: purple,
        branch_name: aqua,
        search_bar: blue,
        keybind_hint: orange,
        selection_bg: bg1,
        selection_fg: fg,
        modal_dim_bg: bg_hard,
        modal_bg: bg0,
        modal_border: yellow,
        base_bg: bg0,
    }
    .build()
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
    PaletteSlots {
        accent: mauve,
        accent_bright: pink,
        green,
        yellow,
        red,
        blue,
        text_primary: text,
        text_secondary: subtext,
        text_muted: overlay,
        border_unfocused: surface1,
        role_name: pink,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: surface0,
        selection_fg: text,
        modal_dim_bg: crust,
        modal_bg: base,
        modal_border: teal,
        base_bg: base,
    }
    .build()
}

fn tokyo_night_day_palette() -> ThemePalette {
    let blue = Color::Rgb(0x2E, 0x7D, 0xE9);
    let cyan = Color::Rgb(0x00, 0x71, 0x97);
    let purple = Color::Rgb(0x98, 0x54, 0xF1);
    // Deepened from #7847BD so the selected-session name (which now uses
    // `selection_fg`) clears ~4.5:1 contrast on the light `selection_bg` band.
    let magenta = Color::Rgb(0x5A, 0x3A, 0x9E);
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
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: text,
        text_secondary: subtext,
        text_muted: muted,
        border_unfocused: bg_highlight,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: orange,
        selection_bg: bg_highlight,
        selection_fg: magenta,
        modal_dim_bg: bg_dark,
        modal_bg: bg,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
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
    PaletteSlots {
        accent: orange,
        accent_bright: yellow,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg2,
        text_muted: gray,
        border_unfocused: bg1,
        role_name: purple,
        branch_name: aqua,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: bg1,
        selection_fg: fg,
        modal_dim_bg: bg_soft,
        modal_bg: bg0,
        modal_border: orange,
        base_bg: bg0,
    }
    .build()
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
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: base01,
        text_secondary: base00,
        text_muted: base1,
        border_unfocused: base2,
        role_name: magenta,
        branch_name: green,
        search_bar: violet,
        keybind_hint: orange,
        selection_bg: base2,
        selection_fg: base01,
        modal_dim_bg: base2,
        modal_bg: base3,
        modal_border: blue,
        base_bg: base3,
    }
    .build()
}

fn doom_palette() -> ThemePalette {
    let red = Color::Rgb(0xFF, 0x3B, 0x30);
    let bright_red = Color::Rgb(0xFF, 0x5C, 0x54);
    let bright_green = Color::Rgb(0x6E, 0xFF, 0x6E);
    let cyan = Color::Rgb(0x00, 0xD9, 0xFF);
    // Hellfire amber, not `red`: the yellow slot drives `status_working`, and
    // reusing red there made a working session indistinguishable from a
    // blocked one (both `status_blocked` and `status_error` are red).
    let amber = Color::Rgb(0xFF, 0xA5, 0x00);
    let text = Color::Rgb(0xE0, 0xE0, 0xE0);
    let subtext = Color::Rgb(0xA0, 0xA0, 0xA0);
    let muted = Color::Rgb(0x60, 0x60, 0x60);
    let surface = Color::Rgb(0x2A, 0x0F, 0x12);
    let bg_dark = Color::Rgb(0x1A, 0x08, 0x0A);
    let highlight = Color::Rgb(0x40, 0x15, 0x1E);
    PaletteSlots {
        accent: bright_red,
        accent_bright: bright_green,
        green: bright_green,
        yellow: amber,
        red,
        blue: cyan,
        text_primary: text,
        text_secondary: subtext,
        text_muted: muted,
        border_unfocused: highlight,
        role_name: bright_red,
        branch_name: bright_green,
        search_bar: cyan,
        keybind_hint: amber,
        selection_bg: highlight,
        selection_fg: bright_green,
        modal_dim_bg: bg_dark,
        modal_bg: surface,
        modal_border: bright_red,
        base_bg: bg_dark,
    }
    .build()
}

fn nord_palette() -> ThemePalette {
    let frost_cyan = Color::Rgb(0x88, 0xC0, 0xD0); // nord8
    let frost_teal = Color::Rgb(0x8F, 0xBC, 0xBB); // nord7
    let frost_blue = Color::Rgb(0x81, 0xA1, 0xC1); // nord9
    let frost_deep = Color::Rgb(0x5E, 0x81, 0xAC); // nord10
    let green = Color::Rgb(0xA3, 0xBE, 0x8C); // nord14
    let yellow = Color::Rgb(0xEB, 0xCB, 0x8B); // nord13
    let red = Color::Rgb(0xBF, 0x61, 0x6A); // nord11
    let purple = Color::Rgb(0xB4, 0x8E, 0xAD); // nord15
    let snow = Color::Rgb(0xEC, 0xEF, 0xF4); // nord6
    let snow_dim = Color::Rgb(0xD8, 0xDE, 0xE9); // nord4
    let polar3 = Color::Rgb(0x4C, 0x56, 0x6A); // nord3
    let polar2 = Color::Rgb(0x43, 0x4C, 0x5E); // nord2
    let polar1 = Color::Rgb(0x3B, 0x42, 0x52); // nord1
    let polar0 = Color::Rgb(0x2E, 0x34, 0x40); // nord0
    PaletteSlots {
        accent: frost_cyan,
        accent_bright: frost_teal,
        green,
        yellow,
        red,
        blue: frost_blue,
        text_primary: snow,
        text_secondary: snow_dim,
        text_muted: polar3,
        border_unfocused: polar2,
        role_name: purple,
        branch_name: green,
        search_bar: frost_deep,
        keybind_hint: yellow,
        selection_bg: polar2,
        selection_fg: snow,
        modal_dim_bg: polar0,
        modal_bg: polar1,
        modal_border: frost_cyan,
        base_bg: polar0,
    }
    .build()
}

fn dracula_palette() -> ThemePalette {
    let purple = Color::Rgb(0xBD, 0x93, 0xF9);
    let pink = Color::Rgb(0xFF, 0x79, 0xC6);
    let green = Color::Rgb(0x50, 0xFA, 0x7B);
    let yellow = Color::Rgb(0xF1, 0xFA, 0x8C);
    let red = Color::Rgb(0xFF, 0x55, 0x55);
    let cyan = Color::Rgb(0x8B, 0xE9, 0xFD);
    let fg = Color::Rgb(0xF8, 0xF8, 0xF2);
    let fg_dim = Color::Rgb(0xC5, 0xC5, 0xD8);
    let comment = Color::Rgb(0x62, 0x72, 0xA4);
    let current_line = Color::Rgb(0x44, 0x47, 0x5A);
    let bg = Color::Rgb(0x28, 0x2A, 0x36);
    let bg_dark = Color::Rgb(0x21, 0x22, 0x2C);
    PaletteSlots {
        accent: purple,
        accent_bright: pink,
        green,
        yellow,
        red,
        blue: cyan,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: comment,
        border_unfocused: current_line,
        role_name: pink,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: current_line,
        selection_fg: fg,
        modal_dim_bg: bg_dark,
        modal_bg: bg,
        modal_border: purple,
        base_bg: bg,
    }
    .build()
}

fn one_dark_palette() -> ThemePalette {
    let blue = Color::Rgb(0x61, 0xAF, 0xEF);
    let cyan = Color::Rgb(0x56, 0xB6, 0xC2);
    let green = Color::Rgb(0x98, 0xC3, 0x79);
    let yellow = Color::Rgb(0xE5, 0xC0, 0x7B);
    let red = Color::Rgb(0xE0, 0x6C, 0x75);
    let purple = Color::Rgb(0xC6, 0x78, 0xDD);
    let fg = Color::Rgb(0xAB, 0xB2, 0xBF);
    let fg_dim = Color::Rgb(0x9D, 0xA5, 0xB4);
    let comment = Color::Rgb(0x5C, 0x63, 0x70);
    let gutter = Color::Rgb(0x3E, 0x44, 0x51);
    let bg = Color::Rgb(0x28, 0x2C, 0x34);
    let bg_light = Color::Rgb(0x2C, 0x31, 0x3A);
    let bg_dark = Color::Rgb(0x21, 0x25, 0x2B);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: comment,
        border_unfocused: gutter,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: gutter,
        selection_fg: fg,
        modal_dim_bg: bg_dark,
        modal_bg: bg_light,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
}

fn rose_pine_moon_palette() -> ThemePalette {
    let iris = Color::Rgb(0xC4, 0xA7, 0xE7); // purple
    let rose = Color::Rgb(0xEA, 0x9A, 0x97);
    let foam = Color::Rgb(0x9C, 0xCF, 0xD8); // teal/cyan
    let gold = Color::Rgb(0xF6, 0xC1, 0x77); // yellow
    let love = Color::Rgb(0xEB, 0x6F, 0x92); // red/pink
    let pine = Color::Rgb(0x3E, 0x8F, 0xB0); // blue
    let text = Color::Rgb(0xE0, 0xDE, 0xF4);
    let subtle = Color::Rgb(0x90, 0x8C, 0xAA);
    let muted = Color::Rgb(0x6E, 0x6A, 0x86);
    let highlight_med = Color::Rgb(0x44, 0x41, 0x5A);
    let surface = Color::Rgb(0x2A, 0x27, 0x3F);
    let base = Color::Rgb(0x23, 0x21, 0x36);
    PaletteSlots {
        accent: iris,
        accent_bright: rose,
        green: foam,
        yellow: gold,
        red: love,
        blue: pine,
        text_primary: text,
        text_secondary: subtle,
        text_muted: muted,
        border_unfocused: highlight_med,
        role_name: iris,
        branch_name: foam,
        search_bar: pine,
        keybind_hint: gold,
        selection_bg: highlight_med,
        selection_fg: text,
        modal_dim_bg: base,
        modal_bg: surface,
        modal_border: iris,
        base_bg: base,
    }
    .build()
}

fn everforest_palette() -> ThemePalette {
    let green = Color::Rgb(0xA7, 0xC0, 0x80);
    let aqua = Color::Rgb(0x83, 0xC0, 0x92);
    let yellow = Color::Rgb(0xDB, 0xBC, 0x7F);
    let red = Color::Rgb(0xE6, 0x7E, 0x80);
    let blue = Color::Rgb(0x7F, 0xBB, 0xB3);
    let purple = Color::Rgb(0xD6, 0x99, 0xB6);
    let fg = Color::Rgb(0xD3, 0xC6, 0xAA);
    let grey1 = Color::Rgb(0x9D, 0xA9, 0xA0);
    let grey0 = Color::Rgb(0x7A, 0x84, 0x78);
    let bg2 = Color::Rgb(0x3D, 0x48, 0x4D);
    let bg1 = Color::Rgb(0x34, 0x3F, 0x44);
    let bg0 = Color::Rgb(0x2D, 0x35, 0x3B);
    let bg_dim = Color::Rgb(0x23, 0x2A, 0x2E);
    PaletteSlots {
        accent: green,
        accent_bright: aqua,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: grey1,
        text_muted: grey0,
        border_unfocused: bg2,
        role_name: purple,
        branch_name: aqua,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: bg2,
        selection_fg: fg,
        modal_dim_bg: bg_dim,
        modal_bg: bg1,
        modal_border: green,
        base_bg: bg0,
    }
    .build()
}

fn kanagawa_palette() -> ThemePalette {
    let crystal_blue = Color::Rgb(0x7E, 0x9C, 0xD8);
    let spring_blue = Color::Rgb(0x7F, 0xB4, 0xCA); // cyan
    let spring_green = Color::Rgb(0x98, 0xBB, 0x6C);
    let carp_yellow = Color::Rgb(0xE6, 0xC3, 0x84);
    let wave_red = Color::Rgb(0xE4, 0x68, 0x76);
    let oni_violet = Color::Rgb(0x95, 0x7F, 0xB8);
    let fuji_white = Color::Rgb(0xDC, 0xD7, 0xBA);
    let old_white = Color::Rgb(0xC8, 0xC0, 0x93);
    let fuji_gray = Color::Rgb(0x72, 0x71, 0x69);
    let sumi_ink3 = Color::Rgb(0x36, 0x36, 0x46);
    let wave_blue2 = Color::Rgb(0x2D, 0x4F, 0x67);
    let sumi_ink2 = Color::Rgb(0x2A, 0x2A, 0x37);
    let sumi_ink1 = Color::Rgb(0x1F, 0x1F, 0x28);
    let sumi_ink0 = Color::Rgb(0x16, 0x16, 0x1D);
    PaletteSlots {
        accent: crystal_blue,
        accent_bright: spring_blue,
        green: spring_green,
        yellow: carp_yellow,
        red: wave_red,
        blue: crystal_blue,
        text_primary: fuji_white,
        text_secondary: old_white,
        text_muted: fuji_gray,
        border_unfocused: sumi_ink3,
        role_name: oni_violet,
        branch_name: spring_green,
        search_bar: spring_blue,
        keybind_hint: carp_yellow,
        selection_bg: wave_blue2,
        selection_fg: fuji_white,
        modal_dim_bg: sumi_ink0,
        modal_bg: sumi_ink2,
        modal_border: crystal_blue,
        base_bg: sumi_ink1,
    }
    .build()
}

fn solarized_dark_palette() -> ThemePalette {
    let yellow = Color::Rgb(0xB5, 0x89, 0x00);
    let orange = Color::Rgb(0xCB, 0x4B, 0x16);
    let red = Color::Rgb(0xDC, 0x32, 0x2F);
    let magenta = Color::Rgb(0xD3, 0x36, 0x82);
    let violet = Color::Rgb(0x6C, 0x71, 0xC4);
    let blue = Color::Rgb(0x26, 0x8B, 0xD2);
    let cyan = Color::Rgb(0x2A, 0xA1, 0x98);
    let green = Color::Rgb(0x85, 0x99, 0x00);
    let base03 = Color::Rgb(0x00, 0x2B, 0x36);
    let base02 = Color::Rgb(0x07, 0x36, 0x42);
    let base01 = Color::Rgb(0x58, 0x6E, 0x75);
    let base0 = Color::Rgb(0x83, 0x94, 0x96);
    let base1 = Color::Rgb(0x93, 0xA1, 0xA1);
    PaletteSlots {
        // Cyan accent keeps Solarized Dark distinct from Solarized Light,
        // which takes the blue.
        accent: cyan,
        accent_bright: blue,
        green,
        yellow,
        red,
        blue,
        text_primary: base1,
        text_secondary: base0,
        text_muted: base01,
        border_unfocused: base02,
        role_name: magenta,
        branch_name: green,
        search_bar: violet,
        keybind_hint: orange,
        selection_bg: base02,
        selection_fg: base1,
        modal_dim_bg: base03,
        modal_bg: base02,
        modal_border: cyan,
        base_bg: base03,
    }
    .build()
}

fn monokai_palette() -> ThemePalette {
    let pink = Color::Rgb(0xF9, 0x26, 0x72);
    let green = Color::Rgb(0xA6, 0xE2, 0x2E);
    let yellow = Color::Rgb(0xE6, 0xDB, 0x74);
    let orange = Color::Rgb(0xFD, 0x97, 0x1F);
    let purple = Color::Rgb(0xAE, 0x81, 0xFF);
    let cyan = Color::Rgb(0x66, 0xD9, 0xEF);
    let fg = Color::Rgb(0xF8, 0xF8, 0xF2);
    let fg_dim = Color::Rgb(0xCF, 0xCF, 0xC2);
    let comment = Color::Rgb(0x75, 0x71, 0x5E);
    let bg_light = Color::Rgb(0x3E, 0x3D, 0x32);
    let bg = Color::Rgb(0x27, 0x28, 0x22);
    let bg_dark = Color::Rgb(0x1D, 0x1E, 0x19);
    PaletteSlots {
        accent: pink,
        accent_bright: orange,
        green,
        yellow,
        red: pink,
        blue: cyan,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: comment,
        border_unfocused: bg_light,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: bg_light,
        selection_fg: fg,
        modal_dim_bg: bg_dark,
        modal_bg: bg_light,
        modal_border: pink,
        base_bg: bg,
    }
    .build()
}

fn ayu_dark_palette() -> ThemePalette {
    let orange = Color::Rgb(0xFF, 0xB4, 0x54);
    let amber = Color::Rgb(0xE6, 0xB4, 0x50);
    let green = Color::Rgb(0xAA, 0xD9, 0x4C);
    let red = Color::Rgb(0xF0, 0x71, 0x78);
    let blue = Color::Rgb(0x59, 0xC2, 0xFF);
    let purple = Color::Rgb(0xD2, 0xA6, 0xFF);
    let fg = Color::Rgb(0xBF, 0xBD, 0xB6);
    let fg_dim = Color::Rgb(0x9B, 0x99, 0x93);
    let muted = Color::Rgb(0x5C, 0x61, 0x66);
    let panel = Color::Rgb(0x1F, 0x24, 0x30);
    let line = Color::Rgb(0x25, 0x2B, 0x38);
    let bg = Color::Rgb(0x0B, 0x0E, 0x14);
    PaletteSlots {
        accent: orange,
        accent_bright: amber,
        green,
        yellow: amber,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: amber,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: panel,
        modal_border: orange,
        base_bg: bg,
    }
    .build()
}

fn ayu_mirage_palette() -> ThemePalette {
    let orange = Color::Rgb(0xFF, 0xCC, 0x66);
    let amber = Color::Rgb(0xFF, 0xD1, 0x73);
    let green = Color::Rgb(0xD5, 0xFF, 0x80);
    let red = Color::Rgb(0xF2, 0x87, 0x79);
    let blue = Color::Rgb(0x73, 0xD0, 0xFF);
    let purple = Color::Rgb(0xDF, 0xBF, 0xFF);
    let fg = Color::Rgb(0xCC, 0xCA, 0xC2);
    let fg_dim = Color::Rgb(0xA6, 0xAC, 0xB9);
    let muted = Color::Rgb(0x70, 0x76, 0x84);
    let panel = Color::Rgb(0x1F, 0x24, 0x30);
    let line = Color::Rgb(0x2D, 0x34, 0x40);
    let bg = Color::Rgb(0x24, 0x29, 0x36);
    PaletteSlots {
        accent: amber,
        accent_bright: green,
        green,
        yellow: orange,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: orange,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: panel,
        modal_bg: line,
        modal_border: amber,
        base_bg: bg,
    }
    .build()
}

fn material_palette() -> ThemePalette {
    let cyan = Color::Rgb(0x89, 0xDD, 0xFF);
    let teal = Color::Rgb(0x80, 0xCB, 0xC4);
    let green = Color::Rgb(0xC3, 0xE8, 0x8D);
    let yellow = Color::Rgb(0xFF, 0xCB, 0x6B);
    let red = Color::Rgb(0xF0, 0x71, 0x78);
    let blue = Color::Rgb(0x82, 0xAA, 0xFF);
    let purple = Color::Rgb(0xC7, 0x92, 0xEA);
    let fg = Color::Rgb(0xEE, 0xFF, 0xFF);
    let fg_dim = Color::Rgb(0xB2, 0xCC, 0xD6);
    let comment = Color::Rgb(0x54, 0x6E, 0x7A);
    let line = Color::Rgb(0x31, 0x36, 0x3B);
    let surface = Color::Rgb(0x26, 0x2A, 0x2E);
    let bg = Color::Rgb(0x21, 0x25, 0x29);
    PaletteSlots {
        accent: teal,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: comment,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: teal,
        base_bg: bg,
    }
    .build()
}

fn rose_pine_palette() -> ThemePalette {
    let iris = Color::Rgb(0xC4, 0xA7, 0xE7);
    let rose = Color::Rgb(0xEB, 0xBC, 0xBA);
    let foam = Color::Rgb(0x9C, 0xCF, 0xD8);
    let gold = Color::Rgb(0xF6, 0xC1, 0x77);
    let love = Color::Rgb(0xEB, 0x6F, 0x92);
    let pine = Color::Rgb(0x31, 0x74, 0x8F);
    let text = Color::Rgb(0xE0, 0xDE, 0xF4);
    let subtle = Color::Rgb(0x90, 0x8C, 0xAA);
    let muted = Color::Rgb(0x6E, 0x6A, 0x86);
    let highlight_med = Color::Rgb(0x40, 0x3D, 0x52);
    let surface = Color::Rgb(0x1F, 0x1D, 0x2E);
    let base = Color::Rgb(0x19, 0x17, 0x24);
    PaletteSlots {
        // The Moon variant already takes iris; use rose here so the two
        // Rosé Pine flavours read differently in the picker swatch.
        accent: rose,
        accent_bright: iris,
        green: foam,
        yellow: gold,
        red: love,
        blue: pine,
        text_primary: text,
        text_secondary: subtle,
        text_muted: muted,
        border_unfocused: highlight_med,
        role_name: iris,
        branch_name: foam,
        search_bar: pine,
        keybind_hint: gold,
        selection_bg: highlight_med,
        selection_fg: text,
        modal_dim_bg: base,
        modal_bg: surface,
        modal_border: rose,
        base_bg: base,
    }
    .build()
}

fn oxocarbon_palette() -> ThemePalette {
    let cyan = Color::Rgb(0x3D, 0xDB, 0xD9);
    let blue = Color::Rgb(0x78, 0xA9, 0xFF);
    let purple = Color::Rgb(0xBE, 0x95, 0xFF);
    let magenta = Color::Rgb(0xFF, 0x7E, 0xB6);
    let green = Color::Rgb(0x42, 0xBE, 0x65);
    let yellow = Color::Rgb(0xFF, 0xE9, 0x7C);
    let red = Color::Rgb(0xEE, 0x53, 0x96);
    let fg = Color::Rgb(0xF2, 0xF4, 0xF8);
    let fg_dim = Color::Rgb(0xC6, 0xC6, 0xC6);
    let muted = Color::Rgb(0x6F, 0x6F, 0x6F);
    let gray80 = Color::Rgb(0x39, 0x39, 0x39);
    let gray90 = Color::Rgb(0x26, 0x26, 0x26);
    let gray100 = Color::Rgb(0x16, 0x16, 0x16);
    PaletteSlots {
        accent: cyan,
        accent_bright: magenta,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: gray80,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: gray80,
        selection_fg: fg,
        modal_dim_bg: gray100,
        modal_bg: gray90,
        modal_border: cyan,
        base_bg: gray100,
    }
    .build()
}

fn github_dark_palette() -> ThemePalette {
    let blue = Color::Rgb(0x58, 0xA6, 0xFF);
    let cyan = Color::Rgb(0x76, 0xE3, 0xEA);
    let green = Color::Rgb(0x3F, 0xB9, 0x50);
    let yellow = Color::Rgb(0xD2, 0x99, 0x22);
    let red = Color::Rgb(0xF8, 0x51, 0x49);
    let purple = Color::Rgb(0xBC, 0x8C, 0xFF);
    let fg = Color::Rgb(0xC9, 0xD1, 0xD9);
    let fg_dim = Color::Rgb(0xB1, 0xBA, 0xC4);
    let muted = Color::Rgb(0x6E, 0x76, 0x81);
    let border = Color::Rgb(0x30, 0x36, 0x3D);
    let canvas_subtle = Color::Rgb(0x16, 0x1B, 0x22);
    let canvas = Color::Rgb(0x0D, 0x11, 0x17);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: border,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: border,
        selection_fg: fg,
        modal_dim_bg: canvas,
        modal_bg: canvas_subtle,
        modal_border: blue,
        base_bg: canvas,
    }
    .build()
}

fn nightfox_palette() -> ThemePalette {
    let blue = Color::Rgb(0x71, 0x9C, 0xD6);
    let cyan = Color::Rgb(0x63, 0xCD, 0xCF);
    let green = Color::Rgb(0x81, 0xB2, 0x9A);
    let yellow = Color::Rgb(0xDB, 0xC0, 0x74);
    let red = Color::Rgb(0xC9, 0x4F, 0x6D);
    let magenta = Color::Rgb(0x9D, 0x79, 0xD6);
    let fg = Color::Rgb(0xCD, 0xCE, 0xCF);
    let fg_dim = Color::Rgb(0xAE, 0xAF, 0xB0);
    let muted = Color::Rgb(0x73, 0x82, 0x91);
    let sel = Color::Rgb(0x2B, 0x3B, 0x51);
    let bg1 = Color::Rgb(0x19, 0x24, 0x30);
    let bg0 = Color::Rgb(0x13, 0x1A, 0x24);
    PaletteSlots {
        accent: cyan,
        accent_bright: blue,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: sel,
        role_name: magenta,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: sel,
        selection_fg: fg,
        modal_dim_bg: bg0,
        modal_bg: bg1,
        modal_border: cyan,
        base_bg: bg0,
    }
    .build()
}

fn sonokai_palette() -> ThemePalette {
    let red = Color::Rgb(0xFC, 0x5D, 0x7C);
    let orange = Color::Rgb(0xF3, 0x96, 0x60);
    let yellow = Color::Rgb(0xE7, 0xC6, 0x64);
    let green = Color::Rgb(0x9E, 0xD0, 0x72);
    let blue = Color::Rgb(0x76, 0xCC, 0xE0);
    let purple = Color::Rgb(0xB3, 0x9D, 0xF3);
    let fg = Color::Rgb(0xE2, 0xE2, 0xE3);
    let fg_dim = Color::Rgb(0xC1, 0xC1, 0xC3);
    let grey = Color::Rgb(0x7F, 0x84, 0x90);
    let bg4 = Color::Rgb(0x3B, 0x3E, 0x48);
    let bg2 = Color::Rgb(0x33, 0x35, 0x3F);
    let bg0 = Color::Rgb(0x2C, 0x2E, 0x34);
    PaletteSlots {
        accent: orange,
        accent_bright: yellow,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: grey,
        border_unfocused: bg4,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: bg4,
        selection_fg: fg,
        modal_dim_bg: bg0,
        modal_bg: bg2,
        modal_border: orange,
        base_bg: bg0,
    }
    .build()
}

fn melange_palette() -> ThemePalette {
    let tan = Color::Rgb(0xEB, 0xC0, 0x6D);
    let peach = Color::Rgb(0xD4, 0x77, 0x66);
    let green = Color::Rgb(0x85, 0xB6, 0x95);
    let yellow = Color::Rgb(0xEB, 0xC0, 0x6D);
    let red = Color::Rgb(0xD4, 0x73, 0x66);
    let blue = Color::Rgb(0xA3, 0xA9, 0xCE);
    let purple = Color::Rgb(0xCF, 0x9B, 0xC2);
    let fg = Color::Rgb(0xEC, 0xE1, 0xD7);
    let fg_dim = Color::Rgb(0xC1, 0xB2, 0xA4);
    let muted = Color::Rgb(0x86, 0x78, 0x6D);
    let sel = Color::Rgb(0x40, 0x36, 0x30);
    let surface = Color::Rgb(0x33, 0x2C, 0x28);
    let bg = Color::Rgb(0x29, 0x24, 0x22);
    PaletteSlots {
        accent: tan,
        accent_bright: peach,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: sel,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: peach,
        selection_bg: sel,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: tan,
        base_bg: bg,
    }
    .build()
}

fn zenburn_palette() -> ThemePalette {
    let sand = Color::Rgb(0xF0, 0xDF, 0xAF);
    let orange = Color::Rgb(0xDF, 0xAF, 0x8F);
    let green = Color::Rgb(0x7F, 0x9F, 0x7F);
    let yellow = Color::Rgb(0xE3, 0xCE, 0xAB);
    let red = Color::Rgb(0xCC, 0x93, 0x93);
    let blue = Color::Rgb(0x8C, 0xD0, 0xD3);
    let purple = Color::Rgb(0xDC, 0x8C, 0xC3);
    let fg = Color::Rgb(0xDC, 0xDC, 0xCC);
    let fg_dim = Color::Rgb(0xC0, 0xC0, 0xB0);
    let muted = Color::Rgb(0x70, 0x80, 0x70);
    let bg_plus2 = Color::Rgb(0x4F, 0x4F, 0x4F);
    let bg_plus1 = Color::Rgb(0x40, 0x40, 0x40);
    let bg = Color::Rgb(0x3F, 0x3F, 0x3F);
    let bg_minus = Color::Rgb(0x2B, 0x2B, 0x2B);
    PaletteSlots {
        accent: sand,
        accent_bright: orange,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: bg_plus2,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: bg_plus2,
        selection_fg: fg,
        modal_dim_bg: bg_minus,
        modal_bg: bg_plus1,
        modal_border: sand,
        base_bg: bg,
    }
    .build()
}

fn iceberg_palette() -> ThemePalette {
    let blue = Color::Rgb(0x84, 0xA0, 0xC6);
    let cyan = Color::Rgb(0x89, 0xB8, 0xC2);
    let green = Color::Rgb(0xB4, 0xBE, 0x82);
    let yellow = Color::Rgb(0xE2, 0xA4, 0x78);
    let red = Color::Rgb(0xE2, 0x78, 0x78);
    let purple = Color::Rgb(0xA0, 0x93, 0xC7);
    let fg = Color::Rgb(0xC6, 0xC8, 0xD1);
    let fg_dim = Color::Rgb(0xAD, 0xB1, 0xC4);
    let muted = Color::Rgb(0x6B, 0x70, 0x89);
    let line = Color::Rgb(0x3E, 0x44, 0x51);
    let surface = Color::Rgb(0x1E, 0x21, 0x32);
    let bg = Color::Rgb(0x16, 0x18, 0x21);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
}

fn vesper_palette() -> ThemePalette {
    let amber = Color::Rgb(0xFF, 0xC7, 0x7B);
    let peach = Color::Rgb(0xFF, 0xA8, 0x5C);
    let green = Color::Rgb(0x99, 0xFF, 0xE4);
    let yellow = Color::Rgb(0xFF, 0xCF, 0xA8);
    let red = Color::Rgb(0xFF, 0x80, 0x80);
    let blue = Color::Rgb(0xA0, 0xA0, 0xA0);
    let fg = Color::Rgb(0xFF, 0xFF, 0xFF);
    let fg_dim = Color::Rgb(0xA0, 0xA0, 0xA0);
    let muted = Color::Rgb(0x50, 0x50, 0x50);
    let line = Color::Rgb(0x2A, 0x2A, 0x2A);
    let surface = Color::Rgb(0x18, 0x18, 0x18);
    let bg = Color::Rgb(0x10, 0x10, 0x10);
    PaletteSlots {
        accent: amber,
        accent_bright: peach,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: peach,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: amber,
        base_bg: bg,
    }
    .build()
}

fn synthwave_palette() -> ThemePalette {
    let magenta = Color::Rgb(0xFF, 0x7E, 0xDB);
    let cyan = Color::Rgb(0x36, 0xF9, 0xF6);
    let green = Color::Rgb(0x72, 0xF1, 0xB8);
    let yellow = Color::Rgb(0xFE, 0xDE, 0x5D);
    let red = Color::Rgb(0xFE, 0x44, 0x50);
    let orange = Color::Rgb(0xFF, 0x8B, 0x39);
    let fg = Color::Rgb(0xF9, 0xF9, 0xF9);
    let fg_dim = Color::Rgb(0xD3, 0xC5, 0xE8);
    let muted = Color::Rgb(0x84, 0x8B, 0xBD);
    let line = Color::Rgb(0x34, 0x29, 0x4F);
    let surface = Color::Rgb(0x26, 0x1E, 0x3C);
    let bg = Color::Rgb(0x26, 0x22, 0x35);
    PaletteSlots {
        accent: magenta,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue: cyan,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: orange,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: surface,
        modal_bg: line,
        modal_border: magenta,
        base_bg: bg,
    }
    .build()
}

fn nightfly_palette() -> ThemePalette {
    let blue = Color::Rgb(0x82, 0xAA, 0xFF);
    let cyan = Color::Rgb(0x7F, 0xDB, 0xCA);
    let green = Color::Rgb(0xA1, 0xCD, 0x5E);
    let yellow = Color::Rgb(0xE3, 0xD1, 0x8A);
    let red = Color::Rgb(0xFC, 0x51, 0x4E);
    let purple = Color::Rgb(0xAE, 0x81, 0xFF);
    let fg = Color::Rgb(0xBD, 0xC1, 0xC6);
    let fg_dim = Color::Rgb(0xA1, 0xAA, 0xB8);
    let muted = Color::Rgb(0x63, 0x6D, 0x83);
    let line = Color::Rgb(0x2F, 0x3B, 0x54);
    let surface = Color::Rgb(0x1D, 0x30, 0x43);
    let bg = Color::Rgb(0x01, 0x16, 0x27);
    PaletteSlots {
        accent: cyan,
        accent_bright: blue,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: cyan,
        base_bg: bg,
    }
    .build()
}

fn tomorrow_night_palette() -> ThemePalette {
    let blue = Color::Rgb(0x81, 0xA2, 0xBE);
    let aqua = Color::Rgb(0x8A, 0xBE, 0xB7);
    let green = Color::Rgb(0xB5, 0xBD, 0x68);
    let yellow = Color::Rgb(0xF0, 0xC6, 0x74);
    let red = Color::Rgb(0xCC, 0x66, 0x66);
    let purple = Color::Rgb(0xB2, 0x94, 0xBB);
    let orange = Color::Rgb(0xDE, 0x93, 0x5F);
    let fg = Color::Rgb(0xC5, 0xC8, 0xC6);
    let fg_dim = Color::Rgb(0xB4, 0xB7, 0xB4);
    let comment = Color::Rgb(0x96, 0x98, 0x96);
    let line = Color::Rgb(0x37, 0x3B, 0x41);
    let surface = Color::Rgb(0x28, 0x2A, 0x2E);
    let bg = Color::Rgb(0x1D, 0x1F, 0x21);
    PaletteSlots {
        accent: blue,
        accent_bright: aqua,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: comment,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: aqua,
        keybind_hint: orange,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: bg,
        modal_bg: surface,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
}

fn ayu_light_palette() -> ThemePalette {
    let orange = Color::Rgb(0xFA, 0x8D, 0x3E);
    let amber = Color::Rgb(0xE6, 0xBA, 0x7E);
    let green = Color::Rgb(0x6C, 0xA3, 0x00);
    let yellow = Color::Rgb(0xA3, 0x71, 0x00);
    let red = Color::Rgb(0xF0, 0x71, 0x71);
    let blue = Color::Rgb(0x39, 0x9E, 0xE6);
    let purple = Color::Rgb(0xA3, 0x7A, 0xCC);
    let fg = Color::Rgb(0x5C, 0x61, 0x66);
    let fg_dim = Color::Rgb(0x78, 0x7B, 0x80);
    let muted = Color::Rgb(0x8A, 0x91, 0x99);
    let line = Color::Rgb(0xE7, 0xE8, 0xE9);
    let panel = Color::Rgb(0xF3, 0xF4, 0xF5);
    let bg = Color::Rgb(0xFC, 0xFC, 0xFC);
    PaletteSlots {
        accent: orange,
        accent_bright: amber,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: blue,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: panel,
        modal_bg: bg,
        modal_border: orange,
        base_bg: bg,
    }
    .build()
}

fn one_light_palette() -> ThemePalette {
    let blue = Color::Rgb(0x40, 0x78, 0xF2);
    let cyan = Color::Rgb(0x01, 0x84, 0xBC);
    let green = Color::Rgb(0x50, 0xA1, 0x4F);
    let yellow = Color::Rgb(0xC1, 0x84, 0x01);
    let red = Color::Rgb(0xE4, 0x56, 0x49);
    let purple = Color::Rgb(0xA6, 0x26, 0xA4);
    let fg = Color::Rgb(0x38, 0x3A, 0x42);
    let fg_dim = Color::Rgb(0x50, 0x53, 0x5C);
    let muted = Color::Rgb(0x9D, 0xA5, 0xB4);
    let line = Color::Rgb(0xE5, 0xE5, 0xE6);
    let panel = Color::Rgb(0xEA, 0xEA, 0xEB);
    let bg = Color::Rgb(0xFA, 0xFA, 0xFA);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: line,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: line,
        selection_fg: fg,
        modal_dim_bg: panel,
        modal_bg: bg,
        modal_border: blue,
        base_bg: bg,
    }
    .build()
}

fn rose_pine_dawn_palette() -> ThemePalette {
    let iris = Color::Rgb(0x90, 0x7A, 0xA9);
    let rose = Color::Rgb(0xD7, 0x82, 0x7E);
    let foam = Color::Rgb(0x56, 0x94, 0x9F);
    let gold = Color::Rgb(0xEA, 0x9D, 0x34);
    let love = Color::Rgb(0xB4, 0x63, 0x7A);
    let pine = Color::Rgb(0x28, 0x69, 0x83);
    let text = Color::Rgb(0x57, 0x52, 0x79);
    let subtle = Color::Rgb(0x79, 0x75, 0x93);
    let muted = Color::Rgb(0x98, 0x93, 0xA5);
    let highlight_med = Color::Rgb(0xDF, 0xDA, 0xD9);
    let surface = Color::Rgb(0xFF, 0xFA, 0xF3);
    let base = Color::Rgb(0xFA, 0xF4, 0xED);
    PaletteSlots {
        accent: iris,
        accent_bright: rose,
        green: foam,
        yellow: gold,
        red: love,
        blue: pine,
        text_primary: text,
        text_secondary: subtle,
        text_muted: muted,
        border_unfocused: highlight_med,
        role_name: iris,
        branch_name: foam,
        search_bar: pine,
        keybind_hint: gold,
        selection_bg: highlight_med,
        selection_fg: text,
        modal_dim_bg: highlight_med,
        modal_bg: surface,
        modal_border: iris,
        base_bg: base,
    }
    .build()
}

fn github_light_palette() -> ThemePalette {
    let blue = Color::Rgb(0x09, 0x69, 0xDA);
    let cyan = Color::Rgb(0x1B, 0x7C, 0x83);
    let green = Color::Rgb(0x1A, 0x7F, 0x37);
    let yellow = Color::Rgb(0x95, 0x6C, 0x05);
    let red = Color::Rgb(0xCF, 0x22, 0x2E);
    let purple = Color::Rgb(0x82, 0x50, 0xDF);
    let fg = Color::Rgb(0x1F, 0x23, 0x28);
    let fg_dim = Color::Rgb(0x42, 0x4A, 0x53);
    let muted = Color::Rgb(0x6E, 0x77, 0x81);
    let border = Color::Rgb(0xD0, 0xD7, 0xDE);
    let canvas_subtle = Color::Rgb(0xF6, 0xF8, 0xFA);
    let canvas = Color::Rgb(0xFF, 0xFF, 0xFF);
    PaletteSlots {
        accent: blue,
        accent_bright: cyan,
        green,
        yellow,
        red,
        blue,
        text_primary: fg,
        text_secondary: fg_dim,
        text_muted: muted,
        border_unfocused: border,
        role_name: purple,
        branch_name: green,
        search_bar: cyan,
        keybind_hint: yellow,
        selection_bg: border,
        selection_fg: fg,
        modal_dim_bg: canvas_subtle,
        modal_bg: canvas,
        modal_border: blue,
        base_bg: canvas,
    }
    .build()
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
