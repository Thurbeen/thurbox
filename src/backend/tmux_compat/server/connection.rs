//! The control-mode connection — opened, replaced and closed apart from
//! the [`Server`] it reaches — and the one-shot commands run without one.

use super::*;

impl<M: TmuxCompatible> Server<M> {
    /// Run a tmux command and return its stdout (used before control mode is available).
    pub(super) fn tmux_output(&self, args: &[&str]) -> Result<String> {
        let output = self.run_tmux(args)?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Run a tmux command, returning Ok(()) on success (used before control mode is available).
    pub(super) fn tmux_run(&self, args: &[&str]) -> Result<()> {
        self.run_tmux(args)?;
        Ok(())
    }

    /// A fresh backend on the same server, with no connection of its own yet —
    /// for work that opens control mode and should not leave it open on the
    /// backend the rest of the process shares.
    pub(super) fn transient(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            socket: self.socket.clone(),
            session: self.session.clone(),
            name: self.name.clone(),
            link: Connection::new(),
            closed: std::sync::atomic::AtomicBool::new(self.is_closed()),
            history_lines: self.history_lines,
            host: self.host.clone(),
            platform: self.platform,
            multiplexer: PhantomData,
        }
    }

    /// Whether [`WindowLifecycle::shutdown`] has run.
    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Refuse to open a connection once shut down.
    pub(super) fn refuse_if_closed(&self) -> Result<()> {
        if self.is_closed() {
            bail!("{} is shut down", self.name);
        }
        Ok(())
    }

    /// Whether this backend holds a control-mode connection, which is what
    /// decides whether a command goes through it or runs one-shot.
    pub(in crate::backend) fn attached(&self) -> bool {
        self.link
            .control
            .lock()
            .is_ok_and(|control| control.is_some())
    }

