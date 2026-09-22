//! Layout presets: whole arrangements thurbox ships, of which the user picks one.
//!
//! A preset is nothing but the text delivered as the interface's `layout.lua`.
//! It is not a second mechanism beside that file: the choice (`layout` in
//! `settings.toml`) decides *which* text delivery writes there, and from then on
//! the file is ordinary — edited, it is the user's and is never overwritten;
//! untouched, an upgrade refreshes it to the new release's copy of the same
//! preset.
//!
//! Switching is the one act that replaces a file the user may have edited, so
//! it is the one place that backs a file up: [`apply`] moves an edited
//! `layout.lua` aside before writing, and says where it put it.

use std::path::{Path, PathBuf};

use super::bundled;

/// One shipped arrangement.
#[derive(Debug)]
pub struct Preset {
    /// The name `settings.toml`, `thurbox-cli layout set` and the installers
    /// take.
    pub name: &'static str,
    /// One line on what it arranges, for `layout list` and the settings row.
    pub summary: &'static str,
    /// The `layout.lua` it delivers.
    pub layout: &'static str,
}

/// The preset a profile that never chose one runs.
pub const DEFAULT: &str = "classic";

/// Every shipped preset, the default first.
///
/// Kept short on purpose: each one is a whole arrangement that has to load,
/// pass `plugin check` and be maintained through every change to the panes, so
/// a preset earns its place by arranging something the others cannot.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "classic",
        summary: "the session list beside the agent pane; the shell is a tab of it",
        layout: include_str!("../../ui/layout.lua"),
    },
    Preset {
        name: "split-shell",
        summary: "the agent pane on top, the same session's shell in a pane below it",
        layout: include_str!("../../ui/layouts/split-shell.lua"),
    },
];

/// The preset called `name`, if thurbox ships one.
pub fn find(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.name == name)
}

/// The preset called `name`, or the default when there is none — what delivery
/// runs, since a misspelt setting must not leave an interface with no
/// arrangement.
pub fn chosen_or_default(name: &str) -> &'static Preset {
    find(name).unwrap_or(&PRESETS[0])
}

/// Every preset name, comma-separated, for a message that has to list them.
pub fn names() -> String {
    PRESETS
        .iter()
        .map(|preset| preset.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The preset `contents` is, byte for byte — `None` for a layout somebody
/// wrote or edited.
pub fn matching(contents: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.layout == contents)
}

/// What [`apply`] did.
#[derive(Debug, PartialEq, Eq)]
pub struct Applied {
    pub preset: &'static str,
    /// Where an edited `layout.lua` was moved before the preset replaced it.
    pub backup: Option<PathBuf>,
    /// Whether `layout.lua` was written at all. An interface already on the
    /// preset, byte for byte, is left as it is.
    pub written: bool,
}

/// Make `dir`'s `layout.lua` the preset called `name`, keeping any edits.
///
/// Three cases, and only the last costs the user anything to undo:
///
/// * already that preset, byte for byte — nothing is written;
/// * absent, or exactly what delivery last wrote there — replaced outright,
///   since nobody's work is in it;
/// * anything else — the user's own file: moved to `layout.lua.bak` (or
///   `.bak.2`, `.bak.3`… — never over an earlier backup) first.
///
/// The delivery record is updated to the new contents, so from here on
/// delivery treats the file as the untouched preset it is.
pub fn apply(dir: &Path, name: &str) -> Result<Applied, String> {
    let preset = find(name)
        .ok_or_else(|| format!("no layout preset named {name:?} — there is {}", names()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(bundled::LAYOUT);
    let current = std::fs::read_to_string(&path).ok();

    if current.as_deref() == Some(preset.layout) {
        bundled::record_written(dir, bundled::LAYOUT, preset.layout)?;
        return Ok(Applied {
            preset: preset.name,
            backup: None,
            written: false,
        });
    }

    let backup = match &current {
        Some(contents) if !bundled::is_untouched(dir, bundled::LAYOUT, contents) => {
            let to = bundled::free_backup_path(dir);
            std::fs::rename(&path, &to)
                .map_err(|e| format!("could not back up {}: {e}", path.display()))?;
            Some(to)
        }
        _ => None,
    };

    std::fs::write(&path, preset.layout).map_err(|e| format!("{}: {e}", path.display()))?;
    bundled::record_written(dir, bundled::LAYOUT, preset.layout)?;
    Ok(Applied {
        preset: preset.name,
        backup,
        written: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_name_is_unique_and_the_default_ships() {
        let mut names: Vec<&str> = PRESETS.iter().map(|preset| preset.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), PRESETS.len());
        assert_eq!(PRESETS[0].name, DEFAULT);
    }

    #[test]
    fn an_unknown_name_runs_the_default_rather_than_nothing() {
        assert_eq!(chosen_or_default("nope").name, DEFAULT);
    }

    #[test]
    fn backups_never_overwrite_each_other() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("layout.lua.bak"), "one").expect("seed");
        assert_eq!(
            bundled::free_backup_path(dir.path()),
            dir.path().join("layout.lua.bak.2")
        );
    }

    #[test]
    fn a_file_delivery_never_recorded_is_the_users_and_is_backed_up() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("layout.lua"), "-- mine").expect("seed");
        let applied = apply(dir.path(), "split-shell").expect("apply");
        assert_eq!(applied.backup, Some(dir.path().join("layout.lua.bak")));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("layout.lua.bak")).expect("backup"),
            "-- mine"
        );
    }
}
