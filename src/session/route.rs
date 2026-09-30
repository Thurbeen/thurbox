//! Where a session runs, as `sessions.backend_type` records it: the one parser
//! and the one formatter for every spelling a row has ever carried.
//!
//! A route has two independent halves. The **place** is the machine and how it
//! is reached (this one, an ssh host, a WSL distro); the **multiplexer** is
//! which implementation serves the session there. Neither implies the other,
//! and neither says what OS the machine runs — that is the host's own
//! configuration, never something read off a route.
//!
//! The grammar, old spellings first:
//!
//! | key | place | multiplexer |
//! |---|---|---|
//! | `""`, `tmux`, `local-tmux` | local | unqualified |
//! | `local-<mux>` (any other name; legacy) | local | `<mux>` |
//! | `local:<mux>` | local | `<mux>` |
//! | `ssh:<host>` / `wsl:<host>` | remote | unqualified |
//! | `ssh:<host>:<mux>` / `wsl:<host>:<mux>` | remote | `<mux>` |
//!
//! An **unqualified** multiplexer is what every row written before routes
//! carried one means, and it keeps that meaning: [`Route::multiplexer`] settles
//! it the way those rows were always read. Nothing is migrated. New rows are
//! written qualified, so a later change of preference cannot reinterpret them.
//!
//! Every machine is qualified the same way, `<machine>:<mux>`, and that is
//! what new rows are written as. The `local-<mux>` spellings are read only:
//! `local-tmux` was written for the platform's own multiplexer before routes
//! carried one — which on Windows is psmux — so it parses unqualified and keeps
//! that meaning, while an explicit local tmux is `local:tmux` and cannot be
//! mistaken for it.

use std::fmt;

use super::Multiplexer;

/// The prefix of a key whose machine is reached over ssh.
pub const SSH_PREFIX: &str = "ssh:";
/// The prefix of a key whose machine is a WSL distro.
pub const WSL_PREFIX: &str = "wsl:";
/// The prefix of a local key that names its multiplexer.
pub const LOCAL_PREFIX: &str = "local:";
/// The prefix local keys carried before [`LOCAL_PREFIX`]: read, and written
/// only as the one spelling of an unqualified local route.
const LEGACY_LOCAL_PREFIX: &str = "local-";

/// How a remote machine is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Via {
    Ssh,
    Wsl,
}

impl Via {
    fn prefix(self) -> &'static str {
        match self {
            Self::Ssh => SSH_PREFIX,
            Self::Wsl => WSL_PREFIX,
        }
    }
}

/// The machine a session runs on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Place {
    Local,
    Remote { via: Via, host: String },
}

/// A parsed `backend_type`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Route {
    pub place: Place,
    /// `None` for a key written before routes named their multiplexer.
    pub mux: Option<Multiplexer>,
}

/// A key that names no route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// `ssh:` or `wsl:` with no host after it.
    NoHost(String),
    /// Neither a local spelling nor a remote prefix, or a `local:`/`local-`
    /// suffix no
    /// multiplexer is called.
    Unknown(String),
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHost(key) => write!(f, "backend '{key}' names no host"),
            Self::Unknown(key) => write!(f, "no backend is called '{key}'"),
        }
    }
}

impl std::error::Error for RouteError {}

impl Route {
    pub const fn local(mux: Option<Multiplexer>) -> Self {
        Self {
            place: Place::Local,
            mux,
        }
    }

    pub fn remote(via: Via, host: impl Into<String>, mux: Option<Multiplexer>) -> Self {
        Self {
            place: Place::Remote {
                via,
                host: host.into(),
            },
            mux,
        }
    }

    pub fn parse(key: &str) -> Result<Self, RouteError> {
        for via in [Via::Ssh, Via::Wsl] {
            let Some(rest) = key.strip_prefix(via.prefix()) else {
                continue;
            };
            // A host name holds no `:` (`hosts.toml` refuses one), so a known
            // multiplexer after the last one is the route's. Anything else is
            // kept as the host: a row written for a host that cannot load now,
            // which then resolves to nothing rather than to a guess.
            let (host, mux) = match rest.rsplit_once(':') {
                Some((host, name)) => match Multiplexer::parse(name) {
                    Ok(mux) => (host, Some(mux)),
                    Err(_) => (rest, None),
                },
                None => (rest, None),
            };
            if host.is_empty() {
                return Err(RouteError::NoHost(key.to_string()));
            }
            return Ok(Self::remote(via, host, mux));
        }
        match key {
            "" | "tmux" | "local-tmux" => Ok(Self::local(None)),
            _ => key
                .strip_prefix(LOCAL_PREFIX)
                .or_else(|| key.strip_prefix(LEGACY_LOCAL_PREFIX))
                .and_then(|name| Multiplexer::parse(name).ok())
                .map(|mux| Self::local(Some(mux)))
                .ok_or_else(|| RouteError::Unknown(key.to_string())),
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self.place, Place::Remote { .. })
    }

