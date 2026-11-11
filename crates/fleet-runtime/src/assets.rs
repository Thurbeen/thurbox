//! The plugin files Fleet ships for the harnesses that take their hooks as a
//! file, written under Fleet's data directory on this machine. A harness profile names one with
//! `{asset:<name>}`; the agent's own config directory is never written.

use std::fs;
use std::io;
use std::path::Path;

/// Each file, by its path under the asset directory: pi's extension,
/// opencode's plugin directory, kilo's plugins with the TUI config that
/// names one, omp's extension and qwen's settings. Each only runs
/// `fleet signal`, and does nothing where `FLEET_SIGNAL_FILE` is unset.
pub const FILES: &[(&str, &str)] = &[
    ("fleet-pi.ts", include_str!("assets/fleet-pi.ts")),
    (
        "fleet-opencode/index.js",
        include_str!("assets/fleet-opencode/index.js"),
    ),
    (
        "fleet-opencode/tui.js",
        include_str!("assets/fleet-opencode/tui.js"),
    ),
    (
        "fleet-opencode/signal.js",
        include_str!("assets/fleet-opencode/signal.js"),
    ),
    (
        "fleet-kilo/index.js",
        include_str!("assets/fleet-kilo/index.js"),
    ),
    (
        "fleet-kilo/tui.js",
        include_str!("assets/fleet-kilo/tui.js"),
    ),
    (
        "fleet-kilo/tui.json",
        include_str!("assets/fleet-kilo/tui.json"),
    ),
    (
        "fleet-kilo/signal.js",
        include_str!("assets/fleet-kilo/signal.js"),
    ),
    ("fleet-omp.ts", include_str!("assets/fleet-omp.ts")),
    ("fleet-qwen.json", include_str!("assets/fleet-qwen.json")),
];

/// Write each shipped plugin file under `dir`, unless it already holds the
/// same bytes. Each is renamed into place, so an agent starting meanwhile
/// reads a whole file, the old or the new.
pub fn write(dir: &Path) -> io::Result<()> {
    for (name, text) in FILES {
        let path = dir.join(name);
        if fs::read(&path).is_ok_and(|bytes| bytes == text.as_bytes()) {
            continue;
        }
        let parent = path.parent().unwrap_or(dir);
        fs::create_dir_all(parent)?;
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        let tmp = parent.join(format!(".{file}.{}.tmp", std::process::id()));
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_shipped_file_is_written_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("fleet-assets-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(&dir).unwrap();
        for (name, text) in FILES {
            assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), *text, "{name}");
        }
        // A changed file is put back; an unchanged one is left as it is.
        let (name, text) = FILES[0];
        fs::write(dir.join(name), "changed").unwrap();
        write(&dir).unwrap();
        assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), text);
        let leftovers = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        fs::remove_dir_all(&dir).unwrap();
    }
}
