//! Companion shell round trips, prepared on the loop and installed on return.

use std::sync::Arc;

use super::{shell_surface, Painted, Snapshot, Terminals};

/// A shell worker's accepted answer for the coordinator to persist or report.
pub enum ShellResult {
    Ready {
        session: String,
        pane: String,
        /// What its window is stamped with.
        stamp: String,
    },
    Gone {
        session: String,
        open_requested: bool,
        /// The pane found gone.
        pane: Option<String>,
    },
    Failed {
        session: String,
        error: String,
    },
}

pub(super) struct Shelled {
    session: String,
    request: Arc<()>,
    owner: Arc<std::sync::Mutex<crate::backend::SessionParser>>,
    previous: Option<Arc<std::sync::Mutex<crate::backend::SessionParser>>>,
    seen: Option<String>,
    stamp: String,
    readopting: bool,
    backend: Arc<dyn crate::backend::SessionBackend>,
    shell: Result<crate::backend::pane::ShellPane, String>,
}

pub(super) struct Shelling {
    request: Arc<()>,
    seen: Option<String>,
    owner: Arc<std::sync::Mutex<crate::backend::SessionParser>>,
    open_requested: bool,
}

impl Terminals {
    /// Say which shell pane `session`'s record now names, or that it names
    /// none: a worker's answer is accepted only against the record as the
    /// loop holds it.
    pub fn record_shell(&mut self, session: &str, pane: Option<String>) {
        match pane {
            Some(pane) => self.recorded_shells.insert(session.to_string(), pane),
            None => self.recorded_shells.remove(session),
        };
    }

    /// The route of the multiplexer a Fleet session's companion shell runs
    /// on: this machine's own for a local Task, else the route of the
    /// `hosts.toml` host its Fleet host is named as, on that host's own
    /// multiplexer, so a WSL host keeps its `wsl:` place. `None` when no such
    /// host is configured.
    pub fn shell_route(&self, remote_host: Option<&str>) -> Option<crate::session::Route> {
        Some(match remote_host {
            None => crate::session::Route::local(crate::session::Multiplexer::platform_default()),
            Some(host) => self.hosts.get(host)?.default_route(),
        })
    }

    /// The route a saved shell is re-adopted on: the route it was saved on,
    /// while that is still the session's machine and a backend serves it.
    /// Its window's stamp still decides whether it is this session's shell;
    /// the host's current multiplexer is only for a new shell.
    pub fn readopt_route(
        &self,
        remote_host: Option<&str>,
        saved: &str,
    ) -> Option<crate::session::Route> {
        let current = self.shell_route(remote_host)?;
        let saved = crate::session::Route::parse(saved).ok()?;
        (saved.same_machine(&current) && self.backend_on(&saved).is_some()).then_some(saved)
    }

    /// Run `session`'s companion shell on `route`'s backend: the saved route
    /// while its shell is re-adopted, the current one for a new shell.
    pub fn use_shell_route(&mut self, session: &str, route: &crate::session::Route) {
        let Some(backend) = self.backend_on(route) else {
            return;
        };
        if let Some(live) = self.live.get_mut(session) {
            live.session.set_shell_backend(backend);
        }
    }

    /// The backend registered for `route`, if any.
    pub(super) fn backend_on(
        &self,
        route: &crate::session::Route,
    ) -> Option<Arc<dyn crate::backend::SessionBackend>> {
        self.backends.get(route).cloned()
    }

    /// Kill a shell window a deleted session left, on a worker: only where
    /// the window on `route` holding `pane` is still the shell stamped
    /// `stamp`, so a reissued pane id never takes an unrelated window with it.
    pub fn kill_shell_window(&self, route: &str, pane: &str, stamp: &str, name: &str) {
        let Some(backend) = crate::session::Route::parse(route)
            .ok()
            .and_then(|route| self.backend_on(&route))
        else {
            return;
        };
        let (pane, stamp, name) = (pane.to_string(), stamp.to_string(), name.to_string());
        std::thread::spawn(move || {
            let placed = backend.locate(crate::backend::Owner {
                session_id: &stamp,
                name: &name,
                agent_pane: "",
                shell_pane: &pane,
            });
            if placed.is_ok_and(|placed| placed.shell == crate::backend::Located::At(pane.clone()))
            {
                if let Err(e) = backend.kill(&pane) {
                    tracing::warn!("could not close the shell of a deleted session: {e}");
                }
            }
        });
    }

