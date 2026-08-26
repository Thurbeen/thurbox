//! Session CRUD and orchestration subcommands.

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::output::{self, CommandOutput};
use crate::session::SessionId;
use crate::storage::Database;
use crate::sync::SharedSession;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// List all active sessions.
    List {
        /// Only list children of this parent session UUID.
        #[arg(long)]
        parent: Option<String>,
    },
    /// Get a session by UUID.
    Get {
        /// Session UUID.
        uuid: String,
    },
    /// Create a new session (runs synchronously — tmux window live on return).
    Create {
        /// Session name (1-64 chars, no slashes or leading '.').
        #[arg(long)]
        name: String,
        /// Absolute path to the repository or working directory.
        #[arg(long)]
        repo_path: PathBuf,
        /// Coding agent to launch (e.g. "claude", "codex"). Falls back to the
        /// default agent from `agents.toml`.
        #[arg(long)]
        agent: Option<String>,
        /// If set, create a git worktree on this branch off --base-branch.
        #[arg(long)]
        worktree_branch: Option<String>,
        /// Base branch for the worktree (default: main).
        #[arg(long)]
        base_branch: Option<String>,
        /// Remote host to run the session on (name from `hosts.toml`). The
        /// worktree and tmux window are created on that host over SSH.
        #[arg(long)]
        host: Option<String>,
        /// Parent session UUID (lead/worker relationship for orchestration).
        /// Must reference an existing active session.
        #[arg(long)]
        parent: Option<String>,
        /// Additional repo to span (repeatable). `PATH` or `PATH@BASE` — each
        /// gets its own isolated worktree on `--worktree-branch` off `BASE`
        /// (default: the primary's `--base-branch`). Makes a multi-repo session.
        #[arg(long = "add-repo")]
        add_repo: Vec<String>,
        /// Additional directory to span (repeatable), attached as-is (no
        /// worktree / branch). Makes a multi-repo session.
        #[arg(long = "add-dir")]
        add_dir: Vec<String>,
    },
    /// Soft-delete a session.
    ///
    /// By default only the DB row is soft-deleted (the TUI cleans up the
    /// tmux window and worktree on next sync). Pass `--force` to also
    /// kill the tmux window, remove worktrees, and cancel pending
    /// scheduled commands — useful for headless cleanup when the TUI
    /// isn't running.
    Delete {
        /// Session UUID.
        uuid: String,
        /// Also kill the tmux window, remove worktrees, and cancel
        /// pending scheduled commands for this session.
        #[arg(long)]
        force: bool,
    },
    /// Restore a soft-deleted session.
    Restore {
        /// Session UUID.
        uuid: String,
        /// Recover a force-deleted session best-effort: only committed branch
        /// state comes back (uncommitted/untracked work was lost on delete).
        #[arg(long)]
        best_effort: bool,
    },
    /// Restart a session in-place (kills the window, re-spawns with --resume).
    Restart {
        /// Session UUID.
        uuid: String,
    },
    /// Type text into a session's terminal, followed by Enter.
    Send {
        /// Session UUID.
        uuid: String,
        /// Text to send.
        text: String,
    },
    /// Capture rendered pane contents as text.
    Capture {
        /// Session UUID.
        uuid: String,
        /// Scrollback lines to include (default 200, max 10000).
        #[arg(long, default_value_t = 200)]
        lines: u32,
    },
    /// Mark a session as the pending focus target for the running TUI.
    ///
    /// Writes the session id into the SQLite `metadata` row the TUI polls;
    /// the next external-state tick reads + clears it and switches the
    /// active terminal. Used by the macOS click-to-focus path
    /// (`terminal-notifier -execute` shells back into this), and works as
    /// a generic "switch the TUI to `<session>`" hook from any external
    /// trigger. A no-op when the TUI isn't running (the request just
    /// sits in the DB until either it is or the row is overwritten).
    Focus {
        /// Session UUID.
        uuid: String,
    },
    /// Report an agent lifecycle transition (called from an agent hook).
    ///
    /// Records the session's state so the TUI can render it (working/blocked/
    /// done/idle) — works headless; the TUI picks it up via its data_version
    /// poll. Identity defaults to the calling session ($THURBOX_SESSION,
    /// injected at spawn), so an agent hook passes no id.
    Signal {
        /// The reported state. `idle` = agent ready/at-rest (e.g. a fresh
        /// session boot); `done` = a turn just finished (shows until you look).
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(crate::session::HOOK_STATES))]
        state: String,
        /// Override the calling session (UUID). Defaults to $THURBOX_SESSION,
        /// then a lookup by the agent conversation id ($THURBOX_SESSION_ID).
        #[arg(long)]
        session: Option<String>,
    },
}

