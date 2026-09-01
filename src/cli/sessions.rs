//! Session CRUD and orchestration subcommands.

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::output::{self, CommandOutput};
use crate::session::SessionId;
use crate::storage::Database;
use crate::sync::SharedSession;

// `Create` carries every spawn option and is far larger than `Stop`/`Key`/…,
// which is what this lint measures. Boxing it is what the lint wants and what
// clap cannot take (a `Subcommand` variant's fields are the argument
// definitions), and the enum is constructed exactly once per process from
// argv — so the size difference costs one short-lived stack value, not a hot
// path. `cli/automations.rs` carries the same kind of targeted allow for the
// same reason.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
pub enum Action {
    /// List all active sessions.
    ///
    /// Each row carries the session's reported agent state plus how old that
    /// report is and what its agent is able to report at all (`hook_state`,
    /// `hook_state_age_secs`, `hook_coverage`, `hook_states_reportable`).
    /// `state` is the one word to read: an agent's own report, or `unreported`
    /// / `uncovered` when there is none.
    List {
        /// Only list children of this parent session UUID.
        #[arg(long)]
        parent: Option<String>,
        /// List the deleted sessions instead — what a peer thurbox mirroring
        /// this machine reads, with each row's `force_deleted` mark.
        #[arg(long)]
        deleted: bool,
        /// Also check each session's pane against its reported state.
        ///
        /// Off by default because it costs a multiplexer query and a `ps` **per
        /// session**; `session get` does it for one session without asking.
        #[arg(long)]
        verify: bool,
    },
    /// Get a session by name, UUID, or unique id prefix.
    ///
    /// `show` is an alias: the CLI reads one thing with `get` and lists with
    /// `list` everywhere, and the other spellings other nouns grew are kept so
    /// nothing that already worked stops working.
    #[command(alias = "show")]
    ///
    /// Reports the session's agent state with the age of that report, the
    /// coverage of its agent's hooks, and — for a local session — what the
    /// pane's foreground process says about it (`hook_corroboration`,
    /// `hook_state_contradicted`). Pass `--no-verify` to skip the pane probe.
    Get {
        /// Session UUID.
        uuid: String,
        /// Skip the pane check and report the stored state alone.
        #[arg(long)]
        no_verify: bool,
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
        /// Launch this executable instead of an agent from `agents.toml`.
        ///
        /// Makes the session *anything* — a shell, a REPL, a build watcher, a
        /// tool with flags thurbox has never heard of. The command is stored
        /// with the session and replayed on restart, since there is no registry
        /// entry to look up. It has no conversation, so `--resume` is refused
        /// for it; `--agent shell` is the ready-made version of this.
        #[arg(long, conflicts_with = "agent")]
        command: Option<String>,
        /// One argument for `--command` (repeatable, in order). Passed to the
        /// process as-is — no shell sees it, so quoting is not your problem.
        ///
        /// `allow_hyphen_values` because the usual reason to pass an argument
        /// is to pass a *switch*: `--command /bin/sh --arg -c --arg '<script>'`
        /// is how a driver hands over a command line it was itself given as a
        /// string. Without it clap reads `-c` as an unknown flag of thurbox's
        /// own and refuses the invocation.
        #[arg(long = "arg", requires = "command", allow_hyphen_values = true)]
        arg: Vec<String>,
        /// Extra environment as `KEY=VALUE` (repeatable). thurbox's own
        /// `THURBOX_*` identity vars always win over these.
        #[arg(long = "env")]
        env: Vec<String>,
        /// Resume an existing agent conversation instead of starting a new one.
        ///
        /// The id as the *agent* knows it, or `latest` for an agent that
        /// resolves "the last conversation in this directory" itself. This is
        /// how a session that began elsewhere arrives: its checkout comes in as
        /// `--repo-path`, its conversation as this.
        #[arg(long)]
        resume: Option<String>,
        /// What to do when a session of this name is already active.
        #[arg(long = "on-existing", value_enum, default_value_t = OnExisting::Allow)]
        on_existing: OnExisting,
    },
    /// Soft-delete a session. (`remove` is an alias.)
    #[command(alias = "remove")]
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
    ///
    /// On native Windows the local multiplexer is psmux, which is only known to
    /// implement `enter`, `escape`, `tab`, `backspace` and `ctrl-<letter>`. A
    /// multiplexer types a key name it does not recognise into the pane as
    /// literal text, so treat the other names as unverified there.
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
    /// Report whether a session's status hooks are wired and firing.
    ///
    /// Every shipped hook command ends in `|| true`, so a signal that never
    /// lands is invisible: it looks exactly like an agent that has not
    /// signalled yet. This inspects the wiring instead of the silence — the
    /// extension, this agent's coverage, its payload on disk, whether a hook
    /// command could find `thurbox-cli` at all, what was last reported and
    /// when, and whether the pane agrees. Exits non-zero when a session's
    /// wiring is broken; an agent thurbox ships no hooks for but which is
    /// signalling anyway warns rather than fails.
    Doctor {
        /// Session UUID; every active session when omitted.
        uuid: Option<String>,
    },
    /// Park a session: kill its pane, keep the row, the checkout and the
    /// conversation.
    ///
    /// The verb that was missing between "leave it running" and "delete it".
    /// A stopped session costs no process and no terminal, and `session start`
    /// puts it back where it was — nothing else reclaims its pane in the
    /// meantime, which is what separates this from a window that merely died.
    Stop {
        /// Session name, UUID, or unique id prefix.
        session: String,
    },
    /// Put a stopped session's pane back, resuming its conversation the way a
    /// restart does. A session that is already running is left alone.
    Start {
        /// Session name, UUID, or unique id prefix.
        session: String,
    },
    /// Fork a session: a new one beside it, continuing its conversation.
    ///
    /// The interface has had this since v1; this is the same operation without
    /// it. For an agent that declares `fork_args` the new session continues the
    /// parent's conversation; for one that does not, it is a second session in
    /// the same directory and branch, which the output says plainly.
    Fork {
        /// Session to fork — name, UUID, or unique id prefix.
        session: String,
        /// Name for the new session (default: `<parent>-fork`).
        #[arg(long)]
        name: Option<String>,
    },
    /// Run a command in a session's directory, on the machine it lives on.
    ///
    /// Not typed into the pane — a separate process in the session's context,
    /// with its output returned. What "check the state of that session's work"
    /// needs, without a driver having to reconstruct the cwd and the host
    /// itself.
    Exec {
        /// Session name, UUID, or unique id prefix.
        session: String,
        /// Exit with the command's own exit code instead of thurbox's.
        ///
        /// Off by default because thurbox's exit codes mean something specific
        /// (0 ok, 1 failed, 2 usage) and overloading them silently would break
        /// a caller that reads them; the command's code is always in the output
        /// either way. With the flag, a command exiting 2 is that command's 2,
        /// not a usage error — which is exactly the distinction Gas City's
        /// `proc.exec` capability is defined by. A command terminated by a
        /// signal has no code to carry and takes the generic failure code.
        #[arg(long = "exit-passthrough")]
        exit_passthrough: bool,
        /// The command and its arguments, after `--`.
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
    /// Read/write a session's metadata — the driver's own key/value space.
    Meta {
        #[command(subcommand)]
        action: MetaAction,
    },
    /// Report an agent lifecycle transition (called from an agent hook).
    ///
    /// Records the session's state so the TUI can render it (working/blocked/
    /// done/idle) — works headless; the TUI picks it up via its data_version
    /// poll. Identity defaults to the calling session ($THURBOX_SESSION,
    /// injected at spawn), so an agent hook passes no id.
    ///
    /// This is a **supported integration point**, not an internal of the hooks
    /// extension: $THURBOX_SESSION is set on the pane and inherited by every
    /// process in it, so a driver that launches its own agent there — and that
    /// agent's own hooks — can report state with no arguments at all. From
    /// outside the pane, pass `--session <uuid>`. `session doctor` says whether
    /// the reports are arriving.
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

/// What `session create` should do when the name is already taken.
///
/// One question with four answers rather than a pile of booleans, because they
/// are mutually exclusive by nature and `--help` should teach the whole
/// question at once.
///
/// The default is [`Allow`](Self::Allow) — thurbox does not enforce name
/// uniqueness, and cannot: a database mirroring a shareable host (ADR-24) holds
/// that host's rows beside its own, and two machines may legitimately each have
/// a session called `build`. Uniqueness is therefore something a caller *asks
/// for* per creation, not a property of the namespace.
///
/// Every answer is decided before anything is spawned, so a refusal leaves no
/// window, worktree or row behind. It is a check rather than a lock: two
/// simultaneous creates can still both pass it, which is inherent to a spawn
/// that must make a multiplexer window before it has a row to be unique in.
/// What it does remove is the caller's own list-then-create window, which is
/// far wider and which every integrator was otherwise writing themselves.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum OnExisting {
    /// Create another session with the same name (the default, and what thurbox
    /// has always done). The two are then addressable only by id, since a name
    /// matching several sessions is refused rather than guessed.
    Allow,
    /// Return the existing session instead of creating one, with
    /// `created: false`. Makes creation idempotent — what a driver reconciling
    /// desired state wants.
    Adopt,
    /// Tear the existing session down first (as `delete --force` would), then
    /// create. Its worktree goes with it.
    Replace,
    /// Refuse, naming the session in the way. Exit 1, nothing created.
    Fail,
}

/// `session meta` — per-session key/value, namespaced by convention.
#[derive(Subcommand, Debug)]
pub enum MetaAction {
    /// Set a key. The value is read from stdin when not given as an argument,
    /// so it can contain anything without quoting trouble.
    Set {
        /// Session name, UUID, or unique id prefix.
        session: String,
        /// Key, conventionally namespaced (`fm.task_id`, `gc.bead`).
        key: String,
        /// Value; read from stdin when omitted.
        value: Option<String>,
    },
    /// Print one key's value, or nothing when it is unset.
    Get {
        /// Session name, UUID, or unique id prefix.
        session: String,
        key: String,
    },
    /// List every key set on a session.
    List {
        /// Session name, UUID, or unique id prefix.
        session: String,
    },
    /// Remove one key.
    Unset {
        /// Session name, UUID, or unique id prefix.
        session: String,
        key: String,
    },
}

pub fn run(action: Action, db: &Database) -> Result<CommandOutput, String> {
    match action {
        Action::List {
            deleted: true,
            parent: _,
            verify: _,
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
            verify,
        } => {
            let parent_id = parent
                .as_deref()
                .map(|reference| resolve(db, reference).map(|s| s.id))
                .transpose()?;
            let sessions: Vec<SharedSession> = db
                .list_active_sessions()
                .map_err(|e| format!("list_active_sessions: {e}"))?
                .into_iter()
                .filter(|s| parent_id.is_none() || s.parent_session_id == parent_id)
                .collect();
            let states = db.load_hook_states().unwrap_or_default();
            let bases = db.load_base_branches().unwrap_or_default();
            let registry = crate::agent::agent_config::load_or_seed();
            let assessments: Vec<crate::session::Assessment> = sessions
                .iter()
                .map(|s| assess(&registry, s, &states, verify))
                .collect();
            let json = Value::Array(
                sessions
                    .iter()
                    .zip(&assessments)
                    .map(|(s, hook)| {
                        crate::session_ops::mirror::session_to_json_assessed(
                            s,
                            hook,
                            bases.get(&s.id).map(String::as_str),
                        )
                    })
                    .collect(),
            );
            Ok(
                CommandOutput::new(json, render_session_list(&sessions, &assessments))
                    // `state`, not `hook_state`: the raw latched word is null for a
                    // session that never reported and carries no age or coverage to
                    // judge it by, so an agent reading the default answer would get
                    // less than the human table on the same call. `--json` still
                    // carries `hook_state` verbatim for a consumer that reads it.
                    //
                    // The id is not decoration: every follow-up command resolves a
                    // session by UUID, so omitting it would only buy a second call.
                    .list("sessions", &["name", "agent", "state", "id"])
                    .empty(match parent_id {
                        Some(id) => format!("0 sessions with parent {id}"),
                        None => "0 active sessions on this machine".to_string(),
                    })
                    .help([
                        "thurbox-cli session get <id>   the full record, worktrees included",
                        "thurbox-cli session capture <id> --lines 50   what its pane is showing",
                        "thurbox-cli session list --json   every field, for a script",
                    ]),
            )
        }
        Action::Get { uuid, no_verify } => {
            let session = resolve(db, &uuid)?;
            let states = db.load_hook_states().unwrap_or_default();
            let bases = db.load_base_branches().unwrap_or_default();
            let registry = crate::agent::agent_config::load_or_seed();
            let hook = assess(&registry, &session, &states, !no_verify);
            Ok(CommandOutput::new(
                crate::session_ops::mirror::session_to_json_assessed(
                    &session,
                    &hook,
                    bases.get(&session.id).map(String::as_str),
                ),
                render_session_detail(&session, &hook),
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
            command,
            arg,
            env,
            resume,
            on_existing,
        } => {
            let parent_session_id = parent
                .as_deref()
                .map(|reference| resolve(db, reference).map(|s| s.id))
                .transpose()?;
            let extra_repos = super::parse_extra_repos(&add_repo, &add_dir);
            let env = parse_env(&env)?;
            // Names are not unique, so "already exists" is a decision the
            // caller makes rather than something thurbox assumes.
            if let Some(found) = resolve_existing(db, &name, on_existing)? {
                return Ok(found);
            }
            let req = crate::session_ops::SpawnRequest {
                name,
                repo_path,
                worktree_branch,
                base_branch,
                agent,
                host,
                parent_session_id,
                extra_repos,
                command,
                args: arg,
                env,
                resume_session_id: resume,
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
                    // The pane, the checkouts and the server: everything the
                    // caller would otherwise have to come back for with a
                    // second `session get`, and poll for until it appeared.
                    "backend_id": res.backend_id,
                    "worktrees": res.worktrees.iter().map(worktree_json).collect::<Vec<_>>(),
                    "tmux_socket": crate::agent::tmux::local_socket_name(),
                    "cwd": res.cwd.display().to_string(),
                    "parent_session_id": res.parent_session_id.map(|id| id.to_string()),
                    "hook_failures": res.hook_failures,
                    "sharing": res.sharing,
                    "created": true,
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
            let id = session.id.to_string();
            let mut args = vec!["session", "send", &id, &text];
            if no_enter {
                args.push("--no-enter");
            }
            if let Some(remote) = delegate_to_host(&session, &args)? {
                return Ok(remote);
            }
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
            let id = session.id.to_string();
            if let Some(remote) =
                delegate_to_host(&session, &["session", "key", &id, &resolved.name])?
            {
                return Ok(remote);
            }
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
        Action::Stop { session } => {
            let target = resolve(db, &session)?;
            let killed = crate::session_ops::restart::stop_session_headless(db, target.id)?;
            Ok(CommandOutput::new(
                json!({
                    "id": target.id.to_string(),
                    "name": target.name,
                    "stopped": true,
                    "killed_window": killed,
                }),
                format!(
                    "Stopped '{}' ({}). Its worktree and conversation are untouched.",
                    target.name, target.id
                ),
            )
            .help([
                "thurbox-cli session start <ref>   put its pane back",
                "thurbox-cli session delete <ref>   let it go for good",
            ]))
        }
        Action::Start { session } => {
            let target = resolve(db, &session)?;
            let report = crate::session_ops::restart::start_session_headless(db, target.id)?;
            let mut human = format!("Started '{}' ({})", target.name, target.id);
            push_hook_failures(&mut human, &report.hook_failures);
            Ok(CommandOutput::new(
                json!({
                    "id": target.id.to_string(),
                    "name": target.name,
                    "stopped": false,
                    "hook_failures": report.hook_failures,
                }),
                human,
            ))
        }
        Action::Fork { session, name } => {
            let source = resolve(db, &session)?;
            let res = crate::session_ops::fork_session_headless(
                db,
                source.id,
                name.as_deref().unwrap_or_default(),
            )?;
            // Whether the conversation actually came along is the agent's
            // answer, not thurbox's: an agent with no `fork_args` gets a fresh
            // one, and saying so beats letting the caller assume continuity.
            let registry = crate::agent::agent_config::load_or_seed();
            let continues = registry
                .get(&res.agent)
                .map(|def| !def.fork_args.is_empty())
                .unwrap_or(false);
            let human = format!(
                "Forked '{}' → '{}' ({})\n{}",
                source.name,
                res.name,
                res.session_id,
                if continues {
                    "  continuing its conversation"
                } else {
                    "  starting a fresh conversation (this agent declares no fork_args)"
                }
            );
            Ok(CommandOutput::new(
                json!({
                    "id": res.session_id.to_string(),
                    "name": res.name,
                    "agent": res.agent,
                    "parent_session_id": source.id.to_string(),
                    "backend_id": res.backend_id,
                    "worktrees": res.worktrees.iter().map(worktree_json).collect::<Vec<_>>(),
                    "cwd": res.cwd.display().to_string(),
                    "continues_conversation": continues,
                }),
                human,
            ))
        }
        Action::Exec {
            session,
            exit_passthrough,
            command,
        } => exec_in_session(db, &session, &command, exit_passthrough),
        Action::Meta { action } => run_meta(action, db),
        Action::Doctor { uuid } => super::session_doctor::run(db, uuid.as_deref()),
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
    let id = session.id.to_string();
    let line_count = lines.to_string();
    let mut args = vec!["session", "capture", &id, "--lines", &line_count];
    if ansi {
        args.push("--ansi");
    }
    if let Some(remote) = delegate_to_host(&session, &args)? {
        return Ok(remote);
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
///
/// The STATE column is the same word `--json` reports in `state`, so the two
/// renderings cannot disagree about what a session is doing.
fn render_session_list(sessions: &[SharedSession], hooks: &[crate::session::Assessment]) -> String {
    if sessions.is_empty() {
        return "No active sessions.".to_string();
    }
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .zip(hooks)
        .map(|(s, hook)| {
            // `dash` already maps an empty branch (no worktree) to "-".
            let branch = s.worktrees.first().map(|w| w.branch.as_str());
            vec![
                s.name.clone(),
                s.agent.clone(),
                render_state(hook),
                s.backend_type.clone(),
                output::dash(branch),
                output::dash(s.cwd.as_ref().map(|p| p.display().to_string()).as_deref()),
                s.id.to_string(),
            ]
        })
        .collect();
    output::table(
        &["NAME", "AGENT", "STATE", "BACKEND", "BRANCH", "CWD", "ID"],
        &rows,
    )
}

/// One session's state for a human: the word, how long it has stood, and a `!`
/// when the pane contradicts it.
///
/// An uninstrumented session reads `uncovered`, never `idle` — the point of the
/// whole assessment is that a consumer (human included) can tell "the agent
/// says it is at rest" from "this agent cannot say anything".
fn render_state(hook: &crate::session::Assessment) -> String {
    let mut out = hook.state_word().to_string();
    if hook.state.is_none() {
        return out;
    }
    if let Some(age) = hook.age_secs {
        out.push_str(&format!(" ({})", output::duration_short(age)));
    }
    if hook.contradicted == Some(true) {
        out.push_str(" !");
    }
    out
}

/// Render a single session as an aligned key/value block, with any worktrees
/// listed one per line beneath it.
fn render_session_detail(s: &SharedSession, hook: &crate::session::Assessment) -> String {
    let mut pairs: Vec<(&str, String)> = vec![
        ("name", s.name.clone()),
        ("id", s.id.to_string()),
        ("agent", s.agent.clone()),
        ("state", render_state(hook)),
        (
            "state_source",
            output::dash(hook.state_source.map(|src| src.as_str())),
        ),
        ("hook_coverage", coverage_line(hook)),
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
    if let Some(corroboration) = hook.corroboration {
        pairs.push((
            "pane",
            match hook.foreground_process.as_deref() {
                Some(process) => format!("{} ({process})", corroboration.as_str()),
                None => corroboration.as_str().to_string(),
            },
        ));
    }
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

/// What this session's agent can report, for a human: the verdict plus the
/// states behind it, so `partial` is never a bare word.
fn coverage_line(hook: &crate::session::Assessment) -> String {
    if hook.states_reportable().is_empty() {
        return "none (this agent reports no state)".to_string();
    }
    let mut line = format!(
        "{} ({})",
        hook.coverage.as_str(),
        hook.states_reportable().join(", ")
    );
    if hook.blocked_is_heuristic() {
        line.push_str("; blocked matched from notification text");
    }
    line
}

/// Run a command in a session's directory, on the machine the session lives on.
///
/// Deliberately **not** typed into the pane: the pane belongs to the agent, and
/// borrowing it would interleave with whatever it is doing and put the answer
/// in its scrollback rather than in this process's stdout. A separate process
/// in the same directory answers "what is the state of that session's work"
/// without disturbing the session at all.
///
/// Host-transparent: a session created with `--host` runs this over that host's
/// launcher rather than refusing, so `exec` means the same thing everywhere.
fn exec_in_session(
    db: &Database,
    reference: &str,
    command: &[String],
    exit_passthrough: bool,
) -> Result<CommandOutput, String> {
    let session = resolve(db, reference)?;
    let cwd = session
        .cwd
        .clone()
        .or_else(|| session.worktrees.first().map(|w| w.worktree_path.clone()))
        .ok_or_else(|| format!("session '{}' has no directory to run in", session.name))?;
    let (program, rest) = command
        .split_first()
        .ok_or("nothing to run — pass the command after `--`")?;

    let host = if crate::session::is_remote_backend(&session.backend_type) {
        Some(
            crate::session_ops::resolve_host(&session.backend_type)
                .flatten()
                .ok_or_else(|| {
                    format!(
                        "session '{}' runs on backend '{}', which is not in hosts.toml",
                        session.name, session.backend_type
                    )
                })?,
        )
    } else {
        None
    };
    let output = crate::session_ops::exec_in_dir(host.as_ref(), &cwd, program, rest)
        .map_err(|e| format!("could not run '{program}' in {}: {e}", cwd.display()))?;

    // `None` when the process was terminated without exiting — killed by a
    // signal. Reported as null rather than as a made-up number: no process
    // exits `-1`, so a sentinel there would be indistinguishable from a real
    // code to anything reading the field.
    let code = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let mut human = stdout.clone();
    if !stderr.is_empty() {
        human.push_str(&stderr);
    }
    if human.is_empty() {
        human = match code {
            Some(code) => format!("(no output; exit {code})"),
            None => "(no output; terminated by a signal)".to_string(),
        };
    }

    let payload = json!({
        "id": session.id.to_string(),
        "name": session.name,
        "cwd": cwd.display().to_string(),
        "exit_code": code,
        "stdout": stdout,
        "stderr": stderr,
    });
    // The command failing is not this command failing — unless asked. The exit
    // code is in the document either way, so a caller never has to choose
    // between reading the answer and knowing the result.
    //
    // When asked, the command's code becomes this process's, which is the whole
    // point of the flag: a caller reading only `$?` gets the real answer rather
    // than "something went wrong". A command killed by a signal has no code to
    // carry, so it takes the generic failure code instead of a fabricated one.
    Ok(match (exit_passthrough, code) {
        (true, Some(0)) | (false, _) => CommandOutput::new(payload, human),
        (true, Some(code)) => {
            CommandOutput::failed(payload, human, format!("command exited {code}"))
                .exiting_with(code)
        }
        (true, None) => CommandOutput::failed(
            payload,
            human,
            "command was terminated by a signal".to_string(),
        ),
    })
}

/// `session meta` — storage, and nothing more. Nothing here interprets a key.
fn run_meta(action: MetaAction, db: &Database) -> Result<CommandOutput, String> {
    match action {
        MetaAction::Set {
            session,
            key,
            value,
        } => {
            let target = resolve(db, &session)?;
            // Stdin when no argument: a value can be long, multi-line, or start
            // with a dash, none of which survive being an argv token reliably.
            let value = match value {
                Some(v) => v,
                None => {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                        .map_err(|e| format!("read value from stdin: {e}"))?;
                    buf.trim_end_matches('\n').to_string()
                }
            };
            db.set_session_meta(target.id, &key, &value)
                .map_err(|e| format!("set_session_meta: {e}"))?;
            Ok(CommandOutput::new(
                json!({ "id": target.id.to_string(), "key": key, "value": value }),
                format!("{key} set on '{}'", target.name),
            ))
        }
        MetaAction::Get { session, key } => {
            let target = resolve(db, &session)?;
            let value = db
                .get_session_meta(target.id, &key)
                .map_err(|e| format!("get_session_meta: {e}"))?;
            Ok(CommandOutput::new(
                json!({ "id": target.id.to_string(), "key": key, "value": value }),
                // Bare value on stdout: this is the one command whose output is
                // routinely captured into a shell variable.
                value.unwrap_or_default(),
            ))
        }
        MetaAction::List { session } => {
            let target = resolve(db, &session)?;
            let all = db
                .list_session_meta(target.id)
                .map_err(|e| format!("list_session_meta: {e}"))?;
            let human = if all.is_empty() {
                String::new()
            } else {
                output::table(
                    &["KEY", "VALUE"],
                    &all.iter()
                        .map(|(k, v)| vec![k.clone(), v.clone()])
                        .collect::<Vec<_>>(),
                )
            };
            Ok(
                CommandOutput::new(serde_json::to_value(&all).unwrap_or(Value::Null), human)
                    .empty(format!("no metadata on '{}'", target.name)),
            )
        }
        MetaAction::Unset { session, key } => {
            let target = resolve(db, &session)?;
            let removed = db
                .unset_session_meta(target.id, &key)
                .map_err(|e| format!("unset_session_meta: {e}"))?;
            Ok(CommandOutput::new(
                json!({ "id": target.id.to_string(), "key": key, "removed": removed }),
                if removed {
                    format!("{key} removed from '{}'", target.name)
                } else {
                    format!("{key} was not set on '{}'", target.name)
                },
            ))
        }
    }
}

/// Read repeatable `--env KEY=VALUE` tokens into a map.
///
/// Split on the **first** `=` so a value may contain more (`--env
/// FLAGS=-Dx=1`). An empty value is allowed and meaningful: it sets the
/// variable to the empty string rather than leaving it unset.
pub(crate) fn parse_env(
    tokens: &[String],
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut env = std::collections::BTreeMap::new();
    for token in tokens {
        let (key, value) = token
            .split_once('=')
            .ok_or_else(|| format!("--env expects KEY=VALUE, got '{token}'"))?;
        if key.is_empty() {
            return Err(format!("--env has an empty key: '{token}'"));
        }
        env.insert(key.to_string(), value.to_string());
    }
    Ok(env)
}

/// One worktree as the CLI renders it — the shape `session get` already
/// publishes, so a caller parses one form wherever it meets a worktree.
fn worktree_json(w: &crate::sync::SharedWorktree) -> Value {
    json!({
        "repo_path": w.repo_path.display().to_string(),
        "worktree_path": w.worktree_path.display().to_string(),
        "branch": w.branch,
    })
}

/// Apply the caller's [`OnExisting`] answer before anything is spawned.
///
/// `Ok(Some(output))` means the creation is already answered and must not
/// proceed; `Ok(None)` means carry on and spawn. A refusal is an `Err`, which
/// the entrypoint renders as a structured document on stdout and exits 1 —
/// what Gas City's `RPP-LIFECYCLE-002` requires of a duplicate start, and what
/// firstmate was hand-rolling a `session list` pre-check to achieve.
///
/// Ambiguity blocks `adopt` and `replace` but not `allow`: adopting one of two
/// same-named sessions, or destroying one of them, is a guess about which was
/// meant. It is the same rule [`super::session_ref`] follows, and it has to
/// hold here because a database that mirrors a shareable host can legitimately
/// already contain two.
fn resolve_existing(
    db: &Database,
    name: &str,
    mode: OnExisting,
) -> Result<Option<CommandOutput>, String> {
    if mode == OnExisting::Allow {
        return Ok(None);
    }
    let found = db
        .find_sessions_by_name(name)
        .map_err(|e| format!("find_sessions_by_name: {e}"))?;
    match (mode, found.len()) {
        (OnExisting::Allow, _) | (_, 0) => Ok(None),
        (OnExisting::Fail, _) => Err(format!(
            "a session named '{name}' is already active ({}). Use \
             `--on-existing adopt` to take it as it is, `--on-existing replace` \
             to tear it down first, or pick another name",
            found
                .iter()
                .map(|s| s.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        (OnExisting::Adopt, 1) => Ok(Some(existing_session_output(&found[0]))),
        (OnExisting::Replace, 1) => {
            crate::session_ops::delete::delete_session_headless(db, found[0].id, true)?;
            Ok(None)
        }
        (OnExisting::Adopt | OnExisting::Replace, n) => Err(format!(
            "'{name}' matches {n} active sessions, so there is no single one to \
             {}. Address them by id, or pick another name:\n{}",
            if mode == OnExisting::Adopt {
                "adopt"
            } else {
                "replace"
            },
            found
                .iter()
                .map(|s| format!("  {}  {}", s.id, s.agent))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

/// What `create --on-existing adopt` returns when the session was already there.
///
/// The same document shape a real creation produces, with `created: false` as
/// the only difference — so a caller reads one shape and needs no branch for
/// "did I make this or find it".
fn existing_session_output(session: &SharedSession) -> CommandOutput {
    CommandOutput::new(
        json!({
            "id": session.id.to_string(),
            "name": session.name,
            "agent": session.agent,
            "agent_session_id": session.agent_session_id,
            "backend_id": session.backend_id,
            "worktrees": session.worktrees.iter().map(worktree_json).collect::<Vec<_>>(),
            "tmux_socket": crate::agent::tmux::local_socket_name(),
            "cwd": session.cwd.as_ref().map(|p| p.display().to_string()),
            "parent_session_id": session.parent_session_id.map(|id| id.to_string()),
            "hook_failures": Vec::<String>::new(),
            "sharing": Value::Null,
            "created": false,
        }),
        format!(
            "Session '{}' already exists ({}) — left as it is.",
            session.name, session.id
        ),
    )
    .help(["thurbox-cli session get <id>   what it is doing now"])
}

/// Resolve the session a command was pointed at — a name, a UUID or an id
/// prefix, all equally. See [`super::session_ref`] for why one resolver.
pub(crate) fn resolve(db: &Database, reference: &str) -> Result<SharedSession, String> {
    super::session_ref::resolve(db, reference)
}

/// Run a pane command on the machine the session actually lives on.
///
/// `agent::tmux`'s one-shot helpers talk to the *local* multiplexer, so a
/// session created with `--host` has no pane here. That used to be a refusal,
/// which made `--host` produce a shape no other verb accepted: creatable, and
/// then undrivable. thurbox already knows how to run its own CLI on a host —
/// the mirror pass does it on every tick — so a pane verb is delegated there
/// instead, and means the same thing on every machine.
///
/// `Ok(None)` means "this is local, carry on". `Ok(Some(output))` is the host's
/// own answer, already a document. The refusal survives only where delegation
/// is genuinely impossible: a host with no `hosts.toml` entry, or one whose
/// `thurbox-cli` could not be found or provisioned.
fn delegate_to_host(
    session: &SharedSession,
    args: &[&str],
) -> Result<Option<CommandOutput>, String> {
    if !crate::session::is_remote_backend(&session.backend_type) {
        return Ok(None);
    }
    let host = crate::session_ops::resolve_host(&session.backend_type)
        .flatten()
        .ok_or_else(|| {
            format!(
                "session '{}' runs on backend '{}', which is not in hosts.toml — \
                 cannot reach the machine it lives on",
                session.name, session.backend_type
            )
        })?;
    let cli = crate::session_ops::host_cli::delegated(&host).ok_or_else(|| {
        format!(
            "session '{}' runs on '{}', and no thurbox-cli could be reached there — \
             run this command on that host",
            session.name, host.name
        )
    })?;
    let answer = crate::session_ops::host_cli::run(&host, &cli, args)?;
    let human = answer
        .get("output")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| answer.to_string());
    Ok(Some(CommandOutput::new(answer, human)))
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

/// Everything this machine can say about a session's agent state.
///
/// `hook_state` itself is the **raw persisted value** written by `session
/// signal` and the headless remote-status poll — reported verbatim, because a
/// consumer that has always read that word must keep reading exactly it. What
/// is added around it is the honesty the bare word lacks: how old the report
/// is, what this agent's hooks are able to report at all, and (when `probe`)
/// what actually holds the pane.
///
/// The TUI's *display* status is derived differently again — it folds in
/// terminal quiescence and attach failures, which need a live pane and a render
/// loop and so cannot exist here.
///
/// `probe` costs one multiplexer query plus one `ps`, which is why `session
/// get` does it for one session and `session list` only on `--verify`. A
/// **remote** session is never probed: its pane lives on its own host's
/// multiplexer, so the answer is `unavailable` rather than a guess.
pub(crate) fn assess(
    registry: &crate::session::AgentRegistry,
    s: &SharedSession,
    states: &std::collections::HashMap<crate::session::SessionId, crate::storage::HookRow>,
    probe: bool,
) -> crate::session::Assessment {
    let row = states.get(&s.id);
    let hook = crate::session::Assessment::from_hooks(
        registry,
        &s.agent,
        row.and_then(|r| r.state.as_deref()),
        row.and_then(|r| r.state_at),
        crate::sync::current_time_millis() as i64,
    );
    if !probe {
        return hook;
    }
    if crate::session::is_remote_backend(&s.backend_type) {
        return hook.pane_unavailable();
    }
    // The agent *binary*, not the agent name: `antigravity` runs `agy`, and the
    // pane's foreground process is spelled the way it was invoked.
    let command = registry
        .get(&s.agent)
        .map(|d| d.command.clone())
        .unwrap_or_else(|| s.agent.clone());
    let known: Vec<String> = registry.agents.iter().map(|a| a.command.clone()).collect();
    let pane = crate::agent::tmux::pane_state(&s.name, &s.backend_id);
    hook.with_pane(
        &command,
        &known,
        pane.foreground_process.as_deref(),
        pane.foreground_command.as_deref(),
        pane.dead,
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
                verify: false,
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
    fn meta_round_trips_and_says_when_a_key_was_not_there() {
        // Storage, and nothing more: whatever a driver puts here comes back
        // byte for byte, and thurbox never interprets a key or a value.
        let db = db();
        let session = make_test_session("worker");
        db.upsert_session(&session).unwrap();

        run(
            Action::Meta {
                action: MetaAction::Set {
                    session: "worker".into(),
                    key: "fm.task_id".into(),
                    value: Some("T-1043".into()),
                },
            },
            &db,
        )
        .unwrap();

        let got = run(
            Action::Meta {
                action: MetaAction::Get {
                    session: session.id.to_string(),
                    key: "fm.task_id".into(),
                },
            },
            &db,
        )
        .unwrap();
        // Set by name, read back by id: one session, either spelling.
        assert_eq!(got["value"].as_str(), Some("T-1043"));

        let listed = run(
            Action::Meta {
                action: MetaAction::List {
                    session: "worker".into(),
                },
            },
            &db,
        )
        .unwrap();
        assert_eq!(listed["fm.task_id"].as_str(), Some("T-1043"));

        let removed = run(
            Action::Meta {
                action: MetaAction::Unset {
                    session: "worker".into(),
                    key: "fm.task_id".into(),
                },
            },
            &db,
        )
        .unwrap();
        assert_eq!(removed["removed"].as_bool(), Some(true));
        // Removing what is not there is reported, not pretended.
        let again = run(
            Action::Meta {
                action: MetaAction::Unset {
                    session: "worker".into(),
                    key: "fm.task_id".into(),
                },
            },
            &db,
        )
        .unwrap();
        assert_eq!(again["removed"].as_bool(), Some(false));
    }

    #[test]
    fn exec_runs_in_the_sessions_directory_and_reports_the_commands_own_code() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db();
        let mut session = make_test_session("worker");
        session.cwd = Some(dir.path().to_path_buf());
        db.upsert_session(&session).unwrap();

        // `pwd`/`sh` are POSIX-only, and Git for Windows' `pwd.exe` prints an
        // MSYS-style path (`/c/Users/...`) that Windows' own canonicalize()
        // can't parse — so the print-cwd and nonzero-exit commands are spelled
        // per platform through the native shell instead of a fixed program.
        let print_cwd = if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), "cd".into()]
        } else {
            vec!["pwd".into()]
        };
        let exit_3 = if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), "exit 3".into()]
        } else {
            vec!["sh".into(), "-c".into(), "exit 3".into()]
        };

        let out = run(
            Action::Exec {
                session: "worker".into(),
                exit_passthrough: false,
                command: print_cwd,
            },
            &db,
        )
        .unwrap();
        assert_eq!(out["exit_code"].as_i64(), Some(0));
        let printed = out["stdout"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        // Resolved through the same canonicalization the temp dir went through,
        // so a symlinked /tmp does not make this a path-string comparison.
        assert_eq!(
            std::fs::canonicalize(printed).ok(),
            std::fs::canonicalize(dir.path()).ok(),
            "the command ran in the session's own directory"
        );

        // A failing command is reported, not raised: the caller asked to run
        // something, and it ran. The exit code is in the document either way.
        let failed = run(
            Action::Exec {
                session: "worker".into(),
                exit_passthrough: false,
                command: exit_3.clone(),
            },
            &db,
        )
        .unwrap();
        assert_eq!(failed["exit_code"].as_i64(), Some(3));

        // Unless asked, in which case it also becomes this command's outcome.
        let passthrough = run(
            Action::Exec {
                session: "worker".into(),
                exit_passthrough: true,
                command: exit_3,
            },
            &db,
        )
        .unwrap();
        assert_eq!(passthrough["exit_code"].as_i64(), Some(3));
        assert!(
            passthrough.failure.is_some(),
            "--exit-passthrough makes the command's failure the invocation's"
        );
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
        let hook = crate::session::Assessment::default();
        let rendered = render_session_list(std::slice::from_ref(&s), std::slice::from_ref(&hook));
        assert!(rendered.contains("NAME"));
        assert!(rendered.contains("demo"));
        assert!(rendered.contains("local-tmux"));
        // No worktree → branch column shows a dash.
        assert!(rendered.contains('-'));
        // A session whose agent has no hook wiring reads `uncovered`, never
        // `idle`: the table must not pass off silence as a report.
        assert!(rendered.contains("STATE"));
        assert!(rendered.contains("uncovered"), "got {rendered}");
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
                verify: false,
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
                verify: false,
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
                verify: false,
            },
            &db,
        )
        .unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"].as_str(), Some("worker"));

        // `--parent` names a session, so it resolves like any other reference
        // — and one that matches nothing is an error rather than an empty list
        // that looks like "this parent has no children".
        let err = run(
            Action::List {
                parent: Some("no-such-parent".into()),
                deleted: false,
                verify: false,
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("Session not found"), "got {err}");
    }

    #[test]
    fn get_reports_a_reference_that_matches_nothing() {
        // A reference is a name, a UUID or an id prefix, so an unknown one is
        // "nothing matches" rather than a complaint about its spelling — and it
        // says which spellings were tried.
        let db = db();
        let err = run(
            Action::Get {
                uuid: "not-a-uuid".into(),
                no_verify: true,
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("Session not found"), "got {err}");
        assert!(err.contains("not-a-uuid"), "got {err}");
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
        // status code against a window that was never going to exist. With no
        // hosts.toml entry there is nowhere to delegate to, which is the one
        // case that is still a refusal rather than a round trip.
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
            // The obstacle is the missing host entry, not the verb: with one,
            // the same call is delegated to that host's own `thurbox-cli`.
            assert!(err.contains("hosts.toml"), "got {err}");
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
        // Which session is still the first thing wrong with the call. The
        // reference is no longer required to be a UUID — it may be a name or an
        // id prefix — so the error is "nothing matches", and it says what it
        // tried rather than complaining about the spelling.
        let db = db();
        let err = run(
            Action::Key {
                uuid: "not-a-uuid".into(),
                key: "nonsense".into(),
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("Session not found"), "got {err}");
        assert!(err.contains("not-a-uuid"), "got {err}");
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
