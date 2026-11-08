//! Discord through an externally authenticated `curl`: the REST API behind
//! the Gateway port, as the [Discord adapter page] maps it.
//!
//! - An address is `discord:<channel>:<thread>` for a thread, or
//!   `discord:<channel>` for the channel a thread is opened in. A thread
//!   started from a message has that message's id, so a top-level message
//!   `M` in channel `C` is the conversation `discord:C:M` before its thread
//!   exists.
//! - [`read_event`] reads the listener's envelope, `{dispatch, parentId}`,
//!   and is pure: a thread message is told by its `channel_type` or the
//!   `parentId` the listener adds, Fleet's own posts by the account's bot
//!   user id, a mention by the `mentions` array.
//! - `whoami` asks `GET /users/@me` and checks the guild; `capabilities`
//!   reads the application's message content flags. `history` pages a
//!   thread forward under a budget of calls, with the message it started
//!   from, and answers Unknown rather than a partial list.
//! - `post` sends `POST /channels/<thread>/messages`, and starts the thread
//!   from its message when Discord does not know it yet. `open` posts in the
//!   channel and starts a thread from that post. Every post carries a nonce
//!   derived from the intent key with `enforce_nonce`, so a resend inside
//!   Discord's window returns the post that landed, and mentions nobody.
//!   Fleet's Markdown subset is Discord's own; the text is split at
//!   [`POST_LIMIT`] characters under derived keys.
//!
//! Every call is one bounded `curl` process through fleet-exec, killed with
//! its process tree past the bound. Fleet never constructs an Authorization
//! header: the bot token lives in the operator's curl config file, which
//! Fleet names with `--config` and never reads.
//!
//! [Discord adapter page]: ../../../docs/fleet/controllers/gateway-adapters.md

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
use sha2::{Digest, Sha256};

use crate::curl::{Budget, Reply, landed, send_failure};
use crate::text::split;

pub const KIND: &str = "discord";
/// Every call is `https://discord.com/api/v10/<path>`.
pub const API: &str = "https://discord.com/api/v10";
/// The client an account runs without a `cli` of its own.
pub const DEFAULT_CLI: &str = "curl";
/// The most characters one message's `content` takes.
pub const POST_LIMIT: usize = 2_000;
/// The most posts one text is split into; a longer text is refused.
pub const MAX_CHUNKS: usize = 10;
/// The most calls one operation makes.
pub const MAX_CALLS_PER_OPERATION: u32 = 10;
/// How long Fleet trusts Discord to return a post for a resent nonce.
/// Discord documents "the past few minutes"; Fleet takes one.
pub const NONCE_WINDOW_MS: Millis = 60_000;
/// Discord's documented maximum nonce length.
pub const NONCE_LEN: usize = 25;
/// Messages per history page, Discord's maximum.
const PAGE: usize = 100;
/// The first millisecond of 2015, where snowflake time starts.
pub const EPOCH: Millis = 1_420_070_400_000;
/// `SUPPRESS_EMBEDS`: Fleet's links never unfurl.
const SUPPRESS_EMBEDS: u64 = 1 << 2;

/// Gateway intents a listener identifies with.
pub const INTENT_GUILDS: u64 = 1 << 0;
pub const INTENT_GUILD_MESSAGES: u64 = 1 << 9;
pub const INTENT_MESSAGE_CONTENT: u64 = 1 << 15;
/// Application flags that grant the message content intent.
const APP_MESSAGE_CONTENT: u64 = (1 << 18) | (1 << 19);

/// Discord's JSON error codes the adapter acts on.
const UNKNOWN_CHANNEL: u64 = 10_003;
const UNKNOWN_MESSAGE: u64 = 10_008;
const THREAD_EXISTS: u64 = 160_004;

/// Message types read as messages: `DEFAULT` and `REPLY`. Every other type
/// is a system note, such as a thread's starter reference.
fn is_message_type(t: Option<u64>) -> bool {
    matches!(t, Some(0 | 19))
}

/// Channel types of a public thread, in an announcement or a text channel.
fn is_public_thread(t: u64) -> bool {
    matches!(t, 10 | 11)
}

const PRIVATE_THREAD: u64 = 12;

/// The intents a listener identifies with: guild and message events, and
/// message content when the account holds it.
pub fn intents(message_content: bool) -> u64 {
    let base = INTENT_GUILDS | INTENT_GUILD_MESSAGES;
    if message_content {
        base | INTENT_MESSAGE_CONTENT
    } else {
        base
    }
}

/// One Discord guild, served by `curl` with the operator's config file.
#[derive(Debug, Clone)]
pub struct DiscordGateway {
    account: String,
    guild: String,
    bot: String,
    message_content: bool,
    cli: OsString,
    curl_config: PathBuf,
    call_timeout: Duration,
}

impl DiscordGateway {
    /// The adapter for an account, or why its configuration cannot be
    /// served: an `inbound` other than `discord-gateway`, no guild in
    /// `team`, or no `curlConfig`.
    pub fn new(
        account: &str,
        config: &AccountConfig,
        call_timeout: Duration,
    ) -> Result<DiscordGateway, String> {
        if config.inbound != Inbound::DiscordGateway {
            return Err(format!(
                "inbound {:?} is not how a Discord listener delivers; use \"discord-gateway\"",
                config.inbound.name()
            ));
        }
        let guild = config
            .team
            .clone()
            .ok_or_else(|| "a discord account needs a team: its guild id".to_string())?;
        if !is_snowflake(&guild) || !is_snowflake(&config.bot) {
            return Err("a discord account's team and bot are snowflakes".into());
        }
        let curl_config = config
            .curl_config
            .clone()
            .ok_or_else(|| "a discord account needs a curlConfig".to_string())?;
        Ok(DiscordGateway {
            account: account.to_string(),
            guild,
            bot: config.bot.clone(),
            message_content: config.message_content.unwrap_or(false),
            cli: config.cli.as_deref().unwrap_or(DEFAULT_CLI).into(),
            curl_config,
            call_timeout,
        })
    }

