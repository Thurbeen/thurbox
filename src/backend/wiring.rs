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
use crate::session::HostRegistry;

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
    let local: Arc<dyn SessionBackend> = Arc::new(TmuxBackend::new());
    let mut backends = BackendRegistry::new(local);
    let (hosts, warnings) = crate::agent::host_config::cached_registry();
    let hosts = hosts.clone();
    for host in &hosts.hosts {
        let mut routed = host.clone();
        if matches!(routed.mux().as_str(), "rmux" | "herdr") {
            routed.multiplexer = Some("tmux".into());
        }
        backends.register(Arc::new(TmuxBackend::from_host(&routed)));
    }
    (backends, hosts, warnings.clone())
}
