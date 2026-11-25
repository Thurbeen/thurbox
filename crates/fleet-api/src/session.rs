use serde::{Deserialize, Serialize};

use crate::Observation;
use crate::plan::Observations;
use crate::runtime::Activity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRef {
    pub id: String,
    pub host: String,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Running,
    Idle,
    Paused,
    Lost,
    Gone,
}

impl SessionState {
    pub const ALL: &[SessionState] = &[
        SessionState::Running,
        SessionState::Idle,
        SessionState::Paused,
        SessionState::Lost,
        SessionState::Gone,
    ];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Working,
    Attention,
    Idle,
    Unknown,
}

impl AgentState {
    pub fn reported(r: &crate::signal::Reported) -> AgentState {
        use crate::signal::State;
        match r.state {
            Some(State::Working | State::Prompt) => AgentState::Working,
            Some(State::Blocked) => AgentState::Attention,
            Some(State::Idle) => AgentState::Idle,
            Some(State::Started) | None => AgentState::Unknown,
        }
    }
}

pub fn session_key(name: &str, uid: &str) -> String {
    format!("{name}.{uid}")
}

pub fn uid_of_session_key(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_session_state_is_in_all() {
        let listed = |s: SessionState| match s {
            SessionState::Running
            | SessionState::Idle
            | SessionState::Paused
            | SessionState::Lost
            | SessionState::Gone => SessionState::ALL.contains(&s),
        };
        assert!(SessionState::ALL.iter().copied().all(listed));
        assert_eq!(SessionState::ALL.len(), 5);
    }

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
