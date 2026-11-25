use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use super::{shell_surface, Live, Painted, Terminals};
use crate::kernel::layout::{resolve, Region};
use crate::kernel::node::{Axis, Node, Size, SurfaceSource};
use crate::kernel::paint::render;

const AGENT_PANE: &str = "%agent";
const SHELL_PANE: &str = "%shell";

struct Quiet(std::sync::mpsc::Receiver<()>);

impl Iterator for Quiet {
    type Item = crate::backend::PaneChunk;
    fn next(&mut self) -> Option<Self::Item> {
        let _ = self.0.recv();
        None
    }
}

fn shell_stream(
    adopted: crate::backend::AdoptedSession,
    rows: u16,
    cols: u16,
) -> crate::backend::AdoptedSession {
    let blank = crate::backend::contract::PaneSnapshot {
        cols,
        rows,
        cursor: (0, 0),
        normal: Vec::new(),
        alternate: None,
    };
    crate::backend::AdoptedSession {
        output: Box::new(
            std::iter::once(crate::backend::PaneChunk::Snapshot(Box::new(blank)))
                .chain(adopted.output),
        ),
        ..adopted
    }
}

fn wait_for_the_shells_grid(session: &crate::backend::Session) {
    let shell = session.shell_pane.as_ref().expect("a shell");
    assert!(
        shell.wait_resident(std::time::Duration::from_secs(5)),
        "the shell's first snapshot never landed"
    );
}

struct Discard;

impl Write for Discard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Recorder {
    resizes: Mutex<Vec<(String, u16, u16)>>,
    senders: Mutex<Vec<std::sync::mpsc::Sender<()>>>,
    supports_snapshots: bool,
    snapshots: std::sync::atomic::AtomicUsize,
}

impl Recorder {
    fn io(&self) -> (crate::backend::PaneOutput, Box<dyn Write + Send>) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.senders.lock().expect("senders").push(tx);
        (Box::new(Quiet(rx)), Box::new(Discard))
    }

    fn sizes(&self, pane: &str) -> Vec<(u16, u16)> {
        self.resizes
            .lock()
            .expect("resizes")
            .iter()
            .filter(|(id, _, _)| id == pane)
            .map(|(_, rows, cols)| (*rows, *cols))
            .collect()
    }

    fn count(&self) -> usize {
        self.resizes.lock().expect("resizes").len()
    }
}

