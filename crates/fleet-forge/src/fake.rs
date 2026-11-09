//! A fake forge that keeps its changes in one JSON file, so the CLI under
//! test and the test itself see the same forge across processes. It runs no
//! process and reaches no network.
//!
//! Faults are queued per call and consumed in order. They model the Unknown
//! cases of the forge contract: a lost answer, a timeout, a throttled answer
//! with `retryAfter`, an incomplete read, a malformed answer, and proven
//! absence; and for `markReady`, the three outcomes a real forge can give
//! besides success: `moved`, `failed`, and `lost` (Unknown, nothing mutated)
//! or `lost-applied` (Unknown, but the mutation landed).

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fleet_api::forge::{
    ChangeObservation, ChangeRef, ChangeSnapshot, ChangeState, Expected, Forge, Uncertainty,
};
use fleet_api::plan::EffectOutcome;
use fleet_api::{Millis, Observation};
use serde::{Deserialize, Serialize};

pub const KIND: &str = "fake";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgeFault {
    /// `change`, `parse`, `whoami` or `markReady`.
    pub call: String,
    /// For `change`: `lost`, `timeout`, `throttle`, `incomplete`,
    /// `malformed` or `absent`. For `markReady`: `moved`, `failed`, `lost`
    /// or `lost-applied`.
    pub mode: String,
    /// For `throttle`: what the forge says to wait, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<Millis>,
    /// How many calls this fault applies to.
    #[serde(default = "one")]
    pub times: u32,
}

fn one() -> u32 {
    1
}

impl ForgeFault {
    pub fn new(call: &str, mode: &str) -> ForgeFault {
        ForgeFault {
            call: call.into(),
            mode: mode.into(),
            retry_after_ms: None,
            times: 1,
        }
    }

    pub fn retry_after(mut self, ms: Millis) -> ForgeFault {
        self.retry_after_ms = Some(ms);
        self
    }