    /// One bounded call: `curl --include … --config <file> --request <verb>
    /// --url <API>/<path>`, with a JSON body on stdin for a POST, so neither
    /// the text nor anything from the config file is on a command line.
    fn call(
        &self,
        verb: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        let what = format!("{verb} /{}", path.split('?').next().unwrap_or(path));
        let mut args: Vec<OsString> = [
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
        .collect();
        args.push(self.curl_config.clone().into_os_string());
        args.extend(["--request", verb, "--header"].map(OsString::from));
        args.push(
            format!(
                "User-Agent: DiscordBot (https://github.com/Thurbeen/fleet, {})",
                env!("CARGO_PKG_VERSION")
            )
            .into(),
        );
        if body.is_some() {
            args.extend(
                [
                    "--header",
                    "Content-Type: application/json",
                    "--header",
                    "Expect:",
                    "--data-binary",
                    "@-",
                ]
                .map(OsString::from),
            );
        }
        args.push("--url".into());
        args.push(format!("{API}/{path}").into());
        let mut spec = Spec::new(&self.cli, timeout).args(args);
        if let Some(b) = body {
            spec = spec.stdin(serde_json::to_vec(b).expect("a JSON value serialises"));
        }
        match run(&spec) {
            Outcome::Exited {
                code,
                stdout,
                stderr,
            } => answer(
                &Reply::parse(&String::from_utf8_lossy(&stdout), code, &stderr),
                &what,
            ),
            Outcome::TimedOut { .. } => Err(CallError::Timeout(format!(
                "{what}: timed out after {}ms; its process tree was killed",
                timeout.as_millis()
            ))),
            Outcome::NotStarted(why) => Err(CallError::Unavailable(format!(
                "{what}: {} could not start: {why}",
                self.cli.to_string_lossy()
            ))),
        }
    }

    fn budgeted(
        &self,
        verb: &str,
        path: &str,
        body: Option<&Value>,
        budget: &mut Budget,
    ) -> Result<Value, CallError> {
        let timeout = budget.next_timeout().ok_or_else(|| {
            CallError::Incomplete(format!("the bound ran out before {verb} /{path}"))
        })?;
        self.call(verb, path, body, timeout)
    }

    /// Every message of `channel` after the snowflake `after`, paged
    /// forward. `None` when Discord does not know the channel: a thread not
    /// started yet.
    fn read_channel(
        &self,
        channel: &str,
        after: u64,
        budget: &mut Budget,
    ) -> Result<Option<Vec<Value>>, CallError> {
        let mut items = Vec::new();
        let mut after = after;
        loop {
            let path = format!("channels/{channel}/messages?limit={PAGE}&after={after}");
            let v = match self.budgeted("GET", &path, None, budget) {
                Ok(v) => v,
                Err(CallError::Refused { code, .. }) if code == UNKNOWN_CHANNEL => {
                    return Ok(None);
                }
                Err(e) => return Err(e),
            };
            let Some(page) = v.as_array() else {
                return Err(CallError::Unreadable(format!(
                    "GET /channels/{channel}/messages: the answer is not an array"
                )));
            };
            let newest = page.iter().filter_map(|m| snowflake(&m["id"])).max();
            items.extend(page.iter().cloned());
            match newest {
                Some(n) if page.len() >= PAGE => after = n,
                _ => return Ok(Some(items)),
            }
        }
    }

    /// The newest `limit` messages of `channel` at least, paged backward
    /// from the newest. `None` when Discord does not know the channel.
    fn read_newest(
        &self,
        channel: &str,
        limit: u32,
        budget: &mut Budget,
    ) -> Result<Option<Vec<Value>>, CallError> {
        let mut items: Vec<Value> = Vec::new();
        let mut before: Option<u64> = None;
        loop {
            let mut path = format!("channels/{channel}/messages?limit={PAGE}");
            if let Some(b) = before {
                path.push_str(&format!("&before={b}"));
            }
            let v = match self.budgeted("GET", &path, None, budget) {
                Ok(v) => v,
                Err(CallError::Refused { code, .. }) if code == UNKNOWN_CHANNEL => {
                    return Ok(None);
                }
                Err(e) => return Err(e),
            };
            let Some(page) = v.as_array() else {
                return Err(CallError::Unreadable(format!(
                    "GET /channels/{channel}/messages: the answer is not an array"
                )));
            };
            let oldest = page.iter().filter_map(|m| snowflake(&m["id"])).min();
            items.extend(page.iter().cloned());
            match oldest {
                Some(o) if page.len() >= PAGE && items.len() < limit as usize => {
                    before = Some(o);
                }
                _ => return Ok(Some(items)),
            }
        }
    }

    /// A thread's or channel's messages for `query`: the newest ones for a
    /// first read, else every one after its bound.
    fn read(
        &self,
        channel: &str,
        query: &HistoryQuery,
        budget: &mut Budget,
    ) -> Result<Option<Vec<Value>>, CallError> {
        match query {
            HistoryQuery::After { id: None, limit } => self.read_newest(channel, *limit, budget),
            _ => self.read_channel(channel, after_of(query), budget),
        }
    }

    fn thread_history(&self, chat: &str, thread: &str, query: &HistoryQuery) -> HistoryObservation {
        let mut budget = Budget::new(self.call_timeout, MAX_CALLS_PER_OPERATION);
        // The message the thread started from, in its parent channel.
        let starter = match self.budgeted(
            "GET",
            &format!("channels/{chat}/messages/{thread}"),
            None,
            &mut budget,
        ) {
            Ok(m) => Some(m),
            Err(CallError::Refused { code, .. }) if code == UNKNOWN_MESSAGE => None,
            Err(e) => return e.history(),
        };
        let replies = match self.read(thread, query, &mut budget) {
            Ok(items) => items.unwrap_or_default(),
            Err(e) => return e.history(),
        };
        let starter_msg = match &starter {
            Some(m) => match message_of(&self.bot, m, true) {
                Ok(msg) => Some(msg),
                Err(why) => return HistoryObservation::unknown(Uncertainty::Unreadable, why),
            },
            None => None,
        };
        let mut msgs = Vec::new();
        for item in &replies {
            if !is_message_type(item["type"].as_u64()) {
                continue;
            }
            match message_of(&self.bot, item, false) {
                Ok(m) => msgs.push(m),
                Err(why) => return HistoryObservation::unknown(Uncertainty::Unreadable, why),
            }
        }
        msgs.extend(starter_msg.clone());
        let (messages, left_out) = collect(msgs, query);
        HistoryObservation::Known(History {
            messages,
            left_out,
            starter: Some(match &starter_msg {
                Some(m) => Starter::message(m),
                None => Starter::removed(),
            }),
        })
    }

    fn channel_history(&self, chat: &str, query: &HistoryQuery) -> HistoryObservation {
        let mut budget = Budget::new(self.call_timeout, MAX_CALLS_PER_OPERATION);
        let items = match self.read(chat, query, &mut budget) {
            Ok(Some(items)) => items,
            Ok(None) => {
                return HistoryObservation::unknown(
                    Uncertainty::Refused,
                    format!("Discord does not know channel {chat}"),
                );
            }
            Err(e) => return e.history(),
        };
        let mut msgs = Vec::new();
        for item in items.iter().filter(|i| is_message_type(i["type"].as_u64())) {
            match message_of(&self.bot, item, true) {
                Ok(m) => msgs.push(m),
                Err(why) => return HistoryObservation::unknown(Uncertainty::Unreadable, why),
            }
        }
        let (messages, left_out) = collect(msgs, query);
        HistoryObservation::Known(History {
            messages,
            left_out,
            starter: None,
        })
    }

    /// One message in `channel` under `key`'s nonce: its id and time.
    fn send(
        &self,
        channel: &str,
        text: &str,
        key: &str,
        budget: &mut Budget,
    ) -> Result<(String, Millis), CallError> {
        let body = json!({
            "content": text,
            "nonce": nonce(key),
            "enforce_nonce": true,
            "allowed_mentions": {"parse": []},
            "flags": SUPPRESS_EMBEDS,
        });
        let v = self.budgeted(
            "POST",
            &format!("channels/{channel}/messages"),
            Some(&body),
            budget,
        )?;
        let id = v["id"]
            .as_str()
            .filter(|s| is_snowflake(s))
            .ok_or_else(|| {
                CallError::Unreadable("POST /messages answered without a message id".into())
            })?;
        let at = snowflake_millis(id).expect("a checked snowflake");
        Ok((id.to_string(), at))
    }

    /// Start the thread whose id is `message`'s, in `channel`. A thread that
    /// already exists is the one wanted: its id is the message's.
    fn start_thread(
        &self,
        channel: &str,
        message: &str,
        name: &str,
        budget: &mut Budget,
    ) -> Result<(), CallError> {
        match self.budgeted(
            "POST",
            &format!("channels/{channel}/messages/{message}/threads"),
            Some(&json!({"name": name})),
            budget,
        ) {
            Ok(_) => Ok(()),
            Err(CallError::Refused { code, .. }) if code == THREAD_EXISTS => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Post `text` as one or more chunks, the first under `key` alone when
    /// it is the only one, else each under `<key>/<n>/<total>`. With a `thread`,
    /// every chunk goes there, and the thread is started from its message
    /// first when Discord does not know it yet. Without one, the first chunk
    /// goes to the channel and starts a thread the rest go to.
    fn post_chunks(
        &self,
        chat: &str,
        thread: Option<&str>,
        text: &str,
        key: &str,
    ) -> Observation<EffectOutcome> {
        let chunks = split(text, POST_LIMIT);
        if chunks.len() > MAX_CHUNKS {
            return Observation::Known(EffectOutcome::Failed {
                why: format!(
                    "the text would take {} posts; the most one post takes is {MAX_CHUNKS} of {POST_LIMIT} characters",
                    chunks.len()
                ),
            });
        }
        let name = thread_name(text);
        let derived = chunks.len() > 1;
        let mut budget = Budget::new(self.call_timeout, MAX_CALLS_PER_OPERATION);
        let mut first: Option<(String, Millis)> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            let chunk_key = if derived {
                part_key(key, i + 1, chunks.len())
            } else {
                key.to_string()
            };
            let landed = match (thread, &first) {
                (Some(t), _) => match self.send(t, chunk, &chunk_key, &mut budget) {
                    Err(CallError::Refused { code, .. }) if code == UNKNOWN_CHANNEL => self
                        .start_thread(chat, t, &name, &mut budget)
                        .and_then(|()| self.send(t, chunk, &chunk_key, &mut budget)),
                    other => other,
                },
                (None, None) => match self.send(chat, chunk, &chunk_key, &mut budget) {
                    Ok((id, at)) => {
                        // The opening post landed: whatever the thread start
                        // answers, the outcome is no longer a clean refusal.
                        first = Some((id.clone(), at));
                        self.start_thread(chat, &id, &name, &mut budget)
                            .map(|()| (id, at))
                    }
                    Err(e) => Err(e),
                },
                (None, Some((opened, _))) => self.send(opened, chunk, &chunk_key, &mut budget),
            };
            match landed {
                Ok(l) => {
                    first.get_or_insert(l);
                }
                Err(e) => {
                    let partly = first
                        .is_some()
                        .then(|| format!("posts under {key} landed in part"));
                    return send_failure(partly, e.why(), e.certain_failure());
                }
            }
        }
        let (message, at) = first.expect("at least one chunk");
        Observation::Known(landed(KIND, chat, thread, message, at))
    }

    /// Post `text` at the top level of `chat`: every chunk a message in the
    /// channel, and no thread started. The first chunk's id and time come
    /// back as `Posted`.
    fn post_channel(&self, chat: &str, text: &str, key: &str) -> Observation<EffectOutcome> {
        let chunks = split(text, POST_LIMIT);
        if chunks.len() > MAX_CHUNKS {
            return Observation::Known(EffectOutcome::Failed {
                why: format!(
                    "the text would take {} posts; the most one post takes is {MAX_CHUNKS} of {POST_LIMIT} characters",
                    chunks.len()
                ),
            });
        }
        let mut budget = Budget::new(self.call_timeout, MAX_CALLS_PER_OPERATION);
        let mut first: Option<(String, Millis)> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            let chunk_key = if chunks.len() > 1 {
                part_key(key, i + 1, chunks.len())
            } else {
                key.to_string()
            };
            match self.send(chat, chunk, &chunk_key, &mut budget) {
                Ok(l) => {
                    first.get_or_insert(l);
                }
                Err(e) => {
                    let partly = first
                        .is_some()
                        .then(|| format!("posts under {key} landed in part"));
                    return send_failure(partly, e.why(), e.certain_failure());
                }
            }
        }
        let (message, at) = first.expect("at least one chunk");
        Observation::Known(EffectOutcome::Posted { message, at })
    }

    /// Create or delete Fleet's own reaction on the message: `PUT` or
    /// `DELETE /channels/{channel}/messages/{message}/reactions/{emoji}/@me`,
    /// with the emoji URL-encoded. A thread is a channel, so a reply's
    /// channel is the thread; a thread started from a message keeps that
    /// starter message in the parent channel.
    fn react_on(
        &self,
        conversation: &ConversationRef,
        message: &str,
        reaction: &str,
        on: bool,
    ) -> Observation<EffectOutcome> {
        if !is_snowflake(message) {
            return Observation::Known(EffectOutcome::Failed {
                why: format!("{message:?} is not a Discord message id"),
            });
        }
        let Some(emoji) = emoji(reaction) else {
            return Observation::Known(EffectOutcome::Failed {
                why: format!(
                    "{reaction:?} is no emoji Discord knows: name a Unicode emoji, one of the names Fleet maps, or a custom emoji as name:id"
                ),
            });
        };
        let channel = match conversation.thread.as_deref() {
            Some(thread) if thread != message => thread,
            _ => conversation.chat.as_str(),
        };
        let verb = if on { "PUT" } else { "DELETE" };
        let path = format!(
            "channels/{channel}/messages/{message}/reactions/{}/@me",
            url_encode(&emoji)
        );
        match self.call(verb, &path, None, self.call_timeout) {
            Ok(_) => Observation::Known(EffectOutcome::Reacted { already: false }),
            Err(e) => send_failure(None, e.why(), e.certain_failure()),
        }
    }

    /// The link Discord copies for a thread: the guild, then the thread's
    /// channel id.
    fn thread_link(&self, thread: &ConversationRef) -> Option<String> {
        let id = thread.thread.as_deref()?;
        Some(format!("https://discord.com/channels/{}/{id}", self.guild))
    }
}

crate::curl::curl_gateway!(DiscordGateway, "Discord");

impl DiscordGateway {
    fn identity(&self) -> Observation<Identity> {
        let me = match self.call("GET", "users/@me", None, self.call_timeout) {
            Ok(v) => v,
            Err(e) => return Observation::unknown(e.why()),
        };
        let Some(id) = me["id"].as_str().filter(|s| !s.is_empty()) else {
            return Observation::unknown("GET /users/@me named no id");
        };
        // The bot sees the account's guild, or nothing it reads is real.
        let guild = format!("guilds/{}", self.guild);
        if let Err(e) = self.call("GET", &guild, None, self.call_timeout) {
            return Observation::unknown(format!(
                "the bot cannot see guild {}: {}",
                self.guild,
                e.why()
            ));
        }
        Observation::Known(Identity {
            id: id.to_string(),
            name: me["username"].as_str().map(str::to_string),
        })
    }

