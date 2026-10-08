//! Noticing a session whose pane died with nobody asking.
//!
//! A multiplexer crash takes every pane on its server, and nothing that writes
//! the event log runs inside the server to say so — the heartbeat keeper is a
//! window on it too. So the loss is noticed from outside: a periodic look at
//! the local backends' windows, against the rows that say a pane should be
//! there. What it finds becomes a `changed`/`lost` event, which is how a driver
//! tailing `watch` learns its worker is gone.
//!
//! Local sessions only. A session on a `--host` lives on that host's server,
//! which the host's own watcher looks at.

use std::collections::HashMap;

use crate::session::{Route, SessionId};
use crate::storage::Database;

/// What one watcher remembers between looks.
///
/// A pane has to be missing on **two** consecutive looks before it is a loss:
/// `session restart` kills the old window before it spawns the new one, and a
/// look that lands in between would otherwise report a restart as a death.
#[derive(Debug, Default)]
pub struct LostSweep {
    /// Sessions missing on the previous look, with the pane they pointed at.
    suspects: HashMap<SessionId, String>,
}

impl LostSweep {
    /// One look at the local backends' windows: record every loss it
    /// confirms. Returns how many it recorded.
    pub fn sweep(&mut self, db: &Database, backends: &crate::backend::BackendRegistry) -> usize {
        let rows = match db.list_active_sessions() {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("could not list sessions to check their panes: {e}");
                return 0;
            }
        };
        let stopped = db.load_stopped_sessions().unwrap_or_default();
        let lost = db.load_lost_sessions().unwrap_or_default();
        // One listing per backend. A backend that could not be asked proves
        // nothing about its rows: they are not missing on this look, so their
        // confirmation starts over.
        let mut listings: HashMap<String, Option<crate::backend::identity::WindowIndex>> =
            HashMap::new();
        let mut missing = HashMap::new();
        for row in rows {
            if Route::is_remote_key(&row.backend_type) || stopped.contains(&row.id) {
                continue;
            }
            let Ok(backend) = super::windows::backend_for(backends, &row.backend_type) else {
                continue;
            };
            let listing =
                listings
                    .entry(backend.name().to_string())
                    .or_insert_with(|| match backend.discover() {
                        Ok(found) => {
                            Some(crate::backend::identity::WindowIndex::from_listing(found))
                        }
                        Err(e) => {
                            tracing::debug!(
                                "could not list the windows on {}: {e:#}",
                                backend.name()
                            );
                            None
                        }
                    });
            let Some(index) = listing.as_ref() else {
                continue;
            };
            match index.agent_window(&row.id.to_string(), &row.name) {
                // Already reported: nothing to confirm, and no write to take.
                crate::backend::Located::Absent if !lost.contains(&row.id) => {
                    missing.insert(row.id, row.backend_id);
                }
                // Back again — however it came back, on whatever pane id — so
                // its next death is news.
                crate::backend::Located::At(_) if lost.contains(&row.id) => {
                    if let Err(e) = db.clear_session_lost(row.id) {
                        tracing::warn!("could not clear the lost mark of {}: {e}", row.id);
                    }
                }
                _ => {}
            }
        }
        self.confirm(missing)
            .into_iter()
            .filter(|(id, pane)| {
                db.record_session_lost(*id, pane)
                    .map_err(|e| tracing::warn!("could not record the loss of {id}: {e}"))
                    .unwrap_or(false)
            })
            .count()
    }

    /// The sessions this look confirms lost: missing now, and missing with the
    /// same pane on the look before.
    fn confirm(&mut self, missing: HashMap<SessionId, String>) -> Vec<(SessionId, String)> {
        let confirmed = missing
            .iter()
            .filter(|(id, pane)| self.suspects.get(id) == Some(pane))
            .map(|(id, pane)| (*id, pane.clone()))
            .collect();
        self.suspects = missing;
        confirmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing(pairs: &[(SessionId, &str)]) -> HashMap<SessionId, String> {
        pairs
            .iter()
            .map(|(id, pane)| (*id, pane.to_string()))
            .collect()
    }

    #[test]
    fn a_pane_missing_twice_is_lost() {
        let mut sweep = LostSweep::default();
        let id = SessionId::default();
        assert!(sweep.confirm(missing(&[(id, "%1")])).is_empty());
        assert_eq!(
            sweep.confirm(missing(&[(id, "%1")])),
            vec![(id, "%1".to_string())]
        );
    }

    #[test]
    fn a_restart_between_two_looks_is_not_a_loss() {
        let mut sweep = LostSweep::default();
        let id = SessionId::default();
        assert!(sweep.confirm(missing(&[(id, "%1")])).is_empty());
        // The second look finds the row on a new pane — a relaunch the listing
        // has not caught up with yet.
        assert!(sweep.confirm(missing(&[(id, "%7")])).is_empty());
    }

    #[test]
    fn a_pane_that_comes_back_clears_the_suspicion() {
        let mut sweep = LostSweep::default();
        let id = SessionId::default();
        assert!(sweep.confirm(missing(&[(id, "%1")])).is_empty());
        assert!(sweep.confirm(HashMap::new()).is_empty());
        assert!(sweep.confirm(missing(&[(id, "%1")])).is_empty());
    }
}
