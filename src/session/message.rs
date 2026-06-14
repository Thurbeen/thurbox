//! Inter-session messages — a small, general mailbox so one session can hand a
//! structured payload to another without scraping its rendered terminal.
//!
//! A worker (e.g. a `flow` worker) enqueues a message addressed **to** another
//! session (the monitor): a free-form `kind` tag (`"questions"`, `"plan"`,
//! `"result"`, …), a `body`, and optional provenance (`from_session_id`,
//! `from_task_id`). The recipient drains its inbox — chronologically, exactly
//! once — instead of grepping a vt100 pane. The queue is agent-neutral and
//! reusable by any extension; `flow` is just its first consumer.
//!
//! This module is pure data (no local crate imports beyond `super`), matching
//! the architecture rule for `session`. Persistence + delivery semantics live in
//! [`storage::messages`](crate::storage::messages).

use super::SessionId;

/// Maximum byte length of a message `kind` tag. Kinds are short discriminants
/// (`"questions"`, `"plan"`, …), not free text; the cap keeps the column small
/// and rejects accidental misuse (a whole body passed as the kind).
pub const MAX_KIND_LEN: usize = 32;

/// Maximum byte length of a message `body`. Generous for a clarifying-questions
/// block or a written plan, but bounded so a runaway sender can't store
/// megabytes per message. Larger payloads belong in a file the body points at.
pub const MAX_BODY_LEN: usize = 64 * 1024;

/// A persisted inter-session message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMessage {
    pub id: i64,
    /// Recipient session — whose inbox this lands in.
    pub to_session_id: SessionId,
    /// Sender session, when known. `None` for messages enqueued without a
    /// source (e.g. a manual CLI `message send` with no `--from`).
    pub from_session_id: Option<SessionId>,
    /// Originating task id, when the sender is acting on a thurbox task.
    pub from_task_id: Option<i64>,
    /// Free-form short discriminant chosen by the sender (validated for length,
    /// not against a fixed set — any consumer can define its own kinds).
    pub kind: String,
    pub body: String,
    pub created_at: u64,
    /// When the message was claimed/marked read. `None` = unread.
    pub read_at: Option<u64>,
}

impl SessionMessage {
    /// Whether the message is still unread (never claimed).
    pub fn is_unread(&self) -> bool {
        self.read_at.is_none()
    }
}

/// Validate a message `kind`/`body` against the length bounds. Pure so it can be
/// unit-tested and shared by the CLI and storage layers. Returns a human-readable
/// reason on rejection.
pub fn validate_kind_body(kind: &str, body: &str) -> Result<(), String> {
    if kind.trim().is_empty() {
        return Err("kind must not be empty".into());
    }
    if kind.len() > MAX_KIND_LEN {
        return Err(format!(
            "kind too long ({} bytes; max {MAX_KIND_LEN})",
            kind.len()
        ));
    }
    if body.is_empty() {
        return Err("body must not be empty".into());
    }
    if body.len() > MAX_BODY_LEN {
        return Err(format!(
            "body too long ({} bytes; max {MAX_BODY_LEN})",
            body.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_reflects_read_at() {
        let mut m = SessionMessage {
            id: 1,
            to_session_id: SessionId::default(),
            from_session_id: None,
            from_task_id: None,
            kind: "questions".into(),
            body: "?".into(),
            created_at: 0,
            read_at: None,
        };
        assert!(m.is_unread());
        m.read_at = Some(123);
        assert!(!m.is_unread());
    }

    #[test]
    fn validate_rejects_empty_and_oversize() {
        assert!(validate_kind_body("questions", "hi").is_ok());
        assert!(validate_kind_body("", "hi").is_err());
        assert!(validate_kind_body("   ", "hi").is_err());
        assert!(validate_kind_body("questions", "").is_err());
        assert!(validate_kind_body(&"k".repeat(MAX_KIND_LEN + 1), "hi").is_err());
        assert!(validate_kind_body("k", &"b".repeat(MAX_BODY_LEN + 1)).is_err());
        // Exactly at the bound is allowed.
        assert!(validate_kind_body(&"k".repeat(MAX_KIND_LEN), &"b".repeat(MAX_BODY_LEN)).is_ok());
    }
}
