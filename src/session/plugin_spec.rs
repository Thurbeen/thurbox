//! What the interface is made of — the spec, and the record of what it resolved
//! to.
//!
//! Two files in the interface directory, and the split between them is the same
//! one `.bundled.json` and `ui.json` already make: [`SPEC_FILE`] is **hand-edited
//! composition** and [`LOCK_FILE`] is a **machine-written record**. You read a
//! spec diff and you skim a lock diff; a merge conflict in the record must not
//! dirty the half a person maintains.
//!
//! ```toml
//! # ui/plugins.toml
//! [[plugin]]
//! src  = "atlas"                  # bare name, URL, or path
//! file = "plugins/75_atlas.lua"   # load order lives in the filename
//! pin  = "v0.3.1"                 # omit to take the newest at install time
//! ```
//!
//! TOML because `docs/CONFIG.md`'s own rule is that hand-edited registries are
//! TOML, and because a malformed edit becomes a parse error naming its line
//! rather than a nil three frames later. Pure data here in `session` — the
//! dependency sink — so the kernel (which classifies a file's origin and resolves
//! trust) and the CLI (which installs and converges) share one definition,
//! exactly as they do for [`crate::session::AgentDef`] and
//! [`crate::session::HostDef`]. Nothing here touches the filesystem.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The hand-edited spec, in the interface directory.
pub const SPEC_FILE: &str = "plugins.toml";

/// The machine-written record, beside it.
pub const LOCK_FILE: &str = "plugins.lock";

/// One plugin the interface is composed of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEntry {
    /// Where it comes from: a bare name resolving to the repository's examples for
    /// this release, a URL, or a filesystem path. Resolved by the same rules an
    /// extension source is, so one vocabulary covers both.
    pub src: String,
    /// Where it is delivered, relative to the interface directory. Carries the
    /// load order in its filename, because that is where the interface has always
    /// kept it.
    pub file: String,
    /// The version to use. Absent means "whatever was newest when this was
    /// installed", which the lock then pins for everyone else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
}

impl PluginEntry {
    /// The name this entry answers to on the command line.
    ///
    /// A bare-name source is its own name; anything else is named by the file it
    /// delivers, since a URL is not something anyone wants to retype — with the
    /// leading load-order prefix dropped, because `75_` is where the pane sits in
    /// the order and not what it is called. Nobody types `plugin remove 75_atlas`.
    pub fn name(&self) -> &str {
        if is_bare_name(&self.src) {
            &self.src
        } else {
            strip_order(file_stem(&self.file))
        }
    }

    /// Refuse an entry that could write outside the interface directory, or that
    /// names something the host would never load.
    ///
    /// Refused rather than sanitised, for the reason `plugin new` refuses a bad
    /// name: a spec that quietly delivers somewhere other than where it says is
    /// worse than one that stops.
    pub fn validate(&self) -> Result<(), String> {
        if self.src.trim().is_empty() {
            return Err(format!("{}: src is empty", self.file));
        }
        validate_destination(&self.file)
    }
}

/// Refuse a delivery path that escapes the interface directory or is not Lua.
///
/// Shared with the package manifest, whose declared files land under the same
/// directory and are subject to the same rule.
pub fn validate_destination(file: &str) -> Result<(), String> {
    if file.trim().is_empty() {
        return Err("file is empty".to_string());
    }
    if file.starts_with('/') || file.starts_with('\\') || file.contains(':') {
        return Err(format!(
            "{file}: must be relative to the interface directory"
        ));
    }
    // Both separators, because a spec written on Windows is a spec that has to
    // load on Linux.
    if file
        .split(['/', '\\'])
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!(
            "{file}: must not step outside the interface directory"
        ));
    }
    if !file.ends_with(".lua") {
        return Err(format!("{file}: must be a Lua file"));
    }
    Ok(())
}

/// The pane a package manifest names, read **leniently**.
///
/// Only `pane.source` — the path within the package — and none of the copy-model
/// rules [`PackageManifest::validate`] enforces. Those rules exist for a package
/// whose files are copied to prescribed destinations; a repository keeps its own
/// layout, so a manifest written for a clone would legitimately fail them (its
/// modules live at `lib/…` inside its own tree, not at `lib/<name>/…` inside the
/// interface). Reading strictly here would reject a correct repository for breaking
/// a rule that does not apply to it.
pub fn pane_source_of(text: &str) -> Option<String> {
    let value: toml::Value = toml::from_str(text).ok()?;
    let source = value.get("pane")?.get("source")?.as_str()?.trim();
    (!source.is_empty()).then(|| source.to_string())
}

