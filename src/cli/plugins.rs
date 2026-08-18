//! `thurbox-cli plugin` — the headless half of what `F11` shows.
//!
//! Writing a plugin needs three answers the interface only gives to a keyboard:
//! which directory is actually being read, what a valid plugin looks like, and
//! whether the one you just wrote will load. A session driving thurbox through a
//! pipe — increasingly, an agent — cannot press `F11`, so it guesses, and the
//! usual outcome is a file edited in a directory nothing reads.
//!
//! Each subcommand is the same question `F11` answers, asked from outside.
//!
//! This is the one place `cli` reaches `crate::kernel`, and only by
//! fully-qualified path (the rule `agent` already follows here): `check` has to
//! load the *real* host, because the failures worth reporting are
//! declaration-shaped — a plugin with no `render`, a slot nothing places, a key
//! that clashes — and a syntax check passes all of them.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde_json::{json, Value};

use super::output::CommandOutput;

/// The starting point `new` writes, and the one the guide shows: one artifact, so
/// a correct example and a broken scaffold cannot be the same release.
const STARTER: &str = include_str!("../../docs/examples/plugin.lua");

/// Where a starter lands in the load order. Above every bundled pane (10–70), so
/// a new plugin never renders between two that expect to be neighbours.
const STARTER_ORDER: &str = "90";

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Print the interface directory in force, and which rule chose it.
    Dir,
    /// Write a starter plugin into that directory.
    New {
        /// Plugin name — one path segment, no separators (e.g. `notes`).
        name: String,
    },
    /// Load the interface the way the TUI does and report what failed.
    Check,
    /// List every file of the interface: where it came from, and whether it draws.
    List,
    /// Install a plugin from a source, recording it in `plugins.toml`.
    Install {
        /// A bare name from the examples in the repo, a URL, or a filesystem path.
        src: String,
        /// Where the pane lands. Required for a single `.lua` source, which
        /// proposes no destination of its own.
        #[arg(long = "as", value_name = "FILE")]
        as_file: Option<String>,
        /// The version to install, and to keep installing.
        #[arg(long)]
        pin: Option<String>,
    },
    /// Bring the interface into agreement with `plugins.toml`.
    Sync,
    /// Advance an entry, or every entry, to what its source carries now.
    Update {
        /// The entry to advance. Omit for all of them.
        name: Option<String>,
    },
    /// Remove an installed plugin, its spec entry and its record.
    Remove {
        /// The entry's name, or the file it delivered.
        name: String,
    },
    /// List the example plugins a bare name resolves to.
    Available,
}

pub fn run(action: Action) -> Result<CommandOutput, String> {
    match action {
        Action::Dir => dir(),
        Action::New { name } => new(&name),
        Action::Check => check(),
        Action::List => list(),
        Action::Install { src, as_file, pin } => install(&src, as_file.as_deref(), pin.as_deref()),
        Action::Sync => sync(),
        Action::Update { name } => update(name.as_deref()),
        Action::Remove { name } => remove(&name),
        Action::Available => available(),
    }
}

/// The directory the interface would load, without creating one.
///
/// `resolve(false)` deliberately does not materialize the user's copy: being
/// *asked* where plugins live must not write a profile as a side effect.
fn resolve() -> Result<(PathBuf, crate::kernel::bundled::Chosen), String> {
    let (dir, chosen, _report) = crate::kernel::bundled::resolve(false)?;
    Ok((dir, chosen))
}

fn dir() -> Result<CommandOutput, String> {
    let (dir, chosen) = resolve()?;
    let plugins = dir.join("plugins");
    let human = format!(
        "{}\n  plugins  {}\n  chosen   {} ({})",
        dir.display(),
        plugins.display(),
        chosen.as_str(),
        chosen.reason()
    );
    Ok(CommandOutput::new(
        json!({
            "dir": dir.display().to_string(),
            "plugins_dir": plugins.display().to_string(),
            "chosen": chosen.as_str(),
            "reason": chosen.reason(),
            "exists": dir.is_dir(),
        }),
        human,
    ))
}

/// Whether `name` is a single, safe path segment.
///
/// Refused rather than sanitised: a scaffold that quietly writes somewhere other
/// than where it said is worse than one that stops. Same rule
/// `paths::validate_safe_name` applies to anything that becomes a path.
fn check_name(name: &str) -> Result<(), String> {
    crate::paths::validate_safe_name(name)
        .map_err(|e| format!("{e} — a plugin name is one segment, like `notes`"))
}

