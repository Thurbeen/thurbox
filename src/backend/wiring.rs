//! What fills the registry for a running process: the one place that names a
//! concrete adapter.
//!
//! Only the composition roots may call it (`tests/architecture_rules.rs`);
//! every other consumer is handed the registry this builds.

use std::sync::Arc;

use crate::backend::registry::BackendRegistry;
use crate::backend::SessionBackend;
use crate::session::{HostDef, HostRegistry, Multiplexer, Platform, Route};
use crate::shell::HostLauncher;

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
            crate::backend::tmux::on_host(host, launcher.clone(), spec.platform)
        }
        _ => crate::backend::tmux::local(),
    }
}

fn psmux(spec: &BackendSpec) -> Arc<dyn SessionBackend> {
    match (&spec.host, &spec.launcher) {
        (Some(host), Some(launcher)) => {
            crate::backend::psmux::on_host(host, launcher.clone(), spec.platform)
        }
        _ => crate::backend::psmux::local(),
    }
}

fn rmux(spec: &BackendSpec) -> Arc<dyn SessionBackend> {
    match (&spec.host, &spec.launcher) {
        (Some(host), Some(launcher)) => {
            crate::backend::rmux::on_host(host, launcher.clone(), spec.platform)
        }
        _ => crate::backend::rmux::local(),
    }
}

/// Whether an adapter here implements `mux`.
pub fn implements(mux: Multiplexer) -> bool {
    ADAPTERS.iter().any(|(implemented, _)| *implemented == mux)
}

/// The registry as a running interface needs it: one backend per adapter on
/// this machine and on every configured or discovered host, with this
/// platform's own multiplexer as the default.
///
/// Backends are registered, never readied: registration is a map insert,
/// where readying is a blocking connect (an ssh round trip for a remote
/// host), so a down host must not be probed until a session on it is
/// actually attached. The `HostRegistry` comes back alongside because a
/// pane needs more than a connection — a remote session's launch directory
/// resolves against its `HostDef` — and both halves must come from the same
/// read of `hosts.toml`. The warnings are that read's, for callers that
/// surface them.
pub fn configured() -> (BackendRegistry, HostRegistry, Vec<String>) {
    let (hosts, warnings) = crate::agent::host_config::cached_registry();
    (for_hosts(hosts), hosts.clone(), warnings.clone())
}

/// Every adapter for this machine and no host: what a command acting only on
/// a local row needs, without reading `hosts.toml` or discovering WSL
/// distros.
pub fn local_only() -> BackendRegistry {
    for_hosts(&HostRegistry::default())
}

/// The registry for `hosts`, from [`ADAPTERS`].
fn for_hosts(hosts: &HostRegistry) -> BackendRegistry {
    registry_from(ADAPTERS, hosts)
}

