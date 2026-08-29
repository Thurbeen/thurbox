//! TOON (Token-Oriented Object Notation) encoder — the wire format
//! `thurbox-cli` speaks when its output is going to an agent.
//!
//! TOON encodes the JSON data model line-by-line: arrays declare their length
//! and field list once instead of repeating every key on every row, objects use
//! indentation instead of braces, and strings are quoted only when they would
//! otherwise be ambiguous. For the list-shaped answers this CLI returns — a
//! table of sessions, an inbox, a run history — that is roughly 40% fewer
//! tokens than the equivalent JSON, which is why AXI (`axi/1.0-2026-07`,
//! principle 1) asks for it.
//!
//! ```text
//! sessions[2]{name,agent,status}:
//!   flow,claude,working
//!   worker-1,codex,idle
//! ```
//!
//! This implements the **encoder** half of TOON v4.1
//! (<https://github.com/toon-format/spec>), section numbers below refer to that
//! document. There is no decoder: nothing in thurbox reads TOON back.
//!
//! Two deliberate narrowings of the spec's encoder options:
//!
//! - The delimiter is a comma and the indent is two spaces. Both are spec
//!   *options*; tab and pipe exist for data that is full of commas, which
//!   thurbox's is not. [`encode_with`] still threads them, so the conformance
//!   tests can drive the delimiter fixtures.
//! - Field order is the order [`serde_json::Map`] yields, which without the
//!   `preserve_order` feature is alphabetical. §2 asks for "encounter order as
//!   seen by the encoder" and that *is* the encoder's encounter order, so the
//!   output conforms; it simply is not the order the key was written in.

use serde_json::{Map, Value};

/// Encode a JSON value as TOON with the document defaults: comma delimiter,
/// two-space indent.
pub fn encode(value: &Value) -> String {
    encode_with(value, ',', 2)
}

/// Encoder core, parameterized on the document delimiter and indent width so
/// the spec's delimiter and indent fixtures can be exercised (§11, §12).
pub fn encode_with(value: &Value, delim: char, indent: usize) -> String {
    let enc = Encoder { delim, indent };
    let mut out = Vec::new();
    enc.root(value, &mut out);
    out.join("\n")
}

struct Encoder {
    delim: char,
    indent: usize,
}

impl Encoder {
    fn pad(&self, depth: usize) -> String {
        " ".repeat(depth * self.indent)
    }

    /// The header's delimiter marker: omitted for the default comma, otherwise
    /// the character itself inside the brackets and braces (§11).
    fn marker(&self) -> String {
        if self.delim == ',' {
            String::new()
        } else {
            self.delim.to_string()
        }
    }

