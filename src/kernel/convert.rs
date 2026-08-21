//! Lua table → [`Node`].
//!
//! A plugin returns a plain table; nothing here hands it a ratatui object.
//! That indirection is what makes throwing the VM away on reload safe, and it
//! is why a malformed tree is a *reported* error rather than a crash: every
//! failure below returns `Err(String)` naming the path where it happened, so
//! the host can render it in the offending plugin's own region.

use mlua::{Table, Value};
use ratatui::style::Style;

use super::node::{
    parse_color, Align, Axis, Borders, Frame, Identity, Node, Run, Size, SurfaceSource,
};

/// Where in the tree an error happened, built as a borrowed chain rather than a
/// string.
///
/// Every descent used to allocate its child's path eagerly —
/// `&format!("{path}[{}]", index + 1)` — one `String` per node, growing with
/// depth, on a path that is only ever *read* when something is malformed. It
/// was ~15% of conversion for text nobody sees. A chain costs nothing to build
/// and renders itself only when a message is finally formatted.
#[derive(Clone, Copy)]
enum Crumb<'a> {
    Root,
    Field(&'a Crumb<'a>, &'static str),
    Index(&'a Crumb<'a>, usize),
}

impl std::fmt::Display for Crumb<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Crumb::Root => f.write_str("root"),
            Crumb::Field(parent, name) => write!(f, "{parent}.{name}"),
            Crumb::Index(parent, index) => write!(f, "{parent}[{index}]"),
        }
    }
}

/// Deepest tree accepted. Guards against a plugin building a cyclic or
/// pathological structure and taking the render thread down with it.
const MAX_DEPTH: usize = 64;

/// Convert a plugin's returned value into a node tree.
/// Read a plugin's returned tree.
///
/// `owner` is the path of the plugin that produced it, needed because a
/// `surface` naming a **program** names one of that plugin's own — the plugin
/// writes `program = "watch"` and the kernel resolves the owner, so naming another
/// plugin's pane is impossible by construction rather than refused by a check.
pub fn to_node(value: &Value, owner: &str) -> Result<Node, String> {
    convert(value, Crumb::Root, 0, owner)
}

fn convert(value: &Value, path: Crumb<'_>, depth: usize, owner: &str) -> Result<Node, String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "{path}: tree nested deeper than {MAX_DEPTH} levels"
        ));
    }
    match value {
        // A bare string is the shorthand every plugin reaches for first.
        Value::String(s) => Ok(Node::line(s.to_string_lossy(), Style::default())),
        Value::Nil => Ok(Node::empty()),
        Value::Table(table) => convert_table(table, path, depth, owner),
        other => Err(format!(
            "{path}: expected a table or a string, found {}",
            type_name(other)
        )),
    }
}

/// Every field a node may carry, gathered in ONE pass over the table.
///
/// Read individually this cost ~25 keyed lookups per node — `read_size` alone
/// is five — and each one hashes its key and crosses into the VM. At ~570ns a
/// lookup that was ~14µs *per node*, and the session list builds 187 of them:
/// conversion outweighed running every plugin's Lua put together, and was the
/// single largest cost in a frame. One `pairs` pass visits only the handful of
/// keys a node actually has.
///
/// This is the same fix, for the same reason, that [`read_style_field`] already
/// applies one level down — see its comment.
///
/// Keys are read **raw**, as all but two of the individual lookups already
/// were; a node table is data a plugin wrote, never a view onto something else.
#[derive(Default)]
struct Fields {
    kind: Option<Value>,
    len: Option<Value>,
    pct: Option<Value>,
    fill: Option<Value>,
    min: Option<Value>,
    max: Option<Value>,
    class: Option<Value>,
    id: Option<Value>,
    role: Option<Value>,
    frame: Option<Value>,
    block: Option<Value>,
    text: Option<Value>,
    align: Option<Value>,
    wrap: Option<Value>,
    scroll: Option<Value>,
    axis: Option<Value>,
    gap: Option<Value>,
    children: Option<Value>,
    value: Option<Value>,
    cursor: Option<Value>,
    placeholder: Option<Value>,
    focused: Option<Value>,
    style: Option<Value>,
    session: Option<Value>,
    program: Option<Value>,
    cells: Option<Value>,
}

