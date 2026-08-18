//! The interface's plugins as *managed* files: the spec, the lock, and the
//! operations that move the directory between them.
//!
//! `bundled` delivers what the binary carries; this delivers what the spec asks
//! for. The two share the decision that matters — `bundled::decide`, which
//! encodes "do not clobber my edits, remember what I deleted" — because an
//! installed file wants exactly the same treatment as a shipped one and
//! reimplementing that matrix is the one duplication here that would be genuinely
//! dangerous.
//!
//! Everything is keyed on the **destination path relative to the interface
//! directory**, which is the identity the spec, the lock, the inventory, trust and
//! the disabled set already all use.
//!
//! The types live in `session::plugin_spec` (pure data, so the CLI and the kernel
//! agree on one definition); the filesystem is here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::session::plugin_spec::{
    self, LockEntry, PluginEntry, PluginLock, PluginSpec, LOCK_FILE, SPEC_FILE,
};

use super::bundled::{decide, digest, Action, Delivered};

/// Where the spec lives.
pub fn spec_path(dir: &Path) -> PathBuf {
    dir.join(SPEC_FILE)
}

/// Where the record lives.
pub fn lock_path(dir: &Path) -> PathBuf {
    dir.join(LOCK_FILE)
}

/// The spec, or an empty one.
///
/// **No spec means nothing installed**, which is a valid interface and not a
/// failure — an existing directory that predates the manager keeps working
/// untouched. A spec that is *there* and unreadable is a different matter: it is
/// reported with the file named and the location `toml` gave, and no caller of
/// this proceeds to change anything.
pub fn read_spec(dir: &Path) -> Result<PluginSpec, String> {
    match std::fs::read_to_string(spec_path(dir)) {
        Ok(text) => PluginSpec::parse(&text).map_err(|e| format!("{SPEC_FILE}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PluginSpec::default()),
        Err(e) => Err(format!("{}: {e}", spec_path(dir).display())),
    }
}

/// The spec's text, for an edit that has to keep its comments.
pub fn read_spec_text(dir: &Path) -> Result<String, String> {
    match std::fs::read_to_string(spec_path(dir)) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("{}: {e}", spec_path(dir).display())),
    }
}

