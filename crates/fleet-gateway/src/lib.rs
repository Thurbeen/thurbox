//! Implementations of the gateway contract ([`fleet_api::gateway::Gateway`]).
//!
//! - [`SlackGateway`]: Slack's Web API through an externally authenticated
//!   `curl`, one bounded process per call.
//! - [`DiscordGateway`]: Discord's REST API the same way.
//! - [`FakeGateway`]: a file-backed chat platform for tests and trials. It
//!   runs no process and reaches no network, and it models every
//!   `capabilities` combination a real platform can present.
//! - [`GatewayAccounts`]: the adapters of a configuration's
//!   `[gateway.accounts]`, chosen by the account a call names.
//! - [`listener_account`]: what a listener needs to connect to an account,
//!   with the digest its heartbeats carry.
//!
//! This is the only crate that knows a chat platform's client exists. A
//! platform adapter makes bounded calls, each one process through
//! fleet-exec with its tree killed past the bound: no WebSocket, no resident
//! connection, no background thread. The listener outside Fleet holds the
//! connection. Credentials stay with the external client's own
//! configuration: Fleet passes none and stores none.

pub mod curl;
pub mod discord;
pub mod fake;
pub mod slack;
pub mod text;

use std::collections::BTreeMap;
use std::time::Duration;

pub use discord::DiscordGateway;
pub use fake::{FakeConversation, FakeGateway, FakeGatewayState, GatewayFault, PostRecord};
use fleet_api::gateway::{
    AccountConfig, Capabilities, ConversationRef, Delivery, Envelope, Gateway, HistoryObservation,
    HistoryQuery, Identity, Inbound, Uncertainty,
};
use fleet_api::plan::EffectOutcome;
use fleet_api::{Millis, Observation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub use slack::SlackGateway;

/// Which adapter serves a platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Discord,
    Fake,
    Slack,
}

impl Platform {
    pub fn parse(name: &str) -> Option<Platform> {
        match name {
            discord::KIND => Some(Platform::Discord),
            fake::KIND => Some(Platform::Fake),
            slack::KIND => Some(Platform::Slack),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Platform::Discord => discord::KIND,
            Platform::Fake => fake::KIND,
            Platform::Slack => slack::KIND,
        }
    }

    /// The platforms this build has, in the words `fleet version` prints.
    pub const ALL: &[&str] = &[discord::KIND, fake::KIND, slack::KIND];
}

/// The configured accounts, one adapter each.
pub struct GatewayAccounts {
    accounts: BTreeMap<String, (AccountConfig, Box<dyn Gateway>)>,
    call_timeout: Duration,
}

impl std::fmt::Debug for GatewayAccounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayAccounts")
            .field("accounts", &self.accounts.keys().collect::<Vec<_>>())
            .field("call_timeout", &self.call_timeout)
            .finish()
    }
}

