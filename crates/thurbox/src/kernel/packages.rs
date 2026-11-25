use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::session::mod_spec::{self, LockEntry, ModEntry, ModLock, ModSpec, LOCK_FILE, SPEC_FILE};
use crate::session::PackageManifest;

use super::bundled::{decide, digest, Action, Delivered};

pub fn spec_path(dir: &Path) -> PathBuf {
    dir.join(SPEC_FILE)
}

pub fn lock_path(dir: &Path) -> PathBuf {
    dir.join(LOCK_FILE)
}

pub fn read_spec(dir: &Path) -> Result<ModSpec, String> {
    match std::fs::read_to_string(spec_path(dir)) {
        Ok(text) => ModSpec::parse(&text).map_err(|e| format!("{SPEC_FILE}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ModSpec::default()),
        Err(e) => Err(format!("{}: {e}", spec_path(dir).display())),
    }
}

pub fn read_spec_text(dir: &Path) -> Result<String, String> {
    match std::fs::read_to_string(spec_path(dir)) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("{}: {e}", spec_path(dir).display())),
    }
}

pub fn read_lock(dir: &Path) -> Result<ModLock, String> {
    match std::fs::read_to_string(lock_path(dir)) {
        Ok(text) => ModLock::parse(&text).map_err(|e| format!("{LOCK_FILE}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ModLock::default()),
        Err(e) => Err(format!("{}: {e}", lock_path(dir).display())),
    }
}

pub fn write_lock(dir: &Path, lock: &ModLock) -> Result<(), String> {
    // An emptied lock is removed rather than left as an empty table, so a
    // directory with nothing managed looks like one.
    if lock.mods.is_empty() {
        return match std::fs::remove_file(lock_path(dir)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{}: {e}", lock_path(dir).display())),
        };
    }
    let text = lock.to_toml()?;
    crate::paths::write_atomically(&lock_path(dir), text)
        .map_err(|e| format!("{}: {e}", lock_path(dir).display()))
}

pub fn add_to_spec(dir: &Path, entry: &ModEntry) -> Result<(), String> {
    let text = mod_spec::insert_entry(&read_spec_text(dir)?, entry)?;
    write_spec(dir, &text)
}

pub fn remove_from_spec(dir: &Path, key: &str) -> Result<ModEntry, String> {
    let (text, removed) = mod_spec::remove_entry(&read_spec_text(dir)?, key)?;
    write_spec(dir, &text)?;
    Ok(removed)
}

fn write_spec(dir: &Path, text: &str) -> Result<(), String> {
    crate::paths::write_atomically(&spec_path(dir), text)
        .map_err(|e| format!("{}: {e}", spec_path(dir).display()))
}

pub struct Payload {
    pub file: String,
    pub contents: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Installed,
    Updated,
    Current,
    Kept,
    Deleted,
    Removed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Installed => "installed",
            Outcome::Updated => "updated",
            Outcome::Current => "current",
            Outcome::Kept => "kept",
            Outcome::Deleted => "deleted",
            Outcome::Removed => "removed",
        }
    }

    pub fn changed(self) -> bool {
        matches!(
            self,
            Outcome::Installed | Outcome::Updated | Outcome::Removed
        )
    }
}

pub fn deliver(
    dir: &Path,
    entry: &ModEntry,
    resolved: &str,
    version: &str,
    payloads: &[Payload],
    lock: &mut ModLock,
) -> Result<Outcome, String> {
    entry.validate()?;
    for payload in payloads {
        mod_spec::validate_destination(&payload.file)?;
        refuse_a_pane_owned_elsewhere(entry, payload, lock)?;
    }

    let (recorded, tombstoned): (BTreeMap<String, String>, Vec<String>) = lock
        .entry(&entry.file)
        .map(|existing| (existing.files.clone(), existing.removed.clone()))
        .unwrap_or_default();

    let mut files = BTreeMap::new();
    let mut removed = Vec::new();
    // Reported as the strongest thing that happened: a package whose pane was
    // updated and whose module was already current is an update.
    let mut outcome = Outcome::Current;

    for payload in payloads {
        let (effect, candidate) = deliver_payload(dir, payload, &recorded, &tombstoned)?;
        match effect {
            Effect::Recorded(digest) => {
                files.insert(payload.file.clone(), digest);
            }
            // The record is carried through UNCHANGED — see `Action::Preserve`.
            Effect::Unchanged => {
                if let Some(recorded) = recorded.get(&payload.file) {
                    files.insert(payload.file.clone(), recorded.clone());
                }
            }
            Effect::Tombstoned => removed.push(payload.file.clone()),
        }
        escalate(&mut outcome, candidate);
    }

    lock.record(LockEntry {
        src: entry.src.clone(),
        file: entry.file.clone(),
        resolved: resolved.to_string(),
        version: version.to_string(),
        files,
        removed,
    });
    Ok(outcome)
}

fn refuse_a_pane_owned_elsewhere(
    entry: &ModEntry,
    payload: &Payload,
    lock: &ModLock,
) -> Result<(), String> {
    if payload.file.starts_with("lib/") {
        return Ok(());
    }
    match lock.covering(&payload.file) {
        Some(owner) if owner.file != entry.file => Err(format!(
            "{} is already delivered by the entry for {} — remove that one first to \
             key the package on a different pane",
            payload.file, owner.file
        )),
        _ => Ok(()),
    }
}

enum Effect {
    Recorded(String),
    Unchanged,
    Tombstoned,
}