    /// Whether `key` is a remote route. A key naming no route is not one.
    pub fn is_remote_key(key: &str) -> bool {
        Self::parse(key).is_ok_and(|route| route.is_remote())
    }

    pub fn is_local(&self) -> bool {
        self.place == Place::Local
    }

    pub fn via(&self) -> Option<Via> {
        match &self.place {
            Place::Local => None,
            Place::Remote { via, .. } => Some(*via),
        }
    }

    /// The host's bare name — `ssh:devbox:rmux` → `devbox` — or `None` locally.
    pub fn host(&self) -> Option<&str> {
        match &self.place {
            Place::Local => None,
            Place::Remote { host, .. } => Some(host),
        }
    }

    /// Whether two routes are on one machine, whatever serves each.
    pub fn same_machine(&self, other: &Self) -> bool {
        self.place == other.place
    }

    /// The same machine served by `mux`.
    pub fn with_mux(&self, mux: Multiplexer) -> Self {
        Self {
            place: self.place.clone(),
            mux: Some(mux),
        }
    }

    /// The multiplexer this route is served by: its own when it names one,
    /// else what an unqualified key has always meant — this machine's platform
    /// default (`local_default`), or the host's configured multiplexer
    /// (`host_preference`) when that is psmux and tmux otherwise.
    ///
    /// That last rule is the historical one and it matters: a legacy `ssh:box`
    /// row was created on tmux (or psmux), so a host whose preference later
    /// became rmux does not turn that row into an rmux session.
    pub fn multiplexer(
        &self,
        local_default: Multiplexer,
        host_preference: Option<Multiplexer>,
    ) -> Multiplexer {
        if let Some(mux) = self.mux {
            return mux;
        }
        match self.place {
            Place::Local => local_default,
            Place::Remote { .. } => match host_preference {
                Some(Multiplexer::Psmux) => Multiplexer::Psmux,
                _ => Multiplexer::Tmux,
            },
        }
    }

    /// This route with its multiplexer settled by [`Self::multiplexer`].
    pub fn qualify(
        &self,
        local_default: Multiplexer,
        host_preference: Option<Multiplexer>,
    ) -> Self {
        self.with_mux(self.multiplexer(local_default, host_preference))
    }

    /// The key this route is written as. An unqualified local route is
    /// written the one way such rows ever were, `local-tmux`.
    pub fn format(&self) -> String {
        let suffix = self.mux.map(Multiplexer::name);
        match (&self.place, suffix) {
            (Place::Local, None) => format!("{LEGACY_LOCAL_PREFIX}{}", Multiplexer::Tmux.name()),
            (Place::Local, Some(name)) => format!("{LOCAL_PREFIX}{name}"),
            (Place::Remote { via, host }, None) => format!("{}{host}", via.prefix()),
            (Place::Remote { via, host }, Some(name)) => format!("{}{host}:{name}", via.prefix()),
        }
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format())
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn every_legacy_local_spelling_is_the_unqualified_local_route() {
        for key in ["", "tmux", "local-tmux"] {
            assert_eq!(Route::parse(key), Ok(Route::local(None)), "{key:?}");
        }
    }

    #[test]
    fn a_local_key_names_its_multiplexer() {
        for mux in Multiplexer::ALL {
            let key = format!("local:{}", mux.name());
            assert_eq!(Route::parse(&key), Ok(Route::local(Some(mux))), "{key}");
            assert_eq!(Route::local(Some(mux)).format(), key);
        }
    }

    /// The spellings written before `local:` still read as they were written:
    /// `local-tmux` is the platform default, any other `local-<mux>` that mux.
    #[test]
    fn a_legacy_local_key_keeps_its_meaning() {
        for mux in Multiplexer::ALL {
            let key = format!("local-{}", mux.name());
            let expected = match mux {
                Multiplexer::Tmux => Route::local(None),
                _ => Route::local(Some(mux)),
            };
            assert_eq!(Route::parse(&key), Ok(expected), "{key}");
        }
        // An explicit local tmux is not the legacy default.
        assert_ne!(Route::local(Some(Multiplexer::Tmux)).format(), "local-tmux");
    }

    #[test]
    fn a_remote_key_splits_host_from_multiplexer() {
        assert_eq!(
            Route::parse("ssh:h"),
            Ok(Route::remote(Via::Ssh, "h", None))
        );
        assert_eq!(
            Route::parse("wsl:Ubuntu"),
            Ok(Route::remote(Via::Wsl, "Ubuntu", None))
        );
        for mux in Multiplexer::ALL {
            for via in [Via::Ssh, Via::Wsl] {
                let key = format!("{}h:{}", via.prefix(), mux.name());
                let route = Route::parse(&key).unwrap();
                assert_eq!(route, Route::remote(via, "h", Some(mux)), "{key}");
                assert_eq!(route.host(), Some("h"), "{key}");
            }
        }
    }

