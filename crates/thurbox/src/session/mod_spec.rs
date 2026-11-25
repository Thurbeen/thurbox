use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const SPEC_FILE: &str = "mods.toml";

pub const LOCK_FILE: &str = "mods.lock";

pub const MODS_DIR: &str = "mods";

pub const LEGACY_MODS_DIR: &str = "plugins";

pub fn mod_destination(file: &str) -> String {
    match file
        .strip_prefix("plugins/")
        .or_else(|| file.strip_prefix("plugins\\"))
    {
        Some(rest) => format!("{MODS_DIR}/{rest}"),
        None => file.to_string(),
    }
}

pub fn rename_entries_key(doc: &mut toml_edit::DocumentMut) {
    if doc.contains_key(ENTRIES_KEY) {
        return;
    }
    if let Some(entries) = doc.remove(LEGACY_ENTRIES_KEY) {
        doc.insert(ENTRIES_KEY, entries);
    }
}

pub fn for_each_entry_file(
    doc: &mut toml_edit::DocumentMut,
    mut visit: impl FnMut(&mut toml_edit::Value),
) {
    match doc.get_mut(ENTRIES_KEY) {
        Some(toml_edit::Item::ArrayOfTables(tables)) => {
            for table in tables.iter_mut() {
                if let Some(file) = table
                    .get_mut("file")
                    .and_then(toml_edit::Item::as_value_mut)
                {
                    visit(file);
                }
            }
        }
        Some(toml_edit::Item::Value(toml_edit::Value::Array(entries))) => {
            for entry in entries.iter_mut() {
                if let Some(file) = entry
                    .as_inline_table_mut()
                    .and_then(|table| table.get_mut("file"))
                {
                    visit(file);
                }
            }
        }
        _ => {}
    }
}

fn entries_as_tables(doc: &mut toml_edit::DocumentMut) {
    let Some(toml_edit::Item::Value(toml_edit::Value::Array(entries))) = doc.get(ENTRIES_KEY)
    else {
        return;
    };
    let mut tables = toml_edit::ArrayOfTables::new();
    for entry in entries.iter() {
        let Some(table) = entry.as_inline_table() else {
            return;
        };
        tables.push(table.clone().into_table());
    }
    doc.insert(ENTRIES_KEY, toml_edit::Item::ArrayOfTables(tables));
}

const ENTRIES_KEY: &str = "mod";
const LEGACY_ENTRIES_KEY: &str = "plugin";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModEntry {
    pub src: String,
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
}

impl ModEntry {
    pub fn name(&self) -> &str {
        if is_bare_name(&self.src) {
            &self.src
        } else {
            strip_order(file_stem(&self.file))
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.src.trim().is_empty() {
            return Err(format!("{}: src is empty", self.file));
        }
        validate_destination(&self.file)
    }
}

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

pub fn pane_sources_of(text: &str) -> Vec<Option<String>> {
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        return Vec::new();
    };
    let Some(pane) = value.get("pane") else {
        return Vec::new();
    };
    let declared: Vec<&toml::Value> = match pane.as_array() {
        Some(array) => array.iter().collect(),
        None => vec![pane],
    };
    declared
        .into_iter()
        .map(|pane| {
            let source = pane.get("source")?.as_str()?.trim();
            (!source.is_empty()).then(|| source.to_string())
        })
        .collect()
}

pub fn is_nested_pane(file: &str) -> bool {
    !file.starts_with("mods/") && !file.starts_with("lib/") && file.contains('/')
}

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

