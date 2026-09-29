//! Which thurbox window is whose: the names thurbox gives each role's
//! window, and the resolution rule every reconciler asks
//! (ADR-25) — a stamp is proof, a lone namesake is the fallback, and an
//! ambiguous listing is never read as an absent one.
//!
//! Backend-neutral: an adapter lists its windows as [`DiscoveredSession`]s and
//! this decides. How a stamp is stored (a tmux window option, or nothing at
//! all) is the adapter's business.

use std::collections::HashMap;

use crate::backend::contract::{BackendLiveness, DiscoveredSession, WindowRole};

/// Window-name prefix for a thurbox agent window. Combined with the
/// sanitized session name (`{prefix}{sanitized_name}`) to form its name. Live
/// windows carry it, so it never changes.
pub(crate) const WINDOW_PREFIX: &str = "tb-";

/// Prefix for the companion shell window a session lazily spawns.
pub(crate) const SHELL_WINDOW_PREFIX: &str = "tbs-";

/// Sanitize a session name into a tmux-safe window-name component.
///
/// tmux parses target strings as `session:window`, and — depending on
/// version and context (e.g. `run-shell` scripts, `display-message`
/// format expansion) — treats whitespace, colons, commas, and `.` as
/// delimiters within the target string. Any character outside
/// `[A-Za-z0-9_-]` is replaced with `_` so the produced window name
/// round-trips cleanly through every tmux CLI/control-mode call.
///
/// The resulting string is deterministic — callers must use it both at
/// window-creation time and at lookup time for matching to succeed.
pub(crate) fn sanitize_window_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out
}

/// The window name for a thurbox agent session: `tb-<safe>`.
pub(crate) fn agent_window_name(session_name: &str) -> String {
    format!("{WINDOW_PREFIX}{}", sanitize_window_name(session_name))
}

/// The window name for a session's companion shell pane.
pub(crate) fn shell_window_name(session_name: &str) -> String {
    format!(
        "{SHELL_WINDOW_PREFIX}{}",
        sanitize_window_name(session_name)
    )
}

/// Prefix for a pane a *plugin* asked for, holding a program it named.
///
/// A third prefix rather than reusing `tbs-`: window discovery adopts panes by
/// prefix, and a plugin's pane must never be picked up as a session's anything.
pub(crate) const PROGRAM_WINDOW_PREFIX: &str = "tbp-";

/// The window name for a plugin-owned program pane.
///
/// `owner` is a short **digest** of the owning plugin's path, computed by the
/// caller, not the path itself. Two reasons, and the first is a correctness one:
/// [`sanitize_window_name`] maps every character outside `[A-Za-z0-9_-]` to `_`,
/// so `plugins/90_watch.lua` and `plugins.90.watch.lua` would sanitize to the same
/// window and two plugins would share one program. The second is that a path is
/// long enough to make the window list unreadable.
///
/// Deterministic, which is the whole mechanism for finding the window again after
/// a restart — there is no stored pane id to go stale.
pub(crate) fn program_window_name(owner: &str, pane: &str) -> String {
    format!(
        "{PROGRAM_WINDOW_PREFIX}{}-{}",
        sanitize_window_name(owner),
        sanitize_window_name(pane)
    )
}

/// The window name a session's `role` window carries.
pub(in crate::backend) fn window_name_for(role: WindowRole, session_name: &str) -> String {
    match role {
        WindowRole::Shell => shell_window_name(session_name),
        _ => agent_window_name(session_name),
    }
}

impl WindowRole {
    /// The role a window *name* implies, for one spawned before windows were
    /// stamped. `None` for a window that is not thurbox's at all.
    pub(in crate::backend) fn from_window_name(name: &str) -> Option<Self> {
        if name.starts_with(SHELL_WINDOW_PREFIX) {
            Some(Self::Shell)
        } else if name.starts_with(PROGRAM_WINDOW_PREFIX) {
            Some(Self::Program)
        } else if name.starts_with(WINDOW_PREFIX) {
            Some(Self::Agent)
        } else {
            None
        }
    }
}

/// Where a listing puts a session's window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Located {
    /// The window is this pane.
    At(String),
    /// The listing covers the server and nothing on it is this session's.
    Absent,
    /// The listing cannot say: more than one window answers to the name and at
    /// least one of them carries no stamp.
    ///
    /// Never collapse this into [`Located::Absent`]. Reading ambiguity as
    /// absence is what relaunches a session that is already running, so two
    /// colliding windows become three.
    Unknown,
}