impl GatewayAccounts {
    /// Build one adapter per account. `call_timeout` bounds every platform
    /// call. The checks here are the platform's own: a platform this build
    /// lacks, an `inbound` its listener cannot deliver by, a fake without a
    /// dir, a Slack account without a curl config. The pure rules ran at
    /// config load. Opening an adapter calls no platform.
    pub fn new(
        accounts: BTreeMap<String, AccountConfig>,
        call_timeout: Duration,
    ) -> Result<GatewayAccounts, String> {
        let mut out: BTreeMap<String, (AccountConfig, Box<dyn Gateway>)> = BTreeMap::new();
        for (name, cfg) in accounts {
            let at = format!("gateway.accounts.{name}");
            let platform = Platform::parse(&cfg.platform).ok_or_else(|| {
                format!(
                    "{at}: unknown platform {:?}; this build has {}",
                    cfg.platform,
                    Platform::ALL.join(", ")
                )
            })?;
            if cfg.message_content.is_some() && platform != Platform::Discord {
                return Err(format!(
                    "{at}: messageContent is a Discord account's key, not {:?}'s",
                    cfg.platform
                ));
            }
            let adapter: Box<dyn Gateway> = match platform {
                Platform::Fake => {
                    if cfg.inbound != Inbound::Connection {
                        return Err(format!(
                            "{at}: inbound {:?} is not how a fake listener delivers; use \"connection\"",
                            cfg.inbound.name()
                        ));
                    }
                    let dir = cfg
                        .dir
                        .as_ref()
                        .ok_or_else(|| format!("{at}: a fake account needs a dir"))?;
                    Box::new(
                        FakeGateway::open(&name, dir)
                            .map_err(|e| format!("{at}: fake gateway {}: {e}", dir.display()))?,
                    )
                }
                Platform::Discord => Box::new(
                    DiscordGateway::new(&name, &cfg, call_timeout)
                        .map_err(|e| format!("{at}: {e}"))?,
                ),
                Platform::Slack => Box::new(
                    SlackGateway::new(&name, &cfg, call_timeout)
                        .map_err(|e| format!("{at}: {e}"))?,
                ),
            };
            out.insert(name, (cfg, adapter));
        }
        Ok(GatewayAccounts {
            accounts: out,
            call_timeout,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.accounts.keys().map(String::as_str)
    }

    /// The bound every platform call gets.
    pub fn call_timeout(&self) -> Duration {
        self.call_timeout
    }

    pub fn account(&self, name: &str) -> Option<&AccountConfig> {
        self.accounts.get(name).map(|(c, _)| c)
    }

    fn adapter(&self, account: &str) -> Option<&dyn Gateway> {
        self.accounts.get(account).map(|(_, g)| g.as_ref())
    }

    fn not_configured(account: &str) -> String {
        format!("account {account} is not configured under [gateway.accounts]")
    }
}

impl Gateway for GatewayAccounts {
    fn operation_bound(&self) -> Millis {
        self.accounts
            .values()
            .map(|(_, g)| g.operation_bound())
            .max()
            .unwrap_or(0)
    }

    fn parse(&self, account: &str, address: &str) -> Observation<ConversationRef> {
        match self.adapter(account) {
            Some(g) => g.parse(account, address),
            None => Observation::unknown(Self::not_configured(account)),
        }
    }

    /// Pure reading dispatches on the configuration's platform, so a
    /// receive verb can read an envelope for any configured account.
    fn event(&self, account: &AccountConfig, envelope: &Envelope) -> Observation<Option<Delivery>> {
        match self
            .accounts
            .values()
            .find(|(c, _)| c.platform == account.platform)
        {
            Some((_, g)) => g.event(account, envelope),
            None => Observation::unknown(format!(
                "no configured account runs on platform {:?}",
                account.platform
            )),
        }
    }

    fn whoami(&self, account: &str) -> Observation<Identity> {
        match self.adapter(account) {
            Some(g) => g.whoami(account),
            None => Observation::unknown(Self::not_configured(account)),
        }
    }

    fn capabilities(&self, account: &str) -> Observation<Capabilities> {
        match self.adapter(account) {
            Some(g) => g.capabilities(account),
            None => Observation::unknown(Self::not_configured(account)),
        }
    }

    fn history(
        &self,
        account: &str,
        conversation: &ConversationRef,
        query: &HistoryQuery,
    ) -> HistoryObservation {
        match self.adapter(account) {
            Some(g) => g.history(account, conversation, query),
            None => {
                HistoryObservation::unknown(Uncertainty::Unavailable, Self::not_configured(account))
            }
        }
    }

    fn open(
        &self,
        account: &str,
        channel: &ConversationRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        match self.adapter(account) {
            Some(g) => g.open(account, channel, text, intent_key),
            // Nothing ran: a Known refusal, not a question.
            None => Observation::Known(EffectOutcome::Failed {
                why: Self::not_configured(account),
            }),
        }
    }

    fn post(
        &self,
        account: &str,
        conversation: &ConversationRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        match self.adapter(account) {
            Some(g) => g.post(account, conversation, text, intent_key),
            None => Observation::Known(EffectOutcome::Failed {
                why: Self::not_configured(account),
            }),
        }
    }
}

/// What a listener starts from, as `fleet gateway account NAME --json` will
/// print it: the account's name, platform, workspace, bot and inbound
/// method, the Gateway intents of a Discord account, and the digest of
/// exactly those, which the listener's heartbeats
/// carry so Fleet can tell a listener on an old configuration. The client
/// and credential keys are Fleet's side and do not move the digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListenerAccount {
    pub name: String,
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub bot: String,
    pub inbound: Inbound,
    /// Discord only: the Gateway intents the listener identifies with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intents: Option<u64>,
    /// `sha256:<64 hex>` over the other fields.
    pub digest: String,
}

/// The listener-facing view of an account, with its digest.
pub fn listener_account(name: &str, account: &AccountConfig) -> ListenerAccount {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Identity<'a> {
        name: &'a str,
        platform: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        team: Option<&'a str>,
        bot: &'a str,
        inbound: Inbound,
        #[serde(skip_serializing_if = "Option::is_none")]
        intents: Option<u64>,
    }
    let intents = (account.platform == discord::KIND)
        .then(|| discord::intents(account.message_content.unwrap_or(false)));
    let identity = Identity {
        name,
        platform: &account.platform,
        team: account.team.as_deref(),
        bot: &account.bot,
        inbound: account.inbound,
        intents,
    };
    let bytes = serde_json::to_vec(&identity).expect("a struct of strings serialises");
    let hex: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    ListenerAccount {
        name: name.to_string(),
        platform: account.platform.clone(),
        team: account.team.clone(),
        bot: account.bot.clone(),
        inbound: account.inbound,
        intents,
        digest: format!("sha256:{hex}"),
    }
}

