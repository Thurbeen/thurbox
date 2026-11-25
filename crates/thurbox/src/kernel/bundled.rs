use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

pub const BUNDLED: &[(&str, &str)] = &[
    ("README.md", include_str!("../../../../ui/README.md")),
    ("AGENTS.md", include_str!("../../../../ui/AGENTS.md")),
    ("CLAUDE.md", include_str!("../../../../ui/CLAUDE.md")),
    ("GEMINI.md", include_str!("../../../../ui/GEMINI.md")),
    ("layout.lua", include_str!("../../../../ui/layout.lua")),
    (
        "lib/thurbox.d.lua",
        include_str!("../../../../ui/lib/thurbox.d.lua"),
    ),
    (
        "lib/theme.lua",
        include_str!("../../../../ui/lib/theme.lua"),
    ),
    (
        "lib/widgets.lua",
        include_str!("../../../../ui/lib/widgets.lua"),
    ),
    ("lib/ui.lua", include_str!("../../../../ui/lib/ui.lua")),
    ("lib/tree.lua", include_str!("../../../../ui/lib/tree.lua")),
    (
        "lib/panels.lua",
        include_str!("../../../../ui/lib/panels.lua"),
    ),
    (
        "lib/hover.lua",
        include_str!("../../../../ui/lib/hover.lua"),
    ),
    (
        "lib/fuzzy.lua",
        include_str!("../../../../ui/lib/fuzzy.lua"),
    ),
    (
        "lib/chrome.lua",
        include_str!("../../../../ui/lib/chrome.lua"),
    ),
    (
        "lib/modal.lua",
        include_str!("../../../../ui/lib/modal.lua"),
    ),
    (
        "lib/order.lua",
        include_str!("../../../../ui/lib/order.lua"),
    ),
    (
        "lib/scroll.lua",
        include_str!("../../../../ui/lib/scroll.lua"),
    ),
    (
        "lib/pathpicker.lua",
        include_str!("../../../../ui/lib/pathpicker.lua"),
    ),
    (
        "lib/repo_picker.lua",
        include_str!("../../../../ui/lib/repo_picker.lua"),
    ),
    (
        "lib/session_model.lua",
        include_str!("../../../../ui/lib/session_model.lua"),
    ),
    (
        "lib/settings.lua",
        include_str!("../../../../ui/lib/settings.lua"),
    ),
    (
        "lib/textinput.lua",
        include_str!("../../../../ui/lib/textinput.lua"),
    ),
    (
        "mods/10_sessions.lua",
        include_str!("../../../../ui/mods/10_sessions.lua"),
    ),
    (
        "mods/20_agent.lua",
        include_str!("../../../../ui/mods/20_agent.lua"),
    ),
    (
        "mods/60_confirm.lua",
        include_str!("../../../../ui/mods/60_confirm.lua"),
    ),
    (
        "mods/62_rename.lua",
        include_str!("../../../../ui/mods/62_rename.lua"),
    ),
    (
        "mods/64_menu.lua",
        include_str!("../../../../ui/mods/64_menu.lua"),
    ),
    (
        "mods/65_search.lua",
        include_str!("../../../../ui/mods/65_search.lua"),
    ),
    (
        "mods/70_new_session.lua",
        include_str!("../../../../ui/mods/70_new_session.lua"),
    ),
];

