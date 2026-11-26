use std::collections::HashMap;

pub use crate::contract::Located;
use crate::contract::{BackendLiveness, DiscoveredSession, WindowRole};

pub(crate) const WINDOW_PREFIX: &str = "tb-";

pub(crate) const SHELL_WINDOW_PREFIX: &str = "tbs-";

pub fn sanitize_window_name(name: &str) -> String {
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

pub(crate) fn agent_window_name(session_name: &str) -> String {
    format!("{WINDOW_PREFIX}{}", sanitize_window_name(session_name))
}

pub(crate) fn shell_window_name(session_name: &str) -> String {
    format!(
        "{SHELL_WINDOW_PREFIX}{}",
        sanitize_window_name(session_name)
    )
}

pub(crate) const PROGRAM_WINDOW_PREFIX: &str = "tbp-";

pub fn program_window_name(owner: &str, pane: &str) -> String {
    format!(
        "{PROGRAM_WINDOW_PREFIX}{}-{}",
        sanitize_window_name(owner),
        sanitize_window_name(pane)
    )
}

pub fn window_name_for(role: WindowRole, session_name: &str) -> String {
    match role {
        WindowRole::Shell => shell_window_name(session_name),
        _ => agent_window_name(session_name),
    }
}

impl WindowRole {
    pub(crate) fn from_window_name(name: &str) -> Option<Self> {
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

#[derive(Clone, Debug)]
struct ListedWindow {
    pane: String,
    session: String,
    alive: bool,
}

#[derive(Clone, Debug, Default)]
pub struct WindowIndex {
    stamped: HashMap<(String, WindowRole), Vec<ListedWindow>>,
    by_name: HashMap<String, Vec<ListedWindow>>,
    by_pane: HashMap<String, (String, String, WindowRole)>,
}

impl WindowIndex {
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

    pub fn live_agent_window(&self, session_id: &str, session_name: &str) -> Located {
        self.locate(session_id, session_name, WindowRole::Agent, true)
    }

    pub fn shell_window(&self, session_id: &str, session_name: &str) -> Located {
        self.locate(session_id, session_name, WindowRole::Shell, false)
    }

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

    pub fn is_stamped(&self, pane: &str) -> bool {
        self.by_pane
            .get(pane)
            .is_some_and(|(_, stamp, _)| !stamp.is_empty())
    }

    pub(crate) fn locate(
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
            [_, _, ..] => Some(Located::Unknown),
            [only] => Some(if !live_only || only.alive {
                Located::At(only.pane.clone())
            } else {
                Located::Absent
            }),
        }
    }

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
            [only] if session_id.is_empty() || only.session.is_empty() => {
                Located::At(only.pane.clone())
            }
            [_] => Located::Absent,
            _ if !session_id.is_empty() && named.iter().all(|w| !w.session.is_empty()) => {
                Located::Absent
            }
            _ => Located::Unknown,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
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
        // tmux re-splits a target that holds spaces into separate words.
        assert_eq!(sanitize_window_name("Foo Bar"), "Foo_Bar");
    }

    #[test]
    fn sanitize_window_name_replaces_tmux_delimiters() {
        // Colons, dots and commas have meaning inside tmux target strings.
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

    #[test]
    fn a_program_window_is_named_deterministically_and_apart_from_sessions() {
        let once = program_window_name("abcd1234", "watch");
        assert_eq!(once, "tbp-abcd1234-watch");
        assert_eq!(
            once,
            program_window_name("abcd1234", "watch"),
            "deterministic"
        );

        assert!(once.starts_with(PROGRAM_WINDOW_PREFIX));
        assert!(!once.starts_with(&format!("{WINDOW_PREFIX}a")));
        assert!(!once.starts_with(SHELL_WINDOW_PREFIX));
    }

    pub(crate) fn listed(
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

    #[test]
    fn a_stamped_window_is_its_sessions_whatever_it_is_named() {
        let index =
            WindowIndex::from_listing([listed("%3", "tb-old_name", ONE, WindowRole::Agent)]);
        assert_eq!(
            index.agent_window(ONE, "new name"),
            Located::At("%3".into())
        );
    }

    #[test]
    fn a_namesakes_stamped_window_is_never_ours() {
        let index = WindowIndex::from_listing([listed("%7", "tb-fleet", TWO, WindowRole::Agent)]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Absent);
    }

    #[test]
    fn a_reissued_pane_id_cannot_make_a_namesakes_window_ours() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", TWO, WindowRole::Agent),
            listed("%2", "tb-other", ONE, WindowRole::Agent),
        ]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::At("%2".into()));
    }

    #[test]
    fn a_lone_unstamped_namesake_is_adoptable() {
        let index = WindowIndex::from_listing([listed("%4", "tb-fleet", "", WindowRole::Agent)]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::At("%4".into()));
        assert_eq!(index.agent_window("", "fleet"), Located::At("%4".into()));
    }

    #[test]
    fn unstamped_namesakes_are_unknown_rather_than_absent() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", "", WindowRole::Agent),
            listed("%2", "tb-fleet", "", WindowRole::Agent),
        ]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Unknown);
        assert!(!index.agent_window(ONE, "fleet").is_absent());
    }

    #[test]
    fn stamped_namesakes_that_are_all_someone_elses_read_as_absent() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", TWO, WindowRole::Agent),
            listed("%2", "tb-fleet", TWO, WindowRole::Agent),
        ]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Absent);
    }

    #[test]
    fn a_sessions_shell_window_is_not_its_agent() {
        let index = WindowIndex::from_listing([listed("%5", "tbs-fleet", ONE, WindowRole::Shell)]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Absent);
        assert_eq!(index.shell_window(ONE, "fleet"), Located::At("%5".into()));
    }

    #[test]
    fn a_dead_pane_is_still_the_sessions_window_but_not_a_live_one() {
        let index =
            WindowIndex::from_listing([dead(listed("%6", "tb-fleet", ONE, WindowRole::Agent))]);
        assert_eq!(index.agent_window(ONE, "fleet"), Located::At("%6".into()));
        assert_eq!(index.live_agent_window(ONE, "fleet"), Located::Absent);
        assert!(index.places_agent(ONE, "fleet", "%6"));
        assert!(!index.places_agent(ONE, "fleet", "%9"));
    }

    #[test]
    fn a_dead_own_window_does_not_fall_through_to_a_live_namesake() {
        let index = WindowIndex::from_listing([
            dead(listed("%6", "tb-fleet", ONE, WindowRole::Agent)),
            listed("%7", "tb-fleet", "", WindowRole::Agent),
        ]);
        assert_eq!(index.live_agent_window(ONE, "fleet"), Located::Absent);
    }

    #[test]
    fn a_remembered_pane_is_claimable_while_no_window_says_otherwise() {
        let index = WindowIndex::from_listing([
            listed("%1", "tb-fleet", "", WindowRole::Agent),
            listed("%2", "tb-fleet", "", WindowRole::Agent),
        ]);
        assert!(index.places_agent(ONE, "fleet", "%1"));
        assert!(index.places_agent(TWO, "fleet", "%2"));
        assert_eq!(index.agent_window(ONE, "fleet"), Located::Unknown);

        let stamped = WindowIndex::from_listing([listed("%1", "tb-fleet", TWO, WindowRole::Agent)]);
        assert!(!stamped.places_agent(ONE, "fleet", "%1"));
        assert!(stamped.places_agent(TWO, "fleet", "%1"));
        assert!(!stamped.places_agent(TWO, "fleet", "%9"));
    }

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

    #[test]
    fn sanitizing_a_path_would_collide_which_is_why_the_owner_is_digested() {
        assert_eq!(
            sanitize_window_name("mods/90_watch.lua"),
            sanitize_window_name("mods.90.watch.lua"),
            "two distinct paths, one window name — the collision a digest avoids"
        );
        assert_ne!(
            program_window_name("aaaa1111", "watch"),
            program_window_name("bbbb2222", "watch")
        );
    }
}