/// The digest alone.
pub fn account_digest(name: &str, account: &AccountConfig) -> String {
    listener_account(name, account).digest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(platform: &str) -> AccountConfig {
        AccountConfig {
            platform: platform.into(),
            team: Some("T0123ABCD".into()),
            bot: "U0BOT0001".into(),
            inbound: Inbound::Connection,
            message_content: None,
            cli: Some("curl".into()),
            curl_config: Some("/etc/fleet/slack-curl.conf".into()),
            dir: None,
        }
    }

    #[test]
    fn the_digest_covers_what_a_listener_connects_with_and_nothing_else() {
        let a = account("fake");
        let la = listener_account("fake-main", &a);
        assert!(la.digest.starts_with("sha256:") && la.digest.len() == 7 + 64);
        assert_eq!(la.digest, account_digest("fake-main", &a));
        let mut same = a.clone();
        same.cli = Some("/opt/curl".into());
        same.curl_config = Some("/other.conf".into());
        same.dir = Some("elsewhere".into());
        assert_eq!(account_digest("fake-main", &same), la.digest);
        for (what, moved) in [
            ("name", account_digest("fake-other", &a)),
            ("platform", account_digest("fake-main", &account("slack"))),
            (
                "bot",
                account_digest(
                    "fake-main",
                    &AccountConfig {
                        bot: "U0BOT0002".into(),
                        ..a.clone()
                    },
                ),
            ),
            (
                "team",
                account_digest(
                    "fake-main",
                    &AccountConfig {
                        team: None,
                        ..a.clone()
                    },
                ),
            ),
            (
                "inbound",
                account_digest(
                    "fake-main",
                    &AccountConfig {
                        inbound: Inbound::SocketMode,
                        ..a.clone()
                    },
                ),
            ),
        ] {
            assert_ne!(moved, la.digest, "{what} moves the digest");
        }
        let v = serde_json::to_value(&la).unwrap();
        assert_eq!(v["inbound"], "connection");
        assert_eq!(v["name"], "fake-main");
        assert!(v.get("cli").is_none() && v.get("curlConfig").is_none());
    }

    #[test]
    fn an_account_on_a_platform_this_build_lacks_is_refused_with_the_list() {
        let err = GatewayAccounts::new(
            BTreeMap::from([("tg-main".to_string(), account("telegram"))]),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(
            err,
            "gateway.accounts.tg-main: unknown platform \"telegram\"; this build has discord, fake, slack"
        );
        let accounts = GatewayAccounts::new(
            BTreeMap::from([(
                "slack-main".to_string(),
                AccountConfig {
                    inbound: Inbound::SocketMode,
                    ..account("slack")
                },
            )]),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(accounts.names().collect::<Vec<_>>(), ["slack-main"]);
        assert!(
            accounts.operation_bound() > 0,
            "the Slack adapter reports its bound"
        );
        let err = GatewayAccounts::new(
            BTreeMap::from([(
                "slack-main".to_string(),
                AccountConfig {
                    inbound: Inbound::SocketMode,
                    curl_config: None,
                    ..account("slack")
                },
            )]),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(
            err,
            "gateway.accounts.slack-main: a slack account needs a curlConfig"
        );
        let err = GatewayAccounts::new(
            BTreeMap::from([("slack-main".to_string(), account("slack"))]),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(
            err,
            "gateway.accounts.slack-main: inbound \"connection\" is not how a Slack listener delivers; use \"socket-mode\" or \"events-api\""
        );
        let err = GatewayAccounts::new(
            BTreeMap::from([("fake-main".to_string(), account("fake"))]),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(
            err,
            "gateway.accounts.fake-main: a fake account needs a dir"
        );
        let err = GatewayAccounts::new(
            BTreeMap::from([(
                "fake-main".to_string(),
                AccountConfig {
                    inbound: Inbound::EventsApi,
                    dir: Some("x".into()),
                    ..account("fake")
                },
            )]),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(err.contains("inbound \"events-api\" is not how a fake listener delivers"));
        assert_eq!(Platform::parse("fake"), Some(Platform::Fake));
        assert_eq!(Platform::parse("slack"), Some(Platform::Slack));
        assert_eq!(Platform::parse("telegram"), None);
        assert_eq!(Platform::ALL, ["discord", "fake", "slack"]);
        assert_eq!(Platform::parse("discord"), Some(Platform::Discord));
    }
}
