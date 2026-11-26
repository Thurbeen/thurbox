use std::io::IsTerminal;

use serde_json::Value;

#[derive(Debug)]
pub struct CommandOutput {
    pub json: Value,
    pub human: String,
    pub failure: Option<String>,
    pub exit_code: Option<i32>,
    pub agent: AgentView,
}

#[derive(Debug, Default)]
pub struct AgentView {
    pub label: Option<String>,
    pub fields: Vec<String>,
    pub help: Vec<String>,
    pub empty: Option<String>,
    pub max_text: Option<usize>,
    pub scalar: bool,
}

impl CommandOutput {
    pub fn new(json: Value, human: impl Into<String>) -> Self {
        Self {
            json,
            human: human.into(),
            failure: None,
            exit_code: None,
            agent: AgentView::default(),
        }
    }

    pub fn list(mut self, label: &str, fields: &[&str]) -> Self {
        self.agent.label = Some(label.to_string());
        self.agent.fields = fields.iter().map(|f| (*f).to_string()).collect();
        self
    }

    pub fn select_fields(&mut self, fields: Vec<String>) -> Result<(), String> {
        let (Some(label), Some(rows)) = (&self.agent.label, self.json.as_array()) else {
            return Ok(());
        };
        let objects: Vec<_> = rows.iter().filter_map(Value::as_object).collect();
        if let Some(unknown) = fields
            .iter()
            .find(|f| !objects.is_empty() && !objects.iter().any(|o| o.contains_key(*f)))
        {
            let mut known: Vec<&str> = objects
                .iter()
                .flat_map(|o| o.keys().map(String::as_str))
                .collect();
            known.sort_unstable();
            known.dedup();
            return Err(format!(
                "unknown field '{unknown}' for {label}; known: {}",
                known.join(",")
            ));
        }
        if !fields.is_empty() && !objects.is_empty() {
            let headers: Vec<String> = fields.iter().map(|f| f.to_uppercase()).collect();
            let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
            let cells: Vec<Vec<String>> = objects
                .iter()
                .map(|o| fields.iter().map(|f| cell(o.get(f))).collect())
                .collect();
            self.human = table(&headers, &cells);
        }
        self.agent.fields = fields;
        Ok(())
    }

    pub fn collection(mut self, label: &str) -> Self {
        self.agent.label = Some(label.to_string());
        self
    }

    pub fn help<I, S>(mut self, lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.agent.help = lines.into_iter().map(Into::into).collect();
        self
    }

    pub fn empty(mut self, message: impl Into<String>) -> Self {
        self.agent.empty = Some(message.into());
        self
    }

    pub fn truncate(mut self, chars: usize) -> Self {
        self.agent.max_text = Some(chars);
        self
    }

    pub fn scalar(mut self) -> Self {
        self.agent.scalar = true;
        self
    }

    pub fn from_summary(json: Value) -> Self {
        let human = json
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| json.to_string());
        Self::new(json, human)
    }

    pub fn failed(json: Value, human: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            json,
            human: human.into(),
            failure: Some(msg.into()),
            exit_code: None,
            agent: AgentView::default(),
        }
    }

    pub fn exiting_with(mut self, code: i32) -> Self {
        self.exit_code = Some(code);
        self
    }
}

impl std::ops::Deref for CommandOutput {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        &self.json
    }
}

impl std::fmt::Display for CommandOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.json)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Human,
    Toon,
    Json,
    JsonPretty,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FormatFlags {
    pub json: bool,
    pub pretty: bool,
    pub text: bool,
    pub toon: bool,
}

impl Format {
    pub fn resolve(flags: FormatFlags) -> Self {
        Self::resolve_with(flags, std::io::stdout().is_terminal())
    }

    pub fn resolve_with(flags: FormatFlags, stdout_is_tty: bool) -> Self {
        if flags.pretty {
            Format::JsonPretty
        } else if flags.json {
            Format::Json
        } else if flags.toon {
            Format::Toon
        } else if flags.text || stdout_is_tty {
            Format::Human
        } else {
            Format::Toon
        }
    }

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

fn render_toon(out: &CommandOutput) -> String {
    if out.agent.scalar {
        return out.human.clone();
    }
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

    if let Some(message) = &out.agent.empty {
        let is_empty = match json {
            Value::Array(items) => items.is_empty(),
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

fn cell(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "-".to_string(),
        Some(Value::String(s)) if s.is_empty() => "-".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

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

pub fn kv(pairs: &[(&str, String)]) -> String {
    let width = pairs
        .iter()
        .map(|(k, _)| k.chars().count() + ":".len())
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

pub fn dash(s: Option<&str>) -> String {
    match s {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => "-".to_string(),
    }
}

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
        assert!(Format::Json.render(&out).contains("\"extra\""));
    }

    #[test]
    fn selected_fields_drive_the_text_table_with_absent_values_dashed() {
        let mut out = CommandOutput::new(
            json!([{ "name": "one", "branch": null }, { "name": "two", "branch": "main" }]),
            "default table",
        )
        .list("sessions", &["name"]);
        out.select_fields(vec!["branch".into(), "name".into()])
            .expect("both fields are carried");
        assert_eq!(
            Format::Human.render(&out),
            "BRANCH  NAME\n-       one\nmain    two"
        );
    }

    #[test]
    fn selecting_all_keeps_the_commands_own_text_table() {
        let mut out = CommandOutput::new(json!([{ "name": "one" }]), "default table")
            .list("sessions", &["name"]);
        out.select_fields(Vec::new()).expect("all is always valid");
        assert_eq!(Format::Human.render(&out), "default table");
        assert!(out.agent.fields.is_empty());
    }

    #[test]
    fn an_empty_list_has_no_rows_to_refuse_a_field_against() {
        let mut out =
            CommandOutput::new(json!([]), "No active sessions.").list("sessions", &["name"]);
        out.select_fields(vec!["anything".into()])
            .expect("nothing to check against");
        assert_eq!(Format::Human.render(&out), "No active sessions.");
    }

    #[test]
    fn fields_leave_an_output_that_is_not_a_labelled_list_alone() {
        let mut out = CommandOutput::new(json!({ "id": "a" }), "id: a");
        out.select_fields(vec!["colour".into()])
            .expect("a record has no columns to refuse");
        assert_eq!(Format::Human.render(&out), "id: a");
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
        assert!(Format::Json.render(&out).contains(&body));
    }

    #[test]
    fn truncation_cuts_on_a_character_boundary() {
        let out = CommandOutput::new(json!({ "output": "▀".repeat(20) }), "human").truncate(5);
        assert!(Format::Toon
            .render(&out)
            .contains("truncated, 20 chars total"));
    }

    #[test]
    fn an_unlabelled_output_still_renders_as_toon() {
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
        assert_eq!(Format::Human.render(&out), "report");
    }
}
