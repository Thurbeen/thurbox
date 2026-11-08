//! What every curl-backed platform adapter shares: one HTTP answer as
//! `curl --include` prints it, the budget of calls and time one operation
//! may spend, how a split text's sends end, and the `Gateway` impl around
//! the platform's own calls. What an answer means is each platform's own.

use std::time::{Duration, Instant};

use fleet_api::Observation;
use fleet_api::plan::EffectOutcome;

/// One HTTP answer as `curl --include` prints it: the status line and
/// headers, then the body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    /// 0 when no status line was printed.
    pub status: u16,
    /// Lowercased names.
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub exit: Option<i32>,
    /// The first non-empty line of stderr, at most 200 characters.
    pub stderr: String,
}

impl Reply {
    /// Split `--include` output into its last head and the body: an
    /// informational head (`100 Continue`) before the final one is skipped.
    pub fn parse(stdout: &str, exit: Option<i32>, stderr: &[u8]) -> Reply {
        let mut rest = stdout;
        let mut status = 0;
        let mut headers = Vec::new();
        while rest.starts_with("HTTP/") {
            let (head, body) = match rest.find("\r\n\r\n") {
                Some(i) => (&rest[..i], &rest[i + 4..]),
                None => match rest.find("\n\n") {
                    Some(i) => (&rest[..i], &rest[i + 2..]),
                    None => (rest, ""),
                },
            };
            let mut lines = head.lines();
            status = lines
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                .collect();
            rest = body;
        }
        Reply {
            status,
            headers,
            body: rest.to_string(),
            exit,
            stderr: first_line(stderr),
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(200)
        .collect()
}

/// The time and calls one operation may spend: `call_timeout` times
/// `calls`, and no more calls than that either.
#[derive(Debug)]
pub struct Budget {
    deadline: Instant,
    calls_left: u32,
    call_timeout: Duration,
}

impl Budget {
    pub fn new(call_timeout: Duration, calls: u32) -> Budget {
        Budget {
            deadline: Instant::now() + call_timeout * calls,
            calls_left: calls,
            call_timeout,
        }
    }

    /// The timeout of the next call, or `None` when the budget is spent.
    pub fn next_timeout(&mut self) -> Option<Duration> {
        if self.calls_left == 0 {
            return None;
        }
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        self.calls_left -= 1;
        Some(left.min(self.call_timeout))
    }
}

/// How a send that did not land ends: Unknown when an earlier part of the
/// text landed (`partly` says what), a Known refusal when the platform
/// certainly did nothing, and Unknown otherwise.
pub fn send_failure(
    partly: Option<String>,
    why: String,
    certain: bool,
) -> Observation<EffectOutcome> {
    match partly {
        Some(landed) => Observation::unknown(format!("{landed}, then: {why}")),
        None if certain => Observation::Known(EffectOutcome::Failed { why }),
        None => Observation::unknown(why),
    }
}

/// What a text that landed answers: `Posted` in a thread, or `Opened` with
/// the thread its first post started in `chat`.
pub fn landed(
    platform: &str,
    chat: &str,
    thread: Option<&str>,
    message: String,
    at: fleet_api::Millis,
) -> EffectOutcome {
    match thread {
        Some(_) => EffectOutcome::Posted { message, at },
        None => EffectOutcome::Opened {
            conversation: fleet_api::gateway::ConversationRef {
                platform: platform.into(),
                chat: chat.into(),
                thread: Some(message.clone()),
            },
            message,
            at,
        },
    }
}

/// The [`Gateway`] of a curl-backed platform: the account check, the address
/// check and the thread or channel dispatch around each call are the same
/// for every one. It is invoked in the adapter's module, which supplies the
/// fields `account` and `call_timeout`, the constant
/// `MAX_CALLS_PER_OPERATION`, the functions `check_ref`, `parse_address` and
/// `read_event`, and the methods `identity`, `offers`, `thread_history`,
/// `channel_history`, `post_chunks`, `post_channel`, `react_on` and
/// `thread_link`. A macro, since the port is another
/// crate's trait.
///
/// [`Gateway`]: fleet_api::gateway::Gateway
macro_rules! curl_gateway {
    ($t:ty, $name:literal) => {
        impl $t {
            fn other_account(&self, account: &str) -> Option<String> {
                (account != self.account).then(|| {
                    format!(
                        "account {account} is not this {} gateway, which serves {}",
                        $name, self.account
                    )
                })
            }
        }

        impl Gateway for $t {
            fn operation_bound(&self) -> Millis {
                let one = fleet_exec::worst_case(self.call_timeout) * MAX_CALLS_PER_OPERATION;
                u64::try_from(one.as_millis()).unwrap_or(u64::MAX)
            }

            fn parse(&self, account: &str, address: &str) -> Observation<ConversationRef> {
                if let Some(why) = self.other_account(account) {
                    return Observation::unknown(why);
                }
                match parse_address(address) {
                    Ok(r) => Observation::Known(r),
                    Err(why) => Observation::unknown(why),
                }
            }

            fn event(
                &self,
                account: &AccountConfig,
                envelope: &Envelope,
            ) -> Observation<Option<Delivery>> {
                read_event(account, envelope)
            }

            fn whoami(&self, account: &str) -> Observation<Identity> {
                match self.other_account(account) {
                    Some(why) => Observation::unknown(why),
                    None => self.identity(),
                }
            }

            fn capabilities(&self, account: &str) -> Observation<Capabilities> {
                match self.other_account(account) {
                    Some(why) => Observation::unknown(why),
                    None => self.offers(),
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
                if let Err(why) = check_ref(conversation) {
                    return HistoryObservation::unknown(Uncertainty::Unreadable, why);
                }
                match &conversation.thread {
                    Some(thread) => self.thread_history(&conversation.chat, thread, query),
                    None => self.channel_history(&conversation.chat, query),
                }
            }

            fn open(
                &self,
                account: &str,
                channel: &ConversationRef,
                text: &str,
                intent_key: &str,
            ) -> Observation<EffectOutcome> {
                if let Some(why) = self.other_account(account) {
                    return Observation::Known(EffectOutcome::Failed { why });
                }
                if let Err(why) = check_ref(channel) {
                    return Observation::Known(EffectOutcome::Failed { why });
                }
                if channel.thread.is_some() {
                    return Observation::Known(EffectOutcome::Failed {
                        why: format!("{} is a thread, not a channel", channel.address()),
                    });
                }
                self.post_chunks(&channel.chat, None, text, intent_key)
            }

            fn post(
                &self,
                account: &str,
                conversation: &ConversationRef,
                text: &str,
                intent_key: &str,
            ) -> Observation<EffectOutcome> {
                if let Some(why) = self.other_account(account) {
                    return Observation::Known(EffectOutcome::Failed { why });
                }
                if let Err(why) = check_ref(conversation) {
                    return Observation::Known(EffectOutcome::Failed { why });
                }
                match &conversation.thread {
                    Some(thread) => {
                        self.post_chunks(&conversation.chat, Some(thread), text, intent_key)
                    }
                    // A channel address: top-level posts, no thread.
                    None => self.post_channel(&conversation.chat, text, intent_key),
                }
            }

            fn react(
                &self,
                account: &str,
                conversation: &ConversationRef,
                message: &str,
                reaction: &str,
                on: bool,
            ) -> Observation<EffectOutcome> {
                if let Some(why) = self.other_account(account) {
                    return Observation::Known(EffectOutcome::Failed { why });
                }
                if let Err(why) = check_ref(conversation) {
                    return Observation::Known(EffectOutcome::Failed { why });
                }
                self.react_on(conversation, message, reaction, on)
            }

            fn link(&self, account: &str, thread: &ConversationRef) -> Option<String> {
                if self.other_account(account).is_some() || check_ref(thread).is_err() {
                    return None;
                }
                self.thread_link(thread)
            }
        }
    };
}
pub(crate) use curl_gateway;
