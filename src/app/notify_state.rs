//! Per-session bookkeeping for OS notifications: prior `SessionStatus`, last
//! fire timestamp (for dedup), and the dispatcher handle. Pure logic — no
//! direct dependence on `App` state — so the transition rule is unit-testable
//! without spinning up an `App`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::notifications::{Notification, NotificationSender};
use crate::session::settings::NotificationSettings;
use crate::session::{SessionId, SessionStatus};

/// State the notification path keeps across ticks. Owned by `App` and only
/// constructed when the feature is enabled.
pub struct NotificationState {
    sender: NotificationSender,
    settings: NotificationSettings,
    /// Status seen on the previous tick, keyed by session id. A first-time
    /// observation is recorded with no notification — we only fire on a real
    /// transition.
    prev_status: HashMap<SessionId, SessionStatus>,
    /// Per-session moment we last fired a notification. Drives the
    /// `min_interval_secs` dedup floor.
    last_fired_at: HashMap<SessionId, Instant>,
}

impl NotificationState {
    pub fn new(sender: NotificationSender, settings: NotificationSettings) -> Self {
        Self {
            sender,
            settings,
            prev_status: HashMap::new(),
            last_fired_at: HashMap::new(),
        }
    }

    /// Drop bookkeeping for sessions that no longer exist so the maps stay
    /// bounded across long sessions.
    pub fn prune_to(&mut self, live: &[SessionId]) {
        self.prev_status.retain(|id, _| live.contains(id));
        self.last_fired_at.retain(|id, _| live.contains(id));
    }

    /// Observe one session's status this tick. Returns the notification to
    /// fire when the transition crosses the "needs attention" threshold,
    /// dedup window has elapsed, and the active-suppression rule allows it.
    ///
    /// `now` is taken as a parameter so tests are deterministic.
    pub fn observe(
        &mut self,
        id: SessionId,
        status: SessionStatus,
        is_active: bool,
        now: Instant,
    ) -> TransitionDecision {
        let prev = self.prev_status.insert(id, status);

        // First time we see this session: only record, never fire. This
        // prevents a flood of notifications on TUI startup, when every
        // session's initial status looks like a "transition" from nothing.
        let Some(prev) = prev else {
            return TransitionDecision::NoFire;
        };

        if prev == status {
            return TransitionDecision::NoFire;
        }

        if !self.should_notify_on(prev, status) {
            return TransitionDecision::NoFire;
        }

        if is_active && self.settings.suppress_for_active {
            return TransitionDecision::NoFire;
        }

        let interval = Duration::from_secs(self.settings.min_interval_secs);
        if let Some(last) = self.last_fired_at.get(&id) {
            if now.duration_since(*last) < interval {
                return TransitionDecision::ThrottledByDedup;
            }
        }

        self.last_fired_at.insert(id, now);
        TransitionDecision::Fire
    }

    /// Whether a `prev → current` transition is one we notify about. Pure;
    /// no state. The "attention" case is the explicit OSC bell / OSC 9 /
    /// OSC 777 from the agent; the "waiting" case is the timing-only quiet
    /// state, which a user can opt into for agents that don't ring a bell.
    fn should_notify_on(&self, prev: SessionStatus, current: SessionStatus) -> bool {
        match current {
            SessionStatus::Attention => true,
            SessionStatus::Waiting if self.settings.also_on_waiting => {
                // Only the Busy → Waiting edge is interesting; an Idle
                // (exited) → Waiting transition can't happen, and an
                // Attention → Waiting transition is the user already
                // having acknowledged the signal.
                prev == SessionStatus::Busy
            }
            _ => false,
        }
    }

    /// Build the notification body from the session's last OSC message, or a
    /// generic fallback when the agent only rang a bell (no message text).
    pub fn build_notification(
        id: SessionId,
        name: &str,
        agent: &str,
        notification_text: Option<&str>,
        sound: bool,
    ) -> Notification {
        let title = format!("{name} · {agent}");
        let body = notification_text
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Waiting for input".into());
        Notification {
            session_id: id,
            title,
            body,
            sound,
        }
    }

    pub fn send(&self, n: Notification) {
        self.sender.send(n);
    }

    pub fn sound_enabled(&self) -> bool {
        self.settings.sound
    }
}