fn strip_order(stem: &str) -> &str {
    match stem.split_once('_') {
        Some((order, rest)) if !order.is_empty() && order.chars().all(|c| c.is_ascii_digit()) => {
            rest
        }
        _ => stem,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModSpec {
    #[serde(default, rename = "mod", alias = "plugin")]
    pub mods: Vec<ModEntry>,
}

impl ModSpec {
    pub fn parse(text: &str) -> Result<Self, String> {
        let spec: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        for entry in &spec.mods {
            entry.validate()?;
        }
        // Two entries delivering to one path cannot both be honoured, and
        // silently letting the last win would make the interface depend on
        // file order in a way nothing reports.
        let mut seen = BTreeMap::new();
        for (index, entry) in spec.mods.iter().enumerate() {
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

    pub fn manages(&self, file: &str) -> bool {
        self.mods.iter().any(|entry| entry.file == file)
    }

    pub fn find(&self, key: &str) -> Option<&ModEntry> {
        self.index_of(key).map(|index| &self.mods[index])
    }

    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.mods.iter().position(|entry| {
            entry.name() == key
                || entry.file == key
                || entry.src == key
                || file_stem(&entry.file) == key
        })
    }

    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageManifest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub requires_thurbox: Option<String>,
    #[serde(rename = "pane", deserialize_with = "one_or_several_panes")]
    pub panes: Vec<PackageFile>,
    #[serde(default, rename = "module")]
    pub modules: Vec<PackageFile>,
}

fn one_or_several_panes<'de, D>(deserializer: D) -> Result<Vec<PackageFile>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    // Through `toml::Value` rather than an untagged enum: when neither shape
    // matches, untagged reports only "did not match any variant", while this names
    // the field that is actually wrong — and a manifest is hand-written, so that
    // field is most of the message.
    let value = toml::Value::deserialize(deserializer)?;
    match value {
        toml::Value::Array(_) => Vec::<PackageFile>::deserialize(value).map_err(D::Error::custom),
        one => PackageFile::deserialize(one)
            .map(|pane| vec![pane])
            .map_err(D::Error::custom),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageFile {
    pub source: String,
    pub path: String,
}

impl PackageManifest {
    pub const FILE: &str = "mod.toml";

    pub const LEGACY_FILE: &str = "plugin.toml";

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut manifest: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        for pane in &mut manifest.panes {
            pane.path = mod_destination(&pane.path);
        }
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn files(&self) -> impl Iterator<Item = &PackageFile> {
        self.panes.iter().chain(self.modules.iter())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("name is empty".to_string());
        }
        if !is_bare_name(&self.name) {
            return Err(format!(
                "name {:?} must be one plain segment, since it is also a namespace",
                self.name
            ));
        }
        if self.panes.is_empty() {
            return Err("declares no pane".to_string());
        }
        for file in self.files() {
            validate_destination(&file.path)?;
            validate_destination(&file.source)?;
        }
        let namespace = format!("lib/{}/", self.name);
        for module in &self.modules {
            if !module.path.starts_with(&namespace) {
                return Err(format!(
                    "module {} must be delivered under {namespace} — a package \
                     may not replace a shared module",
                    module.path
                ));
            }
        }
        for pane in &self.panes {
            if pane.path.starts_with("lib/") {
                return Err(format!("the pane {} belongs in mods/", pane.path));
            }
        }
        let mut seen = BTreeSet::new();
        for file in self.files() {
            if !seen.insert(file.path.as_str()) {
                return Err(format!("{} is delivered twice", file.path));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    pub src: String,
    pub file: String,
    pub resolved: String,
    pub version: String,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
}

impl LockEntry {
    pub fn digest(&self, file: &str) -> Option<&str> {
        self.files.get(file).map(String::as_str)
    }

    pub fn pin_key(&self) -> String {
        format!("{}@{}", self.src, self.version)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModLock {
    #[serde(default, rename = "mod", alias = "plugin")]
    pub mods: Vec<LockEntry>,
}

impl ModLock {
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn entry(&self, file: &str) -> Option<&LockEntry> {
        self.mods.iter().find(|entry| entry.file == file)
    }

    pub fn covering(&self, file: &str) -> Option<&LockEntry> {
        self.mods
            .iter()
            .find(|entry| entry.files.contains_key(file))
    }

    pub fn record(&mut self, entry: LockEntry) {
        match self
            .mods
            .iter()
            .position(|existing| existing.file == entry.file)
        {
            Some(index) => self.mods[index] = entry,
            None => self.mods.push(entry),
        }
        self.mods.sort_by(|a, b| a.file.cmp(&b.file));
    }

    pub fn forget(&mut self, file: &str) -> Option<LockEntry> {
        let index = self.mods.iter().position(|entry| entry.file == file)?;
        Some(self.mods.remove(index))
    }

    pub fn beyond(&self, spec: &ModSpec) -> Vec<&LockEntry> {
        self.mods
            .iter()
            .filter(|entry| !spec.manages(&entry.file))
            .collect()
    }

    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }
}

pub fn insert_entry(text: &str, entry: &ModEntry) -> Result<String, String> {
    entry.validate()?;
    let mut doc: toml_edit::DocumentMut = if text.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        text.parse()
            .map_err(|e: toml_edit::TomlError| e.to_string())?
    };

    rename_entries_key(&mut doc);
    entries_as_tables(&mut doc);
    let array = doc
        .entry(ENTRIES_KEY)
        .or_insert(toml_edit::Item::ArrayOfTables(
            toml_edit::ArrayOfTables::new(),
        ))
        .as_array_of_tables_mut()
        .ok_or_else(|| format!("{SPEC_FILE}: `{ENTRIES_KEY}` is not a list of entries"))?;

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

pub fn remove_entry(text: &str, key: &str) -> Result<(String, ModEntry), String> {
    let spec = ModSpec::parse(text)?;
    let index = spec
        .index_of(key)
        .ok_or_else(|| format!("{key} is not listed in {SPEC_FILE}"))?;
    let removed = spec.mods[index].clone();

    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    rename_entries_key(&mut doc);
    entries_as_tables(&mut doc);
    let mut orphaned_header = None;
    if let Some(array) = doc
        .get_mut(ENTRIES_KEY)
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

    #[test]
    fn a_plugins_destination_lands_under_mods() {
        assert_eq!(mod_destination("plugins/88_x.lua"), "mods/88_x.lua");
        assert_eq!(mod_destination("plugins\\88_x.lua"), "mods/88_x.lua");
        assert_eq!(mod_destination("mods/88_x.lua"), "mods/88_x.lua");
        assert_eq!(
            mod_destination("lib/x/plugins/u.lua"),
            "lib/x/plugins/u.lua"
        );
        assert_eq!(mod_destination("x/plugins/40_x.lua"), "x/plugins/40_x.lua");
    }

    #[test]
    fn a_legacy_manifest_delivers_its_panes_under_mods() {
        let manifest = PackageManifest::parse(
            "name = \"legacy\"\n[[pane]]\nsource = \"a.lua\"\npath = \"plugins/88_a.lua\"\n\
             [[module]]\nsource = \"u.lua\"\npath = \"lib/legacy/u.lua\"\n",
        )
        .expect("parse");
        assert_eq!(manifest.panes[0].path, "mods/88_a.lua");
        assert_eq!(manifest.modules[0].path, "lib/legacy/u.lua");
    }

    #[test]
    fn an_inline_spec_is_edited_like_a_list_of_tables() {
        let inline = "mod = [{ src = \"a\", file = \"mods/50_a.lua\" }]\n";
        let entry = ModEntry {
            src: "b".into(),
            file: "mods/60_b.lua".into(),
            pin: None,
        };
        let added = insert_entry(inline, &entry).expect("insert");
        assert_eq!(ModSpec::parse(&added).expect("parse").mods.len(), 2);

        let (removed, entry) = remove_entry(inline, "a").expect("remove");
        assert_eq!(entry.file, "mods/50_a.lua");
        assert!(
            ModSpec::parse(&removed).expect("parse").mods.is_empty(),
            "{removed}"
        );
    }

    #[test]
    fn a_plugin_table_reads_as_a_mod_and_an_edit_renames_it() {
        let old = "# mine\n[[plugin]]\nsrc = \"a\"\nfile = \"mods/50_a.lua\"\n";
        assert_eq!(ModSpec::parse(old).expect("parse").mods.len(), 1);
        let entry = ModEntry {
            src: "b".into(),
            file: "mods/60_b.lua".into(),
            pin: None,
        };
        let edited = insert_entry(old, &entry).expect("insert");
        assert!(edited.starts_with("# mine\n[[mod]]\n"), "{edited}");
        assert!(!edited.contains("[[plugin]]"), "{edited}");
        assert_eq!(ModSpec::parse(&edited).expect("parse").mods.len(), 2);
    }

    const SPEC: &str = r#"# what this interface is made of

# the map pane
[[mod]]
src = "atlas"
file = "mods/75_atlas.lua"
pin = "v0.3.1"

[[mod]]
src = "https://example.com/notes.lua"
file = "mods/80_notes.lua"
"#;

    #[test]
    fn a_spec_round_trips() {
        let spec = ModSpec::parse(SPEC).expect("parses");
        assert_eq!(spec.mods.len(), 2);
        assert_eq!(spec.mods[0].src, "atlas");
        assert_eq!(spec.mods[0].pin.as_deref(), Some("v0.3.1"));
        assert_eq!(spec.mods[1].pin, None);

        let again = ModSpec::parse(&spec.to_toml().expect("render")).expect("reparses");
        assert_eq!(again, spec);
    }

    #[test]
    fn an_empty_spec_is_an_interface_with_nothing_installed() {
        let spec = ModSpec::parse("").expect("an empty spec is valid");
        assert!(spec.mods.is_empty());
        assert!(!spec.manages("mods/10_sessions.lua"));
    }

    #[test]
    fn a_malformed_spec_is_reported_with_its_location() {
        let error = ModSpec::parse("[[mod]]\nsrc = \n").expect_err("should fail");
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
            "mods/../../x.lua",
            "plugins\\..\\x.lua",
            "mods/notes.txt",
            "",
        ] {
            let entry = ModEntry {
                src: "atlas".into(),
                file: bad.into(),
                pin: None,
            };
            assert!(entry.validate().is_err(), "{bad:?} should be refused");
        }
        assert!(ModEntry {
            src: "atlas".into(),
            file: "mods/75_atlas.lua".into(),
            pin: None,
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn two_entries_cannot_deliver_to_one_path() {
        let error = ModSpec::parse(
            "[[mod]]\nsrc = \"a\"\nfile = \"mods/x.lua\"\n\
             [[mod]]\nsrc = \"b\"\nfile = \"mods/x.lua\"\n",
        )
        .expect_err("should fail");
        assert!(error.contains("mods/x.lua"), "{error}");
    }

    #[test]
    fn an_entry_is_addressable_by_name_path_or_source() {
        let spec = ModSpec::parse(SPEC).expect("parses");
        for key in ["atlas", "mods/75_atlas.lua"] {
            assert_eq!(
                spec.find(key).map(|e| e.src.as_str()),
                Some("atlas"),
                "{key}"
            );
        }
        // A URL source is named by the file it delivers, since nobody retypes a URL
        // — minus the load-order prefix, which is a position and not a name.
        for key in ["notes", "80_notes", "mods/80_notes.lua"] {
            assert_eq!(
                spec.find(key).map(|e| e.file.as_str()),
                Some("mods/80_notes.lua"),
                "{key}"
            );
        }
        assert_eq!(spec.mods[1].name(), "notes");
    }

    #[test]
    fn adding_an_entry_keeps_the_comments_around_it() {
        let text = insert_entry(
            SPEC,
            &ModEntry {
                src: "top".into(),
                file: "mods/85_top.lua".into(),
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
        let spec = ModSpec::parse(&text).expect("still parses");
        assert_eq!(spec.mods.len(), 3);
        assert!(spec.manages("mods/85_top.lua"));
    }

    #[test]
    fn adding_the_same_destination_twice_updates_rather_than_duplicates() {
        // Otherwise installing twice leaves a spec that will not parse.
        let once = insert_entry(
            SPEC,
            &ModEntry {
                src: "atlas".into(),
                file: "mods/75_atlas.lua".into(),
                pin: Some("v0.4.0".into()),
            },
        )
        .expect("insert");
        let spec = ModSpec::parse(&once).expect("parses");
        assert_eq!(spec.mods.len(), 2);
        assert_eq!(
            spec.find("atlas").and_then(|e| e.pin.as_deref()),
            Some("v0.4.0")
        );
    }

    #[test]
    fn adding_to_an_empty_file_writes_a_usable_spec() {
        let text = insert_entry(
            "",
            &ModEntry {
                src: "atlas".into(),
                file: "mods/75_atlas.lua".into(),
                pin: None,
            },
        )
        .expect("insert");
        assert_eq!(ModSpec::parse(&text).expect("parses").mods.len(), 1);
    }

    #[test]
    fn removing_the_first_entry_keeps_the_file_header_but_not_its_own_comment() {
        // The spec is documented in comments, like `settings.toml`, and every byte
        // above the first entry belongs to that entry as far as `toml_edit` is
        // concerned — so without carrying the header down, one `mod remove`
        // would delete the file's documentation.
        let (text, removed) = remove_entry(SPEC, "atlas").expect("remove");
        assert_eq!(removed.file, "mods/75_atlas.lua");
        assert!(
            text.contains("# what this interface is made of"),
            "the file's header is not the entry's to take:\n{text}"
        );
        assert!(
            !text.contains("# the map pane"),
            "but a comment written about the entry goes with it:\n{text}"
        );
        let spec = ModSpec::parse(&text).expect("parses");
        assert_eq!(spec.mods.len(), 1);
        assert!(!spec.manages("mods/75_atlas.lua"));
        assert!(spec.manages("mods/80_notes.lua"));
    }

    #[test]
    fn removing_a_later_entry_leaves_everything_above_it_alone() {
        let (text, _) = remove_entry(SPEC, "80_notes").expect("remove");
        assert!(text.contains("# what this interface is made of"), "{text}");
        assert!(text.contains("# the map pane"), "{text}");
        assert_eq!(ModSpec::parse(&text).expect("parses").mods.len(), 1);
    }

    #[test]
    fn removing_the_only_entry_is_not_a_lost_header() {
        // Nothing to carry the header down to. It stays where it is, because the
        // document keeps its own trailing trivia.
        let one = "# mine\n\n[[mod]]\nsrc = \"atlas\"\nfile = \"mods/a.lua\"\n";
        let (text, _) = remove_entry(one, "atlas").expect("remove");
        assert!(text.contains("# mine"), "{text}");
        assert!(ModSpec::parse(&text).expect("parses").mods.is_empty());
    }

    #[test]
    fn removing_something_never_listed_says_so() {
        let error = remove_entry(SPEC, "nope").expect_err("should fail");
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn a_lock_records_every_file_an_entry_delivered() {
        let mut lock = ModLock::default();
        let mut files = BTreeMap::new();
        files.insert("mods/75_atlas.lua".to_string(), "aaaa".to_string());
        files.insert("lib/atlas/util.lua".to_string(), "bbbb".to_string());
        lock.record(LockEntry {
            src: "atlas".into(),
            file: "mods/75_atlas.lua".into(),
            resolved: "https://example.com/atlas".into(),
            version: "v0.3.1".into(),
            files,
            removed: Vec::new(),
        });

        let again = ModLock::parse(&lock.to_toml().expect("render")).expect("reparses");
        assert_eq!(again, lock);
        let entry = again.entry("mods/75_atlas.lua").expect("entry");
        assert_eq!(entry.digest("mods/75_atlas.lua"), Some("aaaa"));
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
        let mut lock = ModLock::default();
        for version in ["v1", "v2"] {
            lock.record(LockEntry {
                src: "atlas".into(),
                file: "mods/75_atlas.lua".into(),
                resolved: "https://example.com/atlas".into(),
                version: version.into(),
                files: BTreeMap::new(),
                removed: Vec::new(),
            });
        }
        assert_eq!(lock.mods.len(), 1);
        assert_eq!(lock.mods[0].version, "v2");
    }

    #[test]
    fn a_record_the_spec_no_longer_lists_is_what_convergence_takes_back() {
        let spec = ModSpec::parse(SPEC).expect("parses");
        let mut lock = ModLock::default();
        for file in ["mods/75_atlas.lua", "mods/99_gone.lua"] {
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
        assert_eq!(beyond, ["mods/99_gone.lua"]);

        assert!(lock.forget("mods/99_gone.lua").is_some());
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

    #[test]
    fn both_spellings_of_a_pane_parse_to_the_same_list() {
        let singular = PackageManifest::parse(
            "name = \"atlas\"\npane = { source = \"a.lua\", path = \"mods/75_atlas.lua\" }\n",
        )
        .expect("singular");
        let listed = PackageManifest::parse(
            "name = \"atlas\"\n[[pane]]\nsource = \"a.lua\"\npath = \"mods/75_atlas.lua\"\n",
        )
        .expect("listed");
        assert_eq!(singular, listed);
        assert_eq!(singular.panes.len(), 1);
    }

    #[test]
    fn a_malformed_pane_names_the_field_that_is_wrong() {
        let error = PackageManifest::parse("name = \"atlas\"\n[[pane]]\nsource = \"a.lua\"\n")
            .expect_err("no path");
        assert!(error.contains("path"), "{error}");
    }

    #[test]
    fn a_manifest_must_declare_a_pane() {
        let missing = PackageManifest::parse("name = \"atlas\"\n").expect_err("no pane");
        assert!(missing.contains("pane"), "{missing}");
        let empty = PackageManifest::parse("name = \"atlas\"\npane = []\n").expect_err("empty");
        assert!(empty.contains("declares no pane"), "{empty}");
    }

    #[test]
    fn two_files_of_one_package_may_not_share_a_destination() {
        let error = PackageManifest::parse(
            "name = \"atlas\"\n\
             [[pane]]\nsource = \"a.lua\"\npath = \"mods/75_atlas.lua\"\n\
             [[pane]]\nsource = \"b.lua\"\npath = \"mods/75_atlas.lua\"\n",
        )
        .expect_err("duplicate");
        assert!(error.contains("delivered twice"), "{error}");
    }

    #[test]
    fn every_pane_is_held_to_the_mods_rule() {
        let error = PackageManifest::parse(
            "name = \"atlas\"\n\
             [[pane]]\nsource = \"a.lua\"\npath = \"mods/75_atlas.lua\"\n\
             [[pane]]\nsource = \"b.lua\"\npath = \"lib/atlas/b.lua\"\n",
        )
        .expect_err("second pane in lib/");
        assert!(error.contains("belongs in mods/"), "{error}");
    }

    #[test]
    fn pane_sources_are_read_in_either_spelling() {
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            pane_sources_of("pane = { source = \"mods/40_a.lua\" }\n"),
            vec![some("mods/40_a.lua")]
        );
        assert_eq!(
            pane_sources_of(
                "[[pane]]\nsource = \"mods/40_a.lua\"\n[[pane]]\nsource = \"mods/41_b.lua\"\n"
            ),
            vec![some("mods/40_a.lua"), some("mods/41_b.lua")]
        );
        // A declaration with no usable source still counts.
        assert_eq!(
            pane_sources_of(
                "[[pane]]\nsource = \"mods/40_a.lua\"\n[[pane]]\nsource = \"\"\n[[pane]]\n"
            ),
            vec![some("mods/40_a.lua"), None, None]
        );
        assert!(pane_sources_of("name = \"x\"\n").is_empty());
        assert!(pane_sources_of("not toml [").is_empty());
    }
}
