//! Inter-session message queue subcommands (`thurbox-cli message …`).
//!
//! A general, agent-neutral mailbox: one session hands another a structured
//! payload (clarifying questions, a plan, a result, …) instead of the recipient
//! scraping its rendered terminal. See [`crate::storage::messages`].

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::identity::calling_session;
use crate::cli::output::{self, CommandOutput};
use crate::session::{SessionId, SessionMessage};
use crate::storage::messages::{NewMessage, DEFAULT_RETENTION_DAYS};
use crate::storage::Database;
use crate::sync::{current_time_millis, SharedSession};

/// Wake token typed into a recipient's pane after `send --wake`, nudging the
/// monitor agent to drain its inbox (`thurbox-cli message inbox …`) right away
/// rather than waiting for its next scheduled tick. Idempotent: the agent just
/// re-reads unread messages.
const WAKE_TOKEN: &str = "inbox";

const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Enqueue a message addressed to a session, optionally waking it.
    Send {
        /// Recipient session (UUID or name).
        #[arg(long)]
        to: String,
        /// Free-form short kind tag (e.g. "questions", "plan", "result").
        #[arg(long)]
        kind: String,
        /// Message body.
        #[arg(long)]
        body: String,
        /// Originating task id. Defaults to the caller's `THURBOX_TASK` when run
        /// inside a task-spawned session; pass explicitly to override.
        #[arg(long)]
        task: Option<i64>,
        /// Sender session (UUID or name), for provenance. Defaults to the caller
        /// (`THURBOX_SESSION`) when run inside a session; pass to override.
        #[arg(long)]
        from: Option<String>,
        /// Don't type a wake nudge into the recipient's pane (enqueue silently).
        #[arg(long = "no-wake")]
        no_wake: bool,
    },
    /// Reply to a message: enqueue back to its original sender and wake them.
    /// The replier only needs the message id (no peer UUID/name handling).
    Reply {
        /// The message id being answered (from an `inbox` read).
        message_id: i64,
        /// Reply body.
        #[arg(long)]
        body: String,
        /// Reply kind tag (defaults to "reply").
        #[arg(long, default_value = "reply")]
        kind: String,
        /// Sender session (UUID or name); defaults to the caller (`THURBOX_SESSION`).
        #[arg(long)]
        from: Option<String>,
        /// Don't type a wake nudge into the recipient's pane.
        #[arg(long = "no-wake")]
        no_wake: bool,
    },
    /// Read a session's inbox. Peeks unread by default; `--claim` drains them.
    Inbox {
        /// Recipient session (UUID or name). Defaults to the calling session
        /// (`THURBOX_SESSION`) so an agent reads its own mail with no id.
        #[arg(long = "for")]
        for_session: Option<String>,
        /// Atomically mark the returned messages read (exactly-once drain).
        #[arg(long)]
        claim: bool,
        /// Include already-read messages too (peek only; ignored with --claim).
        #[arg(long)]
        all: bool,
        /// Max messages to return.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Delete old messages (retention sweep).
    Prune {
        /// Delete messages older than this many days (default 14).
        #[arg(long = "older-than-days")]
        older_than_days: Option<u64>,
        /// Only delete already-read messages (unread stay regardless of age).
        #[arg(long = "read-only")]
        read_only: bool,
    },
}

pub fn run(action: Action, db: &Database) -> Result<CommandOutput, String> {
    match action {
        Action::Send {
            to,
            kind,
            body,
            task,
            from,
            no_wake,
        } => send_message(db, to, kind, body, task, from, no_wake),
        Action::Reply {
            message_id,
            body,
            kind,
            from,
            no_wake,
        } => reply_message(db, message_id, body, kind, from, no_wake),
        Action::Inbox {
            for_session,
            claim,
            all,
            limit,
        } => read_inbox(db, for_session, claim, all, limit),
        Action::Prune {
            older_than_days,
            read_only,
        } => prune_messages(db, older_than_days, read_only),
    }
}