    /// History, the nonce window and threads are Discord's own; unmentioned
    /// messages reach Fleet only when the account says it holds the message
    /// content intent and the application has it.
    fn offers(&self) -> Observation<Capabilities> {
        let app = match self.call("GET", "applications/@me", None, self.call_timeout) {
            Ok(v) => v,
            Err(e) => return Observation::unknown(e.why()),
        };
        let flags = app["flags"].as_u64().unwrap_or(0);
        Observation::Known(Capabilities {
            history: true,
            marker: Marker::Nonce {
                window_ms: NONCE_WINDOW_MS,
            },
            unmentioned: self.message_content && flags & APP_MESSAGE_CONTENT != 0,
            threads: true,
            reactions: true,
        })
    }
}

// ----------------------------------------------------------------------
// Addresses, snowflakes, nonces and times.
// ----------------------------------------------------------------------

/// The emoji names Fleet maps to Unicode for Discord: Slack's names for
/// the default reactions, and a few common ones besides.
const EMOJI: &[(&str, &str)] = &[
    ("eyes", "\u{1F440}"),
    ("white_check_mark", "\u{2705}"),
    ("heavy_check_mark", "\u{2714}\u{FE0F}"),
    ("hourglass_flowing_sand", "\u{23F3}"),
    ("hourglass", "\u{231B}"),
    ("thumbsup", "\u{1F44D}"),
    ("+1", "\u{1F44D}"),
    ("speech_balloon", "\u{1F4AC}"),
    ("robot_face", "\u{1F916}"),
    ("x", "\u{274C}"),
];

/// The emoji a reaction name means on Discord: a name Fleet maps, a custom
/// emoji as `name:id`, or a Unicode emoji as it is. `None` for any other
/// ASCII name.
pub fn emoji(name: &str) -> Option<String> {
    let name = name.trim_matches(':');
    if let Some((_, e)) = EMOJI.iter().find(|(n, _)| *n == name) {
        return Some((*e).to_string());
    }
    if let Some((n, id)) = name.split_once(':') {
        return (!n.is_empty() && is_snowflake(id)).then(|| name.to_string());
    }
    (!name.is_empty() && !name.is_ascii()).then(|| name.to_string())
}

/// Percent-encode every byte outside the unreserved set; `:` of a custom
/// emoji included.
fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `discord:<channel>[:<thread>]`, each a snowflake.
pub fn parse_address(address: &str) -> Result<ConversationRef, String> {
    let parsed = ConversationRef::split(address)
        .ok_or_else(|| format!("{address:?} is not <platform>:<chat>[:<thread>]"))?;
    check_ref(&parsed).map_err(|why| format!("{address:?}: {why}"))?;
    Ok(parsed)
}

fn check_ref(r: &ConversationRef) -> Result<(), String> {
    if r.platform != KIND {
        return Err(format!("platform {:?} is not {KIND}", r.platform));
    }
    if !is_snowflake(&r.chat) {
        return Err(format!("{:?} is not a Discord channel id", r.chat));
    }
    if let Some(t) = &r.thread
        && !is_snowflake(t)
    {
        return Err(format!("{t:?} is not a Discord thread id"));
    }
    Ok(())
}

/// A snowflake: an unsigned 64-bit integer in decimal.
pub fn is_snowflake(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.parse::<u64>().is_ok()
}

fn snowflake(v: &Value) -> Option<u64> {
    v.as_str().and_then(|s| s.parse().ok())
}

/// The Unix milliseconds a snowflake was made at.
pub fn snowflake_millis(id: &str) -> Option<Millis> {
    let n: u64 = id.parse().ok()?;
    (n >> 22).checked_add(EPOCH)
}

/// The snowflake just before every id made at or after `at`, for `after`.
fn snowflake_before(at: Millis) -> u64 {
    (at.saturating_sub(EPOCH) << 22).saturating_sub(1)
}

/// Where a history read starts: after a message, the thread's start, or a
/// time.
fn after_of(query: &HistoryQuery) -> u64 {
    match query {
        HistoryQuery::After { id: Some(id), .. } => id.parse().unwrap_or(0),
        HistoryQuery::After { id: None, .. } => 0,
        HistoryQuery::Since { at } => snowflake_before(*at),
    }
}

/// The nonce a post under `key` carries: the first 25 hex characters of the
/// key's SHA-256, so every resend of one key carries the same one.
pub fn nonce(key: &str) -> String {
    Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..NONCE_LEN]
        .to_string()
}

