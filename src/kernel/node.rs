//! The view vocabulary: **four** primitives and nothing else.
//!
//! `text`, `box`, `input`, `surface`. Lists, gauges, dividers, tables and
//! titled panels are *not* here — they are composed in Lua from these four
//! (`ui/lib/widgets.lua`). That is the whole point of design.md D1: a prior
//! attempt froze a catalog at 6 kinds and watched it reach 16 without gaining
//! the ability to express code review, because every new appearance had
//! nowhere to live but a kernel enum variant. Adding a variant here costs a
//! release; adding a widget in Lua costs a file save.
//!
//! `kinds()` is the tripwire — `tests/kernel_node_catalog.rs` pins it at four.
//!
//! Lua never holds a ratatui object. A plugin returns a plain table, this
//! module translates it, and that indirection is exactly why throwing the VM
//! away on reload is safe.

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// The complete node vocabulary. Four. See the module docs before adding one.
pub const KINDS: [&str; 4] = ["text", "box", "input", "surface"];

/// How a child asks for space along its parent's axis.
///
/// Resolution order is exact → percentage → share, each clamped by `min`/`max`.
/// A child that asks for nothing gets an equal share of what is left, which is
/// what makes a plugin's root node work without declaring anything.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub len: Option<u16>,
    pub pct: Option<f64>,
    /// Proportional share of the remainder. Defaults to 1 when nothing is set.
    pub fill: Option<f64>,
    pub min: Option<u16>,
    pub max: Option<u16>,
}

impl Size {
    /// True when the child expressed no opinion and should share the remainder.
    fn is_flexible(&self) -> bool {
        self.len.is_none() && self.pct.is_none()
    }

    fn share(&self) -> f64 {
        self.fill.unwrap_or(1.0).max(0.0)
    }

    fn clamp(&self, value: u16) -> u16 {
        let mut v = value;
        if let Some(min) = self.min {
            v = v.max(min);
        }
        if let Some(max) = self.max {
            v = v.min(max);
        }
        v
    }
}

/// Optional targeting information carried by any node.
///
/// Present so that an input event can be attributed to the node under it, and
/// so styling can one day be expressed separately from structure (design.md
/// D4/D6). The kernel stores and propagates it; it does **not** resolve
/// selectors against it — that stays in userland until a real consumer exists.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Identity {
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub role: Option<String>,
}

impl Identity {
    pub fn is_empty(&self) -> bool {
        self.id.is_none() && self.classes.is_empty() && self.role.is_none()
    }

    /// The click verb this node declares, if any.
    pub fn click_verb(&self) -> Option<ClickVerb> {
        ClickVerb::parse(self.role.as_deref())
    }
}

/// What the kernel itself does when a painted node is clicked.
///
/// This is v1's `ClickAction` (`src/app/mod.rs`) reduced to the part the kernel
/// can honour without knowing what any pane is: each variant names an entry
/// point the *keyboard* already uses, so a click and a keypress cannot drift
/// apart. Anything outside this vocabulary is handed to the owning plugin as an
/// ordinary click event instead.
///
/// It is spelled as a prefix on [`Identity::role`] rather than as a field of
/// its own because identity already travels through `convert` and `paint`
/// untouched — so a plugin declares a click target by writing one string, and
/// the node catalogue stays at four.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickVerb {
    /// `action:<id>` — run a declared action, exactly as its chord would.
    /// v1's `ClickAction::Global`, which the footer pills use.
    Action(String),
    /// `key:<chord>` — replay a keystroke through the normal key path. v1's
    /// `ClickAction::ModalButton`, which is why clicking a modal button and
    /// pressing its letter cannot diverge.
    Key(String),
    /// `focus:<plugin>` — make that plugin the focused one. v1's
    /// `ClickAction::FocusPane` and `CentralTab`, which in v2 are the same act:
    /// a `switch` slot shows whichever occupant holds focus.
    Focus(String),
}

impl ClickVerb {
    /// Read a verb off a role string. A role the kernel does not recognise
    /// (`row`, `overflow`, …) is not a verb — the click goes to the plugin.
    pub fn parse(role: Option<&str>) -> Option<Self> {
        let (verb, rest) = role?.split_once(':')?;
        let rest = rest.trim();
        if rest.is_empty() {
            return None;
        }
        match verb {
            "action" => Some(ClickVerb::Action(rest.to_string())),
            "key" => Some(ClickVerb::Key(rest.to_string())),
            "focus" => Some(ClickVerb::Focus(rest.to_string())),
            _ => None,
        }
    }
}

/// A frame drawn around a node.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    pub title: Option<String>,
    pub borders: Borders,
    pub border_style: Style,
    pub style: Style,
    pub padding: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Borders {
    #[default]
    All,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// One styled run of text within a line.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub text: String,
    pub style: Style,
}

