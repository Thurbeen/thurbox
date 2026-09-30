//! Backend registry for multi-backend session support.
//!
//! Allows multiple `SessionBackend` implementations to coexist, each keyed by
//! the qualified [`Route`] it serves. A container only: what fills it for a
//! running process is [`crate::backend::wiring`], and settling a row's
//! unqualified route is `HostRegistry::qualify`'s — this looks up exactly the
//! route it is asked for and never substitutes another.

use std::collections::HashMap;
use std::sync::Arc;

use crate::backend::SessionBackend;
use crate::session::Route;

/// A registry of session backends keyed by the qualified route each serves.
///
/// The registry always has a default backend: this machine's own
/// multiplexer.
pub struct BackendRegistry {
    backends: HashMap<Route, Arc<dyn SessionBackend>>,
    default_route: Route,
}

impl BackendRegistry {
    /// A registry whose default is `default`, serving `route`.
    pub fn new(route: Route, default: Arc<dyn SessionBackend>) -> Self {
        let mut backends = HashMap::new();
        backends.insert(route.clone(), default);
        Self {
            backends,
            default_route: route,
        }
    }

    /// Register a backend for `route`, replacing any serving it already.
    pub fn register(&mut self, route: Route, backend: Arc<dyn SessionBackend>) {
        self.backends.insert(route, backend);
    }

    /// The backend serving exactly `route`. An unqualified route matches
    /// nothing: which multiplexer it means is decided before this is asked.
    pub fn get(&self, route: &Route) -> Option<&Arc<dyn SessionBackend>> {
        self.backends.get(route)
    }

    /// Return the default backend.
    pub fn default_backend(&self) -> &Arc<dyn SessionBackend> {
        &self.backends[&self.default_route]
    }

    /// The route the default backend serves.
    pub fn default_route(&self) -> &Route {
        &self.default_route
    }

    /// Whether a new session may be created on `route`: only when an
    /// implementation has registered for it. Installing a binary alone never
    /// makes an adapter exist, and neither does the OS this runs on.
    pub fn supports(&self, route: &Route) -> bool {
        self.backends.contains_key(route)
    }

    /// Every registered route.
    pub fn routes(&self) -> impl Iterator<Item = &Route> {
        self.backends.keys()
    }

    /// Every registered backend, with the route it serves.
    pub fn all_backends(&self) -> impl Iterator<Item = (&Route, &Arc<dyn SessionBackend>)> {
        self.backends.iter()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use anyhow::Result;

    use super::*;
    use crate::backend::{AdoptedSession, DiscoveredSession, SpawnedSession};
    use crate::session::{Multiplexer, Via};

    struct StubBackend {
        backend_name: String,
    }

    fn stub(route: &Route) -> Arc<dyn SessionBackend> {
        Arc::new(StubBackend {
            backend_name: route.format(),
        })
    }

    impl SessionBackend for StubBackend {
        fn name(&self) -> &str {
            &self.backend_name
        }
        fn check_available(&self) -> Result<()> {
            Ok(())
        }
        fn ensure_ready(&self) -> Result<()> {
            Ok(())
        }
        fn spawn(
            &self,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<&Path>,
            _: &std::collections::HashMap<String, String>,
            _: u16,
            _: u16,
        ) -> Result<SpawnedSession> {
            unimplemented!()
        }
        fn adopt(&self, _: &str, _: u16, _: u16, _: Option<Vec<u8>>) -> Result<AdoptedSession> {
            unimplemented!()
        }
        fn discover(&self) -> Result<Vec<DiscoveredSession>> {
            Ok(vec![])
        }
        fn resize(&self, _: &str, _: u16, _: u16) -> Result<()> {
            Ok(())
        }
        fn is_dead(&self, _: &str) -> Result<bool> {
            Ok(false)
        }
        fn kill(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn detach(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn pane_pid(&self, _: &str) -> Result<Option<u32>> {
            Ok(None)
        }
    }

    fn local(mux: Multiplexer) -> Route {
        Route::local(Some(mux))
    }

    #[test]
    fn new_registers_the_default() {
        let route = local(Multiplexer::Tmux);
        let registry = BackendRegistry::new(route.clone(), stub(&route));
        assert!(registry.supports(&route));
        assert_eq!(registry.default_route(), &route);
        assert_eq!(registry.default_backend().name(), "local-tmux");
    }

    #[test]
    fn a_route_finds_only_the_backend_registered_for_it() {
        let default = local(Multiplexer::Tmux);
        let mut registry = BackendRegistry::new(default.clone(), stub(&default));
        let remote = Route::remote(Via::Ssh, "box", Some(Multiplexer::Tmux));
        registry.register(remote.clone(), stub(&remote));

        assert_eq!(registry.get(&remote).unwrap().name(), "ssh:box:tmux");
        // Another multiplexer on the same machine is another backend, and an
        // unqualified route is not quietly the default one.
        for missing in [
            remote.with_mux(Multiplexer::Rmux),
            Route::remote(Via::Ssh, "box", None),
            Route::remote(Via::Wsl, "box", Some(Multiplexer::Tmux)),
            Route::local(None),
            local(Multiplexer::Psmux),
        ] {
            assert!(registry.get(&missing).is_none(), "{missing}");
            assert!(!registry.supports(&missing), "{missing}");
        }
    }

    /// Every multiplexer is a route on every machine; which of them work is
    /// what registered, and nothing else. A test registers an implementation
    /// for rmux and herdr the way an adapter someday would — locally and on a
    /// host of either kind — and only those routes resolve.
    #[test]
    fn availability_is_registration_for_every_mux_on_every_machine() {
        let places = [
            Route::local(None),
            Route::remote(Via::Ssh, "box", None),
            Route::remote(Via::Wsl, "Ubuntu", None),
        ];
        for registered in Multiplexer::ALL {
            let default = local(Multiplexer::Tmux);
            let mut registry = BackendRegistry::new(default.clone(), stub(&default));
            for place in &places {
                registry.register(
                    place.with_mux(registered),
                    stub(&place.with_mux(registered)),
                );
            }
            for place in &places {
                for mux in Multiplexer::ALL {
                    let route = place.with_mux(mux);
                    let served = mux == registered || route == default;
                    assert_eq!(registry.supports(&route), served, "{route}");
                    if served {
                        assert_eq!(registry.get(&route).unwrap().name(), route.format());
                    }
                }
            }
        }
    }

    #[test]
    fn routes_and_backends_list_everything_registered() {
        let default = local(Multiplexer::Tmux);
        let mut registry = BackendRegistry::new(default.clone(), stub(&default));
        let extra = Route::remote(Via::Ssh, "box", Some(Multiplexer::Psmux));
        registry.register(extra.clone(), stub(&extra));

        let mut routes: Vec<String> = registry.routes().map(Route::format).collect();
        routes.sort();
        assert_eq!(routes, ["local-tmux", "ssh:box:psmux"]);
        let mut names: Vec<&str> = registry.all_backends().map(|(_, b)| b.name()).collect();
        names.sort();
        assert_eq!(names, ["local-tmux", "ssh:box:psmux"]);
    }
}
