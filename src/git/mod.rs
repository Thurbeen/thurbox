use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::warn;

use crate::paths;
use crate::session::HostDef;
use crate::shell::{posix_quote, ssh_command, wsl_command};

/// The ambient `GIT_*` variables that pin git to a specific repo/index/worktree,
/// overriding the path we point it at via `current_dir`/`-C`. Git exports these
/// to hook processes (a `pre-commit` hook runs with `GIT_DIR`/`GIT_INDEX_FILE`
/// set), so if thurbox — or its test suite under the project's pre-commit
/// `cargo nextest` hook — inherits them, a `git` call targeting an explicit
/// worktree would silently operate on the *hook's* repo instead (writing the
/// wrong index, running the wrong hooks). Every git invocation scrubs them so it
/// always discovers the repo from the path it was given.
const GIT_LOCATION_ENV: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
];

/// Remove the [`GIT_LOCATION_ENV`] variables from `cmd` so an inherited git-hook
/// environment can't redirect a path-targeted git call to the wrong repo.
pub(crate) fn scrub_git_location_env(cmd: &mut Command) {
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
}

// ── working copies for installed interface plugins ─────────────────────────
//
// A plugin that carries more than Lua is a repository: `git clone` delivers
// arbitrary bytes, preserves whatever layout the author chose, identifies exactly
// what it delivered, and refuses to clobber a dirty working tree. These are the
// operations `kernel::packages` needs for that, local only — a plugin's pane has no
// session and therefore no host to run on.

/// Make a git invocation refuse to ask the user anything.
///
/// A clone of a private repository can otherwise block forever on an SSH
/// passphrase or a host-key confirmation, and an install runs from the command
/// drain — so a prompt is a frozen interface, with no indication why. Two knobs
/// cover it: `GIT_TERMINAL_PROMPT=0` stops git asking for HTTPS credentials, and
/// `GIT_SSH_COMMAND` carries the same [`crate::shell::SSH_HARDENING_OPTS`] every
/// other ssh use here applies. A clone that would have prompted fails with a
/// message instead.
fn non_interactive(cmd: &mut Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let ssh = std::iter::once("ssh".to_string())
        .chain(
            crate::shell::SSH_HARDENING_OPTS
                .iter()
                .map(|o| o.to_string()),
        )
        .collect::<Vec<_>>()
        .join(" ");
    cmd.env("GIT_SSH_COMMAND", ssh);
}

/// Run a git command that produces no output worth keeping, or fail with stderr.
fn run_git(mut cmd: Command, what: &str) -> Result<()> {
    let output = cmd
        .output()
        .with_context(|| format!("failed to run {what}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{what} failed: {}", stderr.trim());
    }
    Ok(())
}

/// Does this ref name a commit rather than a branch or a tag?
///
/// The distinction is forced on us by git: `clone --branch` accepts a branch or a
/// tag and **rejects a commit id**, so a pin that is one has to be obtained the
/// long way round. Ambiguity resolves toward the commit — a branch could be named
/// `deadbeef` and would be read as one — because a hex-looking branch name is a
/// curiosity and pinning a commit is the whole point of a lock.
fn names_a_commit(git_ref: &str) -> bool {
    (7..=40).contains(&git_ref.len()) && git_ref.chars().all(|c| c.is_ascii_hexdigit())
}

