//! Signals: what an agent's own hooks report about it, through
//! `fleet signal`.
//!
//! Fleet passes every session a file at start, as [`SIGNAL_FILE_ENV`]. The
//! harness profile ([`crate::harness`]) installs hooks that run
//! `fleet signal <state>`, and each run appends one [`Signal`] line to that
//! file. Fleet reads the file back through the [`Signals`] port and folds it
//! with [`Reported::fold`]: the agent's last state, every prompt it was
//! given (by digest), and the conversation it runs.
//!
//! ```text
//! agent hook ──fleet signal──▶ FLEET_SIGNAL_FILE ──Signals::read──▶ Reported
//!                                                                    │
//!                     status.sessionState, pause, delivery proof ◀───┘
//! ```
//!
//! A runtime knows nothing of it: the file is one more variable in `start`.
//! Unknown never concludes: a file Fleet cannot read is Unknown, and a file
//! the agent never wrote is a session that reported nothing yet.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::meta::Millis;
use crate::observation::Observation;
use crate::runtime::Activity;
use crate::session::SessionRef;

/// The variable the session's file is passed in.
pub const SIGNAL_FILE_ENV: &str = "FLEET_SIGNAL_FILE";

/// The longest line `fleet signal` writes; a longer one is refused.
pub const MAX_LINE: usize = 4096;

/// What a hook says the agent is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// The agent works on a turn.
    Working,
    /// The agent waits for a person to approve something: not idle.
    Blocked,
    /// The agent finished its turn and waits for input.
    Idle,
    /// The agent was given a prompt: a turn starts.
    Prompt,
    /// The runtime started the agent again, and the agent has reported
    /// nothing since: what the previous process reported says nothing of
    /// this one. Written by Fleet, never by a hook.
    Started,
}

impl State {
    pub fn parse(word: &str) -> Option<State> {
        match word {
            "working" => Some(State::Working),
            "blocked" => Some(State::Blocked),
            "idle" => Some(State::Idle),
            "prompt" => Some(State::Prompt),
            _ => None,
        }
    }
}

/// One line of the signal file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    pub at: Millis,
    pub state: State,
    /// The digests of the prompts this report proves the agent was given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<String>,
    /// The harness's own id of the conversation, for its resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
}

impl Signal {
    /// The signal a hook's `payload` (its JSON, when it passed one) makes
    /// for `state`: the prompts it names, by digest, and its conversation.
    /// Claude Code gives `prompt` and `session_id`; Codex's notify gives
    /// `input-messages` and `thread-id`.
    pub fn from_hook(at: Millis, state: State, payload: Option<&serde_json::Value>) -> Signal {
        let field = |names: &[&str]| {
            names
                .iter()
                .find_map(|n| payload.and_then(|p| p[*n].as_str()).map(str::to_string))
        };
        let mut prompts = Vec::new();
        if let Some(p) = payload {
            if let Some(text) = p["prompt"].as_str() {
                prompts.push(prompt_digest(text));
            }
            if let Some(list) = p["input-messages"].as_array() {
                prompts.extend(list.iter().filter_map(|m| m.as_str()).map(prompt_digest));
            }
        }
        // Claude Code's `Stop` lists the work its turn left running: a
        // background shell or subagent, a cron that wakes it later. A stop
        // would end that work, so the agent is not idle.
        let in_flight = |list: &str| {
            payload
                .and_then(|p| p[list].as_array())
                .is_some_and(|l| !l.is_empty())
        };
        let state = if state == State::Idle
            && (in_flight("background_tasks") || in_flight("session_crons"))
        {
            State::Working
        } else if state == State::Blocked
            && payload.and_then(|p| p["notification_type"].as_str()) == Some("idle_prompt")
        {
            // Claude Code's notification after 60 s of input idle: the agent
            // waits for its next prompt, which needs no one. A hook wired
            // without a matcher reports it as `blocked`.
            State::Idle
        } else {
            state
        };
        Signal {
            at,
            state,
            prompts,
            conversation: field(&["session_id", "thread-id"]).filter(|c| !c.is_empty()),
        }
    }
}

