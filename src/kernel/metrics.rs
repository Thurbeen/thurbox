//! Machine, per-agent and account metrics, sampled off the render path.
//!
//! Everything here touches the world: `sysinfo` walks `/proc`, resolving a
//! session's root pid is a control-mode round trip, and account usage shells out
//! to `curl` against a vendor API. So this is the fifth instance of the kernel's
//! worker pattern — sample on a thread, publish the result, let the UI read
//! whatever is currently known.
//!
//! The three sources refresh on their own cadences because they cost wildly
//! different amounts: the machine every second, statusline files with it, and
//! account usage every five minutes (it is a network call, and a rate-limit
//! window does not move faster than that). v1 spends the same three intervals
//! (`METRICS_REFRESH_TICKS`, `USAGE_REFRESH_TICKS`) on the same split.
//!
//! Absence is a real state and is kept distinct from zero: a session whose agent
//! writes no statusline file has no metrics, which must not render as an agent
//! that has spent nothing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::session::{AgentMetrics, AgentUsage};

/// How often the machine and statusline sample is retaken. v1's
/// `METRICS_REFRESH_TICKS` at its ~10ms tick.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// How often account usage is refetched. v1's `USAGE_REFRESH_TICKS`.
const USAGE_INTERVAL: Duration = Duration::from_secs(300);

/// Machine-wide and per-session resource use.
///
/// The kernel's own type rather than v1's `ui::info_panel::SystemMetrics`: the
/// kernel may not reference `ui`, and this is published data, not a widget's
/// input.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SystemMetrics {
    /// Whole-machine CPU, 0-100.
    pub cpu_percent: f32,
    pub memory_used: u64,
    pub memory_total: u64,
}

/// One session's own resource use, sampled from its pane's root process.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SessionResources {
    /// The process's CPU, which may exceed 100 across cores.
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

/// The scope account usage is fetched for: an agent, as seen from a host.
///
/// Usage is account-global, but *which* account depends on the credentials on
/// the machine the agent runs on — so a session on `ssh:devbox` may report a
/// different account than a local one. v1 keys its cache the same way.
type UsageKey = (String, Option<String>);

/// One session's sampling inputs, reduced to what crosses a thread boundary:
/// its id, where its statusline file is, and the handle its pid is resolved
/// through.
type SampleInput = (
    String,
    Option<PathBuf>,
    Option<(Arc<dyn crate::agent::SessionBackend>, String)>,
);

/// What a sample worker reports back.
struct Sample {
    system: SystemMetrics,
    resources: HashMap<String, SessionResources>,
    agents: HashMap<String, AgentMetrics>,
    /// The collector, handed back so it keeps its CPU-delta state: a percentage
    /// is a difference between two samples, so a fresh collector reads zero.
    collector: Box<sysinfo::System>,
}

/// What one session's sample needs: where to find its statusline file, and how
/// to resolve its root pid.
pub struct Subject {
    pub session: String,
    /// The agent's own session id names its statusline file.
    pub agent_session_id: Option<String>,
    /// Backend plus pane id, for the pid lookup. Absent for a session with no
    /// live pane, which then contributes no per-session numbers.
    pub pane: Option<(Arc<dyn crate::agent::SessionBackend>, String)>,
    /// Agent name and host, for the usage scope.
    pub agent: String,
    pub host: Option<String>,
}

/// Everything the info panel reads, and the workers that keep it current.
pub struct Metrics {
    system: SystemMetrics,
    resources: HashMap<String, SessionResources>,
    agents: HashMap<String, AgentMetrics>,
    usage: HashMap<UsageKey, AgentUsage>,
    /// Parked between samples so CPU deltas survive. `None` while a worker has
    /// it, which is also what keeps a second sample from starting.
    collector: Option<Box<sysinfo::System>>,
    last_sample: Option<Instant>,
    last_usage: Option<Instant>,
    tx: Sender<Sample>,
    rx: Receiver<Sample>,
    usage_tx: Sender<(UsageKey, AgentUsage)>,
    usage_rx: Receiver<(UsageKey, AgentUsage)>,
}

