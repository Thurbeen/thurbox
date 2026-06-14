//! Output rendering for `thurbox-cli`.
//!
//! Every subcommand builds a [`CommandOutput`] carrying *both* a machine-
//! readable JSON `Value` and a pre-rendered human string. [`mod@crate::cli`]'s
//! dispatcher picks which to print based on the resolved [`Format`]:
//!
//! - `--json` forces JSON (compact); `--pretty` forces pretty-printed JSON.
//! - `--text` forces the human rendering.
//! - With no flag we auto-detect: a TTY gets human output, a pipe gets JSON, so
//!   existing `thurbox-cli … | jq` pipelines keep working untouched.
//!
//! Subcommands build the human string where they already hold the typed data
//! (no fragile re-parsing of `Value` in a central renderer). The small
//! [`table`]/[`kv`] helpers keep that rendering aligned and consistent.

use std::io::IsTerminal;

use serde_json::Value;

/// A command result carrying both a JSON `Value` (machine output) and a
/// pre-rendered human string. Optionally flags a non-zero exit (e.g. `config
/// validate` on an invalid file still prints its report, then exits 1).
#[derive(Debug)]
pub struct CommandOutput {
    /// Machine-readable representation, printed under `--json` / `--pretty` (and
    /// when stdout is not a TTY).
    pub json: Value,
    /// Human-readable representation, printed by default in a terminal.
    pub human: String,
    /// When `Some`, the dispatcher prints the output normally, then returns this
    /// message as an error so the process exits non-zero.
    pub failure: Option<String>,
}

impl CommandOutput {
    /// Build an output with an explicit human rendering.
    pub fn new(json: Value, human: impl Into<String>) -> Self {
        Self {
            json,
            human: human.into(),
            failure: None,
        }
    }

    /// Build an output whose human rendering is the JSON's `summary` string, when
    /// present (most mutating commands set one); otherwise fall back to compact
    /// JSON. Handy for create/delete/install-style confirmations.
    pub fn from_summary(json: Value) -> Self {
        let human = json
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| json.to_string());
        Self::new(json, human)
    }

    /// Build an output that prints normally but then exits non-zero with `msg`.
    pub fn failed(json: Value, human: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            json,
            human: human.into(),
            failure: Some(msg.into()),
        }
    }
}

// Deref to the inner `Value` so `run(...)` call sites in unit tests keep
// indexing/method access (`out["id"]`, `out.as_array()`) unchanged.
impl std::ops::Deref for CommandOutput {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        &self.json
    }
}

// Display the JSON form, so `format!("{out}")` in tests/diagnostics works.
impl std::fmt::Display for CommandOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.json)
    }
}

/// Which representation to print.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Human-readable text (tables / key-value blocks / confirmation lines).
    Human,
    /// Compact single-line JSON.
    Json,
    /// Indented JSON.
    JsonPretty,
}

impl Format {
    /// Resolve the effective format from the global flags, falling back to TTY
    /// auto-detection. Precedence: `--pretty` > `--json` > `--text` > auto.
    pub fn resolve(json: bool, text: bool, pretty: bool) -> Self {
        Self::resolve_with(json, text, pretty, std::io::stdout().is_terminal())
    }

    /// Resolution core, parameterized on whether stdout is a TTY (testable).
    pub fn resolve_with(json: bool, text: bool, pretty: bool, stdout_is_tty: bool) -> Self {
        if pretty {
            Format::JsonPretty
        } else if json {
            Format::Json
        } else if text || stdout_is_tty {
            // Explicit --text, or auto-detected interactive terminal.
            Format::Human
        } else {
            // Piped/redirected with no flag: stay machine-readable.
            Format::Json
        }
    }

    /// Render an output to a string in this format.
    pub fn render(self, out: &CommandOutput) -> String {
        match self {
            Format::Human => out.human.clone(),
            Format::Json => serde_json::to_string(&out.json)
                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
            Format::JsonPretty => serde_json::to_string_pretty(&out.json)
                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
        }
    }
}

/// Render an aligned, left-justified text table: a header row followed by data
/// rows, columns padded to the widest cell, two-space gutters, no trailing
/// whitespace. Returns headers only when `rows` is empty (callers usually print
/// an explicit "none" line instead).
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
    }
    let header: Vec<String> = headers.iter().map(|h| (*h).to_string()).collect();
    let mut lines = vec![fmt_row(&header, &widths)];
    for row in rows {
        lines.push(fmt_row(row, &widths));
    }
    lines.join("\n")
}

/// Format one table row: each cell but the last is right-padded to its column
/// width with a two-space gutter; the last cell is emitted bare (no trailing
/// pad).
fn fmt_row(cells: &[String], widths: &[usize]) -> String {
    let mut s = String::new();
    let last = cells.len().saturating_sub(1);
    for (i, cell) in cells.iter().enumerate() {
        if i == last {
            s.push_str(cell.trim_end());
        } else {
            let width = widths.get(i).copied().unwrap_or(0);
            let pad = width.saturating_sub(cell.chars().count());
            s.push_str(cell);
            for _ in 0..pad {
                s.push(' ');
            }
            s.push_str("  ");
        }
    }
    s.trim_end().to_string()
}

/// Render an aligned key/value block: `key:` columns padded to the widest key,
/// values following after two spaces. Multi-line values keep their newlines (no
/// re-indentation), so a trailing free-form blob renders naturally.
pub fn kv(pairs: &[(&str, String)]) -> String {
    let width = pairs
        .iter()
        .map(|(k, _)| k.chars().count() + 1) // +1 for the colon
        .max()
        .unwrap_or(0);
    pairs
        .iter()
        .map(|(k, v)| {
            let key = format!("{k}:");
            let pad = width.saturating_sub(key.chars().count());
            format!("{key}{}  {v}", " ".repeat(pad))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render an optional string, mapping `None`/empty to a muted dash.
pub fn dash(s: Option<&str>) -> String {
    match s {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_resolution_precedence() {
        // --pretty wins over everything.
        assert_eq!(
            Format::resolve_with(true, true, true, false),
            Format::JsonPretty
        );
        // --json beats --text.
        assert_eq!(Format::resolve_with(true, true, false, true), Format::Json);
        // --text forces human even when piped.
        assert_eq!(
            Format::resolve_with(false, true, false, false),
            Format::Human
        );
        // Auto: TTY = human, pipe = json.
        assert_eq!(
            Format::resolve_with(false, false, false, true),
            Format::Human
        );
        assert_eq!(
            Format::resolve_with(false, false, false, false),
            Format::Json
        );
    }

    #[test]
    fn table_aligns_columns() {
        let rendered = table(
            &["NAME", "AGENT"],
            &[
                vec!["flow".into(), "claude".into()],
                vec!["worker-long".into(), "codex".into()],
            ],
        );
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "NAME         AGENT");
        assert_eq!(lines[1], "flow         claude");
        assert_eq!(lines[2], "worker-long  codex");
    }

    #[test]
    fn kv_aligns_keys_and_keeps_multiline_values() {
        let rendered = kv(&[
            ("id", "5".into()),
            ("status", "todo".into()),
            ("description", "line1\nline2".into()),
        ]);
        assert_eq!(
            rendered,
            "id:           5\nstatus:       todo\ndescription:  line1\nline2"
        );
    }

    #[test]
    fn from_summary_uses_summary_field() {
        let out = CommandOutput::from_summary(json!({ "ok": true, "summary": "Done it" }));
        assert_eq!(out.human, "Done it");
        // Deref still exposes the JSON for callers/tests.
        assert_eq!(out["ok"], true);
    }
}
