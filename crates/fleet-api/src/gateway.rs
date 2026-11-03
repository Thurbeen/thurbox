//! The Gateway port: how Fleet reads and writes one chat thread, on Slack
//! or any platform an adapter maps onto these types, and the `[gateway]`
//! configuration the controllers decide over. Every answer is Known or
//! Unknown, and a refused write is `Known(EffectOutcome::Failed { why })`.
//!
//! Nothing above this port names a platform. One address format names a
//! conversation everywhere above it, `<platform>:<chat>[:<thread>]`, and
//! only the adapter reads its parts. The
//! [interface](../../../docs/controllers/gateway/interface.md) defines the
//! port, the [adapters page](../../../docs/controllers/gateway/adapters/README.md)
//! maps each platform onto it, and the
//! [config page](../../../docs/configuration/fleet-toml.md#the-gateway-section)
//! defines the section.
//!
//! Every call runs on the machine the manager runs on. Fleet passes no
//! credential and stores none: an account names the external client's own
//! configuration, and the listener holds the platform connection itself.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::meta::Millis;
use crate::observation::Observation;
use crate::plan::EffectOutcome;
use crate::task::Workspace;
use crate::validate::{is_absolute_path, validate_name};

/// How many messages the first `history` read of a thread takes: the
/// newest 200.
pub const HISTORY_FIRST_READ: u32 = 200;

/// The refusal a route posts when it gives none of its own.
pub const DEFAULT_REFUSAL: &str = "Sorry, I only take requests from people on this channel's list.";

/// What the adapter parsed an address into, in Fleet's words. A chat with no
/// threads, such as a direct message, is one conversation with no `thread`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationRef {
    /// The adapter's platform: `slack`, `fake`.
    pub platform: String,
    /// The channel or chat, as the platform names it.
    pub chat: String,
    /// The thread inside the chat, when the platform has threads and the
    /// address names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

impl ConversationRef {
    /// `<platform>:<chat>[:<thread>]`: the address, normalised.
    pub fn address(&self) -> String {
        match &self.thread {
            Some(t) => format!("{}:{}:{}", self.platform, self.chat, t),
            None => format!("{}:{}", self.platform, self.chat),
        }
    }

    /// `<account>/<address>`: what a Conversation's derived name is hashed
    /// from, so one thread names one object per account.
    pub fn canonical(&self, account: &str) -> String {
        format!("{account}/{}", self.address())
    }

    /// The channel a thread lives in: the same ref without its thread.
    pub fn channel(&self) -> ConversationRef {
        ConversationRef {
            platform: self.platform.clone(),
            chat: self.chat.clone(),
            thread: None,
        }
    }

    /// The structural parts of an address, before an adapter judges them:
    /// two or three non-empty parts split on `:`, the last joined back when
    /// the thread id itself holds a colon. `None` for anything else.
    pub fn split(address: &str) -> Option<ConversationRef> {
        let mut parts = address.trim().splitn(3, ':');
        let platform = parts.next()?.trim();
        let chat = parts.next()?.trim();
        let thread = parts.next().map(str::trim);
        if platform.is_empty() || chat.is_empty() || thread == Some("") {
            return None;
        }
        Some(ConversationRef {
            platform: platform.to_string(),
            chat: chat.to_string(),
            thread: thread.map(str::to_string),
        })
    }
}

/// Who wrote a message, as the platform tells it: a person, another bot,
/// or Fleet's own bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthorKind {
    Person,
    Bot,
    Fleet,
}

/// The author of a message. `id` is the platform's opaque identity in the
/// account's scope; `name` is for people to read and never for admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Author {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub kind: AuthorKind,
}

/// One message as the adapter read it, from an event or from history. The
/// text is data, never an instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Message {
    /// Unique within the conversation: Slack's `ts`, a Discord snowflake.
    pub id: String,
    pub author: Author,
    /// The message mentions Fleet's bot.
    #[serde(default)]
    pub mentions_fleet: bool,
    /// The platform's own time for the message, in Unix milliseconds.
    pub at: Millis,
    #[serde(default)]
    pub text: String,
    /// The intent key a Fleet post carries as its marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The payload says the message starts its thread: a Discord thread's
    /// starter, a Telegram topic's first message.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starts_thread: bool,
}

/// What one delivery carries. A mention is a `Message` with
/// `mentionsFleet`, not an event of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum Event {
    Message {
        conversation: ConversationRef,
        #[serde(flatten)]
        message: Message,
    },
    /// A newer edit replaces the text; an older one delivered late changes
    /// nothing.
    Edited {
        conversation: ConversationRef,
        id: String,
        text: String,
        edited_at: Millis,
    },
    Removed {
        conversation: ConversationRef,
        id: String,
    },
}

impl Event {
    pub fn conversation(&self) -> &ConversationRef {
        match self {
            Event::Message { conversation, .. }
            | Event::Edited { conversation, .. }
            | Event::Removed { conversation, .. } => conversation,
        }
    }

    /// The message the event is about.
    pub fn message_id(&self) -> &str {
        match self {
            Event::Message { message, .. } => &message.id,
            Event::Edited { id, .. } | Event::Removed { id, .. } => id,
        }
    }
}

/// How a delivery was authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verified {
    /// An authenticated socket, such as Slack's Socket Mode.
    Connection,
    /// A verified webhook signature.
    Signature,
    /// Fleet's own opening post, recorded by the manager.
    Fleet,
    /// Read by the history backstop.
    History,
}

impl Verified {
    pub fn name(self) -> &'static str {
        match self {
            Verified::Connection => "connection",
            Verified::Signature => "signature",
            Verified::Fleet => "fleet",
            Verified::History => "history",
        }
    }
}

/// What a listener writes to the receive verb, one per delivery: how it
/// authenticated the delivery, when it received it, and the platform's raw
/// event, which only the adapter reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Envelope {
    pub verified: Verified,
    pub received_at: Millis,
    pub payload: Value,
}

