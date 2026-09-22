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
        summary: "the agent pane over the same session's shell; installed panes on the right",
        layout: include_str!("../../ui/layouts/split-shell.lua"),
    },
    Preset {
        name: "focus",
        summary: "the agent pane alone, full width; F9 and each pane's toggle bring columns back",
        layout: include_str!("../../ui/layouts/focus.lua"),
    },
    Preset {
        name: "ide",
        summary: "sessions left, the shell along the bottom, installed panes in a right column",
        layout: include_str!("../../ui/layouts/ide.lua"),
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

/// A line for the message band when `settings.toml` names a preset that an
/// edited `layout.lua` keeps off the screen.
///
/// Delivery never replaces an edited layout, so choosing a preset by editing
/// `layout` by hand changes nothing while one is on disk — and the settings
/// row and `layout list` then name an arrangement nobody is looking at. Said
/// rather than fixed: switching would move the user's file, which only an
/// explicit `layout set` does. Nothing to say for `classic` (an edited copy of
/// the default is simply the user's own layout) or for a name that is no
/// preset (delivery already ran the default for it).
pub fn not_in_force(dir: &Path, chosen: &str) -> Option<String> {
    let preset = find(chosen).filter(|preset| preset.name != DEFAULT)?;
    let current = std::fs::read_to_string(dir.join(bundled::LAYOUT)).ok()?;
    if current == preset.layout || bundled::is_untouched(dir, bundled::LAYOUT, &current) {
        return None;
    }
    Some(format!(
        "layout {name} is chosen but your edited layout.lua is in use · \
         `thurbox-cli layout set {name}` switches, keeping a backup",
        name = preset.name
    ))
}

/// Whether a switch may rewrite the `layout.lua` in `dir`, which `chosen` says
/// how the interface found.
///
/// The one rule `thurbox-cli layout set` and the settings panel's `layout` row
/// both follow. A user's own copy and a `THURBOX_UI_DIR` override are an
/// interface somebody runs, so a switch they ask for is theirs to make. A
/// checkout is a repository's working tree, and rewriting its file would be an
/// edit to somebody's source nobody asked for; the fallback is a throwaway copy
/// that no next start would read.
pub fn may_switch(dir: &Path, chosen: bundled::Chosen) -> Result<(), String> {
    match chosen {
        bundled::Chosen::UserCopy | bundled::Chosen::Override => Ok(()),
        bundled::Chosen::Checkout => Err(format!(
            "{} is a checkout (THURBOX_UI_DIR), and a layout switch does not rewrite \
             a repository's files — unset THURBOX_UI_DIR to switch your own interface",
            dir.display()
        )),
        bundled::Chosen::Fallback => Err(format!(
            "there is no interface directory of your own to switch ({})",
            chosen.reason()
        )),
    }
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
    fn a_switch_may_write_the_users_copy_and_an_override_but_not_a_checkout() {
        // One rule for `thurbox-cli layout set` and the settings row: they used
        // to disagree, the panel refusing an override the CLI rewrote.
        let dir = Path::new("/x/ui");
        assert!(may_switch(dir, bundled::Chosen::UserCopy).is_ok());
        assert!(may_switch(dir, bundled::Chosen::Override).is_ok());
        assert!(may_switch(dir, bundled::Chosen::Checkout).is_err());
        assert!(may_switch(dir, bundled::Chosen::Fallback).is_err());
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