/// Every adapter in `table`, for this machine and for each of `hosts`.
///
/// Registration is by adapter, never by OS (ADR-31): a
/// machine — this one or a host — is served by every multiplexer an adapter
/// implements, whatever its platform or its preference. Those decide only what
/// an unqualified route means ([`HostRegistry::qualify`]) and which backend is
/// the default; a binary that is not installed is reported when its backend
/// is first asked to start, by name.
///
/// `table` must serve this platform's default multiplexer, which the registry
/// is never without.
fn registry_from(table: &[(Multiplexer, AdapterFactory)], hosts: &HostRegistry) -> BackendRegistry {
    let platform = Platform::local();
    let default = Multiplexer::default_for(platform);
    let build = |mux: Multiplexer, host: Option<&HostDef>| {
        let factory = table
            .iter()
            .find(|(implemented, _)| *implemented == mux)
            .map(|(_, factory)| factory)?;
        let spec = match host {
            None => BackendSpec {
                route: Route::local(Some(mux)),
                launcher: None,
                platform,
                host: None,
            },
            Some(host) => BackendSpec {
                route: host.route(Some(mux)),
                launcher: Some(HostLauncher::for_host(host)),
                platform: host.platform(),
                host: Some(host.clone()),
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
    use crate::session::HostDef;

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

    /// What a host's unqualified rows have always meant is served — psmux on
    /// a host that prefers it, tmux on one that prefers a multiplexer nothing
    /// implements — and what nothing implements is never served.
    #[test]
    fn each_host_serves_what_its_unqualified_rows_mean() {
        let hosts = hosts(&[
            ("plain", None),
            ("win", Some("psmux")),
            ("moved", Some("rmux")),
            ("herd", Some("herdr")),
        ]);
        let registry = for_hosts(&hosts);
        for host in &hosts.hosts {
            let meant = hosts.qualify(&host.route(None));
            assert!(registry.supports(&meant), "{}", meant.format());
            assert!(
                !registry.supports(&host.route(Some(Multiplexer::Herdr))),
                "{}",
                host.name
            );
        }
        assert_eq!(
            hosts.qualify(&hosts.hosts[1].route(None)).mux,
            Some(Multiplexer::Psmux)
        );
    }

    #[test]
    fn a_backend_is_named_by_the_route_it_serves() {
        let registry = for_hosts(&hosts(&[("plain", None), ("win", Some("psmux"))]));
        for (route, backend) in registry.all_backends() {
            assert_eq!(backend.name(), route.format());
        }
        assert_eq!(
            registry.default_route(),
            &Route::local(Some(Multiplexer::platform_default()))
        );
    }

    /// Registration is by adapter, never by OS (ADR-31): every adapter here
    /// is registered for this machine and for every host whichever OS either
    /// is, and the platform only picks what an unqualified route means.
    #[test]
    fn every_adapter_is_registered_whatever_the_platform() {
        use crate::session::platform::simulate_local;
        use crate::session::{Platform, Via};
        for platform in Platform::ALL {
            simulate_local(platform, || {
                let registry = for_hosts(&hosts(&[("plain", None), ("win", Some("psmux"))]));
                for mux in [Multiplexer::Tmux, Multiplexer::Psmux, Multiplexer::Rmux] {
                    assert!(
                        registry.supports(&Route::local(Some(mux))),
                        "{} on a {} machine has an adapter and is not registered",
                        mux.name(),
                        platform.name()
                    );
                }
                for mux in [Multiplexer::Tmux, Multiplexer::Psmux, Multiplexer::Rmux] {
                    for host in ["plain", "win"] {
                        assert!(
                            registry.supports(&Route::remote(Via::Ssh, host, Some(mux))),
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
                    &Route::local(Some(default)),
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
        use crate::session::platform::simulate_local;
        use crate::session::{HostKind, Platform};
        use crate::shell::HostLauncher;

        thread_local! {
            static BUILT: RefCell<Vec<(&'static str, BackendSpec)>> = const { RefCell::new(Vec::new()) };
        }

        fn probe(adapter: &'static str, spec: &BackendSpec) -> Arc<dyn SessionBackend> {
            BUILT.with(|built| built.borrow_mut().push((adapter, spec.clone())));
            crate::backend::registry::tests::stub_named(&format!(
                "{adapter}@{}",
                spec.route.format()
            ))
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
                    let registry = registry_from(PROBES, &hosts);
                    let mut routes: Vec<(Route, Option<&HostDef>)> = Multiplexer::ALL
                        .into_iter()
                        .map(|mux| (Route::local(Some(mux)), None))
                        .collect();
                    for host in &hosts.hosts {
                        for mux in Multiplexer::ALL {
                            routes.push((host.route(Some(mux)), Some(host)));
                        }
                    }
                    for (route, host) in routes {
                        let mux = route.mux.expect("qualified");
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
                        &Route::local(Some(Multiplexer::default_for(local))),
                        "the default is this platform's own multiplexer"
                    );
                });
            }
        }

        /// The platform picks what an unqualified local route means and which
        /// adapter is the default; the adapters registered are the same set
        /// either way.
        #[test]
        fn the_platform_never_decides_what_is_registered() {
            let hosts = placements();
            let registered = |local| {
                simulate_local(local, || {
                    let mut routes: Vec<String> = registry_from(PROBES, &hosts)
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
