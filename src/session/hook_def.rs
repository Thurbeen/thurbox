//! Session lifecycle hooks — the user's own commands, run by thurbox at the
//! moments it creates, deletes, restarts or restores a session.
//!
//! This is the *reverse* of the built-in `hooks` extension, which installs
//! status hooks **into** the agent CLIs so they can report to thurbox. Here
//! thurbox reports to the user's scripts. The two share a word and nothing
//! else; this module and its consumers say "lifecycle hook" for the same
//! reason.
//!
//! Pure data: the events, an entry of `hooks.toml`, and the facts handed to a
//! hook — as an environment and as one JSON document — so the convention a
//! hook script relies on is unit-testable without a process.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::SessionId;

/// How long a hook may run before it is killed, when its entry sets no
/// `timeout_secs`.
pub const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 30;

/// The moments a hook can be attached to: a `pre_*` before the operation has
/// any side effect (and able to refuse it), a `post_*` after it has fully
/// succeeded.
///
/// Closed and spelled in serde exactly as `hooks.toml` spells it, so an
/// unknown event is a parse error rather than a hook that silently never fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    #[serde(rename = "session.pre_create")]
    PreCreate,
    #[serde(rename = "session.post_create")]
    PostCreate,
    #[serde(rename = "session.pre_delete")]
    PreDelete,
    #[serde(rename = "session.post_delete")]
    PostDelete,
    #[serde(rename = "session.pre_restart")]
    PreRestart,
    #[serde(rename = "session.post_restart")]
    PostRestart,
    #[serde(rename = "session.pre_restore")]
    PreRestore,
    #[serde(rename = "session.post_restore")]
    PostRestore,
}

impl HookEvent {
    /// Every event, pre before post, in the order the operations are documented.
    pub const ALL: [HookEvent; 8] = [
        HookEvent::PreCreate,
        HookEvent::PostCreate,
        HookEvent::PreDelete,
        HookEvent::PostDelete,
        HookEvent::PreRestart,
        HookEvent::PostRestart,
        HookEvent::PreRestore,
        HookEvent::PostRestore,
    ];

    /// The dotted name — the `event` value in `hooks.toml` and the
    /// `THURBOX_HOOK_EVENT` a hook reads.
    pub fn name(self) -> &'static str {
        match self {
            HookEvent::PreCreate => "session.pre_create",
            HookEvent::PostCreate => "session.post_create",
            HookEvent::PreDelete => "session.pre_delete",
            HookEvent::PostDelete => "session.post_delete",
            HookEvent::PreRestart => "session.pre_restart",
            HookEvent::PostRestart => "session.post_restart",
            HookEvent::PreRestore => "session.pre_restore",
            HookEvent::PostRestore => "session.post_restore",
        }
    }

    /// Whether a failing hook for this event aborts the operation.
    pub fn is_pre(self) -> bool {
        matches!(
            self,
            HookEvent::PreCreate
                | HookEvent::PreDelete
                | HookEvent::PreRestart
                | HookEvent::PreRestore
        )
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// One `[[hooks]]` entry of `hooks.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleHook {
    pub event: HookEvent,
    /// Run through the platform shell (`sh -c`, `cmd /C` on Windows).
    pub command: String,
    /// Seconds before the hook is killed; [`DEFAULT_HOOK_TIMEOUT_SECS`] when
    /// unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl LifecycleHook {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.unwrap_or(DEFAULT_HOOK_TIMEOUT_SECS))
    }
}

/// The whole of `hooks.toml`, in declaration order.
///
/// Unknown fields are tolerated but reported, like every other config file:
/// the loader names each one in a warning and `config validate` fails on it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HooksFile {
    /// Config-format version, for future migrations. Currently `1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_version: Option<u32>,
    #[serde(default)]
    pub hooks: Vec<LifecycleHook>,
}

impl HooksFile {
    /// The hooks declared for `event`, in file order.
    pub fn for_event(&self, event: HookEvent) -> Vec<LifecycleHook> {
        self.hooks
            .iter()
            .filter(|h| h.event == event)
            .cloned()
            .collect()
    }
}

/// One worktree as a hook sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookWorktree {
    pub repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
}

