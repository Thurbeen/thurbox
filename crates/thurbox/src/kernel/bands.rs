use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::registry::{Pill, Registry, QUIT_CHORD};
use super::theme::Themes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Error,
}

impl Level {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "info" => Some(Level::Info),
            "success" => Some(Level::Success),
            "error" => Some(Level::Error),
            _ => None,
        }
    }

    fn badge(self) -> &'static str {
        match self {
            Level::Info => " INFO ",
            Level::Success => " ✓ SYNC ",
            Level::Error => " ERROR ",
        }
    }

    fn role(self) -> &'static str {
        match self {
            Level::Info => "accent",
            Level::Success => "tool_allowed",
            Level::Error => "status_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Band {
    Identity,
    Message,
    Action,
}

impl Band {
    pub fn from_slot(slot: &str) -> Option<Self> {
        match slot {
            "header" => Some(Band::Identity),
            "status" => Some(Band::Message),
            "footer" => Some(Band::Action),
            _ => None,
        }
    }

    pub fn slot(self) -> &'static str {
        match self {
            Band::Identity => "header",
            Band::Message => "status",
            Band::Action => "footer",
        }
    }

    pub const DROP_ORDER: [Band; 3] = [Band::Identity, Band::Action, Band::Message];
}

pub struct BandState<'a> {
    pub version: &'a str,
    pub theme_label: &'a str,
    pub update_available: Option<&'a str>,
    pub session: Option<&'a str>,
    pub session_count: usize,
    pub focus_label: &'a str,
    pub message: Option<(&'a str, Level)>,
    pub progress: Option<&'a str>,
    pub hovered: Option<&'a super::node::Identity>,
    pub registry: &'a Registry,
    pub themes: &'a Themes,
}

impl BandState<'_> {
    fn colour(&self, role: &str) -> Option<ratatui::style::Color> {
        self.themes
            .roles()
            .get(role)
            .and_then(|raw| super::node::parse_color(raw))
    }

    fn style(&self, role: &str) -> Style {
        match self.colour(role) {
            Some(colour) => Style::default().fg(colour),
            None => Style::default(),
        }
    }
}

pub fn occupies(band: Band, state: &BandState<'_>) -> bool {
    match band {
        Band::Message => state.message.is_some() || state.progress.is_some(),
        _ => true,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub rect: Rect,
    pub identity: super::node::Identity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    pub chord: Option<String>,
    pub action: String,
    pub priority: i64,
}

impl Entry {
    pub fn display(&self) -> String {
        match &self.chord {
            Some(chord) => format!("{} · {}", self.label, chord),
            None => self.label.clone(),
        }
    }
}

pub fn entries(pills: &[Pill], registry: &Registry) -> Vec<Entry> {
    let mut out: Vec<Entry> = pills
        .iter()
        .filter_map(|pill| {
            let chord = chord_for(&pill.action, registry)?;
            Some(Entry {
                label: pill.label.clone(),
                chord: Some(chord).filter(|chord| !chord.is_empty()),
                action: pill.action.clone(),
                priority: pill.priority,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.label.cmp(&b.label))
    });
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    PaletteOnly,
    Unknown,
}

impl DropReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::PaletteOnly => "palette-only",
            DropReason::Unknown => "unknown",
        }
    }

    pub fn explain(self, action: &str) -> String {
        match self {
            DropReason::PaletteOnly => format!(
                "{action:?} is a command with no key, and the band draws only what a \
                 chord resolves for — give it one: \
                 `keys = {{ {{ key = \"…\", action = {action:?} }} }}`"
            ),
            DropReason::Unknown => format!(
                "nothing loaded declares {action:?} — no key, no command — so either it is \
                 misspelt or the plugin that declares it is turned off"
            ),
        }
    }
}

pub fn dropped<'a>(pills: &'a [Pill], registry: &Registry) -> Vec<(&'a Pill, DropReason)> {
    pills
        .iter()
        .filter(|pill| chord_for(&pill.action, registry).is_none())
        .map(|pill| {
            let declared = registry
                .commands()
                .iter()
                .any(|command| command.action == pill.action);
            let reason = if declared {
                DropReason::PaletteOnly
            } else {
                DropReason::Unknown
            };
            (pill, reason)
        })
        .collect()
}

