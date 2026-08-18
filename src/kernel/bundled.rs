//! The default interface, compiled into the binary.
//!
//! Design.md D11. With no pane in the kernel, a broken plugin directory means
//! no interface *at all* — a failure mode v1 could not have. The embedded
//! copies make a blank screen unreachable while keeping the shipped interface
//! readable and editable as ordinary files, which matters when extensibility is
//! the point.
//!
//! Four rules, in order:
//!
//! 1. a copy on disk wins, so your edits are what runs;
//! 2. an *unmodified* bundled file is updated on upgrade, so fixes arrive;
//! 3. a *modified* one is never overwritten — it is preserved and reported;
//! 4. a *deleted* one stays deleted, because deleting it is how you remove it.
//!
//! Telling those apart needs a record of what was last written, which is what
//! the manifest is for. (4) is the one that is not obvious: it is exactly the
//! case of a file we know we wrote and can no longer find, and without it the
//! interface would re-deliver itself on every start and no bundled pane could
//! ever be removed (v2-plugin-lifecycle design D1).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Every file that makes up the default interface.
///
/// Listed explicitly rather than globbed at build time: a file that silently
/// stops shipping is worse than one that fails to compile, and `include_str!`
/// gives the latter.
pub const BUNDLED: &[(&str, &str)] = &[
    // Not code. It is here because this directory is the working copy an agent is
    // pointed at when someone asks it to change a pane, and the rules that matter
    // — four node kinds, ask-every-frame, `run` absent until trusted — are not
    // guessable from the Lua beside it.
    ("README.md", include_str!("../../ui/README.md")),
    // The same reasoning, under the name an agent actually reads on its own.
    // README.md is what a person opens; `AGENTS.md` is what a coding CLI loads as
    // context without being told to — which is what stops "install this plugin"
    // being read as a package-manager request. `CLAUDE.md` and `GEMINI.md` are
    // pointers rather than copies: two files saying the same thing drift, and the
    // stale one is the one somebody reads. The `flow` extension surfaces its spec
    // the same way, as symlinks — which it can do because it copies its own files,
    // and delivery here writes contents.
    ("AGENTS.md", include_str!("../../ui/AGENTS.md")),
    ("CLAUDE.md", include_str!("../../ui/CLAUDE.md")),
    ("GEMINI.md", include_str!("../../ui/GEMINI.md")),
    ("layout.lua", include_str!("../../ui/layout.lua")),
    ("lib/theme.lua", include_str!("../../ui/lib/theme.lua")),
    ("lib/widgets.lua", include_str!("../../ui/lib/widgets.lua")),
    ("lib/tree.lua", include_str!("../../ui/lib/tree.lua")),
    ("lib/panels.lua", include_str!("../../ui/lib/panels.lua")),
    ("lib/hover.lua", include_str!("../../ui/lib/hover.lua")),
    ("lib/fuzzy.lua", include_str!("../../ui/lib/fuzzy.lua")),
    (
        "lib/settings.lua",
        include_str!("../../ui/lib/settings.lua"),
    ),
    (
        "lib/textinput.lua",
        include_str!("../../ui/lib/textinput.lua"),
    ),
    (
        "plugins/10_sessions.lua",
        include_str!("../../ui/plugins/10_sessions.lua"),
    ),
    (
        "plugins/20_agent.lua",
        include_str!("../../ui/plugins/20_agent.lua"),
    ),
    (
        "plugins/60_confirm.lua",
        include_str!("../../ui/plugins/60_confirm.lua"),
    ),
    (
        "plugins/65_search.lua",
        include_str!("../../ui/plugins/65_search.lua"),
    ),
    (
        "plugins/70_new_session.lua",
        include_str!("../../ui/plugins/70_new_session.lua"),
    ),
];

/// Records which version of each bundled file was last written, so a user's
/// edits can be told from a stale copy — and, once written, so its absence can
/// be told from never having delivered it at all.
const MANIFEST: &str = ".bundled.json";

/// What the manifest remembers about one shipped file.
///
/// The written form is a bare string because that is what earlier releases
/// wrote: an upgrade that could not read the old manifest would forget every
/// edit the manifest exists to protect.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum Record {
    /// Digest of the contents last written here.
    Written(String),
    /// The user deleted this file; it is never written again.
    Removed { removed: bool },
}

impl Record {
    fn tombstone() -> Self {
        Record::Removed { removed: true }
    }

    fn is_removed(&self) -> bool {
        matches!(self, Record::Removed { removed: true })
    }

    fn digest(&self) -> Option<&str> {
        match self {
            Record::Written(digest) => Some(digest.as_str()),
            Record::Removed { .. } => None,
        }
    }

    /// This record as the delivery decision reads it — see [`decide`].
    fn delivered(&self) -> Delivered<'_> {
        match self {
            Record::Written(digest) => Delivered::Digest(digest.as_str()),
            Record::Removed { .. } => Delivered::Tombstoned,
        }
    }
}

/// Where a file in the interface directory came from, as far as delivery knows.
///
/// Not `Copy`: [`Source::Installed`] carries the source it came from, because
/// naming it is the whole point of the case. A capability is granted **per file**,
/// so "who shipped this" is the question to answer before granting one — and
/// until this existed, a third-party pane and one the user wrote themselves were
/// the same answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Shipped, and byte-identical to what this binary carries.
    Bundled,
    /// Shipped, and since changed by the user.
    Edited,
    /// The user's own file. Delivery never touches it.
    User,
    /// Shipped, and deleted by the user. Restorable from the binary.
    Removed,
    /// Installed from a source the spec names, and still what that source
    /// delivered.
    Installed { src: String },
    /// Installed from a source the spec names, and since changed — locally, or
    /// upstream under the same pin. Either way the contents are no longer the ones
    /// the version delivered, which is what a capability grant was made against.
    InstalledEdited { src: String },
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Bundled => "bundled",
            Source::Edited => "edited",
            Source::User => "user",
            Source::Removed => "removed",
            Source::Installed { .. } => "installed",
            Source::InstalledEdited { .. } => "installed · modified",
        }
    }

    /// The source this file was installed from, when it was.
    pub fn installed_from(&self) -> Option<&str> {
        match self {
            Source::Installed { src } | Source::InstalledEdited { src } => Some(src.as_str()),
            _ => None,
        }
    }

    /// Is this a file the user wrote, rather than one delivered to them?
    ///
    /// The question the Interface tab asks before offering to restore a file from
    /// the binary — which it cannot do for a file that never came from there.
    /// Asked as a question rather than compared against `User` so that an
    /// installed file is not silently treated as the user's own.
    pub fn is_theirs(&self) -> bool {
        matches!(self, Source::User)
    }
}