/// Clone `url` into `dest`, shallow, optionally at `git_ref`.
///
/// `--depth 1` because running a pane needs no history and a shallow clone is
/// dramatically faster; the two properties keeping `.git` is *for* — refusing to
/// overwrite a dirty tree, and `git diff` against the checkout — need none either.
///
/// `git_ref` may be a branch, a tag **or a commit id**; the last cannot be cloned
/// directly, so it is fetched and checked out afterwards. A failure at either step
/// takes the clone back, so a caller that has written nothing yet leaves nothing
/// behind.
pub fn clone_plugin(url: &str, dest: &Path, git_ref: Option<&str>) -> Result<()> {
    if dest.exists() {
        anyhow::bail!("{} already exists", dest.display());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut cmd = git_program();
    non_interactive(&mut cmd);
    cmd.arg("clone").arg("--depth").arg("1");
    // A commit id is not a `--branch`: clone the default tip, then go and get it.
    let commit = git_ref.filter(|git_ref| names_a_commit(git_ref));
    if let Some(name) = git_ref.filter(|git_ref| !names_a_commit(git_ref)) {
        cmd.arg("--branch").arg(name);
    }
    cmd.arg(url).arg(dest);
    run_git(cmd, "git clone")?;

    if let Some(commit) = commit {
        let obtained = fetch_ref(dest, commit).and_then(|()| checkout_plugin(dest, commit));
        if let Err(e) = obtained {
            // Clone succeeded, the pin did not: this working copy is at the wrong
            // revision and nobody asked for that one.
            let _ = std::fs::remove_dir_all(dest);
            // Said distinctly from a failed clone, because the two have different
            // fixes and the common cause is invisible: a rebase or squash merge
            // *replaces* commits, so a pin taken from a pull request that has since
            // been merged names an object the remote no longer has. Reachable
            // without the pin, which is the actual next step.
            return Err(e.context(format!(
                "the repository cloned, but commit {commit} could not be obtained \
                 from it — if that commit came from a branch which was since \
                 rebased, squashed or deleted, the remote no longer has it"
            )));
        }
    }
    Ok(())
}

/// Fetch one ref — a commit id, a branch or a tag — into a shallow working copy.
///
/// What makes a recorded commit reproducible: a shallow clone has only the tip, so
/// reproducing a spec elsewhere has to ask for the exact object the lock names.
/// Checking out the result means checking out `FETCH_HEAD`, not the ref's name: a
/// fetch does not move the local branch a `--branch` clone left checked out.
pub fn fetch_ref(repo: &Path, git_ref: &str) -> Result<()> {
    let mut cmd = git_command(None, repo, &["fetch", "--depth", "1", "origin", git_ref]);
    non_interactive(&mut cmd);
    run_git(cmd, "git fetch")
}

/// Fetch whatever the remote's default branch now points at.
pub fn fetch_tip(repo: &Path) -> Result<()> {
    let mut cmd = git_command(None, repo, &["fetch", "--depth", "1", "origin"]);
    non_interactive(&mut cmd);
    run_git(cmd, "git fetch")
}

/// Check out a revision in a working copy.
pub fn checkout_plugin(repo: &Path, rev: &str) -> Result<()> {
    let cmd = git_command(None, repo, &["checkout", "--detach", rev]);
    run_git(cmd, "git checkout")
}

/// The commit a working copy is on.
///
/// Recorded in the lock instead of the ref that was asked for: `main` moves, and a
/// spec that reproduces "whatever main is now" reproduces nothing.
pub fn head_commit(repo: &Path) -> Result<String> {
    let output = git_command(None, repo, &["rev-parse", "HEAD"])
        .output()
        .context("failed to run git rev-parse")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git rev-parse failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Does this working copy have uncommitted changes?
///
/// The question that makes "your edits are yours" git's job rather than the
/// delivery matrix's: a dirty copy is never moved, and the caller reports it as
/// kept.
pub fn is_dirty(repo: &Path) -> Result<bool> {
    let output = git_command(None, repo, &["status", "--porcelain"])
        .output()
        .context("failed to run git status")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git status failed: {}", stderr.trim());
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// Whether `dir` is the root of a working copy.
pub fn is_working_copy(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// A `git` [`Command`] with the ambient repo-location environment scrubbed (see
/// [`scrub_git_location_env`]). Every git invocation — production and tests —
/// must start from this rather than a bare `Command::new("git")`, so it always
/// targets the path it is pointed at regardless of any inherited `GIT_*` vars.
pub(crate) fn git_program() -> Command {
    let mut cmd = Command::new("git");
    scrub_git_location_env(&mut cmd);
    cmd
}

/// Build the launcher [`Command`] for an off-local host: `ssh <opts> <dest>`
/// for an SSH host, or `wsl.exe -d <distro>` for a WSL distro. Both join and
/// shell-interpret the trailing tokens identically, so callers append the same
/// POSIX-quoted command afterward.
fn host_launcher(h: &HostDef) -> Command {
    if h.is_wsl() {
        wsl_command(&h.distro_name())
    } else {
        ssh_command(&h.destination, &h.ssh_opts)
    }
}

/// Build a `git` [`Command`] targeting `cwd`, run locally, over SSH, or inside
/// a WSL distro.
///
/// For an off-local host the command becomes
/// `<launcher> git -C <cwd> <args…>` (launcher = `ssh <opts> <dest>` or
/// `wsl.exe -d <distro>`), with each token shell-escaped so it survives the
/// host login shell's re-splitting.
fn git_command(host: Option<&HostDef>, cwd: &Path, args: &[&str]) -> Command {
    match host {
        None => {
            let mut cmd = git_program();
            cmd.current_dir(cwd);
            cmd.args(args);
            cmd
        }
        Some(h) => {
            let mut cmd = host_launcher(h);
            cmd.arg(posix_quote("git"));
            cmd.arg(posix_quote("-C"));
            cmd.arg(posix_quote(&cwd.to_string_lossy()));
            for a in args {
                cmd.arg(posix_quote(a));
            }
            cmd
        }
    }
}

/// Cache of resolved host `$HOME` directories, keyed by the host's backend name
/// (`ssh:<name>` / `wsl:<name>`), so we only pay one round-trip per host. A WSL
/// host has no `destination`, so the backend name — unique per host — is the
/// stable key for both kinds. Entries live for the process lifetime:
/// `hosts.toml` is read once at startup, so repointing a host requires a
/// restart anyway — the cache can never be staler than the config it derives
/// from.
fn remote_home_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve the `$HOME` directory on a host (over SSH or inside a WSL distro),
/// caching the result.
pub(crate) fn remote_home(host: &HostDef) -> Result<String> {
    let key = host.backend_name();
    if let Ok(guard) = remote_home_cache().lock() {
        if let Some(home) = guard.get(&key) {
            return Ok(home.clone());
        }
    }
    let mut cmd = host_launcher(host);
    // Pass `$HOME` literally; the host login shell expands it.
    cmd.arg("echo").arg("$HOME");
    let output = cmd
        .stderr(Stdio::piped())
        .output()
        .context("failed to resolve host $HOME")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`echo $HOME` on {key} failed: {}", stderr.trim());
    }
    let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if home.is_empty() {
        anyhow::bail!("$HOME resolved empty for {key}");
    }
    if let Ok(mut guard) = remote_home_cache().lock() {
        guard.insert(key, home.clone());
    }
    Ok(home)
}

/// Deterministic worktree directory path for a repo + branch on the given host.
///
/// Local hosts use [`worktree_path`]. Remote hosts place worktrees under the
/// host's `worktrees_dir` (or `$HOME/.local/share/thurbox/worktrees` resolved
/// over ssh), preserving the same `<repo-hash>/<sanitized-branch>` layout.
fn worktree_path_for(host: Option<&HostDef>, repo_path: &Path, branch: &str) -> Result<PathBuf> {
    match host {
        None => worktree_path(repo_path, branch).context("failed to resolve worktrees directory"),
        Some(h) => {
            let base = match &h.worktrees_dir {
                Some(dir) => dir.clone(),
                None => format!("{}/.local/share/thurbox/worktrees", remote_home(h)?),
            };
            // The host is remote (always POSIX), so the path must be `/`-joined
            // even when thurbox itself runs on Windows — `PathBuf::join` would
            // otherwise insert `\` and produce a path the remote shell rejects.
            Ok(PathBuf::from(worktree_subpath_posix(
                &base, repo_path, branch,
            )))
        }
    }
}

/// Build a per-session **remote** symlink workspace on `host`: a directory
/// holding one symlink per member (`<label> -> <remote member path>`), so a
/// multi-repo remote session launches somewhere every repo is a visible subdir.
///
/// This is the remote analogue of [`crate::workspace::ensure_workspace`], run
/// over the host launcher (`ssh`/`wsl.exe`). All paths are POSIX (`/`-joined):
/// the `sh -c` script assumes a POSIX host, so on a Windows (`psmux`) SSH host
/// it fails and callers fall back to the primary cwd (a multi-repo session
/// there loses its workspace, with a logged error). Returns the remote
/// workspace directory path.
pub fn ensure_remote_workspace(
    host: &HostDef,
    id: &str,
    members: &[(String, PathBuf)],
) -> Result<PathBuf> {
    let ws = remote_workspace_dir(host, id)?;

    // Fresh dir, then one symlink per member, de-duplicating names with a `-2`,
    // `-3`, … suffix (matching the local builder).
    let mut script = format!("rm -rf {ws} && mkdir -p {ws}", ws = posix_quote(&ws));
    let mut used: HashSet<String> = HashSet::new();
    for (label, target) in members {
        let name = crate::paths::unique_link_name(label, &mut used);
        let link = format!("{ws}/{name}");
        script.push_str(&format!(
            " && ln -s {target} {link}",
            target = posix_quote(&target.to_string_lossy()),
            link = posix_quote(&link),
        ));
    }

    run_host_script(host, &script, "workspace build")?;
    Ok(PathBuf::from(ws))
}

/// The remote workspace directory for a session id on `host`, mirroring the
/// local layout (`<thurbox data root>/workspaces/<sanitized id>`). Base:
/// `<worktrees_dir>/..`, or `$HOME/.local/share/thurbox`. Sanitizes the id
/// with the same shared helper as the local builder
/// (`workspace::workspace_dir`) — including its empty-id rejection: an empty
/// segment would make the `rm -rf` in ensure/remove target the workspaces
/// *root*, wiping every session's workspace on the host.
pub(crate) fn remote_workspace_dir(host: &HostDef, id: &str) -> Result<String> {
    let base = match &host.worktrees_dir {
        Some(dir) => Path::new(dir)
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| dir.clone()),
        None => format!("{}/.local/share/thurbox", remote_home(host)?),
    };
    let segment = crate::paths::sanitize_workspace_segment(id);
    anyhow::ensure!(!segment.is_empty(), "empty workspace id");
    Ok(format!("{base}/workspaces/{segment}"))
}

/// Remove the remote symlink workspace for a session id on `host` — the
/// teardown counterpart of [`ensure_remote_workspace`], the remote analogue of
/// [`crate::workspace::remove_workspace`]. Only the workspace dir (symlinks) is
/// removed; the repos the links point at are untouched, and a missing
/// workspace is not an error (`rm -rf` on a missing path succeeds).
pub fn remove_remote_workspace(host: &HostDef, id: &str) -> Result<()> {
    let ws = remote_workspace_dir(host, id)?;
    run_host_script(
        host,
        &format!("rm -rf {}", posix_quote(&ws)),
        "workspace removal",
    )
}

/// Map a finished remote command to `Ok(stdout)`, or an error carrying the
/// trimmed remote stderr when it exited non-zero. Shared by every remote
/// helper below so their failures read uniformly.
fn remote_output_or_stderr(output: std::process::Output, action: &str) -> Result<Vec<u8>> {
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!("remote {action} failed: {}", stderr.trim())
}

/// Run `script` on `host` via [`host_shell_c`], discarding stdout. See
/// [`remote_output_or_stderr`] for the failure shape.
fn run_host_script(host: &HostDef, script: &str, action: &str) -> Result<()> {
    let output = host_shell_c(host, script)
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to run remote {action}"))?;
    remote_output_or_stderr(output, action).map(|_| ())
}

/// Build a `<launcher> sh -c <script>` [`Command`] for a host, correct for each
/// transport's argument handling.
///
/// The two launchers disagree on how trailing args reach the in-host shell:
/// - **`wsl.exe`** gets `--exec` (`-e`), which bypasses `wsl.exe`'s own
///   command-line processing entirely: argv reaches the in-distro process
///   verbatim, so the multi-statement `script` travels as one **unquoted** arg
///   and `sh -c` parses the `posix_quote`d paths inside it exactly like the
///   ssh path. Without `-e`, `wsl.exe` mangles the script — it substitutes
///   `$…` even inside a preserved argument (single quotes don't protect it),
///   and pre-quoting the script makes the in-distro shell treat the quoted
///   blob as one command word ("not found").
/// - **`ssh`** space-joins its trailing args into one string the remote login
///   shell re-splits, so the `script` must be POSIX-quoted to survive as a
///   single `sh -c` argument (mirroring [`git_command`]).
pub(crate) fn host_shell_c(host: &HostDef, script: &str) -> Command {
    let mut cmd = host_launcher(host);
    if host.is_wsl() {
        cmd.arg("-e").arg("sh").arg("-c").arg(script);
    } else {
        cmd.arg(posix_quote("sh"))
            .arg(posix_quote("-c"))
            .arg(posix_quote(script));
    }
    cmd
}

/// Write `bytes` to `remote_path` on `host`, creating the parent directory.
/// Streams the bytes over the host launcher's stdin into `cat > <path>`, so it
/// is transport-neutral (ssh/wsl) and needs no `scp`/`\\wsl$` share. Used to
/// materialize thurbox-managed agent config (e.g. the hooks `--settings
/// claude.json`, with its commands rewritten for the host) on the remote so
/// the agent — launched with a `--settings <path>` that thurbox generated
/// against the *local* config dir — finds the file at that path there too.
pub fn copy_bytes_to_remote(host: &HostDef, bytes: &[u8], remote_path: &str) -> Result<()> {
    use std::io::Write;

    let parent = Path::new(remote_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string());
    // `mkdir -p <dir> && cat > <file>`: stdin is the file body. Both transports
    // pass stdin straight through to the in-host shell.
    let script = format!(
        "mkdir -p {dir} && cat > {file}",
        dir = posix_quote(&parent),
        file = posix_quote(remote_path),
    );

    let mut child = host_shell_c(host, &script)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn remote file-copy")?;
    child
        .stdin
        .take()
        .context("remote file-copy stdin unavailable")?
        .write_all(bytes)
        .context("failed to stream file to remote")?;
    let output = child
        .wait_with_output()
        .context("failed to wait on remote file-copy")?;
    remote_output_or_stderr(output, "file-copy").map(|_| ())
}

/// Expand a leading `~` in a remote path against the host's `$HOME`. Remote
/// paths are never expanded against the *local* home (they're different
/// filesystems), so this is the only `~` handling a remote path gets. A path
/// with no `~` prefix passes through unchanged.
pub fn expand_remote_tilde(host: &HostDef, path: &str) -> Result<String> {
    if path == "~" {
        return remote_home(host);
    }
    match path.strip_prefix("~/") {
        Some(rest) => Ok(format!("{}/{}", remote_home(host)?, rest)),
        None => Ok(path.to_string()),
    }
}

/// Sentinel exit code remote probe scripts use for their "negative" answer, so
/// it can't be confused with a transport failure (ssh's 255, a launch error,
/// or the probed command's own 1).
const REMOTE_PROBE_NEGATIVE: i32 = 3;

/// Sentinel for "the path exists but is not a regular file" — distinct from
/// [`REMOTE_PROBE_NEGATIVE`] *and* from the probed command's own exit 1 (e.g.
/// `cat` failing on a permission-denied file must not be misreported as a
/// file-type problem).
const REMOTE_PROBE_NOT_FILE: i32 = 4;

/// Resolve `%USERPROFILE%` on a **native-Windows** SSH host (a `psmux` host),
/// normalized to forward slashes (`C:/Users/me`) and cached like
/// [`remote_home`] (same key space — a host is either POSIX or Windows, never
/// both). Runs `powershell -NoProfile -Command Write-Output $env:USERPROFILE`,
/// which works whether the host's default sshd shell is `cmd` or PowerShell
/// (`cmd` passes `$env:…` through untouched for PowerShell to evaluate).
pub(crate) fn remote_home_windows(host: &HostDef) -> Result<String> {
    let key = host.backend_name();
    if let Ok(guard) = remote_home_cache().lock() {
        if let Some(home) = guard.get(&key) {
            return Ok(home.clone());
        }
    }
    let mut cmd = host_launcher(host);
    cmd.args(["powershell", "-NoProfile", "-Command"])
        .arg("Write-Output $env:USERPROFILE");
    let output = cmd
        .stderr(Stdio::piped())
        .output()
        .context("failed to resolve host %USERPROFILE%")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("resolving %USERPROFILE% on {key} failed: {}", stderr.trim());
    }
    let home = String::from_utf8_lossy(&output.stdout)
        .trim()
        .replace('\\', "/");
    if home.is_empty() {
        anyhow::bail!("%USERPROFILE% resolved empty for {key}");
    }
    if let Ok(mut guard) = remote_home_cache().lock() {
        guard.insert(key, home.clone());
    }
    Ok(home)
}

/// The longest command line safely below Windows `cmd.exe`'s ~8191-char limit
/// (the sshd default shell may be `cmd`), leaving headroom for the PowerShell
/// wrapper around the base64 payload.
const WINDOWS_COMMAND_BUDGET: usize = 7_500;

/// [`copy_bytes_to_remote`] for a **native-Windows** SSH host: `cat > file`
/// doesn't exist there, so the payload travels base64-encoded inside a
/// PowerShell one-liner (`[IO.File]::WriteAllBytes` + `New-Item -Force` for
/// the parent dir). Bounded by the Windows command-line limit — thurbox's hook
/// payloads are 1–4 KB, well within it; anything larger errors instead of
/// truncating. `remote_path` must be `/`-separated (PowerShell accepts it).
pub(crate) fn copy_bytes_to_remote_windows(
    host: &HostDef,
    bytes: &[u8],
    remote_path: &str,
) -> Result<()> {
    use base64::Engine as _;

    let parent = Path::new(remote_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    anyhow::ensure!(
        !remote_path.contains('\'') && !parent.contains('\''),
        "remote path {remote_path} contains a quote"
    );
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let script = format!(
        "New-Item -ItemType Directory -Force -Path '{parent}' | Out-Null; \
         [IO.File]::WriteAllBytes('{remote_path}', [Convert]::FromBase64String('{b64}'))"
    );
    anyhow::ensure!(
        script.len() <= WINDOWS_COMMAND_BUDGET,
        "payload too large for a Windows command line ({} bytes)",
        bytes.len()
    );
    let mut cmd = host_launcher(host);
    // Double-quoted so the host's default shell (cmd or PowerShell) hands the
    // script to `powershell -Command` as one argument; the payload/path are
    // single-quoted inside, and base64 text never contains quotes.
    cmd.args(["powershell", "-NoProfile", "-Command"])
        .arg(format!("\"{script}\""));
    let output = cmd
        .stderr(Stdio::piped())
        .output()
        .context("failed to spawn windows remote file-copy")?;
    remote_output_or_stderr(output, "windows file-copy").map(|_| ())
}

/// Whether `dir` exists as a directory on `host` (a leading `~` expands against
/// the remote home). `Ok(false)` means the probe *ran* and answered no — a
/// transport failure is an `Err`, distinguished by the exit code: the script
/// answers only 0 / [`REMOTE_PROBE_NEGATIVE`] itself.
pub(crate) fn remote_dir_exists(host: &HostDef, dir: &str) -> Result<bool> {
    let dir = expand_remote_tilde(host, dir)?;
    let script = format!(
        "if test -d {}; then exit 0; else exit {REMOTE_PROBE_NEGATIVE}; fi",
        posix_quote(&dir)
    );
    let output = host_shell_c(host, &script)
        .stderr(Stdio::piped())
        .output()
        .context("failed to run remote dir probe")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(REMOTE_PROBE_NEGATIVE) => Ok(false),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("remote dir probe failed: {}", stderr.trim())
        }
    }
}

