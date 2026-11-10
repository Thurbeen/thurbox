//! Where a session runs: the one parser and the one formatter of a route key.
//!
//! A route has two independent halves. The **place** is the machine and how it
//! is reached (this one, an ssh host, a WSL distro); the **multiplexer** is
//! which implementation serves the session there. Neither implies the other,
//! and neither says what OS the machine runs — that is the host's own
//! configuration, never something read off a route.
//!
//! | key | place | multiplexer |
//! |---|---|---|
//! | `local:<mux>` | local | `<mux>` |
//! | `ssh:<host>:<mux>` / `wsl:<host>:<mux>` | remote | `<mux>` |
//!
//! A key always names its multiplexer. A **host key** — `ssh:<host>` or
//! `wsl:<host>`, what the host picker carries — names a machine only; its
//! multiplexer is the host's own, which `hosts.toml` holds
//! ([`HostRegistry::resolve`](super::HostRegistry::resolve)).

use std::fmt;

use super::Multiplexer;

/// The prefix of a key whose machine is reached over ssh.
pub const SSH_PREFIX: &str = "ssh:";
/// The prefix of a key whose machine is a WSL distro.
pub const WSL_PREFIX: &str = "wsl:";
/// The prefix of a local key.
pub const LOCAL_PREFIX: &str = "local:";

/// How a remote machine is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Via {
    Ssh,
    Wsl,
}

impl Via {
    pub fn prefix(self) -> &'static str {
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

/// A parsed route key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Route {
    pub place: Place,
    pub mux: Multiplexer,
}

/// A key that names no route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// `ssh:` or `wsl:` with no host after it.
    NoHost(String),
    /// No place prefix, or no multiplexer known by the name after it.
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

/// The host a host key or a route key names — `ssh:devbox` and
/// `ssh:devbox:rmux` both name `devbox` — or `None` for any other key.
pub fn host_of_key(key: &str) -> Option<&str> {
    let rest = [SSH_PREFIX, WSL_PREFIX]
        .iter()
        .find_map(|prefix| key.strip_prefix(prefix))?;
    // A host name holds no `:` (`hosts.toml` refuses one), so a known
    // multiplexer after the last one is the route's.
    let host = match rest.rsplit_once(':') {
        Some((host, name)) if Multiplexer::parse(name).is_ok() => host,
        _ => rest,
    };
    (!host.is_empty() && !host.contains(':')).then_some(host)
}

impl Route {
    pub const fn local(mux: Multiplexer) -> Self {
        Self {
            place: Place::Local,
            mux,
        }
    }

    pub fn remote(via: Via, host: impl Into<String>, mux: Multiplexer) -> Self {
        Self {
            place: Place::Remote {
                via,
                host: host.into(),
            },
            mux,
        }
    }

    pub fn parse(key: &str) -> Result<Self, RouteError> {
        let unknown = || RouteError::Unknown(key.to_string());
        if let Some(name) = key.strip_prefix(LOCAL_PREFIX) {
            return Multiplexer::parse(name)
                .map(Self::local)
                .map_err(|_| unknown());
        }
        for via in [Via::Ssh, Via::Wsl] {
            let Some(rest) = key.strip_prefix(via.prefix()) else {
                continue;
            };
            let (host, name) = rest.rsplit_once(':').ok_or_else(unknown)?;
            let mux = Multiplexer::parse(name).map_err(|_| unknown())?;
            if host.is_empty() {
                return Err(RouteError::NoHost(key.to_string()));
            }
            return Ok(Self::remote(via, host, mux));
        }
        Err(unknown())
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

    /// Whether this route is tmux on this machine.
    pub fn is_local_tmux(&self) -> bool {
        self.is_local() && self.mux == Multiplexer::Tmux
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
            mux,
        }
    }

    /// The key this route is written as.
    pub fn format(&self) -> String {
        let name = self.mux.name();
        match &self.place {
            Place::Local => format!("{LOCAL_PREFIX}{name}"),
            Place::Remote { via, host } => format!("{}{host}:{name}", via.prefix()),
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
    fn a_key_that_names_no_multiplexer_is_refused() {
        for key in [
            "",
            "tmux",
            "local-tmux",
            "local-psmux",
            "local-rmux",
            "ssh:box",
            "wsl:Ubuntu",
        ] {
            assert!(Route::parse(key).is_err(), "{key:?} must not parse");
        }
    }

    #[test]
    fn every_route_is_written_with_its_multiplexer() {
        for mux in Multiplexer::ALL {
            let name = mux.name();
            for (route, key) in [
                (Route::local(mux), format!("local:{name}")),
                (
                    Route::remote(Via::Ssh, "box", mux),
                    format!("ssh:box:{name}"),
                ),
                (
                    Route::remote(Via::Wsl, "box", mux),
                    format!("wsl:box:{name}"),
                ),
            ] {
                assert_eq!(route.format(), key);
                assert_eq!(Route::parse(&key), Ok(route), "{key}");
            }
        }
    }

    #[test]
    fn keys_that_name_no_route_are_refused_not_guessed() {
        for key in [
            "local:probe",
            "local:",
            "weird",
            "ssh:",
            "wsl:",
            "ssh::tmux",
            "ssh:box:screen",
        ] {
            assert!(Route::parse(key).is_err(), "{key:?} must not parse");
        }
    }

    #[test]
    fn a_host_key_and_a_route_name_the_same_host() {
        assert_eq!(host_of_key("ssh:devbox"), Some("devbox"));
        assert_eq!(host_of_key("ssh:devbox:rmux"), Some("devbox"));
        assert_eq!(host_of_key("wsl:Ubuntu"), Some("Ubuntu"));
        for key in [
            "",
            "devbox",
            "local:tmux",
            "ssh:",
            "ssh::tmux",
            "ssh:box:screen",
        ] {
            assert_eq!(host_of_key(key), None, "{key:?}");
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
        (place(), mux()).prop_map(|(place, mux)| Route { place, mux })
    }

    proptest! {
        /// Every route reads back as itself.
        #[test]
        fn a_route_round_trips_through_its_key(route in route()) {
            prop_assert_eq!(Route::parse(&route.format()).unwrap(), route);
        }

        /// Place and multiplexer are independent: any multiplexer can be named
        /// on any machine, and naming one changes nothing about the machine.
        #[test]
        fn place_and_multiplexer_vary_independently(place in place(), a in mux(), b in mux()) {
            let one = Route { place: place.clone(), mux: a };
            let two = Route { place, mux: b };
            prop_assert!(one.same_machine(&two));
            let back = Route::parse(&one.format()).unwrap();
            prop_assert_eq!(back.host(), two.host());
            prop_assert_eq!(back.via(), two.via());
        }
    }
}