/// Handle `message send`: enqueue a message addressed to a session.
fn send_message(
    db: &Database,
    to: String,
    kind: String,
    body: String,
    task: Option<i64>,
    from: Option<String>,
    no_wake: bool,
) -> Result<CommandOutput, String> {
    let recipient = resolve_uuid_or_name(db, &to)?;
    // Provenance + task tag default to the calling session's injected
    // identity (`THURBOX_SESSION` / `THURBOX_TASK`), so an agent never has
    // to know or pass its own ids. Explicit flags override.
    let from_session_id = resolve_from(db, from.as_deref())?;
    let from_task_id = task.or_else(calling_task_id);
    let new = NewMessage {
        to_session_id: recipient.id,
        from_session_id,
        from_task_id,
        kind,
        body,
    };
    enqueue_and_wake(db, &recipient, new, no_wake)
}

/// Handle `message reply`: enqueue back to the original message's sender.
fn reply_message(
    db: &Database,
    message_id: i64,
    body: String,
    kind: String,
    from: Option<String>,
    no_wake: bool,
) -> Result<CommandOutput, String> {
    let original = db
        .get_message(message_id)
        .map_err(|e| format!("get_message: {e}"))?
        .ok_or_else(|| format!("Message not found: #{message_id}"))?;
    let sender_id = original
        .from_session_id
        .ok_or_else(|| format!("Message #{message_id} has no known sender — cannot reply"))?;
    let recipient = db
        .get_session_by_id(sender_id)
        .map_err(|e| format!("get_session_by_id: {e}"))?
        .ok_or_else(|| format!("Sender of message #{message_id} ({sender_id}) no longer exists"))?;
    let from_session_id = resolve_from(db, from.as_deref())?;
    // Carry the originating task tag through so the reply stays threaded.
    let new = NewMessage {
        to_session_id: recipient.id,
        from_session_id,
        from_task_id: original.from_task_id,
        kind,
        body,
    };
    enqueue_and_wake(db, &recipient, new, no_wake)
}

/// Resolve the `--from` provenance: an explicit reference, else the calling
/// session's injected id (`THURBOX_SESSION`).
fn resolve_from(db: &Database, from: Option<&str>) -> Result<Option<SessionId>, String> {
    match from {
        Some(f) => Ok(Some(resolve_uuid_or_name(db, f)?.id)),
        None => Ok(calling_session_id(db)),
    }
}

/// Handle `message inbox`: peek or claim a session's messages.
fn read_inbox(
    db: &Database,
    for_session: Option<String>,
    claim: bool,
    all: bool,
    limit: Option<usize>,
) -> Result<CommandOutput, String> {
    let recipient = match for_session {
        Some(ref r) => resolve_uuid_or_name(db, r)?,
        None => calling_session(db).ok_or_else(|| {
            "no --for given and THURBOX_SESSION is unset (not running inside a session)".to_string()
        })?,
    };
    let messages = if claim {
        db.claim_messages(recipient.id, limit)
            .map_err(|e| format!("claim_messages: {e}"))?
    } else {
        db.list_messages(recipient.id, !all, limit)
            .map_err(|e| format!("list_messages: {e}"))?
    };
    let json = Value::Array(messages.iter().map(message_to_json).collect());
    Ok(CommandOutput::new(
        json,
        render_inbox(&recipient.name, &messages, claim),
    ))
}

/// Handle `message prune`: retention sweep of old messages.
fn prune_messages(
    db: &Database,
    older_than_days: Option<u64>,
    read_only: bool,
) -> Result<CommandOutput, String> {
    let days = older_than_days.unwrap_or(DEFAULT_RETENTION_DAYS);
    let cutoff = current_time_millis().saturating_sub(days * MS_PER_DAY);
    let pruned = db
        .prune_messages(cutoff, read_only)
        .map_err(|e| format!("prune_messages: {e}"))?;
    let human = format!(
        "Pruned {pruned} {}message(s) older than {days} day(s).",
        if read_only { "read " } else { "" }
    );
    Ok(CommandOutput::new(
        json!({ "pruned": pruned, "older_than_days": days, "read_only": read_only }),
        human,
    ))
}