/// Read a regular file on `host` (a leading `~` expands against the remote
/// home). `Ok(None)` = the path doesn't exist; an existing-but-not-regular
/// path or a transport failure is an `Err` (see [`remote_dir_exists`] for the
/// exit-code discipline).
pub(crate) fn read_remote_file(host: &HostDef, path: &str) -> Result<Option<String>> {
    let path = expand_remote_tilde(host, path)?;
    let quoted = posix_quote(&path);
    let script = format!(
        "if test -f {quoted}; then cat {quoted}; elif test -e {quoted}; then \
         exit {REMOTE_PROBE_NOT_FILE}; else exit {REMOTE_PROBE_NEGATIVE}; fi"
    );
    let output = host_shell_c(host, &script)
        .stderr(Stdio::piped())
        .output()
        .context("failed to run remote file read")?;
    match output.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned())),
        Some(REMOTE_PROBE_NEGATIVE) => Ok(None),
        Some(REMOTE_PROBE_NOT_FILE) => {
            anyhow::bail!("remote path {path} exists but is not a regular file")
        }
        // Anything else includes `cat`'s own failure (e.g. permission denied,
        // exit 1) — surface its stderr rather than misreporting the file type.
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("remote file read failed: {}", stderr.trim())
        }
    }
}

/// Global cache for repo display names (path → name).
static REPO_NAME_CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, String>>> =
    std::sync::OnceLock::new();

fn repo_name_cache() -> &'static Mutex<HashMap<PathBuf, String>> {
    REPO_NAME_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get a short display name for a repo directory.
///
/// Tries to extract the repo name from `git remote get-url origin`
/// (e.g. `github.com/user/thurbox.git` → `"thurbox"`).
/// Falls back to the directory's file name if no remote is found.
/// Results are cached globally.
pub fn repo_display_name(path: &Path) -> Option<String> {
    let cache = repo_name_cache();
    if let Ok(guard) = cache.lock() {
        if let Some(name) = guard.get(path) {
            return Some(name.clone());
        }
    }
    let name = repo_name_from_remote(path).or_else(|| {
        path.file_name()
            .and_then(|f| f.to_str())
            .map(|s| s.to_string())
    })?;
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path.to_path_buf(), name.clone());
    }
    Some(name)
}

/// True if `path` is itself a git repo: it contains a `.git` directory OR a
/// `.git` file (worktree checkouts and submodules use a `.git` file).
pub fn is_git_repo(path: &Path) -> bool {
    let git = path.join(".git");
    git.is_dir() || git.is_file()
}

/// Immediate child directories of `parent` that are git repos, sorted by file
/// name. Hidden (`.`-prefixed) entries are skipped. Returns an empty vec when
/// `parent` can't be read (missing, permission denied, …).
pub fn scan_child_repos(parent: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut repos: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter(|e| {
            !e.file_name()
                .to_str()
                .map(|n| n.starts_with('.'))
                .unwrap_or(true)
        })
        .map(|e| e.path())
        .filter(|p| is_git_repo(p))
        .collect();
    repos.sort();
    repos
}

/// Immediate sub-directories of a path, each flagged as a git repo or not —
/// the repo picker's path-browser payload. `Missing` distinguishes "the dir
/// isn't there" (a user typo, reported inline) from a transport error (`Err`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirListing {
    /// The requested directory does not exist on the target filesystem.
    Missing,
    /// `(name, is_git)` per immediate sub-directory, sorted by name. Hidden
    /// (`.`-prefixed) entries are included — the picker's filter decides
    /// their visibility (offered only to a `.`-prefix, like the local
    /// completer).
    Entries(Vec<(String, bool)>),
}

/// What a committed repo-picker path turned out to be on the target
/// filesystem. `Dir` (a non-repo directory) is still selectable — it becomes
/// a plain `--add-dir`-style member — but can't take the worktree toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathClass {
    Git,
    Dir,
    Missing,
}

/// List the immediate sub-directories of `dir` on `host` (`None` = local),
/// flagging each as a git repo. One round trip on a remote host: a `sh -c`
/// loop using `test -d` per entry (which, unlike an `ls -p` probe, follows a
/// symlink to a directory) and `test -e <e>/.git` (matching
/// [`is_git_repo`]'s `.git`-file worktree handling). A leading `~` expands
/// against the target's home. Blocking — call from a worker thread for a
/// remote host.
pub fn list_dir_entries_on(host: Option<&HostDef>, dir: &str) -> Result<DirListing> {
    let Some(host) = host else {
        return Ok(list_dir_entries_local(&paths::expand_tilde(dir)));
    };
    let dir = expand_remote_tilde(host, dir)?;
    let output = host_shell_c(host, &list_dir_entries_script(&dir))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to list remote directory")?;
    let stdout = remote_output_or_stderr(output, "dir listing")?;
    Ok(parse_dir_listing(&String::from_utf8_lossy(&stdout)))
}

/// The [`list_dir_entries_on`] shell script (`dir` already tilde-expanded,
/// quoted here). Line protocol: `!missing` alone when the dir is absent; else
/// one line per entry, `g <name>` (git repo) or `d <name>` (plain dir). A
/// `cd` failure on an *existing* dir (permissions) exits non-zero so the
/// stderr surfaces as a real error rather than a bogus "missing". Pure so the
/// quoting is testable without a host.
fn list_dir_entries_script(dir: &str) -> String {
    format!(
        "d={q}; [ -d \"$d\" ] || {{ echo '!missing'; exit 0; }}; cd \"$d\" || exit 1; \
         for e in * .*; do [ -d \"$e\" ] || continue; \
         case \"$e\" in .|..) continue;; esac; \
         if [ -e \"$e/.git\" ]; then printf 'g %s\\n' \"$e\"; \
         else printf 'd %s\\n' \"$e\"; fi; done; exit 0",
        q = posix_quote(dir),
    )
}

/// Local branch of [`list_dir_entries_on`] (`dir` already tilde-expanded).
fn list_dir_entries_local(dir: &Path) -> DirListing {
    if !dir.is_dir() {
        return DirListing::Missing;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        // Exists but unreadable (permissions): an empty listing keeps the
        // browser usable; the path itself may still be committed.
        return DirListing::Entries(Vec::new());
    };
    let mut names: Vec<(String, bool)> = entries
        .filter_map(|e| e.ok())
        // `path().is_dir()` follows symlinks, matching the remote `test -d`.
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let is_git = is_git_repo(&e.path());
            Some((name, is_git))
        })
        .collect();
    names.sort();
    DirListing::Entries(names)
}

/// Parse the [`list_dir_entries_on`] line protocol. Unknown lines are skipped
/// (a host's shell may emit noise) rather than failing the whole listing.
fn parse_dir_listing(stdout: &str) -> DirListing {
    if stdout.lines().next().map(str::trim) == Some("!missing") {
        return DirListing::Missing;
    }
    let mut entries: Vec<(String, bool)> = stdout
        .lines()
        .filter_map(|line| {
            let (tag, name) = line.split_once(' ')?;
            let is_git = match tag {
                "g" => true,
                "d" => false,
                _ => return None,
            };
            (!name.is_empty()).then(|| (name.to_string(), is_git))
        })
        .collect();
    entries.sort();
    DirListing::Entries(entries)
}

/// Classify `path` on `host` in one round trip: does it exist, and is it a
/// git repo? The repo picker's Enter-commit validation — replacing the old
/// exists-only `ls` probe with the same trip cost. Blocking — call
/// from a worker thread.
pub fn classify_path_on(host: &HostDef, path: &str) -> Result<PathClass> {
    let path = expand_remote_tilde(host, path)?;
    let output = host_shell_c(host, &classify_path_script(&path))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to check remote path")?;
    let stdout = remote_output_or_stderr(output, "path check")?;
    parse_path_class(&String::from_utf8_lossy(&stdout))
}

/// The [`classify_path_on`] shell script (`path` already tilde-expanded).
fn classify_path_script(path: &str) -> String {
    format!(
        "p={q}; if [ ! -d \"$p\" ]; then echo missing; \
         elif [ -e \"$p/.git\" ]; then echo git; else echo dir; fi",
        q = posix_quote(path),
    )
}

/// Parse the single-word [`classify_path_on`] output.
fn parse_path_class(stdout: &str) -> Result<PathClass> {
    match stdout.trim() {
        "git" => Ok(PathClass::Git),
        "dir" => Ok(PathClass::Dir),
        "missing" => Ok(PathClass::Missing),
        other => anyhow::bail!("unexpected path-check output: {other:?}"),
    }
}