/// What a session's signal file says, folded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reported {
    /// The last state reported, when any was.
    pub state: Option<State>,
    /// Every prompt digest reported, with when it was last reported: a
    /// send is proven only by a report made after it.
    pub prompts: BTreeMap<String, Millis>,
    /// The last conversation id reported.
    pub conversation: Option<String>,
}

impl Reported {
    /// Fold the file's text. A line that does not parse, as one cut short
    /// by a crash, is skipped: it proves nothing.
    pub fn fold(text: &str) -> Reported {
        let mut r = Reported::default();
        for line in text.lines() {
            let Ok(s) = serde_json::from_str::<Signal>(line) else {
                continue;
            };
            r.state = Some(s.state);
            for digest in s.prompts {
                let at = r.prompts.entry(digest).or_default();
                *at = (*at).max(s.at);
            }
            if s.conversation.is_some() {
                r.conversation = s.conversation;
            }
        }
        r
    }

    /// What the agent is doing, when it reported anything.
    pub fn activity(&self) -> Option<Activity> {
        match self.state? {
            State::Idle => Some(Activity::Idle),
            State::Working | State::Blocked | State::Prompt => Some(Activity::Active),
            State::Started => None,
        }
    }

    /// Whether the agent reported anything since the runtime last started
    /// it.
    pub fn reported(&self) -> bool {
        self.state.is_some_and(|s| s != State::Started)
    }

    /// Whether the agent's prompt hook reported being given `text`.
    pub fn prompted(&self, text: &str) -> bool {
        self.prompts.contains_key(&prompt_digest(text))
    }

    /// Whether the agent's prompt hook reported the prompt `digest` at or
    /// after `since`, when the send it proves was intended.
    pub fn prompted_since(&self, digest: &str, since: Millis) -> bool {
        self.prompts.get(digest).is_some_and(|at| *at >= since)
    }
}

/// The digest a prompt is reported and matched by: FNV-1a over the text
/// with its line endings and surrounding whitespace normalised, as a
/// terminal may change both. Not a secret: it only matches a prompt Fleet
/// sent to the one session that reports it.
pub fn prompt_digest(text: &str) -> String {
    let normal = text.replace("\r\n", "\n");
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in normal.trim().bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The signal port: where a session's file is, and what it says.
pub trait Signals {
    /// The file to pass a session on `host` under `key`, as the session
    /// sees it; `None` when this port cannot read one back from that host.
    fn file(&self, host: &str, key: &str) -> Option<String>;
    /// The session's file, folded. A file never written is `Known` and
    /// empty; a file that cannot be read is Unknown.
    fn read(&self, session: &SessionRef) -> Observation<Reported>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_last_state_wins_and_every_prompt_is_kept() {
        let lines = [
            json!({"at": 1, "state": "prompt", "prompts": [prompt_digest("do it")], "conversation": "c1"}),
            json!({"at": 2, "state": "working"}),
            json!({"at": 3, "state": "idle"}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let r = Reported::fold(&format!("{text}{{\"at\": 4, \"sta"));
        assert_eq!(r.state, Some(State::Idle));
        assert_eq!(r.activity(), Some(Activity::Idle));
        assert!(r.prompted("do it\r\n"));
        assert!(!r.prompted("do something else"));
        assert_eq!(r.conversation.as_deref(), Some("c1"));
        assert_eq!(Reported::fold("").activity(), None);
    }

    #[test]
    fn a_hook_payload_names_its_prompts_and_conversation() {
        let claude = json!({"prompt": "brief", "session_id": "abc"});
        let s = Signal::from_hook(1, State::Prompt, Some(&claude));
        assert_eq!(s.prompts, vec![prompt_digest("brief")]);
        assert_eq!(s.conversation.as_deref(), Some("abc"));
        let codex = json!({"type": "agent-turn-complete", "thread-id": "t9",
                           "input-messages": ["one", "two"]});
        let s = Signal::from_hook(1, State::Idle, Some(&codex));
        assert_eq!(s.prompts.len(), 2);
        assert_eq!(s.conversation.as_deref(), Some("t9"));
        assert!(
            Signal::from_hook(1, State::Working, None)
                .prompts
                .is_empty()
        );
    }
}
