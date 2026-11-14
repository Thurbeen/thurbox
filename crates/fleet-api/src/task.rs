//! Task: one agent session doing one brief.

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

/// The finalizer every Task carries from create: TaskController releases the
/// session before the record goes.
pub const RELEASE_FINALIZER: &str = "fleet.dev/release";

/// The runtime host a Task runs on when it names none: this machine.
pub const LOCAL_HOST: &str = "local";

/// The host a Task runs on: its own `spec.host`, else its TaskConfig's
/// (`config_host`), else [`LOCAL_HOST`]. The manager resolves a launch by
/// this rule, and `fleet apply` checks a Task's host by it.
pub fn effective_host(spec: &serde_json::Value, config_host: Option<&str>) -> String {
    match spec.get("host") {
        Some(own) => own.as_str().unwrap_or_default().to_string(),
        None => config_host.unwrap_or(LOCAL_HOST).to_string(),
    }
}

/// What a Task asks for. Fixed once created, like a Pod's spec: only
/// `dispatch` and `forge` may change, `amendments` only grows through
/// `fleet amend`, and a different brief is a different Task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskSpec {
    /// The agent harness, e.g. `claude-code`. Passed to the runtime as is.
    #[serde(default)]
    pub harness: String,
    /// The runtime host the session runs on.
    #[serde(default = "local_host")]
    pub host: String,
    /// The model the harness runs, when it is not the harness's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_ref: Option<crate::config::ConfigRef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Pause the session once its agent was idle this long; overrides the
    /// TaskConfig's. Absent means the TaskConfig decides, and without one,
    /// never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_ms: Option<Millis>,
    /// The brief, sent once and never changed. Empty for an interactive
    /// Task: its session spawns and waits for its user.
    #[serde(default)]
    pub brief: String,
    /// Additions to the brief, oldest first; appended only by `fleet amend`,
    /// never changed or reordered. The worker's assignment is the brief plus
    /// every amendment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub amendments: Vec<Amendment>,
    pub workspace: Workspace,
    /// Set by whoever starts the work; nothing spawns while it is false.
    #[serde(default)]
    pub dispatch: bool,
    /// The structured outputs a worker's result must carry, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, OutputDecl>,
    /// Overrides of the `[forge]` relay and draft defaults for the changes
    /// this Task's worker declares; mutable like `dispatch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<ForgeSpec>,
}

fn local_host() -> String {
    LOCAL_HOST.to_string()
}

/// One addition to a Task's brief, stamped by `fleet amend`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Amendment {
    /// What it adds, delivered as typed after Fleet's header.
    pub text: String,
    /// Why, as the amending caller gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Outputs it adds to the ones a result must carry; never a name
    /// already declared.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, OutputDecl>,
    /// Who amended: `cli/<user>`, or `plugin/<name>` for a plugin plan.
    pub from: String,
    /// Unix milliseconds.
    pub at: Millis,
    /// The spec generation the amendment created.
    pub generation: u64,
}

impl TaskSpec {
    /// A Task with no brief: nothing is sent, the session waits for its
    /// user, and an amendment has nothing to add to.
    pub fn is_interactive(&self) -> bool {
        self.brief.is_empty()
    }

    /// The outputs a result for `generation` must match: `spec.outputs`
    /// and those of every amendment at or below it.
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

    /// The first message a worker gets: the brief, then every amendment so
    /// far, with the generation a result names when there are any.
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

    /// What a briefed worker is told about the amendments after generation
    /// `after`, up to `upto`: one message, so an amendment queued behind a
    /// newer one still reaches the worker.
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

/// One amendment as the worker reads it.
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

/// Where the session works: a list of absolute URLs. Fleet checks that each
/// is one, and nothing else: the scheme, the reference and any query are the
/// runtime's to read. The runtime makes one directory per URL before the
/// first start and removes what it made after the release. The first is
/// where the agent starts; the others are the extra directories it spans.
/// An empty list: the agent starts in the runtime's default directory.
///
/// ```text
/// ["git+https://host/org/app.git#main",   a worktree, made from main
///  "git+file:///home/me/lib",             a worktree of a local repository
///  "file:///home/me/notes"]               the directory, as it is
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Workspace(pub Vec<String>);

/// What a workspace that does not decode is told: the shapes before the URL
/// list (`{dir}`, `{repositories}`) are refused by this, never converted.
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
    /// A workspace of these URLs.
    pub fn of<S: Into<String>>(urls: impl IntoIterator<Item = S>) -> Workspace {
        Workspace(urls.into_iter().map(Into::into).collect())
    }

