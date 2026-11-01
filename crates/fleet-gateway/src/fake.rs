//! A fake chat platform that keeps one account's threads in one JSON file,
//! so the CLI under test and the test itself see the same platform across
//! processes. It runs no process and reaches no network.
//!
//! The fake models what differs between real platforms, through the
//! `capabilities` a test sets:
//!
//! - **history** on or off: off answers `Unsupported`, as Telegram would.
//! - **marker** `metadata`, `nonce { windowMs }` or `none`: with a nonce,
//!   a repeated key inside the window returns the existing post; with
//!   metadata or nothing, a repeat makes a second post, as Slack would, and
//!   only a metadata marker is readable back from history.
//! - **unmentioned** and **threads**: reported as set; the fake delivers
//!   whatever a test feeds its `event`, so a suite models a mentions-only
//!   platform by feeding only mentions.
//!
//! Its addresses are `fake:<chat>[:<thread>]`. A payload for `event` is
//! `{"id": "<delivery id>", "event": {…}}` in the port's own event shape;
//! a payload without `event` is one the Gateway ignores. Faults are queued
//! per call and consumed in order, as on the fake forge: `lost`, `timeout`,
//! `throttle`, `incomplete` and `malformed` for reads; `failed`, `lost` and
//! `lost-applied` for `open` and `post`.
//!
//! The platform's clock is the state's `clock`, which a test sets: every
//! post takes the clock as its `at` and advances it one second, so `at`
//! orders posts the way a platform's own timestamps would.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fleet_api::gateway::{
    AccountConfig, Author, AuthorKind, Capabilities, ConversationRef, Delivery, Envelope, Event,
    Gateway, History, HistoryObservation, HistoryQuery, Identity, Marker, Message, Starter,
    Uncertainty,
};
use fleet_api::plan::EffectOutcome;
use fleet_api::{Millis, Observation};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const KIND: &str = "fake";

/// What the fake's bot is called when a test sets nothing.
pub const DEFAULT_BOT: &str = "B0FLEET001";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayFault {
    /// `parse`, `event`, `whoami`, `capabilities`, `history`, `open` or
    /// `post`.
    pub call: String,
    /// For reads: `lost`, `timeout`, `throttle`, `incomplete` or
    /// `malformed`. For `open` and `post`: `failed`, `lost` or
    /// `lost-applied`.
    pub mode: String,
    /// For `throttle`: what the platform says to wait, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<Millis>,
    /// How many calls this fault applies to.
    #[serde(default = "one")]
    pub times: u32,
}

fn one() -> u32 {
    1
}

impl GatewayFault {
    pub fn new(call: &str, mode: &str) -> GatewayFault {
        GatewayFault {
            call: call.into(),
            mode: mode.into(),
            retry_after_ms: None,
            times: 1,
        }
    }

    pub fn retry_after(mut self, ms: Millis) -> GatewayFault {
        self.retry_after_ms = Some(ms);
        self
    }

    pub fn times(mut self, n: u32) -> GatewayFault {
        self.times = n;
        self
    }
}

/// One thread, or one chat without threads, on the fake platform.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct FakeConversation {
    /// In the order they were added; `history` sorts by `at`.
    pub messages: Vec<Message>,
    /// The platform reports the thread's first message deleted.
    pub starter_removed: bool,
}

/// One post Fleet made, in order: what a test checks posting against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PostRecord {
    /// The key the post carries as its marker: the intent key, or a derived
    /// `<key>/<n>` of a split text.
    pub key: String,
    pub conversation: ConversationRef,
    pub message: String,
    pub at: Millis,
    pub text: String,
}

fn default_capabilities() -> Capabilities {
    Capabilities {
        history: true,
        marker: Marker::Metadata,
        unmentioned: true,
        threads: true,
    }
}

fn default_identity() -> Identity {
    Identity {
        id: DEFAULT_BOT.into(),
        name: Some("fleet".into()),
    }
}

