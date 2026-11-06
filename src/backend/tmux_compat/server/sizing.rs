//! Window size: what a pane is resized to, and which client's size the
//! server follows.

use super::*;

/// `(rows, cols)` within what `resize-window` accepts, so a resize an `if-shell`
/// runs can never be the one that fails (see `Server::resize_window`). tmux's
/// bounds are 1 and `WINDOW_MAXIMUM`, 10000.
pub(super) fn tmux_size(rows: u16, cols: u16) -> (u16, u16) {
    (rows.clamp(1, 10_000), cols.clamp(1, 10_000))
}

impl<M: TmuxCompatible> Server<M> {
    /// Resize a pane, forcing a SIGWINCH even if dimensions haven't changed.
    pub(super) fn force_resize(&self, pane_id: &str, rows: u16, cols: u16) -> Result<()> {
        // Briefly resize to different dimensions to guarantee a SIGWINCH,
        // then resize to the actual target. This causes TUI apps to repaint.
        if rows > 1 {
            self.resize(pane_id, rows - 1, cols)?;
        } else {
            self.resize(pane_id, rows + 1, cols)?;
        }
        self.resize(pane_id, rows, cols)?;
        Ok(())
    }

    /// [`PaneStream::resize`]: size `backend_id`'s window and pane to the rect
    /// this instance paints it into.
    pub(super) fn resize_window(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
    ) -> crate::backend::contract::BackendResult<()> {
        // Sent, not asked. The caller is the render thread matching the pane to
        // the rect it is painting into, and the answer is of no use to the
        // frame: a resize tells the *agent* how to wrap, which is a message to
        // the host. Waiting for the confirmation put a control-mode round trip
        // inside the paint, and on a link that had gone bad that was the whole
        // interface frozen until the command timed out.
        //
        // Still two resizes and still in this order — a pane cannot exceed its
        // window — but as one list, so they take the lock once and tmux runs
        // them without returning to its event loop in between. Sent separately
        // they could be refused separately, and a window resized around a pane
        // that was not leaves the agent wrapping at the old width until some
        // later rect change asks again.
        //
        // Not `admit_server`, whose question is a round trip the paint must not
        // wait on: only what is already known of this connection. One not
        // admitted — a reconnect to a server not yet asked, or refused — is
        // sent nothing; the pane's next attach asks, and a later paint sizes it.
        if !self.admitted_here() {
            return Ok(());
        }
        let (rows, cols) = tmux_size(rows, cols);
        let target = self.spell_target(backend_id);
        let window = format!("resize-window -t {target} -x {cols} -y {rows}");
        let pane = format!("resize-pane -t {target} -x {cols} -y {rows}");
        if !M::CONDITIONAL_RESIZE {
            // No `if-shell -F` to decide with: the last instance to paint wins.
            return Ok(self.ctrl_command_detached(&[&window, &pane], 2)?);
        }
        // A pane is the size of the rect ONE instance paints it into. Several
        // instances attached to one server each paint their own rect, and when
        // each resized to its own, whichever painted last — a toast taking a
        // row is enough — re-wrapped the agent for everybody. So the window
        // names its sizer (`SIZER_OPTION`), and a paint resizes only a window
        // that is this instance's to size: one nobody claims, one it already
        // sizes, or any window at all while it is the only client attached,
        // which is what makes a sizer that quit or crashed let go.
        // `claim_size` is how the name changes hands.
        //
        // Decided by tmux, in this same list, so a decision costs no round trip
        // and two instances cannot both win it. The shape is fixed on purpose:
        // the response queue expects a known number of `%begin` blocks per
        // list, and `if-shell` answers with one more block for each command it
        // runs — four taken and one declined, measured on tmux 3.7c. So the
        // name is settled first by a `set-option -F` that always answers once,
        // and each resize is its own `if-shell` whose else runs one command
        // too: five blocks, whichever way it goes. A pane that is gone fails
        // the first command and tmux drops the rest, which the queue expects
        // of any list; an inner command failing does NOT stop the list, which
        // is why the sizes are clamped to what tmux accepts (`tmux_size`).
        let me = &self.link.sizer;
        let may = format!(
            "#{{||:#{{==:#{{session_attached}},1}},#{{||:#{{==:#{{{SIZER_OPTION}}},}},#{{==:#{{{SIZER_OPTION}}},{me}}}}}}}"
        );
        let settle = format!(
            "set-option -F -w -t {target} {SIZER_OPTION} '#{{?{may},{me},#{{{SIZER_OPTION}}}}}'"
        );
        let mine = format!("#{{==:#{{{SIZER_OPTION}}},{me}}}");
        let only_if_mine = |cmd: &str| {
            format!("if-shell -F -t {target} '{mine}' '{cmd}' 'display-message -p \"\"'")
        };
        Ok(self
            .ctrl_command_detached(&[&settle, &only_if_mine(&window), &only_if_mine(&pane)], 5)?)
    }

    /// [`PaneStream::claim_size`]: take the window's sizing for this instance
    /// and size it.
    pub(super) fn claim_window_size(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
    ) -> crate::backend::contract::BackendResult<()> {
        if !M::CONDITIONAL_RESIZE {
            return self.resize_window(backend_id, rows, cols);
        }
        // Unconditional: this is the instance being typed into, which is what
        // decides who sizes (see `resize_window`). Not `admit_server`, as `resize` is
        // not: this runs on the input loop.
        if !self.admitted_here() {
            return Ok(());
        }
        let (rows, cols) = tmux_size(rows, cols);
        let target = self.spell_target(backend_id);
        Ok(self.ctrl_command_detached(
            &[
                &format!(
                    "set-option -w -t {target} {SIZER_OPTION} {}",
                    self.link.sizer
                ),
                &format!("resize-window -t {target} -x {cols} -y {rows}"),
                &format!("resize-pane -t {target} -x {cols} -y {rows}"),
            ],
            3,
        )?)
    }
}
