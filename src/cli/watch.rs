//! `thurbox-cli watch` — session state as a stream, so nothing has to poll.
//!
//! Every consumer of thurbox outside its own process has had one way to learn
//! that something changed: ask again, on a timer. That is a poll loop in every
//! integration, each with its own interval, each wrong in one direction or the
//! other — too slow to react, or spending a query a second to react to nothing.
//!
//! The mechanism is the one the interface's own sync worker already uses:
//! SQLite's `PRAGMA data_version` changes when *another* connection commits, so
//! a reader can sit on a cheap pragma and re-read only when there is something
//! to re-read (see [`crate::sync`]). This is that gate in the CLI, turning
//! what it finds into one JSON object per line.
//!
//! Deliberately not the kernel's event bus. That would need an IPC channel out
//! of a running interface, and would then only work while one is running —
//! whereas everything a driver needs to be woken by (a status transition, a
//! session appearing or going) is already in the database, written by whoever
//! made the change, interface or not. A richer transport can replace what is
//! behind this command later without the command itself changing.

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use clap::Args;
use serde_json::json;

use crate::session::SessionId;
use crate::storage::Database;

/// How often the change gate is checked. A `PRAGMA data_version` on an open
/// connection is a memory read, not a query — this is a latency choice, not a
/// cost one.
const POLL: Duration = Duration::from_millis(250);

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
}

/// One session's watched state — everything a transition can be reported from.
///
/// Compared as a whole rather than field by field, so adding a field here is
/// all it takes for a change in it to become an event.
#[derive(PartialEq, Clone)]
struct Watched {
    name: String,
    state: Option<String>,
    backend_id: String,
    stopped: bool,
}

/// Stream session changes until the deadline (or forever) — one JSON object per
/// line, flushed as it is written so a reader blocked on the pipe wakes on it.
pub fn run(db: &Database, args: WatchArgs) -> Result<(), String> {
    let filter = args
        .session
        .as_deref()
        .map(|reference| super::session_ref::resolve(db, reference).map(|s| s.id))
        .transpose()?;

    let deadline = args
        .for_secs
        .map(|secs| Instant::now() + Duration::from_secs(secs));
    let mut previous = read(db, filter);
    if args.initial {
        for (id, row) in &previous {
            emit("present", id, row);
        }
    }
    let mut version = db.data_version().unwrap_or_default();

    loop {
        if deadline.is_some_and(|end| Instant::now() >= end) {
            return Ok(());
        }
        std::thread::sleep(POLL);

        // The gate: unchanged means no other connection has committed, so
        // nothing can have happened and the rows need not be read at all.
        let current = db.data_version().unwrap_or(version);
        if current == version {
            continue;
        }
        version = current;

        let now = read(db, filter);
        for (id, row) in &now {
            match previous.get(id) {
                None => emit("created", id, row),
                Some(before) if before != row => emit("changed", id, row),
                Some(_) => {}
            }
        }
        for (id, row) in &previous {
            if !now.contains_key(id) {
                emit("gone", id, row);
            }
        }
        previous = now;
    }
}

/// The watched state of every session (or the one asked for).
fn read(db: &Database, only: Option<SessionId>) -> HashMap<SessionId, Watched> {
    let states = db.load_hook_states().unwrap_or_default();
    let stopped = db.load_stopped_sessions().unwrap_or_default();
    db.list_active_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| only.is_none() || only == Some(s.id))
        .map(|s| {
            let state = states.get(&s.id).and_then(|h| h.state.clone());
            (
                s.id,
                Watched {
                    name: s.name,
                    state,
                    backend_id: s.backend_id,
                    stopped: stopped.contains(&s.id),
                },
            )
        })
        .collect()
}

/// One event. Written with an explicit flush: a stream whose reader is blocked
/// on the next line must not have that line sitting in a buffer.
fn emit(event: &str, id: &SessionId, row: &Watched) {
    let line = json!({
        "event": event,
        "session": id.to_string(),
        "name": row.name,
        "state": row.state,
        "backend_id": row.backend_id,
        "stopped": row.stopped,
        "at": crate::sync::current_time_millis(),
    });
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