    /// Root form discovery (§5). An empty object is an empty document; a bare
    /// primitive is its own token; everything else delegates.
    fn root(&self, value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) if map.is_empty() => {}
            Value::Object(map) => {
                // A root object of uniform objects takes the keyless keyed
                // tabular header (§9.5).
                match keyed_fields(map) {
                    Some(fields) => {
                        out.push(format!(
                            "[{}:{}]{{{}}}:",
                            map.len(),
                            self.marker(),
                            self.field_list(&fields)
                        ));
                        self.entry_rows(map, &fields, 1, out);
                    }
                    None => self.object_body(map, 0, out),
                }
            }
            Value::Array(items) if items.is_empty() => out.push("[]".to_string()),
            Value::Array(items) => self.array_body(None, items, 0, out),
            prim => out.push(self.primitive(prim)),
        }
    }

    /// One object's fields, each on its own line at `depth` (§8).
    fn object_body(&self, map: &Map<String, Value>, depth: usize, out: &mut Vec<String>) {
        for (key, value) in map {
            self.field(key, value, depth, out);
        }
    }

    /// A single `key: value` field and whatever scope it opens.
    fn field(&self, key: &str, value: &Value, depth: usize, out: &mut Vec<String>) {
        let pad = self.pad(depth);
        let key = encode_key(key);
        match value {
            Value::Array(items) if items.is_empty() => {
                // §9.1: an empty array in field position is `key: []`, never a
                // zero-length header.
                out.push(format!("{pad}{key}: []"));
            }
            Value::Array(items) => self.array_body(Some(&key), items, depth, out),
            Value::Object(map) if map.is_empty() => out.push(format!("{pad}{key}:")),
            Value::Object(map) => match keyed_fields(map) {
                Some(fields) => {
                    out.push(format!(
                        "{pad}{key}[{}:{}]{{{}}}:",
                        map.len(),
                        self.marker(),
                        self.field_list(&fields)
                    ));
                    self.entry_rows(map, &fields, depth + 1, out);
                }
                None => {
                    out.push(format!("{pad}{key}:"));
                    self.object_body(map, depth + 1, out);
                }
            },
            prim => out.push(format!("{pad}{key}: {}", self.primitive(prim))),
        }
    }

    /// A non-empty array: tabular when every element is a uniform object
    /// (§9.3), inline when every element is a primitive (§9.1), list form
    /// otherwise (§9.4). `key` is `None` at the document root.
    fn array_body(&self, key: Option<&str>, items: &[Value], depth: usize, out: &mut Vec<String>) {
        let pad = self.pad(depth);
        let key = key.unwrap_or("");
        let n = items.len();
        let marker = self.marker();

        if let Some(fields) = tabular_fields(items) {
            out.push(format!(
                "{pad}{key}[{n}{marker}]{{{}}}:",
                self.field_list(&fields)
            ));
            for item in items {
                let mut cells = Vec::new();
                collect_cells(item, &fields, &mut cells);
                let row: Vec<String> = cells.iter().map(|c| self.cell(c)).collect();
                out.push(format!(
                    "{}{}",
                    self.pad(depth + 1),
                    row.join(&self.delim.to_string())
                ));
            }
            return;
        }

        if items.iter().all(is_primitive) {
            let values: Vec<String> = items.iter().map(|v| self.cell(v)).collect();
            out.push(format!(
                "{pad}{key}[{n}{marker}]: {}",
                values.join(&self.delim.to_string())
            ));
            return;
        }

        out.push(format!("{pad}{key}[{n}{marker}]:"));
        for item in items {
            self.list_item(item, depth + 1, out);
        }
    }

    /// One element of an array in list form (§9.4, §10).
    fn list_item(&self, item: &Value, depth: usize, out: &mut Vec<String>) {
        let pad = self.pad(depth);
        match item {
            Value::Object(map) if map.is_empty() => out.push(format!("{pad}-")),
            Value::Object(map) => {
                // §10: the object's first field rides the hyphen line and any
                // scope it opens stays where it was. Emitting the whole object
                // one level in and then rewriting the first line's indent to
                // `- ` does exactly that — the hyphen occupies the two columns
                // the extra indent level would have, so every deeper line
                // (tabular rows at depth+2, sibling fields at depth+1) is
                // already at the depth §10 requires.
                let mut lines = Vec::new();
                self.object_body(map, depth + 1, &mut lines);
                let inner = self.pad(depth + 1);
                if let Some(first) = lines.first_mut() {
                    let body = first.strip_prefix(&inner).unwrap_or(first).to_string();
                    *first = format!("{pad}- {body}");
                }
                out.extend(lines);
            }
            Value::Array(inner) if inner.is_empty() => {
                // §9.2: a list item's empty array keeps the header form; the
                // `key: []` spelling is field-position only.
                out.push(format!("{pad}- [0{}]:", self.marker()));
            }
            Value::Array(inner) if inner.iter().all(is_primitive) => {
                let values: Vec<String> = inner.iter().map(|v| self.cell(v)).collect();
                out.push(format!(
                    "{pad}- [{}{}]: {}",
                    inner.len(),
                    self.marker(),
                    values.join(&self.delim.to_string())
                ));
            }
            Value::Array(inner) => {
                // A keyless tabular header is root-only (§6), so a nested array
                // of objects stays in list form here however uniform it is.
                out.push(format!("{pad}- [{}{}]:", inner.len(), self.marker()));
                for nested in inner {
                    self.list_item(nested, depth + 1, out);
                }
            }
            prim => out.push(format!("{pad}- {}", self.primitive(prim))),
        }
    }

    /// The entry rows of a keyed tabular object (§9.5).
    fn entry_rows(
        &self,
        map: &Map<String, Value>,
        fields: &[Field],
        depth: usize,
        out: &mut Vec<String>,
    ) {
        for (key, value) in map {
            let mut cells = Vec::new();
            collect_cells(value, fields, &mut cells);
            let row: Vec<String> = cells.iter().map(|c| self.cell(c)).collect();
            out.push(format!(
                "{}{}: {}",
                self.pad(depth),
                encode_key(key),
                row.join(&self.delim.to_string())
            ));
        }
    }

    /// Render a field list, expanding nested-uniform columns into `name{sub}`
    /// groups (§9.3).
    fn field_list(&self, fields: &[Field]) -> String {
        fields
            .iter()
            .map(|f| match f {
                Field::Leaf(name) => encode_key(name),
                Field::Group(name, sub) => {
                    format!("{}{{{}}}", encode_key(name), self.field_list(sub))
                }
            })
            .collect::<Vec<_>>()
            .join(&self.delim.to_string())
    }

    /// A primitive in object-field or root position: quoting keys off the
    /// document delimiter (§11.1).
    fn primitive(&self, value: &Value) -> String {
        scalar(value, self.delim)
    }

    /// A primitive in a row, inline array, or entry row: quoting keys off the
    /// active delimiter (§11.1). Identical here because thurbox emits one
    /// delimiter per document, but the two rules are distinct in the spec.
    fn cell(&self, value: &Value) -> String {
        scalar(value, self.delim)
    }
}