/// Remote analogue of [`scan_child_repos`]: the immediate child git repos of
/// `parent` on `host`, as absolute paths, sorted. Hidden entries are skipped
/// (matching the local scan). One round trip; blocking — call from a worker
/// thread. A missing/unreadable parent is an error (unlike the local scan's
/// empty vec) so the picker can tell the user instead of silently importing
/// nothing.
pub fn scan_child_repos_on(host: &HostDef, parent: &str) -> Result<Vec<PathBuf>> {
    let parent = expand_remote_tilde(host, parent)?;
    let output = host_shell_c(host, &scan_child_repos_script(&parent))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to scan remote parent directory")?;
    let stdout = remote_output_or_stderr(output, "parent scan")?;
    let parent = Path::new(&parent);
    collect_scanned_children(parent, &String::from_utf8_lossy(&stdout))
}

/// The [`scan_child_repos_on`] shell script (`parent` already tilde-expanded).
/// `*` skips hidden entries, matching the local scan; a missing/unreadable
/// parent exits non-zero (surfaced as an error, unlike the local scan's empty
/// vec, so the picker can tell the user instead of silently importing nothing).
fn scan_child_repos_script(parent: &str) -> String {
    format!(
        "d={q}; cd \"$d\" || exit 1; \
         for e in *; do if [ -d \"$e\" ] && [ -e \"$e/.git\" ]; then \
         printf '%s\\n' \"$e\"; fi; done; exit 0",
        q = posix_quote(parent),
    )
}

/// Join the scanned child names onto `parent`, sorted. A literal `*` line
/// (an empty dir's unexpanded glob never passes the `-d` test, but keep the
/// guard) and blank lines are skipped.
fn collect_scanned_children(parent: &Path, stdout: &str) -> Result<Vec<PathBuf>> {
    let mut repos: Vec<PathBuf> = stdout
        .lines()
        .filter(|name| !name.is_empty() && *name != "*")
        .map(|name| parent.join(name))
        .collect();
    repos.sort();
    Ok(repos)
}

/// Parse repo name from the origin remote URL.
fn repo_name_from_remote(path: &Path) -> Option<String> {
    let output = git_program()
        .args(["remote", "get-url", "origin"])
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_repo_name_from_url(&url)
}

/// Extract repo name from a git remote URL.
///
/// Handles common formats:
/// - `git@github.com:user/repo.git` → `"repo"`
/// - `https://github.com/user/repo.git` → `"repo"`
/// - `https://github.com/user/repo` → `"repo"`
fn parse_repo_name_from_url(url: &str) -> Option<String> {
    // Split on the last '/' (HTTPS/SSH with path) or ':' (SSH shorthand).
    let after_sep = if url.contains('/') {
        url.rsplit('/').next()?
    } else {
        url.rsplit(':').next()?
    };
    let name = after_sep.strip_suffix(".git").unwrap_or(after_sep);
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// List local branch names for a repo.
pub fn list_branches(repo_path: &Path) -> Result<Vec<String>> {
    list_branches_on(None, repo_path)
}

/// [`list_branches`], optionally on a remote `host`.
pub fn list_branches_on(host: Option<&HostDef>, repo_path: &Path) -> Result<Vec<String>> {
    let output = git_command(host, repo_path, &["branch", "--format=%(refname:short)"])
        .output()
        .context("failed to run git branch")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git branch failed: {stderr}");
    }

    let branches = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    Ok(branches)
}

/// Raw unified `git diff <base>..HEAD` output for a worktree, for the native
/// code-review view, optionally on a remote `host` (via `ssh <dest> git …`).
/// Returns `None` on failure (not a git dir, bad base, …); the caller falls
/// back to a narrower range or surfaces a status.
///
/// `--no-color` keeps the output parseable; the result is fed to
/// [`crate::session::parse_unified_diff`].
pub fn diff_against_on(host: Option<&HostDef>, worktree: &Path, base: &str) -> Option<String> {
    let range = format!("{base}..HEAD");
    run_diff(host, worktree, &["diff", "--no-color", &range])
}

/// Raw unified diff of the worktree's **uncommitted** changes vs `HEAD`
/// (staged + unstaged), for the review view's "working changes" target.
pub fn diff_working_on(host: Option<&HostDef>, worktree: &Path) -> Option<String> {
    run_diff(host, worktree, &["diff", "--no-color", "HEAD"])
}

/// The **complete** list of changed files as `--numstat -M -z`, with exact counts.
///
/// Separate from the diff text because the two have different bounds: a diff body is
/// capped (see `kernel::diff::MAX_DIFF_BYTES`) and this is not. Deriving the file
/// list from the capped body made it silently short — on this repository's own diff,
/// 310 files of 433 — so a reviewer navigating it could not tell it ended early, and
/// the totals were a fraction of the truth. Twelve kilobytes for four hundred files
/// is not worth capping.
///
/// `-z` rather than the human format: a rename in `--numstat` otherwise arrives as
/// `old => new` (or a brace form) and has to be un-guessed. NUL-separated, a rename
/// is an empty path field followed by two more records.
pub fn diff_numstat_on(
    host: Option<&HostDef>,
    worktree: &Path,
    base: Option<&str>,
) -> Option<String> {
    let range = base.map_or_else(|| "HEAD".to_string(), |base| format!("{base}..HEAD"));
    run_diff(
        host,
        worktree,
        &["diff", "--no-color", "--numstat", "-M", "-z", &range],
    )
}

/// Each changed file's status (`M`/`A`/`D`/`R…`) as `--name-status -M -z`.
///
/// The companion to [`diff_numstat_on`]: `--numstat` carries the counts and cannot
/// tell a deletion from a rewrite. Cheap — a fraction of the cost of the diff itself.
pub fn diff_name_status_on(
    host: Option<&HostDef>,
    worktree: &Path,
    base: Option<&str>,
) -> Option<String> {
    let range = base.map_or_else(|| "HEAD".to_string(), |base| format!("{base}..HEAD"));
    run_diff(
        host,
        worktree,
        &["diff", "--no-color", "--name-status", "-M", "-z", &range],
    )
}

/// Raw unified diff of a single commit (`git show`), for the review view's
/// per-commit target. `--format=` suppresses the log message, leaving the patch.
pub fn show_commit_on(host: Option<&HostDef>, worktree: &Path, sha: &str) -> Option<String> {
    run_diff(host, worktree, &["show", "--no-color", "--format=", sha])
}

/// List the commits in `<base>..HEAD` as `(short-sha, subject)`, newest first —
/// the choices for the review view's per-commit target picker.
pub fn list_commits_on(
    host: Option<&HostDef>,
    worktree: &Path,
    base: &str,
) -> Vec<(String, String)> {
    let range = format!("{base}..HEAD");
    let Some(out) = run_diff(
        host,
        worktree,
        &["log", "--no-color", "--format=%h%x09%s", &range],
    ) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(sha, subj)| (sha.to_string(), subj.to_string()))
        .collect()
}

/// Run a `git` command and capture stdout, logging + returning `None` on
/// failure. Shared by the review-view diff/log helpers.
///
/// `core.quotepath=false` keeps non-ASCII paths verbatim (git otherwise
/// C-quotes them, e.g. `"caf\303\251.rs"`), so the parser keys comments/marks on
/// the real UTF-8 path.
fn run_diff(host: Option<&HostDef>, worktree: &Path, args: &[&str]) -> Option<String> {
    let mut full = vec!["-c", "core.quotepath=false"];
    full.extend_from_slice(args);
    let output = git_command(host, worktree, &full).output().ok()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("git {args:?} failed: {}", stderr.trim());
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Create a git worktree on a new branch and return the worktree directory path.
///
/// Creates `new_branch` starting from `base_branch`.
/// Path format: `~/.local/share/thurbox/worktrees/<repo-hash>/<sanitized-branch>`
pub fn create_worktree(repo_path: &Path, new_branch: &str, base_branch: &str) -> Result<PathBuf> {
    create_worktree_on(None, repo_path, new_branch, base_branch)
}

/// [`create_worktree`], optionally on a remote `host` (via `ssh <dest> git …`).
/// On a remote host the worktree is created under the host's remote worktrees
/// directory and the returned path is a remote path.
pub fn create_worktree_on(
    host: Option<&HostDef>,
    repo_path: &Path,
    new_branch: &str,
    base_branch: &str,
) -> Result<PathBuf> {
    let wt_path = worktree_path_for(host, repo_path, new_branch)?;

    let output = git_command(
        host,
        repo_path,
        &[
            "worktree",
            "add",
            "-b",
            new_branch,
            &wt_path.display().to_string(),
            base_branch,
        ],
    )
    .output()
    .context("failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {stderr}");
    }

    Ok(wt_path)
}

/// Idempotently provision a worktree on `branch`, returning its directory.
///
/// Unlike [`create_worktree`] (which always passes `-b` and fails if the branch
/// already exists), this is safe to call repeatedly — the case that recurring
/// **spawn automations** hit on every fire:
///
/// - worktree directory already present → reuse it as-is;
/// - branch exists but has no worktree → attach a worktree to it
///   ([`add_existing_worktree`]);
/// - neither exists → create the branch + worktree off `base_branch`.
pub fn create_or_attach_worktree(
    repo_path: &Path,
    branch: &str,
    base_branch: &str,
) -> Result<PathBuf> {
    let wt_path =
        worktree_path(repo_path, branch).context("failed to resolve worktrees directory")?;
    if wt_path.exists() {
        return Ok(wt_path);
    }
    if branch_exists(repo_path, branch) {
        return add_existing_worktree(repo_path, branch);
    }
    create_worktree(repo_path, branch, base_branch)
}

/// Remove a git worktree (force removal).
pub fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> Result<()> {
    remove_worktree_on(None, repo_path, worktree_path)
}

/// [`remove_worktree`], optionally on a remote `host`.
pub fn remove_worktree_on(
    host: Option<&HostDef>,
    repo_path: &Path,
    worktree_path: &Path,
) -> Result<()> {
    let output = git_command(
        host,
        repo_path,
        &[
            "worktree",
            "remove",
            "--force",
            &worktree_path.display().to_string(),
        ],
    )
    .output()
    .context("failed to run git worktree remove")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree remove failed: {stderr}");
    }

    Ok(())
}

/// Detect the repository's default branch name.
///
/// Tries `git symbolic-ref refs/remotes/origin/HEAD` first (most reliable),
/// then falls back to checking for `main` or `master` among local branches.
pub fn default_branch(repo_path: &Path, local_branches: &[String]) -> Option<String> {
    default_branch_on(None, repo_path, local_branches)
}

/// [`default_branch`], optionally on a remote `host`.
pub fn default_branch_on(
    host: Option<&HostDef>,
    repo_path: &Path,
    local_branches: &[String],
) -> Option<String> {
    if let Some(name) = default_branch_from_remote_on(host, repo_path) {
        if local_branches.iter().any(|b| b == &name) {
            return Some(name);
        }
    }

    for candidate in ["main", "master"] {
        if local_branches.iter().any(|b| b == candidate) {
            return Some(candidate.to_string());
        }
    }

    None
}

