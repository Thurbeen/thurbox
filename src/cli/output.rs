//! Output rendering for `thurbox-cli`.
//!
//! Every subcommand builds a [`CommandOutput`] carrying *both* a machine-
//! readable JSON `Value` and a pre-rendered human string. [`mod@crate::cli`]'s
//! dispatcher picks which to print based on the resolved [`Format`]:
//!
//! - `--json` forces JSON (compact); `--pretty` forces pretty-printed JSON.
//! - `--text` forces the human rendering; `--toon` forces TOON.
//! - With no flag we auto-detect: a TTY gets human output, a pipe gets
//!   [`Format::Toon`].
//!
//! **A pipe used to get JSON.** It gets TOON now because the thing on the other
//! end of that pipe is almost always an agent, and TOON says the same thing in
//! roughly 40% fewer tokens (AXI principle 1 — see [`mod@crate::cli::toon`]).
//! JSON did not go anywhere: `--json` still produces exactly the bytes it
//! always did, every field included, and every in-repo consumer passes it.
//! A pipeline that relied on the *auto* JSON needs the flag spelled out.
//!
//! Subcommands build the human string where they already hold the typed data
//! (no fragile re-parsing of `Value` in a central renderer). The small
//! [`table`]/[`kv`] helpers keep that rendering aligned and consistent, and
//! [`AgentView`] carries the handful of extra facts the TOON rendering needs
//! that the JSON alone cannot supply — what to call the list, which of its
//! fields are worth an agent's context, and what to do next.

use std::io::IsTerminal;

use serde_json::Value;

/// A command result carrying both a JSON `Value` (machine output) and a
/// pre-rendered human string. Optionally flags a non-zero exit (e.g. `config
/// validate` on an invalid file still prints its report, then exits 1).
#[derive(Debug)]
pub struct CommandOutput {
    /// Machine-readable representation, printed under `--json` / `--pretty`.
    pub json: Value,
    /// Human-readable representation, printed by default in a terminal.
    pub human: String,
    /// When `Some`, the dispatcher prints the output normally, then returns this
    /// message as an error so the process exits non-zero.
    pub failure: Option<String>,
    /// The process exit code this output asks for, when [`failure`](Self::failure)
    /// is set. `None` means the generic "it ran and failed" code.
    ///
    /// Only one command sets it — `session exec --exit-passthrough`, whose whole
    /// purpose is to make the in-session command's own code the invocation's.
    /// It is opt-in precisely because thurbox's codes are a contract (0 ok,
    /// 1 failed, 2 usage) and a command exiting 2 would otherwise be
    /// indistinguishable from a usage error.
    pub exit_code: Option<i32>,
    /// How to render [`Format::Toon`] — the agent-facing view.
    pub agent: AgentView,
}

/// The facts the TOON rendering needs that [`CommandOutput::json`] cannot
/// carry: what the top-level collection is called, which of its fields earn
/// their tokens, what to say when there are none, and where to go next.
///
/// Every field is optional and the default is honest: an output that declares
/// nothing renders as the plain TOON of its JSON, which is already the win.
/// Declaring the rest is worth it on the commands agents actually run in a
/// loop.
#[derive(Debug, Default)]
pub struct AgentView {
    /// Name for a top-level array, e.g. `sessions` in `sessions[6]{…}:`.
    pub label: Option<String>,
    /// The columns a list view shows by default. Empty keeps every field.
    pub fields: Vec<String>,
    /// Concrete next-step commands (AXI principle 9), rendered as `help[N]:`.
    pub help: Vec<String>,
    /// What a zero-result answer says, naming what was searched (AXI principle
    /// 5). Without it an empty list is a bare `[]`, which an agent cannot tell
    /// apart from a command that failed quietly.
    pub empty: Option<String>,
    /// Cap on any single string in the TOON body, in characters (AXI principle
    /// 3). `None` — the default — means no cap.
    ///
    /// This is set per command rather than globally, because most fields here
    /// are bounded by what they are: a name, a branch, a UUID. It belongs on
    /// the few that can run away — a captured pane, a task description, a
    /// message body — where a single field can otherwise cost more context
    /// than every other answer of the session put together. `--json` is never
    /// capped: a script asking for the record wants the record.
    pub max_text: Option<usize>,
}

impl CommandOutput {
    /// Build an output with an explicit human rendering.
    pub fn new(json: Value, human: impl Into<String>) -> Self {
        Self {
            json,
            human: human.into(),
            failure: None,
            exit_code: None,
            agent: AgentView::default(),
        }
    }

