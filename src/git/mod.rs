//! Every `git` invocation thurbox makes, local or on a remote host.
//!
//! Split by concern — `command` builds the process (and scrubs the inherited
//! `GIT_*` that would silently retarget it), `plugin` clones a plugin's working
//! copy, `remote` runs commands over ssh/`wsl.exe` and decodes what came back,
//! `discovery` answers "what is here", `diff` produces diffs and stats, and
//! `worktree` creates, syncs and removes the checkouts sessions live in.
//! `git::*` is one flat surface; no caller names a submodule.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::warn;

use crate::paths;
use crate::session::HostDef;
use crate::shell::posix_quote;

mod command;
mod diff;
mod discovery;
mod plugin;
mod remote;
mod worktree;

#[cfg(test)]
mod tests;

// One flat `git::*` surface, as before: the split is by file, and no caller
// outside this module names a submodule.
//
// A glob re-export clamps each item to its own visibility rather than widening
// it, which is what makes one line per file enough: `pub` items stay the public
// API they were, and the `pub(super)` helpers a sibling needs ride the same
// glob without becoming anybody else's business. Naming them individually would
// have meant an import list per file, kept in step by hand.
// `command` has no `pub` items — its widest is `pub(crate)` — so this glob
// says so rather than claiming a public surface it does not have.
pub(crate) use command::*;
pub use diff::*;
pub use discovery::*;
pub use plugin::*;
pub use remote::*;
pub use worktree::*;
