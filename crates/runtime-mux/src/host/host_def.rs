use serde::{Deserialize, Serialize};

use super::{Multiplexer, Platform, Route, Via};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HostKind {
    #[default]
    Ssh,
    Wsl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDef {
    pub name: String,
    #[serde(default)]
    pub kind: HostKind,
    #[serde(default)]
    pub destination: String,
    #[serde(default)]
    pub distro: Option<String>,
    #[serde(default)]
    pub socket: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub ssh_opts: Vec<String>,
    #[serde(default)]
    pub worktrees_dir: Option<String>,
    #[serde(default)]
    pub multiplexer: Option<String>,
    #[serde(default)]
    pub platform: Option<Platform>,
    #[serde(default = "default_share_sessions")]
    pub share_sessions: bool,
    #[serde(default)]
    pub path_prepend: Vec<String>,
}

fn default_share_sessions() -> bool {
    true
}

// By hand rather than derived: a derived `Default` would make `share_sessions`
// `false`, and every `HostDef { name, .. Default::default() }` — the tests, the
// WSL constructor — would silently opt its host out of sharing.
impl Default for HostDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: HostKind::default(),
            destination: String::new(),
            distro: None,
            socket: None,
            session: None,
            ssh_opts: Vec::new(),
            worktrees_dir: None,
            multiplexer: None,
            platform: None,
            share_sessions: true,
            path_prepend: Vec::new(),
        }
    }
}

impl HostDef {
    pub fn wsl(distro: impl Into<String>) -> Self {
        let distro = distro.into();
        Self {
            name: distro.clone(),
            kind: HostKind::Wsl,
            distro: Some(distro),
            share_sessions: true,
            ..Self::default()
        }
    }

    pub fn shareable(&self) -> bool {
        self.share_sessions
    }

    pub fn is_wsl(&self) -> bool {
        self.kind == HostKind::Wsl
    }

    pub fn distro_name(&self) -> String {
        self.distro.clone().unwrap_or_else(|| self.name.clone())
    }

    pub fn backend_name(&self) -> String {
        format!("{}{}", self.via().prefix(), self.name)
    }

    fn via(&self) -> Via {
        if self.is_wsl() {
            Via::Wsl
        } else {
            Via::Ssh
        }
    }

    pub fn picker_detail(&self) -> String {
        match self.kind {
            HostKind::Ssh => self.destination.clone(),
            HostKind::Wsl => "WSL".to_string(),
        }
    }

    pub fn multiplexer(&self) -> Option<Multiplexer> {
        self.multiplexer
            .as_deref()
            .and_then(|name| Multiplexer::parse(name).ok())
    }

    pub fn mux(&self) -> String {
        self.multiplexer
            .clone()
            .unwrap_or_else(|| "tmux".to_string())
    }

    pub fn platform(&self) -> Platform {
        if self.is_wsl() {
            return Platform::Posix;
        }
        self.platform.unwrap_or(match self.multiplexer() {
            Some(Multiplexer::Psmux) => Platform::Windows,
            _ => Platform::Posix,
        })
    }

    pub fn is_windows(&self) -> bool {
        self.platform() == Platform::Windows
    }

    pub fn route(&self, mux: Multiplexer) -> Route {
        Route::remote(self.via(), self.name.clone(), mux)
    }