impl Fields {
    fn read(table: &Table, path: Crumb<'_>) -> Result<Self, String> {
        let mut out = Self::default();
        for pair in table.pairs::<Value, Value>() {
            let (key, value) = pair.map_err(|e| format!("{path}: {e}"))?;
            // Integer keys are the array part — the children of a box, read in
            // order below rather than here, where `pairs` promises no order.
            let Value::String(key) = key else { continue };
            let slot = match key.as_bytes().as_ref() {
                b"type" => &mut out.kind,
                b"len" => &mut out.len,
                b"pct" => &mut out.pct,
                b"fill" => &mut out.fill,
                b"min" => &mut out.min,
                b"max" => &mut out.max,
                b"class" => &mut out.class,
                b"id" => &mut out.id,
                b"role" => &mut out.role,
                b"frame" => &mut out.frame,
                b"block" => &mut out.block,
                b"text" => &mut out.text,
                b"align" => &mut out.align,
                b"wrap" => &mut out.wrap,
                b"scroll" => &mut out.scroll,
                b"axis" => &mut out.axis,
                b"gap" => &mut out.gap,
                b"children" => &mut out.children,
                b"value" => &mut out.value,
                b"cursor" => &mut out.cursor,
                b"placeholder" => &mut out.placeholder,
                b"focused" => &mut out.focused,
                b"style" => &mut out.style,
                b"session" => &mut out.session,
                b"program" => &mut out.program,
                b"cells" => &mut out.cells,
                // A key this kernel does not know is not an error: it is how a
                // plugin carries its own bookkeeping on the same table.
                _ => continue,
            };
            *slot = Some(value);
        }
        Ok(out)
    }
}

fn convert_table(
    table: &Table,
    path: Crumb<'_>,
    depth: usize,
    owner: &str,
) -> Result<Node, String> {
    let fields = Fields::read(table, path)?;
    let size = read_size(&fields, path)?;
    let identity = read_identity(&fields, path)?;
    let frame = read_frame(&fields, path)?;

    let declared: Option<String> = val_string(fields.kind.as_ref(), "type", path)?;
    let kind = match declared {
        Some(name) => normalize_kind(&name).ok_or_else(|| {
            format!(
                "{path}: unknown node kind {name:?} — the vocabulary is text, box, input, surface"
            )
        })?,
        // Inference keeps simple plugins terse: the presence of a field
        // implies the kind, and a table of children is a box.
        None => {
            if fields.cells.is_some() || fields.session.is_some() {
                "surface"
            } else if fields.value.is_some() || fields.placeholder.is_some() {
                "input"
            } else if fields.text.is_some() {
                "text"
            } else {
                "box"
            }
        }
    };

    match kind {
        "text" => Ok(Node::Text {
            lines: read_lines(
                fields.text.as_ref().unwrap_or(&Value::Nil),
                Crumb::Field(&path, "text"),
                depth + 1,
            )?,
            align: read_align(&fields, path)?,
            wrap: val_bool(fields.wrap.as_ref(), "wrap", path)?.unwrap_or(false),
            scroll: val_u16(fields.scroll.as_ref(), "scroll", path)?.unwrap_or(0),
            frame,
            size,
            identity,
        }),
        "box" => {
            let axis = match val_string(fields.axis.as_ref(), "axis", path)?.as_deref() {
                Some("horizontal") | Some("row") | Some("h") => Axis::Horizontal,
                Some("vertical") | Some("column") | Some("v") | None => Axis::Vertical,
                Some(other) => {
                    return Err(format!(
                        "{path}: axis must be \"vertical\" or \"horizontal\", found {other:?}"
                    ))
                }
            };
            Ok(Node::Box {
                axis,
                gap: val_u16(fields.gap.as_ref(), "gap", path)?.unwrap_or(0),
                children: read_children(table, &fields, path, depth, owner)?,
                frame,
                size,
                identity,
            })
        }
        "input" => Ok(Node::Input {
            value: val_string(fields.value.as_ref(), "value", path)?.unwrap_or_default(),
            cursor: val_u16(fields.cursor.as_ref(), "cursor", path)?.unwrap_or(0) as usize,
            placeholder: val_string(fields.placeholder.as_ref(), "placeholder", path)?
                .unwrap_or_default(),
            // Absent means "not the one being typed into". A pane that draws one
            // field and never says so gets no caret, which is visible at once —
            // the alternative default hands the caret to every field on screen
            // and the last one painted wins, which is not.
            focused: val_bool(fields.focused.as_ref(), "focused", path)?.unwrap_or(false),
            style: read_style_value(fields.style.as_ref(), "style", path)?,
            frame,
            size,
            identity,
        }),
        "surface" => {
            let source = if let Some(session) =
                val_string(fields.session.as_ref(), "session", path)?
            {
                SurfaceSource::Session(session)
            } else if let Some(program) = val_string(fields.program.as_ref(), "program", path)? {
                // Resolved against the plugin that drew it. The name is checked
                // here so a bad one is a conversion error naming its path, rather
                // than a pane that never appears.
                super::terminal::validate_program_name(&program)
                    .map_err(|e| format!("{path}.program: {e}"))?;
                SurfaceSource::Program(
                    super::terminal::ProgramKey::new(owner, &program).surface_id(),
                )
            } else {
                SurfaceSource::Cells(read_lines(
                    fields.cells.as_ref().unwrap_or(&Value::Nil),
                    Crumb::Field(&path, "cells"),
                    depth + 1,
                )?)
            };
            Ok(Node::Surface {
                source,
                scroll: val_u16(fields.scroll.as_ref(), "scroll", path)?.unwrap_or(0),
                frame,
                size,
                identity,
            })
        }
        _ => unreachable!("kind was validated above"),
    }
}

