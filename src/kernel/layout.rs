//! The constraint pass: resolve every rect **before** any plugin renders.
//!
//! This is design.md D2, and it is the one place the POC had to be inverted.
//! The POC rendered every plugin first and arranged the returned trees
//! afterwards, which makes a plugin's own rect unknowable while it is
//! rendering — so percentage widths, wrapping, truncation and scroll-window
//! derivation are all impossible. The prior v2 attempt named this its single
//! biggest gap.
//!
//! Here the order is: arrangement (a Lua function of the available size) →
//! slot rects → per-plugin rects → *then* each plugin is called and told the
//! rect it is drawing into.
//!
//! The arrangement can do this without calling plugins because a plugin
//! declares the space it wants **statically**, in its declaration table, not
//! in its render output. That is what breaks the circularity.

use std::collections::BTreeSet;

use mlua::{Table, Value};
use ratatui::layout::Rect;

use super::node::{divide, Axis, Size};

/// The size an arrangement is resolved at when the question is *whether* a slot
/// is placed rather than *where*.
///
/// Placement is size-dependent by design — the bundled arrangement drops the
/// session column below `two_panel_min_cols` and the header below twenty rows —
/// so "unplaced" is only a meaningful verdict at a size where the slot should
/// have been placed. This is comfortably above every breakpoint the bundled
/// arrangement has, and above a plausibly-raised `two_panel_min_cols`, so what
/// is missing here is missing because nothing names it rather than because the
/// screen is small.
///
/// Reported alongside any verdict drawn from it, so a surprising answer is
/// explicable rather than mysterious.
pub const REFERENCE: Rect = Rect {
    x: 0,
    y: 0,
    width: 200,
    height: 50,
};

/// How a slot's occupants share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlotMode {
    /// Every occupant is visible, sharing the slot by declared size.
    #[default]
    Stack,
    /// One occupant is visible; the rest are selectable alternatives.
    Switch,
}

/// A node in the arrangement tree: either a named slot (a leaf) or a box that
/// divides its rect among children.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    /// Slot placed here, when this is a leaf.
    pub slot: Option<String>,
    pub axis: Axis,
    pub gap: u16,
    pub children: Vec<Region>,
    pub size: Size,
}

impl Default for Region {
    fn default() -> Self {
        Self {
            slot: None,
            axis: Axis::Vertical,
            gap: 0,
            children: Vec::new(),
            size: Size::default(),
        }
    }
}

/// Where one slot ended up.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotRect {
    pub slot: String,
    pub rect: Rect,
}

/// Walk the arrangement, assigning a rect to every named slot.
///
/// A slot named more than once keeps its first placement; a slot named nowhere
/// simply gets no rect, and its plugins are not rendered (which is how a
/// responsive arrangement hides a column at a narrow width).
pub fn resolve(region: &Region, area: Rect) -> Vec<SlotRect> {
    let mut out = Vec::new();
    walk(region, area, &mut out);
    out
}

/// Which slots an arrangement places, without their rects.
///
/// The set a loaded plugin's slot is compared against to answer the one failure
/// no other signal reports: a plugin whose slot the arrangement names nowhere
/// loads without error, declares its keys, is granted its capabilities — and
/// draws nothing. Every other check passes it.
pub fn placed_slots(region: &Region, area: Rect) -> BTreeSet<String> {
    resolve(region, area)
        .into_iter()
        .map(|placed| placed.slot)
        .collect()
}

fn walk(region: &Region, area: Rect, out: &mut Vec<SlotRect>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if let Some(slot) = &region.slot {
        if !out.iter().any(|placed| &placed.slot == slot) {
            out.push(SlotRect {
                slot: slot.clone(),
                rect: area,
            });
        }
        return;
    }
    if region.children.is_empty() {
        return;
    }

    let sizes: Vec<Size> = region.children.iter().map(|child| child.size).collect();
    let total = match region.axis {
        Axis::Vertical => area.height,
        Axis::Horizontal => area.width,
    };
    let lengths = divide(total, &sizes, region.gap);

    let mut cursor = match region.axis {
        Axis::Vertical => area.y,
        Axis::Horizontal => area.x,
    };
    for (child, length) in region.children.iter().zip(lengths) {
        let rect = match region.axis {
            Axis::Vertical => Rect {
                x: area.x,
                y: cursor,
                width: area.width,
                height: length,
            },
            Axis::Horizontal => Rect {
                x: cursor,
                y: area.y,
                width: length,
                height: area.height,
            },
        };
        walk(child, rect, out);
        cursor = cursor.saturating_add(length).saturating_add(region.gap);
    }
}

