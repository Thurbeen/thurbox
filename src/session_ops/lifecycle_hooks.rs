//! Running the user's session lifecycle hooks (`hooks.toml`).
//!
//! The data is [`crate::session::hook_def`], the file is
//! [`crate::agent::hooks_config`]; this is the process. Each of the four
//! session pipelines in this module (`spawn`, `delete`, `restart`, `restore`)
//! calls [`fire_pre`] before its first side effect and [`fire_post`] after its
//! last, so a hook fires once per operation whichever interface asked — the
//! TUI, `thurbox-cli`, an automation, an extension — because every one of
//! them ends in those pipelines. That is the whole mechanism; nothing in the
//! kernel or the interface knows hooks exist.
//!
//! Everything here is synchronous and blocks the calling thread, which is a
//! worker in the TUI and the main thread in the CLI — never the render loop
//! (rule 5). `session_ops` has no async runtime to lean on, and entering one
//! from a thread that may already be inside one is the trap `kernel::terminal`
//! documents, so the timeout is a `try_wait` poll rather than a timed future.

use std::io::{Read, Write};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::session::{HookContext, HookEvent, LifecycleHook};
use crate::sync::SharedSession;

/// How often the runner checks whether the hook has exited. Coarse enough to
/// cost nothing, fine enough that a timeout lands within a frame of the
/// deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Bytes of each stream the reader keeps. Only the tail is reported, but a
/// hook that prints a megabyte still has to be *drained* or it blocks on a
/// full pipe — which is why the readers are threads and this is a cap rather
/// than a limit.
const STREAM_KEEP: usize = 64 * 1024;

/// How long to wait for the reader threads after the hook has exited. They end
/// when the pipes close, which is at once unless the hook left a grandchild
/// holding them (`sleep 100 &`); then the output is simply not reported, rather
/// than the operation waiting on a process the hook forgot about.
const READER_GRACE: Duration = Duration::from_millis(500);

/// Run every `pre_*` hook for `event`, in file order, stopping at the first
/// failure. `Err` is the reason the operation must not proceed: the caller
/// returns it before its first side effect.
pub fn fire_pre(event: HookEvent, ctx: &HookContext) -> Result<(), String> {
    debug_assert!(event.is_pre(), "{event} is not a pre-event");
    for hook in crate::agent::hooks_config::hooks_for(event) {
        if let Err(reason) = run_hook(&hook, event, ctx) {
            tracing::warn!("{reason}; refusing the operation");
            return Err(reason);
        }
    }
    Ok(())
}

/// Run every `post_*` hook for `event`, in file order, all of them. Returns
/// each failure's message; the operation has already succeeded and nothing
/// here can change that.
pub fn fire_post(event: HookEvent, ctx: &HookContext) -> Vec<String> {
    debug_assert!(!event.is_pre(), "{event} is not a post-event");
    let mut failures = Vec::new();
    for hook in crate::agent::hooks_config::hooks_for(event) {
        if let Err(reason) = run_hook(&hook, event, ctx) {
            tracing::warn!("{reason}");
            failures.push(reason);
        }
    }
    failures
}

/// The facts a persisted session yields for its delete/restart/restore hooks.
///
/// The primary repository is the first worktree's repository when there is
/// one — `cwd` is then the worktree — and `cwd` itself otherwise. The base
/// branch is a database column this row does not carry, so it stays unknown.
pub fn context_for(session: &SharedSession) -> HookContext {
    let primary = session.worktrees.first();
    HookContext {
        session_id: Some(session.id),
        name: session.name.clone(),
        agent: session.agent.clone(),
        agent_session_id: session.agent_session_id.clone(),
        repo: primary
            .map(|w| w.repo_path.clone())
            .or_else(|| session.cwd.clone()),
        cwd: session.cwd.clone(),
        branch: primary.map(|w| w.branch.clone()),
        host: host_name(&session.backend_type),
        parent_session_id: session.parent_session_id,
        backend_id: Some(session.backend_id.clone()).filter(|id| !id.is_empty()),
        worktrees: session.worktrees.iter().map(worktree).collect(),
        additional_dirs: session.additional_dirs.clone(),
        ..HookContext::default()
    }
}

