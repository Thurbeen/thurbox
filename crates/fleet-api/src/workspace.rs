//! Workspace: the files one Session works in, which the runtime makes.
//!
//! A Task names what its Session works in (`spec.workspace`): a list of
//! URLs. A Workspace is that list made real on a host by the host's runtime,
//! through its `workspace` verb: one directory per URL, under Fleet's own
//! deterministic `key`. One Task owns at most one Workspace, and its one
//! Session runs in it:
//!
//! ```text
//! Task ──owns──▶ Workspace {key, host, dirs} ◀──runs in── Session
//!   spec.workspace      status.workspace               status.session
//! ```
//!
//! A Workspace is not a stored kind. TaskController records it in the
//! Task's `status.workspace` and is its only writer, as it is the Session's.
//! Its lifecycle follows the Task's:
//!
//! 1. Before the first start, the controller plans `Materialize`, the
//!    runtime's `workspace` call, under the workspace key, the Session's key.
//! 2. The spawn receives the ready Workspace and makes nothing.
//! 3. After the release, the controller plans `CleanWorkspace`, the
//!    runtime's `stop` with `release`, and only then drops the release
//!    finalizer.
//!
//! The runtime port ([`crate::runtime::Runtime`]) is the only way Fleet
//! reaches a Workspace. Every call answers Known or Unknown, and Unknown
//! never concludes: the `workspace` call is idempotent by key, so a lost
//! answer is settled by asking again under the same key.
//!
//! The host is observed, not declared: `WorkspaceRef::host` is the host the
//! runtime made the Workspace on. Credentials stay with the host and its
//! runtime; Fleet sends none.

use serde::{Deserialize, Serialize};

/// A Workspace that exists: Fleet's `key`, the `host` the runtime made it
/// on, and the `dirs` it answered, one per URL in the Task's order. The
/// Session runs in the first and spans the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceRef {
    pub key: String,
    pub host: String,
    pub dirs: Vec<String>,
}

impl WorkspaceRef {
    /// Where the Session starts: the first directory.
    pub fn dir(&self) -> Option<&str> {
        self.dirs.first().map(String::as_str)
    }

    /// The other directories the Session spans, in the Task's order.
    pub fn extra_dirs(&self) -> &[String] {
        self.dirs.get(1..).unwrap_or_default()
    }
}