/// The record, or an empty one.
pub fn read_lock(dir: &Path) -> Result<PluginLock, String> {
    match std::fs::read_to_string(lock_path(dir)) {
        Ok(text) => PluginLock::parse(&text).map_err(|e| format!("{LOCK_FILE}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PluginLock::default()),
        Err(e) => Err(format!("{}: {e}", lock_path(dir).display())),
    }
}

/// Write the record.
///
/// Rendered wholesale rather than edited in place: nobody hand-maintains this
/// file, which is the entire reason it is a second file (design D2).
pub fn write_lock(dir: &Path, lock: &PluginLock) -> Result<(), String> {
    // An emptied lock is removed rather than left as an empty table, so a
    // directory with nothing managed looks like one.
    if lock.plugins.is_empty() {
        return match std::fs::remove_file(lock_path(dir)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{}: {e}", lock_path(dir).display())),
        };
    }
    let text = lock.to_toml()?;
    std::fs::write(lock_path(dir), text).map_err(|e| format!("{}: {e}", lock_path(dir).display()))
}

/// Record an entry in the spec, keeping every comment already in the file.
pub fn add_to_spec(dir: &Path, entry: &PluginEntry) -> Result<(), String> {
    let text = plugin_spec::insert_entry(&read_spec_text(dir)?, entry)?;
    write_spec(dir, &text)
}

/// Drop an entry from the spec, returning it. Errors when the spec does not list
/// it, which the caller reports rather than treating as done.
pub fn remove_from_spec(dir: &Path, key: &str) -> Result<PluginEntry, String> {
    let (text, removed) = plugin_spec::remove_entry(&read_spec_text(dir)?, key)?;
    write_spec(dir, &text)?;
    Ok(removed)
}

fn write_spec(dir: &Path, text: &str) -> Result<(), String> {
    std::fs::write(spec_path(dir), text).map_err(|e| format!("{}: {e}", spec_path(dir).display()))
}

/// What one file of a package is, and what it should contain.
pub struct Payload {
    /// Destination, relative to the interface directory.
    pub file: String,
    pub contents: String,
}

/// What converging one entry did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Written where nothing was.
    Installed,
    /// Advanced to what the source now carries.
    Updated,
    /// Present and already what the source carries.
    Current,
    /// Changed by the user, and therefore theirs. Not overwritten.
    Kept,
    /// Deleted by the user, and remembered as such. Not reinstalled.
    Deleted,
    /// Taken back, because the spec no longer lists it.
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

    /// Did this outcome change the directory?
    pub fn changed(self) -> bool {
        matches!(
            self,
            Outcome::Installed | Outcome::Updated | Outcome::Removed
        )
    }
}

/// Write one entry's files, deferring to the user wherever they have acted.
///
/// The lock is both the input and the output: the digests it already holds are
/// what tell "we wrote this and nobody touched it" from "the user changed it",
/// and the digests written back are what the *next* run and the trust decision
/// read. On any outcome the caller may report, the lock is left consistent with
/// what is actually on disk.
///
/// A file the user deleted is tombstoned in the same sense `bundled` means it:
/// the digest is dropped from the record so convergence stops offering to put it
/// back, which is what makes deleting a managed pane a way to remove it rather
/// than something the next `sync` undoes.
pub fn deliver(
    dir: &Path,
    entry: &PluginEntry,
    resolved: &str,
    version: &str,
    payloads: &[Payload],
    lock: &mut PluginLock,
) -> Result<Outcome, String> {
    entry.validate()?;
    for payload in payloads {
        plugin_spec::validate_destination(&payload.file)?;
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
    // Ranked by what most needs the reader's attention, not by how much work was
    // done. `Kept` and `Deleted` both mean "you will NOT see the new version", and
    // a package whose module updated while its pane was left alone has to report
    // the pane — otherwise an edit silently costing you an update reads as a clean
    // update.
    let mut escalate = |candidate: Outcome| {
        let rank = |outcome: Outcome| match outcome {
            Outcome::Current => 0,
            Outcome::Updated => 1,
            Outcome::Installed => 2,
            Outcome::Deleted => 3,
            Outcome::Kept => 4,
            Outcome::Removed => 5,
        };
        if rank(candidate) > rank(outcome) {
            outcome = candidate;
        }
    };

    for payload in payloads {
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

        let action = decide(existing.as_deref(), delivered, &payload.contents);
        match action {
            Action::Write | Action::Update => {
                std::fs::write(&path, &payload.contents)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                files.insert(payload.file.clone(), digest(&payload.contents));
                escalate(if action == Action::Write {
                    Outcome::Installed
                } else {
                    Outcome::Updated
                });
            }
            Action::Settle => {
                files.insert(payload.file.clone(), digest(&payload.contents));
            }
            // Theirs. The record is carried through UNCHANGED — recording the
            // digest now on disk would make the user's edit look like our own
            // delivery, and the very next run would decide it was safe to
            // overwrite. `bundled::materialize` leaves the manifest alone on
            // `Preserve` for exactly this reason; "the last thing we wrote" is
            // what makes an edit detectable, so it must not be overwritten with
            // what the user wrote.
            Action::Preserve => {
                if let Some(recorded) = recorded.get(&payload.file) {
                    files.insert(payload.file.clone(), recorded.clone());
                }
                escalate(Outcome::Kept);
            }
            // Deleted, and recorded as such so the next run leaves it alone. Written
            // out rather than inferred from a missing digest, which is also what a
            // newly-added file looks like.
            Action::Tombstone | Action::Leave => {
                removed.push(payload.file.clone());
                escalate(Outcome::Deleted);
            }
        }
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

/// Take back the files one record delivered, for an entry the spec no longer
/// lists.
///
/// Only files still byte-identical to what was delivered are deleted; anything
/// the user changed is left where it is and reported as kept, which is the same
/// rule `bundled::retire` applies to a plugin that stopped shipping. Removal does
/// not need the source to be reachable — everything needed is in the record.
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

/// Remove the directories a withdrawn file leaves empty, up to (never including)
/// the interface directory.
///
/// A package's modules live in a namespace of their own, so removing one leaves an
/// empty `lib/<package>/` that nothing will ever look in again. Only *empty*
/// directories go, so anything the user put there keeps its home — and the walk
/// stops at `dir`, which is not ours to remove.
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

/// The directory in the thurbox repository that bare plugin names resolve against
/// — `ui-plugins/`, mirroring `extensions/`, and pinned to the same release tag by
/// the same helper.
///
/// Pinning to the tag is worth keeping even for examples: a pane reads `thurbox.*`,
/// a contract that moves, so what a bare name fetches matches the binary asking
/// for it.
pub const OFFICIAL_SET: &str = "ui-plugins";

/// One example pane, for discovery and for typo help on a failed bare-name
/// install.
///
/// **Examples, not a supported catalogue.** What lives in `ui-plugins/` is there to
/// be read and copied from — one pane that draws the `input` node kind, one that
/// runs a program — not a set thurbox maintains on anybody's behalf. Installing one
/// is a convenience over `cp`, and delivery treats it as the user's the moment they
/// edit it.
///
/// A small static list rather than a remote index, exactly as
/// `OFFICIAL_EXTENSIONS` is: a misspelled name should be caught without a network
/// round-trip. Keep in step with `ui-plugins/<name>/`.
pub const EXAMPLE_PLUGINS: &[(&str, &str)] = &[
    ("tasks", "A todo pane: read the snapshot, send or spawn"),
    ("top", "CPU, memory and load as gauges, parsed from `top`"),
];

/// What a source resolved to.
pub struct Resolved {
    pub source: crate::agent::extension_config::ExtensionSource,
    /// The location, as a string worth recording and showing.
    pub at: String,
    /// The version this resolution represents.
    pub version: String,
}

/// Where a source points, at a pin if one is asked for.
///
/// A pin only means anything for a **bare name**: a URL is already a location and
/// a filesystem path has no versions, so for those the pin is recorded and
/// otherwise ignored. For a bare name it selects the git ref, which is what makes
/// a lock reproducible after the binary moves on.
pub fn resolve_source(src: &str, pin: Option<&str>) -> Resolved {
    use crate::agent::extension_config as ext;
    let bare = plugin_spec::is_bare_name(src);
    let version = match (pin, bare) {
        (Some(pin), _) => pin.to_string(),
        // A bare name is fetched at the binary's release tag, so that IS the
        // version of a freshly-installed one.
        (None, true) => ext::official_ref(),
        (None, false) => String::new(),
    };
    let source = if bare {
        ext::ExtensionSource::Remote(format!(
            "{}/{src}",
            ext::official_set_base_at(OFFICIAL_SET, &version)
        ))
    } else {
        ext::resolve_source_in(src, OFFICIAL_SET)
    };
    let at = match &source {
        ext::ExtensionSource::Remote(base) => base.clone(),
        ext::ExtensionSource::Local(dir) => dir.display().to_string(),
        ext::ExtensionSource::Git(url) => url.clone(),
    };
    Resolved {
        source,
        at,
        version,
    }
}

/// A fetched package: its manifest, if it has one, and its files.
pub struct Fetched {
    pub manifest: Option<crate::session::PackageManifest>,
    pub payloads: Vec<Payload>,
}

/// Read a package from wherever it resolved to.
///
/// Two shapes. A **package** is a directory with a `plugin.toml` naming a pane and
/// any modules it brings. A bare `.lua` at the end of a URL or path is the
/// **degenerate** case — one file, no modules, no declared compatibility — which
/// is what makes "install this single file somebody published" a supported thing
/// rather than a special case.
///
/// `as_file` overrides the destination the manifest proposes. It is required for
/// the degenerate shape, which proposes none.
pub fn fetch(src: &str, resolved: &Resolved, as_file: Option<&str>) -> Result<Fetched, String> {
    use crate::agent::extension_config as ext;

    if src.trim().ends_with(".lua") {
        let file = as_file
            .ok_or_else(|| {
                format!("{src} is a single file, so it needs a destination: --as plugins/90_x.lua")
            })?
            .to_string();
        plugin_spec::validate_destination(&file)?;
        // Fetched relative to the *parent*, since the source names the file itself.
        let (base, name) = split_last(src);
        let source = match &resolved.source {
            ext::ExtensionSource::Remote(_) => ext::ExtensionSource::Remote(base),
            ext::ExtensionSource::Local(_) => ext::ExtensionSource::Local(ext::expand_tilde(&base)),
            // A `.lua` source cannot also be a repository: `git_url` matched first,
            // so a `git+…/x.lua` never reaches here.
            ext::ExtensionSource::Git(url) => ext::ExtensionSource::Git(url.clone()),
        };
        let contents = ext::fetch_file(&source, &name)?;
        return Ok(Fetched {
            manifest: None,
            payloads: vec![Payload { file, contents }],
        });
    }

    let manifest_text = ext::fetch_file(&resolved.source, crate::session::PackageManifest::FILE)
        .map_err(|e| {
            format!(
                "{src}: no {} at {} ({e})",
                crate::session::PackageManifest::FILE,
                resolved.at
            )
        })?;
    let manifest = crate::session::PackageManifest::parse(&manifest_text)
        .map_err(|e| format!("{src}: {e}"))?;

    let mut payloads = Vec::new();
    for (index, file) in manifest.files().enumerate() {
        // Only the PANE may be redirected. A module's path is the namespace rule
        // the manifest was validated against, and letting `--as` move it would
        // hand back the collision that rule exists to prevent.
        let destination = match (index, as_file) {
            (0, Some(file)) => file.to_string(),
            _ => file.path.clone(),
        };
        plugin_spec::validate_destination(&destination)?;
        payloads.push(Payload {
            file: destination,
            contents: ext::fetch_file(&resolved.source, &file.source)?,
        });
    }
    Ok(Fetched {
        manifest: Some(manifest),
        payloads,
    })
}

/// Split a `.lua` source into the directory holding it and its file name.
fn split_last(src: &str) -> (String, String) {
    let src = src.trim().trim_end_matches('/');
    match src.rsplit_once(['/', '\\']) {
        Some((base, name)) => (base.to_string(), name.to_string()),
        None => (".".to_string(), src.to_string()),
    }
}

/// Where one file of the interface stands with the user, for what it declares.
///
/// One implementation because the question splits two ways and the split is easy
/// to get wrong: a file the user wrote is judged on its contents, an installed one
/// on the `src@version` it came from **and** its contents. Doing that at each call
/// site invites a caller that checks only one half.
pub fn trust_of(
    dir: &Path,
    relative: &str,
    lock: &PluginLock,
    registry: &super::registry::Registry,
) -> super::inventory::Trust {
    let absolute = dir.join(relative);
    let key = absolute.to_string_lossy();
    let current = std::fs::read_to_string(&absolute)
        .ok()
        .map(|text| digest(&text));

    match lock.covering(relative) {
        Some(entry) => super::inventory::Trust::resolve_installed(
            registry
                .trusted_pin(&key)
                .zip(registry.trusted_digest(&key)),
            &entry.pin_key(),
            current.as_deref(),
        ),
        None => super::inventory::Trust::resolve(registry.trusted_digest(&key), current.as_deref()),
    }
}

/// What happened to one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryReport {
    pub name: String,
    pub file: String,
    pub src: String,
    /// The version now installed.
    pub version: String,
    /// The version this moved from, when it moved.
    pub from: Option<String>,
    pub outcome: Outcome,
}

/// Is this entry's source a repository, and therefore a working copy on disk?
///
/// Re-derived from the source rather than stored as a field: the source is what
/// decided the mechanism at install time, so asking it again cannot disagree with
/// itself the way a second recorded flag could.
pub fn is_repository(src: &str) -> bool {
    crate::agent::extension_config::git_url(src).is_some()
}

/// The directory name a repository is cloned into: its last path segment, without
/// `.git`.
///
/// Refused rather than sanitised when it is not one safe segment, for the reason
/// `plugin new` refuses a bad name: a clone landing somewhere other than where it
/// said is worse than one that stops.
pub fn repository_name(url: &str) -> Result<String, String> {
    let last = last_segment(url, cfg!(windows));
    if last.is_empty() {
        return Err(format!("could not read a plugin name out of {url}"));
    }
    crate::paths::validate_safe_name(last)
        .map_err(|e| format!("{url} names {last:?}, which is not usable as a directory: {e}"))?;
    Ok(last.to_string())
}

/// The last segment of a source, without `.git`.
///
/// `windows` decides whether a **backslash separates**. On Windows a local source is
/// `D:\src\thurbox-widget`; on POSIX a backslash is a legal filename character, and
/// splitting on one there would rename a directory that legitimately contains it. A
/// remote URL separates with `/` or `:` on either platform, so this changes nothing
/// but the local-path case.
///
/// The platform is a **parameter** rather than a `cfg!` read inside the body, so both
/// behaviours are testable on one machine. That is not tidiness: this is exactly the
/// bug that got shipped. Every repository test passed on Linux and *all ten* failed on
/// Windows, because the only difference was a separator the Linux suite could not
/// express — the same way `bundled::relative_to` was invisible here until Windows CI
/// said otherwise.
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

/// The pane inside a freshly-cloned working copy.
///
/// Three answers in order, and the order is the point:
///
/// 1. **`--as`**, because an explicit request wins over anything inferred.
/// 2. **The repository's own `plugin.toml`**, if it has one. A manifest is therefore
///    *not* irrelevant to a cloned plugin: it is how a repository with several panes
///    names its entry point, and it means one repository serves both install
///    mechanisms rather than presenting two doors that behave differently. Read
///    leniently (`pane_source_of`) — the copy-model rules do not apply to a tree
///    that keeps its own layout.
/// 3. **The single `.lua` under `plugins/`**, which is what a one-pane repository
///    looks like and makes "clone it and it works" true with no manifest at all.
///
/// Anything else is ambiguous and says so, listing what it found: guessing which of
/// several panes is the entry point would be a silent wrong answer.
fn pane_in_working_copy(root: &Path, name: &str, as_file: Option<&str>) -> Result<String, String> {
    if let Some(rel) = as_file {
        let rel = rel.trim_start_matches('/');
        // Accept either `plugins/40_x.lua` (inside the repo) or the full
        // `<name>/plugins/40_x.lua`, since both readings are natural.
        let rel = rel.strip_prefix(&format!("{name}/")).unwrap_or(rel);
        let file = format!("{name}/{rel}");
        plugin_spec::validate_destination(&file)?;
        if !root.join(rel).is_file() {
            return Err(format!("{rel} is not in the repository"));
        }
        return Ok(file);
    }

    // The repository's own manifest, when it has one.
    if let Ok(text) = std::fs::read_to_string(root.join(crate::session::PackageManifest::FILE)) {
        if let Some(source) = plugin_spec::pane_source_of(&text) {
            let file = format!("{name}/{}", source.trim_start_matches('/'));
            plugin_spec::validate_destination(&file)?;
            if !root.join(&source).is_file() {
                return Err(format!(
                    "{} names {source} as its pane, which is not in the repository",
                    crate::session::PackageManifest::FILE
                ));
            }
            return Ok(file);
        }
    }

    let plugins = root.join("plugins");
    let mut found: Vec<String> = std::fs::read_dir(&plugins)
        .map_err(|_| {
            "the repository has no plugins/ directory, so name its pane: \
             --as plugins/NN_name.lua"
                .to_string()
        })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".lua"))
        .collect();
    found.sort();
    match found.len() {
        1 => Ok(format!("{name}/plugins/{}", found[0])),
        0 => Err("the repository's plugins/ directory holds no Lua".to_string()),
        _ => Err(format!(
            "the repository has more than one pane ({}) — name the one to load: \
             --as plugins/<file>",
            found.join(", ")
        )),
    }
}

/// Install a plugin by obtaining a working copy of its repository.
///
/// Everything the repository holds is delivered, in the layout its author chose.
/// The lock records the **commit**, not the ref asked for, which is what makes the
/// same spec reproduce the same bytes after a branch moves.
fn install_repository(
    dir: &Path,
    src: &str,
    url: &str,
    as_file: Option<&str>,
    pin: Option<&str>,
    spec: &PluginSpec,
    lock: &mut PluginLock,
) -> Result<EntryReport, String> {
    let name = repository_name(url)?;
    let root = dir.join(&name);

    // Occupied by something this spec does not manage: not ours to replace.
    if root.exists()
        && !spec
            .plugins
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

    crate::git::clone_plugin(url, &root, pin).map_err(|e| format!("{e:#}"))?;
    let commit = crate::git::head_commit(&root).map_err(|e| format!("{e:#}"))?;

    let file = pane_in_working_copy(&root, &name, as_file).map_err(|e| {
        // A clone whose pane cannot be identified is not an install: take it back so
        // a second attempt with `--as` starts clean.
        let _ = std::fs::remove_dir_all(&root);
        e
    })?;

    let entry = PluginEntry {
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
        outcome: Outcome::Installed,
    })
}