/// Render an inbox read as a table of messages (or a friendly empty line).
fn render_inbox(recipient: &str, messages: &[SessionMessage], claimed: bool) -> String {
    if messages.is_empty() {
        return format!(
            "No {}messages for '{recipient}'.",
            if claimed { "unread " } else { "" }
        );
    }
    let rows: Vec<Vec<String>> = messages
        .iter()
        .map(|m| {
            vec![
                m.id.to_string(),
                m.kind.clone(),
                output::dash(m.from_task_id.map(|t| t.to_string()).as_deref()),
                first_line(&m.body),
            ]
        })
        .collect();
    let verb = if claimed { "Claimed" } else { "Inbox for" };
    let header = format!("{verb} '{recipient}' — {} message(s):", messages.len());
    format!(
        "{header}\n{}",
        output::table(&["ID", "KIND", "TASK", "BODY"], &rows)
    )
}

/// Max display width of a message-body preview in the inbox table.
const BODY_PREVIEW_MAX: usize = 60;

/// First line of a body, truncated to [`BODY_PREVIEW_MAX`] (ellipsis included)
/// for table display.
fn first_line(body: &str) -> String {
    let line = body.lines().next().unwrap_or("");
    if line.chars().count() > BODY_PREVIEW_MAX {
        let truncated: String = line.chars().take(BODY_PREVIEW_MAX - 1).collect();
        format!("{truncated}…")
    } else {
        line.to_string()
    }
}

/// Enqueue a message + best-effort wake nudge, then build the command output.
/// Shared by `send` and `reply`. `new.to_session_id` must be `recipient.id`
/// (the recipient is also needed by name for the wake nudge).
fn enqueue_and_wake(
    db: &Database,
    recipient: &SharedSession,
    new: NewMessage,
    no_wake: bool,
) -> Result<CommandOutput, String> {
    let id = db
        .enqueue_message(&new)
        .map_err(|e| format!("enqueue_message: {e}"))?;

    let mut woke = false;
    if !no_wake {
        // Best-effort nudge: a missing/dead window must not fail the send (the
        // message is already durably queued for the next drain).
        match crate::agent::tmux::send_prompt_now(
            &recipient.name,
            &recipient.backend_id,
            WAKE_TOKEN,
        ) {
            Ok(()) => woke = true,
            Err(e) => {
                tracing::debug!("message: wake nudge to {} failed: {e}", recipient.name)
            }
        }
        // Keep the headless janitor ticking so a missed wake is still drained in
        // bounded time even when the TUI never started (durability is already
        // guaranteed by the queue; this guarantees timeliness too). Tied to the
        // wake path so silent (`--no-wake`) enqueues stay tmux-free.
        crate::cli::automations::arm_heartbeat();
    }

    let human = format!(
        "Enqueued message #{id} to '{}'{}.",
        recipient.name,
        if woke { " (woke it)" } else { "" }
    );
    Ok(CommandOutput::new(
        json!({
            "enqueued": true,
            "message_id": id,
            "to_session_id": recipient.id.to_string(),
            "to_session_name": recipient.name,
            "woke": woke,
        }),
        human,
    ))
}

/// The calling session's id from `THURBOX_SESSION` (used for provenance).
fn calling_session_id(db: &Database) -> Option<SessionId> {
    calling_session(db).map(|s| s.id)
}

/// The calling session's originating task id from the injected `THURBOX_TASK`.
fn calling_task_id() -> Option<i64> {
    std::env::var("THURBOX_TASK").ok()?.parse().ok()
}

/// Resolve a session reference that may be either a UUID or a session name.
fn resolve_uuid_or_name(db: &Database, reference: &str) -> Result<SharedSession, String> {
    if let Ok(id) = reference.parse::<SessionId>() {
        if let Some(session) = db
            .get_session_by_id(id)
            .map_err(|e| format!("get_session_by_id: {e}"))?
        {
            return Ok(session);
        }
    }
    db.get_session_by_name(reference)
        .map_err(|e| format!("get_session_by_name: {e}"))?
        .ok_or_else(|| format!("Session not found: {reference}"))
}

