//! Installed integrations: the hooks Fleet writes into an agent's own config
//! directory, for a harness that takes none at launch. `fleet integration`
//! drives them; the adapter does the I/O (`fleet_runtime::integration`).
//!
//! Fleet touches only what it owns, as its profile's `[install]` table lists
//! it ([`crate::harness::Install`]):
//!
//! - an **own file**, whole, in a directory the agent merges;
//! - a **block** appended to a file the user shares, between a first and a
//!   last comment line naming Fleet.
//!
//! A ledger in Fleet's data directory ([`LEDGER`]) records every file
//! written, every block added and every directory created, with what was
//! written. Uninstalling removes exactly those. Fleet never creates the
//! agent's config directory itself.
//!
//! The format and the states are in docs/fleet/integration.md.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::harness::{Authority, Install, OwnedBlock, Profile};

/// The ledger's file name, under Fleet's data directory.
pub const LEDGER: &str = "integrations.json";

/// The ledger's format version.
pub const LEDGER_VERSION: u32 = 1;

/// What is installed for one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// The harness takes its hooks at launch.
    NotNeeded,
    /// Fleet knows no hooks for the harness.
    Unsupported,
    /// The integration is not checked on this machine's platform.
    UnsupportedPlatform,
    /// The agent's config directory is not there.
    AgentMissing,
    NotInstalled,
    Current,
    /// An older version, a missing file or block, or another directory.
    Outdated,
    /// A file or block Fleet wrote holds something else now.
    Modified,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::NotNeeded => "not-needed",
            State::Unsupported => "unsupported",
            State::UnsupportedPlatform => "unsupported-platform",
            State::AgentMissing => "agent-missing",
            State::NotInstalled => "not-installed",
            State::Current => "current",
            State::Outdated => "outdated",
            State::Modified => "modified",
        }
    }
}

/// Every integration Fleet installed on this machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ledger {
    #[serde(default = "ledger_version")]
    pub version: u32,
    #[serde(default)]
    pub integrations: BTreeMap<String, Entry>,
}

fn ledger_version() -> u32 {
    LEDGER_VERSION
}

/// What Fleet wrote for one agent. Paths are relative to `dir`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub version: u32,
    /// The agent's config directory, absolute.
    pub dir: String,
    #[serde(default)]
    pub files: Vec<WrittenFile>,
    #[serde(default)]
    pub blocks: Vec<WrittenBlock>,
    /// The directories Fleet created, outermost first.
    #[serde(default)]
    pub dirs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrittenFile {
    pub path: String,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrittenBlock {
    pub path: String,
    /// The hash of the block, its comment lines included.
    pub hash: String,
    pub comment: String,
    /// What Fleet put between the user's last byte and the block.
    pub prefix: String,
    /// Fleet created the file: once the block is gone and it is empty, it
    /// goes too.
    pub created_file: bool,
}

impl Entry {
    /// Every path the entry wrote, relative to its directory.
    pub fn paths(&self) -> BTreeSet<&str> {
        self.files
            .iter()
            .map(|f| f.path.as_str())
            .chain(self.blocks.iter().map(|b| b.path.as_str()))
            .collect()
    }

    fn hash_of(&self, path: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.hash.as_str())
            .or_else(|| {
                self.blocks
                    .iter()
                    .find(|b| b.path == path)
                    .map(|b| b.hash.as_str())
            })
    }
}

/// What the adapter found on disk, in the entry's directory when there is
/// one, else in the directory the agent reads now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    pub dir_exists: bool,
    /// For each path the entry wrote, the hash of what is there now: the
    /// whole file, or Fleet's block in it. `None`: absent.
    pub found: BTreeMap<String, Option<String>>,
}

/// The platform `fleet integration` runs on, given by the adapter so the
/// rule is tested on every platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Unix,
    Windows,
}

/// Whether `install` may be written on `platform`: on Windows only when the
/// profile says its hooks run there.
pub fn supported(install: &Install, platform: Platform) -> Result<(), String> {
    if platform == Platform::Windows && !install.windows {
        return Err(
            "not supported on Windows yet: its hooks are POSIX shell commands no Windows run has checked"
                .into(),
        );
    }
    Ok(())
}

