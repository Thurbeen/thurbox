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
use thurbox::backend::WindowRole;
use thurbox::backend::{AdoptedSession, DiscoveredSession, SessionBackend, SpawnedSession};
use thurbox::session::Route;

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
}

#[derive(Default)]
struct State {
    next: u32,
    windows: Vec<Window>,
    unreachable: bool,
    /// Every call that reached the backend, in order, as `verb detail`.
    calls: Vec<String>,
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
        });
        pane
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
    fn issue(&mut self) -> String {
        let pane = format!("%{}", self.next);
        self.next += 1;
        pane
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

/// A pane's output: the fake never prints, and its stream ends at once. Pane
/// I/O through the fake is not modelled yet; lifecycle never reads a stream.
struct Silent;

impl Read for Silent {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
}

/// Keystrokes go nowhere; pane I/O is not what this fake is for yet.
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
        self.lock("ensure_ready".into()).map(drop)
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

    fn shutdown(&self) {
        self.state.lock().unwrap().calls.push("shutdown".into());
    }
}
