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

use std::collections::{HashMap, HashSet};
use std::io::{ErrorKind, Write};
use std::time::{Duration, Instant};

use clap::Args;
use serde_json::Value;

use crate::cli::output::Format;
use crate::cli::CommandError;
use crate::session::{Assessment, SessionId};
use crate::storage::{Database, HookRow, SessionEventRow, SessionFacts};

/// How often the change gate is checked. A `PRAGMA data_version` on an open
/// connection is a memory read, not a query — this is a latency choice, not a
/// cost one.
const POLL: Duration = Duration::from_millis(250);

/// How often the local backends' windows are checked for a session whose pane
/// died under it ([`crate::session_ops::lost`]). A loss takes two looks to
/// confirm, so this is half the time a driver waits to hear of one.
const SWEEP: Duration = Duration::from_secs(3);

/// How many events one wake-up reads. A backlog larger than this is drained
/// over consecutive reads rather than in one allocation.
const BATCH: usize = crate::session_ops::events::BATCH;

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
    /// `hook_state_contradicted` and `detected_agent`.
    ///
    /// Off by default for the same reason `session list --verify` is: it costs
    /// a multiplexer query and a `ps` per event.
    #[arg(long)]
    pub verify: bool,
}

/// Stream session changes until the deadline (or forever) — one event per line,
/// flushed as it is written so a reader blocked on the pipe wakes on it.
pub fn run(
    db: &Database,
    backends: &super::Backends<'_>,
    args: WatchArgs,
    format: Format,
) -> Result<(), CommandError> {
    // The lost-pane sweep and the read fields only ever look at local rows, so
    // they take the cheap registry; only `--verify` reads a pane through the
    // full one.
    let local = backends.local();
    let backends = args.verify.then(|| backends.get());
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
    // Taken before the baseline rows for the reason the head is: a host that
    // flips between the two is reported twice rather than not at all.
    let mut hosts = HostVerdicts::default();
    hosts.look(db, filter);
    let mut stopped_state: HashMap<SessionId, bool> = seed_facts
        .iter()
        .map(|(id, facts)| (*id, facts.stopped))
        .collect();
    if args.initial {
        let facts = &seed_facts;
        let states = db.load_hook_states().unwrap_or_default();
        let mut read_fields = crate::session_ops::read_fields::ReadFields::default();
        for session in db.list_active_sessions().unwrap_or_default() {
            if filter.is_some_and(|only| only != session.id) {
                continue;
            }
            let Some(facts) = facts.get(&session.id) else {
                continue;
            };
            let row = states.get(&session.id).cloned().unwrap_or_default();
            let hook = assess(
                &registry,
                facts,
                &row,
                facts.stopped,
                backends.map(|b| (b, &session)),
            );
            let mut line = crate::session_ops::events::present(seq, session.id, facts, &hook);
            read_fields.annotate(db, Some(local), &registry, &session, &mut line);
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
    let mut lost = crate::session_ops::lost::LostSweep::default();
    lost.sweep(db, local);
    crate::session_ops::launch_failure::sweep(db, local);
    let mut swept = Instant::now();
    if !drain(
        db,
        &registry,
        filter,
        &mut seq,
        backends,
        &mut stopped_state,
        &mut out,
        local,
    ) {
        return Ok(());
    }

    loop {
        let Some(nap) = remaining(deadline) else {
            return Ok(());
        };
        std::thread::sleep(nap);

        // A loss this watcher records is its own commit, which the gate below
        // cannot see — so it reads the log itself.
        let mut found = 0;
        if swept.elapsed() >= SWEEP {
            found = lost.sweep(db, local) + crate::session_ops::launch_failure::sweep(db, local);
            swept = Instant::now();
            let mut fields = crate::session_ops::read_fields::ReadFields::default();
            for (session, down) in hosts.look(db, filter) {
                let line = host_line(db, &registry, &session, down, backends);
                // A parked session reads `stopped` either way: nothing changed.
                let Some(mut line) = line.filter(|line| line["stopped"] != Value::Bool(true))
                else {
                    continue;
                };
                fields.annotate(db, Some(local), &registry, &session, &mut line);
                if !out.write(&line) {
                    return Ok(());
                }
            }
        }

        // The gate: unchanged means no other connection has committed, so
        // nothing can have happened and the log need not be read at all.
        let current = db.data_version().unwrap_or(version);
        if current == version && found == 0 {
            continue;
        }
        version = current;

        if !drain(
            db,
            &registry,
            filter,
            &mut seq,
            backends,
            &mut stopped_state,
            &mut out,
            local,
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
#[allow(clippy::too_many_arguments)]
fn drain(
    db: &Database,
    registry: &crate::session::AgentRegistry,
    filter: Option<SessionId>,
    seq: &mut i64,
    backends: Option<&crate::backend::BackendRegistry>,
    stopped_state: &mut HashMap<SessionId, bool>,
    out: &mut Stream,
    local: &crate::backend::BackendRegistry,
) -> bool {
    loop {
        let events = match crate::session_ops::events::drain(db, *seq, filter) {
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
        let ids: Vec<_> = events.iter().map(|e| e.session_id).collect();
        let facts = db.load_session_facts_for(&ids).unwrap_or_default();
        let mut fields = crate::session_ops::read_fields::ReadFields::default();
        let mut gap = db.session_event_gap(*seq).unwrap_or(false);
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
            let mut value = line(db, registry, &facts, event, stopped, backends);
            value["gap"] = serde_json::json!(gap);
            gap = false;
            fields.annotate_id(db, Some(local), registry, event.session_id, &mut value);
            if !out.write(&value) {
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
///
/// `seen_at` is the exception, and deliberately the row's: the acknowledgment
/// is a fact about *now* ("somebody has looked at this since"), so a replayed
/// `done` an operator has since read reports `idle` — the same answer `session
/// get` gives for that row in that second. `to_state` still carries the event's
/// own word verbatim, so nothing about the transition is lost.
fn line(
    db: &Database,
    registry: &crate::session::AgentRegistry,
    facts: &HashMap<SessionId, SessionFacts>,
    event: &SessionEventRow,
    stopped: bool,
    backends: Option<&crate::backend::BackendRegistry>,
) -> Value {
    let unknown = SessionFacts {
        name: String::new(),
        agent: String::new(),
        backend_id: String::new(),
        backend_type: String::new(),
        stopped: false,
        seen_at: None,
        launch_failure: None,
    };
    let facts = &*crate::session_ops::events::as_of(
        event,
        facts.get(&event.session_id).unwrap_or(&unknown),
    );
    // Only the pane probe needs the backend, and only a live row has a pane to
    // probe — so the lookup is paid for exactly where it is used.
    let row = backends
        .filter(|_| event.event != "gone")
        .and_then(|_| db.get_session_by_id(event.session_id).ok().flatten());
    let hook = assess(
        registry,
        facts,
        &HookRow {
            state: event.to_state.clone(),
            state_at: Some(event.at_ms),
            seen_at: facts.seen_at,
        },
        stopped,
        backends.zip(row.as_ref()),
    );
    crate::session_ops::events::render(event, facts, &hook, stopped)
}

/// What the stored columns, the host's recorded verdict and, with `--verify`,
/// the pane say about a session at the moment an event describes.
///
/// The same fold `session get`/`list` make ([`crate::session_ops::events::assess`],
/// then [`crate::session_ops::host_cli::with_host_verdict`]), so the words this
/// stream uses are the words every other surface uses. The verdict is the
/// host's *now*, like `seen_at` (see [`line`]): a report replayed from a host
/// that has since gone quiet is a report nobody can confirm.
fn assess(
    registry: &crate::session::AgentRegistry,
    facts: &SessionFacts,
    columns: &HookRow,
    stopped: bool,
    probe: Option<(
        &crate::backend::BackendRegistry,
        &crate::sync::SharedSession,
    )>,
) -> Assessment {
    let down = crate::session_ops::host_cli::recorded_down(&facts.backend_type);
    assess_sampled(registry, facts, columns, stopped, down, probe)
}

/// [`assess`] with the host verdict already read, as `down`.
fn assess_sampled(
    registry: &crate::session::AgentRegistry,
    facts: &SessionFacts,
    columns: &HookRow,
    stopped: bool,
    down: bool,
    probe: Option<(
        &crate::backend::BackendRegistry,
        &crate::sync::SharedSession,
    )>,
) -> Assessment {
    let hook = crate::session_ops::events::assess(registry, facts, columns, stopped);
    if stopped {
        return hook;
    }
    let hook = crate::session_ops::host_cli::with_host_down(hook, down);
    let Some((backends, session)) = probe else {
        return hook;
    };
    if crate::session::Route::is_remote_key(&session.backend_type) {
        return hook.pane_unavailable();
    }
    crate::cli::sessions::probe_pane(hook, registry, &facts.agent, backends, session)
}

/// The remote sessions whose host was recorded as not answering at the last
/// look.
///
/// A host verdict is a file the interface's probe writes, not a row, so its
/// going down or coming back appends nothing to the event log. Each sweep
/// compares and reports the flips itself, as `changed`/`host_down` and
/// `changed`/`host_up`.
#[derive(Default)]
struct HostVerdicts {
    down: HashSet<SessionId>,
}

impl HostVerdicts {
    /// Look again: every watched remote session whose host went down or came
    /// back since the last look, with the row it was read from and whether it
    /// is now down. The verdict is read once per route spelling, not once per
    /// session.
    fn look(
        &mut self,
        db: &Database,
        filter: Option<SessionId>,
    ) -> Vec<(crate::sync::SharedSession, bool)> {
        let mut by_host: HashMap<String, bool> = HashMap::new();
        let mut down = HashSet::new();
        let mut flips = Vec::new();
        for session in db.list_active_sessions().unwrap_or_default() {
            if filter.is_some_and(|only| only != session.id)
                || !crate::session::Route::is_remote_key(&session.backend_type)
            {
                continue;
            }
            let is_down = *by_host
                .entry(session.backend_type.clone())
                .or_insert_with_key(|route| crate::session_ops::host_cli::recorded_down(route));
            if is_down {
                down.insert(session.id);
            }
            if is_down != self.down.contains(&session.id) {
                flips.push((session, is_down));
            }
        }
        self.down = down;
        flips
    }
}

/// One host flip as a stream line. Not in the log, so it has no `seq` of its
/// own to resume from (`null`), and no hook transition (`from_state` and
/// `to_state` are `null`): what changed is `state`. Assessed with the `down`
/// the sweep sampled, not a second read, so `reason` and `state` cannot
/// disagree when the verdict moves in between.
fn host_line(
    db: &Database,
    registry: &crate::session::AgentRegistry,
    session: &crate::sync::SharedSession,
    down: bool,
    backends: Option<&crate::backend::BackendRegistry>,
) -> Option<Value> {
    let facts = db
        .load_session_facts_for(&[session.id])
        .ok()?
        .remove(&session.id)?;
    let columns = db.load_hook_state(session.id).ok().flatten()?;
    let hook = assess_sampled(
        registry,
        &facts,
        &columns,
        facts.stopped,
        down,
        backends.map(|b| (b, session)),
    );
    let mut line = crate::session_ops::events::present(0, session.id, &facts, &hook);
    line["seq"] = Value::Null;
    line["event"] = serde_json::json!("changed");
    line["reason"] = serde_json::json!(if down { "host_down" } else { "host_up" });
    line["to_state"] = Value::Null;
    Some(line)
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
    "exit_code",
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
    let seq = event
        .get("seq")
        .and_then(Value::as_i64)
        .map_or_else(|| "-".to_string(), |seq| seq.to_string());
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
    let exit = event
        .get("exit_code")
        .and_then(Value::as_i64)
        .map(|code| format!("  exit {code}"))
        .unwrap_or_default();
    format!(
        "{seq:>5}  {:<8}{reason}  {}  [{}]{transition}{exit}",
        field("event"),
        field("name"),
        field("state"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host line is one sample of the verdict: its `reason` and its `state`
    /// say the same thing even when the verdict changed between the sweep's
    /// look and the line being built — here, cleared before the line is.
    #[test]
    fn a_host_line_reads_the_verdict_it_was_sampled_with() {
        let temp = tempfile::TempDir::new().unwrap();
        let _paths = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        let session = crate::sync::SharedSession {
            id: SessionId::default(),
            name: "on-a-host".into(),
            agent: "claude".into(),
            backend_id: "%1".into(),
            backend_type: "ssh:elsewhere:tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        };
        db.upsert_session(&session).unwrap();
        db.set_hook_state(session.id, "idle").unwrap();
        let registry = crate::agent::agent_config::load_or_seed();

        // No verdict on file: the host answered again after the look.
        let down = host_line(&db, &registry, &session, true, None).unwrap();
        assert_eq!(down["reason"], "host_down");
        assert_eq!(down["state"], "unreachable", "{down}");
    }
}
