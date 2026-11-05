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
    /// The brief, sent once and never changed.
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

/// Where the session works. Fleet names it; the runtime makes it, and cleans
/// it up when the session is deleted, as the runtime is configured to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    untagged,
    expecting = "a workspace: {\"dir\": PATH} or {\"repositories\": [{\"name\", \"url\", \"ref\"?}]}"
)]
pub enum Workspace {
    Dir(DirWorkspace),
    Repositories(RepositoriesWorkspace),
}

/// A directory that exists before the Task; the runtime attaches it as is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirWorkspace {
    pub dir: String,
}

/// Repositories the runtime materializes under the session key. The first is
/// the primary, where the agent starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositoriesWorkspace {
    pub repositories: Vec<Repository>,
}

impl Workspace {
    /// A `{dir}` workspace.
    pub fn at(dir: impl Into<String>) -> Workspace {
        Workspace::Dir(DirWorkspace { dir: dir.into() })
    }

    /// The directory of a `{dir}` workspace.
    pub fn dir(&self) -> Option<&str> {
        match self {
            Workspace::Dir(w) => Some(&w.dir),
            Workspace::Repositories(_) => None,
        }
    }

    /// The directory of a `{dir}` workspace as text, normalised so two
    /// spellings of one directory compare equal: `\\` as `/`, repeated and
    /// trailing separators dropped, `.` segments dropped. Nothing is
    /// resolved against a filesystem, so a symlink or `..` is another
    /// spelling. A `repositories` workspace has no shared directory.
    pub fn normalised(&self) -> Option<String> {
        self.dir().map(normalise_dir)
    }
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

/// A repository a Task works on: its identity, never how it is provisioned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Repository {
    /// Unique in the Task; labels the repository for people and the runtime.
    pub name: String,
    /// The repository's forge URL.
    pub url: String,
    /// Where to start from. Opaque to Fleet; the runtime interprets it.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
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
    /// Effect intents, written before the effect runs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub intents: BTreeMap<String, Intent>,
    /// Effect receipts, written after; an intent without one is a question.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub receipts: BTreeMap<String, Receipt>,
    #[serde(default)]
    pub spawn_attempts: u32,
    #[serde(default)]
    pub release_attempts: u32,
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
    /// until its delivery is known. A record from before the rename holds
    /// it as `resume`, under a `resume/<n>` key, and is read the same way.
    #[serde(default, alias = "resume", skip_serializing_if = "Option::is_none")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRef {
    pub id: String,
    pub host: String,
    pub key: String,
}

/// The status fields the worker owns.
pub const WORKER_FIELDS: &[&str] = &["result", "declared"];

/// The status fields ForgeController owns.
pub const FORGE_FIELDS: &[&str] = &["forge"];

/// The status fields GatewayController owns.
pub const GATEWAY_FIELDS: &[&str] = &["gateway"];

/// The status fields nobody owns: reached only through typed mutations.
/// Every field in none of these lists is TaskController's.
pub const UNOWNED_FIELDS: &[&str] = &["inbox", "answered"];

/// The session key for a Task: deterministic, so a second spawn finds the
/// first session instead of twinning it. The uid keeps a Task recreated under
/// the same name from adopting a session it never owned.
pub fn session_key(name: &str, uid: &str) -> String {
    format!("{name}.{uid}")
}

/// The uid part of a session key: `<name>.<uid>`, or a bare uid, which is
/// what an adapter whose keys must be short hands back from `list`.
pub fn uid_of_session_key(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_runs_on_this_machine_unless_it_names_a_host() {
        let spec: TaskSpec = serde_json::from_value(serde_json::json!({
            "harness": "claude-code", "brief": "b", "workspace": {"dir": "w"}
        }))
        .unwrap();
        assert_eq!(spec.host, LOCAL_HOST);
        assert!(!spec.dispatch);
    }

    #[test]
    fn a_workspace_is_a_directory_or_repositories_and_nothing_else() {
        let parse = |w: serde_json::Value| {
            serde_json::from_value::<TaskSpec>(serde_json::json!({
                "harness": "claude-code", "brief": "b", "workspace": w
            }))
            .map(|s| s.workspace)
        };
        assert_eq!(
            parse(serde_json::json!({"dir": "w"})).unwrap(),
            Workspace::at("w")
        );
        let repos = parse(serde_json::json!({"repositories": [
            {"name": "app", "url": "https://example.invalid/app", "ref": "main"}
        ]}))
        .unwrap();
        let Workspace::Repositories(w) = repos else {
            panic!("{repos:?}")
        };
        assert_eq!(w.repositories[0].reference.as_deref(), Some("main"));
        for refused in [
            serde_json::json!({"dir": "w", "repositories": []}),
            serde_json::json!({"dir": "w", "branch": "x"}),
            serde_json::json!({"repositories": [{"name": "a", "url": "u", "baseBranch": "x"}]}),
            serde_json::json!({}),
        ] {
            let err = parse(refused.clone()).unwrap_err().to_string();
            assert!(err.contains("a workspace:"), "{refused}: {err}");
        }
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
        let repos: Workspace = serde_json::from_value(serde_json::json!({"repositories": [
            {"name": "app", "url": "https://example.invalid/app"}
        ]}))
        .unwrap();
        assert_eq!(repos.normalised(), None);
    }

    #[test]
    fn a_session_key_carries_the_uid() {
        let key = session_key("t", "u1");
        assert_eq!(uid_of_session_key(&key), "u1");
        assert_eq!(uid_of_session_key("u1"), "u1");
    }
}