    /// Name the top-level collection and pick the fields its TOON table shows.
    /// Chained onto a list command: `.list("sessions", &["name", "status"])`.
    pub fn list(mut self, label: &str, fields: &[&str]) -> Self {
        self.agent.label = Some(label.to_string());
        self.agent.fields = fields.iter().map(|f| (*f).to_string()).collect();
        self
    }

    /// Name the collection a *document* wraps, without trimming its fields.
    ///
    /// For an answer that is an object carrying a list among other facts: the
    /// body renders whole, and the label is what the zero-result note is
    /// measured against — an object is never `is_empty()`, so without this a
    /// document with no rows in it would print its header and say nothing.
    pub fn collection(mut self, label: &str) -> Self {
        self.agent.label = Some(label.to_string());
        self
    }

    /// Attach the next-step suggestions this result makes sensible. Each is a
    /// runnable command; parameterize what you cannot know as `<id>` rather
    /// than guessing a value.
    pub fn help<I, S>(mut self, lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.agent.help = lines.into_iter().map(Into::into).collect();
        self
    }

    /// Say what a zero-result answer means, naming the context searched.
    pub fn empty(mut self, message: impl Into<String>) -> Self {
        self.agent.empty = Some(message.into());
        self
    }

    /// Cap this result's free-text fields in the TOON view, with the size and
    /// the escape hatch named in place. `--full` lifts it.
    pub fn truncate(mut self, chars: usize) -> Self {
        self.agent.max_text = Some(chars);
        self
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
            exit_code: None,
            agent: AgentView::default(),
        }
    }

    /// Ask for a specific exit code rather than the generic failure one.
    ///
    /// Only meaningful together with a failure: an output that succeeded exits
    /// 0 whatever this says, because a command that did what was asked has not
    /// failed no matter what it printed.
    pub fn exiting_with(mut self, code: i32) -> Self {
        self.exit_code = Some(code);
        self
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
    /// TOON — the agent-facing format, and what a pipe gets by default.
    Toon,
    /// Compact single-line JSON.
    Json,
    /// Indented JSON.
    JsonPretty,
}

/// Flags that select an output format, resolved together so precedence lives
/// in one place rather than in the order of a chain of `if`s at the call site.
#[derive(Clone, Copy, Debug, Default)]
pub struct FormatFlags {
    pub json: bool,
    pub pretty: bool,
    pub text: bool,
    pub toon: bool,
}

impl Format {
    /// Resolve the effective format from the global flags, falling back to TTY
    /// auto-detection.
    pub fn resolve(flags: FormatFlags) -> Self {
        Self::resolve_with(flags, std::io::stdout().is_terminal())
    }

    /// Resolution core, parameterized on whether stdout is a TTY (testable).
    ///
    /// Precedence `--pretty` > `--json` > `--toon` > `--text` > auto, with auto
    /// being human on a terminal and TOON down a pipe. The explicit flags are
    /// ordered most-specific-first so that a script which belts-and-braces two
    /// of them still gets the stricter machine format.
    pub fn resolve_with(flags: FormatFlags, stdout_is_tty: bool) -> Self {
        if flags.pretty {
            Format::JsonPretty
        } else if flags.json {
            Format::Json
        } else if flags.toon {
            Format::Toon
        } else if flags.text || stdout_is_tty {
            // Explicit --text, or auto-detected interactive terminal.
            Format::Human
        } else {
            // Piped or redirected with no flag: the reader is almost certainly
            // an agent, so answer in the format that costs it least.
            Format::Toon
        }
    }

    /// Render an output to a string in this format.
    pub fn render(self, out: &CommandOutput) -> String {
        match self {
            Format::Human => out.human.clone(),
            Format::Toon => render_toon(out),
            Format::Json => serde_json::to_string(&out.json)
                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
            Format::JsonPretty => serde_json::to_string_pretty(&out.json)
                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
        }
    }
}

