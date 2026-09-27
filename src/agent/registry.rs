//! Backend registry for multi-backend session support.
//!
//! Allows multiple `SessionBackend` implementations to coexist. Sessions select
//! their backend at creation time; the registry routes by name.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::agent::SessionBackend;
use crate::session::HostRegistry;

/// A registry of session backends keyed by name.
///
/// Each backend is registered under its `name()` (e.g., `"local-tmux"`, `"extra-backend"`).
/// The registry always has a default backend that is used when no explicit backend
/// name is specified.
pub struct BackendRegistry {
    backends: HashMap<String, Arc<dyn SessionBackend>>,
    ambiguous_routes: HashSet<String>,
    default_name: String,
}

impl BackendRegistry {
    /// Create a new registry with the given backend as the default.
    pub fn new(default: Arc<dyn SessionBackend>) -> Self {
        let name = default.name().to_string();
        let mut backends = HashMap::new();
        backends.insert(name.clone(), default);
        Self {
            backends,
            ambiguous_routes: HashSet::new(),
            default_name: name,
        }
    }

    /// Register local and host routes without connecting to remote hosts.
    /// Both SSH mux routes remain available after a host preference changes;
    /// the unsuffixed alias follows the current preference for legacy rows.
    /// Return the hosts from the same config read for launch and path lookup.
    pub fn from_configured_hosts() -> (Self, HostRegistry, Vec<String>) {
        let (hosts, warnings) = crate::agent::host_config::cached_registry();
        let hosts = hosts.clone();
        let backends = Self::from_host_registry(&hosts);
        (backends, hosts, warnings.clone())
    }

    /// Build routes from an already resolved host registry without contacting hosts.
    pub fn from_host_registry(hosts: &HostRegistry) -> Self {
        let local: Arc<dyn SessionBackend> = if cfg!(windows) {
            Arc::new(crate::agent::psmux::PsmuxBackend::local())
        } else {
            Arc::new(crate::agent::tmux::LocalTmuxBackend::new())
        };
        let mut backends = Self::new(local);
        if cfg!(windows) {
            backends.register_alias(
                crate::session::LOCAL_BACKEND_TYPE,
                backends.default_backend().clone(),
            );
        }
        // A legacy alias can collide with another host's qualified route.
        let mut legacy_aliases = Vec::new();
        for host in &hosts.hosts {
            if host.is_wsl() {
                backends.register(Arc::new(crate::agent::tmux::TmuxBackend::from_host(host)));
                continue;
            }
            let mut tmux_host = host.clone();
            tmux_host.multiplexer = Some("tmux".into());
            let mut tmux = crate::agent::tmux::TmuxBackend::from_host(&tmux_host);
            tmux.set_name(format!("{}:tmux", host.backend_name()));
            let tmux: Arc<dyn SessionBackend> = Arc::new(tmux);

            let mut psmux_host = host.clone();
            psmux_host.multiplexer = Some("psmux".into());
            let psmux: Arc<dyn SessionBackend> =
                Arc::new(crate::agent::psmux::PsmuxBackend::from_host(&psmux_host));

            let legacy = if host.mux() == "psmux" {
                psmux.clone()
            } else {
                tmux.clone()
            };
            backends.register(tmux);
            backends.register(psmux);
            legacy_aliases.push((host.backend_name(), legacy));
        }
        for (name, backend) in legacy_aliases {
            if backends.ambiguous_routes.contains(&name) {
                continue;
            }
            if backends.has(&name) {
                // The persisted key cannot identify which host owns it. Fail closed.
                backends.backends.remove(&name);
                backends.ambiguous_routes.insert(name);
            } else {
                backends.register_alias(name, backend);
            }
        }
        backends
    }

    /// Register an additional backend. Its `name()` is used as the key.
    pub fn register(&mut self, backend: Arc<dyn SessionBackend>) {
        let name = backend.name().to_string();
        self.backends.insert(name, backend);
    }

    fn register_alias(&mut self, name: impl Into<String>, backend: Arc<dyn SessionBackend>) {
        self.backends.insert(name.into(), backend);
    }

