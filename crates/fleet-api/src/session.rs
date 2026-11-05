//! Session: one running agent, the unit the runtime port starts, observes,
//! briefs, sends to and releases.
//!
//! A Session is not a stored kind. It is the thing a Task owns once spawned:
//! the Task's `status.session` names it, and the Task's controller is the
//! only writer of what Fleet knows about it. A Session has:
//!
//! - an identity, [`SessionRef`]: the runtime's `id`, the `host` it runs on
//!   and Fleet's own `key`, deterministic per Task so a second spawn finds the
//!   first Session instead of twinning it;
//! - a lifecycle, [`SessionState`]: what the runtime last proved about it,
//!   folded from the runtime port's answers by [`SessionState::observe`].
//!
//! The runtime port ([`crate::runtime::Runtime`]) is the only way Fleet
//! reaches a Session. Fleet decides; the runtime runs the agent, holds its
//! window, workspace and conversation, and answers Known or Unknown. Unknown
//! never changes a Session's state: a state is a fact the runtime proved, and
//! a doubt keeps the last one.
//!
//! One Task owns at most one Session, for its whole life: the Session starts
//! with the Task's spawn and ends with the Task's release. A Session that the
//! runtime holds but Fleet did not spawn belongs to no Task and is never
//! adopted, because the key carries the Task's uid.

use serde::{Deserialize, Serialize};

use crate::Observation;
use crate::plan::Observations;
use crate::runtime::Activity;

/// The identity of a Session: the runtime's `id`, the `host` it runs on and
/// Fleet's `key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRef {
    pub id: String,
    pub host: String,
    pub key: String,
}

/// What the runtime last proved about a Session. Each state is one Known
/// answer of the runtime port; a view reads it, nothing decides on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// The runtime holds the Session and nothing proves its agent stopped,
    /// lost or idle.
    Running,
    /// The agent waits for input, with no background work to lose.
    Idle,
    /// The agent is stopped, by Fleet or by anyone else; the row, workspace
    /// and conversation are kept.
    Paused,
    /// The runtime holds the Session, but its agent's window is gone, as
    /// after a host reboot.
    Lost,
    /// A readable session list no longer shows the Session under its key.
    Gone,
}

impl SessionState {
    /// Fold the runtime's answers about `session` into a state, or `None`
    /// when nothing Known was answered. The strongest proof wins: gone over
    /// lost, lost over paused, paused over the agent's activity, and any
    /// proof of presence alone is `Running`. A session found under the key
    /// that is not `session` counts as gone, as it does for `WorkerGone`.
    pub fn observe(session: &SessionRef, o: &Observations) -> Option<SessionState> {
        let found = match &o.find {
            Some(Observation::Known(Some(found))) => Some(found == session),
            Some(Observation::Known(None)) => Some(false),
            _ => None,
        };
        if found == Some(false) {
            return Some(SessionState::Gone);
        }
        let known = |v: &Option<Observation<bool>>| match v {
            Some(Observation::Known(b)) => Some(*b),
            _ => None,
        };
        let (lost, paused) = (known(&o.lost), known(&o.paused));
        if lost == Some(true) {
            return Some(SessionState::Lost);
        }
        if paused == Some(true) {
            return Some(SessionState::Paused);
        }
        match o.activity {
            Some(Observation::Known(Activity::Idle)) => return Some(SessionState::Idle),
            Some(Observation::Known(Activity::Active)) => return Some(SessionState::Running),
            _ => {}
        }
        let present = found == Some(true) || lost == Some(false) || paused == Some(false);
        present.then_some(SessionState::Running)
    }
}

/// The session key for a Task: deterministic, so a second spawn finds the
/// first session instead of twinning it. The uid keeps a Task recreated under
/// the same name from adopting a session it never owned.
pub fn session_key(name: &str, uid: &str) -> String {
    format!("{name}.{uid}")
}

/// The uid part of a session key: `<name>.<uid>`, or a bare uid, which is
/// what an adapter whose keys must be short hands back from `list`.
pub fn uid_of_session_key(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionRef {
        SessionRef {
            id: "s1".into(),
            host: "local".into(),
            key: "t.u1".into(),
        }
    }

    fn known<T>(v: T) -> Option<Observation<T>> {
        Some(Observation::Known(v))
    }

    #[test]
    fn nothing_known_is_no_state() {
        assert_eq!(
            SessionState::observe(&session(), &Observations::default()),
            None
        );
        let o = Observations {
            find: Some(Observation::unknown("list unreadable")),
            lost: Some(Observation::unknown("timeout")),
            ..Default::default()
        };
        assert_eq!(SessionState::observe(&session(), &o), None);
    }

    #[test]
    fn a_readable_list_without_the_session_proves_it_gone() {
        let o = Observations {
            find: known(None),
            lost: known(false),
            activity: known(Activity::Active),
            ..Default::default()
        };
        assert_eq!(
            SessionState::observe(&session(), &o),
            Some(SessionState::Gone)
        );
        let other = SessionRef {
            id: "s2".into(),
            ..session()
        };
        let o = Observations {
            find: known(Some(other)),
            ..Default::default()
        };
        assert_eq!(
            SessionState::observe(&session(), &o),
            Some(SessionState::Gone)
        );
    }

    #[test]
    fn lost_beats_paused_beats_activity_beats_presence() {
        let s = session();
        let o = Observations {
            find: known(Some(s.clone())),
            lost: known(true),
            paused: known(true),
            activity: known(Activity::Idle),
            ..Default::default()
        };
        assert_eq!(SessionState::observe(&s, &o), Some(SessionState::Lost));
        let o = Observations {
            lost: known(false),
            ..o
        };
        assert_eq!(SessionState::observe(&s, &o), Some(SessionState::Paused));
        let o = Observations {
            paused: known(false),
            ..o
        };
        assert_eq!(SessionState::observe(&s, &o), Some(SessionState::Idle));
        let o = Observations {
            activity: known(Activity::Active),
            ..o
        };
        assert_eq!(SessionState::observe(&s, &o), Some(SessionState::Running));
        let o = Observations {
            activity: None,
            ..o
        };
        assert_eq!(SessionState::observe(&s, &o), Some(SessionState::Running));
    }

    #[test]
    fn presence_alone_is_running() {
        for o in [
            Observations {
                find: known(Some(session())),
                ..Default::default()
            },
            Observations {
                lost: known(false),
                ..Default::default()
            },
            Observations {
                paused: known(false),
                ..Default::default()
            },
        ] {
            assert_eq!(
                SessionState::observe(&session(), &o),
                Some(SessionState::Running),
                "{o:?}"
            );
        }
    }

    #[test]
    fn a_state_is_written_by_its_name() {
        assert_eq!(serde_json::to_value(SessionState::Lost).unwrap(), "Lost");
        assert_eq!(serde_json::to_value(SessionState::Gone).unwrap(), "Gone");
    }

    #[test]
    fn a_session_key_carries_the_uid() {
        let key = session_key("t", "u1");
        assert_eq!(key, "t.u1");
        assert_eq!(uid_of_session_key(&key), "u1");
        assert_eq!(uid_of_session_key("u1"), "u1");
    }
}