/// Accept the primitive names plus the aliases a plugin author will reach for.
fn normalize_kind(name: &str) -> Option<&'static str> {
    match name {
        "text" | "paragraph" | "line" => Some("text"),
        "box" | "vstack" | "hstack" | "column" | "row" | "stack" => Some("box"),
        "input" | "field" => Some("input"),
        "surface" | "terminal" => Some("surface"),
        _ => None,
    }
}

fn read_children(
    table: &Table,
    fields: &Fields,
    path: Crumb<'_>,
    depth: usize,
    owner: &str,
) -> Result<Vec<Node>, String> {
    // Children live under `children`, or in the array part of the table so a
    // plugin can write `{ a, b, c }` without the extra key. Taken from the
    // single field pass, so a box costs no lookup of its own.
    let list = match fields.children.clone().unwrap_or(Value::Nil) {
        Value::Table(t) => t,
        Value::Nil => table.clone(),
        other => {
            return Err(format!(
                "{path}.children: expected a table, found {}",
                type_name(&other)
            ))
        }
    };

    let mut children = Vec::new();
    for (index, entry) in list.sequence_values::<Value>().enumerate() {
        let entry = entry.map_err(|e| lua_err(path, "children", &e))?;
        children.push(convert(
            &entry,
            Crumb::Index(&path, index + 1),
            depth + 1,
            owner,
        )?);
    }
    Ok(children)
}

/// Read text in any of the shapes a plugin may write it:
/// a string, a list of lines, a `{ text =, style = }` table, or a list of runs.
///
/// `depth` is threaded for the same reason [`convert`] threads it: a table that
/// contains itself is a legal Lua value, and text is a place a plugin can hand
/// one over.
fn read_lines(value: &Value, path: Crumb<'_>, depth: usize) -> Result<Vec<Vec<Run>>, String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "{path}: tree nested deeper than {MAX_DEPTH} levels"
        ));
    }
    match value {
        Value::Nil => Ok(Vec::new()),
        Value::String(s) => Ok(vec![vec![Run {
            text: s.to_string_lossy(),
            style: Style::default(),
        }]]),
        Value::Integer(_) | Value::Number(_) | Value::Boolean(_) => Ok(vec![vec![Run {
            text: scalar_to_string(value),
            style: Style::default(),
        }]]),
        Value::Table(table) => {
            // `{ text = ..., style = ... }` is one line, not a list of lines.
            // One raw read answers it; asking `contains_key` first was a second
            // crossing into the VM for the same question, per line per frame.
            if !matches!(table.raw_get::<Value>("text"), Ok(Value::Nil) | Err(_)) {
                return Ok(vec![read_runs(value, path, depth + 1)?]);
            }
            let mut lines = Vec::new();
            for (index, entry) in table.sequence_values::<Value>().enumerate() {
                let entry = entry.map_err(|e| lua_err(path, "line", &e))?;
                lines.push(read_runs(
                    &entry,
                    Crumb::Index(&path, index + 1),
                    depth + 1,
                )?);
            }
            Ok(lines)
        }
        other => Err(format!("{path}: expected text, found {}", type_name(other))),
    }
}

/// One line: a string, a `{ text =, style = }` table, or a list of those.
fn read_runs(value: &Value, path: Crumb<'_>, depth: usize) -> Result<Vec<Run>, String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "{path}: tree nested deeper than {MAX_DEPTH} levels"
        ));
    }
    match value {
        Value::String(s) => Ok(vec![Run {
            text: s.to_string_lossy(),
            style: Style::default(),
        }]),
        Value::Integer(_) | Value::Number(_) | Value::Boolean(_) => Ok(vec![Run {
            text: scalar_to_string(value),
            style: Style::default(),
        }]),
        Value::Nil => Ok(Vec::new()),
        Value::Table(table) => {
            // Read once and branch on what came back, rather than asking
            // `contains_key` and then reading it again — this is the hottest
            // leaf in the renderer, one call per span per plugin per frame.
            let raw: Value = table
                .raw_get("text")
                .map_err(|e| lua_err(path, "text", &e))?;
            if !matches!(raw, Value::Nil) {
                let text = match raw {
                    Value::String(s) => s.to_string_lossy(),
                    ref other => scalar_to_string(other),
                };
                return Ok(vec![Run {
                    text,
                    style: read_style_field(table, "style", path)?,
                }]);
            }
            let mut runs = Vec::new();
            for (index, entry) in table.sequence_values::<Value>().enumerate() {
                let entry = entry.map_err(|e| lua_err(path, "span", &e))?;
                runs.extend(read_runs(
                    &entry,
                    Crumb::Index(&path, index + 1),
                    depth + 1,
                )?);
            }
            Ok(runs)
        }
        other => Err(format!(
            "{path}: expected a line, found {}",
            type_name(other)
        )),
    }
}

fn read_size(fields: &Fields, path: Crumb<'_>) -> Result<Size, String> {
    Ok(Size {
        len: val_u16(fields.len.as_ref(), "len", path)?,
        pct: val_f64(fields.pct.as_ref(), "pct", path)?,
        fill: val_f64(fields.fill.as_ref(), "fill", path)?,
        min: val_u16(fields.min.as_ref(), "min", path)?,
        max: val_u16(fields.max.as_ref(), "max", path)?,
    })
}