    /// The directory `dir`, used as it is: `["file://<dir>"]`.
    pub fn at(dir: impl AsRef<str>) -> Workspace {
        Workspace(vec![file_url(dir.as_ref())])
    }

    pub fn urls(&self) -> &[String] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The directory the first URL names when it is `file://`: where the
    /// agent starts, made by nobody. `None` for any other workspace.
    pub fn dir(&self) -> Option<String> {
        self.0.first().and_then(|u| file_path(u))
    }

    /// [`Workspace::dir`] as text, normalised so two spellings of one
    /// directory compare equal: `\\` as `/`, repeated and trailing
    /// separators dropped, `.` segments dropped. Nothing is resolved against
    /// a filesystem, so a symlink or `..` is another spelling.
    pub fn normalised(&self) -> Option<String> {
        self.dir().as_deref().map(normalise_dir)
    }
}

/// `file://<dir>`: the URL of a directory used as it is. The bytes a URL
/// reads apart, `%`, `?`, `#` and whitespace, are percent-encoded, as a
/// runtime decodes them.
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

/// The path a `file://` URL names, percent-decoded as a runtime reads it,
/// with a Windows drive's leading `/` dropped; `None` for any other URL.
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

/// The directory a stored Task's spec names as its first URL, when that is
/// `file://`: what a thread's directory is checked against.
pub fn spec_dir(spec: &serde_json::Value) -> Option<String> {
    spec["workspace"][0].as_str().and_then(file_path)
}

/// The scheme of an absolute URL, `<scheme>://…`, in lower case; `None`
/// when `url` is not one. A path, `C:/…` included, is not a URL.
pub fn url_scheme(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !rest.is_empty())
    .then(|| scheme.to_ascii_lowercase())
}

/// See [`Workspace::normalised`].
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

