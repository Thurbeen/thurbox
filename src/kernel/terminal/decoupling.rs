//! The agent pane and the companion shell are two **independent** surfaces.
//!
//! The shell was a second tab of the centre pane, and the kernel was built to
//! match: one rect, one size memo and one "which view is in it" flag per
//! session, because only one of the two could ever be on screen. An
//! arrangement may give the shell a slot of its own — `layout.lua` is the
//! operator's file and any legal arrangement has to work — and then both are on
//! screen at once, in slots of different sizes, and every one of those shared
//! cells is read by the wrong pane (#1220).
//!
//! These pin the four properties that make them decoupled, in every
//! arrangement the layout permits — both visible, each alone, and with the two
//! columns swapped:
//!
//! * **own geometry** — each pane's grid is the size of the slot it occupies;
//! * **own content** — each rect shows the pane that owns it;
//! * **own lifecycle** — opening, painting or hiding one moves nothing about
//!   the other, and a settled frame issues no resize at all;
//! * **no shared area** — a point, a wheel tick and a rect resolve to the pane
//!   they landed in rather than to whichever painted last.
//!
//! Offline: a stub backend records the sizes a real multiplexer would be sent,
//! so the loop that made the panes flash is a list that stops growing rather
//! than something a human has to watch a terminal for.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

// Nothing from `crate::agent` is imported here, by rule rather than by
// accident: the kernel reaches it by fully-qualified path only, never by `use`
// (`tests/architecture_rules.rs`), and a test is not an exception to it.
use super::{shell_surface, Live, Painted, Terminals};
use crate::kernel::layout::{resolve, Region};
use crate::kernel::node::{Axis, Node, Size, SurfaceSource};
use crate::kernel::paint::render;

/// The pane id the stub hands out for the agent. The shell gets one of its own
/// from `spawn`, so every recorded resize says which pane it was for.
const AGENT_PANE: &str = "%agent";
const SHELL_PANE: &str = "%shell";

/// Output that never arrives and never ends.
///
/// The reader loop treats EOF as "the pane exited", so a stub that returns
/// `Ok(0)` at once would retire the pane before the first frame is painted.
/// Blocking on a channel whose sender the harness holds ends the loop at drop
/// instead.
struct Quiet(std::sync::mpsc::Receiver<()>);

impl Read for Quiet {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        let _ = self.0.recv();
        Ok(0)
    }
}

/// Input nobody reads: what a pane is *sent* is not what these are about.
struct Discard;

impl Write for Discard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A backend that records the size every pane was told about, instead of
/// talking to a multiplexer.
///
/// The record is the whole point: "the agent adopted the shell's dimensions"
/// and "the two resized each other every frame" are both statements about this
/// list, and neither needs a terminal to observe.
#[derive(Default)]
struct Recorder {
    resizes: Mutex<Vec<(String, u16, u16)>>,
    senders: Mutex<Vec<std::sync::mpsc::Sender<()>>>,
    /// Snapshot requests taken, none of them ever answered.
    snapshots: std::sync::atomic::AtomicUsize,
}

impl Recorder {
    /// A pane's I/O, with the sender kept alive so its reader blocks.
    fn io(&self) -> (Box<dyn Read + Send>, Box<dyn Write + Send>) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.senders.lock().expect("senders").push(tx);
        (Box::new(Quiet(rx)), Box::new(Discard))
    }

    /// Every size `pane` was told about, oldest first.
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