/// What `event` read out of an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Delivery {
    /// The same on every redelivery of one event, different for an edit or
    /// removal of the same message: the platform's event id where one
    /// exists, else derived by the adapter.
    pub id: String,
    pub verified: Verified,
    pub received_at: Millis,
    pub event: Event,
}

/// How an adapter recognises Fleet's own posts again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Marker {
    /// The post carries the intent key as metadata the platform reads back.
    Metadata,
    /// A nonce the platform honours for `windowMs`: a retry inside the
    /// window returns the existing post.
    Nonce { window_ms: Millis },
    /// Nothing: an Unknown post outcome can never be recovered.
    None,
}

/// What the platform offers on an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capabilities {
    /// Bots can read a thread's history.
    pub history: bool,
    pub marker: Marker,
    /// Messages without a mention reach Fleet.
    pub unmentioned: bool,
    /// The platform has threads inside a chat.
    pub threads: bool,
}

/// Fleet's bot identity, as the platform reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Identity {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Which messages a `history` read asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum HistoryQuery {
    /// The messages after `id`, or the whole thread when `None`; at most
    /// `limit`, the newest ones, with a count of those left out.
    After { id: Option<String>, limit: u32 },
    /// Every message at or after `at`. A Known answer is complete: a read
    /// that cannot page through everything is Unknown, never partial.
    Since { at: Millis },
}

/// The thread's first message, as a history read found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Starter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AuthorKind>,
    /// The marker on the starter, when Fleet posted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The platform reports the first message deleted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
}

impl Starter {
    pub fn message(m: &Message) -> Starter {
        Starter {
            id: Some(m.id.clone()),
            kind: Some(m.author.kind),
            key: m.key.clone(),
            removed: false,
        }
    }

    pub fn removed() -> Starter {
        Starter {
            id: None,
            kind: None,
            key: None,
            removed: true,
        }
    }
}

/// A Known `history` answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct History {
    /// Oldest first.
    #[serde(default)]
    pub messages: Vec<Message>,
    /// How many older messages an `After` read left out under its limit.
    #[serde(default)]
    pub left_out: u64,
    /// The thread's first message; absent for a chat without threads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starter: Option<Starter>,
}

/// Why a read could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Uncertainty {
    /// A call ran past its bound and its process tree was killed.
    Timeout,
    /// The bound ran out before every page was read.
    Incomplete,
    /// The platform asked Fleet to slow down.
    Throttled,
    /// The answer could not be parsed.
    Unreadable,
    /// The platform answered no, for a reason that is not proven absence.
    Refused,
    /// The client could not be started on this machine.
    Unavailable,
}

impl Uncertainty {
    pub fn reason(self) -> &'static str {
        match self {
            Uncertainty::Timeout => "Timeout",
            Uncertainty::Incomplete => "Incomplete",
            Uncertainty::Throttled => "Throttled",
            Uncertainty::Unreadable => "Unreadable",
            Uncertainty::Refused => "Refused",
            Uncertainty::Unavailable => "CliUnavailable",
        }
    }
}

/// The answer to `history`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryObservation {
    Known(History),
    /// Bots cannot read history on this platform.
    Unsupported,
    Unknown {
        kind: Uncertainty,
        why: String,
        /// How long the platform asked Fleet to wait, in milliseconds, when
        /// it said.
        retry_after: Option<Millis>,
    },
}

impl HistoryObservation {
    pub fn unknown(kind: Uncertainty, why: impl Into<String>) -> HistoryObservation {
        HistoryObservation::Unknown {
            kind,
            why: why.into(),
            retry_after: None,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, HistoryObservation::Unknown { .. })
    }

    pub fn known(&self) -> Option<&History> {
        match self {
            HistoryObservation::Known(h) => Some(h),
            _ => None,
        }
    }
}

/// The derived key of part `n` of `total` of a post an adapter split:
/// `<key>/<n>/<total>`. Every part names the count, so a read that finds
/// the parts tells a whole post from one that landed in part.
pub fn part_key(key: &str, n: usize, total: usize) -> String {
    format!("{key}/{n}/{total}")
}

/// Which part of the post under `key` a marker names: `(n, total)`, with
/// `(1, 1)` for a post that was not split, or `None` for the marker of
/// some other post.
pub fn part_of(marker: &str, key: &str) -> Option<(usize, usize)> {
    if marker == key {
        return Some((1, 1));
    }
    let rest = marker.strip_prefix(key)?.strip_prefix('/')?;
    let (n, total) = rest.split_once('/')?;
    let (n, total): (usize, usize) = (n.parse().ok()?, total.parse().ok()?);
    (n >= 1 && n <= total).then_some((n, total))
}

/// What a read of a thread holds of Fleet's post under one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostFound {
    /// The post, or the first part of a split whose every part is there:
    /// what the post's receipt names.
    Whole { message: String, at: Millis },
    /// A split with parts missing: the parts found, of the total.
    Partial { found: Vec<usize>, total: usize },
}

/// Fleet's own post under `key` among `messages`: the post itself, every
/// part of a split, or the parts of it that are there. A post is found
/// whole only when every part is.
pub fn find_post<'m>(
    messages: impl IntoIterator<Item = &'m Message>,
    key: &str,
) -> Option<PostFound> {
    let mut parts: BTreeMap<usize, &Message> = BTreeMap::new();
    let mut total = 0;
    for m in messages {
        if m.author.kind != AuthorKind::Fleet {
            continue;
        }
        let Some((n, of)) = m.key.as_deref().and_then(|k| part_of(k, key)) else {
            continue;
        };
        if of == 1 {
            return Some(PostFound::Whole {
                message: m.id.clone(),
                at: m.at,
            });
        }
        total = total.max(of);
        parts.entry(n).or_insert(m);
    }
    if total == 0 {
        return None;
    }
    let found: Vec<usize> = parts.keys().copied().collect();
    if found.len() == total && found.iter().zip(1..).all(|(n, i)| *n == i) {
        let first = parts[&1];
        return Some(PostFound::Whole {
            message: first.id.clone(),
            at: first.at,
        });
    }
    Some(PostFound::Partial { found, total })
}

