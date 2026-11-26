use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::condition::Condition;
use crate::follow::{ForgeSpec, ForgeStatus};
use crate::inbox::{Deliveries, DeliveryAttempt, InboxEntry};
use crate::meta::Millis;
use crate::plan::{Intent, Receipt};
use crate::result::{OutputDecl, TaskResult};
use crate::talk::{Answer, GatewayStatus};

pub const KIND: &str = "Task";

pub const RELEASE_FINALIZER: &str = "fleet.dev/release";

pub const LOCAL_HOST: &str = "local";

pub const VIEW_WAIT_MS: Millis = 60_000;

pub fn effective_host(spec: &serde_json::Value, config_host: Option<&str>) -> String {
    match spec.get("host") {
        Some(own) => own.as_str().unwrap_or_default().to_string(),
        None => config_host.unwrap_or(LOCAL_HOST).to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskSpec {
    #[serde(default)]
    pub harness: String,
    #[serde(default = "local_host")]
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_ref: Option<crate::config::ConfigRef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_telemetry: Option<bool>,
    #[serde(default)]
    pub brief: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub amendments: Vec<Amendment>,
    pub workspace: Workspace,
    #[serde(default)]
    pub dispatch: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, OutputDecl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<ForgeSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<crate::subscription::Subscription>,
}

fn local_host() -> String {
    LOCAL_HOST.to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Amendment {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, OutputDecl>,
    pub from: String,
    pub at: Millis,
    pub generation: u64,
}

impl TaskSpec {
    pub fn is_interactive(&self) -> bool {
        self.brief.is_empty()
    }

    pub fn outputs_at(&self, generation: u64) -> BTreeMap<String, OutputDecl> {
        let mut outputs = self.outputs.clone();
        for a in self
            .amendments
            .iter()
            .filter(|a| a.generation <= generation)
        {
            outputs.extend(a.outputs.clone());
        }
        outputs
    }

    pub fn assignment(&self, task: &str, generation: u64) -> String {
        let amendments: Vec<&Amendment> = self
            .amendments
            .iter()
            .filter(|a| a.generation <= generation)
            .collect();
        if amendments.is_empty() {
            return self.brief.clone();
        }
        let mut text = self.brief.clone();
        text.push_str(&format!(
            "\n\nFleet: your brief has {} amendment(s), below; they add to it. Record your \
             result with `fleet result {task} --generation {generation}`.",
            amendments.len()
        ));
        for a in amendments {
            text.push_str("\n\n");
            text.push_str(&amendment_block(a));
        }
        text
    }

    pub fn amendment_message(&self, task: &str, after: u64, upto: u64) -> String {
        let amendments: Vec<&Amendment> = self
            .amendments
            .iter()
            .filter(|a| a.generation > after && a.generation <= upto)
            .collect();
        let (what, them) = match amendments.as_slice() {
            [one] => (
                format!(
                    "an amendment to your brief (generation {}) from {}",
                    one.generation, one.from
                ),
                "it",
            ),
            _ => (
                format!(
                    "{} amendments to your brief (up to generation {upto})",
                    amendments.len()
                ),
                "them",
            ),
        };
        let mut text = format!(
            "Fleet: {what}.\n\
             It adds to your brief: your brief and earlier amendments still apply.\n\
             It may give more context, add an ask, or ask you a question.\n\
             Address {them}, then record your result with\n\
             `fleet result {task} --generation {upto}`; a result already recorded\n\
             must be recorded again so it covers this amendment."
        );
        for a in amendments {
            text.push_str("\n\n");
            text.push_str(&amendment_block(a));
        }
        text
    }
}

fn amendment_block(a: &Amendment) -> String {
    let mut text = format!(
        "--- Amendment, generation {}, from {}\n",
        a.generation, a.from
    );
    if let Some(reason) = &a.reason {
        text.push_str(&format!("Reason: {reason}\n"));
    }
    if !a.outputs.is_empty() {
        let outputs: Vec<String> = a
            .outputs
            .iter()
            .map(|(name, d)| {
                let ty = serde_json::to_value(d.type_)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                if d.required {
                    format!("{name}: {ty}, required")
                } else {
                    format!("{name}: {ty}")
                }
            })
            .collect();
        text.push_str(&format!(
            "New outputs your result must carry: {}\n",
            outputs.join("; ")
        ));
    }
    text.push_str("---\n");
    text.push_str(&a.text);
    text
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Workspace(pub Vec<String>);

pub const WORKSPACE_EXPECTED: &str = "a workspace: a list of absolute URLs, such as [\"git+https://host/org/repo.git#main\", \"file:///home/me/notes\"]";

impl<'de> Deserialize<'de> for Workspace {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Workspace, D::Error> {
        struct Urls;
        impl<'de> serde::de::Visitor<'de> for Urls {
            type Value = Workspace;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(WORKSPACE_EXPECTED)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Workspace, A::Error> {
                let mut urls = Vec::new();
                while let Some(url) = seq.next_element::<String>()? {
                    urls.push(url);
                }
                Ok(Workspace(urls))
            }
        }
        d.deserialize_seq(Urls)
    }
}

impl Workspace {
    pub fn of<S: Into<String>>(urls: impl IntoIterator<Item = S>) -> Workspace {
        Workspace(urls.into_iter().map(Into::into).collect())
    }

    pub fn at(dir: impl AsRef<str>) -> Workspace {
        Workspace(vec![file_url(dir.as_ref())])
    }

    pub fn urls(&self) -> &[String] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn dir(&self) -> Option<String> {
        self.0.first().and_then(|u| file_path(u))
    }

    pub fn normalised(&self) -> Option<String> {
        self.dir().as_deref().map(normalise_dir)
    }
}

pub fn file_url(dir: &str) -> String {
    let mut url = String::from("file://");
    for c in dir.chars() {
        if matches!(c, '%' | '?' | '#') || c.is_whitespace() {
            let mut buf = [0; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                url.push_str(&format!("%{b:02X}"));
            }
        } else {
            url.push(c);
        }
    }
    url
}

pub fn file_path(url: &str) -> Option<String> {
    let raw = url.strip_prefix("file://").filter(|p| !p.is_empty())?;
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let path = String::from_utf8_lossy(&out).into_owned();
    // `/C:/dir` is how a URL spells the Windows path `C:/dir`.
    let b = path.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        return Some(path[1..].to_string());
    }
    Some(path)
}

pub fn spec_dir(spec: &serde_json::Value) -> Option<String> {
    spec["workspace"][0].as_str().and_then(file_path)
}

pub fn url_scheme(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !rest.is_empty())
    .then(|| scheme.to_ascii_lowercase())
}

pub fn normalise_dir(dir: &str) -> String {
    let text = dir.trim().replace('\\', "/");
    let absolute = text.starts_with('/');
    let parts: Vec<&str> = text
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Pending,
    Spawning,
    Spawned,
    Briefed,
    Stalled,
    Concluded,
    Releasing,
}

impl Phase {
    pub const ALL: &[Phase] = &[
        Phase::Pending,
        Phase::Spawning,
        Phase::Spawned,
        Phase::Briefed,
        Phase::Stalled,
        Phase::Concluded,
        Phase::Releasing,
    ];
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskStatus {
    #[serde(default)]
    pub phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<crate::config::ExecutionIdentity>,
    #[serde(default)]
    pub observed_generation: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_state: Option<SessionState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<AgentState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<crate::screen::AgentActivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<crate::workspace::WorkspaceRef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub intents: BTreeMap<String, Intent>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub receipts: BTreeMap<String, Receipt>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prompts: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub loose_prompts: BTreeMap<String, String>,
    #[serde(default)]
    pub spawn_attempts: u32,
    #[serde(default)]
    pub release_attempts: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub materialize_attempts: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub clean_attempts: u32,
    #[serde(default)]
    pub brief_sends: u32,
    #[serde(default)]
    pub brief_probe_failures: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub revivals: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub revive_attempts: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pauses: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resumes: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resume_attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_since: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_request_answered: Option<Millis>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub restarts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_request_answered: Option<RestartRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_conversation: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restart_retired: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub catch_ups: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catch_up: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_checked_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<BriefRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inbox: Vec<InboxEntry>,
    #[serde(default, skip_serializing_if = "Deliveries::is_empty")]
    pub deliveries: Deliveries,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub delivery_attempts: BTreeMap<String, DeliveryAttempt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<ForgeStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayStatus>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub answered: BTreeMap<String, Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_requested_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_requested_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_requested: Option<RestartRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox: Option<crate::event::OutboxCursor>,
    #[serde(
        default,
        skip_serializing_if = "crate::subscription::SubscriptionsStatus::is_empty"
    )]
    pub subscriptions: crate::subscription::SubscriptionsStatus,
    #[serde(
        default,
        skip_serializing_if = "crate::task_event::FleetEvents::is_empty"
    )]
    pub fleet_events: crate::task_event::FleetEvents,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl TaskStatus {
    pub fn record_answers(&mut self, entry: &InboxEntry) {
        for id in &entry.answers {
            self.answered.entry(id.clone()).or_insert_with(|| Answer {
                key: entry.key.clone(),
                from: entry.from.clone(),
                at: entry.queued_at,
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestartRequest {
    pub at: Millis,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BriefRecord {
    pub key: String,
    pub generation: u64,
    #[serde(default)]
    pub recovered: bool,
}

pub use crate::session::{AgentState, SessionRef, SessionState, session_key, uid_of_session_key};

pub const WORKER_FIELDS: &[&str] = &["result", "declared"];

pub const FORGE_FIELDS: &[&str] = &["forge"];

pub const GATEWAY_FIELDS: &[&str] = &["gateway"];

pub const UNOWNED_FIELDS: &[&str] = &[
    "inbox",
    "answered",
    "wakeRequestedAt",
    "pauseRequestedAt",
    "restartRequested",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_phase_is_in_all() {
        let listed = |p: Phase| match p {
            Phase::Pending
            | Phase::Spawning
            | Phase::Spawned
            | Phase::Briefed
            | Phase::Stalled
            | Phase::Concluded
            | Phase::Releasing => Phase::ALL.contains(&p),
        };
        assert!(Phase::ALL.iter().copied().all(listed));
        assert_eq!(Phase::ALL.len(), 7);
    }

    #[test]
    fn a_task_runs_on_this_machine_unless_it_names_a_host() {
        let spec: TaskSpec = serde_json::from_value(serde_json::json!({
            "harness": "claude-code", "brief": "b", "workspace": ["file:///w"]
        }))
        .unwrap();
        assert_eq!(spec.host, LOCAL_HOST);
        assert!(!spec.dispatch);
    }

    #[test]
    fn a_workspace_is_a_list_of_urls_and_nothing_else() {
        let parse = |w: serde_json::Value| {
            serde_json::from_value::<TaskSpec>(serde_json::json!({
                "harness": "claude-code", "brief": "b", "workspace": w
            }))
            .map(|s| s.workspace)
        };
        assert_eq!(
            parse(serde_json::json!(["file:///w"])).unwrap(),
            Workspace::at("/w")
        );
        assert_eq!(parse(serde_json::json!([])).unwrap(), Workspace::default());
        for refused in [
            serde_json::json!({"dir": "w"}),
            serde_json::json!({"repositories": [{"name": "a", "url": "https://x/a"}]}),
            serde_json::json!("file:///w"),
            serde_json::json!({}),
        ] {
            let err = parse(refused.clone()).unwrap_err().to_string();
            assert!(err.contains("a list of absolute URLs"), "{refused}: {err}");
        }
    }

    #[test]
    fn a_url_has_a_scheme_and_a_path_has_none() {
        assert_eq!(
            url_scheme("git+https://h/o/r.git#main").as_deref(),
            Some("git+https")
        );
        assert_eq!(url_scheme("FILE:///w").as_deref(), Some("file"));
        for not in ["/w", "C:/w", "C:\\w", "file://", "://x", "1x://y", "w"] {
            assert_eq!(url_scheme(not), None, "{not}");
        }
        assert_eq!(Workspace::at("/w").dir().as_deref(), Some("/w"));
        assert_eq!(Workspace::of(["git+https://h/o/r"]).dir(), None);
    }

    #[test]
    fn a_workspace_normalises_its_spelling() {
        for (a, b) in [
            ("/work/conversations", "/work/conversations/"),
            ("/work/conversations", "/work//conversations"),
            ("/work/conversations", "/work/./conversations"),
            ("/work/conversations", " /work/conversations "),
            ("C:/work/x", "C:\\work\\x\\"),
            ("work/x", "work/x/"),
        ] {
            assert_eq!(normalise_dir(a), normalise_dir(b), "{a} vs {b}");
        }
        assert_ne!(normalise_dir("/work/a"), normalise_dir("/work/a/b"));
        assert_ne!(normalise_dir("/work/a"), normalise_dir("work/a"));
        assert_eq!(normalise_dir("/"), "/");
        assert_eq!(Workspace::at("/w/").normalised().as_deref(), Some("/w"));
        assert_eq!(Workspace::of(["git+https://h/o/r"]).normalised(), None);
    }

    #[test]
    fn a_directory_round_trips_through_its_file_url() {
        for dir in ["/work/team notes", "/work/a#b?c", "/work/100%"] {
            let url = file_url(dir);
            assert!(!url.contains(char::is_whitespace), "{url}");
            assert!(!url["file://".len()..].contains(['#', '?']), "{url}");
            assert_eq!(file_path(&url).as_deref(), Some(dir), "{url}");
        }
        assert_eq!(
            file_path("file:///work/conv%2D123").as_deref(),
            Some("/work/conv-123")
        );
    }

    #[test]
    fn a_file_url_names_a_windows_drive_path_as_the_runtime_reads_it() {
        let dir = file_path("file:///C:/work/conv-123").expect("a path");
        assert_eq!(dir, "C:/work/conv-123");
        assert_eq!(normalise_dir(&dir), normalise_dir(r"C:\work\conv-123"));
    }
}