/// Is this destination inside an installed plugin's own directory rather than the
/// interface's shared `plugins/`?
///
/// A plugin obtained as a repository keeps the layout its author chose, so its pane
/// lives at `<name>/…` — which the loader has to be told about, because it otherwise
/// reads only the top level of `plugins/`.
pub fn is_nested_pane(file: &str) -> bool {
    !file.starts_with("plugins/") && !file.starts_with("lib/") && file.contains('/')
}

/// Whether a source is a bare name rather than a URL or a path.
///
/// Bare names are what resolve against the repository's examples; the
/// discrimination matters here only for naming an entry, and the authoritative
/// resolution lives with the installer.
pub fn is_bare_name(src: &str) -> bool {
    !src.contains("://")
        && !src.contains('/')
        && !src.contains('\\')
        && !src.contains(':')
        && !src.is_empty()
}

fn file_stem(file: &str) -> &str {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    name.strip_suffix(".lua").unwrap_or(name)
}

/// Drop a leading `<digits>_` load-order prefix.
///
/// The interface has always kept load order in the filename, so `75_atlas.lua` is
/// the atlas pane at position 75 — the number is not part of its name.
fn strip_order(stem: &str) -> &str {
    match stem.split_once('_') {
        Some((order, rest)) if !order.is_empty() && order.chars().all(|c| c.is_ascii_digit()) => {
            rest
        }
        _ => stem,
    }
}

/// The interface's composition, as the spec states it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSpec {
    /// `[[plugin]]` entries, in the order they appear.
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<PluginEntry>,
}

impl PluginSpec {
    /// Read a spec, reporting a malformed one with the location of the problem.
    ///
    /// An absent file is not this function's concern: **no spec means nothing
    /// installed**, which is a valid interface, so the caller passes an empty
    /// string or does not call at all.
    pub fn parse(text: &str) -> Result<Self, String> {
        let spec: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        for entry in &spec.plugins {
            entry.validate()?;
        }
        // Two entries delivering to one path cannot both be honoured, and
        // silently letting the last win would make the interface depend on
        // file order in a way nothing reports.
        let mut seen = BTreeMap::new();
        for (index, entry) in spec.plugins.iter().enumerate() {
            if let Some(first) = seen.insert(entry.file.clone(), index) {
                return Err(format!(
                    "{}: delivered twice, by entry {} and entry {}",
                    entry.file,
                    first + 1,
                    index + 1
                ));
            }
        }
        Ok(spec)
    }

    /// Is this file one the spec is responsible for?
    ///
    /// The question `install` asks before writing and `sync` asks before
    /// removing: a file the spec never listed is nobody's to touch.
    pub fn manages(&self, file: &str) -> bool {
        self.plugins.iter().any(|entry| entry.file == file)
    }

    /// The entry a command-line argument refers to.
    ///
    /// Accepts the entry's name, the path it delivers to, its source, or the bare
    /// filename — so `plugin remove atlas`, `plugin remove 75_atlas` and `plugin
    /// remove plugins/75_atlas.lua` all work. What the user has in front of them
    /// is a listing that shows the path, and a message that names it either way.
    pub fn find(&self, key: &str) -> Option<&PluginEntry> {
        self.index_of(key).map(|index| &self.plugins[index])
    }

    /// Position of the entry `key` refers to.
    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.plugins.iter().position(|entry| {
            entry.name() == key
                || entry.file == key
                || entry.src == key
                || file_stem(&entry.file) == key
        })
    }

    /// Render the spec as TOML.
    ///
    /// Used to create the file. An **existing** file is edited through
    /// [`insert_entry`] / [`remove_entry`] instead, which keep the comments a
    /// person put there.
    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }
}

