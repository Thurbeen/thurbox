//! Slack through an externally authenticated `curl`: the Web API behind the
//! Gateway port, as the adapters page maps it.
//!
//! - An address is `slack:<channel>:<parent ts>` for a thread, or
//!   `slack:<channel>` for the channel a thread is opened in.
//! - [`read_event`] reads a Socket Mode envelope or an Events API body and
//!   is pure: a reply is told from a parent by `thread_ts != ts`, Fleet's
//!   own posts by the account's bot user id, a mention by `<@bot>` in the
//!   text, and the marker from the message metadata.
//! - `whoami` and `capabilities` ask `auth.test`. `history` pages
//!   `conversations.replies` (or `conversations.history` for a channel)
//!   under a budget of calls, and answers Unknown rather than a partial
//!   list.
//! - `open` and `post` are `chat.postMessage` without and with `thread_ts`.
//!   Each post carries the intent key as message metadata; the text is
//!   converted from Fleet's Markdown subset to mrkdwn with `&`, `<` and `>`
//!   escaped, and split at [`POST_LIMIT`] characters under derived keys.
//! - A rate limit (HTTP 429, or `ratelimited`) is Unknown, with Slack's
//!   `Retry-After` where the answer is a history read.
//!
//! Every call is one bounded `curl` process through fleet-exec, killed with
//! its process tree past the bound. Fleet never constructs an Authorization
//! header: the bot token lives in the operator's curl config file, which
//! Fleet names with `--config` and never reads.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use fleet_api::gateway::{
    AccountConfig, Author, AuthorKind, Capabilities, ConversationRef, Delivery, Envelope, Event,
    Gateway, History, HistoryObservation, HistoryQuery, Identity, Inbound, Marker, Message,
    Starter, Uncertainty, part_key,
};
use fleet_api::plan::EffectOutcome;
use fleet_api::{Millis, Observation};
use fleet_exec::{Outcome, Spec, run};
use serde_json::{Map, Value, json};

pub use crate::curl::Reply;
use crate::curl::{Budget, landed, send_failure};
use crate::text::split;

pub const KIND: &str = "slack";
/// Every call is `POST https://slack.com/api/<method>` with a JSON body.
pub const API: &str = "https://slack.com/api";
/// The client an account runs without a `cli` of its own.
pub const DEFAULT_CLI: &str = "curl";
/// The most characters one post takes: Slack's recommended length. A longer
/// text is split into consecutive posts under derived keys.
pub const POST_LIMIT: usize = 4_000;
/// The most posts one text is split into: Slack truncates past 40,000
/// characters, so a text past this many chunks is refused, never cut.
pub const MAX_CHUNKS: usize = 10;
/// The `event_type` of the metadata every Fleet post carries; the intent
/// key rides in `event_payload.key`.
pub const METADATA_EVENT_TYPE: &str = "fleet_post";
/// The most calls one operation makes: a history read's pages and its
/// parent lookup, or a split post's chunks. The bound reported for the port
/// is this many worst-case calls.
pub const MAX_CALLS_PER_OPERATION: u32 = 10;
/// Messages per history page, the count Slack recommends.
const PAGE: u32 = 200;

/// What Slack offers on every account: a thread's history can be read, a
/// post's metadata is read back, every message in a channel the bot is in
/// is delivered, and conversations are threads.
pub const CAPABILITIES: Capabilities = Capabilities {
    history: true,
    marker: Marker::Metadata,
    unmentioned: true,
    threads: true,
};

/// One Slack workspace, served by `curl` with the operator's config file.
#[derive(Debug, Clone)]
pub struct SlackGateway {
    account: String,
    team: Option<String>,
    bot: String,
    cli: OsString,
    curl_config: PathBuf,
    call_timeout: Duration,
}

