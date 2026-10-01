//! An in-memory session backend that behaves like a multiplexer: it holds
//! windows, stamps them, lists them, reports which are alive, and can be made
//! unreachable.
//!
//! It exists to prove that lifecycle follows a row's route. A backend that
//! accepted every call would prove nothing — the four hand-rolled stubs in
//! `src/` pass on silent defaults — so this one keeps the state a real
//! multiplexer keeps and answers from it, and `tests/backend_contract.rs` holds
//! it to the same contract as `TmuxBackend`. It never runs a process and never
//! speaks the tmux command grammar: what reaches it is the trait, and nothing
//! else can.
//!
//! Registered under a route no adapter serves (`local:rmux`, `ssh:<h>:rmux`),
//! it is what a future RMUX or Herdr adapter would be to `session_ops`.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use thurbox::backend::identity::{window_name_for, WindowIndex};
use thurbox::backend::{
    AdoptedSession, DiscoveredSession, Key, Located, Owner, PaneState, Placed, SessionBackend,
    SpawnedSession, WindowRole, WindowSpec,
};
use thurbox::session::Route;

/// What the fake hands a hook to report through, the state word to follow.
pub const SIGNAL_COMMAND: &str = "thurbox-probe-signal";

/// One window, as the fake multiplexer holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// `%N`, issued in order and never reused while the fake lives.
    pub pane: String,
    pub name: String,
    /// The owning row's id as stamped on the window; empty when unstamped.
    pub session: String,
    pub role: WindowRole,
    pub alive: bool,
    /// What the window was asked to run, `command arg…`.
    pub command: String,
    /// Where it was asked to run it.
    pub cwd: Option<String>,
    /// The `PATH` it was opened with, as a real spawn prefixes it.
    pub path: Option<String>,
    /// What reached the pane, in order: typed text, `\n` for each Enter, and
    /// `<name>` for any other key. What a capture reads back.
    pub screen: String,
    /// The last hook state the pane's agent reported through this backend's
    /// own status channel — never a tmux pane option: the fake has none.
    pub hook: Option<String>,
}

#[derive(Default)]
struct State {
    next: u32,
    windows: Vec<Window>,
    unreachable: bool,
    /// Shut down: nothing is readied again.
    closed: bool,
    /// Every call that reached the backend, in order, as `verb detail`.
    calls: Vec<String>,
    /// Hook reports not yet drained by an attached interface.
    hook_events: Vec<(String, String)>,
    /// What the heartbeat runs, while one is kept.
    heartbeat: Option<String>,
}

/// See the module doc.
pub struct RecordingBackend {
    name: String,
    state: Mutex<State>,
}

impl RecordingBackend {
    /// A fake serving `route`, named by it as every registered backend is.
    pub fn new(route: &Route) -> Arc<Self> {
        Arc::new(Self {
            name: route.format(),
            state: Mutex::new(State::default()),
        })
    }

    /// The windows it holds, oldest first.
    pub fn windows(&self) -> Vec<Window> {
        self.state.lock().unwrap().windows.clone()
    }

    /// The windows stamped as `session`'s, whatever their role.
    pub fn windows_of(&self, session: &str) -> Vec<Window> {
        self.windows()
            .into_iter()
            .filter(|w| w.session == session)
            .collect()
    }

    /// Every call it received.
    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }

    /// What reached `pane`, or nothing for a pane it does not hold.
    pub fn screen(&self, pane: &str) -> String {
        self.windows()
            .into_iter()
            .find(|w| w.pane == pane)
            .map(|w| w.screen)
            .unwrap_or_default()
    }

    /// Start the call log over, so a check reads only what follows.
    pub fn forget_calls(&self) {
        self.state.lock().unwrap().calls.clear();
    }

    /// Whether any call named `verb`.
    pub fn called(&self, verb: &str) -> bool {
        self.calls()
            .iter()
            .any(|c| c.split_whitespace().next() == Some(verb))
    }

    /// Make every question fail as an unreachable machine's does, or answer
    /// again.
    pub fn set_reachable(&self, reachable: bool) {
        self.state.lock().unwrap().unreachable = !reachable;
    }

    /// The window's program exits; `remain-on-exit` keeps the window.
    pub fn exit(&self, pane: &str) {
        let mut state = self.state.lock().unwrap();
        if let Some(w) = state.windows.iter_mut().find(|w| w.pane == pane) {
            w.alive = false;
        }
    }

    /// Open a window directly, as something other than thurbox would — the
    /// state a test starts from.
    pub fn open(&self, name: &str, session: &str, role: WindowRole) -> String {
        let mut state = self.state.lock().unwrap();
        let pane = state.issue();
        state.windows.push(Window {
            pane: pane.clone(),
            name: name.to_string(),
            session: session.to_string(),
            role,
            alive: true,
            command: String::new(),
            cwd: None,
            screen: String::new(),
            path: None,
            hook: None,
        });
        pane
    }

    /// An agent hook running in `pane` reports `state` the way this backend's
    /// panes do: into the backend's own record, where an attached interface
    /// drains it live and a headless poll lists it.
    pub fn hook(&self, pane: &str, state: &str) {
        let mut state_ = self.state.lock().unwrap();
        let window = state_
            .windows
            .iter_mut()
            .find(|w| w.pane == pane)
            .unwrap_or_else(|| panic!("no pane {pane} to report from"));
        window.hook = Some(state.to_string());
        state_
            .hook_events
            .push((pane.to_string(), state.to_string()));
    }

    fn lock(&self, call: String) -> Result<std::sync::MutexGuard<'_, State>> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(call);
        if state.unreachable {
            bail!("{}: the machine did not answer", self.name);
        }
        Ok(state)
    }
}

