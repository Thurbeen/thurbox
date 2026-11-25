use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use ratatui::layout::{Position, Rect};

use super::snapshot::Snapshot;
use crate::session::WORKING_QUIET_MS;

mod attach;
pub use attach::Start;
mod fleet;
pub mod links;
mod paint;
mod programs;
mod shells;
pub mod stream_pane;

pub use shells::{ShellOpening, ShellResult};

pub use paint::CursorOn;

pub use links::{drawn_link_paints, paint_hyperlinks, HyperlinkPaint};
pub use programs::{plan_keys, validate_program_name, KeysPlan, ProgramKey, ProgramTransition};

use paint::Painted;
use programs::ProgramSlot;

const SHELL_SUFFIX: &str = "#shell";

// xterm's Cb before the +32 offset: 0 left button, 32 a move with it held, 64/65 the wheel.
fn mouse_report(
    encoding: vt100::MouseProtocolEncoding,
    button: u32,
    col: u32,
    row: u32,
    press: bool,
) -> Option<Vec<u8>> {
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let end = if press { 'M' } else { 'm' };
            Some(format!("\x1b[<{button};{col};{row}{end}").into_bytes())
        }
        vt100::MouseProtocolEncoding::Default | vt100::MouseProtocolEncoding::Utf8 => {
            let button = if press { button } else { 3 };
            let utf8 = encoding == vt100::MouseProtocolEncoding::Utf8;
            let mut out = b"\x1b[M".to_vec();
            for n in [button, col, row] {
                let value = n + 32;
                if utf8 {
                    let c = char::from_u32(value).filter(|_| value <= 2047)?;
                    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                } else {
                    out.push(u8::try_from(value).ok()?);
                }
            }
            Some(out)
        }
    }
}

fn split_surface(surface: &str) -> (&str, bool) {
    match surface.strip_suffix(SHELL_SUFFIX) {
        Some(id) => (id, true),
        None => (surface, false),
    }
}

pub fn shell_surface(session: &str) -> String {
    format!("{session}{SHELL_SUFFIX}")
}

struct Live {
    session: crate::backend::Session,
    agent: Painted,
    shell: Painted,
}

impl Live {
    fn pane(&self, shell: bool) -> Pane<'_> {
        Pane { live: self, shell }
    }
}

#[derive(Clone, Copy)]
struct Pane<'a> {
    live: &'a Live,
    shell: bool,
}

impl<'a> Pane<'a> {
    fn painted(&self) -> &'a Painted {
        if self.shell {
            &self.live.shell
        } else {
            &self.live.agent
        }
    }

    fn parser(&self) -> Option<&'a Arc<std::sync::Mutex<crate::backend::SessionParser>>> {
        match (self.shell, &self.live.session.shell_pane) {
            (true, Some(pane)) => Some(&pane.parser),
            (true, None) => None,
            (false, _) => Some(&self.live.session.parser),
        }
    }

    fn sized_elsewhere(&self) -> bool {
        match (self.shell, &self.live.session.shell_pane) {
            (true, Some(pane)) => pane.sized_elsewhere(),
            (true, None) => false,
            (false, _) => self.live.session.sized_elsewhere(),
        }
    }

    fn retake_size(&self) {
        match (self.shell, &self.live.session.shell_pane) {
            (true, Some(pane)) => pane.retake_size(),
            (true, None) => {}
            (false, _) => self.live.session.retake_size(),
        }
    }

    fn send(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        match (self.shell, &self.live.session.shell_pane) {
            (true, Some(pane)) => pane.send_input(bytes),
            (true, None) => Err(anyhow::anyhow!("this session has no companion shell")),
            (false, _) => self.live.session.send_input(bytes),
        }
    }

    fn resize(&self, rows: u16, cols: u16) -> bool {
        if self.shell {
            self.live.session.resize_shell(rows, cols)
        } else {
            self.live.session.resize(rows, cols)
        }
    }

    fn on_screen(&self) -> bool {
        self.painted().on_screen()
    }

    fn is_dead(&self) -> Option<bool> {
        if self.shell {
            return self.live.session.shell_is_dead()?.ok();
        }
        self.live.session.is_dead().ok()
    }

    fn last_output_at(&self) -> Option<u64> {
        Some(self.wired()?.last_output_at())
    }

    fn content_stamp(&self) -> Option<u64> {
        Some(self.wired()?.content_stamp())
    }

    fn wired(&self) -> Option<&'a crate::backend::pane::WiredPane> {
        if self.shell {
            return self.live.session.shell_pane.as_deref();
        }
        Some(&*self.live.session)
    }

    fn is_resident(&self) -> bool {
        self.wired()
            .is_none_or(crate::backend::pane::WiredPane::is_resident)
    }

    fn output_seq(&self) -> Option<u64> {
        Some(self.wired()?.output_seq())
    }

    fn output_seq_cell(&self) -> Option<&'a std::sync::atomic::AtomicU64> {
        Some(self.wired()?.output_seq_cell())
    }
}

type CachedRows = (u64, std::rc::Rc<Vec<String>>);

pub struct Terminals {
    backends: std::sync::Arc<crate::backend::BackendRegistry>,
    rows_cache: RefCell<HashMap<String, CachedRows>>,
    live: HashMap<String, Live>,
    ready: RefCell<std::collections::HashSet<String>>,
    meta: HashMap<String, AgentMeta>,
    meta_version: u64,
    printing: std::collections::HashSet<String>,
    printing_version: u64,
    shelling: HashMap<String, shells::Shelling>,
    shelled: (Sender<shells::Shelled>, Receiver<shells::Shelled>),
    shell_results: Vec<ShellResult>,
    shells_after_start: HashMap<String, (u16, u16)>,
    runtime: Option<tokio::runtime::Handle>,
    programs: HashMap<ProgramKey, ProgramSlot>,
    program_transitions: Vec<ProgramTransition>,
    keep_hidden: Option<std::time::Duration>,
    scrollback: usize,
    stream_view: Option<stream_pane::View>,
    fleet: HashMap<String, crate::fleet_link::FleetLink>,
    input_notices: (Sender<String>, Receiver<String>),
    app_copy_focus: Option<std::sync::Arc<crate::backend::pane::AppCopy>>,
    app_copy_tokens: u64,
    size_focus: Option<(String, String)>,
    attach: attach::Attach,
}

