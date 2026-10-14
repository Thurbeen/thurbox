//! A remote session nobody is looking at sends no output over the link.
//!
//! Control mode streams every pane's `%output` to the interface, so the bytes a
//! remote host sends grow with every busy session on it, shown or not. A pane
//! whose grid was dropped (ADR-P27) reads its output only for a handful of
//! signals, so on an ssh host its output is paused instead and those signals
//! come from a tmux format subscription.
//!
//! These run a real tmux server behind a stand-in `ssh` that records what the
//! control connection carries, and pin both halves: no `%output` for the hidden
//! pane, and every signal the interface reads off it — the title, the input
//! modes, when it last printed, its hook state — still arriving, with the grid
//! coming back current and live once it is shown.

#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use thurbox::backend::tmux::TmuxBackend;
use thurbox::backend::tmux_compat::server::TMUX_SESSION;
use thurbox::backend::{Session, SessionBackend};
use thurbox::session::{HostDef, Platform};
use thurbox::shell::HostLauncher;

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

/// The destination the stand-in `ssh` answers for.
const DEST: &str = "e2e@hidden.invalid";

const SESSION: &str = "tbx-hidden";

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|out| out.status.success())
}

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
        session: Some(SESSION.into()),
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

fn wait_for(what: &str, secs: u64, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
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

fn screen_text(session: &Session) -> String {
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
    let pane = busy_pane(&server, dir.path(), SESSION);
    assert!(pane.starts_with('%'), "a pane id: {pane:?}");

    let backend = host_backend(server.socket());
    backend.ensure_ready().expect("attach over ssh");
    let session = Session::adopt_dormant("hidden".into(), 24, 80, &pane, &backend, HashMap::new())
        .expect("adopt");
    assert!(!session.is_resident(), "adopted without a grid");

    // The pane goes on printing; what reaches the interface for it must not.
    std::thread::sleep(Duration::from_millis(1500));
    let mark = log_len(&log);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        output_lines(&log, mark, &pane),
        0,
        "a hidden remote pane's output still crossed the link"
    );

    // What the interface reads off a hidden pane still arrives: the title …
    wait_for("the title set while hidden", 10, || {
        session.agent_title().as_deref() == Some("hidden-title")
    });
    // … and that it is printing.
    assert!(
        session.millis_since_last_output() < 3_000,
        "a printing hidden pane reads as quiet: {} ms",
        session.millis_since_last_output()
    );

    // Its hook state, which rides its own subscription.
    let out = server.tmux(&["set-option", "-p", "-t", &pane, "@thurbox_state", "blocked"]);
    assert!(out.status.success(), "set the hook option");
    wait_for("the hook state of a hidden pane", 10, || {
        backend
            .take_hook_state_events()
            .iter()
            .any(|(id, state)| id == &pane && state == "blocked")
    });

    // Once it stops, it reads as quiet — the stuck-`working` fallback's input.
    std::fs::write(dir.path().join("stop"), "").expect("stop");
    wait_for("the hidden pane to read as quiet", 15, || {
        session.millis_since_last_output() > 2_500
    });

    // Shown again: the grid is the current screen, with the modes set while
    // nothing was streamed, and output flows again.
    assert!(session.restore_pane(false), "asked for the grid");
    assert!(
        session.wait_resident(Duration::from_secs(10)),
        "the grid came back"
    );
    let text = screen_text(&session);
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
    wait_for("output after the pane is shown", 10, || {
        screen_text(&session).contains("AFTER")
    });
    assert!(
        output_lines(&log, mark, &pane) > 0,
        "output of a shown pane crosses the link"
    );

    session.detach();
    backend.shutdown();
}

/// A local pane's output costs no link, and streams as it always has: a
/// dormant local pane printing twenty times a second is never seen quiet for
/// longer than a few of its lines, where a report would come once a second.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hidden_local_pane_still_streams_its_output() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::pin("tbx-hidden-local");
    let dir = tempfile::tempdir().expect("tempdir");
    let pane = busy_pane(&server, dir.path(), TMUX_SESSION);
    let backend = thurbox::backend::tmux::local();
    backend.ensure_ready().expect("attach");
    let session = Session::adopt_dormant("local".into(), 24, 80, &pane, &backend, HashMap::new())
        .expect("adopt");
    wait_for("the title set while hidden", 10, || {
        session.agent_title().as_deref() == Some("hidden-title")
    });
    let mut quietest = 0;
    for _ in 0..40 {
        quietest = quietest.max(session.millis_since_last_output());
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        quietest < 500,
        "a hidden local pane's output stopped streaming: quiet for {quietest} ms"
    );
    session.detach();
    backend.shutdown();
}