/// The port. Text is Fleet's Markdown subset: paragraphs, emphasis, code,
/// links, lists and quotes. The adapter converts it and splits a long text
/// into consecutive posts under derived keys that name the part and the
/// count (`<key>/1/3`, `<key>/2/3`, `<key>/3/3`, see [`part_key`]). There
/// is no edit, delete or merge method: Fleet never changes a post it made.
pub trait Gateway {
    /// The longest one call of this trait can take, every bound and grace
    /// included: the manager's lease is sized to outlast it.
    fn operation_bound(&self) -> Millis;
    /// The normalised reference for an address on an account. Unknown for an
    /// account that is not configured, or an address the adapter cannot
    /// read.
    fn parse(&self, account: &str, address: &str) -> Observation<ConversationRef>;
    /// What an envelope carries: a delivery, `None` for a payload the
    /// Gateway ignores (a reaction, a presence change), or Unknown for one
    /// the adapter cannot read. Pure, so the receive verb runs it without
    /// the lease.
    fn event(&self, account: &AccountConfig, envelope: &Envelope) -> Observation<Option<Delivery>>;
    /// Fleet's bot identity on the account, as the platform reports it.
    fn whoami(&self, account: &str) -> Observation<Identity>;
    /// What the platform offers on the account.
    fn capabilities(&self, account: &str) -> Observation<Capabilities>;
    /// The messages the query asks for, oldest first, and the thread's first
    /// message as `starter`. `Unsupported` where bots cannot read history.
    fn history(
        &self,
        account: &str,
        conversation: &ConversationRef,
        query: &HistoryQuery,
    ) -> HistoryObservation;
    /// Open a thread in `channel` whose first post is `text`, carrying
    /// `intent_key` as its marker: `Opened { conversation, message, at }`.
    /// An open that takes two platform calls keys both from the one key and
    /// finishes a half-done open on retry.
    fn open(
        &self,
        account: &str,
        channel: &ConversationRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome>;
    /// Post `text` in the conversation, carrying `intent_key` as its marker:
    /// `Posted { message, at }`, with `at` the platform's own time. To a
    /// channel address (no `thread`), every part is a top-level post in the
    /// channel, and no thread is started.
    fn post(
        &self,
        account: &str,
        conversation: &ConversationRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome>;
    /// A URL that opens the thread, or `None` where the adapter cannot make
    /// one without a platform call. Pure: it calls no platform.
    fn link(&self, account: &str, thread: &ConversationRef) -> Option<String> {
        let _ = (account, thread);
        None
    }
}

// ----------------------------------------------------------------------
// The `[gateway]` section.
// ----------------------------------------------------------------------

/// How an account's listener receives deliveries, which fixes how each is
/// authenticated. An envelope claiming another method is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Inbound {
    /// Slack Socket Mode: an authenticated connection.
    SocketMode,
    /// Slack's Events API: a signed HTTPS delivery.
    EventsApi,
    /// The fake's listener: an authenticated connection.
    Connection,
    /// Discord's Gateway WebSocket, held by the listener: an authenticated
    /// connection.
    DiscordGateway,
}

impl Inbound {
    /// The one `verified` an envelope on this account may claim.
    pub fn verified(self) -> Verified {
        match self {
            Inbound::SocketMode | Inbound::Connection | Inbound::DiscordGateway => {
                Verified::Connection
            }
            Inbound::EventsApi => Verified::Signature,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Inbound::SocketMode => "socket-mode",
            Inbound::EventsApi => "events-api",
            Inbound::Connection => "connection",
            Inbound::DiscordGateway => "discord-gateway",
        }
    }
}

/// `[gateway.accounts.<name>]`: one chat account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountConfig {
    /// `slack`, `discord`, or `fake` with a `dir`.
    pub platform: String,
    /// The platform's workspace identity: a Slack team or enterprise id, a
    /// Discord guild id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Fleet's bot id there, checked against the platform's `whoami`.
    pub bot: String,
    pub inbound: Inbound,
    /// Discord only: the bot holds the privileged message content intent,
    /// so its listener asks for it and unmentioned messages reach Fleet.
    /// Absent means `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_content: Option<bool>,
    /// The client executable, a path or a name on PATH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    /// An absolute path Fleet passes to `curl -K` and never reads: the
    /// operator's file holding the bot token header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curl_config: Option<PathBuf>,
    /// The fake's directory, relative to the config file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
}

/// When an `Admitted` message in an untied thread starts a Task.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mention {
    /// The message mentions Fleet.
    #[default]
    Required,
    /// The message starts a thread: every top-level post is a request.
    TopLevel,
}

/// `[gateway.routes.<name>]`: the channels one set of admission rules and
/// one Task template cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteConfig {
    pub account: String,
    /// Platform channel ids. A channel belongs to at most one route.
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub mention: Mention,
    /// Names under `[gateway.people]`.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Bot or app ids whose messages count as a person's.
    #[serde(default)]
    pub allow_bots: Vec<String>,
    /// The TaskConfig a new conversation Task references.
    pub task_config: String,
    /// An absolute path the manager reads when it loads the config and
    /// copies into each Task it creates.
    pub brief: PathBuf,
    /// The Task's workspace. A `dir` is a base directory: each thread's
    /// Tasks get `<dir>/<conversation>`, so no thread waits for another.
    /// `repositories` are materialized for each Task.
    pub workspace: Workspace,
    /// The one polite refusal; `DEFAULT_REFUSAL` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

impl RouteConfig {
    pub fn refusal(&self) -> &str {
        self.refusal.as_deref().unwrap_or(DEFAULT_REFUSAL)
    }
}

/// `[gateway.people.<name>]`: one platform id per account name.
pub type Person = BTreeMap<String, String>;