/// A package's own manifest, `plugin.toml`, shipped beside its Lua.
///
/// ```toml
/// name = "atlas"
/// description = "A map of your sessions"
/// version = "v0.3.1"
/// requires_thurbox = ">=2.0"
///
/// # the pane, and where it lands unless `--as` says otherwise
/// pane = { source = "atlas.lua", path = "plugins/75_atlas.lua" }
///
/// # shared modules, which must live under lib/<name>/
/// [[module]]
/// source = "util.lua"
/// path = "lib/atlas/util.lua"
/// ```
///
/// One pane per package, because a spec entry names one destination. A single
/// `.lua` URL is the degenerate case with no manifest at all: one file, no
/// modules, no declared compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageManifest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The version this package calls itself. Recorded in the lock when the
    /// source offers nothing better to pin to.
    #[serde(default)]
    pub version: Option<String>,
    /// The thurbox versions this package expects. Advisory: recorded and
    /// reported, not enforced at load — one place in the kernel knowing about
    /// versions is enough.
    #[serde(default)]
    pub requires_thurbox: Option<String>,
    pub pane: PackageFile,
    /// Shared modules the pane requires.
    #[serde(default, rename = "module")]
    pub modules: Vec<PackageFile>,
}

/// One file a package delivers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageFile {
    /// Path within the package to read from.
    pub source: String,
    /// Destination, relative to the interface directory.
    pub path: String,
}

impl PackageManifest {
    /// The file name a package manifest goes by.
    pub const FILE: &'static str = "plugin.toml";

    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Every file this package delivers, pane first.
    pub fn files(&self) -> impl Iterator<Item = &PackageFile> {
        std::iter::once(&self.pane).chain(self.modules.iter())
    }

    /// Refuse a manifest that would deliver outside the interface directory, or
    /// that would put a module anywhere but its own namespace.
    ///
    /// The namespace rule is the whole of the collision story: `lib/fuzzy.lua` is
    /// a namespace with one tenant, and a package free to write there could
    /// replace a shipped module every other pane requires. Checked at **install**
    /// time rather than at load, because the loader deliberately knows nothing
    /// about packages.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err(format!("{}: name is empty", Self::FILE));
        }
        if !is_bare_name(&self.name) {
            return Err(format!(
                "{}: name {:?} must be one plain segment, since it is also a namespace",
                Self::FILE,
                self.name
            ));
        }
        for file in self.files() {
            validate_destination(&file.path)?;
            validate_destination(&file.source)?;
        }
        let namespace = format!("lib/{}/", self.name);
        for module in &self.modules {
            if !module.path.starts_with(&namespace) {
                return Err(format!(
                    "{}: module {} must be delivered under {namespace} — a package \
                     may not replace a shared module",
                    Self::FILE,
                    module.path
                ));
            }
        }
        if self.pane.path.starts_with("lib/") {
            return Err(format!(
                "{}: the pane {} belongs in plugins/",
                Self::FILE,
                self.pane.path
            ));
        }
        Ok(())
    }
}

/// What one entry resolved to, and what was delivered for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// The source, copied from the spec so the record stands alone.
    pub src: String,
    /// The destination, which is the key the spec is matched on.
    pub file: String,
    /// Where the source actually resolved to — the fetch URL, or the path.
    pub resolved: String,
    /// The version installed. Recorded even when the spec left the pin open, so
    /// the same spec applied elsewhere delivers this rather than whatever is
    /// newest.
    pub version: String,
    /// Digest of every file delivered for this entry, keyed by path relative to
    /// the interface directory.
    ///
    /// More than one because a package may bring shared modules under
    /// `lib/<its own name>/`. This is also what the trust decision reads: a
    /// managed file is trusted at a version only while its contents are still the
    /// ones that version delivered (design D5).
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Files this entry delivered and the user then deleted.
    ///
    /// Recorded **explicitly**, and this is why: "absent from `files`" cannot mean
    /// "deleted", because it is also what a path this version delivers for the
    /// first time looks like. Conflating the two meant a new release that *added*
    /// a module was read as a module the user had removed, and never delivered it.
    /// `.bundled.json` writes its tombstones out for the same reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
}

impl LockEntry {
    /// Digest recorded for one delivered file.
    pub fn digest(&self, file: &str) -> Option<&str> {
        self.files.get(file).map(String::as_str)
    }