impl State {
    fn index(&self) -> WindowIndex {
        WindowIndex::from_listing(self.windows.iter().map(|w| DiscoveredSession {
            backend_id: w.pane.clone(),
            name: w.name.clone(),
            is_alive: w.alive,
            session: w.session.clone(),
            role: w.role,
        }))
    }

    /// Where `owner`'s `role` window is, by the listing's own rule.
    fn place(&self, owner: Owner<'_>, role: WindowRole) -> Located {
        let index = self.index();
        match role {
            WindowRole::Shell => index.shell_window(owner.session_id, owner.name),
            _ => index.agent_window(owner.session_id, owner.name),
        }
    }

    fn issue(&mut self) -> String {
        let pane = format!("%{}", self.next);
        self.next += 1;
        pane
    }

    /// The live window `pane` names, for input: an exited one accepts none.
    fn input(&mut self, pane: &str) -> Result<&mut Window> {
        let window = match self.windows.iter_mut().find(|w| w.pane == pane) {
            Some(w) => w,
            None => bail!("can't find pane: {pane}"),
        };
        if !window.alive {
            bail!("its pane {pane} has exited and accepts no input");
        }
        Ok(window)
    }

    fn find(&self, pane: &str) -> Result<&Window> {
        match self.windows.iter().find(|w| w.pane == pane) {
            Some(w) => Ok(w),
            None => bail!("can't find pane: {pane}"),
        }
    }

    /// One session, one window per role (ADR-25): the newest window keeps a
    /// stamp two windows carry, as tmux's own sweep decides it.
    fn retire_duplicates(&mut self, session: &str, role: WindowRole) {
        if session.is_empty() {
            return;
        }
        let stamped: Vec<u32> = self
            .windows
            .iter()
            .filter(|w| w.session == session && w.role == role)
            .map(|w| pane_number(&w.pane))
            .collect();
        if let Some(keep) = stamped.iter().max().copied() {
            self.windows.retain(|w| {
                !(w.session == session && w.role == role && pane_number(&w.pane) != keep)
            });
        }
    }
}

fn pane_number(pane: &str) -> u32 {
    pane.trim_start_matches('%').parse().unwrap_or(0)
}

/// The role a window's name implies, for one nobody stamped.
fn role_of(name: &str) -> WindowRole {
    if name.starts_with("tbs-") {
        WindowRole::Shell
    } else if name.starts_with("tbp-") {
        WindowRole::Program
    } else {
        WindowRole::Agent
    }
}

/// A pane's output: the fake never prints, and its stream ends at once. What
/// reaches a pane headlessly is kept on its [`Window::screen`] instead.
struct Silent;

impl Read for Silent {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
}

/// Keystrokes written to an attached stream go nowhere: the verbs are what
/// the fake models.
struct Discard;

