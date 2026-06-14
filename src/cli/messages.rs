//! Inter-session message queue subcommands (`thurbox-cli message …`).
//!
//! A general, agent-neutral mailbox: one session hands another a structured
//! payload (clarifying questions, a plan, a result, …) instead of the recipient
//! scraping its rendered terminal. See [`crate::storage::messages`].

use clap::Subcommand;
use serde_json::{json, Value};

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
        /// Originating task id, when sending on behalf of a thurbox task.
        #[arg(long)]
        task: Option<i64>,
        /// Sender session (UUID or name), for provenance.
        #[arg(long)]
        from: Option<String>,
        /// Don't type a wake nudge into the recipient's pane (enqueue silently).
        #[arg(long = "no-wake")]
        no_wake: bool,
    },
    /// Read a session's inbox. Peeks unread by default; `--claim` drains them.
    Inbox {
        /// Recipient session (UUID or name).
        #[arg(long = "for")]
        for_session: String,
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
        } => {
            let recipient = resolve_uuid_or_name(db, &to)?;
            let from_session_id = match from {
                Some(ref f) => Some(resolve_uuid_or_name(db, f)?.id),
                None => None,
            };
            let new = NewMessage {
                to_session_id: recipient.id,
                from_session_id,
                from_task_id: task,
                kind,
                body,
            };
            let id = db
                .enqueue_message(&new)
                .map_err(|e| format!("enqueue_message: {e}"))?;

            let mut woke = false;
            if !no_wake {
                // Best-effort nudge: a missing/dead window must not fail the send
                // (the message is already durably queued for the next drain).
                match crate::agent::tmux::send_prompt_now(&recipient.name, WAKE_TOKEN) {
                    Ok(()) => woke = true,
                    Err(e) => tracing::debug!(
                        "message send: wake nudge to {} failed: {e}",
                        recipient.name
                    ),
                }
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
        Action::Inbox {
            for_session,
            claim,
            all,
            limit,
        } => {
            let recipient = resolve_uuid_or_name(db, &for_session)?;
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
        Action::Prune {
            older_than_days,
            read_only,
        } => {
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
    }
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

/// First line of a body, truncated for table display.
fn first_line(body: &str) -> String {
    let line = body.lines().next().unwrap_or("");
    if line.chars().count() > 60 {
        let truncated: String = line.chars().take(57).collect();
        format!("{truncated}…")
    } else {
        line.to_string()
    }
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

        // Peek (unread) without consuming.
        let peek = run(
            Action::Inbox {
                for_session: "flow".into(),
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
                for_session: "flow".into(),
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
                for_session: "flow".into(),
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
                for_session: "flow".into(),
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
                for_session: "flow".into(),
                claim: true,
                all: false,
                limit: None,
            },
            &db,
        )
        .unwrap();
        send("second");

        // Default peek shows only the unread one...
        let unread = run(
            Action::Inbox {
                for_session: "flow".into(),
                claim: false,
                all: false,
                limit: None,
            },
            &db,
        )
        .unwrap();
        assert_eq!(unread.as_array().unwrap().len(), 1);
        assert_eq!(unread[0]["body"], "second");

        // ...--all shows both (read + unread).
        let all = run(
            Action::Inbox {
                for_session: "flow".into(),
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
}
