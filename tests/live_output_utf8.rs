//! Non-ASCII text a pane prints while it is attached reaches the grid intact.
//!
//! tmux hands a pane's bytes to control mode in `%output` lines cut wherever its
//! read happened to end, which is often in the middle of a multi-byte UTF-8
//! character, and it passes bytes `>= 0x80` through raw. A reader that decoded
//! each line as text on its own turned both halves of such a character into
//! U+FFFD, which vt100 drops, so a Cyrillic word lost a letter at random until
//! the agent next redrew it.
//!
//! Only output that arrives *after* attaching goes that way: what a pane
//! printed before comes in as a `capture-pane` snapshot of whole lines, which is
//! why the harnesses that print first and attach second never saw it. Driven
//! against a real tmux server on a private socket, because the thing under test
//! is tmux's own framing.

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use thurbox::kernel::paint::SurfaceProvider;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::terminal::Terminals;
use thurbox::session::SessionState;

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SOCKET: &str = "thurbox-utf8-test";

/// `agent::tmux::TMUX_SESSION` in a test build — see `tests/attach_by_name.rs`.
const SESSION: &str = "thurbox-dev";

const ID: &str = "33333333-3333-3333-3333-333333333333";

const ROWS: u16 = 24;
const COLS: u16 = 100;

/// Two-byte characters throughout, so nearly every cut tmux makes lands inside
/// one.
const SENTENCE: &str = "съешь же ещё этих мягких французских булок да выпей чаю";

const LINES: usize = 1000;

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

fn snapshot(pane: &str) -> Snapshot {
    Snapshot {
        sessions: vec![SessionRow {
            id: ID.into(),
            name: "utf8".into(),
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
        }],
        ..Snapshot::default()
    }
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

fn paint(terminals: &Terminals) {
    let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).expect("terminal");
    term.draw(|frame| {
        assert!(terminals.render_session(frame, Rect::new(0, 0, COLS, ROWS), ID, 0));
    })
    .expect("draw");
}

fn agent_parser(terminals: &Terminals) -> Arc<Mutex<thurbox::agent::SessionParser>> {
    terminals
        .search_sources(&[ID.to_string()])
        .into_iter()
        .find(|source| !source.shell)
        .expect("the agent pane is a search source")
        .parser
}

/// Every row the grid holds, history first, as text.
fn grid_lines(terminals: &Terminals) -> Vec<String> {
    let parser = agent_parser(terminals);
    let mut parser = parser.lock().expect("parser");
    let (rows, cols) = parser.screen().size();
    parser.screen_mut().set_scrollback(usize::MAX);
    let depth = parser.screen().scrollback();
    let mut seen = std::collections::BTreeMap::new();
    let mut offset = depth;
    loop {
        parser.screen_mut().set_scrollback(offset);
        for (r, text) in parser.screen().rows(0, cols).enumerate() {
            seen.entry(depth - offset + r).or_insert(text);
        }
        if offset == 0 {
            break;
        }
        offset = offset.saturating_sub(usize::from(rows));
    }
    parser.screen_mut().set_scrollback(0);
    seen.into_values().collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn cyrillic_printed_while_attached_loses_no_letter() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin(SOCKET);
    let dir = tempfile::tempdir().expect("tempdir");
    let go = dir.path().join("go");
    let text: String = (0..LINES)
        .map(|i| format!("{i:04} {SENTENCE}\n"))
        .chain(std::iter::once("done> ".to_string()))
        .collect();
    let file = dir.path().join("cyrillic");
    std::fs::write(&file, text).expect("write the output");

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
    // It waits for `go`, so every byte of it is live `%output` and none is in
    // the snapshot taken on attach.
    let pane = tmux(&[
        "new-window",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        SESSION,
        "-n",
        "tb-utf8",
        &format!(
            "sh -c 'while [ ! -e {go} ]; do sleep 0.05; done; cat {file}; exec sleep 100000'",
            go = go.display(),
            file = file.display()
        ),
    ])
    .trim()
    .to_string();

    let mut terminals = Terminals::new();
    let snap = snapshot(&pane);
    wait_for("the pane to attach", || {
        terminals.sync(&snap, ROWS, COLS);
        terminals.is_attached(ID)
    });
    wait_for("the grid", || {
        paint(&terminals);
        agent_parser(&terminals)
            .lock()
            .expect("parser")
            .screen()
            .size()
            == (ROWS, COLS)
    });

    std::fs::write(&go, b"").expect("go");
    wait_for("the output to reach the grid", || {
        grid_lines(&terminals)
            .last()
            .is_some_and(|last| last.starts_with("done>"))
    });

    let numbered: Vec<String> = grid_lines(&terminals)
        .into_iter()
        .filter(|line| line.len() > 4 && line[..4].bytes().all(|b| b.is_ascii_digit()))
        .collect();
    assert!(
        numbered.len() > 200,
        "only {} lines reached the grid",
        numbered.len()
    );
    let damaged: Vec<&String> = numbered
        .iter()
        .filter(|line| line.trim_end() != format!("{} {SENTENCE}", &line[..4]))
        .collect();
    assert!(
        damaged.is_empty(),
        "{} of {} lines differ from what the pane printed, e.g. {:?}",
        damaged.len(),
        numbered.len(),
        damaged.iter().take(3).collect::<Vec<_>>()
    );
}