pub fn run(action: Action, db: &Database) -> Result<CommandOutput, String> {
    match action {
        Action::List { parent } => {
            let parent_id = parent.as_deref().map(parse_session_id).transpose()?;
            let sessions: Vec<SharedSession> = db
                .list_active_sessions()
                .map_err(|e| format!("list_active_sessions: {e}"))?
                .into_iter()
                .filter(|s| parent_id.is_none() || s.parent_session_id == parent_id)
                .collect();
            let states = db.load_hook_states().unwrap_or_default();
            let json = Value::Array(
                sessions
                    .iter()
                    .map(|s| session_json_with_state(s, &states))
                    .collect(),
            );
            Ok(CommandOutput::new(json, render_session_list(&sessions)))
        }
        Action::Get { uuid } => {
            let session = resolve(db, &uuid)?;
            let states = db.load_hook_states().unwrap_or_default();
            Ok(CommandOutput::new(
                session_json_with_state(&session, &states),
                render_session_detail(&session),
            ))
        }
        Action::Create {
            name,
            repo_path,
            agent,
            worktree_branch,
            base_branch,
            host,
            parent,
            add_repo,
            add_dir,
        } => {
            let parent_session_id = parent.as_deref().map(parse_session_id).transpose()?;
            let extra_repos = super::parse_extra_repos(&add_repo, &add_dir);
            let req = crate::session_ops::SpawnRequest {
                name,
                repo_path,
                worktree_branch,
                base_branch,
                agent,
                host,
                parent_session_id,
                extra_repos,
                ..Default::default()
            };
            let res = crate::session_ops::spawn_session_headless(db, req)?;
            let mut human = format!(
                "Created session '{}' ({}) — {}\ncwd: {}",
                res.name,
                res.agent,
                res.session_id,
                res.cwd.display()
            );
            push_hook_failures(&mut human, &res.hook_failures);
            Ok(CommandOutput::new(
                json!({
                    "id": res.session_id.to_string(),
                    "name": res.name,
                    "agent": res.agent,
                    "agent_session_id": res.agent_session_id,
                    "cwd": res.cwd.display().to_string(),
                    "parent_session_id": res.parent_session_id.map(|id| id.to_string()),
                    "hook_failures": res.hook_failures,
                }),
                human,
            ))
        }
        Action::Delete { uuid, force } => delete_session(db, &uuid, force),
        Action::Restore { uuid, best_effort } => restore_deleted(db, &uuid, best_effort),
        Action::Restart { uuid } => {
            let session = resolve(db, &uuid)?;
            let report = crate::session_ops::restart_session_headless(db, session.id)?;
            let mut human = format!("Restarted session '{}' ({})", session.name, session.id);
            push_hook_failures(&mut human, &report.hook_failures);
            Ok(CommandOutput::new(
                json!({
                    "restarted": true,
                    "session_id": session.id.to_string(),
                    "session_name": session.name,
                    "hook_failures": report.hook_failures,
                }),
                human,
            ))
        }
        Action::Send { uuid, text } => {
            let session = resolve(db, &uuid)?;
            if text.trim().is_empty() {
                return Err("text must not be empty".into());
            }
            crate::agent::tmux::send_prompt_now(&session.name, &session.backend_id, &text)
                .map_err(|e| format!("send_prompt_now: {e}"))?;
            Ok(CommandOutput::new(
                json!({
                    "sent": true,
                    "session_id": session.id.to_string(),
                    "session_name": session.name,
                }),
                format!("Sent to '{}'.", session.name),
            ))
        }
        Action::Capture { uuid, lines } => {
            let session = resolve(db, &uuid)?;
            let output =
                crate::agent::tmux::capture_pane_text(&session.name, &session.backend_id, lines)
                    .map_err(|e| format!("capture_pane_text: {e}"))?;
            let human = output.clone();
            Ok(CommandOutput::new(
                json!({
                    "session_id": session.id.to_string(),
                    "session_name": session.name,
                    "lines": lines,
                    "output": output,
                }),
                human,
            ))
        }
        Action::Focus { uuid } => {
            let session = resolve(db, &uuid)?;
            db.set_pending_focus_session_id(session.id)
                .map_err(|e| format!("set_pending_focus_session_id: {e}"))?;
            Ok(CommandOutput::new(
                json!({
                    "focused": true,
                    "session_id": session.id.to_string(),
                    "session_name": session.name,
                }),
                format!("Focus requested for '{}'.", session.name),
            ))
        }
        Action::Signal { state, session } => {
            let target = resolve_signal_target(db, session.as_deref())?;
            db.set_hook_state(target.id, &state)
                .map_err(|e| format!("set_hook_state: {e}"))?;
            Ok(CommandOutput::new(
                json!({
                    "signaled": true,
                    "session_id": target.id.to_string(),
                    "session_name": target.name,
                    "state": state,
                }),
                format!("Signaled {state} for '{}'.", target.name),
            ))
        }
    }
}

