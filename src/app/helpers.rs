//! Miscellaneous helper functions used by the app module.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::info;

use crate::git;
use crate::session::{SessionInfo, WorktreeInfo};

pub(super) fn open_url(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok();
}

/// Spawn `editor_cmd` with `worktree` appended as the final argument.
///
/// `editor_cmd` is whitespace-split so callers can include flags
/// (e.g. `"code --wait"` or `"nvim --server /tmp/s --remote"`). Returns an
/// error if the command string is empty or fails to spawn.
pub(super) fn open_in_editor(paths: &[PathBuf], editor_cmd: &str) -> std::io::Result<()> {
    if paths.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no paths to open",
        ));
    }
    let mut parts = editor_cmd.split_whitespace();
    let Some(program) = parts.next() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "editor command is empty",
        ));
    };
    let extra_args: Vec<&str> = parts.collect();

    std::process::Command::new(program)
        .args(&extra_args)
        .args(paths)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Resolve the editor command from the DB setting, falling back to
/// `$VISUAL` then `$EDITOR`. Returns `None` if none are set.
pub(super) fn resolve_editor_command(db: &crate::storage::Database) -> Option<String> {
    if let Ok(Some(cmd)) = db.get_editor_command() {
        let trimmed = cmd.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Write a `.mcp.json` file into a VM directory via SSH so Claude Code discovers
/// the project's MCP servers when running inside the sandbox.
pub(super) fn write_vm_mcp_json(
    instance: &crate::agent::vm::VmInstance,
    vm_cwd: &std::path::Path,
    mcp_servers: &[crate::session::McpServerConfig],
) -> anyhow::Result<()> {
    if mcp_servers.is_empty() {
        return Ok(());
    }

    let doc = crate::session::McpServerConfig::to_mcp_json(mcp_servers);
    let json_str = serde_json::to_string_pretty(&doc)?;

    let key_path = instance.ssh_key_path();
    let ssh_port = instance.ssh_port.to_string();
    let user = instance.ssh_user();
    let dest = vm_cwd.join(".mcp.json");

    let shell_cmd = format!("cat > '{}'", dest.to_string_lossy().replace('\'', "'\\''"));
    let output = std::process::Command::new("ssh")
        .args([
            "-i",
            &key_path.to_string_lossy(),
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "LogLevel=ERROR",
            "-o",
            "BatchMode=yes",
            "-p",
            &ssh_port,
            &format!("{user}@localhost"),
            &shell_cmd,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(json_str.as_bytes())?;
            }
            child.wait_with_output()
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ssh write .mcp.json failed: {stderr}");
    }

    info!("Wrote .mcp.json into VM at {}", dest.display());
    Ok(())
}

/// Write a `.mcp.json` file into a container directory via `docker exec` so Claude Code
/// discovers the project's MCP servers when running inside the devcontainer.
pub(super) fn write_container_mcp_json(
    runtime: &str,
    docker_container_id: &str,
    container_cwd: &std::path::Path,
    mcp_servers: &[crate::session::McpServerConfig],
) -> anyhow::Result<()> {
    if mcp_servers.is_empty() {
        return Ok(());
    }

    let docker_id = docker_container_id;

    let doc = crate::session::McpServerConfig::to_mcp_json(mcp_servers);
    let json_str = serde_json::to_string_pretty(&doc)?;

    let dest = container_cwd.join(".mcp.json");
    let shell_cmd = format!("cat > '{}'", dest.to_string_lossy().replace('\'', "'\\''"));
    let output = std::process::Command::new(runtime)
        .args(["exec", "-i", docker_id, "sh", "-c", &shell_cmd])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(json_str.as_bytes())?;
            }
            child.wait_with_output()
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{runtime} exec write .mcp.json failed: {stderr}");
    }

    info!("Wrote .mcp.json into container at {}", dest.display());
    Ok(())
}