    /// Look up a backend by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn SessionBackend>> {
        self.backends.get(name)
    }

    /// Return the default backend.
    pub fn default_backend(&self) -> &Arc<dyn SessionBackend> {
        self.backends.get(&self.default_name).unwrap()
    }

    /// Check whether a backend with the given name is registered.
    pub fn has(&self, name: &str) -> bool {
        self.backends.contains_key(name)
    }

    /// Whether a persisted key could name two different hosts.
    pub fn is_ambiguous_route(&self, name: &str) -> bool {
        self.ambiguous_routes.contains(name)
    }

    /// A route is selectable only if its registered backend matches the chosen host.
    pub fn supports_choice(&self, choice: &crate::session::BackendChoice) -> bool {
        if choice.host.is_none()
            && choice.backend_type == crate::session::LOCAL_BACKEND_TYPE
            && choice.multiplexer != crate::session::Multiplexer::platform_default()
        {
            return false;
        }
        let Some(backend) = self.get(&choice.backend_type) else {
            return false;
        };
        match choice.host.as_ref() {
            Some(host) if !host.is_wsl() => {
                backend.name() == format!("{}:{}", host.backend_name(), choice.multiplexer.name())
            }
            _ => true,
        }
    }

    /// Return the name of the default backend.
    pub fn default_name(&self) -> &str {
        &self.default_name
    }

    /// Iterate over all registered backend names.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.backends.keys().map(|s| s.as_str())
    }

    /// Iterate over canonical registrations once, excluding persisted-key aliases.
    pub fn all_backends(&self) -> impl Iterator<Item = &Arc<dyn SessionBackend>> {
        self.backends
            .iter()
            .filter(|(name, backend)| name.as_str() == backend.name())
            .map(|(_, backend)| backend)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use anyhow::Result;

    use super::*;
    use crate::agent::backend::{AdoptedSession, DiscoveredSession, SpawnedSession};

    struct StubBackend {
        backend_name: &'static str,
    }

    impl SessionBackend for StubBackend {
        fn name(&self) -> &str {
            self.backend_name
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
            _: &HashMap<String, String>,
            _: u16,
            _: u16,
        ) -> Result<SpawnedSession> {
            unimplemented!()
        }
        fn spawn_headless(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<&Path>,
            _: &HashMap<String, String>,
        ) -> Result<String> {
            anyhow::bail!("stub backend cannot spawn")
        }
        fn adopt(&self, _: &str, _: u16, _: u16, _: Option<Vec<u8>>) -> Result<AdoptedSession> {
            unimplemented!()
        }
        fn discover(&self) -> Result<Vec<DiscoveredSession>> {
            Ok(vec![])
        }
        fn headless_discover(&self) -> Result<Vec<DiscoveredSession>> {
            self.discover()
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

    #[test]
    fn new_registers_default() {
        let backend: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "local-tmux",
        });
        let registry = BackendRegistry::new(backend);

        assert!(registry.has("local-tmux"));
        assert_eq!(registry.default_name(), "local-tmux");
        assert_eq!(registry.default_backend().name(), "local-tmux");
    }

    #[test]
    fn register_additional_backend() {
        let default: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "local-tmux",
        });
        let mut registry = BackendRegistry::new(default);

        let extra: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "extra-backend",
        });
        registry.register(extra);

        assert!(registry.has("extra-backend"));
        assert_eq!(
            registry.get("extra-backend").unwrap().name(),
            "extra-backend"
        );
        assert_eq!(registry.default_name(), "local-tmux");
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let default: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "local-tmux",
        });
        let registry = BackendRegistry::new(default);

        assert!(registry.get("nonexistent").is_none());
        assert!(!registry.has("nonexistent"));
    }

    #[test]
    fn names_returns_all_registered() {
        let default: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "local-tmux",
        });
        let mut registry = BackendRegistry::new(default);

        let extra: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "extra-backend",
        });
        registry.register(extra);

        let mut names: Vec<&str> = registry.names().collect();
        names.sort();
        assert_eq!(names, vec!["extra-backend", "local-tmux"]);
    }

    #[test]
    fn all_backends_returns_all_registered() {
        let default: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "local-tmux",
        });
        let mut registry = BackendRegistry::new(default);

        let extra: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "extra-backend",
        });
        registry.register(extra);

        let mut names: Vec<&str> = registry.all_backends().map(|b| b.name()).collect();
        names.sort();
        assert_eq!(names, vec!["extra-backend", "local-tmux"]);
    }

    #[test]
    fn all_backends_single_default() {
        let default: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "local-tmux",
        });
        let registry = BackendRegistry::new(default);

        let backends: Vec<_> = registry.all_backends().collect();
        assert_eq!(backends.len(), 1);
        assert_eq!(backends[0].name(), "local-tmux");
    }

    #[test]
    fn persisted_route_aliases_do_not_duplicate_backend_iteration() {
        let default: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "local-psmux",
        });
        let mut registry = BackendRegistry::new(default.clone());
        registry.register_alias("local-tmux", default);
        assert_eq!(registry.all_backends().count(), 1);
    }
}
