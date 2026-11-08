//! A fake host that keeps its state in one JSON file, so the CLI under test
//! and the test itself see the same sessions across processes.
//!
//! Faults are queued per call and consumed in order. They model the failure
//! cases of the runtime contract: a lost answer (`unknown`, with or without
//! the effect having happened), a refusal (`fail`), a delay, and a crash of
//! the caller right after the effect (`abort`), which is how a test puts the
//! manager between an effect and its receipt.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use fleet_api::Observation;
use fleet_api::harness::profile;
use fleet_api::plan::{EffectOutcome, SpawnSpec};
use fleet_api::runtime::{Runtime, TrustGap, WorkspaceTrust};
use fleet_api::task::{RepositoriesWorkspace, SessionRef};
use fleet_api::workspace::{WorkspaceRef, Workspaces};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fault {
    /// `materialize`, `findWorkspace`, `cleanWorkspace` (the workspace
    /// port), `host`, `createDir`, `removeDir`, `spawn`, `find`, `list`, `events`, `ready`,
    /// `workspaceTrust`, `brief`, `briefDelivered`, `atRest`, `release`,
    /// `lost`, `revive`, `paused`, `pause`, `resume`.
    pub call: String,
    /// `unknown`, `fail` or `abort`.
    pub outcome: String,
    /// For `unknown`: whether the effect happened before the answer was lost.
    #[serde(default)]
    pub perform: bool,
    /// Sleep this long before answering.
    #[serde(default)]
    pub delay_ms: u64,
    /// How many calls this fault applies to.
    #[serde(default = "one")]
    pub times: u32,
}

fn one() -> u32 {
    1
}

fn yes() -> bool {
    true
}

impl Fault {
    pub fn new(call: &str, outcome: &str) -> Fault {
        Fault {
            call: call.into(),
            outcome: outcome.into(),
            perform: false,
            delay_ms: 0,
            times: 1,
        }
    }

    pub fn performed(mut self) -> Fault {
        self.perform = true;
        self
    }

    pub fn times(mut self, n: u32) -> Fault {
        self.times = n;
        self
    }

