//! Where `telemetry`'s export of the session event log has got to (ADR-37).
//!
//! The exporter runs from `automation tick`, a fresh process each time, so the
//! position in [`super::session_events`] it has sent up to lives here. Two
//! things keep a row from going out twice when the keeper, an OS timer and a
//! hand-run `tick` overlap: a short lease that only one exporter holds at a
//! time, and a cursor that moves by compare-and-swap. A failed export moves
//! nothing, so its rows go out with the next tick — a duplicate is preferred
//! to a gap.
//!
//! A table of its own rather than a `metadata` key: the snapshot triggers
//! (schema v49) bump `rows_version` on every `metadata` write, and an advance
//! every minute would rebuild the interface's snapshot for nothing.

use rusqlite::{params, OptionalExtension};

use crate::session::SessionId;

use super::{Database, SessionEventRow};

/// One event to export, with the facts of the session it belongs to — `None`
/// where the row has been removed from the database since.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportEvent {
    pub event: SessionEventRow,
    pub session: Option<ExportSession>,
}

/// The facts of a session an exported event is labelled with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSession {
    pub name: String,
    pub agent: String,
    pub backend_type: String,
    /// Epoch ms the row was created.
    pub created_at: i64,
}

impl Database {
    /// Take the export lease until `now_ms + lease_ms`, returning the cursor to
    /// export from — `None` while another exporter holds the lease.
    ///
    /// The first claim starts the cursor at the head of the log, so turning
    /// telemetry on does not replay the history from before it was on.
    pub fn claim_telemetry_export(
        &self,
        now_ms: i64,
        lease_ms: i64,
    ) -> rusqlite::Result<Option<i64>> {
        let tx = self.write_transaction()?;
        self.conn.execute(
            "INSERT OR IGNORE INTO telemetry_export (id, cursor, lease_until) \
             VALUES (1, (SELECT COALESCE(MAX(seq), 0) FROM session_events), 0)",
            [],
        )?;
        let claimed = self.conn.execute(
            "UPDATE telemetry_export SET lease_until = ?1 WHERE id = 1 AND lease_until <= ?2",
            params![now_ms + lease_ms, now_ms],
        )?;
        let cursor = if claimed == 1 {
            Some(self.conn.query_row(
                "SELECT cursor FROM telemetry_export WHERE id = 1",
                [],
                |r| r.get(0),
            )?)
        } else {
            None
        };
        tx.commit()?;
        Ok(cursor)
    }

    /// Move the cursor from `from` to `to` and give the lease up. `false` when
    /// the cursor was no longer at `from` — an exporter whose lease ran out
    /// lost the rows to the one that took it over.
    pub fn advance_telemetry_cursor(&self, from: i64, to: i64) -> rusqlite::Result<bool> {
        let moved = self.conn.execute(
            "UPDATE telemetry_export SET cursor = ?2, lease_until = 0 \
             WHERE id = 1 AND cursor = ?1",
            params![from, to],
        )?;
        Ok(moved == 1)
    }

