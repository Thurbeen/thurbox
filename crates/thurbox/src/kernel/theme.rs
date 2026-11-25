use std::collections::BTreeMap;

use ratatui::style::{Modifier, Style};

use crate::theme::{ThemeEntry, ThemePalette, ThemePreset};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeChoice {
    pub name: String,
    pub display_name: String,
    pub is_light: bool,
    pub is_custom: bool,
}

pub struct Themes {
    active: ThemeEntry,
    choices: Vec<ThemeChoice>,
    custom: Vec<ThemeEntry>,
    pub warnings: Vec<String>,
    version: u64,
    chosen: Option<String>,
}

impl Themes {
    pub fn load(stored: Option<&str>) -> Self {
        let (custom, warnings) = crate::config::themes::load_or_seed_with_warnings();

        let mut choices: Vec<ThemeChoice> = ThemePreset::all()
            .iter()
            .map(|preset| {
                let entry = ThemeEntry::from_preset(*preset);
                ThemeChoice {
                    name: entry.name,
                    display_name: entry.display_name,
                    is_light: entry.is_light,
                    is_custom: false,
                }
            })
            .collect();
        // User themes come after the built-ins, as v1's picker orders them.
        for entry in &custom {
            choices.push(ThemeChoice {
                name: entry.name.clone(),
                display_name: entry.display_name.clone(),
                is_light: entry.is_light,
                is_custom: true,
            });
        }

        let active = resolve(stored, &custom);

        Self {
            active,
            choices,
            custom,
            warnings,
            version: 0,
            chosen: None,
        }
    }

    pub fn entry(&self, name: &str) -> Option<ThemeEntry> {
        self.choices
            .iter()
            .any(|choice| choice.name == name)
            .then(|| resolve(Some(name), &self.custom))
    }

    pub fn active_name(&self) -> &str {
        &self.active.name
    }

    pub fn active(&self) -> &ThemeEntry {
        &self.active
    }

    pub fn choices(&self) -> &[ThemeChoice] {
        &self.choices
    }

    pub fn select(&mut self, name: &str) -> Result<(), String> {
        if !self.choices.iter().any(|choice| choice.name == name) {
            return Err(format!("unknown theme {name:?}"));
        }
        self.active = resolve(Some(name), &self.custom);
        self.mark_changed();
        self.chosen = Some(name.to_string());
        Ok(())
    }

    pub fn take_chosen(&mut self) -> Option<String> {
        self.chosen.take()
    }

    pub fn preview(&mut self, name: &str) -> Result<(), String> {
        if !self.choices.iter().any(|choice| choice.name == name) {
            return Err(format!("unknown theme {name:?}"));
        }
        self.active = resolve(Some(name), &self.custom);
        self.mark_changed();
        Ok(())
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn mark_changed(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    pub fn roles(&self) -> BTreeMap<&'static str, String> {
        palette_roles(&self.active.palette)
    }

    pub fn selection_style(&self) -> Style {
        let roles = self.roles();
        let colour = |role: &str| {
            roles
                .get(role)
                .and_then(|raw| super::node::parse_color(raw))
        };
        let mut style = Style::default();
        if let Some(bg) = colour("selection_bg") {
            style = style.bg(bg);
        }
        if let Some(fg) = colour("selection_fg") {
            style = style.fg(fg);
        }
        // A palette defining neither still has to show a selection.
        if style == Style::default() {
            style = style.add_modifier(Modifier::REVERSED);
        }
        style
    }
}

fn resolve(wanted: Option<&str>, custom: &[ThemeEntry]) -> ThemeEntry {
    let Some(name) = wanted else {
        return ThemeEntry::from_preset(ThemePreset::Default);
    };
    // A user theme may shadow a preset name; v1 lets the custom one win, so a
    // user who overrode "default" keeps their override.
    if let Some(entry) = custom.iter().find(|entry| entry.name == name) {
        return entry.clone();
    }
    match ThemePreset::from_str(name) {
        Some(preset) => ThemeEntry::from_preset(preset),
        None => ThemeEntry::from_preset(ThemePreset::Default),
    }
}

pub fn palette_roles(p: &ThemePalette) -> BTreeMap<&'static str, String> {
    let ThemePalette {
        accent,
        accent_bright,
        status_working,
        status_blocked,
        status_done,
        status_idle,
        status_error,
        status_unreachable,
        status_running,
        status_unknown,
        status_background,
        text_primary,
        text_secondary,
        text_muted,
        border_focused,
        border_unfocused,
        role_name,
        branch_name,
        search_bar,
        keybind_hint,
        tool_allowed,
        tool_disallowed,
        danger,
        selection_bg,
        selection_fg,
        modal_dim_bg,
        modal_bg,
        modal_border,
        inverted_fg,
        diff_added,
        diff_removed,
        diff_added_bg,
        diff_removed_bg,
        app_bg,
        nerd_font_enabled: _,
    } = p;