    /// The `src@version` a capability grant is recorded against.
    pub fn pin_key(&self) -> String {
        format!("{}@{}", self.src, self.version)
    }
}

/// What each entry resolved to, so the same spec produces the same interface.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginLock {
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<LockEntry>,
}

impl PluginLock {
    /// Read a lock. A malformed one is reported like a malformed spec — it is
    /// machine-written, but it is also a file people merge.
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// The record for a delivered path.
    pub fn entry(&self, file: &str) -> Option<&LockEntry> {
        self.plugins.iter().find(|entry| entry.file == file)
    }

    /// The record covering a delivered path, whether it is the entry's own pane
    /// or a module the same package brought.
    ///
    /// A package's `lib/<name>/…` files are recorded under the entry that
    /// delivered them, so this is how the inventory says where such a file came
    /// from — the alternative being a `lib/` module reported as the user's own.
    pub fn covering(&self, file: &str) -> Option<&LockEntry> {
        self.plugins
            .iter()
            .find(|entry| entry.files.contains_key(file))
    }

    /// Replace or add the record for one entry, keyed by destination.
    pub fn record(&mut self, entry: LockEntry) {
        match self
            .plugins
            .iter()
            .position(|existing| existing.file == entry.file)
        {
            Some(index) => self.plugins[index] = entry,
            None => self.plugins.push(entry),
        }
        self.plugins.sort_by(|a, b| a.file.cmp(&b.file));
    }

    /// Drop the record for a destination, returning it.
    pub fn forget(&mut self, file: &str) -> Option<LockEntry> {
        let index = self.plugins.iter().position(|entry| entry.file == file)?;
        Some(self.plugins.remove(index))
    }

    /// Records the spec no longer lists.
    ///
    /// What convergence removes, and the reason the lock cannot silently disagree
    /// with the spec: the spec is authoritative, so a record beyond it is a file
    /// to take back.
    pub fn beyond(&self, spec: &PluginSpec) -> Vec<&LockEntry> {
        self.plugins
            .iter()
            .filter(|entry| !spec.manages(&entry.file))
            .collect()
    }

    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }
}

/// Add an entry to a spec's text, keeping every comment already in it.
///
/// The spec is the one file here a person maintains by hand, so a command that
/// rewrote it from the parsed form would delete their notes as the price of
/// installing a plugin. `toml_edit` is the same tool `settings.toml` is written
/// back through, and for the same reason.
pub fn insert_entry(text: &str, entry: &PluginEntry) -> Result<String, String> {
    entry.validate()?;
    let mut doc: toml_edit::DocumentMut = if text.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        text.parse()
            .map_err(|e: toml_edit::TomlError| e.to_string())?
    };

    let array = doc
        .entry("plugin")
        .or_insert(toml_edit::Item::ArrayOfTables(
            toml_edit::ArrayOfTables::new(),
        ))
        .as_array_of_tables_mut()
        .ok_or_else(|| format!("{SPEC_FILE}: `plugin` is not a list of entries"))?;

    // An install over an existing entry is an update to it, not a second copy —
    // otherwise `install` twice leaves a spec that cannot be parsed.
    let existing = array
        .iter()
        .position(|table| table.get("file").and_then(|f| f.as_str()) == Some(&entry.file));

    let mut table = toml_edit::Table::new();
    table["src"] = toml_edit::value(&entry.src);
    table["file"] = toml_edit::value(&entry.file);
    if let Some(pin) = &entry.pin {
        table["pin"] = toml_edit::value(pin);
    }
    match existing {
        // Assigning into the slot keeps the entry where the author put it, and
        // keeps any comment attached to the surrounding document.
        Some(index) => *array.get_mut(index).expect("index from position") = table,
        None => array.push(table),
    }
    Ok(doc.to_string())
}