fn read_identity(fields: &Fields, path: Crumb<'_>) -> Result<Identity, String> {
    // `class` accepts one name or a list, mirroring how it reads in markup.
    let classes = match fields.class.clone().unwrap_or(Value::Nil) {
        Value::Nil => Vec::new(),
        Value::String(s) => s
            .to_string_lossy()
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        Value::Table(list) => {
            let mut out = Vec::new();
            for entry in list.sequence_values::<String>() {
                out.push(entry.map_err(|e| lua_err(path, "class", &e))?);
            }
            out
        }
        ref other => {
            return Err(format!(
                "{path}.class: expected a string or list, found {}",
                type_name(other)
            ))
        }
    };
    Ok(Identity {
        id: val_string(fields.id.as_ref(), "id", path)?,
        classes,
        role: val_string(fields.role.as_ref(), "role", path)?,
    })
}

fn read_frame(fields: &Fields, path: Crumb<'_>) -> Result<Option<Frame>, String> {
    // `block` is what the POC called it; keep it working.
    let raw = fields
        .frame
        .clone()
        .or_else(|| fields.block.clone())
        .unwrap_or(Value::Nil);
    match raw {
        Value::Nil => Ok(None),
        Value::Boolean(false) => Ok(None),
        Value::Boolean(true) => Ok(Some(Frame::default())),
        Value::String(title) => Ok(Some(Frame::titled(title.to_string_lossy()))),
        Value::Table(spec) => {
            let borders = match opt_string(&spec, "borders", path)?.as_deref() {
                Some("none") => Borders::None,
                Some("all") | None => Borders::All,
                Some(other) => {
                    return Err(format!(
                        "{path}.frame.borders: expected \"all\" or \"none\", found {other:?}"
                    ))
                }
            };
            // A plain string stays a plain string; a table of runs is styled, the
            // same shape a `text` node accepts.
            let title = match spec.get::<Value>("title") {
                Ok(Value::Nil) => None,
                Ok(value) => Some(read_runs(
                    &value,
                    Crumb::Field(&Crumb::Field(&path, "frame"), "title"),
                    0,
                )?),
                Err(e) => return Err(format!("{path}.frame.title: {e}")),
            };
            Ok(Some(Frame {
                title,
                borders,
                border_style: read_style_field(&spec, "border_style", path)?,
                style: read_style_field(&spec, "style", path)?,
                padding: opt_u16(&spec, "padding", path)?.unwrap_or(0),
            }))
        }
        ref other => Err(format!(
            "{path}.frame: expected a table, string or boolean, found {}",
            type_name(other)
        )),
    }
}

fn read_align(fields: &Fields, path: Crumb<'_>) -> Result<Align, String> {
    match val_string(fields.align.as_ref(), "align", path)?.as_deref() {
        Some("center") | Some("centre") => Ok(Align::Center),
        Some("right") => Ok(Align::Right),
        Some("left") | None => Ok(Align::Left),
        Some(other) => Err(format!(
            "{path}.align: expected \"left\", \"center\" or \"right\", found {other:?}"
        )),
    }
}

/// Set `fg`/`bg` on `style`, ignoring any other key.
///
/// An unparseable colour leaves the field alone rather than defaulting it: a
/// typo should lose that one colour, not repaint the span.
fn apply_color(style: &mut Style, field: &[u8], raw: &str) {
    let Some(color) = parse_color(raw) else {
        return;
    };
    match field {
        b"fg" => *style = style.fg(color),
        b"bg" => *style = style.bg(color),
        _ => {}
    }
}

/// The modifier a boolean style key turns on.
fn modifier_for(field: &[u8]) -> Option<ratatui::style::Modifier> {
    use ratatui::style::Modifier;
    Some(match field {
        b"bold" => Modifier::BOLD,
        b"dim" => Modifier::DIM,
        b"italic" => Modifier::ITALIC,
        b"underline" => Modifier::UNDERLINED,
        b"reversed" => Modifier::REVERSED,
        b"crossed-out" | b"crossed_out" => Modifier::CROSSED_OUT,
        _ => return None,
    })
}

/// Read a style, which may be a colour shorthand or a full table.
/// [`read_style_field`] against an already-gathered value.
fn read_style_value(value: Option<&Value>, key: &str, path: Crumb<'_>) -> Result<Style, String> {
    read_style_raw(value.cloned().unwrap_or(Value::Nil), key, path)
}

fn read_style_field(table: &Table, key: &str, path: Crumb<'_>) -> Result<Style, String> {
    let raw: Value = table.raw_get(key).map_err(|e| lua_err(path, key, &e))?;
    read_style_raw(raw, key, path)
}

