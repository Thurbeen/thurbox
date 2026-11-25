use crate::observation::Observation;
use crate::plan::{EffectOutcome, SpawnSpec};
use crate::task::{SessionRef, Workspace};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceTrust {
    Trusted { how: String },
    Untrusted { gap: TrustGap, why: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustGap {
    Unsupported,
    Mismatch,
}

impl TrustGap {
    pub fn reason(self) -> &'static str {
        match self {
            TrustGap::Unsupported => "Unsupported",
            TrustGap::Mismatch => "Mismatch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Active,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Attach {
    Stream { argv: Vec<String> },
    Tty { argv: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Place {
    pub name: String,
    #[serde(default)]
    pub config: serde_json::Map<String, serde_json::Value>,
}

impl Place {
    pub const LOCAL: &'static str = "local";

    pub fn local() -> Place {
        Place {
            name: Place::LOCAL.into(),
            config: serde_json::Map::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlaceReport {
    pub name: String,
    pub platform: Platform,
    #[serde(default)]
    pub dynamic: bool,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Posix,
    Windows,
}

pub trait Runtime {
    fn prepare_execution(
        &self,
        execution: &crate::config::Execution,
    ) -> Observation<crate::config::Execution> {
        Observation::Known(execution.clone())
    }

    fn env_source(
        &self,
        _source: &crate::config::EnvSource,
    ) -> Observation<std::collections::BTreeMap<String, String>> {
        Observation::unknown("environment sources unsupported by this runtime")
    }

    fn operation_bound(&self) -> crate::Millis {
        0
    }
    fn host(&self, host: &str) -> Observation<bool>;
    fn create_dir(&self, host: &str, dir: &str) -> Observation<Result<bool, String>>;
    fn remove_dir(&self, host: &str, dir: &str) -> Observation<Result<(), String>>;
    fn workspace(&self, _host: &str, _key: &str, urls: &[String]) -> Observation<EffectOutcome> {
        Observation::Known(match urls.first() {
            Some(url) => EffectOutcome::Unsupported { url: url.clone() },
            None => EffectOutcome::Failed {
                why: "an empty workspace list makes nothing".into(),
            },
        })
    }
    fn find_workspace(
        &self,
        host: &str,
        key: &str,
        urls: &[String],
    ) -> Observation<Option<crate::workspace::WorkspaceRef>> {
        self.workspace(host, key, urls).map(|made| match made {
            EffectOutcome::Materialized { workspace, .. } => Some(*workspace),
            _ => None,
        })
    }
    fn release_workspace(
        &self,
        _workspace: &crate::workspace::WorkspaceRef,
    ) -> Observation<EffectOutcome> {
        Observation::Known(EffectOutcome::Deleted)
    }
    fn spawn(&self, spec: &SpawnSpec) -> Observation<EffectOutcome>;
    fn find(&self, host: &str, key: &str) -> Observation<Option<SessionRef>>;
    fn ready(&self, session: &SessionRef) -> Observation<bool>;
    fn workspace_trust(
        &self,
        session: &SessionRef,
        workspace: &Workspace,
        ready: Option<&crate::workspace::WorkspaceRef>,
    ) -> Observation<WorkspaceTrust>;
    fn brief(
        &self,
        session: &SessionRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome>;
    fn deliver(
        &self,
        session: &SessionRef,
        text: &str,
        intent_key: &str,
        inbox: &[String],
    ) -> Observation<EffectOutcome> {
        let _ = inbox;
        self.brief(session, text, intent_key)
    }
    fn brief_delivered(&self, session: &SessionRef, intent_key: &str) -> Observation<bool>;
    fn at_rest(&self, session: &SessionRef) -> Observation<bool>;
    fn release(&self, session: &SessionRef) -> Observation<EffectOutcome>;
    fn lost(&self, session: &SessionRef) -> Observation<bool>;
    fn revive(
        &self,
        session: &SessionRef,
        conversation: Option<&str>,
        launch: &SpawnSpec,
    ) -> Observation<EffectOutcome>;
    fn screen(&self, _session: &SessionRef) -> Observation<Option<crate::screen::Screen>> {
        Observation::unknown("this runtime reads no screen")
    }
    fn attach(&self, _session: &SessionRef) -> Observation<Option<Attach>> {
        Observation::Known(None)
    }
    fn shell(&self, _session: &SessionRef) -> Observation<Option<Attach>> {
        Observation::Known(None)
    }
    fn signals(&self, _session: &SessionRef) -> Option<Observation<crate::signal::Reported>> {
        None
    }
    fn outbox(
        &self,
        _session: &SessionRef,
        _from: u64,
    ) -> Option<Observation<Option<crate::event::OutboxRead>>> {
        None
    }
    fn paused(&self, _session: &SessionRef) -> Observation<bool> {
        Observation::unknown("this runtime cannot tell a stopped session")
    }
    fn pause(&self, _session: &SessionRef) -> Observation<EffectOutcome> {
        Observation::Known(EffectOutcome::Failed {
            why: "this runtime cannot pause a session".into(),
        })
    }
    fn resume(
        &self,
        _session: &SessionRef,
        _conversation: Option<&str>,
        _launch: &SpawnSpec,
    ) -> Observation<EffectOutcome> {
        Observation::Known(EffectOutcome::Failed {
            why: "this runtime cannot resume a session".into(),
        })
    }
    /// Stop the agent of `session`, running or stopped, and start it again
    /// in its own workspace under the same identity: on `conversation`, as
    /// for `resume`, or on its launch argv when `None`. `Restarted` once it
    /// started; `RestartUnsupported` on a runtime that cannot pause, with
    /// nothing stopped. Each call cuts the agent's turn, so Fleet makes it
    /// once per key.
    fn restart(
        &self,
        _session: &SessionRef,
        _conversation: Option<&str>,
        _launch: &SpawnSpec,
    ) -> Observation<EffectOutcome> {
        Observation::Known(EffectOutcome::RestartUnsupported {
            why: "this runtime cannot pause a session".into(),
        })
    }
}
