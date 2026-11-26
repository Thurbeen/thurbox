use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde_json::{json, Value};

use super::output::CommandOutput;

const STARTER: &str = include_str!("../../../../examples/lua/mod.lua");

const STARTER_ORDER: &str = "90";

#[derive(Subcommand, Debug)]
pub enum Action {
    #[command(about = "Print the interface directory in force, and which rule chose it")]
    Dir,
    #[command(about = "Write a starter mod into that directory")]
    New {
        #[arg(help = "Mod name — one path segment, no separators (e.g. `notes`)")]
        name: String,
    },
    #[command(about = "Load the interface the way the TUI does and report what failed")]
    Check,
    #[command(
        about = "List every file of the interface: where it came from, and whether it draws"
    )]
    List,
    #[command(about = "Install a mod from a source, recording it in `mods.toml`")]
    Install {
        #[arg(help = "A bare name from the examples in the repo, a URL, or a filesystem path")]
        src: String,
        #[arg(
            help = "Where the pane lands — or, for a package declaring several panes, which of their destinations the entry is keyed on; those are not moved. Required for a single `.lua` source, which proposes no destination of its own"
        )]
        #[arg(long = "as", value_name = "FILE")]
        as_file: Option<String>,
        #[arg(help = "The version to install, and to keep installing")]
        #[arg(long)]
        pin: Option<String>,
    },
    #[command(about = "Bring the interface into agreement with `mods.toml`")]
    Sync,
    #[command(about = "Advance an entry, or every entry, to what its source carries now")]
    Update {
        #[arg(help = "The entry to advance. Omit for all of them")]
        name: Option<String>,
    },
    #[command(about = "Remove an installed mod, its spec entry and its record")]
    Remove {
        #[arg(help = "The entry's name, or the file it delivered")]
        name: String,
    },
    #[command(about = "List the example mods a bare name resolves to")]
    Available,
    #[command(about = "List every event a mod can subscribe to, with its payload fields")]
    Events,
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
        Action::Events => events(),
    }
}

fn events() -> Result<CommandOutput, String> {
    let specs = crate::kernel::events::KERNEL_EVENTS;
    let mut rows: Vec<Vec<String>> = specs
        .iter()
        .map(|spec| {
            vec![
                spec.name.to_string(),
                spec.fields.join(", "),
                spec.when.to_string(),
            ]
        })
        .collect();
    rows.push(vec![
        format!("{}<name>", crate::kernel::events::USER_PREFIX),
        "source, plus whatever the emitter passed".to_string(),
        "a mod ran command(\"emit\", { text = \"<name>\", … })".to_string(),
    ]);
    let table = super::output::table(&["event", "payload", "fires when"], &rows);
    Ok(CommandOutput::new(
        json!({
            "events": specs
                .iter()
                .map(|spec| json!({ "name": spec.name, "fields": spec.fields, "when": spec.when }))
                .collect::<Vec<_>>(),
            "user_prefix": crate::kernel::events::USER_PREFIX,
        }),
        table,
    ))
}

fn resolve() -> Result<(PathBuf, crate::kernel::bundled::Chosen), String> {
    let (dir, chosen, _report) = crate::kernel::bundled::resolve(false)?;
    Ok((dir, chosen))
}

fn dir() -> Result<CommandOutput, String> {
    let (dir, chosen) = resolve()?;
    let mods = dir.join(crate::session::mod_spec::MODS_DIR);
    let human = format!(
        "{}\n  mods  {}\n  chosen   {} ({})",
        dir.display(),
        mods.display(),
        chosen.as_str(),
        chosen.reason()
    );
    Ok(CommandOutput::new(
        json!({
            "dir": dir.display().to_string(),
            "mods_dir": mods.display().to_string(),
            "plugins_dir": mods.display().to_string(),
            "chosen": chosen.as_str(),
            "reason": chosen.reason(),
            "exists": dir.is_dir(),
        }),
        human,
    ))
}

fn check_name(name: &str) -> Result<(), String> {
    crate::paths::validate_safe_name(name)
        .map_err(|e| format!("{e} — a mod name is one segment, like `notes`"))
}

