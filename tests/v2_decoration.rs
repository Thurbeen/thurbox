//! Decoration must not cost a pane its styling.
//!
//! Regression: `to_lua` deliberately dropped each run's `Style` on the way out
//! to a decorator, on the reasoning that a decorator "sets the styles it
//! wants". But a decorator is handed the tree and returns one, so whatever the
//! boundary drops is dropped from the pane — and the common case is a
//! decorator that returns its input *untouched* (search, whenever its query is
//! empty). The session list is decorated by search, so every colour it drew was
//! being thrown away on every frame: it rendered in the terminal's default
//! foreground while every undecorated pane was themed correctly.
//!
//! The bug was invisible to the existing tests because they render a plugin
//! directly, which never crosses the decoration boundary.

use ratatui::style::{Color, Modifier, Style};

use thurbox::kernel::convert::{to_lua, to_node};
use thurbox::kernel::node::{Identity, Node, Run};

fn styled_tree() -> Node {
    Node::Text {
        identity: Identity {
            id: Some("row-1".into()),
            classes: vec![],
            role: Some("row".into()),
        },
        size: Default::default(),
        frame: None,
        lines: vec![vec![
            Run {
                text: "○ ".into(),
                style: Style::default().fg(Color::Green),
            },
            Run {
                text: "fix-osc52".into(),
                style: Style::default()
                    .fg(Color::White)
                    .bg(Color::Indexed(24))
                    .add_modifier(Modifier::BOLD),
            },
        ]],
        align: Default::default(),
        wrap: false,
        scroll: Default::default(),
    }
}

/// The round trip a decorator sits inside: Node → Lua → Node.
fn round_trip(node: &Node) -> Node {
    let lua = mlua::Lua::new();
    let value = to_lua(&lua, node).expect("to_lua");
    to_node(&value).expect("to_node")
}

#[test]
fn a_style_survives_the_trip_out_to_a_decorator_and_back() {
    let before = styled_tree();
    let after = round_trip(&before);

    let Node::Text { lines, .. } = &after else {
        panic!("expected a text node, got {after:?}");
    };
    let runs = &lines[0];

    assert_eq!(
        runs[0].style.fg,
        Some(Color::Green),
        "the status dot lost its colour crossing the boundary"
    );
    assert_eq!(runs[1].style.fg, Some(Color::White), "name lost its fg");
    assert_eq!(
        runs[1].style.bg,
        Some(Color::Indexed(24)),
        "the selection bar lost its background"
    );
    assert!(
        runs[1].style.add_modifier.contains(Modifier::BOLD),
        "the selection bar lost its bold"
    );
}

#[test]
fn an_untouched_tree_comes_back_unchanged() {
    // The strongest form of the rule, and the case that actually broke: a
    // decorator that changes nothing must cost nothing.
    let before = styled_tree();
    assert_eq!(round_trip(&before), before);
}

#[test]
fn an_unstyled_run_stays_unstyled() {
    // The inverse guard: the fix must not invent a style for a plain run, or
    // every unstyled span would start carrying a table through the tree diff.
    let node = Node::Text {
        identity: Identity::default(),
        size: Default::default(),
        frame: None,
        lines: vec![vec![Run {
            text: "plain".into(),
            style: Style::default(),
        }]],
        align: Default::default(),
        wrap: false,
        scroll: Default::default(),
    };
    assert_eq!(round_trip(&node), node);
}

/// The rule applied to every field a node carries, not only a text run's style.
///
/// `an_untouched_tree_comes_back_unchanged` above proves the rule for the field
/// that was fixed; these are the ones that were still being dropped, each of which
/// makes an identity decorator change the pane it decorated: a framed box lost its
/// border colour, a sized child lost the `min`/`max` that keep it on screen, a
/// scrolled paragraph jumped back to the top, an input lost its colour, and a
/// plugin-fed surface — where a review diff's syntax colouring lives — was
/// flattened to plain text.
#[test]
fn every_field_survives_the_trip_out_and_back() {
    use thurbox::kernel::node::{Align, Axis, Borders, Frame, Size, SurfaceSource};

    let framed = Node::Box {
        axis: Axis::Horizontal,
        gap: 1,
        identity: Identity::default(),
        size: Size {
            len: Some(4),
            pct: None,
            fill: Some(2.0),
            min: Some(3),
            max: Some(9),
        },
        frame: Some(Frame {
            title: Some("Panel".into()),
            borders: Borders::All,
            border_style: Style::default().fg(Color::Magenta),
            style: Style::default().bg(Color::Indexed(17)),
            padding: 1,
        }),
        children: vec![
            Node::Text {
                identity: Identity::default(),
                size: Default::default(),
                frame: None,
                lines: vec![vec![Run {
                    text: "scrolled".into(),
                    style: Style::default(),
                }]],
                align: Align::Right,
                wrap: true,
                scroll: 7,
            },
            Node::Input {
                identity: Identity::default(),
                size: Default::default(),
                frame: None,
                value: "typed".into(),
                cursor: 3,
                placeholder: "hint".into(),
                style: Style::default().fg(Color::Yellow),
            },
            Node::Surface {
                identity: Identity::default(),
                size: Default::default(),
                frame: None,
                scroll: 2,
                source: SurfaceSource::Cells(vec![vec![
                    Run {
                        text: "+ added".into(),
                        style: Style::default().fg(Color::Green),
                    },
                    Run {
                        text: " tail".into(),
                        style: Style::default().fg(Color::Red),
                    },
                ]]),
            },
        ],
    };

    assert_eq!(round_trip(&framed), framed);
}
