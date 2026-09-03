//! `thurbox-cli watch` — session state as a stream, so nothing has to poll.
//!
//! Every consumer of thurbox outside its own process has had one way to learn
//! that something changed: ask again, on a timer. That is a poll loop in every
//! integration, each with its own interval, each wrong in one direction or the
//! other — too slow to react, or spending a query a second to react to nothing.
//!
//! The stream is the `session_events` log ([`crate::storage::session_events`]),
//! not a sampled diff of the session table. That distinction is the whole
//! command: a diff every 250 ms collapses any two transitions inside one
//! sample, so `working → blocked → working` around an auto-answered permission
//! arrived as *nothing at all* and the driver never learned the permission had
//! been asked. Each writer appends its own event in its own transaction, so the
//! stream is what happened rather than what a sample could still see.
//!
//! The wake-up is still the gate the interface's own sync worker uses: SQLite's
//! `PRAGMA data_version` changes when *another* connection commits, so a reader
//! sits on a cheap pragma and reads the log's tail only when there is a tail to
//! read (see [`crate::sync`]).
//!
//! Deliberately not the kernel's event bus. That would need an IPC channel out
//! of a running interface, and would then only work while one is running —
//! whereas everything a driver needs to be woken by is already in the database,
//! written by whoever made the change, interface or not. A richer transport can
//! replace what is behind this command later without the command itself
//! changing.

use std::collections::HashMap;
use std::io::{ErrorKind, Write};
use std::time::{Duration, Instant};

use clap::Args;
use serde_json::{json, Value};

use crate::cli::output::Format;
use crate::cli::CommandError;
use crate::session::{Assessment, SessionId};
use crate::storage::{Database, SessionEventRow, SessionFacts};

/// How often the change gate is checked. A `PRAGMA data_version` on an open
/// connection is a memory read, not a query — this is a latency choice, not a
/// cost one.
const POLL: Duration = Duration::from_millis(250);

/// How many events one wake-up reads. A backlog larger than this is drained
/// over consecutive reads rather than in one allocation.
const BATCH: usize = 512;

#[derive(Args, Debug)]
pub struct WatchArgs {
    /// Only report this session (name, UUID, or unique id prefix).
    #[arg(long)]
    pub session: Option<String>,
    /// Stop after this many seconds. Runs until interrupted when omitted.
    #[arg(long)]
    pub for_secs: Option<u64>,
    /// Report the current state once, before waiting for changes.
    ///
    /// What a driver starting up wants: the baseline and every change after it
    /// in one stream, with no separate `session list` first.
    #[arg(long)]
    pub initial: bool,
    /// Resume after this sequence number instead of from now.
    ///
    /// Every event carries a `seq`. A driver that persists the last one it
    /// handled restarts with `--since <seq>` and receives exactly what it
    /// missed — the gap a stream otherwise has across a restart.
    #[arg(long, value_name = "SEQ")]
    pub since: Option<i64>,
    /// Also check each event's session against its pane, filling
    /// `hook_state_contradicted`.
    ///
    /// Off by default for the same reason `session list --verify` is: it costs
    /// a multiplexer query and a `ps` per event.
    #[arg(long)]
    pub verify: bool,
}

