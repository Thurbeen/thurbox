//! The chrome bands: what the application says about itself.
//!
//! Three single-row surfaces — who is running (`identity`), what just happened
//! (`message`), and what can be pressed (`action`). They are the non-modal half
//! of the rule `kernel::modals` states for the modal half: **system chrome is
//! kernel-owned, and plugins contribute data to it**
//! (`openspec/changes/v2-system-modals/design.md` D1).
//!
//! Two properties follow from that, and both are the point:
//!
//! * **Drawing a band runs no Lua.** Contributions are collected once, at load
//!   and reload, and every value that changes while running is read from state
//!   the kernel already holds. A plugin that throws, hangs or fails to load
//!   costs its own pane and its own entries — never the chrome.
//! * **The arrangement decides placement, the kernel decides contents.** A band
//!   is named as a slot in `ui/layout.lua` exactly as a pane's region is, so the
//!   bars stay movable in the file whose whole purpose is deciding what the
//!   screen looks like. A band the arrangement omits simply does not draw.
//!
//! See `openspec/changes/v2-chrome-bands/` for the specs and the decisions.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::registry::{Pill, Registry, QUIT_CHORD};
use super::theme::Themes;

/// How severe a message is. v1's `StatusLevel`, and the same three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Error,
}

impl Level {
    /// The badge v1 prints in front of the text.
    fn badge(self) -> &'static str {
        match self {
            Level::Info => " INFO ",
            Level::Success => " ✓ SYNC ",
            Level::Error => " ERROR ",
        }
    }

    /// The theme role the badge and the text take.
    fn role(self) -> &'static str {
        match self {
            Level::Info => "accent",
            Level::Success => "tool_allowed",
            Level::Error => "status_error",
        }
    }
}

/// Which band a slot names.
///
/// A slot the arrangement placed that is not one of these belongs to plugins,
/// and is not this module's business.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    /// The slot name this band is placed under.
    pub fn slot(self) -> &'static str {
        match self {
            Band::Identity => "header",
            Band::Message => "status",
            Band::Action => "footer",
        }
    }

    /// Order bands are surrendered in when the screen is too short.
    ///
    /// Identity first: it is the least urgent row on screen, and the pane area
    /// is what the user is working in. The message band goes last, because a
    /// hidden error is the failure this chrome exists to prevent. v1 drops its
    /// header below 20 rows for the same reason.
    pub const DROP_ORDER: [Band; 3] = [Band::Identity, Band::Action, Band::Message];
}

/// Everything the bands draw from, gathered before any of them paints.
///
/// Borrowed rather than owned so assembling it costs nothing: the caller
/// already holds every field.
pub struct BandState<'a> {
    pub version: &'a str,
    pub theme_label: &'a str,
    /// The newer release, when one is known to exist.
    pub update_available: Option<&'a str>,
    /// The selected session's name, when there is one.
    pub session: Option<&'a str>,
    pub session_count: usize,
    pub automation_count: usize,
    /// The focused surface's name, for the action band's left cluster.
    pub focus_label: &'a str,
    /// What just happened, and how severe. `None` leaves the message band with
    /// nothing to say, which is what keeps it from occupying a row.
    pub message: Option<(&'a str, Level)>,
    /// Work in flight, described. Outlives a message deliberately: creation
    /// phases routinely run past the retention window, so they are progress
    /// rather than a toast.
    pub progress: Option<&'a str>,
    /// The identity under the pointer, so an entry can light. Resolved by the
    /// loop from the hitboxes this module returned on the previous frame — the
    /// same list clicks are routed through, so a lit entry and a pressed one can
    /// never be different entries.
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

/// Whether this band has anything to occupy a row with.
///
/// Only the message band can be empty: the other two always have something to
/// report. An arrangement can therefore place all three unconditionally and the
/// message row costs nothing while there is nothing to say.
pub fn occupies(band: Band, state: &BandState<'_>) -> bool {
    match band {
        Band::Message => state.message.is_some() || state.progress.is_some(),
        _ => true,
    }
}

/// Where an entry was drawn, and what pressing it means.
///
/// Bands are painted by the kernel rather than walked as a node tree, so they do
/// not pass through the hit recording the panes get. They return their hitboxes
/// instead, and the loop routes clicks and hover through them — which is what
/// makes an entry a button rather than a picture of one.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub rect: Rect,
    pub identity: super::node::Identity,
}

