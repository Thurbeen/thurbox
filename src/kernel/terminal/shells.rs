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
    owner: Arc<std::sync::Mutex<crate::backend::SessionParser>>,
    previous: Option<Arc<std::sync::Mutex<crate::backend::SessionParser>>>,
    seen: Option<String>,
    readopting: bool,
    backend: Arc<dyn crate::backend::SessionBackend>,
    shell: Result<crate::backend::pane::ShellPane, String>,
}

impl Shelled {
    fn discard(self, runtime: Option<tokio::runtime::Handle>) {
        // Only a newly created window belongs to this request. An adopted
        // window may still be in use by another interface.
        if !self.readopting {
            if let Ok(shell) = self.shell {
                std::thread::spawn(move || {
                    let _guard = runtime.as_ref().map(|handle| handle.enter());
                    if let Err(error) = self.backend.kill(&shell.backend_id) {
                        tracing::debug!("could not remove an unwanted shell: {error}");
                    }
                });
            }
        }
    }
}

impl Terminals {
    /// Queue a companion shell in the session's launch directory. Repeated
    /// commands share the in-flight request until its answer is collected.
    /// `seen` is the persisted shell id at the time the command was decided.
    pub fn open_shell(
        &mut self,
        session: &str,
        rows: u16,
        cols: u16,
        cwd: Option<&std::path::Path>,
        seen: Option<&str>,
    ) -> Result<(), String> {
        if let Some(open_requested) = self.shelling.get_mut(session) {
            *open_requested = true;
            return Ok(());
        }
        let live = self
            .live
            .get(session)
            .ok_or("this session has no live pane to attach a shell to")?;
        let Some(open) = live.session.shell_open_task(rows, cols, cwd) else {
            return Ok(());
        };
        self.start_shell(session, seen.map(str::to_string), false, open);
        Ok(())
    }

    /// Queue re-adoption of a persisted shell. Its liveness is checked on the
    /// worker, and a missing pane is forgotten only after that worker answers.
    pub fn readopt_shell(&mut self, session: &str, pane: &str, rows: u16, cols: u16) {
        if self.shelling.contains_key(session) || self.has_shell(session) {
            return;
        }
        let Some(live) = self.live.get(session) else {
            return;
        };
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
        self.shelling.insert(session.clone(), !readopting);
        let tx = self.shelled.0.clone();
        let runtime = self.runtime.clone();
        std::thread::spawn(move || {
            let _guard = runtime.as_ref().map(|handle| handle.enter());
            let shell = work().map_err(|error| error.to_string());
            let done = Shelled {
                session,
                owner,
                previous,
                seen,
                readopting,
                backend,
                shell,
            };
            if let Err(error) = tx.send(done) {
                error.0.discard(runtime.clone());
            }
        });
    }

    pub(super) fn collect_shells(&mut self, snapshot: &Snapshot) {
        while let Ok(done) = self.shelled.1.try_recv() {
            let open_requested = self.shelling.remove(&done.session).unwrap_or(false);
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
                let recorded = done.shell.as_ref().ok().is_some_and(|shell| {
                    snapshot.session(&done.session).is_some_and(|row| {
                        row.shell_backend_id.as_deref() == Some(shell.backend_id.as_str())
                    }) || self.shell_pane_id(&done.session).as_deref()
                        == Some(shell.backend_id.as_str())
                });
                if !recorded {
                    done.discard(self.runtime.clone());
                }
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
                owner: Arc::clone(&session.parser),
                previous: None,
                seen: Some("%2".into()),
                readopting: true,
                backend: Arc::clone(&backend),
                shell: Err("old shell went away".into()),
            };
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
}
