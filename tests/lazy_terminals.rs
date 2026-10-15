//! A session nobody is looking at keeps no terminal of its own.
//!
//! tmux already parses every pane and keeps its screen and history. The interface
//! used to run a second vt100 emulator per session on top of that, with a full
//! grid and `scrollback_lines` of history each, whether or not the session was
//! ever shown — ~6 MiB a session once its history filled. So a session that is
//! not on screen now holds a two-cell grid that still reads every byte for what
//! the interface needs live (the title, a bell, a notification, when it last
//! printed), and its real grid is rebuilt from the multiplexer when something
//! wants it: a paint, or a search.
//!
//! What has to hold for that to be invisible is what these pin, against a real
//! tmux server on a private socket: the rebuilt terminal is the one that was
//! never dropped, output that arrives while it is gone is neither lost nor
//! doubled, the first frame after coming back is the current screen, a search
//! still finds history nobody is looking at, a session off screen still
//! reports its title and its output, non-ASCII text printed while a grid is
//! live reaches it with every character intact, and — on an ssh host — a
//! session off screen sends no output at all yet loses none of those
//! signals (`remote`).

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use thurbox::kernel::paint::SurfaceProvider;
use thurbox::kernel::search::{self, Request};
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::terminal::Terminals;
use thurbox::session::SessionState;

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-lazy-test";

/// `backend::tmux_compat::server::TMUX_SESSION` in a test build — see `tests/attach_by_name.rs`.
const SESSION: &str = "thurbox-dev";

const ID: &str = "22222222-2222-2222-2222-222222222222";

const ROWS: u16 = 24;
const COLS: u16 = 80;

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn tmux(args: &[&str]) -> String {
    let out = Command::new("tmux")
        .arg("-L")
        .arg(SOCKET)
        .args(args)
        .output()
        .expect("run tmux");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn row(pane: &str) -> SessionRow {
    SessionRow {
        id: ID.into(),
        name: "lazy".into(),
        agent: "claude".into(),
        status: SessionState::Idle,
        cwd: None,
        repo: None,
        repos: Vec::new(),
        branch: None,
        base_branch: None,
        backend: "local-tmux".into(),
        backend_id: Some(pane.into()),
        remote_host: None,
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
    }
}

fn snapshot(pane: &str) -> Snapshot {
    Snapshot {
        sessions: vec![row(pane)],
        ..Snapshot::default()
    }
}

/// A pane running `script` in its own window, at the size the interface will
/// attach it at, so nothing is resized underneath the comparison.
fn pane_running(script: &str) -> String {
    tmux(&[
        "new-session",
        "-d",
        "-s",
        SESSION,
        "-x",
        &COLS.to_string(),
        "-y",
        &ROWS.to_string(),
        "-n",
        "idle",
        "sh",
    ]);
    tmux(&["set-option", "-g", "history-limit", "5000"]);
    tmux(&[
        "new-window",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        SESSION,
        "-n",
        "tb-lazy",
        script,
    ])
    .trim()
    .to_string()
}

/// Everything tmux holds for the pane, as text — how a test knows output has
/// reached the multiplexer before asking the interface about it.
fn tmux_text(pane: &str) -> String {
    tmux(&["capture-pane", "-p", "-J", "-S", "-", "-t", pane])
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

async fn attach(terminals: &mut Terminals, snap: &Snapshot) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        terminals.sync(snap, ROWS, COLS);
        if terminals.is_attached(ID) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "never attached: {}",
        terminals.failure(ID).unwrap_or_default()
    );
}

