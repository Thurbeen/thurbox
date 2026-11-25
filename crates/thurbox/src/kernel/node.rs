use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const KINDS: [&str; 4] = ["text", "box", "input", "surface"];

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub len: Option<u16>,
    pub pct: Option<f64>,
    pub fill: Option<f64>,
    pub min: Option<u16>,
    pub max: Option<u16>,
}

impl Size {
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

    pub fn click_verb(&self) -> Option<ClickVerb> {
        ClickVerb::parse(self.role.as_deref())
    }

    pub fn is_drag_handle(&self) -> bool {
        self.role.as_deref() == Some(DRAG_ROLE)
    }
}

pub const DRAG_ROLE: &str = "drag";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickVerb {
    Action(String),
    Key(String),
    Focus(String),
    Url(String),
}

impl ClickVerb {
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
            "url" => Some(ClickVerb::Url(rest.to_string())),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overlay {
    pub top_left: Vec<Run>,
    pub top_right: Vec<Run>,
    pub bottom_left: Vec<Run>,
    pub bottom_right: Vec<Run>,
    pub right_column: Vec<Run>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderKind {
    #[default]
    Rounded,
    Square,
    Thick,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    pub title: Option<Vec<Run>>,
    pub title_align: Align,
    pub borders: Borders,
    pub border_type: BorderKind,
    pub border_style: Style,
    pub style: Style,
    pub padding: u16,
    pub overlay: Option<Box<Overlay>>,
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

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub text: String,
    pub style: Style,
    pub identity: Option<Box<Identity>>,
}

impl Run {
    pub fn plain(text: impl Into<String>) -> Self {
        Run {
            text: text.into(),
            style: Style::default(),
            identity: None,
        }
    }

    pub fn width(&self) -> u16 {
        use unicode_width::UnicodeWidthStr;
        u16::try_from(self.text.width()).unwrap_or(u16::MAX)
    }
}

impl Frame {
    pub fn titled(title: impl Into<String>) -> Self {
        Frame {
            title: Some(vec![Run::plain(title)]),
            ..Frame::default()
        }
    }

    pub fn title_text(&self) -> Option<String> {
        self.title
            .as_ref()
            .map(|runs| runs.iter().map(|run| run.text.as_str()).collect())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Text {
        lines: Vec<Vec<Run>>,
        align: Align,
        wrap: bool,
        scroll: u16,
        style: Style,
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
        focused: bool,
        style: Style,
        frame: Option<Frame>,
        size: Size,
        identity: Identity,
    },
    Surface {
        source: SurfaceSource,
        scroll: u16,
        mark: Option<u16>,
        frame: Option<Frame>,
        size: Size,
        identity: Identity,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SurfaceSource {
    Session(String),
    Program(String),
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

    pub fn kind(&self) -> &'static str {
        match self {
            Node::Text { .. } => "text",
            Node::Box { .. } => "box",
            Node::Input { .. } => "input",
            Node::Surface { .. } => "surface",
        }
    }

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

    pub fn first_live_surface(&self) -> Option<&str> {
        match self {
            Node::Surface {
                source: SurfaceSource::Session(id) | SurfaceSource::Program(id),
                ..
            } => Some(id),
            Node::Box { children, .. } => children.iter().find_map(Node::first_live_surface),
            _ => None,
        }
    }

    pub fn session_surface(&self, id: &str) -> Option<(u16, Option<u16>)> {
        match self {
            Node::Surface {
                source: SurfaceSource::Session(shown),
                scroll,
                mark,
                ..
            } if shown == id => Some((*scroll, *mark)),
            Node::Box { children, .. } => {
                children.iter().find_map(|child| child.session_surface(id))
            }
            _ => None,
        }
    }

    pub fn has_focused_input(&self) -> bool {
        match self {
            Node::Input { focused, .. } => *focused,
            Node::Box { children, .. } => children.iter().any(Node::has_focused_input),
            _ => false,
        }
    }

    pub fn empty() -> Self {
        Node::Text {
            lines: Vec::new(),
            align: Align::Left,
            wrap: false,
            scroll: 0,
            style: Style::default(),
            frame: None,
            size: Size::default(),
            identity: Identity::default(),
        }
    }

    pub fn line(text: impl Into<String>, style: Style) -> Self {
        Node::Text {
            lines: vec![vec![Run {
                text: text.into(),
                style,
                identity: None,
            }]],
            align: Align::Left,
            wrap: true,
            scroll: 0,
            style: Style::default(),
            frame: None,
            size: Size::default(),
            identity: Identity::default(),
        }
    }
}

pub fn divide(total: u16, children: &[Size], gap: u16) -> Vec<u16> {
    if children.is_empty() {
        return Vec::new();
    }
    let gaps = gap.saturating_mul(children.len().saturating_sub(1) as u16);
    let available = total.saturating_sub(gaps);

    let mut lengths: Vec<u16> = children
        .iter()
        .map(|size| requested_length(size, available))
        .collect();

    let fixed: u32 = children
        .iter()
        .zip(&lengths)
        .filter(|(size, _)| !size.is_flexible())
        .map(|(_, len)| u32::from(*len))
        .sum();

    if fixed > u32::from(available) {
        scale_fixed_to_fit(children, &mut lengths, available, fixed);
    } else {
        share_among_flexible(
            children,
            &mut lengths,
            available.saturating_sub(fixed as u16),
        );
    }
    lengths
}

fn requested_length(size: &Size, available: u16) -> u16 {
    if let Some(len) = size.len {
        size.clamp(len)
    } else if let Some(pct) = size.pct {
        let raw = (f64::from(available) * pct / 100.0).round();
        size.clamp(raw.clamp(0.0, f64::from(u16::MAX)) as u16)
    } else {
        0
    }
}

fn scale_fixed_to_fit(children: &[Size], lengths: &mut [u16], available: u16, fixed: u32) {
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
}

fn share_among_flexible(children: &[Size], lengths: &mut [u16], remainder: u16) {
    let total_share: f64 = children
        .iter()
        .filter(|size| size.is_flexible())
        .map(Size::share)
        .sum();
    if total_share <= 0.0 {
        return;
    }

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
            remainder.saturating_sub(handed_out)
        } else {
            let raw = f64::from(remainder) * size.share() / total_share;
            raw.floor() as u16
        };
        let clamped = size.clamp(length).min(remainder.saturating_sub(handed_out));
        lengths[index] = clamped;
        handed_out = handed_out.saturating_add(clamped);
    }
}

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

#[derive(Debug, Clone, PartialEq)]
pub enum StyleField {
    Text(String),
    Flag(bool),
}

pub fn to_line(runs: &[Run]) -> Line<'_> {
    Line::from(
        runs.iter()
            .map(|run| Span::styled(run.text.as_str(), run.style))
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
        assert_eq!(KINDS.len(), 4);
        assert_eq!(KINDS, ["text", "box", "input", "surface"]);
    }

    #[test]
    fn exact_percent_and_fill_share_one_axis() {
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
    fn oversubscription_hands_the_rounding_remainder_out_in_order() {
        let one = size(Some(1), None, None);
        assert_eq!(divide(2, &[one, one, one], 0), vec![1, 1, 0]);
        let ten = size(Some(10), None, None);
        let fill = size(None, None, None);
        assert_eq!(divide(10, &[ten, fill, ten], 0), vec![5, 0, 5]);
        assert_eq!(divide(12, &[ten, ten, ten], 0), vec![4, 4, 4]);
    }

    #[test]
    fn a_percentage_is_of_the_space_after_gaps() {
        let sizes = [size(None, Some(50.0), None), size(None, None, None)];
        assert_eq!(divide(21, &sizes, 1), vec![10, 10]);
        let sizes = [size(None, Some(150.0), None)];
        assert_eq!(divide(10, &sizes, 0), vec![10]);
    }

    #[test]
    fn a_minimum_never_takes_more_than_is_left() {
        let at_least_eight = Size {
            min: Some(8),
            ..Size::default()
        };
        assert_eq!(divide(10, &[at_least_eight, at_least_eight], 0), vec![8, 2]);
        let nothing = Size {
            fill: Some(0.0),
            ..Size::default()
        };
        assert_eq!(
            divide(10, &[size(Some(3), None, None), nothing], 0),
            vec![3, 0]
        );
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
    fn a_url_verb_keeps_the_links_own_colons() {
        assert_eq!(
            ClickVerb::parse(Some("url:https://example.test/a:b?q=1#f")),
            Some(ClickVerb::Url("https://example.test/a:b?q=1#f".to_string()))
        );
        assert_eq!(
            ClickVerb::parse(Some("url:mailto:someone@example.test")),
            Some(ClickVerb::Url("mailto:someone@example.test".to_string()))
        );
        assert_eq!(ClickVerb::parse(Some("url:")), None);
        assert_eq!(ClickVerb::parse(Some("url:   ")), None);
    }

    #[test]
    fn a_plain_role_is_not_a_verb() {
        assert_eq!(ClickVerb::parse(Some("row")), None);
        assert_eq!(ClickVerb::parse(Some("overflow")), None);
        assert_eq!(ClickVerb::parse(Some("action:")), None);
        assert_eq!(ClickVerb::parse(Some("shout:hello")), None);
        assert_eq!(ClickVerb::parse(None), None);
    }

    #[test]
    fn the_first_live_surface_is_either_kind_but_never_mod_drawn_cells() {
        let session = Node::Surface {
            source: SurfaceSource::Session("s1".into()),
            scroll: 0,
            mark: None,
            frame: None,
            size: Size::default(),
            identity: Identity::default(),
        };
        let program = Node::Surface {
            source: SurfaceSource::Program("program:mods/90_watch.lua#watch".into()),
            scroll: 0,
            mark: None,
            frame: None,
            size: Size::default(),
            identity: Identity::default(),
        };
        let cells = Node::Surface {
            source: SurfaceSource::Cells(vec![Vec::new()]),
            scroll: 0,
            mark: None,
            frame: None,
            size: Size::default(),
            identity: Identity::default(),
        };

        assert_eq!(session.first_live_surface(), Some("s1"));
        assert_eq!(session.first_session_surface(), Some("s1"));

        assert_eq!(
            program.first_live_surface(),
            Some("program:mods/90_watch.lua#watch")
        );
        assert_eq!(
            program.first_session_surface(),
            None,
            "a program is not a session, and everything that is about sessions must \
             keep getting that answer"
        );

        assert_eq!(cells.first_live_surface(), None);

        let wrapped = Node::Box {
            axis: Axis::Vertical,
            gap: 0,
            children: vec![cells, program],
            frame: None,
            size: Size::default(),
            identity: Identity::default(),
        };
        assert_eq!(
            wrapped.first_live_surface(),
            Some("program:mods/90_watch.lua#watch"),
            "the mod-drawn cells are skipped rather than claiming the keys"
        );
    }
}
