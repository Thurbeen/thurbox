//! The multiplexer choice is independent of the machine that runs a session.

use super::{HostDef, Platform, Route};

/// A name accepted in settings, hosts, and per-create commands. Implementations
/// register separately; accepting a name here never claims its binary is usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Multiplexer {
    Tmux,
    Psmux,
    Rmux,
    Herdr,
}

impl Multiplexer {
    /// Every name, in the order a choice lists them. The one source of the
    /// multiplexer vocabulary: parsing, the choices offered and the route
    /// grammar all read it.
    pub const ALL: [Self; 4] = [Self::Tmux, Self::Psmux, Self::Rmux, Self::Herdr];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Psmux => "psmux",
            Self::Rmux => "rmux",
            Self::Herdr => "herdr",
        }
    }

    /// Whether this multiplexer runs natively on a machine of `platform`.
    /// psmux is a Windows program and tmux a POSIX one; a binary of either
    /// name found on the other kind of machine is not something thurbox can
    /// drive there.
    pub const fn runs_on(self, platform: Platform) -> bool {
        match self {
            Self::Tmux => matches!(platform, Platform::Posix),
            Self::Psmux => matches!(platform, Platform::Windows),
            Self::Rmux | Self::Herdr => true,
        }
    }

    /// The multiplexers a machine of `platform` offers, in [`Self::ALL`] order:
    /// those that run there and were `found` installed.
    ///
    /// `found` is `None` when nobody has looked — a host whose probe has not
    /// answered, or a native-Windows host, which has no `sh` to probe with.
    /// That offers what the machine was already trusted to run: its platform
    /// default and its configured `preference`, never an optional one nobody
    /// has seen.
    pub fn available_on(
        platform: Platform,
        found: Option<&[Self]>,
        preference: Option<Self>,
    ) -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|mux| match found {
                Some(found) => mux.runs_on(platform) && found.contains(mux),
                None => *mux == Self::default_for(platform) || Some(*mux) == preference,
            })
            .collect()
    }

    /// What a create uses when nothing names a multiplexer, among the
    /// `available` ones: the platform default when it is there, else the first
    /// one that is — a machine with only RMUX installed runs RMUX.
    pub fn preferred(platform: Platform, available: &[Self]) -> Option<Self> {
        let default = Self::default_for(platform);
        if available.contains(&default) {
            return Some(default);
        }
        available.first().copied()
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|mux| mux.name() == name)
            .ok_or_else(|| {
                let names: Vec<&str> = Self::ALL.iter().map(|mux| mux.name()).collect();
                format!("Unknown multiplexer '{name}'. Choose {}.", names.join(", "))
            })
    }

    /// What a machine of `platform` runs when nothing names a multiplexer: a
    /// default policy for the unqualified route, never a gate on which
    /// multiplexers a machine can be driven with.
    pub const fn default_for(platform: Platform) -> Self {
        match platform {
            Platform::Windows => Self::Psmux,
            Platform::Posix => Self::Tmux,
        }
    }

    /// The one thing to do when this multiplexer's binary is missing here.
    ///
    /// Never a package-manager line thurbox has not verified: where the command
    /// depends on a distribution, this names the package and links the
    /// project's own install page instead of guessing an invocation.
    pub fn install_hint(self) -> String {
        match self {
            Self::Psmux => {
                "install psmux, the Windows multiplexer: https://github.com/psmux/psmux".to_string()
            }
            Self::Tmux if cfg!(target_os = "macos") => {
                "install tmux 3.2 or newer (`brew install tmux`), or see \
                 https://github.com/tmux/tmux/wiki/Installing"
                    .to_string()
            }
            Self::Tmux => "install tmux 3.2 or newer — the package is called `tmux` on every \
                           major distribution; see https://github.com/tmux/tmux/wiki/Installing"
                .to_string(),
            Self::Rmux | Self::Herdr => format!("install {}", self.name()),
        }
    }

    /// [`Self::default_for`] this machine.
    pub fn platform_default() -> Self {
        Self::default_for(Platform::local())
    }
}

/// What a create will use, before any worktree or pane is made. The host name
/// stays separate from the multiplexer choice in both the API and the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendChoice {
    pub host: Option<HostDef>,
    pub multiplexer: Multiplexer,
    /// Where the session will run. Always qualified: a new row names its
    /// multiplexer, so a later change of preference cannot reinterpret it.
    pub route: Route,
}

