use ratatui::style::{Color, Modifier, Style};

/// Centralized color and style constants for the Thurbox UI.
///
/// All widget files reference these constants instead of hard-coding colors,
/// enabling consistent theming and single-line visual changes.
pub struct Theme;

impl Theme {
    // ── Accent ──────────────────────────────────────────────────────────────

    /// Primary accent color used for focused borders, selected items, branding.
    pub const ACCENT: Color = Color::Cyan;

    // ── Status colors ───────────────────────────────────────────────────────

    pub const STATUS_BUSY: Color = Color::Green;
    pub const STATUS_WAITING: Color = Color::Yellow;
    pub const STATUS_IDLE: Color = Color::DarkGray;
    pub const STATUS_ERROR: Color = Color::Red;

    // ── Text hierarchy ──────────────────────────────────────────────────────

    pub const TEXT_PRIMARY: Color = Color::White;
    pub const TEXT_SECONDARY: Color = Color::Gray;
    pub const TEXT_MUTED: Color = Color::DarkGray;

    // ── Borders ─────────────────────────────────────────────────────────────

    pub const BORDER_FOCUSED: Color = Color::Cyan;
    pub const BORDER_UNFOCUSED: Color = Color::Gray;

    // ── Domain-specific colors ──────────────────────────────────────────────

    pub const ROLE_NAME: Color = Color::Magenta;
    pub const ADMIN_BADGE: Color = Color::Yellow;
    pub const BRANCH_NAME: Color = Color::Green;

    // ── Keybind hints / tool permissions ─────────────────────────────────────

    pub const KEYBIND_HINT: Color = Color::Yellow;
    pub const TOOL_ALLOWED: Color = Color::Green;
    pub const TOOL_DISALLOWED: Color = Color::Red;

    // ── Admin ─────────────────────────────────────────────────────────────

    pub const ADMIN_BORDER: Color = Color::Yellow;

    // ── Danger / destructive ────────────────────────────────────────────────

    pub const DANGER: Color = Color::Red;

    // ── Background colors ───────────────────────────────────────────────────

    pub const INVERTED_FG: Color = Color::Black;

    // ── Composite styles ────────────────────────────────────────────────────

    /// Style for a focused panel/modal title: bold black on accent background.
    pub fn focused_title() -> Style {
        Style::default()
            .fg(Self::INVERTED_FG)
            .bg(Self::ACCENT)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for an unfocused panel title: dimmed secondary text.
    pub fn unfocused_title() -> Style {
        Style::default().fg(Self::BORDER_UNFOCUSED)
    }

    /// Style for section headers (e.g. info panel sections, help overlay).
    pub fn section_header() -> Style {
        Style::default()
            .fg(Self::ACCENT)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for labels in info panels and status displays.
    pub fn label() -> Style {
        Style::default().fg(Self::TEXT_MUTED)
    }

    /// Style for keybind hint keys in modal footers.
    pub fn keybind() -> Style {
        Style::default().fg(Self::KEYBIND_HINT)
    }

    /// Style for keybind descriptions in modal footers.
    pub fn keybind_desc() -> Style {
        Style::default().fg(Self::TEXT_MUTED)
    }

    /// Style for selected/active list items.
    pub fn selected_item() -> Style {
        Style::default()
            .fg(Self::ACCENT)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for normal (unselected) list items.
    pub fn normal_item() -> Style {
        Style::default().fg(Self::TEXT_PRIMARY)
    }

    /// Style for admin section title: yellow bold.
    pub fn admin_title() -> Style {
        Style::default()
            .fg(Self::ADMIN_BORDER)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for project metadata lines (repo info, role count).
    pub fn project_meta() -> Style {
        Style::default().fg(Self::TEXT_MUTED)
    }

    /// Style for the block cursor in text fields.
    pub fn cursor() -> Style {
        Style::default()
            .fg(Self::INVERTED_FG)
            .bg(Self::TEXT_PRIMARY)
    }
}