impl SlackGateway {
    /// The adapter for an account, or why its configuration cannot be
    /// served: an `inbound` no Slack listener delivers by, or no
    /// `curlConfig`. The pure rules ran at config load.
    pub fn new(
        account: &str,
        config: &AccountConfig,
        call_timeout: Duration,
    ) -> Result<SlackGateway, String> {
        if !matches!(config.inbound, Inbound::SocketMode | Inbound::EventsApi) {
            return Err(format!(
                "inbound {:?} is not how a Slack listener delivers; use \"socket-mode\" or \"events-api\"",
                config.inbound.name()
            ));
        }
        let curl_config = config
            .curl_config
            .clone()
            .ok_or_else(|| "a slack account needs a curlConfig".to_string())?;
        Ok(SlackGateway {
            account: account.to_string(),
            team: config.team.clone(),
            bot: config.bot.clone(),
            cli: config.cli.as_deref().unwrap_or(DEFAULT_CLI).into(),
            curl_config,
            call_timeout,
        })
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    /// One bounded call: `curl --include … --config <file> --data-binary @-
    /// --url <API>/<method>` with the body on stdin, so neither the text nor
    /// anything from the config file is on a command line. Only Slack's write
    /// methods read a JSON body; the read methods take form arguments and
    /// answer a JSON body with `invalid_arguments`.
    fn call(&self, method: &str, body: &Value, timeout: Duration) -> Result<Value, CallError> {
        let (content_type, payload) = if method.starts_with("chat.") {
            (
                "Content-Type: application/json; charset=utf-8",
                serde_json::to_vec(body).expect("a JSON value serialises"),
            )
        } else {
            (
                "Content-Type: application/x-www-form-urlencoded",
                form(body),
            )
        };
        let args: Vec<OsString> = [
            "--include",
            "--silent",
            "--show-error",
            "--suppress-connect-headers",
            "--proto",
            "=https",
            "--config",
        ]
        .into_iter()
        .map(OsString::from)
        .chain([self.curl_config.clone().into_os_string()])
        .chain(
            [
                "--request",
                "POST",
                "--header",
                content_type,
                // No `100 Continue` round trip: the whole answer is one head.
                "--header",
                "Expect:",
                "--data-binary",
                "@-",
                "--url",
            ]
            .into_iter()
            .map(OsString::from),
        )
        .chain([OsString::from(format!("{API}/{method}"))])
        .collect();
        let spec = Spec::new(&self.cli, timeout).args(args).stdin(payload);
        match run(&spec) {
            Outcome::Exited {
                code,
                stdout,
                stderr,
            } => {
                let reply = Reply::parse(&String::from_utf8_lossy(&stdout), code, &stderr);
                reply.answer(method)
            }
            Outcome::TimedOut { .. } => Err(CallError::Timeout(format!(
                "{method}: timed out after {}ms; its process tree was killed",
                timeout.as_millis()
            ))),
            Outcome::NotStarted(why) => Err(CallError::Unavailable(format!(
                "{method}: {} could not start: {why}",
                self.cli.to_string_lossy()
            ))),
        }
    }

    /// One call of a budgeted operation.
    fn budgeted(
        &self,
        method: &str,
        body: &Value,
        budget: &mut Budget,
    ) -> Result<Value, CallError> {
        let timeout = budget.next_timeout().ok_or_else(|| {
            CallError::Incomplete(format!(
                "the bound ran out before {method} was read through"
            ))
        })?;
        self.call(method, body, timeout)
    }

    /// Every page of a paginated read, under the budget. `has_more` with no
    /// cursor is Incomplete, never a partial list.
    fn pages(
        &self,
        method: &str,
        base: &Value,
        budget: &mut Budget,
    ) -> Result<Vec<Value>, CallError> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut body = base.clone();
            if let Some(c) = &cursor {
                body["cursor"] = json!(c);
            }
            let v = self.budgeted(method, &body, budget)?;
            let Some(page) = v["messages"].as_array() else {
                return Err(CallError::Unreadable(format!(
                    "{method}: the answer has no messages array"
                )));
            };
            items.extend(page.iter().cloned());
            let next = v["response_metadata"]["next_cursor"]
                .as_str()
                .filter(|c| !c.is_empty())
                .map(str::to_string);
            match next {
                Some(c) => cursor = Some(c),
                None if v["has_more"] == true => {
                    return Err(CallError::Incomplete(format!(
                        "{method}: more messages remain but no cursor was given"
                    )));
                }
                None => return Ok(items),
            }
        }
    }

    fn thread_history(&self, chat: &str, thread: &str, query: &HistoryQuery) -> HistoryObservation {
        let mut budget = Budget::new(self.call_timeout, MAX_CALLS_PER_OPERATION);
        let mut base = json!({
            "channel": chat,
            "ts": thread,
            "limit": PAGE,
            "include_all_metadata": true,
        });
        match query {
            HistoryQuery::After { id: Some(id), .. } => base["oldest"] = json!(id),
            HistoryQuery::After { id: None, .. } => {}
            HistoryQuery::Since { at } => {
                base["oldest"] = json!(ts_of_millis(*at));
                base["inclusive"] = json!(true);
            }
        }
        let items = match self.pages("conversations.replies", &base, &mut budget) {
            Ok(items) => items,
            Err(e) => return e.history(),
        };
        let is_parent = |m: &&Value| m["ts"].as_str() == Some(thread);
        let mut parent = items.iter().find(is_parent).cloned();
        if parent.is_none() && !matches!(query, HistoryQuery::After { id: None, .. }) {
            // A read bounded by `oldest` may leave the parent out: ask for
            // it alone, so `starter` is always answered.
            let alone =
                json!({"channel": chat, "ts": thread, "limit": 1, "include_all_metadata": true});
            match self.budgeted("conversations.replies", &alone, &mut budget) {
                Ok(v) => {
                    parent = v["messages"]
                        .as_array()
                        .and_then(|a| a.iter().find(is_parent).cloned());
                }
                Err(e) => return e.history(),
            }
        }
        let Some(parent) = parent else {
            return HistoryObservation::unknown(
                Uncertainty::Unreadable,
                "conversations.replies did not return the thread's parent",
            );
        };
        match collect_thread(&self.bot, &items, &parent, query) {
            Ok(h) => HistoryObservation::Known(h),
            Err(why) => HistoryObservation::unknown(Uncertainty::Unreadable, why),
        }
    }

    fn channel_history(&self, chat: &str, query: &HistoryQuery) -> HistoryObservation {
        let mut budget = Budget::new(self.call_timeout, MAX_CALLS_PER_OPERATION);
        let mut base = json!({"channel": chat, "limit": PAGE, "include_all_metadata": true});
        match query {
            HistoryQuery::After { id: Some(id), .. } => base["oldest"] = json!(id),
            HistoryQuery::After { id: None, .. } => {}
            HistoryQuery::Since { at } => {
                base["oldest"] = json!(ts_of_millis(*at));
                base["inclusive"] = json!(true);
            }
        }
        let items = match self.pages("conversations.history", &base, &mut budget) {
            Ok(items) => items,
            Err(e) => return e.history(),
        };
        match collect(&self.bot, items.iter(), query) {
            Ok((messages, left_out)) => HistoryObservation::Known(History {
                messages,
                left_out,
                starter: None,
            }),
            Err(why) => HistoryObservation::unknown(Uncertainty::Unreadable, why),
        }
    }

    /// `chat.postMessage` once: the posted message's `ts`, and its time.
    fn post_once(
        &self,
        chat: &str,
        thread: Option<&str>,
        text: &str,
        key: &str,
        budget: &mut Budget,
    ) -> Result<(String, Millis), CallError> {
        let mut body = json!({
            "channel": chat,
            "text": text,
            "mrkdwn": true,
            "unfurl_links": false,
            "unfurl_media": false,
            "metadata": {"event_type": METADATA_EVENT_TYPE, "event_payload": {"key": key}},
        });
        if let Some(t) = thread {
            body["thread_ts"] = json!(t);
        }
        let v = self.budgeted("chat.postMessage", &body, budget)?;
        let ts = v["ts"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| {
            CallError::Unreadable("chat.postMessage answered ok without a ts".into())
        })?;
        let at = ts_millis(ts).ok_or_else(|| {
            CallError::Unreadable(format!(
                "chat.postMessage answered ts {ts:?}, which is not a Slack timestamp"
            ))
        })?;
        Ok((ts.to_string(), at))
    }

    /// Post `text` as one or more chunks, the first under `key` alone when
    /// it is the only one, else each under `<key>/<n>/<total>`. `thread` is the
    /// thread to reply in, or `None` to open one with the first chunk, which
    /// the rest then reply to. The first chunk's `ts` and time come back.
    fn post_chunks(
        &self,
        chat: &str,
        thread: Option<&str>,
        text: &str,
        key: &str,
    ) -> Observation<EffectOutcome> {
        let chunks = split(&to_mrkdwn(text), POST_LIMIT);
        if chunks.len() > MAX_CHUNKS {
            return Observation::Known(EffectOutcome::Failed {
                why: format!(
                    "the text would take {} posts; the most one post takes is {MAX_CHUNKS} of {POST_LIMIT} characters",
                    chunks.len()
                ),
            });
        }
        let derived = chunks.len() > 1;
        let mut budget = Budget::new(self.call_timeout, MAX_CALLS_PER_OPERATION);
        let mut first: Option<(String, Millis)> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            let chunk_key = if derived {
                part_key(key, i + 1, chunks.len())
            } else {
                key.to_string()
            };
            let reply_to = thread.or(first.as_ref().map(|(ts, _)| ts.as_str()));
            match self.post_once(chat, reply_to, chunk, &chunk_key, &mut budget) {
                Ok(landed) => {
                    first.get_or_insert(landed);
                }
                Err(e) => {
                    // Something landed: the outcome is for a recovery read
                    // to settle, whatever Slack said last.
                    let partly = (i > 0)
                        .then(|| format!("{i} of {} posts landed under {key}", chunks.len()));
                    return send_failure(partly, e.why(), e.certain_failure());
                }
            }
        }
        let (message, at) = first.expect("at least one chunk");
        Observation::Known(landed(KIND, chat, thread, message, at))
    }
}

crate::curl::curl_gateway!(SlackGateway, "Slack");

impl SlackGateway {
    fn identity(&self) -> Observation<Identity> {
        let v = match self.call("auth.test", &json!({}), self.call_timeout) {
            Ok(v) => v,
            Err(e) => return Observation::unknown(e.why()),
        };
        let Some(id) = v["user_id"].as_str().filter(|s| !s.is_empty()) else {
            return Observation::unknown("auth.test named no user_id");
        };
        if let Some(team) = &self.team
            && v["team_id"] != *team
            && v["enterprise_id"] != *team
        {
            return Observation::unknown(format!(
                "auth.test answered for team {}, not the account's {team:?}",
                v["team_id"]
            ));
        }
        Observation::Known(Identity {
            id: id.to_string(),
            name: v["user"].as_str().map(str::to_string),
        })
    }

    /// Slack's capabilities are the platform's, the same on every account;
    /// `auth.test` proves the account answers before they are claimed.
    fn offers(&self) -> Observation<Capabilities> {
        match self.call("auth.test", &json!({}), self.call_timeout) {
            Ok(_) => Observation::Known(CAPABILITIES),
            Err(e) => Observation::unknown(e.why()),
        }
    }
}

// ----------------------------------------------------------------------
// Addresses and timestamps.
// ----------------------------------------------------------------------

