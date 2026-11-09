//! The one loader for a config file thurbox owns: where it lives, seeding it
//! on first run, parsing it with every unknown key reported, and writing it
//! back atomically.
//!
//! Every file degrades the same way — a problem is a warning and the caller's
//! fallback, never a refusal to start — so the only thing a file says for
//! itself is its name, its seed and what it degrades to.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

// The lenient TOML parse is the multiplexer runtime's too (`hosts.toml` is
// read there): one definition, re-exported for every other file.
pub use runtime_mux::toml_file::{compact_toml_error, parse, parse_toml_reporting_unknown};

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

/// Run `change`, a read-change-write of `path`, holding the lock every
/// process takes for that file, a lock on `<path>.lock`.
pub fn locked<T>(path: &Path, change: impl FnOnce() -> T) -> std::io::Result<T> {
    crate::paths::with_file_lock(path, change)
}

/// Point every config path into `dir` until the guard drops. The tests of
/// each config file isolate through here because the architecture rules hold
/// test code to production's grants, and most of those files reach `paths`
/// only through this loader.
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
    fn an_absent_file_is_seeded_into_a_directory_it_creates() {
        let temp = tempfile::TempDir::new().unwrap();
        // The config directory does not exist yet: seeding must create it.
        let _guard = isolate(&temp.path().join("absent"));
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