/// One column of a tabular header: a plain field, or a nested-uniform column
/// folded into a field group (§9.3).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Field {
    Leaf(String),
    Group(String, Vec<Field>),
}

fn is_primitive(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

/// Tabular detection for an array of objects (§9.3): every element an object,
/// none empty, all sharing one key set, and every column either all-primitive
/// or itself uniformly nested. `None` means the array must use list form.
fn tabular_fields(items: &[Value]) -> Option<Vec<Field>> {
    let first = items.first()?.as_object()?;
    if first.is_empty() {
        return None;
    }
    let objects: Vec<&Map<String, Value>> =
        items.iter().map(Value::as_object).collect::<Option<_>>()?;
    columns(&objects)
}

/// Keyed tabular detection for an object of objects (§9.5) — the same column
/// rules, with a floor of two entries.
fn keyed_fields(map: &Map<String, Value>) -> Option<Vec<Field>> {
    if map.len() < 2 {
        return None;
    }
    let objects: Vec<&Map<String, Value>> =
        map.values().map(Value::as_object).collect::<Option<_>>()?;
    columns(&objects)
}

/// The shared column classifier behind both tabular forms. Field order comes
/// from the first object; every object must carry the same key set.
fn columns(objects: &[&Map<String, Value>]) -> Option<Vec<Field>> {
    let first = objects.first()?;
    if first.is_empty() {
        return None;
    }
    for object in objects {
        if object.len() != first.len() || !first.keys().all(|k| object.contains_key(k)) {
            return None;
        }
    }
    let mut fields = Vec::with_capacity(first.len());
    for key in first.keys() {
        let column: Vec<&Value> = objects.iter().map(|o| &o[key]).collect();
        if column.iter().all(|v| is_primitive(v)) {
            fields.push(Field::Leaf(key.clone()));
            continue;
        }
        // A nested-uniform column: every value a non-empty object whose own
        // columns classify. Anything else — a null beside an object, an array,
        // an empty object — disqualifies the whole array.
        let nested: Vec<&Map<String, Value>> = column
            .iter()
            .map(|v| v.as_object().filter(|m| !m.is_empty()))
            .collect::<Option<_>>()?;
        fields.push(Field::Group(key.clone(), columns(&nested)?));
    }
    Some(fields)
}

/// Walk a row's field list depth-first, pre-order, collecting the leaf values
/// in header order (§9.3).
fn collect_cells<'a>(value: &'a Value, fields: &[Field], out: &mut Vec<&'a Value>) {
    let Some(map) = value.as_object() else { return };
    for field in fields {
        match field {
            Field::Leaf(name) => {
                if let Some(v) = map.get(name) {
                    out.push(v);
                }
            }
            Field::Group(name, sub) => {
                if let Some(v) = map.get(name) {
                    collect_cells(v, sub, out);
                }
            }
        }
    }
}

/// Encode one primitive token, quoting per §7.2 against `delim`.
fn scalar(value: &Value, delim: char) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => number(n),
        Value::String(s) => {
            if needs_quote(s, delim) {
                format!("\"{}\"", escape(s))
            } else {
                s.clone()
            }
        }
        // Not reachable: callers classify containers before asking for a token.
        other => other.to_string(),
    }
}