impl Terminals {
    pub fn view_through(&mut self, view: stream_pane::View) {
        self.stream_view = Some(view);
        self.keep_hidden = None;
    }

    pub fn take_input_notices(&self) -> Vec<String> {
        self.input_notices.1.try_iter().collect()
    }

    pub fn attach_fleet_through(&mut self, link: crate::fleet_link::FleetLink) {
        self.fleet.insert(link.name().to_string(), link);
        self.keep_hidden = None;
    }

    #[doc(hidden)]
    pub fn retry_failed_attaches_after(&mut self, interval: std::time::Duration) {
        self.attach.retry_after = interval;
    }

    pub fn with_registry(
        backends: std::sync::Arc<crate::backend::BackendRegistry>,
        settings: &crate::session::settings::Settings,
    ) -> Self {
        Self {
            backends,
            live: HashMap::new(),
            ready: RefCell::new(std::collections::HashSet::new()),
            meta: HashMap::new(),
            meta_version: 0,
            printing: std::collections::HashSet::new(),
            printing_version: 0,
            shelling: HashMap::new(),
            shelled: std::sync::mpsc::channel(),
            shell_results: Vec::new(),
            shells_after_start: HashMap::new(),
            attach: attach::Attach::new(),
            runtime: tokio::runtime::Handle::try_current().ok(),
            programs: HashMap::new(),
            program_transitions: Vec::new(),
            rows_cache: RefCell::new(HashMap::new()),
            keep_hidden: match settings.hidden_terminal_secs {
                0 => None,
                secs => Some(std::time::Duration::from_secs(secs)),
            },
            scrollback: settings.scrollback_lines,
            stream_view: None,
            fleet: HashMap::new(),
            input_notices: std::sync::mpsc::channel(),
            app_copy_focus: None,
            app_copy_tokens: 0,
            size_focus: None,
        }
    }

    pub fn sync(&mut self, snapshot: &Snapshot, rows: u16, cols: u16) {
        self.collect_attached(rows, cols);
        self.drop_lost_panes(snapshot);
        self.rows_cache
            .borrow_mut()
            .retain(|surface, _| self.surface_parser(surface).is_some());

        self.attach_unresolved(snapshot, rows, cols);
        self.settle_starts(snapshot);
        self.evict_hidden();

        let present: std::collections::HashSet<&str> = snapshot
            .sessions
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        self.live.retain(|id, _| present.contains(id.as_str()));
        self.collect_shells(snapshot);
        self.open_shells_after_start(snapshot);
        let failures = self.attach.failed.len();
        self.attach
            .failed
            .retain(|id, _| present.contains(id.as_str()));
        if self.attach.failed.len() != failures {
            self.mark_failures_changed();
        }
    }

    pub fn keep_hidden_for(&mut self, keep: Option<std::time::Duration>) {
        self.keep_hidden = keep;
    }

    fn evict_hidden(&self) {
        let Some(keep) = self.keep_hidden else {
            return;
        };
        for live in self.live.values() {
            for shell in [false, true] {
                let pane = live.pane(shell);
                if !pane.is_resident() || pane.on_screen() {
                    continue;
                }
                let shown = pane.painted().shown_at.get();
                if shown.is_some_and(|at| at.elapsed() < keep) {
                    continue;
                }
                live.session.evict_pane(shell);
            }
        }
    }

    fn key(&self, backend: &str) -> String {
        backend.to_string()
    }

    fn backend_for(&self, backend: &str) -> Option<Arc<dyn crate::backend::SessionBackend>> {
        let route = crate::session::Route::parse(backend).ok()?;
        self.backends.get(&route).cloned()
    }

    pub fn forget(&mut self, session: &str) {
        self.live.remove(session);
        if self.attach.failed.remove(session).is_some() {
            self.mark_failures_changed();
        }
    }

    #[must_use = "the caller decides whether the keystroke was consumed from this"]
    pub fn send(&self, session: &str, bytes: Vec<u8>) -> bool {
        match self.pane(session) {
            Some(pane) => pane.send(bytes).is_ok(),
            None => false,
        }
    }

