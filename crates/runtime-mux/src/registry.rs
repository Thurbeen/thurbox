use std::collections::HashMap;
use std::sync::Arc;

use crate::Route;
use crate::SessionBackend;

#[derive(Clone)]
pub struct BackendRegistry {
    backends: HashMap<Route, Arc<dyn SessionBackend>>,
    default_route: Route,
}

impl BackendRegistry {
    pub fn new(route: Route, default: Arc<dyn SessionBackend>) -> Self {
        let mut backends = HashMap::new();
        backends.insert(route.clone(), default);
        Self {
            backends,
            default_route: route,
        }
    }

    pub fn register(&mut self, route: Route, backend: Arc<dyn SessionBackend>) {
        self.backends.insert(route, backend);
    }

    pub fn get(&self, route: &Route) -> Option<&Arc<dyn SessionBackend>> {
        self.backends.get(route)
    }

    pub fn default_backend(&self) -> &Arc<dyn SessionBackend> {
        &self.backends[&self.default_route]
    }

    pub fn default_route(&self) -> &Route {
        &self.default_route
    }

    pub fn supports(&self, route: &Route) -> bool {
        self.backends.contains_key(route)
    }

    pub fn routes(&self) -> impl Iterator<Item = &Route> {
        self.backends.keys()
    }

    pub fn all_backends(&self) -> impl Iterator<Item = (&Route, &Arc<dyn SessionBackend>)> {
        self.backends.iter()
    }

    pub fn shutdown_all(&self) {
        std::thread::scope(|scope| {
            for backend in self.backends.values() {
                scope.spawn(move || backend.shutdown());
            }
        });
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn inert() -> BackendRegistry {
    inert_serving(&[])
}

#[cfg(any(test, feature = "test-support"))]
pub fn inert_serving(routes: &[Route]) -> BackendRegistry {
    let local = Route::local(crate::Multiplexer::platform_default());
    let mut registry = BackendRegistry::new(local.clone(), stub::stub(&local));
    for route in routes {
        registry.register(route.clone(), stub::stub(route));
    }
    registry
}

#[cfg(any(test, feature = "test-support"))]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod stub {
    use std::path::Path;

    use super::*;
    use crate::{
        AdoptedSession, DiscoveredSession, PaneIo, PaneStream, SpawnedSession, StatusChannel,
        WindowLifecycle, WindowRole,
    };

    pub(crate) struct StubBackend {
        pub(crate) backend_name: String,
        pub(crate) shutdowns: std::sync::atomic::AtomicUsize,
        windows: std::sync::Mutex<Vec<(String, String, String, WindowRole)>>,
    }

    pub(crate) fn typed_stub(route: &Route) -> Arc<StubBackend> {
        Arc::new(StubBackend {
            backend_name: route.format(),
            shutdowns: std::sync::atomic::AtomicUsize::new(0),
            windows: std::sync::Mutex::new(Vec::new()),
        })
    }

    impl StubBackend {
        fn place(&self, owner: &crate::Owner<'_>, role: WindowRole) -> crate::Located {
            self.windows
                .lock()
                .unwrap()
                .iter()
                .find(|(_, id, _, r)| id == owner.session_id && *r == role)
                .map_or(crate::Located::Absent, |(pane, ..)| {
                    crate::Located::At(pane.clone())
                })
        }
    }

    pub(crate) fn stub(route: &Route) -> Arc<dyn SessionBackend> {
        typed_stub(route)
    }

    pub(crate) fn stub_named(name: &str) -> Arc<dyn SessionBackend> {
        Arc::new(StubBackend {
            backend_name: name.to_string(),
            shutdowns: std::sync::atomic::AtomicUsize::new(0),
            windows: std::sync::Mutex::new(Vec::new()),
        })
    }

    impl PaneStream for StubBackend {
        fn spawn(
            &self,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<&Path>,
            _: &std::collections::HashMap<String, String>,
            _: u16,
            _: u16,
        ) -> crate::contract::BackendResult<SpawnedSession> {
            Err(anyhow::anyhow!("{}: nothing to spawn on", self.backend_name).into())
        }

        fn adopt(
            &self,
            _: &str,
            _: u16,
            _: u16,
            _: Option<Vec<u8>>,
        ) -> crate::contract::BackendResult<AdoptedSession> {
            Err(anyhow::anyhow!("{}: nothing to adopt", self.backend_name).into())
        }

        fn resize(&self, _: &str, _: u16, _: u16) -> crate::contract::BackendResult<()> {
            Ok(())
        }

        fn detach(&self, _: &str) -> crate::contract::BackendResult<()> {
            Ok(())
        }
    }

    impl WindowLifecycle for StubBackend {
        fn pane_state(&self, pane: &str) -> crate::contract::BackendResult<crate::PaneState> {
            if self
                .windows
                .lock()
                .unwrap()
                .iter()
                .any(|(id, ..)| id == pane)
            {
                Ok(crate::PaneState {
                    dead: Some(false),
                    ..Default::default()
                })
            } else {
                Err(anyhow::anyhow!("this stub has no such pane").into())
            }
        }

        fn pane_path(&self, _: &str) -> crate::contract::BackendResult<Option<String>> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }

        fn create_window(
            &self,
            spec: &crate::WindowSpec<'_>,
        ) -> crate::contract::BackendResult<crate::PaneId> {
            let mut windows = self.windows.lock().unwrap();
            let pane = format!("%{}", windows.len());
            windows.push((
                pane.clone(),
                spec.owner.session_id.to_string(),
                spec.owner.name.to_string(),
                spec.role,
            ));
            Ok(crate::PaneId::new(pane)?)
        }

        fn locate(&self, owner: crate::Owner<'_>) -> crate::contract::BackendResult<crate::Placed> {
            Ok(crate::Placed {
                agent: self.place(&owner, WindowRole::Agent),
                shell: self.place(&owner, WindowRole::Shell),
            })
        }

        fn rename_windows(
            &self,
            owner: crate::Owner<'_>,
            to: &str,
        ) -> crate::contract::BackendResult<()> {
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
            _: crate::WindowRole,
        ) -> crate::contract::BackendResult<()> {
            Ok(())
        }

        fn window_panes(&self, _: &str) -> crate::contract::BackendResult<Vec<(String, bool)>> {
            Ok(Vec::new())
        }

        fn set_pane_retention(&self, _: &str, _: bool) -> crate::contract::BackendResult<()> {
            Ok(())
        }

        fn shutdown(&self) {
            self.shutdowns
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        fn default_shell(&self) -> String {
            "/bin/sh".to_string()
        }

        fn check_available(&self) -> crate::contract::BackendResult<()> {
            Ok(())
        }

        fn ensure_ready(&self) -> crate::contract::BackendResult<()> {
            Ok(())
        }

        fn discover(&self) -> crate::contract::BackendResult<Vec<DiscoveredSession>> {
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

        fn is_dead(&self, _: &str) -> crate::contract::BackendResult<bool> {
            Ok(false)
        }

        fn kill(&self, pane: &str) -> crate::contract::BackendResult<()> {
            self.windows.lock().unwrap().retain(|(p, ..)| p != pane);
            Ok(())
        }

        fn pane_pid(&self, _: &str) -> crate::contract::BackendResult<Option<u32>> {
            Ok(None)
        }
    }

    impl PaneIo for StubBackend {
        fn send_text(&self, _: &str, _: &str, _: bool) -> crate::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn send_text_after(
            &self,
            _: &str,
            _: &str,
            _: std::time::Duration,
        ) -> crate::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn send_key(&self, _: &str, _: &crate::Key) -> crate::contract::BackendResult<String> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn capture(&self, _: &str, _: u32, _: bool) -> crate::contract::BackendResult<String> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }
    }

    impl StatusChannel for StubBackend {
        fn hook_signal_command(&self) -> Option<String> {
            None
        }

        fn record_hook_state(&self, _: &str, _: &str) -> crate::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no status channel").into())
        }

        fn hook_states(&self) -> crate::contract::BackendResult<Vec<(String, String)>> {
            Err(anyhow::anyhow!("this stub has no status channel").into())
        }

        fn take_hook_state_events(&self) -> Vec<(String, String)> {
            Vec::new()
        }
    }

