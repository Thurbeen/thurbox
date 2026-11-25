pub mod curl;
pub mod discord;
pub mod fake;
pub mod slack;
pub mod text;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use discord::DiscordGateway;
pub use fake::{FakeConversation, FakeGateway, FakeGatewayState, GatewayFault, PostRecord};
use fleet_api::gateway::{
    AccountConfig, Capabilities, ConversationRef, Delivery, Envelope, Fetched, Gateway,
    HistoryObservation, HistoryQuery, Identity, Inbound, Uncertainty,
};
use fleet_api::plan::EffectOutcome;
use fleet_api::{Millis, Observation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub use slack::SlackGateway;

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

    pub const ALL: &[&str] = &[discord::KIND, fake::KIND, slack::KIND];
}

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

    pub fn check_channel(&self, account: &str, channel: &str) -> Result<(), String> {
        let (cfg, _) = self
            .accounts
            .get(account)
            .ok_or_else(|| Self::not_configured(account))?;
        let address = format!("{}:{channel}", cfg.platform);
        let parsed = match Platform::parse(&cfg.platform) {
            Some(Platform::Discord) => discord::parse_address(&address),
            Some(Platform::Fake) => fake::parse_address(&address),
            Some(Platform::Slack) => slack::parse_address(&address),
            None => Err(format!("unknown platform {:?}", cfg.platform)),
        }?;
        if parsed.thread.is_some() || parsed.chat != channel {
            return Err(format!(
                "{address:?} is not one channel id on {}",
                cfg.platform
            ));
        }
        Ok(())
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

    fn react(
        &self,
        account: &str,
        conversation: &ConversationRef,
        message: &str,
        reaction: &str,
        on: bool,
    ) -> Observation<EffectOutcome> {
        match self.adapter(account) {
            Some(g) => g.react(account, conversation, message, reaction, on),
            None => Observation::Known(EffectOutcome::Failed {
                why: Self::not_configured(account),
            }),
        }
    }

    fn link(&self, account: &str, thread: &ConversationRef) -> Option<String> {
        self.adapter(account).and_then(|g| g.link(account, thread))
    }

    fn fetch(
        &self,
        account: &str,
        conversation: &ConversationRef,
        file: &str,
        dir: &Path,
    ) -> Observation<Fetched> {
        match self.adapter(account) {
            Some(g) => g.fetch(account, conversation, file, dir),
            None => Observation::Known(Fetched::Refused {
                why: Self::not_configured(account),
            }),
        }
    }
}

pub fn fresh_private_dir(parent: &Path, stem: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(parent)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    for n in 0..100 {
        let path = parent.join(format!("{stem}-{}-{nanos}-{n}", std::process::id()));
        match private_dir_builder().create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "no fresh directory {stem}-… could be made under {}",
            parent.display()
        ),
    ))
}

#[cfg(unix)]
fn private_dir_builder() -> std::fs::DirBuilder {
    let mut builder = std::fs::DirBuilder::new();
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
}

#[cfg(not(unix))]
fn private_dir_builder() -> std::fs::DirBuilder {
    std::fs::DirBuilder::new()
}

pub fn fresh_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

pub fn saved_name(id: &str, name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim_start_matches('.');
    let clean: String = clean.chars().take(100).collect();
    if clean.is_empty() || clean.chars().all(|c| c == '_') {
        let id: String = id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(100)
            .collect();
        if id.is_empty() { "file".into() } else { id }
    } else {
        clean
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListenerAccount {
    pub name: String,
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub bot: String,
    pub inbound: Inbound,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intents: Option<u64>,
    pub digest: String,
}

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
    fn a_log_channel_is_checked_by_the_accounts_own_address_rules() {
        let accounts = GatewayAccounts::new(
            BTreeMap::from([
                (
                    "slack-main".to_string(),
                    AccountConfig {
                        inbound: Inbound::SocketMode,
                        ..account("slack")
                    },
                ),
                (
                    "discord-main".to_string(),
                    AccountConfig {
                        team: Some("1200000000000000000".into()),
                        bot: "1210000000000000000".into(),
                        inbound: Inbound::DiscordGateway,
                        ..account("discord")
                    },
                ),
            ]),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(accounts.check_channel("slack-main", "C0456EFGH"), Ok(()));
        assert!(accounts.check_channel("slack-main", "general").is_err());
        assert!(
            accounts
                .check_channel("slack-main", "C0456EFGH:1730000000.000100")
                .is_err(),
            "a thread is not a channel"
        );
        assert_eq!(
            accounts.check_channel("discord-main", "1300000000000000000"),
            Ok(())
        );
        assert!(accounts.check_channel("discord-main", "C0456EFGH").is_err());
        assert!(accounts.check_channel("nobody", "C0456EFGH").is_err());
        // Discord links a thread under its guild; Slack makes no link.
        let thread =
            ConversationRef::split("discord:1300000000000000000:1300000000000000001").unwrap();
        assert_eq!(
            accounts.link("discord-main", &thread).as_deref(),
            Some("https://discord.com/channels/1200000000000000000/1300000000000000001")
        );
        let thread = ConversationRef::split("slack:C0123ABCD:1730000000.000100").unwrap();
        assert_eq!(accounts.link("slack-main", &thread), None);
    }

    #[test]
    fn a_fresh_private_dir_is_new_each_call_and_only_its_owner_enters_it() {
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/fresh-private-dir")
            .join(std::process::id().to_string());
        let a = fresh_private_dir(&parent, "fleet-file-F1").unwrap();
        let b = fresh_private_dir(&parent, "fleet-file-F1").unwrap();
        assert_ne!(a, b);
        assert!(
            a.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("fleet-file-F1-")
        );
        let part = a.join(".part");
        fresh_private_file(&part).unwrap();
        assert_eq!(
            fresh_private_file(&part).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists,
            "a file already there is never reused"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&a), 0o700);
            assert_eq!(mode(&part), 0o600);
            let target = parent.join("elsewhere");
            std::os::unix::fs::symlink(&target, a.join(".link")).unwrap();
            assert_eq!(
                fresh_private_file(&a.join(".link")).unwrap_err().kind(),
                std::io::ErrorKind::AlreadyExists,
                "a planted link is not followed"
            );
            assert!(!target.exists());
        }
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn a_saved_name_is_one_plain_path_segment() {
        assert_eq!(saved_name("F1", "error.png"), "error.png");
        assert_eq!(
            saved_name("F1", "screen shot (1).png"),
            "screen_shot__1_.png"
        );
        assert_eq!(saved_name("F1", "../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(saved_name("F1", ".bashrc"), "bashrc");
        assert_eq!(saved_name("F1", ""), "F1");
        assert_eq!(saved_name("F1", "日本"), "F1");
        assert_eq!(saved_name("", ""), "file");
        assert_eq!(saved_name("F1", &"a".repeat(300)).len(), 100);
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