impl Metrics {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let (usage_tx, usage_rx) = channel();
        Self {
            system: SystemMetrics::default(),
            resources: HashMap::new(),
            agents: HashMap::new(),
            usage: HashMap::new(),
            collector: Some(Box::new(sysinfo::System::new())),
            last_sample: None,
            last_usage: None,
            tx,
            rx,
            usage_tx,
            usage_rx,
        }
    }

    pub fn system(&self) -> SystemMetrics {
        self.system
    }

    pub fn resources(&self, session: &str) -> Option<&SessionResources> {
        self.resources.get(session)
    }

    pub fn agent(&self, session: &str) -> Option<&AgentMetrics> {
        self.agents.get(session)
    }

    /// Account usage for a session's (agent, host) scope, when it has been
    /// fetched.
    pub fn usage(&self, agent: &str, host: Option<&str>) -> Option<&AgentUsage> {
        self.usage
            .get(&(agent.to_string(), host.map(str::to_string)))
    }

    /// Whether the next [`Self::sample`] would actually start work — the
    /// machine sample or the account-usage fetch is due.
    ///
    /// Asked before the subject list is built: assembling it clones four
    /// strings per session, and doing that on every 10 ms iteration for a
    /// once-per-second consumer was the same waste `serve_runs` already
    /// refuses one function away.
    pub fn wants_sample(&self) -> bool {
        let sample_due = self
            .last_sample
            .map_or(true, |at| at.elapsed() >= SAMPLE_INTERVAL);
        let usage_due = self
            .last_usage
            .map_or(true, |at| at.elapsed() >= USAGE_INTERVAL);
        sample_due || usage_due
    }

    /// Start a sample if one is due and none is running.
    ///
    /// Gate with [`Self::wants_sample`]; the interval and the parked collector
    /// both re-check here, so an unguarded call is still safe. The per-session
    /// prune rides the same cadence — a deleted session's numbers linger at
    /// most one interval longer, which nothing renders.
    pub fn sample(&mut self, subjects: Vec<Subject>) {
        self.forget_gone(&subjects);
        self.sample_machine(&subjects);
        self.fetch_usage(&subjects);
    }

    /// Drop per-session numbers for sessions that no longer exist.
    ///
    /// [`Self::poll`] *merges* each sample rather than replacing it, so a session
    /// whose statusline was unreadable this round keeps what it last reported —
    /// which means a deleted one keeps it for the life of the process. `Notifier`
    /// prunes its own per-session bookkeeping off the same live set.
    ///
    /// The account usage map is keyed by agent and host rather than by session, so
    /// it is not per-session state and is left alone.
    fn forget_gone(&mut self, subjects: &[Subject]) {
        let live: std::collections::HashSet<&str> = subjects
            .iter()
            .map(|subject| subject.session.as_str())
            .collect();
        self.agents.retain(|id, _| live.contains(id.as_str()));
        self.resources.retain(|id, _| live.contains(id.as_str()));
    }

    fn sample_machine(&mut self, subjects: &[Subject]) {
        let due = self
            .last_sample
            .map_or(true, |at| at.elapsed() >= SAMPLE_INTERVAL);
        if !due {
            return;
        }
        // An absent collector means a sample is already in flight: a slow one
        // delays the next rather than stacking a second walk of /proc onto it.
        let Some(collector) = self.collector.take() else {
            return;
        };
        self.last_sample = Some(Instant::now());

        // Only what the worker needs, so nothing borrows `self` across threads.
        let work: Vec<SampleInput> = subjects
            .iter()
            .map(|subject| {
                (
                    subject.session.clone(),
                    subject
                        .agent_session_id
                        .as_ref()
                        .and_then(|id| statusline_file(id)),
                    subject.pane.clone(),
                )
            })
            .collect();

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(collect(collector, work));
        });
    }

    fn fetch_usage(&mut self, subjects: &[Subject]) {
        let due = self
            .last_usage
            .map_or(true, |at| at.elapsed() >= USAGE_INTERVAL);
        if !due {
            return;
        }
        self.last_usage = Some(Instant::now());

        let (hosts, _warnings) = crate::agent::host_config::cached_registry();
        let mut planned: std::collections::HashSet<UsageKey> = std::collections::HashSet::new();
        for subject in subjects {
            if !crate::usage::is_supported(&subject.agent) {
                continue;
            }
            let key: UsageKey = (subject.agent.clone(), subject.host.clone());
            if !planned.insert(key.clone()) {
                continue;
            }
            // A remote scope needs its host definition to read credentials
            // there. A host that is configured away is reported rather than
            // retried, and only when nothing was ever fetched for that scope —
            // keeping last-known numbers across a transient outage, as v1 does.
            let host = match &subject.host {
                None => None,
                Some(name) => match hosts.get(name) {
                    Some(host) => Some(host.clone()),
                    None => {
                        self.usage.entry(key).or_insert_with(|| AgentUsage {
                            note: Some("usage unavailable (host not configured)".to_string()),
                            ..Default::default()
                        });
                        continue;
                    }
                },
            };
            let tx = self.usage_tx.clone();
            let agent = key.0.clone();
            tokio::spawn(async move {
                let usage = crate::usage::fetch(&agent, host.as_ref()).await;
                let _ = tx.send((key, usage));
            });
        }
    }

    /// Fold finished samples in. Returns true when anything arrived, so the
    /// caller can repaint.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(sample) = self.rx.try_recv() {
            self.system = sample.system;
            self.resources = sample.resources;
            // Merged rather than replaced: a session whose file was unreadable
            // this round keeps what it last reported instead of blanking.
            self.agents.extend(sample.agents);
            self.collector = Some(sample.collector);
            changed = true;
        }
        while let Ok((key, usage)) = self.usage_rx.try_recv() {
            self.usage.insert(key, usage);
            changed = true;
        }
        changed
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// The statusline JSON an agent writes for a session, if the directory resolves.
fn statusline_file(agent_session_id: &str) -> Option<PathBuf> {
    crate::paths::metrics_directory().map(|dir| dir.join(format!("{agent_session_id}.json")))
}