/// A profile's `[install]` table, checked when the profile is parsed.
pub fn check(install: &Install) -> Result<(), String> {
    if install.version == 0 {
        return Err("install.version starts at 1".into());
    }
    if install.dir.trim().is_empty() {
        return Err("install.dir is empty".into());
    }
    if install.files.is_empty() && install.blocks.is_empty() {
        return Err("install lists no file and no block".into());
    }
    let mut seen = BTreeSet::new();
    for path in install
        .files
        .iter()
        .map(|f| &f.path)
        .chain(install.blocks.iter().map(|b| &b.path))
    {
        let parts: Vec<&str> = path.split(['/', '\\']).collect();
        if path.is_empty()
            || path.starts_with(['/', '\\'])
            || path.contains(':')
            || parts
                .iter()
                .any(|p| p.is_empty() || *p == "." || *p == "..")
        {
            return Err(format!(
                "install path {path:?}: name a file inside the config directory"
            ));
        }
        if !seen.insert(path.as_str()) {
            return Err(format!("install path {path:?} is listed twice"));
        }
    }
    if let Some(b) = install
        .blocks
        .iter()
        .find(|b| b.comment.trim().is_empty() || b.comment.contains('\n'))
    {
        return Err(format!(
            "install block {:?}: a comment is one token",
            b.path
        ));
    }
    Ok(())
}

/// The agent's config directory: the first of its variables `env` gives,
/// else its default under `home`. `None` when neither can name it.
pub fn config_dir(
    install: &Install,
    env: impl Fn(&str) -> Option<String>,
    home: Option<&str>,
) -> Option<String> {
    if let Some(dir) = install
        .dir_env
        .iter()
        .find_map(|v| env(v).filter(|d| !d.is_empty()))
    {
        return Some(dir);
    }
    match install.dir.strip_prefix("~/") {
        Some(rest) => home
            .filter(|h| !h.is_empty())
            .map(|h| format!("{}/{rest}", h.trim_end_matches(['/', '\\']))),
        None => Some(install.dir.clone()),
    }
}

/// An own file's text, at `version`.
pub fn file_text(text: &str, version: u32) -> String {
    text.replace("{version}", &version.to_string())
}

fn open_marker(harness: &str, comment: &str) -> String {
    format!("{comment} >>> fleet integration {harness},")
}

fn close_marker(harness: &str, comment: &str) -> String {
    format!("{comment} <<< fleet integration {harness}")
}

/// A block, its comment lines included, ending in a newline.
pub fn block_text(harness: &str, block: &OwnedBlock, version: u32) -> String {
    let body = file_text(block.text.trim_start_matches('\n'), version);
    let body = if body.ends_with('\n') {
        body
    } else {
        format!("{body}\n")
    };
    format!(
        "{} version {version}: `fleet integration uninstall {harness}` removes this block\n{body}{}\n",
        open_marker(harness, &block.comment),
        close_marker(harness, &block.comment),
    )
}

/// Where Fleet's block for `harness` is in `text`: from the start of its
/// first comment line to the end of its last, newline included. A first line
/// with no last one after it, or a second block, is an error: removing from
/// one first line to a later last line would take the user's lines between.
pub fn find_block(
    text: &str,
    harness: &str,
    comment: &str,
) -> Result<Option<Range<usize>>, String> {
    let open = open_marker(harness, comment);
    let close = close_marker(harness, comment);
    let broken = || {
        format!(
            "Fleet's block for {harness} lost its first or last comment line, or is there twice"
        )
    };
    let mut at = 0;
    let mut start = None;
    let mut found = None;
    for line in text.split_inclusive('\n') {
        let end = at + line.len();
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare.starts_with(&open) {
            if start.is_some() || found.is_some() {
                return Err(broken());
            }
            start = Some(at);
        } else if bare == close {
            match start.take() {
                Some(s) => found = Some(s..end),
                None => return Err(broken()),
            }
        }
        at = end;
    }
    if start.is_some() {
        return Err(broken());
    }
    Ok(found)
}

/// `current` with `block` appended after a blank line, and what was put
/// between them: the user's bytes are left as they are.
pub fn append_block(current: &str, block: &str) -> (String, String) {
    let prefix = match current {
        "" => "",
        c if c.ends_with('\n') => "\n",
        _ => "\n\n",
    };
    (format!("{current}{prefix}{block}"), prefix.to_string())
}

/// `text` without the block at `range`, and without the `prefix` Fleet put
/// before it when that is still there.
pub fn remove_block(text: &str, range: Range<usize>, prefix: &str) -> String {
    let start = if text[..range.start].ends_with(prefix) {
        range.start - prefix.len()
    } else {
        range.start
    };
    let (before, after) = (&text[..start], &text[range.end..]);
    // A line the user added after the block keeps a line of its own.
    let joint = if !before.is_empty() && !before.ends_with('\n') && !after.is_empty() {
        "\n"
    } else {
        ""
    };
    format!("{before}{joint}{after}")
}