impl crate::backend::PaneStream for Recorder {
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
        let (output, input) = self.io();
        Ok(crate::backend::SpawnedSession {
            backend_id: SHELL_PANE.to_string(),
            output,
            input,
            size: None,
        })
    }

    fn adopt(
        &self,
        _: &str,
        _: u16,
        _: u16,
        _: Option<Vec<u8>>,
    ) -> crate::backend::contract::BackendResult<crate::backend::AdoptedSession> {
        let (output, input) = self.io();
        Ok(crate::backend::AdoptedSession {
            output,
            input,
            seed_len: 0,
            size: None,
        })
    }

    fn resize(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
    ) -> crate::backend::contract::BackendResult<()> {
        self.resizes
            .lock()
            .expect("resizes")
            .push((backend_id.to_string(), rows, cols));
        Ok(())
    }

    fn detach(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
        Ok(())
    }

    fn request_snapshot(&self, _: &str) -> crate::backend::contract::BackendResult<()> {
        self.snapshots
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

impl crate::backend::WindowLifecycle for Recorder {
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

    fn set_pane_retention(&self, _: &str, _: bool) -> crate::backend::contract::BackendResult<()> {
        Ok(())
    }

    fn shutdown(&self) {}

    fn check_available(&self) -> crate::backend::contract::BackendResult<()> {
        Ok(())
    }

    fn ensure_ready(&self) -> crate::backend::contract::BackendResult<()> {
        Ok(())
    }

    fn discover(
        &self,
    ) -> crate::backend::contract::BackendResult<Vec<crate::backend::DiscoveredSession>> {
        Ok(Vec::new())
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

    fn default_shell(&self) -> String {
        "/bin/sh".to_string()
    }
}

impl crate::backend::PaneIo for Recorder {
    fn send_text(&self, _: &str, _: &str, _: bool) -> crate::backend::contract::BackendResult<()> {
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

    fn capture(&self, _: &str, _: u32, _: bool) -> crate::backend::contract::BackendResult<String> {
        Err(anyhow::anyhow!("this stub has no panes to read").into())
    }
}

impl crate::backend::StatusChannel for Recorder {
    fn hook_signal_command(&self) -> Option<String> {
        None
    }

    fn record_hook_state(&self, _: &str, _: &str) -> crate::backend::contract::BackendResult<()> {
        Err(anyhow::anyhow!("this stub has no status channel").into())
    }

    fn hook_states(&self) -> crate::backend::contract::BackendResult<Vec<(String, String)>> {
        Err(anyhow::anyhow!("this stub has no status channel").into())
    }

    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

impl crate::backend::SessionBackend for Recorder {
    fn name(&self) -> &str {
        "local:tmux"
    }
    fn capabilities(&self) -> crate::backend::Capabilities {
        crate::backend::Capabilities {
            supports_snapshots: self.supports_snapshots,
            ..crate::backend::Capabilities::NONE
        }
    }
}

struct Harness {
    terminals: Terminals,
    backend: Arc<Recorder>,
    id: String,
    _paths: crate::paths::TestPathGuard,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new(rows: u16, cols: u16) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = crate::paths::TestPathGuard::new(dir.path());
        let recorder = Arc::new(Recorder::default());
        let backend: Arc<dyn crate::backend::SessionBackend> = recorder.clone();
        let mut session = crate::backend::Session::adopt(
            "probe".to_string(),
            rows,
            cols,
            AGENT_PANE,
            &backend,
            None,
            1000,
        )
        .expect("adopt the agent pane");
        let shell = crate::backend::Session::shell_over(
            shell_stream(
                backend
                    .adopt(SHELL_PANE, rows, cols, None)
                    .expect("open the companion shell"),
                rows,
                cols,
            ),
            &backend,
            SHELL_PANE,
            rows,
            cols,
            1000,
        );
        session.install_shell_over(shell, Arc::clone(&backend));
        wait_for_the_shells_grid(&session);

        let mut terminals = Terminals::with_registry(
            std::sync::Arc::new(crate::backend::registry::inert()),
            &crate::session::settings::Settings::default(),
        );
        let id = "probe-0000".to_string();
        terminals.live.insert(
            id.clone(),
            Live {
                session,
                agent: Painted {
                    size: std::cell::Cell::new((rows, cols)),
                    rect: std::cell::Cell::new(Rect::default()),
                    ..Default::default()
                },
                shell: Painted::default(),
            },
        );
        recorder.resizes.lock().expect("resizes").clear();
        Self {
            terminals,
            backend: recorder,
            id,
            _paths: paths,
            _dir: dir,
        }
    }

    fn shell(&self) -> String {
        shell_surface(&self.id)
    }

    fn print(&self, shell: bool, text: &str) {
        let live = self.terminals.live.get(&self.id).expect("live");
        let parser = live.pane(shell).parser().expect("parser");
        let mut parser = parser.lock().expect("lock");
        parser.process(b"\x1b[?25l");
        parser.process(text.as_bytes());
    }

    fn grid(&self, shell: bool) -> (u16, u16) {
        let live = self.terminals.live.get(&self.id).expect("live");
        let parser = live.pane(shell).parser().expect("parser");
        let size = parser.lock().expect("lock").screen().size();
        (size.0.get(), size.1.get())
    }

    fn frame(&self, arrangement: &Region, width: u16, height: u16) -> Screen {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        let placed = resolve(arrangement, Rect::new(0, 0, width, height));
        self.terminals.forget_rects();
        terminal
            .draw(|frame| {
                for slot in &placed {
                    let Some(surface) = self.surface_in(&slot.slot) else {
                        continue;
                    };
                    render(frame, slot.rect, &surface, &self.terminals);
                }
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        Screen {
            rects: placed
                .iter()
                .map(|slot| (slot.slot.clone(), slot.rect))
                .collect(),
            buffer,
        }
    }

    fn surface_in(&self, slot: &str) -> Option<Node> {
        let session = match slot {
            "center" | "agent" => self.id.clone(),
            "shell" => self.shell(),
            _ => return None,
        };
        Some(Node::Surface {
            source: SurfaceSource::Session(session),
            scroll: 0,
            mark: None,
            frame: None,
            size: Default::default(),
            identity: Default::default(),
        })
    }
}

struct Screen {
    rects: HashMap<String, Rect>,
    buffer: ratatui::buffer::Buffer,
}

impl Screen {
    fn rect(&self, slot: &str) -> Rect {
        *self
            .rects
            .get(slot)
            .unwrap_or_else(|| panic!("slot {slot} was not placed"))
    }

    fn first_row(&self, slot: &str) -> String {
        let rect = self.rect(slot);
        (rect.x..rect.x + rect.width)
            .map(|x| self.buffer[(x, rect.y)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    }
}

fn shell_under_the_session_list() -> Region {
    column_and_centre("sessions", "shell", "center")
}

fn agent_under_the_session_list() -> Region {
    column_and_centre("sessions", "agent", "shell")
}

fn column_and_centre(top: &str, bottom: &str, centre: &str) -> Region {
    Region {
        axis: Axis::Horizontal,
        children: vec![
            Region {
                axis: Axis::Vertical,
                size: Size {
                    pct: Some(25.0),
                    min: Some(20),
                    ..Default::default()
                },
                children: vec![
                    Region {
                        slot: Some(top.to_string()),
                        ..Default::default()
                    },
                    Region {
                        slot: Some(bottom.to_string()),
                        size: Size {
                            pct: Some(35.0),
                            min: Some(6),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            Region {
                slot: Some(centre.to_string()),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

fn one_pane(slot: &str) -> Region {
    Region {
        slot: Some(slot.to_string()),
        ..Default::default()
    }
}

const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;

#[tokio::test]
async fn each_pane_is_the_size_of_its_own_slot() {
    let harness = Harness::new(HEIGHT, WIDTH);
    let screen = harness.frame(&shell_under_the_session_list(), WIDTH, HEIGHT);

    let agent = screen.rect("center");
    let shell = screen.rect("shell");
    assert_ne!(
        (agent.height, agent.width),
        (shell.height, shell.width),
        "the fixture is only a reproduction while the two slots differ"
    );

    assert_eq!(
        harness.grid(false),
        (agent.height, agent.width),
        "the agent's grid must be its own slot's size"
    );
    assert_eq!(
        harness.grid(true),
        (shell.height, shell.width),
        "the shell's grid must be its own slot's size"
    );

    assert_eq!(
        harness.backend.sizes(AGENT_PANE),
        vec![(agent.height, agent.width)],
        "the agent pane was resized to something that is not its slot"
    );
    assert_eq!(
        harness.backend.sizes(SHELL_PANE),
        vec![(shell.height, shell.width)],
        "the shell pane was resized to something that is not its slot"
    );
}

#[tokio::test]
async fn a_settled_arrangement_stops_resizing() {
    let harness = Harness::new(HEIGHT, WIDTH);
    let arrangement = shell_under_the_session_list();
    harness.frame(&arrangement, WIDTH, HEIGHT);
    let after_first = harness.backend.count();

    for _ in 0..5 {
        harness.frame(&arrangement, WIDTH, HEIGHT);
    }

    assert_eq!(
        harness.backend.count(),
        after_first,
        "a frame that changed nothing resized a pane"
    );
}

#[tokio::test]
async fn each_rect_shows_the_pane_that_owns_it() {
    let harness = Harness::new(HEIGHT, WIDTH);
    harness.print(false, "AGENT-SCREEN");
    harness.print(true, "SHELL-SCREEN");
    let screen = harness.frame(&shell_under_the_session_list(), WIDTH, HEIGHT);

    assert_eq!(screen.first_row("center"), "AGENT-SCREEN");
    assert_eq!(screen.first_row("shell"), "SHELL-SCREEN");
}

#[tokio::test]
async fn the_shell_draws_itself_with_the_agent_nowhere_on_screen() {
    let harness = Harness::new(HEIGHT, WIDTH);
    harness.print(true, "SHELL-ALONE");
    let screen = harness.frame(&one_pane("shell"), WIDTH, HEIGHT);

    assert_eq!(screen.first_row("shell"), "SHELL-ALONE");
    assert_eq!(
        harness.grid(true),
        (HEIGHT, WIDTH),
        "a shell alone on the screen takes the screen"
    );
    assert_eq!(harness.backend.sizes(AGENT_PANE), Vec::new());
}

#[tokio::test]
async fn the_agent_is_untouched_by_a_shell_that_appears_beside_it() {
    let harness = Harness::new(HEIGHT, WIDTH);
    harness.print(false, "AGENT-SCREEN");

    let alone = harness.frame(&one_pane("center"), WIDTH, HEIGHT);
    let agent_grid = harness.grid(false);
    let agent_resizes = harness.backend.sizes(AGENT_PANE);
    assert_eq!(
        agent_grid,
        (alone.rect("center").height, alone.rect("center").width)
    );
    assert_eq!(alone.first_row("center"), "AGENT-SCREEN");

    let both = harness.frame(&shell_under_the_session_list(), WIDTH, HEIGHT);

    let agent = both.rect("center");
    assert_eq!(harness.grid(false), (agent.height, agent.width));
    assert_eq!(
        harness.backend.sizes(AGENT_PANE),
        [agent_resizes, vec![(agent.height, agent.width)]].concat(),
        "the agent was sized by something other than its own slot"
    );
    assert_eq!(both.first_row("center"), "AGENT-SCREEN");
}

#[tokio::test]
async fn the_panes_are_independent_with_the_columns_swapped() {
    let harness = Harness::new(HEIGHT, WIDTH);
    harness.print(false, "AGENT-SCREEN");
    harness.print(true, "SHELL-SCREEN");

    let screen = harness.frame(&agent_under_the_session_list(), WIDTH, HEIGHT);

    let agent = screen.rect("agent");
    let shell = screen.rect("shell");
    assert_eq!(harness.grid(false), (agent.height, agent.width));
    assert_eq!(harness.grid(true), (shell.height, shell.width));
    assert_eq!(screen.first_row("agent"), "AGENT-SCREEN");
    assert_eq!(screen.first_row("shell"), "SHELL-SCREEN");
    assert_eq!(
        harness.backend.sizes(AGENT_PANE),
        vec![(agent.height, agent.width)]
    );
    assert_eq!(
        harness.backend.sizes(SHELL_PANE),
        vec![(shell.height, shell.width)]
    );
}

#[tokio::test]
async fn a_point_resolves_to_the_pane_it_landed_in() {
    let harness = Harness::new(HEIGHT, WIDTH);
    let screen = harness.frame(&shell_under_the_session_list(), WIDTH, HEIGHT);
    let agent = screen.rect("center");
    let shell = screen.rect("shell");

    assert_eq!(harness.terminals.last_rect(&harness.id), Some(agent));
    assert_eq!(harness.terminals.last_rect(&harness.shell()), Some(shell));

    harness.print(true, "\x1b[?1000h\x1b[?1006h");
    assert!(
        harness
            .terminals
            .forward_wheel(shell.x + 1, shell.y + 1, true),
        "the shell asked for the mouse and the tick landed in its rect"
    );
    assert!(
        !harness
            .terminals
            .forward_wheel(agent.x + 1, agent.y + 1, true),
        "a tick over the agent was answered by the shell"
    );
}

#[tokio::test]
async fn a_surface_that_stopped_drawing_cannot_be_hit() {
    let harness = Harness::new(HEIGHT, WIDTH);
    harness.frame(&shell_under_the_session_list(), WIDTH, HEIGHT);
    assert!(harness.terminals.last_rect(&harness.shell()).is_some());

    let screen = harness.frame(&one_pane("center"), WIDTH, HEIGHT);
    assert_eq!(
        harness.terminals.last_rect(&harness.id),
        Some(screen.rect("center"))
    );
    assert_eq!(harness.terminals.last_rect(&harness.shell()), None);
}

#[tokio::test]
async fn text_is_read_off_the_surface_that_was_named() {
    let harness = Harness::new(HEIGHT, WIDTH);
    harness.print(false, "AGENT-SCREEN");
    harness.print(true, "SHELL-SCREEN");
    harness.frame(&shell_under_the_session_list(), WIDTH, HEIGHT);

    let agent = harness
        .terminals
        .visible_text(&harness.id)
        .expect("agent text");
    let shell = harness
        .terminals
        .visible_text(&harness.shell())
        .expect("shell text");
    assert!(agent.contains("AGENT-SCREEN"), "{agent}");
    assert!(!agent.contains("SHELL-SCREEN"), "{agent}");
    assert!(shell.contains("SHELL-SCREEN"), "{shell}");
    assert!(!shell.contains("AGENT-SCREEN"), "{shell}");
}

#[tokio::test]
async fn a_press_is_captured_by_the_pane_it_landed_in() {
    let harness = Harness::new(HEIGHT, WIDTH);
    let screen = harness.frame(&shell_under_the_session_list(), WIDTH, HEIGHT);
    let agent = screen.rect("center");
    let shell = screen.rect("shell");

    harness.print(true, "\x1b[?1002h\x1b[?1006h");
    assert_eq!(
        harness.terminals.forward_press(shell.x + 1, shell.y + 1),
        Some(harness.shell())
    );
    assert_eq!(
        harness.terminals.forward_press(agent.x + 1, agent.y + 1),
        None,
        "a press over the agent was answered by the shell"
    );

    assert!(harness
        .terminals
        .forward_motion(&harness.shell(), shell.x + 2, shell.y + 2));
    assert!(harness
        .terminals
        .forward_motion(&harness.shell(), agent.x + 2, agent.y + 2));
    assert!(!harness
        .terminals
        .forward_motion(&harness.id, shell.x + 2, shell.y + 2));
}

#[tokio::test]
async fn a_search_reads_both_of_a_sessions_screens() {
    let harness = Harness::new(HEIGHT, WIDTH);
    harness.print(false, "AGENT-SCREEN");
    harness.print(true, "SHELL-SCREEN");

    let sources = harness
        .terminals
        .search_sources(std::slice::from_ref(&harness.id));
    let answer = crate::kernel::search::run(
        crate::kernel::search::Request {
            query: "screen".into(),
            sessions: None,
        },
        &sources,
        &std::sync::Mutex::default(),
    );
    let found: Vec<(&str, bool, &str)> = answer
        .hits
        .iter()
        .map(|hit| (hit.session.as_str(), hit.shell, hit.text.as_str()))
        .collect();
    assert!(
        found.contains(&(harness.id.as_str(), false, "AGENT-SCREEN")),
        "{found:?}"
    );
    assert!(
        found.contains(&(harness.id.as_str(), true, "SHELL-SCREEN")),
        "{found:?}"
    );
}

#[tokio::test]
async fn links_are_found_on_the_surface_that_printed_them() {
    let harness = Harness::new(HEIGHT, WIDTH);
    harness.print(false, "https://example.invalid/agent");
    harness.print(true, "https://example.invalid/shell");

    let agent: Vec<String> = harness
        .terminals
        .links(&harness.id)
        .into_iter()
        .map(|(url, _, _)| url)
        .collect();
    let shell: Vec<String> = harness
        .terminals
        .links(&harness.shell())
        .into_iter()
        .map(|(url, _, _)| url)
        .collect();
    assert_eq!(agent, vec!["https://example.invalid/agent".to_string()]);
    assert_eq!(shell, vec!["https://example.invalid/shell".to_string()]);
}

#[tokio::test]
async fn only_the_surface_that_takes_the_keys_paints_a_cursor() {
    let harness = Harness::new(4, 10);
    for shell in [false, true] {
        let live = harness.terminals.live.get(&harness.id).expect("live");
        let parser = live.pane(shell).parser().expect("parser");
        parser.lock().expect("lock").process(b"$ ");
    }
    let agent = harness.surface_in("agent").expect("agent");
    let shell = harness.surface_in("shell").expect("shell");
    let paint = |input: Option<&str>| {
        let provider = harness.terminals.cursor_on(input);
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).expect("test terminal");
        terminal
            .draw(|frame| {
                render(frame, Rect::new(0, 0, 10, 4), &agent, &provider);
                render(frame, Rect::new(10, 0, 10, 4), &shell, &provider);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (
            buffer[(2, 0)].symbol().to_string(),
            buffer[(12, 0)].symbol().to_string(),
        )
    };

    assert_eq!(paint(Some(&harness.id)), ("\u{2588}".into(), " ".into()));
    assert_eq!(
        paint(Some(&harness.shell())),
        (" ".into(), "\u{2588}".into())
    );
    assert_eq!(paint(None), (" ".into(), " ".into()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replacement_shell_takes_its_own_first_frame_size() {
    let mut harness = Harness::new(HEIGHT, WIDTH);
    let arrangement = shell_under_the_session_list();
    let before = harness.frame(&arrangement, WIDTH, HEIGHT);
    let rect = before.rect("shell");
    drop(harness.backend.senders.lock().unwrap().remove(1));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !harness.terminals.live[&harness.id]
            .session
            .shell_pane
            .as_ref()
            .unwrap()
            .has_exited()
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the old shell to exit");
    let row = super::tests::row(&harness.id, "local:tmux", Some(AGENT_PANE));
    let snapshot = crate::kernel::snapshot::Snapshot {
        sessions: vec![row],
        ..Default::default()
    };
    let backend: Arc<dyn crate::backend::SessionBackend> = harness.backend.clone();
    harness.terminals.start_shell(&harness.id, move || {
        let adopted = shell_stream(
            backend
                .adopt(SHELL_PANE, HEIGHT, WIDTH, None)
                .map_err(|e| e.to_string())?,
            HEIGHT,
            WIDTH,
        );
        let shell =
            crate::backend::Session::shell_over(adopted, &backend, SHELL_PANE, HEIGHT, WIDTH, 1000);
        Ok((shell, backend))
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            harness.terminals.collect_shells(&snapshot);
            if !harness.terminals.drain_shell_results().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the replacement shell to land");
    wait_for_the_shells_grid(&harness.terminals.live[&harness.id].session);
    harness.print(true, "REPLACEMENT");
    let after = harness.frame(&arrangement, WIDTH, HEIGHT);
    assert_eq!(harness.grid(true), (rect.height, rect.width));
    assert_eq!(after.first_row("shell"), "REPLACEMENT");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grid_that_never_arrives_does_not_stall_any_frame() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _paths = crate::paths::TestPathGuard::new(dir.path());
    let recorder = Arc::new(Recorder {
        supports_snapshots: true,
        ..Default::default()
    });
    let backend: Arc<dyn crate::backend::SessionBackend> = recorder.clone();
    let session = crate::backend::Session::adopt_dormant(
        "probe".to_string(),
        24,
        80,
        AGENT_PANE,
        &backend,
        1000,
    )
    .expect("adopt");
    let mut terminals = Terminals::with_registry(
        std::sync::Arc::new(crate::backend::registry::inert()),
        &crate::session::settings::Settings::default(),
    );
    let id = "probe-0000".to_string();
    terminals.live.insert(
        id.clone(),
        Live {
            session,
            agent: Painted {
                size: std::cell::Cell::new((24, 80)),
                ..Default::default()
            },
            shell: Painted::default(),
        },
    );

    let paint = || {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
        let started = std::time::Instant::now();
        terminal
            .draw(|frame| {
                use crate::kernel::paint::SurfaceProvider;
                assert!(terminals.render_session(frame, Rect::new(0, 0, 80, 24), &id, 0));
            })
            .expect("draw");
        started.elapsed()
    };
    for frame in 0..6 {
        let elapsed = paint();
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "paint {frame} waited {elapsed:?} for an unanswered snapshot"
        );
    }
    assert_eq!(
        recorder
            .snapshots
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "asked once"
    );
}

#[tokio::test]
async fn hidden_output_does_not_owe_a_frame() {
    use std::sync::atomic::Ordering;
    let harness = Harness::new(HEIGHT, WIDTH);
    harness.frame(&one_pane("agent"), WIDTH, HEIGHT);
    let before = harness.terminals.visible_output_generation();
    let all_before = harness.terminals.output_generation();
    harness.print(true, "hidden output");
    harness
        .terminals
        .output_seq_cell(&harness.shell())
        .unwrap()
        .fetch_add(1, Ordering::Release);
    assert_eq!(
        harness.terminals.visible_output_generation(),
        before,
        "hidden shell output must not redraw the agent"
    );
    assert_ne!(
        harness.terminals.output_generation(),
        all_before,
        "hidden output still invalidates search content"
    );
    harness.frame(&one_pane("shell"), WIDTH, HEIGHT);
    let before = harness.terminals.visible_output_generation();
    harness
        .terminals
        .output_seq_cell(&harness.shell())
        .unwrap()
        .fetch_add(1, Ordering::Release);
    assert_ne!(
        harness.terminals.visible_output_generation(),
        before,
        "visible output still owes a frame"
    );
    assert!(harness
        .terminals
        .visible_text(&harness.shell())
        .unwrap()
        .contains("hidden output"));
}