fn read_style_raw(raw: Value, key: &str, path: Crumb<'_>) -> Result<Style, String> {
    match raw {
        Value::Nil => Ok(Style::default()),
        // `style = "cyan"` — the shorthand every plugin uses for a foreground.
        Value::String(name) => Ok(match parse_color(&name.to_string_lossy()) {
            Some(color) => Style::default().fg(color),
            None => Style::default(),
        }),
        // Applied field by field rather than collected into a map first.
        //
        // This is the hottest path in the whole renderer: it runs once per SPAN
        // per plugin per frame, and the map it used to build allocated a heap
        // `String` for every key and value. At ~100 spans a pane that was
        // thousands of allocations a frame, and it dominated render time — the
        // session list cost 1.2ms with zero sessions in it.
        Value::Table(spec) => {
            let mut style = Style::default();
            for pair in spec.pairs::<mlua::LuaString, Value>() {
                let (field, value) = pair.map_err(|e| lua_err(path, key, &e))?;
                let field = field.as_bytes();
                match value {
                    Value::String(s) => {
                        apply_color(&mut style, &field, &s.to_string_lossy());
                    }
                    Value::Integer(n) => {
                        apply_color(&mut style, &field, &n.to_string());
                    }
                    Value::Boolean(true) => {
                        if let Some(modifier) = modifier_for(&field) {
                            style = style.add_modifier(modifier);
                        }
                    }
                    _ => {}
                }
            }
            Ok(style)
        }
        ref other => Err(format!(
            "{path}.{key}: expected a style string or table, found {}",
            type_name(other)
        )),
    }
}

fn opt_string(table: &Table, key: &str, path: Crumb<'_>) -> Result<Option<String>, String> {
    match table
        .raw_get::<Value>(key)
        .map_err(|e| lua_err(path, key, &e))?
    {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s.to_string_lossy())),
        Value::Integer(n) => Ok(Some(n.to_string())),
        ref other => Err(format!(
            "{path}.{key}: expected a string, found {}",
            type_name(other)
        )),
    }
}

fn opt_u16(table: &Table, key: &str, path: Crumb<'_>) -> Result<Option<u16>, String> {
    match opt_f64(table, key, path)? {
        None => Ok(None),
        Some(n) if n.is_finite() && n >= 0.0 => Ok(Some(n.min(f64::from(u16::MAX)) as u16)),
        Some(n) => Err(format!(
            "{path}.{key}: expected a non-negative number, found {n}"
        )),
    }
}

fn opt_f64(table: &Table, key: &str, path: Crumb<'_>) -> Result<Option<f64>, String> {
    match table
        .raw_get::<Value>(key)
        .map_err(|e| lua_err(path, key, &e))?
    {
        Value::Nil => Ok(None),
        Value::Integer(n) => Ok(Some(n as f64)),
        Value::Number(n) => Ok(Some(n)),
        ref other => Err(format!(
            "{path}.{key}: expected a number, found {}",
            type_name(other)
        )),
    }
}

/// The `opt_*` coercions, against a field already gathered by [`Fields::read`]
/// rather than a fresh lookup. Same rules, same messages — only the source of
/// the value differs.
fn val_string(value: Option<&Value>, key: &str, path: Crumb<'_>) -> Result<Option<String>, String> {
    match value.unwrap_or(&Value::Nil) {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s.to_string_lossy())),
        Value::Integer(n) => Ok(Some(n.to_string())),
        other => Err(format!(
            "{path}.{key}: expected a string, found {}",
            type_name(other)
        )),
    }
}

fn val_bool(value: Option<&Value>, key: &str, path: Crumb<'_>) -> Result<Option<bool>, String> {
    match value.unwrap_or(&Value::Nil) {
        Value::Nil => Ok(None),
        Value::Boolean(b) => Ok(Some(*b)),
        other => Err(format!(
            "{path}.{key}: expected a boolean, found {}",
            type_name(other)
        )),
    }
}

fn val_u16(value: Option<&Value>, key: &str, path: Crumb<'_>) -> Result<Option<u16>, String> {
    match val_f64(value, key, path)? {
        None => Ok(None),
        Some(n) if n.is_finite() && n >= 0.0 => Ok(Some(n.min(f64::from(u16::MAX)) as u16)),
        Some(n) => Err(format!(
            "{path}.{key}: expected a non-negative number, found {n}"
        )),
    }
}

fn val_f64(value: Option<&Value>, key: &str, path: Crumb<'_>) -> Result<Option<f64>, String> {
    match value.unwrap_or(&Value::Nil) {
        Value::Nil => Ok(None),
        Value::Integer(n) => Ok(Some(*n as f64)),
        Value::Number(n) => Ok(Some(*n)),
        other => Err(format!(
            "{path}.{key}: expected a number, found {}",
            type_name(other)
        )),
    }
}

fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::Integer(n) => n.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::String(s) => s.to_string_lossy(),
        _ => String::new(),
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Nil => "nil",
        Value::Boolean(_) => "a boolean",
        Value::Integer(_) | Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Table(_) => "a table",
        Value::Function(_) => "a function",
        _ => "an unsupported value",
    }
}

fn lua_err(path: Crumb<'_>, key: &str, error: &mlua::Error) -> String {
    format!("{path}.{key}: {error}")
}

