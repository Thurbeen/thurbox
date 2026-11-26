use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::harness::Payload;
use crate::meta::Millis;
use crate::observation::Observation;
use crate::runtime::Activity;
use crate::session::SessionRef;

pub const SIGNAL_FILE_ENV: &str = "FLEET_SIGNAL_FILE";

pub const MAX_LINE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Working,
    Blocked,
    Idle,
    Prompt,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    pub at: Millis,
    pub state: State,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox: Option<String>,
}

impl Signal {
    pub fn from_hook(at: Millis, state: State, payload: Option<&serde_json::Value>) -> Signal {
        Signal::from_hook_with(at, state, payload, &Payload::default())
    }

    pub fn from_hook_with(
        at: Millis,
        state: State,
        payload: Option<&serde_json::Value>,
        fields: &Payload,
    ) -> Signal {
        let mut prompts = Vec::new();
        let mut report = |text: &str| {
            prompts.push(prompt_digest(text));
            let Some(tag) = fields.paste.as_deref() else {
                return;
            };
            if let Some(inside) = inside_tag(text, tag) {
                prompts.push(prompt_digest(inside));
            }
            if has_tag(text, tag) {
                prompts.push(loose_prompt_digest(text, tag));
            }
        };
        if let Some(p) = payload {
            for name in &fields.prompt {
                match &p[name] {
                    serde_json::Value::String(text) => report(text),
                    // Codex's `input-messages` are strings; Kimi's prompt is a list of `{"text": ...}` parts.
                    serde_json::Value::Array(list) => {
                        list.iter()
                            .filter_map(|m| m.as_str().or_else(|| m["text"].as_str()))
                            .for_each(&mut report);
                    }
                    _ => {}
                }
            }
        }
        let conversation = fields
            .conversation
            .iter()
            .find_map(|n| payload.and_then(|p| p[n].as_str()).map(str::to_string));
        // Claude Code's `Stop` lists the background work its turn left running.
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
            // Claude Code notifies after 60 s of input idle, which needs no one.
            State::Idle
        } else {
            state
        };
        Signal {
            at,
            state,
            prompts,
            conversation: conversation.filter(|c| !c.is_empty()),
            seq: None,
            inbox: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reported {
    pub state: Option<State>,
    pub prompts: BTreeMap<String, Millis>,
    pub prompt_seqs: BTreeMap<String, u64>,
    pub conversation: Option<String>,
    pub inbox: Option<String>,
}

impl Reported {
    pub fn fold(text: &str) -> Reported {
        let mut r = Reported::default();
        for line in text.lines() {
            let Ok(s) = serde_json::from_str::<Signal>(line) else {
                continue;
            };
            r.state = Some(s.state);
            for digest in s.prompts {
                if let Some(seq) = s.seq {
                    let latest = r.prompt_seqs.entry(digest.clone()).or_default();
                    *latest = (*latest).max(seq);
                }
                let at = r.prompts.entry(digest).or_default();
                *at = (*at).max(s.at);
            }
            if s.conversation.is_some() {
                r.conversation = s.conversation;
            }
            if s.state == State::Started || s.inbox.is_some() {
                r.inbox = s.inbox;
            }
        }
        r
    }

    pub fn fold_restarted(before: &str, after: &str) -> Reported {
        let mut r = Reported::fold(&format!("{before}\n{after}"));
        let since = Reported::fold(after);
        r.state = Some(since.state.unwrap_or(State::Started));
        r.inbox = since.inbox;
        r
    }

    pub fn activity(&self) -> Option<Activity> {
        match self.state? {
            State::Idle => Some(Activity::Idle),
            State::Working | State::Blocked | State::Prompt => Some(Activity::Active),
            State::Started => None,
        }
    }

    pub fn reported(&self) -> bool {
        self.state.is_some_and(|s| s != State::Started)
    }

    pub fn prompted(&self, text: &str) -> bool {
        self.prompts.contains_key(&prompt_digest(text))
    }

    pub fn prompted_since(&self, digest: &str, since: Millis) -> bool {
        self.prompts.get(digest).is_some_and(|at| *at >= since)
    }
}

// A terminal may change a prompt's line endings and surrounding whitespace.
pub fn prompt_digest(text: &str) -> String {
    let normal = text.replace("\r\n", "\n");
    fnv(0xcbf2_9ce4_8422_2325, normal.trim().bytes())
}

// Claude Code 2.1.327 wraps only the tail of a long paste and may change blank lines at the split.
pub fn loose_prompt_digest(text: &str, tag: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, len)) = next_tag(rest, tag) {
        kept.push_str(&rest[..at]);
        rest = &rest[at + len..];
    }
    kept.push_str(rest);
    fnv(
        0x6c6f_6f73_6570_726f,
        kept.bytes().filter(|b| !b.is_ascii_whitespace()),
    )
}

