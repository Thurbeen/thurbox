//! Spawning a session headlessly and handing it a prompt once its agent is up:
//! what a task run and a spawn automation both do (issue #1468 gives each verb
//! one owner, and this is the step the two share).

use crate::session::SessionId;
use crate::storage::Database;

use super::SpawnRequest;

/// Seconds to wait after a headless spawn before delivering the prompt, giving
/// the agent CLI time to start.
pub const BOOT_DELAY_SECS: u64 = 3;

/// Why a [`spawn_and_deliver`] attempt didn't fully succeed. The split lets each
/// caller preserve its own behavior: a spawn failure leaves no session, while a
/// delivery failure still created one (callers that record a session id keep it).
#[derive(Debug)]
pub enum SpawnDeliverError {
    /// The spawn itself failed; no session was created.
    Spawn(super::OpError),
    /// The session spawned but the delayed prompt delivery failed.
    Deliver {
        session_id: SessionId,
        message: String,
    },
}

/// Wire the built-in extensions just before a command launches an agent.
///
/// The TUI's boot and the heartbeat's tick each run the install, but a machine
/// driven by `thurbox-cli` alone reaches its first spawn before either has: the
/// heartbeat is armed only *after* `session create`, so that first session
/// launched without claude's `--settings` and never reported a state. `create`
/// and the task path reach it through [`ensure_hooks_wired_for`] once the
/// request validates; `restart`, `start` and `fork` call it unconditionally.
pub fn ensure_hooks_wired(db: &Database, backends: &crate::backend::BackendRegistry) {
    for m in &super::ensure_builtin_extensions(db, backends) {
        tracing::info!("{m}");
    }
}

/// [`ensure_hooks_wired`] for a new session, once `req` would be accepted: a
/// request the spawn refuses (a task title with a `/` is an unsafe session
/// name) must not change the agent config either.
pub fn ensure_hooks_wired_for(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    req: &SpawnRequest,
) -> Result<(), crate::session_ops::OpError> {
    super::spawn::validate_request(db, req)?;
    ensure_hooks_wired(db, backends);
    Ok(())
}

/// Spawn a fresh headless session for `req` and deliver `prompt` once the agent
/// boots (after [`BOOT_DELAY_SECS`]). Returns the new session id on success.
pub fn spawn_and_deliver(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    name: &str,
    req: SpawnRequest,
    prompt: &str,
) -> Result<SessionId, SpawnDeliverError> {
    ensure_hooks_wired_for(db, backends, &req).map_err(SpawnDeliverError::Spawn)?;
    let spawned =
        super::spawn_session_headless(db, backends, req).map_err(SpawnDeliverError::Spawn)?;
    let session_id = spawned.session_id;
    super::send_text_when_booted(
        db,
        backends,
        session_id,
        prompt,
        std::time::Duration::from_secs(BOOT_DELAY_SECS),
    )
    .map_err(|e| SpawnDeliverError::Deliver {
        session_id,
        message: format!("spawned {name} but prompt delivery failed: {e}"),
    })?;
    Ok(session_id)
}