/// What `observe` decided to do this tick. Surface-level callers only care
/// about `Fire`; the other variants are kept distinct for tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionDecision {
    /// No transition / not a transition we notify on / active suppressed.
    NoFire,
    /// Transition was notify-worthy but we fired one too recently.
    ThrottledByDedup,
    /// Fire a notification.
    Fire,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn test_state(
        also_on_waiting: bool,
        suppress_active: bool,
        interval: u64,
    ) -> NotificationState {
        let (tx, _rx) = mpsc::channel();
        let sender = crate::notifications::NotificationSender::__test_with_sender(tx);
        NotificationState::new(
            sender,
            NotificationSettings {
                also_on_waiting,
                suppress_for_active: suppress_active,
                sound: true,
                min_interval_secs: interval,
            },
        )
    }

    #[test]
    fn first_observation_never_fires() {
        let mut s = test_state(false, true, 0);
        let id = SessionId::default();
        let now = Instant::now();
        assert_eq!(
            s.observe(id, SessionStatus::Attention, false, now),
            TransitionDecision::NoFire
        );
    }

    #[test]
    fn same_status_never_fires() {
        let mut s = test_state(false, true, 0);
        let id = SessionId::default();
        let now = Instant::now();
        let _ = s.observe(id, SessionStatus::Attention, false, now);
        assert_eq!(
            s.observe(id, SessionStatus::Attention, false, now),
            TransitionDecision::NoFire
        );
    }

    #[test]
    fn busy_to_attention_fires() {
        let mut s = test_state(false, true, 0);
        let id = SessionId::default();
        let now = Instant::now();
        let _ = s.observe(id, SessionStatus::Busy, false, now);
        assert_eq!(
            s.observe(id, SessionStatus::Attention, false, now),
            TransitionDecision::Fire
        );
    }

    #[test]
    fn busy_to_waiting_only_fires_when_opted_in() {
        let mut s = test_state(false, true, 0);
        let id = SessionId::default();
        let now = Instant::now();
        let _ = s.observe(id, SessionStatus::Busy, false, now);
        assert_eq!(
            s.observe(id, SessionStatus::Waiting, false, now),
            TransitionDecision::NoFire
        );

        let mut s = test_state(true, true, 0);
        let _ = s.observe(id, SessionStatus::Busy, false, now);
        assert_eq!(
            s.observe(id, SessionStatus::Waiting, false, now),
            TransitionDecision::Fire
        );
    }

    #[test]
    fn attention_to_waiting_never_fires_even_with_also_on_waiting() {
        // The user has already seen the Attention signal; sliding back to
        // Waiting is not a new event worth re-notifying.
        let mut s = test_state(true, true, 0);
        let id = SessionId::default();
        let now = Instant::now();
        let _ = s.observe(id, SessionStatus::Attention, false, now);
        assert_eq!(
            s.observe(id, SessionStatus::Waiting, false, now),
            TransitionDecision::NoFire
        );
    }

    #[test]
    fn active_session_suppressed_by_default() {
        let mut s = test_state(false, true, 0);
        let id = SessionId::default();
        let now = Instant::now();
        let _ = s.observe(id, SessionStatus::Busy, true, now);
        assert_eq!(
            s.observe(id, SessionStatus::Attention, true, now),
            TransitionDecision::NoFire
        );
    }

    #[test]
    fn active_session_fires_when_suppression_off() {
        let mut s = test_state(false, false, 0);
        let id = SessionId::default();
        let now = Instant::now();
        let _ = s.observe(id, SessionStatus::Busy, true, now);
        assert_eq!(
            s.observe(id, SessionStatus::Attention, true, now),
            TransitionDecision::Fire
        );
    }

    #[test]
    fn rapid_re_attention_is_throttled() {
        let mut s = test_state(false, true, 60);
        let id = SessionId::default();
        let t0 = Instant::now();
        let _ = s.observe(id, SessionStatus::Busy, false, t0);
        assert_eq!(
            s.observe(id, SessionStatus::Attention, false, t0),
            TransitionDecision::Fire
        );

        // Attention → Busy → Attention within the dedup window.
        let _ = s.observe(id, SessionStatus::Busy, false, t0 + Duration::from_secs(1));
        assert_eq!(
            s.observe(
                id,
                SessionStatus::Attention,
                false,
                t0 + Duration::from_secs(2)
            ),
            TransitionDecision::ThrottledByDedup
        );

        // Past the window, a fresh fire is allowed.
        let _ = s.observe(id, SessionStatus::Busy, false, t0 + Duration::from_secs(70));
        assert_eq!(
            s.observe(
                id,
                SessionStatus::Attention,
                false,
                t0 + Duration::from_secs(80)
            ),
            TransitionDecision::Fire
        );
    }

    #[test]
    fn prune_drops_stale_sessions() {
        let mut s = test_state(false, true, 0);
        let keep = SessionId::default();
        let gone = SessionId::default();
        let now = Instant::now();
        let _ = s.observe(keep, SessionStatus::Busy, false, now);
        let _ = s.observe(gone, SessionStatus::Busy, false, now);
        s.prune_to(&[keep]);
        // The pruned session's first observation post-prune is a fresh
        // baseline → no fire.
        assert_eq!(
            s.observe(gone, SessionStatus::Attention, false, now),
            TransitionDecision::NoFire
        );
        // The kept session still has its baseline → a transition fires.
        assert_eq!(
            s.observe(keep, SessionStatus::Attention, false, now),
            TransitionDecision::Fire
        );
    }

    #[test]
    fn body_falls_back_when_message_is_empty() {
        let id = SessionId::default();
        let n = NotificationState::build_notification(id, "demo", "claude", None, true);
        assert_eq!(n.body, "Waiting for input");
        let n = NotificationState::build_notification(id, "demo", "claude", Some("   "), true);
        assert_eq!(n.body, "Waiting for input");
        let n =
            NotificationState::build_notification(id, "demo", "claude", Some("approved?"), true);
        assert_eq!(n.body, "approved?");
        assert_eq!(n.title, "demo · claude");
    }
}