/// Query the remote's default branch via `git symbolic-ref`.
pub fn default_branch_from_remote(repo_path: &Path) -> Option<String> {
    default_branch_from_remote_on(None, repo_path)
}

/// [`default_branch_from_remote`], optionally on a remote `host`.
pub fn default_branch_from_remote_on(host: Option<&HostDef>, repo_path: &Path) -> Option<String> {
    let output = git_command(
        host,
        repo_path,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    )
    .stderr(Stdio::null())
    .output()
    .ok()?;

    if !output.status.success() {
        return None;
    }

    let full_ref = String::from_utf8_lossy(&output.stdout).trim().to_string();
    full_ref.strip_prefix("origin/").map(|s| s.to_string())
}

/// Add an existing branch as a worktree (no `-b` flag — branch must already exist).
///
/// Returns the worktree directory path. If the worktree path already exists on
/// disk the function returns early with `Ok(path)`.
pub fn add_existing_worktree(repo_path: &Path, branch: &str) -> Result<PathBuf> {
    let wt_path =
        worktree_path(repo_path, branch).context("failed to resolve worktrees directory")?;

    if wt_path.exists() {
        return Ok(wt_path);
    }

    let output = git_program()
        .args(["worktree", "add", &wt_path.display().to_string(), branch])
        .current_dir(repo_path)
        .output()
        .context("failed to run git worktree add (existing branch)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add (existing) failed: {stderr}");
    }

    Ok(wt_path)
}

/// Check whether a local branch exists in the repository.
pub fn branch_exists(repo_path: &Path, branch: &str) -> bool {
    branch_exists_on(None, repo_path, branch)
}

/// [`branch_exists`], optionally on a remote `host`.
pub fn branch_exists_on(host: Option<&HostDef>, repo_path: &Path, branch: &str) -> bool {
    git_command(host, repo_path, &["rev-parse", "--verify", branch])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Deterministic worktree directory path for a repo + branch.
///
/// Worktrees are placed under the XDG data directory to avoid being inside
/// the source repo (which would cause Claude Code to discover duplicate
/// `.claude/commands/` skill files).
///
/// Path format: `~/.local/share/thurbox/worktrees/<repo-hash>/<sanitized-branch>`
fn worktree_path(repo_path: &Path, branch: &str) -> Option<PathBuf> {
    Some(worktree_subpath(
        paths::worktrees_directory()?,
        repo_path,
        branch,
    ))
}

/// Stable 64-bit FNV-1a hash of `input`, rendered as 16 lowercase hex digits.
///
/// This is the `<repo-hash>` segment of a worktree path. It must be a **fixed**
/// algorithm: `std::collections::hash_map::DefaultHasher` (SipHash) is
/// explicitly documented as *not* guaranteed stable across Rust versions or
/// builds, so the same repo path could hash to different directories after a
/// toolchain bump — orphaning every persisted worktree. FNV-1a is a tiny,
/// dependency-free, fully specified algorithm, so a given path always maps to
/// the same directory.
fn stable_repo_hash(input: &str) -> String {
    // FNV-1a 64-bit constants (offset basis + prime).
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for &byte in input.as_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// The two load-bearing path segments of the worktree layout: the stable
/// repo-path hash and the sanitized branch name. Both the local
/// ([`worktree_subpath`]) and remote ([`worktree_subpath_posix`]) variants
/// derive their `<repo-hash>/<sanitized-branch>` tail from this, so the
/// hash+sanitize logic lives in one place.
fn worktree_segments(repo_path: &Path, branch: &str) -> (String, String) {
    let repo_hash = stable_repo_hash(&repo_path.display().to_string());
    let sanitized = branch.replace('/', "-");
    (repo_hash, sanitized)
}

/// The deterministic `<base>/<repo-hash>/<sanitized-branch>` worktree layout,
/// shared by local ([`worktree_path`]) and remote ([`worktree_path_for`])
/// resolution so both produce identical sub-paths under their own base.
fn worktree_subpath(base: PathBuf, repo_path: &Path, branch: &str) -> PathBuf {
    let (repo_hash, sanitized) = worktree_segments(repo_path, branch);
    base.join(repo_hash).join(sanitized)
}

/// The same `<base>/<repo-hash>/<sanitized-branch>` layout as [`worktree_subpath`],
/// but rendered as a POSIX (`/`-joined) string for a **remote** host. This is
/// separate from the `PathBuf` form because on Windows `PathBuf::join` inserts
/// `\`, which the remote login shell would not accept.
fn worktree_subpath_posix(base: &str, repo_path: &Path, branch: &str) -> String {
    let (repo_hash, sanitized) = worktree_segments(repo_path, branch);
    format!("{}/{repo_hash}/{sanitized}", base.trim_end_matches('/'))
}

/// Result of attempting to sync a worktree with its base ref.
#[derive(Debug)]
pub enum SyncResult {
    /// Rebase succeeded (includes already-up-to-date).
    Synced,
    /// Rebase failed due to conflicts (aborted, stash restored).
    Conflict(String),
    /// Unexpected failure.
    Error(String),
}

/// Stash uncommitted changes. Returns `true` if anything was stashed.
fn git_stash(host: Option<&HostDef>, worktree_path: &Path) -> Result<bool> {
    let output = git_command(host, worktree_path, &["stash"])
        .output()
        .context("failed to run git stash")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git stash failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(!stdout.contains("No local changes to save"))
}

/// Fetch from origin.
pub fn git_fetch(worktree_path: &Path) -> Result<()> {
    git_fetch_on(None, worktree_path)
}

/// [`git_fetch`], optionally on a remote `host`.
pub fn git_fetch_on(host: Option<&HostDef>, worktree_path: &Path) -> Result<()> {
    let output = git_command(host, worktree_path, &["fetch", "origin"])
        .output()
        .context("failed to run git fetch")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git fetch failed: {stderr}");
    }

    Ok(())
}

/// Rebase the current branch onto `base_ref`. Returns `Ok(())` on success,
/// or an error if there are conflicts (rebase is aborted before returning).
fn git_rebase_onto(host: Option<&HostDef>, worktree_path: &Path, base_ref: &str) -> Result<()> {
    let output = git_command(host, worktree_path, &["rebase", base_ref])
        .output()
        .context("failed to run git rebase")?;

    if !output.status.success() {
        let _ = git_command(host, worktree_path, &["rebase", "--abort"]).output();

        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("rebase conflict: {stderr}");
    }

    Ok(())
}

/// Pop the most recent stash entry.
fn git_stash_pop(host: Option<&HostDef>, worktree_path: &Path) -> Result<()> {
    let output = git_command(host, worktree_path, &["stash", "pop"])
        .output()
        .context("failed to run git stash pop")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git stash pop failed: {stderr}");
    }

    Ok(())
}

/// Check whether a git error message indicates a transient index-lock failure.
fn is_transient_error(msg: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "could not write index",
        "Unable to write new index file",
        "index.lock': File exists",
        "Another git process seems to be running",
    ];
    PATTERNS.iter().any(|p| msg.contains(p))
}

/// Find the shared git directory for a worktree (handles linked worktrees).
fn git_common_dir(worktree_path: &Path) -> Option<PathBuf> {
    let output = git_program()
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(worktree_path)
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let path = Path::new(&dir);
    // git may return a relative path; resolve it against the worktree
    if path.is_absolute() {
        Some(PathBuf::from(dir))
    } else {
        Some(worktree_path.join(dir))
    }
}

/// Age threshold for mtime-based stale lock removal.
const STALE_LOCK_AGE: Duration = Duration::from_secs(60);

/// Remove a stale `index.lock` if we can confirm no live process holds it.
///
/// On Linux: reads the PID from the lock file content (if present) and checks `/proc/{pid}`.
/// Fallback (all platforms): removes if the lock file's mtime exceeds [`STALE_LOCK_AGE`].
fn cleanup_stale_index_lock(worktree_path: &Path) {
    let Some(git_dir) = git_common_dir(worktree_path) else {
        return;
    };
    let lock_path = git_dir.join("index.lock");
    if !lock_path.exists() {
        return;
    }

    #[cfg(target_os = "linux")]
    if try_remove_by_pid(&lock_path) {
        return;
    }

    try_remove_by_age(&lock_path);
}

/// Attempt to remove a lock file by checking if the owning PID is still alive.
///
/// Returns `true` if the PID was parseable (regardless of removal outcome),
/// meaning the caller should not fall through to the mtime-based check.
#[cfg(target_os = "linux")]
fn try_remove_by_pid(lock_path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(lock_path) else {
        return false;
    };
    let Some(pid_str) = content.split_whitespace().next() else {
        return false;
    };
    let Ok(pid) = pid_str.parse::<u32>() else {
        return false;
    };

    if Path::new(&format!("/proc/{pid}")).exists() {
        return true;
    }

    if std::fs::remove_file(lock_path).is_ok() {
        warn!(
            "Removed stale index.lock (dead PID {pid}) at {}",
            lock_path.display()
        );
    }
    true
}

/// Remove a lock file if its mtime exceeds [`STALE_LOCK_AGE`].
fn try_remove_by_age(lock_path: &Path) {
    let age = std::fs::metadata(lock_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok());

    let Some(age) = age else { return };

    if age > STALE_LOCK_AGE && std::fs::remove_file(lock_path).is_ok() {
        warn!(
            "Removed stale index.lock (age {:?}) at {}",
            age,
            lock_path.display()
        );
    }
}

/// Per-attempt delays for `stash_with_retry`. The first entry (zero) is the
/// initial attempt; subsequent entries are the backoff delays before each retry.
const STASH_ATTEMPT_DELAYS: &[Duration] = &[
    Duration::ZERO,
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_secs(1),
];

/// Run `git stash` with retries on transient index-lock errors.
///
/// Returns `Ok(true)` if changes were stashed, `Ok(false)` if nothing to stash.
fn stash_with_retry(host: Option<&HostDef>, worktree_path: &Path) -> Result<bool> {
    let max_retries = STASH_ATTEMPT_DELAYS.len() - 1;
    let mut last_err = String::new();

    for (attempt, delay) in STASH_ATTEMPT_DELAYS.iter().enumerate() {
        if attempt > 0 {
            warn!(
                "Retrying git stash (retry {}/{}) in {}",
                attempt,
                max_retries,
                worktree_path.display()
            );
            std::thread::sleep(*delay);
            // The stale-lock sweep touches the local filesystem (/proc, mtime),
            // so it only applies to a local worktree — a remote path can't be
            // stat'd here.
            if host.is_none() {
                cleanup_stale_index_lock(worktree_path);
            }
        }
        match git_stash(host, worktree_path) {
            Ok(stashed) => return Ok(stashed),
            Err(e) => {
                let msg = format!("{e:#}");
                if !is_transient_error(&msg) {
                    anyhow::bail!("{msg}");
                }
                last_err = msg;
            }
        }
    }

    anyhow::bail!("transient error persisted after retries: {last_err}")
}

