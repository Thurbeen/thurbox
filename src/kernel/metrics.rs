//! Machine and per-agent metrics, sampled off the render path.
//!
//! Everything here touches the world: `sysinfo` walks `/proc`, and resolving a
//! session's root pid is a control-mode round trip. So this is the fifth
//! instance of the kernel's worker pattern — sample on a thread, publish the
//! result, let the UI read whatever is currently known.
//!
//! Account quotas are not sampled here (#1601). Asking a provider means reading
//! credentials and calling its API, which belongs to a plugin the user installs
//! and grants `run` — `thurbox-info-panel` does it with `quota-axi` — not to a
//! core that would do it for every session whether anything reads the answer.
//!
//! Absence is a real state and is kept distinct from zero: a session whose agent
//! writes no statusline file has no metrics, which must not render as an agent
//! that has spent nothing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::session::AgentMetrics;

/// How often the machine and statusline sample is retaken. v1's
/// `METRICS_REFRESH_TICKS` at its ~10ms tick.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

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

/// One session's sampling inputs, reduced to what crosses a thread boundary:
/// its id, where its statusline file is, and the handle its pid is resolved
/// through.
type SampleInput = (
    String,
    Option<PathBuf>,
    Option<(Arc<dyn crate::backend::SessionBackend>, String)>,
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
    pub pane: Option<(Arc<dyn crate::backend::SessionBackend>, String)>,
}

/// Everything the info panel reads, and the workers that keep it current.
pub struct Metrics {
    version: u64,
    system: SystemMetrics,
    resources: HashMap<String, SessionResources>,
    agents: HashMap<String, AgentMetrics>,
    /// Parked between samples so CPU deltas survive. `None` while a worker has
    /// it, which is also what keeps a second sample from starting.
    collector: Option<Box<sysinfo::System>>,
    last_sample: Option<Instant>,
    tx: Sender<Sample>,
    rx: Receiver<Sample>,
}