/// What materializing did, for reporting to the user.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Files created because they were absent.
    pub written: Vec<String>,
    /// Files replaced because they matched what we last wrote.
    pub updated: Vec<String>,
    /// Files left alone because the user had changed them.
    pub preserved: Vec<String>,
    /// Files the user deleted, recorded so they are not written again.
    pub removed: Vec<String>,
    /// Files we wrote that this binary no longer carries, and took back.
    pub retired: Vec<String>,
    pub errors: Vec<String>,
}

impl Report {
    pub fn is_empty(&self) -> bool {
        self.written.is_empty()
            && self.updated.is_empty()
            && self.preserved.is_empty()
            && self.removed.is_empty()
            && self.retired.is_empty()
            && self.errors.is_empty()
    }
}

/// A stable digest of a file's contents.
///
/// FNV-1a: not cryptographic, and does not need to be — it answers "did the
/// user change this file", where the adversary is a stray editor, not an
/// attacker.
///
/// Trust drift (`kernel::registry`) reads it for the same question and takes
/// the same limit: it reports that a trusted file **changed**, not that it was
/// tampered with. Nothing in the security story rests on it — someone who can
/// write to the interface directory can also add a plugin of their own, so a
/// stronger hash here would buy a guarantee the rest of the design already
/// declines to make.
pub fn digest(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Write the bundled interface into `dir`, preserving anything the user edited
/// and re-writing nothing they deleted.
pub fn materialize(dir: &Path) -> Report {
    let mut report = Report::default();
    let mut manifest = read_manifest(dir);

    for (relative, contents) in BUNDLED {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                report.errors.push(format!("{}: {e}", parent.display()));
                continue;
            }
        }

        let existing = std::fs::read_to_string(&path).ok();
        let action = decide(
            existing.as_deref(),
            manifest
                .get(*relative)
                .map_or(Delivered::Never, Record::delivered),
            contents,
        );

        match action {
            Action::Leave => {}
            Action::Settle => {
                manifest.insert((*relative).to_string(), Record::Written(digest(contents)));
            }
            Action::Tombstone => {
                manifest.insert((*relative).to_string(), Record::tombstone());
                report.removed.push((*relative).to_string());
            }
            Action::Preserve => report.preserved.push((*relative).to_string()),
            Action::Write | Action::Update => match std::fs::write(&path, contents) {
                Ok(()) => {
                    manifest.insert((*relative).to_string(), Record::Written(digest(contents)));
                    if action == Action::Write {
                        report.written.push((*relative).to_string());
                    } else {
                        report.updated.push((*relative).to_string());
                    }
                }
                Err(e) => report.errors.push(format!("{}: {e}", path.display())),
            },
        }
    }

    retire(dir, &mut manifest, &mut report);

    if let Err(e) = write_manifest(dir, &manifest) {
        report.errors.push(e);
    }
    report
}

/// Take back what we shipped and no longer ship.
///
/// Without this, renaming a bundled plugin between releases leaves the old copy
/// on disk and loading, so an upgrade the user did nothing to hands them two of
/// it. Same rule as everywhere else, read from the other side: we clean up only
/// what we wrote and the user has not claimed (design D2).
fn retire(dir: &Path, manifest: &mut BTreeMap<String, Record>, report: &mut Report) {
    let shipped: BTreeSet<&str> = BUNDLED.iter().map(|(name, _)| *name).collect();
    let stale: Vec<String> = manifest
        .keys()
        .filter(|name| !shipped.contains(name.as_str()))
        .cloned()
        .collect();

    for relative in stale {
        let path = dir.join(&relative);
        let ours = manifest.get(&relative).and_then(Record::digest).is_some();
        match std::fs::read_to_string(&path) {
            Ok(current)
                if ours && manifest[&relative].digest() == Some(digest(&current).as_str()) =>
            {
                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        manifest.remove(&relative);
                        report.retired.push(relative);
                    }
                    Err(e) => report.errors.push(format!("{}: {e}", path.display())),
                }
            }
            // Theirs now — it outlived the release that shipped it.
            Ok(_) => {
                manifest.remove(&relative);
                report.preserved.push(relative);
            }
            // Gone already, or a tombstone for something no longer shipped.
            Err(_) => {
                manifest.remove(&relative);
            }
        }
    }
}

/// How deep `lib/` is walked.
///
/// A package's modules live one level down (`lib/<package>/…`); the extra level is
/// for a package that groups its own, and the bound is here at all so a symlink
/// loop cannot make the inventory — the recovery tool — hang.
const LIB_DEPTH: usize = 3;

/// One file's identity within the interface directory, from an absolute path.
///
/// Trust, the disabled set and the rebindings are stored **absolute** (a repo's
/// `./ui` and the config directory's are different sets of files and must not
/// share them), while everything that reads a file — `build`, the spec, the lock,
/// the inventory — keys on the path relative to the directory with **`/`**
/// separators. This is the conversion between the two, and it exists in one place
/// because getting it wrong is invisible on Linux: `Path::join` writes a `\` on
/// Windows, so a hand-rolled strip yields `plugins\90_notes.lua`, which matches
/// nothing and fails silently. That is exactly how "turn a plugin off" came to be
/// a no-op on Windows.
///
/// `None` when `absolute` is not inside `dir`, or is `dir` itself.
pub fn relative_to(dir: &Path, absolute: &str) -> Option<String> {
    let root = dir.to_string_lossy();
    let rest = absolute.strip_prefix(root.as_ref())?;
    let relative: String = rest
        .trim_start_matches(['/', '\\'])
        .replace('\\', "/")
        .to_string();
    (!relative.is_empty()).then_some(relative)
}