    fn pane(&self, surface: &str) -> Option<Pane<'_>> {
        let (id, shell) = split_surface(surface);
        Some(self.live.get(id)?.pane(shell))
    }

    pub fn is_dead(&self, surface: &str) -> Option<bool> {
        self.pane(surface)?.is_dead()
    }

    pub fn take_app_copy(
        &mut self,
        focused: Option<&str>,
    ) -> Vec<crate::backend::pane::AppCopyRequest> {
        let current = focused
            .and_then(|surface| self.pane(surface))
            .and_then(|pane| pane.wired())
            .map(|wired| wired.app_copy());
        let unchanged = match (&self.app_copy_focus, current) {
            (Some(held), Some(now)) => std::sync::Arc::ptr_eq(held, now),
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            let current = current.cloned();
            if let Some(old) = self.app_copy_focus.take() {
                old.blur();
            }
            if let Some(new) = &current {
                self.app_copy_tokens += 1;
                new.focus(self.app_copy_tokens);
            }
            self.app_copy_focus = current;
        }
        self.app_copy_focus
            .as_ref()
            .map(|copy| copy.take())
            .unwrap_or_default()
    }

    pub fn focus(&mut self, surface: Option<&str>) {
        let Some(surface) = surface else {
            self.size_focus = None;
            return;
        };
        let wired = match self.program_key(surface) {
            Some(key) => self.programs.get(key).map(|slot| &*slot.pane),
            None => self.pane(surface).and_then(|pane| pane.wired()),
        };
        let Some(wired) = wired else {
            return;
        };
        if self
            .size_focus
            .as_ref()
            .is_some_and(|(held, pane)| held == surface && pane == wired.backend_id())
        {
            return;
        }
        wired.claim_size();
        self.size_focus = Some((surface.to_string(), wired.backend_id().to_string()));
    }

    fn surface_parser(
        &self,
        surface: &str,
    ) -> Option<(
        &Painted,
        &Arc<std::sync::Mutex<crate::backend::SessionParser>>,
    )> {
        let pane = self.pane(surface)?;
        Some((pane.painted(), pane.parser()?))
    }

    pub fn visible_text(&self, session: &str) -> Option<String> {
        if !self.pane(session)?.is_resident() {
            return None;
        }
        let (_, parser) = self.surface_parser(session)?;
        let parser = parser.lock().ok()?;
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let (rows, cols) = (rows.get(), cols.get());
        let mut out = String::new();
        for row in 0..rows {
            let line = screen.contents_between(row, 0, row, cols);
            out.push_str(line.trim_end());
            out.push('\n');
        }
        Some(out)
    }

    pub fn search_sources(&self, sessions: &[String]) -> Vec<super::search::Source> {
        let scrollback = self.scrollback;
        sessions
            .iter()
            .filter_map(|id| Some((id, self.live.get(id)?)))
            .flat_map(|(id, live)| {
                [false, true].into_iter().filter_map(move |shell| {
                    let pane = live.pane(shell);
                    let wired = pane.wired()?;
                    let restore = (!wired.is_resident()).then(|| {
                        let (backend, pane_id) = live.session.backend_handle();
                        let pane_id = if pane.shell {
                            wired.backend_id().to_string()
                        } else {
                            pane_id
                        };
                        let restore: super::search::Restore = Arc::new(move || {
                            backend
                                .snapshot(&pane_id)
                                .map_err(|e| tracing::debug!(pane = %pane_id, "could not read the pane back: {e:#}"))
                                .ok()
                                .map(|snapshot| crate::backend::pane::parser_from_snapshot(&snapshot, scrollback))
                        });
                        restore
                    });
                    Some(super::search::Source {
                        session: id.clone(),
                        shell,
                        parser: Arc::clone(pane.parser()?),
                        stamp: pane.last_output_at()?,
                        restore,
                    })
                })
            })
            .collect()
    }

    pub fn has_shell(&self, session: &str) -> bool {
        self.live
            .get(session)
            .is_some_and(|live| live.session.shell_pane.is_some())
    }

    pub fn forget_rects(&self) {
        for live in self.live.values() {
            live.agent.rect.set(Rect::default());
            live.shell.rect.set(Rect::default());
        }
        for slot in self.programs.values() {
            slot.painted.rect.set(Rect::default());
        }
    }

    pub fn last_rect(&self, session: &str) -> Option<Rect> {
        if let Some(key) = self.program_key(session) {
            return self
                .programs
                .get(key)
                .filter(|slot| slot.painted.on_screen())
                .map(|slot| slot.painted.rect.get());
        }
        self.pane(session)
            .filter(Pane::on_screen)
            .map(|pane| pane.painted().rect.get())
    }

    pub fn last_top(&self, surface: &str) -> u16 {
        if let Some(key) = self.program_key(surface) {
            return self
                .programs
                .get(key)
                .map_or(0, |slot| slot.painted.top.get());
        }
        self.pane(surface)
            .map_or(0, |pane| pane.painted().top.get())
    }

    fn pane_at(&self, x: u16, y: u16) -> Option<(&str, Pane<'_>)> {
        let position = Position::new(x, y);
        self.live.iter().find_map(|(id, live)| {
            [false, true]
                .into_iter()
                .map(|shell| live.pane(shell))
                .find(|pane| pane.on_screen() && pane.painted().rect.get().contains(position))
                .map(|pane| (id.as_str(), pane))
        })
    }

    pub fn forward_wheel(&self, x: u16, y: u16, up: bool) -> bool {
        let Some((_, pane)) = self.pane_at(x, y) else {
            return false;
        };
        let Some(parser) = pane.parser() else {
            return false;
        };
        let encoding = match parser.lock() {
            Ok(parser) => {
                let screen = parser.screen();
                (screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None)
                    .then(|| screen.mouse_protocol_encoding())
            }
            Err(_) => None,
        };
        let Some(encoding) = encoding else {
            return false;
        };

        let rect = pane.painted().rect.get();
        let col = u32::from(x - rect.x) + 1;
        let row = u32::from(y - rect.y) + u32::from(pane.painted().top.get()) + 1;
        let button = if up { 64 } else { 65 };
        let Some(bytes) = mouse_report(encoding, button, col, row, true) else {
            return false;
        };
        match pane.send(bytes) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("forwarding a wheel tick to the pty failed: {e}");
                false
            }
        }
    }

    pub fn forward_press(&self, x: u16, y: u16) -> Option<String> {
        let (id, pane) = self.pane_at(x, y)?;
        let key = if pane.shell {
            shell_surface(id)
        } else {
            id.to_string()
        };
        self.forward_button(pane, x, y, 0, true, |_| true)
            .then_some(key)
    }

    pub fn forward_motion(&self, surface: &str, x: u16, y: u16) -> bool {
        self.pane(surface).is_some_and(|pane| {
            self.forward_button(pane, x, y, 32, true, |mode| {
                matches!(
                    mode,
                    vt100::MouseProtocolMode::ButtonMotion | vt100::MouseProtocolMode::AnyMotion
                )
            })
        })
    }

    pub fn forward_move(&self, x: u16, y: u16) -> bool {
        let Some((_, pane)) = self.pane_at(x, y) else {
            return false;
        };
        // xterm: 35 is "motion, no button", 3 under the 32 move flag.
        self.forward_button(pane, x, y, 35, true, |mode| {
            mode == vt100::MouseProtocolMode::AnyMotion
        })
    }

    pub fn forward_release(&self, surface: &str, x: u16, y: u16) -> bool {
        self.pane(surface).is_some_and(|pane| {
            self.forward_button(pane, x, y, 0, false, |mode| {
                mode != vt100::MouseProtocolMode::Press
            })
        })
    }

    fn forward_button(
        &self,
        pane: Pane<'_>,
        x: u16,
        y: u16,
        button: u32,
        press: bool,
        wants: impl Fn(vt100::MouseProtocolMode) -> bool,
    ) -> bool {
        let Some(parser) = pane.parser() else {
            return false;
        };
        let asked = match parser.lock() {
            Ok(parser) => {
                let screen = parser.screen();
                let mode = screen.mouse_protocol_mode();
                (mode != vt100::MouseProtocolMode::None && wants(mode))
                    .then(|| screen.mouse_protocol_encoding())
            }
            Err(_) => None,
        };
        let Some(encoding) = asked else {
            return false;
        };

        if !pane.on_screen() {
            return false;
        }
        let rect = pane.painted().rect.get();
        let col = u32::from(x.clamp(rect.x, rect.x + rect.width - 1) - rect.x) + 1;
        let row = u32::from(y.clamp(rect.y, rect.y + rect.height - 1) - rect.y)
            + u32::from(pane.painted().top.get())
            + 1;
        let Some(bytes) = mouse_report(encoding, button, col, row, press) else {
            return false;
        };
        match pane.send(bytes) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("forwarding a button event to the pty failed: {e}");
                false
            }
        }
    }

    pub fn is_attached(&self, session: &str) -> bool {
        self.live.contains_key(session)
    }

    pub fn backend_handle(
        &self,
        session: &str,
    ) -> Option<(Arc<dyn crate::backend::SessionBackend>, String)> {
        self.live
            .get(session)
            .map(|live| live.session.backend_handle())
    }

    pub fn output_stamp(&self, surface: &str) -> Option<u64> {
        if let Some(key) = self.program_key(surface) {
            return self
                .programs
                .get(key)
                .map(|slot| slot.pane.last_output_at());
        }
        self.pane(surface)?.content_stamp()
    }

    pub fn output_seq(&self, surface: &str) -> Option<u64> {
        if let Some(key) = self.program_key(surface) {
            return self.programs.get(key).map(|slot| slot.pane.output_seq());
        }
        self.pane(surface)?.output_seq()
    }

    pub fn output_seq_cell(&self, surface: &str) -> Option<&std::sync::atomic::AtomicU64> {
        if let Some(key) = self.program_key(surface) {
            return self
                .programs
                .get(key)
                .map(|slot| slot.pane.output_seq_cell());
        }
        self.pane(surface)?.output_seq_cell()
    }

    pub fn output_generation(&self) -> u64 {
        let sessions = self.live.values().fold(0u64, |acc, live| {
            acc.wrapping_add(live.session.output_seq()).wrapping_add(
                live.session
                    .shell_pane
                    .as_ref()
                    .map_or(0, |pane| pane.output_seq()),
            )
        });
        self.programs.values().fold(sessions, |acc, slot| {
            acc.wrapping_add(slot.pane.output_seq())
        })
    }

    pub fn visible_output_generation(&self) -> u64 {
        let sessions = self.live.values().fold(0u64, |acc, live| {
            let agent = if live.agent.on_screen() {
                live.session.output_seq()
            } else {
                0
            };
            let shell = if live.shell.on_screen() {
                live.session
                    .shell_pane
                    .as_ref()
                    .map_or(0, |pane| pane.output_seq())
            } else {
                0
            };
            acc.wrapping_add(agent).wrapping_add(shell)
        });
        self.programs
            .values()
            .filter(|slot| slot.painted.on_screen())
            .fold(sessions, |acc, slot| {
                acc.wrapping_add(slot.pane.output_seq())
            })
    }

    pub fn millis_since_output(&self, session: &str) -> Option<u64> {
        self.live
            .get(session)
            .map(|live| live.session.millis_since_last_output())
    }

    pub fn sync_printing(&mut self) {
        let printing: std::collections::HashSet<String> = self
            .live
            .iter()
            .filter(|(_, live)| live.session.millis_since_last_output() <= WORKING_QUIET_MS)
            .map(|(session, _)| session.clone())
            .collect();
        if printing != self.printing {
            self.printing = printing;
            self.printing_version = self.printing_version.wrapping_add(1);
        }
    }

    pub fn printing(&self) -> &std::collections::HashSet<String> {
        &self.printing
    }

    pub fn printing_version(&self) -> u64 {
        self.printing_version
    }

    pub fn meta(&mut self) -> &HashMap<String, AgentMeta> {
        self.sync_meta();
        self.meta_map()
    }

    pub fn sync_meta(&mut self) {
        let mut moved = false;
        for (id, live) in &mut self.live {
            if let Some((activity, notification)) = live.session.sync_agent_meta() {
                let entry = self.meta.entry(id.clone()).or_default();
                if entry.activity != activity || entry.notification != notification {
                    entry.activity = activity;
                    entry.notification = notification;
                    moved = true;
                }
            }
        }
        let before = self.meta.len();
        self.meta.retain(|id, _| self.live.contains_key(id));
        if moved || self.meta.len() != before {
            self.mark_meta_changed();
        }
    }

    pub fn meta_map(&self) -> &HashMap<String, AgentMeta> {
        &self.meta
    }

    pub fn meta_version(&self) -> u64 {
        self.meta_version
    }

    fn mark_meta_changed(&mut self) {
        self.meta_version = self.meta_version.wrapping_add(1);
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentMeta {
    pub activity: Option<String>,
    pub notification: Option<String>,
}

#[cfg(test)]
mod decoupling;

#[cfg(test)]
mod tests {
    use super::attach::{Attached, ATTACH_RETRY_INTERVAL};
    use super::paint::{grid_top, paint_sized_elsewhere, screen_widget, FromRow};
    use super::*;
    use crate::kernel::snapshot::SessionRow;

    #[test]
    fn repeated_offline_attach_results_reuse_the_interface() {
        use crate::kernel::host::{Epoch, LuaHost, Published, RenderContext};

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("mods")).unwrap();
        std::fs::write(
            dir.path().join("mods/probe.lua"),
            r#"return { name = "probe", slot = "center", pure = true,
                render = function()
                    return { text = thurbox.sessions[1].attach_error or "connected" }
                end }"#,
        )
        .unwrap();
        let host = LuaHost::new(dir.path());
        assert!(host.error.is_none(), "{:?}", host.error);
        let mut terminals = Terminals::with_registry(
            Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let snapshot = Snapshot {
            sessions: (0..10)
                .map(|i| row(&format!("offline-{i}"), "ssh:offline:tmux", Some("%1")))
                .collect(),
            ..Default::default()
        };
        let themes = crate::kernel::theme::Themes::load(None);
        let registry = crate::kernel::registry::Registry::default();
        let diffs = crate::kernel::diff::DiffStore::new();
        let repos = crate::kernel::repos::RepoStore::new();
        let render = |terminals: &Terminals| {
            host.publish(&Published {
                epoch: Epoch {
                    failed: terminals.failed_version(),
                    ..Default::default()
                },
                snapshot: &snapshot,
                attach_errors: &terminals.failures(),
                starts: &Default::default(),
                inflight: &[],
                themes: &themes,
                registry: &registry,
                diffs: &diffs,
                links: &Default::default(),
                search: None,
                meta: &Default::default(),
                metrics: &Default::default(),
                status_rows: 0,
                can_open: false,
                inventory: &[],
                ui_dir: "ui",
                settings: &Default::default(),
                repos: &repos,
                wants: &Default::default(),
                focus: None,
                selection: None,
                hovered: None,
                printing: &Default::default(),
            })
            .unwrap();
            format!(
                "{:?}",
                host.render(
                    0,
                    RenderContext {
                        width: 80,
                        height: 24,
                        focused: false,
                        elapsed: 0.0,
                        frame: 0,
                    }
                )
                .unwrap()
                .node
            )
        };
        let complete = |terminals: &mut Terminals, id: &str, message: &str| {
            terminals
                .attach
                .attached
                .0
                .send(Attached {
                    session: id.into(),
                    pane: "%1".into(),
                    backend: "ssh:offline:tmux".into(),
                    readied: false,
                    session_handle: Err(message.into()),
                    started: std::time::Instant::now(),
                    refused_in: None,
                })
                .unwrap();
            terminals.sync(&snapshot, 24, 80);
        };
        for row in &snapshot.sessions {
            complete(&mut terminals, &row.id, "host unreachable");
        }
        assert!(render(&terminals).contains("host unreachable"));
        let version = terminals.failed_version();
        let skipped = host.skipped_renders();
        let reused = host.reused_groups();
        for _ in 0..3 {
            for row in &snapshot.sessions {
                terminals.attach.failed.get_mut(&row.id).unwrap().at -= ATTACH_RETRY_INTERVAL;
                complete(&mut terminals, &row.id, "host unreachable");
                assert!(render(&terminals).contains("host unreachable"));
            }
        }
        println!("30 repeated offline results: {} failure invalidations, {} cached renders, {} reused groups",
            terminals.failed_version() - version, host.skipped_renders() - skipped,
            host.reused_groups() - reused);
        assert_eq!(host.skipped_renders() - skipped, 30);
        assert_eq!(terminals.failed_version(), version);

        complete(&mut terminals, "offline-0", "permission denied");
        assert!(render(&terminals).contains("permission denied"));
        assert_eq!(terminals.failed_version(), version + 1);
        terminals.forget("offline-0");
        assert!(render(&terminals).contains("connected"));
        assert_eq!(terminals.failed_version(), version + 2);
    }

    #[test]
    fn retry_bookkeeping_is_private_to_the_terminal_store() {
        let mut terminals = Terminals::with_registry(
            Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.fail("a", Some("%1".into()), "host unreachable".into());
        let version = terminals.failed_version();
        terminals.attach.failed.get_mut("a").unwrap().at -= ATTACH_RETRY_INTERVAL;
        let retry_started = std::time::Instant::now();
        terminals.fail("a", Some("%2".into()), "host unreachable".into());
        assert_eq!(terminals.failed_version(), version);
        let failure = &terminals.attach.failed["a"];
        assert_eq!(failure.pane.as_deref(), Some("%2"));
        assert!(
            failure.at >= retry_started,
            "a retry must restart its cooldown"
        );
        terminals.fail("b", Some("%2".into()), "host unreachable".into());
        assert_eq!(terminals.failed_version(), version + 1);
        terminals.sync(&Snapshot::default(), 24, 80);
        assert!(terminals.failures().is_empty());
        assert_eq!(terminals.failed_version(), version + 2);
    }

    #[test]
    fn a_grid_taller_than_its_rect_shows_its_bottom() {
        use ratatui::widgets::Widget;
        let mut parser = vt100::Parser::new(6.try_into().unwrap(), 4.try_into().unwrap(), 0);
        parser.process(b"r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5");
        let area = Rect::new(0, 0, 4, 4);
        let top = grid_top(parser.screen(), area);
        assert_eq!(top, 2);
        let view = FromRow {
            screen: parser.screen(),
            top,
        };
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        screen_widget(&view, true).render(area, &mut buffer);
        let rows: Vec<String> = (0..4)
            .map(|y| (0..2).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        assert_eq!(rows, ["r2", "r3", "r4", "r5"]);
        assert_eq!(
            buffer[(2, 3)].symbol(),
            "\u{2588}",
            "the cursor, moved up by top"
        );

        use crate::kernel::selection::{PaneBounds, Selection, TermPos};
        let selection = Selection {
            anchor: TermPos { row: 0, col: 0 },
            cursor: TermPos { row: 1, col: 3 },
            dragging: false,
            pane: PaneBounds::from_rect(area),
        };
        assert_eq!(
            crate::kernel::selection::extract_text_from_rows(
                parser.screen(),
                &selection,
                (0, 0),
                top
            ),
            "r2\nr3"
        );
    }

    #[test]
    fn a_pane_sized_elsewhere_says_whose_size_it_is() {
        for width in [80u16, 12] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 5)).unwrap();
            terminal
                .draw(|frame| paint_sized_elsewhere(frame, frame.area(), (3, 60)))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let bottom: String = (0..width).map(|x| buffer[(x, 4)].symbol()).collect();
            let full = " 60\u{d7}3 \u{b7} sized by another thurbox \u{b7} type here to resize ";
            let shown: String = full.chars().take(usize::from(width)).collect();
            assert!(bottom.trim_end().ends_with(shown.trim_end()), "{bottom:?}");
            let above: String = (0..width).map(|x| buffer[(x, 3)].symbol()).collect();
            assert_eq!(above.trim(), "", "only the bottom row is painted");
        }
    }

    #[test]
    fn only_a_terminal_that_holds_focus_paints_its_cursor() {
        use ratatui::widgets::Widget;
        let mut parser = vt100::Parser::new(2.try_into().unwrap(), 8.try_into().unwrap(), 0);
        parser.process(b"$ ");
        for (cursor, painted) in [(true, "\u{2588}"), (false, " ")] {
            let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 8, 2));
            let view = FromRow {
                screen: parser.screen(),
                top: 0,
            };
            screen_widget(&view, cursor).render(buffer.area, &mut buffer);
            assert_eq!(buffer[(2, 0)].symbol(), painted, "cursor shown: {cursor}");
            assert_eq!(buffer[(0, 0)].symbol(), "$", "the grid paints either way");
        }
    }

    #[test]
    fn a_mouse_event_is_encoded_the_way_the_pane_asked_for_it() {
        use vt100::MouseProtocolEncoding as E;
        assert_eq!(
            mouse_report(E::Sgr, 64, 12, 3, true).expect("sgr"),
            b"\x1b[<64;12;3M".to_vec()
        );
        assert_eq!(
            mouse_report(E::Sgr, 65, 12, 3, true).expect("sgr"),
            b"\x1b[<65;12;3M".to_vec()
        );
        // SGR keeps the button on a release; the final letter is what flips.
        assert_eq!(
            mouse_report(E::Sgr, 0, 12, 3, false).expect("sgr"),
            b"\x1b[<0;12;3m".to_vec()
        );
        // The original `CSI M` encoding offsets every field by 32.
        assert_eq!(
            mouse_report(E::Default, 64, 12, 3, true).expect("default"),
            vec![0x1b, b'[', b'M', 96, 44, 35]
        );
        // The original encoding spells every release button 3.
        assert_eq!(
            mouse_report(E::Default, 0, 12, 3, false).expect("default"),
            vec![0x1b, b'[', b'M', 35, 44, 35]
        );
        // Past 223 the original encoding has no legal report.
        assert_eq!(mouse_report(E::Default, 64, 224, 3, true), None);
        assert!(mouse_report(E::Default, 64, 223, 223, true).is_some());
        // `?1005` writes each field as a UTF-8 character, so the 223 cap lifts.
        assert_eq!(
            mouse_report(E::Utf8, 64, 12, 3, true).expect("utf8"),
            b"\x1b[M`,#"
        );
        assert_eq!(
            mouse_report(E::Utf8, 0, 300, 3, false).expect("utf8"),
            "\x1b[M#\u{14c}#".as_bytes()
        );
    }

    pub(super) fn row(id: &str, backend: &str, backend_id: Option<&str>) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            name: "demo".to_string(),
            agent: "claude".to_string(),
            status: crate::session::SessionState::Idle,
            cwd: None,
            repo: None,
            repos: Vec::new(),
            branch: None,
            base_branch: None,
            backend: backend.to_string(),
            backend_id: backend_id.map(str::to_string),
            remote_host: None,
            place: None,
            agent_session_id: None,
            parent_id: None,
            display_order: None,
            worktree_count: 0,
            git: None,
            stopped: false,
            hook_state: None,
            key: None,
            reports_as: None,
            detected_agent: None,
            shell_backend_id: None,
            member_dirs: Vec::new(),
            meta: Default::default(),
            deleting: None,
            state_why: None,
            status_source: None,
        }
    }

    #[cfg(unix)]
    struct FakeLivenessBackend(std::sync::atomic::AtomicU8);

    #[cfg(unix)]
    impl crate::backend::PaneStream for FakeLivenessBackend {
        fn spawn(
            &self,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<&std::path::Path>,
            _: &HashMap<String, String>,
            _: u16,
            _: u16,
        ) -> crate::backend::contract::BackendResult<crate::backend::SpawnedSession> {
            unreachable!()
        }

        fn adopt(
            &self,
            _: &str,
            _: u16,
            _: u16,
            _: Option<Vec<u8>>,
        ) -> crate::backend::contract::BackendResult<crate::backend::AdoptedSession> {
            unreachable!()
        }

        fn resize(&self, _: &str, _: u16, _: u16) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn detach(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    impl crate::backend::WindowLifecycle for FakeLivenessBackend {
        fn pane_state(
            &self,
            _: &str,
        ) -> crate::backend::contract::BackendResult<crate::backend::PaneState> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }

        fn pane_path(&self, _: &str) -> crate::backend::contract::BackendResult<Option<String>> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }

        fn create_window(
            &self,
            _: &crate::backend::WindowSpec<'_>,
        ) -> crate::backend::contract::BackendResult<crate::backend::PaneId> {
            Err(anyhow::anyhow!(
                "{}: a stub opens no windows",
                crate::backend::SessionBackend::name(self)
            )
            .into())
        }

        fn locate(
            &self,
            _: crate::backend::Owner<'_>,
        ) -> crate::backend::contract::BackendResult<crate::backend::Placed> {
            Ok(crate::backend::Placed {
                agent: crate::backend::Located::Absent,
                shell: crate::backend::Located::Absent,
            })
        }

        fn rename_windows(
            &self,
            _: crate::backend::Owner<'_>,
            _: &str,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn stamp_window(
            &self,
            _: &str,
            _: &str,
            _: crate::backend::WindowRole,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn window_panes(
            &self,
            _: &str,
        ) -> crate::backend::contract::BackendResult<Vec<(String, bool)>> {
            Ok(Vec::new())
        }

        fn set_pane_retention(
            &self,
            _: &str,
            _: bool,
        ) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn shutdown(&self) {}

        fn default_shell(&self) -> String {
            "/bin/sh".to_string()
        }

        fn check_available(&self) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn ensure_ready(&self) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn discover(
            &self,
        ) -> crate::backend::contract::BackendResult<Vec<crate::backend::DiscoveredSession>>
        {
            match self.0.load(std::sync::atomic::Ordering::Relaxed) {
                0 => Ok(Vec::new()),
                1 => Err(crate::backend::BackendError::Unreachable(
                    "host unreachable".into(),
                )),
                3 => Err(crate::backend::BackendError::Refused(
                    "permission denied".into(),
                )),
                _ => Ok(vec![crate::backend::DiscoveredSession {
                    backend_id: "%1".into(),
                    name: "tb-demo".into(),
                    is_alive: false,
                    session: "a".into(),
                    role: crate::backend::WindowRole::Agent,
                }]),
            }
        }

        fn is_dead(&self, _: &str) -> crate::backend::contract::BackendResult<bool> {
            Ok(false)
        }

        fn kill(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
            Ok(())
        }

        fn pane_pid(&self, _: &str) -> crate::backend::contract::BackendResult<Option<u32>> {
            Ok(None)
        }
    }

    #[cfg(unix)]
    impl crate::backend::PaneIo for FakeLivenessBackend {
        fn send_text(
            &self,
            _: &str,
            _: &str,
            _: bool,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn send_text_after(
            &self,
            _: &str,
            _: &str,
            _: std::time::Duration,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn send_key(
            &self,
            _: &str,
            _: &crate::backend::Key,
        ) -> crate::backend::contract::BackendResult<String> {
            Err(anyhow::anyhow!("this stub has no panes to type into").into())
        }

        fn capture(
            &self,
            _: &str,
            _: u32,
            _: bool,
        ) -> crate::backend::contract::BackendResult<String> {
            Err(anyhow::anyhow!("this stub has no panes to read").into())
        }
    }

    #[cfg(unix)]
    impl crate::backend::StatusChannel for FakeLivenessBackend {
        fn hook_signal_command(&self) -> Option<String> {
            None
        }

        fn record_hook_state(
            &self,
            _: &str,
            _: &str,
        ) -> crate::backend::contract::BackendResult<()> {
            Err(anyhow::anyhow!("this stub has no status channel").into())
        }

        fn hook_states(&self) -> crate::backend::contract::BackendResult<Vec<(String, String)>> {
            Err(anyhow::anyhow!("this stub has no status channel").into())
        }

        fn take_hook_state_events(&self) -> Vec<(String, String)> {
            Vec::new()
        }
    }

    #[cfg(unix)]
    impl crate::backend::SessionBackend for FakeLivenessBackend {
        fn name(&self) -> &str {
            "fake"
        }
        fn capabilities(&self) -> crate::backend::Capabilities {
            crate::backend::Capabilities::NONE
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_local_tmux_row_attaches_through_the_stream_view() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("viewed");
        let program = dir.path().join("runtime-mux");
        std::fs::write(
            &program,
            format!("#!/bin/sh\nprintf '%s ' \"$@\" > '{}'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend: std::sync::Arc<dyn crate::backend::SessionBackend> =
            std::sync::Arc::new(FakeLivenessBackend(std::sync::atomic::AtomicU8::new(0)));
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let route = crate::session::Route::local(crate::session::Multiplexer::Tmux);
        std::sync::Arc::get_mut(&mut terminals.backends)
            .expect("unshared registry")
            .register(route.clone(), backend);
        terminals.view_through(stream_pane::View {
            program,
            socket: "thurbox-test".into(),
            scrollback: 1000,
        });
        let row = row("a", &route.format(), Some("%1"));
        assert_eq!(row.backend, "local:tmux");
        terminals.start_attach(&row, "%1".into(), 24, 80);
        let _ = terminals
            .attach
            .attached
            .1
            .recv_timeout(std::time::Duration::from_secs(10));
        let argv = std::fs::read_to_string(&marker)
            .expect("the attach did not go through the stream view");
        assert!(argv.contains("view --socket thurbox-test"), "{argv}");
        assert!(argv.contains("--pane %1"), "{argv}");
    }

    fn snapshot(rows: Vec<SessionRow>) -> Snapshot {
        Snapshot {
            sessions: rows,
            taken_at_ms: 0,
            ..Snapshot::default()
        }
    }

    #[test]
    fn a_session_with_no_pane_is_recorded_rather_than_retried() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.sync(&snapshot(vec![row("a", "local:tmux", None)]), 24, 80);
        assert!(!terminals.is_attached("a"));
        assert_eq!(terminals.failure("a"), Some("session has no pane yet"));
        assert_eq!(terminals.attach.failed["a"].pane, None);
    }

    #[test]
    fn a_paused_fleet_row_whose_stream_ended_says_it_is_paused_and_attaches_once_it_runs() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.attach_fleet_through(crate::fleet_link::FleetLink::local("/nonexistent/fleet"));
        let id = format!("{}:t1", crate::session::settings::IMPLIED_FLEET);
        let mut fleet_row = row(&id, super::fleet::BACKEND, None);
        fleet_row.key = Some("k1".to_string());
        fleet_row.stopped = true;
        fleet_row.status = crate::session::SessionState::Stopped;
        terminals.fail(&id, None, "session has no pane yet".to_string());

        terminals.sync(&snapshot(vec![fleet_row.clone()]), 24, 80);
        assert_eq!(terminals.failure(&id), Some(attach::PAUSED));
        assert!(!terminals.attach.attaching.contains_key(&id));

        fleet_row.stopped = false;
        fleet_row.status = crate::session::SessionState::Working;
        terminals.sync(&snapshot(vec![fleet_row]), 24, 80);
        assert!(terminals.attach.attaching.contains_key(&id));
    }

    #[test]
    fn a_start_stays_pending_while_its_row_is_paused_and_ends_on_an_attach_failure_after_it() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.attach_fleet_through(crate::fleet_link::FleetLink::local("/nonexistent/fleet"));
        let id = format!("{}:t1", crate::session::settings::IMPLIED_FLEET);
        let mut fleet_row = row(&id, super::fleet::BACKEND, None);
        fleet_row.key = Some("k1".to_string());
        fleet_row.stopped = true;
        fleet_row.status = crate::session::SessionState::Stopped;
        terminals.fail(&id, Some("k1".to_string()), "an older failure".to_string());

        terminals.start_requested(&id);
        terminals.sync(&snapshot(vec![fleet_row.clone()]), 24, 80);
        assert!(terminals.starting());
        assert_eq!(terminals.starts().get(&id), Some(&Start::Pending));

        fleet_row.stopped = false;
        fleet_row.status = crate::session::SessionState::Working;
        terminals.fail(&id, Some("k1".to_string()), "no live view".to_string());
        terminals.sync(&snapshot(vec![fleet_row.clone()]), 24, 80);
        assert!(!terminals.starting());
        assert!(terminals.starts().is_empty());

        terminals.start_requested(&id);
        terminals.start_failed(&id, "the lease is held elsewhere");
        terminals.sync(&snapshot(vec![fleet_row]), 24, 80);
        assert!(!terminals.starting());
        assert_eq!(
            terminals.starts().get(&id),
            Some(&Start::Failed("the lease is held elsewhere".to_string()))
        );

        terminals.sync(&snapshot(Vec::new()), 24, 80);
        assert!(terminals.starts().is_empty());
    }

    #[test]
    fn a_resume_after_a_refused_start_reads_as_starting_again_once_the_row_runs() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.attach_fleet_through(crate::fleet_link::FleetLink::local("/nonexistent/fleet"));
        let id = format!("{}:t1", crate::session::settings::IMPLIED_FLEET);
        let mut fleet_row = row(&id, super::fleet::BACKEND, None);
        fleet_row.key = Some("k1".to_string());
        fleet_row.stopped = true;
        fleet_row.status = crate::session::SessionState::Stopped;
        terminals.start_requested(&id);
        terminals.start_failed(&id, "the lease is held elsewhere");
        terminals.sync(&snapshot(vec![fleet_row.clone()]), 24, 80);
        assert_eq!(terminals.failure(&id), Some(attach::PAUSED));

        fleet_row.stopped = false;
        fleet_row.status = crate::session::SessionState::Working;
        terminals.sync(&snapshot(vec![fleet_row]), 24, 80);
        assert_eq!(terminals.starts().get(&id), Some(&Start::Pending));
        assert_eq!(terminals.failure(&id), None);
    }

    #[test]
    fn a_paused_row_that_ends_or_is_lost_does_not_read_as_starting() {
        for (state_why, status) in [
            (
                super::super::snapshot::fleet::SESSION_ENDED,
                crate::session::SessionState::Failed,
            ),
            ("lost · cannot resume", crate::session::SessionState::Failed),
        ] {
            let mut terminals = Terminals::with_registry(
                std::sync::Arc::new(crate::backend::registry::inert()),
                &crate::session::settings::Settings::default(),
            );
            terminals
                .attach_fleet_through(crate::fleet_link::FleetLink::local("/nonexistent/fleet"));
            let id = format!("{}:t1", crate::session::settings::IMPLIED_FLEET);
            let mut fleet_row = row(&id, super::fleet::BACKEND, None);
            fleet_row.key = Some("k1".to_string());
            fleet_row.stopped = true;
            fleet_row.status = crate::session::SessionState::Stopped;
            terminals.sync(&snapshot(vec![fleet_row.clone()]), 24, 80);
            assert_eq!(terminals.failure(&id), Some(attach::PAUSED));

            fleet_row.stopped = false;
            fleet_row.status = status;
            fleet_row.state_why = Some(state_why.to_string());
            terminals.sync(&snapshot(vec![fleet_row]), 24, 80);
            assert!(!terminals.starting(), "{state_why}");
            assert!(terminals.starts().is_empty(), "{state_why}");
        }
    }

    #[test]
    fn a_resume_fleet_runs_neither_attaches_nor_restarts_its_wait() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.attach_fleet_through(crate::fleet_link::FleetLink::local("/nonexistent/fleet"));
        terminals.attach.start_wait = std::time::Duration::ZERO;
        let id = format!("{}:t1", crate::session::settings::IMPLIED_FLEET);
        let mut fleet_row = row(&id, super::fleet::BACKEND, None);
        fleet_row.key = Some("k1".to_string());
        fleet_row.stopped = true;
        fleet_row.status = crate::session::SessionState::Stopped;
        fleet_row.state_why = Some(super::super::snapshot::fleet::RESUMING.to_string());

        for _ in 0..3 {
            terminals.sync(&snapshot(vec![fleet_row.clone()]), 24, 80);
            assert_eq!(terminals.failure(&id), None);
            assert!(!terminals.attach.attaching.contains_key(&id));
        }
        assert_eq!(
            terminals.starts().get(&id),
            Some(&Start::Failed(
                "Fleet did not resume it within 0 s".to_string()
            ))
        );
        let settled = terminals.failed_version();
        terminals.sync(&snapshot(vec![fleet_row]), 24, 80);
        assert_eq!(terminals.failed_version(), settled);
    }

    #[test]
    fn a_start_with_no_pane_after_the_wait_fails_and_stops_the_clock() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.attach_fleet_through(crate::fleet_link::FleetLink::local("/nonexistent/fleet"));
        let id = format!("{}:t1", crate::session::settings::IMPLIED_FLEET);
        let mut fleet_row = row(&id, super::fleet::BACKEND, None);
        fleet_row.key = Some("k1".to_string());
        fleet_row.stopped = true;
        fleet_row.status = crate::session::SessionState::Stopped;
        terminals.attach.start_wait = std::time::Duration::ZERO;

        terminals.start_requested(&id);
        let version = terminals.failed_version();
        terminals.sync(&snapshot(vec![fleet_row]), 24, 80);

        assert!(!terminals.starting());
        assert_eq!(
            terminals.starts().get(&id),
            Some(&Start::Failed(
                "Fleet did not resume it within 0 s".to_string()
            ))
        );
        assert_ne!(terminals.failed_version(), version);
    }

    #[test]
    fn an_attach_begun_before_a_start_does_not_end_it_by_failing() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.attach_fleet_through(crate::fleet_link::FleetLink::local("/nonexistent/fleet"));
        let id = format!("{}:t1", crate::session::settings::IMPLIED_FLEET);
        let mut fleet_row = row(&id, super::fleet::BACKEND, None);
        fleet_row.key = Some("k1".to_string());
        fleet_row.status = crate::session::SessionState::Failed;
        terminals.fail(&id, Some("k1".to_string()), "held off".to_string());
        let begun = std::time::Instant::now();

        terminals.start_requested(&id);
        terminals
            .attach
            .attaching
            .insert(id.clone(), super::fleet::BACKEND.to_string());
        terminals
            .attach
            .attached
            .0
            .send(attach::Attached {
                session: id.clone(),
                pane: "k1".into(),
                backend: super::fleet::BACKEND.into(),
                readied: false,
                session_handle: Err("the window is gone".into()),
                started: begun,
                refused_in: None,
            })
            .unwrap();
        terminals.sync(&snapshot(vec![fleet_row]), 24, 80);

        assert_eq!(terminals.starts().get(&id), Some(&Start::Pending));
        assert!(
            terminals.attach.attaching.contains_key(&id),
            "a failure from before the start held off the attach it waits for"
        );
    }

    #[test]
    fn an_unknown_backend_is_reported_not_panicked() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.sync(
            &snapshot(vec![row("a", "ssh:nowhere:tmux", Some("%1"))]),
            24,
            80,
        );
        assert!(!terminals.is_attached("a"));
        let message = terminals.failure("a").unwrap_or_default().to_string();
        assert!(message.contains("no backend"), "{message}");
        assert_eq!(
            terminals.attach.failed["a"].pane.as_deref(),
            Some("%1"),
            "the pane that failed is remembered, so the same one is not retried"
        );
    }

    #[test]
    fn a_vanished_session_is_dropped() {
        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        terminals.sync(&snapshot(vec![row("a", "local:tmux", None)]), 24, 80);
        assert!(terminals.attach.failed.contains_key("a"));

        terminals.sync(&snapshot(Vec::new()), 24, 80);
        assert!(terminals.attach.failed.is_empty(), "state must not leak");
        assert!(terminals.live.is_empty());
    }

    #[test]
    fn sending_to_an_unattached_session_reports_failure() {
        let terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        assert!(!terminals.send("nope", vec![b'x']));
    }

    #[test]
    fn an_unattached_session_has_no_output_age() {
        let terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        assert_eq!(terminals.millis_since_output("nope"), None);
    }
}
