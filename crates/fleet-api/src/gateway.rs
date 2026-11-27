use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::meta::Millis;
use crate::observation::Observation;
use crate::plan::EffectOutcome;
use crate::task::Workspace;
use crate::validate::{is_absolute_path, validate_name};

pub const HISTORY_FIRST_READ: u32 = 200;

pub const DEFAULT_REFUSAL: &str = "Sorry, I only take requests from people on this channel's list.";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationRef {
    pub platform: String,
    pub chat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

impl ConversationRef {
    pub fn address(&self) -> String {
        match &self.thread {
            Some(t) => format!("{}:{}:{}", self.platform, self.chat, t),
            None => format!("{}:{}", self.platform, self.chat),
        }
    }

    pub fn canonical(&self, account: &str) -> String {
        format!("{account}/{}", self.address())
    }

    pub fn channel(&self) -> ConversationRef {
        ConversationRef {
            platform: self.platform.clone(),
            chat: self.chat.clone(),
            thread: None,
        }
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthorKind {
    Person,
    Bot,
    Fleet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Author {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub kind: AuthorKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Message {
    pub id: String,
    pub author: Author,
    #[serde(default)]
    pub mentions_fleet: bool,
    pub at: Millis,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starts_thread: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Attachment {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mimetype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "outcome"
)]
pub enum Fetched {
    Saved {
        path: PathBuf,
        bytes: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mimetype: Option<String>,
    },
    Refused {
        why: String,
    },
}

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

    pub fn message_id(&self) -> &str {
        match self {
            Event::Message { message, .. } => &message.id,
            Event::Edited { id, .. } | Event::Removed { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verified {
    Connection,
    Signature,
    Fleet,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Envelope {
    pub verified: Verified,
    pub received_at: Millis,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Delivery {
    pub id: String,
    pub verified: Verified,
    pub received_at: Millis,
    pub event: Event,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub direct: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Marker {
    Metadata,
    Nonce { window_ms: Millis },
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capabilities {
    pub history: bool,
    pub marker: Marker,
    pub unmentioned: bool,
    pub threads: bool,
    #[serde(default)]
    pub reactions: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Identity {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum HistoryQuery {
    After { id: Option<String>, limit: u32 },
    Since { at: Millis },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Starter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AuthorKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct History {
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub left_out: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starter: Option<Starter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Uncertainty {
    Timeout,
    Incomplete,
    Throttled,
    Unreadable,
    Refused,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryObservation {
    Known(History),
    Unsupported,
    Unknown {
        kind: Uncertainty,
        why: String,
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

pub fn part_key(key: &str, n: usize, total: usize) -> String {
    format!("{key}/{n}/{total}")
}

pub fn part_of(marker: &str, key: &str) -> Option<(usize, usize)> {
    if marker == key {
        return Some((1, 1));
    }
    let rest = marker.strip_prefix(key)?.strip_prefix('/')?;
    let (n, total) = rest.split_once('/')?;
    let (n, total): (usize, usize) = (n.parse().ok()?, total.parse().ok()?);
    (n >= 1 && n <= total).then_some((n, total))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostFound {
    Whole { message: String, at: Millis },
    Partial { found: Vec<usize>, total: usize },
}

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

pub trait Gateway {
    fn operation_bound(&self) -> Millis;
    fn parse(&self, account: &str, address: &str) -> Observation<ConversationRef>;
    fn event(&self, account: &AccountConfig, envelope: &Envelope) -> Observation<Option<Delivery>>;
    fn whoami(&self, account: &str) -> Observation<Identity>;
    fn capabilities(&self, account: &str) -> Observation<Capabilities>;
    fn history(
        &self,
        account: &str,
        conversation: &ConversationRef,
        query: &HistoryQuery,
    ) -> HistoryObservation;
    fn open(
        &self,
        account: &str,
        channel: &ConversationRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome>;
    fn post(
        &self,
        account: &str,
        conversation: &ConversationRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome>;
    fn react(
        &self,
        account: &str,
        conversation: &ConversationRef,
        message: &str,
        reaction: &str,
        on: bool,
    ) -> Observation<EffectOutcome>;
    fn link(&self, account: &str, thread: &ConversationRef) -> Option<String> {
        let _ = (account, thread);
        None
    }
    fn fetch(
        &self,
        account: &str,
        conversation: &ConversationRef,
        file: &str,
        dir: &Path,
    ) -> Observation<Fetched> {
        let _ = (account, conversation, file, dir);
        Observation::Known(Fetched::Refused {
            why: "this platform's adapter fetches no files".into(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Inbound {
    SocketMode,
    EventsApi,
    Connection,
    DiscordGateway,
}

impl Inbound {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountConfig {
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub bot: String,
    pub inbound: Inbound,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_content: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curl_config: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorConfig {
    pub program: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_timeout_ms: Option<Millis>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mention {
    #[default]
    Required,
    TopLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteConfig {
    pub account: String,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub mention: Mention,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub allow_bots: Vec<String>,
    pub task_config: String,
    pub brief: PathBuf,
    pub workspace: RouteWorkspace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    untagged,
    expecting = "a route workspace: {\"base\": ABSOLUTE_DIR} or a list of absolute URLs"
)]
pub enum RouteWorkspace {
    Urls(Workspace),
    Base(BaseWorkspace),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseWorkspace {
    pub base: String,
}

impl RouteWorkspace {
    pub fn base(dir: impl Into<String>) -> RouteWorkspace {
        RouteWorkspace::Base(BaseWorkspace { base: dir.into() })
    }

    pub fn base_dir(&self) -> Option<&str> {
        match self {
            RouteWorkspace::Base(b) => Some(&b.base),
            RouteWorkspace::Urls(_) => None,
        }
    }
}

pub const ANY_CHANNEL: &str = "*";

pub const ANY_DM: &str = "dm:*";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Covered<'a> {
    pub name: &'a str,
    pub route: &'a RouteConfig,
    pub direct: bool,
}

impl RouteConfig {
    pub fn refusal(&self) -> &str {
        self.refusal.as_deref().unwrap_or(DEFAULT_REFUSAL)
    }

    pub fn is_wildcard(&self) -> bool {
        self.channels.iter().any(|c| c == ANY_CHANNEL)
    }

    pub fn covers_dms(&self) -> bool {
        self.channels.iter().any(|c| c == ANY_DM)
    }

    pub fn channel_ids(&self) -> impl Iterator<Item = &str> {
        self.channels
            .iter()
            .map(String::as_str)
            .filter(|c| *c != ANY_CHANNEL && *c != ANY_DM)
    }

    pub fn first_channel(&self) -> Option<&str> {
        self.channel_ids().next()
    }
}

pub type Person = BTreeMap<String, String>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_timeout_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beat_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connectors: BTreeMap<String, ConnectorConfig>,
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountConfig>,
    #[serde(default)]
    pub people: BTreeMap<String, Person>,
    #[serde(default)]
    pub routes: BTreeMap<String, RouteConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<LogsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<ReactionsConfig>,
}

pub const DEFAULT_WORKING: &str = "eyes";
pub const DEFAULT_REPLIED: &str = "white_check_mark";
pub const DEFAULT_QUEUED: &str = "hourglass_flowing_sand";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReactionsConfig {
    Enabled(bool),
    Names(ReactionNames),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReactionNames {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replied: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reactions {
    pub working: String,
    pub replied: String,
    pub queued: String,
}

impl Default for Reactions {
    fn default() -> Self {
        Reactions {
            working: DEFAULT_WORKING.into(),
            replied: DEFAULT_REPLIED.into(),
            queued: DEFAULT_QUEUED.into(),
        }
    }
}

impl ReactionsConfig {
    pub fn reactions(&self) -> Option<Reactions> {
        match self {
            ReactionsConfig::Enabled(false) => None,
            ReactionsConfig::Enabled(true) => Some(Reactions::default()),
            ReactionsConfig::Names(n) => {
                let base = Reactions::default();
                Some(Reactions {
                    working: n.working.clone().unwrap_or(base.working),
                    replied: n.replied.clone().unwrap_or(base.replied),
                    queued: n.queued.clone().unwrap_or(base.queued),
                })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogsConfig {
    pub account: String,
    pub channel: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogDestination {
    pub account: String,
    pub channel: ConversationRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogThread {
    pub account: String,
    pub conversation: ConversationRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayDefaults {
    pub call_timeout_ms: Millis,
    pub poll_ms: Millis,
    pub active_ms: Millis,
    pub retain_ms: Millis,
    pub beat_ms: Millis,
    pub route: Option<RouteConfig>,
    pub brief: Option<String>,
    pub people: BTreeMap<String, Person>,
    pub account: Option<ListenerAccount>,
    pub routes: BTreeMap<String, RouteConfig>,
    pub questions: Option<String>,
    pub platforms: BTreeMap<String, String>,
    pub logs: Option<LogDestination>,
    pub reactions: Option<Reactions>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerAccount {
    pub config: AccountConfig,
    pub digest: String,
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
            reactions: Some(Reactions::default()),
        }
    }
}

impl GatewayDefaults {
    pub fn for_route(&self, route: Option<RouteConfig>, brief: Option<String>) -> GatewayDefaults {
        GatewayDefaults {
            brief: brief.filter(|_| route.is_some()),
            route,
            account: None,
            ..self.clone()
        }
    }

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
            reactions: match &self.reactions {
                Some(r) => r.reactions(),
                None => Some(Reactions::default()),
            },
        }
    }

    pub fn is_routed(&self, account: &str) -> bool {
        self.routes.values().any(|r| r.account == account)
    }

    pub fn route_of(&self, account: &str, channel: &str) -> Option<(&str, &RouteConfig)> {
        self.route_for(account, channel, false)
            .map(|c| (c.name, c.route))
    }

    pub fn route_for(&self, account: &str, chat: &str, direct: bool) -> Option<Covered<'_>> {
        let routes = || self.routes.iter().filter(|(_, r)| r.account == account);
        let (found, direct) =
            if let Some(found) = routes().find(|(_, r)| r.channel_ids().any(|c| c == chat)) {
                (found, false)
            } else if self.is_log_channel(account, chat) {
                return None;
            } else if let Some(found) = routes().find(|(_, r)| direct && r.covers_dms()) {
                (found, true)
            } else {
                (routes().find(|(_, r)| r.is_wildcard())?, false)
            };
        Some(Covered {
            name: found.0.as_str(),
            route: found.1,
            direct,
        })
    }

    pub fn has_dm_route(&self, account: &str) -> bool {
        self.routes
            .values()
            .any(|r| r.account == account && r.covers_dms())
    }

    fn is_log_channel(&self, account: &str, channel: &str) -> bool {
        self.logs
            .as_ref()
            .is_some_and(|l| l.account == account && l.channel == channel)
    }

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
            if let Some(p) = &account.credentials
                && !is_absolute_path(&p.to_string_lossy())
            {
                return Err(format!("{at}: credentials must be an absolute path"));
            }
        }
        for (name, connector) in &self.connectors {
            let at = format!("gateway.connectors.{name}");
            validate_name(name).map_err(|e| format!("{at}: {e}"))?;
            if connector.program.trim().is_empty() {
                return Err(format!("{at}: program is empty"));
            }
            match connector.call_timeout_ms {
                Some(0) => return Err(format!("{at}: callTimeoutMs must be at least 1")),
                Some(ms) if ms > 300_000 => {
                    return Err(format!("{at}: callTimeoutMs must be at most 300000"));
                }
                _ => {}
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
        let mut wildcards: BTreeMap<&str, &str> = BTreeMap::new();
        let mut dm_routes: BTreeMap<&str, &str> = BTreeMap::new();
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
                if channel == ANY_CHANNEL {
                    if let Some(other) = wildcards.insert(route.account.as_str(), name.as_str()) {
                        return Err(format!(
                            "gateway.routes: routes {other} and {name} are both the wildcard route of account {:?}",
                            route.account
                        ));
                    }
                    continue;
                }
                if channel == ANY_DM {
                    if let Some(other) = dm_routes.insert(route.account.as_str(), name.as_str()) {
                        return Err(format!(
                            "gateway.routes: routes {other} and {name} are both the DM route of account {:?}",
                            route.account
                        ));
                    }
                    if self.accounts[&route.account].platform == "discord" {
                        return Err(format!(
                            "{at}: the Discord adapter reads no direct message; {ANY_DM:?} needs a slack or fake account"
                        ));
                    }
                    continue;
                }
                if let Some(other) =
                    channels.insert((route.account.as_str(), channel.as_str()), name.as_str())
                {
                    return Err(format!(
                        "gateway.routes: channel {channel:?} belongs to routes {other} and {name}"
                    ));
                }
            }
            if route.is_wildcard() && route.mention == Mention::TopLevel {
                return Err(format!(
                    "{at}: a wildcard route needs mention = \"required\""
                ));
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
            if let RouteWorkspace::Urls(w) = &route.workspace {
                crate::validate::validate_workspace(w)
                    .map_err(|e| format!("{at}: workspace{e}"))?;
            }
            let dir = route.workspace.base_dir().map(Path::new);
            if let Some(d) = route.workspace.base_dir()
                && !is_absolute_path(d)
            {
                return Err(format!("{at}: workspace.base must be an absolute path"));
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
        if let Some(q) = &self.questions {
            let Some(route) = self.routes.get(q) else {
                return Err(format!(
                    "gateway.questions {q:?} is not under [gateway.routes]"
                ));
            };
            if route.first_channel().is_none() {
                return Err(format!(
                    "gateway.questions {q:?} lists no channel id to open a thread in"
                ));
            }
        }
        if let Some(ReactionsConfig::Names(names)) = &self.reactions {
            for (field, name) in [
                ("working", &names.working),
                ("replied", &names.replied),
                ("queued", &names.queued),
            ] {
                if let Some(name) = name
                    && (name.trim_matches(':').is_empty() || name.chars().any(char::is_whitespace))
                {
                    return Err(format!(
                        "gateway.reactions.{field}: {name:?} is not one emoji name"
                    ));
                }
            }
            if let Some(r) = ReactionsConfig::Names(names.clone()).reactions() {
                for (a, b, x, y) in [
                    ("working", "replied", &r.working, &r.replied),
                    ("working", "queued", &r.working, &r.queued),
                    ("replied", "queued", &r.replied, &r.queued),
                ] {
                    if x.trim_matches(':') == y.trim_matches(':') {
                        return Err(format!(
                            "gateway.reactions: {a} and {b} name the same emoji"
                        ));
                    }
                }
            }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Admission {
    Own,
    Admitted,
    Context,
}

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

pub fn starts_task(
    route: &RouteConfig,
    direct: bool,
    admission: Admission,
    message: &Message,
) -> bool {
    admission == Admission::Admitted
        && if direct {
            message.starts_thread || message.mentions_fleet
        } else {
            match route.mention {
                Mention::Required => message.mentions_fleet,
                Mention::TopLevel => message.starts_thread,
            }
        }
}

pub fn earns_refusal(direct: bool, admission: Admission, message: &Message) -> bool {
    admission == Admission::Context
        && message.author.kind == AuthorKind::Person
        && (message.mentions_fleet || (direct && message.starts_thread))
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
            files: Vec::new(),
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
            credentials: None,
            options: Default::default(),
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
            workspace: RouteWorkspace::base(dir),
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
    fn reactions_default_on_turn_off_with_false_and_take_platform_names() {
        let parse = |v: serde_json::Value| -> GatewayConfig {
            let mut c = serde_json::to_value(config()).unwrap();
            c["reactions"] = v;
            serde_json::from_value(c).unwrap()
        };
        assert_eq!(config().defaults().reactions, Some(Reactions::default()));
        assert_eq!(
            Reactions::default(),
            Reactions {
                working: "eyes".into(),
                replied: "white_check_mark".into(),
                queued: "hourglass_flowing_sand".into(),
            }
        );
        assert_eq!(parse(json!(false)).defaults().reactions, None);
        assert_eq!(
            parse(json!(true)).defaults().reactions,
            Some(Reactions::default())
        );
        let named = parse(json!({"working": "gear", "queued": "pause_button"}));
        assert_eq!(
            named.defaults().reactions,
            Some(Reactions {
                working: "gear".into(),
                replied: "white_check_mark".into(),
                queued: "pause_button".into(),
            })
        );
        assert_eq!(named.validate(), Ok(()));
        for (bad, why) in [
            (json!({"replied": ""}), "gateway.reactions.replied"),
            (
                json!({"working": ":eyes:", "replied": "eyes"}),
                "the same emoji",
            ),
            (
                json!({"working": "eyes", "replied": "eyes"}),
                "the same emoji",
            ),
            (json!({"queued": " "}), "gateway.reactions.queued"),
            (
                json!({"working": "hourglass_flowing_sand"}),
                "working and queued name the same emoji",
            ),
            (
                json!({"queued": ":white_check_mark:"}),
                "replied and queued name the same emoji",
            ),
        ] {
            let err = parse(bad.clone()).validate().unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
        let mut c = serde_json::to_value(config()).unwrap();
        c["reactions"] = json!({"done": "x"});
        assert!(serde_json::from_value::<GatewayConfig>(c).is_err());
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
        // A Teams thread id holds colons.
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
                               "workspace": {"base": "/work/conversations"}}}
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
        let mut c = config();
        let urls: RouteWorkspace =
            serde_json::from_value(serde_json::json!(["git+https://example.invalid/app.git"]))
                .unwrap();
        c.routes.get_mut("eng").unwrap().workspace = urls.clone();
        let mut ops = route("/work");
        ops.channels = vec!["C0789IJKL".into()];
        ops.workspace = urls;
        c.routes.insert("ops".into(), ops);
        assert_eq!(c.validate(), Ok(()));
        let mut c = config();
        c.routes.get_mut("eng").unwrap().workspace =
            serde_json::from_value(serde_json::json!(["/work/app"])).unwrap();
        assert!(
            c.validate()
                .unwrap_err()
                .contains("routes.eng: workspace[0] \"/work/app\" is not an absolute URL")
        );
        for refused in [
            serde_json::json!({"dir": "/w"}),
            serde_json::json!({"repositories": []}),
            serde_json::json!({"base": "/w", "dir": "/w"}),
        ] {
            assert!(
                serde_json::from_value::<RouteWorkspace>(refused.clone()).is_err(),
                "{refused}"
            );
        }
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
        c.routes.get_mut("eng").unwrap().workspace = RouteWorkspace::base("work");
        assert!(
            c.validate()
                .unwrap_err()
                .contains("routes.eng: workspace.base must be an absolute path")
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
            "account": "a", "taskConfig": "t", "brief": "/b", "workspace": {"base": "/w"}, "colour": "red"
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
    fn a_wildcard_route_covers_every_channel_no_route_lists_but_the_log_channel() {
        let mut c = config();
        let mut any = route("/work/anywhere");
        any.channels = vec![ANY_CHANNEL.into()];
        c.routes.insert("any".into(), any.clone());
        c.logs = Some(LogsConfig {
            account: "fake-main".into(),
            channel: "C0456EFGH".into(),
        });
        assert_eq!(c.validate(), Ok(()));
        let route_name = |chat: &str| c.route_of("fake-main", chat).map(|(n, _)| n);
        assert_eq!(route_name("C0123ABCD"), Some("eng"));
        assert_eq!(route_name("C0999WILD"), Some("any"));
        assert_eq!(route_name("D0123ABCD"), Some("any"));
        assert_eq!(route_name("C0456EFGH"), None, "never the log channel");
        assert_eq!(c.route_of("other", "C0999WILD"), None, "one account only");
        let mut z = c.clone();
        let eng = z.routes.remove("eng").unwrap();
        z.routes.insert("zz".into(), eng);
        z.questions = Some("zz".into());
        assert_eq!(
            z.route_of("fake-main", "C0123ABCD").map(|(n, _)| n),
            Some("zz")
        );
        assert!(any.is_wildcard() && !c.routes["eng"].is_wildcard());
        assert_eq!(any.first_channel(), None);
        any.channels.push("C0789IJKL".into());
        assert_eq!(any.first_channel(), Some("C0789IJKL"));
        assert_eq!(any.channel_ids().collect::<Vec<_>>(), ["C0789IJKL"]);
        let mut bad = c.clone();
        bad.routes
            .get_mut("eng")
            .unwrap()
            .channels
            .push("C0456EFGH".into());
        assert!(
            bad.validate()
                .unwrap_err()
                .contains("the log channel must be no route's channel")
        );
        let mut two = c.clone();
        let mut second = route("/work/second");
        second.channels = vec![ANY_CHANNEL.into()];
        two.routes.insert("second".into(), second.clone());
        assert!(two.validate().unwrap_err().contains(
            "routes any and second are both the wildcard route of account \"fake-main\""
        ));
        let mut elsewhere = c.clone();
        elsewhere.accounts.insert("other".into(), account());
        elsewhere
            .people
            .get_mut("alex")
            .unwrap()
            .insert("other".into(), "U0123ABCD".into());
        second.account = "other".into();
        elsewhere.routes.insert("second".into(), second);
        assert_eq!(elsewhere.validate(), Ok(()));
        let mut top = c.clone();
        top.routes.get_mut("any").unwrap().mention = Mention::TopLevel;
        assert!(
            top.validate()
                .unwrap_err()
                .contains("routes.any: a wildcard route needs mention = \"required\"")
        );
        let mut q = c.clone();
        q.questions = Some("any".into());
        assert!(
            q.validate()
                .unwrap_err()
                .contains("gateway.questions \"any\" lists no channel id")
        );
        q.routes
            .get_mut("any")
            .unwrap()
            .channels
            .push("C0789IJKL".into());
        assert_eq!(q.validate(), Ok(()));
    }

    #[test]
    fn a_dm_route_covers_every_dm_no_route_lists_and_nothing_else() {
        let mut c = config();
        let mut any = route("/work/anywhere");
        any.channels = vec![ANY_CHANNEL.into(), ANY_DM.into()];
        c.routes.insert("any".into(), any.clone());
        c.logs = Some(LogsConfig {
            account: "fake-main".into(),
            channel: "C0456EFGH".into(),
        });
        assert_eq!(c.validate(), Ok(()));
        let covered = |c: &GatewayConfig, chat: &str, direct| {
            c.route_for("fake-main", chat, direct)
                .map(|k| (k.name.to_string(), k.direct))
        };
        assert_eq!(covered(&c, "D0123ALEX", true), Some(("any".into(), true)));
        assert_eq!(covered(&c, "G0123GRP", false), Some(("any".into(), false)));
        assert_eq!(covered(&c, "C0123ABCD", true), Some(("eng".into(), false)));
        assert_eq!(covered(&c, "C0456EFGH", true), None);
        assert_eq!(c.route_for("other", "D0123ALEX", true), None);
        assert!(c.has_dm_route("fake-main") && !c.has_dm_route("other"));
        let mut plain = c.clone();
        plain.routes.get_mut("any").unwrap().channels = vec![ANY_CHANNEL.into()];
        assert_eq!(
            covered(&plain, "D0123ALEX", true),
            Some(("any".into(), false))
        );
        assert!(!plain.has_dm_route("fake-main"));
        let mut only = c.clone();
        only.routes.get_mut("any").unwrap().channels = vec![ANY_DM.into()];
        assert_eq!(only.validate(), Ok(()));
        assert_eq!(covered(&only, "C0999WILD", false), None);
        assert_eq!(covered(&only, "D0123ALEX", false), None);
        assert_eq!(
            covered(&only, "D0123ALEX", true),
            Some(("any".into(), true))
        );
        assert!(any.covers_dms() && any.is_wildcard());
        assert!(!only.routes["any"].is_wildcard());
        assert_eq!(only.routes["any"].first_channel(), None);
        only.routes.get_mut("any").unwrap().mention = Mention::TopLevel;
        assert_eq!(only.validate(), Ok(()));
        let mut two = c.clone();
        let mut second = route("/work/second");
        second.channels = vec![ANY_DM.into()];
        two.routes.insert("second".into(), second);
        assert!(
            two.validate()
                .unwrap_err()
                .contains("routes any and second are both the DM route of account \"fake-main\"")
        );
        let mut q = only.clone();
        q.questions = Some("any".into());
        assert!(
            q.validate()
                .unwrap_err()
                .contains("gateway.questions \"any\" lists no channel id")
        );
        let mut discord = c.clone();
        discord.accounts.get_mut("fake-main").unwrap().platform = "discord".into();
        assert!(discord.validate().unwrap_err().contains(
            "routes.any: the Discord adapter reads no direct message; \"dm:*\" needs a slack or fake account"
        ));
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
        assert!(starts_task(
            route,
            false,
            Admission::Admitted,
            &msg(person("U0123ABCD"), true)
        ));
        assert!(!starts_task(
            route,
            false,
            Admission::Admitted,
            &msg(person("U0123ABCD"), false)
        ));
        assert!(!starts_task(
            route,
            false,
            Admission::Context,
            &msg(person("U0999ZZZZ"), true)
        ));
        let mut top = route.clone();
        top.mention = Mention::TopLevel;
        let mut starter = msg(bot, false);
        starter.starts_thread = true;
        assert!(starts_task(&top, false, Admission::Admitted, &starter));
        assert!(!starts_task(
            &top,
            false,
            Admission::Admitted,
            &msg(person("U0123ABCD"), true)
        ));
        assert!(earns_refusal(
            false,
            Admission::Context,
            &msg(person("U0999ZZZZ"), true)
        ));
        assert!(!earns_refusal(
            false,
            Admission::Context,
            &msg(person("U0999ZZZZ"), false)
        ));
        assert!(!earns_refusal(
            false,
            Admission::Context,
            &msg(other_bot.clone(), true)
        ));
        assert!(!earns_refusal(
            false,
            Admission::Admitted,
            &msg(person("U0123ABCD"), true)
        ));
        let mut dm = msg(person("U0123ABCD"), false);
        dm.starts_thread = true;
        assert!(starts_task(route, true, Admission::Admitted, &dm));
        assert!(!starts_task(route, false, Admission::Admitted, &dm));
        assert!(starts_task(
            route,
            true,
            Admission::Admitted,
            &msg(person("U0123ABCD"), true)
        ));
        assert!(!starts_task(
            route,
            true,
            Admission::Admitted,
            &msg(person("U0123ABCD"), false)
        ));
        assert!(!starts_task(route, true, Admission::Context, &dm));
        let mut stranger = msg(person("U0999ZZZZ"), false);
        stranger.starts_thread = true;
        assert!(earns_refusal(true, Admission::Context, &stranger));
        assert!(!earns_refusal(false, Admission::Context, &stranger));
        assert!(!earns_refusal(
            true,
            Admission::Context,
            &msg(person("U0999ZZZZ"), false)
        ));
        let mut bot_dm = msg(other_bot, false);
        bot_dm.starts_thread = true;
        assert!(!earns_refusal(true, Admission::Context, &bot_dm));
    }
}