/// Lua files under `root`, as paths relative to it, sorted.
///
/// Sorted so the inventory's order is deterministic rather than the filesystem's,
/// and separators normalised to `/` because that is the identity trust, the
/// disabled set, the lock and the spec all use.
fn lua_files(root: &Path, depth: usize) -> Vec<String> {
    fn walk(base: &Path, at: &Path, depth: usize, out: &mut Vec<String>) {
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(at) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                walk(base, &path, depth - 1, out);
            } else if path.extension().is_some_and(|ext| ext == "lua") {
                if let Ok(relative) = path.strip_prefix(base) {
                    out.push(
                        relative
                            .components()
                            .map(|part| part.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("/"),
                    );
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, depth, &mut out);
    out.sort();
    out
}

/// Where each file of the interface in `dir` came from.
///
/// Keyed by path relative to `dir`. Covers the three places the interface lives
/// — the arrangement, `lib/` and `plugins/` — plus every bundled file the user
/// deleted, which by definition is not on disk to be found.
pub fn sources(dir: &Path) -> BTreeMap<String, Source> {
    let bundled: BTreeMap<&str, &str> = BUNDLED.iter().copied().collect();
    // A directory with no lock has nothing installed, and a malformed one must not
    // make the inventory unavailable — the inventory is the recovery tool, so it
    // degrades to "I do not know where this came from" rather than refusing.
    let lock = super::packages::read_lock(dir).unwrap_or_default();
    let spec = super::packages::read_spec(dir).unwrap_or_default();
    let mut out = BTreeMap::new();

    let mut classify = |relative: String, path: &Path| {
        let Ok(current) = std::fs::read_to_string(path) else {
            return;
        };
        let source = match bundled.get(relative.as_str()) {
            Some(shipped) if *shipped == current => Source::Bundled,
            Some(_) => Source::Edited,
            // Shipped takes precedence deliberately: a package that delivered over
            // a bundled path is still, to delivery, a shipped file that changed —
            // and `install` refuses that destination anyway.
            None => match lock.covering(&relative) {
                Some(entry) => {
                    let src = entry.src.clone();
                    // A managed file whose contents are not the ones its version
                    // delivered. Locally edited, or re-tagged upstream; the
                    // inventory cannot tell which, and says only that they differ.
                    if entry.digest(&relative) == Some(digest(&current).as_str()) {
                        Source::Installed { src }
                    } else {
                        Source::InstalledEdited { src }
                    }
                }
                None => Source::User,
            },
        };
        out.insert(relative, source);
    };

    // Every top-level file we ship, not `layout.lua` by name: a shipped file the
    // inventory cannot see is one the Interface tab cannot restore, and the tab's
    // whole claim is that it lists every file.
    for (relative, _) in BUNDLED {
        if relative.contains('/') {
            continue;
        }
        let path = dir.join(relative);
        if path.is_file() {
            classify((*relative).to_string(), &path);
        }
    }
    // `plugins/` flat, `lib/` deep. Not an inconsistency: `build` reads only the
    // top level of `plugins/`, so a nested file there is never loaded and
    // reporting it would be a pane that "failed" nobody asked to load — while
    // `require` splits a module name on every dot, so `lib/<vendor>/util.lua` is
    // requirable and is where a package puts its own modules without colliding
    // with `lib/theme.lua`.
    for name in lua_files(&dir.join("plugins"), 1) {
        classify(format!("plugins/{name}"), &dir.join("plugins").join(&name));
    }
    for name in lua_files(&dir.join("lib"), LIB_DEPTH) {
        classify(format!("lib/{name}"), &dir.join("lib").join(&name));
    }

    // Panes the spec names outside `plugins/` — a plugin obtained as a repository
    // keeps its author's layout. Without this such a pane is LOADED but absent from
    // the inventory, which is exactly the "a file the tab cannot account for"
    // problem the tab exists to prevent.
    //
    // The rest of a working copy is deliberately NOT walked: a repository can hold
    // hundreds of files, none of them interface code, and listing them would bury
    // the panes among one plugin's assets.
    for entry in &spec.plugins {
        if !crate::session::plugin_spec::is_nested_pane(&entry.file) {
            continue;
        }
        let path = dir.join(&entry.file);
        if path.is_file() {
            classify(entry.file.clone(), &path);
        }
    }

    // The spec, which is part of what the interface is made of and is the one file
    // here a person maintains by hand. The LOCK is deliberately absent, along with
    // `.bundled.json` and `ui.json`: bookkeeping nobody edits is not something the
    // tab has to account for, and listing it would only offer actions that refuse.
    let spec = crate::session::plugin_spec::SPEC_FILE;
    if dir.join(spec).is_file() {
        out.insert(spec.to_string(), Source::User);
    }

    for (relative, record) in read_manifest(dir) {
        if record.is_removed() && bundled.contains_key(relative.as_str()) {
            out.insert(relative, Source::Removed);
        }
    }
    out
}

/// Write one embedded file back, clearing whatever the manifest recorded.
///
/// Covers both undo cases at once — a file the user deleted and one they edited
/// — because both are "put back what we ship and forget what happened to it".
pub fn restore(dir: &Path, relative: &str) -> Result<(), String> {
    let path = checked(dir, relative)?;
    let contents = BUNDLED
        .iter()
        .find(|(name, _)| *name == relative)
        .map(|(_, contents)| *contents)
        .ok_or_else(|| format!("{relative} is not part of the bundled interface"))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;

    let mut manifest = read_manifest(dir);
    manifest.insert(relative.to_string(), Record::Written(digest(contents)));
    write_manifest(dir, &manifest)
}

/// Delete one file of the interface.
///
/// A bundled file is tombstoned so delivery leaves it alone from here on; a
/// user's own file is simply gone, and nothing records it — we never wrote it.
pub fn remove(dir: &Path, relative: &str) -> Result<(), String> {
    let path = checked(dir, relative)?;
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;

    let mut manifest = read_manifest(dir);
    if BUNDLED.iter().any(|(name, _)| *name == relative) {
        manifest.insert(relative.to_string(), Record::tombstone());
    } else {
        manifest.remove(relative);
    }
    write_manifest(dir, &manifest)
}

/// Resolve a path that came from a plugin, or refuse it.
///
/// Lua has no filesystem and this command is the one door into one, so the door
/// opens only onto Lua files inside the interface directory: no absolute paths,
/// no `..`, and nothing that is not a `.lua` file — which also keeps the
/// manifest itself out of reach.
fn checked(dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(relative);
    if !candidate
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(format!("{relative} is not inside the interface directory"));
    }
    if candidate.extension() != Some("lua".as_ref()) {
        return Err(format!("{relative} is not a Lua file"));
    }
    Ok(dir.join(candidate))
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Action {
    Write,
    Update,
    Preserve,
    /// Present and identical: nothing to write, but record that it is ours.
    Settle,
    /// Ours, and gone: remember that, and never write it again.
    Tombstone,
    /// Already recorded as removed.
    Leave,
}

/// What delivery last did with one file, as the decision needs to know it.
///
/// Deliberately not [`Record`], which is the manifest's *on-disk* shape: the same
/// decision governs a file that arrived from a package, whose record lives in a
/// lockfile rather than in `.bundled.json`. Stating the decision over this
/// instead is what lets the two delivery paths share one implementation of "do
/// not clobber my edits, remember what I deleted" rather than growing a second
/// copy of the only subtle logic here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivered<'a> {
    /// Never delivered to this path.
    Never,
    /// Delivered, and these were the contents.
    Digest(&'a str),
    /// Delivered, and since deleted by the user.
    Tombstoned,
}

/// What to do with one file, given what is there, what we last put there, and
/// what we would put there now.
///
/// Pure, and the whole of the policy — every caller that writes a delivered file
/// asks this first. Two properties are load-bearing and are why the order of the
/// arms matters:
///
/// * **An edit is the user's to keep.** A file whose contents do not match what
///   we recorded is `Preserve`, whatever we would rather write.
/// * **A deletion is remembered.** A file we wrote and that is now gone is
///   tombstoned once and left alone forever after, so removing a plugin is a way
///   to remove it rather than a thing delivery undoes on next launch.
///
/// `Settle` is tested before `Update` so a byte-identical file is adopted without
/// a write — including one the user deleted and then put back unchanged, which is
/// theirs by the first rule and ours by the second, and is resolved in favour of
/// adopting it.
pub(crate) fn decide(existing: Option<&str>, delivered: Delivered<'_>, payload: &str) -> Action {
    match (existing, delivered) {
        // Never delivered here: a first run, or a file new in this release.
        (None, Delivered::Never) => Action::Write,
        // Already known to be gone. Say nothing; it was said once.
        (None, Delivered::Tombstoned) => Action::Leave,
        // We wrote it and it is no longer there: the user removed it.
        (None, Delivered::Digest(_)) => Action::Tombstone,
        (Some(current), _) if current == payload => Action::Settle,
        // Replace only what we last wrote and the user has not touched.
        (Some(current), Delivered::Digest(recorded)) if recorded == digest(current) => {
            Action::Update
        }
        (Some(_), _) => Action::Preserve,
    }
}

fn read_manifest(dir: &Path) -> BTreeMap<String, Record> {
    std::fs::read_to_string(dir.join(MANIFEST))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_manifest(dir: &Path, manifest: &BTreeMap<String, Record>) -> Result<(), String> {
    let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(MANIFEST), text)
        .map_err(|e| format!("{}: {e}", dir.join(MANIFEST).display()))
}

/// Which rule chose the interface directory.
///
/// Reported alongside the path because "where" without "why" is what makes the
/// wrong-directory mistake so easy to repeat: a session that sees the user's copy
/// when it expected a checkout's `./ui` learns nothing from the path alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chosen {
    /// `THURBOX_UI_DIR` named it.
    Override,
    /// A `ui` directory beside the working directory, named by `THURBOX_UI_DIR`.
    ///
    /// Kept as a distinct reason from [`Chosen::Override`] purely so the report can
    /// say "the checkout" when the override happens to point at one. It is no
    /// longer reachable without the variable: an automatic `./ui` rule made the
    /// interface the one config that ignored the `thurbox` / `thurbox-dev` split,
    /// so `cargo run` in the repository read `~/.config/thurbox-dev` for agents,
    /// settings, themes and the database and the *checkout* for its panes.
    Checkout,
    /// The user's own copy, materialized from the embedded interface.
    UserCopy,
    /// The embedded interface in a throwaway directory, because the user's copy
    /// could not be written.
    Fallback,
}

impl Chosen {
    /// A short name for reports and machine output.
    pub fn as_str(self) -> &'static str {
        match self {
            Chosen::Override => "override",
            Chosen::Checkout => "checkout",
            Chosen::UserCopy => "user-copy",
            Chosen::Fallback => "fallback",
        }
    }

    /// Why this directory, in words.
    pub fn reason(self) -> &'static str {
        match self {
            Chosen::Override => "THURBOX_UI_DIR names it",
            Chosen::Checkout => "THURBOX_UI_DIR pointing at a checkout's ./ui",
            Chosen::UserCopy => "your own copy of the interface",
            Chosen::Fallback => "the embedded interface — your copy could not be written",
        }
    }
}