/// The pane a session's row points at now — what a post-restart/restore hook
/// is told, since the pane the operation started with is gone. `None` when the
/// row is missing or the platform reported no id (psmux).
pub(crate) fn current_pane(
    db: &crate::storage::Database,
    id: crate::session::SessionId,
) -> Option<String> {
    db.get_session_by_id(id)
        .ok()
        .flatten()
        .map(|s| s.backend_id)
        .filter(|pane| !pane.is_empty())
}

/// The host a backend name denotes (`ssh:devbox` → `devbox`); `None` for local.
pub(crate) fn host_name(backend_type: &str) -> Option<String> {
    crate::session::is_remote_backend(backend_type)
        .then(|| {
            backend_type
                .split_once(':')
                .map(|(_, name)| name.to_string())
        })
        .flatten()
}

pub(crate) fn worktree(w: &crate::sync::SharedWorktree) -> crate::session::HookWorktree {
    crate::session::HookWorktree {
        repo_path: w.repo_path.clone(),
        worktree_path: w.worktree_path.clone(),
        branch: w.branch.clone(),
    }
}

/// Run one hook to completion or to its timeout.
///
/// The shell gets the inherited environment plus the `THURBOX_*` facts and the
/// config/data-dir overrides, the JSON on a piped stdin (written and closed
/// before waiting, so a hook that never reads it is not blocked by it), and
/// nothing of thurbox's own stdio — the TUI owns that terminal. Both output
/// pipes are drained on threads for the whole run; a deadline without draining
/// deadlocks on a full pipe, and draining without a deadline hangs on a hook
/// that never exits.
pub fn run_hook(hook: &LifecycleHook, event: HookEvent, ctx: &HookContext) -> Result<(), String> {
    let label = format!("hook `{}` for {event}", elide(&hook.command));
    tracing::debug!("{label}: running");

    let mut cmd = super::platform_shell(&hook.command);
    cmd.envs(ctx.env(event))
        .envs(super::thurbox_dir_overrides())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = ctx.workdir() {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{label} could not start: {e}"))?;

    // The payload is small, but a hook that exits without reading would make
    // a synchronous write fail on a closed pipe — and one that reads slowly
    // would stall the runner before its deadline was even armed.
    if let Some(mut stdin) = child.stdin.take() {
        let payload = ctx.json(event);
        std::thread::spawn(move || {
            let _ = stdin.write_all(payload.as_bytes());
        });
    }
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);

    let started = Instant::now();
    let timeout = hook.timeout();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("{label} timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(e) => break Err(format!("{label} could not be waited on: {e}")),
        }
    };
    let out_tail = stdout.map(collect).unwrap_or_default();
    let err_tail = stderr.map(collect).unwrap_or_default();

    let status = status?;
    if status.success() {
        return Ok(());
    }
    let code = status
        .code()
        .map_or_else(|| "with a signal".to_string(), |c| c.to_string());
    let detail = if err_tail.is_empty() {
        out_tail
    } else {
        err_tail
    };
    Err(match detail.is_empty() {
        true => format!("{label} exited {code}"),
        false => format!("{label} exited {code}: {detail}"),
    })
}

/// Read a stream to its end on a thread, keeping only the last
/// [`STREAM_KEEP`] bytes, and hand the tail back over a channel — so the
/// runner can wait for it *with a deadline* ([`READER_GRACE`]).
fn drain(mut stream: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut kept: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    kept.extend_from_slice(&chunk[..n]);
                    if kept.len() > 2 * STREAM_KEEP {
                        kept.drain(..kept.len() - STREAM_KEEP);
                    }
                }
            }
        }
        let _ = tx.send(kept);
    });
    rx
}