/// Remove an entry from a spec's text, keeping every comment already in it.
///
/// Returns the text and the entry that was removed; `None` when the spec does not
/// list it, which the caller reports rather than treating as success.
pub fn remove_entry(text: &str, key: &str) -> Result<(String, PluginEntry), String> {
    let spec = PluginSpec::parse(text)?;
    let index = spec
        .index_of(key)
        .ok_or_else(|| format!("{key} is not listed in {SPEC_FILE}"))?;
    let removed = spec.plugins[index].clone();

    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    let mut orphaned_header = None;
    if let Some(array) = doc
        .get_mut("plugin")
        .and_then(|item| item.as_array_of_tables_mut())
    {
        // By destination rather than by the parsed index: `toml_edit` and `toml`
        // agree on order here, but matching on the key that identifies an entry
        // cannot drift if one day they do not.
        let at = array
            .iter()
            .position(|table| table.get("file").and_then(|f| f.as_str()) == Some(&removed.file));
        if let Some(at) = at {
            let carried = header_of(array.get(at));
            array.remove(at);
            // Everything above the FIRST entry is attached to it, so removing that
            // entry takes the file's own header with it — and the seeded spec is
            // documented in comments, like `settings.toml`. Whatever stood above
            // the last blank line was addressed to the file rather than to the
            // entry, so it has to be put back.
            if at == 0 {
                match (carried, array.get_mut(0)) {
                    // Down onto whatever is first now.
                    (Some(header), Some(next)) => {
                        let decor = next.decor_mut();
                        let existing = decor
                            .prefix()
                            .and_then(|prefix| prefix.as_str())
                            .unwrap_or_default()
                            .to_string();
                        decor.set_prefix(format!("{header}{existing}"));
                    }
                    // Nothing left to attach it to. An emptied spec still keeps
                    // what it says about itself, so the document holds it.
                    (Some(header), None) => orphaned_header = Some(header),
                    (None, _) => {}
                }
            }
        }
    }
    if let Some(header) = orphaned_header {
        let trailing = doc.trailing().as_str().unwrap_or_default().to_string();
        doc.set_trailing(format!("{header}{trailing}"));
    }
    Ok((doc.to_string(), removed))
}