    /// Run a one-shot command whose failure is reported as `what` rather than
    /// by its argv: a paste's argv is the text being pasted.
    ///
    /// `output()` rather than `status()` is the point of it: a `status()` child
    /// inherits this process's stderr, so the multiplexer's own `can't find
    /// pane` would land there directly — a second, unstructured stream beside
    /// the error document the CLI puts on stdout. Captured, it becomes part of
    /// the one answer.
    pub(super) fn one_shot(&self, what: &str, args: &[&str]) -> Result<std::process::Output> {
        let output = self
            .mux_command(args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport.launch_failure(
                    &format!("Failed to run {} command", M::MULTIPLEXER.name()),
                    e,
                )
            })?;
        if !output.status.success()
            || (M::EXIT_ZERO_ERRORS && mux_reported_error(&output, self.transport.is_ssh(), false))
        {
            bail!(
                "{} {what} {}",
                self.transport.mux(),
                mux_failure(&output, self.transport.is_ssh())
            );
        }
        Ok(output)
    }

    /// Execute a tmux command on the thurbox socket and check for errors.
    pub(super) fn run_tmux(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = self
            .mux_command(args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport.launch_failure(
                    &format!("Failed to run {} command", M::MULTIPLEXER.name()),
                    e,
                )
            })?;

        if !output.status.success()
            || (M::EXIT_ZERO_ERRORS && mux_reported_error(&output, self.transport.is_ssh(), false))
        {
            return Err(self.command_failure(args, &output).into());
        }

        Ok(output)
    }

    pub(super) fn command_failure(
        &self,
        args: &[&str],
        output: &std::process::Output,
    ) -> crate::backend::contract::BackendError {
        let detail = mux_failure(output, self.transport.is_ssh());
        let message = format!(
            "{} {} failed: {detail}",
            M::MULTIPLEXER.name(),
            args.join(" ")
        );
        if self.transport.is_ssh() && output.status.code() == Some(SSH_ERROR_EXIT) {
            return crate::backend::contract::BackendError::Unreachable(message);
        }
        if output.status.code().is_none() {
            return crate::backend::contract::BackendError::Io(message);
        }
        match self.refusal(&detail, message) {
            crate::backend::contract::BackendError::Absent(message)
                if !output.stdout.is_empty() =>
            {
                crate::backend::contract::BackendError::Refused(message)
            }
            classified => classified,
        }
    }

    /// The multiplexer's refusal `reply`, typed by what this adapter's words
    /// say: the one classifier a one-shot's stderr and a control-mode `%error`
    /// both go through.
    pub(super) fn refusal(
        &self,
        reply: &str,
        message: String,
    ) -> crate::backend::contract::BackendError {
        let reply = reply.trim();
        if !reply.contains('\n') && M::answered_absent(reply).is_some() {
            return crate::backend::contract::BackendError::Absent(message);
        }
        if reply.starts_with("duplicate session: ") || reply.starts_with("duplicate window: ") {
            return crate::backend::contract::BackendError::Busy(message);
        }
        crate::backend::contract::BackendError::Refused(message)
    }

    /// `error`, with a control-mode [`Refusal`] in it typed by [`Self::refusal`].
    pub(super) fn classified(&self, error: anyhow::Error) -> anyhow::Error {
        match error.downcast::<Refusal>() {
            Ok(refused) => self
                .refusal(
                    &refused.reply,
                    format!("{} {refused}", M::MULTIPLEXER.name()),
                )
                .into(),
            Err(error) => error,
        }
    }

    /// The live connection, or an error when none has been started.
    ///
    /// A clone of the handle, so the caller's round trip runs with the lock
    /// already let go.
    pub(super) fn connection(&self) -> Result<Arc<ControlMode>> {
        self.link
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Control mode not started — call ensure_ready() first"))
    }

    /// Run a closure with the active control mode, or bail if it has not been
    /// started yet.
    pub(super) fn with_control<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&ControlMode) -> Result<R>,
    {
        let ctrl = self.connection()?;
        f(&ctrl).map_err(|e| self.classified(e))
    }

    /// Start control mode unless it is running.
    pub(super) fn ensure_control(&self) -> Result<()> {
        if self.attached() {
            return Ok(());
        }
        let _connecting = self
            .link
            .connecting
            .lock()
            .map_err(|e| anyhow::anyhow!("connecting lock: {e}"))?;
        if self.attached() {
            return Ok(());
        }
        debug!("Starting {} control mode", M::MULTIPLEXER.name());
        let fresh = ControlMode::start(
            &self.located_transport(),
            &self.socket(),
            &self.session,
            &self.link.sizer,
            &M::control_policy(&self.transport, &self.session),
        )
        .map_err(|e| self.classified(e))?;
        self.install(fresh)
    }

    /// Replace the connection `dead` with a fresh one; `None` replaces whatever
    /// is there.
    ///
    /// Callers that found the same connection dead queue on `connecting`, and
    /// all but the first find it already replaced and reuse the replacement
    /// instead of making a handshake each. The old connection keeps serving
    /// everybody else until the new one is in place.
    pub(super) fn reconnect_control(&self, dead: Option<&Arc<ControlMode>>) -> Result<()> {
        self.refuse_if_closed()?;
        let _connecting = self
            .link
            .connecting
            .lock()
            .map_err(|e| anyhow::anyhow!("connecting lock: {e}"))?;
        if let Some(dead) = dead {
            let current = self.connection().ok();
            if current.is_some_and(|current| !Arc::ptr_eq(&current, dead)) {
                return Ok(());
            }
        }
        // A failed `start()` leaves the existing handle in place, so a retry
        // reconnects cleanly instead of hitting `control = None` and reporting
        // the misleading "call ensure_ready() first".
        let start = || {
            ControlMode::start(
                &self.located_transport(),
                &self.socket(),
                &self.session,
                &self.link.sizer,
                &M::control_policy(&self.transport, &self.session),
            )
            .map_err(|e| self.classified(e))
        };
        // A connection that cannot be made again may be a server whose socket
        // went while the old connection held on: look for it once more.
        let fresh = match start() {
            Ok(fresh) => fresh,
            Err(_) if self.relocate(true) == Search::Found => start()?,
            Err(e) => return Err(e),
        };
        self.install(fresh)?;
        debug!("Control mode reconnected successfully");
        Ok(())
    }

    /// Make `fresh` the connection, unless [`WindowLifecycle::shutdown`] came
    /// first, and retire whatever it replaces.
    ///
    /// Retired rather than closed: other callers may be waiting on the old
    /// connection for answers to commands that have already run. Both it and
    /// a refused `fresh` are dealt with after the lock is let go, since a
    /// teardown waits on the child.
    pub(super) fn install(&self, fresh: ControlMode) -> Result<()> {
        let fresh = Arc::new(fresh);
        match self.swap(&fresh) {
            Ok(old) => {
                if let Some(old) = old {
                    old.retire();
                }
                Ok(())
            }
            Err(e) => {
                fresh.close();
                Err(e)
            }
        }
    }

    /// [`Self::install`]'s step under the lock `shutdown` takes the connection
    /// out under: refused once shut down, so a handshake that outlives a quit
    /// is torn down instead of left behind. Answers the connection replaced.
    ///
    /// The replaced connection is recorded as retiring before `control` is let
    /// go, because `shutdown` reads `retiring` only after taking `control`:
    /// recorded later, a quit in between found nothing to close and the old
    /// connection's child outlived it.
    pub(super) fn swap(&self, fresh: &Arc<ControlMode>) -> Result<Option<Arc<ControlMode>>> {
        let mut guard = self
            .link
            .control
            .lock()
            .map_err(|e| anyhow::anyhow!("control lock: {e}"))?;
        if self.is_closed() {
            bail!("{} is shut down", self.name);
        }
        let old = guard.replace(Arc::clone(fresh));
        self.link
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Some(old) = &old {
            if let Ok(mut retiring) = self.link.retiring.lock() {
                retiring.retain(|weak| weak.strong_count() > 0);
                retiring.push(Arc::downgrade(old));
            }
        }
        Ok(old)
    }

    /// Send a command via control mode and return the response.
    ///
    /// A connection that fails is replaced, but the command is sent again only
    /// when tmux cannot have seen it — the write itself failed. One that was
    /// written and never answered may have run, and running a `new-window`
    /// twice opens two windows, so the caller hears the failure instead.
    pub(super) fn ctrl_command(&self, cmd: &str) -> Result<String> {
        self.ctrl_command_list(&[cmd])
    }

    /// [`Self::ctrl_command`] for a command list, using the adapter's reply
    /// count (see `ControlMode::send_command_list`).
    pub(super) fn ctrl_command_list(&self, cmds: &[&str]) -> Result<String> {
        let blocks = if M::COMMAND_LIST_SINGLE_REPLY {
            1
        } else {
            cmds.len()
        };
        let ctrl = self.connection()?;
        match ctrl
            .send_command_list(cmds, blocks)
            .map_err(|e| self.classified(e))
        {
            Ok(val) => Ok(val),
            Err(err) if is_broken_pipe(&err) => {
                warn!("Control mode error, reconnecting: {err:#}");
                self.reconnect_control(Some(&ctrl))?;
                self.with_control(|ctrl| ctrl.send_command_list(cmds, blocks))
            }
            Err(err) if is_recv_timeout(&err) => {
                warn!("Control mode error, reconnecting: {err:#}");
                if let Err(e) = self.reconnect_control(Some(&ctrl)) {
                    debug!("reconnect after an unanswered command failed: {e:#}");
                }
                Err(err)
            }
            Err(err) => Err(err),
        }
    }

    /// Ask one question on a budget, for a caller that must not be made to
    /// wait: a link that has stopped carrying anything cannot overrun it.
    ///
    /// No reconnect on failure, unlike every other path here. A reconnect is a
    /// fresh ssh handshake plus the implicit attach response read back
    /// synchronously — precisely the unbounded wait this exists to avoid — and
    /// the callers that *can* wait will reconnect soon enough.
    pub(super) fn ctrl_command_within(
        &self,
        cmd: &str,
        budget: std::time::Duration,
    ) -> Result<String> {
        self.with_control(|ctrl| ctrl.send_command_within(cmd, budget))
    }

    /// Send a command whose answer nobody reads, without waiting for it.
    ///
    /// Reconnect-free for [`Self::ctrl_command_within`]'s reasons.
    /// `blocks` is tmux's count; a one-block server needs one waiter slot.
    pub(super) fn ctrl_command_detached(&self, cmds: &[&str], blocks: usize) -> Result<()> {
        let blocks = if M::COMMAND_LIST_SINGLE_REPLY {
            1
        } else {
            blocks
        };
        self.with_control(|ctrl| ctrl.send_command_detached(cmds, blocks))
    }
}
