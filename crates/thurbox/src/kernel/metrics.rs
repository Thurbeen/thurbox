use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::session::AgentMetrics;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SystemMetrics {
    pub cpu_percent: f32,
    pub memory_used: u64,
    pub memory_total: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SessionResources {
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

type SampleInput = (
    String,
    Option<PathBuf>,
    Option<(Arc<dyn crate::backend::SessionBackend>, String)>,
);

struct Sample {
    system: SystemMetrics,
    resources: HashMap<String, SessionResources>,
    agents: HashMap<String, AgentMetrics>,
    collector: Box<sysinfo::System>,
}

pub struct Subject {
    pub session: String,
    pub agent_session_id: Option<String>,
    pub pane: Option<(Arc<dyn crate::backend::SessionBackend>, String)>,
}

pub struct Metrics {
    version: u64,
    system: SystemMetrics,
    resources: HashMap<String, SessionResources>,
    agents: HashMap<String, AgentMetrics>,
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
            collector: sysinfo::System::new().ok().map(Box::new),
            last_sample: None,
            tx,
            rx,
        }
    }

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

    pub fn wants_sample(&self) -> bool {
        self.last_sample
            .is_none_or(|at| at.elapsed() >= SAMPLE_INTERVAL)
    }

    pub fn sample(&mut self, subjects: Vec<Subject>) {
        self.forget_gone(&subjects);
        self.sample_machine(&subjects);
    }

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
            .is_none_or(|at| at.elapsed() >= SAMPLE_INTERVAL);
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

fn statusline_file(agent_session_id: &str) -> Option<PathBuf> {
    crate::paths::metrics_directory().map(|dir| dir.join(format!("{agent_session_id}.json")))
}

type BackendPanes = (
    Arc<dyn crate::backend::SessionBackend>,
    Vec<(String, String)>,
);

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

    struct Panes {
        pids: HashMap<String, u32>,
        batched: bool,
    }

    impl crate::backend::PaneStream for Panes {
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

        fn resize(&self, _: &str, _: u16, _: u16) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn detach(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }
    }

    impl crate::backend::WindowLifecycle for Panes {
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
            Err(anyhow::anyhow!(
                "{}: a stub opens no windows",
                crate::backend::SessionBackend::name(self)
            )
            .into())
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

        fn default_shell(&self) -> String {
            "/bin/sh".to_string()
        }

        fn check_available(&self) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn ensure_ready(&self) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn discover(
            &self,
        ) -> crate::backend::contract::BackendResult<Vec<crate::backend::DiscoveredSession>>
        {
            Ok(Vec::new())
        }

        fn is_dead(&self, _: &str) -> crate::backend::contract::BackendResult<bool> {
            Ok(false)
        }

        fn kill(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
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

    impl crate::backend::PaneIo for Panes {
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
    }

    impl crate::backend::StatusChannel for Panes {
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
    }

    impl crate::backend::SessionBackend for Panes {
        fn name(&self) -> &str {
            "panes"
        }
        fn capabilities(&self) -> crate::backend::Capabilities {
            crate::backend::Capabilities::NONE
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

        let system = sysinfo::System::new().expect("sysinfo supports this host");
        let sample = collect(Box::new(system), subjects);

        let mut resourced: Vec<&str> = sample.resources.keys().map(String::as_str).collect();
        resourced.sort_unstable();
        assert_eq!(resourced, ["one", "three"]);
        let agents: Vec<&str> = sample.agents.keys().map(String::as_str).collect();
        assert_eq!(agents, ["one"]);
        assert!(sample.system.memory_total > 0);
    }
}