impl Write for Discard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SessionBackend for RecordingBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn check_available(&self) -> Result<()> {
        self.lock("check_available".into()).map(drop)
    }

    fn ensure_ready(&self) -> Result<()> {
        let state = self.lock("ensure_ready".into())?;
        if state.closed {
            bail!("{} is shut down", self.name);
        }
        Ok(())
    }

    fn spawn(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        _env: &HashMap<String, String>,
        _rows: u16,
        _cols: u16,
    ) -> Result<SpawnedSession> {
        let mut state = self.lock(format!("spawn {window_name}"))?;
        let pane = state.issue();
        state.windows.push(Window {
            pane: pane.clone(),
            name: window_name.to_string(),
            session: String::new(),
            role: role_of(window_name),
            alive: true,
            command: std::iter::once(command.to_string())
                .chain(args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" "),
            cwd: cwd.map(|p| p.display().to_string()),
            screen: String::new(),
            path: None,
            hook: None,
        });
        Ok(SpawnedSession {
            backend_id: pane,
            output: Box::new(Silent),
            input: Box::new(Discard),
            size: None,
        })
    }

    fn adopt(
        &self,
        backend_id: &str,
        _rows: u16,
        _cols: u16,
        _seed: Option<Vec<u8>>,
    ) -> Result<AdoptedSession> {
        let state = self.lock(format!("adopt {backend_id}"))?;
        state.find(backend_id)?;
        Ok(AdoptedSession {
            output: Box::new(Silent),
            input: Box::new(Discard),
            seed_len: 0,
            size: None,
        })
    }

    fn create_window(&self, spec: &WindowSpec<'_>) -> Result<String> {
        let name = window_name_for(spec.role, spec.owner.name);
        let mut state = self.lock(format!("create_window {name} {}", spec.owner.session_id))?;
        let pane = state.issue();
        state.windows.push(Window {
            pane: pane.clone(),
            name,
            // Stamped as it is created, which is what makes it findable by id.
            session: spec.owner.session_id.to_string(),
            role: spec.role,
            alive: true,
            command: std::iter::once(spec.command.to_string())
                .chain(spec.args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" "),
            cwd: spec.cwd.map(|p| p.display().to_string()),
            screen: String::new(),
            path: spec.env.get("PATH").cloned(),
            hook: None,
        });
        state.retire_duplicates(spec.owner.session_id, spec.role);
        Ok(pane)
    }

    fn locate(&self, owner: Owner<'_>) -> Result<Placed> {
        let state = self.lock(format!("locate {}", owner.session_id))?;
        // Every window here carries its own stamp, so an ambiguous name is
        // left ambiguous: nothing the fake could do would be proof.
        Ok(Placed {
            agent: state.place(owner, WindowRole::Agent),
            shell: state.place(owner, WindowRole::Shell),
        })
    }

    fn rename_windows(&self, owner: Owner<'_>, to: &str) -> Result<()> {
        let mut state = self.lock(format!("rename_windows {} {to}", owner.session_id))?;
        for role in [WindowRole::Agent, WindowRole::Shell] {
            match state.place(owner, role) {
                Located::At(pane) => {
                    let window = state
                        .windows
                        .iter_mut()
                        .find(|w| w.pane == pane)
                        .expect("a placed window is a held one");
                    window.name = window_name_for(role, to);
                }
                Located::Absent => {}
                Located::Unknown => bail!("several windows are named after '{}'", owner.name),
            }
        }
        Ok(())
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        let state = self.lock("discover".into())?;
        Ok(state
            .windows
            .iter()
            .map(|w| DiscoveredSession {
                backend_id: w.pane.clone(),
                name: w.name.clone(),
                is_alive: w.alive,
                session: w.session.clone(),
                role: w.role,
            })
            .collect())
    }

    fn stamp_window(&self, backend_id: &str, session_id: &str, role: WindowRole) -> Result<()> {
        let mut state = self.lock(format!("stamp_window {backend_id} {session_id}"))?;
        state.find(backend_id)?;
        let window = state
            .windows
            .iter_mut()
            .find(|w| w.pane == backend_id)
            .expect("found above");
        if !session_id.is_empty() {
            window.session = session_id.to_string();
        }
        window.role = role;
        state.retire_duplicates(session_id, role);
        Ok(())
    }

    fn window_panes(&self, window_name: &str) -> Result<Vec<(String, bool)>> {
        let state = self.lock(format!("window_panes {window_name}"))?;
        Ok(state
            .windows
            .iter()
            .filter(|w| w.name == window_name)
            .map(|w| (w.pane.clone(), !w.alive))
            .collect())
    }

    fn set_pane_retention(&self, backend_id: &str, keep: bool) -> Result<()> {
        let state = self.lock(format!("set_pane_retention {backend_id} {keep}"))?;
        state.find(backend_id).map(drop)
    }

    fn send_text(&self, pane: &str, text: &str, submit: bool) -> Result<()> {
        let mut state = self.lock(format!("send_text {pane} {text}"))?;
        let window = state.input(pane)?;
        window.screen.push_str(text);
        if submit {
            window.screen.push('\n');
        }
        Ok(())
    }

    fn send_text_after(&self, pane: &str, text: &str, delay: std::time::Duration) -> Result<()> {
        // The fake's time passes at once: what a real timer would type later
        // is on the screen when this returns.
        let mut state = self.lock(format!(
            "send_text_after {pane} {}s {text}",
            delay.as_secs()
        ))?;
        let window = state.input(pane)?;
        window.screen.push_str(text);
        window.screen.push('\n');
        Ok(())
    }

    fn send_key(&self, pane: &str, key: &Key) -> Result<String> {
        let mut state = self.lock(format!("send_key {pane} {}", key.name()))?;
        let window = state.input(pane)?;
        match key.name() {
            "enter" => window.screen.push('\n'),
            other => window.screen.push_str(&format!("<{other}>")),
        }
        Ok(key.name().to_string())
    }

    fn capture(&self, pane: &str, lines: u32, _ansi: bool) -> Result<String> {
        let state = self.lock(format!("capture {pane}"))?;
        let screen = &state.find(pane)?.screen;
        let all: Vec<&str> = screen.lines().collect();
        let keep = all.len().saturating_sub(lines as usize);
        Ok(all[keep..].join("\n"))
    }

    fn pane_state(&self, pane: &str) -> Result<PaneState> {
        let state = self.lock(format!("pane_state {pane}"))?;
        let window = state.find(pane)?;
        Ok(PaneState {
            foreground_process: window.command.split_whitespace().next().map(str::to_string),
            foreground_command: (!window.command.is_empty()).then(|| window.command.clone()),
            foreground_cwd: window.cwd.clone(),
            dead: Some(!window.alive),
            ..PaneState::default()
        })
    }

    fn pane_path(&self, pane: &str) -> Result<Option<String>> {
        let state = self.lock(format!("pane_path {pane}"))?;
        Ok(state.find(pane)?.path.clone())
    }

    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        let state = self.lock(format!("resize {backend_id} {rows}x{cols}"))?;
        state.find(backend_id).map(drop)
    }

    fn is_dead(&self, backend_id: &str) -> Result<bool> {
        let state = self.lock(format!("is_dead {backend_id}"))?;
        Ok(!state.find(backend_id)?.alive)
    }

    fn kill(&self, backend_id: &str) -> Result<()> {
        let mut state = self.lock(format!("kill {backend_id}"))?;
        // Idempotent, as the contract asks: a pane already gone is the
        // outcome a kill wanted.
        state.windows.retain(|w| w.pane != backend_id);
        Ok(())
    }

    fn detach(&self, backend_id: &str) -> Result<()> {
        self.lock(format!("detach {backend_id}")).map(drop)
    }

    fn default_shell(&self) -> String {
        "/bin/sh".to_string()
    }

    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        let state = self.lock(format!("pane_pid {backend_id}"))?;
        let window = state.find(backend_id)?;
        Ok(window.alive.then(|| 10_000 + pane_number(&window.pane)))
    }

    fn pane_pids(&self) -> Result<HashMap<String, u32>> {
        let state = self.lock("pane_pids".into())?;
        Ok(state
            .windows
            .iter()
            .filter(|w| w.alive)
            .map(|w| (w.pane.clone(), 10_000 + pane_number(&w.pane)))
            .collect())
    }

    fn pane_ids(&self) -> Result<HashSet<String>> {
        let state = self.lock("pane_ids".into())?;
        Ok(state.windows.iter().map(|w| w.pane.clone()).collect())
    }

    /// A command of the fake's own, nothing like tmux's: what a backend whose
    /// panes report somewhere other than a pane option would hand the hooks.
    fn hook_signal_command(&self) -> Option<String> {
        Some(format!("{SIGNAL_COMMAND} "))
    }

    fn record_hook_state(&self, pane: &str, state: &str) -> Result<()> {
        let mut state_ = self.lock(format!("record_hook_state {pane} {state}"))?;
        state_.find(pane)?;
        let window = state_
            .windows
            .iter_mut()
            .find(|w| w.pane == pane)
            .expect("found above");
        window.hook = Some(state.to_string());
        state_
            .hook_events
            .push((pane.to_string(), state.to_string()));
        Ok(())
    }

    fn hook_states(&self) -> Result<Vec<(String, String)>> {
        let state = self.lock("hook_states".into())?;
        Ok(state
            .windows
            .iter()
            .filter_map(|w| Some((w.pane.clone(), w.hook.clone()?)))
            .collect())
    }

    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.state.lock().unwrap().hook_events)
    }

    fn ensure_heartbeat(
        &self,
        program: &Path,
        args: &[String],
        every: std::time::Duration,
    ) -> Result<()> {
        let command = std::iter::once(program.display().to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        let mut state = self.lock(format!("ensure_heartbeat {command} {}s", every.as_secs()))?;
        state.heartbeat.get_or_insert(command);
        Ok(())
    }

    fn heartbeat_running(&self) -> Result<bool> {
        Ok(self.lock("heartbeat_running".into())?.heartbeat.is_some())
    }

    fn stop_heartbeat(&self) -> Result<bool> {
        Ok(self
            .lock("stop_heartbeat".into())?
            .heartbeat
            .take()
            .is_some())
    }

    fn shutdown(&self) {
        let mut state = self.state.lock().unwrap();
        state.calls.push("shutdown".into());
        state.closed = true;
    }
}
