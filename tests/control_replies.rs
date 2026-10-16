//! Every command written down a control connection is answered, and each
//! answer has to reach the command that asked for it.
//!
//! The reader hands `%begin`/`%end` blocks to waiters strictly in order, so a
//! line written with no waiter of its own has its answer handed to whichever
//! command was queued next. Typing into a remote pane while the interface lists
//! that host's windows then reads the keystroke's empty reply as the listing:
//! the host holds no windows, every session on it has lost its pane, and all of
//! them read `Unreachable` at once while the link is perfectly fine.

use std::collections::HashMap;
use std::io::Write;

use thurbox::backend::tmux::TmuxBackend;
use thurbox::backend::SessionBackend;

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

fn have_tmux() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn a_keystroke_never_answers_the_next_command() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _server = TmuxServer::pin("thurbox-control-replies");
    let backend = TmuxBackend::new();
    backend.ensure_ready().expect("the backend readies");
    let pane = backend
        .spawn("tb-replies", "cat", &[], None, &HashMap::new(), 24, 80)
        .expect("spawn")
        .backend_id;
    let mut adopted = backend
        .adopt(&pane, 24, 80, Some(Vec::new()))
        .expect("adopt");

    let mut wrong = Vec::new();
    for n in 0..50 {
        adopted.input.write_all(b"x").expect("type");
        adopted.input.flush().expect("flush");
        // A round trip of its own on the control connection, answered with
        // the pane's pid — or, handed the keystroke's empty reply, with none.
        if !matches!(backend.pane_pid(&pane), Ok(Some(_))) {
            wrong.push(n);
        }
    }
    backend.shutdown();
    assert!(
        wrong.is_empty(),
        "{} of 50 commands right after a keystroke were not answered with the pane typed \
         into (attempts {wrong:?})",
        wrong.len()
    );
}