/// Resolve the base ref a worktree should be compared/rebased against.
///
/// Prefers the branch's configured upstream (`@{upstream}`), then the remote's
/// advertised default branch (`origin/HEAD`, e.g. `origin/main` or
/// `origin/master` — whatever the repo actually uses), then the conventional
/// `origin/main` / `origin/master`. Returns `None` when none resolve, so the
/// caller can surface a clear error instead of blindly rebasing onto a
/// possibly-missing `origin/main`. Shared by [`sync_worktree`] (the rebase
/// target) and [`ahead_behind`] (the comparison base) so the "behind" count is
/// always measured against the ref sync would rebase onto.
fn resolve_base_ref(host: Option<&HostDef>, worktree_path: &Path) -> Option<String> {
    // A branch with an explicit upstream rebases onto exactly that.
    if run_git_capture_on(
        host,
        &["rev-parse", "--verify", "--quiet", "@{upstream}"],
        worktree_path,
    )
    .is_some()
    {
        return Some("@{upstream}".to_string());
    }

    // The remote's advertised HEAD (origin/HEAD → e.g. "origin/main").
    if let Some(out) = run_git_capture_on(
        host,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        worktree_path,
    ) {
        let advertised = out.trim();
        if !advertised.is_empty() {
            return Some(advertised.to_string());
        }
    }

    // Conventional fallbacks.
    ["origin/main", "origin/master"]
        .into_iter()
        .find(|r| {
            run_git_capture_on(
                host,
                &["rev-parse", "--verify", "--quiet", r],
                worktree_path,
            )
            .is_some()
        })
        .map(str::to_string)
}

/// High-level sync: stash, fetch, rebase onto the base ref, pop stash.
///
/// `base_ref` pins the ref to rebase onto; when `None` it is derived from the
/// worktree via `resolve_base_ref` (upstream → `origin/HEAD` →
/// `origin/main` → `origin/master`) rather than hardcoding `origin/main`.
/// On conflict the rebase is aborted and any stash is restored.
/// Retries `git stash` on transient index-lock errors.
pub fn sync_worktree(worktree_path: &Path, base_ref: Option<&str>) -> SyncResult {
    sync_worktree_on(None, worktree_path, base_ref)
}

/// [`sync_worktree`], optionally on a remote `host`. Every git subcommand runs
/// through the host launcher (`ssh …` / `wsl.exe …`), so `worktree_path` is
/// interpreted on the host — a remote worktree path never touches the local
/// filesystem (which was the "no such file or directory" bug).
pub fn sync_worktree_on(
    host: Option<&HostDef>,
    worktree_path: &Path,
    base_ref: Option<&str>,
) -> SyncResult {
    // Local-only stale-lock sweep; a remote worktree can't be stat'd here.
    if host.is_none() {
        cleanup_stale_index_lock(worktree_path);
    }

    let stashed = match stash_with_retry(host, worktree_path) {
        Ok(s) => s,
        Err(e) => return SyncResult::Error(format!("stash: {e:#}")),
    };

    let restore_stash = || {
        if stashed {
            let _ = git_stash_pop(host, worktree_path);
        }
    };

    if let Err(e) = git_fetch_on(host, worktree_path) {
        restore_stash();
        return SyncResult::Error(format!("fetch: {e:#}"));
    }

    // Resolve the rebase target after the fetch so derived refs (origin/HEAD,
    // origin/main, …) reflect the just-fetched remote state.
    let base_ref = match base_ref
        .map(str::to_string)
        .or_else(|| resolve_base_ref(host, worktree_path))
    {
        Some(r) => r,
        None => {
            restore_stash();
            return SyncResult::Error(
                "could not resolve a base ref to sync onto (no upstream, \
                 origin/HEAD, origin/main, or origin/master)"
                    .to_string(),
            );
        }
    };

    if let Err(e) = git_rebase_onto(host, worktree_path, &base_ref) {
        restore_stash();
        return SyncResult::Conflict(format!("{e:#}"));
    }

    if stashed {
        if let Err(e) = git_stash_pop(host, worktree_path) {
            return SyncResult::Error(format!("stash pop: {e:#}"));
        }
    }

    SyncResult::Synced
}

/// Run `git <args>` in `cwd` locally, returning stdout on success (`None`
/// otherwise).
fn run_git_capture(args: &[&str], cwd: &Path) -> Option<String> {
    run_git_capture_on(None, args, cwd)
}