fn new(name: &str) -> Result<CommandOutput, String> {
    check_name(name)?;
    let (dir, chosen) = resolve()?;

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

    let mods = dir.join(crate::session::mod_spec::MODS_DIR);
    std::fs::create_dir_all(&mods).map_err(|e| format!("{}: {e}", mods.display()))?;

    let file = mods.join(format!("{STARTER_ORDER}_{name}.lua"));
    if file.exists() {
        return Err(format!("already exists: {}", file.display()));
    }
    let body = STARTER.replace("example", name);
    crate::paths::write_atomically(&file, body).map_err(|e| format!("{}: {e}", file.display()))?;

    let mut human = format!(
        "wrote {}\n  edit it, then `thurbox-cli mod check`\n  ({} — {})",
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

fn file_of(host: &crate::kernel::host::LuaHost, mod_name: &str) -> String {
    let files: Vec<&str> = host
        .mods
        .iter()
        .filter(|loaded| loaded.name == mod_name)
        .map(|loaded| loaded.path.as_str())
        .collect();
    match files.is_empty() {
        true => mod_name.to_string(),
        false => files.join(", "),
    }
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
    let loaded_names: Vec<String> = host.mods.iter().map(|loaded| loaded.name.clone()).collect();

    if let Some(error) = host.error.clone() {
        let human = format!("{}\n  ✗ {error}", dir.display());
        return Ok(CommandOutput::failed(
            json!({
                "dir": dir.display().to_string(),
                "ok": false,
                "error": error,
                "loaded": loaded_names,
            }),
            human,
            "the interface did not load",
        ));
    }

    let loads = if loaded_names.is_empty() {
        format!("{}\n  ✓ loads — no panes", dir.display())
    } else {
        format!("{}\n  ✓ loads — {}", dir.display(), loaded_names.join(", "))
    };

    let reference = crate::kernel::layout::REFERENCE;
    let size = format!("{}x{}", reference.width, reference.height);
    let unplaced = match host.unplaced_slots(reference) {
        Ok(slots) => slots,
        Err(error) => {
            let human = format!("{loads}\n  ✗ {error}");
            return Ok(CommandOutput::failed(
                json!({
                    "dir": dir.display().to_string(),
                    "ok": false,
                    "error": error,
                    "loaded": loaded_names,
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
                .mods
                .iter()
                .filter(|loaded| &loaded.slot == slot)
                .map(|loaded| loaded.path.as_str())
                .collect();
            lines.push(format!(
                "  ✗ {} — loaded, but nothing places slot {slot:?}\n      {}",
                files.join(", "),
                layout_fix(&host, slot)
            ));
            rows.push(json!({ "slot": slot, "files": files }));
        }
        lines.push(format!("  (checked at {size})"));
        return Ok(CommandOutput::failed(
            json!({
                "dir": dir.display().to_string(),
                "ok": false,
                "loaded": loaded_names,
                "unplaced": rows,
                "checked_at": size,
            }),
            lines.join("\n"),
            "a mod loaded but nothing places it",
        ));
    }

    let mut warnings = Vec::new();
    let sources = crate::kernel::bundled::sources(&dir);
    for (index, loaded) in host.mods.iter().enumerate() {
        if let Some(reason) = host.undiscoverable(index) {
            warnings.push(json!({ "file": loaded.path, "warning": reason }));
        }
        for binding in &loaded.bindings {
            if crate::kernel::registry::protected_action(&binding.action) {
                warnings.push(json!({
                    "file": loaded.path,
                    "warning": format!("{} is reserved for the kernel", binding.action),
                    "action": binding.action,
                }));
            }
        }
        if matches!(
            sources.get(&loaded.path),
            Some(crate::kernel::bundled::Source::Bundled)
        ) {
            continue;
        }
        for handler in host.raw_gesture_handlers(index) {
            if handler == "on_key" || handler == "on_scroll" {
                continue;
            }
            warnings.push(json!({
                "file": loaded.path,
                "warning": format!("{handler} handles gestures that are not advertised as catalog actions; declare semantic actions or addressed input equivalents"),
                "handler": handler,
            }));
        }
    }
    let registry = declared(&host);
    let catalog = registry.action_catalog();
    for loaded in &host.mods {
        for action in loaded
            .bindings
            .iter()
            .map(|entry| entry.action.as_str())
            .chain(loaded.commands.iter().map(|entry| entry.action.as_str()))
        {
            if let Some(owner) = catalog
                .iter()
                .find(|entry| entry.name == action)
                .map(|entry| entry.owner.as_str())
            {
                if owner != loaded.name {
                    warnings.push(json!({
                        "file": loaded.path,
                        "warning": format!("{action} is owned by {owner}; this declaration cannot be advertised"),
                        "action": action,
                    }));
                }
            }
        }
    }
    let advertised: std::collections::BTreeSet<&str> =
        catalog.iter().map(|entry| entry.name.as_str()).collect();
    let menu = regex::Regex::new(r#"label\s*=\s*"[^"]+"\s*,\s*action\s*=\s*"([a-z][a-z0-9_.]*)""#)
        .expect("menu pattern");
    let clicked = regex::Regex::new(r#"action:([a-z][a-z0-9_.]*)"#).expect("click pattern");
    let concatenated =
        regex::Regex::new(r#"role\s*=\s*['"]action:['"]\s*\.\.\s*([A-Za-z_][A-Za-z0-9_]*)"#)
            .expect("concatenated click pattern");
    let quoted = regex::Regex::new(r#"['"]([^'"]+)['"]"#).expect("string pattern");
    let literal_values =
        regex::Regex::new(r#"^\s*['"][^'"]+['"](?:\s*,\s*['"][^'"]+['"])*\s*(?:--.*)?$"#)
            .expect("literal assignments");
    for loaded in &host.mods {
        let Ok(source) = std::fs::read_to_string(dir.join(&loaded.path)) else {
            continue;
        };
        let mut constants = std::collections::BTreeMap::new();
        for line in source.lines() {
            let Some((names, values)) = line
                .trim()
                .strip_prefix("local ")
                .and_then(|s| s.split_once('='))
            else {
                continue;
            };
            if !literal_values.is_match(values) {
                continue;
            }
            let names: Vec<_> = names.split(',').map(str::trim).collect();
            let values: Vec<_> = quoted
                .captures_iter(values)
                .map(|row| row[1].to_string())
                .collect();
            if names.len() == values.len() {
                constants.extend(names.into_iter().zip(values));
            }
        }
        let mut used: Vec<String> = menu
            .captures_iter(&source)
            .chain(clicked.captures_iter(&source))
            .map(|hit| hit[1].to_string())
            .collect();
        for hit in concatenated.captures_iter(&source) {
            let variable = &hit[1];
            if let Some(name) = constants.get(variable) {
                used.push(name.clone());
            } else {
                warnings.push(json!({
                    "file": loaded.path,
                    "warning": format!("constructed click action {variable} cannot be advertised; declare a constant action name"),
                }));
            }
        }
        for name in used {
            if name.contains('.') && !advertised.contains(name.as_str()) {
                warnings.push(json!({
                    "file": loaded.path,
                    "warning": format!("{name} is used by a menu or clickable node but has no catalog action"),
                    "action": name,
                }));
            }
        }
    }
    let mut dropped = Vec::new();
    for loaded in &host.mods {
        for (pill, reason) in crate::kernel::bands::dropped(&loaded.pills, &registry) {
            dropped.push(json!({
                "file": loaded.path,
                "label": pill.label,
                "action": pill.action,
                "reason": reason.as_str(),
                "detail": reason.explain(&pill.action),
            }));
        }
    }

    for conflict in registry.conflicts() {
        warnings.push(json!({
            "file": file_of(&host, &conflict.shadowed_mod),
            "warning": conflict.message(),
            "chord": conflict.chord,
            "shadowed_by": file_of(&host, &conflict.kept_mod),
        }));
    }
    let mut human = loads;
    for warning in &warnings {
        human.push_str(&format!(
            "\n  ! {} — {}",
            warning["file"].as_str().unwrap_or_default(),
            warning["warning"].as_str().unwrap_or_default()
        ));
        if let Some(other) = warning["shadowed_by"].as_str() {
            human.push_str(&format!(" (declared in {other})"));
        }
    }
    for pill in &dropped {
        human.push_str(&format!(
            "\n  ! {} — pill {} is not drawn: {}",
            pill["file"].as_str().unwrap_or_default(),
            pill["label"],
            pill["detail"].as_str().unwrap_or_default()
        ));
    }

    Ok(CommandOutput::new(
        json!({
            "dir": dir.display().to_string(),
            "ok": true,
            "loaded": loaded_names,
            "warnings": warnings,
            "dropped_pills": dropped,
            "checked_at": size,
        }),
        human,
    ))
}

fn declared(host: &crate::kernel::host::LuaHost) -> crate::kernel::registry::Registry {
    let mut registry = crate::kernel::registry::Registry::load();
    crate::kernel::declare_interface(&mut registry, host);
    registry
}

fn list() -> Result<CommandOutput, String> {
    let (dir, _chosen) = resolve()?;
    let host = crate::kernel::host::LuaHost::new(&dir);
    let sources = crate::kernel::bundled::sources(&dir);
    let registry = crate::kernel::registry::Registry::load();
    let lock = crate::kernel::packages::read_lock(&dir).unwrap_or_default();
    let managed: std::collections::BTreeMap<String, String> =
        crate::kernel::packages::read_spec(&dir)
            .map(|spec| {
                spec.mods
                    .iter()
                    .map(|entry| (entry.file.clone(), entry.name().to_string()))
                    .collect()
            })
            .unwrap_or_default();
    let placed = host
        .placed_slots(crate::kernel::layout::REFERENCE)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let rows = crate::kernel::inventory::rows(
        &host.mods,
        &sources,
        &Default::default(),
        &placed,
        host.error.as_deref(),
        &|path| crate::kernel::packages::trust_of(&dir, path, &lock, &registry),
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
                "installed_from": row.source.installed_from(),
                "entry": managed.get(&row.path),
                "state": row.state.as_str(),
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

fn layout_fix(host: &crate::kernel::host::LuaHost, slot: &str) -> String {
    let is_strip = host
        .mods
        .iter()
        .any(|loaded| loaded.slot == slot && loaded.strip);
    if is_strip {
        "it is a strip, and this layout.lua has no strips loop — add it before the message \
         band: for _, strip in ipairs(ctx.strips or {}) do children[#children + 1] = \
         { slot = strip.slot, len = strip.len } end"
            .to_string()
    } else {
        format!("add it to layout.lua's children: {{ slot = {slot:?} }}")
    }
}

fn placement_hint(dir: &Path, file: &str) -> Option<String> {
    let host = host_at(dir);
    let index = host.mods.iter().position(|loaded| loaded.path == file)?;
    let loaded = &host.mods[index];
    if loaded.floats || loaded.decorates.is_some() {
        return None;
    }
    let unplaced = host
        .unplaced_slots(crate::kernel::layout::REFERENCE)
        .unwrap_or_default();
    if unplaced.contains(&loaded.slot) {
        return Some(format!(
            "nothing places slot {slot:?} yet — {}",
            layout_fix(&host, &loaded.slot),
            slot = loaded.slot
        ));
    }
    if host.undiscoverable(index).is_some() {
        return Some(format!(
            "it shares the {slot:?} slot, so it is not shown by default, and ctrl+h/ctrl+l \
             pass it over — give it a key of its own, or declare a pill so the action \
             band offers it",
            slot = loaded.slot
        ));
    }
    None
}

fn delivered_panes(dir: &Path, file: &str) -> Vec<String> {
    let mut panes = vec![file.to_string()];
    if let Ok(lock) = crate::kernel::packages::read_lock(dir) {
        if let Some(entry) = lock.entry(file) {
            panes.extend(
                entry
                    .files
                    .keys()
                    .filter(|delivered| {
                        delivered.as_str() != file && !delivered.starts_with("lib/")
                    })
                    .cloned(),
            );
        }
    }
    panes
}

fn install(src: &str, as_file: Option<&str>, pin: Option<&str>) -> Result<CommandOutput, String> {
    let dir = install_dir()?;
    let report = crate::kernel::packages::install(&dir, src, as_file, pin)?;

    let panes = delivered_panes(&dir, &report.file);
    let per_pane: Vec<Option<String>> = panes
        .iter()
        .map(|pane| placement_hint(&dir, pane))
        .collect();
    let hint = per_pane.first().cloned().flatten();
    let hints: Vec<String> = per_pane.into_iter().flatten().collect();
    let repository = crate::kernel::packages::is_repository(&report.src);
    let mut human = format!(
        "{} {} from {} ({})",
        report.outcome.as_str(),
        report.file,
        report.src,
        report.version
    );
    if panes.len() > 1 {
        human.push_str(&format!(
            "\n  and {}, from the same package",
            panes[1..].join(", ")
        ));
    }
    if repository {
        human.push_str("\n  a working copy of that repository is now in your interface directory");
    }
    for hint in &hints {
        human.push_str(&format!("\n  {hint}"));
    }
    let legacy_manifest = report.legacy_manifest.then(|| {
        format!(
            "read the package's {}: it has no {} yet",
            crate::session::PackageManifest::LEGACY_FILE,
            crate::session::PackageManifest::FILE
        )
    });
    if let Some(note) = &legacy_manifest {
        human.push_str(&format!("\n  {note}"));
    }
    human.push_str("\n  `thurbox-cli mod check` to confirm it loads and draws");
    Ok(CommandOutput::new(
        json!({
            "dir": dir.display().to_string(),
            "legacy_manifest": legacy_manifest,
            "file": report.file,
            "name": report.name,
            "src": report.src,
            "version": report.version,
            "outcome": report.outcome.as_str(),
            "repository": repository,
            "panes": panes,
            "placement_hint": hint,
            "placement_hints": hints,
            "summary": format!("{} {}", report.outcome.as_str(), report.file),
        }),
        human,
    ))
}

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
    let mods = crate::kernel::packages::EXAMPLE_MODS;
    let table = super::output::table(
        &["name", "description"],
        &mods
            .iter()
            .map(|(name, description)| vec![(*name).to_string(), (*description).to_string()])
            .collect::<Vec<_>>(),
    );
    Ok(CommandOutput::new(
        json!({
            "plugins": mods
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

    #[test]
    fn check_refuses_a_subscription_to_an_unknown_event() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mods = dir.path().join("mods");
        std::fs::create_dir_all(&mods).expect("mkdir");
        std::fs::write(
            mods.join("10_typo.lua"),
            r#"return {
                 name = "typo", slot = "center",
                 events = { "sesion.status" },
                 on_event = function() end,
                 render = function() return { text = "" } end,
               }"#,
        )
        .expect("write");
        // nextest runs each test in its own process, so this env var reaches nothing else.
        std::env::set_var("THURBOX_UI_DIR", dir.path());
        let output = check().expect("check runs");
        std::env::remove_var("THURBOX_UI_DIR");
        assert!(
            output.failure.is_some(),
            "an unknown subscription must fail the check"
        );
        let error = output.json["error"].as_str().unwrap_or_default();
        assert!(error.contains("sesion.status"), "{error}");
        assert!(error.contains("session.status"), "{error}");
    }

    #[test]
    fn the_starter_declares_what_a_mod_needs() {
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

    #[test]
    fn a_starter_is_named_to_load_after_the_bundled_panes() {
        // Windows joins path components with a backslash.
        let file = std::path::Path::new("mods").join(format!("{STARTER_ORDER}_notes.lua"));
        assert_eq!(file.parent().and_then(|p| p.to_str()), Some("mods"));
        assert_eq!(
            file.file_name().and_then(|n| n.to_str()),
            Some("90_notes.lua")
        );
    }
}