impl Located {
    /// The pane, when there is one to act on.
    pub fn pane(self) -> Option<String> {
        match self {
            Self::At(pane) => Some(pane),
            _ => None,
        }
    }

    /// Whether the listing positively says there is no such window. The only
    /// answer a relaunch may act on.
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

/// One thurbox window as a listing reported it.
#[derive(Clone, Debug)]
struct ListedWindow {
    pane: String,
    /// The owning session's stamp (on tmux, `@thurbox_session`), empty for a window spawned before
    /// windows were stamped (or by a multiplexer without window options).
    session: String,
    alive: bool,
}

/// A backend's thurbox windows, indexed the two ways ownership is asked about.
///
/// Built from one `list-windows`, so every question below is answered against
/// the same instant rather than a fresh round trip each.
#[derive(Clone, Debug, Default)]
pub struct WindowIndex {
    /// Windows carrying a stamp, by the identity they carry.
    stamped: HashMap<(String, WindowRole), Vec<ListedWindow>>,
    /// Every thurbox window by name, stamped or not. This is what tells an
    /// *ambiguous* name apart from an absent one.
    by_name: HashMap<String, Vec<ListedWindow>>,
    /// The window each listed pane sits in, for the one question asked the
    /// other way round: is *this* pane still ours (see [`Self::places_agent`]).
    by_pane: HashMap<String, (String, String, WindowRole)>,
}

impl WindowIndex {
    /// Index one backend's listing.
    pub fn from_listing(windows: impl IntoIterator<Item = DiscoveredSession>) -> Self {
        let mut index = Self::default();
        for window in windows {
            let listed = ListedWindow {
                pane: window.backend_id,
                session: window.session,
                alive: window.is_alive,
            };
            if !listed.session.is_empty() {
                index
                    .stamped
                    .entry((listed.session.clone(), window.role))
                    .or_default()
                    .push(listed.clone());
            }
            index.by_pane.insert(
                listed.pane.clone(),
                (window.name.clone(), listed.session.clone(), window.role),
            );
            index.by_name.entry(window.name).or_default().push(listed);
        }
        index
    }

    /// Where a session's agent window is, counting one whose pane has exited —
    /// `remain-on-exit` keeps that window in place, and it is still the
    /// session's own (the interface attaches to it to show what went wrong).
    pub fn agent_window(&self, session_id: &str, session_name: &str) -> Located {
        self.locate(session_id, session_name, WindowRole::Agent, false)
    }

    pub fn agent_liveness(&self, session_id: &str, session_name: &str) -> BackendLiveness {
        match self.agent_window(session_id, session_name) {
            Located::At(_) => match self.live_agent_window(session_id, session_name) {
                Located::At(_) => BackendLiveness::Live,
                _ => BackendLiveness::Exited,
            },
            Located::Absent => BackendLiveness::Missing,
            Located::Unknown => BackendLiveness::Unknown,
        }
    }

    /// Where a session's *running* agent window is. The question every
    /// relaunch and liveness gate asks: a dead pane is not an agent.
    pub fn live_agent_window(&self, session_id: &str, session_name: &str) -> Located {
        self.locate(session_id, session_name, WindowRole::Agent, true)
    }

    /// Where a session's companion shell window is. Resolved by the same stamp
    /// as its agent: the row's `shell_backend_id` is only ever set once the
    /// interface has opened the shell, so a session that has one is routinely a
    /// session whose column is NULL.
    pub fn shell_window(&self, session_id: &str, session_name: &str) -> Located {
        self.locate(session_id, session_name, WindowRole::Shell, false)
    }

    /// Whether the listing puts `pane` in an agent window this session may
    /// claim — the positive reading, where a pane the listing does not place is
    /// only an absence.
    ///
    /// Asked of a pane the row already remembers, so it is answered the other
    /// way round from [`Self::agent_window`]: a stamp for this session settles
    /// it, and an *unstamped* window of this session's name is claimable too —
    /// that is the pre-ADR-25 shape, and nothing in the listing contradicts it.
    /// What it will not do is hand over a window stamped for somebody else,
    /// which is what a remembered pane id becomes once a tmux server restart
    /// has reissued it.
    pub fn places_agent(&self, session_id: &str, session_name: &str, pane: &str) -> bool {
        let Some((window, stamp, role)) = self.by_pane.get(pane) else {
            return false;
        };
        *role == WindowRole::Agent
            && if stamp.is_empty() {
                *window == agent_window_name(session_name)
            } else {
                stamp == session_id
            }
    }