/// `slack:<channel>[:<ts>]`: a channel id (`C…`, `G…` or `D…`, uppercase
/// letters and digits) and a Slack timestamp (`<seconds>.<fraction>`).
/// A body as Slack's form arguments: strings as they are, numbers and
/// booleans as written, anything else as its JSON text.
fn form(body: &Value) -> Vec<u8> {
    fn encode(out: &mut String, s: &str) {
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                out.push(char::from(b));
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    let mut out = String::new();
    for (k, v) in body.as_object().into_iter().flatten() {
        if !out.is_empty() {
            out.push('&');
        }
        encode(&mut out, k);
        out.push('=');
        match v {
            Value::String(s) => encode(&mut out, s),
            other => encode(&mut out, &other.to_string()),
        }
    }
    out.into_bytes()
}

pub fn parse_address(address: &str) -> Result<ConversationRef, String> {
    let parsed = ConversationRef::split(address)
        .ok_or_else(|| format!("{address:?} is not <platform>:<chat>[:<thread>]"))?;
    if parsed.platform != KIND {
        return Err(format!(
            "{address:?} names platform {:?}, not {KIND}",
            parsed.platform
        ));
    }
    check_ref(&parsed).map_err(|why| format!("{address:?}: {why}"))?;
    Ok(parsed)
}

/// The parts of a reference are a Slack channel id and, when present, a
/// Slack timestamp.
fn check_ref(r: &ConversationRef) -> Result<(), String> {
    if r.platform != KIND {
        return Err(format!("platform {:?} is not {KIND}", r.platform));
    }
    if !is_channel_id(&r.chat) {
        return Err(format!("{:?} is not a Slack channel id", r.chat));
    }
    if let Some(t) = &r.thread
        && ts_key(t).is_none()
    {
        return Err(format!("{t:?} is not a Slack message timestamp"));
    }
    Ok(())
}

fn is_channel_id(s: &str) -> bool {
    s.len() >= 3
        && s.starts_with(['C', 'G', 'D'])
        && s.chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
}

/// A Slack timestamp as (seconds, microseconds): `1730000000.000100`.
pub fn ts_key(ts: &str) -> Option<(u64, u64)> {
    let (secs, frac) = ts.split_once('.')?;
    if secs.is_empty()
        || frac.is_empty()
        || frac.len() > 6
        || !secs.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let micros: u64 = format!("{frac:0<6}").parse().ok()?;
    Some((secs.parse().ok()?, micros))
}

/// A Slack timestamp as Unix milliseconds, the port's time.
pub fn ts_millis(ts: &str) -> Option<Millis> {
    let (secs, micros) = ts_key(ts)?;
    secs.checked_mul(1000)?.checked_add(micros / 1000)
}

/// Unix milliseconds as a Slack timestamp, for `oldest`.
pub fn ts_of_millis(at: Millis) -> String {
    format!("{}.{:06}", at / 1000, (at % 1000) * 1000)
}

/// `a` is later than `b`, both Slack timestamps.
fn ts_after(a: &str, b: &str) -> bool {
    match (ts_key(a), ts_key(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

// ----------------------------------------------------------------------
// Reading messages and events, pure.
// ----------------------------------------------------------------------

/// Message subtypes the Gateway reads as messages; every other subtype is
/// a system note, a change notice or a tombstone.
fn is_message_subtype(subtype: Option<&str>) -> bool {
    matches!(
        subtype,
        None | Some("file_share" | "thread_broadcast" | "bot_message")
    )
}

/// One Slack message object, from an event or from history, in the port's
/// words. The account's `bot` is the one author that is Fleet, and its own
/// posts mention nobody; a message with a `bot_id`, or of subtype
/// `bot_message`, is another bot's, under its bot user id where Slack gives
/// one and its `bot_id` otherwise.
pub fn message_of(bot: &str, item: &Value) -> Result<Message, String> {
    let ts = item["ts"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or("a message has no ts")?;
    let at = ts_millis(ts).ok_or_else(|| format!("message ts {ts:?} is not a Slack timestamp"))?;
    let user = item["user"].as_str().filter(|u| !u.is_empty());
    let bot_id = item["bot_id"].as_str().filter(|b| !b.is_empty());
    let subtype = item["subtype"].as_str();
    let name = name_of(item);
    let author = match (user, bot_id) {
        (Some(u), _) if u == bot => Author {
            id: u.into(),
            name,
            kind: AuthorKind::Fleet,
        },
        (Some(u), None) if subtype != Some("bot_message") => Author {
            id: u.into(),
            name,
            kind: AuthorKind::Person,
        },
        (Some(u), _) => Author {
            id: u.into(),
            name,
            kind: AuthorKind::Bot,
        },
        (None, Some(b)) => Author {
            id: b.into(),
            name,
            kind: AuthorKind::Bot,
        },
        (None, None) => return Err(format!("message {ts} has neither a user nor a bot_id")),
    };
    let text = item["text"].as_str().unwrap_or("").to_string();
    let thread_ts = item["thread_ts"].as_str();
    Ok(Message {
        id: ts.to_string(),
        // Fleet's own post never mentions Fleet, whatever it quotes.
        mentions_fleet: author.kind != AuthorKind::Fleet && mentions(&text, bot),
        at,
        key: key_of(item),
        starts_thread: thread_ts.is_none_or(|t| t == ts),
        text,
        author,
    })
}

/// `<@U…>` or `<@U…|name>` with Fleet's bot user id.
fn mentions(text: &str, bot: &str) -> bool {
    text.contains(&format!("<@{bot}>")) || text.contains(&format!("<@{bot}|"))
}

/// The intent key a Fleet post carries as its metadata.
fn key_of(item: &Value) -> Option<String> {
    let metadata = &item["metadata"];
    (metadata["event_type"] == METADATA_EVENT_TYPE)
        .then(|| metadata["event_payload"]["key"].as_str())
        .flatten()
        .map(str::to_string)
}

fn name_of(item: &Value) -> String {
    item["username"]
        .as_str()
        .or_else(|| item["user_profile"]["display_name"].as_str())
        .or_else(|| item["user_profile"]["real_name"].as_str())
        .unwrap_or("")
        .to_string()
}

/// The messages of a thread read, oldest first, with the parent's
/// `starter`: a `tombstone` parent is one Slack reports deleted. `items`
/// may hold the parent on every page, or not at all when `oldest` left it
/// out; it is counted once either way, and only when the query covers it.
pub fn collect_thread(
    bot: &str,
    items: &[Value],
    parent: &Value,
    query: &HistoryQuery,
) -> Result<History, String> {
    let starter = if parent["subtype"] == "tombstone" {
        Starter::removed()
    } else {
        Starter::message(&message_of(bot, parent)?)
    };
    let (messages, left_out) = collect(bot, items.iter().chain([parent]), query)?;
    Ok(History {
        messages,
        left_out,
        starter: Some(starter),
    })
}

/// The messages among `items` the query covers, each once, oldest first,
/// and how many older ones an `After` read left out under its limit.
fn collect<'a>(
    bot: &str,
    items: impl Iterator<Item = &'a Value>,
    query: &HistoryQuery,
) -> Result<(Vec<Message>, u64), String> {
    let mut seen = BTreeSet::new();
    let mut messages = Vec::new();
    for item in items {
        if !is_message_subtype(item["subtype"].as_str()) {
            continue;
        }
        let m = message_of(bot, item)?;
        let covered = match query {
            HistoryQuery::After { id: Some(id), .. } => ts_after(&m.id, id),
            HistoryQuery::After { id: None, .. } => true,
            HistoryQuery::Since { at } => m.at >= *at,
        };
        if !covered || !seen.insert(m.id.clone()) {
            continue;
        }
        messages.push(m);
    }
    messages.sort_by_key(|m| ts_key(&m.id));
    let left_out = match query {
        HistoryQuery::After { limit, .. } => {
            let left_out = messages.len().saturating_sub(*limit as usize);
            messages.drain(..left_out);
            left_out as u64
        }
        HistoryQuery::Since { .. } => 0,
    };
    Ok((messages, left_out))
}

/// What an envelope carries, read without any call. The payload is a
/// Socket Mode envelope (`type: events_api` around an `event_callback`)
/// or an Events API body (the `event_callback` itself). Other envelope
/// types (`hello`, `disconnect`, `interactive`, `slash_commands`,
/// `url_verification`), events that are not messages, message subtypes
/// that are system notes, and events of another workspace are ignored. A
/// payload that is no object, has no type, or has a message event without
/// its `event_id`, `channel` or `ts` is Unknown.
pub fn read_event(account: &AccountConfig, envelope: &Envelope) -> Observation<Option<Delivery>> {
    let Some(outer) = envelope.payload.as_object() else {
        return Observation::unknown("the payload is not an object");
    };
    let inner: &Map<String, Value> = match outer.get("type").and_then(Value::as_str) {
        Some("events_api") => match outer.get("payload").and_then(Value::as_object) {
            Some(p) => p,
            None => return Observation::unknown("the Socket Mode envelope has no payload object"),
        },
        Some("event_callback") => outer,
        Some(_) => return Observation::Known(None),
        None => return Observation::unknown("the payload has no type"),
    };
    if inner.get("type").and_then(Value::as_str) != Some("event_callback") {
        return Observation::Known(None);
    }
    if let Some(team) = &account.team {
        let of = |k: &str| inner.get(k).and_then(Value::as_str);
        if of("team_id") != Some(team) && of("enterprise_id") != Some(team) {
            return Observation::Known(None);
        }
    }
    let Some(event_id) = inner
        .get("event_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Observation::unknown("the event has no event_id");
    };
    let Some(ev) = inner.get("event").and_then(Value::as_object) else {
        return Observation::unknown("the event_callback has no event object");
    };
    if ev.get("type").and_then(Value::as_str) != Some("message") {
        return Observation::Known(None);
    }
    let Some(channel) = ev
        .get("channel")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Observation::unknown("the message event has no channel");
    };
    let conversation = |thread: &str| ConversationRef {
        platform: KIND.into(),
        chat: channel.into(),
        thread: Some(thread.into()),
    };
    let subtype = str_of(ev, "subtype");
    let event = if is_message_subtype(subtype) {
        let message = match message_of(&account.bot, &Value::Object(ev.clone())) {
            Ok(m) => m,
            Err(why) => return Observation::unknown(why),
        };
        let thread = str_of(ev, "thread_ts").unwrap_or(&message.id).to_string();
        Event::Message {
            conversation: conversation(&thread),
            message,
        }
    } else {
        match subtype {
            Some("message_changed") => {
                let Some(m) = ev.get("message").and_then(Value::as_object) else {
                    return Observation::unknown("message_changed has no message object");
                };
                let Some(id) = str_of(m, "ts") else {
                    return Observation::unknown("the changed message has no ts");
                };
                let edited = m
                    .get("edited")
                    .and_then(|e| e.get("ts"))
                    .and_then(Value::as_str)
                    .or_else(|| str_of(ev, "ts"));
                let Some(edited_at) = edited.and_then(ts_millis) else {
                    return Observation::unknown("message_changed has no readable edit time");
                };
                let thread = str_of(m, "thread_ts").unwrap_or(id);
                Event::Edited {
                    conversation: conversation(thread),
                    id: id.into(),
                    text: str_of(m, "text").unwrap_or("").into(),
                    edited_at,
                }
            }
            Some("message_deleted") => {
                let Some(id) = str_of(ev, "deleted_ts") else {
                    return Observation::unknown("message_deleted has no deleted_ts");
                };
                let thread = ev
                    .get("previous_message")
                    .and_then(|p| p.get("thread_ts"))
                    .and_then(Value::as_str)
                    .unwrap_or(id);
                Event::Removed {
                    conversation: conversation(thread),
                    id: id.into(),
                }
            }
            // `message_replied`, `channel_join`, `tombstone` and the rest.
            _ => return Observation::Known(None),
        }
    };
    Observation::Known(Some(Delivery {
        id: event_id.into(),
        verified: envelope.verified,
        received_at: envelope.received_at,
        event,
    }))
}

// ----------------------------------------------------------------------
// One call's answer.
// ----------------------------------------------------------------------

/// Why a call gave no usable answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// Slack asked to slow down (HTTP 429, or `ratelimited`): nothing was
    /// done, and `retry_after` is its `Retry-After` in milliseconds.
    Throttled { retry_after: Option<Millis> },
    /// Slack answered `ok: false` with this error: the request was not
    /// performed. Not `internal_error` or `fatal_error`, which Slack
    /// documents as possibly performed in part: those are `Unreadable`.
    Refused(String),
    /// No answer that tells whether the request was performed: an
    /// unreadable body, or Slack's `internal_error`/`fatal_error`.
    Unreadable(String),
    /// The call ran past its bound and its process tree was killed.
    Timeout(String),
    /// The budget of calls ran out before the read was complete.
    Incomplete(String),
    /// `curl` could not start, so nothing was sent.
    Unavailable(String),
}

impl CallError {
    pub fn why(&self) -> String {
        match self {
            CallError::Throttled { retry_after } => match retry_after {
                Some(ms) => format!("Slack asked to slow down; retry after {ms}ms"),
                None => "Slack asked to slow down".into(),
            },
            CallError::Refused(why)
            | CallError::Unreadable(why)
            | CallError::Timeout(why)
            | CallError::Incomplete(why)
            | CallError::Unavailable(why) => why.clone(),
        }
    }

    pub fn uncertainty(&self) -> Uncertainty {
        match self {
            CallError::Throttled { .. } => Uncertainty::Throttled,
            CallError::Refused(_) => Uncertainty::Refused,
            CallError::Unreadable(_) => Uncertainty::Unreadable,
            CallError::Timeout(_) => Uncertainty::Timeout,
            CallError::Incomplete(_) => Uncertainty::Incomplete,
            CallError::Unavailable(_) => Uncertainty::Unavailable,
        }
    }

    /// Slack said no, or curl never ran: the write was certainly not made.
    fn certain_failure(&self) -> bool {
        matches!(self, CallError::Refused(_) | CallError::Unavailable(_))
    }

    fn history(&self) -> HistoryObservation {
        HistoryObservation::Unknown {
            kind: self.uncertainty(),
            why: self.why(),
            retry_after: match self {
                CallError::Throttled { retry_after } => *retry_after,
                _ => None,
            },
        }
    }
}

impl Reply {
    /// `Retry-After` in seconds, as milliseconds, at least one second.
    pub fn retry_after(&self) -> Option<Millis> {
        self.header("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|secs| secs.saturating_mul(1000).max(1000))
    }

    /// The Web API's answer: its JSON when `ok`, else why not.
    pub fn answer(&self, method: &str) -> Result<Value, CallError> {
        if self.status == 429 {
            return Err(CallError::Throttled {
                retry_after: self.retry_after(),
            });
        }
        let v: Value = match serde_json::from_str(self.body.trim()) {
            Ok(v) => v,
            Err(e) => {
                return Err(CallError::Unreadable(match (self.exit, self.status) {
                    (Some(0) | None, 0) => format!("{method}: no answer: {e}"),
                    (Some(0) | None, status) => {
                        format!("{method}: HTTP {status} with an unreadable body: {e}")
                    }
                    (Some(code), _) => {
                        format!("{method}: curl exited {code}: {}", self.stderr)
                    }
                }));
            }
        };
        if v["ok"] == true {
            return Ok(v);
        }
        let error = v["error"].as_str().unwrap_or("an error without a name");
        if error == "ratelimited" {
            return Err(CallError::Throttled {
                retry_after: self.retry_after(),
            });
        }
        if v["ok"] == false {
            // Slack documents these two as "some aspect of the operation
            // may have succeeded": the request may have been performed.
            if matches!(error, "internal_error" | "fatal_error") {
                return Err(CallError::Unreadable(format!(
                    "{method}: {error}; Slack says some aspect of the operation may have succeeded"
                )));
            }
            return Err(CallError::Refused(format!("{method}: {error}")));
        }
        Err(CallError::Unreadable(format!(
            "{method}: the answer has no ok field"
        )))
    }
}

// ----------------------------------------------------------------------
// Text: Fleet's Markdown subset to mrkdwn, and splitting.
// ----------------------------------------------------------------------

/// `&`, `<` and `>` as Slack wants them in text.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// Fleet's Markdown subset (paragraphs, emphasis, code, links, lists and
/// quotes) as mrkdwn: `**bold**` to `*bold*`, `*em*` and `_em_` to `_em_`,
/// `~~gone~~` to `~gone~`, `[text](url)` to `<url|text>`, a heading to a
/// bold line, a bullet to `•`, a quote line to `&gt;`, code spans and
/// fences kept, and everything else escaped. Text inside code is escaped
/// and never formatted. A fence left open is closed.
pub fn to_mrkdwn(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut in_fence = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out.push_str("```\n");
            continue;
        }
        if in_fence {
            out.push_str(&escape(line));
            out.push('\n');
            continue;
        }
        let indent = &line[..line.len() - trimmed.len()];
        let converted = if let Some(rest) = heading(trimmed) {
            format!("*{}*", inline(rest))
        } else if let Some(rest) = bullet(trimmed) {
            format!("{indent}• {}", inline(rest))
        } else if let Some((number, rest)) = ordered(trimmed) {
            format!("{indent}{number}. {}", inline(rest))
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            format!("&gt; {}", inline(rest.trim_start()))
        } else {
            format!("{indent}{}", inline(trimmed))
        };
        out.push_str(&converted);
        out.push('\n');
    }
    if in_fence {
        out.push_str("```\n");
    }
    out.trim_end_matches('\n').to_string()
}

fn heading(line: &str) -> Option<&str> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if rest.starts_with(' ') {
            return Some(rest.trim());
        }
    }
    None
}