/// Canonical decimal form (§2). Rust's `f64` Display is already shortest
/// round-trip and never uses exponent notation, which is exactly the canonical
/// range's requirement; only the two ends need special handling.
fn number(n: &serde_json::Number) -> String {
    if n.is_i64() || n.is_u64() {
        return n.to_string();
    }
    let Some(x) = n.as_f64() else {
        return n.to_string();
    };
    if x == 0.0 {
        // Covers -0.0, which §2 normalizes to 0.
        return "0".to_string();
    }
    let magnitude = x.abs();
    if (1e-6..1e21).contains(&magnitude) {
        return format!("{x}");
    }
    // Outside the canonical range the spec allows JSON exponent form; it asks
    // for a lowercase `e` and an explicit sign, which Rust omits when positive.
    let formatted = format!("{x:e}");
    match formatted.split_once('e') {
        Some((mantissa, exp)) if !exp.starts_with('-') => format!("{mantissa}e+{exp}"),
        _ => formatted,
    }
}

/// §7.2 — the conditions under which a string value must be quoted.
fn needs_quote(s: &str, delim: char) -> bool {
    if s.is_empty() {
        return true;
    }
    if s.starts_with([' ', '\t']) || s.ends_with([' ', '\t']) {
        return true;
    }
    if matches!(s, "true" | "false" | "null") {
        return true;
    }
    if is_numeric_like(s) {
        return true;
    }
    if s.starts_with('-') || s.starts_with('#') {
        return true;
    }
    s.contains(delim)
        || s.chars()
            .any(|c| matches!(c, ':' | '"' | '\\' | '[' | ']' | '{' | '}') || c.is_control())
}

/// `/^[+-]?[0-9]+(?:\.[0-9]+)?(?:e[+-]?[0-9]+)?$/i` from §7.2, ASCII digits
/// only — a string that would otherwise decode back as a number.
fn is_numeric_like(s: &str) -> bool {
    let rest = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (mantissa, exponent) = match rest.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (rest, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((int, frac)) => is_ascii_digits(int) && is_ascii_digits(frac),
        None => is_ascii_digits(mantissa),
    };
    let exponent_ok = match exponent {
        Some(e) => is_ascii_digits(e.strip_prefix(['+', '-']).unwrap_or(e)),
        None => true,
    };
    mantissa_ok && exponent_ok
}

fn is_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// §7.1 — the escape table, in the order the spec matches it.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// §7.3 — a key may stay bare only if it matches
/// `^[A-Za-z_][A-Za-z0-9_.]*$`; anything else is quoted and escaped.
fn encode_key(key: &str) -> String {
    let mut chars = key.chars();
    let bare = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        }
        _ => false,
    };
    if bare {
        key.to_string()
    } else {
        format!("\"{}\"", escape(key))
    }
}

/// Encode an array of objects as one tabular block under `label`, with the
/// field order given rather than the order [`serde_json::Map`] happens to
/// yield.
///
/// [`encode`] cannot do this: it reads its field order out of the map, and
/// `serde_json`'s map is a `BTreeMap`, so a session row would head
/// `{agent,branch,name,status}` when the column that identifies it belongs
/// first. This is also where a list view drops to its 3–4 useful fields (AXI
/// principle 2) — a field a row does not carry encodes as `null`, keeping the
/// row's cell count equal to the header's as §9.3 requires.
///
/// `rows` that are not objects are skipped; an empty result is `label: []`
/// per §9.1, and callers pair that with their own zero-result line.
pub fn encode_table(label: &str, fields: &[&str], rows: &[Value]) -> String {
    let objects: Vec<&Map<String, Value>> = rows.iter().filter_map(Value::as_object).collect();
    if objects.is_empty() || fields.is_empty() {
        return format!("{}: []", encode_key(label));
    }
    let header = fields
        .iter()
        .map(|f| encode_key(f))
        .collect::<Vec<_>>()
        .join(",");
    let mut lines = vec![format!(
        "{}[{}]{{{header}}}:",
        encode_key(label),
        objects.len()
    )];
    for object in objects {
        let cells: Vec<String> = fields
            .iter()
            .map(|f| scalar(object.get(*f).unwrap_or(&Value::Null), ','))
            .collect();
        lines.push(format!("  {}", cells.join(",")));
    }
    lines.join("\n")
}