    /// The resolution rule, in one place.
    ///
    /// A stamp is proof and is taken first. Without one the *name* is all
    /// there is, and it only decides anything while a single window answers to
    /// it: a lone unstamped window of the right name is this session's (the
    /// migration path for a window spawned before stamping, and for every
    /// window under a multiplexer with no window options), a lone window
    /// stamped for somebody else is theirs, and several windows with no stamp
    /// between them cannot be told apart.
    pub(in crate::backend) fn locate(
        &self,
        session_id: &str,
        session_name: &str,
        role: WindowRole,
        live_only: bool,
    ) -> Located {
        match self.stamped_match(session_id, role, live_only) {
            Some(found) => found,
            None => self.named_match(session_id, session_name, role, live_only),
        }
    }

    /// The stamp half: proof, and so taken first.
    ///
    /// `None` means there is no stamp to go on and the name is all that is
    /// left — which is not the same as an answer of [`Located::Absent`], and is
    /// why this is an `Option` rather than a `Located`.
    fn stamped_match(
        &self,
        session_id: &str,
        role: WindowRole,
        live_only: bool,
    ) -> Option<Located> {
        if session_id.is_empty() {
            return None;
        }
        match self
            .stamped
            .get(&(session_id.to_string(), role))?
            .as_slice()
        {
            [] => None,
            // One session, one window per role — two is a listing nobody can
            // act on rather than a choice to make. This holds regardless of
            // liveness: a dead entry does not make the ambiguity go away, and
            // must never fall through to a same-named window that belongs to
            // somebody else.
            [_, _, ..] => Some(Located::Unknown),
            [only] => Some(if !live_only || only.alive {
                Located::At(only.pane.clone())
            } else {
                Located::Absent
            }),
        }
    }

    /// The name half, reached only when no stamp decided it.
    fn named_match(
        &self,
        session_id: &str,
        session_name: &str,
        role: WindowRole,
        live_only: bool,
    ) -> Located {
        let usable = |w: &&ListedWindow| !live_only || w.alive;
        let window = window_name_for(role, session_name);
        let named: Vec<&ListedWindow> = self
            .by_name
            .get(&window)
            .into_iter()
            .flatten()
            .filter(usable)
            .collect();
        match named.as_slice() {
            [] => Located::Absent,
            // A caller with no id of its own (a window addressed only by name)
            // can claim a lone window; one with an id can only claim a window
            // that is not already somebody else's.
            [only] if session_id.is_empty() || only.session.is_empty() => {
                Located::At(only.pane.clone())
            }
            [_] => Located::Absent,
            // Every candidate stamped, none of them ours: definitively not here.
            _ if !session_id.is_empty() && named.iter().all(|w| !w.session.is_empty()) => {
                Located::Absent
            }
            _ => Located::Unknown,
        }
    }
}

#[cfg(test)]
pub(in crate::backend) mod tests {
    use super::*;

    fn dead(mut window: DiscoveredSession) -> DiscoveredSession {
        window.is_alive = false;
        window
    }

    const ONE: &str = "11111111-1111-4111-8111-111111111111";
    const TWO: &str = "22222222-2222-4222-8222-222222222222";

    #[test]
    fn sanitize_window_name_passes_through_safe_chars() {
        assert_eq!(sanitize_window_name("abc-123_XYZ"), "abc-123_XYZ");
    }

    #[test]
    fn sanitize_window_name_replaces_spaces() {
        // Bug: session names with spaces broke `tmux send-keys` / capture
        // because the target string `session:window with spaces` was
        // re-split by tmux into `session`, `window`, `with`, `spaces`.
        assert_eq!(sanitize_window_name("Foo Bar"), "Foo_Bar");
    }

    #[test]
    fn sanitize_window_name_replaces_tmux_delimiters() {
        // Colons, dots, commas all have meaning inside tmux target strings.
        assert_eq!(sanitize_window_name("a:b.c,d"), "a_b_c_d");
    }

    #[test]
    fn sanitize_window_name_replaces_non_ascii() {
        assert_eq!(sanitize_window_name("café"), "caf_");
    }