/// A node in the view tree.
///
/// Every variant carries `size` (what it asks of its parent) and `identity`
/// (how it can be targeted). `Surface` carries no children by design — it is
/// the geometry-first escape hatch (design.md D3).
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Text {
        lines: Vec<Vec<Run>>,
        align: Align,
        wrap: bool,
        scroll: u16,
        frame: Option<Frame>,
        size: Size,
        identity: Identity,
    },
    Box {
        axis: Axis,
        gap: u16,
        children: Vec<Node>,
        frame: Option<Frame>,
        size: Size,
        identity: Identity,
    },
    Input {
        value: String,
        cursor: usize,
        placeholder: String,
        style: Style,
        frame: Option<Frame>,
        size: Size,
        identity: Identity,
    },
    /// Pre-rendered cells the kernel paints. Fed by a live session's terminal
    /// or by a plugin that produces cells itself. `source` names which.
    Surface {
        source: SurfaceSource,
        scroll: u16,
        frame: Option<Frame>,
        size: Size,
        identity: Identity,
    },
}

/// Where a surface's cells come from.
#[derive(Debug, Clone, PartialEq)]
pub enum SurfaceSource {
    /// A live session, named by its id. The kernel resolves it to that
    /// session's terminal grid.
    Session(String),
    /// Cells the plugin produced itself, one entry per line.
    Cells(Vec<Vec<Run>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

impl Node {
    pub fn size(&self) -> Size {
        match self {
            Node::Text { size, .. }
            | Node::Box { size, .. }
            | Node::Input { size, .. }
            | Node::Surface { size, .. } => *size,
        }
    }

    pub fn identity(&self) -> &Identity {
        match self {
            Node::Text { identity, .. }
            | Node::Box { identity, .. }
            | Node::Input { identity, .. }
            | Node::Surface { identity, .. } => identity,
        }
    }

    pub fn frame(&self) -> Option<&Frame> {
        match self {
            Node::Text { frame, .. }
            | Node::Box { frame, .. }
            | Node::Input { frame, .. }
            | Node::Surface { frame, .. } => frame.as_ref(),
        }
    }

    /// The kind name, as a plugin would have written it.
    pub fn kind(&self) -> &'static str {
        match self {
            Node::Text { .. } => "text",
            Node::Box { .. } => "box",
            Node::Input { .. } => "input",
            Node::Surface { .. } => "surface",
        }
    }

    /// The first session-backed surface in this tree, if any.
    ///
    /// Used to answer "which session is the focused pane showing?" without the
    /// kernel knowing anything about which plugin is the terminal.
    pub fn first_session_surface(&self) -> Option<&str> {
        match self {
            Node::Surface {
                source: SurfaceSource::Session(id),
                ..
            } => Some(id),
            Node::Box { children, .. } => children.iter().find_map(Node::first_session_surface),
            _ => None,
        }
    }

    /// An empty placeholder, used where a plugin produced nothing.
    pub fn empty() -> Self {
        Node::Text {
            lines: Vec::new(),
            align: Align::Left,
            wrap: false,
            scroll: 0,
            frame: None,
            size: Size::default(),
            identity: Identity::default(),
        }
    }

    /// A single line of plain text — the building block for error panes.
    pub fn line(text: impl Into<String>, style: Style) -> Self {
        Node::Text {
            lines: vec![vec![Run {
                text: text.into(),
                style,
            }]],
            align: Align::Left,
            wrap: true,
            scroll: 0,
            frame: None,
            size: Size::default(),
            identity: Identity::default(),
        }
    }
}

/// Divide `total` among `children` along one axis.
///
/// This is the arithmetic behind design.md D2's promise that a plugin is told
/// its *own* rect: the parent resolves every child's length here, before any
/// child is asked to render into it.
///
/// Exact and percentage requests are honoured first, then flexible children
/// split the remainder by share. Over-subscription is handled by scaling the
/// fixed requests down proportionally rather than by letting a child go
/// negative — a spec requirement, since a plugin can ask for more than exists.
pub fn divide(total: u16, children: &[Size], gap: u16) -> Vec<u16> {
    if children.is_empty() {
        return Vec::new();
    }
    let gaps = gap.saturating_mul(children.len().saturating_sub(1) as u16);
    let available = total.saturating_sub(gaps);

    // Pass 1: what each sized child asks for.
    let mut lengths: Vec<u16> = children
        .iter()
        .map(|size| {
            if let Some(len) = size.len {
                size.clamp(len)
            } else if let Some(pct) = size.pct {
                let raw = (f64::from(available) * pct / 100.0).round();
                size.clamp(raw.clamp(0.0, f64::from(u16::MAX)) as u16)
            } else {
                0
            }
        })
        .collect();

    let fixed: u32 = children
        .iter()
        .zip(&lengths)
        .filter(|(size, _)| !size.is_flexible())
        .map(|(_, len)| u32::from(*len))
        .sum();

    // Over-subscribed: scale the fixed requests to fit rather than emit a
    // negative length. Deterministic, and every child still gets >= 0.
    if fixed > u32::from(available) {
        let scale = f64::from(available) / fixed as f64;
        let mut used = 0u32;
        for (size, len) in children.iter().zip(lengths.iter_mut()) {
            if size.is_flexible() {
                *len = 0;
                continue;
            }
            let scaled = (f64::from(*len) * scale).floor() as u16;
            *len = scaled;
            used += u32::from(scaled);
        }
        // Hand the rounding remainder out a row at a time, in order, rather than
        // all of it to the first child. It matters more than rounding usually
        // does: N children of `len = 1` in N-1 rows scale to 0 apiece, so the
        // remainder *is* the whole rect, and giving it to the first child paints
        // one enormous row and loses every other one. Spread, the tail is clipped
        // instead — which is what a list one row too long should look like, and
        // the difference between a search strip that truncates and one that goes
        // blank. One pass is enough: each dropped fraction is below 1, so the
        // remainder is smaller than the number of fixed children.
        let mut remainder = u32::from(available).saturating_sub(used);
        for (size, len) in children.iter().zip(lengths.iter_mut()) {
            if remainder == 0 {
                break;
            }
            if size.is_flexible() {
                continue;
            }
            *len = len.saturating_add(1);
            remainder -= 1;
        }
        return lengths;
    }

    // Pass 2: flexible children share what is left, by their `fill` weight.
    let remainder = available.saturating_sub(fixed as u16);
    let total_share: f64 = children
        .iter()
        .filter(|size| size.is_flexible())
        .map(Size::share)
        .sum();

    if total_share > 0.0 {
        let mut handed_out = 0u16;
        let flexible: Vec<usize> = children
            .iter()
            .enumerate()
            .filter(|(_, size)| size.is_flexible())
            .map(|(i, _)| i)
            .collect();

        for (nth, &index) in flexible.iter().enumerate() {
            let size = &children[index];
            let length = if nth + 1 == flexible.len() {
                // Last flexible child absorbs the rounding drift, so the
                // children always sum to exactly `available`.
                remainder.saturating_sub(handed_out)
            } else {
                let raw = f64::from(remainder) * size.share() / total_share;
                raw.floor() as u16
            };
            // `min` may ask for more than the share, but never for more than is
            // left: two children with `min = 8` in a 10-row box would otherwise
            // sum to 16, and `paint`/`divide_slot` would hand out rects that start
            // and end outside their parent.
            let clamped = size.clamp(length).min(remainder.saturating_sub(handed_out));
            lengths[index] = clamped;
            handed_out = handed_out.saturating_add(clamped);
        }
    }

    lengths
}

/// Parse a colour written as a name, a `#rrggbb` hex string, or a palette index.
pub fn parse_color(raw: &str) -> Option<Color> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(Color::Rgb(r, g, b));
        }
        return None;
    }
    if let Ok(index) = value.parse::<u8>() {
        return Some(Color::Indexed(index));
    }
    let named = match value.to_ascii_lowercase().replace('_', "-").as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" | "white" => Color::Gray,
        "dark-gray" | "dark-grey" => Color::DarkGray,
        "light-red" => Color::LightRed,
        "light-green" => Color::LightGreen,
        "light-yellow" => Color::LightYellow,
        "light-blue" => Color::LightBlue,
        "light-magenta" => Color::LightMagenta,
        "light-cyan" => Color::LightCyan,
        "light-white" => Color::White,
        "reset" => Color::Reset,
        _ => return None,
    };
    Some(named)
}