/// Build a system prompt snippet that gives the Claude Code agent context about
/// its Thurbox-managed environment: repositories, worktrees, branches, and other
/// active sessions.
///
/// Returns an empty string when there is nothing useful to inject (no repos,
/// no other sessions).
pub(super) fn build_session_context_prompt(
    session_name: &str,
    role: &str,
    worktrees: &[WorktreeInfo],
    cwd: Option<&Path>,
    additional_dirs: &[PathBuf],
    other_sessions: &[&SessionInfo],
) -> String {
    // Nothing useful to say — skip injection entirely.
    let has_dirs = cwd.is_some() || !worktrees.is_empty() || !additional_dirs.is_empty();
    if !has_dirs && other_sessions.is_empty() {
        return String::new();
    }

    let mut out = String::from("# Thurbox Session Context\n");

    // -- This Session --
    out.push_str("\n## This Session\n");
    out.push_str(&format!("- **Name:** {session_name}\n"));
    out.push_str(&format!("- **Role:** {role}\n"));

    // -- Accessible Directories --
    // Lists every directory the agent can access: cwd (primary), worktree
    // checkouts, and additional dirs passed via --add-dir.
    if has_dirs {
        out.push_str("\n## Accessible Directories\n");

        if !worktrees.is_empty() {
            for wt in worktrees {
                let name = git::repo_display_name(&wt.repo_path)
                    .unwrap_or_else(|| wt.repo_path.display().to_string());
                out.push_str(&format!(
                    "- `{}` — **{name}** (branch: `{}`) worktree of `{}`\n",
                    wt.worktree_path.display(),
                    wt.branch,
                    wt.repo_path.display(),
                ));
            }
        } else if let Some(cwd) = cwd {
            let label = git::repo_display_name(cwd)
                .map(|name| format!(" — **{name}**"))
                .unwrap_or_default();
            out.push_str(&format!("- `{}`{label} (primary)\n", cwd.display()));
        }

        let wt_paths: HashSet<&Path> = worktrees
            .iter()
            .map(|wt| wt.worktree_path.as_path())
            .collect();
        for dir in additional_dirs {
            if !wt_paths.contains(dir.as_path()) && cwd.map_or(true, |c| c != dir) {
                let label = git::repo_display_name(dir)
                    .map(|name| format!(" — **{name}**"))
                    .unwrap_or_default();
                out.push_str(&format!("- `{}`{label}\n", dir.display()));
            }
        }
    }

    // -- Other Active Sessions --
    if !other_sessions.is_empty() {
        out.push_str("\n## Other Active Sessions\n");

        // Collect this session's repo display names for overlap detection.
        let my_repos: HashSet<String> = collect_my_repo_names(worktrees, cwd);

        let mut has_overlap = false;
        for sess in other_sessions {
            let sess_repos = session_repo_summary(sess);
            if !has_overlap && sess_repos.iter().any(|(name, _)| my_repos.contains(name)) {
                has_overlap = true;
            }
            let repo_part = if sess_repos.is_empty() {
                String::new()
            } else {
                let parts: Vec<String> = sess_repos
                    .iter()
                    .map(|(name, branch)| match branch {
                        Some(b) => format!("**{name}** (branch: `{b}`)"),
                        None => format!("**{name}**"),
                    })
                    .collect();
                format!(" on {}", parts.join(", "))
            };
            out.push_str(&format!("- \"{}\" [{}]{repo_part}\n", sess.name, sess.role));
        }

        if has_overlap {
            out.push_str(
                "\nCoordinate with other active sessions \
                 — avoid modifying overlapping files to prevent merge conflicts.\n",
            );
        }
    }

    out
}

/// Collect display names of repos for this session (for overlap detection).
fn collect_my_repo_names(worktrees: &[WorktreeInfo], cwd: Option<&Path>) -> HashSet<String> {
    let mut names = HashSet::new();
    for wt in worktrees {
        if let Some(name) = git::repo_display_name(&wt.repo_path) {
            names.insert(name);
        }
    }
    if worktrees.is_empty() {
        if let Some(cwd) = cwd {
            if let Some(name) = git::repo_display_name(cwd) {
                names.insert(name);
            }
        }
    }
    names
}

