use std::sync::Arc;

use super::{shell_surface, Painted, Snapshot, Terminals};

#[derive(Debug)]
pub enum ShellResult {
    Ready { session: String },
    Failed { session: String, error: String },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ShellOpening {
    Opening,
    AfterStart,
}

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
    pub fn open_shell(
        &mut self,
        row: &crate::kernel::snapshot::SessionRow,
        rows: u16,
        cols: u16,
    ) -> Result<ShellOpening, String> {
        let Some(live) = self.live.get(&row.id) else {
            if row.stopped {
                let waiting = self
                    .shells_after_start
                    .insert(row.id.clone(), (rows, cols))
                    .is_some();
                return Ok(if waiting {
                    ShellOpening::Opening
                } else {
                    ShellOpening::AfterStart
                });
            }
            return Err("this session has no live pane to attach a shell to".into());
        };
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
            return Ok(ShellOpening::Opening);
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
        Ok(ShellOpening::Opening)
    }

    pub(super) fn open_shells_after_start(&mut self, snapshot: &Snapshot) {
        let wanted: Vec<(String, (u16, u16))> = self.shells_after_start.drain().collect();
        for (session, (rows, cols)) in wanted {
            let Some(row) = snapshot.session(&session) else {
                continue;
            };
            if self.live.contains_key(&session) {
                if let Err(error) = self.open_shell(row, rows, cols) {
                    self.shell_results
                        .push(ShellResult::Failed { session, error });
                }
                continue;
            }
            if matches!(
                self.attach.starts.get(&session),
                Some((super::Start::Pending, _))
            ) {
                self.shells_after_start.insert(session, (rows, cols));
            }
        }
    }

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

    #[test]
    fn a_shell_waiting_on_a_start_that_failed_is_given_up() {
        let (mut terminals, _) = terminals_with_probe();
        terminals.live.remove("probe");
        let mut row = super::super::tests::row("probe", "fleet", Some("%1"));
        row.stopped = true;
        assert_eq!(
            terminals.open_shell(&row, 24, 80),
            Ok(super::ShellOpening::AfterStart)
        );
        assert_eq!(
            terminals.open_shell(&row, 24, 80),
            Ok(super::ShellOpening::Opening)
        );
        let snapshot = Snapshot {
            sessions: vec![row],
            ..Default::default()
        };
        terminals.start_requested("probe");
        terminals.open_shells_after_start(&snapshot);
        assert!(terminals.shells_after_start.contains_key("probe"));

        terminals.start_failed("probe", "the lease is held elsewhere");
        terminals.open_shells_after_start(&snapshot);
        assert!(terminals.shells_after_start.is_empty());
    }

    fn shell_over(
        backend: &Arc<dyn crate::backend::SessionBackend>,
        rest: crate::backend::PaneOutput,
    ) -> crate::backend::pane::ShellPane {
        let blank = crate::backend::contract::PaneSnapshot {
            cols: 80,
            rows: 24,
            cursor: (0, 0),
            normal: Vec::new(),
            alternate: None,
        };
        let adopted = crate::backend::AdoptedSession {
            output: Box::new(
                std::iter::once(crate::backend::PaneChunk::Snapshot(Box::new(blank))).chain(rest),
            ),
            input: Box::new(std::io::sink()),
            seed_len: 0,
            size: None,
        };
        crate::backend::Session::shell_over(adopted, backend, "probe#shell", 24, 80, 100)
    }

    struct Open(std::sync::mpsc::Receiver<()>);

    impl Iterator for Open {
        type Item = crate::backend::PaneChunk;
        fn next(&mut self) -> Option<Self::Item> {
            let _ = self.0.recv();
            None
        }
    }

    fn collect_until(terminals: &mut Terminals, done: impl Fn(&Terminals) -> bool) {
        let snapshot = snapshot_with_probe();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done(terminals) && std::time::Instant::now() < deadline {
            terminals.collect_shells(&snapshot);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(done(terminals), "never happened");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_shell_whose_stream_ended_is_opened_again() {
        let (mut terminals, backend) = terminals_with_probe();
        let row = super::super::tests::row("probe", "fleet", Some("%1"));
        let parser_of = |terminals: &Terminals| {
            terminals.live["probe"]
                .session
                .shell_pane
                .as_ref()
                .map(|shell| Arc::clone(&shell.parser))
        };

        // A live shell.
        let (keep_open, open) = std::sync::mpsc::channel::<()>();
        let first = Arc::clone(&backend);
        terminals.start_shell("probe", move || {
            Ok((shell_over(&first, Box::new(Open(open))), first))
        });
        collect_until(&mut terminals, |t| t.has_shell("probe"));
        let live = parser_of(&terminals).expect("the first shell");
        terminals
            .open_shell(&row, 24, 80)
            .expect("a live shell is left as it is");
        assert!(
            !terminals.shelling.contains_key("probe"),
            "no second open while the shell lives"
        );

        // Its stream ends: the shell exited, and the next open asks for one
        // again (here refused, since the row names no listed Fleet).
        drop(keep_open);
        collect_until(&mut terminals, |t| {
            t.live["probe"]
                .session
                .shell_pane
                .as_ref()
                .is_some_and(|shell| shell.has_exited())
        });
        assert!(
            terminals
                .open_shell(&row, 24, 80)
                .is_err_and(|e| e.contains("Fleet")),
            "an ended shell is opened again"
        );

        // The new shell replaces the ended one.
        let second = Arc::clone(&backend);
        terminals.start_shell("probe", move || {
            Ok((shell_over(&second, Box::new(std::iter::empty())), second))
        });
        collect_until(&mut terminals, |t| {
            parser_of(t).is_some_and(|parser| !Arc::ptr_eq(&parser, &live))
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_shell_over_a_stream_takes_its_first_snapshot_as_its_screen() {
        let (_, backend) = terminals_with_probe();
        let snapshot = crate::backend::contract::PaneSnapshot {
            cols: 40,
            rows: 4,
            cursor: (0, 1),
            normal: vec!["kept-text".into()],
            alternate: None,
        };
        let adopted = crate::backend::AdoptedSession {
            output: Box::new(std::iter::once(crate::backend::PaneChunk::Snapshot(
                Box::new(snapshot),
            ))),
            input: Box::new(std::io::sink()),
            seed_len: 0,
            size: None,
        };
        let shell = crate::backend::Session::shell_over(adopted, &backend, "t#shell", 4, 40, 100);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut text = String::new();
        while std::time::Instant::now() < deadline {
            text = shell.parser.lock().unwrap().screen().contents();
            if text.contains("kept-text") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(text.contains("kept-text"), "the screen: {text:?}");
    }

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