fn new(name: &str) -> Result<CommandOutput, String> {
    check_name(name)?;
    let (dir, chosen) = resolve()?;

    // A starter `require`s `lib.theme` and `lib.widgets`, so a directory that has
    // no `lib/` is not an interface yet and the new plugin would fail to load
    // before it was ever edited. Delivering the bundled files first is what the
    // interface does on its own first run; it preserves anything already there.
    let delivered = if dir.join("lib").is_dir() {
        false
    } else {
        let report = crate::kernel::bundled::materialize(&dir);
        if !report.errors.is_empty() {
            return Err(format!(
                "could not set up {}: {}",
                dir.display(),
                report.errors.join("; ")
            ));
        }
        true
    };

    let plugins = dir.join("plugins");
    std::fs::create_dir_all(&plugins).map_err(|e| format!("{}: {e}", plugins.display()))?;

    let file = plugins.join(format!("{STARTER_ORDER}_{name}.lua"));
    if file.exists() {
        return Err(format!("already exists: {}", file.display()));
    }
    // The example names itself `example`; a scaffold should answer to the name it
    // was asked for, including in its actions and its settings rows.
    let body = STARTER.replace("example", name);
    std::fs::write(&file, body).map_err(|e| format!("{}: {e}", file.display()))?;

    let mut human = format!(
        "wrote {}\n  edit it, then `thurbox-cli plugin check`\n  ({} — {})",
        file.display(),
        chosen.as_str(),
        chosen.reason()
    );
    if delivered {
        human.push_str("\n  (set up the interface here first — it had no lib/)");
    }
    Ok(CommandOutput::new(
        json!({
            "created": file.display().to_string(),
            "name": name,
            "dir": dir.display().to_string(),
            "delivered_interface": delivered,
            "summary": format!("created {}", file.display()),
        }),
        human,
    ))
}

/// Build the host the way the running interface builds it — including the set of
/// files the user turned off.
///
/// Loading everything on disk is not what the interface does, and the difference
/// is the whole point of the switch: a plugin turned off declares no key,
/// occupies no slot and cannot fail to load, which is what makes turning one off
/// the way back from a broken pane. A check that read it anyway would report a
/// failure the interface does not have — and, once the arrangement is consulted,
/// would fault a slot the user correctly removed along with the pane.
///
/// Mirrors `App::publish_disabled`: the paths are stored absolute and `build`
/// compares them relative, through the one conversion between the two
/// (`bundled::relative_to`).
fn host_at(dir: &std::path::Path) -> crate::kernel::host::LuaHost {
    let mut host = crate::kernel::host::LuaHost::new(dir);
    let disabled: Vec<String> = crate::kernel::registry::Registry::load()
        .disabled()
        .filter_map(|absolute| crate::kernel::bundled::relative_to(dir, absolute))
        .collect();
    if !disabled.is_empty() {
        host.set_disabled(disabled);
        host.reload();
    }
    host
}

