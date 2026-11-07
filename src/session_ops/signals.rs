//! The one way a hook report becomes a session's state.
//!
//! A report arrives three ways: `thurbox-cli session signal` from the agent's
//! own hook, the interface's subscription to a backend's status channel, and
//! the heartbeat's poll of that channel. All three are the same report about
//! the same row, so all three are judged here — allow-listed, refused for a
//! parked session, and, for a word read back from a channel, dropped when it
//! changes nothing — and written by the column that changed (ADR-26).
//!
//! The interface also writes the two edges that follow a report: the user
//! seeing a finished turn, and submitted input retiring the report it
//! superseded. Those are here too, so the snapshot worker writes no session
//! state of its own.

use crate::session::background_work::{ReportFrom, StopReport};
use crate::session::{SessionId, HOOK_STATES};
use crate::storage::{Database, HookRow};

/// Whether `word` is a state a hook may report. Everything else — a pane
/// option is free text set on a machine thurbox may not control — is never
/// interpreted as one.
pub fn is_state(word: &str) -> bool {
    HOOK_STATES.contains(&word)
}

/// Whether a word read back from a backend's status channel is worth writing,
/// judged against a copy of the row: `held` is the state it holds and
/// `stopped` whether it is parked.
///
/// A channel re-reports on every reconnect and every poll, so a word the row
/// already holds is not news, and writing it would re-stamp the report and
/// resurrect an acknowledged `done` as unseen. [`report`] decides the same
/// again under the write lock; this is for a caller that corrects its copy in
/// place before storage answers, or that would otherwise take the write lock
/// for every row on every cycle.
pub fn echo_changes(held: Option<&str>, stopped: bool, reported: &str) -> bool {
    is_state(reported) && !stopped && held != Some(reported)
}

/// What [`report`] did with a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reported {
    /// Stored as `state` — which sub-work in flight can make differ from the
    /// word reported (see [`crate::session::background_work::settle_report`]) —
    /// stamped `at`.
    Stored { state: String, at: i64 },
    /// The word is not a state.
    NotAState,
    /// No row took it: the session is parked or gone, or the report was an
    /// echo of the state it already holds.
    NotTaken,
}

/// Record one hook report for session `id`. `from` says where it came from:
/// [`ReportFrom::Echo`] for a word read back from a status channel, which is
/// taken only as a change; anything else is the agent itself, whose repeat of
/// a state is news of its liveness and re-stamps it. `stop` is the agent's
/// `Stop` payload, read for a `done`.
pub fn report(
    db: &Database,
    id: SessionId,
    reported: &str,
    from: ReportFrom<'_>,
    stop: Option<&StopReport>,
) -> rusqlite::Result<Reported> {
    if !is_state(reported) {
        return Ok(Reported::NotAState);
    }
    Ok(match db.report_hook_state(id, reported, from, stop)? {
        Some((at, state)) => Reported::Stored { state, at },
        None => Reported::NotTaken,
    })
}

/// The user saw the turn stamped `at` finish. False, and nothing written, once
/// a newer turn replaced it or it was already seen.
pub fn mark_seen(db: &Database, id: SessionId, at: i64) -> rusqlite::Result<bool> {
    db.mark_turn_seen(id, at)
}

/// Forget `report`, which submitted input superseded, unless a hook reported
/// again since it was read: a prompt hook running while the input lands wins.
pub fn retire(db: &Database, id: SessionId, report: &HookRow) -> rusqlite::Result<bool> {
    db.clear_hook_state_if_unchanged(id, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_echo_is_news_only_as_a_change_of_a_live_rows_state() {
        assert!(echo_changes(None, false, "working"));
        assert!(echo_changes(Some("working"), false, "done"));
        assert!(!echo_changes(Some("done"), false, "done"));
        assert!(!echo_changes(Some("working"), true, "done"));
        assert!(!echo_changes(None, false, "'; DROP TABLE sessions; --"));
    }
}