/// The part of a table's leading comments that was addressed to the file rather
/// than to that table.
///
/// `toml_edit` attaches every byte above the first entry to it, blank lines
/// included, so this is the only way to tell a file header from the comment
/// somebody wrote about the entry immediately below. The split is the **last
/// blank line**: a header, a blank line, then a comment sitting directly on the
/// entry is how the seeded spec is written and how people write TOML generally.
fn header_of(table: Option<&toml_edit::Table>) -> Option<String> {
    let prefix = table?.decor().prefix()?.as_str()?;
    // Splitting on the *raw* text rather than on parsed comments, because that is
    // what has to be written back byte for byte.
    let cut = prefix.rfind("\n\n").map(|at| at + 2)?;
    Some(prefix[..cut].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file header, then an entry with a comment of its own — the two kinds of
    /// comment that have to be told apart when an entry is removed.
    const SPEC: &str = r#"# what this interface is made of

# the map pane
[[plugin]]
src = "atlas"
file = "plugins/75_atlas.lua"
pin = "v0.3.1"

[[plugin]]
src = "https://example.com/notes.lua"
file = "plugins/80_notes.lua"
"#;

    #[test]
    fn a_spec_round_trips() {
        let spec = PluginSpec::parse(SPEC).expect("parses");
        assert_eq!(spec.plugins.len(), 2);
        assert_eq!(spec.plugins[0].src, "atlas");
        assert_eq!(spec.plugins[0].pin.as_deref(), Some("v0.3.1"));
        assert_eq!(spec.plugins[1].pin, None);

        let again = PluginSpec::parse(&spec.to_toml().expect("render")).expect("reparses");
        assert_eq!(again, spec);
    }

    #[test]
    fn an_empty_spec_is_an_interface_with_nothing_installed() {
        let spec = PluginSpec::parse("").expect("an empty spec is valid");
        assert!(spec.plugins.is_empty());
        assert!(!spec.manages("plugins/10_sessions.lua"));
    }

    #[test]
    fn a_malformed_spec_is_reported_with_its_location() {
        let error = PluginSpec::parse("[[plugin]]\nsrc = \n").expect_err("should fail");
        assert!(
            error.contains("line") || error.contains("TOML"),
            "the reader needs to know WHERE: {error}"
        );
    }

    #[test]
    fn an_entry_that_would_escape_the_directory_is_refused() {
        for bad in [
            "../outside.lua",
            "/etc/passwd.lua",
            "plugins/../../x.lua",
            "plugins\\..\\x.lua",
            "plugins/notes.txt",
            "",
        ] {
            let entry = PluginEntry {
                src: "atlas".into(),
                file: bad.into(),
                pin: None,
            };
            assert!(entry.validate().is_err(), "{bad:?} should be refused");
        }
        assert!(PluginEntry {
            src: "atlas".into(),
            file: "plugins/75_atlas.lua".into(),
            pin: None,
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn two_entries_cannot_deliver_to_one_path() {
        let error = PluginSpec::parse(
            "[[plugin]]\nsrc = \"a\"\nfile = \"plugins/x.lua\"\n\
             [[plugin]]\nsrc = \"b\"\nfile = \"plugins/x.lua\"\n",
        )
        .expect_err("should fail");
        assert!(error.contains("plugins/x.lua"), "{error}");
    }

    #[test]
    fn an_entry_is_addressable_by_name_path_or_source() {
        let spec = PluginSpec::parse(SPEC).expect("parses");
        for key in ["atlas", "plugins/75_atlas.lua"] {
            assert_eq!(
                spec.find(key).map(|e| e.src.as_str()),
                Some("atlas"),
                "{key}"
            );
        }
        // A URL source is named by the file it delivers, since nobody retypes a URL
        // — minus the load-order prefix, which is a position and not a name.
        for key in ["notes", "80_notes", "plugins/80_notes.lua"] {
            assert_eq!(
                spec.find(key).map(|e| e.file.as_str()),
                Some("plugins/80_notes.lua"),
                "{key}"
            );
        }
        assert_eq!(spec.plugins[1].name(), "notes");
    }

    #[test]
    fn adding_an_entry_keeps_the_comments_around_it() {
        let text = insert_entry(
            SPEC,
            &PluginEntry {
                src: "top".into(),
                file: "plugins/85_top.lua".into(),
                pin: Some("v1".into()),
            },
        )
        .expect("insert");
        assert!(
            text.contains("# what this interface is made of"),
            "the file's header survived:\n{text}"
        );
        assert!(
            text.contains("# the map pane"),
            "and so did the entry's:\n{text}"
        );
        let spec = PluginSpec::parse(&text).expect("still parses");
        assert_eq!(spec.plugins.len(), 3);
        assert!(spec.manages("plugins/85_top.lua"));
    }

    #[test]
    fn adding_the_same_destination_twice_updates_rather_than_duplicates() {
        // Otherwise installing twice leaves a spec that will not parse.
        let once = insert_entry(
            SPEC,
            &PluginEntry {
                src: "atlas".into(),
                file: "plugins/75_atlas.lua".into(),
                pin: Some("v0.4.0".into()),
            },
        )
        .expect("insert");
        let spec = PluginSpec::parse(&once).expect("parses");
        assert_eq!(spec.plugins.len(), 2);
        assert_eq!(
            spec.find("atlas").and_then(|e| e.pin.as_deref()),
            Some("v0.4.0")
        );
    }

    #[test]
    fn adding_to_an_empty_file_writes_a_usable_spec() {
        let text = insert_entry(
            "",
            &PluginEntry {
                src: "atlas".into(),
                file: "plugins/75_atlas.lua".into(),
                pin: None,
            },
        )
        .expect("insert");
        assert_eq!(PluginSpec::parse(&text).expect("parses").plugins.len(), 1);
    }

    #[test]
    fn removing_the_first_entry_keeps_the_file_header_but_not_its_own_comment() {
        // The spec is documented in comments, like `settings.toml`, and every byte
        // above the first entry belongs to that entry as far as `toml_edit` is
        // concerned — so without carrying the header down, one `plugin remove`
        // would delete the file's documentation.
        let (text, removed) = remove_entry(SPEC, "atlas").expect("remove");
        assert_eq!(removed.file, "plugins/75_atlas.lua");
        assert!(
            text.contains("# what this interface is made of"),
            "the file's header is not the entry's to take:\n{text}"
        );
        assert!(
            !text.contains("# the map pane"),
            "but a comment written about the entry goes with it:\n{text}"
        );
        let spec = PluginSpec::parse(&text).expect("parses");
        assert_eq!(spec.plugins.len(), 1);
        assert!(!spec.manages("plugins/75_atlas.lua"));
        assert!(spec.manages("plugins/80_notes.lua"));
    }

    #[test]
    fn removing_a_later_entry_leaves_everything_above_it_alone() {
        let (text, _) = remove_entry(SPEC, "80_notes").expect("remove");
        assert!(text.contains("# what this interface is made of"), "{text}");
        assert!(text.contains("# the map pane"), "{text}");
        assert_eq!(PluginSpec::parse(&text).expect("parses").plugins.len(), 1);
    }

    #[test]
    fn removing_the_only_entry_is_not_a_lost_header() {
        // Nothing to carry the header down to. It stays where it is, because the
        // document keeps its own trailing trivia.
        let one = "# mine\n\n[[plugin]]\nsrc = \"atlas\"\nfile = \"plugins/a.lua\"\n";
        let (text, _) = remove_entry(one, "atlas").expect("remove");
        assert!(text.contains("# mine"), "{text}");
        assert!(PluginSpec::parse(&text).expect("parses").plugins.is_empty());
    }

    #[test]
    fn removing_something_never_listed_says_so() {
        let error = remove_entry(SPEC, "nope").expect_err("should fail");
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn a_lock_records_every_file_an_entry_delivered() {
        let mut lock = PluginLock::default();
        let mut files = BTreeMap::new();
        files.insert("plugins/75_atlas.lua".to_string(), "aaaa".to_string());
        files.insert("lib/atlas/util.lua".to_string(), "bbbb".to_string());
        lock.record(LockEntry {
            src: "atlas".into(),
            file: "plugins/75_atlas.lua".into(),
            resolved: "https://example.com/atlas".into(),
            version: "v0.3.1".into(),
            files,
            removed: Vec::new(),
        });

        let again = PluginLock::parse(&lock.to_toml().expect("render")).expect("reparses");
        assert_eq!(again, lock);
        let entry = again.entry("plugins/75_atlas.lua").expect("entry");
        assert_eq!(entry.digest("plugins/75_atlas.lua"), Some("aaaa"));
        assert_eq!(entry.pin_key(), "atlas@v0.3.1");
        // A module the package brought is traceable to the entry that brought it,
        // which is what stops it being reported as the user's own file.
        assert_eq!(
            again.covering("lib/atlas/util.lua").map(|e| e.src.as_str()),
            Some("atlas")
        );
    }

    #[test]
    fn recording_the_same_destination_twice_replaces_it() {
        let mut lock = PluginLock::default();
        for version in ["v1", "v2"] {
            lock.record(LockEntry {
                src: "atlas".into(),
                file: "plugins/75_atlas.lua".into(),
                resolved: "https://example.com/atlas".into(),
                version: version.into(),
                files: BTreeMap::new(),
                removed: Vec::new(),
            });
        }
        assert_eq!(lock.plugins.len(), 1);
        assert_eq!(lock.plugins[0].version, "v2");
    }

    #[test]
    fn a_record_the_spec_no_longer_lists_is_what_convergence_takes_back() {
        let spec = PluginSpec::parse(SPEC).expect("parses");
        let mut lock = PluginLock::default();
        for file in ["plugins/75_atlas.lua", "plugins/99_gone.lua"] {
            lock.record(LockEntry {
                src: "atlas".into(),
                file: file.into(),
                resolved: "https://example.com/atlas".into(),
                version: "v1".into(),
                files: BTreeMap::new(),
                removed: Vec::new(),
            });
        }
        let beyond: Vec<&str> = lock.beyond(&spec).iter().map(|e| e.file.as_str()).collect();
        assert_eq!(beyond, ["plugins/99_gone.lua"]);

        assert!(lock.forget("plugins/99_gone.lua").is_some());
        assert!(lock.beyond(&spec).is_empty());
    }

    #[test]
    fn a_bare_name_is_told_from_a_url_or_a_path() {
        for bare in ["atlas", "top-panel", "notes2"] {
            assert!(is_bare_name(bare), "{bare}");
        }
        for not in [
            "https://example.com/x.lua",
            "./local/dir",
            "/abs/dir",
            "C:\\panes",
            "",
        ] {
            assert!(!is_bare_name(not), "{not}");
        }
    }
}