/// One backend and its sampled panes, as `(session, pane_id)` pairs — the unit
/// the batched pid lookup in [`pane_pids`] groups subjects into.
type BackendPanes = (Arc<dyn crate::agent::SessionBackend>, Vec<(String, String)>);

/// Take one sample. Runs on a worker thread.
fn collect(mut collector: Box<sysinfo::System>, subjects: Vec<SampleInput>) -> Sample {
    collector.refresh_cpu_all();
    collector.refresh_memory();
    let system = SystemMetrics {
        cpu_percent: collector.global_cpu_usage(),
        memory_used: collector.used_memory(),
        memory_total: collector.total_memory(),
    };
    let pids = pane_pids(&subjects);
    let resources = process_resources(&mut collector, &pids);
    let agents = statusline_metrics(subjects);
    Sample {
        system,
        resources,
        agents,
        collector,
    }
}

/// Each sampled session's pane pid, batched per backend.
///
/// Each single-pane lookup is a control-mode round trip serialized on the
/// connection mutex keystrokes share, so asking per session per second scaled
/// the contention with the session count. One `pane_pids` call per backend
/// answers for all of its sessions; a backend without the batched command
/// falls back to the per-pane lookup.
fn pane_pids(subjects: &[SampleInput]) -> Vec<(String, u32)> {
    let mut by_backend: HashMap<usize, BackendPanes> = HashMap::new();
    for (session, _, pane) in subjects {
        if let Some((backend, pane_id)) = pane {
            // Keyed by the Arc's address: backends carry no id of their own
            // here, and two subjects on one host share the same Arc.
            let key = Arc::as_ptr(backend).cast::<()>() as usize;
            by_backend
                .entry(key)
                .or_insert_with(|| (Arc::clone(backend), Vec::new()))
                .1
                .push((session.clone(), pane_id.clone()));
        }
    }
    let mut pids = Vec::new();
    for (backend, sessions) in by_backend.into_values() {
        match backend.pane_pids() {
            Ok(map) => pids.extend(
                sessions
                    .into_iter()
                    .filter_map(|(session, pane_id)| Some((session, *map.get(&pane_id)?))),
            ),
            Err(_) => pids.extend(sessions.into_iter().filter_map(|(session, pane_id)| {
                Some((session, backend.pane_pid(&pane_id).ok().flatten()?))
            })),
        }
    }
    pids
}