/// The whole `[gateway]` section as the config file gives it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayConfig {
    /// Bound for each adapter call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_timeout_ms: Option<Millis>,
    /// The history backstop interval for a Tied thread, and the cadence of
    /// the other periodic reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_ms: Option<Millis>,
    /// How long after a Tied thread's last message its backstop keeps
    /// polling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_ms: Option<Millis>,
    /// An Untied or Orphaned Conversation's retention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_ms: Option<Millis>,
    /// The longest a listener goes between beats; stale after three.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beat_ms: Option<Millis>,
    /// The route whose first channel takes a Question from an untied,
    /// unlabelled Task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<String>,
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountConfig>,
    #[serde(default)]
    pub people: BTreeMap<String, Person>,
    #[serde(default)]
    pub routes: BTreeMap<String, RouteConfig>,
    /// Where every Task's result and change news are posted, as top-level
    /// posts. Absent: they are posted nowhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<LogsConfig>,
}

/// `[gateway] logs = { account, channel }`: the log destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogsConfig {
    /// A name under `[gateway.accounts]`.
    pub account: String,
    /// One channel id on that account. No route covers it.
    pub channel: String,
}

/// The log destination as the controllers read it: the account and the
/// channel's address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogDestination {
    pub account: String,
    pub channel: ConversationRef,
}

/// The thread a log post names: the tied thread and the account it is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogThread {
    pub account: String,
    pub conversation: ConversationRef,
}

/// What the controllers decide over, as values: the `[gateway]` timings,
/// and the part of the config the object in hand names. The manager fills
/// the per-object part; the controllers read no configuration themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayDefaults {
    pub call_timeout_ms: Millis,
    pub poll_ms: Millis,
    pub active_ms: Millis,
    pub retain_ms: Millis,
    pub beat_ms: Millis,
    /// For a Conversation: the route its spec names, while the config has
    /// it. `None`: the route left the config.
    pub route: Option<RouteConfig>,
    /// For a Conversation: the route's brief, as the manager read the file
    /// when it loaded the config. `None` while the route is.
    pub brief: Option<String>,
    /// The people table, for admission.
    pub people: BTreeMap<String, Person>,
    /// For a Listener: the account its spec names, while the config has it.
    /// `None`: the account left the config.
    pub account: Option<ListenerAccount>,
    /// For a Task: every route by name, so the controller finds the one
    /// its Conversation's spec names.
    pub routes: BTreeMap<String, RouteConfig>,
    /// For a Task: the route whose first channel takes a Question from an
    /// untied Task without the route label.
    pub questions: Option<String>,
    /// For a Task: every account's platform by account name, so the
    /// controller names the channel a Question opens a thread in.
    pub platforms: BTreeMap<String, String>,
    /// For a Task: where its result and change news are logged. `None`:
    /// nowhere.
    pub logs: Option<LogDestination>,
}

/// The account a Listener reports on, as the loaded config has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerAccount {
    pub config: AccountConfig,
    /// The digest `fleet gateway account NAME --json` prints, which a
    /// listener's beats carry.
    pub digest: String,
    /// Some route uses the account.
    pub routed: bool,
}

impl Default for GatewayDefaults {
    fn default() -> Self {
        GatewayDefaults {
            call_timeout_ms: 15_000,
            poll_ms: 300_000,
            active_ms: 86_400_000,
            retain_ms: 604_800_000,
            beat_ms: 60_000,
            route: None,
            brief: None,
            people: BTreeMap::new(),
            account: None,
            routes: BTreeMap::new(),
            questions: None,
            platforms: BTreeMap::new(),
            logs: None,
        }
    }
}

impl GatewayDefaults {
    /// The same timings and people, for the Conversation whose spec names
    /// `route`, with the route's brief text.
    pub fn for_route(&self, route: Option<RouteConfig>, brief: Option<String>) -> GatewayDefaults {
        GatewayDefaults {
            brief: brief.filter(|_| route.is_some()),
            route,
            account: None,
            ..self.clone()
        }
    }

    /// The same timings, for the Listener of `account`.
    pub fn for_account(&self, account: Option<ListenerAccount>) -> GatewayDefaults {
        GatewayDefaults {
            route: None,
            brief: None,
            account,
            ..self.clone()
        }
    }
}

impl GatewayConfig {
    /// The timing values, each the file's or the default.
    pub fn defaults(&self) -> GatewayDefaults {
        let base = GatewayDefaults::default();
        GatewayDefaults {
            call_timeout_ms: self.call_timeout_ms.unwrap_or(base.call_timeout_ms),
            poll_ms: self.poll_ms.unwrap_or(base.poll_ms),
            active_ms: self.active_ms.unwrap_or(base.active_ms),
            retain_ms: self.retain_ms.unwrap_or(base.retain_ms),
            beat_ms: self.beat_ms.unwrap_or(base.beat_ms),
            route: None,
            brief: None,
            people: self.people.clone(),
            account: None,
            routes: self.routes.clone(),
            questions: self.questions.clone(),
            platforms: self
                .accounts
                .iter()
                .map(|(name, a)| (name.clone(), a.platform.clone()))
                .collect(),
            logs: self.logs.as_ref().and_then(|l| {
                let account = self.accounts.get(&l.account)?;
                Some(LogDestination {
                    account: l.account.clone(),
                    channel: ConversationRef {
                        platform: account.platform.clone(),
                        chat: l.channel.clone(),
                        thread: None,
                    },
                })
            }),
        }
    }

    /// Whether some route uses the account.
    pub fn is_routed(&self, account: &str) -> bool {
        self.routes.values().any(|r| r.account == account)
    }

    /// The route a channel on an account belongs to, if any.
    pub fn route_of(&self, account: &str, channel: &str) -> Option<(&str, &RouteConfig)> {
        self.routes
            .iter()
            .find(|(_, r)| r.account == account && r.channels.iter().any(|c| c == channel))
            .map(|(n, r)| (n.as_str(), r))
    }

