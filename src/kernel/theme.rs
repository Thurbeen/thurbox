//! The active colour scheme, resolved once and published as named roles.
//!
//! Design.md D14: resolution is a *contract* — every plugin expresses colour by
//! naming a role, so something must turn roles into colours before any pane can
//! draw. Choosing a theme, by contrast, is presentation, and stays a plugin.
//!
//! Nothing here is reimplemented. `session::theme_config` is pure data and
//! already reachable from the kernel: it carries all 36 presets and a 31-role
//! palette, `agent::themes_config` reads the user's `themes.toml`, and the
//! choice lives in `metadata.active_theme` exactly where v1 put it. So a v1
//! user's themes and current selection carry over untouched.

use std::collections::BTreeMap;

use ratatui::style::{Modifier, Style};

use crate::session::theme_config::{ThemeEntry, ThemePalette, ThemePreset};
use crate::storage::Database;

/// One selectable theme, flattened for publishing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeChoice {
    /// Stable identifier, as persisted. Unchanged from v1.
    pub name: String,
    pub display_name: String,
    pub is_light: bool,
    /// True for a theme defined in the user's `themes.toml`.
    pub is_custom: bool,
}

/// The resolved theme plus everything a picker needs.
pub struct Themes {
    active: ThemeEntry,
    choices: Vec<ThemeChoice>,
    /// The user's `themes.toml` entries, kept so a palette other than the
    /// active one can be resolved without re-reading the file — the picker's
    /// swatch previews the *highlighted* theme, which is a per-keystroke read.
    custom: Vec<ThemeEntry>,
    /// Problems found while loading user themes. Surfaced, never fatal.
    pub warnings: Vec<String>,
}

impl Themes {
    /// Resolve from the presets, the user's `themes.toml` and the persisted
    /// choice.
    ///
    /// Every failure degrades rather than aborts: an unparseable user theme is
    /// skipped, and a recorded choice that no longer resolves falls back to the
    /// default. Losing the interface over a colour would be absurd.
    pub fn load(db: Option<&Database>) -> Self {
        let (custom, warnings) = crate::agent::themes_config::load_or_seed_with_warnings();

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

        let wanted = db.and_then(|db| db.get_active_theme().ok().flatten());
        let active = resolve(wanted.as_deref(), &custom);

        Self {
            active,
            choices,
            custom,
            warnings,
        }
    }

    /// Resolve any offered theme to its full entry, palette included.
    ///
    /// v1's picker reads `entry.palette` to draw the swatch for the row under
    /// the cursor (`ui::theme_picker_modal::render_preview`), so previewing
    /// requires a palette the user has *not* selected.
    pub fn entry(&self, name: &str) -> Option<ThemeEntry> {
        self.choices
            .iter()
            .any(|choice| choice.name == name)
            .then(|| resolve(Some(name), &self.custom))
    }

    /// The active theme's identifier.
    pub fn active_name(&self) -> &str {
        &self.active.name
    }

    pub fn active(&self) -> &ThemeEntry {
        &self.active
    }

    pub fn choices(&self) -> &[ThemeChoice] {
        &self.choices
    }

    /// Make `name` active, persist it, and report whether it resolved.
    ///
    /// Persisting an unknown name is refused rather than silently ignored: the
    /// alternative is a stored choice that never applies and cannot be
    /// diagnosed.
    pub fn select(&mut self, name: &str, db: Option<&Database>) -> Result<(), String> {
        if !self.choices.iter().any(|choice| choice.name == name) {
            return Err(format!("unknown theme {name:?}"));
        }
        self.active = resolve(Some(name), &self.custom);
        if let Some(db) = db {
            db.set_active_theme(name)
                .map_err(|e| format!("persist theme: {e}"))?;
        }
        Ok(())
    }

    /// Make `name` active **without persisting it** — the live preview a picker
    /// shows while you move through the list.
    ///
    /// Separate from [`Self::select`] because persistence is the difference
    /// between trying a palette and choosing one: a preview that wrote to the
    /// database would leak every theme the cursor passed over into every other
    /// thurbox process, and leave the last one you glanced at active after you
    /// pressed `Esc`.
    pub fn preview(&mut self, name: &str) -> Result<(), String> {
        if !self.choices.iter().any(|choice| choice.name == name) {
            return Err(format!("unknown theme {name:?}"));
        }
        self.active = resolve(Some(name), &self.custom);
        Ok(())
    }

    /// Reload from disk and the database, keeping the active choice if it still
    /// resolves. Called when `themes.toml` changes or another instance switches.
    pub fn refresh(&mut self, db: Option<&Database>) {
        *self = Self::load(db);
    }

    /// The active palette as role name → colour string, ready to publish.
    ///
    /// Colours are emitted as `#rrggbb` (or a palette index) so Lua can hand
    /// them straight back through the node vocabulary without the kernel
    /// needing a second colour type on the boundary.
    pub fn roles(&self) -> BTreeMap<&'static str, String> {
        palette_roles(&self.active.palette)
    }

    /// How a text selection is painted: the palette's own selection pair.
    ///
    /// v1's `apply_selection_highlight` uses exactly these two roles. Reverse
    /// video is the fallback rather than the rule because it inverts whatever
    /// each cell already had — so a selection over styled text came out a
    /// different colour per span and matched no theme.
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

/// Resolve a name against the presets and user themes, falling back sensibly.
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

/// Every role in a palette, named as a plugin names it.
///
/// Listed explicitly rather than derived: the field set is a published contract,
/// so adding a role should be a visible edit here, and a *removed* field becomes
/// a compile error instead of a silently missing role.
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

/// Render a ratatui colour in the form [`super::node::parse_color`] accepts.
///
/// `Reset` is emitted as `"reset"` rather than dropped: a theme setting a role
/// to the terminal's own colour is *defining* it, and a plugin asking for that
/// role should get an answer.
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
        let (custom, _) = crate::agent::themes_config::load_or_seed_with_warnings();
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
        // 31 colours; nerd_font_enabled is a flag, not a role.
        let palette = ThemePreset::Default.palette();
        let roles = palette_roles(&palette);
        assert_eq!(
            roles.len(),
            31,
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
        // The round trip that matters: whatever we publish, a plugin must be
        // able to hand straight back as a style.
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
        let error = themes.select("nonsense", None).unwrap_err();
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
        themes.select(&other, None).expect("select");
        assert_eq!(themes.active_name(), other);
        assert_ne!(before, themes.roles(), "the palette should have changed");
    }

    #[test]
    fn the_choice_round_trips_through_the_database() {
        let db = Database::open_in_memory().expect("in-memory database");
        let mut themes = Themes::load(Some(&db));
        let other = themes
            .choices()
            .iter()
            .find(|c| c.name != themes.active_name())
            .expect("more than one theme")
            .name
            .clone();
        themes.select(&other, Some(&db)).expect("select");

        // A fresh load — as a restart or another instance would do — sees it.
        let reloaded = Themes::load(Some(&db));
        assert_eq!(reloaded.active_name(), other);
    }
}
