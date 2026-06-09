//! One-shot background job handle polled from `tick`.
//!
//! Grouped out of the [`App`](super::App) god object: each of the app's
//! fire-and-poll jobs (metrics refresh, git stats, worktree creation, session
//! spawn) used to carry its own `(in_progress: bool, rx: Option<Receiver<T>>)`
//! field pair plus a hand-rolled `try_recv` match. [`BackgroundTask`] folds
//! the bool into `rx.is_some()` so the two can never drift, and centralizes
//! the poll-state machine.

use std::sync::mpsc;

/// At most one job in flight, delivering a single `T` over a channel.
pub(crate) struct BackgroundTask<T> {
    rx: Option<mpsc::Receiver<T>>,
}

impl<T> Default for BackgroundTask<T> {
    fn default() -> Self {
        Self { rx: None }
    }
}

/// Outcome of a single [`BackgroundTask::poll`].
pub(crate) enum TaskPoll<T> {
    /// No job in flight, or the job hasn't delivered yet.
    Pending,
    /// The job delivered its result; the task is idle again.
    Done(T),
    /// The worker dropped its sender without delivering (e.g. it panicked);
    /// the task is idle again so the next cadence can retry.
    Died,
}

impl<T> BackgroundTask<T> {
    pub(crate) fn in_progress(&self) -> bool {
        self.rx.is_some()
    }

    /// Mark a job in flight and return the sender to hand to the worker.
    /// Callers must check [`in_progress`](Self::in_progress) first; starting
    /// over an in-flight job would orphan its receiver.
    pub(crate) fn start(&mut self) -> mpsc::Sender<T> {
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        tx
    }

    /// Non-blocking poll: returns the job's result (or its death) exactly
    /// once, clearing the in-flight state.
    pub(crate) fn poll(&mut self) -> TaskPoll<T> {
        let Some(rx) = &self.rx else {
            return TaskPoll::Pending;
        };
        match rx.try_recv() {
            Ok(value) => {
                self.rx = None;
                TaskPoll::Done(value)
            }
            Err(mpsc::TryRecvError::Empty) => TaskPoll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.rx = None;
                TaskPoll::Died
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_task_polls_pending() {
        let mut task: BackgroundTask<u8> = BackgroundTask::default();
        assert!(!task.in_progress());
        assert!(matches!(task.poll(), TaskPoll::Pending));
    }

    #[test]
    fn delivered_result_polls_done_then_idle() {
        let mut task = BackgroundTask::default();
        let tx = task.start();
        assert!(task.in_progress());
        assert!(matches!(task.poll(), TaskPoll::Pending)); // not delivered yet
        tx.send(7u8).unwrap();
        assert!(matches!(task.poll(), TaskPoll::Done(7)));
        assert!(!task.in_progress());
    }

    #[test]
    fn dropped_sender_polls_died_then_idle() {
        let mut task: BackgroundTask<u8> = BackgroundTask::default();
        drop(task.start());
        assert!(matches!(task.poll(), TaskPoll::Died));
        assert!(!task.in_progress());
        assert!(matches!(task.poll(), TaskPoll::Pending));
    }
}