impl BackendChoice {
    pub fn resolve(
        host: Option<HostDef>,
        explicit: Option<&str>,
        local_default: Option<&str>,
    ) -> Result<Self, String> {
        let unqualified = match &host {
            Some(host) => host.route(None),
            None => Route::local(None),
        };
        let configured = match &host {
            Some(host) => host.multiplexer.as_deref(),
            None => local_default,
        };
        let multiplexer = match explicit.or(configured).unwrap_or("default") {
            // What an unqualified route means here — the one rule
            // `Route::multiplexer` holds for rows written before routes did.
            "default" => unqualified.multiplexer(
                Multiplexer::platform_default(),
                host.as_ref().and_then(HostDef::multiplexer),
            ),
            name => Multiplexer::parse(name)?,
        };
        Ok(Self {
            route: unqualified.with_mux(multiplexer),
            host,
            multiplexer,
        })
    }

    /// The key the row is written under.
    pub fn backend_type(&self) -> String {
        self.route.format()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_beats_host_or_local_defaults() {
        let host = HostDef {
            name: "example".into(),
            multiplexer: Some("rmux".into()),
            ..Default::default()
        };
        let chosen = BackendChoice::resolve(Some(host), Some("herdr"), None).unwrap();
        assert_eq!(chosen.backend_type(), "ssh:example:herdr");
        assert_eq!(chosen.multiplexer, Multiplexer::Herdr);
        let local = BackendChoice::resolve(None, Some("herdr"), Some("rmux")).unwrap();
        assert_eq!(local.backend_type(), "local:herdr");
    }

    #[test]
    fn a_new_row_names_its_multiplexer() {
        let host = HostDef {
            name: "example".into(),
            ..Default::default()
        };
        let chosen = BackendChoice::resolve(Some(host), None, None).unwrap();
        assert_eq!(chosen.backend_type(), "ssh:example:tmux");
        let local = BackendChoice::resolve(None, None, None).unwrap();
        assert_eq!(
            local.route,
            Route::local(Some(Multiplexer::platform_default()))
        );
        assert_eq!(
            local.backend_type(),
            format!("local:{}", Multiplexer::platform_default().name())
        );
    }

    #[test]
    fn wsl_uses_its_host_preference_and_explicit_choice_wins() {
        let mut host = HostDef::wsl("example");
        host.multiplexer = Some("rmux".into());
        let configured = BackendChoice::resolve(Some(host.clone()), None, None).unwrap();
        assert_eq!(configured.backend_type(), "wsl:example:rmux");
        let overridden = BackendChoice::resolve(Some(host), Some("tmux"), None).unwrap();
        assert_eq!(overridden.backend_type(), "wsl:example:tmux");
    }

    #[test]
    fn every_name_parses_back_to_its_multiplexer() {
        for mux in Multiplexer::ALL {
            assert_eq!(Multiplexer::parse(mux.name()), Ok(mux));
        }
        let refused = Multiplexer::parse("screen").unwrap_err();
        for mux in Multiplexer::ALL {
            assert!(refused.contains(mux.name()), "{refused}");
        }
    }
}

#[cfg(test)]
mod availability_tests {
    use super::*;
    use Multiplexer::{Psmux, Rmux, Tmux};

    #[test]
    fn only_what_runs_on_the_platform_and_was_found_is_offered() {
        let found = [Tmux, Psmux, Rmux];
        assert_eq!(
            Multiplexer::available_on(Platform::Posix, Some(&found), None),
            vec![Tmux, Rmux],
            "psmux is native Windows, whatever a POSIX PATH holds"
        );
        assert_eq!(
            Multiplexer::available_on(Platform::Windows, Some(&found), None),
            vec![Psmux, Rmux]
        );
        assert_eq!(
            Multiplexer::available_on(Platform::Posix, Some(&[Rmux]), None),
            vec![Rmux]
        );
        assert!(Multiplexer::available_on(Platform::Posix, Some(&[]), None).is_empty());
    }

    #[test]
    fn an_unlooked_machine_offers_its_default_and_its_preference_only() {
        assert_eq!(
            Multiplexer::available_on(Platform::Posix, None, None),
            vec![Tmux]
        );
        assert_eq!(
            Multiplexer::available_on(Platform::Windows, None, None),
            vec![Psmux]
        );
        assert_eq!(
            Multiplexer::available_on(Platform::Posix, None, Some(Rmux)),
            vec![Tmux, Rmux]
        );
    }

    #[test]
    fn the_preferred_one_is_the_platform_default_when_it_is_available() {
        assert_eq!(
            Multiplexer::preferred(Platform::Posix, &[Tmux, Rmux]),
            Some(Tmux)
        );
        assert_eq!(Multiplexer::preferred(Platform::Posix, &[Rmux]), Some(Rmux));
        assert_eq!(
            Multiplexer::preferred(Platform::Windows, &[Psmux, Rmux]),
            Some(Psmux)
        );
        assert_eq!(Multiplexer::preferred(Platform::Posix, &[]), None);
    }
}