    pub fn default_route(&self) -> Route {
        self.route(self.multiplexer().unwrap_or(Multiplexer::Tmux))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRegistry {
    #[serde(default)]
    pub config_version: Option<u32>,
    #[serde(default)]
    pub hosts: Vec<HostDef>,
}

impl HostRegistry {
    pub fn get(&self, name: &str) -> Option<&HostDef> {
        self.hosts.iter().find(|h| h.name == name)
    }

    pub fn host_of(&self, route: &Route) -> Option<&HostDef> {
        self.get(route.host()?)
    }

    pub fn resolve(&self, key: &str) -> Option<&HostDef> {
        match super::route::host_of_key(key) {
            Some(host) => self.get(host),
            None => self.get(key),
        }
    }

    pub fn refuse_unroutable_names(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        self.hosts.retain(|host| {
            let routable = !host.name.contains(':');
            if !routable {
                warnings.push(format!(
                    "host '{}' is ignored: a host name may not contain ':', \
                     which separates it from the multiplexer in a session's route",
                    host.name
                ));
            }
            routable
        });
        warnings
    }

    pub fn names(&self) -> Vec<&str> {
        self.hosts.iter().map(|h| h.name.as_str()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> HostRegistry {
        HostRegistry {
            config_version: None,
            hosts: vec![
                HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                },
                HostDef::wsl("Ubuntu"),
                HostDef {
                    name: "winbox".into(),
                    destination: "me@winbox".into(),
                    multiplexer: Some("psmux".into()),
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn sharing_is_on_unless_the_entry_says_otherwise() {
        let on: HostDef = toml::from_str("name = \"devbox\"\ndestination = \"me@devbox\"").unwrap();
        assert!(on.shareable());
        assert!(HostDef::default().shareable());
        assert!(HostDef::wsl("Ubuntu").shareable());
        let off: HostDef = toml::from_str(
            "name = \"devbox\"\ndestination = \"me@devbox\"\nshare_sessions = false",
        )
        .unwrap();
        assert!(!off.shareable());
    }

    #[test]
    fn resolve_accepts_both_spellings_of_a_host() {
        // Which spelling a caller holds depends on where it came from, and
        // resolving only one of them refused every session the new-session flow
        // tried to create on a host ("Unknown host 'ssh:devbox'").
        let hosts = registry();
        for key in ["devbox", "ssh:devbox"] {
            assert_eq!(
                hosts.resolve(key).map(|h| h.name.as_str()),
                Some("devbox"),
                "{key} should resolve"
            );
        }
        for key in ["Ubuntu", "wsl:Ubuntu"] {
            assert_eq!(
                hosts.resolve(key).map(|h| h.name.as_str()),
                Some("Ubuntu"),
                "{key} should resolve"
            );
        }
    }

    #[test]
    fn resolve_does_not_invent_a_host() {
        let hosts = registry();
        for key in ["", "nope", "ssh:nope", "wsl:nope", "ssh:", "devbox2"] {
            assert!(hosts.resolve(key).is_none(), "{key} must not resolve");
        }
    }

    #[test]
    fn a_rows_multiplexer_never_decides_its_hosts_platform() {
        for configured in [
            None,
            Some("tmux"),
            Some("psmux"),
            Some("rmux"),
            Some("herdr"),
        ] {
            let host = HostDef {
                name: "box".into(),
                multiplexer: configured.map(str::to_string),
                ..Default::default()
            };
            let hosts = HostRegistry {
                config_version: None,
                hosts: vec![host.clone()],
            };
            for key in ["tmux", "psmux", "rmux", "herdr"].map(|mux| format!("ssh:box:{mux}")) {
                let route = Route::parse(&key).expect("a route");
                let resolved = hosts.host_of(&route).expect("box resolves");
                assert_eq!(
                    resolved.is_windows(),
                    host.is_windows(),
                    "{key} against a host configured with {configured:?}"
                );
            }
        }
    }

    #[test]
    fn route_host_os_launcher_and_multiplexer_are_independent() {
        use super::super::{BackendChoice, Platform};
        for kind in [HostKind::Ssh, HostKind::Wsl] {
            for platform in [None, Some(Platform::Posix), Some(Platform::Windows)] {
                for preference in std::iter::once(None).chain(Multiplexer::ALL.map(Some)) {
                    let host = HostDef {
                        name: "box".into(),
                        kind,
                        destination: "me@box".into(),
                        platform,
                        multiplexer: preference.map(|m| m.name().to_string()),
                        ..Default::default()
                    };
                    let os = match (kind, platform, preference) {
                        (HostKind::Wsl, _, _) => Platform::Posix,
                        (HostKind::Ssh, Some(os), _) => os,
                        (HostKind::Ssh, None, Some(Multiplexer::Psmux)) => Platform::Windows,
                        (HostKind::Ssh, None, _) => Platform::Posix,
                    };
                    let hosts = HostRegistry {
                        config_version: None,
                        hosts: vec![host.clone()],
                    };
                    for chosen in Multiplexer::ALL {
                        let case = format!("{kind:?}/{platform:?}/{preference:?} on {chosen:?}");
                        let choice =
                            BackendChoice::resolve(Some(host.clone()), Some(chosen.name()), None)
                                .unwrap();
                        assert_eq!(choice.multiplexer, chosen, "{case}");
                        assert_eq!(choice.route, host.route(chosen), "{case}");
                        let route = Route::parse(&choice.backend_type()).unwrap();
                        let via = match kind {
                            HostKind::Ssh => Via::Ssh,
                            HostKind::Wsl => Via::Wsl,
                        };
                        assert_eq!(route.via(), Some(via), "{case}");
                        assert_eq!(route.host(), Some("box"), "{case}");
                        assert_eq!(route.mux, chosen, "{case}");
                        let resolved = hosts.host_of(&route).expect("box resolves");
                        assert_eq!(resolved.platform(), os, "{case}");
                    }
                }
            }
        }
    }

    #[test]
    fn every_multiplexer_is_a_local_route_on_either_os() {
        use super::super::BackendChoice;
        for chosen in Multiplexer::ALL {
            let choice = BackendChoice::resolve(None, Some(chosen.name()), None).unwrap();
            assert_eq!(choice.route, Route::local(chosen));
            assert_eq!(choice.backend_type(), format!("local:{}", chosen.name()));
        }
    }

    #[test]
    fn a_legacy_entry_keeps_its_platform_and_a_declared_one_wins() {
        use super::super::Platform;
        let legacy: HostRegistry = toml::from_str(
            "[[hosts]]\nname = \"win\"\ndestination = \"me@win\"\nmultiplexer = \"psmux\"\n\n\
             [[hosts]]\nname = \"nix\"\ndestination = \"me@nix\"\n",
        )
        .unwrap();
        assert_eq!(legacy.get("win").unwrap().platform(), Platform::Windows);
        assert_eq!(legacy.get("nix").unwrap().platform(), Platform::Posix);

        let declared: HostRegistry = toml::from_str(
            "[[hosts]]\nname = \"win\"\ndestination = \"me@win\"\nplatform = \"windows\"\nmultiplexer = \"tmux\"\n\n\
             [[hosts]]\nname = \"odd\"\ndestination = \"me@odd\"\nplatform = \"posix\"\nmultiplexer = \"psmux\"\n\n\
             [[hosts]]\nname = \"distro\"\nkind = \"wsl\"\nplatform = \"windows\"\n",
        )
        .unwrap();
        assert_eq!(declared.get("win").unwrap().platform(), Platform::Windows);
        assert_eq!(declared.get("odd").unwrap().platform(), Platform::Posix);
        assert_eq!(declared.get("distro").unwrap().platform(), Platform::Posix);
        assert!(toml::from_str::<HostRegistry>(
            "[[hosts]]\nname = \"x\"\ndestination = \"d\"\nplatform = \"beos\"\n"
        )
        .is_err());
    }

    #[test]
    fn the_backend_prefix_is_not_checked_against_the_hosts_kind() {
        // A name is unique across kinds, so the prefix carries no information
        // the name does not. Asserted so the leniency is a decision, not an
        // accident.
        let hosts = registry();
        assert_eq!(
            hosts.resolve("ssh:Ubuntu").map(|h| h.kind),
            Some(HostKind::Wsl)
        );
        assert_eq!(
            hosts.resolve("wsl:devbox").map(|h| h.kind),
            Some(HostKind::Ssh)
        );
    }

    #[test]
    fn is_windows_is_declared_by_the_multiplexer_alone() {
        let hosts = registry();
        assert!(hosts.resolve("winbox").expect("winbox").is_windows());
        // Every other shape is POSIX: the default, an explicit `tmux`, and a
        // WSL distro (Linux inside, whatever machine hosts it).
        assert!(!hosts.resolve("devbox").expect("devbox").is_windows());
        assert!(!hosts.resolve("Ubuntu").expect("Ubuntu").is_windows());
        let explicit_tmux = HostDef {
            name: "box".into(),
            multiplexer: Some("tmux".into()),
            ..Default::default()
        };
        assert!(!explicit_tmux.is_windows());
    }

    #[test]
    fn backend_name_prefixes_with_ssh() {
        let h = HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        assert_eq!(h.backend_name(), "ssh:devbox");
    }

    #[test]
    fn wsl_constructor_and_backend_name() {
        let h = HostDef::wsl("Ubuntu");
        assert!(h.is_wsl());
        assert_eq!(h.kind, HostKind::Wsl);
        assert_eq!(h.distro_name(), "Ubuntu");
        assert_eq!(h.backend_name(), "wsl:Ubuntu");
        assert_eq!(h.picker_detail(), "WSL");
        // WSL distros run `tmux` inside the distro, same as a Unix SSH host.
        assert_eq!(h.mux(), "tmux");

        // An SSH host (the default kind) keeps the ssh prefix and is not WSL.
        let ssh = HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        assert!(!ssh.is_wsl());
        assert_eq!(ssh.picker_detail(), "me@devbox");
    }

    #[test]
    fn distro_name_falls_back_to_host_name() {
        let h = HostDef {
            name: "work".into(),
            kind: HostKind::Wsl,
            distro: None,
            ..Default::default()
        };
        assert_eq!(h.distro_name(), "work");
    }

    #[test]
    fn registry_lookup_by_name_and_backend() {
        let reg = HostRegistry {
            config_version: None,
            hosts: vec![
                HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                },
                HostDef::wsl("Ubuntu"),
            ],
        };
        assert_eq!(reg.get("devbox").unwrap().destination, "me@devbox");
        let host_of = |key: &str| {
            reg.host_of(&Route::parse(key).expect("a route"))
                .map(|h| h.name.as_str())
        };
        assert_eq!(host_of("ssh:devbox:tmux"), Some("devbox"));
        assert_eq!(host_of("ssh:devbox:psmux"), Some("devbox"));
        assert_eq!(host_of("wsl:Ubuntu:tmux"), Some("Ubuntu"));
        assert_eq!(host_of("local:tmux"), None);
        assert_eq!(host_of("ssh:nope:tmux"), None);
    }

    #[test]
    fn a_host_key_resolves_to_its_hosts_own_multiplexer() {
        let hosts = registry();
        let route = |key: &str| hosts.resolve(key).map(HostDef::default_route);
        assert_eq!(
            route("ssh:devbox"),
            Some(Route::remote(Via::Ssh, "devbox", Multiplexer::Tmux))
        );
        assert_eq!(
            route("ssh:winbox"),
            Some(Route::remote(Via::Ssh, "winbox", Multiplexer::Psmux))
        );
        assert_eq!(
            route("wsl:Ubuntu"),
            Some(Route::remote(Via::Wsl, "Ubuntu", Multiplexer::Tmux))
        );
        assert_eq!(route("ssh:nope"), None);
    }

    #[test]
    fn a_recorded_multiplexer_survives_a_changed_host_preference() {
        let registry = HostRegistry {
            config_version: None,
            hosts: vec![HostDef {
                name: "example".into(),
                multiplexer: Some("rmux".into()),
                ..Default::default()
            }],
        };
        // The host comes back as configured, whichever route found it.
        let host = registry
            .host_of(&Route::parse("ssh:example:psmux").unwrap())
            .unwrap();
        assert_eq!(host.multiplexer(), Some(Multiplexer::Rmux));
    }

    #[test]
    fn a_host_name_holding_a_colon_is_refused_at_load() {
        let mut registry: HostRegistry =
            toml::from_str("[[hosts]]\nname = \"example:rmux\"\n\n[[hosts]]\nname = \"example\"\n")
                .unwrap();
        let warnings = registry.refuse_unroutable_names();
        assert_eq!(registry.names(), ["example"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("'example:rmux'"), "{}", warnings[0]);
    }

    #[test]
    fn parses_wsl_host_from_toml() {
        let toml = r#"
[[hosts]]
name = "Ubuntu"
kind = "wsl"

[[hosts]]
name = "custom"
kind = "wsl"
distro = "Debian"
worktrees_dir = "/home/me/wt"
"#;
        let reg: HostRegistry = toml::from_str(toml).unwrap();
        let u = reg.get("Ubuntu").unwrap();
        assert!(u.is_wsl());
        assert_eq!(u.distro_name(), "Ubuntu");
        assert_eq!(u.backend_name(), "wsl:Ubuntu");
        let c = reg.get("custom").unwrap();
        assert_eq!(c.distro_name(), "Debian");
        assert_eq!(c.worktrees_dir.as_deref(), Some("/home/me/wt"));
    }

    #[test]
    fn parses_minimal_and_full_toml() {
        let toml = r#"
[[hosts]]
name = "minimal"
destination = "host1"

[[hosts]]
name = "full"
destination = "me@host2"
socket = "tb2"
session = "tb2"
ssh_opts = ["-o", "ControlMaster=auto"]
worktrees_dir = "/home/me/wt"
"#;
        let reg: HostRegistry = toml::from_str(toml).unwrap();
        assert_eq!(reg.hosts.len(), 2);
        let minimal = reg.get("minimal").unwrap();
        assert_eq!(minimal.destination, "host1");
        assert!(minimal.socket.is_none());
        assert!(minimal.ssh_opts.is_empty());
        let full = reg.get("full").unwrap();
        assert_eq!(full.socket.as_deref(), Some("tb2"));
        assert_eq!(full.ssh_opts, ["-o", "ControlMaster=auto"]);
        assert_eq!(full.worktrees_dir.as_deref(), Some("/home/me/wt"));
    }
}