fn chord_for(action: &str, registry: &Registry) -> Option<String> {
    let mut first = None;
    for binding in registry.bindings() {
        if binding.action != action {
            continue;
        }
        if is_function_key(&binding.chord) {
            return Some(compact_chord(&binding.chord));
        }
        first.get_or_insert_with(|| compact_chord(&binding.chord));
    }
    first
}

fn is_function_key(chord: &str) -> bool {
    chord
        .strip_prefix('f')
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn compact_chord(chord: &str) -> String {
    let mut modifiers = String::new();
    let mut key = chord;
    while let Some((prefix, rest)) = key.split_once('+') {
        let symbol = match prefix {
            "ctrl" => "^",
            "shift" => "⇧",
            "alt" => "⌥",
            "cmd" => "⌘",
            _ => break,
        };
        modifiers.push_str(symbol);
        key = rest;
    }
    let mut chars = key.chars();
    let key = match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    };
    format!("{modifiers}{key}")
}

const ENTRY_PADDING: usize = 2;
const ENTRY_GAP: usize = 1;

pub fn entries_width(entries: &[Entry]) -> usize {
    entries
        .iter()
        .map(|entry| entry.display().chars().count() + ENTRY_PADDING)
        .sum::<usize>()
        + entries.len().saturating_sub(1) * ENTRY_GAP
}

pub fn fit(mut entries: Vec<Entry>, width: usize) -> Vec<Entry> {
    while !entries.is_empty() && entries_width(&entries) > width {
        entries.pop();
    }
    entries
}

pub const QUIT_ACTION: &str = "core.quit";

pub fn quit_entry() -> Entry {
    Entry {
        label: "Quit".to_string(),
        chord: Some(compact_chord(QUIT_CHORD)),
        action: QUIT_ACTION.to_string(),
        priority: i64::MIN,
    }
}

fn left_spans(state: &BandState<'_>) -> Vec<Span<'static>> {
    let badge = state
        .style("inverted_fg")
        .bg(state
            .colour("accent")
            .unwrap_or(ratatui::style::Color::Reset))
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(
        format!(" {} ", capitalise(state.focus_label)),
        badge,
    )];

    spans.push(Span::styled(
        format!(" {} session(s) ", state.session_count),
        state.style("text_secondary"),
    ));

    let key = state.style("keybind_hint").add_modifier(Modifier::BOLD);
    let desc = state.style("text_muted");
    spans.extend([
        Span::styled(" ^H", key),
        Span::styled("/", desc),
        Span::styled("^L", key),
        Span::styled(" Focus ", desc),
        Span::styled("^O", key),
        Span::styled(" Open ", desc),
    ]);
    spans
}

