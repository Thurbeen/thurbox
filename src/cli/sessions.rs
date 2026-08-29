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
        /// List the deleted sessions instead — what a peer thurbox mirroring
        /// this machine reads, with each row's `force_deleted` mark.
        #[arg(long)]
        deleted: bool,
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
        /// Relaunch only if the session has no live window; a running session
        /// is left alone. What a peer asks for after this host rebooted.
        #[arg(long)]
        if_missing: bool,
    },
    /// Mirror the sessions of a shareable host (or every one) into this
    /// database — the pass the interface runs on its own cadence.
    Sync {
        /// One host from hosts.toml; every shareable host when omitted.
        #[arg(long)]
        host: Option<String>,
        /// Also register, on the host, the local sessions it does not know —
        /// ones created here before the host's database became the record.
        #[arg(long)]
        adopt: bool,
    },
    /// Record a session that is already running on this machine's tmux server
    /// — a row for a window a peer created before sharing existed. Takes the
    /// JSON `session get` prints; launches nothing.
    Register {
        /// The session as `session get --json` prints it.
        #[arg(long = "json-row")]
        json_row: String,
    },
    /// Type text into a session's terminal, followed by Enter.
    ///
    /// The text is delivered as one bracketed paste, so it arrives literally —
    /// no shell sees it, and a leading `-`, quotes or newlines survive intact.
    /// Local sessions only: the pane lives on this machine's tmux server, so a
    /// session on a `--host` runs `thurbox-cli` there instead.
    Send {
        /// Session UUID.
        uuid: String,
        /// Text to send.
        text: String,
        /// Type the text but do not press Enter, leaving it unsubmitted in the
        /// agent's composer. `session key <uuid> enter` submits it — which is
        /// what a type-then-verify-then-submit protocol needs.
        #[arg(long = "no-enter")]
        no_enter: bool,
    },
    /// Send one named special key to a session's terminal.
    ///
    /// The companion to `session send --no-enter`: `escape` and `ctrl-c`
    /// interrupt a turn, `ctrl-u` clears a composer line, `enter` submits what
    /// is typed. Local sessions only, like `session send`.
    Key {
        /// Session UUID.
        uuid: String,
        /// Key name. One of: enter, escape, tab, backspace, space, up, down,
        /// left, right, home, end, page-up, page-down, delete — or
        /// `ctrl-<letter>` (e.g. ctrl-c, ctrl-u). Case-insensitive; `ctrl+c`
        /// and `c-c` spell the same key.
        key: String,
    },
    /// Capture rendered pane contents, plus the pane's live cursor and
    /// foreground-process state.
    ///
    /// `--json` additionally reports `cursor_row`/`cursor_col` (0-based,
    /// relative to the visible pane), `foreground_process` and
    /// `foreground_command` (what is running in the pane's tty right now, argv0
    /// and full command line), and `foreground_cwd` (where that process is —
    /// unlike `session get`'s `cwd`, which is where the session was launched).
    /// Any of them is `null` when it cannot be determined. Local sessions only:
    /// a session created with `--host` has no pane on this machine.
    Capture {
        /// Session UUID.
        uuid: String,
        /// Scrollback lines to include (default 200, max 10000).
        #[arg(long, default_value_t = 200)]
        lines: u32,
        /// Preserve ANSI styling in the captured text instead of flattening it
        /// to plain text.
        #[arg(long)]
        ansi: bool,
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
        Action::List {
            deleted: true,
            parent: _,
        } => {
            let rows = db
                .list_deleted_sessions()
                .map_err(|e| format!("list_deleted_sessions: {e}"))?;
            let json = Value::Array(rows.iter().map(deleted_session_to_json).collect());
            let human = if rows.is_empty() {
                "No deleted sessions.".to_string()
            } else {
                output::table(
                    &["NAME", "AGENT", "BACKEND", "RECOVERABLE", "ID"],
                    &rows
                        .iter()
                        .map(|r| {
                            vec![
                                r.name.clone(),
                                r.agent.clone(),
                                r.backend_type.clone(),
                                if r.force_deleted { "in part" } else { "fully" }.to_string(),
                                r.id.to_string(),
                            ]
                        })
                        .collect::<Vec<_>>(),
                )
            };
            Ok(CommandOutput::new(json, human)
                .list(
                    "deleted_sessions",
                    &["name", "agent", "force_deleted", "id"],
                )
                .empty("0 deleted sessions to restore")
                .help([
                    "thurbox-cli session restore <id>   bring one back",
                    "thurbox-cli session restore <id> --best-effort   for a force-deleted row",
                ]))
        }
        Action::List {
            parent,
            deleted: false,
        } => {
            let parent_id = parent.as_deref().map(parse_session_id).transpose()?;
            let sessions: Vec<SharedSession> = db
                .list_active_sessions()
                .map_err(|e| format!("list_active_sessions: {e}"))?
                .into_iter()
                .filter(|s| parent_id.is_none() || s.parent_session_id == parent_id)
                .collect();
            let states = db.load_hook_states().unwrap_or_default();
            let bases = db.load_base_branches().unwrap_or_default();
            let json = Value::Array(
                sessions
                    .iter()
                    .map(|s| session_json_with_state(s, &states, &bases))
                    .collect(),
            );
            Ok(CommandOutput::new(json, render_session_list(&sessions))
                // The id is not decoration: every follow-up command resolves a
                // session by UUID, so omitting it would only buy a second call.
                .list("sessions", &["name", "agent", "hook_state", "id"])
                .empty(match parent_id {
                    Some(id) => format!("0 sessions with parent {id}"),
                    None => "0 active sessions on this machine".to_string(),
                })
                .help([
                    "thurbox-cli session get <id>   the full record, worktrees included",
                    "thurbox-cli session capture <id> --lines 50   what its pane is showing",
                    "thurbox-cli session list --json   every field, for a script",
                ]))
        }
        Action::Get { uuid } => {
            let session = resolve(db, &uuid)?;
            let states = db.load_hook_states().unwrap_or_default();
            let bases = db.load_base_branches().unwrap_or_default();
            Ok(CommandOutput::new(
                session_json_with_state(&session, &states, &bases),
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
            // A host driven from afar has no interface of its own to arm the
            // heartbeat: this creation is the moment its sessions start needing
            // the tick (status polls, extension self-heal, reaping).
            super::automations::arm_heartbeat();
            let mut human = format!(
                "Created session '{}' ({}) — {}\ncwd: {}",
                res.name,
                res.agent,
                res.session_id,
                res.cwd.display()
            );
            if let Some(note) = &res.sharing {
                human.push_str(&format!("\n  {note}"));
            }
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
                    "sharing": res.sharing,
                }),
                human,
            ))
        }
        Action::Delete { uuid, force } => delete_session(db, &uuid, force),
        Action::Restore { uuid, best_effort } => restore_deleted(db, &uuid, best_effort),
        Action::Restart { uuid, if_missing } => {
            let session = resolve(db, &uuid)?;
            let report = crate::session_ops::restart::restart_session_headless_with(
                db, session.id, if_missing,
            )?;
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
        Action::Send {
            uuid,
            text,
            no_enter,
        } => {
            let session = resolve(db, &uuid)?;
            if text.trim().is_empty() {
                return Err("text must not be empty".into());
            }
            require_local_pane(&session)?;
            let submit = !no_enter;
            crate::agent::tmux::send_text_now(&session.name, &session.backend_id, &text, submit)
                .map_err(|e| format!("send_text_now: {e}"))?;
            let human = if submit {
                format!("Sent to '{}'.", session.name)
            } else {
                format!("Typed into '{}' (not submitted).", session.name)
            };
            Ok(CommandOutput::new(
                json!({
                    "sent": true,
                    "submitted": submit,
                    "session_id": session.id.to_string(),
                    "session_name": session.name,
                }),
                human,
            ))
        }
        Action::Key { uuid, key } => {
            let session = resolve(db, &uuid)?;
            let resolved =
                crate::agent::tmux::resolve_key(&key).ok_or_else(|| unknown_key(&key))?;
            require_local_pane(&session)?;
            crate::agent::tmux::send_key_now(&session.name, &session.backend_id, &resolved.tmux)
                .map_err(|e| format!("send_key_now: {e}"))?;
            Ok(CommandOutput::new(
                json!({
                    "sent": true,
                    "key": resolved.name,
                    "tmux_key": resolved.tmux,
                    "session_id": session.id.to_string(),
                    "session_name": session.name,
                }),
                format!("Sent {} to '{}'.", resolved.name, session.name),
            ))
        }
        Action::Capture { uuid, lines, ansi } => capture_pane(db, &uuid, lines, ansi),
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
        Action::Sync { host, adopt } => {
            let reports = crate::session_ops::mirror::sync(db, host.as_deref(), adopt)?;
            let json = Value::Array(reports.iter().map(|r| r.to_json()).collect());
            let human = if reports.is_empty() {
                "No shareable hosts configured.".to_string()
            } else {
                reports
                    .iter()
                    .map(render_mirror_report)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(CommandOutput::new(json, human))
        }
        Action::Register { json_row } => {
            let value: Value =
                serde_json::from_str(&json_row).map_err(|e| format!("--json-row: {e}"))?;
            let row = crate::session_ops::mirror::session_from_json(
                &value,
                crate::session_ops::spawn::LOCAL_TMUX_BACKEND_TYPE,
            )?;
            register_running_session(db, row)
        }
        Action::Signal { state, session } => {
            let target = resolve_signal_target(db, session.as_deref())?;
            db.set_hook_state(target.id, &state)
                .map_err(|e| format!("set_hook_state: {e}"))?;
            // The same state on the pane, for a peer's live subscription:
            // best-effort, and nothing at all outside tmux.
            if let Err(e) = crate::agent::tmux::set_own_pane_state(&state) {
                tracing::debug!("could not set the pane state option: {e:#}");
            }
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

/// Read a session's pane: its rendered text, and the live state around it.
///
/// The text is the whole human rendering, unchanged — the cursor position,
/// foreground process and live cwd are additive JSON fields, so a caller that
/// only ever read `output` sees exactly what it always did.
///
/// Refuses a remote session up front. `capture` has only ever read the *local*
/// multiplexer, so a `--host` session's pane — which lives on that host's own
/// tmux server — was already unreachable here; saying so beats the "can't find
/// window" tmux reports for a window that was never meant to be local.
fn capture_pane(
    db: &Database,
    uuid: &str,
    lines: u32,
    ansi: bool,
) -> Result<CommandOutput, String> {
    let session = resolve(db, uuid)?;
    if crate::session::is_remote_backend(&session.backend_type) {
        return Err(format!(
            "Session '{}' runs on backend '{}'; capture reads the local multiplexer only",
            session.name, session.backend_type
        ));
    }
    let output =
        crate::agent::tmux::capture_pane_text(&session.name, &session.backend_id, lines, ansi)
            .map_err(|e| format!("capture_pane_text: {e}"))?;
    // Read after the capture, so a pane that is simply not there fails as it
    // always has rather than reporting a screenful of nothing with null state.
    // Same target resolution, so the state describes the pane just captured.
    let state = crate::agent::tmux::pane_state(&session.name, &session.backend_id);
    let human = output.clone();
    Ok(CommandOutput::new(
        json!({
            "session_id": session.id.to_string(),
            "session_name": session.name,
            "lines": lines,
            "ansi": ansi,
            "output": output,
            "cursor_row": state.cursor_row,
            "cursor_col": state.cursor_col,
            "foreground_process": state.foreground_process,
            "foreground_command": state.foreground_command,
            "foreground_cwd": state.foreground_cwd,
        }),
        human,
    )
    // `--lines` is the real control here and the caller already chose it; this
    // cap only catches the case where those lines are far wider than anyone
    // expected. `--json` is uncapped, which is what the `| jq -r .output`
    // sentinel greps in the extensions rely on.
    .truncate(CAPTURE_TEXT_CAP)
    .help([
        "thurbox-cli session capture <id> --lines 40   a shorter tail",
        "thurbox-cli session send <id> <text>   type into the pane",
    ]))
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

/// How much captured pane text the TOON view shows before it says how much
/// more there is. Roughly a screenful of wide output — past that an agent is
/// paying for scrollback it did not ask to read, and `--lines`/`--full` are
/// both one flag away.
const CAPTURE_TEXT_CAP: usize = 4000;

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

/// Refuse a session whose pane is not on this machine.
///
/// `session send` and `session key` are one-shots against the *local* tmux
/// server (`agent::tmux`'s helpers bypass the transport seam), so an `ssh:`/
/// `wsl:` session has no pane here to type into. Left to itself the local
/// `send-keys` fails against a window that does not exist — a tmux status code
/// blaming the wrong machine — so name the reason instead. Same exit code (1),
/// a usable message.
fn require_local_pane(session: &SharedSession) -> Result<(), String> {
    if crate::session::is_remote_backend(&session.backend_type) {
        return Err(format!(
            "session '{}' runs on '{}'; `session send` and `session key` reach \
             only this machine's tmux server — run thurbox-cli on that host",
            session.name, session.backend_type
        ));
    }
    Ok(())
}

/// What `session key` says about a spelling it does not know, listing the set
/// so the answer is in the error rather than in `--help`.
fn unknown_key(key: &str) -> String {
    let names = crate::agent::tmux::NAMED_KEYS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    format!("Unknown key '{key}'. Known keys: {names}, or ctrl-<letter> (e.g. ctrl-c).")
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
    bases: &std::collections::HashMap<crate::session::SessionId, String>,
) -> Value {
    crate::session_ops::mirror::session_to_json(
        s,
        states.get(&s.id).and_then(|r| r.state.as_deref()),
        bases.get(&s.id).map(String::as_str),
    )
}

/// A deleted row as `session list --deleted` prints it: the session's facts
/// plus when it went and whether only committed work can come back.
fn deleted_session_to_json(r: &crate::storage::DeletedSessionInfo) -> Value {
    json!({
        "id": r.id.to_string(),
        "name": r.name,
        "agent": r.agent,
        "backend_type": r.backend_type,
        "backend_id": r.backend_id,
        "agent_session_id": r.agent_session_id,
        "cwd": r.cwd.as_ref().map(|p| p.display().to_string()),
        "parent_session_id": r.parent_session_id.map(|id| id.to_string()),
        "deleted_at": r.deleted_at,
        "force_deleted": r.force_deleted,
        "worktrees": r.worktrees.iter().map(|w| json!({
            "repo_path": w.repo_path.display().to_string(),
            "worktree_path": w.worktree_path.display().to_string(),
            "branch": w.branch,
        })).collect::<Vec<_>>(),
    })
}

fn render_mirror_report(r: &crate::session_ops::mirror::MirrorReport) -> String {
    match &r.error {
        Some(error) => format!("{}: not mirrored — {error}", r.host),
        None => format!(
            "{}: {} adopted, {} updated, {} deleted, {} restored{}{}",
            r.host,
            r.adopted.len(),
            r.updated.len(),
            r.deleted.len(),
            r.restored.len(),
            match r.unknown_local.len() {
                0 => String::new(),
                n => format!(", {n} local session(s) the host does not know (use --adopt)"),
            },
            match r.registered.len() {
                0 => String::new(),
                n => format!(", {n} registered on the host"),
            },
        ),
    }
}

/// `session register`: a row for an agent window that is already running on
/// this machine's server. The window must exist — this records, it never
/// launches — and neither the id nor the name may already be a session here.
fn register_running_session(
    db: &Database,
    row: crate::session_ops::mirror::HostRow,
) -> Result<CommandOutput, String> {
    let mut session = row.session;
    if db
        .get_session_by_id(session.id)
        .map_err(|e| format!("get_session_by_id: {e}"))?
        .is_some()
    {
        return Err(format!("session {} is already registered here", session.id));
    }
    if let Some(existing) = db
        .get_session_by_name(&session.name)
        .map_err(|e| format!("get_session_by_name: {e}"))?
    {
        return Err(format!(
            "a session named '{}' already exists here ({})",
            session.name, existing.id
        ));
    }
    let pane = crate::agent::tmux::agent_window_pane(None, &session.name)
        .map_err(|e| format!("could not list windows: {e:#}"))?
        .ok_or_else(|| {
            format!(
                "no live window for '{}' on this machine's tmux server; register records a \
                 running session, it does not launch one",
                session.name
            )
        })?;
    session.backend_id = pane;
    db.upsert_session(&session)
        .map_err(|e| format!("upsert_session: {e}"))?;
    if let Some(state) = row.hook_state.as_deref() {
        let _ = db.set_hook_state(session.id, state);
    }
    if let Some(base) = row.base_branch.as_deref() {
        let _ = db.set_session_base_branch(session.id, base);
    }
    Ok(CommandOutput::new(
        json!({
            "registered": true,
            "id": session.id.to_string(),
            "name": session.name,
            "backend_id": session.backend_id,
        }),
        format!(
            "Registered running session '{}' ({})",
            session.name, session.id
        ),
    ))
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
        let v = run(
            Action::List {
                parent: None,
                deleted: false,
            },
            &db,
        )
        .unwrap();
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

        let v = run(
            Action::List {
                parent: None,
                deleted: false,
            },
            &db,
        )
        .unwrap();
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
        let v = run(
            Action::List {
                parent: None,
                deleted: false,
            },
            &db,
        )
        .unwrap();
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
                deleted: false,
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
                deleted: false,
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
        // Both empty and whitespace-only text are rejected (trimmed check),
        // whether or not the text would have been submitted.
        for text in ["", "   \t\n"] {
            for no_enter in [false, true] {
                let err = run(
                    Action::Send {
                        uuid: id.to_string(),
                        text: text.to_string(),
                        no_enter,
                    },
                    &db,
                )
                .unwrap_err();
                assert!(err.contains("text"), "got {err}");
            }
        }
    }

    #[test]
    fn send_and_key_refuse_a_session_on_another_host() {
        // The one-shot helpers drive this machine's tmux server, so a remote
        // session has no pane here. It must say so rather than fail as a tmux
        // status code against a window that was never going to exist.
        let db = db();
        let mut shared = make_test_session("remote-demo");
        shared.backend_type = "ssh:devbox".into();
        let id = shared.id;
        db.upsert_session(&shared).unwrap();

        for action in [
            Action::Send {
                uuid: id.to_string(),
                text: "hello".into(),
                no_enter: true,
            },
            Action::Key {
                uuid: id.to_string(),
                key: "enter".into(),
            },
        ] {
            let err = run(action, &db).unwrap_err();
            assert!(err.contains("ssh:devbox"), "got {err}");
            assert!(err.contains("this machine"), "got {err}");
        }
    }

    #[test]
    fn key_refuses_a_name_tmux_would_type_as_text() {
        // Rejected before anything is sent: tmux injects an unrecognized key
        // name into the pane as literal text, so a typo must not reach it.
        let db = db();
        let shared = make_test_session("demo");
        let id = shared.id;
        db.upsert_session(&shared).unwrap();

        let err = run(
            Action::Key {
                uuid: id.to_string(),
                key: "escpe".into(),
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("Unknown key 'escpe'"), "got {err}");
        // The error carries the answer, so `--help` is not the only place it is.
        assert!(err.contains("escape") && err.contains("ctrl-"), "got {err}");
    }

    #[test]
    fn key_reports_a_missing_session_before_a_bad_key() {
        // A malformed UUID is still the first thing wrong with the call.
        let db = db();
        let err = run(
            Action::Key {
                uuid: "not-a-uuid".into(),
                key: "nonsense".into(),
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("Invalid session UUID"), "got {err}");
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
