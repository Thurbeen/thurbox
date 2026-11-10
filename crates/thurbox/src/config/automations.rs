//! Automations: named, scheduled runs, kept in `automations.json` beside
//! `settings.toml`.
//!
//! Fleet has no schedule kind yet (ADR-41), so the schedule is thurbox's: a
//! plain file every firer reads and writes under the file lock every process
//! takes ([`crate::paths`]'s), and a tick that fires what is due — the
//! interface's, while it runs, and `thurbox-cli automation tick` from an OS
//! timer. What a fire does to a session goes through Fleet's command line
//! (`kernel::automations`).
//!
//! The file holds each automation's definition and its schedule state (the
//! last and next run, and its most recent runs), so a fire is claimed in the
//! file, under the lock, before it runs: two firers never fire one run twice,
//! and a firer that dies mid-run leaves a run marked running that the next
//! tick records as timed out.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::session::automation::{AutomationSchedule, DEFAULT_TIMEOUT_SECS};

const FILE: &str = "automations.json";

/// How many runs an automation keeps: the recent history a person looks at a
/// scheduler for.
pub const RUNS_KEPT: usize = 20;

/// Every automation, in the order they were created.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Automations {
    pub automations: Vec<Automation>,
}

/// One automation: what fires it, what it does, and how its runs went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Automation {
    /// Unique among automations: what every command addresses it by.
    pub name: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// `cron:<expr>` or `at:<unix millis>`.
    pub trigger: String,
    /// IANA name; `None` is the system's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// What a send or spawn types into the session; an exec ignores it.
    #[serde(default)]
    pub prompt: String,
    pub action: Action,
    /// Seconds a run may take; `None` is [`DEFAULT_TIMEOUT_SECS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Unix millis.
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<u64>,
    /// `None` once the schedule has no further occurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<u64>,
    /// The most recent runs, oldest first, at most [`RUNS_KEPT`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<Run>,
}

fn enabled() -> bool {
    true
}

/// What an automation does when it fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Action {
    /// Queue the prompt for a session: `fleet send <session> <prompt>`.
    Send { session: String },
    /// Create a session in `repo` running `agent` (Fleet's default harness
    /// when `None`), then queue the prompt for it.
    Spawn {
        repo: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    /// Run a command headlessly on this machine, with no session.
    Exec {
        command: String,
        /// Where it runs; `None` is the home directory.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
        /// Variables it runs with on top of what it inherits.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
}

impl Action {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Send { .. } => "send",
            Self::Spawn { .. } => "spawn",
            Self::Exec { .. } => "exec",
        }
    }
}

/// One fire of an automation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// Unix millis; with the automation's name, what identifies the run.
    pub started_at: u64,
    pub status: RunStatus,
    #[serde(default)]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    /// An exec's exit code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The session a send or spawn reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Claimed and not finished: recorded before the run starts.
    Running,
    Success,
    Error,
    /// Killed at its deadline, or still marked running past it.
    TimedOut,
}

impl Automation {
    /// The schedule the trigger spells.
    pub fn schedule(&self) -> Result<AutomationSchedule, String> {
        crate::session::automation::parse_trigger(&self.trigger, None, None)
    }

    /// A run's deadline.
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS))
    }

    /// When it fires next after `now`, or `None` while disabled and once the
    /// schedule has no further occurrence.
    pub fn next_after(&self, now: u64) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        self.schedule()
            .ok()?
            .next_after(now, self.timezone.as_deref())
    }

    /// Start a run at `now`, keeping at most [`RUNS_KEPT`].
    pub fn start_run(&mut self, now: u64) {
        self.last_run_at = Some(now);
        self.runs.push(Run {
            started_at: now,
            status: RunStatus::Running,
            detail: String::new(),
            finished_at: None,
            exit_code: None,
            session: None,
        });
        let excess = self.runs.len().saturating_sub(RUNS_KEPT);
        self.runs.drain(..excess);
    }

    /// Record a run still marked running past its deadline as timed out: its
    /// firer hung or died.
    pub fn expire_runs(&mut self, now: u64) {
        let deadline = self.timeout().as_millis() as u64;
        for run in &mut self.runs {
            if run.status == RunStatus::Running && now > run.started_at.saturating_add(deadline) {
                run.status = RunStatus::TimedOut;
                run.detail = "no outcome was recorded before its deadline".into();
                run.finished_at = Some(now);
            }
        }
    }
}

/// The automations as stored; none when there is no file. A file that does
/// not parse is an error, never an empty list: a tick that read it as empty
/// and wrote it back would lose every automation.
pub fn load() -> Result<Automations, String> {
    let Some(path) = super::file::path(FILE) else {
        return Ok(Automations::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Automations::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Apply `change` to the stored automations under the file lock and write
/// them back, unless `change` refuses. Answers what `change` answered.
pub fn update<T>(change: impl FnOnce(&mut Automations) -> Result<T, String>) -> Result<T, String> {
    let path = super::file::path(FILE).ok_or("could not resolve the config directory")?;
    super::file::locked(&path, || {
        let mut all = load()?;
        let answer = change(&mut all)?;
        let text = serde_json::to_string_pretty(&all).map_err(|e| e.to_string())?;
        super::file::save(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(answer)
    })
    .map_err(|e| format!("lock {}: {e}", path.display()))?
}
