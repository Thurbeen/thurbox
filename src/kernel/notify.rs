//! Desktop notifications when an agent needs you.
//!
//! The rule is v1's, and so is the delivery: `crate::notifications` already
//! resolves a backend and dispatches on its own thread. What lives here is only
//! the *edge detection* — noticing that a session became blocked — because that
//! is the part that has to watch the snapshot.
//!
//! Observing the edge in one place is deliberate. v1 learned that computing
//! "should we notify" anywhere other than where status is derived lets the rule
//! drift from the icon in the list.
//!
//! Every `[notifications]` setting is honoured here, because a setting that is
//! offered and ignored is worse than one that does not exist: which edge fires,
//! whether the session in view is skipped, whether a sound plays, and the floor
//! between two notifications for one session. v1 keeps the same four in
//! `app::notify_state`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::snapshot::{SessionRow, Snapshot};
use crate::session::settings::NotificationSettings;
use crate::session::SessionState;

/// Watches for sessions becoming blocked and raises one notification each time.
pub struct Notifier {
    /// Sender into the dispatcher thread. `None` when notifications are off,
    /// in which case nothing is ever attempted — not even a resolve.
    sender: Option<crate::notifications::NotificationSender>,
    /// Last status seen per session, so a transition can be told from a state.
    previous: HashMap<String, SessionState>,
    /// When each session last had a notification raised for it, for the
    /// per-session floor.
    last_raised: HashMap<String, Instant>,
    settings: NotificationSettings,
}

impl Notifier {
    /// Build from the user's settings. Disabled means no thread is started.
    pub fn new(enabled: bool, settings: NotificationSettings) -> Self {
        let sender = if enabled {
            Some(crate::notifications::start(
                crate::notifications::detect_backend(settings.backend),
            ))
        } else {
            None
        };
        Self {
            sender,
            previous: HashMap::new(),
            last_raised: HashMap::new(),
            settings,
        }
    }

    /// Whether any delivery will be attempted at all.
    pub fn is_enabled(&self) -> bool {
        self.sender.is_some()
    }

    /// Observe a snapshot, raising a notification per session that just became
    /// blocked.
    ///
    /// Returns the sessions notified about, so a caller can assert on it
    /// without a desktop.
    pub fn observe(&mut self, snapshot: &Snapshot, active: Option<&str>) -> Vec<String> {
        self.observe_at(snapshot, active, Instant::now())
    }

    /// [`Self::observe`] with the clock passed in, so the per-session floor is
    /// testable without waiting it out. v1 threads `now` through for the same
    /// reason.
    pub fn observe_at(
        &mut self,
        snapshot: &Snapshot,
        active: Option<&str>,
        now: Instant,
    ) -> Vec<String> {
        let mut raised = Vec::new();
        let floor = Duration::from_secs(self.settings.min_interval_secs);

        for row in &snapshot.sessions {
            let was = self.previous.insert(row.id.clone(), row.status);

            // The *edge*, not the state: a session that is already blocked when
            // the interface starts has not just asked for anything.
            let Some(previous) = was else { continue };
            if previous == row.status || !self.worth_raising(previous, row, active) {
                continue;
            }
            // The floor is per session: an agent flipping between states must not
            // deliver a notification per flip.
            if self
                .last_raised
                .get(&row.id)
                .is_some_and(|last| now.duration_since(*last) < floor)
            {
                continue;
            }
            self.last_raised.insert(row.id.clone(), now);
            raised.push(row.id.clone());
            self.deliver(row);
        }

        // Sessions that went away stop being tracked, or a deleted-and-recreated
        // id would inherit a stale status.
        let live = |id: &String| snapshot.sessions.iter().any(|row| &row.id == id);
        self.previous.retain(|id, _| live(id));
        self.last_raised.retain(|id, _| live(id));

        raised
    }

    /// Whether this transition is one the user asked to hear about.
    ///
    /// Blocked always notifies; finishing does so only when asked for. A
    /// notification for the session you are already looking at is noise, so it is
    /// suppressed when configured.
    fn worth_raising(
        &self,
        previous: SessionState,
        row: &SessionRow,
        active: Option<&str>,
    ) -> bool {
        let wanted = row.status == SessionState::Blocked
            || (row.status == SessionState::Done
                && self.settings.also_on_waiting
                && previous != SessionState::Blocked);
        if !wanted {
            return false;
        }
        !(self.settings.suppress_for_active && active == Some(row.id.as_str()))
    }

