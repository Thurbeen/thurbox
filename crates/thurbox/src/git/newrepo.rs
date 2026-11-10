//! Making a directory for a repository that does not exist yet — empty,
//! `git init`-ed, or cloned into — on this machine.
//!
//! The creation flow offers all three once a typed path turns out not to exist.
//! Each is one call, so the flow issues a single command and nothing can land
//! between the `mkdir` and what goes into it. Two rules hold:
//!
//! - A path that exists is used only when it is an **empty directory** —
//!   anything else is someone's data, and is refused untouched.
//! - A failed `git` step removes the directory **only if this call created
//!   it**. One the user made themselves is not ours to take away. "Created"
//!   is decided by the create itself — a plain `mkdir` of the last component,
//!   which fails if anything is already there — never by an earlier look, so
//!   a directory that appears in between is still someone else's.

use std::path::Path;

use anyhow::{Context, Result};

use super::{git_program, non_interactive, run_git};

/// What goes into the new directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewRepo {
    /// Nothing: a plain directory.
    Empty,
    /// `git init`.
    Init,
    /// `git clone <url>` into it, without ever prompting.
    Clone { url: String },
}

/// Create `path` and fill it as `what` says. Blocking — call from a worker
/// thread.
pub fn create_repo_dir(path: &Path, what: &NewRepo) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let created = match std::fs::create_dir(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let empty = path.is_dir()
                && std::fs::read_dir(path)
                    .with_context(|| format!("read {}", path.display()))?
                    .next()
                    .is_none();
            if !empty {
                anyhow::bail!("{} already exists and is not empty", path.display());
            }
            false
        }
        Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
    };
    let filled = match what {
        NewRepo::Empty => Ok(()),
        NewRepo::Init => {
            let mut cmd = git_program();
            cmd.args(["init", "-q"]).arg(path);
            run_git(cmd, "git init")
        }
        NewRepo::Clone { url } => {
            let mut cmd = git_program();
            non_interactive(&mut cmd);
            // `--` so a URL can never be read as an option, whatever validated it.
            cmd.args(["clone", "-q", "--", url]).arg(path);
            run_git(cmd, "git clone")
        }
    };
    if filled.is_err() && created {
        // Best effort: the error being returned is the git one, and that is the
        // one worth reporting.
        let _ = std::fs::remove_dir_all(path);
    }
    filled
}