fn fnv(basis: u64, bytes: impl Iterator<Item = u8>) -> String {
    let mut hash = basis;
    for b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn has_tag(text: &str, tag: &str) -> bool {
    next_tag(text, tag).is_some()
}

fn next_tag(text: &str, tag: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(lt) = text[from..].find('<').map(|i| from + i) {
        let after = &text[lt + 1..];
        let name = after.strip_prefix('/').unwrap_or(after);
        if let Some(r) = name.strip_prefix(tag).filter(|r| r.starts_with([' ', '>']))
            && let Some(gt) = r.find('>')
        {
            let end = text.len() - r.len() + gt + 1;
            return Some((lt, end - lt));
        }
        from = lt + 1;
    }
    None
}

fn inside_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let named = |t: &'a str| t.strip_prefix(tag).filter(|r| r.starts_with([' ', '>']));
    let open = named(text.trim().strip_prefix('<')?)?;
    let body = &open[open.find('>')? + 1..];
    let close = body.rfind("</")?;
    let end = named(&body[close + 2..])?;
    (end.find('>')? == end.len() - 1).then(|| &body[..close])
}

pub trait Signals {
    fn file(&self, host: &str, key: &str) -> Option<String>;
    fn read(&self, session: &SessionRef) -> Observation<Reported>;
    fn written(&self, _session: &SessionRef) -> Option<u64> {
        None
    }
    fn outbox(
        &self,
        _session: &SessionRef,
        _from: u64,
    ) -> Observation<Option<crate::event::OutboxRead>> {
        Observation::unknown("this signal port reads no outbox")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_restart_drops_the_inbox_the_previous_process_reported() {
        let before = format!(
            "{}\n",
            json!({"at": 1, "state": "idle", "inbox": "/old.sock"})
        );
        assert_eq!(Reported::fold(&before).inbox.as_deref(), Some("/old.sock"));
        assert_eq!(Reported::fold_restarted(&before, "").inbox, None);
        let marked = format!("{before}{}\n", json!({"at": 2, "state": "started"}));
        assert_eq!(Reported::fold(&marked).inbox, None);
        let after = format!(
            "{}\n",
            json!({"at": 3, "state": "idle", "inbox": "/new.sock"})
        );
        assert_eq!(
            Reported::fold_restarted(&before, &after).inbox.as_deref(),
            Some("/new.sock")
        );
    }

    #[test]
    fn a_restart_hides_the_states_reported_before_it() {
        let before = format!(
            "{}\n",
            json!({"at": 900, "state": "idle", "conversation": "c1"})
        );
        let r = Reported::fold_restarted(&before, "");
        assert_eq!(r.state, Some(State::Started));
        assert!(!r.reported());
        assert_eq!(r.conversation.as_deref(), Some("c1"));
        let after = format!("{}\n", json!({"at": 1, "state": "working"}));
        let r = Reported::fold_restarted(&before, &after);
        assert_eq!(r.state, Some(State::Working));
        assert_eq!(r.conversation.as_deref(), Some("c1"));
    }

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
        let kimi = json!({"prompt": [{"type": "text", "text": "brief"}, {"type": "image"}],
                          "session_id": "session_1"});
        let s = Signal::from_hook(1, State::Prompt, Some(&kimi));
        assert_eq!(s.prompts, vec![prompt_digest("brief")]);
        assert!(
            Signal::from_hook(1, State::Working, None)
                .prompts
                .is_empty()
        );
    }

    #[test]
    fn a_profiles_payload_fields_name_its_prompts_and_conversation() {
        let fields = Payload {
            prompt: vec!["user_message".into()],
            conversation: vec!["sessionID".into()],
            paste: None,
        };
        let hook = json!({"user_message": "brief", "sessionID": "o1", "session_id": "no"});
        let s = Signal::from_hook_with(1, State::Prompt, Some(&hook), &fields);
        assert_eq!(s.prompts, vec![prompt_digest("brief")]);
        assert_eq!(s.conversation.as_deref(), Some("o1"));
        let claude = json!({"prompt": "brief", "session_id": "abc"});
        let s = Signal::from_hook_with(1, State::Prompt, Some(&claude), &fields);
        assert!(s.prompts.is_empty() && s.conversation.is_none());
    }

