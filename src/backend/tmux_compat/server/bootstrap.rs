//! Bringing the server and the thurbox session into being and configuring
//! them, and admitting a server new enough to be talked to.

use super::*;

/// Session-level tmux options applied to the thurbox tmux session.
///
/// Single source of truth for both the TUI and headless paths — applied
/// (alongside the server-wide options + `default-command`) by
/// [`Server::apply_session_config`].
///
/// **Session options only.** `set-option -t <session> <key>` does not mean "for
/// this session" when `<key>` is a *window* option: tmux resolves the target
/// down to the session's CURRENT window and sets it there (measured, tmux
/// 3.2a — the option is on `@0` and a window created a moment later does not
/// have it). Since `apply_session_config` runs on every `ensure_ready`, which
/// window ends up carrying such an option is an accident of timing. Window
/// options therefore live in [`WINDOW_OPTS`] when every window should have them,
/// and in [`birth_options`] when a window has to be given them as it is created:
/// `remain-on-exit`, which depends on what the window is *for*, and
/// `window-size`, which no tmux in the supported range survives as a
/// server-wide default.
pub(super) const SESSION_OPTS: &[(&str, &str)] = &[("status", "off"), ("history-limit", "5000")];

/// Window options applied to **every** window on thurbox's own tmux server.
///
/// Set with `-w -g` rather than per session: a window option has no session
/// scope to be set at (see [`SESSION_OPTS`]), and the alternative — setting it
/// on each window as it is born — would miss any window thurbox did not create.
/// The blast radius is thurbox's own socket, which holds nothing else.
///
/// `window-size` is **not** here, and must not be: made the server-wide default
/// it kills the server on every window creation from an unattached client (see
/// [`birth_options`], where it is said per window instead). Best-effort either
/// way — `resize-window -x/-y` already flips a window to `manual` when it
/// resizes it (measured, tmux 3.2a), and thurbox resizes every pane it paints.
pub(super) const WINDOW_OPTS: &[(&str, &str)] = &[
    // The default a window is BORN with, so the one role that wants a corpse
    // asks for it (in the same command list as its creation — see
    // `birth_options`) and nothing else inherits one. Said here rather than
    // left to tmux's own default because the user's `~/.tmux.conf` is read on
    // thurbox's socket too, and `set -g remain-on-exit on` there would have
    // every window born keeping its corpse — a program pane whose death is
    // then never announced, since tmux reports a pane's death only by closing
    // its window.
    //
    // Which role loses a race is not a choice between the two: born `off`, an
    // agent window whose command exits instantly used to vanish before its
    // `on` arrived, taking the server with it when it was the last one. Neither
    // role waits on a round trip now.
    ("remain-on-exit", "off"),
];

/// What [`Server::admit_server`]'s question found.
pub(super) enum Admission {
    /// The multiplexer said there is no server or no session.
    NothingThere,
    Admitted,
}

impl<M: TmuxCompatible> Server<M> {
    /// Whether the server behind the current connection was admitted
    /// ([`Self::admit_server`]), read without asking: for the paint and input
    /// loops, which must not wait on that question and must not name a pane to
    /// a server it has not passed — a reconnect to one never asked, or refused.
    /// Always true where there is no floor.
    pub(super) fn admitted_here(&self) -> bool {
        use std::sync::atomic::Ordering;
        M::VERSION_FLOOR.is_none()
            || self.link.admitted.load(Ordering::Acquire)
                == self.link.generation.load(Ordering::Acquire) + 1
    }

    /// Refuse a multiplexer below [`TmuxCompatible::VERSION_FLOOR`] before a
    /// pane is named to it, or a pane born on it.
    ///
    /// Both halves of it: the running server, since an upgrade leaves a server
    /// started before it on the old code, and the client binary, since psmux
    /// before 3.3.8 resolves a `-t` in the client too (#1557). Asked once per
    /// control connection; a one-shot answer is not kept, since nothing would
    /// tell it the server had been replaced. Only the multiplexer's own words
    /// for a missing server or a missing session on it — nothing broader — are
    /// let through, since nothing named there can arrive; any other answer
    /// that is not a version new enough refuses.
    pub(super) fn admit_server(&self) -> Result<()> {
        use std::sync::atomic::Ordering;
        let Some(refuse_old) = M::VERSION_FLOOR else {
            return Ok(());
        };
        let current = || self.link.generation.load(Ordering::Acquire) + 1;
        if self.link.admitted.load(Ordering::Acquire) == current() {
            return Ok(());
        }
        let asked_before = self.link.questions.load(Ordering::Acquire);
        // Guards no state a panic could leave half-written.
        let mut last_failure = self
            .link
            .admitting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Read before asking: a connection installed while the question is out
        // leaves this admission stale, and the next caller asks again.
        let generation = current();
        if self.link.admitted.load(Ordering::Acquire) == generation {
            return Ok(());
        }
        // A question settled while this caller waited, and failed on this
        // connection: asking again would wait it out once more.
        if self.link.questions.load(Ordering::Acquire) != asked_before {
            if let Some((failed_at, failure)) = last_failure.as_ref() {
                if *failed_at == generation {
                    bail!("{failure}");
                }
            }
        }
        let attached = self.attached();
        let outcome = self.ask_versions(attached, refuse_old);
        *last_failure = outcome
            .as_ref()
            .err()
            .map(|e| (generation, format!("{e:#}")));
        self.link.questions.fetch_add(1, Ordering::AcqRel);
        if matches!(outcome?, Admission::Admitted) && attached {
            self.link.admitted.store(generation, Ordering::Release);
        }
        Ok(())
    }

