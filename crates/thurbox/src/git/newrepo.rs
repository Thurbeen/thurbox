use std::path::Path;

use anyhow::{Context, Result};

use super::{git_program, non_interactive, run_git};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewRepo {
    Empty,
    Init,
    Clone { url: String },
}

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
        let _ = std::fs::remove_dir_all(path);
    }
    filled
}