fn default_clock() -> Millis {
    1_730_000_000_000
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct FakeGatewayState {
    pub capabilities: Capabilities,
    /// What `whoami` answers.
    pub identity: Identity,
    /// The platform's clock, in Unix milliseconds: the `at` of the next
    /// post.
    pub clock: Millis,
    /// The longest text one post takes; a longer one is split into
    /// consecutive posts under derived keys. `None`: never split.
    pub post_limit: Option<usize>,
    /// Threads by address.
    pub conversations: BTreeMap<String, FakeConversation>,
    pub posts: Vec<PostRecord>,
    pub faults: Vec<GatewayFault>,
    pub calls: BTreeMap<String, u64>,
}

impl Default for FakeGatewayState {
    fn default() -> Self {
        FakeGatewayState {
            capabilities: default_capabilities(),
            identity: default_identity(),
            clock: default_clock(),
            post_limit: None,
            conversations: BTreeMap::new(),
            posts: Vec::new(),
            faults: Vec::new(),
            calls: BTreeMap::new(),
        }
    }
}

/// One fake account's directory, shared by every process that opens it.
#[derive(Debug, Clone)]
pub struct FakeGateway {
    account: String,
    dir: PathBuf,
}

impl FakeGateway {
    pub fn open(account: &str, dir: &Path) -> io::Result<FakeGateway> {
        fs::create_dir_all(dir)?;
        Ok(FakeGateway {
            account: account.to_string(),
            dir: dir.to_path_buf(),
        })
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    /// Read-modify-write under the platform's lock.
    pub fn update<T>(&self, f: impl FnOnce(&mut FakeGatewayState) -> T) -> io::Result<T> {
        let _lock = Lock::acquire(&self.dir.join("lock"))?;
        let mut state = self.read_unlocked()?;
        let out = f(&mut state);
        self.write_unlocked(&state)?;
        Ok(out)
    }

    pub fn state(&self) -> io::Result<FakeGatewayState> {
        let _lock = Lock::acquire(&self.dir.join("lock"))?;
        self.read_unlocked()
    }

    fn read_unlocked(&self) -> io::Result<FakeGatewayState> {
        match fs::read(self.state_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(FakeGatewayState::default()),
            Err(e) => Err(e),
        }
    }

    fn write_unlocked(&self, state: &FakeGatewayState) -> io::Result<()> {
        let tmp = self.dir.join(format!("state.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, self.state_path())
    }

    pub fn add_fault(&self, fault: GatewayFault) -> io::Result<()> {
        self.update(|s| s.faults.push(fault))
    }

    pub fn set_capabilities(&self, capabilities: Capabilities) -> io::Result<()> {
        self.update(|s| s.capabilities = capabilities)
    }

    pub fn set_identity(&self, identity: Identity) -> io::Result<()> {
        self.update(|s| s.identity = identity)
    }

    /// Move the platform's clock, for the `at` of the next post.
    pub fn set_clock(&self, at: Millis) -> io::Result<()> {
        self.update(|s| s.clock = at)
    }

    pub fn set_post_limit(&self, limit: Option<usize>) -> io::Result<()> {
        self.update(|s| s.post_limit = limit)
    }

    /// Put messages on a thread, creating it, as if people wrote them.
    pub fn seed(&self, conversation: &ConversationRef, messages: Vec<Message>) -> io::Result<()> {
        self.update(|s| {
            s.conversations
                .entry(conversation.address())
                .or_default()
                .messages
                .extend(messages);
        })
    }

    /// Remove one message from a thread, as a person deleting it would.
    pub fn remove_message(&self, conversation: &ConversationRef, id: &str) -> io::Result<()> {
        self.update(|s| {
            if let Some(c) = s.conversations.get_mut(&conversation.address()) {
                c.messages.retain(|m| m.id != id);
            }
        })
    }

    /// Mark the thread's first message deleted, as the platform reports it.
    pub fn remove_starter(&self, conversation: &ConversationRef) -> io::Result<()> {
        self.update(|s| {
            s.conversations
                .entry(conversation.address())
                .or_default()
                .starter_removed = true;
        })
    }

    pub fn conversation(
        &self,
        conversation: &ConversationRef,
    ) -> io::Result<Option<FakeConversation>> {
        Ok(self
            .state()?
            .conversations
            .get(&conversation.address())
            .cloned())
    }

    /// Every post Fleet made, in order.
    pub fn posts(&self) -> io::Result<Vec<PostRecord>> {
        Ok(self.state()?.posts)
    }

    pub fn calls(&self, call: &str) -> io::Result<u64> {
        Ok(self.state()?.calls.get(call).copied().unwrap_or(0))
    }

    /// Count the call and take its fault, if one is queued.
    fn begin(&self, call: &str) -> Result<Option<GatewayFault>, String> {
        self.update(|s| {
            *s.calls.entry(call.to_string()).or_default() += 1;
            let i = s.faults.iter().position(|f| f.call == call)?;
            let f = s.faults[i].clone();
            if f.times <= 1 {
                s.faults.remove(i);
            } else {
                s.faults[i].times -= 1;
            }
            Some(f)
        })
        .map_err(|e| format!("fake gateway unreadable: {e}"))
    }

    fn other_account(&self, account: &str) -> Option<String> {
        (account != self.account).then(|| {
            format!(
                "account {account} is not this fake gateway, which serves {}",
                self.account
            )
        })
    }

    /// A read fault, in the history vocabulary.
    fn read_fault(f: &GatewayFault) -> HistoryObservation {
        match f.mode.as_str() {
            "lost" => HistoryObservation::unknown(Uncertainty::Refused, "injected: answer lost"),
            "timeout" => {
                HistoryObservation::unknown(Uncertainty::Timeout, "injected: the call timed out")
            }
            "throttle" => HistoryObservation::Unknown {
                kind: Uncertainty::Throttled,
                why: "injected: the platform asked to slow down".into(),
                retry_after: f.retry_after_ms,
            },
            "incomplete" => HistoryObservation::unknown(
                Uncertainty::Incomplete,
                "injected: the bound ran out before every page was read",
            ),
            "malformed" => HistoryObservation::unknown(
                Uncertainty::Unreadable,
                "injected: the answer could not be parsed",
            ),
            other => HistoryObservation::unknown(
                Uncertainty::Unreadable,
                format!("injected: unknown fault mode {other}"),
            ),
        }
    }

    /// Append Fleet's post(s) of `text` under `key` to the thread at
    /// `address`, splitting past `post_limit`, and return the first
    /// message's id and time.
    fn land(
        s: &mut FakeGatewayState,
        conversation: &ConversationRef,
        text: &str,
        key: &str,
        starts_thread: bool,
    ) -> (String, Millis) {
        let chunks: Vec<String> = match s.post_limit {
            Some(limit) if limit > 0 && text.chars().count() > limit => text
                .chars()
                .collect::<Vec<_>>()
                .chunks(limit)
                .map(|c| c.iter().collect())
                .collect(),
            _ => vec![text.to_string()],
        };
        let derived = chunks.len() > 1;
        let mut first: Option<(String, Millis)> = None;
        for (i, chunk) in chunks.into_iter().enumerate() {
            let at = s.clock;
            s.clock += 1000;
            let id = at.to_string();
            let key = if derived {
                format!("{key}/{}", i + 1)
            } else {
                key.to_string()
            };
            let message = Message {
                id: id.clone(),
                author: Author {
                    id: s.identity.id.clone(),
                    name: s.identity.name.clone().unwrap_or_default(),
                    kind: AuthorKind::Fleet,
                },
                mentions_fleet: false,
                at,
                text: chunk.clone(),
                // Only a metadata marker is readable back from history.
                key: (s.capabilities.marker == Marker::Metadata).then(|| key.clone()),
                starts_thread: starts_thread && i == 0,
            };
            s.conversations
                .entry(conversation.address())
                .or_default()
                .messages
                .push(message);
            s.posts.push(PostRecord {
                key,
                conversation: conversation.clone(),
                message: id.clone(),
                at,
                text: chunk,
            });
            first.get_or_insert((id, at));
        }
        first.expect("at least one chunk")
    }

    /// With a nonce marker, the post the platform already holds for a key
    /// inside the window, if any.
    fn existing_within_window<'a>(s: &'a FakeGatewayState, key: &str) -> Option<&'a PostRecord> {
        let Marker::Nonce { window_ms } = s.capabilities.marker else {
            return None;
        };
        s.posts
            .iter()
            .rev()
            .find(|p| p.key == key && s.clock.saturating_sub(p.at) <= window_ms)
    }

    /// Perform a write under its fault, with `land` doing the mutation when
    /// one is due. `apply` receives the state and answers the outcome.
    fn write(
        &self,
        call: &str,
        account: &str,
        apply: impl FnOnce(&mut FakeGatewayState) -> EffectOutcome,
    ) -> Observation<EffectOutcome> {
        if let Some(why) = self.other_account(account) {
            return Observation::Known(EffectOutcome::Failed { why });
        }
        let fault = match self.begin(call) {
            Ok(f) => f,
            Err(why) => return Observation::unknown(why),
        };
        match fault.as_ref().map(|f| f.mode.as_str()) {
            Some("lost") => {
                return Observation::unknown("injected: the answer was lost; nothing was posted");
            }
            Some("failed") => {
                return Observation::Known(EffectOutcome::Failed {
                    why: format!("injected: the platform refused the {call}"),
                });
            }
            Some("lost-applied") => {
                return match self.update(apply) {
                    Ok(_) => {
                        Observation::unknown("injected: the answer was lost after the post landed")
                    }
                    Err(e) => Observation::unknown(format!("fake gateway unreadable: {e}")),
                };
            }
            Some(other) => {
                return Observation::Known(EffectOutcome::Failed {
                    why: format!("injected: unknown fault mode {other}"),
                });
            }
            None => {}
        }
        match self.update(apply) {
            Ok(outcome) => Observation::Known(outcome),
            Err(e) => Observation::unknown(format!("fake gateway unreadable: {e}")),
        }
    }
}

struct Lock(File);

impl Lock {
    fn acquire(path: &Path) -> io::Result<Lock> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        f.lock()?;
        Ok(Lock(f))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The fake's address: `fake:<chat>[:<thread>]`, each part one token.
fn parse_address(address: &str) -> Result<ConversationRef, String> {
    let parsed = ConversationRef::split(address)
        .ok_or_else(|| format!("{address:?} is not <platform>:<chat>[:<thread>]"))?;
    if parsed.platform != KIND {
        return Err(format!(
            "{address:?} names platform {:?}, not {KIND}",
            parsed.platform
        ));
    }
    let token = |s: &str| !s.is_empty() && !s.contains(char::is_whitespace) && !s.contains(':');
    if !token(&parsed.chat) || parsed.thread.as_deref().is_some_and(|t| !token(t)) {
        return Err(format!("{address:?}: a chat or thread id is not one token"));
    }
    Ok(parsed)
}

impl Gateway for FakeGateway {
    fn operation_bound(&self) -> Millis {
        0
    }

    fn parse(&self, account: &str, address: &str) -> Observation<ConversationRef> {
        if let Some(why) = self.other_account(account) {
            return Observation::unknown(why);
        }
        match self.begin("parse") {
            Ok(Some(_)) => return Observation::unknown("injected: the address could not be read"),
            Ok(None) => {}
            Err(why) => return Observation::unknown(why),
        }
        match parse_address(address) {
            Ok(r) => Observation::Known(r),
            Err(why) => Observation::unknown(why),
        }
    }

    /// The payload is `{"id", "event"}` in the port's shape, or something
    /// the Gateway ignores. The account's `bot` tells Fleet's own posts and
    /// mentions apart, whatever the payload claimed.
    fn event(&self, account: &AccountConfig, envelope: &Envelope) -> Observation<Option<Delivery>> {
        if account.platform != KIND {
            return Observation::unknown(format!(
                "the account runs on {:?}, not {KIND}",
                account.platform
            ));
        }
        match self.begin("event") {
            Ok(Some(_)) => return Observation::unknown("injected: the payload could not be read"),
            Ok(None) => {}
            Err(why) => return Observation::unknown(why),
        }
        let Some(payload) = envelope.payload.as_object() else {
            return Observation::unknown("the payload is not an object");
        };
        let Some(event) = payload.get("event") else {
            // A reaction, a presence change: nothing the Gateway reads.
            return Observation::Known(None);
        };
        let Some(id) = payload.get("id").and_then(Value::as_str) else {
            return Observation::unknown("the payload has no delivery id");
        };
        let mut event: Event = match serde_json::from_value(event.clone()) {
            Ok(e) => e,
            Err(e) => return Observation::unknown(format!("the event could not be read: {e}")),
        };
        if event.conversation().platform != KIND {
            return Observation::unknown("the event names another platform");
        }
        if let Event::Message { message, .. } = &mut event {
            if message.author.id == account.bot {
                message.author.kind = AuthorKind::Fleet;
            }
            if message.text.contains(&format!("<@{}>", account.bot)) {
                message.mentions_fleet = true;
            }
        }
        Observation::Known(Some(Delivery {
            id: id.to_string(),
            verified: envelope.verified,
            received_at: envelope.received_at,
            event,
        }))
    }

    fn whoami(&self, account: &str) -> Observation<Identity> {
        if let Some(why) = self.other_account(account) {
            return Observation::unknown(why);
        }
        match self.begin("whoami") {
            Ok(Some(_)) => Observation::unknown("injected: whoami could not tell"),
            Ok(None) => match self.state() {
                Ok(s) => Observation::Known(s.identity),
                Err(e) => Observation::unknown(e.to_string()),
            },
            Err(why) => Observation::unknown(why),
        }
    }

    fn capabilities(&self, account: &str) -> Observation<Capabilities> {
        if let Some(why) = self.other_account(account) {
            return Observation::unknown(why);
        }
        match self.begin("capabilities") {
            Ok(Some(_)) => Observation::unknown("injected: capabilities could not be read"),
            Ok(None) => match self.state() {
                Ok(s) => Observation::Known(s.capabilities),
                Err(e) => Observation::unknown(e.to_string()),
            },
            Err(why) => Observation::unknown(why),
        }
    }

    fn history(
        &self,
        account: &str,
        conversation: &ConversationRef,
        query: &HistoryQuery,
    ) -> HistoryObservation {
        if let Some(why) = self.other_account(account) {
            return HistoryObservation::unknown(Uncertainty::Unavailable, why);
        }
        let fault = match self.begin("history") {
            Ok(f) => f,
            Err(why) => return HistoryObservation::unknown(Uncertainty::Unreadable, why),
        };
        let s = match self.state() {
            Ok(s) => s,
            Err(e) => {
                return HistoryObservation::unknown(
                    Uncertainty::Unreadable,
                    format!("fake gateway unreadable: {e}"),
                );
            }
        };
        if !s.capabilities.history {
            return HistoryObservation::Unsupported;
        }
        if let Some(f) = fault {
            return Self::read_fault(&f);
        }
        let Some(c) = s.conversations.get(&conversation.address()) else {
            return HistoryObservation::unknown(
                Uncertainty::Refused,
                format!("no such conversation {}", conversation.address()),
            );
        };
        let mut all = c.messages.clone();
        all.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        let starter = conversation.thread.as_ref().and_then(|_| {
            if c.starter_removed {
                Some(Starter::removed())
            } else {
                all.first().map(Starter::message)
            }
        });
        let (messages, left_out) = match query {
            HistoryQuery::After { id, limit } => {
                let after = id
                    .as_ref()
                    .and_then(|id| all.iter().find(|m| m.id == *id).map(|m| m.at));
                let mut newer: Vec<Message> = match after {
                    Some(at) => all.into_iter().filter(|m| m.at > at).collect(),
                    None => all,
                };
                let limit = *limit as usize;
                let left_out = newer.len().saturating_sub(limit);
                if left_out > 0 {
                    newer.drain(..left_out);
                }
                (newer, left_out as u64)
            }
            HistoryQuery::Since { at } => (all.into_iter().filter(|m| m.at >= *at).collect(), 0),
        };
        HistoryObservation::Known(History {
            messages,
            left_out,
            starter,
        })
    }

    fn open(
        &self,
        account: &str,
        channel: &ConversationRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        if channel.thread.is_some() {
            return Observation::Known(EffectOutcome::Failed {
                why: format!("{} is a thread, not a channel", channel.address()),
            });
        }
        let channel = channel.clone();
        let text = text.to_string();
        let key = intent_key.to_string();
        self.write("open", account, move |s| {
            if let Some(p) = Self::existing_within_window(s, &key) {
                return EffectOutcome::Opened {
                    conversation: p.conversation.clone(),
                    message: p.message.clone(),
                    at: p.at,
                };
            }
            let at = s.clock;
            let thread = ConversationRef {
                platform: channel.platform.clone(),
                chat: channel.chat.clone(),
                thread: Some(at.to_string()),
            };
            let (message, at) = Self::land(s, &thread, &text, &key, true);
            EffectOutcome::Opened {
                conversation: thread,
                message,
                at,
            }
        })
    }

    fn post(
        &self,
        account: &str,
        conversation: &ConversationRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        let conversation = conversation.clone();
        let text = text.to_string();
        let key = intent_key.to_string();
        let exists = match self.state() {
            Ok(s) => s.conversations.contains_key(&conversation.address()),
            Err(e) => return Observation::unknown(format!("fake gateway unreadable: {e}")),
        };
        if !exists {
            return Observation::Known(EffectOutcome::Failed {
                why: format!("no such conversation {}", conversation.address()),
            });
        }
        self.write("post", account, move |s| {
            if let Some(p) = Self::existing_within_window(s, &key) {
                return EffectOutcome::Posted {
                    message: p.message.clone(),
                    at: p.at,
                };
            }
            let (message, at) = Self::land(s, &conversation, &text, &key, false);
            EffectOutcome::Posted { message, at }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_api::gateway::{Inbound, Verified};
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn fake(name: &str) -> (FakeGateway, PathBuf) {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        // Unit tests have no CARGO_TARGET_TMPDIR: use the workspace's own
        // target directory, never the shared system temp.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/fake-gateway")
            .join(format!("{name}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        (FakeGateway::open("fake-main", &dir).unwrap(), dir)
    }

    fn account() -> AccountConfig {
        AccountConfig {
            platform: KIND.into(),
            team: None,
            bot: DEFAULT_BOT.into(),
            inbound: Inbound::Connection,
            message_content: None,
            cli: None,
            curl_config: None,
            dir: None,
        }
    }

    fn person(id: &str, at: Millis, text: &str) -> Message {
        Message {
            id: at.to_string(),
            author: Author {
                id: id.into(),
                name: "Alex".into(),
                kind: AuthorKind::Person,
            },
            mentions_fleet: false,
            at,
            text: text.into(),
            key: None,
            starts_thread: false,
        }
    }

    fn thread() -> ConversationRef {
        ConversationRef::split("fake:C0123ABCD:1730000000000").unwrap()
    }

    #[test]
    fn addresses_parse_for_this_account_and_platform_only() {
        let (g, dir) = fake("parse");
        assert_eq!(
            g.parse("fake-main", "fake:C0123ABCD"),
            Observation::Known(thread().channel())
        );
        assert_eq!(
            g.parse("fake-main", "fake:C0123ABCD:1730000000000"),
            Observation::Known(thread())
        );
        for bad in [
            "slack:C0123ABCD",
            "fake:",
            "fake:C0 1:2",
            "fake:C1:t:x",
            "nonsense",
        ] {
            assert!(g.parse("fake-main", bad).is_unknown(), "{bad}");
        }
        assert!(g.parse("other", "fake:C0123ABCD").is_unknown());
        g.add_fault(GatewayFault::new("parse", "malformed"))
            .unwrap();
        assert!(g.parse("fake-main", "fake:C0123ABCD").is_unknown());
        assert_eq!(g.calls("parse").unwrap(), 8);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_envelope_becomes_a_delivery_and_the_bot_id_marks_fleets_own_words() {
        let (g, dir) = fake("event");
        let envelope = |payload: Value| Envelope {
            verified: Verified::Connection,
            received_at: 1_730_000_300_100,
            payload,
        };
        let message = json!({"type": "message", "conversation": {"platform": "fake", "chat": "C0123ABCD", "thread": "1730000000000"},
            "id": "1730000300000", "author": {"id": "U0123ABCD", "name": "Alex", "kind": "person"},
            "at": 1_730_000_300_000u64, "text": format!("<@{DEFAULT_BOT}> please look")});
        let d = g
            .event(
                &account(),
                &envelope(json!({"id": "Ev01", "event": message})),
            )
            .known()
            .cloned()
            .unwrap()
            .unwrap();
        assert_eq!(d.id, "Ev01");
        assert_eq!(d.verified, Verified::Connection);
        let Event::Message {
            message,
            conversation,
        } = &d.event
        else {
            panic!("{d:?}")
        };
        assert_eq!(conversation, &thread());
        assert!(
            message.mentions_fleet,
            "the bot id in the text is a mention"
        );
        assert_eq!(message.author.kind, AuthorKind::Person);
        let own = json!({"type": "message", "conversation": {"platform": "fake", "chat": "C0123ABCD"},
            "id": "1", "author": {"id": DEFAULT_BOT, "name": "fleet", "kind": "bot"}, "at": 1, "text": "hi",
            "key": "task/t/u/gateway/comment/c1"});
        let d = g
            .event(&account(), &envelope(json!({"id": "Ev02", "event": own})))
            .known()
            .cloned()
            .unwrap()
            .unwrap();
        let Event::Message { message, .. } = &d.event else {
            panic!("{d:?}")
        };
        assert_eq!(
            message.author.kind,
            AuthorKind::Fleet,
            "the account's bot is Fleet"
        );
        assert_eq!(
            g.event(
                &account(),
                &envelope(json!({"id": "Ev03", "reaction": "eyes"}))
            ),
            Observation::Known(None),
            "a payload without an event is ignored"
        );
        assert!(g.event(&account(), &envelope(json!("text"))).is_unknown());
        assert!(
            g.event(&account(), &envelope(json!({"event": own})))
                .is_unknown(),
            "no delivery id"
        );
        assert!(
            g.event(
                &account(),
                &envelope(json!({"id": "Ev04", "event": {"type": "wave"}}))
            )
            .is_unknown()
        );
        let mut slack = account();
        slack.platform = "slack".into();
        assert!(
            g.event(&slack, &envelope(json!({"id": "Ev05", "event": own})))
                .is_unknown()
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn history_reads_after_an_id_or_since_a_time_and_reports_the_starter() {
        let (g, dir) = fake("history");
        let t = thread();
        g.seed(
            &t,
            vec![
                person("U1", 1_730_000_000_000, "first"),
                person("U2", 1_730_000_300_000, "second"),
                person("U1", 1_730_000_400_000, "third"),
            ],
        )
        .unwrap();
        let all = g.history(
            "fake-main",
            &t,
            &HistoryQuery::After {
                id: None,
                limit: 200,
            },
        );
        let h = all.known().unwrap();
        assert_eq!(
            h.messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
        assert_eq!(h.left_out, 0);
        assert_eq!(
            h.starter,
            Some(Starter {
                id: Some("1730000000000".into()),
                kind: Some(AuthorKind::Person),
                key: None,
                removed: false
            })
        );
        let newest = g.history("fake-main", &t, &HistoryQuery::After { id: None, limit: 2 });
        let h = newest.known().unwrap();
        assert_eq!(
            h.messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            ["second", "third"]
        );
        assert_eq!(h.left_out, 1, "the oldest was left out under the limit");
        let after = g.history(
            "fake-main",
            &t,
            &HistoryQuery::After {
                id: Some("1730000300000".into()),
                limit: 200,
            },
        );
        assert_eq!(after.known().unwrap().messages.len(), 1);
        let since = g.history(
            "fake-main",
            &t,
            &HistoryQuery::Since {
                at: 1_730_000_300_000,
            },
        );
        assert_eq!(since.known().unwrap().messages.len(), 2);
        // A chat without a thread has no starter.
        let chat = t.channel();
        g.seed(&chat, vec![person("U1", 5, "dm")]).unwrap();
        assert_eq!(
            g.history("fake-main", &chat, &HistoryQuery::Since { at: 0 })
                .known()
                .unwrap()
                .starter,
            None
        );
        g.remove_starter(&t).unwrap();
        assert_eq!(
            g.history("fake-main", &t, &HistoryQuery::Since { at: 0 })
                .known()
                .unwrap()
                .starter,
            Some(Starter::removed())
        );
        // Unknowns, each in its own words; an unknown thread is a refusal, not absence.
        assert!(matches!(
            g.history(
                "fake-main",
                &ConversationRef::split("fake:C9:1").unwrap(),
                &HistoryQuery::Since { at: 0 }
            ),
            HistoryObservation::Unknown {
                kind: Uncertainty::Refused,
                ..
            }
        ));
        g.add_fault(GatewayFault::new("history", "throttle").retry_after(5000))
            .unwrap();
        assert!(matches!(
            g.history("fake-main", &t, &HistoryQuery::Since { at: 0 }),
            HistoryObservation::Unknown {
                kind: Uncertainty::Throttled,
                retry_after: Some(5000),
                ..
            }
        ));
        g.add_fault(GatewayFault::new("history", "incomplete"))
            .unwrap();
        assert!(matches!(
            g.history("fake-main", &t, &HistoryQuery::Since { at: 0 }),
            HistoryObservation::Unknown {
                kind: Uncertainty::Incomplete,
                ..
            }
        ));
        g.set_capabilities(Capabilities {
            history: false,
            ..default_capabilities()
        })
        .unwrap();
        assert_eq!(
            g.history("fake-main", &t, &HistoryQuery::Since { at: 0 }),
            HistoryObservation::Unsupported
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn open_and_post_land_keyed_posts_on_the_platforms_clock() {
        let (g, dir) = fake("post");
        let channel = thread().channel();
        let opened = g.open(
            "fake-main",
            &channel,
            "Question: which DB?",
            "task/t/u7/gateway/question/q1",
        );
        let Observation::Known(EffectOutcome::Opened {
            conversation,
            message,
            at,
        }) = opened
        else {
            panic!("{opened:?}")
        };
        assert_eq!(at, default_clock());
        assert_eq!(
            conversation,
            ConversationRef {
                thread: Some(at.to_string()),
                ..channel.clone()
            }
        );
        assert_eq!(message, at.to_string());
        let posted = g.post(
            "fake-main",
            &conversation,
            "Thanks.",
            "task/t/u7/gateway/comment/c1",
        );
        let Observation::Known(EffectOutcome::Posted {
            message: m2,
            at: at2,
        }) = posted
        else {
            panic!("{posted:?}")
        };
        assert_eq!(at2, at + 1000, "the clock advanced one second");
        assert_ne!(m2, message);
        let posts = g.posts().unwrap();
        assert_eq!(
            posts.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(),
            [
                "task/t/u7/gateway/question/q1",
                "task/t/u7/gateway/comment/c1"
            ]
        );
        let h = g.history("fake-main", &conversation, &HistoryQuery::Since { at: 0 });
        let h = h.known().unwrap();
        assert_eq!(h.messages.len(), 2);
        assert_eq!(
            h.messages[0].key.as_deref(),
            Some("task/t/u7/gateway/question/q1"),
            "a metadata marker reads back"
        );
        assert_eq!(h.messages[0].author.kind, AuthorKind::Fleet);
        assert!(h.messages[0].starts_thread);
        assert_eq!(
            h.starter.as_ref().and_then(|s| s.key.clone()).as_deref(),
            Some("task/t/u7/gateway/question/q1")
        );
        // Refusals are Known; a thread is not a channel; a missing thread takes no post.
        assert!(matches!(
            g.open("fake-main", &conversation, "x", "k"),
            Observation::Known(EffectOutcome::Failed { .. })
        ));
        assert!(matches!(
            g.post(
                "fake-main",
                &ConversationRef::split("fake:C9:1").unwrap(),
                "x",
                "k"
            ),
            Observation::Known(EffectOutcome::Failed { .. })
        ));
        assert!(matches!(
            g.post("other", &conversation, "x", "k"),
            Observation::Known(EffectOutcome::Failed { .. })
        ));
        // Faults: a refusal, a lost answer with nothing posted, and one with the post landed.
        g.add_fault(GatewayFault::new("post", "failed")).unwrap();
        assert!(matches!(
            g.post("fake-main", &conversation, "x", "k1"),
            Observation::Known(EffectOutcome::Failed { .. })
        ));
        g.add_fault(GatewayFault::new("post", "lost")).unwrap();
        assert!(g.post("fake-main", &conversation, "x", "k2").is_unknown());
        assert_eq!(g.posts().unwrap().len(), 2);
        g.add_fault(GatewayFault::new("post", "lost-applied"))
            .unwrap();
        assert!(g.post("fake-main", &conversation, "x", "k3").is_unknown());
        assert_eq!(g.posts().unwrap().last().unwrap().key, "k3");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_marker_capability_decides_what_a_repeated_key_does() {
        let (g, dir) = fake("marker");
        let channel = thread().channel();
        let t = ConversationRef {
            thread: Some("1".into()),
            ..channel.clone()
        };
        g.seed(&t, vec![]).unwrap();
        // Metadata: a repeat is a second post, both carrying the key.
        g.post("fake-main", &t, "a", "k").known().unwrap();
        g.post("fake-main", &t, "a", "k").known().unwrap();
        assert_eq!(g.posts().unwrap().len(), 2);
        // Nonce: a repeat inside the window returns the existing post, and
        // the key is not readable from history.
        g.set_capabilities(Capabilities {
            marker: Marker::Nonce { window_ms: 60_000 },
            ..default_capabilities()
        })
        .unwrap();
        let first = g.post("fake-main", &t, "b", "n");
        let again = g.post("fake-main", &t, "b", "n");
        assert_eq!(first, again);
        assert_eq!(g.posts().unwrap().len(), 3);
        g.set_clock(default_clock() + 10 * 60_000).unwrap();
        assert_ne!(
            g.post("fake-main", &t, "b", "n"),
            first,
            "past the window the nonce is forgotten"
        );
        assert!(
            g.history("fake-main", &t, &HistoryQuery::Since { at: 0 })
                .known()
                .unwrap()
                .messages
                .iter()
                .skip(2)
                .all(|m| m.key.is_none())
        );
        // None: every call is a new post.
        g.set_capabilities(Capabilities {
            marker: Marker::None,
            ..default_capabilities()
        })
        .unwrap();
        let x = g.post("fake-main", &t, "c", "z");
        assert_ne!(g.post("fake-main", &t, "c", "z"), x);
        // The opening post obeys the same rule.
        g.set_capabilities(Capabilities {
            marker: Marker::Nonce { window_ms: 60_000 },
            ..default_capabilities()
        })
        .unwrap();
        let o = g.open("fake-main", &channel, "q", "open-key");
        assert_eq!(g.open("fake-main", &channel, "q", "open-key"), o);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_long_text_is_split_into_consecutive_posts_under_derived_keys() {
        let (g, dir) = fake("split");
        let t = thread();
        g.seed(&t, vec![]).unwrap();
        g.set_post_limit(Some(4)).unwrap();
        let Observation::Known(EffectOutcome::Posted { message, .. }) =
            g.post("fake-main", &t, "abcdefghij", "k")
        else {
            panic!()
        };
        let posts = g.posts().unwrap();
        assert_eq!(
            posts
                .iter()
                .map(|p| (p.key.as_str(), p.text.as_str()))
                .collect::<Vec<_>>(),
            [("k/1", "abcd"), ("k/2", "efgh"), ("k/3", "ij")]
        );
        assert_eq!(message, posts[0].message, "the first post is the answer");
        let short = g.post("fake-main", &t, "abc", "s");
        assert!(matches!(
            short,
            Observation::Known(EffectOutcome::Posted { .. })
        ));
        assert_eq!(g.posts().unwrap().last().unwrap().key, "s");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn whoami_and_capabilities_answer_what_a_test_set_and_unknown_under_a_fault() {
        let (g, dir) = fake("identity");
        assert_eq!(
            g.whoami("fake-main"),
            Observation::Known(default_identity())
        );
        assert_eq!(
            g.capabilities("fake-main"),
            Observation::Known(default_capabilities())
        );
        g.set_identity(Identity {
            id: "B0OTHER".into(),
            name: None,
        })
        .unwrap();
        assert_eq!(g.whoami("fake-main").known().unwrap().id, "B0OTHER");
        g.add_fault(GatewayFault::new("whoami", "lost").times(2))
            .unwrap();
        assert!(g.whoami("fake-main").is_unknown());
        assert!(g.whoami("fake-main").is_unknown());
        assert!(!g.whoami("fake-main").is_unknown());
        g.add_fault(GatewayFault::new("capabilities", "lost"))
            .unwrap();
        assert!(g.capabilities("fake-main").is_unknown());
        assert!(g.whoami("other").is_unknown());
        assert!(g.capabilities("other").is_unknown());
        // The state round-trips through its file, so another process sees it.
        let again = FakeGateway::open("fake-main", &dir).unwrap();
        assert_eq!(again.state().unwrap(), g.state().unwrap());
        assert_eq!(again.calls("whoami").unwrap(), 5);
        let _ = fs::remove_dir_all(dir);
    }
}