/// Delete a session, reporting what `--force` teardown actually managed.
fn delete_session(db: &Database, uuid: &str, force: bool) -> Result<CommandOutput, String> {
    let session = resolve(db, uuid)?;
    let report = crate::session_ops::delete_session_headless(db, session.id, force)?;
    let mut human = format!("Deleted session '{}' ({})", session.name, session.id);
    if force {
        for line in output::kv(&force_delete_detail(&report)).lines() {
            human.push_str(&format!("\n  {line}"));
        }
    }
    push_hook_failures(&mut human, &report.hook_failures);
    Ok(CommandOutput::new(
        json!({
            "deleted": true,
            "id": session.id.to_string(),
            "name": session.name,
            "forced": force,
            "killed_window": report.killed_window,
            "removed_worktrees": report.removed_worktrees,
            "worktree_errors": report.worktree_errors,
            "disabled_automations": report.disabled_automations,
            "remote_teardown_error": report.remote_teardown_error,
            "hook_failures": report.hook_failures,
        }),
        human,
    ))
}

/// The human half of a `--force` delete: teardown counts, plus whichever of the
/// two best-effort failures happened.
fn force_delete_detail(
    report: &crate::session_ops::ForceDeleteReport,
) -> Vec<(&'static str, String)> {
    let mut detail = vec![
        ("killed window", report.killed_window.to_string()),
        (
            "removed worktrees",
            report.removed_worktrees.len().to_string(),
        ),
        (
            "disabled automations",
            report.disabled_automations.to_string(),
        ),
    ];
    if !report.worktree_errors.is_empty() {
        detail.push(("worktree errors", report.worktree_errors.join("; ")));
    }
    if let Some(err) = &report.remote_teardown_error {
        detail.push(("remote teardown error", err.clone()));
    }
    detail
}

/// Restore a deleted session: the row, its worktrees and its agent — the same
/// pipeline the interface's undo runs, so the two cannot disagree about what
/// restoring means (it used to clear the flag alone, handing back a session
/// with no worktree and no window). A force-deleted row is refused without
/// `--best-effort`, since only committed work can return.
fn restore_deleted(db: &Database, uuid: &str, best_effort: bool) -> Result<CommandOutput, String> {
    let id: SessionId = uuid
        .parse()
        .map_err(|_| format!("Invalid session UUID: {uuid}"))?;
    // The pipeline refuses this too, but its message is interface-neutral; the
    // command line is where `--best-effort` is the way to say yes.
    let deleted = db
        .get_deleted_session_by_id(id)
        .map_err(|e| format!("get_deleted_session_by_id: {e}"))?
        .ok_or_else(|| format!("Deleted session not found: {uuid}"))?;
    if deleted.force_deleted && !best_effort {
        return Err(format!(
            "Session '{}' was force-deleted; pass --best-effort to recover committed work (uncommitted/untracked changes are gone)",
            deleted.name
        ));
    }
    let report = crate::session_ops::restore_session_headless(db, id, best_effort)?;
    let mut human = match report.best_effort {
        true => format!(
            "Restored session '{}' ({id}) — best-effort: uncommitted work was not recovered",
            report.name
        ),
        false => format!("Restored session '{}' ({id})", report.name),
    };
    if report.worktrees_recovered < report.worktrees_wanted {
        human.push_str(&format!(
            "\n  worktrees recovered: {}/{}",
            report.worktrees_recovered, report.worktrees_wanted
        ));
    }
    if let Some(err) = &report.respawn_error {
        human.push_str(&format!("\n  agent not relaunched: {err}"));
    }
    push_hook_failures(&mut human, &report.hook_failures);
    Ok(CommandOutput::new(
        json!({
            "restored": true,
            "id": id.to_string(),
            "name": report.name,
            "best_effort": report.best_effort,
            "worktrees_wanted": report.worktrees_wanted,
            "worktrees_recovered": report.worktrees_recovered,
            "respawn_error": report.respawn_error,
            "hook_failures": report.hook_failures,
        }),
        human,
    ))
}