/// Build a ratatui `Style` from a table of style keys.
pub fn style_from(fields: &HashMap<String, StyleField>) -> Style {
    let mut style = Style::default();
    for (key, value) in fields {
        match (key.as_str(), value) {
            ("fg", StyleField::Text(v)) => {
                if let Some(color) = parse_color(v) {
                    style = style.fg(color);
                }
            }
            ("bg", StyleField::Text(v)) => {
                if let Some(color) = parse_color(v) {
                    style = style.bg(color);
                }
            }
            ("bold", StyleField::Flag(true)) => style = style.add_modifier(Modifier::BOLD),
            ("dim", StyleField::Flag(true)) => style = style.add_modifier(Modifier::DIM),
            ("italic", StyleField::Flag(true)) => style = style.add_modifier(Modifier::ITALIC),
            ("underline", StyleField::Flag(true)) => {
                style = style.add_modifier(Modifier::UNDERLINED)
            }
            ("reversed", StyleField::Flag(true)) => style = style.add_modifier(Modifier::REVERSED),
            ("crossed-out" | "crossed_out", StyleField::Flag(true)) => {
                style = style.add_modifier(Modifier::CROSSED_OUT)
            }
            _ => {}
        }
    }
    style
}

/// A single field inside a style table — either a colour string or a flag.
#[derive(Debug, Clone, PartialEq)]
pub enum StyleField {
    Text(String),
    Flag(bool),
}