/// Divide one slot's rect among the plugins occupying it.
///
/// `sizes` are the plugins' **declared** sizes, read from their declaration
/// tables — never from their render output, which does not exist yet.
pub fn divide_slot(area: Rect, axis: Axis, sizes: &[Size], gap: u16) -> Vec<Rect> {
    let total = match axis {
        Axis::Vertical => area.height,
        Axis::Horizontal => area.width,
    };
    let lengths = divide(total, sizes, gap);
    let mut cursor = match axis {
        Axis::Vertical => area.y,
        Axis::Horizontal => area.x,
    };
    let mut rects = Vec::with_capacity(lengths.len());
    for length in lengths {
        rects.push(match axis {
            Axis::Vertical => Rect {
                x: area.x,
                y: cursor,
                width: area.width,
                height: length,
            },
            Axis::Horizontal => Rect {
                x: cursor,
                y: area.y,
                width: length,
                height: area.height,
            },
        });
        cursor = cursor.saturating_add(length).saturating_add(gap);
    }
    rects
}

/// Read an arrangement returned by `ui/layout.lua`.
///
/// Shape mirrors the view tree deliberately, so a plugin author learns one set
/// of sizing rules rather than two:
///
/// ```lua
/// return function(ctx)
///   if ctx.width < 80 then return { slot = "center" } end
///   return { axis = "horizontal", children = {
///       { slot = "sessions", pct = 30, min = 24, max = 48 },
///       { slot = "center" },
///   } }
/// end
/// ```
pub fn region_from_lua(value: &Value, path: &str) -> Result<Region, String> {
    match value {
        // A bare string names a slot — the terse form for a one-slot screen.
        Value::String(name) => Ok(Region {
            slot: Some(name.to_string_lossy()),
            ..Region::default()
        }),
        Value::Table(table) => region_from_table(table, path),
        other => Err(format!(
            "{path}: expected an arrangement table, found {}",
            match other {
                Value::Nil => "nil",
                Value::Function(_) => "a function",
                _ => "an unsupported value",
            }
        )),
    }
}

fn region_from_table(table: &Table, path: &str) -> Result<Region, String> {
    let slot: Option<String> = match table.get::<Value>("slot") {
        Ok(Value::String(s)) => Some(s.to_string_lossy()),
        Ok(Value::Nil) => None,
        Ok(_) => return Err(format!("{path}.slot: expected a string")),
        Err(e) => return Err(format!("{path}.slot: {e}")),
    };

    let axis = match table.get::<Value>("axis") {
        Ok(Value::String(s)) => match s.to_string_lossy().as_str() {
            "horizontal" | "row" | "h" => Axis::Horizontal,
            "vertical" | "column" | "v" => Axis::Vertical,
            other => {
                return Err(format!(
                    "{path}.axis: expected \"vertical\" or \"horizontal\", found {other:?}"
                ))
            }
        },
        _ => Axis::Vertical,
    };

    let size = Size {
        len: opt_u16(table, "len", path)?,
        pct: opt_f64(table, "pct", path)?,
        fill: opt_f64(table, "fill", path)?,
        min: opt_u16(table, "min", path)?,
        max: opt_u16(table, "max", path)?,
    };
    let gap = opt_u16(table, "gap", path)?.unwrap_or(0);

    // Children live under `children`, or in the array part.
    let list = match table.get::<Value>("children") {
        Ok(Value::Table(t)) => Some(t),
        Ok(Value::Nil) => {
            if slot.is_some() {
                None
            } else {
                Some(table.clone())
            }
        }
        Ok(_) => return Err(format!("{path}.children: expected a table")),
        Err(e) => return Err(format!("{path}.children: {e}")),
    };

    let mut children = Vec::new();
    if let Some(list) = list {
        for (index, entry) in list.sequence_values::<Value>().enumerate() {
            let entry = entry.map_err(|e| format!("{path}.children: {e}"))?;
            children.push(region_from_lua(&entry, &format!("{path}[{}]", index + 1))?);
        }
    }

    Ok(Region {
        slot,
        axis,
        gap,
        children,
        size,
    })
}

fn opt_u16(table: &Table, key: &str, path: &str) -> Result<Option<u16>, String> {
    match opt_f64(table, key, path)? {
        None => Ok(None),
        Some(n) if n.is_finite() && n >= 0.0 => Ok(Some(n.min(f64::from(u16::MAX)) as u16)),
        Some(n) => Err(format!(
            "{path}.{key}: expected a non-negative number, found {n}"
        )),
    }
}