/// The human half of a post-hook failure list: one indented line each. The
/// operation succeeded; these are what the user's own hooks had to say.
fn push_hook_failures(human: &mut String, failures: &[String]) {
    for failure in failures {
        human.push_str(&format!("\n  hook failed: {failure}"));
    }
}

/// Resolve the session a `signal` targets: an explicit `--session` UUID, else
/// the calling session from `$THURBOX_SESSION`, else a lookup by the agent
/// conversation id from `$THURBOX_SESSION_ID` (the env fallback for agents whose
/// hooks don't inherit `$THURBOX_SESSION`). Errors when none resolves.
fn resolve_signal_target(db: &Database, session: Option<&str>) -> Result<SharedSession, String> {
    if let Some(uuid) = session {
        return resolve(db, uuid);
    }
    crate::cli::identity::calling_session_or_by_agent_id(db)?
        .ok_or_else(|| "not inside a thurbox session; pass --session <uuid>".into())
}

/// Render the session list as an aligned table (or a friendly empty line).
fn render_session_list(sessions: &[SharedSession]) -> String {
    if sessions.is_empty() {
        return "No active sessions.".to_string();
    }
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            // `dash` already maps an empty branch (no worktree) to "-".
            let branch = s.worktrees.first().map(|w| w.branch.as_str());
            vec![
                s.name.clone(),
                s.agent.clone(),
                s.backend_type.clone(),
                output::dash(branch),
                output::dash(s.cwd.as_ref().map(|p| p.display().to_string()).as_deref()),
                s.id.to_string(),
            ]
        })
        .collect();
    output::table(&["NAME", "AGENT", "BACKEND", "BRANCH", "CWD", "ID"], &rows)
}

/// Render a single session as an aligned key/value block, with any worktrees
/// listed one per line beneath it.
fn render_session_detail(s: &SharedSession) -> String {
    let pairs: Vec<(&str, String)> = vec![
        ("name", s.name.clone()),
        ("id", s.id.to_string()),
        ("agent", s.agent.clone()),
        ("backend", s.backend_type.clone()),
        (
            "agent_session_id",
            output::dash(s.agent_session_id.as_deref()),
        ),
        (
            "cwd",
            output::dash(s.cwd.as_ref().map(|p| p.display().to_string()).as_deref()),
        ),
        (
            "parent",
            output::dash(s.parent_session_id.map(|id| id.to_string()).as_deref()),
        ),
    ];
    let mut block = output::kv(&pairs);
    for w in &s.worktrees {
        block.push_str(&format!(
            "\nworktree:  {} @ {}",
            w.branch,
            w.worktree_path.display()
        ));
    }
    block
}

fn parse_session_id(uuid: &str) -> Result<SessionId, String> {
    uuid.parse()
        .map_err(|_| format!("Invalid session UUID: {uuid}"))
}

fn resolve(db: &Database, uuid: &str) -> Result<SharedSession, String> {
    let id = parse_session_id(uuid)?;
    db.get_session_by_id(id)
        .map_err(|e| format!("get_session_by_id: {e}"))?
        .ok_or_else(|| format!("Session not found: {uuid}"))
}