fn bullet(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest);
        }
    }
    None
}

fn ordered(line: &str) -> Option<(&str, &str)> {
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let rest = line[digits..].strip_prefix(". ")?;
    Some((&line[..digits], rest))
}

/// Inline Markdown of one line as mrkdwn.
fn inline(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // A code span: kept, its text escaped, never formatted.
        if c == '`'
            && let Some(end) = find(&chars, i + 1, &['`'])
        {
            let code: String = chars[i + 1..end].iter().collect();
            out.push('`');
            out.push_str(&escape(&code));
            out.push('`');
            i = end + 1;
            continue;
        }
        // A link: `[text](url)` to `<url|text>` for a web or mail URL.
        if c == '['
            && let Some(close) = find(&chars, i + 1, &[']'])
            && chars.get(close + 1) == Some(&'(')
            && let Some(end) = find(&chars, close + 2, &[')'])
        {
            let label: String = chars[i + 1..close].iter().collect();
            let url: String = chars[close + 2..end].iter().collect();
            if is_link_url(&url) && !label.is_empty() {
                out.push('<');
                out.push_str(&escape(&url));
                out.push('|');
                out.push_str(&inline(&label));
                out.push('>');
                i = end + 1;
                continue;
            }
        }
        if let Some(next) = emphasis(&chars, i, &mut out) {
            i = next;
            continue;
        }
        out.push_str(&escape(&c.to_string()));
        i += 1;
    }
    out
}