    /// Every rule the config page states, each refused with the entry it
    /// names. Platform-specific checks (a platform this build lacks, an
    /// `inbound` the platform has no listener for, a fake without a dir)
    /// belong to the adapters and run when the gateway opens.
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("callTimeoutMs", self.call_timeout_ms),
            ("pollMs", self.poll_ms),
            ("activeMs", self.active_ms),
            ("retainMs", self.retain_ms),
            ("beatMs", self.beat_ms),
        ] {
            if value == Some(0) {
                return Err(format!("gateway.{field} must be at least 1"));
            }
        }
        if let Some(ms) = self.call_timeout_ms
            && ms > 300_000
        {
            return Err("gateway.callTimeoutMs must be at most 300000".into());
        }
        for (name, account) in &self.accounts {
            let at = format!("gateway.accounts.{name}");
            validate_name(name).map_err(|e| format!("{at}: {e}"))?;
            if account.platform.trim().is_empty() {
                return Err(format!("{at}: platform is empty"));
            }
            if account.bot.trim().is_empty() {
                return Err(format!("{at}: bot is empty"));
            }
            if let Some(p) = &account.curl_config
                && !is_absolute_path(&p.to_string_lossy())
            {
                return Err(format!("{at}: curlConfig must be an absolute path"));
            }
            if account.cli.as_deref().is_some_and(|c| c.trim().is_empty()) {
                return Err(format!("{at}: cli is empty"));
            }
        }
        for (name, ids) in &self.people {
            let at = format!("gateway.people.{name}");
            validate_name(name).map_err(|e| format!("{at}: {e}"))?;
            for (account, id) in ids {
                if !self.accounts.contains_key(account) {
                    return Err(format!(
                        "{at}: account {account:?} is not under [gateway.accounts]"
                    ));
                }
                if id.trim().is_empty() {
                    return Err(format!("{at}: the id on account {account:?} is empty"));
                }
            }
        }
        let mut channels: BTreeMap<(&str, &str), &str> = BTreeMap::new();
        let mut dirs: Vec<(&str, &Path)> = Vec::new();
        for (name, route) in &self.routes {
            let at = format!("gateway.routes.{name}");
            validate_name(name).map_err(|e| format!("{at}: {e}"))?;
            if !self.accounts.contains_key(&route.account) {
                return Err(format!(
                    "{at}: account {:?} is not under [gateway.accounts]",
                    route.account
                ));
            }
            if route.channels.is_empty() {
                return Err(format!("{at}: the route covers no channel"));
            }
            let mut own = BTreeSet::new();
            for channel in &route.channels {
                if channel.trim().is_empty() {
                    return Err(format!("{at}: a channel id is empty"));
                }
                if !own.insert(channel.as_str()) {
                    return Err(format!("{at}: channel {channel:?} is listed twice"));
                }
                if let Some(other) =
                    channels.insert((route.account.as_str(), channel.as_str()), name.as_str())
                {
                    return Err(format!(
                        "gateway.routes: channel {channel:?} belongs to routes {other} and {name}"
                    ));
                }
            }
            if route.allow.is_empty() && route.allow_bots.is_empty() {
                return Err(format!("{at}: the route admits nobody"));
            }
            for person in &route.allow {
                let Some(ids) = self.people.get(person) else {
                    return Err(format!(
                        "{at}: allow names {person:?}, which is not under [gateway.people]"
                    ));
                };
                if !ids.contains_key(&route.account) {
                    return Err(format!(
                        "{at}: allow names {person:?}, who has no id on account {:?}",
                        route.account
                    ));
                }
            }
            if route.allow_bots.iter().any(|b| b.trim().is_empty()) {
                return Err(format!("{at}: a bot id under allowBots is empty"));
            }
            validate_name(&route.task_config).map_err(|e| format!("{at}: taskConfig: {e}"))?;
            if !is_absolute_path(&route.brief.to_string_lossy()) {
                return Err(format!("{at}: brief must be an absolute path"));
            }
            crate::validate::validate_workspace(&route.workspace)
                .map_err(|e| format!("{at}: workspace{e}"))?;
            // A base directory needs its own route; repositories are
            // materialized per Task, so nothing is shared between routes.
            let dir = route.workspace.dir().map(Path::new);
            if let Some(d) = route.workspace.dir()
                && !is_absolute_path(d)
            {
                return Err(format!("{at}: workspace.dir must be an absolute path"));
            }
            if route
                .refusal
                .as_deref()
                .is_some_and(|r| r.trim().is_empty())
            {
                return Err(format!("{at}: refusal is empty"));
            }
            let Some(dir) = dir else { continue };
            for (other, other_dir) in &dirs {
                if dir.starts_with(other_dir) || other_dir.starts_with(dir) {
                    return Err(format!(
                        "gateway.routes: the workspace of route {name} equals, contains or lies inside route {other}'s"
                    ));
                }
            }
            dirs.push((name.as_str(), dir));
        }
        if let Some(q) = &self.questions
            && !self.routes.contains_key(q)
        {
            return Err(format!(
                "gateway.questions {q:?} is not under [gateway.routes]"
            ));
        }
        if let Some(logs) = &self.logs {
            if !self.accounts.contains_key(&logs.account) {
                return Err(format!(
                    "gateway.logs: account {:?} is not under [gateway.accounts]",
                    logs.account
                ));
            }
            if logs.channel.trim().is_empty() {
                return Err("gateway.logs: the channel id is empty".into());
            }
            // A reply under a log post must never be admitted: the receive
            // verb ignores a channel that no route covers.
            if let Some((route, _)) = self.route_of(&logs.account, &logs.channel) {
                return Err(format!(
                    "gateway.logs: channel {:?} belongs to route {route}; the log channel must be no route's channel",
                    logs.channel
                ));
            }
        }
        Ok(())
    }
}

// ----------------------------------------------------------------------
// Admission.
// ----------------------------------------------------------------------

/// How admission sorts a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Admission {
    /// Fleet's own post: never relayed.
    Own,
    /// A person under `allow`, or a bot under `allowBots`: a turn.
    Admitted,
    /// Anyone else: relayed with the next admitted message, starts no turn.
    Context,
}

