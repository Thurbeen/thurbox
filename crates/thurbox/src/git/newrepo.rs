//! Making a directory for a repository that does not exist yet — empty,
//! `git init`-ed, or cloned into — locally and over a transport.
//!
//! The creation flow offers all three once a typed path turns out not to exist.
//! Each is one call and one round trip, so the flow issues a single command and
//! nothing can land between the `mkdir` and what goes into it. Two rules hold on
//! every path, POSIX script and PowerShell alike:
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

use super::{
    git_program, host_probe, non_interactive, powershell_quote, remote_output_or_stderr, run_git,
};
use crate::session::HostDef;
use crate::shell::posix_quote;

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

/// Create `path` on `host` (local when `None`) and fill it as `what` says.
/// Blocking — call from a worker thread.
pub fn create_repo_dir(host: Option<&HostDef>, path: &Path, what: &NewRepo) -> Result<()> {
    match host {
        Some(host) => {
            let path = path.to_string_lossy();
            let output = host_probe(
                host,
                &create_repo_dir_script(&path, what),
                &create_repo_dir_script_windows(&path, what),
            )
            .output()
            .context("failed to reach the host")?;
            remote_output_or_stderr(output, "repository creation").map(|_| ())
        }
        None => create_local(path, what),
    }
}

fn create_local(path: &Path, what: &NewRepo) -> Result<()> {
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

/// The POSIX script for [`create_repo_dir`] on a host (`path` already
/// tilde-expanded). Refusals and git's own complaints go to stderr, which is
/// what [`remote_output_or_stderr`] reports.
pub(super) fn create_repo_dir_script(path: &str, what: &NewRepo) -> String {
    let fill = match what {
        NewRepo::Empty => "true".to_string(),
        NewRepo::Init => "git init -q \"$p\"".to_string(),
        // BatchMode for the same reason `non_interactive` sets it locally: a
        // prompt on a host nobody is looking at is a hang, not a question.
        NewRepo::Clone { url } => format!(
            "GIT_TERMINAL_PROMPT=0 GIT_SSH_COMMAND='ssh -o BatchMode=yes' \
             git clone -q -- {} \"$p\"",
            posix_quote(url)
        ),
    };
    // `mkdir` without `-p` for the last component: it fails if anything is
    // already there, which is what makes `made` a fact rather than a guess.
    format!(
        "p={q}; made=0; \
         mkdir -p -- \"$(dirname -- \"$p\")\" || exit 1; \
         if mkdir -- \"$p\" 2>/dev/null; then made=1; \
         elif [ ! -e \"$p\" ]; then echo \"cannot create $p\" >&2; exit 1; \
         elif [ ! -d \"$p\" ] || [ -n \"$(ls -A \"$p\")\" ]; then \
           echo \"$p already exists and is not empty\" >&2; exit 3; \
         fi; \
         if ! {fill}; then [ \"$made\" = 1 ] && rm -rf \"$p\"; exit 1; fi",
        q = posix_quote(path),
    )
}

/// The [`create_repo_dir_script`] twin for a **native-Windows** host.
pub(super) fn create_repo_dir_script_windows(path: &str, what: &NewRepo) -> String {
    let fill = match what {
        NewRepo::Empty => String::new(),
        NewRepo::Init => "git init -q $p\n".to_string(),
        NewRepo::Clone { url } => format!(
            "$env:GIT_TERMINAL_PROMPT='0'\n\
             $env:GIT_SSH_COMMAND='ssh -o BatchMode=yes'\n\
             git clone -q -- {} $p\n",
            powershell_quote(url)
        ),
    };
    // No `-Force`, and `-ErrorAction Stop`: the create fails if anything is
    // there (so `$made` is a fact), and a refusal such as a permission error
    // lands in `catch` rather than passing as a success with no folder.
    format!(
        "$p={q}\n\
         $made=$false\n\
         try {{ New-Item -ItemType Directory -Path $p -ErrorAction Stop | Out-Null; $made=$true }}\n\
         catch {{\n\
           if (-not (Test-Path -LiteralPath $p)) {{\n\
             [Console]::Error.WriteLine(\"cannot create ${{p}}: $($_.Exception.Message)\"); exit 1\n\
           }}\n\
           if (-not (Test-Path -LiteralPath $p -PathType Container) -or \
               (Get-ChildItem -LiteralPath $p -Force | Select-Object -First 1)) {{\n\
             [Console]::Error.WriteLine(\"$p already exists and is not empty\"); exit 3\n\
           }}\n\
         }}\n\
         $global:LASTEXITCODE=0\n\
         {fill}\
         if ($LASTEXITCODE -ne 0) {{\n\
           if ($made) {{ Remove-Item -LiteralPath $p -Recurse -Force }}\n\
           exit 1\n\
         }}\n\
         exit 0",
        q = powershell_quote(path),
    )
}