/// An entry resolved for drawing: its label, and the chord in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    /// The chord actually bound, which is what the user must press. `None` when
    /// the action is reachable but unbound. The Quit entry's is *reserved* rather
    /// than bound — see [`quit_entry`], the one entry no binding backs.
    pub chord: Option<String>,
    pub action: String,
    pub priority: i64,
}

impl Entry {
    /// `Help · F1`, or just `Help` when nothing is bound to it.
    pub fn display(&self) -> String {
        match &self.chord {
            Some(chord) => format!("{} · {}", self.label, chord),
            None => self.label.clone(),
        }
    }
}

/// Resolve declared pills into drawable entries, highest priority first.
///
/// An entry naming an action the registry does not know is **dropped**. A chip
/// that lights on hover and then does nothing costs a press to discover, which
/// is worse than never offering it — learned from the tab strip, which kept
/// advertising a review pane after that plugin was deleted.
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
    // Descending priority, then by label so the order is total and stable.
    out.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.label.cmp(&b.label))
    });
    out
}

/// The chord to advertise for an action, or `None` when nothing declares it.
///
/// An F-key alternate wins over a `ctrl+<letter>` primary, which is what both
/// v1's footer and the agent pane's tab strip show. It is not cosmetic: a bare
/// `ctrl+<letter>` is handed to the agent while a terminal has focus, so the
/// F-key is the one that works from where the user usually is.
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

/// `ctrl+t` → `^T`, `f8` → `F8`. The spelling the rest of the interface uses,
/// so one action reads the same wherever it is offered.
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

/// Two cells of padding either side of a label, as v1's pills carry.
const ENTRY_PADDING: usize = 2;
/// One blank cell between neighbouring entries.
const ENTRY_GAP: usize = 1;

/// Width a run of entries occupies.
pub fn entries_width(entries: &[Entry]) -> usize {
    entries
        .iter()
        .map(|entry| entry.display().chars().count() + ENTRY_PADDING)
        .sum::<usize>()
        + entries.len().saturating_sub(1) * ENTRY_GAP
}

/// Drop entries until the run fits, lowest priority first.
///
/// Dropped rather than truncated: half a label is not a smaller button, it is an
/// unreadable one. v1's footer sheds the same way.
pub fn fit(mut entries: Vec<Entry>, width: usize) -> Vec<Entry> {
    while !entries.is_empty() && entries_width(&entries) > width {
        entries.pop();
    }
    entries
}

/// The action the Quit entry runs.
///
/// Quit is **reserved, not declared**: `ctrl+q` is handled before the registry is
/// consulted (`dispatch_reserved`), so no binding exists for an entry to resolve
/// against — and none may, because a *rebindable* quit is what the reserved chord
/// exists to prevent. So the band carries this entry itself and the loop routes a
/// press on it directly, which is why the action name lives here rather than in a
/// declaration table.
pub const QUIT_ACTION: &str = "core.quit";

/// v1's rightmost footer pill, and the only entry with no binding behind it.
///
/// The chord is [`QUIT_CHORD`] — the very string the reserved table
/// refuses to rebind — put through `compact_chord` like every other entry's, so
/// the one action the registry cannot resolve still reads identically to the ones
/// it can. Its priority is below any declared entry's so it renders last, which
/// is v1's `FOOTER_BUTTONS` order.
pub fn quit_entry() -> Entry {
    Entry {
        label: "Quit".to_string(),
        chord: Some(compact_chord(QUIT_CHORD)),
        action: QUIT_ACTION.to_string(),
        priority: i64::MIN,
    }
}

