//! Companion shells: the stream `fleet shell` names, opened on a worker and
//! installed on return.
//!
//! The shell lives with the session's runtime, on the Task's place, not with
//! this interface: a second open, from this interface or the next one, finds
//! the same shell, and Fleet's release of the Task closes it. So nothing here
//! records a shell or closes one.

use std::sync::Arc;

use super::{shell_surface, Painted, Snapshot, Terminals};

/// A shell worker's accepted answer for the coordinator to report.
#[derive(Debug)]
pub enum ShellResult {
    Ready { session: String },
    Failed { session: String, error: String },
}

/// What a shell worker opened: the shell, and the stream's way up that its
/// input and resize take.
type Opened = (
    crate::backend::pane::ShellPane,
    Arc<dyn crate::backend::SessionBackend>,
);

pub(super) struct Shelled {
    session: String,
    request: Arc<()>,
    owner: Arc<std::sync::Mutex<crate::backend::SessionParser>>,
    shell: Result<Opened, String>,
}

pub(super) struct Shelling {
    request: Arc<()>,
    owner: Arc<std::sync::Mutex<crate::backend::SessionParser>>,
}

impl Terminals {
    /// Open `row`'s companion shell, through `fleet shell` on the row's
    /// Fleet. A shell already open, or one being opened for the same
    /// attachment, is left as it is; one whose stream ended is opened again.
    pub fn open_shell(
        &mut self,
        row: &crate::kernel::snapshot::SessionRow,
        rows: u16,
        cols: u16,
    ) -> Result<(), String> {
        let live = self
            .live
            .get(&row.id)
            .ok_or("this session has no live pane to attach a shell to")?;
        if live
            .session
            .shell_pane
            .as_ref()
            .is_some_and(|shell| !shell.has_exited())
            || self
                .shelling
                .get(&row.id)
                .is_some_and(|pending| Arc::ptr_eq(&pending.owner, &live.session.parser))
        {
            return Ok(());
        }
        let link = row
            .fleet()
            .and_then(|fleet| self.fleet.get(fleet))
            .cloned()
            .ok_or("this session's Fleet is not listed")?;
        let task = row.task().to_string();
        let scrollback = self.scrollback;
        let notices = super::stream_pane::Notices::new(
            self.input_notices.0.clone(),
            &format!("{} shell", row.name),
        );
        self.start_shell(&row.id, move || {
            super::fleet::open_shell(&link, &task, rows, cols, scrollback, notices)
        });
        Ok(())
    }

    /// Run `work`, which opens `session`'s shell, on a worker. Its answer is
    /// installed only while the session is still attached as it is now.
    pub(super) fn start_shell(
        &mut self,
        session: &str,
        work: impl FnOnce() -> Result<Opened, String> + Send + 'static,
    ) {
        let owner = Arc::clone(&self.live[session].session.parser);
        let session = session.to_string();
        let request = Arc::new(());
        self.shelling.insert(
            session.clone(),
            Shelling {
                request: Arc::clone(&request),
                owner: Arc::clone(&owner),
            },
        );
        let tx = self.shelled.0.clone();
        let runtime = self.runtime.clone();
        std::thread::spawn(move || {
            // The stream's reader and writer run on the process's runtime.
            let _guard = runtime.as_ref().map(|handle| handle.enter());
            let _ = tx.send(Shelled {
                session,
                request,
                owner,
                shell: work(),
            });
        });
    }

    pub(super) fn collect_shells(&mut self, snapshot: &Snapshot) {
        while let Ok(done) = self.shelled.1.try_recv() {
            let current = self
                .shelling
                .get(&done.session)
                .is_some_and(|pending| Arc::ptr_eq(&pending.request, &done.request));
            if !current {
                continue;
            }
            self.shelling.remove(&done.session);
            let wanted = snapshot.session(&done.session).is_some()
                && self
                    .live
                    .get(&done.session)
                    .is_some_and(|live| Arc::ptr_eq(&live.session.parser, &done.owner));
            // An answer for an attachment that went: dropping its stream
            // detaches from the shell, which the runtime keeps.
            if !wanted {
                continue;
            }
            let result = match done.shell {
                Ok((shell, backend)) => {
                    let live = self.live.get_mut(&done.session).unwrap();
                    live.session.install_shell_over(shell, backend);
                    live.shell = Painted::default();
                    self.rows_cache
                        .borrow_mut()
                        .remove(&shell_surface(&done.session));
                    ShellResult::Ready {
                        session: done.session,
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
    use super::{Arc, ShellResult, Snapshot, Terminals};

    fn terminals_with_probe() -> (Terminals, Arc<dyn crate::backend::SessionBackend>) {
        let mut terminals = Terminals::with_registry(
            Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let backend = Arc::clone(terminals.backends.default_backend());
        let session = crate::backend::Session::stub("probe", &backend);
        terminals.live.insert(
            "probe".into(),
            super::super::Live {
                session,
                agent: Default::default(),
                shell: Default::default(),
            },
        );
        (terminals, backend)
    }

    fn snapshot_with_probe() -> Snapshot {
        Snapshot {
            sessions: vec![super::super::tests::row("probe", "fleet", Some("%1"))],
            ..Default::default()
        }
    }

    /// An answer for an attachment that was replaced while the worker ran is
    /// not installed: the shell would belong to a session no longer shown.
    #[test]
    fn an_answer_for_a_replaced_attachment_is_dropped() {
        let (mut terminals, backend) = terminals_with_probe();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        terminals.start_shell("probe", move || {
            let _ = rx.recv();
            Err("the stream did not start".into())
        });
        // The session is attached again before the worker answers.
        terminals.live.get_mut("probe").unwrap().session =
            crate::backend::Session::stub("probe", &backend);
        tx.send(()).unwrap();
        let snapshot = snapshot_with_probe();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while terminals.shelling.contains_key("probe") && std::time::Instant::now() < deadline {
            terminals.collect_shells(&snapshot);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!terminals.shelling.contains_key("probe"));
        assert!(terminals.drain_shell_results().is_empty());
    }

    /// A failed open is reported, so the interface can say why.
    #[test]
    fn a_failed_open_is_reported() {
        let (mut terminals, _) = terminals_with_probe();
        terminals.start_shell("probe", || Err("fleet shell: no session".into()));
        let snapshot = snapshot_with_probe();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut results = Vec::new();
        while results.is_empty() && std::time::Instant::now() < deadline {
            terminals.collect_shells(&snapshot);
            results = terminals.drain_shell_results();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            matches!(
                results.as_slice(),
                [ShellResult::Failed { session, error }]
                    if session == "probe" && error.contains("no session")
            ),
            "{results:?}"
        );
    }
}