    pub fn times(mut self, n: u32) -> ForgeFault {
        self.times = n;
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct FakeForgeState {
    /// Changes by `<project>/<id>`.
    pub changes: BTreeMap<String, ChangeSnapshot>,
    pub faults: Vec<ForgeFault>,
    pub calls: BTreeMap<String, u64>,
    /// What `whoami` answers.
    pub login: Option<String>,
    /// The intent key of every mark-ready that mutated a change, in order.
    pub marked_ready: Vec<String>,
}

/// The fake forge's directory, shared by every process that opens it.
#[derive(Debug, Clone)]
pub struct FakeForge {
    host: String,
    dir: PathBuf,
}

/// `<project>/<id>`: the key a change is stored under.
pub fn change_key(project: &str, id: &str) -> String {
    format!("{project}/{id}")
}

impl FakeForge {
    pub fn open(host: &str, dir: &Path) -> io::Result<FakeForge> {
        fs::create_dir_all(dir)?;
        Ok(FakeForge {
            host: host.to_string(),
            dir: dir.to_path_buf(),
        })
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    /// Read-modify-write under the forge's lock.
    pub fn update<T>(&self, f: impl FnOnce(&mut FakeForgeState) -> T) -> io::Result<T> {
        let _lock = Lock::acquire(&self.dir.join("lock"))?;
        let mut state = self.read_unlocked()?;
        let out = f(&mut state);
        self.write_unlocked(&state)?;
        Ok(out)
    }

    pub fn state(&self) -> io::Result<FakeForgeState> {
        let _lock = Lock::acquire(&self.dir.join("lock"))?;
        self.read_unlocked()
    }

    fn read_unlocked(&self) -> io::Result<FakeForgeState> {
        match fs::read(self.state_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(FakeForgeState::default()),
            Err(e) => Err(e),
        }
    }

    fn write_unlocked(&self, state: &FakeForgeState) -> io::Result<()> {
        let tmp = self.dir.join(format!("state.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, self.state_path())
    }

    pub fn add_fault(&self, fault: ForgeFault) -> io::Result<()> {
        self.update(|s| s.faults.push(fault))
    }

    /// Put a change on the forge, replacing what was there.
    pub fn set_change(&self, project: &str, id: &str, change: ChangeSnapshot) -> io::Result<()> {
        self.update(|s| {
            s.changes.insert(change_key(project, id), change);
        })
    }

    pub fn remove_change(&self, project: &str, id: &str) -> io::Result<()> {
        self.update(|s| {
            s.changes.remove(&change_key(project, id));
        })
    }

    pub fn change(&self, project: &str, id: &str) -> io::Result<Option<ChangeSnapshot>> {
        Ok(self.state()?.changes.get(&change_key(project, id)).cloned())
    }

    pub fn calls(&self, call: &str) -> io::Result<u64> {
        Ok(self.state()?.calls.get(call).copied().unwrap_or(0))
    }

    /// The intent keys of every mark-ready that mutated a change, in order.
    pub fn marked_ready(&self) -> io::Result<Vec<String>> {
        Ok(self.state()?.marked_ready)
    }

    /// Take the change out of draft, recording the intent key. `None` when
    /// there is no such change.
    fn apply_ready(&self, change: &ChangeRef, intent_key: &str) -> io::Result<Option<()>> {
        self.update(|s| {
            let c = s
                .changes
                .get_mut(&change_key(&change.project, &change.id))?;
            if c.state == ChangeState::Draft {
                c.state = ChangeState::Open;
            }
            s.marked_ready.push(intent_key.to_string());
            Some(())
        })
    }

    /// Count the call and take its fault, if one is queued.
    fn begin(&self, call: &str) -> Result<Option<ForgeFault>, String> {
        self.update(|s| {
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
        .map_err(|e| format!("fake forge unreadable: {e}"))
    }
}

struct Lock(File);

impl Lock {
    fn acquire(path: &Path) -> io::Result<Lock> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        f.lock()?;
        Ok(Lock(f))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Forge for FakeForge {
    fn operation_bound(&self) -> Millis {
        0
    }

    fn parse(&self, url: &str) -> Observation<ChangeRef> {
        match self.begin("parse") {
            Ok(Some(f)) if f.mode == "malformed" => {
                return Observation::unknown("injected: the URL could not be read");
            }
            Ok(_) => {}
            Err(why) => return Observation::unknown(why),
        }
        match crate::cli::parse_change_url(url) {
            Some((host, project, id)) if host == self.host => Observation::Known(ChangeRef {
                kind: KIND.into(),
                host,
                project,
                id,
            }),
            Some((host, _, _)) => Observation::unknown(format!(
                "the URL names {host}, not the configured host {}",
                self.host
            )),
            None => Observation::unknown("the URL does not name a change"),
        }
    }

    fn whoami(&self, host: &str) -> Observation<String> {
        if host != self.host {
            return Observation::unknown(format!("host {host} is not this fake forge"));
        }
        match self.begin("whoami") {
            Ok(Some(_)) => Observation::unknown("injected: whoami could not tell"),
            Ok(None) => match self.state() {
                Ok(s) => Observation::Known(s.login.unwrap_or_else(|| "fake-login".into())),
                Err(e) => Observation::unknown(e.to_string()),
            },
            Err(why) => Observation::unknown(why),
        }
    }

    fn change(&self, change: &ChangeRef) -> ChangeObservation {
        let fault = match self.begin("change") {
            Ok(f) => f,
            Err(why) => return ChangeObservation::unknown(Uncertainty::Unreadable, why),
        };
        if let Some(f) = fault {
            return match f.mode.as_str() {
                "lost" => ChangeObservation::unknown(Uncertainty::Refused, "injected: answer lost"),
                "timeout" => {
                    ChangeObservation::unknown(Uncertainty::Timeout, "injected: the call timed out")
                }
                "throttle" => ChangeObservation::Unknown {
                    kind: Uncertainty::Throttled,
                    why: "injected: the forge asked to slow down".into(),
                    retry_after: f.retry_after_ms,
                },
                "incomplete" => ChangeObservation::unknown(
                    Uncertainty::Incomplete,
                    "injected: the bound ran out before every page was read",
                ),
                "malformed" => ChangeObservation::unknown(
                    Uncertainty::Unreadable,
                    "injected: the answer could not be parsed",
                ),
                "absent" => ChangeObservation::Known(None),
                other => ChangeObservation::unknown(
                    Uncertainty::Unreadable,
                    format!("injected: unknown fault mode {other}"),
                ),
            };
        }
        match self.state() {
            Ok(s) => ChangeObservation::Known(
                s.changes
                    .get(&change_key(&change.project, &change.id))
                    .cloned(),
            ),
            Err(e) => ChangeObservation::unknown(
                Uncertainty::Unreadable,
                format!("fake forge unreadable: {e}"),
            ),
        }
    }

    /// A fresh read of the change's head and state, `Moved` when either is
    /// not what was expected, else the change leaves draft. Faults: `moved`
    /// refuses as if the fresh read disagreed, `failed` is the forge saying
    /// no, `lost` is Unknown with nothing mutated, `lost-applied` is Unknown
    /// with the mutation landed.
    fn mark_ready(
        &self,
        change: &ChangeRef,
        expect: &Expected,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        let fault = match self.begin("markReady") {
            Ok(f) => f,
            Err(why) => return Observation::unknown(why),
        };
        let current = match self.state() {
            Ok(s) => s
                .changes
                .get(&change_key(&change.project, &change.id))
                .cloned(),
            Err(e) => return Observation::unknown(format!("fake forge unreadable: {e}")),
        };
        match fault.as_ref().map(|f| f.mode.as_str()) {
            Some("lost") => {
                return Observation::unknown("injected: the answer was lost; nothing was mutated");
            }
            Some("lost-applied") => {
                if let Err(e) = self.apply_ready(change, intent_key) {
                    return Observation::unknown(format!("fake forge unreadable: {e}"));
                }
                return Observation::unknown(
                    "injected: the answer was lost after the mutation landed",
                );
            }
            Some("failed") => {
                return Observation::Known(EffectOutcome::Failed {
                    why: "injected: the forge refused to mark the change ready".into(),
                });
            }
            Some("moved") => {
                let (head, state) = current.as_ref().map_or_else(
                    || (format!("{}-moved", expect.head), ChangeState::Open),
                    |c| (c.head.id.clone(), c.state),
                );
                return Observation::Known(EffectOutcome::Moved { head, state });
            }
            Some(other) => {
                return Observation::Known(EffectOutcome::Failed {
                    why: format!("injected: unknown fault mode {other}"),
                });
            }
            None => {}
        }
        let Some(c) = current else {
            return Observation::Known(EffectOutcome::Failed {
                why: "the change does not exist on the forge".into(),
            });
        };
        if c.head.id != expect.head || c.state != expect.state {
            return Observation::Known(EffectOutcome::Moved {
                head: c.head.id,
                state: c.state,
            });
        }
        let already = c.state != ChangeState::Draft;
        match self.apply_ready(change, intent_key) {
            Ok(Some(())) => Observation::Known(EffectOutcome::MarkedReady { already }),
            Ok(None) => Observation::Known(EffectOutcome::Failed {
                why: "the change vanished before the mutation".into(),
            }),
            Err(e) => Observation::unknown(format!("fake forge unreadable: {e}")),
        }
    }
}