/// A thread's name: the text's first non-empty line, at most 100
/// characters, or `Fleet`.
pub fn thread_name(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty());
    match line {
        Some(l) => l.chars().take(100).collect(),
        None => "Fleet".into(),
    }
}

/// An ISO 8601 time as Discord writes it (`2024-10-27T03:33:20.000000+00:00`)
/// as Unix milliseconds.
pub fn iso_millis(s: &str) -> Option<Millis> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    let mut ms = 0i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.len() - frac.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let f = &frac[..digits];
        ms = format!("{:0<3}", &f[..f.len().min(3)]).parse().ok()?;
        rest = &frac[digits..];
    }
    let offset = match rest {
        "Z" | "+00:00" | "" => 0,
        o if o.len() == 6 && o.as_bytes()[3] == b':' => {
            let sign = match o.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hours: i64 = o.get(1..3)?.parse().ok()?;
            let minutes: i64 = o.get(4..6)?.parse().ok()?;
            sign * (hours * 60 + minutes)
        }
        _ => return None,
    };
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + se - offset * 60;
    u64::try_from(secs * 1000 + ms).ok()
}

// ----------------------------------------------------------------------
// Reading messages and events, pure.
// ----------------------------------------------------------------------

/// One Discord message object in the port's words. The account's `bot` is
/// the one author that is Fleet, and its own posts mention nobody; another
/// bot, or a webhook, is a `bot` author. The time is the snowflake's.
pub fn message_of(bot: &str, item: &Value, starts_thread: bool) -> Result<Message, String> {
    let id = item["id"]
        .as_str()
        .filter(|s| is_snowflake(s))
        .ok_or("a message has no id")?;
    let at = snowflake_millis(id).ok_or_else(|| format!("message id {id:?} is no snowflake"))?;
    let author = &item["author"];
    let author_id = author["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("message {id} has no author"))?;
    let kind = if author_id == bot {
        AuthorKind::Fleet
    } else if author["bot"] == true || item["webhook_id"].is_string() {
        AuthorKind::Bot
    } else {
        AuthorKind::Person
    };
    let name = author["global_name"]
        .as_str()
        .or_else(|| author["username"].as_str())
        .unwrap_or("")
        .to_string();
    let mentioned = item["mentions"]
        .as_array()
        .is_some_and(|ms| ms.iter().any(|m| m["id"] == bot));
    Ok(Message {
        id: id.to_string(),
        mentions_fleet: kind != AuthorKind::Fleet && mentioned,
        at,
        text: item["content"].as_str().unwrap_or("").to_string(),
        key: None,
        starts_thread,
        author: Author {
            id: author_id.into(),
            name,
            kind,
        },
    })
}

/// The messages the query covers, each once, oldest first, and how many
/// older ones an `After` read left out under its limit.
fn collect(all: Vec<Message>, query: &HistoryQuery) -> (Vec<Message>, u64) {
    let mut seen = BTreeSet::new();
    let mut messages: Vec<Message> = all
        .into_iter()
        .filter(|m| match query {
            HistoryQuery::After { id: Some(id), .. } => {
                m.id.parse::<u64>().ok() > id.parse::<u64>().ok()
            }
            HistoryQuery::After { id: None, .. } => true,
            HistoryQuery::Since { at } => m.at >= *at,
        })
        .filter(|m| seen.insert(m.id.clone()))
        .collect();
    messages.sort_by_key(|m| m.id.parse::<u64>().unwrap_or(0));
    let left_out = match query {
        HistoryQuery::After { limit, .. } => {
            let n = messages.len().saturating_sub(*limit as usize);
            messages.drain(..n);
            n as u64
        }
        HistoryQuery::Since { .. } => 0,
    };
    (messages, left_out)
}

/// What an envelope carries, read without any call. The payload is
/// `{dispatch, parentId}`: the raw Gateway dispatch, and the parent channel
/// the listener adds for a message in a thread, `null` in a channel. `MESSAGE_CREATE`,
/// `MESSAGE_UPDATE` and `MESSAGE_DELETE` are read; other dispatches, other
/// guilds, direct messages, private threads, system message types and an
/// update without new text are ignored. A payload without its dispatch,
/// a message without its `id` or `channel_id`, an envelope without its
/// `parentId` key, or a public thread's message with a `null` one is
/// Unknown.
pub fn read_event(account: &AccountConfig, envelope: &Envelope) -> Observation<Option<Delivery>> {
    let Some(outer) = envelope.payload.as_object() else {
        return Observation::unknown("the payload is not an object");
    };
    let Some(dispatch) = outer.get("dispatch").and_then(Value::as_object) else {
        return Observation::unknown("the payload has no dispatch object");
    };
    if dispatch.get("op").and_then(Value::as_u64) != Some(0) {
        return Observation::Known(None);
    }
    let t = dispatch.get("t").and_then(Value::as_str).unwrap_or("");
    if !matches!(t, "MESSAGE_CREATE" | "MESSAGE_UPDATE" | "MESSAGE_DELETE") {
        return Observation::Known(None);
    }
    let Some(d) = dispatch.get("d").and_then(Value::as_object) else {
        return Observation::unknown(format!("{t} has no d object"));
    };
    let s = |k: &str| str_of(d, k).filter(|v| !v.is_empty());
    match (s("guild_id"), &account.team) {
        (None, _) => return Observation::Known(None),
        (Some(g), Some(team)) if g != team => return Observation::Known(None),
        _ => {}
    }
    let (Some(id), Some(channel)) = (s("id"), s("channel_id")) else {
        return Observation::unknown(format!("{t} has no id or channel_id"));
    };
    let channel_type = d.get("channel_type").and_then(Value::as_u64);
    if channel_type == Some(PRIVATE_THREAD) {
        return Observation::Known(None);
    }
    // The listener names the thread's parent on every event, and `null`
    // for a message in a channel: an edit or a removal carries no channel
    // type, so a missing key cannot be told from a top-level message.
    let parent = match outer.get("parentId") {
        Some(Value::Null) => None,
        Some(Value::String(p)) if is_snowflake(p) => Some(p.as_str()),
        Some(other) => {
            return Observation::unknown(format!("parentId {other} is not a snowflake"));
        }
        None => {
            return Observation::unknown(format!(
                "the envelope has no parentId for message {id}; a listener sends null for a message in a channel"
            ));
        }
    };
    if channel_type.is_some_and(is_public_thread) && parent.is_none() {
        return Observation::unknown(format!(
            "message {id} is in thread {channel}, and the envelope's parentId is null"
        ));
    }
    // In a thread: the thread's conversation. In a channel: the thread the
    // message starts, or will.
    let (conversation, in_thread) = match parent {
        Some(p) => (conv(p, channel), true),
        None => (conv(channel, id), false),
    };
    let item = Value::Object(d.clone());
    let (delivery, event) = match t {
        "MESSAGE_CREATE" => {
            if !is_message_type(d.get("type").and_then(Value::as_u64)) {
                return Observation::Known(None);
            }
            let message = match message_of(&account.bot, &item, !in_thread) {
                Ok(m) => m,
                Err(why) => return Observation::unknown(why),
            };
            (
                format!("create/{id}"),
                Event::Message {
                    conversation,
                    message,
                },
            )
        }
        "MESSAGE_UPDATE" => {
            let Some(edited) = s("edited_timestamp") else {
                // An embed resolving, a pin: not an edit.
                return Observation::Known(None);
            };
            let Some(text) = s("content") else {
                // Without the message content intent an edit carries no text.
                return Observation::Known(None);
            };
            let Some(edited_at) = iso_millis(edited) else {
                return Observation::unknown(format!("edited_timestamp {edited:?} is unreadable"));
            };
            (
                format!("update/{id}/{edited}"),
                Event::Edited {
                    conversation,
                    id: id.into(),
                    text: text.into(),
                    edited_at,
                },
            )
        }
        _ => (
            format!("delete/{id}"),
            Event::Removed {
                conversation,
                id: id.into(),
            },
        ),
    };
    Observation::Known(Some(Delivery {
        id: delivery,
        verified: envelope.verified,
        received_at: envelope.received_at,
        direct: false,
        event,
    }))
}

fn conv(chat: &str, thread: &str) -> ConversationRef {
    ConversationRef {
        platform: KIND.into(),
        chat: chat.into(),
        thread: Some(thread.into()),
    }
}

fn str_of<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    m.get(k).and_then(Value::as_str)
}