/// [`run_git_capture`], optionally on a remote `host`.
fn run_git_capture_on(host: Option<&HostDef>, args: &[&str], cwd: &Path) -> Option<String> {
    let output = git_command(host, cwd, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Sum `git diff --numstat` output into `(files_changed, insertions, deletions)`.
/// Binary files (`-\t-\tpath`) count toward `files_changed` with zero lines.
fn parse_numstat(out: &str) -> (usize, usize, usize) {
    let (mut files, mut ins, mut dels) = (0usize, 0usize, 0usize);
    for line in out.lines() {
        let mut cols = line.split('\t');
        let added = cols.next();
        let deleted = cols.next();
        let path = cols.next();
        if path.is_none() {
            continue;
        }
        files += 1;
        if let Some(n) = added.and_then(|s| s.parse::<usize>().ok()) {
            ins += n;
        }
        if let Some(n) = deleted.and_then(|s| s.parse::<usize>().ok()) {
            dels += n;
        }
    }
    (files, ins, dels)
}

/// Commits the worktree's HEAD is `(ahead, behind)` relative to its base ref,
/// resolved by `resolve_base_ref` (upstream → `origin/HEAD` → `origin/main` →
/// `origin/master`) — the same chain [`sync_worktree`] rebases onto, so the
/// "behind" count is measured against the ref sync would use. Returns `(0, 0)`
/// when no base can be resolved.
pub fn ahead_behind(cwd: &Path) -> (usize, usize) {
    let Some(base) = resolve_base_ref(None, cwd) else {
        return (0, 0);
    };
    // `--left-right --count <base>...HEAD` → "<behind>\t<ahead>".
    let range = format!("{base}...HEAD");
    let Some(out) = run_git_capture(&["rev-list", "--left-right", "--count", &range], cwd) else {
        return (0, 0);
    };
    let mut parts = out.split_whitespace();
    let behind = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let ahead = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (ahead, behind)
}

/// Compute combined git stats (uncommitted diff + dirty + ahead/behind) for a
/// worktree. Returns `None` when the path is not a usable git worktree.
pub fn worktree_stats(cwd: &Path) -> Option<crate::session::GitStats> {
    // Bail early if this path isn't inside a git work tree.
    run_git_capture(&["rev-parse", "--is-inside-work-tree"], cwd)?;
    let numstat = run_git_capture(&["diff", "--numstat", "HEAD"], cwd).unwrap_or_default();
    let (files_changed, insertions, deletions) = parse_numstat(&numstat);
    // One `status --porcelain` drives both dirty (any output) and the untracked
    // count (`??` entries) — files a worktree removal would lose but that `diff
    // HEAD` never reports.
    let status = run_git_capture(&["status", "--porcelain"], cwd).unwrap_or_default();
    let dirty = !status.trim().is_empty();
    let untracked = status.lines().filter(|l| l.starts_with("??")).count();
    let (ahead, behind) = ahead_behind(cwd);
    Some(crate::session::GitStats {
        files_changed,
        insertions,
        deletions,
        untracked,
        dirty,
        ahead,
        behind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::TestPathGuard;

    #[test]
    fn git_program_scrubs_inherited_location_env() {
        // Git exports these to hook processes; git_program must mark every one
        // for removal so an inherited hook environment can't redirect a
        // path-targeted git call to the wrong repo (the bug that corrupted the
        // index when the suite ran under the pre-commit `cargo nextest` hook).
        let cmd = git_program();
        let removed: std::collections::HashSet<&str> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_str().unwrap())
            .collect();
        for var in GIT_LOCATION_ENV {
            assert!(removed.contains(var), "git_program must scrub {var}");
        }
    }

    /// A throwaway repository with one commit, to clone from.
    fn seed_repo(dir: &Path, file: &str, contents: &str) -> String {
        std::fs::create_dir_all(dir.parent().unwrap_or(dir)).ok();
        run_git(
            {
                let mut c = git_program();
                c.arg("init").arg("--initial-branch=main").arg(dir);
                c
            },
            "git init",
        )
        .expect("init");
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, contents).expect("write");
        for args in [
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["add", "-A"],
            vec!["commit", "-m", "seed"],
        ] {
            run_git(git_command(None, dir, &args), "git").expect("seed");
        }
        head_commit(dir).expect("head")
    }

    /// A clone delivers whatever the repository holds — including bytes no text
    /// path could carry — and reports the commit, not the ref.
    #[test]
    fn cloning_a_plugin_delivers_its_files_and_reports_the_commit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let origin = tmp.path().join("origin");
        // A pane in a nested directory, and a non-UTF-8 payload beside it: the exact
        // pair the text fetch path corrupts.
        let commit = seed_repo(&origin, "plugins/40_x.lua", "return {}\n");
        std::fs::write(origin.join("payload.bin"), [0x00u8, 0xff, 0xfe, 0x01]).expect("write");
        for args in [vec!["add", "-A"], vec!["commit", "-m", "payload"]] {
            run_git(git_command(None, &origin, &args), "git").expect("commit");
        }
        let commit = {
            let _ = commit;
            head_commit(&origin).expect("head")
        };

        let dest = tmp.path().join("ui").join("x");
        clone_plugin(&origin.to_string_lossy(), &dest, None).expect("clone");

        assert_eq!(
            std::fs::read(dest.join("payload.bin")).expect("read"),
            [0x00u8, 0xff, 0xfe, 0x01],
            "the bytes survive, which is the whole point of a clone"
        );
        assert!(dest.join("plugins/40_x.lua").is_file());
        assert!(is_working_copy(&dest), "the clone keeps its .git");
        assert_eq!(head_commit(&dest).expect("head"), commit);
        assert!(!is_dirty(&dest).expect("status"));
    }

    #[test]
    fn cloning_over_something_that_exists_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("taken");
        std::fs::create_dir_all(&dest).expect("mkdir");
        std::fs::write(dest.join("mine.lua"), "return {}").expect("write");
        let error =
            clone_plugin("https://example.com/x.git", &dest, None).expect_err("should refuse");
        assert!(error.to_string().contains("already exists"), "{error}");
        assert!(dest.join("mine.lua").is_file(), "and leaves it alone");
    }

    /// The property that makes git the right owner of "your edits are yours".
    #[test]
    fn an_edited_working_copy_reports_itself_dirty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let origin = tmp.path().join("origin");
        seed_repo(&origin, "plugins/40_x.lua", "return {}\n");
        let dest = tmp.path().join("clone");
        clone_plugin(&origin.to_string_lossy(), &dest, None).expect("clone");

        assert!(!is_dirty(&dest).expect("clean"));
        std::fs::write(dest.join("plugins/40_x.lua"), "-- mine\n").expect("edit");
        assert!(
            is_dirty(&dest).expect("dirty"),
            "an edit has to be visible, or nothing can refuse to overwrite it"
        );
    }

    /// A clone must never be able to sit waiting for a passphrase: an install runs
    /// from the command drain, so a prompt is a frozen interface.
    #[test]
    fn git_operations_refuse_to_prompt() {
        let mut cmd = git_program();
        non_interactive(&mut cmd);
        let envs: std::collections::HashMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();
        assert_eq!(
            envs.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
        let ssh = envs.get("GIT_SSH_COMMAND").expect("GIT_SSH_COMMAND");
        assert!(ssh.contains("BatchMode=yes"), "{ssh}");
        assert!(ssh.contains("ConnectTimeout"), "{ssh}");
    }

    #[test]
    fn parse_numstat_sums_changes() {
        let (files, ins, dels) = parse_numstat("1\t2\tfile.rs\n3\t4\tother.rs\n-\t-\tbin.png\n");
        assert_eq!(files, 3);
        assert_eq!(ins, 4);
        assert_eq!(dels, 6);
    }

    #[test]
    fn parse_numstat_empty_is_zero() {
        assert_eq!(parse_numstat(""), (0, 0, 0));
    }

    #[test]
    fn scan_child_repos_finds_git_subdirs_sorted_skipping_others() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Two git repos (.git dir), one plain dir, one hidden git dir.
        for name in ["beta", "alpha"] {
            std::fs::create_dir_all(root.join(name).join(".git")).unwrap();
        }
        std::fs::create_dir_all(root.join("plain")).unwrap();
        std::fs::create_dir_all(root.join(".hidden").join(".git")).unwrap();

        let repos = scan_child_repos(root);
        assert_eq!(repos, vec![root.join("alpha"), root.join("beta")]);
    }

    #[test]
    fn scan_child_repos_detects_git_file_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt = root.join("worktree");
        std::fs::create_dir_all(&wt).unwrap();
        // Worktree checkouts use a `.git` *file*, not a directory.
        std::fs::write(wt.join(".git"), "gitdir: /somewhere\n").unwrap();

        assert!(is_git_repo(&wt));
        assert_eq!(scan_child_repos(root), vec![wt]);
    }

    #[test]
    fn scan_child_repos_missing_parent_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(scan_child_repos(&tmp.path().join("nope")).is_empty());
    }

    #[test]
    fn create_or_attach_worktree_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let ok = git_program()
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("run git")
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("file.txt"), "hi").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        let base = String::from_utf8(
            git_program()
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        let _guard = TestPathGuard::new(tmp.path().join("data"));

        let p1 = create_or_attach_worktree(&repo, "feat/x", &base).expect("first creates");
        assert!(p1.exists());
        let p2 = create_or_attach_worktree(&repo, "feat/x", &base).expect("second reuses");
        assert_eq!(p1, p2);
        // Worktree dir gone but branch remains: re-attach a worktree to it.
        remove_worktree(&repo, &p1).expect("remove worktree");
        let p3 = create_or_attach_worktree(&repo, "feat/x", &base).expect("third reattaches");
        assert_eq!(p1, p3);
        assert!(p3.exists());
    }

    /// Compute the 16-char hex repo hash used in worktree paths.
    fn repo_hash(repo_path: &Path) -> String {
        stable_repo_hash(&repo_path.display().to_string())
    }

    #[test]
    fn worktree_path_simple_branch() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/home/user/repo");
        let result = worktree_path(repo, "main").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(result, base.join("worktrees").join(&hash).join("main"));
    }

    #[test]
    fn worktree_path_slash_branch() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/home/user/repo");
        let result = worktree_path(repo, "feature/foo").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(
            result,
            base.join("worktrees").join(&hash).join("feature-foo")
        );
    }

    #[test]
    fn worktree_path_nested_slashes() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/home/user/repo");
        let result = worktree_path(repo, "feature/team/task").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(
            result,
            base.join("worktrees").join(&hash).join("feature-team-task")
        );
    }

    #[test]
    fn worktree_path_no_slashes_unchanged() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/repo");
        let result = worktree_path(repo, "my-branch").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(result, base.join("worktrees").join(&hash).join("my-branch"));
    }

    #[test]
    fn worktree_path_trailing_slash() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/repo");
        let result = worktree_path(repo, "branch/").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(result, base.join("worktrees").join(&hash).join("branch-"));
    }

    #[test]
    fn worktree_path_leading_slash() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/repo");
        let result = worktree_path(repo, "/branch").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(result, base.join("worktrees").join(&hash).join("-branch"));
    }

    #[test]
    fn worktree_path_different_repos_produce_different_hashes() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let path_a = worktree_path(Path::new("/repo/a"), "main").unwrap();
        let path_b = worktree_path(Path::new("/repo/b"), "main").unwrap();
        assert_ne!(path_a, path_b);
        assert_eq!(path_a.file_name(), path_b.file_name());
    }

    #[test]
    fn worktree_path_same_repo_is_deterministic() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/home/user/repo");
        let first = worktree_path(repo, "main").unwrap();
        let second = worktree_path(repo, "main").unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn stable_repo_hash_is_pinned_fnv1a() {
        // Pin the exact output so a future swap back to a non-stable hasher
        // (e.g. DefaultHasher/SipHash, whose digest varies across builds) is
        // caught: these are the canonical FNV-1a 64-bit hashes for the inputs.
        assert_eq!(stable_repo_hash(""), "cbf29ce484222325");
        assert_eq!(stable_repo_hash("a"), "af63dc4c8601ec8c");
        assert_eq!(stable_repo_hash("/home/user/repo"), "96e5ae60e8caf52a");
    }

    #[test]
    fn resolve_base_ref_none_without_remote() {
        // A repo with no upstream and no remote refs resolves to no base ref,
        // so sync surfaces an error rather than rebasing onto a missing
        // `origin/main`.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let git = |args: &[&str]| {
            let out = git_program()
                .args(args)
                .current_dir(repo)
                .output()
                .expect("run git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("file.txt"), "hi").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);

        assert_eq!(resolve_base_ref(None, repo), None);
    }

    #[test]
    fn resolve_base_ref_prefers_upstream() {
        // A branch tracking an upstream resolves to `@{upstream}`, ahead of the
        // origin/HEAD and origin/main fallbacks.
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let work = tmp.path().join("work");
        let run = |dir: &Path, args: &[&str]| {
            let out = git_program()
                .args(args)
                .current_dir(dir)
                .output()
                .expect("run git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        // Bare remote to push to.
        std::fs::create_dir_all(&remote).unwrap();
        run(&remote, &["init", "-q", "--bare"]);

        // Working repo with one commit, pushed with upstream tracking (`-u`).
        std::fs::create_dir_all(&work).unwrap();
        run(&work, &["init", "-q"]);
        run(&work, &["config", "user.email", "t@example.com"]);
        run(&work, &["config", "user.name", "t"]);
        run(&work, &["config", "commit.gpgsign", "false"]);
        std::fs::write(work.join("file.txt"), "hi").unwrap();
        run(&work, &["add", "."]);
        run(&work, &["commit", "-qm", "init"]);
        run(
            &work,
            &["remote", "add", "origin", &remote.display().to_string()],
        );
        run(&work, &["push", "-q", "-u", "origin", "HEAD"]);

        assert_eq!(
            resolve_base_ref(None, &work),
            Some("@{upstream}".to_string())
        );
    }

    #[test]
    fn default_branch_prefers_main_over_master() {
        let branches = vec![
            "develop".to_string(),
            "master".to_string(),
            "main".to_string(),
        ];
        // Uses a non-existent path so the git command fails, exercising the fallback.
        let result = default_branch(Path::new("/nonexistent"), &branches);
        assert_eq!(result, Some("main".to_string()));
    }

    #[test]
    fn default_branch_falls_back_to_master() {
        let branches = vec!["develop".to_string(), "master".to_string()];
        let result = default_branch(Path::new("/nonexistent"), &branches);
        assert_eq!(result, Some("master".to_string()));
    }

    #[test]
    fn default_branch_returns_none_when_no_candidates() {
        let branches = vec!["develop".to_string(), "feature".to_string()];
        let result = default_branch(Path::new("/nonexistent"), &branches);
        assert_eq!(result, None);
    }

    #[test]
    fn default_branch_returns_none_for_empty_branches() {
        let result = default_branch(Path::new("/nonexistent"), &[]);
        assert_eq!(result, None);
    }

    #[test]
    fn transient_error_detects_could_not_write_index() {
        assert!(is_transient_error("error: could not write index"));
    }

    #[test]
    fn transient_error_detects_unable_to_write_new_index() {
        assert!(is_transient_error("fatal: Unable to write new index file"));
    }

    #[test]
    fn transient_error_detects_index_lock_exists() {
        assert!(is_transient_error(
            "fatal: Unable to create '/repo/.git/index.lock': File exists."
        ));
    }

    #[test]
    fn transient_error_detects_another_git_process() {
        assert!(is_transient_error(
            "Another git process seems to be running in this repository"
        ));
    }

    #[test]
    fn transient_error_rejects_auth_failure() {
        assert!(!is_transient_error(
            "fatal: Authentication failed for 'https://github.com/repo.git'"
        ));
    }

    #[test]
    fn transient_error_rejects_merge_conflict() {
        assert!(!is_transient_error(
            "CONFLICT (content): Merge conflict in src/main.rs"
        ));
    }

    #[test]
    fn transient_error_rejects_empty_string() {
        assert!(!is_transient_error(""));
    }

    #[test]
    fn transient_error_matches_within_anyhow_chain() {
        // is_transient_error is called with format!("{e:#}") which includes anyhow context
        assert!(is_transient_error(
            "git stash failed: could not write index"
        ));
        assert!(is_transient_error(
            "git stash failed: fatal: Unable to create '/repo/.git/index.lock': File exists."
        ));
    }

    #[test]
    fn try_remove_by_age_removes_old_lock() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("index.lock");
        std::fs::write(&lock, "").unwrap();

        // Backdate the file's mtime to exceed STALE_LOCK_AGE
        let old_time = std::time::SystemTime::now() - Duration::from_secs(120);
        let times = std::fs::FileTimes::new().set_modified(old_time);
        let file = std::fs::File::options().write(true).open(&lock).unwrap();
        file.set_times(times).unwrap();

        try_remove_by_age(&lock);
        assert!(!lock.exists(), "old lock should have been removed");
    }

    #[test]
    fn try_remove_by_age_preserves_fresh_lock() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("index.lock");
        std::fs::write(&lock, "").unwrap();

        try_remove_by_age(&lock);
        assert!(lock.exists(), "fresh lock should be preserved");
    }

    // ── remote / ssh helpers ────────────────────────────────────────

    fn host(dest: &str, wt_dir: Option<&str>) -> HostDef {
        HostDef {
            name: "h".into(),
            destination: dest.into(),
            ssh_opts: vec!["-o".into(), "ControlMaster=auto".into()],
            worktrees_dir: wt_dir.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    fn program_and_args(cmd: &Command) -> (String, Vec<String>) {
        (
            cmd.get_program().to_string_lossy().into_owned(),
            cmd.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        )
    }

    #[test]
    fn git_command_local_uses_git_with_args() {
        let cmd = git_command(None, Path::new("/repo"), &["branch", "--list"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "git");
        assert_eq!(args, ["branch", "--list"]);
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/repo")));
    }

    #[test]
    fn git_command_remote_wraps_in_ssh() {
        let h = host("me@box", None);
        let cmd = git_command(
            Some(&h),
            Path::new("/srv/repo"),
            &["worktree", "add", "-b", "x"],
        );
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "ssh");
        // User ssh_opts first, then the always-appended fail-fast hardening
        // (crate::shell::SSH_HARDENING_OPTS), then destination + remote git.
        let mut expected: Vec<String> = vec!["-o".into(), "ControlMaster=auto".into()];
        expected.extend(
            crate::shell::SSH_HARDENING_OPTS
                .iter()
                .map(|s| s.to_string()),
        );
        expected.extend(
            [
                "me@box",
                "git",
                "-C",
                "/srv/repo",
                "worktree",
                "add",
                "-b",
                "x",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        assert_eq!(args, expected);
        // No local current_dir is set for the remote variant.
        assert_eq!(cmd.get_current_dir(), None);
    }

    #[test]
    fn git_command_wsl_wraps_in_wsl_exe() {
        let h = HostDef::wsl("Ubuntu");
        let cmd = git_command(
            Some(&h),
            Path::new("/home/me/repo"),
            &["worktree", "add", "-b", "x"],
        );
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "wsl.exe");
        // A Unix caller passes `--cd /` so wsl.exe doesn't inherit (or mangle,
        // via a prefix-sibling distro name) a caller cwd missing from the
        // target distro — see `shell::wsl_command`.
        #[cfg(unix)]
        let prefix: &[&str] = &["-d", "Ubuntu", "--cd", "/"];
        #[cfg(not(unix))]
        let prefix: &[&str] = &["-d", "Ubuntu"];
        let expected: Vec<&str> = prefix
            .iter()
            .copied()
            .chain(["git", "-C", "/home/me/repo", "worktree", "add", "-b", "x"])
            .collect();
        assert_eq!(args, expected);
        // The child cwd is left to the OS default; `--cd` is the control.
        assert_eq!(cmd.get_current_dir(), None);
    }

    #[test]
    fn host_shell_c_wsl_passes_script_unquoted_via_exec() {
        // WSL: `--exec` hands argv to the in-distro process verbatim, so the
        // multi-statement script travels as a single *unquoted* arg. Without
        // it, wsl.exe substitutes `$…` inside the script and a pre-quoted
        // script arrives as one literal command word ("not found").
        let h = HostDef::wsl("Ubuntu");
        let cmd = host_shell_c(&h, "mkdir -p /a && ln -s /b /a/b");
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "wsl.exe");
        #[cfg(unix)]
        let prefix: &[&str] = &["-d", "Ubuntu", "--cd", "/"];
        #[cfg(not(unix))]
        let prefix: &[&str] = &["-d", "Ubuntu"];
        let expected: Vec<&str> = prefix
            .iter()
            .copied()
            .chain(["-e", "sh", "-c", "mkdir -p /a && ln -s /b /a/b"])
            .collect();
        assert_eq!(args, expected);
    }

    #[test]
    fn host_shell_c_ssh_posix_quotes_script() {
        // SSH space-joins its trailing args, so the script must be POSIX-quoted
        // to survive as a single `sh -c` argument.
        let h = host("me@box", None);
        let cmd = host_shell_c(&h, "mkdir -p /a && ln -s /b /a/b");
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "ssh");
        // The script arg is single-quoted as a whole.
        assert!(
            args.iter().any(|a| a == "'mkdir -p /a && ln -s /b /a/b'"),
            "script should be posix-quoted for ssh; got {args:?}"
        );
    }

    #[test]
    fn browse_scripts_posix_quote_the_user_typed_path() {
        // The dir/path is user-typed and embedded in a `sh -c` script — a
        // single quote or `$` must arrive literally, never as shell syntax.
        let tricky = "/srv/it's $HOME";
        for script in [
            list_dir_entries_script(tricky),
            classify_path_script(tricky),
            scan_child_repos_script(tricky),
        ] {
            assert!(
                script.contains(r#"'/srv/it'\''s $HOME'"#),
                "path must be posix-quoted in: {script}"
            );
        }
        // The listing script's protocol pieces are present.
        let script = list_dir_entries_script("/srv");
        assert!(script.contains("!missing"));
        assert!(script.contains("printf 'g %s\\n'"));
        assert!(script.contains("printf 'd %s\\n'"));
    }

    #[test]
    fn collect_scanned_children_joins_and_sorts() {
        let parent = Path::new("/srv/projects");
        let repos = collect_scanned_children(parent, "web\napi\n\n*\n").unwrap();
        assert_eq!(
            repos,
            vec![
                PathBuf::from("/srv/projects/api"),
                PathBuf::from("/srv/projects/web"),
            ]
        );
    }

    #[test]
    fn parse_dir_listing_reads_the_line_protocol() {
        // `g`/`d` tagged lines, sorted; unknown lines skipped, not fatal.
        let listing = parse_dir_listing("g thurbox\nd scratch\nnoise\ng api server\n");
        assert_eq!(
            listing,
            DirListing::Entries(vec![
                ("api server".into(), true),
                ("scratch".into(), false),
                ("thurbox".into(), true),
            ])
        );
        assert_eq!(parse_dir_listing("!missing\n"), DirListing::Missing);
        assert_eq!(parse_dir_listing(""), DirListing::Entries(Vec::new()));
    }

    #[test]
    fn parse_path_class_reads_the_single_word() {
        assert_eq!(parse_path_class("git\n").unwrap(), PathClass::Git);
        assert_eq!(parse_path_class("dir\n").unwrap(), PathClass::Dir);
        assert_eq!(parse_path_class("missing\n").unwrap(), PathClass::Missing);
        assert!(parse_path_class("garbage").is_err());
    }

    #[test]
    fn list_dir_entries_local_flags_git_repos_and_follows_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("repo").join(".git")).unwrap();
        std::fs::create_dir(root.join("plain")).unwrap();
        std::fs::create_dir(root.join(".hidden")).unwrap();
        std::fs::write(root.join("file.txt"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("plain"), root.join("link")).unwrap();

        let DirListing::Entries(entries) =
            list_dir_entries_on(None, &root.display().to_string()).unwrap()
        else {
            panic!("existing dir must not be Missing");
        };
        // Hidden entries included (the picker filters); files excluded; a
        // symlink to a dir is listed (the `ls -p` gap this fixes).
        assert!(entries.contains(&(".hidden".to_string(), false)));
        assert!(entries.contains(&("repo".to_string(), true)));
        assert!(entries.contains(&("plain".to_string(), false)));
        assert!(!entries.iter().any(|(n, _)| n == "file.txt"));
        #[cfg(unix)]
        assert!(entries.contains(&("link".to_string(), false)));

        assert_eq!(
            list_dir_entries_on(None, &root.join("nope").display().to_string()).unwrap(),
            DirListing::Missing
        );
    }

    #[test]
    fn expand_remote_tilde_passes_plain_paths_through() {
        // No `~` prefix → no remote round-trip, byte-identical passthrough
        // (including a mid-path `~`, which is not a home reference).
        let h = HostDef::wsl("Ubuntu");
        assert_eq!(
            expand_remote_tilde(&h, "/home/me/repos").unwrap(),
            "/home/me/repos"
        );
        assert_eq!(expand_remote_tilde(&h, "/data/~x").unwrap(), "/data/~x");
    }

    #[test]
    fn remote_workspace_dir_derives_base_from_worktrees_dir() {
        // With a configured worktrees_dir the path is pure (no host round-trip):
        // base = its parent, mirroring the local `<data root>/workspaces` layout.
        let h = host("me@box", Some("/data/wt"));
        let ws = remote_workspace_dir(&h, "abc-123").unwrap();
        assert_eq!(ws, "/data/workspaces/abc-123");
    }

    #[test]
    fn remote_workspace_dir_rejects_empty_id() {
        // An empty sanitized segment would make ensure/remove `rm -rf` the
        // workspaces *root* — must error like the local builder.
        let h = host("me@box", Some("/data/wt"));
        assert!(remote_workspace_dir(&h, "").is_err());
        assert!(remote_workspace_dir(&h, " .- ").is_err());
    }

    #[test]
    fn worktree_path_for_remote_uses_configured_dir() {
        let h = host("me@box", Some("/data/wt"));
        let path = worktree_path_for(Some(&h), Path::new("/srv/repo"), "feature/foo").unwrap();
        let s = path.display().to_string();
        assert!(s.starts_with("/data/wt/"), "got {s}");
        assert!(s.ends_with("/feature-foo"), "got {s}");
    }

    #[test]
    fn worktree_path_for_local_matches_worktree_path() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/home/user/repo");
        let via_for = worktree_path_for(None, repo, "main").unwrap();
        let direct = worktree_path(repo, "main").unwrap();
        assert_eq!(via_for, direct);
    }

    // ── parse_repo_name_from_url ────────────────────────────────────

    #[test]
    fn parse_ssh_url() {
        assert_eq!(
            parse_repo_name_from_url("git@github.com:user/thurbox.git"),
            Some("thurbox".to_string())
        );
    }

    #[test]
    fn parse_https_url_with_git_suffix() {
        assert_eq!(
            parse_repo_name_from_url("https://github.com/org/api-server.git"),
            Some("api-server".to_string())
        );
    }

    #[test]
    fn parse_https_url_without_git_suffix() {
        assert_eq!(
            parse_repo_name_from_url("https://github.com/org/api-server"),
            Some("api-server".to_string())
        );
    }

    #[test]
    fn parse_empty_url() {
        assert_eq!(parse_repo_name_from_url(""), None);
    }

    #[test]
    fn parse_ssh_url_no_user_path() {
        assert_eq!(
            parse_repo_name_from_url("git@host:repo.git"),
            Some("repo".to_string())
        );
    }

    #[test]
    fn parse_url_trailing_slash() {
        // Trailing slash produces empty last segment — rsplit('/').next() = ""
        assert_eq!(
            parse_repo_name_from_url("https://github.com/org/repo/"),
            None
        );
    }
}
