//! Workspace: the files one Session works in, and the port through which
//! Fleet makes and removes them.
//!
//! A Task names what its Session works in (`spec.workspace`). A Workspace is
//! that name made real on a host: a directory tree, under Fleet's own
//! deterministic `key`. One Task owns at most one Workspace, and its one
//! Session runs in it:
//!
//! ```text
//! Task ──owns──▶ Workspace {key, host, dir} ◀──runs in── Session
//!   spec.workspace     status.workspace                status.session
//! ```
//!
//! A Workspace is not a stored kind. TaskController records it in the
//! Task's `status.workspace` and is its only writer, as it is the Session's.
//! Its lifecycle follows the Task's:
//!
//! 1. Before the spawn, the controller plans `Materialize` under the
//!    workspace key, the Session's key.
//! 2. The spawn receives the ready Workspace and makes nothing.
//! 3. After the release, the controller plans `CleanWorkspace`, and only then
//!    drops the release finalizer.
//!
//! The [`Workspaces`] port is the only way Fleet reaches a Workspace. Every
//! call answers Known or Unknown, and Unknown never concludes: a lost
//! materialize is answered by [`Workspaces::find`] under the key, never by a
//! second blind materialize.
//!
//! The host is observed, not declared: `WorkspaceRef::host` is the host the
//! adapter reports it made the Workspace on. Credentials stay with the host
//! and the adapter; Fleet sends none.

use serde::{Deserialize, Serialize};

use crate::observation::Observation;
use crate::plan::EffectOutcome;
use crate::task::RepositoriesWorkspace;

/// A Workspace that exists: Fleet's `key`, the `host` the adapter made it
/// on, the `dir` the Session runs in there, and the `extra_dirs` it also
/// spans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceRef {
    pub key: String,
    pub host: String,
    pub dir: String,
    /// The other directories the Session spans as they are, in the order
    /// the Task names them: the worktrees of repositories 2..n.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_dirs: Vec<String>,
    /// The directory an agent keys its workspace trust on, when it is not
    /// `dir`: the primary repository's source checkout, for a worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_root: Option<String>,
}

/// The workspace port. An adapter makes a Workspace with bounded commands
/// run on its host, so one adapter shape serves a local, SSH, container or
/// VM host.
pub trait Workspaces {
    /// The longest one call of this trait can take, every bound included:
    /// the manager's lease is sized to outlast it.
    fn operation_bound(&self) -> crate::Millis {
        0
    }
    /// Make the Workspace `key` on `host` from `repositories`, whole or
    /// nothing. `Materialized { adopted: true }` when a whole one already
    /// holds the key. `Unmapped { url }` names a repository the adapter has
    /// no source for, answered before any command runs. `Failed` is a
    /// refusal that left nothing behind. Unknown when the adapter cannot
    /// tell: the next pass asks `find`.
    fn materialize(
        &self,
        host: &str,
        key: &str,
        repositories: &RepositoriesWorkspace,
    ) -> Observation<EffectOutcome>;
    /// The whole Workspace under `key` on `host`. `Known(None)` when the
    /// host proves none is whole: a partial one is not a Workspace, and the
    /// next `materialize` rolls it back. Unknown when it cannot tell.
    fn find(&self, host: &str, key: &str) -> Observation<Option<WorkspaceRef>>;
    /// Remove the Workspace and everything in it. One already gone is
    /// `Deleted`. Repeating it is harmless.
    fn clean(&self, workspace: &WorkspaceRef) -> Observation<EffectOutcome>;
}
