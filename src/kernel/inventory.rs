//! What the interface is made of, and which of it is actually running.
//!
//! The kernel already knows every one of these facts and, until now, kept all
//! of them: a plugin whose slot the arrangement never places is dropped without
//! a word (`layout.rs`), and a reload that fails leaves the previous version
//! running, so the screen looks right while the file on disk is not what is on
//! it. Both are invisible from inside, which is the failure this module exists
//! to end (v2-plugin-lifecycle design D5).
//!
//! It is pure. The facts arrive as arguments — delivery says where a file came
//! from, the paint pass says what it drew — and one function joins them.

use std::collections::{BTreeMap, HashSet};

use super::bundled::Source;
use super::host::{Capability, Plugin};

/// What a file of the interface is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A file in `plugins/`: something that draws, or decorates what does.
    Pane,
    /// A file in `lib/`: loaded only when a pane requires it.
    Module,
    /// `layout.lua`.
    Arrangement,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Pane => "pane",
            Kind::Module => "module",
            Kind::Arrangement => "arrangement",
        }
    }

    fn of(relative: &str) -> Self {
        if relative == "layout.lua" {
            Kind::Arrangement
        } else if relative.starts_with("lib/") {
            Kind::Module
        } else {
            Kind::Pane
        }
    }
}

/// Why a file is, or is not, on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Drawn on the last frame.
    Visible,
    /// Loaded, its slot placed, but another occupant holds it — or its column
    /// is closed. Normal, and not a fault.
    Hidden,
    /// Loaded, and its slot appears nowhere in the arrangement at this size.
    /// The silent drop, made loud.
    Unplaced,
    /// On disk and not loaded. The reload is all-or-nothing, so this is the
    /// whole environment failing, attributed to the file that names itself in
    /// the error.
    Failed,
    /// Shipped, and deleted by the user. Restorable from the binary.
    Removed,
    /// On disk, intact, and deliberately not loaded.
    ///
    /// A *chosen* state, not a fault — which is why it sorts with the ordinary
    /// ones rather than with `Failed` and `Removed`. It is the answer to "why is
    /// this pane not here" that nothing else could express: the file is fine and
    /// nobody asked for it to run.
    Disabled,
    /// Present, and not the kind of file that draws.
    Present,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Visible => "visible",
            State::Hidden => "hidden",
            State::Unplaced => "unplaced",
            State::Failed => "failed",
            State::Removed => "removed",
            State::Disabled => "disabled",
            State::Present => "present",
        }
    }
}

/// One file of the interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Path relative to the interface directory — the identity every command
    /// about this file uses, since it is the only name a file always has.
    pub path: String,
    /// The plugin's declared name, or the file's own when it declares none.
    pub name: String,
    pub kind: Kind,
    /// Empty for anything that is not a pane.
    pub slot: String,
    pub source: Source,
    pub state: State,
    /// The load error, when one names this file.
    pub error: Option<String>,
    /// Capabilities this file asks for. Empty for almost everything, which is
    /// what makes a file that asks worth noticing in the list.
    pub capabilities: Vec<Capability>,
    /// Whether the user has trusted this file with what it asks for.
    pub trust: Trust,
}

/// Where a file stands with the user, for the capabilities it declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Asks for nothing, so there is nothing to trust it with.
    NotAsked,
    /// Asks, and has not been trusted. It loads and draws; the capability is
    /// simply absent from it.
    Untrusted,
    Trusted,
    /// Trusted, and its contents have changed since. Not a refusal — the
    /// capability still works — but the one thing the user would want to know
    /// before it keeps working.
    Drifted,
}

impl Trust {
    pub fn as_str(self) -> &'static str {
        match self {
            Trust::NotAsked => "",
            Trust::Untrusted => "untrusted",
            Trust::Trusted => "trusted",
            Trust::Drifted => "trusted · modified",
        }
    }

    /// Resolve one file's standing from what was trusted and what is on disk.
    ///
    /// Pure, so the three-way answer is testable without a registry or a
    /// filesystem — and so the drift rule lives in one place rather than at each
    /// caller.
    pub fn resolve(trusted_digest: Option<&str>, current_digest: Option<&str>) -> Self {
        match (trusted_digest, current_digest) {
            (None, _) => Trust::Untrusted,
            (Some(_), None) => Trust::Trusted,
            (Some(was), Some(now)) if was == now => Trust::Trusted,
            (Some(_), Some(_)) => Trust::Drifted,
        }
    }
}

/// Join what delivery knows with what the last frame drew.
///
/// `visible` and `placed` are as of the last painted frame, which is what makes
/// `Unplaced` honest rather than a guess: placement depends on the terminal's
/// current size, so the only true answer is the one the arrangement just gave.
#[allow(clippy::too_many_arguments)]
pub fn rows(
    plugins: &[Plugin],
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
        let loaded = plugins
            .iter()
            .enumerate()
            .find(|(_, plugin)| plugin.path == *path);

        let state = match (source, loaded) {
            (Source::Removed, _) => State::Removed,
            // Checked before the not-loaded arms below: a disabled pane IS not
            // loaded, and without this it would be reported as having failed —
            // the one reading that turns a deliberate choice into an alarm.
            _ if disabled(path) => State::Disabled,
            // A decorator occupies no slot by design, so asking whether it is
            // on screen is the wrong question — it draws into someone else's
            // tree, and reporting it as hidden would read as a fault.
            (_, Some((_, plugin))) if plugin.decorates.is_some() => State::Present,
            (_, Some((index, plugin))) => {
                if visible.contains(&index) {
                    State::Visible
                } else if placed.contains(&plugin.slot) || plugin.floats {
                    State::Hidden
                } else {
                    State::Unplaced
                }
            }
            // Not loaded. For a pane that is a failure; for a module or the
            // arrangement it is the normal case — nothing loads a `lib/` file
            // until a pane requires it.
            (_, None) if kind == Kind::Pane => State::Failed,
            (_, None) => State::Present,
        };

        let error = error
            .filter(|error| blames(error, path))
            .map(|error| error.to_string());

        out.push(Row {
            name: loaded
                .map(|(_, plugin)| plugin.name.clone())
                .unwrap_or_else(|| file_name(path).to_string()),
            path: path.clone(),
            kind,
            slot: loaded
                .map(|(_, plugin)| plugin.slot.clone())
                .filter(|_| kind == Kind::Pane)
                .unwrap_or_default(),
            source: *source,
            state,
            error,
            capabilities: loaded
                .map(|(_, plugin)| plugin.capabilities.clone())
                .unwrap_or_default(),
            trust: match loaded {
                Some((_, plugin)) if !plugin.capabilities.is_empty() => trust(path),
                _ => Trust::NotAsked,
            },
        });
    }
    out
}

/// Whether a load error names this file.
///
/// `load_plugin` prefixes the file stem and `load_arrangement` the file name,
/// so matching on either is what attributes an all-or-nothing reload failure to
/// the one file that caused it.
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
            .map(|(path, source)| ((*path).to_string(), *source))
            .collect()
    }

    #[test]
    fn a_pane_on_disk_that_did_not_load_is_a_failure_with_its_error() {
        let rows = rows(
            &[],
            &sources(&[("plugins/50_mine.lua", Source::User)]),
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
            &sources(&[("plugins/10_sessions.lua", Source::Removed)]),
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
    fn a_file_that_asks_for_nothing_is_never_a_trust_question() {
        // Recording a decision about a capability a file never wanted would fill
        // the trust list with rows nobody can act on.
        assert_eq!(Trust::NotAsked.as_str(), "");
    }
}