/// The interface directory, and which rule chose it.
///
/// In order: `THURBOX_UI_DIR`, a `./ui` dev checkout, then the user's own copy —
/// materialized from the embedded interface on first run, preserving anything
/// they edited. A missing or unwritable config directory is not fatal: the
/// embedded copies are written somewhere throwaway and used from there, because
/// no interface at all is the one outcome worth avoiding
/// (`v2-plugin-kernel` design.md D11).
///
/// Lives here rather than in the interface binary so that anything asking "which
/// directory is live" gets the same answer the interface will use — two
/// implementations of that is the mistake this exists to prevent.
pub fn resolve(materialize_user_copy: bool) -> Result<(PathBuf, Chosen, Report), String> {
    if let Some(dir) = std::env::var_os("THURBOX_UI_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            // Reported as the checkout when that is plainly what it is, so the
            // answer to "which interface am I running" names the thing the reader
            // recognises rather than just the mechanism.
            let chosen = if dir.file_name().is_some_and(|name| name == "ui")
                && dir.join("plugins").is_dir()
                && Path::new("Cargo.toml").is_file()
            {
                Chosen::Checkout
            } else {
                Chosen::Override
            };
            return Ok((absolute(dir), chosen, Report::default()));
        }
        return Err(format!(
            "THURBOX_UI_DIR is not a directory: {}",
            dir.display()
        ));
    }
    // No automatic `./ui` rule. It existed so a checkout could edit its own
    // interface, and the cost was that the interface stopped following the config
    // path every other setting follows: `cargo run` from the repository loaded the
    // repository's panes while reading `~/.config/thurbox-dev` for everything else,
    // silently, with nothing on screen to say which. Editing a checkout's interface
    // is now an explicit `THURBOX_UI_DIR=ui` (see `just tui-ui`), which is the same
    // request stated out loud.
    if let Some(dir) = user_ui_dir() {
        // Only the interface writes; a report is a read, and must not create a
        // profile as a side effect of being asked a question.
        if !materialize_user_copy {
            return Ok((absolute(dir), Chosen::UserCopy, Report::default()));
        }
        let report = materialize(&dir);
        if report.errors.is_empty() {
            return Ok((absolute(dir), Chosen::UserCopy, report));
        }
    }
    Ok((fallback_dir()?, Chosen::Fallback, Report::default()))
}