/// Wait until the interface has taken in everything the pane printed since it
/// attached: its output stamp has moved (`printed`) and then stopped moving.
async fn settle(terminals: &Terminals, printed: bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let quiet = terminals.millis_since_output(ID).unwrap_or(0);
        // An adopted pane's stamp starts at the epoch, so "quiet for years"
        // is "has printed nothing since it was attached".
        let seen = quiet < 3_600_000;
        if quiet > 400 && (seen || !printed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the pane never went quiet");
}

/// One frame of the session, as the rows a person would see.
fn paint(terminals: &Terminals, scroll: u16) -> Vec<String> {
    let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).expect("terminal");
    term.draw(|frame| {
        let area = Rect::new(0, 0, COLS, ROWS);
        assert!(terminals.render_session(frame, area, ID, scroll));
    })
    .expect("draw");
    let buffer = term.backend().buffer().clone();
    (0..ROWS)
        .map(|y| {
            (0..COLS)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn agent_parser(terminals: &Terminals) -> Arc<Mutex<thurbox::backend::SessionParser>> {
    terminals
        .search_sources(&[ID.to_string()])
        .into_iter()
        .find(|source| !source.shell)
        .expect("the agent pane is a search source")
        .parser
}

/// Terminal output that exercises what a replay could get wrong: colours of
/// every kind, lines that wrap, one exactly as wide as the screen, wide
/// characters, a tab, more history than the scrollback keeps, and a last line
/// with no newline, so the cursor is left mid-row.
fn script_bytes(tag: &str, lines: usize) -> Vec<u8> {
    let mut out = String::new();
    for i in 0..lines {
        match i % 7 {
            0 => out.push_str(&format!("\x1b[31m{tag} red {i}\x1b[0m plain\n")),
            1 => out.push_str(&format!(
                "\x1b[1;38;5;208m{tag} bold 256 {i}\x1b[22m still coloured\x1b[0m\n"
            )),
            2 => out.push_str(&format!(
                "\x1b[48;2;10;20;30m{tag} truecolour background {i}\x1b[0m\n"
            )),
            3 => out.push_str(&format!("{tag} wraps {i} {}\n", "abcdefghij".repeat(15))),
            4 => out.push_str(&format!("{tag} wide {i} 漢字かな\tafter a tab\n")),
            5 => out.push_str(&format!("{:-<80}\n", format!("{tag} exact {i} "))),
            _ => out.push_str(&format!("\x1b[3;4m{tag} italic underline {i}\x1b[0m\n")),
        }
    }
    out.push_str(&format!("{tag} prompt> "));
    out.into_bytes()
}

/// What a pty hands on: `\n` leaves as `\r\n` (`onlcr`).
fn through_pty(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 16);
    for &b in bytes {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

/// One row of a screen, cell by cell: what is in it and how it is drawn. A cell
/// never written and a written space read the same, as they do on screen.
fn row_cells(screen: &vt100::Screen, row: u16) -> Vec<String> {
    (0..screen.size().1)
        .map(|col| {
            let Some(cell) = screen.cell(row, col) else {
                return String::new();
            };
            let text = if cell.contents().trim().is_empty() {
                ""
            } else {
                cell.contents()
            };
            format!(
                "{text}|{:?}|{:?}|{}{}{}{}",
                cell.fgcolor(),
                cell.bgcolor(),
                u8::from(cell.bold()),
                u8::from(cell.italic()),
                u8::from(cell.underline()),
                u8::from(cell.inverse()),
            )
        })
        .collect()
}

/// Every row the parser holds, oldest history first, each with its wrap flag.
fn all_rows(parser: &mut vt100::Parser<impl vt100::Callbacks>) -> Vec<(Vec<String>, bool)> {
    let rows = parser.screen().size().0;
    parser.screen_mut().set_scrollback(usize::MAX);
    let depth = parser.screen().scrollback();
    let mut out = Vec::new();
    // Page up to the top, then read one screenful at a time on the way down;
    // the last page overlaps the screen, so rows are keyed by their distance
    // from the bottom.
    let mut seen = std::collections::BTreeMap::new();
    let mut offset = depth;
    loop {
        parser.screen_mut().set_scrollback(offset);
        let screen = parser.screen();
        for r in 0..rows {
            let from_top = depth - offset + usize::from(r);
            seen.entry(from_top)
                .or_insert_with(|| (row_cells(screen, r), screen.row_wrapped(r)));
        }
        if offset == 0 {
            break;
        }
        offset = offset.saturating_sub(usize::from(rows));
    }
    parser.screen_mut().set_scrollback(0);
    out.extend(seen.into_values());
    out
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write script output");
    path.display().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_nobody_has_looked_at_holds_no_screen() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin(SOCKET);
    let dir = tempfile::tempdir().expect("tempdir");
    let a = write(dir.path(), "a", &script_bytes("early", 300));
    let pane = pane_running(&format!("sh -c 'cat {a}; exec sleep 100000'"));
    wait_for("the output to reach tmux", || {
        tmux_text(&pane).contains("early prompt>")
    });

    let mut terminals = Terminals::with_registry(std::sync::Arc::new(
        thurbox::backend::wiring::configured().0,
    ));
    let snap = snapshot(&pane);
    attach(&mut terminals, &snap).await;
    settle(&terminals, false).await;

    let (rows, cols) = grid_size(&terminals);
    assert!(
        rows <= 2 && cols <= 2,
        "a session never painted keeps a {rows}x{cols} grid"
    );
}

fn grid_size(terminals: &Terminals) -> (u16, u16) {
    agent_parser(terminals)
        .lock()
        .expect("parser")
        .screen()
        .size()
}

/// The numbers of every complete `line-N` the parser holds, oldest first. The
/// row the cursor is on is left out: it may hold half a line still arriving.
fn numbered_lines(terminals: &Terminals) -> Vec<u64> {
    let parser = agent_parser(terminals);
    let mut parser = parser.lock().expect("parser");
    let mut rows = all_rows(&mut parser);
    rows.truncate(
        rows.len() - usize::from(ROWS) + usize::from(parser.screen().cursor_position().0),
    );
    rows.into_iter()
        .filter_map(|(cells, _)| {
            let text: String = cells
                .iter()
                .map(|cell| cell.split('|').next().unwrap_or(""))
                .collect();
            text.trim().strip_prefix("line-")?.parse().ok()
        })
        .collect()
}

/// Every number follows the one before it — nothing lost, nothing repeated.
fn assert_contiguous(numbers: &[u64], when: &str) {
    for pair in numbers.windows(2) {
        assert_eq!(
            pair[1],
            pair[0] + 1,
            "{when}: line-{} follows line-{}, so output was lost or repeated",
            pair[1],
            pair[0]
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grid_dropped_and_rebuilt_while_its_pane_prints_loses_and_repeats_nothing() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin(SOCKET);
    let dir = tempfile::tempdir().expect("tempdir");
    let go = dir.path().join("go");
    let stop = dir.path().join("stop");
    // Steady output until the test says stop, so every rebuild below lands
    // while the pane is printing. Each round waits for the pane to have moved
    // on by 300 lines rather than for time to pass: a fixed amount of output
    // paced by the wall clock ran out after three rounds on a loaded machine
    // (#1248).
    let pane = pane_running(&format!(
        "sh -c 'while [ ! -e {go} ]; do sleep 0.05; done; i=0; \
         while [ ! -e {stop} ]; do echo line-$i; i=$((i+1)); \
         if [ $((i % 30)) -eq 0 ]; then sleep 0.02; fi; done; \
         echo finished; exec sleep 100000'",
        go = go.display(),
        stop = stop.display()
    ));
    wait_for("the pane to start", || tmux_text(&pane).contains(""));

    let mut terminals = Terminals::with_registry(std::sync::Arc::new(
        thurbox::backend::wiring::configured().0,
    ));
    terminals.keep_hidden_for(Some(Duration::ZERO));
    let snap = snapshot(&pane);
    attach(&mut terminals, &snap).await;

    std::fs::write(&go, b"").expect("go");
    // Each round: shown, which rebuilds the grid where the snapshot lands in
    // the stream; left to take live output for a moment, then checked; then
    // off screen, which drops it again on the next sync.
    for rebuild in 1..10 {
        wait_for("the pane to print on", || {
            last_printed(&pane).is_some_and(|n| n >= 300 * (rebuild - 1))
        });
        wait_for("the rebuilt grid", || {
            paint(&terminals, 0);
            grid_size(&terminals) == (ROWS, COLS)
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_contiguous(&numbered_lines(&terminals), &format!("rebuild {rebuild}"));
        terminals.forget_rects();
        terminals.sync(&snap, ROWS, COLS);
        assert!(grid_size(&terminals) <= (2, 2), "dropped again");
    }

    // The last one is kept, so the rest of the output lands in a grid that was
    // rebuilt mid-stream — and the history it ends with crosses the splice.
    terminals.keep_hidden_for(None);
    wait_for("the final rebuilt grid", || {
        paint(&terminals, 0);
        grid_size(&terminals) == (ROWS, COLS)
    });
    let rebuilt_at = last_printed(&pane).expect("the pane has printed");
    wait_for("the pane to print past the final rebuild", || {
        last_printed(&pane).is_some_and(|n| n >= rebuilt_at + 300)
    });
    std::fs::write(&stop, b"").expect("stop");
    wait_for("the output to finish", || {
        tmux_text(&pane).contains("finished")
    });
    settle(&terminals, true).await;

    let numbers = numbered_lines(&terminals);
    assert!(
        numbers.len() > 900,
        "the history is there: {}",
        numbers.len()
    );
    assert_contiguous(&numbers, "after the last rebuild");
    assert_eq!(numbers.last().copied(), last_printed(&pane));
}

/// The number of the last `line-N` the pane has printed, as tmux holds it.
fn last_printed(pane: &str) -> Option<u64> {
    tmux_text(pane)
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("line-")?.parse().ok())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rebuilt_terminal_is_the_one_that_was_never_dropped() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin(SOCKET);
    let dir = tempfile::tempdir().expect("tempdir");
    // More than `scrollback_lines` (1000) before the interface attaches, and
    // more after — which arrives while nothing holds a grid for it.
    let before = script_bytes("before", 900);
    let after = script_bytes("after", 400);
    let a = write(dir.path(), "a", &before);
    let b = write(dir.path(), "b", &after);
    let go = dir.path().join("go");
    let pane = pane_running(&format!(
        "sh -c 'cat {a}; while [ ! -e {go} ]; do sleep 0.05; done; cat {b}; exec sleep 100000'",
        go = go.display()
    ));
    wait_for("the first half to reach tmux", || {
        tmux_text(&pane).contains("before prompt>")
    });

    let mut terminals = Terminals::with_registry(std::sync::Arc::new(
        thurbox::backend::wiring::configured().0,
    ));
    let snap = snapshot(&pane);
    attach(&mut terminals, &snap).await;

    std::fs::write(&go, b"").expect("go");
    wait_for("the second half to reach tmux", || {
        tmux_text(&pane).contains("after prompt>")
    });
    settle(&terminals, true).await;
    assert!(
        grid_size(&terminals) <= (2, 2),
        "the second half arrived while there was no grid to put it in"
    );

    let first = paint(&terminals, 0);
    assert!(
        first.iter().any(|line| line.contains("after prompt>")),
        "the first frame must be the current screen, not a stale one:\n{}",
        first.join("\n")
    );

    let mut reference = vt100::Parser::new(ROWS, COLS, 1000);
    reference.process(&through_pty(&before));
    reference.process(&through_pty(&after));

    let parser = agent_parser(&terminals);
    let mut parser = parser.lock().expect("parser");
    assert_eq!(parser.screen().size(), reference.screen().size());
    assert_eq!(
        parser.screen().cursor_position(),
        reference.screen().cursor_position(),
        "the cursor is where the next byte will land"
    );
    let got = all_rows(&mut parser);
    let want = all_rows(&mut reference);
    assert_eq!(got.len(), want.len(), "the same depth of history");
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g, w, "row {i} of {} differs", want.len());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_search_finds_history_in_a_session_nobody_is_looking_at() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin(SOCKET);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut bytes = b"a needle-in-the-history line\n".to_vec();
    bytes.extend(script_bytes("filler", 200));
    let a = write(dir.path(), "a", &bytes);
    let pane = pane_running(&format!("sh -c 'cat {a}; exec sleep 100000'"));
    wait_for("the output to reach tmux", || {
        tmux_text(&pane).contains("filler prompt>")
    });

    let mut terminals = Terminals::with_registry(std::sync::Arc::new(
        thurbox::backend::wiring::configured().0,
    ));
    let snap = snapshot(&pane);
    attach(&mut terminals, &snap).await;
    settle(&terminals, false).await;

    let sources = terminals.search_sources(&[ID.to_string()]);
    let cache = Mutex::new(search::CacheMap::new());
    let answer = search::run(
        Request {
            query: "needle-in-the-history".into(),
            sessions: None,
        },
        &sources,
        &cache,
    );
    let hit = answer
        .hits
        .iter()
        .find(|hit| hit.session == ID && !hit.shell)
        .unwrap_or_else(|| panic!("no hit in the agent pane: {answer:?}"));

    // And the hit lands where it says: showing the session at the hit's offset
    // puts the line on screen.
    let shown = paint(&terminals, hit.scroll as u16);
    assert!(
        shown
            .iter()
            .any(|line| line.contains("needle-in-the-history")),
        "scrolled to {}, the line is not on screen:\n{}",
        hit.scroll,
        shown.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_off_screen_still_reports_its_title_and_its_output() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin(SOCKET);
    let dir = tempfile::tempdir().expect("tempdir");
    let go = dir.path().join("go");
    let pane = pane_running(&format!(
        "sh -c 'echo ready; while [ ! -e {go} ]; do sleep 0.05; done; \
         printf \"\\033]2;busy compiling\\007\"; \
         while :; do echo tick; sleep 0.1; done'",
        go = go.display()
    ));
    wait_for("the pane to start", || tmux_text(&pane).contains("ready"));

    let mut terminals = Terminals::with_registry(std::sync::Arc::new(
        thurbox::backend::wiring::configured().0,
    ));
    let snap = snapshot(&pane);
    attach(&mut terminals, &snap).await;
    settle(&terminals, false).await;
    assert!(
        !terminals.printing().contains(ID),
        "quiet before the signal"
    );

    std::fs::write(&go, b"").expect("go");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        terminals.sync_meta();
        terminals.sync_printing();
        let activity = terminals
            .meta_map()
            .get(ID)
            .and_then(|meta| meta.activity.clone());
        if activity.as_deref() == Some("busy compiling") && terminals.printing().contains(ID) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "never saw the title and the output: title {activity:?}, printing {}",
            terminals.printing().contains(ID)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_title_set_before_the_interface_attached_is_still_reported() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin(SOCKET);
    // The title is set once, before anything attaches, and never again: the
    // only place it still exists is tmux's `#{pane_title}`.
    let pane =
        pane_running("sh -c 'printf \"\\033]2;reviewing the diff\\007\"; exec sleep 100000'");
    wait_for("tmux to have the title", || {
        tmux(&["display-message", "-p", "-t", &pane, "#{pane_title}"]).trim()
            == "reviewing the diff"
    });

    let mut terminals = Terminals::with_registry(std::sync::Arc::new(
        thurbox::backend::wiring::configured().0,
    ));
    let snap = snapshot(&pane);
    attach(&mut terminals, &snap).await;
    assert!(grid_size(&terminals) <= (2, 2), "attached without a grid");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        terminals.sync_meta();
        let activity = terminals
            .meta_map()
            .get(ID)
            .and_then(|meta| meta.activity.clone());
        if activity.as_deref() == Some("reviewing the diff") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the title tmux kept never reached the session list: {activity:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Two-byte characters throughout, so nearly every cut tmux makes in the
/// stream lands inside one.
const CYRILLIC: &str = "съешь же ещё этих мягких французских булок да выпей чаю";

/// tmux hands a pane's output to control mode in `%output` lines cut wherever
/// its read ended — often inside a multi-byte character — and passes bytes
/// `>= 0x80` through raw. Decoding each line as text on its own turned both
/// halves of such a character into U+FFFD, which vt100 drops, so a word lost a
/// letter at random until the agent next redrew it.
///
/// Only output that arrives while the grid is live goes that way: what a pane
/// printed before comes in as a `capture-pane` snapshot of whole lines, which
/// is why every test above that prints first and looks second never saw it.
#[tokio::test(flavor = "multi_thread")]
async fn non_ascii_printed_while_the_grid_is_live_loses_no_character() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin(SOCKET);
    let dir = tempfile::tempdir().expect("tempdir");
    let go = dir.path().join("go");
    let text: String = (0..1000)
        .map(|i| format!("{i:04} {CYRILLIC}\n"))
        .chain(std::iter::once("done> ".to_string()))
        .collect();
    let file = write(dir.path(), "cyrillic", text.as_bytes());
    // It waits for `go`, so every byte of it is live `%output` and none is in
    // the snapshot the grid is rebuilt from.
    let pane = pane_running(&format!(
        "sh -c 'while [ ! -e {go} ]; do sleep 0.05; done; cat {file}; exec sleep 100000'",
        go = go.display()
    ));

    let mut terminals = Terminals::with_registry(std::sync::Arc::new(
        thurbox::backend::wiring::configured().0,
    ));
    let snap = snapshot(&pane);
    attach(&mut terminals, &snap).await;
    wait_for("the grid", || {
        paint(&terminals, 0);
        grid_size(&terminals) == (ROWS, COLS)
    });
    std::fs::write(&go, b"").expect("go");
    wait_for("the output to reach tmux", || {
        tmux_text(&pane).contains("done>")
    });
    settle(&terminals, true).await;

    let parser = agent_parser(&terminals);
    let mut parser = parser.lock().expect("parser");
    let lines: Vec<String> = all_rows(&mut parser)
        .into_iter()
        .map(|(cells, _)| {
            cells
                .iter()
                .map(|cell| match cell.split('|').next().unwrap_or("") {
                    "" => " ",
                    text => text,
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .filter(|line| line.len() > 4 && line.as_bytes()[..4].iter().all(u8::is_ascii_digit))
        .collect();
    assert!(
        lines.len() > 200,
        "only {} lines reached the grid",
        lines.len()
    );
    let damaged: Vec<&String> = lines
        .iter()
        .filter(|line| **line != format!("{} {CYRILLIC}", &line[..4]))
        .collect();
    assert!(
        damaged.is_empty(),
        "{} of {} lines differ from what the pane printed, e.g. {:?}",
        damaged.len(),
        lines.len(),
        &damaged[..damaged.len().min(3)]
    );
}

// --- A remote session off screen sends no output (ADR-P30) ---
//
// Control mode streams every pane's `%output`, so what a host costs the link
// grows with every busy session on it. On an ssh host a pane with no grid has
// its output paused, and what the interface reads off it arrives through a
// tmux format subscription instead. These run the real server behind a
// stand-in `ssh` that records what the control connection carries.

#[cfg(unix)]
mod remote {
    use super::*;

    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use thurbox::backend::tmux::TmuxBackend;
    use thurbox::backend::{Session, SessionBackend};
    use thurbox::session::{HostDef, Platform};
    use thurbox::shell::HostLauncher;

    /// The destination the stand-in `ssh` answers for.
    const DEST: &str = "e2e@hidden.invalid";

    const REMOTE_SESSION: &str = "tbx-hidden";

    /// Put a stand-in `ssh` first on `PATH` that runs the remote command here,
    /// copying what a control-mode connection sends back into `log`.
    fn stand_in_ssh(log: &Path) -> tempfile::TempDir {
        let bin = tempfile::tempdir().expect("tempdir");
        let ssh = bin.path().join("ssh");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\n\
                 while [ \"$1\" != '{DEST}' ]; do shift; done; shift\n\
                 case \"$*\" in\n\
                 *' -C '*) sh -c \"$*\" | tee -a '{log}' ;;\n\
                 *) exec sh -c \"$*\" ;;\n\
                 esac\n",
                log = log.display()
            ),
        )
        .expect("ssh stand-in");
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![bin.path().to_path_buf()];
        dirs.extend(std::env::split_paths(&path));
        std::env::set_var("PATH", std::env::join_paths(dirs).expect("PATH"));
        bin
    }

    fn host_backend(socket: &str) -> Arc<dyn SessionBackend> {
        let host = HostDef {
            name: "hidden".into(),
            destination: DEST.into(),
            socket: Some(socket.into()),
            session: Some(REMOTE_SESSION.into()),
            share_sessions: false,
            ..Default::default()
        };
        Arc::new(TmuxBackend::on_host(
            &host,
            HostLauncher::Ssh {
                destination: DEST.into(),
                ssh_opts: Vec::new(),
            },
            Platform::Posix,
        ))
    }

    async fn wait_within(what: &str, secs: u64, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if ready() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }

    /// `%output` / `%extended-output` lines for `pane` in what the connection
    /// carried after byte `from`.
    fn output_lines(log: &Path, from: usize, pane: &str) -> usize {
        let bytes = std::fs::read(log).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes[from.min(bytes.len())..]).into_owned();
        let plain = format!("%output {pane} ");
        let extended = format!("%extended-output {pane} ");
        text.lines()
            .filter(|line| line.starts_with(&plain) || line.starts_with(&extended))
            .count()
    }

    fn log_len(log: &Path) -> usize {
        std::fs::metadata(log).map_or(0, |m| m.len() as usize)
    }

    /// A pane that prints a numbered line twenty times a second until `stop`
    /// exists, sets its title and two input modes on the way, then prints `FINAL`
    /// and waits for `go` before printing `AFTER`.
    fn busy_pane(server: &TmuxServer, dir: &Path, session: &str) -> String {
        let stop = dir.join("stop");
        let go = dir.join("go");
        let script = format!(
            "i=0; while [ ! -e '{stop}' ]; do i=$((i+1)); printf 'MARK %s\\n' $i; \
             [ $i = 30 ] && printf '\\033]2;hidden-title\\007\\033[?1h\\033[?2004h'; \
             sleep 0.05; done; printf 'FINAL\\n'; \
             while [ ! -e '{go}' ]; do sleep 0.05; done; printf 'AFTER\\n'; exec sleep 600",
            stop = stop.display(),
            go = go.display(),
        );
        let out = server.tmux(&[
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            "80",
            "-y",
            "24",
            "-n",
            "idle",
            "sh",
        ]);
        assert!(out.status.success(), "new-session");
        let out = server.tmux(&[
            "new-window",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            session,
            "-n",
            "tb-hidden",
            &script,
        ]);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn session_text(session: &Session) -> String {
        let parser = session.parser.lock().expect("parser");
        parser.screen().contents()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hidden_remote_pane_sends_no_output_and_keeps_its_signals() {
        if !have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let server = TmuxServer::pin("tbx-hidden-out");
        let dir = tempfile::tempdir().expect("tempdir");
        let log: PathBuf = dir.path().join("control.log");
        let _bin = stand_in_ssh(&log);
        let pane = busy_pane(&server, dir.path(), REMOTE_SESSION);
        assert!(pane.starts_with('%'), "a pane id: {pane:?}");

        let backend = host_backend(server.socket());
        backend.ensure_ready().expect("attach over ssh");
        let session =
            Session::adopt_dormant("hidden".into(), 24, 80, &pane, &backend, HashMap::new())
                .expect("adopt");
        assert!(!session.is_resident(), "adopted without a grid");

        // The pane goes on printing; what reaches the interface for it must not.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mark = log_len(&log);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            output_lines(&log, mark, &pane),
            0,
            "a hidden remote pane's output still crossed the link"
        );

        // What the interface reads off a hidden pane still arrives: the title …
        wait_within("the title set while hidden", 10, || {
            session.agent_title().as_deref() == Some("hidden-title")
        })
        .await;
        // … and that it is printing.
        assert!(
            session.millis_since_last_output() < 3_000,
            "a printing hidden pane reads as quiet: {} ms",
            session.millis_since_last_output()
        );

        // Its hook state, which rides its own subscription.
        let out = server.tmux(&["set-option", "-p", "-t", &pane, "@thurbox_state", "blocked"]);
        assert!(out.status.success(), "set the hook option");
        wait_within("the hook state of a hidden pane", 10, || {
            backend
                .take_hook_state_events()
                .iter()
                .any(|(id, state)| id == &pane && state == "blocked")
        })
        .await;

        // Once it stops, it reads as quiet — the stuck-`working` fallback's input.
        std::fs::write(dir.path().join("stop"), "").expect("stop");
        wait_within("the hidden pane to read as quiet", 15, || {
            session.millis_since_last_output() > 2_500
        })
        .await;

        // Shown again: the grid is the current screen, with the modes set while
        // nothing was streamed, and output flows again.
        assert!(session.restore_pane(false), "asked for the grid");
        assert!(
            session.wait_resident(Duration::from_secs(10)),
            "the grid came back"
        );
        let text = session_text(&session);
        assert!(text.contains("FINAL"), "the current screen: {text}");
        // A tmux that has no `bracket_paste_flag` (3.2) cannot report the mode,
        // and the unit tests pin that it is then left alone.
        let knows_bracket_paste = !String::from_utf8_lossy(
            &server
                .tmux(&[
                    "display-message",
                    "-p",
                    "-t",
                    &pane,
                    "#{bracket_paste_flag}",
                ])
                .stdout,
        )
        .trim()
        .is_empty();
        {
            let parser = session.parser.lock().expect("parser");
            let screen = parser.screen();
            assert!(screen.application_cursor(), "DECCKM set while hidden");
            if knows_bracket_paste {
                assert!(screen.bracketed_paste(), "bracketed paste set while hidden");
            }
        }
        let mark = log_len(&log);
        std::fs::write(dir.path().join("go"), "").expect("go");
        wait_within("output after the pane is shown", 10, || {
            session_text(&session).contains("AFTER")
        })
        .await;
        assert!(
            output_lines(&log, mark, &pane) > 0,
            "output of a shown pane crosses the link"
        );

        session.detach();
        backend.shutdown();
    }

    /// A local pane's output costs no link, and streams as it always has.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hidden_local_pane_still_streams_its_output() {
        if !have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let server = TmuxServer::pin("tbx-hidden-local");
        let dir = tempfile::tempdir().expect("tempdir");
        let pane = busy_pane(&server, dir.path(), SESSION);
        let backend = thurbox::backend::tmux::local();
        backend.ensure_ready().expect("attach");
        let session =
            Session::adopt_dormant("local".into(), 24, 80, &pane, &backend, HashMap::new())
                .expect("adopt");
        wait_within("the title set while hidden", 10, || {
            session.agent_title().as_deref() == Some("hidden-title")
        })
        .await;
        // Twenty lines a second, each its own chunk: streamed, that is dozens of
        // chunks in two seconds; reported instead, it would be two.
        let before = session.output_seq();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let chunks = session.output_seq() - before;
        assert!(
            chunks >= 10,
            "a hidden local pane's output stopped streaming: {chunks} chunks in 2 s"
        );
        session.detach();
        backend.shutdown();
    }
}