/// Install one plugin, recording it in the spec and the lock.
///
/// Refuses before it writes anything: a destination already holding a file the
/// spec does not manage is somebody else's, and overwriting it is not this
/// command's business (spec: "the existing file is left exactly as it was").
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
    // file. The source's own form chose this (`extension_config::git_url`).
    if let crate::agent::extension_config::ExtensionSource::Git(url) = &resolved.source {
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
    let entry = PluginEntry {
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
        outcome,
    })
}

/// Converge or advance one repository-backed entry.
///
/// git owns the working copy here, and that is the point rather than a shortcut: it
/// refuses to move a dirty tree, which is the "an edit is yours to keep" rule done
/// by the tool that owns it. `advance` distinguishes convergence (hold at the
/// recorded commit) from an update (take what the remote has now).
fn converge_repository(
    dir: &Path,
    entry: &PluginEntry,
    url: &str,
    advance: bool,
    lock: &mut PluginLock,
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
        crate::git::clone_plugin(url, &root, entry.pin.as_deref()).map_err(|e| format!("{e:#}"))?;
        // Reproducing a recorded commit: a shallow clone has only the tip, so the
        // exact object has to be asked for.
        if let Some(commit) = recorded.as_deref().filter(|c| !c.is_empty()) {
            if crate::git::head_commit(&root).map_err(|e| format!("{e:#}"))? != commit {
                crate::git::fetch_ref(&root, commit).map_err(|e| format!("{e:#}"))?;
                crate::git::checkout_plugin(&root, commit).map_err(|e| format!("{e:#}"))?;
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
                crate::git::checkout_plugin(&root, commit).map_err(|e| format!("{e:#}"))?;
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
    crate::git::checkout_plugin(&root, "FETCH_HEAD").map_err(|e| format!("{e:#}"))?;
    let moved = crate::git::head_commit(&root).map_err(|e| format!("{e:#}"))?;
    if moved == head {
        return Ok((Outcome::Current, head, None));
    }
    Ok((Outcome::Updated, moved, Some(head)))
}

/// Record what a repository entry converged to.
fn record_repository(
    lock: &mut PluginLock,
    dir: &Path,
    entry: &PluginEntry,
    url: &str,
    commit: &str,
) {
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

/// Bring the directory into agreement with the spec.
///
/// Installs what is absent, takes back what the spec no longer lists, and leaves
/// everything else alone — including a pane the spec never listed, which is
/// nobody's to touch. Idempotent: a second run reports every entry `current` and
/// changes nothing.
///
/// Resolves each entry at the version the **lock** recorded rather than at
/// whatever is newest, which is what makes the same spec plus lock reproduce the
/// same interface elsewhere. Advancing is [`update`]'s job, asked for explicitly.
pub fn sync(dir: &Path) -> Result<Vec<EntryReport>, String> {
    let spec = read_spec(dir)?;
    let mut lock = read_lock(dir)?;
    let mut reports = Vec::new();

    // Withdrawn first, so a spec that moves a pane from one file to another does
    // not have the old file taken back after the new one is written.
    for stale in lock
        .beyond(&spec)
        .into_iter()
        .cloned()
        .collect::<Vec<LockEntry>>()
    {
        withdraw(dir, &stale)?;
        lock.forget(&stale.file);
        reports.push(EntryReport {
            name: stale.file.clone(),
            file: stale.file.clone(),
            src: stale.src.clone(),
            version: stale.version.clone(),
            from: None,
            outcome: Outcome::Removed,
        });
    }

    for entry in &spec.plugins {
        if let Some(url) = crate::agent::extension_config::git_url(&entry.src) {
            let (outcome, commit, from) = converge_repository(dir, entry, &url, false, &mut lock)?;
            record_repository(&mut lock, dir, entry, &url, &commit);
            reports.push(EntryReport {
                name: entry.name().to_string(),
                file: entry.file.clone(),
                src: entry.src.clone(),
                version: commit,
                from,
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
            outcome,
        });
    }

    write_lock(dir, &lock)?;
    Ok(reports)
}

/// Advance entries to what their source carries now.
///
/// `key` names one entry; `None` is all of them. An entry the spec **pins** is
/// already where the user said it should be, so it is reported as current rather
/// than moved — moving it means editing the pin, which is what a hand-edited spec
/// is for. Finding nothing newer is likewise success, not a failure.
pub fn update(dir: &Path, key: Option<&str>) -> Result<Vec<EntryReport>, String> {
    let spec = read_spec(dir)?;
    let mut lock = read_lock(dir)?;

    let targets: Vec<&PluginEntry> = match key {
        Some(key) => vec![spec
            .find(key)
            .ok_or_else(|| format!("{key} is not listed in {SPEC_FILE}"))?],
        None => spec.plugins.iter().collect(),
    };
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let mut reports = Vec::new();
    for entry in targets {
        if let Some(url) = crate::agent::extension_config::git_url(&entry.src) {
            let (outcome, commit, from) = converge_repository(dir, entry, &url, true, &mut lock)?;
            record_repository(&mut lock, dir, entry, &url, &commit);
            reports.push(EntryReport {
                name: entry.name().to_string(),
                file: entry.file.clone(),
                src: entry.src.clone(),
                version: commit,
                from,
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
            version,
            outcome,
        });
    }

    write_lock(dir, &lock)?;
    Ok(reports)
}

/// Remove one installed plugin: its files, its spec entry and its record.
///
/// Needs nothing from the source, which may be long gone — everything required is
/// in the lock. A key the spec does not list is an error rather than a no-op, so a
/// misspelled removal does not read as success.
pub fn remove(dir: &Path, key: &str) -> Result<EntryReport, String> {
    let removed = remove_from_spec(dir, key)?;
    let mut lock = read_lock(dir)?;
    let record = lock.forget(&removed.file);

    // A repository is one directory, not a file list: taking back the files the lock
    // happens to name would leave the working copy and its `.git` behind.
    if let Some(url) = crate::agent::extension_config::git_url(&removed.src) {
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
        outcome: Outcome::Removed,
    })
}

/// The version to record for a resolution.
///
/// A bare official name resolves at a git ref, which *is* its version. Anything
/// else takes what the package calls itself, and a package that says nothing is
/// recorded as unpinned — honestly, rather than with a version invented for it.
fn version_of(resolved: &Resolved, manifest: Option<&crate::session::PackageManifest>) -> String {
    if !resolved.version.is_empty() {
        return resolved.version.clone();
    }
    manifest
        .and_then(|manifest| manifest.version.clone())
        .unwrap_or_else(|| "unpinned".to_string())
}

/// Add "did you mean" to a failed bare-name fetch.
///
/// The static list is help, not a gate: a pane published upstream after this
/// binary was built still installs by name, and a typo still gets an answer
/// without a network round-trip deciding it.
fn not_found_help(src: &str, error: String) -> String {
    if !plugin_spec::is_bare_name(src) {
        return error;
    }
    // The fetcher's own message is two downloaders' stderr and a raw URL, which
    // buries the one thing a mistyped name needs. It is REPLACED rather than
    // appended to: this is the common failure, and the answer to it is short.
    let mut message = format!("no example plugin named {src:?}");
    if let Some(suggestion) = suggest_plugin(src) {
        message.push_str(&format!(" — did you mean {suggestion:?}?"));
    }
    let available: Vec<&str> = EXAMPLE_PLUGINS.iter().map(|(name, _)| *name).collect();
    message.push_str(&format!("\n  available: {}", available.join(", ")));
    // Kept, indented, because a bare name can also fail for reasons that are not
    // a typo at all — no network, a proxy, a broken package upstream — and
    // swallowing the cause would make those undiagnosable.
    message.push_str(&format!("\n  ({})", error.replace('\n', " ")));
    message
}

/// Suggest the closest example plugin name, within a small edit budget so an
/// unrelated typo suggests nothing. Mirrors `suggest_extension`.
pub fn suggest_plugin(name: &str) -> Option<&'static str> {
    let name = name.trim().to_lowercase();
    let budget = (name.len() / 3).clamp(2, 3);
    EXAMPLE_PLUGINS
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

    /// A Windows local path names its clone directory too.
    ///
    /// Both platforms are asserted from one machine on purpose: the Windows reading
    /// was wrong for the whole life of the clone path and Linux could not see it,
    /// because a `\` is an ordinary filename character here and a separator there.
    /// Every repository test failed on Windows CI with "Name contains invalid
    /// characters" while passing locally.
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

    fn entry(file: &str) -> PluginEntry {
        PluginEntry {
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
        assert!(spec.plugins.is_empty());
        assert!(read_lock(dir.path())
            .expect("no lock either")
            .plugins
            .is_empty());
    }

    #[test]
    fn a_malformed_spec_names_the_file_and_the_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(spec_path(dir.path()), "[[plugin]]\nsrc =\n").expect("write");
        let error = read_spec(dir.path()).expect_err("should fail");
        assert!(error.contains(SPEC_FILE), "names the file: {error}");
        assert!(error.contains("line"), "and the place: {error}");
    }

    #[test]
    fn the_spec_round_trips_through_the_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        add_to_spec(dir.path(), &entry("plugins/75_atlas.lua")).expect("add");
        assert!(read_spec(dir.path())
            .expect("read")
            .manages("plugins/75_atlas.lua"));

        let removed = remove_from_spec(dir.path(), "atlas").expect("remove");
        assert_eq!(removed.file, "plugins/75_atlas.lua");
        assert!(read_spec(dir.path()).expect("read").plugins.is_empty());
    }

    #[test]
    fn removing_something_the_spec_never_listed_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(remove_from_spec(dir.path(), "atlas").is_err());
    }

    #[test]
    fn an_emptied_lock_leaves_no_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut lock = PluginLock::default();
        deliver(
            dir.path(),
            &entry("plugins/a.lua"),
            "https://example.com/atlas",
            "v1",
            &[payload("plugins/a.lua", "-- one\n")],
            &mut lock,
        )
        .expect("deliver");
        write_lock(dir.path(), &lock).expect("write");
        assert!(lock_path(dir.path()).is_file());

        lock.forget("plugins/a.lua");
        write_lock(dir.path(), &lock).expect("write");
        assert!(
            !lock_path(dir.path()).exists(),
            "a directory with nothing managed should look like one"
        );
    }

    #[test]
    fn delivering_writes_then_settles_then_updates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = "plugins/75_atlas.lua";
        let mut lock = PluginLock::default();

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
        let file = "plugins/75_atlas.lua";
        let mut lock = PluginLock::default();
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
        let file = "plugins/75_atlas.lua";
        let mut lock = PluginLock::default();
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
        let mut lock = PluginLock::default();
        deliver(
            dir.path(),
            &entry("plugins/75_atlas.lua"),
            "src",
            "v1",
            &[
                payload("plugins/75_atlas.lua", "-- pane\n"),
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
        let mut lock = PluginLock::default();
        deliver(
            dir.path(),
            &entry("plugins/75_atlas.lua"),
            "src",
            "v1",
            &[
                payload("plugins/75_atlas.lua", "-- pane\n"),
                payload("lib/atlas/util.lua", "-- helper\n"),
            ],
            &mut lock,
        )
        .expect("deliver");
        std::fs::write(dir.path().join("lib/atlas/util.lua"), "-- mine\n").expect("edit");

        let record = lock.forget("plugins/75_atlas.lua").expect("record");
        let report = withdraw(dir.path(), &record).expect("withdraw");
        assert!(!dir.path().join("plugins/75_atlas.lua").exists());
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

    /// The grant, read back through the registry and the lock rather than through
    /// the pure rule — so the wiring is covered too: which of the two rules is
    /// chosen, and what the pin is compared against.
    #[test]
    fn trust_follows_an_installed_pane_across_its_versions() {
        let home = tempfile::tempdir().expect("tempdir");
        std::env::set_var("THURBOX_CONFIG_DIR", home.path());
        let dir = home.path().join("ui");
        std::fs::create_dir_all(dir.join("plugins")).expect("mkdir");
        let file = "plugins/75_atlas.lua";
        let absolute = dir.join(file).to_string_lossy().into_owned();

        let mut lock = PluginLock::default();
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
            super::super::inventory::Trust::Drifted,
            "same version, different contents — the hole the digest closes"
        );

        // An UNMANAGED file stays on the contents-only rule, untouched by any of
        // this: the user wrote it, and its version is not a thing.
        let mine = "plugins/90_mine.lua";
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
        let mut lock = PluginLock::default();
        let escape = PluginEntry {
            src: "atlas".into(),
            file: "../escape.lua".into(),
            pin: None,
        };
        assert!(deliver(dir.path(), &escape, "src", "v1", &[], &mut lock).is_err());

        // And a payload cannot smuggle one past a legitimate entry.
        assert!(deliver(
            dir.path(),
            &entry("plugins/a.lua"),
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