/// Convert a [`Node`] back into the Lua table a plugin would have written.
///
/// Needed only by decoration: a decorator receives another plugin's rendered
/// tree, so the tree has to cross back into Lua. Round-tripping through the
/// same shape `to_node` accepts means a decorator can return the table it was
/// given, modified or not, and it converts back cleanly.
/// A `Style` as the same table shape a plugin would have written, so the value
/// a decorator receives is indistinguishable from one it could author.
///
/// `None` when the style is empty, which keeps an unstyled span free of a
/// pointless table and keeps `to_lua` output stable for the tree diff.
fn style_to_lua(lua: &mlua::Lua, style: &Style) -> Result<Option<Table>, String> {
    use ratatui::style::Modifier;

    let table = lua.create_table().map_err(|e| e.to_string())?;
    let mut any = false;
    if let Some(fg) = style.fg {
        table
            .set("fg", crate::kernel::theme::color_to_string(&fg))
            .map_err(|e| e.to_string())?;
        any = true;
    }
    if let Some(bg) = style.bg {
        table
            .set("bg", crate::kernel::theme::color_to_string(&bg))
            .map_err(|e| e.to_string())?;
        any = true;
    }
    for (modifier, name) in [
        (Modifier::BOLD, "bold"),
        (Modifier::DIM, "dim"),
        (Modifier::ITALIC, "italic"),
        (Modifier::UNDERLINED, "underline"),
        (Modifier::REVERSED, "reversed"),
        (Modifier::CROSSED_OUT, "crossed_out"),
    ] {
        if style.add_modifier.contains(modifier) {
            table.set(name, true).map_err(|e| e.to_string())?;
            any = true;
        }
    }
    Ok(any.then_some(table))
}