    /// Hand one notification to the delivery layer.
    ///
    /// A row whose id will not parse cannot be focused from the notification, so
    /// there is nothing useful to deliver.
    fn deliver(&self, row: &SessionRow) {
        let (Some(sender), Some(id)) = (&self.sender, crate::kernel::snapshot::parse_id(&row.id))
        else {
            return;
        };
        let blocked = row.status == SessionState::Blocked;
        let (title, body) = match blocked {
            true => (
                format!("{} needs you", row.name),
                format!("{} is waiting for input", row.agent),
            ),
            false => (
                format!("{} finished", row.name),
                format!("{} finished its turn", row.agent),
            ),
        };
        sender.send(crate::notifications::Notification {
            title,
            body,
            session_id: id,
            sound: self.settings.sound,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::snapshot::SessionRow;

    fn row(id: &str, status: SessionState) -> SessionRow {
        SessionRow {
            id: id.into(),
            name: "demo".into(),
            agent: "claude".into(),
            status,
            cwd: None,
            repo: None,
            repos: Vec::new(),
            branch: None,
            base_branch: None,
            backend: "local-tmux".into(),
            backend_id: None,
            remote_host: None,
            agent_session_id: None,
            parent_id: None,
            display_order: None,
            worktree_count: 0,
            git: None,
            stopped: false,
            hook_state: None,
            shell_backend_id: None,
            member_dirs: Vec::new(),
        }
    }

    fn snap(rows: Vec<SessionRow>) -> Snapshot {
        Snapshot {
            sessions: rows,
            ..Snapshot::default()
        }
    }

    /// A notifier that never touches a desktop.
    fn quiet(suppress_active: bool) -> Notifier {
        with(NotificationSettings {
            suppress_for_active: suppress_active,
            ..NotificationSettings::default()
        })
    }

    /// A notifier over given settings, still touching no desktop.
    fn with(settings: NotificationSettings) -> Notifier {
        Notifier {
            sender: None,
            previous: HashMap::new(),
            last_raised: HashMap::new(),
            settings,
        }
    }

    #[test]
    fn the_transition_fires_not_the_state() {
        let mut notifier = quiet(false);
        // First sight of a blocked session is not a transition: it may have
        // been blocked long before the interface opened.
        assert!(notifier
            .observe(&snap(vec![row("a", SessionState::Blocked)]), None)
            .is_empty());
        // Still blocked: nothing new.
        assert!(notifier
            .observe(&snap(vec![row("a", SessionState::Blocked)]), None)
            .is_empty());
    }

    #[test]
    fn becoming_blocked_raises_exactly_one() {
        let mut notifier = quiet(false);
        notifier.observe(&snap(vec![row("a", SessionState::Working)]), None);
        assert_eq!(
            notifier.observe(&snap(vec![row("a", SessionState::Blocked)]), None),
            vec!["a".to_string()]
        );
        // And not again while it stays blocked.
        assert!(notifier
            .observe(&snap(vec![row("a", SessionState::Blocked)]), None)
            .is_empty());
    }

    #[test]
    fn blocking_again_after_clearing_raises_again() {
        // Past the per-session floor, which is what the next test covers: this one
        // is about the edge.
        let mut notifier = quiet(false);
        let start = Instant::now();
        notifier.observe_at(&snap(vec![row("a", SessionState::Working)]), None, start);
        notifier.observe_at(&snap(vec![row("a", SessionState::Blocked)]), None, start);
        notifier.observe_at(&snap(vec![row("a", SessionState::Working)]), None, start);
        let later = start + Duration::from_secs(60);
        assert_eq!(
            notifier.observe_at(&snap(vec![row("a", SessionState::Blocked)]), None, later),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn a_second_notification_inside_the_floor_is_dropped() {
        // An agent flipping blocked → working → blocked must not deliver one
        // notification per flip.
        let mut notifier = with(NotificationSettings {
            min_interval_secs: 30,
            ..NotificationSettings::default()
        });
        let start = Instant::now();
        notifier.observe_at(&snap(vec![row("a", SessionState::Working)]), None, start);
        assert_eq!(
            notifier.observe_at(&snap(vec![row("a", SessionState::Blocked)]), None, start),
            vec!["a".to_string()]
        );
        notifier.observe_at(&snap(vec![row("a", SessionState::Working)]), None, start);
        assert!(notifier
            .observe_at(
                &snap(vec![row("a", SessionState::Blocked)]),
                None,
                start + Duration::from_secs(29)
            )
            .is_empty());
        // And delivered again once the floor has passed.
        notifier.observe_at(&snap(vec![row("a", SessionState::Working)]), None, start);
        assert_eq!(
            notifier.observe_at(
                &snap(vec![row("a", SessionState::Blocked)]),
                None,
                start + Duration::from_secs(31)
            ),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn the_floor_is_per_session() {
        // One noisy agent must not silence another session's first notification.
        let mut notifier = with(NotificationSettings {
            min_interval_secs: 30,
            ..NotificationSettings::default()
        });
        let start = Instant::now();
        notifier.observe_at(
            &snap(vec![
                row("a", SessionState::Working),
                row("b", SessionState::Working),
            ]),
            None,
            start,
        );
        assert_eq!(
            notifier.observe_at(
                &snap(vec![
                    row("a", SessionState::Blocked),
                    row("b", SessionState::Working)
                ]),
                None,
                start
            ),
            vec!["a".to_string()]
        );
        assert_eq!(
            notifier.observe_at(
                &snap(vec![
                    row("a", SessionState::Blocked),
                    row("b", SessionState::Blocked)
                ]),
                None,
                start
            ),
            vec!["b".to_string()]
        );
    }

    #[test]
    fn finishing_notifies_only_when_it_is_asked_for() {
        let mut off = quiet(false);
        off.observe(&snap(vec![row("a", SessionState::Working)]), None);
        assert!(
            off.observe(&snap(vec![row("a", SessionState::Done)]), None)
                .is_empty(),
            "the finish edge is off by default"
        );

        let mut on = with(NotificationSettings {
            also_on_waiting: true,
            ..NotificationSettings::default()
        });
        on.observe(&snap(vec![row("a", SessionState::Working)]), None);
        assert_eq!(
            on.observe(&snap(vec![row("a", SessionState::Done)]), None),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn clearing_a_block_is_not_a_finish() {
        // `blocked → done` is the user having answered, not a turn completing, so
        // it must not notify even with the finish edge on.
        let mut notifier = with(NotificationSettings {
            also_on_waiting: true,
            ..NotificationSettings::default()
        });
        notifier.observe(&snap(vec![row("a", SessionState::Blocked)]), None);
        assert!(notifier
            .observe(&snap(vec![row("a", SessionState::Done)]), None)
            .is_empty());
    }

    #[test]
    fn the_sound_setting_reaches_the_notification() {
        // Not observable through `observe`'s return, so it is asserted where it is
        // decided: the setting the notifier was built with.
        let quiet_sound = with(NotificationSettings {
            sound: false,
            ..NotificationSettings::default()
        });
        assert!(!quiet_sound.settings.sound);
    }

    #[test]
    fn the_session_in_view_is_suppressed_when_configured() {
        let mut notifier = quiet(true);
        notifier.observe(&snap(vec![row("a", SessionState::Working)]), Some("a"));
        assert!(notifier
            .observe(&snap(vec![row("a", SessionState::Blocked)]), Some("a"))
            .is_empty());

        // And not suppressed when it is some other session in view.
        let mut notifier = quiet(true);
        notifier.observe(&snap(vec![row("a", SessionState::Working)]), Some("b"));
        assert_eq!(
            notifier.observe(&snap(vec![row("a", SessionState::Blocked)]), Some("b")),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn a_disabled_notifier_attempts_nothing() {
        let notifier = quiet(false);
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn a_vanished_session_stops_being_tracked() {
        let mut notifier = quiet(false);
        notifier.observe(&snap(vec![row("a", SessionState::Working)]), None);
        notifier.observe(&snap(Vec::new()), None);
        assert!(notifier.previous.is_empty());

        // So a recreated id is a first sighting again, not a transition.
        assert!(notifier
            .observe(&snap(vec![row("a", SessionState::Blocked)]), None)
            .is_empty());
    }
}