fn deliver_payload(
    dir: &Path,
    payload: &Payload,
    recorded: &BTreeMap<String, String>,
    tombstoned: &[String],
) -> Result<(Effect, Outcome), String> {
    let path = dir.join(&payload.file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let existing = std::fs::read_to_string(&path).ok();
    let delivered = match recorded.get(&payload.file) {
        Some(recorded) => Delivered::Digest(recorded.as_str()),
        None if tombstoned.contains(&payload.file) => Delivered::Tombstoned,
        None => Delivered::Never,
    };

    match decide(existing.as_deref(), delivered, &payload.contents) {
        action @ (Action::Write | Action::Update) => {
            crate::paths::write_atomically(&path, &payload.contents)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let outcome = match action == Action::Write {
                true => Outcome::Installed,
                false => Outcome::Updated,
            };
            Ok((Effect::Recorded(digest(&payload.contents)), outcome))
        }
        Action::Settle => Ok((
            Effect::Recorded(digest(&payload.contents)),
            Outcome::Current,
        )),
        // Theirs. The record is carried through UNCHANGED — recording the digest
        // now on disk would make the user's edit look like our own delivery, and
        // the very next run would decide it was safe to overwrite.
        // `bundled::materialize` leaves the manifest alone on `Preserve` for
        // exactly this reason; "the last thing we wrote" is what makes an edit
        // detectable, so it must not be overwritten with what the user wrote.
        Action::Preserve => Ok((Effect::Unchanged, Outcome::Kept)),
        // Deleted, and recorded as such so the next run leaves it alone. Written
        // out rather than inferred from a missing digest, which is also what a
        // newly-added file looks like.
        Action::Tombstone | Action::Leave => Ok((Effect::Tombstoned, Outcome::Deleted)),
    }
}

fn escalate(current: &mut Outcome, candidate: Outcome) {
    let rank = |outcome: Outcome| match outcome {
        Outcome::Current => 0,
        Outcome::Updated => 1,
        Outcome::Installed => 2,
        Outcome::Deleted => 3,
        Outcome::Kept => 4,
        Outcome::Removed => 5,
    };
    if rank(candidate) > rank(*current) {
        *current = candidate;
    }
}

pub fn withdraw(dir: &Path, entry: &LockEntry) -> Result<Vec<(String, Outcome)>, String> {
    let mut report = Vec::new();
    for (file, recorded) in &entry.files {
        let path = dir.join(file);
        match std::fs::read_to_string(&path) {
            Ok(current) if digest(&current) == *recorded => {
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                prune_empty(dir, &path);
                report.push((file.clone(), Outcome::Removed));
            }
            // Theirs now — it outlived the entry that delivered it.
            Ok(_) => report.push((file.clone(), Outcome::Kept)),
            Err(_) => report.push((file.clone(), Outcome::Deleted)),
        }
    }
    Ok(report)
}

fn prune_empty(dir: &Path, removed: &Path) {
    let mut at = removed.parent();
    while let Some(parent) = at {
        if parent == dir || !parent.starts_with(dir) {
            return;
        }
        // `remove_dir` fails on a non-empty directory, which is the test — no
        // separate emptiness check to race against.
        if std::fs::remove_dir(parent).is_err() {
            return;
        }
        at = parent.parent();
    }
}

pub const EXAMPLE_SET: &str = "examples/panes";

pub const EXAMPLE_MODS: &[(&str, &str)] = &[
    ("tasks", "A todo pane: read the snapshot, send or spawn"),
    ("top", "CPU, memory and load as gauges, parsed from `top`"),
];

pub struct Resolved {
    pub source: crate::agent::fetch::Source,
    pub at: String,
    pub version: String,
}

pub fn resolve_source(src: &str, pin: Option<&str>) -> Resolved {
    let bare = mod_spec::is_bare_name(src);
    let version = match (pin, bare) {
        (Some(pin), _) => pin.to_string(),
        // A bare name is fetched at the binary's release tag, so that IS the
        // version of a freshly-installed one.
        (None, true) => crate::agent::fetch::official_ref(),
        (None, false) => String::new(),
    };
    let source = if bare {
        crate::agent::fetch::Source::Remote(format!(
            "{}/{src}",
            crate::agent::fetch::official_set_base_at(EXAMPLE_SET, &version)
        ))
    } else {
        crate::agent::fetch::resolve_source_in(src, EXAMPLE_SET)
    };
    let at = match &source {
        crate::agent::fetch::Source::Remote(base) => base.clone(),
        crate::agent::fetch::Source::Local(dir) => dir.display().to_string(),
        crate::agent::fetch::Source::Git(url) => url.clone(),
    };
    Resolved {
        source,
        at,
        version,
    }
}

pub struct Fetched {
    pub manifest: Option<crate::session::PackageManifest>,
    pub payloads: Vec<Payload>,
    pub legacy_manifest: bool,
}

pub fn fetch(src: &str, resolved: &Resolved, as_file: Option<&str>) -> Result<Fetched, String> {
    let as_file = as_file.map(mod_spec::mod_destination);
    let as_file = as_file.as_deref();
    if src.trim().ends_with(".lua") {
        let file = as_file
            .ok_or_else(|| {
                format!("{src} is a single file, so it needs a destination: --as mods/90_x.lua")
            })?
            .to_string();
        mod_spec::validate_destination(&file)?;
        // Fetched relative to the *parent*, since the source names the file itself.
        let (base, name) = split_last(src);
        let source = match &resolved.source {
            crate::agent::fetch::Source::Remote(_) => crate::agent::fetch::Source::Remote(base),
            crate::agent::fetch::Source::Local(_) => {
                crate::agent::fetch::Source::Local(crate::agent::fetch::expand_tilde(&base))
            }
            // A `.lua` source cannot also be a repository: `git_url` matched first,
            // so a `git+…/x.lua` never reaches here.
            crate::agent::fetch::Source::Git(url) => crate::agent::fetch::Source::Git(url.clone()),
        };
        let contents = crate::agent::fetch::fetch_file(&source, &name)?;
        return Ok(Fetched {
            manifest: None,
            payloads: vec![Payload { file, contents }],
            legacy_manifest: false,
        });
    }

    let (manifest_text, legacy_manifest) =
        match crate::agent::fetch::fetch_file(&resolved.source, PackageManifest::FILE) {
            Ok(text) => (text, false),
            Err(e) => {
                crate::agent::fetch::fetch_file(&resolved.source, PackageManifest::LEGACY_FILE)
                    .map(|text| (text, true))
                    .map_err(|_| {
                        format!(
                            "{src}: no {} or {} at {} ({e})",
                            PackageManifest::FILE,
                            PackageManifest::LEGACY_FILE,
                            resolved.at
                        )
                    })?
            }
        };
    let manifest_file = match legacy_manifest {
        true => PackageManifest::LEGACY_FILE,
        false => PackageManifest::FILE,
    };
    let manifest = PackageManifest::parse(&manifest_text)
        .map_err(|e| format!("{src}: {manifest_file}: {e}"))?;

    let mut payloads = Vec::new();
    for (destination, file) in deliveries(&manifest, as_file)? {
        mod_spec::validate_destination(&destination)?;
        payloads.push(Payload {
            file: destination,
            contents: crate::agent::fetch::fetch_file(&resolved.source, &file.source)?,
        });
    }
    Ok(Fetched {
        manifest: Some(manifest),
        payloads,
        legacy_manifest,
    })
}

fn deliveries<'a>(
    manifest: &'a crate::session::PackageManifest,
    as_file: Option<&str>,
) -> Result<Vec<(String, &'a crate::session::PackageFile)>, String> {
    let mut files: Vec<(String, &crate::session::PackageFile)> = match manifest.panes.as_slice() {
        [only] => vec![(as_file.unwrap_or(&only.path).to_string(), only)],
        several => {
            let mut ordered: Vec<(String, &crate::session::PackageFile)> = several
                .iter()
                .map(|pane| (pane.path.clone(), pane))
                .collect();
            if let Some(selected) = as_file {
                let index = several
                    .iter()
                    .position(|pane| pane.path == selected)
                    .ok_or_else(|| pick_a_pane(selected, several))?;
                // Moved to the front rather than swapped, so the rest keep the order
                // their author declared them in.
                let chosen = ordered.remove(index);
                ordered.insert(0, chosen);
            }
            ordered
        }
    };
    files.extend(
        manifest
            .modules
            .iter()
            .map(|module| (module.path.clone(), module)),
    );
    Ok(files)
}

