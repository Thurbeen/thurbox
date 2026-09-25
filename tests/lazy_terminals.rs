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
//! still finds history nobody is looking at, and a session off screen still
//! reports its title and its output.

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

/// `agent::tmux::TMUX_SESSION` in a test build — see `tests/attach_by_name.rs`.
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

fn agent_parser(terminals: &Terminals) -> Arc<Mutex<thurbox::agent::SessionParser>> {
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

    let mut terminals = Terminals::new();
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
    let screen = parser.screen();
    rows.truncate(
        rows.len() - usize::from(screen.size().0) + usize::from(screen.cursor_position().0),
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
    // ~2s of steady output, so the rebuilds below land in the middle of it.
    let pane = pane_running(&format!(
        "sh -c 'while [ ! -e {go} ]; do sleep 0.05; done; i=0; \
         while [ $i -lt 3000 ]; do echo line-$i; i=$((i+1)); \
         if [ $((i % 30)) -eq 0 ]; then sleep 0.02; fi; done; \
         echo finished; exec sleep 100000'",
        go = go.display()
    ));
    wait_for("the pane to start", || tmux_text(&pane).contains(""));

    let mut terminals = Terminals::new();
    terminals.keep_hidden_for(Some(Duration::ZERO));
    let snap = snapshot(&pane);
    attach(&mut terminals, &snap).await;

    std::fs::write(&go, b"").expect("go");
    // Each round: shown, which rebuilds the grid where the snapshot lands in
    // the stream; left to take live output for a moment, then checked; then
    // off screen, which drops it again on the next sync.
    let mut rebuilds = 0;
    while !tmux_text(&pane).contains("line-2000") {
        paint(&terminals, 0);
        tokio::time::sleep(Duration::from_millis(30)).await;
        rebuilds += 1;
        assert_contiguous(&numbered_lines(&terminals), &format!("rebuild {rebuilds}"));
        terminals.forget_rects();
        terminals.sync(&snap, ROWS, COLS);
        assert!(grid_size(&terminals) <= (2, 2), "dropped again");
    }
    assert!(rebuilds > 5, "only {rebuilds} rebuilds happened mid-output");

    // The last one is kept, so the rest of the output lands in a grid that was
    // rebuilt mid-stream — and the history it ends with crosses the splice.
    terminals.keep_hidden_for(None);
    paint(&terminals, 0);
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
    assert_eq!(numbers.last(), Some(&2999));
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

    let mut terminals = Terminals::new();
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

    let mut terminals = Terminals::new();
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

    let mut terminals = Terminals::new();
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

    let mut terminals = Terminals::new();
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