/// An emphasis span opening at `i`, pushed to `out` as mrkdwn, and where
/// it ends: a double marker is bold, a single one italic, a double tilde
/// strikethrough. A marker opens only at a word start before a non-space,
/// closes only after a non-space at a word end, and is literal otherwise.
fn emphasis(chars: &[char], i: usize, out: &mut String) -> Option<usize> {
    const MARKERS: [(&[char], char); 5] = [
        (&['*', '*'], '*'),
        (&['_', '_'], '*'),
        (&['~', '~'], '~'),
        (&['*'], '_'),
        (&['_'], '_'),
    ];
    if !word_start(chars, i) {
        return None;
    }
    for (marker, wrap) in MARKERS {
        if !chars[i..].starts_with(marker) {
            continue;
        }
        let from = i + marker.len();
        if chars.get(from).is_none_or(|c| c.is_whitespace()) {
            continue;
        }
        let Some(end) = find_seq(chars, from, marker) else {
            continue;
        };
        if end == from || chars[end - 1].is_whitespace() || !word_end(chars, end + marker.len()) {
            continue;
        }
        let inner: String = chars[from..end].iter().collect();
        out.push(wrap);
        out.push_str(&inline(&inner));
        out.push(wrap);
        return Some(end + marker.len());
    }
    None
}

/// A string field of a JSON object.
fn str_of<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    m.get(k).and_then(Value::as_str)
}

fn find(chars: &[char], from: usize, any: &[char]) -> Option<usize> {
    (from..chars.len()).find(|&j| any.contains(&chars[j]))
}

fn find_seq(chars: &[char], from: usize, seq: &[char]) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j..].starts_with(seq))
}

/// An emphasis marker opens only at a word start: the line's start, or
/// after a character that is not a letter, a digit or the marker itself.
fn word_start(chars: &[char], i: usize) -> bool {
    i == 0 || !chars[i - 1].is_alphanumeric()
}

/// An emphasis marker closes only at a word end.
fn word_end(chars: &[char], after: usize) -> bool {
    after >= chars.len() || !chars[after].is_alphanumeric()
}