// ----------------------------------------------------------------------
// One call's answer.
// ----------------------------------------------------------------------

/// Why a call gave no usable answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// HTTP 429: nothing was done; `retry_after` in milliseconds.
    Throttled {
        retry_after: Option<Millis>,
    },
    /// A 4xx answer: the request was not performed. `code` is Discord's
    /// JSON error code, 0 when it gave none.
    Refused {
        code: u64,
        why: String,
    },
    /// No answer that tells whether the request was performed: an
    /// unreadable body, or a 5xx.
    Unreadable(String),
    Timeout(String),
    Incomplete(String),
    /// `curl` could not start, so nothing was sent.
    Unavailable(String),
}

impl CallError {
    pub fn why(&self) -> String {
        match self {
            CallError::Throttled { retry_after } => match retry_after {
                Some(ms) => format!("Discord asked to slow down; retry after {ms}ms"),
                None => "Discord asked to slow down".into(),
            },
            CallError::Refused { why, .. }
            | CallError::Unreadable(why)
            | CallError::Timeout(why)
            | CallError::Incomplete(why)
            | CallError::Unavailable(why) => why.clone(),
        }
    }

    fn uncertainty(&self) -> Uncertainty {
        match self {
            CallError::Throttled { .. } => Uncertainty::Throttled,
            CallError::Refused { .. } => Uncertainty::Refused,
            CallError::Unreadable(_) => Uncertainty::Unreadable,
            CallError::Timeout(_) => Uncertainty::Timeout,
            CallError::Incomplete(_) => Uncertainty::Incomplete,
            CallError::Unavailable(_) => Uncertainty::Unavailable,
        }
    }