    /// Queue a companion shell in the session's launch directory. Repeated
    /// commands on the same attachment and observed shell slot share a request.
    /// `seen` is the persisted shell id at the time the command was decided.
    pub fn open_shell(
        &mut self,
        session: &str,
        rows: u16,
        cols: u16,
        cwd: Option<&std::path::Path>,
        seen: Option<&str>,
    ) -> Result<(), String> {
        let live = self
            .live
            .get(session)
            .ok_or("this session has no live pane to attach a shell to")?;
        if let Some(pending) = self.shelling.get_mut(session).filter(|pending| {
            Arc::ptr_eq(&pending.owner, &live.session.parser) && pending.seen.as_deref() == seen
        }) {
            pending.open_requested = true;
            return Ok(());
        }
        let Some(open) = live.session.shell_open_task(rows, cols, cwd) else {
            return Ok(());
        };
        // A new shell's window is stamped with the session's id as this
        // attachment knows it.
        let stamp = live.session.info.id.to_string();
        self.start_shell(session, seen.map(str::to_string), stamp, false, open);
        Ok(())
    }

    /// Queue re-adoption of a persisted shell, whose window was stamped
    /// `stamp`. Its identity is checked on the worker, and a missing pane is
    /// forgotten only after that worker answers.
    pub fn readopt_shell(&mut self, session: &str, pane: &str, stamp: &str, rows: u16, cols: u16) {
        let Some(live) = self.live.get(session) else {
            return;
        };
        if self.has_shell(session)
            || self.shelling.get(session).is_some_and(|pending| {
                Arc::ptr_eq(&pending.owner, &live.session.parser)
                    && pending.seen.as_deref() == Some(pane)
            })
        {
            return;
        }
        let adopt = live.session.shell_adopt_task(pane, stamp, rows, cols);
        self.start_shell(
            session,
            Some(pane.to_string()),
            stamp.to_string(),
            true,
            adopt,
        );
    }

    fn start_shell(
        &mut self,
        session: &str,
        seen: Option<String>,
        stamp: String,
        readopting: bool,
        work: impl FnOnce() -> anyhow::Result<crate::backend::pane::ShellPane> + Send + 'static,
    ) {
        let live = &self.live[session];
        let owner = Arc::clone(&live.session.parser);
        let previous = live
            .session
            .shell_pane
            .as_ref()
            .map(|shell| Arc::clone(&shell.parser));
        let (backend, _) = live.session.backend_handle();
        let session = session.to_string();
        let request = Arc::new(());
        self.shelling.insert(
            session.clone(),
            Shelling {
                request: Arc::clone(&request),
                seen: seen.clone(),
                owner: Arc::clone(&owner),
                open_requested: !readopting,
            },
        );
        let tx = self.shelled.0.clone();
        let runtime = self.runtime.clone();
        std::thread::spawn(move || {
            let _guard = runtime.as_ref().map(|handle| handle.enter());
            let shell = work().map_err(|error| error.to_string());
            let done = Shelled {
                session,
                request,
                owner,
                previous,
                seen,
                stamp,
                readopting,
                backend,
                shell,
            };
            let _ = tx.send(done);
        });
    }

    pub(super) fn collect_shells(&mut self, snapshot: &Snapshot) {
        while let Ok(done) = self.shelled.1.try_recv() {
            let Some(pending) = self.shelling.get(&done.session) else {
                continue;
            };
            if !Arc::ptr_eq(&pending.request, &done.request) {
                continue;
            }
            let open_requested = self.shelling.remove(&done.session).unwrap().open_requested;
            let wanted = snapshot.session(&done.session).is_some_and(|row| {
                self.recorded_shells.get(&done.session) == done.seen.as_ref()
                    && self.key(&row.backend) == self.key(done.backend.name())
            }) && self.live.get(&done.session).is_some_and(|live| {
                Arc::ptr_eq(&live.session.parser, &done.owner)
                    && match (&done.previous, &live.session.shell_pane) {
                        (None, None) => true,
                        (Some(previous), Some(shell)) => Arc::ptr_eq(previous, &shell.parser),
                        _ => false,
                    }
            });
            if !wanted {
                // Pane ids can be reused after a server restart. Without an atomic
                // creation identity, an obsolete reply cannot safely kill its window.
                continue;
            }
            let result = match done.shell {
                Ok(shell) => {
                    let pane = shell.backend_id().to_string();
                    let live = self.live.get_mut(&done.session).unwrap();
                    live.session.install_shell(shell);
                    live.shell = Painted::default();
                    self.rows_cache
                        .borrow_mut()
                        .remove(&shell_surface(&done.session));
                    ShellResult::Ready {
                        session: done.session,
                        pane,
                        stamp: done.stamp,
                    }
                }
                Err(error) if done.readopting => {
                    tracing::debug!("could not re-adopt shell: {error}");
                    ShellResult::Gone {
                        session: done.session,
                        open_requested,
                        pane: done.seen,
                    }
                }
                Err(error) => ShellResult::Failed {
                    session: done.session,
                    error,
                },
            };
            self.shell_results.push(result);
        }
    }