impl crate::agent::backend::SessionBackend for Recorder {
    fn name(&self) -> &str {
        "local-tmux"
    }
    fn check_available(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn ensure_ready(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn spawn(
        &self,
        _: &str,
        _: &str,
        _: &[String],
        _: Option<&std::path::Path>,
        _: &HashMap<String, String>,
        _: u16,
        _: u16,
    ) -> anyhow::Result<crate::agent::backend::SpawnedSession> {
        let (output, input) = self.io();
        Ok(crate::agent::backend::SpawnedSession {
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
    ) -> anyhow::Result<crate::agent::backend::AdoptedSession> {
        let (output, input) = self.io();
        Ok(crate::agent::backend::AdoptedSession {
            output,
            input,
            seed_len: 0,
            size: None,
        })
    }
    fn discover(&self) -> anyhow::Result<Vec<crate::agent::backend::DiscoveredSession>> {
        Ok(Vec::new())
    }
    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> anyhow::Result<()> {
        self.resizes
            .lock()
            .expect("resizes")
            .push((backend_id.to_string(), rows, cols));
        Ok(())
    }
    fn is_dead(&self, _: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    fn kill(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn detach(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn pane_pid(&self, _: &str) -> anyhow::Result<Option<u32>> {
        Ok(None)
    }
    fn default_shell(&self) -> String {
        "/bin/sh".to_string()
    }
    fn supports_snapshots(&self) -> bool {
        true
    }
    fn request_snapshot(&self, _: &str) -> anyhow::Result<()> {
        self.snapshots
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// A live session with a companion shell, attached to a recording backend.
struct Harness {
    terminals: Terminals,
    backend: Arc<Recorder>,
    id: String,
    /// `Terminals::new()` reads the host registry and the agent registry, and
    /// seeds the latter when it is missing — so it is pointed at a directory of
    /// its own rather than at whoever is running the suite.
    _paths: crate::paths::TestPathGuard,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// Attach one session and open its shell, both at the whole terminal's
    /// size — the state the interface is in the moment before the first frame
    /// of an arrangement that shows both.
    fn new(rows: u16, cols: u16) -> Self {
        Self::build(rows, cols, true)
    }

    /// The same session with no companion shell opened yet.
    fn without_shell(rows: u16, cols: u16) -> Self {
        Self::build(rows, cols, false)
    }

    fn build(rows: u16, cols: u16, open_shell: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = crate::paths::TestPathGuard::new(dir.path());
        let recorder = Arc::new(Recorder::default());
        let backend: Arc<dyn crate::agent::backend::SessionBackend> = recorder.clone();
        let provider: Arc<dyn crate::agent::AgentProvider> = Arc::new(
            crate::agent::GenericProvider::new(crate::session::AgentDef::default()),
        );
        let mut session = crate::agent::Session::adopt(
            "probe".to_string(),
            rows,
            cols,
            AGENT_PANE,
            &backend,
            &provider,
            HashMap::new(),
            None,
        )
        .expect("adopt the agent pane");
        if open_shell {
            session
                .ensure_shell_pane(rows, cols, None)
                .expect("open the companion shell");
        }

        let mut terminals = Terminals::new();
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
                shell_asked: std::cell::RefCell::new(None),
            },
        );
        // Both panes were born at the terminal's size; the arrangement is what
        // the assertions are about, so the record starts here.
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

    /// Print `text` on a pane's grid, so the rect it is painted into can be
    /// read back and attributed.
    ///
    /// The cursor is hidden first: `PseudoTerminal` paints one, and a block on
    /// the cell after the text is a true fact about the pane that these
    /// assertions are not about.
    fn print(&self, shell: bool, text: &str) {
        let live = self.terminals.live.get(&self.id).expect("live");
        let parser = live.pane(shell).parser().expect("parser");
        let mut parser = parser.lock().expect("lock");
        parser.process(b"\x1b[?25l");
        parser.process(text.as_bytes());
    }

    /// The grid size a pane currently holds — rows by columns.
    fn grid(&self, shell: bool) -> (u16, u16) {
        let live = self.terminals.live.get(&self.id).expect("live");
        let parser = live.pane(shell).parser().expect("parser");
        let size = parser.lock().expect("lock").screen().size();
        (size.0, size.1)
    }

    /// Paint one frame of `arrangement` into a `width`×`height` screen, and
    /// hand back what each slot's rect ended up showing.
    ///
    /// The real thing: `layout::resolve` divides the screen exactly as it does
    /// for the operator's own `layout.lua`, and each slot is painted with the
    /// same `paint::render` the draw loop calls — only the arrangement is a
    /// fixture, because a legal arrangement is precisely what this is about.
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

    /// The node a slot holds: the agent's surface, the shell's, or nothing.
    ///
    /// Slot names are the arrangement's, so a fixture places the two panes by
    /// naming them — which is what makes the inverted case a change of layout
    /// and not a change of code.
    fn surface_in(&self, slot: &str) -> Option<Node> {
        let session = match slot {
            "center" | "agent" => self.id.clone(),
            // The agent pane on its Shell tab: the same shell surface, in the
            // agent's slot.
            "shell" | "shell-tab" => self.shell(),
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

/// One painted frame: where every slot landed, and the cells that were drawn.
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

    /// The first row of a slot's rect, as text.
    fn first_row(&self, slot: &str) -> String {
        let rect = self.rect(slot);
        (rect.x..rect.x + rect.width)
            .map(|x| self.buffer[(x, rect.y)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    }
}

/// The arrangement from the report: the shell stacked under the session list in
/// the left column, the agent holding `center`.
///
/// A fixture, not a proposal — `ui/layout.lua` ships the arrangement it always
/// did. This is one point in the space of arrangements an operator may write,
/// and the panes have to survive all of them.
fn shell_under_the_session_list() -> Region {
    column_and_centre("sessions", "shell", "center")
}

/// The same arrangement with the two panes swapped: the AGENT in the left
/// column and the shell in `center`.
///
/// Nobody runs this. It is here because anything that still assumes which of
/// the two owns the centre passes the case above and fails this one.
fn agent_under_the_session_list() -> Region {
    column_and_centre("sessions", "agent", "shell")
}

/// A left column of `top` over `bottom`, beside a wide slot named `centre`.
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

/// One slot, holding one surface — the arrangements where only one of the two
/// panes is on screen at all.
fn one_pane(slot: &str) -> Region {
    Region {
        slot: Some(slot.to_string()),
        ..Default::default()
    }
}

/// The screen these run on. Wide enough that the two slots are nowhere near the
/// same size, which is the whole of the report: a 35%-of-a-quarter shell and an
/// agent holding the rest cannot both be right about one geometry.
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

    // The report, as an assertion: the agent rendered at the SHELL's size.
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

    // And neither pane was ever sent the other's dimensions — a grid that
    // happens to end up right after being told the wrong thing twice is the
    // flashing, not the fix.
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

    // The flashing, as a number: two panes taking turns claiming one size memo
    // resize each other on every frame forever. Each pane owning its own means
    // the second frame has nothing to say.
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
async fn a_shell_surface_painted_before_its_shell_exists_asks_for_one_once() {
    // A layout that gives the shell a pane of its own paints `<id>#shell` before
    // anything opened the shell. The pane must not have to ask from its render,
    // so the paint notes it — once per attach, since a shell that fails to open
    // repaints every frame.
    let harness = Harness::without_shell(HEIGHT, WIDTH);
    harness.frame(&one_pane("center"), WIDTH, HEIGHT);
    assert!(
        harness.terminals.take_wanted_shells().is_empty(),
        "painting the agent asks for no shell"
    );

    harness.frame(&one_pane("shell"), WIDTH, HEIGHT);
    assert_eq!(
        harness.terminals.take_wanted_shells(),
        vec![harness.id.clone()]
    );

    harness.frame(&one_pane("shell"), WIDTH, HEIGHT);
    assert!(
        harness.terminals.take_wanted_shells().is_empty(),
        "asked once per attach, not once per frame"
    );
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
    // The agent is not on screen, so nothing sized it — its grid is where the
    // attach left it rather than wherever the shell went.
    assert_eq!(harness.backend.sizes(AGENT_PANE), Vec::new());
}

#[tokio::test]
async fn the_agent_is_untouched_by_a_shell_that_appears_beside_it() {
    let harness = Harness::new(HEIGHT, WIDTH);
    harness.print(false, "AGENT-SCREEN");

    // A frame with the agent alone, then one with the shell placed beside it —
    // the operator's second symptom, which is "opening the shell changed the
    // agent pane".
    let alone = harness.frame(&one_pane("center"), WIDTH, HEIGHT);
    let agent_grid = harness.grid(false);
    let agent_resizes = harness.backend.sizes(AGENT_PANE);
    assert_eq!(
        agent_grid,
        (alone.rect("center").height, alone.rect("center").width)
    );
    assert_eq!(alone.first_row("center"), "AGENT-SCREEN");

    let both = harness.frame(&shell_under_the_session_list(), WIDTH, HEIGHT);

    // The agent moved because the ARRANGEMENT moved it, to its own new slot —
    // and to nothing else. The shell appearing contributes no size of its own.
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

    // Nobody runs this arrangement. It is the one that finds anything still
    // assuming which of the two owns the centre.
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

    // Each surface reports where IT was painted. One rect per session is what
    // sent a click in one pane to the other's grid.
    assert_eq!(harness.terminals.last_rect(&harness.id), Some(agent));
    assert_eq!(harness.terminals.last_rect(&harness.shell()), Some(shell));

    // A wheel tick goes to the pane under the pointer. Only the shell asked for
    // the mouse, so the agent's rect must decline the tick rather than answer
    // for a pane the pointer is nowhere near.
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

    // The shell's slot left the arrangement; the agent's did not.
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

    // Only the shell's program tracks the mouse. A press is a GESTURE: the
    // surface that takes it owns the button until the release, so the name
    // handed back has to say which of the two panes that was — the agent may
    // well be on screen beside it.
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

    // And the moves that follow reach the pane that took it, whether the
    // pointer is still over that pane or has wandered onto the other one.
    assert!(harness
        .terminals
        .forward_motion(&harness.shell(), shell.x + 2, shell.y + 2));
    assert!(harness
        .terminals
        .forward_motion(&harness.shell(), agent.x + 2, agent.y + 2));
    // The agent asked for nothing, so nothing is sent on its behalf.
    assert!(!harness
        .terminals
        .forward_motion(&harness.id, shell.x + 2, shell.y + 2));
}

#[tokio::test]
async fn a_search_reads_both_of_a_sessions_screens() {
    let harness = Harness::new(HEIGHT, WIDTH);
    harness.print(false, "AGENT-SCREEN");
    harness.print(true, "SHELL-SCREEN");

    // A search asks which SESSION holds the text, so both panes answer under
    // the session's own id: the shell is a screen of that session whether or
    // not it is the pane on screen. Scanning only whichever one happened to be
    // painted made a search's answer depend on the arrangement.
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

    // Unlike a search, links are per SURFACE: a link is clicked in the rect
    // that drew it, so an answer that mixed the two panes would offer the
    // shell's URLs at coordinates inside the agent.
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
    // Keys reach the focused pane's FIRST live surface, so that is the one
    // cursor on screen. A pane showing both of a session's surfaces used to
    // paint a cursor in each, one of them a pane nothing could type into.
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
    // A pane without focus, or a float whose keys go to Lua, paints none.
    assert_eq!(paint(None), (" ".into(), " ".into()));
}

/// A snapshot that never arrives costs the paint that asked for it a short
/// wait, and no later paint anything: the pane shows blank until the grid
/// lands and is asked again only once the request has had time to fail.
/// Waiting on every paint was a stall on every frame for as long as the host
/// did not answer.
#[tokio::test(flavor = "multi_thread")]
async fn a_grid_that_never_arrives_does_not_stall_every_frame() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _paths = crate::paths::TestPathGuard::new(dir.path());
    let recorder = Arc::new(Recorder::default());
    let backend: Arc<dyn crate::agent::backend::SessionBackend> = recorder.clone();
    let provider: Arc<dyn crate::agent::AgentProvider> = Arc::new(
        crate::agent::GenericProvider::new(crate::session::AgentDef::default()),
    );
    let session = crate::agent::Session::adopt_dormant(
        "probe".to_string(),
        24,
        80,
        AGENT_PANE,
        &backend,
        &provider,
        HashMap::new(),
    )
    .expect("adopt");
    let mut terminals = Terminals::new();
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
    let first = paint();
    let later: Vec<_> = (0..5).map(|_| paint()).collect();

    assert!(
        first >= std::time::Duration::from_millis(50),
        "the paint that asked waits a moment for the answer: {first:?}"
    );
    for elapsed in &later {
        assert!(
            *elapsed < std::time::Duration::from_millis(50),
            "a later paint waited {elapsed:?} for a grid already asked for"
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
async fn one_shell_painted_in_two_rects_in_one_frame_keeps_the_first() {
    // An agent pane edited before shell panes existed still offers its Shell
    // tab, and a layout that also places the shell pane then painted one
    // terminal into two rects every frame: the pty took whichever size came
    // last, and the other rect showed it wrapped at the wrong width.
    let harness = Harness::new(HEIGHT, WIDTH);
    let both = column_and_centre("sessions", "shell", "shell-tab");
    let screen = harness.frame(&both, WIDTH, HEIGHT);
    let first = screen.rect("shell");
    let second = screen.rect("shell-tab");
    assert_ne!((first.height, first.width), (second.height, second.width));

    harness.frame(&both, WIDTH, HEIGHT);
    let sizes = harness.backend.sizes(SHELL_PANE);
    assert!(
        sizes.iter().all(|size| *size == (first.height, first.width)),
        "the shell is sized to one rect, not both: {sizes:?}"
    );
    assert_eq!(harness.grid(true), (first.height, first.width));
}