/// Render the agent-facing view: the data as TOON, then the zero-result line
/// when there is no data, then the `help[N]:` block.
///
/// The data body is strict TOON ([`mod@crate::cli::toon`]). The `help[N]:`
/// trailer is not, quite — its entries are bare indented lines rather than the
/// `- ` list items §9.4 asks for. That is the AXI convention, which the spec's
/// own reference tooling emits and its validator looks for, and quoting each
/// suggestion into a strict inline array would cost more tokens than the block
/// saves. The two are separated by a blank line so a reader can see where the
/// document ends.
fn render_toon(out: &CommandOutput) -> String {
    let mut blocks: Vec<String> = Vec::new();

    let capped;
    let json = match out.agent.max_text {
        Some(limit) => {
            capped = cap_strings(&out.json, limit);
            &capped
        }
        None => &out.json,
    };

    let rows = json.as_array();
    let body = match (&out.agent.label, rows) {
        // A labelled list: an explicit field order, trimmed to what a list view
        // is for. `--json` remains the way to get every field.
        (Some(label), Some(rows)) if !out.agent.fields.is_empty() => {
            let fields: Vec<&str> = out.agent.fields.iter().map(String::as_str).collect();
            crate::cli::toon::encode_table(label, &fields, rows)
        }
        (Some(label), Some(rows)) => crate::cli::toon::encode(&serde_json::json!({
            label.as_str(): rows
        })),
        _ => crate::cli::toon::encode(json),
    };
    if !body.is_empty() {
        blocks.push(body);
    }

    // AXI principle 5: an empty answer says so, and says what it looked in.
    if let Some(message) = &out.agent.empty {
        let is_empty = match json {
            Value::Array(items) => items.is_empty(),
            // A document that *wraps* its collection is empty when the
            // collection is: the surrounding fields are always there.
            Value::Object(map) => match out.agent.label.as_deref().and_then(|l| map.get(l)) {
                Some(Value::Array(items)) => items.is_empty(),
                _ => map.is_empty(),
            },
            Value::Null => true,
            _ => false,
        };
        if is_empty {
            blocks.push(format!("note: {message}"));
        }
    }

    if !out.agent.help.is_empty() {
        let mut help = vec![format!("help[{}]:", out.agent.help.len())];
        help.extend(out.agent.help.iter().map(|line| format!("  {line}")));
        blocks.push(help.join("\n"));
    }

    blocks.join("\n")
}

/// Copy `value`, shortening every string longer than `limit` and saying so in
/// place — the size that was there and the flag that returns it (AXI principle
/// 3). A hint that omits the total is worse than none: an agent cannot tell
/// whether it is missing a line or a megabyte.
///
/// The cut is on `char_indices`, so it lands on a character boundary rather
/// than splitting a UTF-8 sequence — a captured pane is full of box drawing and
/// emoji, and a byte slice through one panics.
fn cap_strings(value: &Value, limit: usize) -> Value {
    match value {
        Value::String(s) => {
            let total = s.chars().count();
            if total <= limit {
                return value.clone();
            }
            let end = s
                .char_indices()
                .nth(limit)
                .map_or(s.len(), |(byte, _)| byte);
            Value::String(format!(
                "{}… (truncated, {total} chars total — use --full for all of it)",
                &s[..end]
            ))
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| cap_strings(v, limit)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), cap_strings(v, limit)))
                .collect(),
        ),
        other => other.clone(),
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