fn pick_a_pane(given: &str, panes: &[crate::session::PackageFile]) -> String {
    format!(
        "the package carries {} panes and none of them is {given} — an entry is keyed \
         on one of {}, which --as selects rather than moves",
        panes.len(),
        panes
            .iter()
            .map(|pane| pane.path.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn split_last(src: &str) -> (String, String) {
    let src = src.trim().trim_end_matches('/');
    match src.rsplit_once(['/', '\\']) {
        Some((base, name)) => (base.to_string(), name.to_string()),
        None => (".".to_string(), src.to_string()),
    }
}

pub fn trust_of(
    dir: &Path,
    relative: &str,
    lock: &ModLock,
    registry: &super::registry::Registry,
) -> super::inventory::Trust {
    let absolute = dir.join(relative);
    let key = absolute.to_string_lossy();
    let text = std::fs::read_to_string(&absolute).ok();
    let current = text.as_deref().map(digest);
    let shipped = text.as_deref().is_some_and(|text| {
        super::bundled::BUNDLED
            .iter()
            .any(|(name, contents)| *name == relative && *contents == text)
    });

    match lock.covering(relative) {
        Some(entry) => super::inventory::Trust::resolve_installed(
            registry
                .trusted_pin(&key)
                .zip(registry.trusted_digest(&key)),
            &entry.pin_key(),
            current.as_deref(),
        ),
        None if shipped => super::inventory::Trust::Shipped,
        None if registry.trusted_pin(&key).is_some() => super::inventory::Trust::Untrusted,
        None => super::inventory::Trust::resolve(registry.trusted_digest(&key), current.as_deref()),
    }
}

pub fn granted<'a>(
    dir: &Path,
    files: impl IntoIterator<Item = &'a str>,
    registry: &super::registry::Registry,
) -> Vec<String> {
    let lock = read_lock(dir).unwrap_or_default();
    files
        .into_iter()
        .filter(|file| trust_of(dir, file, &lock, registry).grants())
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryReport {
    pub name: String,
    pub file: String,
    pub src: String,
    pub version: String,
    pub from: Option<String>,
    pub legacy_manifest: bool,
    pub outcome: Outcome,
}

pub fn is_repository(src: &str) -> bool {
    crate::agent::fetch::git_url(src).is_some()
}

pub fn repository_name(url: &str) -> Result<String, String> {
    let last = last_segment(url, cfg!(windows));
    if last.is_empty() {
        return Err(format!("could not read a mod name out of {url}"));
    }
    crate::paths::validate_safe_name(last)
        .map_err(|e| format!("{url} names {last:?}, which is not usable as a directory: {e}"))?;
    Ok(last.to_string())
}

fn last_segment(source: &str, windows: bool) -> &str {
    let separators: &[char] = match windows {
        true => &['/', ':', '\\'],
        false => &['/', ':'],
    };
    let mut trimmed = source.trim().trim_end_matches('/');
    if windows {
        trimmed = trimmed.trim_end_matches('\\');
    }
    trimmed
        .rsplit(separators)
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git")
}

fn pane_in_working_copy(
    root: &Path,
    name: &str,
    as_file: Option<&str>,
) -> Result<(String, bool), String> {
    if let Some(rel) = as_file {
        let rel = rel.trim_start_matches('/');
        // Accept either `mods/40_x.lua` (inside the repo) or the full
        // `<name>/mods/40_x.lua`, since both readings are natural.
        let rel = rel.strip_prefix(&format!("{name}/")).unwrap_or(rel);
        let file = format!("{name}/{rel}");
        mod_spec::validate_destination(&file)?;
        if !root.join(rel).is_file() {
            return Err(format!("{rel} is not in the repository"));
        }
        return Ok((file, false));
    }

    let manifest = [PackageManifest::FILE, PackageManifest::LEGACY_FILE]
        .into_iter()
        .find_map(|file| Some((file, std::fs::read_to_string(root.join(file)).ok()?)));
    if let Some((manifest_file, text)) = manifest {
        let sources = mod_spec::pane_sources_of(&text);
        if sources.len() > 1 {
            let named: Vec<&str> = sources
                .iter()
                .map(|source| source.as_deref().unwrap_or("one with no source"))
                .collect();
            return Err(format!(
                "{manifest_file} names {} panes ({}) — name the one to load: --as <dir>/<file>",
                sources.len(),
                named.join(", ")
            ));
        }
        if let Some(Some(source)) = sources.first() {
            let file = format!("{name}/{}", source.trim_start_matches('/'));
            mod_spec::validate_destination(&file)?;
            if !root.join(source).is_file() {
                return Err(format!(
                    "{manifest_file} names {source} as its pane, which is not in the repository"
                ));
            }
            return Ok((file, manifest_file == PackageManifest::LEGACY_FILE));
        }
    }

    let Some((panes_dir, entries)) = [mod_spec::MODS_DIR, mod_spec::LEGACY_MODS_DIR]
        .into_iter()
        .find_map(|dir| Some((dir, std::fs::read_dir(root.join(dir)).ok()?)))
    else {
        return Err(
            "the repository has no mods/ or plugins/ directory, so name its pane: \
             --as mods/NN_name.lua"
                .to_string(),
        );
    };
    let mut found: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".lua"))
        .collect();
    found.sort();
    match found.len() {
        1 => Ok((format!("{name}/{panes_dir}/{}", found[0]), false)),
        0 => Err(format!(
            "the repository's {panes_dir}/ directory holds no Lua"
        )),
        _ => Err(format!(
            "the repository has more than one pane ({}) — name the one to load: \
             --as {panes_dir}/<file>",
            found.join(", ")
        )),
    }
}

fn install_repository(
    dir: &Path,
    src: &str,
    url: &str,
    as_file: Option<&str>,
    pin: Option<&str>,
    spec: &ModSpec,
    lock: &mut ModLock,
) -> Result<EntryReport, String> {
    let name = repository_name(url)?;
    let root = dir.join(&name);

    // Occupied by something this spec does not manage: not ours to replace.
    if root.exists()
        && !spec
            .mods
            .iter()
            .any(|entry| entry.file.starts_with(&format!("{name}/")))
    {
        return Err(format!(
            "{} already exists and is not managed here — move it aside, or remove it first",
            name
        ));
    }
    if root.exists() {
        std::fs::remove_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    }

    crate::git::clone_mod(url, &root, pin).map_err(|e| format!("{e:#}"))?;
    let commit = crate::git::head_commit(&root).map_err(|e| format!("{e:#}"))?;

    let (file, legacy_manifest) =
        pane_in_working_copy(&root, &name, as_file).inspect_err(|_| {
            // A clone whose pane cannot be identified is not an install: take it back so
            // a second attempt with `--as` starts clean.
            let _ = std::fs::remove_dir_all(&root);
        })?;

    let entry = ModEntry {
        src: src.to_string(),
        file: file.clone(),
        pin: pin.map(str::to_string),
    };
    // Only the pane is digested. The rest of a repository is its own business, and a
    // per-file record of it would be thousands of rows the manager never consults —
    // git owns that working copy.
    let mut files = BTreeMap::new();
    if let Ok(contents) = std::fs::read_to_string(dir.join(&file)) {
        files.insert(file.clone(), digest(&contents));
    }
    lock.record(LockEntry {
        src: src.to_string(),
        file: file.clone(),
        resolved: url.to_string(),
        version: commit.clone(),
        files,
        removed: Vec::new(),
    });
    write_lock(dir, lock)?;
    add_to_spec(dir, &entry)?;

    Ok(EntryReport {
        name: entry.name().to_string(),
        file,
        src: src.to_string(),
        version: commit,
        from: None,
        legacy_manifest,
        outcome: Outcome::Installed,
    })
}

pub fn install(
    dir: &Path,
    src: &str,
    as_file: Option<&str>,
    pin: Option<&str>,
) -> Result<EntryReport, String> {
    let spec = read_spec(dir)?;
    let mut lock = read_lock(dir)?;

    let resolved = resolve_source(src, pin);
    // A repository is obtained as a working copy; everything else is fetched file by
    // file. The source's own form chose this (`fetch::git_url`).
    if let crate::agent::fetch::Source::Git(url) = &resolved.source {
        let url = url.clone();
        return install_repository(dir, src, &url, as_file, pin, &spec, &mut lock);
    }
    let fetched = fetch(src, &resolved, as_file).map_err(|e| not_found_help(src, e))?;
    let file = fetched
        .payloads
        .first()
        .map(|payload| payload.file.clone())
        .ok_or_else(|| format!("{src}: delivers no files"))?;

    for payload in &fetched.payloads {
        let path = dir.join(&payload.file);
        if path.exists() && !spec.manages(&payload.file) && lock.covering(&payload.file).is_none() {
            return Err(format!(
                "{} already exists and is not managed here — move it aside, or \
                 install with --as to a different file",
                payload.file
            ));
        }
    }

    let version = version_of(&resolved, fetched.manifest.as_ref());
    let entry = ModEntry {
        src: src.to_string(),
        file: file.clone(),
        pin: pin.map(str::to_string),
    };
    let outcome = deliver(
        dir,
        &entry,
        &resolved.at,
        &version,
        &fetched.payloads,
        &mut lock,
    )?;
    write_lock(dir, &lock)?;
    add_to_spec(dir, &entry)?;

    Ok(EntryReport {
        name: entry.name().to_string(),
        file,
        src: src.to_string(),
        version,
        from: None,
        legacy_manifest: fetched.legacy_manifest,
        outcome,
    })
}

fn converge_repository(
    dir: &Path,
    entry: &ModEntry,
    url: &str,
    advance: bool,
    lock: &mut ModLock,
) -> Result<(Outcome, String, Option<String>), String> {
    let name = repository_name(url)?;
    let root = dir.join(&name);
    let recorded = lock.entry(&entry.file).map(|e| e.version.clone());

    // Absent: a spec that cannot be applied on a fresh machine defeats the lock, so
    // convergence clones it.
    if !crate::git::is_working_copy(&root) {
        if root.exists() {
            return Err(format!(
                "{name} exists but is not a working copy — move it aside"
            ));
        }
        crate::git::clone_mod(url, &root, entry.pin.as_deref()).map_err(|e| format!("{e:#}"))?;
        // Reproducing a recorded commit: a shallow clone has only the tip, so the
        // exact object has to be asked for.
        if let Some(commit) = recorded.as_deref().filter(|c| !c.is_empty()) {
            if crate::git::head_commit(&root).map_err(|e| format!("{e:#}"))? != commit {
                crate::git::fetch_ref(&root, commit).map_err(|e| format!("{e:#}"))?;
                crate::git::checkout_mod(&root, commit).map_err(|e| format!("{e:#}"))?;
            }
        }
        let commit = crate::git::head_commit(&root).map_err(|e| format!("{e:#}"))?;
        return Ok((Outcome::Installed, commit, None));
    }

    let head = crate::git::head_commit(&root).map_err(|e| format!("{e:#}"))?;

    // Modified locally: never moved, and reported so rather than completing
    // silently. Checked before any fetch, so a dirty copy is not even touched.
    if crate::git::is_dirty(&root).map_err(|e| format!("{e:#}"))? {
        return Ok((Outcome::Kept, head, None));
    }

    if !advance {
        // Convergence holds at the recorded commit rather than following a branch.
        match recorded.as_deref().filter(|c| !c.is_empty()) {
            Some(commit) if commit != head => {
                crate::git::fetch_ref(&root, commit).map_err(|e| format!("{e:#}"))?;
                crate::git::checkout_mod(&root, commit).map_err(|e| format!("{e:#}"))?;
                return Ok((Outcome::Updated, commit.to_string(), Some(head)));
            }
            _ => return Ok((Outcome::Current, head, None)),
        }
    }

    // Advancing follows the entry's own ref: a pinned branch moves, a pinned tag or
    // commit does not, and an unpinned entry follows the remote's default branch.
    // What is checked out is what the fetch returned — never the ref's name, since
    // a fetch does not move the local branch a `--branch` clone checked out, and a
    // pinned commit's object is not there until it is asked for by id.
    match entry.pin.as_deref() {
        Some(pin) => crate::git::fetch_ref(&root, pin).map_err(|e| format!("{e:#}"))?,
        None => crate::git::fetch_tip(&root).map_err(|e| format!("{e:#}"))?,
    }
    crate::git::checkout_mod(&root, "FETCH_HEAD").map_err(|e| format!("{e:#}"))?;
    let moved = crate::git::head_commit(&root).map_err(|e| format!("{e:#}"))?;
    if moved == head {
        return Ok((Outcome::Current, head, None));
    }
    Ok((Outcome::Updated, moved, Some(head)))
}

fn record_repository(lock: &mut ModLock, dir: &Path, entry: &ModEntry, url: &str, commit: &str) {
    let mut files = BTreeMap::new();
    if let Ok(contents) = std::fs::read_to_string(dir.join(&entry.file)) {
        files.insert(entry.file.clone(), digest(&contents));
    }
    lock.record(LockEntry {
        src: entry.src.clone(),
        file: entry.file.clone(),
        resolved: url.to_string(),
        version: commit.to_string(),
        files,
        removed: Vec::new(),
    });
}

pub fn sync(dir: &Path) -> Result<Vec<EntryReport>, String> {
    let spec = read_spec(dir)?;
    let mut lock = read_lock(dir)?;
    // Withdrawn first, so a spec that moves a pane from one file to another does
    // not have the old file taken back after the new one is written.
    let mut reports = withdraw_stale(dir, &spec, &mut lock, |_| true)?;

    for entry in &spec.mods {
        if let Some(url) = crate::agent::fetch::git_url(&entry.src) {
            let (outcome, commit, from) = converge_repository(dir, entry, &url, false, &mut lock)?;
            record_repository(&mut lock, dir, entry, &url, &commit);
            reports.push(EntryReport {
                name: entry.name().to_string(),
                file: entry.file.clone(),
                src: entry.src.clone(),
                version: commit,
                from,
                legacy_manifest: false,
                outcome,
            });
            continue;
        }
        let recorded = lock.entry(&entry.file).map(|entry| entry.version.clone());
        let pin = entry.pin.as_deref().or(recorded.as_deref());
        let resolved = resolve_source(&entry.src, pin);
        let fetched = fetch(&entry.src, &resolved, Some(&entry.file))
            .map_err(|e| not_found_help(&entry.src, e))?;
        let version = version_of(&resolved, fetched.manifest.as_ref());
        let outcome = deliver(
            dir,
            entry,
            &resolved.at,
            &version,
            &fetched.payloads,
            &mut lock,
        )?;
        reports.push(EntryReport {
            name: entry.name().to_string(),
            file: entry.file.clone(),
            src: entry.src.clone(),
            version,
            from: None,
            legacy_manifest: false,
            outcome,
        });
    }

    write_lock(dir, &lock)?;
    Ok(reports)
}

fn withdraw_stale(
    dir: &Path,
    spec: &ModSpec,
    lock: &mut ModLock,
    wanted: impl Fn(&LockEntry) -> bool,
) -> Result<Vec<EntryReport>, String> {
    let stale: Vec<LockEntry> = lock
        .beyond(spec)
        .into_iter()
        .filter(|entry| wanted(entry))
        .cloned()
        .collect();
    let mut reports = Vec::new();
    for stale in stale {
        withdraw(dir, &stale)?;
        lock.forget(&stale.file);
        reports.push(EntryReport {
            name: stale.file.clone(),
            file: stale.file.clone(),
            src: stale.src.clone(),
            version: stale.version.clone(),
            from: None,
            legacy_manifest: false,
            outcome: Outcome::Removed,
        });
    }
    Ok(reports)
}

pub fn update(dir: &Path, key: Option<&str>) -> Result<Vec<EntryReport>, String> {
    let spec = read_spec(dir)?;
    let mut lock = read_lock(dir)?;

    let targets: Vec<&ModEntry> = match key {
        Some(key) => vec![spec
            .find(key)
            .ok_or_else(|| format!("{key} is not listed in {SPEC_FILE}"))?],
        None => spec.mods.iter().collect(),
    };
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    // Only a record that is provably a target's own leftover from a hand re-key:
    // same source, covering the target's key, and covering no other live entry's
    // key. The last condition is what makes it the target's — a re-keyed entry's old
    // record also covers that entry's new key, and taking it back from under an
    // update that does not deliver that entry would leave it without its files.
    // Copied packages only: a repository's record names a file inside a working
    // copy git owns.
    let owned_by = |stale: &LockEntry, target: &ModEntry| {
        stale.src == target.src
            && stale.files.contains_key(&target.file)
            && !spec
                .mods
                .iter()
                .any(|other| other.file != target.file && stale.files.contains_key(&other.file))
    };
    let mut reports = withdraw_stale(dir, &spec, &mut lock, |stale| {
        !is_repository(&stale.src) && targets.iter().any(|target| owned_by(stale, target))
    })?;
    for entry in targets {
        if let Some(url) = crate::agent::fetch::git_url(&entry.src) {
            let (outcome, commit, from) = converge_repository(dir, entry, &url, true, &mut lock)?;
            record_repository(&mut lock, dir, entry, &url, &commit);
            reports.push(EntryReport {
                name: entry.name().to_string(),
                file: entry.file.clone(),
                src: entry.src.clone(),
                version: commit,
                from,
                legacy_manifest: false,
                outcome,
            });
            continue;
        }
        let was = lock.entry(&entry.file).map(|entry| entry.version.clone());
        // A pin is the user's statement about which version to run; an update
        // resolves at it rather than past it.
        let resolved = resolve_source(&entry.src, entry.pin.as_deref());
        let fetched = fetch(&entry.src, &resolved, Some(&entry.file))
            .map_err(|e| not_found_help(&entry.src, e))?;
        let version = version_of(&resolved, fetched.manifest.as_ref());
        let outcome = deliver(
            dir,
            entry,
            &resolved.at,
            &version,
            &fetched.payloads,
            &mut lock,
        )?;
        reports.push(EntryReport {
            name: entry.name().to_string(),
            file: entry.file.clone(),
            src: entry.src.clone(),
            from: was.filter(|was| *was != version),
            legacy_manifest: false,
            version,
            outcome,
        });
    }

    write_lock(dir, &lock)?;
    Ok(reports)
}

pub fn remove(dir: &Path, key: &str) -> Result<EntryReport, String> {
    let removed = remove_from_spec(dir, key)?;
    let mut lock = read_lock(dir)?;
    let record = lock.forget(&removed.file);

    // A repository is one directory, not a file list: taking back the files the lock
    // happens to name would leave the working copy and its `.git` behind.
    if let Some(url) = crate::agent::fetch::git_url(&removed.src) {
        let name = repository_name(&url)?;
        let root = dir.join(&name);
        if root.exists() {
            std::fs::remove_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
        }
        write_lock(dir, &lock)?;
        return Ok(EntryReport {
            name: removed.name().to_string(),
            file: removed.file.clone(),
            src: removed.src.clone(),
            version: record.map(|record| record.version).unwrap_or_default(),
            from: None,
            legacy_manifest: false,
            outcome: Outcome::Removed,
        });
    }

    if let Some(record) = &record {
        withdraw(dir, record)?;
    } else {
        // Listed in the spec but never delivered — nothing on disk to take back,
        // and the entry is gone either way.
        let path = dir.join(&removed.file);
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    write_lock(dir, &lock)?;

    Ok(EntryReport {
        name: removed.name().to_string(),
        file: removed.file.clone(),
        src: removed.src.clone(),
        version: record.map(|record| record.version).unwrap_or_default(),
        from: None,
        legacy_manifest: false,
        outcome: Outcome::Removed,
    })
}

fn version_of(resolved: &Resolved, manifest: Option<&crate::session::PackageManifest>) -> String {
    if !resolved.version.is_empty() {
        return resolved.version.clone();
    }
    manifest
        .and_then(|manifest| manifest.version.clone())
        .unwrap_or_else(|| "unpinned".to_string())
}

fn not_found_help(src: &str, error: String) -> String {
    if !mod_spec::is_bare_name(src) {
        return error;
    }
    // The fetcher's own message is two downloaders' stderr and a raw URL, which
    // buries the one thing a mistyped name needs. It is REPLACED rather than
    // appended to: this is the common failure, and the answer to it is short.
    let mut message = format!("no example mod named {src:?}");
    if let Some(suggestion) = suggest_mod(src) {
        message.push_str(&format!(" — did you mean {suggestion:?}?"));
    }
    let available: Vec<&str> = EXAMPLE_MODS.iter().map(|(name, _)| *name).collect();
    message.push_str(&format!("\n  available: {}", available.join(", ")));
    // Kept, indented, because a bare name can also fail for reasons that are not
    // a typo at all — no network, a proxy, a broken package upstream — and
    // swallowing the cause would make those undiagnosable.
    message.push_str(&format!("\n  ({})", error.replace('\n', " ")));
    message
}

pub fn suggest_mod(name: &str) -> Option<&'static str> {
    let name = name.trim().to_lowercase();
    let budget = (name.len() / 3).clamp(2, 3);
    EXAMPLE_MODS
        .iter()
        .map(|(official, _)| (*official, edit_distance(&name, official)))
        .filter(|(_, distance)| *distance <= budget)
        .min_by_key(|(_, distance)| *distance)
        .map(|(official, _)| official)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ac) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, bc) in b.iter().enumerate() {
            let cost = usize::from(ac != bc);
            current.push(
                (previous[j] + cost)
                    .min(previous[j + 1] + 1)
                    .min(current[j] + 1),
            );
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_names_its_directory_on_either_platform() {
        // A remote URL reads the same either way.
        for windows in [true, false] {
            for url in [
                "https://github.com/you/thurbox-widget",
                "https://github.com/you/thurbox-widget.git",
                "https://github.com/you/thurbox-widget/",
                "git@github.com:you/thurbox-widget.git",
            ] {
                assert_eq!(last_segment(url, windows), "thurbox-widget", "{url}");
            }
            assert_eq!(
                last_segment("/home/me/src/thurbox-widget", windows),
                "thurbox-widget"
            );
        }

        // A local Windows path: the drive letter's colon already split, but the
        // backslashes did not, so the whole tail was taken as one name and refused.
        for path in [
            r"D:\a\thurbox\thurbox-widget",
            r"D:\a\thurbox\thurbox-widget\",
            r"\\server\share\thurbox-widget",
            r"C:\Users\me\thurbox-widget.git",
        ] {
            assert_eq!(last_segment(path, true), "thurbox-widget", "{path}");
        }

        // And on POSIX a backslash stays part of the name: it is legal there, so
        // splitting on it would rename somebody's directory.
        assert_eq!(last_segment(r"/home/me/odd\name", false), r"odd\name");
    }

    fn entry(file: &str) -> ModEntry {
        ModEntry {
            src: "atlas".into(),
            file: file.into(),
            pin: Some("v1".into()),
        }
    }

    fn payload(file: &str, contents: &str) -> Payload {
        Payload {
            file: file.into(),
            contents: contents.into(),
        }
    }

    #[test]
    fn a_missing_spec_is_an_interface_with_nothing_installed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = read_spec(dir.path()).expect("no spec is not a failure");
        assert!(spec.mods.is_empty());
        assert!(read_lock(dir.path())
            .expect("no lock either")
            .mods
            .is_empty());
    }

    #[test]
    fn a_malformed_spec_names_the_file_and_the_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(spec_path(dir.path()), "[[mod]]\nsrc =\n").expect("write");
        let error = read_spec(dir.path()).expect_err("should fail");
        assert!(error.contains(SPEC_FILE), "names the file: {error}");
        assert!(error.contains("line"), "and the place: {error}");
    }

    #[test]
    fn the_spec_round_trips_through_the_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        add_to_spec(dir.path(), &entry("mods/75_atlas.lua")).expect("add");
        assert!(read_spec(dir.path())
            .expect("read")
            .manages("mods/75_atlas.lua"));

        let removed = remove_from_spec(dir.path(), "atlas").expect("remove");
        assert_eq!(removed.file, "mods/75_atlas.lua");
        assert!(read_spec(dir.path()).expect("read").mods.is_empty());
    }

    #[test]
    fn removing_something_the_spec_never_listed_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(remove_from_spec(dir.path(), "atlas").is_err());
    }

    #[test]
    fn an_emptied_lock_leaves_no_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut lock = ModLock::default();
        deliver(
            dir.path(),
            &entry("mods/a.lua"),
            "https://example.com/atlas",
            "v1",
            &[payload("mods/a.lua", "-- one\n")],
            &mut lock,
        )
        .expect("deliver");
        write_lock(dir.path(), &lock).expect("write");
        assert!(lock_path(dir.path()).is_file());

        lock.forget("mods/a.lua");
        write_lock(dir.path(), &lock).expect("write");
        assert!(
            !lock_path(dir.path()).exists(),
            "a directory with nothing managed should look like one"
        );
    }

    #[test]
    fn delivering_writes_then_settles_then_updates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = "mods/75_atlas.lua";
        let mut lock = ModLock::default();

        let first = deliver(
            dir.path(),
            &entry(file),
            "src",
            "v1",
            &[payload(file, "-- v1\n")],
            &mut lock,
        )
        .expect("deliver");
        assert_eq!(first, Outcome::Installed);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(file)).expect("read"),
            "-- v1\n"
        );

        // Again at the same version: nothing to do, and it says so.
        let again = deliver(
            dir.path(),
            &entry(file),
            "src",
            "v1",
            &[payload(file, "-- v1\n")],
            &mut lock,
        )
        .expect("deliver");
        assert_eq!(again, Outcome::Current);

        let moved = deliver(
            dir.path(),
            &entry(file),
            "src",
            "v2",
            &[payload(file, "-- v2\n")],
            &mut lock,
        )
        .expect("deliver");
        assert_eq!(moved, Outcome::Updated);
        assert_eq!(lock.entry(file).expect("entry").version, "v2");
    }

    #[test]
    fn an_edit_survives_an_update_and_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = "mods/75_atlas.lua";
        let mut lock = ModLock::default();
        deliver(
            dir.path(),
            &entry(file),
            "src",
            "v1",
            &[payload(file, "-- v1\n")],
            &mut lock,
        )
        .expect("deliver");

        std::fs::write(dir.path().join(file), "-- mine\n").expect("edit");
        let outcome = deliver(
            dir.path(),
            &entry(file),
            "src",
            "v2",
            &[payload(file, "-- v2\n")],
            &mut lock,
        )
        .expect("deliver");
        assert_eq!(outcome, Outcome::Kept);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(file)).expect("read"),
            "-- mine\n",
            "the edit is the user's to keep"
        );
        // And again, because this is where it nearly went wrong: if the record were
        // updated to the digest now on disk, the edit would look like our own
        // delivery and the SECOND run would overwrite it.
        let outcome = deliver(
            dir.path(),
            &entry(file),
            "src",
            "v2",
            &[payload(file, "-- v2\n")],
            &mut lock,
        )
        .expect("deliver");
        assert_eq!(outcome, Outcome::Kept);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(file)).expect("read"),
            "-- mine\n"
        );
    }

    #[test]
    fn a_deletion_is_remembered_rather_than_undone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = "mods/75_atlas.lua";
        let mut lock = ModLock::default();
        deliver(
            dir.path(),
            &entry(file),
            "src",
            "v1",
            &[payload(file, "-- v1\n")],
            &mut lock,
        )
        .expect("deliver");

        std::fs::remove_file(dir.path().join(file)).expect("delete");
        for _ in 0..2 {
            let outcome = deliver(
                dir.path(),
                &entry(file),
                "src",
                "v1",
                &[payload(file, "-- v1\n")],
                &mut lock,
            )
            .expect("deliver");
            assert_eq!(outcome, Outcome::Deleted);
            assert!(
                !dir.path().join(file).exists(),
                "convergence must not put back what the user removed"
            );
        }
    }

    #[test]
    fn a_package_delivers_its_own_modules_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut lock = ModLock::default();
        deliver(
            dir.path(),
            &entry("mods/75_atlas.lua"),
            "src",
            "v1",
            &[
                payload("mods/75_atlas.lua", "-- pane\n"),
                payload("lib/atlas/util.lua", "-- helper\n"),
            ],
            &mut lock,
        )
        .expect("deliver");

        assert!(dir.path().join("lib/atlas/util.lua").is_file());
        // Traceable to the entry that brought it, which is what stops a module
        // being reported as the user's own file.
        assert_eq!(
            lock.covering("lib/atlas/util.lua").map(|e| e.src.as_str()),
            Some("atlas")
        );
    }

    #[test]
    fn withdrawing_removes_what_it_delivered_and_keeps_what_was_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut lock = ModLock::default();
        deliver(
            dir.path(),
            &entry("mods/75_atlas.lua"),
            "src",
            "v1",
            &[
                payload("mods/75_atlas.lua", "-- pane\n"),
                payload("lib/atlas/util.lua", "-- helper\n"),
            ],
            &mut lock,
        )
        .expect("deliver");
        std::fs::write(dir.path().join("lib/atlas/util.lua"), "-- mine\n").expect("edit");

        let record = lock.forget("mods/75_atlas.lua").expect("record");
        let report = withdraw(dir.path(), &record).expect("withdraw");
        assert!(!dir.path().join("mods/75_atlas.lua").exists());
        assert!(
            dir.path().join("lib/atlas/util.lua").is_file(),
            "a file the user changed outlives the entry that delivered it"
        );
        assert_eq!(
            report
                .iter()
                .find(|(file, _)| file == "lib/atlas/util.lua")
                .map(|(_, outcome)| *outcome),
            Some(Outcome::Kept)
        );
    }

    #[test]
    fn trust_follows_an_installed_pane_across_its_versions() {
        let home = tempfile::tempdir().expect("tempdir");
        std::env::set_var("THURBOX_CONFIG_DIR", home.path());
        let dir = home.path().join("ui");
        std::fs::create_dir_all(dir.join("mods")).expect("mkdir");
        let file = "mods/75_atlas.lua";
        let absolute = dir.join(file).to_string_lossy().into_owned();

        let mut lock = ModLock::default();
        deliver(
            &dir,
            &entry(file),
            "src",
            "v1",
            &[payload(file, "-- v1\n")],
            &mut lock,
        )
        .expect("deliver");

        let mut registry = super::super::registry::Registry::load();
        assert_eq!(
            trust_of(&dir, file, &lock, &registry),
            super::super::inventory::Trust::Untrusted,
            "a first install still asks"
        );

        registry
            .trust_installed(&absolute, "atlas@v1", "-- v1\n")
            .expect("trust");
        assert_eq!(
            trust_of(&dir, file, &lock, &registry),
            super::super::inventory::Trust::Trusted
        );

        // An ordinary release. The grant lapses rather than reading as tampering —
        // which is the whole point of recording the version.
        deliver(
            &dir,
            &entry(file),
            "src",
            "v2",
            &[payload(file, "-- v2\n")],
            &mut lock,
        )
        .expect("deliver");
        assert_eq!(
            trust_of(&dir, file, &lock, &registry),
            super::super::inventory::Trust::Untrusted,
            "moving the pin asks again"
        );

        // Granted at v2, then the source re-tags v2 with something else. The
        // capability must NOT ride along.
        registry
            .trust_installed(&absolute, "atlas@v2", "-- v2\n")
            .expect("trust");
        assert_eq!(
            trust_of(&dir, file, &lock, &registry),
            super::super::inventory::Trust::Trusted
        );
        std::fs::write(dir.join(file), "-- swapped\n").expect("re-tag");
        assert_eq!(
            trust_of(&dir, file, &lock, &registry),
            super::super::inventory::Trust::Replaced,
            "same version, different contents — the hole the digest closes"
        );
        assert_eq!(
            trust_of(&dir, file, &ModLock::default(), &registry),
            super::super::inventory::Trust::Untrusted,
            "no lock entry must not turn it into a file of the user's own"
        );

        // An UNMANAGED file stays on the contents-only rule, untouched by any of
        // this: the user wrote it, and its version is not a thing.
        let mine = "mods/90_mine.lua";
        std::fs::write(dir.join(mine), "-- mine\n").expect("write");
        let mine_absolute = dir.join(mine).to_string_lossy().into_owned();
        registry.trust(&mine_absolute, "-- mine\n").expect("trust");
        assert_eq!(
            trust_of(&dir, mine, &lock, &registry),
            super::super::inventory::Trust::Trusted
        );
        std::fs::write(dir.join(mine), "-- edited\n").expect("edit");
        assert_eq!(
            trust_of(&dir, mine, &lock, &registry),
            super::super::inventory::Trust::Drifted
        );
        std::env::remove_var("THURBOX_CONFIG_DIR");
    }

    #[test]
    fn a_destination_that_would_escape_the_directory_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut lock = ModLock::default();
        let escape = ModEntry {
            src: "atlas".into(),
            file: "../escape.lua".into(),
            pin: None,
        };
        assert!(deliver(dir.path(), &escape, "src", "v1", &[], &mut lock).is_err());

        // And a payload cannot smuggle one past a legitimate entry.
        assert!(deliver(
            dir.path(),
            &entry("mods/a.lua"),
            "src",
            "v1",
            &[payload("../escape.lua", "-- no\n")],
            &mut lock,
        )
        .is_err());
        assert!(!dir
            .path()
            .parent()
            .expect("parent")
            .join("escape.lua")
            .exists());
    }
}