    #[test]
    fn a_wrapped_paste_is_also_reported_as_the_text_inside_it() {
        let fields = Payload {
            paste: Some("pasted_content".into()),
            ..Payload::default()
        };
        let brief = "Find it.\n\nFix it.";
        let wrapped =
            format!("\n\n<pasted_content id=\"ab12\">\n{brief}\n</pasted_content id=\"ab12\">\n");
        let claude = json!({"prompt": wrapped, "session_id": "abc"});
        let s = Signal::from_hook_with(1, State::Prompt, Some(&claude), &fields);
        assert_eq!(
            s.prompts,
            vec![
                prompt_digest(&wrapped),
                prompt_digest(brief),
                loose_prompt_digest(brief, "pasted_content"),
            ]
        );
        let typed = json!({"prompt": "a <pasted_contents> b", "session_id": "abc"});
        let s = Signal::from_hook_with(1, State::Prompt, Some(&typed), &fields);
        assert_eq!(s.prompts, vec![prompt_digest("a <pasted_contents> b")]);
        let s = Signal::from_hook(1, State::Prompt, Some(&claude));
        assert_eq!(s.prompts, vec![prompt_digest(&wrapped)]);
    }

    #[test]
    fn a_tag_inside_the_paste_is_part_of_the_text() {
        let fields = Payload {
            paste: Some("pasted_content".into()),
            ..Payload::default()
        };
        let brief = "Why does <pasted_content id=\"x\">a</pasted_content id=\"x\"> break?";
        let wrapped =
            format!("<pasted_content id=\"ab12\">\n{brief}\n</pasted_content id=\"ab12\">");
        let claude = json!({"prompt": wrapped, "session_id": "abc"});
        let s = Signal::from_hook_with(1, State::Prompt, Some(&claude), &fields);
        assert!(s.prompts.contains(&prompt_digest(brief)), "{:?}", s.prompts);
    }

    #[test]
    fn a_brief_split_around_a_paste_is_reported_loosely() {
        let fields = Payload {
            paste: Some("pasted_content".into()),
            ..Payload::default()
        };
        let brief = "Steps:\n- **A lookup in production**\n- **A fix**";
        let split = "Steps:\n-\n\n<pasted_content id=\"df13\">\n**A lookup in production**\n- **A fix**\n</pasted_content id=\"df13\">\n";
        let claude = json!({"prompt": split, "session_id": "abc"});
        let s = Signal::from_hook_with(1, State::Prompt, Some(&claude), &fields);
        assert!(
            !s.prompts.contains(&prompt_digest(brief)),
            "{:?}",
            s.prompts
        );
        assert!(
            s.prompts
                .contains(&loose_prompt_digest(brief, "pasted_content")),
            "{:?}",
            s.prompts
        );
    }

    #[test]
    fn a_loose_digest_drops_only_the_profiles_wrappers_and_whitespace() {
        let tag = "pasted_content";
        let loose = |t: &str| loose_prompt_digest(t, tag);
        assert_eq!(loose("a b\n\tc"), loose("abc"));
        assert_eq!(
            loose("a<pasted_content id=\"1\">b</pasted_content id=\"1\">"),
            loose("ab")
        );
        assert_eq!(loose("a<pasted_content>b</pasted_content>"), loose("ab"));
        assert_ne!(loose("a<pasted_contents>b"), loose("ab"));
        assert_ne!(loose("a<other>b"), loose("ab"));
        assert_ne!(
            loose("a<pasted_content b"),
            loose("ab"),
            "an open tag never closed"
        );
        assert_ne!(loose("ab"), prompt_digest("ab"));
    }

    #[test]
    fn a_prompt_with_no_wrapper_is_not_reported_loosely() {
        let fields = Payload {
            paste: Some("pasted_content".into()),
            ..Payload::default()
        };
        let typed = json!({"prompt": "first\nline", "session_id": "abc"});
        let s = Signal::from_hook_with(1, State::Prompt, Some(&typed), &fields);
        assert_eq!(s.prompts, vec![prompt_digest("first\nline")]);
    }
}