/// [`shared_session_to_json`] plus the session's persisted hooks-driven state
/// (`hook_state`: `working`/`blocked`/`done`/`idle`, `null` when never
/// reported). This is the **raw persisted value** written by `session signal`
/// and the headless remote-status poll — the TUI's display status additionally
/// derives exited→Idle and the stuck-`working` quiescence fallback, which need
/// a live pane and don't exist headless.
fn session_json_with_state(
    s: &SharedSession,
    states: &std::collections::HashMap<crate::session::SessionId, crate::storage::HookRow>,
) -> Value {
    let mut json = shared_session_to_json(s);
    json["hook_state"] = states
        .get(&s.id)
        .and_then(|r| r.state.clone())
        .map_or(Value::Null, Value::String);
    json
}

fn shared_session_to_json(s: &SharedSession) -> Value {
    json!({
        "id": s.id.to_string(),
        "name": s.name,
        "agent": s.agent,
        "backend_type": s.backend_type,
        "agent_session_id": s.agent_session_id,
        "cwd": s.cwd.as_ref().map(|p| p.display().to_string()),
        "parent_session_id": s.parent_session_id.map(|id| id.to_string()),
        "display_order": s.display_order,
        "worktrees": s.worktrees.iter().map(|w| json!({
            "repo_path": w.repo_path.display().to_string(),
            "worktree_path": w.worktree_path.display().to_string(),
            "branch": w.branch,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn list_empty_returns_array() {
        let db = db();
        let v = run(Action::List { parent: None }, &db).unwrap();
        assert!(v.is_array(), "got {v}");
        assert_eq!(v.as_array().unwrap().len(), 0);
        assert_eq!(v.human, "No active sessions.");
    }

    #[test]
    fn signal_explicit_session_sets_hook_state() {
        let db = db();
        let shared = make_test_session("worker");
        let id = shared.id;
        db.upsert_session(&shared).unwrap();

        let out = run(
            Action::Signal {
                state: "blocked".into(),
                session: Some(id.to_string()),
            },
            &db,
        )
        .unwrap();
        assert_eq!(out["state"], "blocked");
        assert_eq!(out["signaled"], true);

        let states = db.load_hook_states().unwrap();
        assert_eq!(states.get(&id).unwrap().state.as_deref(), Some("blocked"));
    }

    #[test]
    fn signal_without_identity_errors() {
        let db = db();
        // No --session and (in test) no THURBOX_SESSION env → clear error.
        std::env::remove_var("THURBOX_SESSION");
        std::env::remove_var("THURBOX_SESSION_ID");
        let err = run(
            Action::Signal {
                state: "done".into(),
                session: None,
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("not inside a thurbox session"), "got {err}");
    }

    fn make_test_session(name: &str) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: "claude".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    #[test]
    fn render_session_list_tabulates_rows() {
        let s = SharedSession {
            id: SessionId::default(),
            name: "demo".into(),
            agent: "dev".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: Some(std::path::PathBuf::from("/tmp/repo")),
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        let rendered = render_session_list(std::slice::from_ref(&s));
        assert!(rendered.contains("NAME"));
        assert!(rendered.contains("demo"));
        assert!(rendered.contains("local-tmux"));
        // No worktree → branch column shows a dash.
        assert!(rendered.contains('-'));
    }

    #[test]
    fn list_returns_session_with_expected_shape() {
        let db = db();
        let id = SessionId::default();
        let shared = SharedSession {
            id,
            name: "demo".into(),
            agent: "dev".into(),
            backend_id: "tb-demo".into(),
            backend_type: "local-tmux".into(),
            agent_session_id: Some("agent-1".into()),
            cwd: Some(std::path::PathBuf::from("/tmp/repo")),
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&shared).unwrap();

        let v = run(Action::List { parent: None }, &db).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let s = &arr[0];
        assert_eq!(s["id"].as_str(), Some(id.to_string().as_str()));
        assert_eq!(s["name"].as_str(), Some("demo"));
        assert_eq!(s["agent"].as_str(), Some("dev"));
        assert_eq!(s["backend_type"].as_str(), Some("local-tmux"));
        assert_eq!(s["agent_session_id"].as_str(), Some("agent-1"));
        assert_eq!(s["cwd"].as_str(), Some("/tmp/repo"));
        assert!(s["parent_session_id"].is_null());
        assert!(s["display_order"].is_null());
        assert!(s["worktrees"].is_array());
    }

    #[test]
    fn list_emits_parent_session_id_and_filters_by_parent() {
        let db = db();
        let parent_id = SessionId::default();
        let parent = SharedSession {
            id: parent_id,
            name: "lead".into(),
            agent: "dev".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&parent).unwrap();
        let mut child = parent.clone();
        child.id = SessionId::default();
        child.name = "worker".into();
        child.parent_session_id = Some(parent_id);
        db.upsert_session(&child).unwrap();

        // Unfiltered list carries the field on both rows.
        let v = run(Action::List { parent: None }, &db).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let worker = arr.iter().find(|s| s["name"] == "worker").unwrap();
        assert_eq!(
            worker["parent_session_id"].as_str(),
            Some(parent_id.to_string().as_str())
        );

        // --parent filters to direct children only.
        let v = run(
            Action::List {
                parent: Some(parent_id.to_string()),
            },
            &db,
        )
        .unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"].as_str(), Some("worker"));

        // Malformed --parent uuid errors.
        let err = run(
            Action::List {
                parent: Some("not-a-uuid".into()),
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("Invalid session UUID"), "got {err}");
    }

    #[test]
    fn get_unknown_uuid_errors() {
        let db = db();
        let err = run(
            Action::Get {
                uuid: "not-a-uuid".into(),
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("Invalid session UUID"), "got {err}");
    }

    #[test]
    fn send_rejects_empty_text() {
        let db = db();
        let id = SessionId::default();
        let shared = SharedSession {
            id,
            name: "demo".into(),
            agent: "dev".into(),
            backend_id: "tb-demo".into(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&shared).unwrap();
        // Both empty and whitespace-only text are rejected (trimmed check).
        for text in ["", "   \t\n"] {
            let err = run(
                Action::Send {
                    uuid: id.to_string(),
                    text: text.to_string(),
                },
                &db,
            )
            .unwrap_err();
            assert!(err.contains("text"), "got {err}");
        }
    }

    #[test]
    fn soft_delete_leaves_session_recoverable() {
        // `session delete` without --force only soft-deletes the DB row,
        // leaving its automations enabled and the session restorable.
        let db = db();
        let id = SessionId::default();
        let shared = SharedSession {
            id,
            name: "Foo Bar".into(),
            agent: "dev".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        };
        db.upsert_session(&shared).unwrap();
        let auto = db
            .create_automation(&crate::storage::automations::NewAutomation {
                name: "noop".into(),
                enabled: true,
                schedule: crate::session::AutomationSchedule::Once { at: u64::MAX },
                timezone: None,
                action: crate::session::AutomationAction::Send { session_id: id },
                prompt: "noop".into(),
                next_run_at: Some(u64::MAX),
            })
            .unwrap();

        let v = run(
            Action::Delete {
                uuid: id.to_string(),
                force: false,
            },
            &db,
        )
        .unwrap();
        assert_eq!(v["deleted"], true);
        assert_eq!(v["forced"], false);
        assert_eq!(v["disabled_automations"], 0);

        // Row is soft-deleted but recoverable; the automation is untouched.
        assert!(db.get_session_by_id(id).unwrap().is_none());
        assert!(db.get_automation(auto).unwrap().unwrap().enabled);
        let restored = run(
            Action::Restore {
                uuid: id.to_string(),
                best_effort: false,
            },
            &db,
        )
        .unwrap();
        assert_eq!(restored["restored"], true);
        assert_eq!(restored["best_effort"], false);
        assert!(db.get_session_by_id(id).unwrap().is_some());
    }

    #[test]
    fn restore_force_deleted_requires_best_effort_flag() {
        let db = db();
        let shared = make_test_session("forced");
        let id = shared.id;
        db.upsert_session(&shared).unwrap();

        // Force-delete marks the row force_deleted; restore must then opt in.
        run(
            Action::Delete {
                uuid: id.to_string(),
                force: true,
            },
            &db,
        )
        .unwrap();

        let err = run(
            Action::Restore {
                uuid: id.to_string(),
                best_effort: false,
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("--best-effort"), "{err}");
        // Still soft-deleted (refused).
        assert!(db.get_deleted_session_by_id(id).unwrap().is_some());

        let restored = run(
            Action::Restore {
                uuid: id.to_string(),
                best_effort: true,
            },
            &db,
        )
        .unwrap();
        assert_eq!(restored["restored"], true);
        assert_eq!(restored["best_effort"], true);
        assert!(db.get_session_by_id(id).unwrap().is_some());
    }
}