    /// Take accepted worker answers once, after sync has folded them in.
    pub fn drain_shell_results(&mut self) -> Vec<ShellResult> {
        std::mem::take(&mut self.shell_results)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Live;
    use super::{Arc, Painted, ShellResult, Shelled, Shelling, Snapshot, Terminals};

    /// A Fleet host named as a `hosts.toml` WSL distro keeps its `wsl:`
    /// place: the shell runs on that distro's multiplexer, not on an ssh host
    /// of the same name.
    #[test]
    fn a_shell_route_takes_its_place_from_the_host() {
        let mut terminals = Terminals::with_registry(
            Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.hosts = crate::session::HostRegistry {
            config_version: Some(1),
            hosts: vec![crate::session::HostDef::wsl("ubuntu")],
        };
        let route = terminals.shell_route(Some("ubuntu")).expect("a route");
        assert!(
            route.format().starts_with("wsl:ubuntu"),
            "{}",
            route.format()
        );
        assert!(terminals.shell_route(Some("nowhere")).is_none());
        assert!(terminals.shell_route(None).expect("local").is_local());
    }

    /// A host whose multiplexer changed keeps the shell it already had: the
    /// shell saved on `ssh:box:tmux` is re-adopted there, not left running
    /// beside a new one on rmux.
    #[test]
    fn a_saved_shell_is_readopted_on_its_own_route_after_the_host_changes_multiplexer() {
        use crate::session::{HostDef, HostRegistry, Multiplexer, Route, Via};
        let mut terminals = Terminals::with_registry(
            Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let saved = Route::remote(Via::Ssh, "box", Multiplexer::Tmux);
        let backend = Arc::clone(terminals.backends.default_backend());
        Arc::get_mut(&mut terminals.backends)
            .expect("unshared registry")
            .register(saved.clone(), backend);
        terminals.hosts = HostRegistry {
            config_version: Some(1),
            hosts: vec![HostDef {
                name: "box".into(),
                multiplexer: Some("rmux".into()),
                ..Default::default()
            }],
        };
        assert_eq!(
            terminals.shell_route(Some("box")),
            Some(Route::remote(Via::Ssh, "box", Multiplexer::Rmux)),
            "a new shell takes the host's multiplexer"
        );
        assert_eq!(
            terminals.readopt_route(Some("box"), &saved.format()),
            Some(saved.clone())
        );
        // Another machine's shell is not this session's companion any more.
        assert_eq!(
            terminals.readopt_route(Some("other"), &saved.format()),
            None
        );
        assert_eq!(terminals.readopt_route(None, &saved.format()), None);
    }

    #[test]
    fn a_late_failed_readoption_cannot_forget_a_newer_shell() {
        for change in ["unchanged", "deleted", "reattached", "shell", "route"] {
            let mut terminals = Terminals::with_registry(
                Arc::new(crate::backend::registry::inert()),
                &crate::session::settings::Settings::default(),
            );
            let backend = Arc::clone(terminals.backends.default_backend());
            let session = crate::backend::Session::stub("probe", &backend);
            let done = Shelled {
                session: "probe".into(),
                request: Arc::new(()),
                owner: Arc::clone(&session.parser),
                previous: None,
                seen: Some("%2".into()),
                stamp: String::new(),
                readopting: true,
                backend: Arc::clone(&backend),
                shell: Err("old shell went away".into()),
            };
            terminals.shelling.insert(
                "probe".into(),
                Shelling {
                    request: Arc::clone(&done.request),
                    seen: done.seen.clone(),
                    owner: Arc::clone(&done.owner),
                    open_requested: false,
                },
            );
            terminals.live.insert(
                "probe".into(),
                Live {
                    session,
                    agent: Painted::default(),
                    shell: Painted::default(),
                },
            );
            let row = super::super::tests::row("probe", backend.name(), None);
            terminals.record_shell("probe", Some("%2".into()));
            let mut snapshot = Snapshot {
                sessions: vec![row],
                ..Default::default()
            };
            match change {
                "deleted" => snapshot.sessions.clear(),
                "reattached" => {
                    terminals.live.get_mut("probe").unwrap().session =
                        crate::backend::Session::stub("probe", &backend);
                }
                "shell" => terminals.record_shell("probe", Some("%3".into())),
                "route" => snapshot.sessions[0].backend = "ssh:another:tmux".into(),
                _ => {}
            }
            assert!(terminals.shelled.0.send(done).is_ok());
            terminals.collect_shells(&snapshot);
            let results = terminals.drain_shell_results();
            if change == "unchanged" {
                assert!(
                    matches!(results.as_slice(), [ShellResult::Gone { session, .. }] if session == "probe")
                );
            } else {
                assert!(results.is_empty(), "{change} must discard the old failure");
            }
            assert!(
                terminals.drain_shell_results().is_empty(),
                "results are consumed once"
            );
        }
    }
    #[test]
    fn an_old_reply_cannot_clear_a_new_attachments_pending_request() {
        let mut terminals = Terminals::with_registry(
            Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let backend = Arc::clone(terminals.backends.default_backend());
        let old = crate::backend::Session::stub("probe", &backend);
        let done = Shelled {
            session: "probe".into(),
            request: Arc::new(()),
            owner: Arc::clone(&old.parser),
            previous: None,
            seen: None,
            stamp: String::new(),
            readopting: false,
            backend: Arc::clone(&backend),
            shell: Err("old request".into()),
        };
        terminals.live.insert(
            "probe".into(),
            Live {
                session: crate::backend::Session::stub("probe", &backend),
                agent: Painted::default(),
                shell: Painted::default(),
            },
        );
        let (release, held) = std::sync::mpsc::channel::<()>();
        terminals.start_shell("probe", None, String::new(), false, move || {
            let _ = held.recv();
            anyhow::bail!("released request")
        });
        terminals.shelled.0.send(done).ok().unwrap();
        let snapshot = Snapshot {
            sessions: vec![super::super::tests::row("probe", backend.name(), None)],
            ..Default::default()
        };
        terminals.collect_shells(&snapshot);
        assert!(
            terminals.shelling.contains_key("probe"),
            "the old reply removed a newer request"
        );
        assert!(terminals.drain_shell_results().is_empty());
        drop(release);
    }

    #[test]
    fn a_changed_attachment_or_shell_slot_can_start_its_own_request() {
        for (readopting, reattached) in [(false, true), (true, true), (true, false)] {
            let mut terminals = Terminals::with_registry(
                Arc::new(crate::backend::registry::inert()),
                &crate::session::settings::Settings::default(),
            );
            let backend = Arc::clone(terminals.backends.default_backend());
            terminals.live.insert(
                "probe".into(),
                Live {
                    session: crate::backend::Session::stub("probe", &backend),
                    agent: Painted::default(),
                    shell: Painted::default(),
                },
            );
            let (release, held) = std::sync::mpsc::channel::<()>();
            terminals.start_shell(
                "probe",
                readopting.then(|| "%2".into()),
                String::new(),
                readopting,
                move || {
                    let _ = held.recv();
                    anyhow::bail!("old request released")
                },
            );
            if reattached {
                terminals.live.get_mut("probe").unwrap().session =
                    crate::backend::Session::stub("probe", &backend);
            }
            let row = super::super::tests::row("probe", backend.name(), None);
            if readopting {
                let pane = if reattached { "%2" } else { "%3" };
                terminals.record_shell("probe", Some(pane.into()));
                terminals.readopt_shell("probe", pane, "", 24, 80);
            } else {
                terminals.open_shell("probe", 24, 80, None, None).unwrap();
            }
            let snapshot = Snapshot {
                sessions: vec![row],
                ..Default::default()
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let result = loop {
                terminals.collect_shells(&snapshot);
                let result = terminals.drain_shell_results();
                if !result.is_empty() || std::time::Instant::now() >= deadline {
                    break result;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            };
            assert!(
                matches!(result.as_slice(), [ShellResult::Gone { .. }]) && readopting
                    || matches!(result.as_slice(), [ShellResult::Failed { .. }]) && !readopting,
                "the replacement's request was swallowed by the old open (readopting={readopting}, reattached={reattached})"
            );
            drop(release);
        }
    }
    #[test]
    fn clearing_the_saved_shell_does_not_swallow_a_new_open() {
        let mut terminals = Terminals::with_registry(
            Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let backend = Arc::clone(terminals.backends.default_backend());
        terminals.live.insert(
            "probe".into(),
            Live {
                session: crate::backend::Session::stub("probe", &backend),
                agent: Painted::default(),
                shell: Painted::default(),
            },
        );
        let (release, held) = std::sync::mpsc::channel::<()>();
        terminals.start_shell("probe", Some("%2".into()), String::new(), true, move || {
            let _ = held.recv();
            anyhow::bail!("old request released")
        });
        terminals.open_shell("probe", 24, 80, None, None).unwrap();
        let snapshot = Snapshot {
            sessions: vec![super::super::tests::row("probe", backend.name(), None)],
            ..Default::default()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let result = loop {
            terminals.collect_shells(&snapshot);
            let result = terminals.drain_shell_results();
            if !result.is_empty() || std::time::Instant::now() >= deadline {
                break result;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(
            matches!(result.as_slice(), [ShellResult::Failed { .. }]),
            "the cleared slot's new open was swallowed by obsolete adoption"
        );
        drop(release);
    }

    #[test]
    fn a_same_attachment_old_reply_cannot_clear_a_new_slots_request() {
        let mut terminals = Terminals::with_registry(
            Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let backend = Arc::clone(terminals.backends.default_backend());
        let session = crate::backend::Session::stub("probe", &backend);
        let done = Shelled {
            session: "probe".into(),
            request: Arc::new(()),
            owner: Arc::clone(&session.parser),
            previous: None,
            seen: Some("%2".into()),
            stamp: String::new(),
            readopting: true,
            backend: Arc::clone(&backend),
            shell: Err("obsolete adoption".into()),
        };
        terminals.live.insert(
            "probe".into(),
            Live {
                session,
                agent: Painted::default(),
                shell: Painted::default(),
            },
        );
        let (release, held) = std::sync::mpsc::channel::<()>();
        terminals.start_shell("probe", None, String::new(), false, move || {
            let _ = held.recv();
            anyhow::bail!("new request released")
        });
        terminals.shelled.0.send(done).ok().unwrap();
        terminals.collect_shells(&Snapshot {
            sessions: vec![super::super::tests::row("probe", backend.name(), None)],
            ..Default::default()
        });
        assert!(
            terminals.shelling.contains_key("probe"),
            "the old slot's reply removed a newer request"
        );
        assert!(terminals.drain_shell_results().is_empty());
        drop(release);
    }
    #[test]
    fn an_obsolete_token_cannot_publish_even_when_the_saved_shell_matches_again() {
        let mut terminals = Terminals::with_registry(
            Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let backend = Arc::clone(terminals.backends.default_backend());
        let session = crate::backend::Session::stub("probe", &backend);
        let done = Shelled {
            session: "probe".into(),
            request: Arc::new(()),
            owner: Arc::clone(&session.parser),
            previous: None,
            seen: Some("%2".into()),
            stamp: String::new(),
            readopting: true,
            backend: Arc::clone(&backend),
            shell: Err("obsolete adoption".into()),
        };
        terminals.live.insert(
            "probe".into(),
            Live {
                session,
                agent: Painted::default(),
                shell: Painted::default(),
            },
        );
        let (release, held) = std::sync::mpsc::channel::<()>();
        terminals.start_shell("probe", Some("%2".into()), String::new(), true, move || {
            let _ = held.recv();
            anyhow::bail!("current request released")
        });
        terminals.shelled.0.send(done).ok().unwrap();
        let row = super::super::tests::row("probe", backend.name(), None);
        terminals.record_shell("probe", Some("%2".into()));
        terminals.collect_shells(&Snapshot {
            sessions: vec![row],
            ..Default::default()
        });
        assert!(terminals.shelling.contains_key("probe"));
        assert!(
            terminals.drain_shell_results().is_empty(),
            "an obsolete job's failure would forget the current saved shell"
        );
        drop(release);
    }
}