fn is_link_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:"))
        && !url.contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_api::gateway::{Gateway, Verified};

    const BOT: &str = "U0BOT0001";

    fn account(team: Option<&str>) -> AccountConfig {
        AccountConfig {
            platform: KIND.into(),
            team: team.map(str::to_string),
            bot: BOT.into(),
            inbound: Inbound::SocketMode,
            message_content: None,
            cli: Some("curl".into()),
            curl_config: Some("/etc/fleet/slack-curl.conf".into()),
            dir: None,
        }
    }

    fn envelope(payload: &str) -> Envelope {
        Envelope {
            verified: Verified::Connection,
            received_at: 1_730_000_300_100,
            payload: serde_json::from_str(payload).expect("a recorded payload is JSON"),
        }
    }

    fn delivery(payload: &str) -> Delivery {
        read_event(&account(Some("T0123ABCD")), &envelope(payload))
            .known()
            .cloned()
            .unwrap_or_else(|| panic!("Unknown for {payload}"))
            .unwrap_or_else(|| panic!("ignored: {payload}"))
    }

    fn message(d: &Delivery) -> (&ConversationRef, &Message) {
        match &d.event {
            Event::Message {
                conversation,
                message,
            } => (conversation, message),
            other => panic!("{other:?}"),
        }
    }

    // The recorded Socket Mode and Events API payloads: synthetic, with
    // made-up ids, in the shapes Slack documents.
    const MENTION: &str = include_str!("../fixtures/slack/socket-mode-mention.json");
    const REPLY: &str = include_str!("../fixtures/slack/socket-mode-reply.json");
    const OWN_POST: &str = include_str!("../fixtures/slack/socket-mode-own-post.json");
    const BOT_MESSAGE: &str = include_str!("../fixtures/slack/socket-mode-bot-message.json");
    const EDIT: &str = include_str!("../fixtures/slack/socket-mode-edit.json");
    const DELETE: &str = include_str!("../fixtures/slack/socket-mode-delete.json");
    const REACTION: &str = include_str!("../fixtures/slack/socket-mode-reaction.json");
    const HELLO: &str = include_str!("../fixtures/slack/socket-mode-hello.json");
    const CHANNEL_JOIN: &str = include_str!("../fixtures/slack/socket-mode-channel-join.json");
    const OTHER_TEAM: &str = include_str!("../fixtures/slack/socket-mode-other-team.json");
    const HTTP_MENTION: &str = include_str!("../fixtures/slack/events-api-mention.json");
    const URL_VERIFICATION: &str =
        include_str!("../fixtures/slack/events-api-url-verification.json");

    #[test]
    fn a_socket_mode_mention_in_a_new_thread_is_a_message_that_starts_it() {
        let d = delivery(MENTION);
        assert_eq!(d.id, "Ev0123ABCD01");
        assert_eq!(d.verified, Verified::Connection);
        assert_eq!(d.received_at, 1_730_000_300_100);
        let (conversation, m) = message(&d);
        assert_eq!(conversation.address(), "slack:C0123ABCD:1730000300.000200");
        assert_eq!(m.id, "1730000300.000200");
        assert_eq!(m.at, 1_730_000_300_000);
        assert!(m.mentions_fleet);
        assert!(
            m.starts_thread,
            "no thread_ts: the message is its own parent"
        );
        assert_eq!(m.author.id, "U0123ABCD");
        assert_eq!(m.author.kind, AuthorKind::Person);
        assert_eq!(m.key, None);
        assert_eq!(m.text, "<@U0BOT0001> please look at the failing build");
        // The `<@id|name>` form is a mention too.
        let named = MENTION.replace("<@U0BOT0001>", "<@U0BOT0001|fleet>");
        assert!(message(&delivery(&named)).1.mentions_fleet);
    }

    #[test]
    fn a_reply_is_told_by_thread_ts_and_carries_no_mention() {
        let d = delivery(REPLY);
        let (conversation, m) = message(&d);
        assert_eq!(conversation.thread.as_deref(), Some("1730000300.000200"));
        assert_eq!(m.id, "1730000360.000300");
        assert!(!m.starts_thread, "thread_ts != ts: a reply");
        assert!(!m.mentions_fleet);
        assert_eq!(m.author.id, "U0456EFGH");
        assert_eq!(m.author.name, "Sam");
    }

    #[test]
    fn fleets_own_post_is_fleet_with_its_key_and_another_bots_is_a_bot() {
        let d = delivery(OWN_POST);
        let (_, m) = message(&d);
        assert_eq!(m.author.kind, AuthorKind::Fleet);
        assert_eq!(m.author.id, BOT);
        assert_eq!(
            m.key.as_deref(),
            Some("task/conv-3f9a1c0d2b7e-1/01JBX0000000000000000000AB/gateway/comment/c1")
        );
        assert!(!m.mentions_fleet);
        let d = delivery(BOT_MESSAGE);
        let (_, m) = message(&d);
        assert_eq!(m.author.kind, AuthorKind::Bot);
        assert_eq!(m.author.id, "B0ALERT001", "no user: the bot_id names it");
        assert_eq!(m.author.name, "alerts");
        assert_eq!(m.key, None, "another app's metadata is not a Fleet key");
    }

    #[test]
    fn an_edit_and_a_deletion_name_their_message_and_thread() {
        let d = delivery(EDIT);
        assert_eq!(d.id, "Ev0123ABCD05");
        match &d.event {
            Event::Edited {
                conversation,
                id,
                text,
                edited_at,
            } => {
                assert_eq!(conversation.thread.as_deref(), Some("1730000300.000200"));
                assert_eq!(id, "1730000360.000300");
                assert_eq!(text, "here is the full log");
                assert_eq!(*edited_at, 1_730_000_420_000);
            }
            other => panic!("{other:?}"),
        }
        let d = delivery(DELETE);
        match &d.event {
            Event::Removed { conversation, id } => {
                assert_eq!(conversation.thread.as_deref(), Some("1730000300.000200"));
                assert_eq!(id, "1730000360.000300");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_events_api_body_reads_the_same_as_the_socket_mode_envelope() {
        let mut e = envelope(HTTP_MENTION);
        e.verified = Verified::Signature;
        let d = read_event(&account(Some("T0123ABCD")), &e)
            .known()
            .cloned()
            .unwrap()
            .unwrap();
        assert_eq!(d.verified, Verified::Signature);
        assert_eq!(d.id, "Ev0123ABCD01");
        assert_eq!(message(&d).1.text, message(&delivery(MENTION)).1.text);
    }

    #[test]
    fn what_the_gateway_ignores_and_what_it_cannot_read() {
        let a = account(Some("T0123ABCD"));
        for (what, payload) in [
            ("a reaction", REACTION),
            ("the hello envelope", HELLO),
            ("a channel join", CHANNEL_JOIN),
            ("another workspace's event", OTHER_TEAM),
            ("a URL verification", URL_VERIFICATION),
            (
                "an interactive envelope",
                r#"{"type":"interactive","envelope_id":"e1","payload":{}}"#,
            ),
            (
                "message_replied",
                r#"{"type":"event_callback","team_id":"T0123ABCD","event_id":"Ev1","event":{"type":"message","subtype":"message_replied","channel":"C0123ABCD","ts":"1.000001"}}"#,
            ),
        ] {
            assert_eq!(
                read_event(&a, &envelope(payload)),
                Observation::Known(None),
                "{what}"
            );
        }
        // Without a team on the account, any workspace's event is read.
        assert!(
            read_event(&account(None), &envelope(OTHER_TEAM))
                .known()
                .unwrap()
                .is_some()
        );
        // An enterprise id on the account matches the org.
        assert!(
            read_event(&account(Some("E0123ABCD")), &envelope(OTHER_TEAM))
                .known()
                .unwrap()
                .is_some()
        );
        for (what, payload) in [
            ("not an object", r#""text""#),
            ("no type", r#"{"event_id":"Ev1"}"#),
            (
                "no payload object",
                r#"{"type":"events_api","envelope_id":"e1"}"#,
            ),
            (
                "no event_id",
                r#"{"type":"event_callback","team_id":"T0123ABCD","event":{"type":"message","channel":"C0123ABCD","user":"U1","ts":"1.000001","text":"hi"}}"#,
            ),
            (
                "no channel",
                r#"{"type":"event_callback","team_id":"T0123ABCD","event_id":"Ev1","event":{"type":"message","user":"U1","ts":"1.000001","text":"hi"}}"#,
            ),
            (
                "no ts",
                r#"{"type":"event_callback","team_id":"T0123ABCD","event_id":"Ev1","event":{"type":"message","channel":"C0123ABCD","user":"U1","text":"hi"}}"#,
            ),
            (
                "no author",
                r#"{"type":"event_callback","team_id":"T0123ABCD","event_id":"Ev1","event":{"type":"message","channel":"C0123ABCD","ts":"1.000001","text":"hi"}}"#,
            ),
            (
                "an edit without its message",
                r#"{"type":"event_callback","team_id":"T0123ABCD","event_id":"Ev1","event":{"type":"message","subtype":"message_changed","channel":"C0123ABCD","ts":"1.000001"}}"#,
            ),
            (
                "a deletion without deleted_ts",
                r#"{"type":"event_callback","team_id":"T0123ABCD","event_id":"Ev1","event":{"type":"message","subtype":"message_deleted","channel":"C0123ABCD","ts":"1.000001"}}"#,
            ),
        ] {
            assert!(read_event(&a, &envelope(payload)).is_unknown(), "{what}");
        }
    }

    #[test]
    fn addresses_are_a_channel_id_and_a_timestamp() {
        let t = parse_address("slack:C0123ABCD:1730000000.000100").unwrap();
        assert_eq!(t.chat, "C0123ABCD");
        assert_eq!(t.thread.as_deref(), Some("1730000000.000100"));
        assert_eq!(parse_address("slack:G0123ABCD").unwrap().thread, None);
        assert!(parse_address("slack:D0123ABCD:1.5").is_ok());
        for bad in [
            "fake:C0123ABCD",
            "slack:c0123abcd",
            "slack:U0123ABCD",
            "slack:C0123ABCD:1730000000",
            "slack:C0123ABCD:1730000000.0001000",
            "slack:C0123ABCD:abc",
            "slack:",
            "slack:C0123ABCD:1.1:x",
            "nonsense",
        ] {
            assert!(parse_address(bad).is_err(), "{bad}");
        }
        assert_eq!(ts_millis("1730000000.000100"), Some(1_730_000_000_000));
        assert_eq!(ts_millis("1730000000.999999"), Some(1_730_000_000_999));
        assert_eq!(ts_millis("1730000000.5"), Some(1_730_000_000_500));
        assert_eq!(ts_millis("1730000000"), None);
        assert_eq!(ts_of_millis(1_730_000_000_250), "1730000000.250000");
        assert!(ts_after("1730000000.000200", "1730000000.000100"));
        assert!(ts_after("1730000001.000000", "1730000000.999999"));
        assert!(!ts_after("1730000000.000100", "1730000000.000100"));
    }

    fn item(ts: &str, user: &str, text: &str) -> Value {
        json!({"type": "message", "ts": ts, "user": user, "text": text, "thread_ts": "1730000300.000200"})
    }

    #[test]
    fn a_thread_read_is_oldest_first_once_each_with_its_starter_and_limit() {
        let parent = json!({"type": "message", "ts": "1730000300.000200", "user": "U0123ABCD",
            "text": "<@U0BOT0001> look", "thread_ts": "1730000300.000200", "reply_count": 3});
        let own = json!({"type": "message", "ts": "1730000500.000000", "user": BOT, "bot_id": "B0FLEET001",
            "text": "On it.", "thread_ts": "1730000300.000200",
            "metadata": {"event_type": "fleet_post", "event_payload": {"key": "task/t/u/gateway/comment/c1"}}});
        // Pages as Slack may give them: the parent on each, a system note,
        // and a reply out of order.
        let items = vec![
            parent.clone(),
            item("1730000360.000300", "U0456EFGH", "second"),
            parent.clone(),
            json!({"type": "message", "subtype": "channel_join", "ts": "1730000370.000000", "user": "U0789IJKL", "text": "joined"}),
            own.clone(),
            item("1730000330.000000", "U0123ABCD", "first reply"),
        ];
        let all = HistoryQuery::After {
            id: None,
            limit: 200,
        };
        let h = collect_thread(BOT, &items, &parent, &all).unwrap();
        assert_eq!(
            h.messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            [
                "1730000300.000200",
                "1730000330.000000",
                "1730000360.000300",
                "1730000500.000000"
            ]
        );
        assert_eq!(h.left_out, 0);
        assert!(h.messages[0].mentions_fleet && h.messages[0].starts_thread);
        assert_eq!(h.messages[3].author.kind, AuthorKind::Fleet);
        assert_eq!(
            h.messages[3].key.as_deref(),
            Some("task/t/u/gateway/comment/c1")
        );
        assert_eq!(
            h.starter,
            Some(Starter {
                id: Some("1730000300.000200".into()),
                kind: Some(AuthorKind::Person),
                key: None,
                removed: false
            })
        );
        let newest = collect_thread(
            BOT,
            &items,
            &parent,
            &HistoryQuery::After { id: None, limit: 2 },
        )
        .unwrap();
        assert_eq!(newest.messages.len(), 2);
        assert_eq!(newest.messages[0].id, "1730000360.000300");
        assert_eq!(newest.left_out, 2);
        let after = collect_thread(
            BOT,
            &items,
            &parent,
            &HistoryQuery::After {
                id: Some("1730000330.000000".into()),
                limit: 200,
            },
        )
        .unwrap();
        assert_eq!(
            after.messages.len(),
            2,
            "strictly after the id, the parent never"
        );
        let since = collect_thread(
            BOT,
            &items,
            &parent,
            &HistoryQuery::Since {
                at: 1_730_000_360_000,
            },
        )
        .unwrap();
        assert_eq!(since.messages.len(), 2);
        assert_eq!(since.left_out, 0);
        // A parent Slack reports deleted is a tombstone.
        let tombstone = json!({"type": "message", "subtype": "tombstone", "ts": "1730000300.000200",
            "user": "USLACKBOT", "text": "This message was deleted.", "thread_ts": "1730000300.000200"});
        let gone: Vec<Value> = items
            .iter()
            .filter(|m| m["ts"] != "1730000300.000200")
            .cloned()
            .chain([tombstone.clone()])
            .collect();
        let h = collect_thread(BOT, &gone, &tombstone, &all).unwrap();
        assert_eq!(h.starter, Some(Starter::removed()));
        assert_eq!(h.messages.len(), 3, "the tombstone is no message");
        // A Fleet-opened thread's starter carries the key.
        let h = collect_thread(BOT, std::slice::from_ref(&own), &own, &all).unwrap();
        assert_eq!(
            h.starter.unwrap().key.as_deref(),
            Some("task/t/u/gateway/comment/c1")
        );
        // An item without an author makes the read Unknown, never a guess.
        let odd = vec![json!({"type": "message", "ts": "1730000600.000000", "text": "?"})];
        assert!(collect_thread(BOT, &odd, &parent, &all).is_err());
    }

    #[test]
    fn a_reply_is_split_into_its_head_and_body_and_judged() {
        let r = Reply::parse(
            "HTTP/2 200 \r\ncontent-type: application/json; charset=utf-8\r\n\r\n{\"ok\":true,\"ts\":\"1.000001\"}",
            Some(0),
            b"",
        );
        assert_eq!(r.status, 200);
        assert_eq!(
            r.header("content-type"),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(r.answer("chat.postMessage").unwrap()["ts"], "1.000001");
        // An informational head before the real one is skipped.
        let r = Reply::parse(
            "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\n\r\n{\"ok\":true}",
            Some(0),
            b"",
        );
        assert_eq!(r.status, 200);
        assert!(r.answer("x").is_ok());
        let r = Reply::parse(
            "HTTP/2 429 \r\nRetry-After: 45\r\n\r\n{\"ok\":false,\"error\":\"ratelimited\"}",
            Some(0),
            b"",
        );
        assert_eq!(
            r.answer("conversations.replies"),
            Err(CallError::Throttled {
                retry_after: Some(45_000)
            })
        );
        let r = Reply::parse(
            "HTTP/2 200 \r\n\r\n{\"ok\":false,\"error\":\"ratelimited\"}",
            Some(0),
            b"",
        );
        assert_eq!(
            r.answer("x"),
            Err(CallError::Throttled { retry_after: None })
        );
        let r = Reply::parse(
            "HTTP/2 200 \r\n\r\n{\"ok\":false,\"error\":\"invalid_auth\"}",
            Some(0),
            b"",
        );
        assert_eq!(
            r.answer("auth.test"),
            Err(CallError::Refused("auth.test: invalid_auth".into()))
        );
        assert!(CallError::Refused(String::new()).certain_failure());
        for error in ["internal_error", "fatal_error"] {
            let r = Reply::parse(
                &format!("HTTP/2 200 \r\n\r\n{{\"ok\":false,\"error\":\"{error}\"}}"),
                Some(0),
                b"",
            );
            let e = r.answer("chat.postMessage").unwrap_err();
            assert!(
                matches!(&e, CallError::Unreadable(w) if w.contains(error) && w.contains("may have")),
                "{e:?}"
            );
            assert!(!e.certain_failure());
        }
        let r = Reply::parse("HTTP/2 200 \r\n\r\n<html>not json", Some(0), b"");
        assert!(matches!(r.answer("x"), Err(CallError::Unreadable(w)) if w.contains("HTTP 200")));
        let r = Reply::parse("", Some(7), b"curl: (7) Failed to connect\n");
        assert_eq!(
            r.answer("x"),
            Err(CallError::Unreadable(
                "x: curl exited 7: curl: (7) Failed to connect".into()
            ))
        );
        let r = Reply::parse("HTTP/2 500 \r\n\r\n", Some(0), b"");
        assert!(matches!(r.answer("x"), Err(CallError::Unreadable(_))));
        let r = Reply::parse("HTTP/2 200 \r\n\r\n{\"data\":1}", Some(0), b"");
        assert!(
            matches!(r.answer("x"), Err(CallError::Unreadable(w)) if w.contains("no ok field"))
        );
        assert_eq!(
            CallError::Throttled {
                retry_after: Some(2000)
            }
            .uncertainty(),
            Uncertainty::Throttled
        );
        assert!(matches!(
            CallError::Throttled {
                retry_after: Some(2000)
            }
            .history(),
            HistoryObservation::Unknown {
                kind: Uncertainty::Throttled,
                retry_after: Some(2000),
                ..
            }
        ));
    }

    #[test]
    fn markdown_becomes_mrkdwn_with_escaping() {
        for (markdown, mrkdwn) in [
            ("plain", "plain"),
            ("a & b < c > d", "a &amp; b &lt; c &gt; d"),
            ("**bold** and __bold__", "*bold* and *bold*"),
            ("*em* and _em_", "_em_ and _em_"),
            ("~~gone~~", "~gone~"),
            ("snake_case_name stays", "snake_case_name stays"),
            ("2 * 3 * 4", "2 * 3 * 4"),
            ("unmatched *star", "unmatched *star"),
            ("`a < b` in code", "`a &lt; b` in code"),
            ("`**not bold**`", "`**not bold**`"),
            (
                "[docs](https://example.test/a?b=1&c=2)",
                "<https://example.test/a?b=1&amp;c=2|docs>",
            ),
            (
                "[mail](mailto:ops@example.test)",
                "<mailto:ops@example.test|mail>",
            ),
            ("[x](javascript:alert(1))", "[x](javascript:alert(1))"),
            ("[**b**](https://e.test)", "<https://e.test|*b*>"),
            ("# Title", "*Title*"),
            ("### Deep **title**", "*Deep *title**"),
            ("#hashtag", "#hashtag"),
            ("- one\n- two", "• one\n• two"),
            ("* one\n  * nested", "• one\n  • nested"),
            ("1. first\n2. second", "1. first\n2. second"),
            ("> quoted *em*", "&gt; quoted _em_"),
            ("para one\n\npara two", "para one\n\npara two"),
            (
                "```rust\nlet x = a < b;\n```",
                "```\nlet x = a &lt; b;\n```",
            ),
            ("```\nopen", "```\nopen\n```"),
            (
                "<@U0123ABCD> is not a mention Fleet makes",
                "&lt;@U0123ABCD&gt; is not a mention Fleet makes",
            ),
        ] {
            assert_eq!(to_mrkdwn(markdown), mrkdwn, "{markdown:?}");
        }
    }

    #[test]
    fn long_text_is_split_at_lines_under_the_limit_and_fences_are_kept_whole() {
        assert_eq!(split("short", 10), ["short"]);
        let text = "aaaa\nbbbb\ncccc\ndddd";
        assert_eq!(split(text, 9), ["aaaa\nbbbb", "cccc\ndddd"]);
        for chunk in split(text, 9) {
            assert!(chunk.chars().count() <= 9);
        }
        // A line longer than the limit is cut inside.
        let long = "x".repeat(25);
        let chunks = split(&long, 10);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat(), long);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 10);
        }
        // Paragraph breaks at a split are dropped, not carried.
        assert_eq!(split("one\n\ntwo\n\nthree", 8), ["one\n\ntwo", "three"]);
        // Characters, not bytes.
        let wide = "é".repeat(12);
        assert_eq!(split(&wide, 10).len(), 2);
        // A fence cut by a split is closed and reopened.
        let fenced = "```\nline one\nline two\nline three\n```\nafter";
        let chunks = split(fenced, 22);
        assert!(chunks.len() >= 2, "{chunks:?}");
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 22, "{chunk:?}");
            assert_eq!(
                chunk.matches("```").count() % 2,
                0,
                "every chunk's fences are balanced: {chunk:?}"
            );
        }
        assert!(chunks[0].starts_with("```\n") && chunks[0].ends_with("\n```"));
        assert!(chunks.last().unwrap().ends_with("after"));
        let rejoined: String = chunks
            .iter()
            .map(|c| c.trim_start_matches("```\n").trim_end_matches("\n```"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rejoined, "line one\nline two\nline three\nafter");
    }

    #[test]
    fn the_account_is_refused_without_what_curl_needs_and_calls_nothing_at_open() {
        let g = SlackGateway::new(
            "slack-main",
            &account(Some("T0123ABCD")),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(g.account(), "slack-main");
        assert!(g.operation_bound() >= 10_000);
        assert!(g.parse("other", "slack:C0123ABCD").is_unknown());
        assert_eq!(
            g.parse("slack-main", "slack:C0123ABCD:1730000000.000100"),
            Observation::Known(ConversationRef {
                platform: KIND.into(),
                chat: "C0123ABCD".into(),
                thread: Some("1730000000.000100".into()),
            })
        );
        assert!(g.parse("slack-main", "slack:C0123ABCD:now").is_unknown());
        let err = SlackGateway::new(
            "slack-main",
            &AccountConfig {
                inbound: Inbound::Connection,
                ..account(None)
            },
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(
            err,
            "inbound \"connection\" is not how a Slack listener delivers; use \"socket-mode\" or \"events-api\""
        );
        let err = SlackGateway::new(
            "slack-main",
            &AccountConfig {
                curl_config: None,
                ..account(None)
            },
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(err, "a slack account needs a curlConfig");
        // Refusals that need no call are Known.
        let thread = parse_address("slack:C0123ABCD:1730000000.000100").unwrap();
        assert!(matches!(
            g.open("slack-main", &thread, "x", "k"),
            Observation::Known(EffectOutcome::Failed { why }) if why.contains("is a thread")
        ));
        assert!(matches!(
            g.post("slack-main", &thread.channel(), "x", "k"),
            Observation::Known(EffectOutcome::Failed { why }) if why.contains("is a channel")
        ));
        assert!(matches!(
            g.post("other", &thread, "x", "k"),
            Observation::Known(EffectOutcome::Failed { .. })
        ));
        let fake = ConversationRef::split("fake:C1:1").unwrap();
        assert!(matches!(
            g.post("slack-main", &fake, "x", "k"),
            Observation::Known(EffectOutcome::Failed { .. })
        ));
        assert!(matches!(
            g.history("slack-main", &fake, &HistoryQuery::Since { at: 0 }),
            HistoryObservation::Unknown {
                kind: Uncertainty::Unreadable,
                ..
            }
        ));
        assert!(matches!(
            g.history("other", &thread, &HistoryQuery::Since { at: 0 }),
            HistoryObservation::Unknown {
                kind: Uncertainty::Unavailable,
                ..
            }
        ));
        // A text past the chunk cap is refused before any call.
        let huge = "y".repeat(POST_LIMIT * MAX_CHUNKS + 1);
        assert!(matches!(
            g.post("slack-main", &thread, &huge, "k"),
            Observation::Known(EffectOutcome::Failed { why }) if why.contains("posts")
        ));
    }
}