/// Task status. TaskController owns every field but `result` and
/// `declared`, which the worker writes through `fleet result` and `fleet
/// declare`, `forge`, which ForgeController owns, `gateway`, which
/// GatewayController owns, and `inbox` and `answered`, which nobody owns:
/// they are reached only through the store's typed append and
/// record-and-remove mutations (see [`crate::inbox`] and [`crate::talk`]).
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
    /// What the runtime last proved about the session (see
    /// [`crate::session`]). Absent until a pass proved something; a pass
    /// that proved nothing keeps the last state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_state: Option<SessionState>,
    /// What the agent is doing (see [`crate::session::AgentState`]): its
    /// hooks' report, else what a screen rule reads off its screen, else
    /// `unknown`. Absent with no session; a pass whose signal file could
    /// not be read keeps the last state, unless a screen rule matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<AgentState>,
    /// Where `agentState` came from: the hooks, a screen rule, or neither
    /// (see [`crate::screen::merge`]). Absent with no session, and for a
    /// harness with no hooks and no screen rules, which has no agent state
    /// to report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<crate::screen::AgentActivity>,
    /// The harness's own id of the session's conversation, as its hooks
    /// last reported it: what a revive or a resume names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// The workspace the runtime made for the session (see
    /// [`crate::workspace`]). Absent for an empty workspace list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<crate::workspace::WorkspaceRef>,
    /// Effect intents, written before the effect runs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub intents: BTreeMap<String, Intent>,
    /// Effect receipts, written after; an intent without one is a question.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub receipts: BTreeMap<String, Receipt>,
    /// Per send key, the digest of the text it carries, written with its
    /// intent: the send counts as delivered only once the agent's prompt
    /// hook reports it. Absent for a harness with no prompt hook.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prompts: BTreeMap<String, String>,
    #[serde(default)]
    pub spawn_attempts: u32,
    #[serde(default)]
    pub release_attempts: u32,
    /// Workspace calls planned before the first start, bounded like spawn
    /// attempts; the current one's key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub materialize_attempts: u32,
    /// Clean effects planned for the workspace after the release; the
    /// current one's key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub clean_attempts: u32,
    /// Sends of the brief, bounded like spawn attempts.
    #[serde(default)]
    pub brief_sends: u32,
    /// Consecutive unreadable delivery probes; three raise NeedsAttention.
    #[serde(default)]
    pub brief_probe_failures: u32,
    /// Revive effects planned over the Task's life; the next one's key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub revivals: u32,
    /// Revive effects since the session was last seen running; three raise
    /// NeedsAttention.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub revive_attempts: u32,
    /// Pause effects planned over the Task's life; the next one's key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pauses: u32,
    /// Resume effects planned over the Task's life; the next one's key. A
    /// record from before catch-up notes had their own counter counts its
    /// notes here as well, so a resume key never repeats a note's key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resumes: u32,
    /// Resume effects since the session was last seen running; three raise
    /// NeedsAttention.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resume_attempts: u32,
    /// When the agent was first seen Idle in the current unbroken run of
    /// Idle observations. Active or Unknown clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_since: Option<Millis>,
    /// Catch-up notes queued over the Task's life; the next one's key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub catch_ups: u32,
    /// The key of the catch-up note owed to a revived or resumed session,
    /// until its delivery is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catch_up: Option<String>,
    /// After a failed or unknown attempt: the manager does not reconcile
    /// this Task again before then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Millis>,
    /// When a Concluded Task's session was last looked up; at rest, the
    /// next lookup is due a fixed interval later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_checked_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<BriefRecord>,
    /// The worker's claim, written by field manager `worker` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    /// Messages queued for the session, oldest first. No owner: appended by
    /// `fleet send`, removed by TaskController, each through a typed
    /// mutation; never replaced as a list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inbox: Vec<InboxEntry>,
    /// The receipts of delivered inbox entries, TaskController's.
    #[serde(default, skip_serializing_if = "Deliveries::is_empty")]
    pub deliveries: Deliveries,
    /// TaskController's sends and probes per inbox entry being delivered.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub delivery_attempts: BTreeMap<String, DeliveryAttempt>,
    /// Facts the worker declared through `fleet declare`, each a JSON
    /// object with a string `kind`; field manager `worker`'s.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared: Vec<serde_json::Value>,
    /// The Forge controller's: one entry per followed change and its
    /// conditions. Absent for a Task that declared nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<ForgeStatus>,
    /// The Gateway controller's: the tie to a chat thread, the relay
    /// cursor, the children read, the post records and its conditions.
    /// Absent for a Task that never talks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayStatus>,
    /// Per Question id, the inbox entry that answered it. No owner: written
    /// only by an inbox append that carries `answers`, first writer wins.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub answered: BTreeMap<String, Answer>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl TaskStatus {
    /// Record what an appended entry answers: each id it carries, unless
    /// an earlier entry answered it already.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BriefRecord {
    /// The effect key the brief was sent under; the session stores it too.
    pub key: String,
    /// The spec generation the brief was sent for; `fleet result` must name it.
    pub generation: u64,
    /// True when the receipt was recovered by asking the session after a crash.
    #[serde(default)]
    pub recovered: bool,
}

/// A Task's session is defined in [`crate::session`]; these names stay here
/// for the code that reaches them through the Task.
pub use crate::session::{AgentState, SessionRef, SessionState, session_key, uid_of_session_key};

/// The status fields the worker owns.
pub const WORKER_FIELDS: &[&str] = &["result", "declared"];

/// The status fields ForgeController owns.
pub const FORGE_FIELDS: &[&str] = &["forge"];

/// The status fields GatewayController owns.
pub const GATEWAY_FIELDS: &[&str] = &["gateway"];

/// The status fields nobody owns: reached only through typed mutations.
/// Every field in none of these lists is TaskController's.
pub const UNOWNED_FIELDS: &[&str] = &["inbox", "answered"];

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A directory becomes a URL a runtime reads back to that directory:
    /// whitespace, `%`, `?` and `#` are escaped, and a path is decoded
    /// before two spellings of it are compared.
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

    /// A URL spells a Windows drive after a `/`; the directory it names is
    /// the drive path, as a runtime reads it.
    #[test]
    fn a_file_url_names_a_windows_drive_path_as_the_runtime_reads_it() {
        let dir = file_path("file:///C:/work/conv-123").expect("a path");
        assert_eq!(dir, "C:/work/conv-123");
        assert_eq!(normalise_dir(&dir), normalise_dir(r"C:\work\conv-123"));
    }
}
