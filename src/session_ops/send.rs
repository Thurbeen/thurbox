//! Typing text into a session's agent: the one implementation the interface's
//! `send` command and `thurbox-cli session send` both call (#1468), so the same
//! text to the same session is refused, typed or handed over the same way
//! whichever of them sends it.
//!
//! The route is decided once, here, by the session's [`Executor`]: a host that
//! takes delegation is asked to do the whole send with its own CLI, which
//! records it in the host's database (ADR-24); anything else is typed through
//! the route's backend under the local policy — the empty and parked refusals,
//! the composer guard, and the mailbox for a line someone is typing on.

use std::time::Duration;

use serde_json::Value;

use crate::session::SharedSession;
use crate::storage::messages::NewMessage;
use crate::storage::Database;

use super::delivery::{DeliveredVia, Handed, Sender};
use super::{Executor, OpError, SendOutcome};

/// How a send behaves.
#[derive(Debug, Clone, Default)]
pub struct SendPolicy {
    /// Press Enter after the text.
    pub submit: bool,
    /// Type even onto a composer that holds text.
    pub force: bool,
    /// Fail when the submission cannot be confirmed.
    pub strict: bool,
    /// Wait for the next working edge after the text is delivered.
    pub wait_turn: bool,
    /// How long that wait may take; unlimited when `None`.
    pub timeout: Option<Duration>,
    /// Who the text is from, should it end up in the mailbox.
    pub from: Sender,
    /// The task it is from, should it end up in the mailbox.
    pub from_task: Option<i64>,
}

impl SendPolicy {
    /// What the interface sends with: submitted, guarded, from thurbox itself.
    pub fn submit() -> Self {
        Self {
            submit: true,
            ..Self::default()
        }
    }
}

/// Where the text went.
#[derive(Debug, Clone)]
pub enum Delivery {
    /// Typed into the pane. `submitted` is `Some(false)` when `submit` was not
    /// asked for, and `None` when the input line could not confirm it.
    Typed {
        submit: bool,
        submitted: Option<bool>,
        enter_retried: bool,
    },
    /// The composer held someone's typing, so the text went as a message
    /// instead.
    Handed(Handed),
    /// The session's host did the send with its own CLI; its whole answer.
    Host(Value),
}

/// What [`send`] did.
pub struct SendReport {
    pub delivery: Delivery,
    /// Why the send counts as failed although it did something — the text is
    /// only in the mailbox, still in the composer, or its submission could not
    /// be confirmed under `strict`. The delivery above still says what was
    /// done, so a caller does not read the failure as "send again".
    pub failure: Option<OpError>,
    /// The turn the send started, when `wait_turn` asked for it and this
    /// machine did the send; `Err` when the wait ended any other way.
    pub turn: Option<Result<Value, super::wait::Error>>,
}

/// Send `text` to `session` under `policy`. `Err` is a send that did nothing:
/// refused, or a pane or host that could not be reached.
pub fn send(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    session: &SharedSession,
    text: &str,
    policy: &SendPolicy,
) -> Result<SendReport, OpError> {
    if text.trim().is_empty() {
        return Err(OpError::Other("text must not be empty".into()));
    }
    refuse_if_parked(db, session)?;
    if let Executor::HostCli { host, cli } = Executor::for_session(session)? {
        return on_host(&host, &cli, session, text, policy);
    }
    let seq = if policy.wait_turn {
        Some(
            db.latest_session_event_seq()
                .map_err(|e| OpError::Other(e.to_string()))?,
        )
    } else {
        None
    };
    let outcome =
        super::send_text_confirmed(db, backends, session, text, policy.submit, policy.force)
            .map_err(|e| e.context("send"))?;
    let delivery = match outcome {
        SendOutcome::ComposerBusy => Delivery::Handed(hand_over(db, session, text, policy)?),
        SendOutcome::Typed {
            submitted,
            enter_retried,
        } => Delivery::Typed {
            submit: policy.submit,
            submitted,
            enter_retried,
        },
    };
    let failure = failure(&delivery, session, policy.strict);
    // Text left in the mailbox starts its turn once the agent takes it, so
    // that is waited for too.
    let turn = match (failure.is_none(), seq) {
        (true, Some(seq)) => Some(super::wait::run(
            db,
            backends,
            session,
            super::wait::Conditions {
                states: vec!["working".into()],
                pane: None,
                timeout: policy.timeout,
            },
            Some(seq),
        )),
        _ => None,
    };
    Ok(SendReport {
        delivery,
        failure,
        turn,
    })
}