fn message_to_json(m: &SessionMessage) -> Value {
    json!({
        "id": m.id,
        "to_session_id": m.to_session_id.to_string(),
        "from_session_id": m.from_session_id.map(|id| id.to_string()),
        "from_task_id": m.from_task_id,
        "kind": m.kind,
        "body": m.body,
        "created_at": m.created_at,
        "read_at": m.read_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn add_session(db: &Database, name: &str) -> SessionId {
        let id = SessionId::default();
        let shared = SharedSession {
            id,
            name: name.into(),
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
        id
    }

    #[test]
    fn send_by_name_then_inbox_peek_and_claim() {
        let db = db();
        add_session(&db, "flow");
        // Send without waking (no tmux in tests).
        let sent = run(
            Action::Send {
                to: "flow".into(),
                kind: "questions".into(),
                body: "q1?".into(),
                task: Some(5),
                from: None,
                no_wake: true,
            },
            &db,
        )
        .unwrap();
        assert_eq!(sent["enqueued"], true);
        assert_eq!(sent["woke"], false);

        let peek = run(
            Action::Inbox {
                for_session: Some("flow".into()),
                claim: false,
                all: false,
                limit: None,
            },
            &db,
        )
        .unwrap();
        assert_eq!(peek.as_array().unwrap().len(), 1);
        assert_eq!(peek[0]["kind"], "questions");
        assert_eq!(peek[0]["from_task_id"], 5);

        // Claim drains exactly once.
        let claimed = run(
            Action::Inbox {
                for_session: Some("flow".into()),
                claim: true,
                all: false,
                limit: None,
            },
            &db,
        )
        .unwrap();
        assert_eq!(claimed.as_array().unwrap().len(), 1);
        let empty = run(
            Action::Inbox {
                for_session: Some("flow".into()),
                claim: true,
                all: false,
                limit: None,
            },
            &db,
        )
        .unwrap();
        assert_eq!(empty.as_array().unwrap().len(), 0);
    }

    #[test]
    fn send_resolves_from_sender_by_name() {
        let db = db();
        add_session(&db, "flow");
        let worker = add_session(&db, "worker");
        run(
            Action::Send {
                to: "flow".into(),
                kind: "result".into(),
                body: "done".into(),
                task: None,
                from: Some("worker".into()),
                no_wake: true,
            },
            &db,
        )
        .unwrap();
        let peek = run(
            Action::Inbox {
                for_session: Some("flow".into()),
                claim: false,
                all: false,
                limit: None,
            },
            &db,
        )
        .unwrap();
        assert_eq!(
            peek[0]["from_session_id"].as_str(),
            Some(worker.to_string().as_str())
        );
    }

    #[test]
    fn reply_routes_back_to_original_sender() {
        let db = db();
        add_session(&db, "flow");
        let worker = add_session(&db, "worker");
        // Worker → flow (provenance recorded via explicit --from, as the env is
        // unset in tests).
        let sent = run(
            Action::Send {
                to: "flow".into(),
                kind: "questions".into(),
                body: "Q1?".into(),
                task: Some(7),
                from: Some("worker".into()),
                no_wake: true,
            },
            &db,
        )
        .unwrap();
        let msg_id = sent["message_id"].as_i64().unwrap();

        // Flow replies by message id — no peer id handling.
        run(
            Action::Reply {
                message_id: msg_id,
                body: "use the new API".into(),
                kind: "reply".into(),
                from: Some("flow".into()),
                no_wake: true,
            },
            &db,
        )
        .unwrap();

        // The reply lands in the worker's inbox, threaded on the original task.
        let worker_inbox = run(
            Action::Inbox {
                for_session: Some("worker".into()),
                claim: false,
                all: false,
                limit: None,
            },
            &db,
        )
        .unwrap();
        assert_eq!(worker_inbox.as_array().unwrap().len(), 1);
        assert_eq!(worker_inbox[0]["body"], "use the new API");
        assert_eq!(worker_inbox[0]["from_task_id"], 7);
        assert_eq!(
            worker_inbox[0]["to_session_id"].as_str(),
            Some(worker.to_string().as_str())
        );
    }

    #[test]
    fn reply_without_known_sender_errors() {
        let db = db();
        add_session(&db, "flow");
        // A message with no provenance (from_session_id = None).
        let sent = run(
            Action::Send {
                to: "flow".into(),
                kind: "note".into(),
                body: "anon".into(),
                task: None,
                from: None,
                no_wake: true,
            },
            &db,
        )
        .unwrap();
        let msg_id = sent["message_id"].as_i64().unwrap();
        let err = run(
            Action::Reply {
                message_id: msg_id,
                body: "hi".into(),
                kind: "reply".into(),
                from: None,
                no_wake: true,
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("no known sender"), "got {err}");
    }

    #[test]
    fn reply_to_missing_message_errors() {
        let db = db();
        let err = run(
            Action::Reply {
                message_id: 999,
                body: "hi".into(),
                kind: "reply".into(),
                from: None,
                no_wake: true,
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("Message not found"), "got {err}");
    }

    #[test]
    fn inbox_without_for_and_no_env_errors() {
        let db = db();
        // No --for and (in tests) THURBOX_SESSION unset → a clear error.
        let err = run(
            Action::Inbox {
                for_session: None,
                claim: false,
                all: false,
                limit: None,
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("THURBOX_SESSION"), "got {err}");
    }

    #[test]
    fn inbox_all_includes_read_messages() {
        let db = db();
        add_session(&db, "flow");
        let send = |body: &str| {
            run(
                Action::Send {
                    to: "flow".into(),
                    kind: "note".into(),
                    body: body.into(),
                    task: None,
                    from: None,
                    no_wake: true,
                },
                &db,
            )
            .unwrap();
        };
        send("first");
        // Claim it (marks read), then send a second unread one.
        run(
            Action::Inbox {
                for_session: Some("flow".into()),
                claim: true,
                all: false,
                limit: None,
            },
            &db,
        )
        .unwrap();
        send("second");

        let unread = run(
            Action::Inbox {
                for_session: Some("flow".into()),
                claim: false,
                all: false,
                limit: None,
            },
            &db,
        )
        .unwrap();
        assert_eq!(unread.as_array().unwrap().len(), 1);
        assert_eq!(unread[0]["body"], "second");

        let all = run(
            Action::Inbox {
                for_session: Some("flow".into()),
                claim: false,
                all: true,
                limit: None,
            },
            &db,
        )
        .unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);
    }

    #[test]
    fn send_rejects_empty_body() {
        let db = db();
        add_session(&db, "flow");
        let err = run(
            Action::Send {
                to: "flow".into(),
                kind: "note".into(),
                body: String::new(),
                task: None,
                from: None,
                no_wake: true,
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("body"), "got {err}");
    }

    #[test]
    fn send_unknown_recipient_errors() {
        let db = db();
        let err = run(
            Action::Send {
                to: "ghost".into(),
                kind: "note".into(),
                body: "hi".into(),
                task: None,
                from: None,
                no_wake: true,
            },
            &db,
        )
        .unwrap_err();
        assert!(err.contains("Session not found"), "got {err}");
    }

    #[test]
    fn prune_reports_count() {
        let db = db();
        let v = run(
            Action::Prune {
                older_than_days: Some(7),
                read_only: true,
            },
            &db,
        )
        .unwrap();
        assert_eq!(v["pruned"], 0);
        assert_eq!(v["older_than_days"], 7);
        assert_eq!(v["read_only"], true);
    }

    #[test]
    fn first_line_keeps_short_and_first_line_only() {
        assert_eq!(first_line("hello"), "hello");
        assert_eq!(first_line("line one\nline two"), "line one");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn first_line_truncates_to_preview_max_with_ellipsis() {
        let long = "x".repeat(BODY_PREVIEW_MAX + 10);
        let rendered = first_line(&long);
        // Result is exactly BODY_PREVIEW_MAX wide: (MAX-1) chars + the ellipsis.
        assert_eq!(rendered.chars().count(), BODY_PREVIEW_MAX);
        assert!(rendered.ends_with('…'));
    }

    #[test]
    fn render_inbox_reports_empty_and_claimed_wording() {
        assert_eq!(render_inbox("flow", &[], false), "No messages for 'flow'.");
        assert_eq!(
            render_inbox("flow", &[], true),
            "No unread messages for 'flow'."
        );
    }
}