    pub fn delay(mut self, ms: u64) -> Fault {
        self.delay_ms = ms;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FakeSession {
    pub id: String,
    pub host: String,
    pub key: String,
    pub harness: String,
    pub workspace: fleet_api::task::Workspace,
    /// The workspace Fleet made and the spawn ran the session in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialized: Option<WorkspaceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub ready: bool,
    /// Whether its agent trusts its workspace; a session that does not
    /// waits for a person, as an agent with no supported trust interface.
    #[serde(default = "yes")]
    pub trusted: bool,
    pub at_rest: bool,
    /// Intent keys of every brief delivered, in order.
    pub briefs: Vec<String>,
    /// The text of each, in the same order, for tests to read back.
    #[serde(default)]
    pub texts: Vec<String>,
    /// Set when the session vanished or was deleted.
    pub gone: bool,
    /// Set while its window is gone and the row is kept, as after a host
    /// reboot; a revive clears it.
    #[serde(default)]
    pub lost: bool,
    /// Set once the Task released (deleted) it.
    #[serde(default)]
    pub released: bool,
    /// Set while its agent is stopped and the row is kept; a resume clears
    /// it.
    #[serde(default)]
    pub paused: bool,
    /// The argv the harness profile launched the agent with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    /// Each argv a revive or a resume started it again with, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restarts: Vec<Vec<String>>,
    /// The environment the spawn started the agent with.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// A workspace the fake workspace adapter made: a record, no files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FakeWorkspace {
    pub workspace: WorkspaceRef,
    pub repositories: Vec<fleet_api::task::Repository>,
    /// Set once Fleet cleaned it; a cleaned workspace is not found.
    #[serde(default)]
    pub cleaned: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct FakeState {
    /// Workspaces by key.
    pub workspaces: BTreeMap<String, FakeWorkspace>,
    /// Repository URLs the fake workspace adapter has no source for.
    pub unmapped: Vec<String>,
    /// `up`, `down` or `unknown`; a host not listed is up.
    pub hosts: BTreeMap<String, String>,
    pub sessions: BTreeMap<String, FakeSession>,
    pub calls: BTreeMap<String, u64>,
    pub faults: Vec<Fault>,
    /// Whether a new session is ready for input at once.
    pub spawn_not_ready: bool,
    /// Whether a new session's agent waits for a person to trust its
    /// workspace.
    pub spawn_untrusted: bool,
    /// Whether the agents' prompt hooks stay silent. Otherwise each agent
    /// reports every text it is given through its signal file, as a coding
    /// agent's prompt hook does.
    pub prompt_hook_silent: bool,
    pub next_id: u64,
    /// One line per effect that happened, in order.
    pub log: Vec<String>,
}

/// The fake host's directory, shared by every process that opens it.
#[derive(Debug, Clone)]
pub struct FakeHost {
    dir: PathBuf,
}

impl FakeHost {
    pub fn open(dir: &Path) -> io::Result<FakeHost> {
        fs::create_dir_all(dir)?;
        Ok(FakeHost {
            dir: dir.to_path_buf(),
        })
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    /// Read-modify-write under the host's lock.
    pub fn update<T>(&self, f: impl FnOnce(&mut FakeState) -> T) -> io::Result<T> {
        let _lock = HostLock::acquire(&self.dir.join("lock"))?;
        let mut state = self.read_unlocked()?;
        let out = f(&mut state);
        self.write_unlocked(&state)?;
        Ok(out)
    }

    pub fn state(&self) -> io::Result<FakeState> {
        let _lock = HostLock::acquire(&self.dir.join("lock"))?;
        self.read_unlocked()
    }

    fn read_unlocked(&self) -> io::Result<FakeState> {
        match fs::read(self.state_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(FakeState::default()),
            Err(e) => Err(e),
        }
    }

    fn write_unlocked(&self, state: &FakeState) -> io::Result<()> {
        let tmp = self.dir.join(format!("state.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, self.state_path())
    }

    pub fn add_fault(&self, fault: Fault) -> io::Result<()> {
        self.update(|s| s.faults.push(fault))
    }

    pub fn set_host(&self, host: &str, state: &str) -> io::Result<()> {
        self.update(|s| {
            s.hosts.insert(host.into(), state.into());
        })
    }

    /// Silence, or restore, every agent's prompt hook.
    pub fn set_prompt_hook_silent(&self, silent: bool) -> io::Result<()> {
        self.update(|s| s.prompt_hook_silent = silent)
    }

    pub fn set_spawn_not_ready(&self, not_ready: bool) -> io::Result<()> {
        self.update(|s| s.spawn_not_ready = not_ready)
    }

    /// The worker finished its turn (or started another one).
    pub fn set_at_rest(&self, key: &str, at_rest: bool) -> io::Result<()> {
        self.update(|s| {
            if let Some(x) = s.sessions.get_mut(key) {
                x.at_rest = at_rest;
            }
        })
    }

    pub fn set_ready(&self, key: &str, ready: bool) -> io::Result<()> {
        self.update(|s| {
            if let Some(x) = s.sessions.get_mut(key) {
                x.ready = ready;
            }
        })
    }

    /// The session disappears from the host, as a crashed agent's would.
    pub fn vanish(&self, key: &str) -> io::Result<()> {
        self.update(|s| {
            if let Some(x) = s.sessions.get_mut(key) {
                x.gone = true;
            }
        })
    }

    /// The host reboots: every session keeps its row and loses its window.
    pub fn reboot(&self) -> io::Result<()> {
        self.update(|s| {
            for x in s.sessions.values_mut().filter(|x| !x.gone) {
                x.lost = true;
                x.ready = false;
            }
        })
    }

    pub fn calls(&self, call: &str) -> io::Result<u64> {
        Ok(self.state()?.calls.get(call).copied().unwrap_or(0))
    }

    pub fn session(&self, key: &str) -> io::Result<Option<FakeSession>> {
        Ok(self.state()?.sessions.get(key).cloned())
    }
}

/// The host's lock is held for one read-modify-write; a fault's delay and an
/// abort run outside it, so a crashing caller never leaves it held.
struct HostLock(File);

impl HostLock {
    fn acquire(path: &Path) -> io::Result<HostLock> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        // Blocking, like the store lock: a holder is bounded and a dead one
        // frees the lock, so a polling deadline only adds load failures.
        f.lock()?;
        Ok(HostLock(f))
    }
}

impl Drop for HostLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The runtime contract served by a [`FakeHost`].
#[derive(Debug, Clone)]
pub struct FakeRuntime {
    host: FakeHost,
}

/// What a queued fault tells a call to do.
enum Plan {
    Normal,
    /// Answer Unknown; perform the effect first when `perform`.
    Unknown {
        perform: bool,
    },
    Fail,
    Abort,
}

impl FakeRuntime {
    pub fn new(host: FakeHost) -> FakeRuntime {
        FakeRuntime { host }
    }

    /// Count the call and take its fault, if one is queued.
    fn begin(&self, call: &str) -> Result<Plan, String> {
        let taken = self
            .host
            .update(|s| {
                *s.calls.entry(call.to_string()).or_default() += 1;
                let i = s.faults.iter().position(|f| f.call == call)?;
                let f = s.faults[i].clone();
                if f.times <= 1 {
                    s.faults.remove(i);
                } else {
                    s.faults[i].times -= 1;
                }
                Some(f)
            })
            .map_err(|e| format!("fake host unreadable: {e}"))?;
        let Some(f) = taken else {
            return Ok(Plan::Normal);
        };
        if f.delay_ms > 0 {
            thread::sleep(Duration::from_millis(f.delay_ms));
        }
        Ok(match f.outcome.as_str() {
            "unknown" => Plan::Unknown { perform: f.perform },
            "fail" => Plan::Fail,
            "abort" => Plan::Abort,
            _ => Plan::Normal,
        })
    }

    /// Run an effect under the plan the fault chose.
    fn effect(
        &self,
        call: &str,
        perform: impl FnOnce(&mut FakeState) -> EffectOutcome,
    ) -> Observation<EffectOutcome> {
        let plan = match self.begin(call) {
            Ok(p) => p,
            Err(why) => return Observation::unknown(why),
        };
        match plan {
            Plan::Fail => Observation::Known(EffectOutcome::Failed {
                why: format!("injected {call} failure"),
            }),
            Plan::Unknown { perform: false } => {
                Observation::unknown(format!("injected: {call} request lost"))
            }
            Plan::Unknown { perform: true } => match self.host.update(perform) {
                Ok(_) => Observation::unknown(format!("injected: {call} answer lost")),
                Err(e) => Observation::unknown(e.to_string()),
            },
            Plan::Abort => {
                let _ = self.host.update(perform);
                // The caller dies between the effect and its receipt.
                std::process::abort();
            }
            Plan::Normal => match self.host.update(perform) {
                Ok(outcome) => Observation::Known(outcome),
                Err(e) => Observation::unknown(e.to_string()),
            },
        }
    }

    /// Run a probe; any fault on a probe makes it Unknown.
    fn probe<T>(
        &self,
        call: &str,
        read: impl FnOnce(&FakeState) -> Observation<T>,
    ) -> Observation<T> {
        match self.begin(call) {
            Ok(Plan::Normal) => {}
            Ok(_) => return Observation::unknown(format!("injected: {call} could not tell")),
            Err(why) => return Observation::unknown(why),
        }
        match self.host.state() {
            Ok(s) => read(&s),
            Err(e) => Observation::unknown(format!("fake host unreadable: {e}")),
        }
    }
}

fn live<'a>(s: &'a FakeState, key: &str) -> Option<&'a FakeSession> {
    s.sessions.get(key).filter(|x| !x.gone)
}

fn session_ref(x: &FakeSession) -> SessionRef {
    SessionRef {
        id: x.id.clone(),
        host: x.host.clone(),
        key: x.key.clone(),
    }
}

impl Runtime for FakeRuntime {
    fn env_source(
        &self,
        source: &fleet_api::config::EnvSource,
    ) -> Observation<std::collections::BTreeMap<String, String>> {
        crate::resolve_env_source(source)
    }

    fn host(&self, host: &str) -> Observation<bool> {
        self.probe("host", |s| match s.hosts.get(host).map(String::as_str) {
            None | Some("up") => Observation::Known(true),
            Some("down") => Observation::Known(false),
            Some(other) => Observation::unknown(format!("host {host} is {other}")),
        })
    }

    /// The fake host is this machine: the directory is made here, and the
    /// effect is logged. `fail` refuses it, `unknown` loses the answer
    /// (after making it when `perform`).
    fn create_dir(&self, host: &str, dir: &str) -> Observation<Result<bool, String>> {
        let made = |s: &mut FakeState| -> EffectOutcome {
            let existed = Path::new(dir).is_dir();
            match fs::create_dir_all(dir) {
                Ok(()) => {
                    if !existed {
                        s.log.push(format!("createDir {host} {dir}"));
                    }
                    EffectOutcome::Created {
                        uid: String::new(),
                        adopted: existed,
                    }
                }
                Err(e) => EffectOutcome::Failed {
                    why: format!("cannot create {dir}: {e}"),
                },
            }
        };
        match self.effect("createDir", made) {
            Observation::Known(EffectOutcome::Failed { why }) => Observation::Known(Err(why)),
            Observation::Known(EffectOutcome::Created { adopted, .. }) => {
                Observation::Known(Ok(!adopted))
            }
            Observation::Known(other) => Observation::unknown(format!("{other:?}")),
            Observation::Unknown { why } => Observation::Unknown { why },
        }
    }

    /// The fake host is this machine: the directory is removed here, and
    /// the effect is logged. `fail` refuses it, `unknown` loses the answer
    /// (after removing it when `perform`).
    fn remove_dir(&self, host: &str, dir: &str) -> Observation<Result<(), String>> {
        let removed = |s: &mut FakeState| -> EffectOutcome {
            match fs::remove_dir_all(dir) {
                Ok(()) => {
                    s.log.push(format!("removeDir {host} {dir}"));
                    EffectOutcome::Deleted
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => EffectOutcome::Deleted,
                Err(e) => EffectOutcome::Failed {
                    why: format!("cannot remove {dir}: {e}"),
                },
            }
        };
        match self.effect("removeDir", removed) {
            Observation::Known(EffectOutcome::Failed { why }) => Observation::Known(Err(why)),
            Observation::Known(_) => Observation::Known(Ok(())),
            Observation::Unknown { why } => Observation::Unknown { why },
        }
    }

    fn spawn(&self, spec: &SpawnSpec) -> Observation<EffectOutcome> {
        let spec = spec.clone();
        // The harness profile's argv, as a runtime that runs a program is
        // given it.
        let argv = match spec.launch_argv() {
            Ok(argv) => argv,
            Err(why) => return Observation::Known(EffectOutcome::Failed { why }),
        };
        self.effect("spawn", move |s| {
            if let Some(x) = live(s, &spec.key) {
                return EffectOutcome::Spawned {
                    session: session_ref(x),
                    adopted: true,
                };
            }
            s.next_id += 1;
            let x = FakeSession {
                id: format!("fake-{}", s.next_id),
                host: spec.host.clone(),
                key: spec.key.clone(),
                harness: spec.harness.clone(),
                workspace: spec.workspace.clone(),
                materialized: spec.ready.clone(),
                model: spec.model.clone(),
                ready: !s.spawn_not_ready,
                trusted: !s.spawn_untrusted,
                at_rest: false,
                briefs: Vec::new(),
                texts: Vec::new(),
                gone: false,
                lost: false,
                released: false,
                paused: false,
                env: spec.env.clone(),
                argv: argv.clone(),
                restarts: Vec::new(),
            };
            s.log.push(format!("spawn {}", spec.key));
            let r = session_ref(&x);
            s.sessions.insert(spec.key.clone(), x);
            EffectOutcome::Spawned {
                session: r,
                adopted: false,
            }
        })
    }

    fn find(&self, host: &str, key: &str) -> Observation<Option<SessionRef>> {
        self.probe("find", |s| match s.hosts.get(host).map(String::as_str) {
            Some("down") | Some("unknown") => {
                Observation::unknown(format!("host {host}: session list unreadable"))
            }
            _ => Observation::Known(live(s, key).filter(|x| x.host == host).map(session_ref)),
        })
    }

    fn ready(&self, session: &SessionRef) -> Observation<bool> {
        self.probe("ready", |s| match live(s, &session.key) {
            Some(x) => Observation::Known(x.ready),
            None => Observation::unknown("no such session to capture"),
        })
    }

    fn workspace_trust(
        &self,
        session: &SessionRef,
        _workspace: &fleet_api::task::Workspace,
        _ready: Option<&WorkspaceRef>,
    ) -> Observation<WorkspaceTrust> {
        self.probe("workspaceTrust", |s| match live(s, &session.key) {
            Some(x) if x.trusted => Observation::Known(WorkspaceTrust::Trusted {
                how: "the fake host trusts every workspace it is told to".into(),
            }),
            Some(_) => Observation::Known(WorkspaceTrust::Untrusted {
                gap: TrustGap::Unsupported,
                why: "the fake session waits for a person to trust its workspace".into(),
            }),
            None => Observation::unknown("no such session to ask"),
        })
    }

    fn brief(
        &self,
        session: &SessionRef,
        text: &str,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        let (key, intent, len) = (session.key.clone(), intent_key.to_string(), text.len());
        let text = text.to_string();
        self.effect("brief", move |s| {
            let Some(x) = s.sessions.get_mut(&key).filter(|x| !x.gone) else {
                return EffectOutcome::Failed {
                    why: "session gone".into(),
                };
            };
            if x.lost || x.paused {
                return EffectOutcome::Failed {
                    why: "the session has no window".into(),
                };
            }
            if x.briefs.contains(&intent) {
                return EffectOutcome::Delivered {
                    already: true,
                    via: None,
                    may_hold: false,
                };
            }
            x.briefs.push(intent.clone());
            if !s.prompt_hook_silent
                && let Some(file) = x.env.get(fleet_api::signal::SIGNAL_FILE_ENV)
            {
                let payload =
                    serde_json::json!({"prompt": text, "session_id": format!("conv-{}", x.id)});
                let signal = fleet_api::signal::Signal::from_hook(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as u64),
                    fleet_api::signal::State::Prompt,
                    Some(&payload),
                );
                let _ = crate::signals::append(Path::new(file), &signal);
            }
            x.texts.push(text);
            x.at_rest = false;
            s.log.push(format!("brief {key} {intent} ({len} bytes)"));
            EffectOutcome::Delivered {
                already: false,
                via: None,
                may_hold: false,
            }
        })
    }

    fn brief_delivered(&self, session: &SessionRef, intent_key: &str) -> Observation<bool> {
        self.probe("briefDelivered", |s| match live(s, &session.key) {
            Some(x) => Observation::Known(x.briefs.iter().any(|b| b == intent_key)),
            None => Observation::unknown("no such session to read the intent key from"),
        })
    }

    fn at_rest(&self, session: &SessionRef) -> Observation<bool> {
        self.probe("atRest", |s| match live(s, &session.key) {
            Some(x) => Observation::Known(x.at_rest),
            None => Observation::unknown("no such session to capture"),
        })
    }

    fn release(&self, session: &SessionRef) -> Observation<EffectOutcome> {
        let key = session.key.clone();
        self.effect("release", move |s| {
            let Some(x) = s.sessions.get_mut(&key) else {
                return EffectOutcome::Released;
            };
            if x.released {
                return EffectOutcome::Released;
            }
            x.released = true;
            x.gone = true;
            s.log.push(format!("release {key}"));
            EffectOutcome::Released
        })
    }

    fn lost(&self, session: &SessionRef) -> Observation<bool> {
        self.probe("lost", |s| match live(s, &session.key) {
            Some(x) => Observation::Known(x.lost),
            None => Observation::unknown("no such session to look at"),
        })
    }

    fn revive(
        &self,
        session: &SessionRef,
        conversation: Option<&str>,
        _: &SpawnSpec,
    ) -> Observation<EffectOutcome> {
        let key = session.key.clone();
        let conversation = conversation.map(str::to_string);
        self.effect("revive", move |s| {
            let ready = !s.spawn_not_ready;
            let Some(x) = s.sessions.get_mut(&key).filter(|x| !x.gone) else {
                return EffectOutcome::Failed {
                    why: "session gone".into(),
                };
            };
            if x.lost {
                x.lost = false;
                x.ready = ready;
                let argv = profile(&x.harness).resume(&x.argv, conversation.as_deref());
                x.restarts.push(argv);
                s.log.push(format!("revive {key}"));
            }
            EffectOutcome::Revived
        })
    }

    fn can_pause(&self) -> bool {
        true
    }

    fn paused(&self, session: &SessionRef) -> Observation<bool> {
        self.probe("paused", |s| match live(s, &session.key) {
            Some(x) => Observation::Known(x.paused),
            None => Observation::unknown("no such session to look at"),
        })
    }

    fn pause(&self, session: &SessionRef) -> Observation<EffectOutcome> {
        let key = session.key.clone();
        self.effect("pause", move |s| {
            let Some(x) = s.sessions.get_mut(&key).filter(|x| !x.gone) else {
                return EffectOutcome::Failed {
                    why: "session gone".into(),
                };
            };
            if !x.paused {
                x.paused = true;
                x.ready = false;
                x.at_rest = true;
                s.log.push(format!("pause {key}"));
            }
            EffectOutcome::Paused
        })
    }

    fn resume(
        &self,
        session: &SessionRef,
        conversation: Option<&str>,
        _: &SpawnSpec,
    ) -> Observation<EffectOutcome> {
        let key = session.key.clone();
        let conversation = conversation.map(str::to_string);
        self.effect("resume", move |s| {
            let ready = !s.spawn_not_ready;
            let Some(x) = s.sessions.get_mut(&key).filter(|x| !x.gone) else {
                return EffectOutcome::Failed {
                    why: "session gone".into(),
                };
            };
            if x.paused {
                x.paused = false;
                x.ready = ready;
                x.at_rest = false;
                let argv = profile(&x.harness).resume(&x.argv, conversation.as_deref());
                x.restarts.push(argv);
                s.log.push(format!("resume {key}"));
            }
            EffectOutcome::Resumed
        })
    }
}

/// The fake workspace adapter: workspaces are records on the fake host, in
/// the same state file as its sessions, with the same injectable faults.
impl Workspaces for FakeRuntime {
    fn materialize(
        &self,
        host: &str,
        key: &str,
        repositories: &RepositoriesWorkspace,
    ) -> Observation<EffectOutcome> {
        let (host, key, repositories) = (host.to_string(), key.to_string(), repositories.clone());
        // One directory per repository, under the key, as a worktree
        // adapter lays them out.
        let root = self.host.dir.join("workspaces").join(&key);
        let mut dirs = repositories
            .repositories
            .iter()
            .map(|r| root.join(&r.name).to_string_lossy().into_owned());
        let dir = dirs
            .next()
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        let extra_dirs: Vec<String> = dirs.collect();
        self.effect("materialize", move |s| {
            if let Some(r) = repositories
                .repositories
                .iter()
                .find(|r| s.unmapped.contains(&r.url))
            {
                return EffectOutcome::Unmapped { url: r.url.clone() };
            }
            if let Some(w) = s.workspaces.get(&key).filter(|w| !w.cleaned) {
                return EffectOutcome::Materialized {
                    workspace: Box::new(w.workspace.clone()),
                    adopted: true,
                };
            }
            let workspace = WorkspaceRef {
                key: key.clone(),
                host: host.clone(),
                dir: dir.clone(),
                extra_dirs: extra_dirs.clone(),
                trust_root: None,
            };
            s.log.push(format!("materialize {host} {key}"));
            s.workspaces.insert(
                key.clone(),
                FakeWorkspace {
                    workspace: workspace.clone(),
                    repositories: repositories.repositories.clone(),
                    cleaned: false,
                },
            );
            EffectOutcome::Materialized {
                workspace: Box::new(workspace),
                adopted: false,
            }
        })
    }

    fn find(&self, host: &str, key: &str) -> Observation<Option<WorkspaceRef>> {
        self.probe("findWorkspace", |s| {
            match s.hosts.get(host).map(String::as_str) {
                Some("down") | Some("unknown") => {
                    Observation::unknown(format!("host {host}: workspaces unreadable"))
                }
                _ => Observation::Known(
                    s.workspaces
                        .get(key)
                        .filter(|w| !w.cleaned && w.workspace.host == host)
                        .map(|w| w.workspace.clone()),
                ),
            }
        })
    }

    fn clean(&self, workspace: &WorkspaceRef) -> Observation<EffectOutcome> {
        let key = workspace.key.clone();
        self.effect("cleanWorkspace", move |s| {
            if let Some(w) = s.workspaces.get_mut(&key).filter(|w| !w.cleaned) {
                w.cleaned = true;
                s.log.push(format!("cleanWorkspace {key}"));
            }
            EffectOutcome::Deleted
        })
    }
}