fn check() -> Result<CommandOutput, String> {
    let (dir, chosen) = resolve()?;
    if !dir.is_dir() {
        return Err(format!(
            "no interface at {} ({}) — run the TUI once, or set THURBOX_UI_DIR",
            dir.display(),
            chosen.as_str()
        ));
    }
    let host = host_at(&dir);
    let loaded: Vec<String> = host
        .plugins
        .iter()
        .map(|plugin| plugin.name.clone())
        .collect();

    // One error means the whole directory failed to load: the host reports the
    // first file that would not compile and stops, because a half-loaded
    // interface is not something to run.
    if let Some(error) = host.error.clone() {
        let human = format!("{}\n  ✗ {error}", dir.display());
        return Ok(CommandOutput::failed(
            json!({
                "dir": dir.display().to_string(),
                "ok": false,
                "error": error,
                "loaded": loaded,
            }),
            human,
            "the interface did not load",
        ));
    }

    // No panes is a real state — a user who removed them all — and the interface
    // runs perfectly well like that, so it is reported rather than failed.
    let loads = if loaded.is_empty() {
        format!("{}\n  ✓ loads — no panes", dir.display())
    } else {
        format!("{}\n  ✓ loads — {}", dir.display(), loaded.join(", "))
    };

    // Loading is not the same as drawing. A plugin whose slot the arrangement
    // names nowhere passes every check above it — it compiles, declares its keys
    // and is listed — and paints nothing, which is the one failure a pipe could
    // not see. Resolved at a stated size because placement is size-dependent;
    // see `layout::REFERENCE`.
    let reference = crate::kernel::layout::REFERENCE;
    let size = format!("{}x{}", reference.width, reference.height);
    let unplaced = match host.unplaced_slots(reference) {
        Ok(slots) => slots,
        // A broken arrangement is survivable at runtime (the loop falls back to
        // a centre-only screen) but it is still a defect, and it is the reason
        // the verdict below cannot be reached — so it is reported as the failure
        // rather than passed over in silence.
        Err(error) => {
            let human = format!("{loads}\n  ✗ {error}");
            return Ok(CommandOutput::failed(
                json!({
                    "dir": dir.display().to_string(),
                    "ok": false,
                    "error": error,
                    "loaded": loaded,
                    "checked_at": size,
                }),
                human,
                "the arrangement did not resolve",
            ));
        }
    };

    if !unplaced.is_empty() {
        let mut lines = vec![loads];
        let mut rows = Vec::new();
        for slot in &unplaced {
            let files: Vec<&str> = host
                .plugins
                .iter()
                .filter(|plugin| &plugin.slot == slot)
                .map(|plugin| plugin.path.as_str())
                .collect();
            lines.push(format!(
                "  ✗ {} — loaded, but nothing places slot {slot:?}\n      \
                 add it to layout.lua's children: {{ slot = {slot:?} }}",
                files.join(", ")
            ));
            rows.push(json!({ "slot": slot, "files": files }));
        }
        lines.push(format!("  (checked at {size})"));
        return Ok(CommandOutput::failed(
            json!({
                "dir": dir.display().to_string(),
                "ok": false,
                "loaded": loaded,
                "unplaced": rows,
                "checked_at": size,
            }),
            lines.join("\n"),
            "a plugin loaded but nothing places it",
        ));
    }

    // Warnings, not failures. A pane that draws nothing because it shares a switch
    // slot is a judgement call about discoverability, not a defect — it loads, it is
    // placed, and the author may have meant it. Failing the exit on a judgement would
    // make `check` unusable as a gate; saying nothing leaves the one install that
    // cannot demonstrate itself completely silent.
    let mut warnings = Vec::new();
    for (index, plugin) in host.plugins.iter().enumerate() {
        if let Some(reason) = host.undiscoverable(index) {
            warnings.push(json!({ "file": plugin.path, "warning": reason }));
        }
    }
    let mut human = loads;
    for warning in &warnings {
        human.push_str(&format!(
            "\n  ! {} — {}",
            warning["file"].as_str().unwrap_or_default(),
            warning["warning"].as_str().unwrap_or_default()
        ));
    }

    Ok(CommandOutput::new(
        json!({
            "dir": dir.display().to_string(),
            "ok": true,
            "loaded": loaded,
            "warnings": warnings,
            "checked_at": size,
        }),
        human,
    ))
}

fn list() -> Result<CommandOutput, String> {
    let (dir, _chosen) = resolve()?;
    let host = crate::kernel::host::LuaHost::new(&dir);
    let sources = crate::kernel::bundled::sources(&dir);
    let registry = crate::kernel::registry::Registry::load();
    // Visibility depends on the terminal's size and the arrangement, neither of
    // which exists here — so every pane is reported as not-drawn rather than
    // guessed at, and the origin (the part that is knowable) is what this is for.
    let rows = crate::kernel::inventory::rows(
        &host.plugins,
        &sources,
        &Default::default(),
        &Default::default(),
        host.error.as_deref(),
        // Trust is a decision the user made in a running interface; from here it
        // is read, not judged — so this reports what was recorded and nothing
        // about whether the file has changed since.
        &|path| {
            let absolute = dir.join(path);
            match registry.is_trusted(&absolute.to_string_lossy()) {
                true => crate::kernel::inventory::Trust::Trusted,
                false => crate::kernel::inventory::Trust::Untrusted,
            }
        },
        &|path| registry.is_disabled(&dir.join(path).to_string_lossy()),
    );

    let table = super::output::table(
        &["file", "kind", "source", "state"],
        &rows
            .iter()
            .map(|row| {
                vec![
                    row.path.clone(),
                    row.kind.as_str().to_string(),
                    row.source.as_str().to_string(),
                    row.state.as_str().to_string(),
                ]
            })
            .collect::<Vec<_>>(),
    );
    let json_rows: Vec<_> = rows
        .iter()
        .map(|row| {
            json!({
                "file": row.path,
                "name": row.name,
                "kind": row.kind.as_str(),
                "source": row.source.as_str(),
                // Named separately from `source` so a script can match on the
                // origin without parsing it out of a label: "installed" is the
                // kind of file it is, and this is who it came from.
                "installed_from": row.source.installed_from(),
                "state": row.state.as_str(),
                // What the file asks to be able to do. Reported because a
                // capability is granted per file, so a script auditing an
                // interface — "which panes here want to run programs?" — should
                // not have to read Lua to find out.
                "capabilities": row
                    .capabilities
                    .iter()
                    .map(|capability| capability.as_str())
                    .collect::<Vec<_>>(),
                "error": row.error,
            })
        })
        .collect();
    Ok(CommandOutput::new(
        json!({ "dir": dir.display().to_string(), "files": json_rows }),
        format!("{}\n{table}", dir.display()),
    ))
}