/// The action band's left cluster, in v1's four parts
/// (`ui::status_bar::footer_left_spans`): the focused surface as a badge, the
/// counts, and the informational chords.
///
/// The chords are hints, not buttons: `^H`/`^L` and `^O` have no single hitbox
/// because they are not one target — v1 draws them the same way, and the
/// pressable things live in the right-hand block.
fn left_spans(state: &BandState<'_>) -> Vec<Span<'static>> {
    // v1 `Theme::focused_title()`: a badge, not coloured text — the same
    // treatment a focused pane's own title gets, so the two agree about what has
    // focus.
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

    // `session(s)` rather than a pluralised word: v1's spelling, and it stays
    // right for every count without a branch.
    spans.push(Span::styled(
        format!(" {} session(s) ", state.session_count),
        state.style("text_secondary"),
    ));
    if state.automation_count > 0 {
        spans.push(Span::styled(
            format!(" {} automation(s) ", state.automation_count),
            state.style("text_primary").bg(state
                .colour("accent")
                .unwrap_or(ratatui::style::Color::Reset)),
        ));
    }

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

/// Fit a span row to `budget` columns, v1's `fit_spans_to_budget`.
///
/// Trailing spans are dropped whole until the prefix fits, so the focus badge is
/// the last thing to give up its space; only when that badge alone still
/// overflows is anything truncated.
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

/// What the focus badge says: the view being shown, or the pane showing it.
///
/// A pane in a switch slot can show more than one thing, and the badge has to
/// name what you are actually looking at — v1 labels its centre pane "Shell"
/// rather than "Terminal" while the shell view is up
/// (`view.rs`: `InputFocus::Terminal if is_shell_view`).
///
/// `surface` is the session surface the focused pane painted, which the kernel
/// already resolves from its tree. A surface named `<id>#<view>` is a sub-view,
/// and the part after the `#` is what it is called — so a pane that adds a view
/// gets a correct badge with no change here.
pub fn focus_label(surface: Option<&str>, plugin: &str) -> String {
    let view = surface
        .and_then(|surface| surface.split_once('#'))
        .map(|(_, view)| view)
        .filter(|view| !view.is_empty());
    capitalise(view.unwrap_or(plugin))
}

/// A pane's own name, title-cased for the badge.
///
/// v1 names the centre pane "Terminal" because its focus is a fixed enum; here
/// the label falls back to the pane's own name, which is the only thing that
/// generalises to a pane v1 never had.
fn capitalise(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Draw a band into its resolved rect.
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

/// The role an entry carries, and the verb a click on it performs.
fn entry_role(action: &str) -> String {
    format!("action:{action}")
}

/// v1 `ui::status_bar::render_header`: brand, tagline, version, then the
/// optional update notice; the session and theme right-aligned over the top.
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

/// The message, badged by severity. Progress outranks it: work in flight is the
/// more useful thing to say while it runs, and it is the thing that outlives the
/// retention window.
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

/// v1 `ui::status_bar::render_footer`: the left cluster, then the entries
/// right-aligned and shed lowest-priority-first when the width runs out — around
/// a Quit entry that never sheds.
fn render_action(frame: &mut Frame, area: Rect, state: &BandState<'_>) -> Vec<Hit> {
    let all = entries(state.registry.pills(), state.registry);
    // The left cluster is bounded by what the entries leave, so it can never run
    // underneath them.
    let budget = usize::from(area.width).saturating_sub(2);
    // Quit's cells come off the top rather than being left to `fit`: v1 sheds
    // Theme → Settings → Help around it and keeps Quit at every width, because
    // the button that gets you out must not be the one that disappears. The
    // declared entries then fit into what is left instead of competing with it.
    let quit = quit_entry();
    let reserved = entries_width(std::slice::from_ref(&quit)) + ENTRY_GAP;
    let mut fitted = fit(all, budget.saturating_sub(reserved));
    fitted.push(quit);
    let block = entries_width(&fitted);
    let left_width = usize::from(area.width).saturating_sub(block);

    let left = fit_to_budget(left_spans(state), left_width);
    if !left.is_empty() {
        frame.render_widget(Paragraph::new(Line::from(left)), area);
    }

    // Placed left-to-right from where the right-aligned block starts, so the
    // hitbox of each entry is known exactly rather than inferred from a
    // paragraph's own alignment arithmetic.
    let start = area.x + area.width.saturating_sub(block as u16);
    let mut cursor = start;
    let mut hits = Vec::new();
    for (index, entry) in fitted.iter().enumerate() {
        if index > 0 {
            cursor = cursor.saturating_add(ENTRY_GAP as u16);
        }
        let label = format!(" {} ", entry.display());
        let width = label.chars().count() as u16;
        let rect = Rect {
            x: cursor,
            y: area.y,
            width: width.min(area.right().saturating_sub(cursor)),
            height: 1,
        };
        let identity = super::node::Identity {
            id: None,
            classes: Vec::new(),
            role: Some(entry_role(&entry.action)),
        };
        // v1's button hover: brighten the fill to the accent and force the
        // foreground, so a chip stays legible whatever its resting pair was. A
        // *row* takes a background band instead; an entry here is a button.
        let hovered = state
            .hovered
            .and_then(|identity| identity.role.as_deref())
            .is_some_and(|role| role == entry_role(&entry.action));
        let style = if hovered {
            state
                .style("inverted_fg")
                .bg(state
                    .colour("accent_bright")
                    .unwrap_or(ratatui::style::Color::Reset))
                .add_modifier(Modifier::BOLD)
        } else {
            // v1 `ui::button_style(primary = false)`: the neutral selection pair
            // every palette guarantees is legible, and BOLD — a chip reads as a
            // button by its fill and its weight together.
            state
                .style("selection_fg")
                .bg(state
                    .colour("selection_bg")
                    .unwrap_or(ratatui::style::Color::Reset))
                .add_modifier(Modifier::BOLD)
        };
        frame.render_widget(Paragraph::new(Line::from(Span::styled(label, style))), rect);
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
    use crate::kernel::registry::{Binding, Scope};

    fn binding(action: &str, chord: &str) -> Binding {
        Binding {
            plugin: "p".into(),
            action: action.into(),
            default_chord: chord.into(),
            chord: chord.into(),
            overridden: false,
            description: String::new(),
            scope: Scope::Global,
            passthrough: false,
            group: String::new(),
        }
    }

    fn pill(action: &str, label: &str, priority: i64) -> Pill {
        Pill {
            plugin: "p".into(),
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
        // Rebound through the registry rather than by hand: `declare` re-applies
        // the stored overrides, so a `chord` set directly on a `Binding` is
        // normalised straight back to its default — which is the registry doing
        // its job, and the reason this goes through `rebind`.
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
        // Not cosmetic: a bare ctrl+<letter> is handed to the agent while a
        // terminal has focus, so the F-key is the one that works from where the
        // user usually is. v1's footer shows the same.
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
        // The tab strip's lesson: a chip pointing at a removed pane lit on hover
        // and did nothing.
        let registry = registry_with(
            vec![binding("help.open", "f1")],
            vec![pill("help.open", "Help", 10), pill("gone.open", "Gone", 9)],
        );

        let resolved = entries(registry.pills(), &registry);
        assert_eq!(resolved.len(), 1, "{resolved:?}");
        assert_eq!(resolved[0].label, "Help");
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

        // Wide enough for two but not three.
        let two = entries_width(&resolved[..2]);
        let fitted = fit(resolved.clone(), two);
        let kept: Vec<&str> = fitted.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(kept, ["Aaa", "Bbb"], "the lowest priority sheds first");

        // Nothing fits: an empty run rather than a truncated label.
        assert!(fit(resolved, 3).is_empty());
    }

    #[test]
    fn quit_is_advertised_with_the_reserved_chord_and_renders_last() {
        // The one entry not resolved from the registry, because quit has no
        // binding to resolve — but it must read like every other entry, and sort
        // behind them the way v1's footer puts `Quit` last.
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
            automation_count: 0,
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
        // v1 labels its centre pane "Shell" while the shell view is up. The
        // surface name carries the view, so the badge follows it.
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
        // A view a future pane invents needs no change here.
        assert_eq!(focus_label(Some("abc-123#logs"), "agent"), "Logs");
        // A malformed surface must not produce an empty badge.
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
        // A hidden error is the failure this chrome exists to prevent.
        assert_eq!(Band::DROP_ORDER.last(), Some(&Band::Message));
        assert_eq!(Band::DROP_ORDER.first(), Some(&Band::Identity));
    }
}