/// A short, stable hash of what Fleet wrote (FNV-1a, 64 bits).
pub fn hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// What the profile installs, relative to the config directory.
pub fn paths(install: &Install) -> BTreeSet<&str> {
    install
        .files
        .iter()
        .map(|f| f.path.as_str())
        .chain(install.blocks.iter().map(|b| b.path.as_str()))
        .collect()
}

/// The state of `profile`'s integration on Unix: what the ledger says Fleet
/// wrote, against what is on disk and what the agent reads now (`resolved`,
/// its config directory).
pub fn state(
    profile: &Profile,
    entry: Option<&Entry>,
    resolved: Option<&str>,
    observed: &Observed,
) -> State {
    state_on(profile, entry, resolved, observed, Platform::Unix)
}

/// As [`state`], on `platform`.
pub fn state_on(
    profile: &Profile,
    entry: Option<&Entry>,
    resolved: Option<&str>,
    observed: &Observed,
    platform: Platform,
) -> State {
    let Some(install) = &profile.install else {
        return if profile.authority == Authority::None {
            State::Unsupported
        } else {
            State::NotNeeded
        };
    };
    if supported(install, platform).is_err() {
        return State::UnsupportedPlatform;
    }
    let Some(entry) = entry else {
        return if observed.dir_exists {
            State::NotInstalled
        } else {
            State::AgentMissing
        };
    };
    if !observed.dir_exists {
        return State::AgentMissing;
    }
    let changed = entry.paths().into_iter().any(|p| {
        matches!(observed.found.get(p), Some(Some(now)) if Some(now.as_str()) != entry.hash_of(p))
    });
    if changed {
        return State::Modified;
    }
    let missing = entry
        .paths()
        .into_iter()
        .any(|p| !matches!(observed.found.get(p), Some(Some(_))));
    if missing
        || entry.version < install.version
        || entry.paths() != paths(install)
        || resolved.is_some_and(|d| d != entry.dir)
    {
        return State::Outdated;
    }
    State::Current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::parse;

    const STUB: &str = r#"
name = "stub"
program = "stub"
authority = "full"
[install]
version = 2
dir_env = ["STUB_HOME"]
dir = "~/.stub"
[[install.files]]
path = "hooks/fleet.json"
text = '{"version":{version}}'
[[install.blocks]]
path = "config.toml"
text = '''
[[hooks]]
command = "fleet signal idle"
'''
"#;

    fn stub() -> Profile {
        parse(STUB).unwrap()
    }

    #[test]
    fn a_block_is_fenced_found_and_removed_to_the_users_bytes() {
        let p = stub();
        let block = block_text("stub", &p.install.as_ref().unwrap().blocks[0], 2);
        assert_eq!(
            block,
            "# >>> fleet integration stub, version 2: `fleet integration uninstall stub` removes this block\n[[hooks]]\ncommand = \"fleet signal idle\"\n# <<< fleet integration stub\n"
        );
        for user in ["", "a = 1", "a = 1\n", "a = 1\n\n"] {
            let (text, prefix) = append_block(user, &block);
            assert!(text.starts_with(user));
            let range = find_block(&text, "stub", "#").unwrap().expect("found");
            assert_eq!(&text[range.clone()], block);
            assert_eq!(remove_block(&text, range, &prefix), user, "{user:?}");
        }
        // Another agent's block, or one cut short, is not Fleet's for stub.
        assert_eq!(
            find_block(&block.replace("stub", "kimi"), "stub", "#"),
            Ok(None)
        );
        assert!(find_block(&block[..block.len() - 30], "stub", "#").is_err());
        // A user's line after the block stays where it is.
        let (text, prefix) = append_block("a = 1", &block);
        let text = format!("{text}b = 2\n");
        let range = find_block(&text, "stub", "#").unwrap().unwrap();
        assert_eq!(remove_block(&text, range, &prefix), "a = 1\nb = 2\n");
        // A first line with no last one, or two first lines, is no block
        // Fleet can remove: it would take the user's lines between them.
        let cut = block.replace("# <<< fleet integration stub\n", "");
        assert!(find_block(&cut, "stub", "#").is_err());
        assert!(find_block(&format!("{cut}{block}"), "stub", "#").is_err());
        assert!(find_block("a = 1\n", "stub", "#").unwrap().is_none());
    }

    #[test]
    fn the_config_dir_is_the_agents_variable_else_its_default_under_home() {
        let install = stub().install.unwrap();
        let none = |_: &str| None;
        assert_eq!(
            config_dir(&install, none, Some("/home/u/")).as_deref(),
            Some("/home/u/.stub")
        );
        assert_eq!(config_dir(&install, none, None), None);
        let set = |v: &str| (v == "STUB_HOME").then(|| "/elsewhere".to_string());
        assert_eq!(
            config_dir(&install, set, None).as_deref(),
            Some("/elsewhere")
        );
        let empty = |_: &str| Some(String::new());
        assert_eq!(
            config_dir(&install, empty, Some("/h")).as_deref(),
            Some("/h/.stub")
        );
    }

    #[test]
    fn the_state_follows_the_ledger_against_the_disk() {
        let p = stub();
        let entry = Entry {
            version: 2,
            dir: "/h/.stub".into(),
            files: vec![WrittenFile {
                path: "hooks/fleet.json".into(),
                hash: "f".into(),
            }],
            blocks: vec![WrittenBlock {
                path: "config.toml".into(),
                hash: "b".into(),
                comment: "#".into(),
                prefix: String::new(),
                created_file: false,
            }],
            dirs: vec![],
        };
        let seen = |f: Option<&str>, b: Option<&str>| Observed {
            dir_exists: true,
            found: [
                ("hooks/fleet.json".to_string(), f.map(str::to_string)),
                ("config.toml".to_string(), b.map(str::to_string)),
            ]
            .into(),
        };
        let here = Some("/h/.stub");
        let at = |e: Option<&Entry>, o: &Observed| state(&p, e, here, o);
        assert_eq!(
            at(Some(&entry), &seen(Some("f"), Some("b"))),
            State::Current
        );
        assert_eq!(
            at(Some(&entry), &seen(Some("x"), Some("b"))),
            State::Modified
        );
        assert_eq!(at(Some(&entry), &seen(None, Some("b"))), State::Outdated);
        let older = Entry {
            version: 1,
            ..entry.clone()
        };
        assert_eq!(
            at(Some(&older), &seen(Some("f"), Some("b"))),
            State::Outdated
        );
        assert_eq!(
            state(
                &p,
                Some(&entry),
                Some("/other"),
                &seen(Some("f"), Some("b"))
            ),
            State::Outdated
        );
        assert_eq!(at(None, &Observed::default()), State::AgentMissing);
        assert_eq!(
            at(
                None,
                &Observed {
                    dir_exists: true,
                    ..Observed::default()
                }
            ),
            State::NotInstalled
        );
        let claude = crate::harness::profile("claude");
        assert_eq!(
            state(&claude, None, None, &Observed::default()),
            State::NotNeeded
        );
        let aider = crate::harness::bare("aider");
        assert_eq!(
            state(&aider, None, None, &Observed::default()),
            State::Unsupported
        );
    }

    #[test]
    fn an_integration_that_does_not_name_windows_is_refused_there() {
        let p = stub();
        let seen = Observed {
            dir_exists: true,
            ..Observed::default()
        };
        assert_eq!(
            state_on(&p, None, Some("/h/.stub"), &seen, Platform::Windows),
            State::UnsupportedPlatform
        );
        assert_eq!(
            state_on(&p, None, Some("/h/.stub"), &seen, Platform::Unix),
            State::NotInstalled
        );
        let install = p.install.as_ref().unwrap();
        let refused = supported(install, Platform::Windows).unwrap_err();
        assert!(
            refused.contains("not supported on Windows yet"),
            "{refused}"
        );
        assert!(supported(install, Platform::Unix).is_ok());
        let windows =
            parse(&STUB.replace("version = 2\n", "version = 2\nwindows = true\n")).unwrap();
        assert!(supported(windows.install.as_ref().unwrap(), Platform::Windows).is_ok());
        // Kimi's hooks are POSIX shell commands: refused on Windows.
        let kimi = crate::harness::profile("kimi");
        assert!(supported(kimi.install.as_ref().unwrap(), Platform::Windows).is_err());
    }

    #[test]
    fn a_ledger_reads_back_what_it_wrote() {
        let mut ledger = Ledger::default();
        ledger.integrations.insert(
            "stub".into(),
            Entry {
                version: 1,
                dir: "/h".into(),
                files: vec![],
                blocks: vec![],
                dirs: vec!["/h/hooks".into()],
            },
        );
        let text = serde_json::to_string(&ledger).unwrap();
        assert!(text.contains(r#""version":1"#), "{text}");
        assert_eq!(serde_json::from_str::<Ledger>(&text).unwrap(), ledger);
        assert_eq!(serde_json::from_str::<Ledger>("{}").unwrap().version, 1);
        assert_eq!(hash(b""), "cbf29ce484222325");
    }
}