/// Hand an agent a prompt the way the interface's `send`, a task dispatch and
/// a send automation all do: submitted, guarded, and failed unless the agent
/// was shown it — a prompt left only in the mailbox counts as not delivered.
pub fn prompt(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    session: &SharedSession,
    text: &str,
    from_task: Option<i64>,
) -> Result<(), OpError> {
    let policy = SendPolicy {
        from_task,
        ..SendPolicy::submit()
    };
    send(db, backends, session, text, &policy)?
        .failure
        .map_or(Ok(()), Err)
}

/// Refuse a send to a **parked** session, naming the fix. `session stop`
/// killed the window on purpose; without this the multiplexer's "can't find
/// window" would describe a crash rather than the state the caller asked for.
pub fn refuse_if_parked(db: &Database, session: &SharedSession) -> Result<(), OpError> {
    let parked = db
        .session_stopped_at(session.id)
        .map_err(|e| OpError::Other(format!("session_stopped_at: {e}")))?
        .is_some();
    if !parked {
        return Ok(());
    }
    Err(OpError::Other(format!(
        "session '{}' is stopped: it has no pane. `thurbox-cli session start {}` puts one back",
        session.name, session.name
    )))
}

/// The whole send, done by the host's own CLI with the same policy. A failure
/// it answered with — a turn wait that timed out after the text went — keeps
/// its answer, which says what the host did.
fn on_host(
    host: &crate::session::HostDef,
    cli: &super::host_cli::CliInfo,
    session: &SharedSession,
    text: &str,
    policy: &SendPolicy,
) -> Result<SendReport, OpError> {
    let id = session.id.to_string();
    let timeout = policy.timeout.map(|t| t.as_secs().to_string());
    let mut args = vec!["session", "send"];
    for (flag, on) in [
        ("--no-enter", !policy.submit),
        ("--force", policy.force),
        ("--strict", policy.strict),
        ("--wait-turn", policy.wait_turn),
    ] {
        if on {
            args.push(flag);
        }
    }
    if let Some(timeout) = &timeout {
        args.extend(["--timeout", timeout]);
    }
    // `--` so text that starts with a dash reaches the host as text.
    args.extend(["--", id.as_str(), text]);
    let (answer, failure) = match super::host_cli::run_classified(host, cli, &args) {
        Ok(answer) => (answer, None),
        Err(failure) => match failure.answer {
            Some(answer) => (answer, Some(failure.error)),
            None => return Err(failure.error),
        },
    };
    Ok(SendReport {
        delivery: Delivery::Host(answer),
        failure,
        turn: None,
    })
}

/// Leave `text` for `session` as a `session-send` message — its agent's own
/// inbox where it has one, the mailbox otherwise.
fn hand_over(
    db: &Database,
    session: &SharedSession,
    text: &str,
    policy: &SendPolicy,
) -> Result<Handed, OpError> {
    let new = NewMessage {
        to_session_id: session.id,
        from_session_id: policy.from.session_id(),
        from_task_id: policy.from_task,
        kind: "session-send".into(),
        body: text.to_string(),
    };
    super::delivery::enqueue_and_deliver(db, session, new, policy.from, false, None)
}

fn failure(delivery: &Delivery, session: &SharedSession, strict: bool) -> Option<OpError> {
    let message = match delivery {
        // Nothing will show the agent a message that is only in the mailbox.
        Delivery::Handed(handed) if handed.outcome.via == DeliveredVia::Mailbox => format!(
            "'{}' has text in its composer and its agent inbox was not reached ({}); the text \
             is only in its mailbox (message #{})",
            session.name,
            handed.outcome.note.as_deref().unwrap_or("no reason given"),
            handed.id,
        ),
        Delivery::Typed {
            submitted: Some(false),
            submit: true,
            ..
        } => format!(
            "'{}' did not submit the text: it is still in its composer after a second Enter",
            session.name
        ),
        Delivery::Typed {
            submitted: None,
            submit: true,
            ..
        } if strict => "submission could not be confirmed".to_string(),
        _ => return None,
    };
    Some(OpError::Other(message))
}