fn opt_f64(table: &Table, key: &str, path: &str) -> Result<Option<f64>, String> {
    match table.get::<Value>(key) {
        Ok(Value::Nil) => Ok(None),
        Ok(Value::Integer(n)) => Ok(Some(n as f64)),
        Ok(Value::Number(n)) => Ok(Some(n)),
        Ok(_) => Err(format!("{path}.{key}: expected a number")),
        Err(e) => Err(format!("{path}.{key}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    fn region_of(source: &str) -> Region {
        let lua = Lua::new();
        let value: Value = lua
            .load(format!("return {source}"))
            .eval()
            .expect("arrangement should evaluate");
        region_from_lua(&value, "layout").expect("arrangement should convert")
    }

    fn area(width: u16, height: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    fn rect_of<'a>(placed: &'a [SlotRect], slot: &str) -> Option<&'a Rect> {
        placed.iter().find(|p| p.slot == slot).map(|p| &p.rect)
    }

    #[test]
    fn a_bare_string_names_a_slot() {
        let placed = resolve(&region_of("\"center\""), area(80, 24));
        assert_eq!(rect_of(&placed, "center"), Some(&area(80, 24)));
    }

    #[test]
    fn two_columns_split_the_width() {
        let region = region_of(
            "{ axis = \"horizontal\", children = { { slot = \"sessions\", pct = 25 }, { slot = \"center\" } } }",
        );
        let placed = resolve(&region, area(100, 30));
        assert_eq!(rect_of(&placed, "sessions").unwrap().width, 25);
        assert_eq!(rect_of(&placed, "center").unwrap().width, 75);
        // Columns must abut, leaving no unpainted gutter.
        assert_eq!(rect_of(&placed, "center").unwrap().x, 25);
    }

    #[test]
    fn min_and_max_bound_a_percentage_column() {
        let region = region_of(
            "{ axis = \"horizontal\", children = { { slot = \"sessions\", pct = 25, min = 30 }, { slot = \"center\" } } }",
        );
        let placed = resolve(&region, area(80, 24));
        assert_eq!(rect_of(&placed, "sessions").unwrap().width, 30);
    }

    #[test]
    fn a_slot_named_nowhere_gets_no_rect() {
        // This is how a responsive arrangement hides a column: it simply stops
        // naming the slot below a threshold.
        let placed = resolve(&region_of("{ slot = \"center\" }"), area(60, 20));
        assert!(rect_of(&placed, "sessions").is_none());
        assert!(rect_of(&placed, "center").is_some());
    }

    #[test]
    fn a_zero_sized_area_places_nothing() {
        let placed = resolve(&region_of("\"center\""), area(0, 24));
        assert!(placed.is_empty());
    }

    #[test]
    fn vertical_rows_stack_without_overlapping() {
        let region =
            region_of("{ children = { { slot = \"body\" }, { slot = \"footer\", len = 1 } } }");
        let placed = resolve(&region, area(80, 24));
        let body = rect_of(&placed, "body").unwrap();
        let footer = rect_of(&placed, "footer").unwrap();
        assert_eq!(body.height, 23);
        assert_eq!(footer.y, 23);
        assert_eq!(footer.height, 1);
    }

    #[test]
    fn the_array_part_is_read_as_children() {
        let region = region_of("{ { slot = \"a\", len = 5 }, { slot = \"b\" } }");
        let placed = resolve(&region, area(20, 10));
        assert_eq!(rect_of(&placed, "a").unwrap().height, 5);
        assert_eq!(rect_of(&placed, "b").unwrap().height, 5);
    }

    #[test]
    fn a_repeated_slot_keeps_its_first_placement() {
        let region = region_of("{ { slot = \"a\", len = 4 }, { slot = \"a\" } }");
        let placed = resolve(&region, area(20, 10));
        assert_eq!(placed.iter().filter(|p| p.slot == "a").count(), 1);
        assert_eq!(rect_of(&placed, "a").unwrap().height, 4);
    }

    #[test]
    fn a_slot_is_divided_among_its_occupants_by_declared_size() {
        // The session list slot holding a list plus a one-line summary.
        let sizes = [
            Size::default(),
            Size {
                len: Some(1),
                ..Size::default()
            },
        ];
        let rects = divide_slot(area(30, 20), Axis::Vertical, &sizes, 0);
        assert_eq!(rects[0].height, 19);
        assert_eq!(rects[1].height, 1);
        assert_eq!(rects[1].y, 19);
    }

    #[test]
    fn a_bad_axis_is_reported_with_its_path() {
        let lua = Lua::new();
        let value: Value = lua
            .load("return { axis = \"diagonal\", children = {} }")
            .eval()
            .unwrap();
        let error = region_from_lua(&value, "layout").unwrap_err();
        assert!(error.contains("layout.axis"), "{error}");
    }

    #[test]
    fn the_narrow_arrangement_drops_the_side_column() {
        // The behaviour v1 hardcoded in compute_layout, now a Lua branch.
        let lua = Lua::new();
        let arrange: mlua::Function = lua
            .load(
                r#"
                return function(ctx)
                  if ctx.width < 80 then return { slot = "center" } end
                  return { axis = "horizontal", children = {
                      { slot = "sessions", pct = 30 },
                      { slot = "center" },
                  } }
                end
                "#,
            )
            .eval()
            .expect("layout function evaluates");

        for (width, expect_sessions) in [(60u16, false), (120u16, true)] {
            let ctx = lua.create_table().unwrap();
            ctx.set("width", width).unwrap();
            let value: Value = arrange.call(ctx).expect("arrangement call");
            let region = region_from_lua(&value, "layout").unwrap();
            let placed = resolve(&region, area(width, 24));
            assert_eq!(
                rect_of(&placed, "sessions").is_some(),
                expect_sessions,
                "width {width}"
            );
        }
    }
}
