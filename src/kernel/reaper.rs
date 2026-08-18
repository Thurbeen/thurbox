//! When a soft-deleted session's agent should be let go.
//!
//! Deleting softly keeps the row and the worktrees so the delete can be undone —
//! but the agent is not part of what an undo restores, and v2 was leaving it
//! running forever: a deleted session kept working, kept writing, and kept its
//! tmux window (parity-gap #12).
//!
//! Only the *timing* lives here, and deliberately: a session disappears from the
//! snapshot the moment it is deleted, so the question is "has it stayed gone long
//! enough", which is arithmetic over ids and instants. The killing itself is
//! `session_ops::reap_soft_deleted`, dispatched as a command like everything else
//! that touches the world.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::snapshot::Snapshot;

/// How long a deleted session keeps its agent, so the delete can still be undone.
/// v1's `UNDO_TIMEOUT`.
pub const UNDO_WINDOW: Duration = Duration::from_secs(10);

/// Watches sessions leave the snapshot and says when to let their agents go.
#[derive(Default)]
pub struct Reaper {
    /// Ids seen in the last snapshot — what "left" is measured against.
    seen: Vec<String>,
    /// Ids that left, and when their undo window closes.
    doomed: HashMap<String, Instant>,
}

impl Reaper {
    /// Fold in a snapshot and return the sessions whose window has closed.
    ///
    /// A returning id cancels its own reap: that is exactly what an undo is, and
    /// it needs no separate signal.
    pub fn observe(&mut self, snapshot: &Snapshot, now: Instant) -> Vec<String> {
        let live: Vec<String> = snapshot.sessions.iter().map(|row| row.id.clone()).collect();

        for id in &self.seen {
            if !live.contains(id) && !self.doomed.contains_key(id) {
                self.doomed.insert(id.clone(), now + UNDO_WINDOW);
            }
        }
        // Undone: it is back, so nothing is owed.
        self.doomed.retain(|id, _| !live.contains(id));
        self.seen = live;

        let due: Vec<String> = self
            .doomed
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &due {
            self.doomed.remove(id);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::snapshot::SessionRow;

    fn row(id: &str) -> SessionRow {
        SessionRow {
            id: id.into(),
            name: id.into(),
            agent: "claude".into(),
            status: "idle".into(),
            cwd: None,
            repo: None,
            repos: Vec::new(),
            branch: None,
            base_branch: None,
            backend: "local-tmux".into(),
            backend_id: Some("%1".into()),
            remote_host: None,
            agent_session_id: None,
            parent_id: None,
            display_order: None,
            worktree_count: 0,
            git: None,
            hook_state: None,
            shell_backend_id: None,
            member_dirs: Vec::new(),
        }
    }

    fn snap(ids: &[&str]) -> Snapshot {
        Snapshot {
            sessions: ids.iter().map(|id| row(id)).collect(),
            ..Snapshot::default()
        }
    }

    #[test]
    fn a_session_that_is_present_is_never_reaped() {
        let mut reaper = Reaper::default();
        let start = Instant::now();
        assert!(reaper.observe(&snap(&["a"]), start).is_empty());
        assert!(reaper
            .observe(&snap(&["a"]), start + UNDO_WINDOW * 10)
            .is_empty());
    }

    #[test]
    fn one_that_left_is_reaped_once_its_undo_window_closes() {
        let mut reaper = Reaper::default();
        let start = Instant::now();
        reaper.observe(&snap(&["a"]), start);
        // Gone, but still undoable.
        assert!(reaper.observe(&snap(&[]), start).is_empty());
        assert!(reaper
            .observe(&snap(&[]), start + UNDO_WINDOW - Duration::from_secs(1))
            .is_empty());
        assert_eq!(
            reaper.observe(&snap(&[]), start + UNDO_WINDOW),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn it_is_reaped_once_and_not_again() {
        let mut reaper = Reaper::default();
        let start = Instant::now();
        reaper.observe(&snap(&["a"]), start);
        reaper.observe(&snap(&[]), start);
        assert_eq!(reaper.observe(&snap(&[]), start + UNDO_WINDOW).len(), 1);
        assert!(reaper
            .observe(&snap(&[]), start + UNDO_WINDOW * 2)
            .is_empty());
    }

    #[test]
    fn coming_back_cancels_the_reap() {
        // Which is what an undo *is* — no separate signal is needed for it.
        let mut reaper = Reaper::default();
        let start = Instant::now();
        reaper.observe(&snap(&["a"]), start);
        reaper.observe(&snap(&[]), start);
        reaper.observe(&snap(&["a"]), start + Duration::from_secs(1));
        assert!(reaper
            .observe(&snap(&["a"]), start + UNDO_WINDOW * 2)
            .is_empty());
    }

    #[test]
    fn a_session_never_seen_alive_is_not_reaped() {
        // The interface starting up with rows already deleted must not kill
        // anything: they left before this process was watching.
        let mut reaper = Reaper::default();
        let start = Instant::now();
        assert!(reaper.observe(&snap(&[]), start).is_empty());
        assert!(reaper
            .observe(&snap(&[]), start + UNDO_WINDOW * 2)
            .is_empty());
    }

    #[test]
    fn each_session_carries_its_own_deadline() {
        let mut reaper = Reaper::default();
        let start = Instant::now();
        reaper.observe(&snap(&["a", "b"]), start);
        reaper.observe(&snap(&["b"]), start);
        let later = start + Duration::from_secs(5);
        reaper.observe(&snap(&[]), later);

        assert_eq!(
            reaper.observe(&snap(&[]), start + UNDO_WINDOW),
            vec!["a".to_string()],
            "b's window closes five seconds after a's"
        );
        assert_eq!(
            reaper.observe(&snap(&[]), later + UNDO_WINDOW),
            vec!["b".to_string()]
        );
    }
}
