//! One session, one window per role — and a server already holding two.
//!
//! A restart kills the session's window and then spawns its replacement, and
//! in between the session is indistinguishable from one whose agent died —
//! which is exactly what a repairer relaunches (the interface's
//! `respawn_missing_agents`, a peer's `restart --if-missing`, extension
//! self-heal). Both then spawn and stamp, and the pair they leave is what
//! ADR-25 cannot answer: `stamped_match` returns `Located::Unknown` for two
//! windows carrying one stamp, so the session can no longer be sent to,
//! killed, captured or renamed (issue #1207).
//!
//! Two halves are asserted here and they are not the same claim. The stamp
//! paths **retire** the loser, so the pair is never left behind; and a pair a
//! server is already carrying — which no prevention reaches, because those
//! windows exist — is repaired the first time something tries to act on the
//! session. What is *not* done is teaching the resolver to pick one of two:
//! `the_listing_itself_still_refuses_a_pair` is the guard on that, and it is
//! the reason the repair is a write rather than a reading.
//!
//! Driven against a real tmux server on a private socket: the invariant is
//! about what the server holds, and nothing in-process stands in for it.

use std::collections::HashMap;
use std::sync::{Arc, Barrier};

use thurbox::agent::tmux::{self, Located};

/// The guard every tmux server in this file is reaped by — see its own doc.
#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

/// A socket of this test's own, so it can never see — or kill — a real session.
const SOCKET: &str = "thurbox-one-window-per-stamp";

const SESSION: &str = "11111111-2222-3333-4444-555555555555";
const OTHER: &str = "99999999-8888-7777-6666-555555555555";
const NAME: &str = "lead";

fn have_tmux() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// A window running a program that outlives the test's own commands.
fn spawn(session_id: &str, name: &str) -> String {
    tmux::spawn_window(
        session_id,
        name,
        "sh",
        &["-c".to_string(), "while :; do sleep 1; done".to_string()],
        None,
        &HashMap::new(),
    )
    .unwrap_or_else(|e| panic!("spawn {name}: {e:#}"))
}

/// Every pane on the test's server with the window and stamp it carries.
fn listing(server: &TmuxServer) -> String {
    String::from_utf8_lossy(
        &server
            .tmux(&[
                "list-panes",
                "-a",
                "-F",
                "#{pane_id}|#{window_id}|#{window_name}|#{@thurbox_session}|#{@thurbox_role}",
            ])
            .stdout,
    )
    .into_owned()
}

/// The panes of every window stamped as `SESSION`'s agent.
fn stamped_panes(server: &TmuxServer) -> Vec<String> {
    listing(server)
        .lines()
        .filter(|line| line.ends_with(&format!("|{SESSION}|agent")))
        .filter_map(|line| line.split('|').next().map(str::to_string))
        .collect()
}

/// Give `pane`'s window `SESSION`'s agent stamp by hand.
///
/// By hand on purpose: a *spawn* retires the window it would have paired with,
/// so the only way to plant the state an operator's server is already in is to
/// write the option outside thurbox.
fn plant_stamp(server: &TmuxServer, pane: &str) {
    for (option, value) in [
        (tmux::WINDOW_SESSION_OPTION, SESSION),
        (tmux::WINDOW_ROLE_OPTION, "agent"),
    ] {
        let out = server.tmux(&["set-option", "-w", "-t", pane, option, value]);
        assert!(out.status.success(), "set {option}: {out:?}");
    }
}

#[test]
fn a_second_spawn_for_one_session_retires_the_window_the_first_left() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(SOCKET);

    let first = spawn(SESSION, NAME);
    // No kill in between, on purpose: this is the state a repairer's relaunch
    // lands in when it runs between a restart's kill and its spawn — nothing
    // there for it to kill, and a second window about to appear beside the one
    // the restart is still creating.
    let second = spawn(SESSION, NAME);

    let index = tmux::local_window_index().expect("list windows");
    assert_eq!(
        index.agent_window(SESSION, NAME),
        Located::At(second.clone()),
        "the session must resolve to the window its last spawn made; server holds:\n{}",
        listing(&server)
    );
    assert_eq!(
        stamped_panes(&server),
        vec![second],
        "the window the first spawn left ({first}) must be gone; server holds:\n{}",
        listing(&server)
    );

    // The symptom as the operator meets it: an unresolvable session is one
    // nothing can be sent to.
    tmux::send_text_now(SESSION, NAME, "true", false)
        .unwrap_or_else(|e| panic!("send to a session with one window: {e:#}"));
}

