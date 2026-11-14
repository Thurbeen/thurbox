//! What fills the registry for a running process: the one place that names a
//! concrete adapter.
//!
//! Only the embedder's composition roots may call it; every other consumer is
//! handed the registry this builds.

use std::sync::Arc;

use crate::registry::BackendRegistry;
use crate::settings::MuxSettings;
use crate::shell::HostLauncher;
use crate::SessionBackend;
use crate::{HostDef, HostRegistry, Multiplexer, Platform, Route};

/// What an adapter is built from: the route it will serve and where that is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSpec {
    /// The qualified route the backend serves, and is named by.
    pub route: Route,
    /// How the host is reached, or `None` for this machine.
    pub launcher: Option<HostLauncher>,
    /// The OS of the machine the multiplexer runs on: this one's, or the
    /// host's — never the multiplexer's.
    pub platform: Platform,
    /// The host as configured, or `None` for this machine.
    pub host: Option<HostDef>,
    /// What the embedder injected: the scrollback a terminal on this backend
    /// keeps, and where the instance keeps its data.
    pub settings: MuxSettings,
}

/// Builds the backend serving a [`BackendSpec`].
pub type AdapterFactory = fn(&BackendSpec) -> Arc<dyn SessionBackend>;

/// One adapter per multiplexer: the only list of them. Adding one is a module
/// and a row here; a route naming a multiplexer with no row is refused by name
/// rather than driven through another's grammar.
const ADAPTERS: &[(Multiplexer, AdapterFactory)] = &[
    (Multiplexer::Tmux, tmux),
    (Multiplexer::Psmux, psmux),
    (Multiplexer::Rmux, rmux),
];

fn tmux(spec: &BackendSpec) -> Arc<dyn SessionBackend> {
    match (&spec.host, &spec.launcher) {
        (Some(host), Some(launcher)) => {
            crate::tmux::on_host(host, launcher.clone(), spec.platform, &spec.settings)
        }
        _ => crate::tmux::local(&spec.settings),
    }
}

fn psmux(spec: &BackendSpec) -> Arc<dyn SessionBackend> {
    match (&spec.host, &spec.launcher) {
        (Some(host), Some(launcher)) => {
            crate::psmux::on_host(host, launcher.clone(), spec.platform, &spec.settings)
        }
        _ => crate::psmux::local(&spec.settings),
    }
}

fn rmux(spec: &BackendSpec) -> Arc<dyn SessionBackend> {
    match (&spec.host, &spec.launcher) {
        (Some(host), Some(launcher)) => {
            crate::rmux::on_host(host, launcher.clone(), spec.platform, &spec.settings)
        }
        _ => crate::rmux::local(&spec.settings),
    }
}

/// Whether an adapter here implements `mux`.
pub fn implements(mux: Multiplexer) -> bool {
    ADAPTERS.iter().any(|(implemented, _)| *implemented == mux)
}

/// The registry as a running interface needs it: one backend per adapter on
/// this machine and on every host in `hosts` — the configured and discovered
/// ones, as [`crate::hosts::cached_registry`] reads them — with this
/// platform's own multiplexer as the default.
///
/// Backends are registered, never readied: registration is a map insert,
/// where readying is a blocking connect (an ssh round trip for a remote
/// host), so a down host must not be probed until a session on it is
/// actually attached. `settings` are the ones the embedder injected.
pub fn for_hosts(hosts: &HostRegistry, settings: &MuxSettings) -> BackendRegistry {
    registry_from(ADAPTERS, hosts, settings)
}

/// Every adapter for this machine and no host: what a command acting only on
/// a local row needs, without reading `hosts.toml` or discovering WSL
/// distros.
pub fn local_only(settings: &MuxSettings) -> BackendRegistry {
    for_hosts(&HostRegistry::default(), settings)
}