    impl SessionBackend for StubBackend {
        fn name(&self) -> &str {
            &self.backend_name
        }
        fn capabilities(&self) -> crate::Capabilities {
            crate::Capabilities {
                keeps_exited_windows: !self.backend_name.ends_with(":psmux"),
                ..crate::Capabilities::NONE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stub::{stub, typed_stub};
    use super::*;
    use crate::{Multiplexer, Via};

    fn local(mux: Multiplexer) -> Route {
        Route::local(mux)
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
        let remote = Route::remote(Via::Ssh, "box", Multiplexer::Tmux);
        registry.register(remote.clone(), stub(&remote));

        assert_eq!(registry.get(&remote).unwrap().name(), "ssh:box:tmux");
        for missing in [
            remote.with_mux(Multiplexer::Rmux),
            Route::remote(Via::Wsl, "box", Multiplexer::Tmux),
            local(Multiplexer::Psmux),
        ] {
            assert!(registry.get(&missing).is_none(), "{missing}");
            assert!(!registry.supports(&missing), "{missing}");
        }
    }

    #[test]
    fn availability_is_registration_for_every_mux_on_every_machine() {
        let places = [
            Route::local(Multiplexer::Tmux),
            Route::remote(Via::Ssh, "box", Multiplexer::Tmux),
            Route::remote(Via::Wsl, "Ubuntu", Multiplexer::Tmux),
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
    fn shutdown_all_retires_every_backend_once() {
        let default = local(Multiplexer::Tmux);
        let here = typed_stub(&default);
        let mut registry = BackendRegistry::new(default, here.clone());
        let remote = Route::remote(Via::Ssh, "box", Multiplexer::Tmux);
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
        let extra = Route::remote(Via::Ssh, "box", Multiplexer::Psmux);
        registry.register(extra.clone(), stub(&extra));

        let mut routes: Vec<String> = registry.routes().map(Route::format).collect();
        routes.sort();
        assert_eq!(routes, ["local:tmux", "ssh:box:psmux"]);
        let mut names: Vec<&str> = registry.all_backends().map(|(_, b)| b.name()).collect();
        names.sort();
        assert_eq!(names, ["local:tmux", "ssh:box:psmux"]);
    }
}
