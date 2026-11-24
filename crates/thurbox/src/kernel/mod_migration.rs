//! The one-shot move of an interface's mods from their old names to their new
//! ones: `plugins/` to `mods/`, `plugins.toml` to `mods.toml`, `plugins.lock` to
//! `mods.lock`, and `plugin-state.json` to `mod-state.json`.
//!
//! Every composition root that reads those files calls [`migrate_in_force`]
//! before it reads them. It is idempotent, writes atomically, deletes nothing a
//! user wrote, and leaves a file in place when its new name is already taken.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session::plugin_spec::{
    mod_destination, PluginLock, LEGACY_MODS_DIR, LOCK_FILE, MODS_DIR, SPEC_FILE,
};

/// The spec's name before mods were mods.
pub const LEGACY_SPEC_FILE: &str = "plugins.toml";
/// The lock's name before mods were mods.
pub const LEGACY_LOCK_FILE: &str = "plugins.lock";
/// The store of trust grants and the disabled set, before mods were mods.
pub const LEGACY_STATE_FILE: &str = "plugin-state.json";
/// The store of trust grants and the disabled set.
pub const STATE_FILE: &str = "mod-state.json";

/// What one migration moved, what it left in place, and what failed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Migration {
    /// Each old name moved to its new one, as `old → new`.
    pub moved: Vec<String>,
    /// Old paths left where they are because the new name was taken.
    pub kept: Vec<String>,
    pub errors: Vec<String>,
}

