//! `thurbox-cli layout` — list the layout presets, and switch between them.
//!
//! A preset is the text delivered as the interface's `layout.lua`
//! (`kernel::presets`). Switching records the choice in `settings.toml`, where
//! delivery reads it, and rewrites `layout.lua` — after moving a copy the user
//! edited out of the way, which is the one thing here that could otherwise cost
//! somebody work.
//!
//! Like `cli::plugins`, this drives the interface's own files, so it reaches
//! `crate::kernel` by fully-qualified path only.

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::json;

use super::output::CommandOutput;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Every preset, which one is chosen, and whether layout.lua still is it.
    List,
    /// Choose a preset: record it in settings.toml and rewrite layout.lua,
    /// backing up a copy you edited first. A running thurbox picks it up.
    Set {
        /// The preset's name, as `layout list` prints it.
        name: String,
    },
}

pub fn run(action: Action) -> Result<CommandOutput, String> {
    match action {
        Action::List => list(),
        Action::Set { name } => set(&name),
    }
}

/// The interface directory a switch may write into.
///
/// Only the user's own copy. A `THURBOX_UI_DIR` checkout is a repository's
/// working tree, and rewriting its `layout.lua` from a settings command would be
/// an edit to somebody's source that nobody asked for.
fn interface_dir() -> Result<PathBuf, String> {
    use crate::kernel::bundled::Chosen;
    let (dir, chosen, report) = crate::kernel::bundled::resolve(true)?;
    if !report.errors.is_empty() {
        return Err(format!(
            "could not set up {}: {}",
            dir.display(),
            report.errors.join("; ")
        ));
    }
    match chosen {
        Chosen::UserCopy | Chosen::Override => Ok(dir),
        Chosen::Checkout => Err(format!(
            "{} is a checkout (THURBOX_UI_DIR), and a layout switch does not rewrite \
             a repository's files — unset THURBOX_UI_DIR to switch your own interface",
            dir.display()
        )),
        Chosen::Fallback => Err(format!(
            "there is no interface directory of your own to switch ({})",
            chosen.reason()
        )),
    }
}

fn list() -> Result<CommandOutput, String> {
    let (settings, _warnings) = crate::agent::settings_config::load_or_seed_with_warnings();
    let current = crate::kernel::presets::chosen_or_default(&settings.layout).name;
    let (dir, _chosen, _report) = crate::kernel::bundled::resolve(false)?;
    let on_disk = std::fs::read_to_string(dir.join(crate::kernel::bundled::LAYOUT)).ok();
    // "Edited" means the file is somebody's own work, which a switch would back
    // up — not merely "not the chosen preset", which an upgrade fixes by itself.
    let edited = on_disk
        .as_deref()
        .is_some_and(|text| crate::kernel::presets::matching(text).is_none());

    let rows: Vec<Vec<String>> = crate::kernel::presets::PRESETS
        .iter()
        .map(|preset| {
            let mark = if preset.name == current { "*" } else { "" };
            vec![
                mark.to_string(),
                preset.name.to_string(),
                preset.summary.to_string(),
            ]
        })
        .collect();
    let mut human = super::output::table(&["", "preset", "arranges"], &rows);
    if edited {
        human.push_str(&format!(
            "\nlayout.lua in {} has your edits, so it is not `{current}` as shipped; \
             `thurbox-cli layout set <name>` backs it up before switching",
            dir.display()
        ));
    }
    Ok(CommandOutput::new(
        json!({
            "current": current,
            "edited": edited,
            "dir": dir.display().to_string(),
            "presets": crate::kernel::presets::PRESETS
                .iter()
                .map(|preset| json!({
                    "name": preset.name,
                    "summary": preset.summary,
                    "current": preset.name == current,
                }))
                .collect::<Vec<_>>(),
        }),
        human,
    ))
}

fn set(name: &str) -> Result<CommandOutput, String> {
    let preset = crate::kernel::presets::find(name).ok_or_else(|| {
        format!(
            "no layout preset named {name:?} — there is {}",
            crate::kernel::presets::names()
        )
    })?;
    let dir = interface_dir()?;

    // The setting first: were the file written and the setting not, the next
    // start would find an untouched preset that is not the chosen one and
    // deliver the old choice straight back over it.
    let (mut settings, _warnings) = crate::agent::settings_config::load_or_seed_with_warnings();
    if settings.layout != preset.name {
        settings.layout = preset.name.to_string();
        crate::agent::settings_config::save_settings(&settings)
            .map_err(|e| format!("could not record the layout in settings.toml: {e}"))?;
    }
    let applied = crate::kernel::presets::apply(&dir, preset.name)?;

    let mut human = format!("layout: {}\n  {}", preset.name, dir.display());
    if let Some(backup) = &applied.backup {
        human.push_str(&format!(
            "\n  your edited layout.lua is kept as {}",
            backup.display()
        ));
    }
    if !applied.written {
        human.push_str("\n  layout.lua already was this preset; nothing rewritten");
    }
    Ok(CommandOutput::new(
        json!({
            "layout": preset.name,
            "dir": dir.display().to_string(),
            "written": applied.written,
            "backup": applied.backup.map(|path| path.display().to_string()),
        }),
        human,
    ))
}