    #[test]
    fn agent_and_shell_window_names_share_sanitization() {
        assert_eq!(agent_window_name("Foo Bar"), "tb-Foo_Bar");
        assert_eq!(shell_window_name("Foo Bar"), "tbs-Foo_Bar");
    }

    /// A plugin's pane gets a prefix of its own, and its name is deterministic —
    /// which is the entire mechanism for finding the window again after a restart.
    #[test]
    fn a_program_window_is_named_deterministically_and_apart_from_sessions() {
        let once = program_window_name("abcd1234", "watch");
        assert_eq!(once, "tbp-abcd1234-watch");
        assert_eq!(
            once,
            program_window_name("abcd1234", "watch"),
            "deterministic"
        );

        // Distinct prefix, so window discovery cannot adopt one as a session's
        // agent (`tb-`) or its companion shell (`tbs-`).
        assert!(once.starts_with(PROGRAM_WINDOW_PREFIX));
        assert!(!once.starts_with(&format!("{WINDOW_PREFIX}a")));
        assert!(!once.starts_with(SHELL_WINDOW_PREFIX));
    }

    /// A listed window, as `discover` would have reported it.
    pub(in crate::backend) fn listed(
        pane: &str,
        window: &str,
        session: &str,
        role: WindowRole,
    ) -> DiscoveredSession {
        DiscoveredSession {
            backend_id: pane.into(),
            name: window.into(),
            is_alive: true,
            session: session.into(),
            role,
        }
    }

    /// The stamp is the identity, so a window answers to its session whatever
    /// it is called — which is what lets a renamed session's row and window
    /// disagree for the moment between the two writes without losing it.
    #[test]
    fn a_stamped_window_is_its_sessions_whatever_it_is_named() {
        let index =
            WindowIndex::from_listing([listed("%3", "tb-old_name", ONE, WindowRole::Agent)]);
        assert_eq!(
            index.agent_window(ONE, "new name"),
            Located::At("%3".into())
        );
    }

    /// The bug this whole mechanism exists for: the only `tb-fleet` on the
    /// server belongs to a live namesake, and a teardown resolving the name
    /// would kill it.
    #[test]
    fn a_namesakes_stamped_window_is_never_ours() {
        let index = WindowIndex::from_listing([listed("%7", "tb-fleet", TWO, WindowRole::Agent)]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Absent);
    }

