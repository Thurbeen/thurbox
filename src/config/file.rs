//! The one loader for a config file thurbox owns: where it lives, seeding it
//! on first run, parsing it with every unknown key reported, and writing it
//! back atomically.
//!
//! Every file degrades the same way — a problem is a warning and the caller's
//! fallback, never a refusal to start — so the only thing a file says for
//! itself is its name, its seed and what it degrades to.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

/// `name` beside `config.toml`: `~/.config/thurbox/<name>`
/// (`~/.config/thurbox-dev/<name>` on a dev build).
pub fn path(name: &str) -> Option<PathBuf> {
    crate::paths::config_file().map(|p| p.with_file_name(name))
}

/// The contents of `name`, writing `seed` there first when the file is
/// absent. `Ok(None)` is a file just seeded, so the caller's defaults are what
/// it holds. `Err` is a warning: the contents could not be established (no
/// path, no directory, a seed that could not be written, an unreadable file).
pub fn read_or_seed(name: &str, seed: &str) -> Result<Option<String>, String> {
    let path = path(name).ok_or_else(|| format!("Could not resolve {name} path"))?;
    if !path.exists() {
        save(&path, seed).map_err(|e| format!("Failed to seed {name}: {e}"))?;
        tracing::info!(path = %path.display(), "Seeded {name}");
        return Ok(None);
    }
    std::fs::read_to_string(&path)
        .map(Some)
        .map_err(|e| format!("Failed to read {name}: {e}"))
}

/// Parse `contents` of `name`, with one warning per unknown key. `Err` is a
/// syntax or type error, compacted for the status bar.
pub fn parse<T: DeserializeOwned>(contents: &str, name: &str) -> Result<(T, Vec<String>), String> {
    parse_toml_reporting_unknown(contents, name)
        .map_err(|e| format!("{name}: {}", compact_toml_error(&e.to_string())))
}

/// [`read_or_seed`] then [`parse`], degrading to `T::default()` with a
/// warning that ends in `degraded` (what the user is left with).
pub fn load_or_seed<T: DeserializeOwned + Default>(
    name: &str,
    seed: &str,
    degraded: &str,
) -> (T, Vec<String>) {
    match read_or_seed(name, seed).and_then(|read| match read {
        Some(contents) => parse(&contents, name),
        None => Ok((T::default(), Vec::new())),
    }) {
        Ok(loaded) => loaded,
        Err(e) => (T::default(), vec![format!("{e}; {degraded}")]),
    }
}

/// Write `contents` to `path` atomically, creating its directory first: a
/// crash mid-write leaves the previous file whole, never a truncated one.
pub fn save(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::paths::write_atomically(path, contents)
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

/// Point every config path into `dir` until the guard drops. The tests of
/// each config file isolate through here: their production code reaches
/// `paths` only through this loader, and the architecture rules hold test code
/// to the same grants.
#[cfg(test)]
pub(crate) fn isolate(dir: &Path) -> crate::paths::TestPathGuard {
    crate::paths::TestPathGuard::new(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, serde::Deserialize)]
    struct Probe {
        #[serde(default)]
        n: u32,
    }

    const SEED: &str = "# n = 1\n";

    #[test]
    fn an_absent_file_is_seeded_and_reads_as_the_defaults() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = isolate(temp.path());
        let (probe, warnings) = load_or_seed::<Probe>("probe.toml", SEED, "using defaults");
        assert_eq!((probe, warnings), (Probe::default(), vec![]));
        let seeded = std::fs::read_to_string(path("probe.toml").unwrap()).unwrap();
        assert_eq!(seeded, SEED);
    }

    #[test]
    fn a_present_file_is_read_with_unknown_keys_reported() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = isolate(temp.path());
        save(&path("probe.toml").unwrap(), "n = 3\nstale = true\n").unwrap();
        let (probe, warnings) = load_or_seed::<Probe>("probe.toml", SEED, "using defaults");
        assert_eq!(probe, Probe { n: 3 });
        assert_eq!(
            warnings,
            ["probe.toml: unknown field `stale` (ignored)".to_string()]
        );
    }

    #[test]
    fn a_malformed_file_degrades_naming_the_file_and_the_fallback() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = isolate(temp.path());
        save(&path("probe.toml").unwrap(), "n = \"three\"").unwrap();
        let (probe, warnings) = load_or_seed::<Probe>("probe.toml", SEED, "using defaults");
        assert_eq!(probe, Probe::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("probe.toml: "), "{warnings:?}");
        assert!(warnings[0].ends_with("; using defaults"), "{warnings:?}");
    }

    #[test]
    fn an_unwritable_seed_degrades_without_a_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = isolate(temp.path());
        let dir = path("probe.toml").unwrap().parent().unwrap().to_path_buf();
        // A file where the config directory should be: neither creating the
        // directory nor writing the seed can succeed.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::write(&dir, "").unwrap();
        let (probe, warnings) = load_or_seed::<Probe>("probe.toml", SEED, "using defaults");
        assert_eq!(probe, Probe::default());
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].starts_with("Failed to seed probe.toml: "),
            "{warnings:?}"
        );
    }
}
