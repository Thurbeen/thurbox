use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use fleet_api::gateway::{
    AccountConfig, Capabilities, ConnectorConfig, ConversationRef, Delivery, Envelope, Fetched,
    Gateway, History, HistoryObservation, HistoryQuery, Identity, Marker, Uncertainty,
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

    pub fn call(&self, verb: &str, request: Value) -> Answer {
        self.call_speaking(PROTOCOL, verb, request)
    }

    fn call_speaking(&self, protocol: u64, verb: &str, mut request: Value) -> Answer {
        if let Some(fields) = request.as_object_mut() {
            fields.insert("protocol".into(), json!(protocol));
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

    fn request(&self, name: Option<&str>, config: &AccountConfig, fields: Value) -> Value {
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
        let manifest =
            self.program
                .manifest(self.request(Some(&self.account), &self.config, json!({})))?;
        *cached = Some(manifest.clone());
        Ok(manifest)
    }

    fn offers(&self, verb: &str) -> Result<bool, String> {
        self.manifest().map(|m| m.verbs.iter().any(|v| v == verb))
    }

    fn call(&self, verb: &str, fields: Value) -> Answer {
        self.call_as(Some(&self.account), &self.config, verb, fields)
    }

    fn call_as(
        &self,
        name: Option<&str>,
        config: &AccountConfig,
        verb: &str,
        fields: Value,
    ) -> Answer {
        if let Err(why) = self.manifest() {
            return Answer::unknown(why);
        }
        self.program.call(verb, self.request(name, config, fields))
    }

    fn ask<T: DeserializeOwned>(&self, verb: &str, fields: Value) -> Observation<T> {
        self.read(self.call(verb, fields), verb)
    }

    fn read<T: DeserializeOwned>(&self, answer: Answer, verb: &str) -> Observation<T> {
        match answer {
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
        let answer = self.call_as(None, account, "event", fields);
        match self.read::<Option<Delivery>>(answer, "event") {
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
        match self.call("history", fields) {
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
        let stem = format!("fleet-file-{}", crate::saved_name(file, ""));
        let folder = match crate::fresh_private_dir(dir, &stem) {
            Ok(folder) => folder,
            Err(e) => return Observation::unknown(format!("{}: {e}", dir.display())),
        };
        let fields = json!({ "conversation": conversation, "file": file, "dir": folder });
        match self.ask::<Fetched>("fetch", fields) {
            Observation::Known(Fetched::Saved { path, .. })
                if path.parent() != Some(folder.as_path()) || path.file_name().is_none() =>
            {
                let _ = std::fs::remove_dir(&folder);
                Observation::Known(Fetched::Refused {
                    why: format!(
                        "connector {} saved {} outside {}",
                        self.program.name,
                        path.display(),
                        folder.display()
                    ),
                })
            }
            saved @ Observation::Known(Fetched::Saved { .. }) => saved,
            other => {
                let _ = std::fs::remove_dir(&folder);
                other
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    Skipped(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    pub name: &'static str,
    pub verdict: Verdict,
}

const LIVE_CASES: [&str; 3] = [
    "a post answers posted",
    "a post with a repeated intent key lands once",
    "history since the post holds it under its key",
];

pub fn check(
    program: &ConnectorProgram,
    account: &AccountConfig,
    channel: Option<&str>,
) -> Vec<Case> {
    let request = |fields: Value| {
        let mut request = fields;
        request["account"] = account_request(Some("check"), account);
        request
    };
    let case = |name: &'static str, result: Result<(), String>| Case {
        name,
        verdict: result.map_or_else(Verdict::Failed, |()| Verdict::Passed),
    };
    let manifest = match program.manifest(request(json!({}))) {
        Ok(m) => m,
        Err(why) => return vec![case(MANIFEST_CASE, Err(why))],
    };
    let mut cases = vec![case(MANIFEST_CASE, known_verbs(&manifest))];
    cases.push(case(
        "a request on another protocol answers Unknown",
        answers_unknown(program.call_speaking(PROTOCOL + 1, "whoami", request(json!({})))),
    ));
    cases.push(case(
        "an unknown verb answers Unknown",
        answers_unknown(program.call("fleet-check-no-such-verb", request(json!({})))),
    ));
    let gateway = ConnectorGateway::new(program.clone(), "check", account);
    cases.push(case(
        "an address it parses keeps its parts",
        parses_back(&gateway, &account.platform),
    ));
    let envelope = json!({"verified": "connection", "receivedAt": 0, "payload": {}});
    cases.push(case(
        "an event it cannot read answers null or Unknown",
        match gateway.event(
            account,
            &serde_json::from_value(envelope).expect("an envelope"),
        ) {
            Observation::Known(Some(d)) => Err(format!("an empty payload read as {d:?}")),
            Observation::Known(None) => Ok(()),
            Observation::Unknown { why } if why.contains("outside the protocol") => Err(why),
            Observation::Unknown { .. } => Ok(()),
        },
    ));
    cases.push(case(
        "whoami names an id",
        match gateway.whoami("check") {
            Observation::Known(i) if !i.id.trim().is_empty() => Ok(()),
            Observation::Known(_) => Err("the id is empty".into()),
            Observation::Unknown { why } => Err(why),
        },
    ));
    let capabilities = gateway.capabilities("check");
    cases.push(case(
        "capabilities promise only verbs the manifest lists",
        match &capabilities {
            Observation::Known(c) => promised(c, &manifest),
            Observation::Unknown { why } => Err(why.clone()),
        },
    ));
    match (channel, capabilities) {
        (Some(channel), Observation::Known(c)) => cases.extend(live(&gateway, channel, &c)),
        (Some(_), Observation::Unknown { .. }) => cases.extend(LIVE_CASES.map(|name| Case {
            name,
            verdict: Verdict::Skipped("capabilities are unknown".into()),
        })),
        (None, _) => cases.extend(LIVE_CASES.map(|name| Case {
            name,
            verdict: Verdict::Skipped("no --channel".into()),
        })),
    }
    cases
}

const MANIFEST_CASE: &str = "the manifest names protocol 1, its platform and every required verb";

fn known_verbs(manifest: &Manifest) -> Result<(), String> {
    match manifest
        .verbs
        .iter()
        .find(|v| !REQUIRED_VERBS.contains(&v.as_str()) && !OPTIONAL_VERBS.contains(&v.as_str()))
    {
        Some(v) => Err(format!(
            "it lists {v:?}, which protocol {PROTOCOL} does not have"
        )),
        None => Ok(()),
    }
}

fn answers_unknown(answer: Answer) -> Result<(), String> {
    match answer {
        Answer::Unknown { .. } => Ok(()),
        Answer::Known(v) => Err(format!("it answered Known: {v}")),
    }
}

fn parses_back(gateway: &ConnectorGateway, platform: &str) -> Result<(), String> {
    if let Observation::Known(c) = gateway.parse("check", "") {
        return Err(format!("an empty address parsed as {c:?}"));
    }
    let sample = format!("{platform}:C0CHECK:T0CHECK");
    let Observation::Known(parsed) = gateway.parse("check", &sample) else {
        return Ok(());
    };
    match gateway.parse("check", &parsed.address()) {
        Observation::Known(again) if again == parsed => Ok(()),
        Observation::Known(again) => Err(format!(
            "{sample} parsed as {parsed:?}, whose address parsed as {again:?}"
        )),
        Observation::Unknown { why } => Err(format!(
            "{sample} parsed, but its address {} did not: {why}",
            parsed.address()
        )),
    }
}

fn promised(capabilities: &Capabilities, manifest: &Manifest) -> Result<(), String> {
    for (promise, verb) in [
        (capabilities.history, "history"),
        (capabilities.reactions, "react"),
    ] {
        if promise && !manifest.verbs.iter().any(|v| v == verb) {
            let field = if verb == "react" { "reactions" } else { verb };
            return Err(format!("{field} is true and the manifest lists no {verb}"));
        }
    }
    Ok(())
}

fn live(gateway: &ConnectorGateway, channel: &str, capabilities: &Capabilities) -> Vec<Case> {
    let failed = |why: String| {
        LIVE_CASES
            .map(|name| Case {
                name,
                verdict: Verdict::Failed(why.clone()),
            })
            .to_vec()
    };
    let channel = match gateway.parse("check", channel) {
        Observation::Known(c) => c,
        Observation::Unknown { why } => return failed(format!("--channel: {why}")),
    };
    let key = format!(
        "fleet-check/{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis())
    );
    let text = "fleet gateway check: a conformance post";
    let (message, at) = match gateway.post("check", &channel, text, &key) {
        Observation::Known(EffectOutcome::Posted { message, at }) => (message, at),
        Observation::Known(other) => return failed(format!("the post answered {other:?}")),
        Observation::Unknown { why } => return failed(why),
    };
    let mut cases = vec![Case {
        name: LIVE_CASES[0],
        verdict: Verdict::Passed,
    }];
    cases.push(Case {
        name: LIVE_CASES[1],
        verdict: if capabilities.marker == Marker::None {
            Verdict::Skipped("marker is none, so a repeat would post twice".into())
        } else {
            match gateway.post("check", &channel, text, &key) {
                Observation::Known(EffectOutcome::Posted { message: again, .. })
                    if again == message =>
                {
                    Verdict::Passed
                }
                Observation::Known(other) => Verdict::Failed(format!(
                    "the repeat answered {other:?}, not the first post {message}"
                )),
                Observation::Unknown { why } => Verdict::Failed(why),
            }
        },
    });
    let thread = ConversationRef {
        thread: Some(message.clone()),
        ..channel
    };
    cases.push(Case {
        name: LIVE_CASES[2],
        verdict: if !capabilities.history {
            Verdict::Skipped("capabilities report no history".into())
        } else {
            match gateway.history("check", &thread, &HistoryQuery::Since { at }) {
                HistoryObservation::Known(h) => match h.messages.iter().find(|m| m.id == message) {
                    None => Verdict::Failed(format!("message {message} is not in it")),
                    Some(m)
                        if capabilities.marker != Marker::None
                            && m.key.as_deref() != Some(key.as_str()) =>
                    {
                        Verdict::Failed(format!("message {message} carries key {:?}", m.key))
                    }
                    Some(_) => Verdict::Passed,
                },
                HistoryObservation::Unsupported => Verdict::Failed(
                    "capabilities report history and the read is unsupported".into(),
                ),
                HistoryObservation::Unknown { why, .. } => Verdict::Failed(why),
            }
        },
    });
    cases
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use fleet_api::gateway::Inbound;
    use std::path::PathBuf;

    const MANIFEST: &str = r#"{"known":{"protocol":1,"platform":"chat","verbs":["manifest","parse","event","whoami","capabilities","post","history","fetch"]}}"#;

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/connector-unit")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn connector(dir: &Path, answers: &[(&str, &str)]) -> ConnectorGateway {
        let mut script = String::from("#!/bin/sh\ncat >/dev/null\ncase $1 in\n");
        for (verb, answer) in answers {
            script.push_str(&format!("{verb}) printf '%s\\n' '{answer}' ;;\n"));
        }
        script.push_str("*) exit 3 ;;\nesac\n");
        let program = dir.join("connector.sh");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(
            &program,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let config = AccountConfig {
            platform: "chat".into(),
            team: None,
            bot: "B1".into(),
            inbound: Inbound::Connection,
            message_content: None,
            cli: None,
            curl_config: None,
            dir: None,
            credentials: None,
            options: Default::default(),
        };
        let program = ConnectorProgram::new(
            "chat",
            &ConnectorConfig {
                program: program.to_string_lossy().into_owned(),
                args: Vec::new(),
                call_timeout_ms: Some(5_000),
            },
            Duration::from_secs(5),
        );
        ConnectorGateway::new(program, "chat-main", &config)
    }

    fn thread() -> ConversationRef {
        ConversationRef {
            platform: "chat".into(),
            chat: "C1".into(),
            thread: Some("T1".into()),
        }
    }

    #[test]
    fn a_verb_the_manifest_does_not_list_is_answered_without_a_call() {
        let dir = scratch("unlisted");
        let gateway = connector(&dir, &[("manifest", MANIFEST)]);
        assert!(matches!(
            gateway.react("chat-main", &thread(), "M1", "eyes", true),
            Observation::Known(EffectOutcome::Failed { why }) if why == "connector chat offers no react"
        ));
        assert!(matches!(
            gateway.open("chat-main", &thread().channel(), "hi", "k1"),
            Observation::Known(EffectOutcome::Failed { .. })
        ));
        assert_eq!(gateway.link("chat-main", &thread()), None);
    }

    #[test]
    fn a_manifest_without_a_required_verb_or_for_another_platform_makes_every_call_unknown() {
        let dir = scratch("manifest");
        let lacking = connector(
            &dir,
            &[(
                "manifest",
                r#"{"known":{"protocol":1,"platform":"chat","verbs":["manifest","parse"]}}"#,
            )],
        );
        let Observation::Unknown { why } = lacking.whoami("chat-main") else {
            panic!("a connector that lacks a verb is not used");
        };
        assert_eq!(why, "connector chat lacks the required verb event");
        let dir = scratch("protocol");
        let newer = connector(
            &dir,
            &[(
                "manifest",
                r#"{"known":{"protocol":2,"platform":"chat","verbs":[]}}"#,
            )],
        );
        let Observation::Unknown { why } = newer.capabilities("chat-main") else {
            panic!("a connector on another protocol is not used");
        };
        assert_eq!(why, "connector chat speaks protocol 2, not 1");
    }

    #[test]
    fn a_post_answers_only_a_posted_or_failed_outcome() {
        let dir = scratch("post");
        let gateway = connector(
            &dir,
            &[
                ("manifest", MANIFEST),
                ("post", r#"{"known":{"type":"reacted"}}"#),
            ],
        );
        let Observation::Unknown { why } = gateway.post("chat-main", &thread(), "hi", "k1") else {
            panic!("a reaction is not a post");
        };
        assert!(why.contains("an outcome it cannot have"), "{why}");
    }

    #[test]
    fn a_history_unknown_keeps_its_kind_and_wait() {
        let dir = scratch("history");
        let gateway = connector(
            &dir,
            &[
                ("manifest", MANIFEST),
                (
                    "history",
                    r#"{"unknown":{"why":"slow down","kind":"throttled","retryAfter":3000}}"#,
                ),
            ],
        );
        let query = HistoryQuery::Since { at: 0 };
        assert_eq!(
            gateway.history("chat-main", &thread(), &query),
            HistoryObservation::Unknown {
                kind: Uncertainty::Throttled,
                why: "connector chat history: slow down".into(),
                retry_after: Some(3000),
            }
        );
    }

    fn scripted(dir: &Path, name: &str, body: &str) -> ConnectorGateway {
        let mut gateway = connector(dir, &[("manifest", MANIFEST)]);
        let program = dir.join(format!("{name}.sh"));
        std::fs::write(
            &program,
            format!("#!/bin/sh\nreq=$(cat)\ncase $1 in\nmanifest) printf '%s\\n' '{MANIFEST}' ;;\n*)\n{body}\n;;\nesac\n"),
        )
        .unwrap();
        std::fs::set_permissions(
            &program,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        gateway.program.program = program.to_string_lossy().into_owned();
        gateway
    }

    #[test]
    fn a_fetch_writes_into_a_fresh_private_dir_and_a_file_outside_it_is_refused() {
        let dir = scratch("fetch");
        let into = dir.join("files");
        std::fs::create_dir_all(&into).unwrap();
        let given = r#"d=$(printf '%s' "$req" | sed 's/.*"dir":"\([^"]*\)".*/\1/')"#;
        let answer = r#"printf '{"known":{"outcome":"saved","path":"%s","bytes":1}}\n' "$p""#;
        for (name, place, kept) in [
            ("inside", "p=\"$d/a.png\"; printf x >\"$p\"", true),
            ("shared", &*format!("p=\"{}/a.png\"", into.display()), false),
            ("above", "p=\"$d/..\"", false),
        ] {
            let gateway = scripted(&dir, name, &format!("{given}\n{place}\n{answer}"));
            let got = gateway.fetch("chat-main", &thread(), "F1", &into);
            let Observation::Known(Fetched::Saved { path, .. }) = &got else {
                assert!(!kept, "{name} answered {got:?}");
                continue;
            };
            assert!(kept, "{name} answered {got:?}");
            let folder = path.parent().unwrap();
            assert_eq!(folder.parent(), Some(into.as_path()));
            assert!(
                folder
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("fleet-file-F1-"),
                "{}",
                folder.display()
            );
            let mode = std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(folder).unwrap().permissions(),
            );
            assert_eq!(mode & 0o777, 0o700);
        }
    }

    #[test]
    fn an_event_request_names_no_account_since_the_port_does_not_say_which() {
        let dir = scratch("event");
        let seen = dir.join("event.json");
        let gateway = scripted(
            &dir,
            "event",
            &format!(
                "printf '%s' \"$req\" >'{}'\nprintf '%s\\n' '{{\"known\":null}}'",
                seen.display()
            ),
        );
        let envelope: Envelope = serde_json::from_value(
            json!({"verified": "connection", "receivedAt": 0, "payload": {}}),
        )
        .unwrap();
        let config = gateway.config.clone();
        assert!(matches!(
            gateway.event(&config, &envelope),
            Observation::Known(None)
        ));
        let request: Value = serde_json::from_slice(&std::fs::read(&seen).unwrap()).unwrap();
        assert_eq!(request["account"]["platform"], "chat", "{request:#}");
        assert!(request["account"].get("name").is_none(), "{request:#}");
    }
}