/// Make a resolved interface directory an absolute identity.
///
/// Trust, the disabled set and every rebinding are keyed by `ui_dir.join(file)`
/// and compared verbatim, so a **relative** directory is not an identity: `ui` in
/// one checkout and `ui` in another produce the same key, and trusting
/// `plugins/70_tools.lua` in one repository would silently grant `run` to a
/// different file of the same name in the next.
///
/// Resolved against the working directory rather than *canonicalised*, which is
/// what `watch::Watcher::new` does for its own purposes. Canonicalising is wrong
/// here because this path is also shown to people and stored: on Windows it
/// returns an extended-length path (`\\?\D:\…`), which then appears in the
/// repository list and the Interface tab, and on macOS it resolves `/var` to
/// `/private/var`, so the directory the user typed is not the one reported back.
/// This needs a stable identity, not the shortest true path — symlinks and all.
fn absolute(dir: PathBuf) -> PathBuf {
    if dir.is_absolute() {
        return dir;
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(dir),
        // No working directory to resolve against is not a reason to have no
        // interface; the key is merely weaker than it wanted to be.
        Err(_) => dir,
    }
}

/// Where the user's copy of the interface lives.
pub fn user_ui_dir() -> Option<PathBuf> {
    crate::paths::config_file().and_then(|config| config.parent().map(|dir| dir.join("ui")))
}