    let mut roles = BTreeMap::new();
    for (name, color) in [
        ("accent", accent),
        ("accent_bright", accent_bright),
        ("status_working", status_working),
        ("status_blocked", status_blocked),
        ("status_done", status_done),
        ("status_idle", status_idle),
        ("status_error", status_error),
        ("status_unreachable", status_unreachable),
        ("status_running", status_running),
        ("status_unknown", status_unknown),
        ("status_background", status_background),
        ("text_primary", text_primary),
        ("text_secondary", text_secondary),
        ("text_muted", text_muted),
        ("border_focused", border_focused),
        ("border_unfocused", border_unfocused),
        ("role_name", role_name),
        ("branch_name", branch_name),
        ("search_bar", search_bar),
        ("keybind_hint", keybind_hint),
        ("tool_allowed", tool_allowed),
        ("tool_disallowed", tool_disallowed),
        ("danger", danger),
        ("selection_bg", selection_bg),
        ("selection_fg", selection_fg),
        ("modal_dim_bg", modal_dim_bg),
        ("modal_bg", modal_bg),
        ("modal_border", modal_border),
        ("inverted_fg", inverted_fg),
        ("diff_added", diff_added),
        ("diff_removed", diff_removed),
        ("diff_added_bg", diff_added_bg),
        ("diff_removed_bg", diff_removed_bg),
        ("app_bg", app_bg),
    ] {
        roles.insert(name, color_to_string(color));
    }
    roles
}