/// Summarize another session's repos as (display_name, optional_branch) pairs.
fn session_repo_summary(sess: &SessionInfo) -> Vec<(String, Option<String>)> {
    let mut entries = Vec::new();
    if !sess.worktrees.is_empty() {
        for wt in &sess.worktrees {
            if let Some(name) = git::repo_display_name(&wt.repo_path) {
                entries.push((name, Some(wt.branch.clone())));
            }
        }
    } else if !sess.repo_display_names.is_empty() {
        for name in &sess.repo_display_names {
            entries.push((name.clone(), None));
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to make a `SessionInfo` for testing.
    fn make_session(
        name: &str,
        role: &str,
        worktrees: Vec<WorktreeInfo>,
        repo_display_names: Vec<String>,
    ) -> SessionInfo {
        let mut info = SessionInfo::new(name.to_string());
        info.role = role.to_string();
        info.worktrees = worktrees;
        info.repo_display_names = repo_display_names;
        info
    }

    fn make_worktree(repo: &str, worktree: &str, branch: &str) -> WorktreeInfo {
        WorktreeInfo {
            repo_path: PathBuf::from(repo),
            worktree_path: PathBuf::from(worktree),
            branch: branch.to_string(),
        }
    }

    #[test]
    fn empty_when_no_context() {
        let result = build_session_context_prompt("test", "developer", &[], None, &[], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn includes_session_identity() {
        let wt = make_worktree("/repo", "/wt/repo", "main");
        let result = build_session_context_prompt("my-session", "architect", &[wt], None, &[], &[]);
        assert!(result.contains("**Name:** my-session"));
        assert!(result.contains("**Role:** architect"));
    }

    #[test]
    fn shows_worktree_dirs() {
        let wt = make_worktree("/home/user/thurbox", "/tmp/wt/abc", "feat-branch");
        let result = build_session_context_prompt("s1", "dev", &[wt], None, &[], &[]);
        assert!(result.contains("## Accessible Directories"));
        assert!(result.contains("`/tmp/wt/abc`"));
        assert!(result.contains("(branch: `feat-branch`)"));
        assert!(result.contains("worktree of `/home/user/thurbox`"));
    }

    #[test]
    fn shows_cwd_as_primary() {
        let cwd = Path::new("/home/user/myproject");
        let result = build_session_context_prompt("s1", "dev", &[], Some(cwd), &[], &[]);
        assert!(result.contains("## Accessible Directories"));
        assert!(result.contains("`/home/user/myproject`"));
        assert!(result.contains("(primary)"));
    }

    #[test]
    fn shows_additional_dirs() {
        let cwd = Path::new("/home/user/main-repo");
        let extra = vec![PathBuf::from("/home/user/extra-dir")];
        let result = build_session_context_prompt("s1", "dev", &[], Some(cwd), &extra, &[]);
        assert!(result.contains("`/home/user/main-repo`"));
        assert!(result.contains("`/home/user/extra-dir`"));
    }

    #[test]
    fn no_other_sessions_section_when_empty() {
        let wt = make_worktree("/repo", "/wt", "main");
        let result = build_session_context_prompt("s1", "dev", &[wt], None, &[], &[]);
        assert!(!result.contains("## Other Active Sessions"));
    }

    #[test]
    fn shows_other_sessions() {
        let wt = make_worktree("/repo", "/wt", "main");
        let other = make_session(
            "other-sess",
            "developer",
            vec![],
            vec!["some-repo".to_string()],
        );
        let others = vec![&other];
        let result = build_session_context_prompt("s1", "dev", &[wt], None, &[], &others);
        assert!(result.contains("## Other Active Sessions"));
        assert!(result.contains("\"other-sess\" [developer]"));
    }

    #[test]
    fn overlap_warning_when_same_repo() {
        let wt = make_worktree("/repo", "/wt", "main");
        let other = make_session(
            "other",
            "dev",
            vec![make_worktree("/repo", "/wt2", "feature")],
            vec![],
        );
        let others = vec![&other];
        let result = build_session_context_prompt("s1", "dev", &[wt], None, &[], &others);
        assert!(result.contains("avoid modifying overlapping files"));
    }

    #[test]
    fn no_overlap_warning_different_repos() {
        let wt = make_worktree("/repo-a", "/wt-a", "main");
        let other = make_session(
            "other",
            "dev",
            vec![make_worktree("/repo-b", "/wt-b", "main")],
            vec![],
        );
        let others = vec![&other];
        let result = build_session_context_prompt("s1", "dev", &[wt], None, &[], &others);
        assert!(!result.contains("avoid modifying overlapping files"));
    }

    #[test]
    fn additional_dir_deduped_with_cwd() {
        let cwd = Path::new("/home/user/repo");
        let extra = vec![PathBuf::from("/home/user/repo")];
        let result = build_session_context_prompt("s1", "dev", &[], Some(cwd), &extra, &[]);
        // The cwd path should appear exactly once.
        assert_eq!(result.matches("/home/user/repo").count(), 1);
    }

    #[test]
    fn additional_dir_deduped_with_worktree() {
        let wt = make_worktree("/repo", "/wt/checkout", "main");
        let extra = vec![PathBuf::from("/wt/checkout")];
        let result = build_session_context_prompt("s1", "dev", &[wt], None, &extra, &[]);
        // The worktree path should appear exactly once (in the worktree line).
        assert_eq!(result.matches("/wt/checkout").count(), 1);
    }

    #[test]
    fn multiple_worktrees_all_listed() {
        let wt1 = make_worktree("/repo-a", "/wt/a", "feat-a");
        let wt2 = make_worktree("/repo-b", "/wt/b", "feat-b");
        let result = build_session_context_prompt("s1", "dev", &[wt1, wt2], None, &[], &[]);
        assert!(result.contains("`/wt/a`"));
        assert!(result.contains("(branch: `feat-a`)"));
        assert!(result.contains("`/wt/b`"));
        assert!(result.contains("(branch: `feat-b`)"));
    }

    #[test]
    fn other_session_shows_branch_from_worktrees() {
        let wt = make_worktree("/repo", "/wt", "main");
        let other = make_session(
            "peer",
            "dev",
            vec![make_worktree("/repo", "/wt2", "hotfix")],
            vec![],
        );
        let others = vec![&other];
        let result = build_session_context_prompt("s1", "dev", &[wt], None, &[], &others);
        assert!(result.contains("(branch: `hotfix`)"));
    }
}
