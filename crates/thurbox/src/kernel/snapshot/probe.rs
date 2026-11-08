//! The pane-probe worker: what holds each unreported session's pane, asked
//! of its backend on a thread because the answer is a `display-message` and a
//! `ps`.

use std::time::{Duration, Instant};

use crate::session::{AgentRegistry, Corroboration};

/// How long a pane verdict stands before it is asked for again.
///
/// The probe shells out — one `display-message` plus one `ps` — so it can no
/// more run on the render path than a git stat can, and this bound is what
/// stops a session that never reports from paying for one per refresh. Two
/// seconds is also about the resolution the answer has: it says an agent is
/// *there*, not what it is doing, and that fact changes when a process starts
/// or exits.
const PANE_PROBE_TTL: Duration = Duration::from_secs(2);

/// One worker's answer: the session it probed, and what holds its pane.
type ProbeResult = (String, Corroboration);

/// One pane's verdict and when it was reached.
pub(super) struct Probe {
    pub(super) corroboration: Corroboration,
    pub(super) at: Instant,
}

/// What holds each session's pane, computed off the render path.
///
/// Another instance of the worker pattern (see [`super::git::GitStats`], and
/// `kernel::updates`): touch the world on a thread, publish the result.
///
/// The interface used to skip this check altogether and derive its dot from
/// the hook columns alone, which is why a driver-launched agent — nothing
/// wired, nothing signalled — drew the green hollow `idle` circle while it
/// worked.
///
/// Asked **only** about rows whose `hook_state` is null. An agent that reports
/// for itself needs no observation ([`crate::session::best_state`] would
/// discard it anyway), and probing every session every refresh is the cost the
/// interface declined to pay in the first place.
#[derive(Default)]
pub(super) struct PaneProbe {
    pub(super) known: std::collections::HashMap<String, Probe>,
    inflight: std::collections::HashSet<String>,
    channel: Option<(
        std::sync::mpsc::Sender<ProbeResult>,
        std::sync::mpsc::Receiver<ProbeResult>,
    )>,
}

impl PaneProbe {
    fn ensure_channel(&mut self) -> std::sync::mpsc::Sender<ProbeResult> {
        if self.channel.is_none() {
            self.channel = Some(std::sync::mpsc::channel());
        }
        self.channel.as_ref().expect("just created").0.clone()
    }

    /// Ask what holds a session's pane, unless a probe is in flight or the last
    /// answer is still fresh.
    pub(super) fn request(
        &mut self,
        backend: std::sync::Arc<dyn crate::backend::SessionBackend>,
        session: &str,
        name: &str,
        agent_command: String,
        registry: &std::sync::Arc<AgentRegistry>,
    ) {
        if self.inflight.contains(session) {
            return;
        }
        if self
            .known
            .get(session)
            .is_some_and(|probe| probe.at.elapsed() < PANE_PROBE_TTL)
        {
            return;
        }
        self.inflight.insert(session.to_string());
        let tx = self.ensure_channel();
        let (session, name) = (session.to_string(), name.to_string());
        let registry = std::sync::Arc::clone(registry);
        std::thread::spawn(move || {
            // Located by the row, then asked about by pane: an answer that
            // is not this row's pane is no answer.
            let pane = backend
                .locate(crate::backend::Owner::new(&session, &name))
                .ok()
                .and_then(|placed| placed.agent.pane())
                .and_then(|pane| backend.pane_state(&pane).ok())
                .unwrap_or_default();
            // Classified on the worker rather than at the fold, so the argv the
            // verdict was read from — a driver's brief runs to kilobytes — never
            // crosses the channel or lands in the snapshot.
            let corroboration = crate::session::classify_foreground(
                &agent_command,
                &registry,
                pane.foreground_process.as_deref(),
                pane.foreground_command.as_deref(),
                pane.dead,
            );
            let _ = tx.send((session, corroboration));
        });
    }

    /// Take whatever the workers have answered, reporting whether any verdict
    /// actually moved — which is what makes the derived rows stale.
    pub(super) fn drain(&mut self) -> bool {
        let Some((_, rx)) = &self.channel else {
            return false;
        };
        let mut moved = false;
        while let Ok((session, corroboration)) = rx.try_recv() {
            self.inflight.remove(&session);
            let changed = self
                .known
                .get(&session)
                .is_none_or(|probe| probe.corroboration != corroboration);
            self.known.insert(
                session,
                Probe {
                    corroboration,
                    at: Instant::now(),
                },
            );
            moved |= changed;
        }
        moved
    }

    pub(super) fn get(&self, session: &str) -> Option<&Corroboration> {
        self.known.get(session).map(|probe| &probe.corroboration)
    }

    /// Forget every session this poll is not currently asking about.
    ///
    /// Hygiene rather than the correctness guard: this cache tracking a set it
    /// has stopped asking about would keep answering a question nobody put to
    /// it, and re-requesting one that never left would waste a subprocess.
    /// What stops a stale verdict from *publishing* is `assess`'s own gate on
    /// `hook.state`, which does not depend on this eviction's timing relative
    /// to a refresh.
    pub(super) fn retain(&mut self, probed: &std::collections::HashSet<&str>) {
        self.known.retain(|id, _| probed.contains(id.as_str()));
    }
}
