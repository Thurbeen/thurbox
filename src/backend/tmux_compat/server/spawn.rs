//! Opening windows: the one-shot local `new-window`, the control-mode spawn
//! on a host, the program and shell a window runs, and retiring a window
//! that duplicates a session's stamp (ADR-25).

use super::*;

/// The pane id out of a one-shot `new-window -P -F '#{pane_id}'`'s stdout.
///
/// The **first** line, not the whole of it: anything a hook's `run-shell`
/// prints is appended after the `-P` answer on the same stream, so trimming the
/// lot yields an id with a shell's complaint stuck to it — which then fails
/// validation and loses a window that exists.
pub(super) fn new_window_pane_id(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

impl<M: TmuxCompatible> Server<M> {
    /// [`WindowLifecycle::create_window`] on a host: the control-mode spawn,
    /// through a connection dropped once the window exists. The host's server
    /// keeps the window for an interface to adopt later.
    pub(super) fn create_remote_window(&self, spec: &WindowSpec<'_>) -> Result<PaneId> {
        let backend = self.transient();
        backend.check_available().with_context(|| {
            format!(
                "remote host is unreachable or {} is missing",
                M::MULTIPLEXER.name()
            )
        })?;
        backend.ensure_ready()?;
        let window_name = window_name_for(spec.role, spec.owner.name);
        // Headless: no live terminal, so use a sane default geometry. The TUI
        // resizes the pane to its real dimensions when it adopts the session.
        let spawned = backend.spawn(
            &window_name,
            spec.command,
            spec.args,
            spec.cwd,
            spec.env,
            24,
            80,
        )?;
        if let Err(e) = backend.stamp_window(&spawned.backend_id, spec.owner.session_id, spec.role)
        {
            debug!(
                "could not stamp the remote window for '{}': {e:#}",
                spec.owner.name
            );
        }
        let pane_id = PaneId::new(spawned.backend_id)?;
        backend.confirm_spawned(spec, &pane_id)?;
        Ok(pane_id)
    }

    /// Refuse a window whose pane is already gone
    /// [`TmuxCompatible::SPAWN_SETTLE`] after it was created. Only `pane_id`
    /// itself counts: a live namesake under the same `tb-<name>` is another
    /// session's window (issue #1456).
    ///
    /// A listing that does not answer refuses nothing: the window was
    /// created, and a question left unanswered is not evidence it died.
    pub(super) fn confirm_spawned(&self, spec: &WindowSpec<'_>, pane_id: &PaneId) -> Result<()> {
        let Some(settle) = M::SPAWN_SETTLE else {
            return Ok(());
        };
        std::thread::sleep(settle);
        let window_name = window_name_for(spec.role, spec.owner.name);
        let panes = match self.spawned_panes(&window_name) {
            Ok(panes) => panes,
            Err(e) => {
                warn!("could not confirm window {window_name} after creating it: {e:#}");
                return Ok(());
            }
        };
        if panes
            .iter()
            .any(|(id, dead)| !dead && id == pane_id.as_str())
        {
            return Ok(());
        }
        Err(self.spawn_exited(spec, &window_name))
    }

    /// Why a create is refused when its window closed with its program.
    pub(super) fn spawn_exited(&self, spec: &WindowSpec<'_>, window_name: &str) -> anyhow::Error {
        anyhow::anyhow!(
            "window {window_name} exited within {}ms of starting `{}`: {} keeps no \
             window once its program exits, so nothing of it is left to read. Check \
             that `{}` runs on {}",
            M::SPAWN_SETTLE.unwrap_or_default().as_millis(),
            spec.command,
            self.transport.mux(),
            spec.command,
            self.name
        )
    }

    /// [`WindowLifecycle::window_panes`], asked through the control connection
    /// where there is one: a one-shot listing over ssh is re-read by the
    /// host's login shell, and `cmd.exe` takes the format's `|` for a pipe.
    pub(super) fn spawned_panes(&self, window_name: &str) -> Result<Vec<(String, bool)>> {
        if !self.attached() {
            return Ok(self.window_panes(window_name)?);
        }
        let listing = self.ctrl_command(&format!(
            "list-windows -t {} -F '{WINDOW_PANES_FORMAT}'",
            self.session
        ))?;
        Ok(named_panes(&listing, window_name))
    }

    /// What `owner`'s `role` window is, where the listing alone could not say.
    ///
    /// A multiplexer without window options (ADR-13) stamps nothing, so
    /// local recovery can select the sole fresh live namesake, but multiple live
    /// names stay unknown. A restarted server can reissue a remembered pane id
    /// to another same-name window. One stamp on two windows is repairable,
    /// on a host as on this machine (issue #1635), and here is where repairing
    /// it matters:
    /// `stamped_match` refuses the pair by design, so without this nothing
    /// would ever look again and the session stayed unaddressable for good
    /// (issue #1207). The refusal itself is untouched — the choice is made by
    /// *retiring* a window, which is a write, and never by reading one of two
    /// as the answer.
    pub(super) fn settle(&self, owner: Owner<'_>, role: WindowRole) -> Result<Located> {
        if !M::WINDOW_OPTIONS {
            if !self.transport.is_remote() {
                return Ok(WindowIndex::from_listing(self.discover_answered()?).locate(
                    owner.session_id,
                    owner.name,
                    role,
                    true,
                ));
            }
            return Ok(Located::Unknown);
        }
        if self
            .retire_duplicate_windows(owner.session_id, role)
            .is_none()
        {
            return Ok(Located::Unknown);
        }
        Ok(WindowIndex::from_listing(self.discover_answered()?).locate(
            owner.session_id,
            owner.name,
            role,
            false,
        ))
    }

    /// The shell tmux should use for `default-command`. Local uses the user's
    /// `$SHELL`; a remote backend uses a POSIX shell guaranteed to exist on the
    /// remote host. Not used on a Windows machine (its multiplexer keeps its
    /// native default shell — see [`session_config`](Self::session_config)).
    ///
    /// The value must be a single, space-free token: it round-trips through the
    /// remote transport's per-argument shell-quoting (`ssh`/`wsl.exe`), where a
    /// space would be re-split by the remote shell into extra `set-option` args.
    /// The login-shell `PATH` fix for remote agents (e.g. `claude` under
    /// `~/.local/bin`) is applied at the *window command* instead — see
    /// [`build_shell_command`](Self::build_shell_command) /
    /// [`login_wrap_for_remote`](Self::login_wrap_for_remote).
    pub(in crate::backend) fn config_shell(&self) -> String {
        if self.transport.is_remote() {
            "/bin/sh".to_string()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        }
    }

    /// The program a **local** window should launch: the agent's command,
    /// resolved against thurbox's own `PATH` — see [`resolve_local_program`].
    /// A remote/WSL backend passes through: its `PATH` is the *host's*, and its
    /// window command is login-wrapped instead
    /// ([`login_wrap_for_remote`](Self::login_wrap_for_remote)).
    pub(in crate::backend) fn program_for_window(&self, command: &str) -> String {
        if self.transport.is_remote() {
            return command.to_string();
        }
        resolve_local_program(command)
    }

    /// Build the shell command string to pass to tmux new-window.
    ///
    /// The whole string is interpreted by the multiplexer server's shell, so
    /// **every** token — the command itself as well as each argument — is
    /// shell-escaped. Leaving the command unescaped would break (or allow
    /// injection through) a command path containing a space or shell
    /// metacharacter; `shell_escape` is a no-op for ordinary binary names so the
    /// common case (`claude`, `/usr/bin/codex`) is unchanged.
    pub(in crate::backend) fn build_shell_command(command: &str, args: &[String]) -> String {
        let mut parts = vec![control_mode::shell_escape(command)];
        for arg in args {
            parts.push(control_mode::shell_escape(arg));
        }
        parts.join(" ")
    }

    /// Wrap a window command in a **login** shell for a remote/WSL backend so the
    /// user's profile `PATH` is present. Agents are commonly installed under
    /// `~/.local/bin` (e.g. `claude`), which the login profile adds to `PATH`; a
    /// non-login shell skips those files, so the agent binary isn't found, the
    /// window command exits 1, and the pane dies instantly — the remote session
    /// appears to "not launch". `exec` replaces the wrapper so no extra process
    /// lingers. A **Windows** host passes through, whatever multiplexer serves
    /// it: it has no `/bin/sh` to wrap with (psmux's adapter builds its windows'
    /// commands itself).
    ///
    /// Local backends pass through too, but **not** because they inherit the
    /// user's interactive `PATH` — that claim used to stand here and was wrong
    /// (see [`resolve_local_program`], which is what makes them safe now). They
    /// are not wrapped because thurbox can resolve a local command itself, and
    /// an absolute path needs no shell's `PATH` at all; a wrap would only add a
    /// second shell whose own quoting rules could differ.
    ///
    /// `/bin/sh -l` reads `~/.profile` but not the user's own shell's files
    /// (`~/.zshenv`, `~/.zprofile`), so the host's login `PATH` is assigned
    /// inside the wrap too ([`crate::agent::host_path`]) — the same `PATH` a
    /// delegated create gives the pane.
    ///
    /// Done here — not via tmux `default-command` — because that value round-trips
    /// through the remote transport's per-arg shell-quoting, where a `-l` flag's
    /// space would be re-split into a stray `set-option` argument.
    pub(in crate::backend) fn login_wrap_for_remote(&self, shell_cmd: &str) -> String {
        if self.transport.is_remote() && self.platform == crate::session::Platform::Posix {
            let path = self
                .host
                .as_ref()
                .and_then(crate::agent::host_path::assignment_for)
                .unwrap_or_default();
            let inner = control_mode::shell_escape(&format!("{path}exec {shell_cmd}"));
            format!("/bin/sh -lc {inner}")
        } else {
            shell_cmd.to_string()
        }
    }

    /// The window command for a **remote/WSL** companion shell pane: the user's
    /// own login shell, interactively — the same environment an `ssh <host>`
    /// login gives you, not a bare `/bin/sh`.
    ///
    /// [`default_shell`](Self::default_shell) returns `/bin/sh` for a remote
    /// Unix host (guaranteed to exist), and the generic
    /// [`login_wrap_for_remote`] would run it as `/bin/sh -lc 'exec /bin/sh'` —
    /// a login-sourced but then bare POSIX shell. That drops everything a real
    /// SSH login loads from the account's shell: its rc files (`~/.bashrc` /
    /// `~/.zshrc`), prompt, aliases, functions, and `PATH` additions. SSH runs
    /// the shell recorded in the user's passwd entry (which `$SHELL` reflects),
    /// so we do the same: bootstrap through the always-present `/bin/sh -l`
    /// (which login-sources the profile and thus exports `$SHELL`), then `exec`
    /// `"$SHELL"` as a **login** shell — tmux gives it a PTY, so it's
    /// interactive and sources the interactive rc chain too. If `$SHELL` is
    /// unset/broken the guard falls back to a plain `/bin/sh -l` so the pane
    /// still opens.
    ///
    /// The fallback is a `command -v` **guard**, never `exec "$SHELL" -l
    /// 2>/dev/null || …`: bash (and zsh) decide interactivity from
    /// `isatty(stdin) && isatty(stderr)`, and an `exec … 2>/dev/null`
    /// redirection **persists** into the exec'd shell — with stderr no longer a
    /// TTY the shell starts **non-interactive** (no prompt, no rc files, no
    /// readline), which reads as a blank "not loading" pane. So we probe
    /// `$SHELL` with `command -v` (whose own `2>/dev/null` is harmless) and only
    /// then `exec` it with all three std streams still on the PTY.
    ///
    /// Windows hosts keep [`default_shell`]'s `powershell` (no `/bin/sh`),
    /// whichever multiplexer serves them; local backends use the platform
    /// default directly.
    pub(in crate::backend) fn remote_shell_pane_command(&self) -> String {
        let inner = control_mode::shell_escape(
            "command -v \"$SHELL\" >/dev/null 2>&1 && exec \"$SHELL\" -l; exec /bin/sh -l",
        );
        format!("/bin/sh -lc {inner}")
    }

    /// The command a new window runs on a server that runs a window command
    /// through a POSIX shell, as the `new-window` line carries it: a POSIX
    /// host's login-shell bootstrap for a companion shell pane, or the program
    /// itself (login-wrapped on a POSIX host). What a tmux window runs; split
    /// out of [`PaneStream::spawn`] so which of the two a window gets is
    /// testable without a server.
    pub(in crate::backend) fn posix_window_command(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
    ) -> String {
        // A remote/WSL companion shell pane (`tbs-` window) opens the user's own
        // interactive login shell — the SSH-login environment — instead of the
        // bare `/bin/sh` the generic login-wrap would produce (see
        // `remote_shell_pane_command`). Agent windows (`tb-`) keep the standard
        // path, and so does a Windows host on any multiplexer: it has no
        // `/bin/sh` to bootstrap from.
        let is_remote_shell_pane = self.transport.is_remote()
            && self.platform == Platform::Posix
            && window_name.starts_with(SHELL_WINDOW_PREFIX);
        if is_remote_shell_pane {
            return self.remote_shell_pane_command();
        }
        let program = self.program_for_window(command);
        let shell_cmd = Self::build_shell_command(&program, args);
        // A remote pane's `PATH` is the host's, restored by the login
        // wrap; a local one is inherited from this process, which need not
        // have the CLI its hooks call on it (see `path_prefix_args`).
        let shell_cmd = match self.transport.is_remote() {
            true => shell_cmd,
            // A shell reads this whole string, so the prefix has to be
            // UTF-8 here; a `PATH` that is not gets no prefix rather than a
            // mangled one (see `path_prefix_args`).
            false => shell_prefix_tokens()
                .map(|tokens| {
                    tokens
                        .into_iter()
                        .chain(std::iter::once(shell_cmd.clone()))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or(shell_cmd),
        };
        self.login_wrap_for_remote(&shell_cmd)
    }

    /// [`WindowLifecycle::create_window`] on this machine's server: a one-shot
    /// `new-window`, no control mode, so nothing attaches to what it opens.
    ///
    /// Returns the new pane's id (`%N`). The window is named after its owner —
    /// which is *not* unique (two sessions can share a name) — so it is stamped
    /// with the owner's id before this returns and every later lookup resolves
    /// that (ADR-25).
    ///
    /// A pane id on stdout outranks a non-zero exit status, which on this path
    /// can belong to a user's tmux hook rather than to the window — see the
    /// read below.
    ///
    /// A server without [`TmuxCompatible::ONE_SHOT_SPAWN_ANSWERS`] is not asked
    /// for the id: it is the one pane under the window's name that was not
    /// there before `new-window` ([`Self::new_named_pane`]). With no id to
    /// weigh, a non-zero status is the whole answer there.
    pub(super) fn create_local_window(
        &self,
        spec: &WindowSpec<'_>,
        retire: bool,
    ) -> Result<String> {
        let session_id = spec.owner.session_id;
        // Ensure the session exists and is configured, without opening a
        // control-mode connection (headless one-shot path).
        let batch_config =
            M::COMMAND_LISTS && M::ONE_SHOT_SPAWN_ANSWERS && M::VERSION_FLOOR.is_none();
        if !batch_config {
            self.ensure_session_configured()?;
        }
        // The headless twin of the check in `spawn`.
        self.admit_server()?;

        let window_name = window_name_for(spec.role, spec.owner.name);
        // Created at the END of the session's window list, so the retention
        // below can name the window this command just made: `{end}` is the last
        // window and `-a` appends after it, so within this one command list
        // `{end}` is exactly the new one. The window's *name* cannot say that —
        // `tb-<session name>` is not unique (two sessions can share a name,
        // which is why the stamp exists), and tmux resolves a duplicate name to
        // the lowest index, which is the older window (measured, tmux 3.2a). A
        // server without the shorthand keeps the plain session target: it gets
        // no retention write either.
        let create_target = if M::ONE_SHOT_SPAWN_ANSWERS {
            format!("{}:{{end}}", self.session)
        } else {
            format!("{}:", self.session)
        };
        // The stamp rides in the same command list as the creation, like the
        // birth options: two `set-option` processes fewer on every `session
        // create` (#1243). `{end}` still names the new window here, and the pane
        // id it would otherwise be written against is not known until the list
        // returns.
        let stamped = M::WINDOW_OPTIONS;
        let before = if M::ONE_SHOT_SPAWN_ANSWERS {
            Vec::new()
        } else {
            self.window_panes(&window_name).with_context(|| {
                format!("list the windows named {window_name} before adding one")
            })?
        };
        let run_new_window = || {
            let mut tmux = self.new_window_command(
                &window_name,
                &create_target,
                spec.command,
                spec.args,
                spec.cwd,
                spec.env,
            )?;
            if stamped {
                for (option, value) in [
                    (WINDOW_SESSION_OPTION, session_id),
                    (WINDOW_ROLE_OPTION, spec.role.as_str()),
                ] {
                    if !value.is_empty() {
                        tmux.args([";", "set-option", "-w", "-t", &create_target, option, value]);
                    }
                }
            }
            tmux.output().map_err(|e| {
                self.transport.launch_failure(
                    &format!(
                        "Failed to run {} new-window for headless spawn",
                        M::MULTIPLEXER.name()
                    ),
                    e,
                )
            })
        };
        let mut output = run_new_window()?;
        // A failed has-session stops the list before new-window. Only a typed
        // absence permits bootstrap and redelivery; hook failures may have
        // already created the pane.
        if batch_config
            && !output.status.success()
            && new_window_pane_id(&output.stdout).is_empty()
            && output_answered::<M>(self.transport.is_ssh(), &output) == Some(Missing::Server)
        {
            self.ensure_session_configured()?;
            output = run_new_window()?;
        }
        let deadline = std::time::Instant::now() + M::COLD_START_GRACE;
        loop {
            if output.status.success()
                || !M::RETRY_NO_SERVER_ERROR
                || output_answered::<M>(self.transport.is_ssh(), &output) != Some(Missing::Server)
                || std::time::Instant::now() >= deadline
            {
                break;
            }
            // A "no server running" reply did not deliver new-window. A
            // different error may mean it did create it.
            std::thread::sleep(std::time::Duration::from_millis(250));
            output = run_new_window()?;
        }
        let pane_id = if M::ONE_SHOT_SPAWN_ANSWERS {
            new_window_pane_id(&output.stdout)
        } else {
            String::new()
        };
        if !output.status.success() {
            // The exit status is not this command's verdict on its own. tmux
            // hands a command-mode client the status of the last `run-shell` its
            // command list triggered, and a *hook* counts: an `after-new-window`
            // left behind by an uninstalled tmux plugin runs a script that is no
            // longer there, `/bin/sh` answers 127, and the client exits 127
            // although `new-window` succeeded and already printed the pane id
            // (measured, tmux 3.5a; stderr is empty, so the message named
            // nothing either). The pane id is the answer to `-P`, so where there
            // is one the window exists and refusing it tears down a session that
            // started fine (issue #1154). The control-mode path never sees this:
            // its reply block carries the `-P` answer alone and no exit status at
            // all.
            if !control_mode::is_valid_pane_id(&pane_id) {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!(
                    "{} new-window exited {} for window {}: {}",
                    M::MULTIPLEXER.name(),
                    output.status,
                    window_name,
                    stderr.trim()
                );
            }
            warn!(
                "{} answered {} for window {} and created it anyway ({}): a hook \
                 on thurbox's own server failed, which is what an uninstalled \
                 plugin's leftover hook does for the life of that server. `{} -L {} \
                 show-hooks -g` names it. Unset the hook rather than killing that \
                 server — it holds every live session",
                M::MULTIPLEXER.name(),
                output.status,
                window_name,
                pane_id,
                self.transport.mux(),
                self.socket()
            );
        }
        let pane_id = if M::ONE_SHOT_SPAWN_ANSWERS {
            pane_id
        } else {
            self.new_named_pane(spec, &window_name, &before)?
        };
        // What stamping does after writing a stamp. A server without window
        // options writes none (ADR-13).
        if stamped && retire {
            let _ = self.retire_duplicate_windows(session_id, spec.role);
        }
        Ok(pane_id)
    }

    /// The pane `new-window` just made on a server that does not answer `-P`:
    /// the one under `window_name` that `before` did not list. psmux does not
    /// reuse pane ids within a server's life, so a new id is the new window,
    /// and a namesake — allowed, and unstamped there (ADR-13) — is never taken
    /// for it. No new id means the window already closed with its program.
    pub(super) fn new_named_pane(
        &self,
        spec: &WindowSpec<'_>,
        window_name: &str,
        before: &[(String, bool)],
    ) -> Result<String> {
        let after = self
            .window_panes(window_name)
            .with_context(|| format!("list the windows named {window_name} after adding one"))?;
        let mut fresh = after
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| !before.iter().any(|(old, _)| old == id));
        match (fresh.next(), fresh.next()) {
            (Some(id), None) => Ok(id),
            (None, _) => Err(self.spawn_exited(spec, window_name)),
            (Some(_), Some(_)) => bail!(
                "more than one window named {window_name} appeared while creating one, so \
                 which is this session's cannot be told"
            ),
        }
    }

    /// The `new-window` command list [`Self::create_local_window`] runs: the
    /// window created detached at `create_target` running `command` — and,
    /// where the server takes them, its birth options chained into the same
    /// invocation.
    pub(super) fn new_window_command(
        &self,
        window_name: &str,
        create_target: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Result<Command> {
        let configure = M::COMMAND_LISTS && M::ONE_SHOT_SPAWN_ANSWERS && M::VERSION_FLOOR.is_none();
        let config = self.session_config();
        let prefix = config_command_list(&["has-session", "-t", &self.session], &config);
        let mut tmux = if configure {
            let mut command = self.mux_command(&prefix)?;
            command.args([";", "new-window", "-d"]);
            command
        } else {
            self.mux_command(&["new-window", "-d"])?
        };
        if M::ONE_SHOT_SPAWN_ANSWERS {
            tmux.arg("-a");
        }
        tmux.args(["-t", create_target, "-n", window_name]);
        if M::ONE_SHOT_SPAWN_ANSWERS {
            tmux.args(["-P", "-F", "#{pane_id}"]);
        }
        if let Some(dir) = cwd {
            tmux.args(["-c", &dir.to_string_lossy()]);
        }
        M::push_window_program(&mut tmux, window_name, command, args, env)?;

        // Chained into the same command list as the creation, not sent after
        // it — `birth_options` has the measurement. This path passes `-d`, so
        // the new window is not current and the bare form the control-mode path
        // uses is not available; `{end}` names it instead, which is why the
        // window is created there.
        if M::WINDOW_SETTINGS && M::ONE_SHOT_SPAWN_ANSWERS {
            for (key, value) in birth_options(window_name) {
                tmux.args([";", "set-window-option", "-t", create_target]);
                tmux.args([key, value]);
            }
        }
        Ok(tmux)
    }

    /// Leave one window carrying `session_id`'s `role` stamp, and say which one
    /// kept it — or `None` when there was never more than one to choose between.
    ///
    /// ADR-25 gives a stamp its meaning under one invariant: one session, one
    /// window per role. Two windows carrying it is not a weaker answer but no
    /// answer at all — [`WindowIndex::stamped_match`] returns [`Located::Unknown`]
    /// by design, and from then on the session cannot be sent to, killed, captured
    /// or renamed. A restart is kill-then-spawn and a repairer relaunches anything
    /// a listing says is gone, so the two overlapping put one stamp on two windows
    /// and the session was lost for good (issue #1207).
    ///
    /// **The highest window id keeps the identity.** The rule is that rather than
    /// "the window I just stamped" because both racers run this: "mine wins" has
    /// each of them retire the other's and can leave the session no window at all,
    /// while a key tmux issues in order and never reissues while the server lives
    /// makes every sweep reach the same verdict from any listing that sees both.
    /// That is also what closes the gap a window opens between being created and
    /// being stamped — the sweep runs *after* each stamp, so the last stamp to land
    /// is followed by a listing that sees every earlier one, whichever order the
    /// windows were created in.
    ///
    /// It is the right half to keep, too. The newest window is the one the most
    /// recent restart asked for, and where both panes resume one conversation it is
    /// the connection that displaced the other — the loser is the pane that printed
    /// "another connection took over this session".
    ///
    /// Liveness is deliberately **not** the key. It changes between two listings
    /// taken a moment apart, so two sweeps could each keep what the other retired,
    /// and a session with no window at all is the one outcome worse than the pair.
    /// A newest window whose pane has already exited is kept and stays on screen
    /// (`remain-on-exit`), which is how the operator gets to see why it exited.
    ///
    /// Over the control connection where there is one, so a host's sweep costs
    /// no ssh handshake of its own; one-shot otherwise, as every other verb a
    /// caller with no connection runs (issue #1635). Only a server with window
    /// options, though: a server
    /// that keeps `@` options in one server-global map (psmux, ADR-13) hands the
    /// same value back for every window, so every window there would read as
    /// stamped for whoever was stamped last; the stamp is withheld at both ends for
    /// that reason (issue #1168) and this must not be the one place that believes
    /// it.
    pub(super) fn retire_duplicate_windows(
        &self,
        session_id: &str,
        role: WindowRole,
    ) -> Option<String> {
        if !M::WINDOW_OPTIONS || session_id.is_empty() {
            return None;
        }
        let listing = self.retire_listing()?;
        let windows = stamped_windows_in(&listing, session_id, role);
        let keep = windows.last()?.clone();
        (!self
            .retire_windows_in(&listing, session_id, role)
            .is_empty())
        .then_some(keep)
    }

    /// The [`RETIRE_FORMAT`] listing, or `None` when the server did not answer.
    pub(super) fn retire_listing(&self) -> Option<String> {
        if self.attached() {
            return self
                .ctrl_command(&format!(
                    "list-windows -t {} -F '{RETIRE_FORMAT}'",
                    self.session
                ))
                .ok();
        }
        let output = self
            .mux_command(&["list-windows", "-t", &self.session, "-F", RETIRE_FORMAT])
            .ok()?
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub(super) fn retire_windows_in(
        &self,
        listing: &str,
        session_id: &str,
        role: WindowRole,
    ) -> Vec<String> {
        if session_id.is_empty() {
            return Vec::new();
        }
        let windows = stamped_windows_in(listing, session_id, role);
        let Some((keep, retire)) = windows.split_last() else {
            return Vec::new();
        };
        let mut retired = Vec::new();
        for window in retire {
            match self.retire_window(window) {
                Ok(()) => {
                    retired.push(window.clone());
                    warn!(
                        "retired {window}: session {session_id}'s {} stamp is held by {keep}",
                        role.as_str()
                    );
                }
                Err(e) => {
                    warn!("could not retire {window}, stamped for session {session_id}: {e:#}")
                }
            }
        }
        retired
    }

    /// Kill a duplicate's window through the control connection where there
    /// is one, one-shot otherwise. One already gone is not an error.
    pub(super) fn retire_window(&self, window: &str) -> Result<()> {
        if !self.attached() {
            return self.kill_window_at(window);
        }
        let (verb, target) = self.kill_command(window)?;
        match self.ctrl_command(&format!("{verb} -t {target}")) {
            Err(e) if !typed_absence(&e) => Err(e),
            _ => Ok(()),
        }
    }

    /// Kill the window `target` is in with a one-shot command rather than
    /// through control mode — on this machine and on a host alike.
    ///
    /// The teardown path's kill. Opening control mode needs
    /// [`ensure_ready`](WindowLifecycle::ensure_ready) — and that *creates* the
    /// server and the thurbox session when they are absent, so tearing a
    /// session down would leave an empty server behind. The window rather than
    /// the pane, so a window somebody split does not keep a process running in
    /// its other pane. One already gone is not an error: the teardown got what
    /// it wanted.
    pub(super) fn kill_window_at(&self, target: &str) -> Result<()> {
        let (verb, target) = self.kill_command(target)?;
        match self.run_tmux(&[verb, "-t", &target]) {
            Err(e) if !typed_absence(&e) => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::tmux_compat::server::tests::TestBackend;

    #[test]
    fn login_wrap_assigns_the_hosts_login_path() {
        let host = crate::session::HostDef {
            name: "login-wrap-path".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        crate::agent::host_path::seed(
            &host,
            Some(crate::agent::host_path::HostEnv {
                home: Some("/home/me".into()),
                base: vec!["/usr/bin".into()],
                shell_login: Some(vec!["/home/me/.local/bin".into(), "/usr/bin".into()]),
                sh_login: None,
                multiplexers: Vec::new(),
            }),
        );
        let backend =
            TestBackend::for_host(&host, crate::session::settings::default_scrollback_lines());
        assert_eq!(
            backend.login_wrap_for_remote("claude"),
            "/bin/sh -lc 'PATH=/home/me/.local/bin:/usr/bin; export PATH; exec claude'"
        );
    }

    /// The commands thurbox runs on a host itself resolve the multiplexer on
    /// the `PATH` its probe found it on, as the window command does (#1461):
    /// a Mac's Homebrew tmux is on the login `PATH`, not sshd's.
    #[test]
    fn a_hosts_multiplexer_commands_carry_its_login_path() {
        let host = crate::session::HostDef {
            name: "mux-command-path".into(),
            destination: "me@mac".into(),
            path_prepend: vec!["/opt/x/bin".into()],
            ..Default::default()
        };
        crate::agent::host_path::seed(
            &host,
            Some(crate::agent::host_path::HostEnv {
                home: Some("/Users/me".into()),
                base: vec!["/usr/bin".into()],
                shell_login: Some(vec!["/opt/homebrew/bin".into(), "/usr/bin".into()]),
                sh_login: None,
                multiplexers: vec![crate::session::Multiplexer::Tmux],
            }),
        );
        let backend =
            TestBackend::for_host(&host, crate::session::settings::default_scrollback_lines());
        let argv: Vec<String> = backend
            .mux_command(&["-V"])
            .expect("command")
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let line = argv.join(" ");
        let assignment = "PATH=/opt/x/bin:/opt/homebrew/bin:/usr/bin; export PATH; ";
        assert!(
            line.contains(&format!("{assignment}exec")),
            "the multiplexer command does not set the host's PATH: {line}"
        );
        assert!(
            line.ends_with(&format!("sh tmux -L {} -V", backend.socket())),
            "the multiplexer runs through the PATH-setting sh: {line}"
        );
    }
}