    /// The same answer when the row remembers that very pane id — which is the
    /// state a tmux server restart leaves behind, since it reissues ids from
    /// `%0`. Nothing here consults the remembered id, and that is the point.
    #[test]
    fn a_reissued_pane_id_cannot_make_a_namesakes_window_ours() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", TWO, WindowRole::Agent),
            listed("%2", "tb-other", ONE, WindowRole::Agent),
        ]);
        // `%1` is what the stale row remembers; it resolves to its own window.
        assert_eq!(index.agent_window(ONE, "fleet"), Located::At("%2".into()));
    }

    /// The migration path: a window spawned before windows were stamped, and
    /// every window under a multiplexer that has no window options.
    #[test]
    fn a_lone_unstamped_namesake_is_adoptable() {
        let index = WindowIndex::from_listing([listed("%4", "tb-fleet", "", WindowRole::Agent)]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::At("%4".into()));
        // And to a caller with no id of its own at all.
        assert_eq!(index.agent_window("", "fleet"), Located::At("%4".into()));
    }

    /// Ambiguity is not absence. Reading it as absence is what relaunches a
    /// session that is already running, so two colliding windows become three.
    #[test]
    fn unstamped_namesakes_are_unknown_rather_than_absent() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", "", WindowRole::Agent),
            listed("%2", "tb-fleet", "", WindowRole::Agent),
        ]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Unknown);
        assert!(!index.agent_window(ONE, "fleet").is_absent());
    }

    /// Once every candidate carries a stamp there is no ambiguity left: none of
    /// them is ours, so the window really is gone and a relaunch is right.
    #[test]
    fn stamped_namesakes_that_are_all_someone_elses_read_as_absent() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", TWO, WindowRole::Agent),
            listed("%2", "tb-fleet", TWO, WindowRole::Agent),
        ]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Absent);
    }

    /// A session stamps both of its windows with the same id, so the role is
    /// what stops its shell being resolved — and killed — as its agent.
    #[test]
    fn a_sessions_shell_window_is_not_its_agent() {
        let index = WindowIndex::from_listing([listed("%5", "tbs-fleet", ONE, WindowRole::Shell)]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Absent);
        assert_eq!(index.shell_window(ONE, "fleet"), Located::At("%5".into()));
    }

    /// `remain-on-exit` keeps a failed agent's window in place so the error is
    /// readable. It is still the session's own window — the interface attaches
    /// to it — but it is not a *running* agent, which is the question every
    /// relaunch gate asks.
    #[test]
    fn a_dead_pane_is_still_the_sessions_window_but_not_a_live_one() {
        let index =
            WindowIndex::from_listing([dead(listed("%6", "tb-fleet", ONE, WindowRole::Agent))]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::At("%6".into()));
        assert_eq!(index.live_agent_window(ONE, "fleet"), Located::Absent);
        assert!(index.places_agent(ONE, "fleet", "%6"));
        assert!(!index.places_agent(ONE, "fleet", "%9"));
    }

    /// A dead own window must never be treated as "no stamped window at
    /// all" — that reading is what let a live unstamped namesake, sharing
    /// this session's `tb-<name>`, get attributed to this session instead.
    #[test]
    fn a_dead_own_window_does_not_fall_through_to_a_live_namesake() {
        let index = WindowIndex::from_listing([
            dead(listed("%6", "tb-fleet", ONE, WindowRole::Agent)),
            listed("%7", "tb-fleet", "", WindowRole::Agent),
        ]);
        assert_eq!(index.live_agent_window(ONE, "fleet"), Located::Absent);
    }

    /// A remembered pane id still resolves among *unstamped* namesakes, which
    /// is how two pre-ADR-25 sessions sharing a name each attach to their own
    /// pane. It stops resolving the moment a window says whose it is: a stamp
    /// for somebody else is what a reissued pane id turns into.
    #[test]
    fn a_remembered_pane_is_claimable_while_no_window_says_otherwise() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", "", WindowRole::Agent),
            listed("%2", "tb-fleet", "", WindowRole::Agent),
        ]);
        assert!(index.places_agent(ONE, "fleet", "%1"));
        assert!(index.places_agent(TWO, "fleet", "%2"));
        // Ambiguous for a caller with nothing but the name to go on.
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Unknown);

        let stamped = WindowIndex::from_listing([listed("%1", "tb-fleet", TWO, WindowRole::Agent)]);
        assert!(!stamped.places_agent(ONE, "fleet", "%1"));
        assert!(stamped.places_agent(TWO, "fleet", "%1"));
        // And a pane nothing listed is nobody's.
        assert!(!stamped.places_agent(TWO, "fleet", "%9"));
    }

    /// A program window is discovered but can never be adopted as a session's
    /// agent, which is what the role is for.
    ///
    /// Discovery used to filter on the `tb-` prefix, which excluded `tbs-` and
    /// `tbp-` outright — and so hid exactly the windows that make a name
    /// ambiguous. They are listed now; the *role*, not the listing, is what
    /// keeps a plugin's program from resolving as somebody's agent.
    #[test]
    fn a_program_window_is_discovered_but_is_nobodys_agent() {
        let program = program_window_name("abcd1234", "watch");
        assert_eq!(
            WindowRole::from_window_name(&program),
            Some(WindowRole::Program)
        );
        assert_eq!(
            WindowRole::from_window_name(&shell_window_name("s")),
            Some(WindowRole::Shell)
        );
        assert_eq!(
            WindowRole::from_window_name(&agent_window_name("s")),
            Some(WindowRole::Agent)
        );
        assert_eq!(WindowRole::from_window_name("someone-elses"), None);
    }

    /// Why the owner is a **digest** rather than the plugin's path.
    ///
    /// `sanitize_window_name` maps every character outside `[A-Za-z0-9_-]` to
    /// `_`, so two different paths sanitize to one window — and two plugins would
    /// then share a single program. The digest is computed by the caller for
    /// exactly this reason; this pins the hazard that makes it necessary.
    #[test]
    fn sanitizing_a_path_would_collide_which_is_why_the_owner_is_digested() {
        assert_eq!(
            sanitize_window_name("plugins/90_watch.lua"),
            sanitize_window_name("plugins.90.watch.lua"),
            "two distinct paths, one window name — the collision a digest avoids"
        );
        // Digested owners of different paths do not collide.
        assert_ne!(
            program_window_name("aaaa1111", "watch"),
            program_window_name("bbbb2222", "watch")
        );
    }
}