    #[test]
    fn keys_that_name_no_route_are_refused_not_guessed() {
        for key in [
            "local-probe",
            "local:probe",
            "local-",
            "local:",
            "weird",
            "ssh:",
            "wsl:",
            "ssh::tmux",
        ] {
            assert!(Route::parse(key).is_err(), "{key:?} must not parse");
        }
    }

    #[test]
    fn an_unknown_suffix_stays_part_of_a_host_that_cannot_load() {
        // `hosts.toml` refuses a `:` in a name, so this host resolves to
        // nothing — the row reads as unreachable, never as some other host.
        assert_eq!(
            Route::parse("ssh:box:screen"),
            Ok(Route::remote(Via::Ssh, "box:screen", None))
        );
    }

    #[test]
    fn an_unqualified_remote_keeps_the_meaning_it_was_written_with() {
        let unqualified = Route::parse("ssh:box").unwrap();
        let cases = [
            (None, Multiplexer::Tmux),
            (Some("tmux"), Multiplexer::Tmux),
            (Some("psmux"), Multiplexer::Psmux),
            // Written before either existed, so written for tmux.
            (Some("rmux"), Multiplexer::Tmux),
            (Some("herdr"), Multiplexer::Tmux),
        ];
        for (preference, expected) in cases {
            for local in Multiplexer::ALL {
                assert_eq!(
                    unqualified
                        .multiplexer(local, preference.map(|p| Multiplexer::parse(p).unwrap())),
                    expected,
                    "host preference {preference:?}, local default {local:?}"
                );
            }
        }
        assert_eq!(
            unqualified.multiplexer(Multiplexer::Psmux, None),
            Multiplexer::Tmux
        );
    }

    #[test]
    fn an_unqualified_local_route_is_the_platform_default() {
        for local in Multiplexer::ALL {
            assert_eq!(Route::local(None).multiplexer(local, None), local);
        }
    }

    #[test]
    fn a_qualified_route_ignores_every_default() {
        for mux in Multiplexer::ALL {
            for local in Multiplexer::ALL {
                for preference in std::iter::once(None).chain(Multiplexer::ALL.map(Some)) {
                    assert_eq!(
                        Route::remote(Via::Ssh, "box", Some(mux)).multiplexer(local, preference),
                        mux
                    );
                    assert_eq!(Route::local(Some(mux)).multiplexer(local, preference), mux);
                }
            }
        }
    }

    fn place() -> impl Strategy<Value = Place> {
        prop_oneof![
            Just(Place::Local),
            (
                "[A-Za-z0-9._-]{1,12}",
                prop_oneof![Just(Via::Ssh), Just(Via::Wsl)]
            )
                .prop_map(|(host, via)| Place::Remote { via, host }),
        ]
    }

    fn mux() -> impl Strategy<Value = Multiplexer> {
        proptest::sample::select(Multiplexer::ALL.to_vec())
    }

    fn route() -> impl Strategy<Value = Route> {
        (place(), proptest::option::of(mux())).prop_map(|(place, mux)| Route { place, mux })
    }

    proptest! {
        /// Every route reads back as itself.
        #[test]
        fn a_route_round_trips_through_its_key(route in route()) {
            prop_assert_eq!(Route::parse(&route.format()).unwrap(), route);
        }

        /// Formatting a parsed key gives one canonical spelling, which parses
        /// to the same route: aliases converge and nothing drifts.
        #[test]
        fn a_key_settles_on_one_canonical_spelling(
            key in prop_oneof![
                Just(String::new()),
                Just("tmux".to_string()),
                Just("local-tmux".to_string()),
                mux().prop_map(|mux| format!("local-{}", mux.name())),
                route().prop_map(|r| r.format()),
            ]
        ) {
            let parsed = Route::parse(&key).unwrap();
            let canonical = parsed.format();
            prop_assert_eq!(&Route::parse(&canonical).unwrap(), &parsed);
            prop_assert_eq!(Route::parse(&canonical).unwrap().format(), canonical);
        }

        /// Place and multiplexer are independent: any multiplexer can be named
        /// on any machine, and naming one changes nothing about the machine.
        #[test]
        fn place_and_multiplexer_vary_independently(place in place(), a in mux(), b in mux()) {
            let one = Route { place: place.clone(), mux: Some(a) };
            let two = Route { place, mux: Some(b) };
            prop_assert!(one.same_machine(&two));
            let back = Route::parse(&one.format()).unwrap();
            prop_assert_eq!(back.host(), two.host());
            prop_assert_eq!(back.via(), two.via());
        }
    }
}