impl Metrics {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            version: 0,
            system: SystemMetrics::default(),
            resources: HashMap::new(),
            agents: HashMap::new(),
            collector: Some(Box::new(sysinfo::System::new())),
            last_sample: None,
            tx,
            rx,
        }
    }

    /// Serial of the latest accepted sample, independent of other workers.
    pub fn version(&self) -> u64 {
        self.version
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

    /// Whether the next [`Self::sample`] would actually start work — the
    /// machine sample is due.
    ///
    /// Asked before the subject list is built: assembling it clones two
    /// strings per session, and doing that on every 10 ms iteration for a
    /// once-per-second consumer was the same waste `serve_runs` already
    /// refuses one function away.
    pub fn wants_sample(&self) -> bool {
        self.last_sample
            .map_or(true, |at| at.elapsed() >= SAMPLE_INTERVAL)
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
    }

    /// Drop per-session numbers for sessions that no longer exist.
    ///
    /// [`Self::poll`] *merges* each sample rather than replacing it, so a session
    /// whose statusline was unreadable this round keeps what it last reported —
    /// which means a deleted one keeps it for the life of the process. `Notifier`
    /// prunes its own per-session bookkeeping off the same live set.
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
        if changed {
            self.version = self.version.wrapping_add(1);
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
type BackendPanes = (
    Arc<dyn crate::backend::SessionBackend>,
    Vec<(String, String)>,
);

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

    impl crate::backend::SessionBackend for Panes {
        fn hook_signal_command(&self) -> Option<String> {
            None
        }
        fn record_hook_state(
            &self,
            _: &str,
            _: &str,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no status channel").into())
        }
        fn hook_states(&self) -> crate::backend::contract::BackendResult<Vec<(String, String)>> {
            Err(anyhow::anyhow!("this stub has no status channel").into())
        }
        fn take_hook_state_events(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn ensure_heartbeat(
            &self,
            _: &std::path::Path,
            _: &[String],
            _: std::time::Duration,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub keeps no heartbeat").into())
        }
        fn heartbeat_running(&self) -> crate::backend::contract::BackendResult<bool> {
            Ok(false)
        }
        fn stop_heartbeat(&self) -> crate::backend::contract::BackendResult<bool> {
            Ok(false)
        }
        fn send_text(
            &self,
            _: &str,
            _: &str,
            _: bool,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }
        fn send_text_after(
            &self,
            _: &str,
            _: &str,
            _: std::time::Duration,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }
        fn send_key(
            &self,
            _: &str,
            _: &crate::backend::Key,
        ) -> crate::backend::contract::BackendResult<String> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }
        fn capture(
            &self,
            _: &str,
            _: u32,
            _: bool,
        ) -> crate::backend::contract::BackendResult<String> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }
        fn pane_state(
            &self,
            _: &str,
        ) -> crate::backend::contract::BackendResult<crate::backend::PaneState> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }
        fn pane_path(&self, _: &str) -> crate::backend::contract::BackendResult<Option<String>> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }
        fn create_window(
            &self,
            _: &crate::backend::WindowSpec<'_>,
        ) -> crate::backend::contract::BackendResult<crate::backend::PaneId> {
            Err(anyhow::anyhow!("{}: a stub opens no windows", self.name()).into())
        }
        fn locate(
            &self,
            _: crate::backend::Owner<'_>,
        ) -> crate::backend::contract::BackendResult<crate::backend::Placed> {
            Ok(crate::backend::Placed {
                agent: crate::backend::Located::Absent,
                shell: crate::backend::Located::Absent,
            })
        }
        fn rename_windows(
            &self,
            _: crate::backend::Owner<'_>,
            _: &str,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }
        fn stamp_window(
            &self,
            _: &str,
            _: &str,
            _: crate::backend::WindowRole,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }
        fn window_panes(
            &self,
            _: &str,
        ) -> crate::backend::contract::BackendResult<Vec<(String, bool)>> {
            Ok(Vec::new())
        }
        fn set_pane_retention(
            &self,
            _: &str,
            _: bool,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }
        fn shutdown(&self) {}
        fn name(&self) -> &str {
            "panes"
        }
        fn default_shell(&self) -> String {
            "/bin/sh".to_string()
        }
        fn check_available(&self) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }
        fn ensure_ready(&self) -> crate::backend::contract::BackendResult<()> {
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
        ) -> crate::backend::contract::BackendResult<crate::backend::SpawnedSession> {
            unimplemented!()
        }
        fn adopt(
            &self,
            _: &str,
            _: u16,
            _: u16,
            _: Option<Vec<u8>>,
        ) -> crate::backend::contract::BackendResult<crate::backend::AdoptedSession> {
            unimplemented!()
        }
        fn discover(
            &self,
        ) -> crate::backend::contract::BackendResult<Vec<crate::backend::DiscoveredSession>>
        {
            Ok(Vec::new())
        }
        fn resize(&self, _: &str, _: u16, _: u16) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }
        fn is_dead(&self, _: &str) -> crate::backend::contract::BackendResult<bool> {
            Ok(false)
        }
        fn kill(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }
        fn detach(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }
        fn pane_pid(&self, pane: &str) -> crate::backend::contract::BackendResult<Option<u32>> {
            Ok(self.pids.get(pane).copied())
        }
        fn pane_pids(&self) -> crate::backend::contract::BackendResult<HashMap<String, u32>> {
            if self.batched {
                Ok(self.pids.clone())
            } else {
                Err(anyhow::anyhow!("no batched lookup").into())
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
        let batched: Arc<dyn crate::backend::SessionBackend> = Arc::new(Panes {
            pids: HashMap::from([("%1".to_string(), me)]),
            batched: true,
        });
        let one_at_a_time: Arc<dyn crate::backend::SessionBackend> = Arc::new(Panes {
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