/// The interface directory an install writes into.
///
/// `resolve(true)` here, unlike everywhere else in this file: installing a plugin
/// into a directory that is not an interface yet would deliver a pane with no
/// `lib/` to require, which fails to load before it is ever looked at — the same
/// reasoning `new` follows.
fn install_dir() -> Result<PathBuf, String> {
    let (dir, chosen, _report) = crate::kernel::bundled::resolve(true)?;
    if !dir.join("lib").is_dir() {
        let report = crate::kernel::bundled::materialize(&dir);
        if !report.errors.is_empty() {
            return Err(format!(
                "could not set up {} ({}): {}",
                dir.display(),
                chosen.as_str(),
                report.errors.join("; ")
            ));
        }
    }
    Ok(dir)
}

/// The line to add to `ui/layout.lua` so a slot is placed.
///
/// Printed at the moment of installing rather than left for `check` to discover:
/// a pane that loads and draws nothing is the one failure with no symptom, so the
/// instruction should arrive before anybody has to go looking for it.
fn placement_hint(dir: &Path, file: &str) -> Option<String> {
    let host = host_at(dir);
    let index = host.plugins.iter().position(|plugin| plugin.path == file)?;
    let plugin = &host.plugins[index];
    if plugin.floats || plugin.decorates.is_some() {
        return None;
    }
    let unplaced = host
        .unplaced_slots(crate::kernel::layout::REFERENCE)
        .unwrap_or_default();
    if unplaced.contains(&plugin.slot) {
        return Some(format!(
            "nothing places slot {slot:?} yet — add to layout.lua's children: \
             {{ slot = {slot:?} }}",
            slot = plugin.slot
        ));
    }
    // Placed, loaded, and still invisible: the alternate occupant of a switch slot.
    // Said here because this is the moment the user is looking — afterwards they are
    // looking at a screen that did not change, with no reason to suspect the install.
    if host.undiscoverable(index).is_some() {
        return Some(format!(
            "it shares the {slot:?} slot, so it is not shown by default — reach it \
             with ctrl+l, or declare a pill so the action band offers it",
            slot = plugin.slot
        ));
    }
    None
}

fn install(src: &str, as_file: Option<&str>, pin: Option<&str>) -> Result<CommandOutput, String> {
    let dir = install_dir()?;
    let report = crate::kernel::packages::install(&dir, src, as_file, pin)?;

    let hint = placement_hint(&dir, &report.file);
    let repository = crate::kernel::packages::is_repository(&report.src);
    let mut human = format!(
        "{} {} from {} ({})",
        report.outcome.as_str(),
        report.file,
        report.src,
        report.version
    );
    if repository {
        // Said at the moment it happens, not only in the guide. Installing a plugin
        // from a repository puts that repository's files on your disk — binaries
        // included — which is what cloning anything does, and is worse left unsaid.
        human.push_str("\n  a working copy of that repository is now in your interface directory");
    }
    if let Some(hint) = &hint {
        human.push_str(&format!("\n  {hint}"));
    }
    human.push_str("\n  `thurbox-cli plugin check` to confirm it loads and draws");
    Ok(CommandOutput::new(
        json!({
            "dir": dir.display().to_string(),
            "file": report.file,
            "name": report.name,
            "src": report.src,
            "version": report.version,
            "outcome": report.outcome.as_str(),
            "repository": repository,
            "placement_hint": hint,
            "summary": format!("{} {}", report.outcome.as_str(), report.file),
        }),
        human,
    ))
}