/// What a hook is told about the session — everything known at the moment
/// the event fires. A field that is not known at that moment is `None`, and
/// [`HookContext::env`] leaves its variable **unset** (never empty), so
/// `${THURBOX_HOST:+…}` idioms work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HookContext {
    /// The thurbox session id. At `pre_create` it is the id the session will
    /// have if creation succeeds — minted early precisely so a pre and its post
    /// can be correlated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub name: String,
    pub agent: String,
    /// The agent's own conversation id — `THURBOX_SESSION_ID`, as the agent
    /// receives it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    /// The primary repository — the one path that exists at every event: at
    /// `pre_create` the worktree is not made, at `post_delete` it is gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    /// The directory the agent runs in (the worktree, or the symlink workspace
    /// of a multi-repo session). Unknown before the worktree exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// The remote host's name; `None` for a local session. When set, `repo`
    /// and `cwd` are paths **on that host**.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<i64>,
    /// The agent conversation a fork resumes from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_session_id: Option<String>,
    /// Delete events: whether the runtime resources are torn down too.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
    /// Restore events: whether the session had been force-deleted, so only
    /// committed work returns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_deleted: Option<bool>,
    /// The multiplexer pane (`%N`) once one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_id: Option<String>,
    pub worktrees: Vec<HookWorktree>,
    pub additional_dirs: Vec<PathBuf>,
}

/// The stdin document: the context with the event folded in.
#[derive(Serialize)]
struct Payload<'a> {
    event: &'a str,
    #[serde(flatten)]
    context: &'a HookContext,
}