/// Stream session changes until the deadline (or forever) — one event per line,
/// flushed as it is written so a reader blocked on the pipe wakes on it.
pub fn run(db: &Database, args: WatchArgs, format: Format) -> Result<(), CommandError> {
    let filter = args
        .session
        .as_deref()
        .map(|reference| super::session_ref::resolve(db, reference).map(|s| s.id))
        .transpose()?;

    let deadline = args
        .for_secs
        .map(|secs| Instant::now() + Duration::from_secs(secs));
    let registry = crate::agent::agent_config::load_or_seed();
    let mut out = Stream::new(format);

    // The head is taken *before* the baseline rows, so a session created
    // between the two is announced twice rather than not at all: a driver can
    // reconcile a repeated `created`, and cannot invent one it never saw.
    let mut seq = args.since.unwrap_or_else(|| {
        db.latest_session_event_seq()
            .map_err(|e| tracing::warn!("could not read the event log head: {e}"))
            .unwrap_or_default()
    });
    // Seeds the per-session park state that `drain` advances from here: an
    // event's own `stopped`/`started` reason updates it, everything else reads
    // it as of the last event *for that session*, never a later batch's
    // snapshot (see `drain`'s doc comment).
    let seed_facts = db.load_session_facts().unwrap_or_default();
    let mut stopped_state: HashMap<SessionId, bool> = seed_facts
        .iter()
        .map(|(id, facts)| (*id, facts.stopped))
        .collect();
    if args.initial {
        let facts = &seed_facts;
        let states = db.load_hook_states().unwrap_or_default();
        for session in db.list_active_sessions().unwrap_or_default() {
            if filter.is_some_and(|only| only != session.id) {
                continue;
            }
            let Some(facts) = facts.get(&session.id) else {
                continue;
            };
            let row = states.get(&session.id);
            let hook = assess(
                &registry,
                facts,
                row.and_then(|r| r.state.as_deref()),
                row.and_then(|r| r.state_at),
                facts.stopped,
                args.verify,
                &session.backend_type,
            );
            let line = present(seq, session.id, facts, &hook);
            if !out.write(&line) {
                return Ok(());
            }
        }
    }
    // Drain once before entering the gate, for two reasons that are the same
    // reason: `--since` is a resume, so what was missed is already waiting; and
    // a commit that landed while the baseline was being read has *already*
    // moved `data_version`, so the gate below would sit on it until somebody
    // else committed again.
    let mut version = db.data_version().unwrap_or_default();
    if !drain(
        db,
        &registry,
        filter,
        &mut seq,
        &args,
        &mut stopped_state,
        &mut out,
    ) {
        return Ok(());
    }

    loop {
        let Some(nap) = remaining(deadline) else {
            return Ok(());
        };
        std::thread::sleep(nap);

        // The gate: unchanged means no other connection has committed, so
        // nothing can have happened and the log need not be read at all.
        let current = db.data_version().unwrap_or(version);
        if current == version {
            continue;
        }
        version = current;

        if !drain(
            db,
            &registry,
            filter,
            &mut seq,
            &args,
            &mut stopped_state,
            &mut out,
        ) {
            return Ok(());
        }
    }
}

/// How long to sleep before the next gate check: a whole [`POLL`] normally, the
/// time left when the deadline is nearer, and `None` once it has passed.
///
/// Sleeping past the deadline is what made `--for-secs 1` take a second and a
/// quarter; the caller's bound is a bound.
fn remaining(deadline: Option<Instant>) -> Option<Duration> {
    match deadline {
        None => Some(POLL),
        Some(end) => end
            .checked_duration_since(Instant::now())
            .map(|left| left.min(POLL)),
    }
}

/// Emit every event after `seq`, advancing it. `false` means the reader is gone
/// and the stream is over.
///
/// `stopped_state` carries each session's park state forward event by event,
/// rather than reading it once per batch: the naming facts (`facts`, below)
/// are the row *as of after the whole batch*, so two events for the same
/// session sharing a batch — a `state` transition followed by a park, say —
/// would otherwise both read the park state that only the second one earned.
fn drain(
    db: &Database,
    registry: &crate::session::AgentRegistry,
    filter: Option<SessionId>,
    seq: &mut i64,
    args: &WatchArgs,
    stopped_state: &mut HashMap<SessionId, bool>,
    out: &mut Stream,
) -> bool {
    loop {
        let events = match db.session_events_since(*seq, filter, BATCH) {
            Ok(events) => events,
            Err(e) => {
                tracing::warn!("could not read the event log: {e}");
                return true;
            }
        };
        if events.is_empty() {
            return true;
        }
        // One read of the naming facts per batch: an event names a session, and
        // the row it names is the same row for every event in the batch. Only
        // `stopped` needs point-in-time tracking; the rest doesn't change
        // mid-batch the way a park does.
        let facts = db.load_session_facts().unwrap_or_default();
        for event in &events {
            *seq = event.seq;
            let stopped = match event.reason.as_str() {
                "stopped" => true,
                "started" => false,
                _ => stopped_state
                    .get(&event.session_id)
                    .copied()
                    .unwrap_or(false),
            };
            stopped_state.insert(event.session_id, stopped);
            if !out.write(&line(db, registry, &facts, event, stopped, args.verify)) {
                return false;
            }
        }
        if events.len() < BATCH {
            return true;
        }
    }
}

