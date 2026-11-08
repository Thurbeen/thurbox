//! Parsing a TOML config file with every unknown key reported rather than
//! fatal, the way every config file thurbox owns is read (`hosts.toml` here,
//! the rest through thurbox's `config::file`, which re-exports these).

use serde::de::DeserializeOwned;

/// Parse `contents` of `name`, with one warning per unknown key. `Err` is a
/// syntax or type error, compacted for the status bar.
pub fn parse<T: DeserializeOwned>(contents: &str, name: &str) -> Result<(T, Vec<String>), String> {
    parse_toml_reporting_unknown(contents, name)
        .map_err(|e| format!("{name}: {}", compact_toml_error(&e.to_string())))
}

/// Parse a TOML config document leniently, reporting every unknown field by
/// path instead of failing on it. Stale keys from older thurbox versions and
/// typos both surface as warnings without stranding the user on defaults; a
/// real syntax/type error still fails the parse.
pub fn parse_toml_reporting_unknown<T: DeserializeOwned>(
    contents: &str,
    file_label: &str,
) -> Result<(T, Vec<String>), toml::de::Error> {
    let mut warnings = Vec::new();
    let de = toml::de::Deserializer::parse(contents)?;
    let value = serde_ignored::deserialize(de, |path| {
        warnings.push(format!("{file_label}: unknown field `{path}` (ignored)"));
    })?;
    Ok((value, warnings))
}

/// Collapse a (possibly multi-line) toml error to `<position>: <message>` for
/// compact status-bar display. toml errors render as a header line with the
/// position, a source snippet, then the message — keep the first and last
/// meaningful lines and drop the snippet in between.
pub fn compact_toml_error(s: &str) -> String {
    let lines: Vec<&str> = s
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('|') && !l.starts_with(char::is_numeric))
        .collect();
    match (lines.first(), lines.last()) {
        (Some(first), Some(last)) if first != last => format!("{first}: {last}"),
        (Some(first), _) => (*first).to_string(),
        _ => s.to_string(),
    }
}