impl Migration {
    /// The one line to show a person, or `None` when nothing happened.
    pub fn notice(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.moved.is_empty() {
            parts.push(format!(
                "plugins are mods now: moved {} (an older thurbox does not move them back)",
                self.moved.join(", ")
            ));
        }
        if !self.kept.is_empty() {
            parts.push(format!(
                "left {} in place because the new name is taken: keep what you need, then \
                 delete the old copy",
                self.kept.join(", ")
            ));
        }
        if !self.errors.is_empty() {
            parts.push(format!(
                "could not finish moving plugins to mods: {}",
                self.errors.join("; ")
            ));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

/// Migrate the interface directory in force and the config directory beside
/// `settings.toml`.
pub fn migrate_in_force() -> Migration {
    let ui_dir = super::bundled::resolve(false).ok().map(|(dir, _, _)| dir);
    let config_dir = config_dir();
    migrate(ui_dir.as_deref(), config_dir.as_deref())
}

/// Migrate `ui_dir`'s files and the state store in `config_dir`.
///
/// Every rewrite is written under its new name first, and the old files stay
/// where they are. Only when every one of those writes succeeded does the
/// directory move and are the old files deleted, so a migration that reports
/// [`Migration::errors`] has left the old layout as it found it.
pub fn migrate(ui_dir: Option<&Path>, config_dir: Option<&Path>) -> Migration {
    let mut migration = Migration::default();
    let mut staged = Staged::default();
    let ui_dir = ui_dir.filter(|dir| dir.is_dir());
    if let Some(ui_dir) = ui_dir {
        if let Err(e) = super::bundled::rename_records(
            ui_dir,
            |relative| planned_destination(ui_dir, relative),
            |relative| stays_in_plugins(ui_dir, relative),
        ) {
            migration.errors.push(e);
        }
        stage_text(
            ui_dir,
            (LEGACY_SPEC_FILE, SPEC_FILE),
            rewrite_spec,
            &mut migration,
            &mut staged,
        );
        stage_text(
            ui_dir,
            (LEGACY_LOCK_FILE, LOCK_FILE),
            rewrite_lock,
            &mut migration,
            &mut staged,
        );
    }
    if let Some(config_dir) = config_dir {
        stage_state(config_dir, ui_dir, &mut migration, &mut staged);
    }
    if !migration.errors.is_empty() {
        return migration;
    }
    if let Some(ui_dir) = ui_dir {
        move_mods_dir(ui_dir, &mut migration);
    }
    if !migration.errors.is_empty() {
        return migration;
    }
    migration.moved.extend(staged.moved);
    for old in staged.replaced {
        if let Err(e) = std::fs::remove_file(&old) {
            migration
                .kept
                .push(format!("{} (could not delete it: {e})", old.display()));
        }
    }
    migration
}

#[derive(Default)]
struct Staged {
    moved: Vec<String>,
    replaced: Vec<PathBuf>,
}

/// Old paths still present, each with the new path it should have become.
///
/// What `thurbox-cli doctor` warns about: a migration that kept a clash, or an
/// interface no migration has run on yet.
pub fn leftovers(ui_dir: Option<&Path>, config_dir: Option<&Path>) -> Vec<(PathBuf, PathBuf)> {
    let mut found = Vec::new();
    if let Some(ui_dir) = ui_dir {
        for (old, new) in [
            (LEGACY_MODS_DIR, MODS_DIR),
            (LEGACY_SPEC_FILE, SPEC_FILE),
            (LEGACY_LOCK_FILE, LOCK_FILE),
        ] {
            found.push((ui_dir.join(old), ui_dir.join(new)));
        }
    }
    if let Some(config_dir) = config_dir {
        found.push((
            config_dir.join(LEGACY_STATE_FILE),
            config_dir.join(STATE_FILE),
        ));
    }
    found.retain(|(old, _)| old.exists());
    found
}

/// The config directory, where the state store lives.
pub fn config_dir() -> Option<PathBuf> {
    crate::paths::config_file().and_then(|file| file.parent().map(Path::to_path_buf))
}

fn move_mods_dir(ui_dir: &Path, migration: &mut Migration) {
    let legacy = ui_dir.join(LEGACY_MODS_DIR);
    let current = ui_dir.join(MODS_DIR);
    if !legacy.is_dir() {
        return;
    }
    if !current.exists() {
        match std::fs::rename(&legacy, &current) {
            Ok(()) => migration
                .moved
                .push(format!("{LEGACY_MODS_DIR}/ → {MODS_DIR}/")),
            Err(e) => {
                migration
                    .errors
                    .push(format!("{} → {}: {e}", legacy.display(), current.display()))
            }
        }
        return;
    }
    let entries = match std::fs::read_dir(&legacy) {
        Ok(entries) => entries,
        Err(e) => {
            migration.errors.push(format!("{}: {e}", legacy.display()));
            return;
        }
    };
    let mut names: Vec<_> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .collect();
    names.sort();
    let mut moved_any = false;
    for name in names {
        let shown = format!("{LEGACY_MODS_DIR}/{}", name.to_string_lossy());
        let to = current.join(&name);
        if to.exists() {
            migration.kept.push(shown);
            continue;
        }
        match std::fs::rename(legacy.join(&name), &to) {
            Ok(()) => moved_any = true,
            Err(e) => migration.errors.push(format!("{shown}: {e}")),
        }
    }
    if moved_any {
        migration
            .moved
            .push(format!("{LEGACY_MODS_DIR}/ → {MODS_DIR}/"));
    }
    let _ = std::fs::remove_dir(&legacy);
}

fn stage_text(
    dir: &Path,
    (legacy_name, name): (&str, &str),
    rewrite: fn(&str) -> Result<String, String>,
    migration: &mut Migration,
    staged: &mut Staged,
) {
    let legacy = dir.join(legacy_name);
    let current = dir.join(name);
    let Ok(text) = std::fs::read_to_string(&legacy) else {
        return;
    };
    let rewritten = match rewrite(&text) {
        Ok(rewritten) => rewritten,
        Err(e) => {
            migration.errors.push(format!("{legacy_name}: {e}"));
            return;
        }
    };
    if current.exists() {
        if std::fs::read_to_string(&current).is_ok_and(|existing| existing == rewritten) {
            staged.replaced.push(legacy);
        } else {
            migration.kept.push(legacy_name.to_string());
        }
        return;
    }
    if let Err(e) = crate::paths::write_atomically(&current, &rewritten) {
        migration.errors.push(format!("{}: {e}", current.display()));
        return;
    }
    staged.moved.push(format!("{legacy_name} → {name}"));
    staged.replaced.push(legacy);
}

/// The spec's text with `[[plugin]]` as `[[mod]]` and each `plugins/`
/// destination under `mods/`, every comment kept.
pub fn rewrite_spec(text: &str) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    crate::session::plugin_spec::rename_entries_key(&mut doc);
    crate::session::plugin_spec::for_each_entry_file(&mut doc, |file| {
        let Some(current) = file.as_str() else {
            return;
        };
        let mapped = mod_destination(current);
        if mapped == current {
            return;
        }
        let decor = file.decor().clone();
        *file = mapped.into();
        *file.decor_mut() = decor;
    });
    Ok(doc.to_string())
}

/// The lock's text with every recorded path under `mods/`.
pub fn rewrite_lock(text: &str) -> Result<String, String> {
    let mut lock = PluginLock::parse(text)?;
    for entry in &mut lock.plugins {
        entry.file = mod_destination(&entry.file);
        entry.files = std::mem::take(&mut entry.files)
            .into_iter()
            .map(|(file, digest)| (mod_destination(&file), digest))
            .collect();
        for removed in &mut entry.removed {
            *removed = mod_destination(removed);
        }
    }
    lock.to_toml()
}

fn stays_in_plugins(ui_dir: &Path, relative: &str) -> bool {
    let mapped = mod_destination(relative);
    mapped != relative && ui_dir.join(relative).exists() && ui_dir.join(&mapped).exists()
}

fn planned_destination(ui_dir: &Path, relative: &str) -> Option<String> {
    let mapped = mod_destination(relative);
    (mapped != relative && !stays_in_plugins(ui_dir, relative)).then_some(mapped)
}

fn stage_state(
    config_dir: &Path,
    ui_dir: Option<&Path>,
    migration: &mut Migration,
    staged: &mut Staged,
) {
    let legacy = config_dir.join(LEGACY_STATE_FILE);
    let current = config_dir.join(STATE_FILE);
    if legacy.is_file() {
        match state_rewritten(&legacy, ui_dir) {
            Ok((text, _)) if !current.exists() => {
                match crate::paths::write_atomically(&current, &text) {
                    Ok(()) => {
                        staged
                            .moved
                            .push(format!("{LEGACY_STATE_FILE} → {STATE_FILE}"));
                        staged.replaced.push(legacy);
                    }
                    Err(e) => migration.errors.push(format!("{}: {e}", current.display())),
                }
                return;
            }
            Ok((text, _))
                if std::fs::read_to_string(&current).is_ok_and(|existing| existing == text) =>
            {
                staged.replaced.push(legacy);
                return;
            }
            Ok(_) => migration.kept.push(LEGACY_STATE_FILE.to_string()),
            Err(e) => {
                migration.errors.push(e);
                return;
            }
        }
    }
    if !current.is_file() {
        return;
    }
    match state_rewritten(&current, ui_dir) {
        Ok((text, true)) => {
            if let Err(e) = crate::paths::write_atomically(&current, text) {
                migration.errors.push(format!("{}: {e}", current.display()));
            }
        }
        Ok((_, false)) => {}
        Err(e) => migration.errors.push(e),
    }
}

fn state_rewritten(path: &Path, ui_dir: Option<&Path>) -> Result<(String, bool), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (Some(ui_dir), Ok(mut parsed)) = (ui_dir, serde_json::from_str::<Value>(&text)) else {
        return Ok((text, false));
    };
    let rename = |key: &str| -> Option<String> {
        if let Some(relative) = super::bundled::relative_to(ui_dir, key) {
            return planned_destination(ui_dir, &relative)
                .map(|mapped| ui_dir.join(mapped).to_string_lossy().into_owned());
        }
        if Path::new(key).is_absolute() {
            return None;
        }
        planned_destination(ui_dir, &key.replace('\\', "/"))
    };
    let mut changed = false;
    if let Some(trusted) = parsed.get_mut("trusted").and_then(Value::as_object_mut) {
        let keys: Vec<String> = trusted.keys().cloned().collect();
        for key in keys {
            let Some(renamed) = rename(&key) else {
                continue;
            };
            if trusted.contains_key(&renamed) {
                continue;
            }
            if let Some(grant) = trusted.remove(&key) {
                trusted.insert(renamed, grant);
                changed = true;
            }
        }
    }
    if let Some(disabled) = parsed.get_mut("disabled").and_then(Value::as_array_mut) {
        for entry in disabled.iter_mut() {
            if let Some(renamed) = entry.as_str().and_then(rename) {
                *entry = Value::String(renamed);
                changed = true;
            }
        }
    }
    if !changed {
        return Ok((text, false));
    }
    let text = serde_json::to_string_pretty(&parsed).map_err(|e| e.to_string())?;
    Ok((text, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn delivered_by_an_old_release(ui: &Path) {
        let report = super::super::bundled::materialize(ui);
        assert!(report.errors.is_empty(), "{report:?}");
        std::fs::rename(ui.join("mods"), ui.join("plugins")).expect("rename");
        let manifest = ui.join(".bundled.json");
        let old = std::fs::read_to_string(&manifest)
            .expect("manifest")
            .replace("\"mods/", "\"plugins/");
        std::fs::write(&manifest, old).expect("manifest");
    }

    fn set_read_only(path: &Path) {
        let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(path, permissions).expect("read-only");
    }

    #[test]
    fn a_kept_clash_survives_the_delivery_that_follows() {
        let root = tempfile::tempdir().expect("tempdir");
        let ui = root.path().join("ui");
        delivered_by_an_old_release(&ui);
        write(&ui.join("mods/10_sessions.lua"), "-- mine\n");

        let migration = migrate(Some(&ui), None);
        assert_eq!(migration.kept, vec!["plugins/10_sessions.lua".to_string()]);
        let delivery = super::super::bundled::materialize(&ui);
        assert!(delivery.errors.is_empty(), "{delivery:?}");
        assert!(
            ui.join("plugins/10_sessions.lua").is_file(),
            "the kept copy was deleted: {delivery:?}"
        );
    }

    #[test]
    fn a_read_only_delivery_record_leaves_the_old_layout_untouched() {
        let root = tempfile::tempdir().expect("tempdir");
        let ui = root.path().join("ui");
        delivered_by_an_old_release(&ui);
        write(
            &ui.join(LEGACY_SPEC_FILE),
            "[[plugin]]\nsrc = \"x\"\nfile = \"plugins/50_x.lua\"\n",
        );
        set_read_only(&ui.join(".bundled.json"));

        let migration = migrate(Some(&ui), Some(root.path()));
        assert!(!migration.errors.is_empty(), "{migration:?}");
        assert!(migration.moved.is_empty(), "{migration:?}");
        assert!(ui.join("plugins/10_sessions.lua").is_file());
        assert!(!ui.join("mods").exists());
        assert!(ui.join(LEGACY_SPEC_FILE).is_file());
    }

    #[test]
    fn a_read_only_plugin_state_still_moves_its_keys() {
        let root = tempfile::tempdir().expect("tempdir");
        let ui = root.path().join("ui");
        write(&ui.join("plugins/10_a.lua"), "-- a\n");
        let old = ui.join("plugins/10_a.lua").to_string_lossy().into_owned();
        let state = root.path().join(LEGACY_STATE_FILE);
        write(
            &state,
            &serde_json::json!({ "trusted": {}, "disabled": [old] }).to_string(),
        );
        set_read_only(&state);

        let migration = migrate(Some(&ui), Some(root.path()));
        assert!(migration.errors.is_empty(), "{migration:?}");
        assert_eq!(
            read_json(&root.path().join(STATE_FILE))["disabled"],
            serde_json::json!([ui.join("mods/10_a.lua").to_string_lossy()])
        );
        assert!(ui.join("mods/10_a.lua").is_file());
    }

    #[test]
    fn an_inline_spec_has_its_destinations_moved_too() {
        let rewritten =
            rewrite_spec("plugin = [{ src = \"atlas\", file = \"plugins/75_atlas.lua\" }]\n")
                .expect("rewrite");
        let spec = crate::session::plugin_spec::PluginSpec::parse(&rewritten).expect("parse");
        assert_eq!(spec.plugins[0].file, "mods/75_atlas.lua", "{rewritten}");
    }

    #[test]
    fn a_destination_keeps_its_comment_and_an_unchanged_one_is_untouched() {
        let rewritten = rewrite_spec(
            "[[plugin]]\nsrc = \"a\"\nfile = \"plugins/75_a.lua\" # keep this order\n\n\
             [[plugin]]\nsrc = \"b\"\nfile =   \"y/40_y.lua\"   # theirs\n",
        )
        .expect("rewrite");
        assert!(
            rewritten.contains("file = \"mods/75_a.lua\" # keep this order"),
            "{rewritten}"
        );
        assert!(
            rewritten.contains("file =   \"y/40_y.lua\"   # theirs"),
            "{rewritten}"
        );
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
    }

    #[test]
    fn a_layout_with_nothing_old_is_left_alone_and_says_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        let ui = root.path().join("ui");
        write(&ui.join("mods/10_a.lua"), "-- a\n");
        let migration = migrate(Some(&ui), Some(root.path()));
        assert_eq!(migration, Migration::default());
        assert_eq!(migration.notice(), None);
    }

    #[test]
    fn both_directories_move_what_mods_lacks_and_keep_the_clash() {
        let root = tempfile::tempdir().expect("tempdir");
        let ui = root.path().join("ui");
        write(&ui.join("plugins/10_a.lua"), "-- old a\n");
        write(&ui.join("plugins/20_b.lua"), "-- b\n");
        write(&ui.join("mods/10_a.lua"), "-- new a\n");

        let migration = migrate(Some(&ui), None);
        assert_eq!(migration.kept, vec!["plugins/10_a.lua".to_string()]);
        assert_eq!(migration.moved, vec!["plugins/ → mods/".to_string()]);
        assert_eq!(
            std::fs::read_to_string(ui.join("mods/10_a.lua")).expect("a"),
            "-- new a\n"
        );
        assert!(ui.join("plugins/10_a.lua").is_file());
        assert!(ui.join("mods/20_b.lua").is_file());
        assert_eq!(
            leftovers(Some(&ui), None),
            vec![(ui.join("plugins"), ui.join("mods"))]
        );
    }

    #[test]
    fn trust_keys_move_absolute_and_relative_but_not_a_kept_clash() {
        let root = tempfile::tempdir().expect("tempdir");
        let ui = root.path().join("ui");
        write(&ui.join("plugins/10_a.lua"), "-- a\n");
        write(&ui.join("plugins/20_kept.lua"), "-- old\n");
        write(&ui.join("mods/20_kept.lua"), "-- new\n");
        let absolute = |relative: &str| ui.join(relative).to_string_lossy().into_owned();
        let elsewhere = root
            .path()
            .join("other/plugins/10_a.lua")
            .to_string_lossy()
            .into_owned();
        write(
            &root.path().join(LEGACY_STATE_FILE),
            &serde_json::json!({
                "trusted": {
                    absolute("plugins/10_a.lua"): "one",
                    absolute("plugins/20_kept.lua"): "two",
                    "plugins/10_a.lua": "three",
                    &elsewhere: "four",
                },
                "disabled": [absolute("plugins/10_a.lua"), absolute("lib/x.lua")],
            })
            .to_string(),
        );

        let migration = migrate(Some(&ui), Some(root.path()));
        assert!(migration.errors.is_empty(), "{migration:?}");
        let state = read_json(&root.path().join(STATE_FILE));
        let trusted = state["trusted"].as_object().expect("trusted");
        assert_eq!(trusted[&absolute("mods/10_a.lua")], "one");
        assert_eq!(trusted[&absolute("plugins/20_kept.lua")], "two");
        assert_eq!(trusted["mods/10_a.lua"], "three");
        assert_eq!(trusted[&elsewhere], "four");
        assert_eq!(trusted.len(), 4);
        assert_eq!(
            state["disabled"],
            serde_json::json!([absolute("mods/10_a.lua"), absolute("lib/x.lua")])
        );
    }

    #[test]
    fn a_run_cut_short_after_the_directory_moved_finishes_the_rest() {
        let root = tempfile::tempdir().expect("tempdir");
        let ui = root.path().join("ui");
        write(&ui.join("mods/10_a.lua"), "-- a\n");
        write(
            &ui.join(".bundled.json"),
            r#"{"plugins/10_a.lua":"abc","plugins/64_menu.lua":{"removed":true}}"#,
        );
        let key = ui.join("plugins/10_a.lua").to_string_lossy().into_owned();
        write(
            &root.path().join(STATE_FILE),
            &serde_json::json!({ "trusted": { &key: "d" }, "disabled": [] }).to_string(),
        );

        let first = migrate(Some(&ui), Some(root.path()));
        assert!(first.errors.is_empty(), "{first:?}");
        let manifest = read_json(&ui.join(".bundled.json"));
        assert_eq!(manifest["mods/10_a.lua"], "abc");
        assert_eq!(manifest["mods/64_menu.lua"]["removed"], true);
        let state = read_json(&root.path().join(STATE_FILE));
        assert_eq!(
            state["trusted"][ui.join("mods/10_a.lua").to_string_lossy().as_ref()],
            "d"
        );

        let again = migrate(Some(&ui), Some(root.path()));
        assert_eq!(again, Migration::default());
    }

    #[test]
    fn a_spec_already_written_by_a_cut_short_run_drops_the_old_copy() {
        let root = tempfile::tempdir().expect("tempdir");
        let ui = root.path().join("ui");
        write(&ui.join("mods/.keep"), "");
        let old = "# mine\n[[plugin]]\nsrc = \"x\"\nfile = \"plugins/50_x.lua\"\n";
        write(&ui.join(LEGACY_SPEC_FILE), old);
        write(&ui.join(SPEC_FILE), &rewrite_spec(old).expect("rewrite"));

        let migration = migrate(Some(&ui), None);
        assert!(migration.kept.is_empty(), "{migration:?}");
        assert!(!ui.join(LEGACY_SPEC_FILE).exists());
    }

    #[test]
    fn the_spec_keeps_its_comments_and_names_its_entries_mod() {
        let rewritten = rewrite_spec(
            "# header\n\n# atlas\n[[plugin]]\nsrc = \"atlas\"\nfile = \"plugins/75_atlas.lua\"\n\
             pin = \"v1\"\n\n[[plugin]]\nsrc = \"https://x/y\"\nfile = \"y/40_y.lua\"\n",
        )
        .expect("rewrite");
        assert!(
            rewritten.starts_with("# header\n\n# atlas\n[[mod]]\n"),
            "{rewritten}"
        );
        assert!(
            rewritten.contains("file = \"mods/75_atlas.lua\""),
            "{rewritten}"
        );
        assert!(rewritten.contains("file = \"y/40_y.lua\""), "{rewritten}");
        assert!(!rewritten.contains("[[plugin]]"), "{rewritten}");
        let spec = crate::session::plugin_spec::PluginSpec::parse(&rewritten).expect("parse");
        assert_eq!(spec.plugins.len(), 2);
    }

    #[test]
    fn the_lock_records_every_path_under_mods() {
        let rewritten = rewrite_lock(
            "[[plugin]]\nsrc = \"a\"\nfile = \"plugins/75_a.lua\"\nresolved = \"r\"\n\
             version = \"v\"\nremoved = [\"plugins/76_gone.lua\"]\n\n[plugin.files]\n\
             \"plugins/75_a.lua\" = \"d1\"\n\"lib/a/u.lua\" = \"d2\"\n",
        )
        .expect("rewrite");
        assert!(!rewritten.contains("plugins/"), "{rewritten}");
        let lock = PluginLock::parse(&rewritten).expect("parse");
        let entry = lock.entry("mods/75_a.lua").expect("entry");
        assert_eq!(entry.digest("mods/75_a.lua"), Some("d1"));
        assert_eq!(entry.digest("lib/a/u.lua"), Some("d2"));
        assert_eq!(entry.removed, vec!["mods/76_gone.lua".to_string()]);
    }
}