/// CPU and memory for each pid, from one process refresh over all of them
/// rather than one walk per session.
fn process_resources(
    collector: &mut sysinfo::System,
    pids: &[(String, u32)],
) -> HashMap<String, SessionResources> {
    let mut resources = HashMap::new();
    if pids.is_empty() {
        return resources;
    }
    let all_pids: Vec<sysinfo::Pid> = pids
        .iter()
        .map(|(_, pid)| sysinfo::Pid::from_u32(*pid))
        .collect();
    let kind = sysinfo::ProcessRefreshKind::nothing()
        .with_memory()
        .with_cpu();
    collector.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&all_pids), false, kind);
    for (session, pid) in pids {
        if let Some(process) = collector.process(sysinfo::Pid::from_u32(*pid)) {
            resources.insert(
                session.clone(),
                SessionResources {
                    cpu_percent: process.cpu_usage(),
                    memory_bytes: process.memory(),
                },
            );
        }
    }
    resources
}

/// Each session's statusline metrics. Best-effort: an agent that writes no
/// statusline file simply has no metrics, which the panel renders as absence.
fn statusline_metrics(subjects: Vec<SampleInput>) -> HashMap<String, AgentMetrics> {
    let mut agents = HashMap::new();
    for (session, statusline, _) in subjects {
        let Some(text) = statusline.and_then(|path| std::fs::read_to_string(path).ok()) else {
            continue;
        };
        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&text) {
            agents.insert(session, AgentMetrics::from_statusline_json(&raw));
        }
    }
    agents
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Pane pids from a table: batched, or — for a backend without the batched
    /// command — one pane at a time.
    struct Panes {
        pids: HashMap<String, u32>,
        batched: bool,
    }

    impl crate::agent::SessionBackend for Panes {
        fn name(&self) -> &str {
            "panes"
        }
        fn check_available(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn ensure_ready(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn spawn(
            &self,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<&Path>,
            _: &HashMap<String, String>,
            _: u16,
            _: u16,
        ) -> anyhow::Result<crate::agent::backend::SpawnedSession> {
            unimplemented!()
        }
        fn adopt(
            &self,
            _: &str,
            _: u16,
            _: u16,
            _: Option<Vec<u8>>,
        ) -> anyhow::Result<crate::agent::backend::AdoptedSession> {
            unimplemented!()
        }
        fn discover(&self) -> anyhow::Result<Vec<crate::agent::backend::DiscoveredSession>> {
            Ok(Vec::new())
        }
        fn resize(&self, _: &str, _: u16, _: u16) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_dead(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        fn kill(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn detach(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn pane_pid(&self, pane: &str) -> anyhow::Result<Option<u32>> {
            Ok(self.pids.get(pane).copied())
        }
        fn pane_pids(&self) -> anyhow::Result<HashMap<String, u32>> {
            if self.batched {
                Ok(self.pids.clone())
            } else {
                anyhow::bail!("no batched lookup")
            }
        }
    }

    #[test]
    fn a_sample_resolves_pids_per_backend_and_reads_only_parseable_statuslines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let good = dir.path().join("good.json");
        std::fs::write(&good, "{}").expect("write");
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "not json").expect("write");
        // This test's own process: a pid sysinfo is certain to find.
        let me = std::process::id();
        let batched: Arc<dyn crate::agent::SessionBackend> = Arc::new(Panes {
            pids: HashMap::from([("%1".to_string(), me)]),
            batched: true,
        });
        let one_at_a_time: Arc<dyn crate::agent::SessionBackend> = Arc::new(Panes {
            pids: HashMap::from([("%3".to_string(), me)]),
            batched: false,
        });
        let subjects: Vec<SampleInput> = vec![
            (
                "one".into(),
                Some(good),
                Some((Arc::clone(&batched), "%1".into())),
            ),
            (
                "two".into(),
                Some(bad),
                Some((Arc::clone(&batched), "%2".into())),
            ),
            (
                "three".into(),
                Some(dir.path().join("missing.json")),
                Some((Arc::clone(&one_at_a_time), "%3".into())),
            ),
            ("four".into(), None, None),
        ];

        let sample = collect(Box::new(sysinfo::System::new()), subjects);

        let mut resourced: Vec<&str> = sample.resources.keys().map(String::as_str).collect();
        resourced.sort_unstable();
        assert_eq!(resourced, ["one", "three"]);
        let agents: Vec<&str> = sample.agents.keys().map(String::as_str).collect();
        assert_eq!(agents, ["one"]);
        assert!(sample.system.memory_total > 0);
    }
}