pub fn color_to_string(color: &ratatui::style::Color) -> String {
    use ratatui::style::Color;
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(n) => n.to_string(),
        Color::Black => "black".into(),
        Color::Red => "red".into(),
        Color::Green => "green".into(),
        Color::Yellow => "yellow".into(),
        Color::Blue => "blue".into(),
        Color::Magenta => "magenta".into(),
        Color::Cyan => "cyan".into(),
        Color::Gray => "gray".into(),
        Color::DarkGray => "dark-gray".into(),
        Color::LightRed => "light-red".into(),
        Color::LightGreen => "light-green".into(),
        Color::LightYellow => "light-yellow".into(),
        Color::LightBlue => "light-blue".into(),
        Color::LightMagenta => "light-magenta".into(),
        Color::LightCyan => "light-cyan".into(),
        Color::White => "light-white".into(),
        Color::Reset => "reset".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_v1_preset_is_selectable() {
        let themes = Themes::load(None);
        assert_eq!(
            themes.choices().iter().filter(|c| !c.is_custom).count(),
            ThemePreset::all().len(),
            "every built-in theme must still be offered"
        );
        assert!(themes.choices().len() >= 36, "v1 shipped 36 presets");
    }

    #[test]
    fn preset_identifiers_still_resolve() {
        // A persisted v1 choice must stay valid, or every user loses their theme
        // on upgrade.
        let (custom, _) = crate::config::themes::load_or_seed_with_warnings();
        for preset in ThemePreset::all() {
            let name = ThemeEntry::from_preset(*preset).name;
            let resolved = resolve(Some(&name), &custom);
            assert_eq!(resolved.name, name, "{name} should resolve to itself");
        }
    }

    #[test]
    fn an_unknown_choice_falls_back_to_the_default() {
        let resolved = resolve(Some("no-such-theme"), &[]);
        assert_eq!(
            resolved.name,
            ThemeEntry::from_preset(ThemePreset::Default).name
        );
    }

    #[test]
    fn no_recorded_choice_is_the_default() {
        let resolved = resolve(None, &[]);
        assert_eq!(
            resolved.name,
            ThemeEntry::from_preset(ThemePreset::Default).name
        );
    }

    #[test]
    fn a_user_theme_shadows_a_preset_of_the_same_name() {
        // v1 lets a custom theme win, so someone who overrode "default" keeps
        // their override.
        let mut mine = ThemeEntry::from_preset(ThemePreset::Default);
        mine.display_name = "Mine".to_string();
        let resolved = resolve(Some(&mine.name.clone()), std::slice::from_ref(&mine));
        assert_eq!(resolved.display_name, "Mine");
    }

    #[test]
    fn the_role_table_covers_every_palette_field() {
        // 34 colours; nerd_font_enabled is a flag, not a role.
        let palette = ThemePreset::Default.palette();
        let roles = palette_roles(&palette);
        assert_eq!(
            roles.len(),
            34,
            "got {:?}",
            roles.keys().collect::<Vec<_>>()
        );
        for required in [
            "accent",
            "text_muted",
            "border_focused",
            "status_working",
            "status_blocked",
            "status_done",
            "status_idle",
            "danger",
        ] {
            assert!(roles.contains_key(required), "missing role {required}");
        }
    }

    #[test]
    fn published_colours_parse_back_through_the_node_vocabulary() {
        for preset in ThemePreset::all() {
            let roles = palette_roles(&preset.palette());
            for (role, text) in roles {
                assert!(
                    super::super::node::parse_color(&text).is_some(),
                    "{role} = {text:?} does not parse back"
                );
            }
        }
    }

    #[test]
    fn light_and_dark_are_both_offered() {
        let themes = Themes::load(None);
        assert!(themes.choices().iter().any(|c| c.is_light));
        assert!(themes.choices().iter().any(|c| !c.is_light));
    }

    #[test]
    fn selecting_an_unknown_theme_is_refused() {
        let mut themes = Themes::load(None);
        let before = themes.active_name().to_string();
        let error = themes.select("nonsense").unwrap_err();
        assert!(error.contains("unknown theme"), "{error}");
        assert_eq!(
            themes.active_name(),
            before,
            "the active theme must not move"
        );
    }

    #[test]
    fn selecting_a_known_theme_changes_the_palette() {
        let mut themes = Themes::load(None);
        let other = themes
            .choices()
            .iter()
            .find(|c| c.name != themes.active_name())
            .expect("more than one theme")
            .name
            .clone();
        let before = themes.roles();
        themes.select(&other).expect("select");
        assert_eq!(themes.active_name(), other);
        assert_ne!(before, themes.roles(), "the palette should have changed");
    }

    #[test]
    fn the_choice_is_handed_out_once_and_a_stored_one_is_loaded() {
        let mut themes = Themes::load(None);
        let other = themes
            .choices()
            .iter()
            .find(|c| c.name != themes.active_name())
            .expect("more than one theme")
            .name
            .clone();
        themes.select(&other).expect("select");
        let chosen = themes
            .take_chosen()
            .expect("the choice is left to be stored");
        assert_eq!(themes.take_chosen(), None, "handed out once");

        // A fresh load — as a restart or another instance would do — sees it.
        let reloaded = Themes::load(Some(&chosen));
        assert_eq!(reloaded.active_name(), other);
    }
}