/// Every adapter in `table`, for this machine and for each of `hosts`.
///
/// Registration is by adapter, never by OS (ADR-31): a
/// machine — this one or a host — is served by every multiplexer an adapter
/// implements, whatever its platform or its preference. Those decide only which
/// multiplexer a host key means ([`HostDef::default_route`]) and which backend
/// is the default; a binary that is not installed is reported when its backend
/// is first asked to start, by name.
///
/// `table` must serve this platform's default multiplexer, which the registry
/// is never without.
fn registry_from(
    table: &[(Multiplexer, AdapterFactory)],
    hosts: &HostRegistry,
    settings: &MuxSettings,
) -> BackendRegistry {
    let platform = crate::shell::local_platform();
    let default = Multiplexer::default_for(platform);
    let build = |mux: Multiplexer, host: Option<&HostDef>| {
        let factory = table
            .iter()
            .find(|(implemented, _)| *implemented == mux)
            .map(|(_, factory)| factory)?;
        let spec = match host {
            None => BackendSpec {
                route: Route::local(mux),
                launcher: None,
                platform,
                host: None,
                settings: settings.clone(),
            },
            Some(host) => BackendSpec {
                route: host.route(mux),
                launcher: Some(HostLauncher::for_host(host)),
                platform: host.platform(),
                host: Some(host.clone()),
                settings: settings.clone(),
            },
        };
        Some((spec.route.clone(), factory(&spec)))
    };
    let (route, backend) =
        build(default, None).expect("the adapter table serves this platform's default");
    let mut backends = BackendRegistry::new(route, backend);
    for (mux, _) in table {
        for host in std::iter::once(None).chain(hosts.hosts.iter().map(Some)) {
            if host.is_none() && *mux == default {
                continue;
            }
            if let Some((route, backend)) = build(*mux, host) {
                backends.register(route, backend);
            }
        }
    }
    backends
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HostDef;

    fn hosts(preferences: &[(&str, Option<&str>)]) -> HostRegistry {
        HostRegistry {
            config_version: None,
            hosts: preferences
                .iter()
                .map(|(name, mux)| HostDef {
                    name: (*name).into(),
                    multiplexer: mux.map(str::to_string),
                    ..Default::default()
                })
                .collect(),
        }
    }

    /// A host's own multiplexer is served when an adapter implements it —
    /// psmux on a host that prefers it, tmux on one that names none — and
    /// what nothing implements is never served.
    #[test]
    fn each_host_serves_its_own_multiplexer() {
        let hosts = hosts(&[
            ("plain", None),
            ("win", Some("psmux")),
            ("moved", Some("rmux")),
            ("herd", Some("herdr")),
        ]);
        let registry = for_hosts(&hosts, &MuxSettings::default());
        for host in &hosts.hosts {
            let own = host.default_route();
            assert_eq!(
                registry.supports(&own),
                own.mux != Multiplexer::Herdr,
                "{}",
                own.format()
            );
        }
        assert_eq!(hosts.hosts[0].default_route().mux, Multiplexer::Tmux);
        assert_eq!(hosts.hosts[1].default_route().mux, Multiplexer::Psmux);
    }

    #[test]
    fn a_backend_is_named_by_the_route_it_serves() {
        let registry = for_hosts(
            &hosts(&[("plain", None), ("win", Some("psmux"))]),
            &MuxSettings::default(),
        );
        for (route, backend) in registry.all_backends() {
            assert_eq!(backend.name(), route.format());
        }
        assert_eq!(
            registry.default_route(),
            &Route::local(Multiplexer::platform_default())
        );
    }

    /// Registration is by adapter, never by OS (ADR-31): every adapter here
    /// is registered for this machine and for every host whichever OS either
    /// is, and the platform only picks which backend is the default.
    #[test]
    fn every_adapter_is_registered_whatever_the_platform() {
        use crate::shell::simulate_local;
        use crate::{Platform, Via};
        for platform in Platform::ALL {
            simulate_local(platform, || {
                let registry = for_hosts(
                    &hosts(&[("plain", None), ("win", Some("psmux"))]),
                    &MuxSettings::default(),
                );
                for mux in [Multiplexer::Tmux, Multiplexer::Psmux, Multiplexer::Rmux] {
                    assert!(
                        registry.supports(&Route::local(mux)),
                        "{} on a {} machine has an adapter and is not registered",
                        mux.name(),
                        platform.name()
                    );
                }
                for mux in [Multiplexer::Tmux, Multiplexer::Psmux, Multiplexer::Rmux] {
                    for host in ["plain", "win"] {
                        assert!(
                            registry.supports(&Route::remote(Via::Ssh, host, mux)),
                            "{} on host {host} from a {} machine is not registered",
                            mux.name(),
                            platform.name()
                        );
                    }
                }
                let default = match platform {
                    Platform::Windows => Multiplexer::Psmux,
                    Platform::Posix => Multiplexer::Tmux,
                };
                assert_eq!(
                    registry.default_route(),
                    &Route::local(default),
                    "on a {} machine",
                    platform.name()
                );
            });
        }
    }

    /// The selection matrix: a probe adapter registered for each of
    /// [`Multiplexer::ALL`] — Herdr included, which no adapter here implements
    /// — on this machine, an ssh host and a WSL
    /// distro, from a POSIX and a Windows thurbox, and on a POSIX and a Windows
    /// ssh host. Each route reaches the adapter registered for its own
    /// multiplexer, whatever the host prefers; the spec it is built from names
    /// the platform and launcher the placement says, whatever the multiplexer;
    /// and the launcher carries the probe's own command line with nothing of
    /// tmux's added to it.
    mod selection {
        use std::cell::RefCell;

        use super::*;
        use crate::shell::simulate_local;
        use crate::shell::HostLauncher;
        use crate::{HostKind, Platform};

        thread_local! {
            static BUILT: RefCell<Vec<(&'static str, BackendSpec)>> = const { RefCell::new(Vec::new()) };
        }

        fn probe(adapter: &'static str, spec: &BackendSpec) -> Arc<dyn SessionBackend> {
            BUILT.with(|built| built.borrow_mut().push((adapter, spec.clone())));
            crate::registry::stub::stub_named(&format!("{adapter}@{}", spec.route.format()))
        }
        fn tmux_probe(spec: &BackendSpec) -> Arc<dyn SessionBackend> {
            probe("tmux-probe", spec)
        }
        fn psmux_probe(spec: &BackendSpec) -> Arc<dyn SessionBackend> {
            probe("psmux-probe", spec)
        }
        fn rmux_probe(spec: &BackendSpec) -> Arc<dyn SessionBackend> {
            probe("rmux-probe", spec)
        }
        fn herdr_probe(spec: &BackendSpec) -> Arc<dyn SessionBackend> {
            probe("herdr-probe", spec)
        }

        const PROBES: &[(Multiplexer, AdapterFactory)] = &[
            (Multiplexer::Tmux, tmux_probe),
            (Multiplexer::Psmux, psmux_probe),
            (Multiplexer::Rmux, rmux_probe),
            (Multiplexer::Herdr, herdr_probe),
        ];

        fn probe_name(mux: Multiplexer) -> String {
            format!("{}-probe", mux.name())
        }

        /// Every placement a route can name: an ssh host of each platform,
        /// each preferring every multiplexer in turn, and a WSL distro.
        fn placements() -> HostRegistry {
            let mut hosts = Vec::new();
            for platform in Platform::ALL {
                for preferred in std::iter::once(None).chain(Multiplexer::ALL.map(Some)) {
                    hosts.push(HostDef {
                        name: format!(
                            "{}-{}",
                            platform.name(),
                            preferred.map_or("none", Multiplexer::name)
                        ),
                        destination: "user@box".into(),
                        multiplexer: preferred.map(|m| m.name().to_string()),
                        platform: Some(platform),
                        ..Default::default()
                    });
                }
            }
            hosts.push(HostDef {
                name: "distro".into(),
                kind: HostKind::Wsl,
                ..Default::default()
            });
            HostRegistry {
                config_version: None,
                hosts,
            }
        }

        fn spec_for(route: &Route) -> BackendSpec {
            BUILT.with(|built| {
                built
                    .borrow()
                    .iter()
                    .rev()
                    .find(|(_, spec)| &spec.route == route)
                    .map(|(_, spec)| spec.clone())
                    .unwrap_or_else(|| panic!("no adapter was built for {}", route.format()))
            })
        }

        #[test]
        fn every_route_reaches_the_adapter_registered_for_its_multiplexer() {
            let hosts = placements();
            for local in Platform::ALL {
                simulate_local(local, || {
                    BUILT.with(|built| built.borrow_mut().clear());
                    let registry = registry_from(PROBES, &hosts, &MuxSettings::default());
                    let mut routes: Vec<(Route, Option<&HostDef>)> = Multiplexer::ALL
                        .into_iter()
                        .map(|mux| (Route::local(mux), None))
                        .collect();
                    for host in &hosts.hosts {
                        for mux in Multiplexer::ALL {
                            routes.push((host.route(mux), Some(host)));
                        }
                    }
                    for (route, host) in routes {
                        let mux = route.mux;
                        let at = format!("{} from a {} machine", route.format(), local.name());
                        assert_eq!(Route::parse(&route.format()).as_ref(), Ok(&route), "{at}");
                        let backend = registry
                            .get(&route)
                            .unwrap_or_else(|| panic!("{at} is not registered"));
                        assert_eq!(
                            backend.name(),
                            format!("{}@{}", probe_name(mux), route.format()),
                            "{at} reached another multiplexer's adapter"
                        );
                        let spec = spec_for(&route);
                        assert_eq!(spec.host.as_ref(), host, "{at}");
                        assert_eq!(
                            spec.platform,
                            host.map_or(local, HostDef::platform),
                            "{at}: the platform is the machine's, never the multiplexer's"
                        );
                        assert_eq!(
                            spec.launcher,
                            host.map(HostLauncher::for_host),
                            "{at}: the launcher is the placement's"
                        );

                        // The probe's own grammar, unchanged.
                        let command = crate::shell::launch(
                            spec.launcher.as_ref(),
                            mux.name(),
                            &["ls", "--all"],
                        );
                        let argv: Vec<String> = std::iter::once(command.get_program())
                            .chain(command.get_args())
                            .map(|a| a.to_string_lossy().into_owned())
                            .collect();
                        assert!(
                            argv.ends_with(&[mux.name().to_string(), "ls".into(), "--all".into()]),
                            "{at}: the launcher changed the adapter's command line: {argv:?}"
                        );
                        assert!(
                            !argv.iter().any(|a| a == "-L"),
                            "{at}: the launcher added tmux's -L: {argv:?}"
                        );
                    }
                    assert_eq!(
                        registry.default_route(),
                        &Route::local(Multiplexer::default_for(local)),
                        "the default is this platform's own multiplexer"
                    );
                });
            }
        }

        /// The platform picks which adapter is the default; the adapters
        /// registered are the same set either way.
        #[test]
        fn the_platform_never_decides_what_is_registered() {
            let hosts = placements();
            let registered = |local| {
                simulate_local(local, || {
                    let mut routes: Vec<String> =
                        registry_from(PROBES, &hosts, &MuxSettings::default())
                            .routes()
                            .map(Route::format)
                            .collect();
                    routes.sort();
                    routes
                })
            };
            assert_eq!(registered(Platform::Posix), registered(Platform::Windows));
        }
    }

    #[test]
    fn only_implemented_multiplexers_are_served() {
        assert!(implements(Multiplexer::Tmux));
        assert!(implements(Multiplexer::Psmux));
        assert!(implements(Multiplexer::Rmux));
        assert!(!implements(Multiplexer::Herdr));
    }
}