/// Flatten runs into a ratatui `Line`.
pub fn to_line(runs: &[Run]) -> Line<'static> {
    Line::from(
        runs.iter()
            .map(|run| Span::styled(run.text.clone(), run.style))
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(len: Option<u16>, pct: Option<f64>, fill: Option<f64>) -> Size {
        Size {
            len,
            pct,
            fill,
            ..Size::default()
        }
    }

    #[test]
    fn the_catalog_is_exactly_four_kinds() {
        // The tripwire for design.md D1. Growing this is a design decision,
        // not a convenience — see the module docs.
        assert_eq!(KINDS.len(), 4);
        assert_eq!(KINDS, ["text", "box", "input", "surface"]);
    }

    #[test]
    fn exact_percent_and_fill_share_one_axis() {
        // 3 exact + 50% of 20 = 10, leaving 7 split evenly between two flexible.
        let sizes = [
            size(Some(3), None, None),
            size(None, Some(50.0), None),
            size(None, None, None),
            size(None, None, None),
        ];
        let lengths = divide(20, &sizes, 0);
        assert_eq!(lengths, vec![3, 10, 3, 4]);
        assert_eq!(lengths.iter().sum::<u16>(), 20);
    }

    #[test]
    fn fill_weights_split_the_remainder_proportionally() {
        let sizes = [size(None, None, Some(2.0)), size(None, None, Some(1.0))];
        let lengths = divide(30, &sizes, 0);
        assert_eq!(lengths, vec![20, 10]);
    }

    #[test]
    fn gaps_come_out_of_the_available_space() {
        let sizes = [size(None, None, None), size(None, None, None)];
        let lengths = divide(11, &sizes, 1);
        assert_eq!(lengths.iter().sum::<u16>(), 10);
    }

    #[test]
    fn oversubscription_scales_down_and_never_goes_negative() {
        // Three children demanding 10 each in a 12-wide axis.
        let sizes = [
            size(Some(10), None, None),
            size(Some(10), None, None),
            size(Some(10), None, None),
        ];
        let lengths = divide(12, &sizes, 0);
        assert!(lengths.iter().sum::<u16>() <= 12);
        assert_eq!(lengths.len(), 3);
    }

    #[test]
    fn min_and_max_clamp_a_share() {
        let sizes = [
            Size {
                fill: Some(1.0),
                max: Some(4),
                ..Size::default()
            },
            Size::default(),
        ];
        let lengths = divide(20, &sizes, 0);
        assert_eq!(lengths[0], 4);
    }

    #[test]
    fn no_children_divides_to_nothing() {
        assert!(divide(50, &[], 1).is_empty());
    }

    #[test]
    fn colours_parse_from_name_hex_and_index() {
        assert_eq!(parse_color("light-cyan"), Some(Color::LightCyan));
        assert_eq!(parse_color("#5fafff"), Some(Color::Rgb(0x5f, 0xaf, 0xff)));
        assert_eq!(parse_color("33"), Some(Color::Indexed(33)));
        assert_eq!(parse_color("not-a-colour"), None);
        assert_eq!(parse_color(""), None);
    }

    #[test]
    fn identity_is_optional() {
        assert!(Identity::default().is_empty());
        assert!(Node::empty().identity().is_empty());
    }

    #[test]
    fn a_role_prefix_names_a_click_verb() {
        assert_eq!(
            ClickVerb::parse(Some("action:help.open")),
            Some(ClickVerb::Action("help.open".to_string()))
        );
        assert_eq!(
            ClickVerb::parse(Some("key:ctrl+q")),
            Some(ClickVerb::Key("ctrl+q".to_string()))
        );
        assert_eq!(
            ClickVerb::parse(Some("focus:review")),
            Some(ClickVerb::Focus("review".to_string()))
        );
    }

    #[test]
    fn a_plain_role_is_not_a_verb() {
        // `row` and `overflow` are what widgets.lua already writes; they must
        // keep meaning "ask the plugin", not "the kernel handles this".
        assert_eq!(ClickVerb::parse(Some("row")), None);
        assert_eq!(ClickVerb::parse(Some("overflow")), None);
        assert_eq!(ClickVerb::parse(Some("action:")), None);
        assert_eq!(ClickVerb::parse(Some("shout:hello")), None);
        assert_eq!(ClickVerb::parse(None), None);
    }
}