pub const MANIFEST: &str = ".bundled.json";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum Record {
    Written(String),
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

    fn delivered(&self) -> Delivered<'_> {
        match self {
            Record::Written(digest) => Delivered::Digest(digest.as_str()),
            Record::Removed { .. } => Delivered::Tombstoned,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Bundled,
    Edited,
    User,
    Removed,
    Installed { src: String },
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

    pub fn installed_from(&self) -> Option<&str> {
        match self {
            Source::Installed { src } | Source::InstalledEdited { src } => Some(src.as_str()),
            _ => None,
        }
    }

    pub fn is_theirs(&self) -> bool {
        matches!(self, Source::User)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub written: Vec<String>,
    pub updated: Vec<String>,
    pub preserved: Vec<String>,
    pub removed: Vec<String>,
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

pub fn digest(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

pub fn materialize(dir: &Path) -> Report {
    let mut report = Report::default();
    let mut manifest = read_manifest(dir);

    for (relative, contents) in BUNDLED {
        deliver(dir, relative, contents, &mut manifest, &mut report);
    }

    retire(dir, &mut manifest, &mut report);

    if let Err(e) = write_manifest(dir, &manifest) {
        report.errors.push(e);
    }
    report
}

fn deliver(
    dir: &Path,
    relative: &str,
    contents: &str,
    manifest: &mut BTreeMap<String, Record>,
    report: &mut Report,
) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            report.errors.push(format!("{}: {e}", parent.display()));
            return;
        }
    }

    let existing = std::fs::read_to_string(&path).ok();
    let action = decide(
        existing.as_deref(),
        manifest
            .get(relative)
            .map_or(Delivered::Never, Record::delivered),
        contents,
    );

    match action {
        Action::Leave => {}
        Action::Settle => {
            manifest.insert(relative.to_string(), Record::Written(digest(contents)));
        }
        Action::Tombstone => {
            manifest.insert(relative.to_string(), Record::tombstone());
            report.removed.push(relative.to_string());
        }
        Action::Preserve => report.preserved.push(relative.to_string()),
        Action::Write | Action::Update => match crate::paths::write_atomically(&path, contents) {
            Ok(()) => {
                manifest.insert(relative.to_string(), Record::Written(digest(contents)));
                if action == Action::Write {
                    report.written.push(relative.to_string());
                } else {
                    report.updated.push(relative.to_string());
                }
            }
            Err(e) => report.errors.push(format!("{}: {e}", path.display())),
        },
    }
}

const WITHDRAWN: &[&str] = &["mods/80_restore.lua"];

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
            Ok(_) if WITHDRAWN.contains(&relative.as_str()) => {
                // Never over an existing backup: `rename` replaces its destination.
                let aside = std::iter::once(format!("{relative}.bak"))
                    .chain((2..).map(|n| format!("{relative}.bak.{n}")))
                    .find(|name| !dir.join(name).exists())
                    .expect("an unbounded series has a free name");
                match std::fs::rename(&path, dir.join(&aside)) {
                    Ok(()) => {
                        manifest.remove(&relative);
                        report
                            .retired
                            .push(format!("{relative} (your edit is in {aside})"));
                    }
                    Err(e) => report.errors.push(format!("{}: {e}", path.display())),
                }
            }
            Ok(_) => {
                manifest.remove(&relative);
                report.preserved.push(relative);
            }
            Err(_) => {
                manifest.remove(&relative);
            }
        }
    }
}

const LIB_DEPTH: usize = 3;

// `Path::join` writes `\` on Windows: identities always use `/`.
pub fn relative_to(dir: &Path, absolute: &str) -> Option<String> {
    let root = dir.to_string_lossy();
    let rest = absolute.strip_prefix(root.as_ref())?;
    let relative: String = rest
        .trim_start_matches(['/', '\\'])
        .replace('\\', "/")
        .to_string();
    (!relative.is_empty()).then_some(relative)
}

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

fn source_of(
    relative: &str,
    current: &str,
    bundled: &BTreeMap<&str, &str>,
    lock: &crate::session::mod_spec::ModLock,
) -> Source {
    match bundled.get(relative) {
        Some(shipped) if *shipped == current => Source::Bundled,
        Some(_) => Source::Edited,
        None => match lock.covering(relative) {
            Some(entry) => {
                let src = entry.src.clone();
                if entry.digest(relative) == Some(digest(current).as_str()) {
                    Source::Installed { src }
                } else {
                    Source::InstalledEdited { src }
                }
            }
            None => Source::User,
        },
    }
}

