use std::collections::{BTreeMap, HashSet};

use super::bundled::Source;
use super::host::{Capability, Mod};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pane,
    Module,
    Arrangement,
    Doc,
    Manifest,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Pane => "pane",
            Kind::Module => "module",
            Kind::Arrangement => "arrangement",
            Kind::Doc => "doc",
            Kind::Manifest => "manifest",
        }
    }

    fn of(relative: &str) -> Self {
        if relative == "layout.lua" {
            Kind::Arrangement
        } else if relative == crate::session::mod_spec::SPEC_FILE {
            // Before the `mods/` fallthrough, for the same reason `Doc` is: a
            // manifest reported as a pane that failed to load is a false alarm,
            // and the loader never offers it one (it reads `mods/*.lua`).
            Kind::Manifest
        } else if relative.ends_with(".md") {
            Kind::Doc
        } else if relative.starts_with("lib/") {
            Kind::Module
        } else {
            Kind::Pane
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Visible,
    Hidden,
    OnDemand,
    Unplaced,
    Failed,
    Removed,
    Disabled,
    Present,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Visible => "visible",
            State::Hidden => "hidden",
            State::OnDemand => "on demand",
            State::Unplaced => "unplaced",
            State::Failed => "failed",
            State::Removed => "removed",
            State::Disabled => "disabled",
            State::Present => "present",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: String,
    pub name: String,
    pub kind: Kind,
    pub slot: String,
    pub source: Source,
    pub state: State,
    pub error: Option<String>,
    pub capabilities: Vec<Capability>,
    pub trust: Trust,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    NotAsked,
    Untrusted,
    Trusted,
    Drifted,
    Replaced,
    Shipped,
}

impl Trust {
    pub fn as_str(self) -> &'static str {
        match self {
            Trust::NotAsked => "",
            Trust::Untrusted => "untrusted",
            Trust::Trusted => "trusted",
            Trust::Drifted => "trusted · modified",
            Trust::Replaced => "untrusted · modified",
            Trust::Shipped => "shipped",
        }
    }

    pub fn grants(self) -> bool {
        matches!(self, Trust::Trusted | Trust::Drifted | Trust::Shipped)
    }

    pub fn resolve(trusted_digest: Option<&str>, current_digest: Option<&str>) -> Self {
        match (trusted_digest, current_digest) {
            (None, _) => Trust::Untrusted,
            (Some(_), None) => Trust::Trusted,
            (Some(was), Some(now)) if was == now => Trust::Trusted,
            (Some(_), Some(_)) => Trust::Drifted,
        }
    }

    pub fn resolve_installed(
        granted: Option<(&str, &str)>,
        current_pin: &str,
        current_digest: Option<&str>,
    ) -> Self {
        match granted {
            None => Trust::Untrusted,
            // Granted to a different version than the one installed now. Not a
            // warning — simply not granted, so the capability is absent until the
            // user says so again.
            Some((pin, _)) if pin != current_pin => Trust::Untrusted,
            Some((_, was)) => match current_digest {
                None => Trust::Trusted,
                Some(now) if was == now => Trust::Trusted,
                Some(_) => Trust::Replaced,
            },
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn rows(
    mods: &[Mod],
    sources: &BTreeMap<String, Source>,
    visible: &HashSet<usize>,
    placed: &HashSet<String>,
    error: Option<&str>,
    trust: &dyn Fn(&str) -> Trust,
    disabled: &dyn Fn(&str) -> bool,
) -> Vec<Row> {
    let mut out = Vec::with_capacity(sources.len());
    for (path, source) in sources {
        let kind = Kind::of(path);
        let loaded = mods
            .iter()
            .enumerate()
            .find(|(_, found)| found.path == *path);

        let state = match (source, loaded) {
            (Source::Removed, _) => State::Removed,
            // Checked before the not-loaded arms below: a disabled pane IS not
            // loaded, and without this it would be reported as having failed —
            // the one reading that turns a deliberate choice into an alarm.
            _ if disabled(path) => State::Disabled,
            // A decorator occupies no slot by design, so asking whether it is
            // on screen is the wrong question — it draws into someone else's
            // tree, and reporting it as hidden would read as a fault.
            (_, Some((_, found))) if found.decorates.is_some() => State::Present,
            (_, Some((index, found))) => {
                if visible.contains(&index) {
                    State::Visible
                } else if found.floats {
                    // Asked before the slot question, because a float's slot is
                    // beside the point — it is not waiting for a column to open.
                    State::OnDemand
                } else if placed.contains(&found.slot) {
                    State::Hidden
                } else {
                    State::Unplaced
                }
            }
            // Not loaded. For a pane that is a failure; for a module or the
            // arrangement it is the normal case — nothing loads a `lib/` file
            // until a pane requires it.
            (_, None) if kind == Kind::Pane => State::Failed,
            // A doc and a `lib/` module are both "on disk, nothing loads it until
            // something asks" — neither is a fault.
            (_, None) => State::Present,
        };

        let error = error
            .filter(|error| blames(error, path))
            .map(ToString::to_string);

        out.push(Row {
            name: loaded
                .map(|(_, found)| found.name.clone())
                .unwrap_or_else(|| file_name(path).to_string()),
            path: path.clone(),
            kind,
            slot: loaded
                .map(|(_, found)| found.slot.clone())
                .filter(|_| kind == Kind::Pane)
                .unwrap_or_default(),
            source: source.clone(),
            state,
            error,
            capabilities: loaded
                .map(|(_, found)| found.capabilities.clone())
                .unwrap_or_default(),
            trust: match loaded {
                Some((_, found)) if !found.capabilities.is_empty() => trust(path),
                _ => Trust::NotAsked,
            },
        });
    }
    out
}

fn blames(error: &str, path: &str) -> bool {
    let name = file_name(path);
    let stem = name.strip_suffix(".lua").unwrap_or(name);
    error.starts_with(&format!("{name}:")) || error.starts_with(&format!("{stem}:"))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(pairs: &[(&str, Source)]) -> BTreeMap<String, Source> {
        pairs
            .iter()
            .map(|(path, source)| ((*path).to_string(), source.clone()))
            .collect()
    }

    #[test]
    fn a_pane_on_disk_that_did_not_load_is_a_failure_with_its_error() {
        let rows = rows(
            &[],
            &sources(&[("mods/50_mine.lua", Source::User)]),
            &HashSet::new(),
            &HashSet::new(),
            Some("50_mine: attempt to index a nil value"),
            &|_| Trust::NotAsked,
            &|_| false,
        );
        assert_eq!(rows[0].state, State::Failed);
        assert!(rows[0].error.is_some());
    }

    #[test]
    fn an_error_naming_another_file_is_not_pinned_on_this_one() {
        let rows = rows(
            &[],
            &sources(&[("lib/theme.lua", Source::Bundled)]),
            &HashSet::new(),
            &HashSet::new(),
            Some("50_mine: attempt to index a nil value"),
            &|_| Trust::NotAsked,
            &|_| false,
        );
        assert_eq!(rows[0].state, State::Present);
        assert_eq!(rows[0].error, None);
    }

    #[test]
    fn a_removed_file_is_listed_though_nothing_loaded_it() {
        let rows = rows(
            &[],
            &sources(&[("mods/10_sessions.lua", Source::Removed)]),
            &HashSet::new(),
            &HashSet::new(),
            None,
            &|_| Trust::NotAsked,
            &|_| false,
        );
        assert_eq!(rows[0].state, State::Removed);
        assert_eq!(rows[0].source, Source::Removed);
    }

    #[test]
    fn the_arrangement_and_the_modules_are_listed_as_what_they_are() {
        let rows = rows(
            &[],
            &sources(&[
                ("layout.lua", Source::Edited),
                ("lib/theme.lua", Source::Bundled),
            ]),
            &HashSet::new(),
            &HashSet::new(),
            None,
            &|_| Trust::NotAsked,
            &|_| false,
        );
        assert_eq!(rows[0].kind, Kind::Arrangement);
        assert_eq!(rows[0].source, Source::Edited);
        assert_eq!(rows[1].kind, Kind::Module);
        assert!(rows.iter().all(|row| row.slot.is_empty()));
    }

    #[test]
    fn trust_reads_four_ways() {
        // The drift rule in one place, because the three callers must not each
        // decide what "trusted but changed" means.
        assert_eq!(Trust::resolve(None, Some("a")), Trust::Untrusted);
        assert_eq!(Trust::resolve(Some("a"), Some("a")), Trust::Trusted);
        assert_eq!(Trust::resolve(Some("a"), Some("b")), Trust::Drifted);
        // Trusted, and the file cannot be read now — reported as trusted rather
        // than as drifted, because "changed" is a claim we cannot make.
        assert_eq!(Trust::resolve(Some("a"), None), Trust::Trusted);
    }

    #[test]
    fn only_trusted_and_an_edited_own_file_hold_the_grant() {
        assert!(Trust::Trusted.grants());
        assert!(Trust::Drifted.grants(), "the user's own file, edited");
        assert!(
            !Trust::Replaced.grants(),
            "an installed file, changed under its pin"
        );
        assert!(!Trust::Untrusted.grants());
        assert!(!Trust::NotAsked.grants());
        assert!(Trust::Shipped.grants(), "the binary's own copy");
    }

    #[test]
    fn an_installed_files_trust_is_judged_on_its_version_and_its_contents() {
        // Never granted.
        assert_eq!(
            Trust::resolve_installed(None, "atlas@v1", Some("a")),
            Trust::Untrusted
        );
        // Granted, untouched — and again after a reinstall at the same version,
        // which rewrites identical contents.
        assert_eq!(
            Trust::resolve_installed(Some(("atlas@v1", "a")), "atlas@v1", Some("a")),
            Trust::Trusted
        );
        // The pin moved. Not a warning: simply not granted, so the capability is
        // absent until the user says so again. This is what stops every ordinary
        // release reading as tampering.
        assert_eq!(
            Trust::resolve_installed(Some(("atlas@v1", "a")), "atlas@v2", Some("b")),
            Trust::Untrusted
        );
        // Edited locally at the granted version.
        assert_eq!(
            Trust::resolve_installed(Some(("atlas@v1", "a")), "atlas@v1", Some("mine")),
            Trust::Replaced
        );
        // THE ROW THAT CANNOT BE DROPPED: same source, same version, different
        // contents — an upstream re-tag. Keying the grant on `src@version` alone
        // would let a substitution keep a capability granted to what that version
        // used to be, which is a supply-chain hole opened by the very change meant
        // to reduce warning fatigue.
        assert_eq!(
            Trust::resolve_installed(Some(("atlas@v1", "a")), "atlas@v1", Some("swapped")),
            Trust::Replaced
        );
        // Unreadable now: reported as trusted, because "changed" is a claim we
        // cannot make — the same answer the unmanaged rule gives.
        assert_eq!(
            Trust::resolve_installed(Some(("atlas@v1", "a")), "atlas@v1", None),
            Trust::Trusted
        );
    }

    #[test]
    fn a_file_that_asks_for_nothing_is_never_a_trust_question() {
        // Recording a decision about a capability a file never wanted would fill
        // the trust list with rows nobody can act on.
        assert_eq!(Trust::NotAsked.as_str(), "");
    }
}
