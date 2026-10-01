//! What fills the registry for a running process: the one place that names a
//! concrete adapter.
//!
//! Only the composition roots may call it (`tests/architecture_rules.rs`).
//! Every other consumer is handed the registry this builds, or — while the
//! backend-boundary sequence is under way — builds one here through a crossing
//! the architecture test lists as transitional.

use std::sync::Arc;

use crate::backend::registry::BackendRegistry;
use crate::backend::tmux::TmuxBackend;
use crate::backend::SessionBackend;
use crate::session::{HostRegistry, Multiplexer, Route};

/// The multiplexers an adapter here implements. The tmux adapter speaks tmux
/// and psmux; a route naming any other is refused by name rather than driven
/// through the tmux command grammar, and adding one is an adapter plus a
/// line here.
const IMPLEMENTED: [Multiplexer; 2] = [Multiplexer::Tmux, Multiplexer::Psmux];

/// Whether an adapter here implements `mux`.
pub fn implements(mux: Multiplexer) -> bool {
    IMPLEMENTED.contains(&mux)
}

/// The registry as a running interface needs it: the local multiplexer as
/// the default plus one backend per configured or discovered host — the
/// same construction the v1 binary did by hand.
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

/// The registry for `hosts`.
///
/// This machine serves its platform's own multiplexer, the one the local
/// adapter runs. Each host serves the multiplexer its unqualified rows have
/// always meant — which is its preference whenever that is implemented — so
/// a host that prefers rmux keeps serving the tmux rows written before, and
/// is never registered for rmux itself until an adapter implements it. No
/// route is rewritten to another: one nothing serves is simply absent.
fn for_hosts(hosts: &HostRegistry) -> BackendRegistry {
    let local: Arc<dyn SessionBackend> = Arc::new(TmuxBackend::new());
    let mut backends =
        BackendRegistry::new(Route::local(Some(Multiplexer::platform_default())), local);
    for host in &hosts.hosts {
        let route = hosts.qualify(&host.route(None));
        let Some(mux) = route.mux.filter(|mux| implements(*mux)) else {
            continue;
        };
        backends.register(route, Arc::new(TmuxBackend::for_route(host, mux)));
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

    #[test]
    fn each_host_serves_what_its_unqualified_rows_mean() {
        let registry = for_hosts(&hosts(&[
            ("plain", None),
            ("win", Some("psmux")),
            ("moved", Some("rmux")),
            ("herd", Some("herdr")),
        ]));
        let served = |host: &str, mux: Multiplexer| {
            registry.supports(&Route::remote(crate::session::Via::Ssh, host, Some(mux)))
        };
        assert!(served("plain", Multiplexer::Tmux));
        assert!(served("win", Multiplexer::Psmux));
        assert!(!served("win", Multiplexer::Tmux));
        // Rows written for tmux keep a backend; the preference itself has none.
        for host in ["moved", "herd"] {
            assert!(served(host, Multiplexer::Tmux), "{host}");
            assert!(!served(host, Multiplexer::Rmux), "{host}");
            assert!(!served(host, Multiplexer::Herdr), "{host}");
        }
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

    /// Registration is by adapter, never by OS (§4b.3): every adapter here
    /// is registered for this machine and for every host whichever OS either
    /// is, and the platform only picks what an unqualified route means.
    #[test]
    fn every_adapter_is_registered_whatever_the_platform() {
        use crate::session::platform::simulate_local;
        use crate::session::{Platform, Via};
        for platform in Platform::ALL {
            simulate_local(platform, || {
                let registry = for_hosts(&hosts(&[("plain", None), ("win", Some("psmux"))]));
                for mux in [Multiplexer::Tmux, Multiplexer::Psmux] {
                    assert!(
                        registry.supports(&Route::local(Some(mux))),
                        "{} on a {} machine has an adapter and is not registered",
                        mux.name(),
                        platform.name()
                    );
                }
                for mux in [Multiplexer::Tmux, Multiplexer::Psmux] {
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

    /// The selection matrix (E1, E4): a probe adapter registered for each of
    /// [`Multiplexer::ALL`] — the RMUX and Herdr ones included, which no
    /// adapter here implements — on this machine, an ssh host and a WSL
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

                        // E4: the probe's own grammar, unchanged.
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
        assert!(!implements(Multiplexer::Rmux));
        assert!(!implements(Multiplexer::Herdr));
    }
}