fn interface_files(dir: &Path, spec: &crate::session::mod_spec::ModSpec) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    for (relative, _) in BUNDLED {
        if relative.contains('/') {
            continue;
        }
        let path = dir.join(relative);
        if path.is_file() {
            files.push(((*relative).to_string(), path));
        }
    }
    for name in lua_files(&dir.join("mods"), 1) {
        files.push((format!("mods/{name}"), dir.join("mods").join(&name)));
    }
    for name in lua_files(&dir.join("lib"), LIB_DEPTH) {
        files.push((format!("lib/{name}"), dir.join("lib").join(&name)));
    }

    for entry in &spec.mods {
        if !crate::session::mod_spec::is_nested_pane(&entry.file) {
            continue;
        }
        let path = dir.join(&entry.file);
        if path.is_file() {
            files.push((entry.file.clone(), path));
        }
    }
    files
}

pub fn sources(dir: &Path) -> BTreeMap<String, Source> {
    let bundled: BTreeMap<&str, &str> = BUNDLED.iter().copied().collect();
    let lock = super::packages::read_lock(dir).unwrap_or_default();
    let spec = super::packages::read_spec(dir).unwrap_or_default();
    let mut out = BTreeMap::new();

    for (relative, path) in interface_files(dir, &spec) {
        let Ok(current) = std::fs::read_to_string(&path) else {
            continue;
        };
        let source = source_of(&relative, &current, &bundled, &lock);
        out.insert(relative, source);
    }

    let spec = crate::session::mod_spec::SPEC_FILE;
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

pub fn restore(dir: &Path, relative: &str) -> Result<Option<PathBuf>, String> {
    let path = checked(dir, relative)?;
    let contents = BUNDLED
        .iter()
        .find(|(name, _)| *name == relative)
        .map(|(_, contents)| *contents)
        .ok_or_else(|| format!("{relative} is not part of the bundled interface"))?;

    let mut manifest = read_manifest(dir);
    let backup = match std::fs::read_to_string(&path) {
        Ok(current) if relative == LAYOUT && current != contents => {
            let untouched = manifest
                .get(relative)
                .and_then(Record::digest)
                .is_some_and(|recorded| recorded == digest(&current));
            if untouched {
                None
            } else {
                let to = free_backup_path(dir);
                std::fs::rename(&path, &to)
                    .map_err(|e| format!("could not back up {}: {e}", path.display()))?;
                Some(to)
            }
        }
        _ => None,
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    if let Err(e) = crate::paths::write_atomically(&path, contents) {
        if let Some(backup) = &backup {
            let _ = std::fs::rename(backup, &path);
        }
        return Err(format!("{}: {e}", path.display()));
    }

    manifest.insert(relative.to_string(), Record::Written(digest(contents)));
    write_manifest(dir, &manifest)?;
    Ok(backup)
}

const LAYOUT: &str = "layout.lua";

fn free_backup_path(dir: &Path) -> PathBuf {
    let first = dir.join(format!("{LAYOUT}.bak"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{LAYOUT}.bak.{n}")))
        .find(|candidate| !candidate.exists())
        .expect("an unused backup name")
}

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
    Settle,
    Tombstone,
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivered<'a> {
    Never,
    Digest(&'a str),
    Tombstoned,
}

pub(crate) fn decide(existing: Option<&str>, delivered: Delivered<'_>, payload: &str) -> Action {
    match (existing, delivered) {
        (None, Delivered::Never) => Action::Write,
        (None, Delivered::Tombstoned) => Action::Leave,
        (None, Delivered::Digest(_)) => Action::Tombstone,
        (Some(current), _) if current == payload => Action::Settle,
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
    crate::paths::write_atomically(&dir.join(MANIFEST), text)
        .map_err(|e| format!("{}: {e}", dir.join(MANIFEST).display()))
}

pub fn rename_records(
    dir: &Path,
    rename: impl Fn(&str) -> Option<String>,
    forget: impl Fn(&str) -> bool,
) -> Result<(), String> {
    let mut manifest = read_manifest(dir);
    let copies: Vec<(String, Record)> = manifest
        .iter()
        .filter_map(|(old, record)| rename(old).map(|new| (new, record.clone())))
        .filter(|(new, _)| !manifest.contains_key(new))
        .collect();
    let forgotten: Vec<String> = manifest.keys().filter(|key| forget(key)).cloned().collect();
    if copies.is_empty() && forgotten.is_empty() {
        return Ok(());
    }
    manifest.extend(copies);
    for key in forgotten {
        manifest.remove(&key);
    }
    write_manifest(dir, &manifest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chosen {
    Override,
    Checkout,
    UserCopy,
    Fallback,
}

impl Chosen {
    pub fn as_str(self) -> &'static str {
        match self {
            Chosen::Override => "override",
            Chosen::Checkout => "checkout",
            Chosen::UserCopy => "user-copy",
            Chosen::Fallback => "fallback",
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Chosen::Override => "THURBOX_UI_DIR names it",
            Chosen::Checkout => "THURBOX_UI_DIR pointing at a checkout's ./ui",
            Chosen::UserCopy => "your own copy of the interface",
            Chosen::Fallback => "the embedded interface — your copy could not be written",
        }
    }
}

pub fn resolve(materialize_user_copy: bool) -> Result<(PathBuf, Chosen, Report), String> {
    if let Some(dir) = std::env::var_os("THURBOX_UI_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            let chosen = if dir.file_name().is_some_and(|name| name == "ui")
                && dir.join("mods").is_dir()
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
    if let Some(dir) = user_ui_dir() {
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

fn absolute(dir: PathBuf) -> PathBuf {
    if dir.is_absolute() {
        return dir;
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(dir),
        Err(_) => dir,
    }
}

pub fn user_ui_dir() -> Option<PathBuf> {
    crate::paths::config_file().and_then(|config| config.parent().map(|dir| dir.join("ui")))
}

pub fn fallback_dir() -> Result<PathBuf, String> {
    // The system temp dir is world-writable: create exclusively, never adopt a directory found there.
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

    mod decide {
        use super::*;

        const OLD: &str = "-- v1\n";
        const NEW: &str = "-- v2\n";
        const MINE: &str = "-- mine\n";

        #[test]
        fn nothing_there_and_nothing_recorded_is_written() {
            assert_eq!(decide(None, Delivered::Never, NEW), Action::Write);
        }

        #[test]
        fn nothing_there_but_we_wrote_it_once_is_tombstoned() {
            let recorded = digest(OLD);
            assert_eq!(
                decide(None, Delivered::Digest(&recorded), NEW),
                Action::Tombstone
            );
        }

        #[test]
        fn nothing_there_and_already_tombstoned_is_left() {
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
            let recorded = digest(OLD);
            assert_eq!(
                decide(Some(MINE), Delivered::Digest(&recorded), NEW),
                Action::Preserve
            );
        }

        #[test]
        fn a_file_we_never_wrote_is_preserved() {
            assert_eq!(decide(Some(MINE), Delivered::Never, NEW), Action::Preserve);
        }

        #[test]
        fn a_tombstone_does_not_authorise_overwriting_what_replaced_it() {
            assert_eq!(
                decide(Some(MINE), Delivered::Tombstoned, NEW),
                Action::Preserve
            );
        }

        #[test]
        fn putting_back_an_identical_copy_readopts_it() {
            assert_eq!(
                decide(Some(NEW), Delivered::Tombstoned, NEW),
                Action::Settle
            );
        }
    }

    #[test]
    fn a_stored_path_becomes_the_identity_the_loader_compares() {
        let dir = Path::new("/home/me/.config/thurbox/ui");
        assert_eq!(
            relative_to(dir, "/home/me/.config/thurbox/ui/mods/90_notes.lua").as_deref(),
            Some("mods/90_notes.lua")
        );

        // A Windows root, with the separators `Path::join` writes there.
        let windows = Path::new(r"C:\Users\me\AppData\Roaming\thurbox\ui");
        assert_eq!(
            relative_to(
                windows,
                r"C:\Users\me\AppData\Roaming\thurbox\ui\mods\90_notes.lua"
            )
            .as_deref(),
            Some("mods/90_notes.lua"),
            "`build` keys on `mods/<name>`, so a backslash here matches nothing"
        );

        assert_eq!(relative_to(dir, "/elsewhere/mods/x.lua"), None);
        assert_eq!(relative_to(dir, "/home/me/.config/thurbox/ui"), None);
        assert_eq!(relative_to(dir, "/home/me/.config/thurbox/ui/"), None);
    }

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
            agents.contains("thurbox-cli mod install"),
            "the command an agent needs when told to install a mod"
        );
        assert!(
            agents.contains("npm") && agents.contains("cargo"),
            "and the package managers it must NOT reach for, named so the \
             instruction is unmissable"
        );
        assert!(
            agents.contains("thurbox-cli mod check"),
            "and the gate for every edit"
        );

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
        let names: Vec<&str> = BUNDLED.iter().map(|(name, _)| *name).collect();
        for required in [
            "layout.lua",
            "lib/theme.lua",
            "lib/widgets.lua",
            "lib/ui.lua",
            "mods/10_sessions.lua",
            "mods/20_agent.lua",
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
        assert!(dir.path().join("mods/10_sessions.lua").exists());
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        let report = materialize(dir.path());
        assert!(report.is_empty(), "{report:?}");
    }

    #[test]
    fn a_file_new_in_this_release_reaches_an_existing_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());

        let newcomer = "AGENTS.md";
        std::fs::remove_file(dir.path().join(newcomer)).expect("remove");
        let mut manifest = read_manifest(dir.path());
        manifest.remove(newcomer);
        write_manifest(dir.path(), &manifest).expect("write manifest");

        let edited = dir.path().join("mods/10_sessions.lua");
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
            .contains(&"mods/10_sessions.lua".to_string()));
    }

    #[test]
    fn a_user_edit_is_preserved_and_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());

        let edited = dir.path().join("lib/theme.lua");
        std::fs::write(&edited, "-- mine\nreturn {}").expect("edit");

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
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        let path = dir.path().join("mods/10_sessions.lua");
        std::fs::remove_file(&path).expect("remove");

        let report = materialize(dir.path());
        assert_eq!(report.removed, vec!["mods/10_sessions.lua"]);
        assert!(!path.exists());

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

        let mut manifest = read_manifest(dir.path());
        assert!(manifest["lib/theme.lua"].is_removed());
        manifest.insert("mods/20_agent.lua".into(), Record::Written("stale".into()));
        write_manifest(dir.path(), &manifest).expect("manifest");

        let report = materialize(dir.path());
        assert!(!dir.path().join("lib/theme.lua").exists());
        assert!(report.removed.is_empty(), "{report:?}");
    }

    #[test]
    fn discarding_the_directory_delivers_everything_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        std::fs::remove_file(dir.path().join("mods/10_sessions.lua")).expect("remove");
        materialize(dir.path());

        std::fs::remove_dir_all(dir.path()).expect("discard");
        std::fs::create_dir_all(dir.path()).expect("recreate");

        let report = materialize(dir.path());
        assert_eq!(report.written.len(), BUNDLED.len());
        assert!(dir.path().join("mods/10_sessions.lua").exists());
    }

    #[test]
    fn a_user_written_file_is_never_touched() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        let mine = dir.path().join("mods/50_mine.lua");
        std::fs::write(&mine, "return { name = \"mine\" }").expect("write");

        let report = materialize(dir.path());
        assert!(report.is_empty(), "{report:?}");
        assert_eq!(
            std::fs::read_to_string(&mine).expect("read"),
            "return { name = \"mine\" }"
        );
        assert!(!read_manifest(dir.path()).contains_key("mods/50_mine.lua"));
    }

    #[test]
    fn a_file_that_stops_shipping_is_retired_unless_it_was_edited() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());

        let untouched = dir.path().join("mods/40_gone.lua");
        let claimed = dir.path().join("mods/41_kept.lua");
        std::fs::write(&untouched, "return {}").expect("write");
        std::fs::write(&claimed, "-- mine now\nreturn {}").expect("write");
        let mut manifest = read_manifest(dir.path());
        manifest.insert(
            "mods/40_gone.lua".into(),
            Record::Written(digest("return {}")),
        );
        manifest.insert(
            "mods/41_kept.lua".into(),
            Record::Written(digest("return {}")),
        );
        write_manifest(dir.path(), &manifest).expect("manifest");

        let report = materialize(dir.path());
        assert_eq!(report.retired, vec!["mods/40_gone.lua"]);
        assert_eq!(report.preserved, vec!["mods/41_kept.lua"]);
        assert!(!untouched.exists());
        assert!(claimed.exists());
        let manifest = read_manifest(dir.path());
        assert!(!manifest.contains_key("mods/40_gone.lua"));
        assert!(!manifest.contains_key("mods/41_kept.lua"));
    }

    #[test]
    fn an_edited_withdrawn_pane_is_kept_aside_rather_than_loaded() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());

        let pane = dir.path().join("mods/80_restore.lua");
        std::fs::write(&pane, "-- mine\nreturn {}").expect("write");
        let mut manifest = read_manifest(dir.path());
        manifest.insert(
            "mods/80_restore.lua".into(),
            Record::Written(digest("return {}")),
        );
        write_manifest(dir.path(), &manifest).expect("manifest");

        let report = materialize(dir.path());
        assert_eq!(
            report.retired,
            vec!["mods/80_restore.lua (your edit is in mods/80_restore.lua.bak)"]
        );
        assert!(report.preserved.is_empty(), "{:?}", report.preserved);
        assert!(!pane.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("mods/80_restore.lua.bak")).expect("read"),
            "-- mine\nreturn {}"
        );
        assert!(!read_manifest(dir.path()).contains_key("mods/80_restore.lua"));

        std::fs::write(&pane, "-- again\nreturn {}").expect("write");
        let mut manifest = read_manifest(dir.path());
        manifest.insert(
            "mods/80_restore.lua".into(),
            Record::Written(digest("return {}")),
        );
        write_manifest(dir.path(), &manifest).expect("manifest");
        materialize(dir.path());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("mods/80_restore.lua.bak")).expect("read"),
            "-- mine\nreturn {}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("mods/80_restore.lua.bak.2")).expect("read"),
            "-- again\nreturn {}"
        );
    }

    #[test]
    fn a_legacy_manifest_still_protects_an_edit() {
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

        let removed = "mods/10_sessions.lua";
        remove(dir.path(), removed).expect("remove");
        assert!(!dir.path().join(removed).exists());
        restore(dir.path(), removed).expect("restore");
        assert!(dir.path().join(removed).exists());
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
        let mine = "mods/50_mine.lua";
        std::fs::write(dir.path().join(mine), "return {}").expect("write");

        remove(dir.path(), mine).expect("remove");
        assert!(!read_manifest(dir.path()).contains_key(mine));
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
            "mods/../../escape.lua",
        ] {
            assert!(checked(dir.path(), refused).is_err(), "{refused} allowed");
        }
        assert!(checked(dir.path(), "mods/10_sessions.lua").is_ok());
    }

    #[test]
    fn sources_name_where_each_file_came_from() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        std::fs::write(dir.path().join("lib/theme.lua"), "-- mine").expect("edit");
        std::fs::write(dir.path().join("mods/50_mine.lua"), "return {}").expect("write");
        remove(dir.path(), "mods/20_agent.lua").expect("remove");

        let sources = sources(dir.path());
        assert_eq!(sources["layout.lua"], Source::Bundled);
        assert_eq!(sources["lib/theme.lua"], Source::Edited);
        assert_eq!(sources["mods/50_mine.lua"], Source::User);
        assert_eq!(sources["mods/20_agent.lua"], Source::Removed);
    }

    #[test]
    fn an_installed_file_names_the_source_it_came_from() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());

        let entry = crate::session::ModEntry {
            src: "atlas".into(),
            file: "mods/75_atlas.lua".into(),
            pin: Some("v0.3.1".into()),
        };
        let mut lock = crate::session::ModLock::default();
        super::super::packages::deliver(
            dir.path(),
            &entry,
            "https://example.com/atlas",
            "v0.3.1",
            &[
                super::super::packages::Payload {
                    file: "mods/75_atlas.lua".into(),
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
            fresh["mods/75_atlas.lua"],
            Source::Installed {
                src: "atlas".into()
            }
        );
        assert!(
            !fresh["mods/75_atlas.lua"].is_theirs(),
            "an installed pane is not the user's own file"
        );
        assert_eq!(fresh["mods/75_atlas.lua"].installed_from(), Some("atlas"));
        assert_eq!(fresh["lib/atlas/util.lua"].installed_from(), Some("atlas"));

        std::fs::write(dir.path().join("mods/75_atlas.lua"), "-- mine\n").expect("edit");
        let after = sources(dir.path());
        assert_eq!(
            after["mods/75_atlas.lua"],
            Source::InstalledEdited {
                src: "atlas".into()
            }
        );
        assert_eq!(after["mods/75_atlas.lua"].installed_from(), Some("atlas"));
    }

    #[test]
    fn the_spec_is_inventoried_and_the_lock_is_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        super::super::packages::add_to_spec(
            dir.path(),
            &crate::session::ModEntry {
                src: "atlas".into(),
                file: "mods/75_atlas.lua".into(),
                pin: None,
            },
        )
        .expect("add");
        std::fs::write(super::super::packages::lock_path(dir.path()), "").expect("lock");

        let sources = sources(dir.path());
        assert!(sources.contains_key(crate::session::mod_spec::SPEC_FILE));
        assert!(!sources.contains_key(crate::session::mod_spec::LOCK_FILE));
        assert!(!sources.contains_key(MANIFEST));
    }

    #[test]
    fn a_broken_lock_does_not_break_the_inventory() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize(dir.path());
        std::fs::write(dir.path().join("mods/50_mine.lua"), "return {}").expect("write");
        std::fs::write(
            super::super::packages::lock_path(dir.path()),
            "[[plugin]]\nsrc =\n",
        )
        .expect("write");

        let sources = sources(dir.path());
        assert_eq!(sources["layout.lua"], Source::Bundled);
        assert_eq!(sources["mods/50_mine.lua"], Source::User);
    }

    struct RemoveOnDrop(PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_fallback_directory_holds_a_loadable_interface() {
        let dir = RemoveOnDrop(fallback_dir().expect("fallback"));
        assert!(dir.0.join("mods").is_dir());
        assert!(dir.0.join("layout.lua").exists());
    }

    #[test]
    fn the_digest_distinguishes_contents() {
        assert_eq!(digest("a"), digest("a"));
        assert_ne!(digest("a"), digest("b"));
    }

    #[test]
    fn the_fallback_refuses_a_directory_it_did_not_create() {
        let planted =
            std::env::temp_dir().join(format!("thurbox-ui-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&planted);
        std::fs::create_dir_all(planted.join("mods")).expect("plant");
        std::fs::write(planted.join("mods/99_evil.lua"), "return {}").expect("plant");
        let planted = RemoveOnDrop(planted);

        let dir = RemoveOnDrop(fallback_dir().expect("fallback"));
        assert_ne!(dir.0, planted.0, "adopted a pre-existing directory");
        assert!(
            !dir.0.join("mods/99_evil.lua").exists(),
            "the planted file came along: {}",
            dir.0.display()
        );
        assert!(dir.0.join("layout.lua").exists(), "still a whole interface");
    }

    #[test]
    fn the_resolved_directory_is_absolute_and_still_the_one_that_was_asked_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("mods")).expect("mkdir");
        std::env::set_var("THURBOX_UI_DIR", dir.path());
        let (resolved, chosen, _) = resolve(false).expect("resolve");
        std::env::remove_var("THURBOX_UI_DIR");

        assert_eq!(chosen, Chosen::Override);
        assert!(
            resolved.is_absolute(),
            "a relative interface directory is not an identity: {}",
            resolved.display()
        );
        // Canonicalising would return `\\?\D:\…` on Windows and `/private/var` for `/var` on macOS.
        assert_eq!(
            resolved,
            dir.path(),
            "the resolved directory must be the one that was asked for"
        );
    }
}
