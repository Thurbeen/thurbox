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
/// multiplexer. A clone shares the same backends — the handles, not new
/// connections.
#[derive(Clone)]
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

    /// Retire every backend's long-lived resources, all at once: each
    /// [`SessionBackend::shutdown`] runs on its own thread, so quitting costs
    /// the slowest connection rather than their sum. Called once, by the
    /// process that built the registry, as it exits.
    pub fn shutdown_all(&self) {
        std::thread::scope(|scope| {
            for backend in self.backends.values() {
                scope.spawn(move || backend.shutdown());
            }
        });
    }
}

/// A registry for the crate's own unit tests, which may name neither the
/// factory nor an adapter: this machine's route served by an in-memory backend
/// that starts with no windows and runs nothing. It keeps the windows it is
/// asked to open, by owner, so a pipeline that creates a session and tears it
/// down again finds what it made; the faithful fake, held to the tmux
/// adapter's contract, is the integration tests' `RecordingBackend`.
#[cfg(test)]
pub(crate) fn inert() -> BackendRegistry {
    inert_serving(&[])
}

/// [`inert`], also serving each of `routes` with a backend of its own.
#[cfg(test)]
pub(crate) fn inert_serving(routes: &[Route]) -> BackendRegistry {
    let local = Route::local(Some(crate::session::Multiplexer::platform_default()));
    let mut registry = BackendRegistry::new(local.clone(), tests::stub(&local));
    for route in routes {
        registry.register(route.clone(), tests::stub(route));
    }
    registry
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;

    use anyhow::Result;

    use super::*;
    use crate::backend::{AdoptedSession, DiscoveredSession, SpawnedSession, WindowRole};
    use crate::session::{Multiplexer, Via};

    struct StubBackend {
        backend_name: String,
        shutdowns: std::sync::atomic::AtomicUsize,
        /// `(pane, owner id, owner name, role)` for every window opened.
        windows: std::sync::Mutex<Vec<(String, String, String, WindowRole)>>,
    }

    fn typed_stub(route: &Route) -> Arc<StubBackend> {
        Arc::new(StubBackend {
            backend_name: route.format(),
            shutdowns: std::sync::atomic::AtomicUsize::new(0),
            windows: std::sync::Mutex::new(Vec::new()),
        })
    }

    impl StubBackend {
        fn place(
            &self,
            owner: &crate::backend::Owner<'_>,
            role: WindowRole,
        ) -> crate::backend::Located {
            self.windows
                .lock()
                .unwrap()
                .iter()
                .find(|(_, id, _, r)| id == owner.session_id && *r == role)
                .map_or(crate::backend::Located::Absent, |(pane, ..)| {
                    crate::backend::Located::At(pane.clone())
                })
        }
    }

    pub(crate) fn stub(route: &Route) -> Arc<dyn SessionBackend> {
        typed_stub(route)
    }

    /// A stub named `name` rather than by a route — a probe adapter's, which
    /// says which factory built it.
    pub(crate) fn stub_named(name: &str) -> Arc<dyn SessionBackend> {
        Arc::new(StubBackend {
            backend_name: name.to_string(),
            shutdowns: std::sync::atomic::AtomicUsize::new(0),
            windows: std::sync::Mutex::new(Vec::new()),
        })
    }

    impl SessionBackend for StubBackend {
        fn hook_signal_command(&self) -> Option<String> {
            None
        }
        fn record_hook_state(&self, _: &str, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("this stub has no status channel")
        }
        fn hook_states(&self) -> anyhow::Result<Vec<(String, String)>> {
            anyhow::bail!("this stub has no status channel")
        }
        fn take_hook_state_events(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn ensure_heartbeat(
            &self,
            _: &std::path::Path,
            _: &[String],
            _: std::time::Duration,
        ) -> anyhow::Result<()> {
            anyhow::bail!("this stub keeps no heartbeat")
        }
        fn heartbeat_running(&self) -> anyhow::Result<bool> {
            Ok(false)
        }
        fn stop_heartbeat(&self) -> anyhow::Result<bool> {
            Ok(false)
        }
        fn send_text(&self, _: &str, _: &str, _: bool) -> anyhow::Result<()> {
            anyhow::bail!("this stub has no panes to type into")
        }
        fn send_text_after(&self, _: &str, _: &str, _: std::time::Duration) -> anyhow::Result<()> {
            anyhow::bail!("this stub has no panes to type into")
        }
        fn send_key(&self, _: &str, _: &crate::backend::Key) -> anyhow::Result<String> {
            anyhow::bail!("this stub has no panes to type into")
        }
        fn capture(&self, _: &str, _: u32, _: bool) -> anyhow::Result<String> {
            anyhow::bail!("this stub has no panes to read")
        }
        fn pane_state(&self, _: &str) -> anyhow::Result<crate::backend::PaneState> {
            anyhow::bail!("this stub has no panes to read")
        }
        fn pane_path(&self, _: &str) -> anyhow::Result<Option<String>> {
            anyhow::bail!("this stub has no panes to read")
        }
        fn create_window(&self, spec: &crate::backend::WindowSpec<'_>) -> anyhow::Result<String> {
            let mut windows = self.windows.lock().unwrap();
            let pane = format!("%{}", windows.len());
            windows.push((
                pane.clone(),
                spec.owner.session_id.to_string(),
                spec.owner.name.to_string(),
                spec.role,
            ));
            Ok(pane)
        }
        fn locate(
            &self,
            owner: crate::backend::Owner<'_>,
        ) -> anyhow::Result<crate::backend::Placed> {
            Ok(crate::backend::Placed {
                agent: self.place(&owner, WindowRole::Agent),
                shell: self.place(&owner, WindowRole::Shell),
            })
        }
        fn rename_windows(&self, owner: crate::backend::Owner<'_>, to: &str) -> anyhow::Result<()> {
            for window in self.windows.lock().unwrap().iter_mut() {
                if window.1 == owner.session_id {
                    window.2 = to.to_string();
                }
            }
            Ok(())
        }
        fn stamp_window(
            &self,
            _: &str,
            _: &str,
            _: crate::backend::WindowRole,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        fn window_panes(&self, _: &str) -> anyhow::Result<Vec<(String, bool)>> {
            Ok(Vec::new())
        }
        fn set_pane_retention(&self, _: &str, _: bool) -> anyhow::Result<()> {
            Ok(())
        }
        fn shutdown(&self) {
            self.shutdowns
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn name(&self) -> &str {
            &self.backend_name
        }
        fn default_shell(&self) -> String {
            "/bin/sh".to_string()
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
            anyhow::bail!("{}: nothing to spawn on", self.backend_name)
        }
        fn adopt(&self, _: &str, _: u16, _: u16, _: Option<Vec<u8>>) -> Result<AdoptedSession> {
            anyhow::bail!("{}: nothing to adopt", self.backend_name)
        }
        fn discover(&self) -> Result<Vec<DiscoveredSession>> {
            Ok(self
                .windows
                .lock()
                .unwrap()
                .iter()
                .map(|(pane, id, name, role)| DiscoveredSession {
                    backend_id: pane.clone(),
                    name: name.clone(),
                    is_alive: true,
                    session: id.clone(),
                    role: *role,
                })
                .collect())
        }
        fn resize(&self, _: &str, _: u16, _: u16) -> Result<()> {
            Ok(())
        }
        fn is_dead(&self, _: &str) -> Result<bool> {
            Ok(false)
        }
        fn kill(&self, pane: &str) -> Result<()> {
            self.windows.lock().unwrap().retain(|(p, ..)| p != pane);
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
        assert_eq!(registry.default_backend().name(), "local:tmux");
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
    /// for each multiplexer the way a factory does — locally and on a
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

    /// Quit's one call reaches every backend the registry holds, the default
    /// and each host's alike.
    #[test]
    fn shutdown_all_retires_every_backend_once() {
        let default = local(Multiplexer::Tmux);
        let here = typed_stub(&default);
        let mut registry = BackendRegistry::new(default, here.clone());
        let remote = Route::remote(Via::Ssh, "box", Some(Multiplexer::Tmux));
        let there = typed_stub(&remote);
        registry.register(remote, there.clone());

        registry.shutdown_all();

        for backend in [here, there] {
            assert_eq!(
                backend.shutdowns.load(std::sync::atomic::Ordering::Relaxed),
                1,
                "{}",
                backend.backend_name
            );
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
        assert_eq!(routes, ["local:tmux", "ssh:box:psmux"]);
        let mut names: Vec<&str> = registry.all_backends().map(|(_, b)| b.name()).collect();
        names.sort();
        assert_eq!(names, ["local:tmux", "ssh:box:psmux"]);
    }
}