#[test]
fn a_repairers_relaunch_inside_a_restart_leaves_the_session_one_window() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(SOCKET);

    // The agent that is running when the restart arrives.
    spawn(SESSION, NAME);

    // `respawn_local` is kill → spawn, and this is the moment between them.
    tmux::kill_window(SESSION, NAME).expect("the restart's kill");
    let index = tmux::local_window_index().expect("list windows");
    assert!(
        index.live_agent_window(SESSION, NAME).is_absent(),
        "the premise: a repairer asks whether the window is gone and is told yes"
    );

    // `relaunch_is_owed` is satisfied, so the repairer spawns — and the
    // restart's own spawn arrives afterwards, against a row it still owns.
    let repaired = spawn(SESSION, NAME);
    let restarted = spawn(SESSION, NAME);

    assert_eq!(
        stamped_panes(&server),
        vec![restarted.clone()],
        "the repairer's window ({repaired}) must not survive beside the restart's; \
         server holds:\n{}",
        listing(&server)
    );
    let index = tmux::local_window_index().expect("list windows");
    assert_eq!(
        index.agent_window(SESSION, NAME),
        Located::At(restarted),
        "one window, so the session resolves again; server holds:\n{}",
        listing(&server)
    );
    tmux::send_text_now(SESSION, NAME, "true", false)
        .unwrap_or_else(|e| panic!("send after a relaunch inside a restart: {e:#}"));
}

#[test]
fn two_restarts_at_once_still_leave_the_session_one_window() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(SOCKET);

    // The window both restarts are replacing.
    spawn(SESSION, NAME);

    // Released together so the kills and the spawns interleave rather than
    // being serialised by thread startup — the retirement's rule has to hold
    // whatever the order, which is why it keeps the highest window id rather
    // than "the one I just made".
    let barrier = Arc::new(Barrier::new(2));
    let restarts: Vec<_> = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let _ = tmux::kill_window(SESSION, NAME);
                spawn(SESSION, NAME)
            })
        })
        .collect();
    let spawned: Vec<String> = restarts
        .into_iter()
        .map(|t| t.join().expect("join"))
        .collect();

    let survivors = stamped_panes(&server);
    assert_eq!(
        survivors.len(),
        1,
        "two overlapping restarts must leave one window; they spawned {spawned:?} \
         and the server holds:\n{}",
        listing(&server)
    );
    let index = tmux::local_window_index().expect("list windows");
    assert_eq!(
        index.agent_window(SESSION, NAME),
        Located::At(survivors[0].clone()),
        "the surviving window must be the one the session resolves to; server holds:\n{}",
        listing(&server)
    );
    tmux::send_text_now(SESSION, NAME, "true", false)
        .unwrap_or_else(|e| panic!("send after two overlapping restarts: {e:#}"));
}

#[test]
fn a_pair_already_on_the_server_becomes_addressable_again() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(SOCKET);

    // The shape a machine is already in: a stale window from an earlier race
    // and the session's own beside it. Planted in this order so the stale one
    // holds the lower window id, and stamped only once both exist — no spawn
    // sees the pair, which is what makes this the *already broken* case rather
    // than the prevented one. An operator's server that has held such a pair
    // for a day is reached by nothing else: the windows are already there.
    let stale = spawn(OTHER, "other");
    let mine = spawn(SESSION, NAME);
    plant_stamp(&server, &stale);
    assert_eq!(
        stamped_panes(&server).len(),
        2,
        "the premise: one id on two windows; server holds:\n{}",
        listing(&server)
    );

    // Acting on the session is what repairs it — the same verb the operator
    // found refused ("has no window of its own here").
    tmux::send_text_now(SESSION, NAME, "true", false)
        .unwrap_or_else(|e| panic!("send to a session a pair had locked out: {e:#}"));

    assert_eq!(
        stamped_panes(&server),
        vec![mine],
        "the older window must have been retired, not merely stepped over; \
         server holds:\n{}",
        listing(&server)
    );
    let index = tmux::local_window_index().expect("list windows");
    assert!(
        matches!(index.agent_window(SESSION, NAME), Located::At(_)),
        "and the session resolves on its own from then on; server holds:\n{}",
        listing(&server)
    );
}

#[test]
fn the_listing_itself_still_refuses_a_pair() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin(SOCKET);

    // The repair above is a write — a window is retired and the next listing
    // has one answer. `WindowIndex` is not part of it: its refusal is the last
    // guard against addressing the wrong pane, and a resolver taught to pick
    // one of two would hide this defect rather than fix it.
    let mine = spawn(SESSION, NAME);
    let impostor = spawn(OTHER, "other");
    plant_stamp(&server, &impostor);

    let index = tmux::local_window_index().expect("list windows");
    assert_eq!(
        index.agent_window(SESSION, NAME),
        Located::Unknown,
        "two windows carry one stamp and the listing must refuse them; server holds:\n{}",
        listing(&server)
    );
    assert_eq!(
        index.live_agent_window(SESSION, NAME),
        Located::Unknown,
        "liveness does not make the ambiguity go away either; its own window is {mine}"
    );
}