    /// [`Self::admit_server`]'s question, to the client binary and then the
    /// server.
    pub(super) fn ask_versions(
        &self,
        attached: bool,
        refuse_old: fn(&str, &str) -> Result<()>,
    ) -> Result<Admission> {
        // Read strictly here, then judged by the adapter's own binary check,
        // whose advice is about the binary: its server may be fine, and
        // restarting that would end every agent and change nothing.
        let mux = self.transport.mux();
        let banner = self.tmux_output(&["-V"])?;
        let Some(client) = M::banner_version(&banner).filter(|v| is_version_answer(v)) else {
            bail!(
                "the {mux} binary did not give a version thurbox can hold to its floor (it \
                 answered {banner:?}); put a released {mux} first on PATH"
            );
        };
        // The value just read strictly is the one judged: the whole banner,
        // read leniently, could name another line or drop a suffix.
        M::check_banner(client, &self.socket())?;
        let answer = match attached {
            true => self
                .ctrl_command("display-message -p '#{version}'")
                .context("could not ask the server its version")?,
            false => match self.ask_version_once()? {
                Some(answer) => answer,
                None => return Ok(Admission::NothingThere),
            },
        };
        let version = answer.trim();
        if !is_version_answer(version) {
            bail!("the server did not say its version (it answered {version:?})");
        }
        refuse_old(version, &self.socket())?;
        Ok(Admission::Admitted)
    }