fn fit_to_budget(mut spans: Vec<Span<'static>>, budget: usize) -> Vec<Span<'static>> {
    if budget == 0 {
        return Vec::new();
    }
    let width = |spans: &[Span<'static>]| -> usize {
        spans.iter().map(|span| span.content.chars().count()).sum()
    };
    while spans.len() > 1 && width(&spans) > budget {
        spans.pop();
    }
    if width(&spans) > budget {
        if let Some(first) = spans.first().cloned() {
            return vec![Span::styled(truncate(&first.content, budget), first.style)];
        }
    }
    spans
}

pub fn focus_label(surface: Option<&str>, mod_name: &str) -> String {
    let view = surface
        .and_then(|surface| surface.split_once('#'))
        .map(|(_, view)| view)
        .filter(|view| !view.is_empty());
    capitalise(view.unwrap_or(mod_name))
}

fn capitalise(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub fn render(frame: &mut Frame, area: Rect, band: Band, state: &BandState<'_>) -> Vec<Hit> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    match band {
        Band::Identity => {
            render_identity(frame, area, state);
            Vec::new()
        }
        Band::Message => {
            render_message(frame, area, state);
            Vec::new()
        }
        Band::Action => render_action(frame, area, state),
    }
}

fn entry_role(action: &str) -> String {
    format!("action:{action}")
}

struct PreparedEntry {
    label: String,
    width: u16,
    role: String,
}

struct ActionCache {
    registry: *const Registry,
    registry_version: u64,
    width: u16,
    entries: Vec<PreparedEntry>,
}

thread_local! {
    static ACTION_CACHE: std::cell::RefCell<Option<ActionCache>> =
        const { std::cell::RefCell::new(None) };
}

fn prepare_action(registry: &Registry, width: u16) -> Vec<PreparedEntry> {
    let all = entries(registry.pills(), registry);
    let budget = usize::from(width).saturating_sub(2);
    let quit = quit_entry();
    let reserved = entries_width(std::slice::from_ref(&quit)) + ENTRY_GAP;
    let mut fitted = fit(all, budget.saturating_sub(reserved));
    fitted.push(quit);
    fitted
        .into_iter()
        .map(|entry| {
            let label = format!(" {} ", entry.display());
            PreparedEntry {
                width: label.chars().count() as u16,
                role: entry_role(&entry.action),
                label,
            }
        })
        .collect()
}

fn render_identity(frame: &mut Frame, area: Rect, state: &BandState<'_>) {
    let mut spans = vec![
        Span::styled(
            " thurbox",
            state.style("accent").add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  Multi-Session Agent Orchestrator",
            state.style("text_secondary"),
        ),
        Span::styled(format!("  v{}", state.version), state.style("text_muted")),
    ];
    if let Some(latest) = state.update_available {
        spans.push(Span::styled(
            format!("  ⬆ v{latest} available"),
            state.style("accent").add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    let mut right: Vec<Span<'_>> = Vec::new();
    if let Some(session) = state.session {
        right.push(Span::styled(
            session.to_string(),
            state.style("text_primary"),
        ));
        right.push(Span::raw("  "));
    }
    right.push(Span::styled(
        format!("◐ {} ", state.theme_label),
        state.style("accent"),
    ));
    frame.render_widget(
        Paragraph::new(Line::from(right).alignment(ratatui::layout::Alignment::Right)),
        area,
    );
}

fn render_message(frame: &mut Frame, area: Rect, state: &BandState<'_>) {
    let spans = if let Some(progress) = state.progress {
        vec![
            Span::styled(" ⋯ ", state.style("status_working")),
            Span::styled(progress.to_string(), state.style("text_secondary")),
        ]
    } else if let Some((text, level)) = state.message {
        let badge = state
            .style(level.role())
            .add_modifier(Modifier::REVERSED)
            .add_modifier(Modifier::BOLD);
        vec![
            Span::styled(level.badge(), badge),
            Span::raw(" "),
            Span::styled(text.to_string(), state.style(level.role())),
        ]
    } else {
        return;
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_action(frame: &mut Frame, area: Rect, state: &BandState<'_>) -> Vec<Hit> {
    ACTION_CACHE.with_borrow_mut(|cache| {
        let current = (
            state.registry as *const Registry,
            state.registry.version(),
            area.width,
        );
        if cache
            .as_ref()
            .map(|c| (c.registry, c.registry_version, c.width))
            != Some(current)
        {
            *cache = Some(ActionCache {
                registry: current.0,
                registry_version: current.1,
                width: current.2,
                entries: prepare_action(state.registry, area.width),
            });
        }
        let prepared = &cache.as_ref().expect("just filled").entries;
        paint_action(frame, area, state, prepared)
    })
}

fn paint_action(
    frame: &mut Frame,
    area: Rect,
    state: &BandState<'_>,
    prepared: &[PreparedEntry],
) -> Vec<Hit> {
    let block = prepared
        .iter()
        .map(|entry| usize::from(entry.width))
        .sum::<usize>()
        + prepared.len().saturating_sub(1) * ENTRY_GAP;
    let left_width = usize::from(area.width).saturating_sub(block);

    let left = fit_to_budget(left_spans(state), left_width);
    if !left.is_empty() {
        frame.render_widget(Paragraph::new(Line::from(left)), area);
    }

    let start = area.x + area.width.saturating_sub(block as u16);
    let mut cursor = start;
    let mut hits = Vec::new();
    for (index, entry) in prepared.iter().enumerate() {
        if index > 0 {
            cursor = cursor.saturating_add(ENTRY_GAP as u16);
        }
        let width = entry.width;
        let rect = Rect {
            x: cursor,
            y: area.y,
            width: width.min(area.right().saturating_sub(cursor)),
            height: 1,
        };
        let identity = super::node::Identity {
            id: None,
            classes: Vec::new(),
            role: Some(entry.role.clone()),
        };
        let hovered = state
            .hovered
            .and_then(|identity| identity.role.as_deref())
            .is_some_and(|role| role == entry.role);
        let style = if hovered {
            state
                .style("inverted_fg")
                .bg(state
                    .colour("accent_bright")
                    .unwrap_or(ratatui::style::Color::Reset))
                .add_modifier(Modifier::BOLD)
        } else {
            state
                .style("selection_fg")
                .bg(state
                    .colour("selection_bg")
                    .unwrap_or(ratatui::style::Color::Reset))
                .add_modifier(Modifier::BOLD)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(entry.label.as_str(), style))),
            rect,
        );
        hits.push(Hit { rect, identity });
        cursor = cursor.saturating_add(width);
    }
    hits
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return String::new();
    }
    let kept: String = text.chars().take(width - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::registry::{Binding, CommandDecl, Scope};

    fn binding(action: &str, chord: &str) -> Binding {
        Binding {
            owner: "p".into(),
            action: action.into(),
            default_chord: chord.into(),
            chord: chord.into(),
            overridden: false,
            description: String::new(),
            scope: Scope::Global,
            passthrough: false,
            release: false,
            group: String::new(),
        }
    }

    fn pill(action: &str, label: &str, priority: i64) -> Pill {
        Pill {
            owner: "p".into(),
            action: action.into(),
            label: label.into(),
            priority,
        }
    }

    fn registry_with(bindings: Vec<Binding>, pills: Vec<Pill>) -> Registry {
        let mut registry = Registry::default();
        registry.declare_all(bindings, Vec::new(), pills);
        registry
    }

    #[test]
    fn an_entry_shows_the_chord_in_force_not_the_default() {
        let mut registry = registry_with(
            vec![binding("help.open", "f1")],
            vec![pill("help.open", "Help", 10)],
        );
        assert_eq!(
            entries(registry.pills(), &registry)[0].display(),
            "Help · F1"
        );

        registry
            .rebind("help.open", Some("ctrl+shift+h"))
            .expect("rebind");
        let resolved = entries(registry.pills(), &registry);
        assert_eq!(
            resolved[0].display(),
            "Help · ^⇧H",
            "the entry must show what the user actually presses"
        );
    }

    #[test]
    fn a_chord_is_spelled_the_way_the_rest_of_the_interface_spells_it() {
        let registry = registry_with(
            vec![binding("shell.open", "ctrl+t")],
            vec![pill("shell.open", "Shell", 10)],
        );
        assert_eq!(
            entries(registry.pills(), &registry)[0].display(),
            "Shell · ^T"
        );
    }

    #[test]
    fn an_f_key_alternate_is_advertised_over_a_ctrl_letter() {
        let registry = registry_with(
            vec![binding("shell.open", "ctrl+t"), binding("shell.open", "f8")],
            vec![pill("shell.open", "Shell", 10)],
        );
        assert_eq!(
            entries(registry.pills(), &registry)[0].display(),
            "Shell · F8"
        );
    }

    #[test]
    fn an_entry_naming_an_unknown_action_is_dropped() {
        let registry = registry_with(
            vec![binding("help.open", "f1")],
            vec![pill("help.open", "Help", 10), pill("gone.open", "Gone", 9)],
        );

        let resolved = entries(registry.pills(), &registry);
        assert_eq!(resolved.len(), 1, "{resolved:?}");
        assert_eq!(resolved[0].label, "Help");
    }

    #[test]
    fn a_dropped_pill_tells_a_palette_only_action_from_one_nobody_declared() {
        let mut registry = registry_with(
            vec![binding("help.open", "f1")],
            vec![
                pill("help.open", "Help", 10),
                pill("notes.open", "Notes", 9),
                pill("notes.opne", "Memory", 8),
            ],
        );
        registry.declare_commands(vec![CommandDecl {
            owner: "p".into(),
            action: "notes.open".into(),
            description: "open the notes".into(),
        }]);

        let resolved = entries(registry.pills(), &registry);
        assert_eq!(resolved.len(), 1, "the drop stays: {resolved:?}");

        let reported = dropped(registry.pills(), &registry);
        let reasons: Vec<(&str, DropReason)> = reported
            .iter()
            .map(|(pill, reason)| (pill.label.as_str(), *reason))
            .collect();
        assert_eq!(
            reasons,
            [
                ("Notes", DropReason::PaletteOnly),
                ("Memory", DropReason::Unknown),
            ]
        );
        assert!(
            reported[0].1.explain("notes.open").contains("notes.open"),
            "the explanation names the action it is about: {:?}",
            reported[0].1.explain("notes.open")
        );
    }

    #[test]
    fn a_command_the_user_bound_a_chord_to_is_no_longer_a_dropped_pill() {
        let mut registry = registry_with(Vec::new(), vec![pill("notes.open", "Notes", 9)]);
        registry.declare_commands(vec![CommandDecl {
            owner: "p".into(),
            action: "notes.open".into(),
            description: "open the notes".into(),
        }]);
        assert_eq!(dropped(registry.pills(), &registry).len(), 1);

        registry.rebind("notes.open", Some("f9")).expect("rebind");
        assert!(dropped(registry.pills(), &registry).is_empty());
        assert_eq!(
            entries(registry.pills(), &registry)[0].display(),
            "Notes · F9"
        );
    }

    #[test]
    fn entries_are_ordered_by_priority_and_shed_from_the_bottom() {
        let registry = registry_with(
            vec![
                binding("a.open", "f1"),
                binding("b.open", "f2"),
                binding("c.open", "f3"),
            ],
            vec![
                pill("c.open", "Ccc", 1),
                pill("a.open", "Aaa", 10),
                pill("b.open", "Bbb", 5),
            ],
        );

        let resolved = entries(registry.pills(), &registry);
        let order: Vec<&str> = resolved.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(order, ["Aaa", "Bbb", "Ccc"], "highest priority first");

        let two = entries_width(&resolved[..2]);
        let fitted = fit(resolved.clone(), two);
        let kept: Vec<&str> = fitted.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(kept, ["Aaa", "Bbb"], "the lowest priority sheds first");

        assert!(fit(resolved, 3).is_empty());
    }

    #[test]
    fn quit_is_advertised_with_the_reserved_chord_and_renders_last() {
        let quit = quit_entry();
        assert_eq!(quit.display(), "Quit · ^Q");
        assert_eq!(quit.action, QUIT_ACTION);

        let registry = registry_with(
            vec![binding("help.open", "f1")],
            vec![pill("help.open", "Help", 0)],
        );
        let declared = entries(registry.pills(), &registry);
        assert!(
            quit.priority < declared[0].priority,
            "quit must sort behind a declared entry, even an unprioritised one"
        );
    }

    #[test]
    fn only_the_message_band_can_be_empty() {
        let themes = Themes::load(None);
        let registry = Registry::default();
        let mut state = BandState {
            version: "1.2.3",
            theme_label: "Default",
            update_available: None,
            session: None,
            session_count: 0,
            focus_label: "Terminal",
            message: None,
            progress: None,
            hovered: None,
            registry: &registry,
            themes: &themes,
        };

        assert!(occupies(Band::Identity, &state));
        assert!(occupies(Band::Action, &state));
        assert!(
            !occupies(Band::Message, &state),
            "an empty message band must not take a row"
        );

        state.message = Some(("saved", Level::Info));
        assert!(occupies(Band::Message, &state));
    }

    #[test]
    fn the_badge_names_the_view_being_shown_not_just_the_pane() {
        assert_eq!(
            focus_label(Some("abc-123#shell"), "agent"),
            "Shell",
            "the shell view names itself"
        );
        assert_eq!(
            focus_label(Some("abc-123"), "agent"),
            "Agent",
            "the plain surface falls back to the pane"
        );
        assert_eq!(
            focus_label(None, "sessions"),
            "Sessions",
            "a pane with no session surface is named by the pane"
        );
        assert_eq!(focus_label(Some("abc-123#logs"), "agent"), "Logs");
        assert_eq!(focus_label(Some("abc-123#"), "agent"), "Agent");
    }

    #[test]
    fn a_slot_maps_to_its_band_and_back() {
        for band in Band::DROP_ORDER {
            assert_eq!(Band::from_slot(band.slot()), Some(band));
        }
        assert_eq!(Band::from_slot("center"), None, "a pane slot is not a band");
        assert_eq!(Band::from_slot("sessions"), None);
    }

    #[test]
    fn the_message_band_is_surrendered_last() {
        assert_eq!(Band::DROP_ORDER.last(), Some(&Band::Message));
        assert_eq!(Band::DROP_ORDER.first(), Some(&Band::Identity));
    }
}