/// One report line, and the JSON beside it.
fn entry_rows(reports: &[crate::kernel::packages::EntryReport]) -> (Vec<String>, Vec<Value>) {
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    for report in reports {
        let moved = match &report.from {
            Some(from) => format!(" ({from} → {})", report.version),
            None if report.version.is_empty() => String::new(),
            None => format!(" ({})", report.version),
        };
        lines.push(format!(
            "  {:<9} {}{moved}",
            report.outcome.as_str(),
            report.file
        ));
        rows.push(json!({
            "file": report.file,
            "name": report.name,
            "src": report.src,
            "version": report.version,
            "from": report.from,
            "outcome": report.outcome.as_str(),
        }));
    }
    (lines, rows)
}

fn sync() -> Result<CommandOutput, String> {
    let dir = install_dir()?;
    let reports = crate::kernel::packages::sync(&dir)?;
    let (lines, rows) = entry_rows(&reports);
    let changed = reports.iter().any(|report| report.outcome.changed());

    let human = if reports.is_empty() {
        format!("{}\n  nothing installed", dir.display())
    } else {
        format!("{}\n{}", dir.display(), lines.join("\n"))
    };
    Ok(CommandOutput::new(
        json!({
            "dir": dir.display().to_string(),
            "changed": changed,
            "entries": rows,
        }),
        human,
    ))
}

fn update(name: Option<&str>) -> Result<CommandOutput, String> {
    let dir = install_dir()?;
    let reports = crate::kernel::packages::update(&dir, name)?;
    let (lines, rows) = entry_rows(&reports);

    // Nothing newer upstream is success, not a failure — a scheduled update that
    // exits non-zero on "already current" is one nobody can schedule.
    let human = if reports.is_empty() {
        format!("{}\n  nothing installed", dir.display())
    } else if reports.iter().any(|report| report.from.is_some()) {
        format!("{}\n{}", dir.display(), lines.join("\n"))
    } else {
        format!("{}\n{}\n  already current", dir.display(), lines.join("\n"))
    };
    Ok(CommandOutput::new(
        json!({
            "dir": dir.display().to_string(),
            "moved": reports.iter().filter(|report| report.from.is_some()).count(),
            "entries": rows,
        }),
        human,
    ))
}

fn remove(name: &str) -> Result<CommandOutput, String> {
    let (dir, _chosen) = resolve()?;
    let report = crate::kernel::packages::remove(&dir, name)?;
    Ok(CommandOutput::new(
        json!({
            "dir": dir.display().to_string(),
            "file": report.file,
            "name": report.name,
            "src": report.src,
            "outcome": report.outcome.as_str(),
            "summary": format!("removed {}", report.file),
        }),
        format!("removed {} ({})", report.file, report.src),
    ))
}

fn available() -> Result<CommandOutput, String> {
    let plugins = crate::kernel::packages::EXAMPLE_PLUGINS;
    let table = super::output::table(
        &["name", "description"],
        &plugins
            .iter()
            .map(|(name, description)| vec![(*name).to_string(), (*description).to_string()])
            .collect::<Vec<_>>(),
    );
    Ok(CommandOutput::new(
        json!({
            "plugins": plugins
                .iter()
                .map(|(name, description)| json!({ "name": name, "description": description }))
                .collect::<Vec<_>>(),
        }),
        table,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The starter has to be a plugin, not a snippet: the whole point is that a
    /// new file loads before it is edited.
    #[test]
    fn the_starter_declares_what_a_plugin_needs() {
        assert!(STARTER.contains("render = function"));
        assert!(STARTER.contains("name = \"example\""));
        assert!(STARTER.contains("keys = {"));
    }

    #[test]
    fn a_name_that_is_not_one_segment_is_refused() {
        for bad in ["../escape", "a/b", "", "."] {
            assert!(check_name(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(check_name("notes").is_ok());
    }

    #[test]
    fn the_starter_answers_to_the_name_it_was_given() {
        let body = STARTER.replace("example", "notes");
        assert!(body.contains("name = \"notes\""));
        assert!(body.contains("notes.refresh"));
        assert!(!body.contains("example"), "no stale name is left behind");
    }

    /// `plugins/` is where the host looks, so that is where a starter goes.
    #[test]
    fn a_starter_is_named_to_load_after_the_bundled_panes() {
        // Asserted by component rather than as one string: Windows joins with a
        // backslash, and the native separator is the right one to write there.
        let file = std::path::Path::new("plugins").join(format!("{STARTER_ORDER}_notes.lua"));
        assert_eq!(file.parent().and_then(|p| p.to_str()), Some("plugins"));
        assert_eq!(
            file.file_name().and_then(|n| n.to_str()),
            Some("90_notes.lua")
        );
    }
}