    /// Discord said no, asked Fleet to slow down, or curl never ran: the
    /// write was certainly not made.
    fn certain_failure(&self) -> bool {
        matches!(
            self,
            CallError::Refused { .. } | CallError::Throttled { .. } | CallError::Unavailable(_)
        )
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

/// The REST API's answer: its JSON on a 2xx, else why not.
pub fn answer(reply: &Reply, what: &str) -> Result<Value, CallError> {
    // `204 No Content`, as a reaction's create and delete answer.
    if (200..=299).contains(&reply.status) && reply.body.trim().is_empty() && reply.exit == Some(0)
    {
        return Ok(Value::Null);
    }
    let body: Result<Value, _> = serde_json::from_str(reply.body.trim());
    if reply.status == 429 {
        // `retry_after` is seconds as a float; the header whole seconds.
        let from_body = body
            .as_ref()
            .ok()
            .and_then(|v| v["retry_after"].as_f64())
            .map(|s| (s * 1000.0).ceil() as Millis);
        let from_header = reply
            .header("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|s| s.saturating_mul(1000));
        return Err(CallError::Throttled {
            retry_after: from_body.or(from_header).map(|ms| ms.max(1)),
        });
    }
    let v = match body {
        Ok(v) => v,
        Err(e) => {
            return Err(CallError::Unreadable(match (reply.exit, reply.status) {
                (Some(0) | None, 0) => format!("{what}: no answer: {e}"),
                (Some(0) | None, status) => {
                    format!("{what}: HTTP {status} with an unreadable body: {e}")
                }
                (Some(code), _) => format!("{what}: curl exited {code}: {}", reply.stderr),
            }));
        }
    };
    match reply.status {
        200..=299 => Ok(v),
        400..=499 => {
            let code = v["code"].as_u64().unwrap_or(0);
            let message = v["message"].as_str().unwrap_or("no message");
            Err(CallError::Refused {
                code,
                why: format!("{what}: HTTP {} {code} {message}", reply.status),
            })
        }
        status => Err(CallError::Unreadable(format!(
            "{what}: HTTP {status}; the request may have been performed"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reaction_name_maps_to_the_emoji_discord_takes() {
        assert_eq!(emoji("eyes").as_deref(), Some("\u{1F440}"));
        assert_eq!(emoji(":white_check_mark:").as_deref(), Some("\u{2705}"));
        assert_eq!(
            emoji(fleet_api::gateway::DEFAULT_QUEUED).as_deref(),
            Some("\u{23F3}")
        );
        assert_eq!(emoji("\u{1F680}").as_deref(), Some("\u{1F680}"));
        assert_eq!(
            emoji("fleet:1250000000000000000").as_deref(),
            Some("fleet:1250000000000000000")
        );
        for bad in ["", "no_such_emoji", "fleet:abc", ":1250000000000000000"] {
            assert_eq!(emoji(bad), None, "{bad:?}");
        }
        assert_eq!(url_encode("\u{1F440}"), "%F0%9F%91%80");
        assert_eq!(url_encode("fleet:12"), "fleet%3A12");
    }
    use fleet_api::gateway::Verified;

    const GUILD: &str = "1200000000000000000";
    const BOT: &str = "1210000000000000000";
    const CHANNEL: &str = "1230000000000000000";
    const THREAD: &str = "1300000000000000000";

    fn account(message_content: bool) -> AccountConfig {
        AccountConfig {
            platform: KIND.into(),
            team: Some(GUILD.into()),
            bot: BOT.into(),
            inbound: Inbound::DiscordGateway,
            message_content: Some(message_content),
            cli: Some("curl".into()),
            curl_config: Some("/etc/fleet/discord-curl.conf".into()),
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

    fn read(payload: &str) -> Observation<Option<Delivery>> {
        read_event(&account(false), &envelope(payload))
    }

    fn delivery(payload: &str) -> Delivery {
        read(payload)
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

    // Recorded dispatches: synthetic, with made-up snowflakes, in the
    // shapes Discord documents.
    const MENTION: &str = include_str!("../fixtures/discord/message-create-mention.json");
    const REPLY: &str = include_str!("../fixtures/discord/message-create-thread-reply.json");
    const OWN_POST: &str = include_str!("../fixtures/discord/message-create-own-post.json");
    const WEBHOOK: &str = include_str!("../fixtures/discord/message-create-webhook.json");
    const STARTER: &str = include_str!("../fixtures/discord/message-create-thread-starter.json");
    const NO_PARENT: &str = include_str!("../fixtures/discord/message-create-no-parent.json");
    const OTHER_GUILD: &str = include_str!("../fixtures/discord/message-create-other-guild.json");
    const DM: &str = include_str!("../fixtures/discord/message-create-dm.json");
    const PRIVATE: &str = include_str!("../fixtures/discord/message-create-private-thread.json");
    const EDIT: &str = include_str!("../fixtures/discord/message-update.json");
    const EMBED_UPDATE: &str = include_str!("../fixtures/discord/message-update-embed.json");
    const DELETE: &str = include_str!("../fixtures/discord/message-delete.json");
    const DELETE_NO_PARENT: &str =
        include_str!("../fixtures/discord/message-delete-no-parent.json");
    const READY: &str = include_str!("../fixtures/discord/ready.json");

    #[test]
    fn a_top_level_mention_is_the_conversation_of_the_thread_it_starts() {
        let d = delivery(MENTION);
        let mention_id = "1311111111111111111";
        assert_eq!(d.id, format!("create/{mention_id}"));
        assert_eq!(d.verified, Verified::Connection);
        let (conv, m) = message(&d);
        assert_eq!(conv.address(), format!("discord:{CHANNEL}:{mention_id}"));
        assert_eq!(m.id, mention_id);
        assert!(m.mentions_fleet && m.starts_thread);
        assert_eq!(m.author.kind, AuthorKind::Person);
        assert_eq!(m.author.name, "Alex");
        assert_eq!(m.at, snowflake_millis(mention_id).unwrap());
        assert_eq!(m.key, None);
        // A redelivery after a resume is the same delivery.
        assert_eq!(delivery(MENTION).id, d.id);
    }

    #[test]
    fn a_thread_message_names_the_parent_the_listener_added() {
        let d = delivery(REPLY);
        let (conv, m) = message(&d);
        assert_eq!(conv.address(), format!("discord:{CHANNEL}:{THREAD}"));
        assert!(!m.starts_thread && !m.mentions_fleet);
        assert_eq!(m.text, "here is the log");
        let own_post = delivery(OWN_POST);
        let (_, own) = message(&own_post);
        assert_eq!(own.author.kind, AuthorKind::Fleet);
        assert!(!own.mentions_fleet, "Fleet never mentions itself");
        let webhook = delivery(WEBHOOK);
        let (_, hook) = message(&webhook);
        assert_eq!(hook.author.kind, AuthorKind::Bot);
        assert!(
            read(NO_PARENT).is_unknown(),
            "a thread message without parentId cannot be placed"
        );
        assert!(
            read(DELETE_NO_PARENT).is_unknown(),
            "a removal carries no channel type: without parentId it cannot be placed"
        );
    }

    #[test]
    fn what_the_gateway_does_not_read_is_ignored() {
        for (what, payload) in [
            ("a thread starter reference", STARTER),
            ("another guild", OTHER_GUILD),
            ("a direct message", DM),
            ("a private thread", PRIVATE),
            ("an embed resolving", EMBED_UPDATE),
            ("a READY dispatch", READY),
        ] {
            assert_eq!(read(payload), Observation::Known(None), "{what}");
        }
        assert!(read("[]").is_unknown());
        assert!(read(r#"{"parentId": null}"#).is_unknown());
        let mut throttled = CallError::Throttled { retry_after: None };
        assert!(throttled.certain_failure(), "a 429 performed nothing");
        throttled = CallError::Timeout("slow".into());
        assert!(!throttled.certain_failure());
    }

    #[test]
    fn edits_and_removals_carry_their_own_delivery_ids() {
        let d = delivery(EDIT);
        match &d.event {
            Event::Edited {
                conversation,
                id,
                text,
                edited_at,
            } => {
                assert_eq!(
                    conversation.address(),
                    format!("discord:{CHANNEL}:{THREAD}")
                );
                assert_eq!(id, "1322222222222222222");
                assert_eq!(text, "here is the full log");
                assert_eq!(*edited_at, 1_730_000_400_000);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            d.id,
            "update/1322222222222222222/2024-10-27T03:40:00.000000+00:00"
        );
        let d = delivery(DELETE);
        assert_eq!(d.id, "delete/1322222222222222222");
        assert!(matches!(&d.event, Event::Removed { id, .. } if id == "1322222222222222222"));
    }

    #[test]
    fn addresses_snowflakes_nonces_and_times() {
        let r = parse_address(&format!("discord:{CHANNEL}:{THREAD}")).unwrap();
        assert_eq!(
            (r.chat.as_str(), r.thread.as_deref()),
            (CHANNEL, Some(THREAD))
        );
        assert!(parse_address(&format!("discord:{CHANNEL}")).is_ok());
        for bad in [
            "discord:general:1",
            "slack:C0123ABCD",
            &format!("discord:{CHANNEL}:x1"),
            "discord",
        ] {
            assert!(parse_address(bad).is_err(), "{bad}");
        }
        assert_eq!(
            snowflake_millis("175928847299117063"),
            Some(1_462_015_105_796)
        );
        assert_eq!(snowflake_before(EPOCH + 1) + 1, 1 << 22);
        let n = nonce("conversation/u1/refuse/U1");
        assert_eq!(n.len(), NONCE_LEN);
        assert_eq!(n, nonce("conversation/u1/refuse/U1"));
        assert_ne!(n, nonce("conversation/u1/refuse/U1/1"));
        assert_eq!(thread_name("\n  Hello there  \nmore"), "Hello there");
        assert_eq!(thread_name("").as_str(), "Fleet");
        assert_eq!(thread_name(&"x".repeat(150)).chars().count(), 100);
        assert_eq!(
            iso_millis("2024-10-27T03:40:00.000000+00:00"),
            Some(1_730_000_400_000)
        );
        assert_eq!(iso_millis("2024-10-27T03:40:00Z"), Some(1_730_000_400_000));
        assert_eq!(iso_millis("1970-01-01T00:00:00.123+00:00"), Some(123));
        assert_eq!(iso_millis("yesterday"), None);
        assert_eq!(intents(false), 513);
        assert_eq!(intents(true), 513 | 32_768);
    }

    #[test]
    fn answers_are_known_only_when_discord_says_so() {
        let parse = |s: &str| Reply::parse(s, Some(0), b"");
        assert_eq!(
            answer(&parse("HTTP/2 200 \r\n\r\n{\"id\":\"1\"}"), "GET /x").unwrap()["id"],
            "1"
        );
        let refused = answer(
            &parse("HTTP/2 404 \r\n\r\n{\"code\":10003,\"message\":\"Unknown Channel\"}"),
            "POST /channels/1/messages",
        )
        .unwrap_err();
        assert!(matches!(refused, CallError::Refused { code: 10_003, .. }));
        assert!(refused.certain_failure());
        let throttled = answer(
            &parse("HTTP/2 429 \r\nretry-after: 2\r\n\r\n{\"retry_after\":1.25,\"global\":false}"),
            "GET /x",
        )
        .unwrap_err();
        assert_eq!(
            throttled,
            CallError::Throttled {
                retry_after: Some(1250)
            }
        );
        let server = answer(&parse("HTTP/2 502 \r\n\r\n{}"), "POST /x").unwrap_err();
        assert!(matches!(server, CallError::Unreadable(_)) && !server.certain_failure());
        let html = answer(&parse("HTTP/2 502 \r\n\r\n<html>"), "POST /x").unwrap_err();
        assert!(matches!(html, CallError::Unreadable(_)));
    }

    #[test]
    fn an_account_needs_the_discord_listener_a_guild_and_a_curl_config() {
        let t = Duration::from_secs(1);
        assert!(DiscordGateway::new("d", &account(false), t).is_ok());
        let err = |a: AccountConfig| DiscordGateway::new("d", &a, t).unwrap_err();
        assert_eq!(
            err(AccountConfig {
                inbound: Inbound::SocketMode,
                ..account(false)
            }),
            "inbound \"socket-mode\" is not how a Discord listener delivers; use \"discord-gateway\""
        );
        assert_eq!(
            err(AccountConfig {
                team: None,
                ..account(false)
            }),
            "a discord account needs a team: its guild id"
        );
        assert_eq!(
            err(AccountConfig {
                curl_config: None,
                ..account(false)
            }),
            "a discord account needs a curlConfig"
        );
        assert_eq!(
            err(AccountConfig {
                bot: "fleet".into(),
                ..account(false)
            }),
            "a discord account's team and bot are snowflakes"
        );
    }
}