/// Sort a message by the route and the people table. A person is admitted
/// when some name under `route.allow` maps to the author's id on the
/// route's account; a bot when its id is under `route.allowBots`.
pub fn admit(
    route: &RouteConfig,
    people: &BTreeMap<String, Person>,
    message: &Message,
) -> Admission {
    match message.author.kind {
        AuthorKind::Fleet => Admission::Own,
        AuthorKind::Person => {
            let listed = route.allow.iter().any(|name| {
                people
                    .get(name)
                    .and_then(|ids| ids.get(&route.account))
                    .is_some_and(|id| *id == message.author.id)
            });
            if listed {
                Admission::Admitted
            } else {
                Admission::Context
            }
        }
        AuthorKind::Bot => {
            if route.allow_bots.contains(&message.author.id) {
                Admission::Admitted
            } else {
                Admission::Context
            }
        }
    }
}

/// Whether an admitted message in an untied thread starts a Task: it meets
/// the route's mention rule.
pub fn starts_task(route: &RouteConfig, admission: Admission, message: &Message) -> bool {
    admission == Admission::Admitted
        && match route.mention {
            Mention::Required => message.mentions_fleet,
            Mention::TopLevel => message.starts_thread,
        }
}

/// Whether a message earns the route's refusal: a person who is not on the
/// allowlist and mentions Fleet. An unlisted bot gets none.
pub fn earns_refusal(admission: Admission, message: &Message) -> bool {
    admission == Admission::Context
        && message.author.kind == AuthorKind::Person
        && message.mentions_fleet
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn person(id: &str) -> Author {
        Author {
            id: id.into(),
            name: String::new(),
            kind: AuthorKind::Person,
        }
    }

    fn msg(author: Author, mentions: bool) -> Message {
        Message {
            id: "1".into(),
            author,
            mentions_fleet: mentions,
            at: 1,
            text: "hi".into(),
            key: None,
            starts_thread: false,
        }
    }

    fn account() -> AccountConfig {
        AccountConfig {
            platform: "fake".into(),
            team: None,
            bot: "B0FLEET001".into(),
            inbound: Inbound::Connection,
            message_content: None,
            cli: None,
            curl_config: None,
            dir: Some("gateway".into()),
        }
    }

    fn route(dir: &str) -> RouteConfig {
        RouteConfig {
            account: "fake-main".into(),
            channels: vec!["C0123ABCD".into()],
            mention: Mention::Required,
            allow: vec!["alex".into()],
            allow_bots: vec!["B0123ABCD".into()],
            task_config: "conversation".into(),
            brief: "/etc/fleet/briefs/conversation.md".into(),
            workspace: Workspace::at(dir),
            refusal: None,
        }
    }

    fn config() -> GatewayConfig {
        GatewayConfig {
            accounts: BTreeMap::from([("fake-main".to_string(), account())]),
            people: BTreeMap::from([(
                "alex".to_string(),
                BTreeMap::from([("fake-main".to_string(), "U0123ABCD".to_string())]),
            )]),
            routes: BTreeMap::from([("eng".to_string(), route("/work/conversations"))]),
            questions: Some("eng".into()),
            ..GatewayConfig::default()
        }
    }

    #[test]
    fn an_address_splits_into_platform_chat_and_optional_thread() {
        let thread = ConversationRef::split("slack:C0123ABCD:1730000000.000100").unwrap();
        assert_eq!(
            thread,
            ConversationRef {
                platform: "slack".into(),
                chat: "C0123ABCD".into(),
                thread: Some("1730000000.000100".into()),
            }
        );
        assert_eq!(thread.address(), "slack:C0123ABCD:1730000000.000100");
        assert_eq!(
            thread.canonical("slack-main"),
            "slack-main/slack:C0123ABCD:1730000000.000100"
        );
        assert_eq!(thread.channel().address(), "slack:C0123ABCD");
        let channel = ConversationRef::split("discord:1300000000000000001").unwrap();
        assert_eq!(channel.thread, None);
        // A thread id that itself holds a colon stays whole.
        let odd = ConversationRef::split("teams:19:abc@thread.tacv2:1730000000").unwrap();
        assert_eq!(odd.chat, "19");
        assert_eq!(odd.thread.as_deref(), Some("abc@thread.tacv2:1730000000"));
        for bad in ["", "slack", "slack:", ":C1", "slack:C1:", "  "] {
            assert_eq!(ConversationRef::split(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_vocabulary_serialises_in_the_interface_pages_words() {
        assert_eq!(serde_json::to_value(AuthorKind::Fleet).unwrap(), "fleet");
        assert_eq!(
            serde_json::to_value(Verified::Signature).unwrap(),
            "signature"
        );
        assert_eq!(serde_json::to_value(Marker::Metadata).unwrap(), "metadata");
        assert_eq!(
            serde_json::to_value(Marker::Nonce { window_ms: 5 }).unwrap(),
            json!({"nonce": {"windowMs": 5}})
        );
        assert_eq!(serde_json::to_value(Marker::None).unwrap(), "none");
        assert_eq!(
            serde_json::to_value(Inbound::SocketMode).unwrap(),
            "socket-mode"
        );
        assert_eq!(Inbound::SocketMode.verified(), Verified::Connection);
        assert_eq!(Inbound::EventsApi.verified(), Verified::Signature);
        assert_eq!(
            serde_json::to_value(Mention::TopLevel).unwrap(),
            "top-level"
        );
        let event = Event::Message {
            conversation: ConversationRef::split("fake:C1:t1").unwrap(),
            message: msg(person("U1"), true),
        };
        let v = serde_json::to_value(&event).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["id"], "1");
        assert_eq!(v["mentionsFleet"], true);
        assert_eq!(v["conversation"]["thread"], "t1");
        assert_eq!(serde_json::from_value::<Event>(v).unwrap(), event);
        let edited: Event = serde_json::from_value(json!({
            "type": "edited", "conversation": {"platform": "fake", "chat": "C1"}, "id": "9",
            "text": "fixed", "editedAt": 7
        }))
        .unwrap();
        assert_eq!(serde_json::to_value(&edited).unwrap()["editedAt"], 7);
        let removed: Event = serde_json::from_value(json!({
            "type": "removed", "conversation": {"platform": "fake", "chat": "C1"}, "id": "9"
        }))
        .unwrap();
        assert_eq!(removed.message_id(), "9");
        assert_eq!(removed.conversation().address(), "fake:C1");
        let envelope: Envelope = serde_json::from_value(json!({
            "verified": "connection", "receivedAt": 1_730_000_300_100u64, "payload": {"x": 1}
        }))
        .unwrap();
        assert_eq!(envelope.verified, Verified::Connection);
        assert!(
            serde_json::from_value::<Envelope>(
                json!({"verified": "mail", "receivedAt": 1, "payload": {}})
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_value(Starter::removed()).unwrap(),
            json!({"removed": true})
        );
        assert_eq!(
            serde_json::to_value(HistoryQuery::After {
                id: None,
                limit: HISTORY_FIRST_READ
            })
            .unwrap(),
            json!({"type": "after", "id": null, "limit": 200})
        );
    }

    #[test]
    fn the_section_loads_from_the_config_pages_shape_with_defaults() {
        let toml_like = json!({
            "pollMs": 1000,
            "questions": "eng",
            "accounts": {"slack-main": {"platform": "slack", "team": "T0123ABCD", "bot": "U0BOT0001",
                                        "inbound": "socket-mode", "cli": "curl",
                                        "curlConfig": "/etc/fleet/slack-curl.conf"}},
            "people": {"alex": {"slack-main": "U0123ABCD"}},
            "routes": {"eng": {"account": "slack-main", "channels": ["C0123ABCD"], "allow": ["alex"],
                               "taskConfig": "conversation", "brief": "/etc/fleet/briefs/conversation.md",
                               "workspace": {"dir": "/work/conversations"}}}
        });
        let cfg: GatewayConfig = serde_json::from_value(toml_like).unwrap();
        assert_eq!(cfg.validate(), Ok(()));
        let d = cfg.defaults();
        assert_eq!(d.poll_ms, 1000);
        assert_eq!(d.retain_ms, 604_800_000);
        assert_eq!(d.beat_ms, 60_000);
        assert_eq!(cfg.routes["eng"].mention, Mention::Required);
        assert_eq!(cfg.routes["eng"].refusal(), DEFAULT_REFUSAL);
        assert_eq!(
            cfg.route_of("slack-main", "C0123ABCD").map(|(n, _)| n),
            Some("eng")
        );
        assert_eq!(cfg.route_of("slack-main", "C0789IJKL"), None);
        assert_eq!(cfg.route_of("other", "C0123ABCD"), None);
    }

    #[test]
    fn each_invalid_case_names_its_entry() {
        assert_eq!(config().validate(), Ok(()));
        let mut c = config();
        c.routes.get_mut("eng").unwrap().account = "nobody".into();
        assert!(
            c.validate()
                .unwrap_err()
                .contains("routes.eng: account \"nobody\"")
        );
        let mut c = config();
        c.routes.get_mut("eng").unwrap().allow = vec!["kim".into()];
        assert!(
            c.validate()
                .unwrap_err()
                .contains("allow names \"kim\", which is not under")
        );
        let mut c = config();
        c.people.insert(
            "kim".into(),
            BTreeMap::from([("other".to_string(), "U1".to_string())]),
        );
        assert!(
            c.validate()
                .unwrap_err()
                .contains("people.kim: account \"other\"")
        );
        let mut c = config();
        c.people
            .get_mut("alex")
            .unwrap()
            .insert("fake-main".into(), " ".into());
        assert!(
            c.validate()
                .unwrap_err()
                .contains("people.alex: the id on account \"fake-main\" is empty")
        );
        let mut c = config();
        c.accounts.insert("second".into(), account());
        c.people.insert(
            "kim".into(),
            BTreeMap::from([("second".to_string(), "U2".to_string())]),
        );
        c.routes.get_mut("eng").unwrap().allow.push("kim".into());
        assert!(
            c.validate()
                .unwrap_err()
                .contains("who has no id on account \"fake-main\"")
        );
        let mut c = config();
        c.routes.insert("ops".into(), route("/work/ops"));
        assert!(
            c.validate()
                .unwrap_err()
                .contains("channel \"C0123ABCD\" belongs to routes eng and ops")
        );
        for dir in [
            "/work/conversations",
            "/work",
            "/work/conversations/deeper",
            "/work/conversations/",
        ] {
            let mut c = config();
            let mut ops = route(dir);
            ops.channels = vec!["C0789IJKL".into()];
            c.routes.insert("ops".into(), ops);
            assert!(
                c.validate().unwrap_err().contains(
                    "the workspace of route ops equals, contains or lies inside route eng's"
                ),
                "{dir}"
            );
        }
        // Repositories are materialized per Task: two routes share nothing.
        let mut c = config();
        let repositories: Workspace = serde_json::from_value(serde_json::json!(
            {"repositories": [{"name": "app", "url": "https://example.invalid/app"}]}
        ))
        .unwrap();
        c.routes.get_mut("eng").unwrap().workspace = repositories.clone();
        let mut ops = route("/work");
        ops.channels = vec!["C0789IJKL".into()];
        ops.workspace = repositories;
        c.routes.insert("ops".into(), ops);
        assert_eq!(c.validate(), Ok(()));
        let mut c = config();
        c.routes.get_mut("eng").unwrap().workspace =
            serde_json::from_value(serde_json::json!({"repositories": []})).unwrap();
        assert!(
            c.validate()
                .unwrap_err()
                .contains("routes.eng: workspace.repositories must name at least one repository")
        );
        let mut c = config();
        let mut ops = route("/work/conversations-2");
        ops.channels = vec!["C0789IJKL".into()];
        c.routes.insert("ops".into(), ops);
        assert_eq!(
            c.validate(),
            Ok(()),
            "a sibling with a shared prefix is not inside"
        );
        let mut c = config();
        c.questions = Some("nowhere".into());
        assert!(
            c.validate()
                .unwrap_err()
                .contains("gateway.questions \"nowhere\"")
        );
        // Both portable absolute forms pass on every platform.
        for brief in [
            "/etc/fleet/briefs/x.md",
            "C:/fleet/briefs/x.md",
            "C:\\fleet\\briefs\\x.md",
        ] {
            let mut c = config();
            c.routes.get_mut("eng").unwrap().brief = brief.into();
            assert_eq!(c.validate(), Ok(()), "{brief}");
        }
        let mut c = config();
        c.routes.get_mut("eng").unwrap().brief = "briefs/x.md".into();
        assert!(
            c.validate()
                .unwrap_err()
                .contains("routes.eng: brief must be an absolute path")
        );
        let mut c = config();
        c.routes.get_mut("eng").unwrap().workspace = Workspace::at("work");
        assert!(
            c.validate()
                .unwrap_err()
                .contains("routes.eng: workspace.dir must be an absolute path")
        );
        let mut c = config();
        let r = c.routes.get_mut("eng").unwrap();
        r.allow.clear();
        r.allow_bots.clear();
        assert!(
            c.validate()
                .unwrap_err()
                .contains("routes.eng: the route admits nobody")
        );
        let mut c = config();
        c.routes.get_mut("eng").unwrap().channels.clear();
        assert!(
            c.validate()
                .unwrap_err()
                .contains("routes.eng: the route covers no channel")
        );
        let mut c = config();
        c.routes
            .get_mut("eng")
            .unwrap()
            .channels
            .push("C0123ABCD".into());
        assert!(c.validate().unwrap_err().contains("is listed twice"));
        let mut c = config();
        c.retain_ms = Some(0);
        assert!(
            c.validate()
                .unwrap_err()
                .contains("gateway.retainMs must be at least 1")
        );
        let mut c = config();
        c.accounts.get_mut("fake-main").unwrap().curl_config = Some("relative.conf".into());
        assert!(
            c.validate()
                .unwrap_err()
                .contains("curlConfig must be an absolute path")
        );
        let mut c = config();
        c.accounts.get_mut("fake-main").unwrap().bot = String::new();
        assert!(
            c.validate()
                .unwrap_err()
                .contains("accounts.fake-main: bot is empty")
        );
        let mut c = config();
        c.routes.get_mut("eng").unwrap().refusal = Some("  ".into());
        assert!(c.validate().unwrap_err().contains("refusal is empty"));
        assert!(serde_json::from_value::<RouteConfig>(json!({
            "account": "a", "taskConfig": "t", "brief": "/b", "workspace": {"dir": "/w"}, "colour": "red"
        })).unwrap_err().to_string().contains("unknown field `colour`"));
        assert!(
            serde_json::from_value::<AccountConfig>(json!({
                "platform": "fake", "bot": "B1", "inbound": "carrier-pigeon"
            }))
            .unwrap_err()
            .to_string()
            .contains("unknown variant `carrier-pigeon`")
        );
    }

    #[test]
    fn admission_sorts_by_the_people_table_and_the_bot_list() {
        let c = config();
        let route = &c.routes["eng"];
        assert_eq!(
            admit(route, &c.people, &msg(person("U0123ABCD"), true)),
            Admission::Admitted
        );
        assert_eq!(
            admit(route, &c.people, &msg(person("U0999ZZZZ"), true)),
            Admission::Context
        );
        let bot = Author {
            id: "B0123ABCD".into(),
            name: "alerts".into(),
            kind: AuthorKind::Bot,
        };
        assert_eq!(
            admit(route, &c.people, &msg(bot.clone(), false)),
            Admission::Admitted
        );
        let other_bot = Author {
            id: "B0999ZZZZ".into(),
            name: String::new(),
            kind: AuthorKind::Bot,
        };
        assert_eq!(
            admit(route, &c.people, &msg(other_bot.clone(), true)),
            Admission::Context
        );
        let fleet = Author {
            id: "B0FLEET001".into(),
            name: "fleet".into(),
            kind: AuthorKind::Fleet,
        };
        assert_eq!(admit(route, &c.people, &msg(fleet, true)), Admission::Own);
        // A person's id on another account does not admit them here.
        let mut elsewhere = config();
        elsewhere.people.get_mut("alex").unwrap().clear();
        elsewhere
            .people
            .get_mut("alex")
            .unwrap()
            .insert("other".into(), "U0123ABCD".into());
        assert_eq!(
            admit(route, &elsewhere.people, &msg(person("U0123ABCD"), true)),
            Admission::Context
        );
        // The mention rule decides whether an admitted message starts a Task.
        assert!(starts_task(
            route,
            Admission::Admitted,
            &msg(person("U0123ABCD"), true)
        ));
        assert!(!starts_task(
            route,
            Admission::Admitted,
            &msg(person("U0123ABCD"), false)
        ));
        assert!(!starts_task(
            route,
            Admission::Context,
            &msg(person("U0999ZZZZ"), true)
        ));
        let mut top = route.clone();
        top.mention = Mention::TopLevel;
        let mut starter = msg(bot, false);
        starter.starts_thread = true;
        assert!(starts_task(&top, Admission::Admitted, &starter));
        assert!(!starts_task(
            &top,
            Admission::Admitted,
            &msg(person("U0123ABCD"), true)
        ));
        // One refusal for an unlisted person who mentions Fleet; none for a bot.
        assert!(earns_refusal(
            Admission::Context,
            &msg(person("U0999ZZZZ"), true)
        ));
        assert!(!earns_refusal(
            Admission::Context,
            &msg(person("U0999ZZZZ"), false)
        ));
        assert!(!earns_refusal(Admission::Context, &msg(other_bot, true)));
        assert!(!earns_refusal(
            Admission::Admitted,
            &msg(person("U0123ABCD"), true)
        ));
    }
}