/// A duration in seconds, compactly: `45s`, `12m`, `3h`, `2d`.
///
/// One unit, always the largest that fits, because the reader of a status table
/// is asking "is this recent" rather than "exactly how old".
pub fn duration_short(secs: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match secs {
        s if s < MINUTE => format!("{s}s"),
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        s => format!("{}d", s / DAY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build the flag set with one flag on, for the precedence table below.
    fn flags(name: &str) -> FormatFlags {
        FormatFlags {
            json: name == "json",
            pretty: name == "pretty",
            text: name == "text",
            toon: name == "toon",
        }
    }

    #[test]
    fn each_flag_selects_its_format() {
        for (flag, want) in [
            ("pretty", Format::JsonPretty),
            ("json", Format::Json),
            ("toon", Format::Toon),
            ("text", Format::Human),
        ] {
            assert_eq!(Format::resolve_with(flags(flag), false), want, "--{flag}");
            // An explicit flag beats TTY detection in either direction.
            assert_eq!(
                Format::resolve_with(flags(flag), true),
                want,
                "--{flag} on a tty"
            );
        }
    }

    #[test]
    fn duration_short_picks_one_unit() {
        assert_eq!(duration_short(0), "0s");
        assert_eq!(duration_short(59), "59s");
        assert_eq!(duration_short(60), "1m");
        assert_eq!(duration_short(3_599), "59m");
        assert_eq!(duration_short(3_600), "1h");
        assert_eq!(duration_short(86_399), "23h");
        assert_eq!(duration_short(86_400), "1d");
    }

    #[test]
    fn format_resolution_precedence() {
        let all = FormatFlags {
            json: true,
            pretty: true,
            text: true,
            toon: true,
        };
        assert_eq!(Format::resolve_with(all, false), Format::JsonPretty);
        let no_pretty = FormatFlags {
            pretty: false,
            ..all
        };
        assert_eq!(Format::resolve_with(no_pretty, false), Format::Json);
        let toon_and_text = FormatFlags {
            text: true,
            toon: true,
            ..FormatFlags::default()
        };
        assert_eq!(Format::resolve_with(toon_and_text, true), Format::Toon);
    }

    #[test]
    fn auto_detection_is_human_on_a_tty_and_toon_down_a_pipe() {
        let none = FormatFlags::default();
        assert_eq!(Format::resolve_with(none, true), Format::Human);
        // The regression that matters: a pipe used to get JSON. The reader is
        // an agent, so it gets the format that costs it least.
        assert_eq!(Format::resolve_with(none, false), Format::Toon);
    }

    #[test]
    fn toon_renders_a_labelled_list_with_its_fields_in_order() {
        let out = CommandOutput::new(
            json!([{ "id": "a", "name": "one", "extra": 1 }, { "id": "b", "name": "two", "extra": 2 }]),
            "human",
        )
        .list("sessions", &["name", "id"])
        .help(["thurbox-cli session get <id>"]);
        assert_eq!(
            Format::Toon.render(&out),
            "sessions[2]{name,id}:\n  one,a\n  two,b\nhelp[1]:\n  thurbox-cli session get <id>"
        );
        // The dropped field is still in --json: trimming is the agent view only.
        assert!(Format::Json.render(&out).contains("\"extra\""));
    }

    #[test]
    fn toon_says_so_when_there_is_nothing_rather_than_printing_a_bare_list() {
        let out = CommandOutput::new(json!([]), "none")
            .list("tasks", &["id"])
            .empty("0 tasks — the todo list is empty");
        let rendered = Format::Toon.render(&out);
        assert!(rendered.starts_with("tasks: []"), "{rendered}");
        assert!(rendered.contains("note: 0 tasks"), "{rendered}");
    }

    #[test]
    fn the_empty_note_stays_out_of_a_non_empty_answer() {
        let out = CommandOutput::new(json!([{ "id": 1 }]), "one")
            .list("tasks", &["id"])
            .empty("0 tasks");
        assert!(!Format::Toon.render(&out).contains("note:"));
    }

    #[test]
    fn truncation_names_the_total_and_the_escape_hatch() {
        let body = "x".repeat(50);
        let out = CommandOutput::new(json!({ "output": body }), "human").truncate(10);
        let rendered = Format::Toon.render(&out);
        assert!(rendered.contains("truncated, 50 chars total"), "{rendered}");
        assert!(rendered.contains("--full"), "{rendered}");
        // Uncapped formats are untouched, which is what `| jq -r .output` needs.
        assert!(Format::Json.render(&out).contains(&body));
    }

    #[test]
    fn truncation_cuts_on_a_character_boundary() {
        // A captured pane is full of multi-byte glyphs; a byte slice through
        // one panics, and this is the shape that would do it.
        let out = CommandOutput::new(json!({ "output": "▀".repeat(20) }), "human").truncate(5);
        assert!(Format::Toon
            .render(&out)
            .contains("truncated, 20 chars total"));
    }

    #[test]
    fn an_unlabelled_output_still_renders_as_toon() {
        // The baseline every command gets for free, with no agent view declared.
        let out = CommandOutput::new(json!({ "focused": true, "session_name": "flow" }), "ok");
        assert_eq!(
            Format::Toon.render(&out),
            "focused: true\nsession_name: flow"
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

    #[test]
    fn from_summary_falls_back_to_compact_json() {
        let out = CommandOutput::from_summary(json!({ "ok": true }));
        assert_eq!(out.human, "{\"ok\":true}");
    }

    #[test]
    fn table_with_no_rows_is_header_only() {
        assert_eq!(table(&["A", "B"], &[]), "A  B");
    }

    #[test]
    fn kv_with_no_pairs_is_empty() {
        assert_eq!(kv(&[]), "");
    }

    #[test]
    fn dash_maps_none_and_empty_to_dash() {
        assert_eq!(dash(None), "-");
        assert_eq!(dash(Some("")), "-");
        assert_eq!(dash(Some("x")), "x");
    }

    #[test]
    fn render_picks_the_matching_representation() {
        let out = CommandOutput::new(json!({ "k": 1 }), "human line");
        assert_eq!(Format::Human.render(&out), "human line");
        assert_eq!(Format::Json.render(&out), "{\"k\":1}");
        assert_eq!(Format::JsonPretty.render(&out), "{\n  \"k\": 1\n}");
    }

    #[test]
    fn failed_carries_the_exit_message() {
        let out = CommandOutput::failed(json!({}), "report", "boom");
        assert_eq!(out.failure.as_deref(), Some("boom"));
        // Output still renders normally before the caller acts on `failure`.
        assert_eq!(Format::Human.render(&out), "report");
    }
}
