//! Companion shell round trips, prepared on the loop and installed on return.

use super::*;

/// A shell worker's accepted answer for the coordinator to persist or report.
pub enum ShellResult {
    Ready {
        session: String,
        pane: String,
    },
    Gone {
        session: String,
        open_requested: bool,
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
        self.start_shell(session, seen.map(str::to_string), false, open);
        Ok(())
    }

    /// Queue re-adoption of a persisted shell. Its liveness is checked on the
    /// worker, and a missing pane is forgotten only after that worker answers.
    pub fn readopt_shell(&mut self, session: &str, pane: &str, rows: u16, cols: u16) {
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
        let adopt = live.session.shell_adopt_task(pane, rows, cols);
        self.start_shell(session, Some(pane.to_string()), true, adopt);
    }

    fn start_shell(
        &mut self,
        session: &str,
        seen: Option<String>,
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
                row.shell_backend_id == done.seen
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
                    let pane = shell.backend_id.clone();
                    let live = self.live.get_mut(&done.session).unwrap();
                    live.session.install_shell(shell);
                    live.shell = Painted::default();
                    self.rows_cache
                        .borrow_mut()
                        .remove(&shell_surface(&done.session));
                    ShellResult::Ready {
                        session: done.session,
                        pane,
                    }
                }
                Err(error) if done.readopting => {
                    tracing::debug!("could not re-adopt shell: {error}");
                    ShellResult::Gone {
                        session: done.session,
                        open_requested,
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
    use super::*;

    #[test]
    fn a_late_failed_readoption_cannot_forget_a_newer_shell() {
        for change in ["unchanged", "deleted", "reattached", "shell", "route"] {
            let mut terminals =
                Terminals::with_registry(Arc::new(crate::backend::registry::inert()));
            let backend = Arc::clone(terminals.backends.default_backend());
            let session = crate::backend::Session::stub("probe", &backend);
            let done = Shelled {
                session: "probe".into(),
                request: Arc::new(()),
                owner: Arc::clone(&session.parser),
                previous: None,
                seen: Some("%2".into()),
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
            let mut row = super::super::tests::row("probe", backend.name(), None);
            row.shell_backend_id = Some("%2".into());
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
                "shell" => snapshot.sessions[0].shell_backend_id = Some("%3".into()),
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
        let mut terminals = Terminals::with_registry(Arc::new(crate::backend::registry::inert()));
        let backend = Arc::clone(terminals.backends.default_backend());
        let old = crate::backend::Session::stub("probe", &backend);
        let done = Shelled {
            session: "probe".into(),
            request: Arc::new(()),
            owner: Arc::clone(&old.parser),
            previous: None,
            seen: None,
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
        terminals.start_shell("probe", None, false, move || {
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
            let mut terminals =
                Terminals::with_registry(Arc::new(crate::backend::registry::inert()));
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
            let mut row = super::super::tests::row("probe", backend.name(), None);
            if readopting {
                let pane = if reattached { "%2" } else { "%3" };
                row.shell_backend_id = Some(pane.into());
                terminals.readopt_shell("probe", pane, 24, 80);
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
        let mut terminals = Terminals::with_registry(Arc::new(crate::backend::registry::inert()));
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
        terminals.start_shell("probe", Some("%2".into()), true, move || {
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
        let mut terminals = Terminals::with_registry(Arc::new(crate::backend::registry::inert()));
        let backend = Arc::clone(terminals.backends.default_backend());
        let session = crate::backend::Session::stub("probe", &backend);
        let done = Shelled {
            session: "probe".into(),
            request: Arc::new(()),
            owner: Arc::clone(&session.parser),
            previous: None,
            seen: Some("%2".into()),
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
        terminals.start_shell("probe", None, false, move || {
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
        let mut terminals = Terminals::with_registry(Arc::new(crate::backend::registry::inert()));
        let backend = Arc::clone(terminals.backends.default_backend());
        let session = crate::backend::Session::stub("probe", &backend);
        let done = Shelled {
            session: "probe".into(),
            request: Arc::new(()),
            owner: Arc::clone(&session.parser),
            previous: None,
            seen: Some("%2".into()),
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
        terminals.start_shell("probe", Some("%2".into()), true, move || {
            let _ = held.recv();
            anyhow::bail!("current request released")
        });
        terminals.shelled.0.send(done).ok().unwrap();
        let mut row = super::super::tests::row("probe", backend.name(), None);
        row.shell_backend_id = Some("%2".into());
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