    /// The server's `#{version}` from a one-shot, or `None` when the
    /// multiplexer said in its own words that there is no server or no session
    /// — and only then: `can't find window` is also what psmux 3.3.8 says of a
    /// live server refusing a target, and that server runs what comes next.
    pub(super) fn ask_version_once(&self) -> Result<Option<String>> {
        let args = ["display-message", "-t", &self.session, "-p", "#{version}"];
        let output = self
            .mux_command(&args)?
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                self.transport
                    .launch_failure("Failed to ask the server its version", e)
            })?;
        let is_ssh = self.transport.is_ssh();
        if listing_output_is_absence::<M>(is_ssh, &output) {
            return Ok(None);
        }
        if !output.status.success() || !mux_stderr(&output, is_ssh).trim().is_empty() {
            return Err(anyhow::Error::from(self.command_failure(&args, &output))
                .context("could not ask the server its version"));
        }
        let answer = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // psmux reports some failures on stdout with status 0
        // (`EXIT_ZERO_ERRORS`).
        if let Some(error) = answer
            .strip_prefix("ERROR:")
            .filter(|_| M::EXIT_ZERO_ERRORS)
        {
            if !error.contains('\n') && M::answered_absent(error.trim()) == Some(Missing::Server) {
                return Ok(None);
            }
            bail!("the server refused the version question: {answer}");
        }
        Ok(Some(answer))
    }

    /// Check if the thurbox tmux session exists.
    pub(super) fn session_exists(&self) -> bool {
        self.tmux_run(&[M::SESSION_PROBE_COMMAND, "-t", &self.session])
            .is_ok()
    }

    /// Apply server + session config to the tmux session.
    ///
    /// Idempotent (`set-option` overwrites), so it is safe to call on every
    /// [`ensure_ready`](Self::ensure_ready) — the session may have been created
    /// elsewhere (e.g. a headless spawn) without these options, and re-applying
    /// is the single source of truth for both the TUI and headless paths.
    ///
    /// Where the server takes command lists the whole config is **one**
    /// invocation (#1243): it runs on every `session create`, and as ten
    /// processes it was most of that command's cost. A failure in the list is
    /// then re-run one option at a time, which is what tells a fatal option
    /// from a best-effort one.
    pub(super) fn apply_session_config(&self) -> Result<()> {
        let config = self.session_config();
        if M::COMMAND_LISTS && self.tmux_run(&config_command_list(&[], &config)).is_ok() {
            return Ok(());
        }
        for option in &config {
            let args: Vec<&str> = option.args.iter().map(String::as_str).collect();
            match self.tmux_run(&args) {
                Ok(()) => {}
                Err(e) if option.fatal => return Err(e),
                Err(e) => debug!(
                    "{} option {} not set: {e}",
                    M::MULTIPLEXER.name(),
                    option.args.join(" ")
                ),
            }
        }
        Ok(())
    }

    /// Every `set-option` [`apply_session_config`](Self::apply_session_config)
    /// runs, in order.
    pub(in crate::backend) fn session_config(&self) -> Vec<ConfigOption> {
        let scope = M::SERVER_SCOPE;
        let mut config = Vec::new();
        let mut set = |args: &[&str], fatal: bool| {
            let mut command = args.to_vec();
            if M::SERVER_OPTIONS_NEED_TARGET && args.first() == Some(&scope) {
                command.splice(1..1, ["-t", &self.session]);
            }
            config.push(ConfigOption::set(&command, fatal));
        };
        // Use a non-login shell so that macOS path_helper (/etc/zprofile)
        // doesn't clobber PATH additions from ~/.zshenv (e.g. cargo, asdf).
        // For a remote backend the local `$SHELL` path may not exist on the
        // remote host, so fall back to a POSIX shell there.
        //
        // On a Windows machine we deliberately do NOT pin `default-command`:
        // `$SHELL` and `/bin/sh` don't exist there, and forcing a Windows shell
        // would have to match the multiplexer's own command-execution model, so
        // its native default shell is the safe choice. Decided by the platform
        // of the machine the server runs on — not by the OS thurbox was built
        // for (a Windows thurbox driving a WSL distro left its tmux on the
        // login shell) and not by the multiplexer's name.
        if self.platform == Platform::Posix {
            set(&[scope, "default-command", &self.config_shell()], true);
        }

        // Server-wide options every supported multiplexer understands. A
        // failure here means the server can't host sessions, so it is
        // propagated.
        set(&[scope, "default-terminal", "xterm-256color"], true);
        set(&[scope, "extended-keys", "on"], true);

        // `extended-keys-format csi-u` is best-effort: the option landed in tmux
        // 3.5, but thurbox's floor is 3.2, so an older tmux rejects it ("invalid
        // option"). It is advisory only — thurbox injects keystroke bytes directly
        // via `send-keys` (not through tmux's key forwarder), so it never
        // re-encodes what an agent receives; it just sets what `tmux show-options`
        // reports, which some agents (notably `pi`) probe at startup and warn about
        // unless it is `csi-u`. Ignoring the error keeps a 3.2–3.4 host working (pi
        // users there simply miss the hint) while 3.5+ hosts get the preferred
        // format.
        set(&[scope, "extended-keys-format", "csi-u"], false);

        for (key, val) in SESSION_OPTS {
            set(&["-t", &self.session, key, val], true);
        }

        // Window-level options — see `WINDOW_OPTS` for why these are global to
        // the server and why failing to set one is not fatal.
        for (key, val) in WINDOW_OPTS {
            set(&["-w", "-g", key, val], false);
        }

        // What only this multiplexer takes — tmux's clipboard and mouse
        // options, which psmux has no equivalent of.
        config.extend(M::session_config(&self.session));
        config
    }

    /// Ensure the thurbox tmux session exists and its options are applied,
    /// **without** starting control mode.
    ///
    /// Shared by [`ensure_ready`](Self::ensure_ready) (which then starts control
    /// mode) and the headless spawn paths ([`create_local_window`],
    /// [`StatusChannel::ensure_heartbeat`]) that drive tmux via one-shot commands and
    /// must not open a control-mode connection.
    pub(in crate::backend) fn ensure_session_configured(&self) -> Result<()> {
        for attempt in 1..M::COLD_START_ATTEMPTS {
            match self.ensure_session_configured_once() {
                Ok(()) => return Ok(()),
                Err(e) if self.retryable_cold_start_error(&e) => {
                    debug!(
                        "{} cold session bootstrap attempt {attempt} lost its server: {e:#}",
                        self.name
                    );
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
        match self.ensure_session_configured_once() {
            Ok(()) => Ok(()),
            Err(e)
                if self.retryable_cold_start_error(&e)
                    && M::COLD_START_GRACE > std::time::Duration::ZERO =>
            {
                self.wait_and_configure(e)
            }
            Err(e) => Err(e),
        }
    }

    pub(super) fn wait_and_configure(&self, error: anyhow::Error) -> Result<()> {
        let deadline = std::time::Instant::now() + M::COLD_START_GRACE;
        let mut last_error = error;
        loop {
            if self.session_exists() {
                match self.apply_session_config() {
                    Ok(()) => return Ok(()),
                    Err(e) if M::RETRY_NO_SERVER_ERROR && typed_absence(&e) => {
                        last_error = e;
                    }
                    Err(e) => return Err(e),
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(last_error);
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }

    pub(super) fn retryable_cold_start_error(&self, error: &anyhow::Error) -> bool {
        !crate::agent::preflight::is_missing_dependency(error)
            && (!self.session_exists() || (M::RETRY_NO_SERVER_ERROR && typed_absence(error)))
    }

    pub(super) fn ensure_session_configured_once(&self) -> Result<()> {
        // The common case — the session is there — asked and configured in one
        // process: `has-session` failing stops the list before any option is
        // set, and the path below then says why.
        if M::COMMAND_LISTS {
            let config = self.session_config();
            let list = config_command_list(&["has-session", "-t", &self.session], &config);
            if self.tmux_run(&list).is_ok() {
                return Ok(());
            }
        }
        if !self.session_exists() && !self.found_after_all()? {
            // No session to ask for `#{version}` yet, and creating one may start
            // a server — with an idle shell in it — that every spawn would then
            // refuse. The binary is what would start it, so it answers instead.
            M::check_banner(&self.tmux_output(&["-V"])?, &self.socket())?;
            debug!(
                "Creating {} session '{}' on socket '{}'",
                M::MULTIPLEXER.name(),
                self.session,
                self.socket()
            );
            let mut args = vec!["new-session", "-d", "-s", &self.session];
            args.extend_from_slice(M::BOOTSTRAP_SIZE_ARGS);
            if let Err(e) = self.run_tmux(&args) {
                // The check above is not a lock, and after a reboot every
                // session's relaunch runs it at once: one wins and the rest are
                // told `duplicate session`. Failing them is how a machine came
                // back with all but one of its agents missing — each loser
                // aborted its whole respawn, kept the pane id the dead server
                // had given it, and that id then named whichever window the
                // winner got. The session we wanted exists either way, so ask
                // rather than assume, and only report a failure that left none.
                if !self.session_exists() {
                    // A launcher that never started is already the whole story
                    // (`preflight::launch_failure`), and this context in front
                    // of it costs the reader the part that names the binary and
                    // the fix — a message row is one line wide.
                    if crate::agent::preflight::is_missing_dependency(&e) {
                        return Err(e);
                    }
                    return Err(e).with_context(|| {
                        format!("Failed to create {} session", M::MULTIPLEXER.name())
                    });
                }
                debug!(
                    "{} session '{}' was created by a peer; continuing",
                    M::MULTIPLEXER.name(),
                    self.session
                );
            }
            // Cheap defensiveness: poll until the freshly-created session
            // answers the session probe before applying options. (The `no server
            // running on 'thurbox__thurbox'` failure that originally motivated
            // this on psmux was session *nesting*, now fixed at the root by
            // `strip_mux_nesting_env`; this poll is a harmless belt against any
            // genuinely-async `new-session -d`, and one session probe when the
            // first probe succeeds — which it does on the normal path, and only
            // when a session had to be created at all.)
            self.wait_for_session_ready(std::time::Duration::from_secs(5));
        }
        self.apply_session_config()
    }

    /// Poll until the freshly-created session answers its probe.
    /// Defensive belt against an async `new-session -d`; normally a no-op (the
    /// first probe succeeds). See
    /// [`ensure_session_configured`](Self::ensure_session_configured).
    pub(super) fn wait_for_session_ready(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.session_exists() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `remain-on-exit` is a WINDOW option, and `window-size` is one too: neither
    /// can be set for a session, so neither belongs in the session list. Both
    /// are stated as the window is created (`birth_options`).
    #[test]
    fn the_session_option_list_holds_no_window_options() {
        for (key, _) in SESSION_OPTS {
            assert!(
                !["remain-on-exit", "window-size"].contains(key),
                "{key} is a window option and is silently applied to whichever \
                 window happens to be current"
            );
        }
    }

    /// `window-size manual` may be said for a window, never for the server.
    ///
    /// tmux works out a window's size *before* the window exists
    /// (`spawn_window` → `default_window_size(…, w = NULL)`) and the manual
    /// branch of `clients_calculate_size` reads `w->manual_sx` with no NULL
    /// check, so a server whose default is `manual` dies on the next
    /// `new-window` from an unattached client — 3.3 … 3.6. Measured on 3.5a:
    /// `server exited unexpectedly` every time with the server-wide write, a
    /// pane id every time without it. 3.2 and 3.2a have the option too and
    /// survive the server-wide write (measured).
    #[test]
    fn the_server_wide_window_options_do_not_size_windows_by_hand() {
        for (key, value) in WINDOW_OPTS {
            assert!(
                *key != "window-size",
                "a server-wide `window-size {value}` kills the server on the \
                 next window creation; say it per window (`birth_options`)"
            );
        }
        assert!(
            birth_options("tb-anything")
                .iter()
                .any(|(key, value)| *key == "window-size" && *value == "manual"),
            "the window that is created still has to be told"
        );
    }
}
