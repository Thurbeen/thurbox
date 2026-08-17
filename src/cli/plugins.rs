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

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::json;

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
}

pub fn run(action: Action) -> Result<CommandOutput, String> {
    match action {
        Action::Dir => dir(),
        Action::New { name } => new(&name),
        Action::Check => check(),
        Action::List => list(),
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

fn check() -> Result<CommandOutput, String> {
    let (dir, chosen) = resolve()?;
    if !dir.is_dir() {
        return Err(format!(
            "no interface at {} ({}) — run the TUI once, or set THURBOX_UI_DIR",
            dir.display(),
            chosen.as_str()
        ));
    }
    let host = crate::kernel::host::LuaHost::new(&dir);
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
    let human = if loaded.is_empty() {
        format!("{}\n  ✓ loads — no panes", dir.display())
    } else {
        format!("{}\n  ✓ loads — {}", dir.display(), loaded.join(", "))
    };
    Ok(CommandOutput::new(
        json!({
            "dir": dir.display().to_string(),
            "ok": true,
            "loaded": loaded,
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
                "state": row.state.as_str(),
                "error": row.error,
            })
        })
        .collect();
    Ok(CommandOutput::new(
        json!({ "dir": dir.display().to_string(), "files": json_rows }),
        format!("{}\n{table}", dir.display()),
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