/// One event, as the fields a driver decides on.
///
/// The state is assessed from the event's **own** `to_state` rather than the
/// row's current one: two transitions inside one wake-up are two events, and
/// re-reading the row would report the later one twice. `stopped` is likewise
/// the mark as of *this* event ([`drain`]'s `stopped_state`), not the row's
/// current one.
fn line(
    db: &Database,
    registry: &crate::session::AgentRegistry,
    facts: &HashMap<SessionId, SessionFacts>,
    event: &SessionEventRow,
    stopped: bool,
    verify: bool,
) -> Value {
    let unknown = SessionFacts {
        name: String::new(),
        agent: String::new(),
        backend_id: String::new(),
        stopped: false,
    };
    let facts = facts.get(&event.session_id).unwrap_or(&unknown);
    // Only the pane probe needs the backend, and only a live row has a pane to
    // probe — so the lookup is paid for exactly where it is used.
    let probe = verify && event.event != "gone";
    let backend_type = probe
        .then(|| db.get_session_by_id(event.session_id).ok().flatten())
        .flatten()
        .map(|s| s.backend_type)
        .unwrap_or_default();
    let hook = assess(
        registry,
        facts,
        event.to_state.as_deref(),
        Some(event.at_ms),
        stopped,
        probe,
        &backend_type,
    );
    let mut line = base(event.session_id, facts, &hook);
    line["seq"] = json!(event.seq);
    line["event"] = json!(event.event);
    line["reason"] = json!(event.reason);
    line["from_state"] = json!(event.from_state);
    line["to_state"] = json!(event.to_state);
    line["stopped"] = json!(stopped);
    line["at"] = json!(event.at_ms);
    line
}

/// The baseline `--initial` emits: the same shape as an event, with no
/// transition to report.
fn present(seq: i64, id: SessionId, facts: &SessionFacts, hook: &Assessment) -> Value {
    let mut line = base(id, facts, hook);
    line["seq"] = json!(seq);
    line["event"] = json!("present");
    line["reason"] = Value::Null;
    line["from_state"] = Value::Null;
    line["to_state"] = json!(hook.hook_state);
    line["stopped"] = json!(facts.stopped);
    line["at"] = json!(crate::sync::current_time_millis());
    line
}

/// The fields every line carries whatever produced it.
///
/// `state` is [`Assessment::state_word`] — the same one word `session get` and
/// `session list` answer with, so a driver never has to reconcile two
/// vocabularies — and the gating fields beside it are what say how much that
/// word is worth. They are on the line rather than a `session get` away
/// precisely because a driver reacting to a `blocked` needs to know whether
/// this agent's `blocked` is a text match on a notification body before it
/// acts on one.
fn base(id: SessionId, facts: &SessionFacts, hook: &Assessment) -> Value {
    json!({
        "session": id.to_string(),
        "name": facts.name,
        "agent": facts.agent,
        "backend_id": facts.backend_id,
        "state": hook.state_word(),
        "hook_state": hook.hook_state,
        "state_source": hook.state_source.map(|source| source.as_str()),
        "hook_coverage": hook.coverage.as_str(),
        "hook_blocked_is_heuristic": hook.blocked_is_heuristic(),
        "hook_state_contradicted": hook.contradicted,
    })
}