/// A throwaway directory holding the embedded interface, for use when the
/// user's copy will not load.
///
/// Materializing into a fresh directory rather than loading from memory keeps
/// one loading path in the host: whatever ran before still runs, and `require`
/// resolves the same way. It is fresh, so it carries no manifest and therefore
/// none of the user's removals — the recovery floor is the whole interface.
pub fn fallback_dir() -> Result<PathBuf, String> {
    // Created **exclusively**, not `create_dir_all`ed into. The name is
    // predictable (a pid), the system temp directory is world-writable, and
    // `materialize` deliberately preserves files it finds — so a directory
    // pre-created by somebody else would hand thurbox their Lua as its whole
    // interface, with whatever the user has trusted. A directory we did not create
    // is not ours to load from, so move to the next name rather than adopt it.
    let base = std::env::temp_dir();
    let pid = std::process::id();
    let mut refused = Vec::new();
    for attempt in 0..64u32 {
        let dir = match attempt {
            0 => base.join(format!("thurbox-ui-fallback-{pid}")),
            n => base.join(format!("thurbox-ui-fallback-{pid}-{n}")),
        };
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                let report = materialize(&dir);
                if !report.errors.is_empty() {
                    return Err(report.errors.join("; "));
                }
                return Ok(dir);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                refused.push(dir.display().to_string());
            }
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        }
    }
    Err(format!(
        "no directory for the recovery interface: {} already exist",
        refused.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decision matrix, arm by arm.
    ///
    /// Tested here rather than only through `materialize` because both delivery
    /// paths now depend on it — a package install asks the same question about a
    /// file recorded in a lockfile — and a matrix reached only through the
    /// filesystem is one whose arms are easy to leave uncovered.
    mod decide {
        use super::*;

        /// Contents whose digest a record can be built from.
        const OLD: &str = "-- v1\n";
        const NEW: &str = "-- v2\n";
        const MINE: &str = "-- mine\n";

        #[test]
        fn nothing_there_and_nothing_recorded_is_written() {
            assert_eq!(decide(None, Delivered::Never, NEW), Action::Write);
        }

        #[test]
        fn nothing_there_but_we_wrote_it_once_is_tombstoned() {
            // The user deleted it. Recorded so the next run leaves it alone —
            // this is the arm that makes deleting a plugin a way to remove it.
            let recorded = digest(OLD);
            assert_eq!(
                decide(None, Delivered::Digest(&recorded), NEW),
                Action::Tombstone
            );
        }

        #[test]
        fn nothing_there_and_already_tombstoned_is_left() {
            // And said once, not once per launch.
            assert_eq!(decide(None, Delivered::Tombstoned, NEW), Action::Leave);
        }

        #[test]
        fn what_we_would_write_is_already_there_so_it_settles() {
            assert_eq!(decide(Some(NEW), Delivered::Never, NEW), Action::Settle);
            let recorded = digest(OLD);
            assert_eq!(
                decide(Some(NEW), Delivered::Digest(&recorded), NEW),
                Action::Settle
            );
        }

        #[test]
        fn ours_and_untouched_is_updated() {
            let recorded = digest(OLD);
            assert_eq!(
                decide(Some(OLD), Delivered::Digest(&recorded), NEW),
                Action::Update
            );
        }

        #[test]
        fn a_file_the_user_changed_is_preserved() {
            // The load-bearing arm: whatever we would rather write, an edit stays.
            let recorded = digest(OLD);
            assert_eq!(
                decide(Some(MINE), Delivered::Digest(&recorded), NEW),
                Action::Preserve
            );
        }

        #[test]
        fn a_file_we_never_wrote_is_preserved() {
            // Someone else's file at a path we now ship. Never overwritten.
            assert_eq!(decide(Some(MINE), Delivered::Never, NEW), Action::Preserve);
        }

        #[test]
        fn a_tombstone_does_not_authorise_overwriting_what_replaced_it() {
            // Deleted, then a file of their own put at the same path. The
            // tombstone says "do not write here", not "this one is ours".
            assert_eq!(
                decide(Some(MINE), Delivered::Tombstoned, NEW),
                Action::Preserve
            );
        }

        #[test]
        fn putting_back_an_identical_copy_readopts_it() {
            // Deleted, then restored byte-identically. Theirs by the edit rule and
            // ours by the deletion rule; `Settle` before `Update` resolves it in
            // favour of adopting it, so the next release can update it again.
            assert_eq!(
                decide(Some(NEW), Delivered::Tombstoned, NEW),
                Action::Settle
            );
        }
    }

    /// The absolute→relative conversion, including the Windows separator.
    ///
    /// Written with literal `\` rather than by joining paths, so it runs the
    /// Windows case on Linux too: `Path::join` writes `/` here, which is exactly
    /// why a hand-rolled strip looked correct on every developer machine while
    /// "turn a plugin off" was a silent no-op on Windows. Caught by a Windows CI
    /// run, and this is what stops it coming back.
    #[test]
    fn a_stored_path_becomes_the_identity_the_loader_compares() {
        let dir = Path::new("/home/me/.config/thurbox/ui");
        assert_eq!(
            relative_to(dir, "/home/me/.config/thurbox/ui/plugins/90_notes.lua").as_deref(),
            Some("plugins/90_notes.lua")
        );

        // A Windows root, with the separators `Path::join` would have written.
        let windows = Path::new(r"C:\Users\me\AppData\Roaming\thurbox\ui");
        assert_eq!(
            relative_to(
                windows,
                r"C:\Users\me\AppData\Roaming\thurbox\ui\plugins\90_notes.lua"
            )
            .as_deref(),
            Some("plugins/90_notes.lua"),
            "`build` keys on `plugins/<name>`, so a backslash here matches nothing"
        );

        // Not inside the directory, and the directory itself: neither is a file
        // identity, and returning `""` for the second is how an empty entry used
        // to reach the disabled set.
        assert_eq!(relative_to(dir, "/elsewhere/plugins/x.lua"), None);
        assert_eq!(relative_to(dir, "/home/me/.config/thurbox/ui"), None);
        assert_eq!(relative_to(dir, "/home/me/.config/thurbox/ui/"), None);
    }

    /// The directory tells whoever edits it how to, under the name their tool reads.
    ///
    /// `README.md` is what a person opens; `AGENTS.md` is what a coding CLI loads as
    /// context on its own. Without the second, "install this plugin" reads as a
    /// package-manager request — which is the specific confusion it exists to
    /// prevent, so the instruction that prevents it is pinned here.
    #[test]
    fn the_interface_ships_its_own_guidance_for_whoever_edits_it() {
        let file = |name: &str| -> &str {
            BUNDLED
                .iter()
                .find(|(path, _)| *path == name)
                .map(|(_, contents)| *contents)
                .unwrap_or_else(|| panic!("{name} is not bundled"))
        };

        let agents = file("AGENTS.md");
        assert!(
            agents.contains("thurbox-cli plugin install"),
            "the command an agent needs when told to install a plugin"
        );
        assert!(
            agents.contains("npm") && agents.contains("cargo"),
            "and the package managers it must NOT reach for, named so the \
             instruction is unmissable"
        );
        assert!(
            agents.contains("thurbox-cli plugin check"),
            "and the gate for every edit"
        );

        // The per-agent files point at it rather than repeating it: two copies
        // drift, and the stale one is the one somebody reads.
        for pointer in ["CLAUDE.md", "GEMINI.md"] {
            let contents = file(pointer);
            assert!(contents.contains("AGENTS.md"), "{pointer} must point at it");
            assert!(
                contents.len() < agents.len() / 4,
                "{pointer} is a pointer, not a copy"
            );
        }
    }

    #[test]
    fn the_bundle_covers_the_whole_interface() {
        // A file that stops shipping is worse than one that fails to compile.
        let names: Vec<&str> = BUNDLED.iter().map(|(name, _)| *name).collect();
        for required in [
            "layout.lua",
            "lib/theme.lua",
            "lib/widgets.lua",
            "plugins/10_sessions.lua",
            "plugins/20_agent.lua",
        ] {
            assert!(names.contains(&required), "{required} is not bundled");
        }
        for (name, contents) in BUNDLED {
            assert!(!contents.is_empty(), "{name} is empty");
        }
    }

    #[test]
    fn a_fresh_directory_gets_the_whole_interface() {
        let dir = tempfile::tempdir().expect("tempdir");
        let report = materialize(dir.path());
        assert_eq!(report.written.len(), BUNDLED.len());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(dir.path().join("plugins/10_sessions.lua").exists());
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        let report = materialize(dir.path());
        assert!(report.is_empty(), "{report:?}");
    }

    /// A file new in this release reaches a directory that already exists.
    ///
    /// The upgrade path, and the one that matters for guidance in particular:
    /// `AGENTS.md` is worth nothing if only fresh installs get it. Delivery
    /// re-runs on every launch, and a path never written here is `Write` — while an
    /// edited neighbour is still left alone in the same pass.
    #[test]
    fn a_file_new_in_this_release_reaches_an_existing_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());

        // Look like an older delivery: the file was never written, and the manifest
        // never knew about it.
        let newcomer = "AGENTS.md";
        std::fs::remove_file(dir.path().join(newcomer)).expect("remove");
        let mut manifest = read_manifest(dir.path());
        manifest.remove(newcomer);
        write_manifest(dir.path(), &manifest).expect("write manifest");

        // And carry an edit into the upgrade, which must survive it.
        let edited = dir.path().join("plugins/10_sessions.lua");
        std::fs::write(&edited, "-- mine\n").expect("edit");

        let report = materialize(dir.path());
        assert!(
            report.written.contains(&newcomer.to_string()),
            "a file new in this release is delivered on upgrade: {report:?}"
        );
        assert!(dir.path().join(newcomer).is_file());
        assert_eq!(
            std::fs::read_to_string(&edited).expect("read"),
            "-- mine\n",
            "and the edit beside it is untouched"
        );
        assert!(report
            .preserved
            .contains(&"plugins/10_sessions.lua".to_string()));
    }

    #[test]
    fn a_user_edit_is_preserved_and_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());

        let edited = dir.path().join("lib/theme.lua");
        std::fs::write(&edited, "-- mine\nreturn {}").expect("edit");

        // A newer binary would carry different contents; materializing again
        // must not clobber the edit.
        let report = materialize(dir.path());
        assert_eq!(report.preserved, vec!["lib/theme.lua"]);
        assert_eq!(
            std::fs::read_to_string(&edited).expect("read"),
            "-- mine\nreturn {}"
        );
    }

    #[test]
    fn an_unmodified_file_is_updated_on_upgrade() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());

        // Simulate the previous release having shipped different contents: the
        // file on disk differs, but the manifest says we wrote it.
        let path = dir.path().join("lib/theme.lua");
        let older = "-- an older release\nreturn {}";
        std::fs::write(&path, older).expect("write");
        let mut manifest = read_manifest(dir.path());
        manifest.insert("lib/theme.lua".into(), Record::Written(digest(older)));
        write_manifest(dir.path(), &manifest).expect("manifest");

        let report = materialize(dir.path());
        assert_eq!(report.updated, vec!["lib/theme.lua"]);
        assert_ne!(std::fs::read_to_string(&path).expect("read"), older);
    }

    #[test]
    fn a_deleted_file_stays_deleted() {
        // The whole point of the manifest's third state: deleting a plugin is
        // how you remove it, so delivery must not undo that on the next start.
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        let path = dir.path().join("plugins/10_sessions.lua");
        std::fs::remove_file(&path).expect("remove");

        let report = materialize(dir.path());
        assert_eq!(report.removed, vec!["plugins/10_sessions.lua"]);
        assert!(!path.exists());

        // And it is said once, not on every start.
        let report = materialize(dir.path());
        assert!(report.is_empty(), "{report:?}");
        assert!(!path.exists());
    }

    #[test]
    fn a_deletion_survives_an_upgrade_that_changes_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        std::fs::remove_file(dir.path().join("lib/theme.lua")).expect("remove");
        materialize(dir.path());

        // A newer release carrying different contents still must not write it.
        let mut manifest = read_manifest(dir.path());
        assert!(manifest["lib/theme.lua"].is_removed());
        manifest.insert(
            "plugins/20_agent.lua".into(),
            Record::Written("stale".into()),
        );
        write_manifest(dir.path(), &manifest).expect("manifest");

        let report = materialize(dir.path());
        assert!(!dir.path().join("lib/theme.lua").exists());
        assert!(report.removed.is_empty(), "{report:?}");
    }

    #[test]
    fn discarding_the_directory_delivers_everything_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        std::fs::remove_file(dir.path().join("plugins/10_sessions.lua")).expect("remove");
        materialize(dir.path());

        // The record lives with the directory, so losing one loses the other.
        std::fs::remove_dir_all(dir.path()).expect("discard");
        std::fs::create_dir_all(dir.path()).expect("recreate");

        let report = materialize(dir.path());
        assert_eq!(report.written.len(), BUNDLED.len());
        assert!(dir.path().join("plugins/10_sessions.lua").exists());
    }

    #[test]
    fn a_user_written_file_is_never_touched() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        let mine = dir.path().join("plugins/50_mine.lua");
        std::fs::write(&mine, "return { name = \"mine\" }").expect("write");

        let report = materialize(dir.path());
        assert!(report.is_empty(), "{report:?}");
        assert_eq!(
            std::fs::read_to_string(&mine).expect("read"),
            "return { name = \"mine\" }"
        );
        assert!(!read_manifest(dir.path()).contains_key("plugins/50_mine.lua"));
    }

    #[test]
    fn a_file_that_stops_shipping_is_retired_unless_it_was_edited() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());

        // Two files from a release that shipped them and this one does not.
        let untouched = dir.path().join("plugins/40_gone.lua");
        let claimed = dir.path().join("plugins/41_kept.lua");
        std::fs::write(&untouched, "return {}").expect("write");
        std::fs::write(&claimed, "-- mine now\nreturn {}").expect("write");
        let mut manifest = read_manifest(dir.path());
        manifest.insert(
            "plugins/40_gone.lua".into(),
            Record::Written(digest("return {}")),
        );
        manifest.insert(
            "plugins/41_kept.lua".into(),
            Record::Written(digest("return {}")),
        );
        write_manifest(dir.path(), &manifest).expect("manifest");

        let report = materialize(dir.path());
        assert_eq!(report.retired, vec!["plugins/40_gone.lua"]);
        assert_eq!(report.preserved, vec!["plugins/41_kept.lua"]);
        assert!(!untouched.exists());
        assert!(claimed.exists());
        let manifest = read_manifest(dir.path());
        assert!(!manifest.contains_key("plugins/40_gone.lua"));
        assert!(!manifest.contains_key("plugins/41_kept.lua"));
    }

    #[test]
    fn a_legacy_manifest_still_protects_an_edit() {
        // Earlier releases wrote `path -> "digest"`. Failing to read that would
        // silently overwrite every edit on the upgrade that changed the format.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("lib")).expect("mkdir");
        let path = dir.path().join("lib/theme.lua");
        std::fs::write(&path, "-- mine\nreturn {}").expect("write");
        std::fs::write(
            dir.path().join(MANIFEST),
            "{\"lib/theme.lua\": \"0000000000000000\"}",
        )
        .expect("manifest");

        let report = materialize(dir.path());
        assert_eq!(report.preserved, vec!["lib/theme.lua"]);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "-- mine\nreturn {}"
        );
    }

    #[test]
    fn restoring_undoes_a_removal_and_an_edit() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());

        let removed = "plugins/10_sessions.lua";
        remove(dir.path(), removed).expect("remove");
        assert!(!dir.path().join(removed).exists());
        restore(dir.path(), removed).expect("restore");
        assert!(dir.path().join(removed).exists());
        // And delivery agrees it is ours again, rather than re-tombstoning it.
        let report = materialize(dir.path());
        assert!(report.is_empty(), "{report:?}");

        let edited = dir.path().join("lib/theme.lua");
        std::fs::write(&edited, "-- mine").expect("edit");
        restore(dir.path(), "lib/theme.lua").expect("restore");
        assert_ne!(std::fs::read_to_string(&edited).expect("read"), "-- mine");
    }

    #[test]
    fn removing_a_user_file_records_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        let mine = "plugins/50_mine.lua";
        std::fs::write(dir.path().join(mine), "return {}").expect("write");

        remove(dir.path(), mine).expect("remove");
        assert!(!read_manifest(dir.path()).contains_key(mine));
        // Nothing to restore: we never shipped it.
        assert!(restore(dir.path(), mine).is_err());
    }

    #[test]
    fn the_door_opens_only_onto_lua_inside_the_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        for refused in [
            "../secrets.lua",
            "/etc/passwd.lua",
            ".bundled.json",
            "plugins/../../escape.lua",
        ] {
            assert!(checked(dir.path(), refused).is_err(), "{refused} allowed");
        }
        assert!(checked(dir.path(), "plugins/10_sessions.lua").is_ok());
    }

    #[test]
    fn sources_name_where_each_file_came_from() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        std::fs::write(dir.path().join("lib/theme.lua"), "-- mine").expect("edit");
        std::fs::write(dir.path().join("plugins/50_mine.lua"), "return {}").expect("write");
        remove(dir.path(), "plugins/20_agent.lua").expect("remove");

        let sources = sources(dir.path());
        assert_eq!(sources["layout.lua"], Source::Bundled);
        assert_eq!(sources["lib/theme.lua"], Source::Edited);
        assert_eq!(sources["plugins/50_mine.lua"], Source::User);
        assert_eq!(sources["plugins/20_agent.lua"], Source::Removed);
    }

    /// A file the user did not write is a different answer from one they did,
    /// because a capability is granted per file and the source is the fact to know
    /// before granting it.
    #[test]
    fn an_installed_file_names_the_source_it_came_from() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());

        let entry = crate::session::PluginEntry {
            src: "atlas".into(),
            file: "plugins/75_atlas.lua".into(),
            pin: Some("v0.3.1".into()),
        };
        let mut lock = crate::session::PluginLock::default();
        super::super::packages::deliver(
            dir.path(),
            &entry,
            "https://example.com/atlas",
            "v0.3.1",
            &[
                super::super::packages::Payload {
                    file: "plugins/75_atlas.lua".into(),
                    contents: "return {}\n".into(),
                },
                super::super::packages::Payload {
                    file: "lib/atlas/util.lua".into(),
                    contents: "return {}\n".into(),
                },
            ],
            &mut lock,
        )
        .expect("deliver");
        super::super::packages::write_lock(dir.path(), &lock).expect("write lock");

        let fresh = sources(dir.path());
        assert_eq!(
            fresh["plugins/75_atlas.lua"],
            Source::Installed {
                src: "atlas".into()
            }
        );
        assert!(
            !fresh["plugins/75_atlas.lua"].is_theirs(),
            "an installed pane is not the user's own file"
        );
        assert_eq!(
            fresh["plugins/75_atlas.lua"].installed_from(),
            Some("atlas")
        );
        // A module the package brought is traceable too — otherwise a `lib/` file
        // somebody else shipped reads as one the user wrote.
        assert_eq!(fresh["lib/atlas/util.lua"].installed_from(), Some("atlas"));

        // Changed since: locally, or re-tagged upstream. The inventory cannot tell
        // which, and says only that the contents are no longer the version's.
        std::fs::write(dir.path().join("plugins/75_atlas.lua"), "-- mine\n").expect("edit");
        let after = sources(dir.path());
        assert_eq!(
            after["plugins/75_atlas.lua"],
            Source::InstalledEdited {
                src: "atlas".into()
            }
        );
        assert_eq!(
            after["plugins/75_atlas.lua"].installed_from(),
            Some("atlas")
        );
    }

    #[test]
    fn the_spec_is_inventoried_and_the_lock_is_not() {
        // The spec is part of what the interface is made of and a person maintains
        // it. The lock is bookkeeping, like `.bundled.json` and `ui.json`, and
        // listing it would only offer actions that refuse.
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        super::super::packages::add_to_spec(
            dir.path(),
            &crate::session::PluginEntry {
                src: "atlas".into(),
                file: "plugins/75_atlas.lua".into(),
                pin: None,
            },
        )
        .expect("add");
        std::fs::write(super::super::packages::lock_path(dir.path()), "").expect("lock");

        let sources = sources(dir.path());
        assert!(sources.contains_key(crate::session::plugin_spec::SPEC_FILE));
        assert!(!sources.contains_key(crate::session::plugin_spec::LOCK_FILE));
        assert!(!sources.contains_key(MANIFEST));
    }

    /// A malformed lock must not take the inventory with it: the inventory is the
    /// recovery tool, so it degrades to "I do not know" rather than refusing.
    #[test]
    fn a_broken_lock_does_not_break_the_inventory() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        std::fs::write(dir.path().join("plugins/50_mine.lua"), "return {}").expect("write");
        std::fs::write(
            super::super::packages::lock_path(dir.path()),
            "[[plugin]]\nsrc =\n",
        )
        .expect("write");

        let sources = sources(dir.path());
        assert_eq!(sources["layout.lua"], Source::Bundled);
        assert_eq!(sources["plugins/50_mine.lua"], Source::User);
    }

    #[test]
    fn the_fallback_directory_holds_a_loadable_interface() {
        let dir = fallback_dir().expect("fallback");
        assert!(dir.join("plugins").is_dir());
        assert!(dir.join("layout.lua").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_digest_distinguishes_contents() {
        assert_eq!(digest("a"), digest("a"));
        assert_ne!(digest("a"), digest("b"));
    }

    /// The recovery floor must never adopt a directory somebody else made. The
    /// name is a pid in a world-writable temp directory and `materialize`
    /// preserves what it finds, so adopting one would load a stranger's Lua as the
    /// whole interface — under whatever the user has already trusted.
    #[test]
    fn the_fallback_refuses_a_directory_it_did_not_create() {
        let planted =
            std::env::temp_dir().join(format!("thurbox-ui-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&planted);
        std::fs::create_dir_all(planted.join("plugins")).expect("plant");
        std::fs::write(planted.join("plugins/99_evil.lua"), "return {}").expect("plant");

        let dir = fallback_dir().expect("fallback");
        assert_ne!(dir, planted, "adopted a pre-existing directory");
        assert!(
            !dir.join("plugins/99_evil.lua").exists(),
            "the planted file came along: {}",
            dir.display()
        );
        assert!(dir.join("layout.lua").exists(), "still a whole interface");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&planted);
    }

    /// Every key the interface stores — trust, disabled, rebindings — is
    /// `ui_dir.join(file)` compared verbatim, so the directory has to be an
    /// absolute identity. A relative `ui` is shared by every checkout on the
    /// machine, which made trusting a plugin in one worktree grant `run` to a
    /// same-named file in another.
    #[test]
    fn the_resolved_directory_is_absolute_and_still_the_one_that_was_asked_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("plugins")).expect("mkdir");
        std::env::set_var("THURBOX_UI_DIR", dir.path());
        let (resolved, chosen, _) = resolve(false).expect("resolve");
        std::env::remove_var("THURBOX_UI_DIR");

        assert_eq!(chosen, Chosen::Override);
        assert!(
            resolved.is_absolute(),
            "a relative interface directory is not an identity: {}",
            resolved.display()
        );
        // The second half of the contract, and the one that is easy to lose:
        // an already-absolute directory comes back *unchanged*. Canonicalising
        // would satisfy the assertion above while returning something else --
        // an extended-length `\\?\D:\…` on Windows, `/private/var` for `/var` on
        // macOS -- and this path is shown in the repository list and the
        // Interface tab, so it has to be the directory the user named.
        assert_eq!(
            resolved,
            dir.path(),
            "the resolved directory must be the one that was asked for"
        );
    }
}
