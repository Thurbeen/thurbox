//! Hook status and the heartbeat ([`StatusChannel`]), read from pane
//! options over the server's own connection (ADR-32).

use super::*;
use crate::session::Platform;

impl<M: TmuxCompatible> StatusChannel for Server<M> {
    /// Only where a hook can report through the pane option
    /// ([`TmuxCompatible::HOOK_STATUS`]).
    fn hook_signal_command(&self) -> Option<String> {
        M::HOOK_STATUS.then(|| M::hook_signal_command(self))
    }

    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        // `try_lock`, not `lock`: this runs on the UI thread every tick, and a
        // skipped drain only defers queued events to the next tick.
        let control = self
            .link
            .control
            .try_lock()
            .ok()
            .and_then(|guard| guard.clone());
        control
            .map(|ctrl| ctrl.take_sub_events())
            .unwrap_or_default()
    }

    fn record_hook_state(
        &self,
        pane: &str,
        state: &str,
    ) -> crate::backend::contract::BackendResult<()> {
        self.refuse_without_hook_status()?;
        if !control_mode::is_valid_pane_id(pane) {
            return Err(anyhow::anyhow!("'{pane}' is not a pane id").into());
        }
        Ok(self
            .one_shot(
                "set-option (hook state)",
                &[
                    "set-option",
                    "-p",
                    "-t",
                    &self.target(pane)?,
                    control_mode::REMOTE_HOOK_STATE_OPTION,
                    state,
                ],
            )
            .map(drop)?)
    }

    /// Read-only by design — no `ensure_ready`, so a poll never creates the
    /// server or the session. A server that says it has no such session (or
    /// that is not running) holds no states; a question that never reached
    /// one — an unreachable host, a missing binary — is an `Err`.
    fn hook_states(&self) -> crate::backend::contract::BackendResult<Vec<(String, String)>> {
        self.refuse_without_hook_status()?;
        match self.run_tmux(&["has-session", "-t", &self.session]) {
            Ok(_) => {}
            Err(e) if typed_absence(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        }
        let format = hook_state_format();
        let body = self.run_tmux(&["list-panes", "-s", "-t", &self.session, "-F", &format])?;
        Ok(control_mode::parse_pane_hook_states(
            String::from_utf8_lossy(&body.stdout).trim(),
        ))
    }

    /// A detached window, not `tb-` prefixed so [`Self::discover`] ignores
    /// it. The live window also keeps the server alive, so spawn-only
    /// automations work with no other sessions. Asked of the backend serving
    /// this machine's default multiplexer: `check_available` decides whether
    /// its binary may start a server at all (an old psmux may not).
    fn ensure_heartbeat(
        &self,
        program: &Path,
        args: &[String],
        every: std::time::Duration,
    ) -> crate::backend::contract::BackendResult<()> {
        // A probe nobody answered is not a reason to stop arming: readying the
        // session below either starts the server or fails with its own error.
        if self.heartbeat_running().unwrap_or(false) {
            return Ok(());
        }
        self.ensure_heartbeat_session()?;
        let loop_cmd = heartbeat_loop_command(self.platform, program, args, every)?;
        let out = self
            .mux_command(&[
                "new-window",
                "-d",
                "-t",
                &self.session,
                "-n",
                HEARTBEAT_WINDOW,
                &loop_cmd,
            ])?
            .output()
            .map_err(|e| {
                self.transport
                    .launch_failure("Failed to create automation heartbeat window", e)
            })?;
        // Asked again rather than believed: the status may be a failing user
        // hook's and not this window's — see the same read in
        // `create_local_window`. There is no `-P` answer to trust here, so the
        // listing is what says whether the window exists.
        if !out.status.success() && !self.heartbeat_running()? {
            return Err(anyhow::anyhow!(
                "{} new-window (heartbeat) {}",
                self.transport.mux(),
                mux_failure(&out, self.transport.is_ssh())
            )
            .into());
        }
        debug!("Armed automation heartbeat keeper window on {}", self.name);
        Ok(())
    }

    /// No server, or no session on it, is a heartbeat not running. Any other
    /// failed listing is an unanswered question (`listing_is_absence`).
    fn heartbeat_running(&self) -> crate::backend::contract::BackendResult<bool> {
        let out = self
            .mux_command(&["list-windows", "-t", &self.session, "-F", "#{window_name}"])?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport.launch_failure(
                    &format!("Failed to run {} command", M::MULTIPLEXER.name()),
                    e,
                )
            })?;
        if !out.status.success() {
            if listing_output_is_absence::<M>(self.transport.is_ssh(), &out) {
                return Ok(false);
            }
            return Err(anyhow::anyhow!(
                "{} list-windows (heartbeat) {}",
                self.transport.mux(),
                mux_failure(&out, self.transport.is_ssh())
            )
            .into());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|w| w == HEARTBEAT_WINDOW))
    }

    /// Automations stop firing headlessly until something arms it again —
    /// which any `automation` write does, so this is a pause, not a removal.
    fn stop_heartbeat(&self) -> crate::backend::contract::BackendResult<bool> {
        if !self.heartbeat_running()? {
            return Ok(false);
        }
        let target = format!("{}:{HEARTBEAT_WINDOW}", self.session);
        match self.run_tmux(&["kill-window", "-t", &target]) {
            Ok(_) => Ok(true),
            Err(e) if typed_absence(&e) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

/// Window name for the headless automation heartbeat keeper. Deliberately NOT
/// `tb-` prefixed so [`Server::discover`](WindowLifecycle::discover) ignores it — it is
/// infrastructure, not a session.
pub(super) const HEARTBEAT_WINDOW: &str = "automation-heartbeat";

/// The keeper's loop, as the window command, in the shell of the machine the
/// server runs on ([`crate::shell::HostShell::heartbeat_loop`]).
pub(super) fn heartbeat_loop_command(
    platform: Platform,
    program: &Path,
    args: &[String],
    every: std::time::Duration,
) -> Result<String> {
    crate::shell::HostShell::for_platform(platform)
        .heartbeat_loop(&program.display().to_string(), args, every.as_secs())
        .context("the automation heartbeat")
}

impl<M: TmuxCompatible> Server<M> {
    /// Refuse a status verb on a server whose hooks have no channel
    /// ([`TmuxCompatible::HOOK_STATUS`]).
    pub(super) fn refuse_without_hook_status(&self) -> Result<()> {
        if !M::HOOK_STATUS {
            bail!(
                "{} carries no hook status: its pane-option channel is not proven",
                self.name
            );
        }
        Ok(())
    }

    /// The thurbox session on this server, created when it is not there — as
    /// the backend would create it, peers racing to included, and only once
    /// the backend agrees its binary may start one. No session config: the
    /// keeper needs none, and every spawn or attach applies it.
    pub(super) fn ensure_heartbeat_session(&self) -> Result<()> {
        let exists = || self.session_exists();
        if exists() || self.found_after_all()? {
            return Ok(());
        }
        self.check_available()?;
        let mut args = vec!["new-session", "-d", "-s", &self.session];
        args.extend_from_slice(M::BOOTSTRAP_SIZE_ARGS);
        let out = self
            .mux_command(&args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport.launch_failure(
                    &format!("Failed to run {} command", M::MULTIPLEXER.name()),
                    e,
                )
            })?;
        if !out.status.success() && !exists() {
            bail!(
                "{} new-session (heartbeat) {}",
                self.transport.mux(),
                mux_failure(&out, self.transport.is_ssh())
            );
        }
        // As `ensure_session_configured` waits after creating one: a server
        // whose `new-session -d` returns before the session answers would
        // refuse the keeper's `new-window`.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::backend::tmux_compat::server::tests::answering_mux;

    /// A kill that failed is not "there was none to stop".
    #[cfg(unix)]
    #[test]
    fn a_heartbeat_that_could_not_be_killed_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let backend = answering_mux(
            dir.path(),
            HEARTBEAT_WINDOW,
            "error connecting to /tmp/tmux-1/thurbox-test (Permission denied)",
        );
        assert!(backend.heartbeat_running().unwrap());
        assert!(backend.stop_heartbeat().is_err());
    }

    /// The Windows keeper names its interpreter, like every psmux window: a
    /// server started over Windows OpenSSH would otherwise hand the loop to
    /// `cmd.exe` and the keeper window would close at once.
    #[test]
    fn the_windows_heartbeat_loop_runs_in_named_powershell() {
        use base64::Engine as _;
        let program = heartbeat_loop_command(
            Platform::Windows,
            Path::new("C:\\bin\\thurbox-cli.exe"),
            &["automation".to_string(), "tick".to_string()],
            std::time::Duration::from_secs(30),
        )
        .expect("the keeper fits a Windows command line");
        let encoded = program
            .strip_prefix("powershell.exe -NoLogo -EncodedCommand ")
            .expect("an explicit PowerShell launch");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64");
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(
            String::from_utf16(&units).expect("UTF-16LE"),
            "while ($true) { & 'C:\\bin\\thurbox-cli.exe' 'automation' 'tick' *> $null; \
             Start-Sleep 30 }"
        );
    }
}