/// What the stored columns — and, with `--verify`, the pane — say about a
/// session at the moment an event describes.
///
/// Reuses [`Assessment`] rather than re-deriving coverage here, so the words
/// this stream uses are the words every other surface uses.
fn assess(
    registry: &crate::session::AgentRegistry,
    facts: &SessionFacts,
    state: Option<&str>,
    state_at: Option<i64>,
    stopped: bool,
    verify: bool,
    backend_type: &str,
) -> Assessment {
    let hook = Assessment::from_hooks(
        registry,
        &facts.agent,
        state,
        state_at,
        crate::sync::current_time_millis() as i64,
    );
    if stopped {
        return hook.parked();
    }
    if !verify {
        return hook;
    }
    if crate::session::is_remote_backend(backend_type) {
        return hook.pane_unavailable();
    }
    // The agent *binary*, not the agent name: `antigravity` runs `agy`, and the
    // pane's foreground process is spelled the way it was invoked.
    let command = registry
        .get(&facts.agent)
        .map(|d| d.command.clone())
        .unwrap_or_else(|| facts.agent.clone());
    let known: Vec<String> = registry.agents.iter().map(|a| a.command.clone()).collect();
    let pane = crate::agent::tmux::pane_state(&facts.name, &facts.backend_id);
    hook.with_pane(
        &command,
        &known,
        pane.foreground_process.as_deref(),
        pane.foreground_command.as_deref(),
        pane.dead,
    )
}

/// The columns the non-JSON renderings show, in the order they answer "what
/// happened, to what, and how sure are we".
const COLUMNS: &[&str] = &[
    "seq",
    "event",
    "reason",
    "name",
    "state",
    "from_state",
    "to_state",
    "session",
];

/// stdout as a line-framed stream, in the format the caller asked for.
///
/// A stream's unit is the line, which is what every format here is bent to:
/// `--pretty` would otherwise put one document across many lines and leave a
/// `read -r` loop parsing a brace. TOON declares its field list once and then
/// writes rows, which is exactly the shape of a stream — with the length the
/// header would carry left off, because a stream has none.
struct Stream {
    format: Format,
    header: bool,
}

impl Stream {
    fn new(format: Format) -> Self {
        Self {
            format,
            header: false,
        }
    }

    /// Write one event. `false` means the reader closed the pipe — the stream
    /// is over and nothing further should be written to it.
    fn write(&mut self, event: &Value) -> bool {
        let mut lines = Vec::new();
        match self.format {
            Format::Human => lines.push(human(event)),
            Format::Toon => {
                if !self.header {
                    self.header = true;
                    lines.push(format!("events{{{}}}:", COLUMNS.join(",")));
                }
                lines.push(format!("  {}", row(event)));
            }
            // `--pretty` frames a document, and this is a stream of them.
            Format::Json | Format::JsonPretty => {
                lines.push(serde_json::to_string(event).unwrap_or_default());
            }
        }
        let mut out = std::io::stdout().lock();
        for line in lines {
            match writeln!(out, "{line}").and_then(|()| out.flush()) {
                Ok(()) => {}
                // The reader is gone. Exiting now is the difference between a
                // `watch | head -1` that ends and one that sits out the whole
                // of its `--for-secs`.
                Err(e) if e.kind() == ErrorKind::BrokenPipe => return false,
                Err(e) => {
                    tracing::warn!("watch stream write failed: {e}");
                    return false;
                }
            }
        }
        true
    }
}

/// One TOON row: the [`COLUMNS`] cells of this event, comma-delimited and
/// quoted per §7.2 by the same encoder every other TOON surface uses — a
/// session name is free-form and can otherwise carry the delimiter itself.
fn row(event: &Value) -> String {
    COLUMNS
        .iter()
        .map(|column| crate::cli::toon::scalar(event.get(*column).unwrap_or(&Value::Null), ','))
        .collect::<Vec<_>>()
        .join(",")
}

/// One human line: what happened, to which session, and the transition.
fn human(event: &Value) -> String {
    let field = |key: &str| {
        event
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let seq = event.get("seq").and_then(Value::as_i64).unwrap_or_default();
    let reason = match field("reason").as_str() {
        "" => String::new(),
        reason => format!(" ({reason})"),
    };
    let transition = match (field("from_state").as_str(), field("to_state").as_str()) {
        ("", "") => String::new(),
        (from, to) => format!(
            "  {} → {}",
            if from.is_empty() { "-" } else { from },
            if to.is_empty() { "-" } else { to }
        ),
    };
    format!(
        "{seq:>5}  {:<8}{reason}  {}  [{}]{transition}",
        field("event"),
        field("name"),
        field("state"),
    )
}
