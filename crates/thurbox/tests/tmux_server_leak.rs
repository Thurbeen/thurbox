use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;

#[path = "../../runtime-mux/tests/support/tmux_server.rs"]
mod tmux_server;

#[cfg(unix)]
use tmux_server::TmuxServer;

#[cfg(unix)]
fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// --- the panic that used to leak ------------------------------------------

#[cfg(unix)]
const PROBE_REPORT_ENV: &str = "THURBOX_PANIC_PROBE_REPORT";

#[cfg(unix)]
const PROBE: &str = "a_harness_that_panics_after_starting_a_server";

#[cfg(unix)]
fn server_processes(socket: &str) -> Vec<String> {
    let out = Command::new("ps")
        .args(["-eo", "pid=,args="])
        .output()
        .expect("run ps");
    let needle = format!("-L {socket}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.contains(&needle))
        .map(str::trim)
        .map(str::to_string)
        .collect()
}

#[test]
#[cfg(unix)]
fn a_panicking_harness_still_reaps_its_tmux_server() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let report_dir = tempfile::tempdir().expect("tempdir");
    let report = report_dir.path().join("socket");
    let exe = std::env::current_exe().expect("this test binary");
    let out = Command::new(&exe)
        .args(["--exact", "--nocapture", PROBE])
        .env(PROBE_REPORT_ENV, &report)
        .output()
        .expect("run a child copy of the test binary");

    assert!(
        !out.status.success(),
        "the probe was supposed to panic, and did not:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // An empty report means the probe never got a server started, and there is
    // nothing for the assertion below to be about: passing it then would be
    // passing for the wrong reason.
    let socket = std::fs::read_to_string(&report).unwrap_or_default();
    let socket = socket.trim();
    if socket.is_empty() {
        eprintln!("skipping: the probe could not start a tmux server");
        return;
    }

    let survivors = server_processes(socket);
    assert!(
        survivors.is_empty(),
        "a harness panicked and left its tmux server running — and with its \
         socket directory gone with it, nothing can connect to reap it:\n  {}",
        survivors.join("\n  ")
    );
}

#[test]
#[cfg(unix)]
fn a_harness_that_panics_after_starting_a_server() {
    let Some(report) = std::env::var_os(PROBE_REPORT_ENV) else {
        return;
    };
    if !have_tmux() {
        return;
    }

    let server = TmuxServer::pin(&format!("thurbox-panic-probe-{}", std::process::id()));
    // A pane that outlives the test: one that exits takes the server with it
    // and would turn this probe into a test of tmux's own idle shutdown.
    let started = server.tmux(&["new-session", "-d", "-s", "probe", "sleep", "300"]);
    // The report is what says a server exists to leak; the panic happens
    // either way, so the caller reads one failing child rather than having to
    // tell "could not start a server" from "did not panic".
    if started.status.success() {
        std::fs::write(&report, server.socket()).expect("report the socket");
    }

    panic!("the probe panics here, as a failed assertion would");
}

fn code_of(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const WINDOW: usize = 15;

const SET: [&str; 2] = ["set_var(", ".env("];
const CLEAR: [&str; 2] = ["remove_var(", "env_remove("];

fn touches(line: &str, prev: &str, konst: Option<&str>, literal: &str, verbs: &[&str]) -> bool {
    let line = line.trim_start();
    if line.starts_with("use ") {
        return false;
    }
    let verbed = verbs.iter().any(|v| line.contains(v));
    // A real use of the constant, not this file's own description of one:
    // `marks(Some("SOCKET_OVERRIDE_ENV"), …)` names it inside a string, and a
    // use never is.
    let names_konst = konst.is_some_and(|k| {
        line.match_indices(k)
            .any(|(at, _)| at == 0 || !line[..at].ends_with('"'))
    });
    if names_konst {
        // …and it still has to be a set or a clear. `var(SOCKET_OVERRIDE_ENV)`
        // reads the socket rather than moving it, and counting that as a pin
        // would fail a harness for asking a question.
        let continued = prev.trim_end().ends_with('(') && verbs.iter().any(|v| prev.contains(v));
        return verbed || continued;
    }
    line.contains(literal) && verbed
}

fn marks(lines: &[&str], konst: Option<&str>, literal: &str, verbs: &[&str]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(i, l)| {
            touches(
                l,
                if *i == 0 { "" } else { lines[i - 1] },
                konst,
                literal,
                verbs,
            )
        })
        .map(|(i, _)| i)
        .collect()
}

fn claim_nearest(xs: &mut Vec<usize>, at: usize) -> bool {
    let found = xs
        .iter()
        .enumerate()
        .filter(|(_, x)| x.abs_diff(at) <= WINDOW)
        .min_by_key(|(_, x)| x.abs_diff(at))
        .map(|(i, _)| i);
    match found {
        Some(i) => {
            xs.remove(i);
            true
        }
        None => false,
    }
}

fn scope_failures(src: &str) -> (usize, Vec<String>) {
    let code = code_of(src);
    let lines: Vec<&str> = code.lines().map(str::trim_end).collect();

    let pinned = marks(
        &lines,
        Some("SOCKET_OVERRIDE_ENV"),
        "\"THURBOX_SOCKET\"",
        &SET,
    );
    let mut clears = marks(
        &lines,
        Some("SOCKET_OWNER_ENV"),
        "\"THURBOX_SOCKET_FOR\"",
        &CLEAR,
    );
    // No crate constant for this one: tmux's own variable, set by name.
    let mut scopes = marks(&lines, None, "\"TMUX_TMPDIR\"", &SET);

    let mut failures = Vec::new();
    for at in &pinned {
        let mut missing = Vec::new();
        if !claim_nearest(&mut clears, *at) {
            missing.push("clear THURBOX_SOCKET_FOR");
        }
        if !claim_nearest(&mut scopes, *at) {
            missing.push("set TMUX_TMPDIR to a directory of its own");
        }
        if !missing.is_empty() {
            failures.push(format!("{} must {}", at + 1, missing.join(" and ")));
        }
    }
    (pinned.len(), failures)
}

const GUARD: &str = "crates/runtime-mux/tests/support/tmux_server.rs";

const HARNESS_DIRS: [&str; 2] = ["crates/thurbox/tests", "crates/runtime-mux/tests"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

const GUARD_TYPE: &str = "TmuxServer";

#[test]
fn the_guard_scopes_every_socket_it_pins() {
    let src = std::fs::read_to_string(workspace_root().join(GUARD)).expect("read the guard");
    let (pins, failures) = scope_failures(&src);
    assert!(
        pins > 0,
        "{GUARD} pins no socket — this gate has stopped looking at anything"
    );
    assert!(
        failures.is_empty(),
        "these pins in {GUARD} leak a tmux server when the suite runs inside a \
         thurbox pane:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn no_harness_pins_a_socket_outside_the_guard() {
    // The one file that is *about* the resolution, sets the pair on purpose,
    // and never starts a multiplexer.
    const EXEMPT: [&str; 1] = ["cli_socket_isolation.rs"];

    let mut offenders = Vec::new();
    let mut guards = 0usize;
    let entries = HARNESS_DIRS.iter().flat_map(|dir| {
        std::fs::read_dir(workspace_root().join(dir))
            .expect("read a tests directory")
            .flatten()
    });
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || EXEMPT.contains(&name.as_str()) {
            continue;
        }
        let src = std::fs::read_to_string(entry.path()).expect("read test source");
        let code = code_of(&src);
        let lines: Vec<&str> = code.lines().map(str::trim_end).collect();
        for at in marks(
            &lines,
            Some("SOCKET_OVERRIDE_ENV"),
            "\"THURBOX_SOCKET\"",
            &SET,
        ) {
            offenders.push(format!(
                "{name}:{} pins a socket by hand — pin it with {GUARD_TYPE} \
                 ({GUARD}), which reaps the server when it is dropped",
                at + 1
            ));
        }
        for (i, line) in lines.iter().enumerate() {
            let held = held_guard(line, lines.get(i + 1).unwrap_or(&""));
            guards += usize::from(held == Some(true));
            if held == Some(false) {
                offenders.push(format!(
                    "{name}:{} drops its guard where it builds it — the server \
                     is reaped before the test has started",
                    i + 1
                ));
            }
        }
    }

    assert!(
        guards > 0,
        "no harness holds a {GUARD_TYPE} — this gate has stopped looking at \
         anything"
    );
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "these leak a tmux server whenever the test does not reach its own \
         teardown:\n  {}",
        offenders.join("\n  ")
    );
}

fn held_guard(line: &str, next: &str) -> Option<bool> {
    let ctor = format!("{GUARD_TYPE}::");
    let line = line.trim_start();
    let at = line.find(&ctor)?;
    let before = line[..at].trim_end();
    let bound = (before.ends_with('=') && !before.starts_with("let _ ="))
        || before.ends_with(':')
        || before.ends_with('(')
        || before.ends_with(',');
    // `next` because rustfmt breaks a method chain across lines, and a chain
    // whose first link sits on the following line is the same temporary.
    let rest = after_call(&line[at + ctor.len()..]);
    let rest = if rest.is_empty() {
        next.trim_start()
    } else {
        rest
    };
    Some(bound && !rest.starts_with('.'))
}

fn after_call(after: &str) -> &str {
    let mut depth = 0usize;
    for (at, c) in after.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return after[at + 1..].trim_start();
                }
            }
            _ => {}
        }
    }
    ""
}