pub fn to_lua(lua: &mlua::Lua, node: &Node) -> Result<Value, String> {
    let table = lua.create_table().map_err(|e| e.to_string())?;
    let set = |key: &str, value: Value| -> Result<(), String> {
        table.set(key, value).map_err(|e| e.to_string())
    };

    set("type", string_value(lua, node.kind())?)?;

    let size = node.size();
    if let Some(len) = size.len {
        table.set("len", len).map_err(|e| e.to_string())?;
    }
    if let Some(pct) = size.pct {
        table.set("pct", pct).map_err(|e| e.to_string())?;
    }
    if let Some(fill) = size.fill {
        table.set("fill", fill).map_err(|e| e.to_string())?;
    }
    // `min`/`max` round-trip for the same reason the styles below do: a decorator
    // handed the tree returns one, so a field dropped here is dropped from the
    // pane -- and an identity decorator must leave the pane exactly as it was.
    if let Some(min) = size.min {
        table.set("min", min).map_err(|e| e.to_string())?;
    }
    if let Some(max) = size.max {
        table.set("max", max).map_err(|e| e.to_string())?;
    }

    let identity = node.identity();
    if let Some(id) = &identity.id {
        set("id", string_value(lua, id)?)?;
    }
    if !identity.classes.is_empty() {
        set("class", string_value(lua, &identity.classes.join(" "))?)?;
    }
    if let Some(role) = &identity.role {
        set("role", string_value(lua, role)?)?;
    }
    if let Some(frame) = node.frame() {
        let spec = lua.create_table().map_err(|e| e.to_string())?;
        if let Some(text) = frame.title_text() {
            spec.set("title", text).map_err(|e| e.to_string())?;
        }
        spec.set(
            "borders",
            match frame.borders {
                Borders::All => "all",
                Borders::None => "none",
            },
        )
        .map_err(|e| e.to_string())?;
        spec.set("padding", frame.padding)
            .map_err(|e| e.to_string())?;
        if let Some(style) = style_to_lua(lua, &frame.border_style)? {
            spec.set("border_style", style).map_err(|e| e.to_string())?;
        }
        if let Some(style) = style_to_lua(lua, &frame.style)? {
            spec.set("style", style).map_err(|e| e.to_string())?;
        }
        table.set("frame", spec).map_err(|e| e.to_string())?;
    }

    match node {
        Node::Text {
            lines,
            align,
            wrap,
            scroll,
            ..
        } => {
            let out = lua.create_table().map_err(|e| e.to_string())?;
            for (index, runs) in lines.iter().enumerate() {
                let line = lua.create_table().map_err(|e| e.to_string())?;
                for (span, run) in runs.iter().enumerate() {
                    let entry = lua.create_table().map_err(|e| e.to_string())?;
                    entry
                        .set("text", run.text.clone())
                        .map_err(|e| e.to_string())?;
                    // The style MUST round-trip. A decorator is handed the
                    // tree and returns one, so anything dropped here is
                    // dropped from the pane -- and a decorator that returns
                    // its input untouched (the common case, when its query is
                    // empty) would silently strip every colour the pane drew.
                    // That is exactly what happened: the session list rendered
                    // colourless because search decorates its slot.
                    if let Some(style) = style_to_lua(lua, &run.style)? {
                        entry.set("style", style).map_err(|e| e.to_string())?;
                    }
                    line.set(span + 1, entry).map_err(|e| e.to_string())?;
                }
                out.set(index + 1, line).map_err(|e| e.to_string())?;
            }
            table.set("text", out).map_err(|e| e.to_string())?;
            set(
                "align",
                string_value(
                    lua,
                    match align {
                        Align::Left => "left",
                        Align::Center => "center",
                        Align::Right => "right",
                    },
                )?,
            )?;
            table.set("wrap", *wrap).map_err(|e| e.to_string())?;
            table.set("scroll", *scroll).map_err(|e| e.to_string())?;
        }
        Node::Box {
            axis,
            gap,
            children,
            ..
        } => {
            set(
                "axis",
                string_value(
                    lua,
                    match axis {
                        Axis::Vertical => "vertical",
                        Axis::Horizontal => "horizontal",
                    },
                )?,
            )?;
            table.set("gap", *gap).map_err(|e| e.to_string())?;
            let out = lua.create_table().map_err(|e| e.to_string())?;
            for (index, child) in children.iter().enumerate() {
                out.set(index + 1, to_lua(lua, child)?)
                    .map_err(|e| e.to_string())?;
            }
            table.set("children", out).map_err(|e| e.to_string())?;
        }
        Node::Input {
            value,
            cursor,
            placeholder,
            focused,
            style,
            ..
        } => {
            table
                .set("value", value.clone())
                .map_err(|e| e.to_string())?;
            table.set("cursor", *cursor).map_err(|e| e.to_string())?;
            table
                .set("placeholder", placeholder.clone())
                .map_err(|e| e.to_string())?;
            table.set("focused", *focused).map_err(|e| e.to_string())?;
            if let Some(style) = style_to_lua(lua, style)? {
                table.set("style", style).map_err(|e| e.to_string())?;
            }
        }
        Node::Surface { source, scroll, .. } => {
            table.set("scroll", *scroll).map_err(|e| e.to_string())?;
            match source {
                SurfaceSource::Session(id) => table
                    .set("session", id.clone())
                    .map_err(|e| e.to_string())?,
                // The resolved id, not the bare name the plugin wrote: the tree a
                // decorator receives has to name the same surface the paint will.
                SurfaceSource::Program(id) => table
                    .set("program", id.clone())
                    .map_err(|e| e.to_string())?,
                SurfaceSource::Cells(lines) => {
                    // Emitted as runs rather than flattened to one string per
                    // line: a plugin-fed surface is where a review diff's syntax
                    // colouring lives, and joining the text throws all of it away
                    // the moment any decorator touches the pane.
                    let out = lua.create_table().map_err(|e| e.to_string())?;
                    for (index, runs) in lines.iter().enumerate() {
                        let line = lua.create_table().map_err(|e| e.to_string())?;
                        for (span, run) in runs.iter().enumerate() {
                            let entry = lua.create_table().map_err(|e| e.to_string())?;
                            entry
                                .set("text", run.text.clone())
                                .map_err(|e| e.to_string())?;
                            if let Some(style) = style_to_lua(lua, &run.style)? {
                                entry.set("style", style).map_err(|e| e.to_string())?;
                            }
                            line.set(span + 1, entry).map_err(|e| e.to_string())?;
                        }
                        out.set(index + 1, line).map_err(|e| e.to_string())?;
                    }
                    table.set("cells", out).map_err(|e| e.to_string())?;
                }
            }
        }
    }

    Ok(Value::Table(table))
}

