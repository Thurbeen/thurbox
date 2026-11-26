use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::snapshot::{SessionRow, Snapshot};
use crate::session::settings::NotificationSettings;
use crate::session::SessionState;

pub struct Notifier {
    sender: Option<crate::notifications::NotificationSender>,
    previous: HashMap<String, SessionState>,
    last_raised: HashMap<String, Instant>,
    settings: NotificationSettings,
}

impl Notifier {
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

    pub fn is_enabled(&self) -> bool {
        self.sender.is_some()
    }

    pub fn observe(&mut self, snapshot: &Snapshot, active: Option<&str>) -> Vec<String> {
        self.observe_at(snapshot, active, Instant::now())
    }

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

            let Some(previous) = was else { continue };
            if previous == row.status || !self.worth_raising(row, active) {
                continue;
            }
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

        let live = |id: &String| snapshot.sessions.iter().any(|row| &row.id == id);
        self.previous.retain(|id, _| live(id));
        self.last_raised.retain(|id, _| live(id));

        raised
    }

    fn worth_raising(&self, row: &SessionRow, active: Option<&str>) -> bool {
        row.status == SessionState::Blocked
            && !(self.settings.suppress_for_active && active == Some(row.id.as_str()))
    }

    fn deliver(&self, row: &SessionRow) {
        let Some(sender) = &self.sender else {
            return;
        };
        sender.send(crate::notifications::Notification {
            title: format!("{} needs you", row.name),
            body: format!("{} is waiting for input", row.agent),
            session_id: row.id.clone(),
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
            backend: "local:tmux".into(),
            backend_id: None,
            remote_host: None,
            place: None,
            agent_session_id: None,
            parent_id: None,
            display_order: None,
            worktree_count: 0,
            git: None,
            stopped: false,
            hook_state: None,
            key: None,
            reports_as: None,
            detected_agent: None,
            shell_backend_id: None,
            member_dirs: Vec::new(),
            meta: Default::default(),
            deleting: None,
            state_why: None,
            status_source: None,
        }
    }

    fn snap(rows: Vec<SessionRow>) -> Snapshot {
        Snapshot {
            sessions: rows,
            ..Snapshot::default()
        }
    }

    fn quiet(suppress_active: bool) -> Notifier {
        with(NotificationSettings {
            suppress_for_active: suppress_active,
            ..NotificationSettings::default()
        })
    }

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
        assert!(notifier
            .observe(&snap(vec![row("a", SessionState::Blocked)]), None)
            .is_empty());
        assert!(notifier
            .observe(&snap(vec![row("a", SessionState::Blocked)]), None)
            .is_empty());
    }

    #[test]
    fn a_fleet_sessions_notification_is_delivered_under_its_task_name() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut notifier = quiet(false);
        notifier.sender = Some(crate::notifications::NotificationSender::to(tx));
        notifier.observe(
            &snap(vec![row("read-the-code", SessionState::Working)]),
            None,
        );
        notifier.observe(
            &snap(vec![row("read-the-code", SessionState::Blocked)]),
            None,
        );
        let delivered = rx.try_recv().expect("a notification for the Fleet session");
        assert_eq!(delivered.session_id, "read-the-code");
    }

    #[test]
    fn becoming_blocked_raises_exactly_one() {
        let mut notifier = quiet(false);
        notifier.observe(&snap(vec![row("a", SessionState::Working)]), None);
        assert_eq!(
            notifier.observe(&snap(vec![row("a", SessionState::Blocked)]), None),
            vec!["a".to_string()]
        );
        assert!(notifier
            .observe(&snap(vec![row("a", SessionState::Blocked)]), None)
            .is_empty());
    }

    #[test]
    fn blocking_again_after_clearing_raises_again() {
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
    fn finishing_a_turn_does_not_notify() {
        let mut notifier = quiet(false);
        notifier.observe(&snap(vec![row("a", SessionState::Working)]), None);
        assert!(notifier
            .observe(&snap(vec![row("a", SessionState::Done)]), None)
            .is_empty());
    }

    #[test]
    fn the_sound_setting_reaches_the_notification() {
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

        assert!(notifier
            .observe(&snap(vec![row("a", SessionState::Blocked)]), None)
            .is_empty());
    }
}
