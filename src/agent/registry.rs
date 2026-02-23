//! Backend registry for multi-backend session support.
//!
//! Allows multiple `SessionBackend` implementations to coexist. Sessions select
//! their backend at creation time; the registry routes by name.

use std::collections::HashMap;
use std::sync::Arc;

use crate::agent::SessionBackend;

/// A registry of session backends keyed by name.
///
/// Each backend is registered under its `name()` (e.g., `"local-tmux"`, `"qemu-vm"`).
/// The registry always has a default backend that is used when no explicit backend
/// name is specified.
pub struct BackendRegistry {
    backends: HashMap<String, Arc<dyn SessionBackend>>,
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
            default_name: name,
        }
    }

    /// Register an additional backend. Its `name()` is used as the key.
    pub fn register(&mut self, backend: Arc<dyn SessionBackend>) {
        let name = backend.name().to_string();
        self.backends.insert(name, backend);
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

    /// Return the name of the default backend.
    pub fn default_name(&self) -> &str {
        &self.default_name
    }

    /// Iterate over all registered backend names.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.backends.keys().map(|s| s.as_str())
    }

    /// Iterate over all registered backends.
    pub fn all_backends(&self) -> impl Iterator<Item = &Arc<dyn SessionBackend>> {
        self.backends.values()
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
        fn adopt(&self, _: &str, _: u16, _: u16) -> Result<AdoptedSession> {
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
        fn prepare_vm(&self, _vm_id: &str) -> Result<()> {
            Ok(())
        }
        fn default_shell(&self) -> String {
            "/bin/sh".to_string()
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

        let vm: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "qemu-vm",
        });
        registry.register(vm);

        assert!(registry.has("qemu-vm"));
        assert_eq!(registry.get("qemu-vm").unwrap().name(), "qemu-vm");
        // Default is still local-tmux
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

        let vm: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "qemu-vm",
        });
        registry.register(vm);

        let mut names: Vec<&str> = registry.names().collect();
        names.sort();
        assert_eq!(names, vec!["local-tmux", "qemu-vm"]);
    }

    #[test]
    fn all_backends_returns_all_registered() {
        let default: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "local-tmux",
        });
        let mut registry = BackendRegistry::new(default);

        let vm: Arc<dyn SessionBackend> = Arc::new(StubBackend {
            backend_name: "qemu-vm",
        });
        registry.register(vm);

        let mut names: Vec<&str> = registry.all_backends().map(|b| b.name()).collect();
        names.sort();
        assert_eq!(names, vec!["local-tmux", "qemu-vm"]);
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
}