fn string_value(lua: &mlua::Lua, text: &str) -> Result<Value, String> {
    lua.create_string(text)
        .map(Value::String)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    /// Evaluate a Lua expression and convert whatever it returns.
    fn node_of(source: &str) -> Result<Node, String> {
        let lua = Lua::new();
        let value: Value = lua
            .load(format!("return {source}"))
            .eval()
            .expect("test source should evaluate");
        to_node(&value, "plugins/90_test.lua")
    }

    #[test]
    fn a_bare_string_is_a_text_node() {
        let node = node_of("\"hello\"").expect("string converts");
        assert_eq!(node.kind(), "text");
    }

    #[test]
    fn kinds_are_inferred_from_the_fields_present() {
        assert_eq!(node_of("{ text = \"hi\" }").unwrap().kind(), "text");
        assert_eq!(node_of("{ children = {} }").unwrap().kind(), "box");
        assert_eq!(node_of("{ value = \"x\" }").unwrap().kind(), "input");
        assert_eq!(node_of("{ session = \"abc\" }").unwrap().kind(), "surface");
    }

    #[test]
    fn aliases_map_onto_the_four_primitives() {
        assert_eq!(node_of("{ type = \"vstack\" }").unwrap().kind(), "box");
        assert_eq!(
            node_of("{ type = \"terminal\", session = \"a\" }")
                .unwrap()
                .kind(),
            "surface"
        );
        assert_eq!(
            node_of("{ type = \"paragraph\", text = \"a\" }")
                .unwrap()
                .kind(),
            "text"
        );
    }

    #[test]
    fn an_unknown_kind_is_reported_not_rendered() {
        let error = node_of("{ type = \"gauge\", ratio = 0.5 }").unwrap_err();
        assert!(error.contains("unknown node kind"), "{error}");
        assert!(error.contains("gauge"), "{error}");
    }

    #[test]
    fn the_array_part_is_read_as_children() {
        let node = node_of("{ \"a\", \"b\" }").expect("array converts");
        match node {
            Node::Box { children, .. } => assert_eq!(children.len(), 2),
            other => panic!("expected a box, got {}", other.kind()),
        }
    }

    #[test]
    fn identity_round_trips_through_conversion() {
        let node = node_of(
            "{ text = \"x\", id = \"row-1\", class = \"session-row match\", role = \"row\" }",
        )
        .expect("identity converts");
        let identity = node.identity();
        assert_eq!(identity.id.as_deref(), Some("row-1"));
        assert_eq!(identity.classes, vec!["session-row", "match"]);
        assert_eq!(identity.role.as_deref(), Some("row"));
    }

    #[test]
    fn a_class_list_is_accepted_as_well_as_a_string() {
        let node = node_of("{ text = \"x\", class = { \"a\", \"b\" } }").unwrap();
        assert_eq!(node.identity().classes, vec!["a", "b"]);
    }

    #[test]
    fn text_accepts_lines_and_spans() {
        let node = node_of(
            "{ text = { \"plain\", { text = \"styled\", style = \"cyan\" }, { { text = \"two \" }, { text = \"spans\" } } } }",
        )
        .expect("multi-line text converts");
        match node {
            Node::Text { lines, .. } => {
                assert_eq!(lines.len(), 3);
                assert_eq!(lines[2].len(), 2, "third line should hold two spans");
            }
            other => panic!("expected text, got {}", other.kind()),
        }
    }

    #[test]
    fn a_frame_accepts_a_bare_title() {
        let node = node_of("{ text = \"x\", frame = \"Sessions\" }").unwrap();
        assert_eq!(
            node.frame().and_then(|f| f.title_text()).as_deref(),
            Some("Sessions")
        );
    }

    #[test]
    fn block_is_still_accepted_as_a_frame() {
        let node = node_of("{ text = \"x\", block = { title = \"T\", padding = 1 } }").unwrap();
        let frame = node.frame().expect("block should become a frame");
        assert_eq!(frame.title_text().as_deref(), Some("T"));
        assert_eq!(frame.padding, 1);
    }

    #[test]
    fn a_wrong_typed_field_names_its_path() {
        let error = node_of("{ text = \"x\", len = \"wide\" }").unwrap_err();
        assert!(error.contains("root.len"), "{error}");
    }

    #[test]
    fn a_negative_size_is_rejected() {
        let error = node_of("{ text = \"x\", len = -3 }").unwrap_err();
        assert!(error.contains("non-negative"), "{error}");
    }

    #[test]
    fn a_bad_child_names_its_index() {
        let error = node_of("{ children = { \"ok\", { type = \"nope\" } } }").unwrap_err();
        assert!(error.contains("root[2]"), "{error}");
    }

    #[test]
    fn deep_nesting_is_refused_rather_than_overflowing() {
        let lua = Lua::new();
        let value: Value = lua
            .load(
                r#"
                local node = { text = "leaf" }
                for _ = 1, 200 do node = { children = { node } } end
                return node
                "#,
            )
            .eval()
            .expect("deep tree builds");
        let error = to_node(&value, "plugins/90_test.lua").unwrap_err();
        assert!(error.contains("nested deeper"), "{error}");
    }

    /// A table that contains itself is a legal Lua value, and `text` reaches the
    /// run reader rather than the child reader — so it needs its own depth guard.
    /// Without one this is a stack overflow, which is an abort rather than a
    /// panic: the terminal-restoring hook never runs.
    #[test]
    fn a_cyclic_text_table_is_refused_rather_than_overflowing() {
        let lua = Lua::new();
        for source in [
            "local t = {}; t[1] = t; return { text = t }",
            "local t = {}; t[1] = t; return { type = \"surface\", cells = t }",
        ] {
            let value: Value = lua.load(source).eval().expect("cyclic table builds");
            let error = to_node(&value, "plugins/90_test.lua").unwrap_err();
            assert!(error.contains("nested deeper"), "{source}: {error}");
        }
    }

    #[test]
    fn sizes_are_read_off_the_node() {
        let node = node_of("{ text = \"x\", len = 3, min = 1, max = 9 }").unwrap();
        let size = node.size();
        assert_eq!(size.len, Some(3));
        assert_eq!(size.min, Some(1));
        assert_eq!(size.max, Some(9));
    }

    #[test]
    fn surface_cells_are_read_as_lines() {
        let node = node_of("{ type = \"surface\", cells = { \"row one\", \"row two\" } }").unwrap();
        match node {
            Node::Surface {
                source: SurfaceSource::Cells(lines),
                ..
            } => assert_eq!(lines.len(), 2),
            other => panic!("expected a cell surface, got {}", other.kind()),
        }
    }
}
