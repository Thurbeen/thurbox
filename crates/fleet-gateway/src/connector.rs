use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use fleet_api::gateway::{
    AccountConfig, Capabilities, ConnectorConfig, ConversationRef, Delivery, Envelope, Fetched,
    Gateway, History, HistoryObservation, HistoryQuery, Identity, Uncertainty,
};
use fleet_api::plan::EffectOutcome;
use fleet_api::{Millis, Observation};
use fleet_exec::{Outcome, Spec};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

pub const PROTOCOL: u64 = 1;
pub const REQUIRED_VERBS: &[&str] = &[
    "manifest",
    "parse",
    "event",
    "whoami",
    "capabilities",
    "post",
];
pub const OPTIONAL_VERBS: &[&str] = &["history", "open", "react", "link", "fetch"];
const ANSWER_CAP: usize = 4 << 20;
const CALLS_PER_OPERATION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub protocol: u64,
    pub platform: String,
    pub verbs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
enum Wire {
    Known(Value),
    Unknown {
        why: String,
        #[serde(default)]
        kind: Option<Uncertainty>,
        #[serde(default)]
        retry_after: Option<Millis>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Known(Value),
    Unknown {
        why: String,
        kind: Option<Uncertainty>,
        retry_after: Option<Millis>,
    },
}

impl Answer {
    fn unknown(why: String) -> Answer {
        Answer::Unknown {
            why,
            kind: None,
            retry_after: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConnectorProgram {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub call_timeout: Duration,
}

impl ConnectorProgram {
    pub fn new(name: &str, config: &ConnectorConfig, default_timeout: Duration) -> Self {
        ConnectorProgram {
            name: name.to_string(),
            program: config.program.clone(),
            args: config.args.clone(),
            call_timeout: config
                .call_timeout_ms
                .map_or(default_timeout, Duration::from_millis),
        }
    }

    pub fn call(&self, verb: &str, mut request: Value) -> Answer {
        if let Some(fields) = request.as_object_mut() {
            fields.insert("protocol".into(), json!(PROTOCOL));
        }
        let mut spec = Spec::new(&self.program, self.call_timeout)
            .args(&self.args)
            .arg(verb)
            .stdin(request.to_string().into_bytes());
        spec.output_limit = Some(ANSWER_CAP);
        let out = fleet_exec::run(&spec);
        let Outcome::Exited {
            code: Some(0),
            stdout,
            ..
        } = &out
        else {
            return Answer::unknown(format!(
                "connector {} ({}) {verb}: {}",
                self.name,
                self.program,
                out.failure()
            ));
        };
        match serde_json::from_slice::<Wire>(stdout) {
            Ok(Wire::Known(value)) => Answer::Known(value),
            Ok(Wire::Unknown {
                why,
                kind,
                retry_after,
            }) => Answer::Unknown {
                why: format!("connector {} {verb}: {why}", self.name),
                kind,
                retry_after,
            },
            Err(e) => Answer::unknown(format!(
                "connector {} {verb} answered outside the protocol: {e}",
                self.name
            )),
        }
    }

    pub fn manifest(&self, request: Value) -> Result<Manifest, String> {
        let value = match self.call("manifest", request) {
            Answer::Known(v) => v,
            Answer::Unknown { why, .. } => return Err(why),
        };
        let manifest: Manifest = serde_json::from_value(value).map_err(|e| {
            format!(
                "connector {} manifest answered outside the protocol: {e}",
                self.name
            )
        })?;
        if manifest.protocol != PROTOCOL {
            return Err(format!(
                "connector {} speaks protocol {}, not {PROTOCOL}",
                self.name, manifest.protocol
            ));
        }
        if manifest.platform != self.name {
            return Err(format!(
                "connector {} speaks for platform {:?}, not {:?}",
                self.name, manifest.platform, self.name
            ));
        }
        if let Some(missing) = REQUIRED_VERBS
            .iter()
            .find(|v| !manifest.verbs.iter().any(|m| m == *v))
        {
            return Err(format!(
                "connector {} lacks the required verb {missing}",
                self.name
            ));
        }
        Ok(manifest)
    }
}

pub fn account_request(name: Option<&str>, account: &AccountConfig) -> Value {
    let mut out = json!({
        "platform": account.platform,
        "bot": account.bot,
        "inbound": account.inbound,
    });
    if let Some(name) = name {
        out["name"] = json!(name);
    }
    if let Some(team) = &account.team {
        out["team"] = json!(team);
    }
    if let Some(credentials) = &account.credentials {
        out["credentials"] = json!(credentials);
    }
    if !account.options.is_empty() {
        out["options"] = json!(account.options);
    }
    out
}

#[derive(Debug)]
pub struct ConnectorGateway {
    program: ConnectorProgram,
    account: String,
    config: AccountConfig,
    manifest: Mutex<Option<Manifest>>,
}

impl ConnectorGateway {
    pub fn new(program: ConnectorProgram, account: &str, config: &AccountConfig) -> Self {
        ConnectorGateway {
            program,
            account: account.to_string(),
            config: config.clone(),
            manifest: Mutex::new(None),
        }
    }

    fn request(&self, config: &AccountConfig, fields: Value) -> Value {
        let name = (config == &self.config).then_some(self.account.as_str());
        let mut request = fields;
        request["account"] = account_request(name, config);
        request
    }

    fn manifest(&self) -> Result<Manifest, String> {
        let mut cached = self
            .manifest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(m) = cached.as_ref() {
            return Ok(m.clone());
        }
        let manifest = self
            .program
            .manifest(self.request(&self.config, json!({})))?;
        *cached = Some(manifest.clone());
        Ok(manifest)
    }

    fn offers(&self, verb: &str) -> Result<bool, String> {
        self.manifest().map(|m| m.verbs.iter().any(|v| v == verb))
    }

    fn call(&self, config: &AccountConfig, verb: &str, fields: Value) -> Answer {
        if let Err(why) = self.manifest() {
            return Answer::unknown(why);
        }
        self.program.call(verb, self.request(config, fields))
    }

    fn ask<T: DeserializeOwned>(&self, verb: &str, fields: Value) -> Observation<T> {
        self.ask_as(&self.config, verb, fields)
    }

    fn ask_as<T: DeserializeOwned>(
        &self,
        config: &AccountConfig,
        verb: &str,
        fields: Value,
    ) -> Observation<T> {
        match self.call(config, verb, fields) {
            Answer::Known(value) => serde_json::from_value(value).map_or_else(
                |e| {
                    Observation::unknown(format!(
                        "connector {} {verb} answered outside the protocol: {e}",
                        self.program.name
                    ))
                },
                Observation::Known,
            ),
            Answer::Unknown { why, .. } => Observation::unknown(why),
        }
    }

    fn platform_check(&self, verb: &str, conversation: &ConversationRef) -> Result<(), String> {
        if conversation.platform == self.config.platform {
            Ok(())
        } else {
            Err(format!(
                "connector {} {verb} named platform {:?}, not {:?}",
                self.program.name, conversation.platform, self.config.platform
            ))
        }
    }

    fn effect(
        &self,
        verb: &str,
        fields: Value,
        accept: fn(&EffectOutcome) -> bool,
    ) -> Observation<EffectOutcome> {
        match self.offers(verb) {
            Err(why) => return Observation::unknown(why),
            Ok(false) => {
                return Observation::Known(EffectOutcome::Failed {
                    why: format!("connector {} offers no {verb}", self.program.name),
                });
            }
            Ok(true) => {}
        }
        match self.ask::<EffectOutcome>(verb, fields) {
            Observation::Known(outcome) if accept(&outcome) => Observation::Known(outcome),
            Observation::Known(other) => Observation::unknown(format!(
                "connector {} {verb} answered an outcome it cannot have: {other:?}",
                self.program.name
            )),
            unknown => unknown,
        }
    }
}

impl Gateway for ConnectorGateway {
    fn operation_bound(&self) -> Millis {
        let bound = fleet_exec::worst_case(self.program.call_timeout) * CALLS_PER_OPERATION;
        u64::try_from(bound.as_millis()).unwrap_or(u64::MAX)
    }

    fn parse(&self, _account: &str, address: &str) -> Observation<ConversationRef> {
        match self.ask::<ConversationRef>("parse", json!({ "address": address })) {
            Observation::Known(c) => match self.platform_check("parse", &c) {
                Ok(()) => Observation::Known(c),
                Err(why) => Observation::unknown(why),
            },
            unknown => unknown,
        }
    }

    fn event(&self, account: &AccountConfig, envelope: &Envelope) -> Observation<Option<Delivery>> {
        let fields = json!({ "envelope": envelope });
        match self.ask_as::<Option<Delivery>>(account, "event", fields) {
            Observation::Known(Some(mut delivery)) => {
                if let Err(why) = self.platform_check("event", delivery.event.conversation()) {
                    return Observation::unknown(why);
                }
                delivery.verified = envelope.verified;
                Observation::Known(Some(delivery))
            }
            other => other,
        }
    }

    fn whoami(&self, _account: &str) -> Observation<Identity> {
        self.ask("whoami", json!({}))
    }

    fn capabilities(&self, _account: &str) -> Observation<Capabilities> {
        self.ask("capabilities", json!({}))
    }

    fn history(
        &self,
        _account: &str,
        conversation: &ConversationRef,
        query: &HistoryQuery,
    ) -> HistoryObservation {
        match self.offers("history") {
            Err(why) => return HistoryObservation::unknown(Uncertainty::Unavailable, why),
            Ok(false) => return HistoryObservation::Unsupported,
            Ok(true) => {}
        }
        let fields = json!({ "conversation": conversation, "query": query });
        match self.call(&self.config, "history", fields) {
            Answer::Known(value) => match serde_json::from_value::<History>(value) {
                Ok(history) => HistoryObservation::Known(history),
                Err(e) => HistoryObservation::unknown(
                    Uncertainty::Unreadable,
                    format!(
                        "connector {} history answered outside the protocol: {e}",
                        self.program.name
                    ),
                ),
            },
            Answer::Unknown {
                why,
                kind,
                retry_after,
            } => HistoryObservation::Unknown {
                kind: kind.unwrap_or(Uncertainty::Unreadable),
                why,
                retry_after,
            },
        }
    }

    fn open(
        &self,
        _account: &str,
        channel: &ConversationRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        let fields = json!({ "channel": channel, "text": text, "intentKey": intent_key });
        match self.effect("open", fields, |o| {
            matches!(
                o,
                EffectOutcome::Opened { .. } | EffectOutcome::Failed { .. }
            )
        }) {
            Observation::Known(EffectOutcome::Opened {
                conversation,
                message,
                at,
            }) => match self.platform_check("open", &conversation) {
                Ok(()) => Observation::Known(EffectOutcome::Opened {
                    conversation,
                    message,
                    at,
                }),
                Err(why) => Observation::unknown(why),
            },
            other => other,
        }
    }

    fn post(
        &self,
        _account: &str,
        conversation: &ConversationRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        let fields = json!({ "conversation": conversation, "text": text, "intentKey": intent_key });
        self.effect("post", fields, |o| {
            matches!(
                o,
                EffectOutcome::Posted { .. } | EffectOutcome::Failed { .. }
            )
        })
    }

    fn react(
        &self,
        _account: &str,
        conversation: &ConversationRef,
        message: &str,
        reaction: &str,
        on: bool,
    ) -> Observation<EffectOutcome> {
        let fields = json!({
            "conversation": conversation, "message": message, "reaction": reaction, "on": on,
        });
        self.effect("react", fields, |o| {
            matches!(
                o,
                EffectOutcome::Reacted { .. } | EffectOutcome::Failed { .. }
            )
        })
    }

    fn link(&self, _account: &str, thread: &ConversationRef) -> Option<String> {
        if !self.offers("link").unwrap_or(false) {
            return None;
        }
        match self.ask::<Option<String>>("link", json!({ "thread": thread })) {
            Observation::Known(link) => link,
            Observation::Unknown { .. } => None,
        }
    }

    fn fetch(
        &self,
        _account: &str,
        conversation: &ConversationRef,
        file: &str,
        dir: &Path,
    ) -> Observation<Fetched> {
        match self.offers("fetch") {
            Err(why) => return Observation::unknown(why),
            Ok(false) => {
                return Observation::Known(Fetched::Refused {
                    why: format!("connector {} offers no fetch", self.program.name),
                });
            }
            Ok(true) => {}
        }
        let fields = json!({ "conversation": conversation, "file": file, "dir": dir });
        match self.ask::<Fetched>("fetch", fields) {
            Observation::Known(Fetched::Saved { path, .. }) if path.parent() != Some(dir) => {
                Observation::Known(Fetched::Refused {
                    why: format!(
                        "connector {} saved {} outside {}",
                        self.program.name,
                        path.display(),
                        dir.display()
                    ),
                })
            }
            other => other,
        }
    }
}
