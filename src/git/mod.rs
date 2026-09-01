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

/// Look up an open PR for the worktree's current branch via `gh pr view`.
///
/// Returns `Some(pr_number)` only when the PR is OPEN — merged and closed PRs
/// report `None`, and any `gh` failure (missing binary, no auth, no remote, no
/// PR) is a `None` too. Best-effort by design: the session-delete confirmation
/// treats absence as "no reason to ask".
pub fn open_pr_number(worktree_path: &Path) -> Option<u64> {
    let output = Command::new("gh")
        .args(["pr", "view", "--json", "state,number"])
        .current_dir(worktree_path)
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let body = std::str::from_utf8(&output.stdout).ok()?;
    if !body.contains("\"state\":\"OPEN\"") {
        return None;
    }

    let after = body.split("\"number\":").nth(1)?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

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