    /// Give the lease up without moving the cursor, after a failed export.
    pub fn release_telemetry_export(&self) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE telemetry_export SET lease_until = 0 WHERE id = 1",
            [],
        )?;
        Ok(())
    }

    /// Up to `limit` events after `since`, oldest first, each with its
    /// session's facts.
    pub fn session_events_for_export(
        &self,
        since: i64,
        limit: usize,
    ) -> rusqlite::Result<Vec<ExportEvent>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT e.seq, e.session_id, e.event, e.reason, e.from_state, e.to_state, \
             e.at_ms, e.exit_code, e.detail, s.name, s.agent, s.backend_type, s.created_at \
             FROM session_events e LEFT JOIN sessions s ON s.id = e.session_id \
             WHERE e.seq > ?1 ORDER BY e.seq LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since, limit as i64], |row| {
            let id: String = row.get(1)?;
            let name: Option<String> = row.get(9)?;
            let session = match name {
                Some(name) => Some(ExportSession {
                    name,
                    agent: row.get(10)?,
                    backend_type: row.get(11)?,
                    created_at: row.get(12)?,
                }),
                None => None,
            };
            Ok((
                id,
                ExportEvent {
                    event: SessionEventRow {
                        seq: row.get(0)?,
                        session_id: SessionId::default(),
                        event: row.get(2)?,
                        reason: row.get(3)?,
                        from_state: row.get(4)?,
                        to_state: row.get(5)?,
                        at_ms: row.get(6)?,
                        exit_code: row.get(7)?,
                        detail: row.get(8)?,
                    },
                    session,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, mut export) = row?;
            if let Ok(id) = id.parse::<SessionId>() {
                export.event.session_id = id;
                out.push(export);
            }
        }
        Ok(out)
    }

    /// The state `id` was in at `seq`, and since when: its last event up to
    /// `seq` that says anything about the agent's state. `None` when that event
    /// ended the state (a stop, a loss, a delete) or there is none.
    ///
    /// What lets a turn that began before one export and ended in the next be
    /// measured from its real start. A rename (`updated`) says nothing about
    /// the agent, so it is passed over.
    pub fn state_at_seq(&self, id: SessionId, seq: i64) -> rusqlite::Result<Option<(String, i64)>> {
        let row: Option<(String, Option<String>, i64)> = self
            .conn
            .prepare_cached(
                "SELECT reason, to_state, at_ms FROM session_events \
                 WHERE session_id = ?1 AND seq <= ?2 AND reason <> 'updated' \
                 ORDER BY seq DESC LIMIT 1",
            )?
            .query_row(params![id.to_string(), seq], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        Ok(match row {
            Some((reason, Some(state), at)) if reason == "state" => Some((state, at)),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::SharedSession;

    fn row(name: &str) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: "claude".into(),
            backend_id: "%1".into(),
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
            key: None,
        }
    }

    #[test]
    fn the_first_claim_starts_at_the_head_of_the_log() {
        let db = Database::open_in_memory().unwrap();
        let session = row("worker");
        db.upsert_session(&session).unwrap();
        let head = db.latest_session_event_seq().unwrap();
        assert!(head > 0);
        assert_eq!(
            db.claim_telemetry_export(1_000, 30_000).unwrap(),
            Some(head)
        );
    }

    #[test]
    fn a_held_lease_turns_a_second_exporter_away_until_it_runs_out() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.claim_telemetry_export(1_000, 30_000).unwrap(), Some(0));
        assert_eq!(db.claim_telemetry_export(2_000, 30_000).unwrap(), None);
        assert_eq!(db.claim_telemetry_export(31_000, 30_000).unwrap(), Some(0));
    }

    #[test]
    fn the_cursor_moves_only_from_where_it_was() {
        let db = Database::open_in_memory().unwrap();
        db.claim_telemetry_export(1_000, 30_000).unwrap();
        assert!(db.advance_telemetry_cursor(0, 5).unwrap());
        assert!(!db.advance_telemetry_cursor(0, 9).unwrap());
        assert_eq!(db.claim_telemetry_export(2_000, 30_000).unwrap(), Some(5));
    }

    #[test]
    fn a_release_keeps_the_cursor_and_frees_the_lease() {
        let db = Database::open_in_memory().unwrap();
        db.claim_telemetry_export(1_000, 30_000).unwrap();
        db.release_telemetry_export().unwrap();
        assert_eq!(db.claim_telemetry_export(2_000, 30_000).unwrap(), Some(0));
    }

    #[test]
    fn exported_events_carry_their_sessions_facts() {
        let db = Database::open_in_memory().unwrap();
        let session = row("worker");
        db.upsert_session(&session).unwrap();
        db.set_hook_state(session.id, "working").unwrap();

        let events = db.session_events_for_export(0, 100).unwrap();
        assert_eq!(events.len(), 2);
        let facts = events[1].session.as_ref().expect("the row stands");
        assert_eq!(
            (facts.name.as_str(), facts.agent.as_str()),
            ("worker", "claude")
        );
        assert_eq!(events[1].event.to_state.as_deref(), Some("working"));
    }

    #[test]
    fn the_state_at_a_seq_skips_renames_and_ends_at_a_stop() {
        let db = Database::open_in_memory().unwrap();
        let session = row("worker");
        db.upsert_session(&session).unwrap();
        assert_eq!(db.state_at_seq(session.id, i64::MAX).unwrap(), None);

        db.set_hook_state(session.id, "working").unwrap();
        let working = db.latest_session_event_seq().unwrap();
        let state = db.state_at_seq(session.id, working).unwrap();
        assert_eq!(state.map(|(s, _)| s).as_deref(), Some("working"));

        db.conn_ref()
            .execute(
                "INSERT INTO session_events (session_id, event, reason, at_ms) \
                 VALUES (?1, 'changed', 'updated', 0)",
                [session.id.to_string()],
            )
            .unwrap();
        let state = db.state_at_seq(session.id, i64::MAX).unwrap();
        assert_eq!(state.map(|(s, _)| s).as_deref(), Some("working"));

        db.conn_ref()
            .execute(
                "INSERT INTO session_events (session_id, event, reason, at_ms) \
                 VALUES (?1, 'changed', 'stopped', 0)",
                [session.id.to_string()],
            )
            .unwrap();
        assert_eq!(db.state_at_seq(session.id, i64::MAX).unwrap(), None);
    }

    #[test]
    fn the_cursor_moves_no_snapshot_generation() {
        let db = Database::open_in_memory().unwrap();
        let rows = |db: &Database| -> i64 {
            db.conn_ref()
                .query_row("SELECT rows_version FROM snapshot_versions", [], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        let before = rows(&db);
        db.claim_telemetry_export(1_000, 30_000).unwrap();
        db.advance_telemetry_cursor(0, 1).unwrap();
        assert_eq!(rows(&db), before);
    }
}