impl HookContext {
    /// The `THURBOX_*` variables a hook for `event` receives — only the ones
    /// whose value is known.
    pub fn env(&self, event: HookEvent) -> Vec<(String, String)> {
        let mut vars: Vec<(String, String)> =
            vec![("THURBOX_HOOK_EVENT".into(), event.name().into())];
        let mut set = |key: &str, value: Option<String>| {
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                vars.push((key.to_string(), value));
            }
        };
        set("THURBOX_SESSION", self.session_id.map(|id| id.to_string()));
        set("THURBOX_SESSION_ID", self.agent_session_id.clone());
        set("THURBOX_SESSION_NAME", Some(self.name.clone()));
        set("THURBOX_AGENT", Some(self.agent.clone()));
        set("THURBOX_REPO", self.repo.as_deref().map(display));
        set("THURBOX_CWD", self.cwd.as_deref().map(display));
        set("THURBOX_BRANCH", self.branch.clone());
        set("THURBOX_BASE_BRANCH", self.base_branch.clone());
        set("THURBOX_HOST", self.host.clone());
        set(
            "THURBOX_PARENT_SESSION",
            self.parent_session_id.map(|id| id.to_string()),
        );
        set("THURBOX_TASK", self.task_id.map(|id| id.to_string()));
        vars
    }

    /// The JSON document written to the hook's stdin.
    pub fn json(&self, event: HookEvent) -> String {
        serde_json::to_string(&Payload {
            event: event.name(),
            context: self,
        })
        .expect("a HookContext serializes: every field is a string, a number, a bool or a list")
    }

    /// Where the hook runs: the primary repository when it is a directory on
    /// this machine. A remote session's paths are the host's, so a same-named
    /// local directory is a coincidence, not a match. `None` means the hook
    /// inherits thurbox's own working directory.
    pub fn workdir(&self) -> Option<&Path> {
        if self.host.is_some() {
            return None;
        }
        self.repo.as_deref().filter(|p| p.is_dir())
    }
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_round_trips_by_its_dotted_name() {
        for event in HookEvent::ALL {
            let toml = format!("event = \"{}\"\ncommand = \"true\"\n", event.name());
            let hook: LifecycleHook =
                toml::from_str(&toml).unwrap_or_else(|e| panic!("{event}: {e}"));
            assert_eq!(hook.event, event);
            assert_eq!(toml::to_string(&hook).unwrap(), toml);
            assert!(event.name().starts_with("session."));
            assert_eq!(event.is_pre(), event.name().contains(".pre_"));
        }
    }

    #[test]
    fn an_unknown_event_is_a_parse_error() {
        let err =
            toml::from_str::<LifecycleHook>("event = \"session.on_create\"\ncommand = \"true\"\n")
                .unwrap_err()
                .to_string();
        assert!(
            err.contains("session.on_create") || err.contains("unknown variant"),
            "{err}"
        );
    }

    #[test]
    fn a_missing_command_is_a_parse_error() {
        assert!(toml::from_str::<LifecycleHook>("event = \"session.pre_create\"\n").is_err());
    }

    #[test]
    fn a_file_lists_hooks_in_order_and_filters_by_event() {
        let file: HooksFile = toml::from_str(
            r#"
            [[hooks]]
            event = "session.post_create"
            command = "first"

            [[hooks]]
            event = "session.pre_create"
            command = "veto"
            timeout_secs = 5

            [[hooks]]
            event = "session.post_create"
            command = "second"
            "#,
        )
        .unwrap();
        let post: Vec<String> = file
            .for_event(HookEvent::PostCreate)
            .into_iter()
            .map(|h| h.command)
            .collect();
        assert_eq!(post, ["first", "second"]);
        let pre = file.for_event(HookEvent::PreCreate);
        assert_eq!(pre[0].timeout(), Duration::from_secs(5));
        assert_eq!(
            file.for_event(HookEvent::PostCreate)[0].timeout(),
            Duration::from_secs(DEFAULT_HOOK_TIMEOUT_SECS)
        );
        assert!(file.for_event(HookEvent::PreDelete).is_empty());
    }

    #[test]
    fn env_sets_exactly_the_documented_names() {
        let sid = SessionId::default();
        let parent = SessionId::default();
        let ctx = HookContext {
            session_id: Some(sid),
            name: "demo".into(),
            agent: "claude".into(),
            agent_session_id: Some("conv-1".into()),
            repo: Some(PathBuf::from("/srv/repo")),
            cwd: Some(PathBuf::from("/srv/repo/worktrees/demo")),
            branch: Some("feat/x".into()),
            base_branch: Some("main".into()),
            host: Some("devbox".into()),
            parent_session_id: Some(parent),
            task_id: Some(7),
            ..HookContext::default()
        };
        let env = ctx.env(HookEvent::PostCreate);
        let mut names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "THURBOX_AGENT",
                "THURBOX_BASE_BRANCH",
                "THURBOX_BRANCH",
                "THURBOX_CWD",
                "THURBOX_HOOK_EVENT",
                "THURBOX_HOST",
                "THURBOX_PARENT_SESSION",
                "THURBOX_REPO",
                "THURBOX_SESSION",
                "THURBOX_SESSION_ID",
                "THURBOX_SESSION_NAME",
                "THURBOX_TASK",
            ]
        );
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("THURBOX_HOOK_EVENT"), Some("session.post_create"));
        assert_eq!(get("THURBOX_SESSION"), Some(sid.to_string().as_str()));
        assert_eq!(
            get("THURBOX_PARENT_SESSION"),
            Some(parent.to_string().as_str())
        );
        assert_eq!(get("THURBOX_TASK"), Some("7"));
        assert_eq!(get("THURBOX_CWD"), Some("/srv/repo/worktrees/demo"));
    }

    #[test]
    fn an_unknown_fact_is_unset_not_empty() {
        let ctx = HookContext {
            name: "demo".into(),
            agent: "claude".into(),
            ..HookContext::default()
        };
        let env = ctx.env(HookEvent::PreCreate);
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "THURBOX_HOOK_EVENT",
                "THURBOX_SESSION_NAME",
                "THURBOX_AGENT"
            ]
        );
        assert!(env.iter().all(|(_, v)| !v.is_empty()));
    }

    #[test]
    fn json_carries_the_event_and_the_worktrees() {
        let ctx = HookContext {
            name: "demo".into(),
            agent: "codex".into(),
            worktrees: vec![HookWorktree {
                repo_path: PathBuf::from("/srv/repo"),
                worktree_path: PathBuf::from("/srv/repo/worktrees/demo"),
                branch: "feat/x".into(),
            }],
            force: Some(true),
            ..HookContext::default()
        };
        let value: serde_json::Value =
            serde_json::from_str(&ctx.json(HookEvent::PreDelete)).unwrap();
        assert_eq!(value["event"], "session.pre_delete");
        assert_eq!(value["name"], "demo");
        assert_eq!(value["force"], true);
        assert_eq!(value["worktrees"][0]["branch"], "feat/x");
        assert_eq!(value["worktrees"][0]["repo_path"], "/srv/repo");
        // Unknown facts are absent, not null.
        assert!(value.get("host").is_none());
        assert!(value.get("session_id").is_none());
    }

    #[test]
    fn workdir_is_the_local_repo_and_nothing_for_a_remote_one() {
        let dir = tempfile::tempdir().unwrap();
        let local = HookContext {
            repo: Some(dir.path().to_path_buf()),
            ..HookContext::default()
        };
        assert_eq!(local.workdir(), Some(dir.path()));

        // The same path, but on a host: not ours to cd into.
        let remote = HookContext {
            host: Some("devbox".into()),
            ..local.clone()
        };
        assert_eq!(remote.workdir(), None);

        let gone = HookContext {
            repo: Some(dir.path().join("missing")),
            ..HookContext::default()
        };
        assert_eq!(gone.workdir(), None);
    }
}
