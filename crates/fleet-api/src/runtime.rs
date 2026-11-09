//! The runtime port: the calls a Task's lifecycle makes on an agent host.
//! Every call answers Known or Unknown, and Unknown never concludes.
//!
//! Hosts are the runtime's names (`Task.spec.host`). Keys are fleet's session
//! keys; an adapter may store them under its own prefix, and hands them back
//! unprefixed.

use crate::observation::Observation;
use crate::plan::{EffectOutcome, SpawnSpec};
use crate::task::{SessionRef, Workspace};

/// Whether a session's agent trusts the workspace it runs in. Workspace
/// trust is the agent's own gate on loading a folder's project settings; it
/// is not a permission policy and approves no tool action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceTrust {
    /// The agent trusts its workspace, or its harness has no trust gate;
    /// `how` says which, for people.
    Trusted { how: String },
    /// The agent does not trust its workspace, and only a person can change
    /// that; `why` says what to do.
    Untrusted { gap: TrustGap, why: String },
}

/// Why a workspace is known not to be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustGap {
    /// The harness has no supported way to be trusted for one session.
    Unsupported,
    /// The session is not the one the runtime would have started for this
    /// workspace: another trust, none, or another directory.
    Mismatch,
}

impl TrustGap {
    pub fn reason(self) -> &'static str {
        match self {
            TrustGap::Unsupported => "Unsupported",
            TrustGap::Mismatch => "Mismatch",
        }
    }
}

/// What the agent in a running session is doing, as its own hooks report
/// it through `fleet signal` ([`crate::signal`]): a runtime knows nothing
/// of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// The agent works on a turn, or holds background work that a stop
    /// would end.
    Active,
    /// The agent waits for input, with no background work to lose.
    Idle,
}

pub trait Runtime {
    /// Resolve runtime model-variable selection into the frozen private environment.
    fn prepare_execution(
        &self,
        execution: &crate::config::Execution,
    ) -> Observation<crate::config::Execution> {
        Observation::Known(execution.clone())
    }

    fn env_source(
        &self,
        _source: &crate::config::EnvSource,
    ) -> Observation<std::collections::BTreeMap<String, String>> {
        Observation::unknown("environment sources unsupported by this runtime")
    }

    /// The longest one call of this trait can take, every bound and grace
    /// included: the manager's lease is sized to outlast it.
    fn operation_bound(&self) -> crate::Millis {
        0
    }
    /// Whether the host answers. Unknown when the probe cannot tell.
    fn host(&self, host: &str) -> Observation<bool>;
    /// Make the directory `dir` on `host`, with its missing parents, before
    /// a Task names it: `Ok(true)` when this call made it, `Ok(false)` when
    /// it was there already. `Err` says why the runtime cannot make it;
    /// Unknown when it cannot tell whether it did. Repeating it is harmless.
    fn create_dir(&self, host: &str, dir: &str) -> Observation<Result<bool, String>>;
    /// Remove the directory `dir` on `host` and everything in it. A
    /// directory that is gone already is `Ok`. `Err` says why the runtime
    /// cannot remove it; Unknown when it cannot tell whether it did.
    /// Repeating it is harmless.
    fn remove_dir(&self, host: &str, dir: &str) -> Observation<Result<(), String>>;
    /// `Spawned`, or `Spawned { adopted: true }` when a live session already
    /// holds the key. Unknown on timeout: the next pass looks the key up with
    /// `find` and never retries blindly.
    fn spawn(&self, spec: &SpawnSpec) -> Observation<EffectOutcome>;
    /// The session holding `key`, `Known(None)` when a readable list shows
    /// none, Unknown when the list cannot be read.
    fn find(&self, host: &str, key: &str) -> Observation<Option<SessionRef>>;
    /// Ready when the agent can take a message now: a working turn can, a
    /// dialog that holds the agent cannot.
    fn ready(&self, session: &SessionRef) -> Observation<bool>;
    /// Whether the agent in `session` trusts `workspace`, or `ready` when
    /// Fleet made the workspace, so a brief cannot land in a trust prompt.
    /// Unknown when the session cannot prove it.
    fn workspace_trust(
        &self,
        session: &SessionRef,
        workspace: &Workspace,
        ready: Option<&crate::workspace::WorkspaceRef>,
    ) -> Observation<WorkspaceTrust>;
    /// Send the brief once under `intent_key`; the session stores the key, so
    /// a second send of the same key is `Delivered { already: true }`.
    fn brief(
        &self,
        session: &SessionRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome>;
    /// Read the intent key back: did a brief under it arrive?
    fn brief_delivered(&self, session: &SessionRef, intent_key: &str) -> Observation<bool>;
    /// The agent finished its turn and waits. Nothing is deleted unless this
    /// is Known(true).
    fn at_rest(&self, session: &SessionRef) -> Observation<bool>;
    /// Delete the session once; repeating it is a no-op.
    fn release(&self, session: &SessionRef) -> Observation<EffectOutcome>;
    /// Known(true) only when the runtime still holds the session but proves
    /// that its agent's window is gone, as after a host reboot, and nobody
    /// stopped it on purpose. A running or stopped session is Known(false).
    fn lost(&self, session: &SessionRef) -> Observation<bool>;
    /// Start a lost session's agent again in its own workspace, resuming its
    /// conversation under the same identity: the harness profile's resume
    /// argv names `conversation`, when the agent's hooks reported one, and
    /// `launch` is what its spawn started, for a runtime that starts the
    /// argv itself. A running session is left alone, so repeating it is
    /// harmless.
    fn revive(
        &self,
        session: &SessionRef,
        conversation: Option<&str>,
        launch: &SpawnSpec,
    ) -> Observation<EffectOutcome>;
    /// Whether this runtime can pause a session and resume it. Fleet never
    /// pauses a session on a runtime that cannot.
    fn can_pause(&self) -> bool {
        false
    }
    /// Known(true) when the runtime holds the session and its agent is
    /// stopped, by Fleet or by anyone else. A running or lost session is
    /// Known(false).
    fn paused(&self, _session: &SessionRef) -> Observation<bool> {
        Observation::unknown("this runtime cannot tell a stopped session")
    }
    /// Stop the agent of `session`, and keep its row, workspace and
    /// conversation. A stopped session is left alone, so repeating it is
    /// harmless.
    fn pause(&self, _session: &SessionRef) -> Observation<EffectOutcome> {
        Observation::Known(EffectOutcome::Failed {
            why: "this runtime cannot pause a session".into(),
        })
    }
    /// Start the stopped agent of `session` again in its own workspace,
    /// resuming `conversation` under the same identity, as for `revive`. A
    /// running session is left alone, so repeating it is harmless.
    fn resume(
        &self,
        _session: &SessionRef,
        _conversation: Option<&str>,
        _launch: &SpawnSpec,
    ) -> Observation<EffectOutcome> {
        Observation::Known(EffectOutcome::Failed {
            why: "this runtime cannot resume a session".into(),
        })
    }
}