fn collect(rx: mpsc::Receiver<Vec<u8>>) -> String {
    rx.recv_timeout(READER_GRACE)
        .map(|bytes| super::exec_tail(&bytes))
        .unwrap_or_default()
}

/// The command as an error message names it: one line, at most 60 chars.
fn elide(command: &str) -> String {
    let one_line = command.lines().next().unwrap_or_default();
    let mut out: String = one_line.chars().take(60).collect();
    if out.len() < command.len() {
        out.push('…');
    }
    out
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::session::SessionId;

    fn hook(event: HookEvent, command: &str, timeout_secs: Option<u64>) -> LifecycleHook {
        LifecycleHook {
            event,
            command: command.to_string(),
            timeout_secs,
        }
    }

    fn write_hooks_file(dir: &std::path::Path, body: &str) {
        let path = crate::agent::hooks_config::hooks_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body.replace("{dir}", &dir.display().to_string())).unwrap();
    }

    #[test]
    fn a_hook_sees_the_environment_the_json_and_the_workdir() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let out = temp.path().join("seen");
        let sid = SessionId::default();
        let ctx = HookContext {
            session_id: Some(sid),
            name: "demo".into(),
            agent: "claude".into(),
            repo: Some(repo.clone()),
            branch: Some("feat/x".into()),
            ..HookContext::default()
        };
        let h = hook(
            HookEvent::PostCreate,
            &format!(
                "printf '%s\\n%s\\n%s\\n%s\\n' \"$THURBOX_HOOK_EVENT\" \"$THURBOX_SESSION\" \"$PWD\" \"$THURBOX_CONFIG_DIR\" > {out}; cat >> {out}",
                out = out.display()
            ),
            None,
        );
        run_hook(&h, HookEvent::PostCreate, &ctx).unwrap();

        let seen = std::fs::read_to_string(&out).unwrap();
        let mut lines = seen.lines();
        assert_eq!(lines.next(), Some("session.post_create"));
        assert_eq!(lines.next(), Some(sid.to_string().as_str()));
        assert_eq!(
            lines.next().map(std::path::PathBuf::from),
            Some(repo.canonicalize().unwrap())
        );
        let config_dir = lines.next().unwrap();
        assert_eq!(
            std::path::Path::new(config_dir),
            crate::paths::config_file().unwrap().parent().unwrap()
        );
        let json: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(json["event"], "session.post_create");
        assert_eq!(json["branch"], "feat/x");
    }

    #[test]
    fn a_non_zero_exit_reports_the_code_and_the_stderr_tail() {
        let h = hook(
            HookEvent::PreCreate,
            "echo 'refusing: protected branch' >&2; exit 3",
            None,
        );
        let err = run_hook(&h, HookEvent::PreCreate, &HookContext::default()).unwrap_err();
        assert!(
            err.contains("exited 3: refusing: protected branch"),
            "{err}"
        );
        assert!(err.contains("session.pre_create"), "{err}");
    }

    #[test]
    fn stdout_stands_in_when_stderr_is_silent() {
        let h = hook(HookEvent::PreCreate, "echo why; exit 1", None);
        let err = run_hook(&h, HookEvent::PreCreate, &HookContext::default()).unwrap_err();
        assert!(err.ends_with("exited 1: why"), "{err}");
    }

    #[test]
    fn a_hanging_hook_is_killed_at_its_timeout() {
        let h = hook(HookEvent::PreRestart, "sleep 5", Some(1));
        let started = Instant::now();
        let err = run_hook(&h, HookEvent::PreRestart, &HookContext::default()).unwrap_err();
        assert!(err.contains("timed out after 1s"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_chatty_hook_neither_deadlocks_nor_bloats_the_report() {
        // `yes` fills the pipe far faster than a 500-char tail could ever want;
        // without the reader threads the child would block on write and the
        // timeout would be the only way out.
        let h = hook(
            HookEvent::PostCreate,
            "yes | head -c 5000000; exit 2",
            Some(10),
        );
        let started = Instant::now();
        let err = run_hook(&h, HookEvent::PostCreate, &HookContext::default()).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "{:?}",
            started.elapsed()
        );
        assert!(err.contains("exited 2"), "{err}");
        assert!(
            err.len() < 700,
            "the report is a tail, not the stream: {}",
            err.len()
        );
    }

    #[test]
    fn a_backgrounded_grandchild_does_not_hold_the_operation() {
        let h = hook(HookEvent::PostCreate, "sleep 30 & exit 0", Some(5));
        let started = Instant::now();
        run_hook(&h, HookEvent::PostCreate, &HookContext::default()).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn pre_hooks_stop_at_the_first_failure() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        write_hooks_file(
            temp.path(),
            r#"
            [[hooks]]
            event = "session.pre_create"
            command = "touch {dir}/first; exit 1"
            [[hooks]]
            event = "session.pre_create"
            command = "touch {dir}/second"
            "#,
        );
        let err = fire_pre(HookEvent::PreCreate, &HookContext::default()).unwrap_err();
        assert!(err.contains("exited 1"), "{err}");
        assert!(temp.path().join("first").exists());
        assert!(
            !temp.path().join("second").exists(),
            "the second hook must not run"
        );
    }

    #[test]
    fn post_hooks_all_run_past_a_failure() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        write_hooks_file(
            temp.path(),
            r#"
            [[hooks]]
            event = "session.post_create"
            command = "touch {dir}/first; exit 1"
            [[hooks]]
            event = "session.post_create"
            command = "touch {dir}/second"
            [[hooks]]
            event = "session.post_delete"
            command = "touch {dir}/other-event"
            "#,
        );
        let failures = fire_post(HookEvent::PostCreate, &HookContext::default());
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(temp.path().join("first").exists());
        assert!(temp.path().join("second").exists());
        assert!(!temp.path().join("other-event").exists());
    }

    #[test]
    fn no_hooks_file_means_nothing_runs_and_nothing_fails() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        assert_eq!(
            fire_pre(HookEvent::PreDelete, &HookContext::default()),
            Ok(())
        );
        assert!(fire_post(HookEvent::PostDelete, &HookContext::default()).is_empty());
    }

    #[test]
    fn a_persisted_session_yields_its_facts() {
        let sid = SessionId::default();
        let session = SharedSession {
            id: sid,
            name: "demo".into(),
            agent: "codex".into(),
            backend_id: "%7".into(),
            backend_type: "ssh:devbox".into(),
            agent_session_id: Some("conv".into()),
            cwd: Some("/srv/wt".into()),
            additional_dirs: vec!["/srv/other".into()],
            worktrees: vec![crate::sync::SharedWorktree {
                repo_path: "/srv/repo".into(),
                worktree_path: "/srv/wt".into(),
                branch: "feat/x".into(),
            }],
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        let ctx = context_for(&session);
        assert_eq!(ctx.session_id, Some(sid));
        assert_eq!(ctx.repo.as_deref(), Some(std::path::Path::new("/srv/repo")));
        assert_eq!(ctx.cwd.as_deref(), Some(std::path::Path::new("/srv/wt")));
        assert_eq!(ctx.branch.as_deref(), Some("feat/x"));
        assert_eq!(ctx.host.as_deref(), Some("devbox"));
        assert_eq!(ctx.backend_id.as_deref(), Some("%7"));
        assert_eq!(ctx.worktrees.len(), 1);
        assert_eq!(ctx.additional_dirs.len(), 1);
        assert_eq!(host_name("local-tmux"), None);
        assert_eq!(host_name("wsl:Ubuntu").as_deref(), Some("Ubuntu"));
    }
}
