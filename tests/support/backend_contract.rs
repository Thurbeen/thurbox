//! What every `SessionBackend` must do, as one suite each implementation runs.
//!
//! `TmuxBackend` runs it under a private server and the in-memory
//! `RecordingBackend` runs it always, so the fake the routing tests trust is
//! held to the behaviour of the adapter it stands in for. A check here is about
//! what a caller can observe through the trait — a window's identity, whether a
//! listing places it, whether a kill sticks — never about how a backend does it.

#![allow(dead_code)]

use std::collections::HashMap;

use thurbox::backend::identity::{Located, WindowIndex};
use thurbox::backend::{Key, Owner, Placed, SessionBackend, WindowRole, WindowSpec};

/// Three rows, as their stamps. Uuid-shaped, as a real row's are.
const OWNER_A: &str = "00000000-0000-4000-8000-00000000000a";
const OWNER_B: &str = "00000000-0000-4000-8000-00000000000b";
const OWNER_C: &str = "00000000-0000-4000-8000-00000000000c";
const OWNER_D: &str = "00000000-0000-4000-8000-00000000000d";

/// Run the contract against `backend`, which must hold no thurbox window
/// named after `contract` when called.
pub fn suite(backend: &dyn SessionBackend) {
    backend.ensure_ready().expect("the backend readies");
    let env = HashMap::new();
    let spawn = |name: &str| {
        backend
            .spawn(name, "sleep", &["300".to_string()], None, &env, 24, 80)
            .unwrap_or_else(|e| panic!("spawn {name}: {e:#}"))
            .backend_id
    };

    // A window, once stamped, is listed as its owner's in its role.
    let a = spawn("tb-contract");
    backend
        .stamp_window(&a, OWNER_A, WindowRole::Agent)
        .expect("stamp");
    let listed = backend.discover().expect("discover");
    let window = listed
        .iter()
        .find(|w| w.backend_id == a)
        .unwrap_or_else(|| panic!("discover does not list the window it spawned ({a})"));
    assert_eq!(window.name, "tb-contract");
    assert_eq!(window.session, OWNER_A, "the stamp reads back as the owner");
    assert_eq!(window.role, WindowRole::Agent);
    assert!(window.is_alive);

    // A namesake stamped for another row is that row's, and a row with no
    // window of its own is never handed either of them.
    let b = spawn("tb-contract");
    backend
        .stamp_window(&b, OWNER_B, WindowRole::Agent)
        .expect("stamp the namesake");
    let index = WindowIndex::from_listing(backend.discover().expect("discover"));
    assert_eq!(
        index.agent_window(OWNER_A, "contract"),
        Located::At(a.clone())
    );
    assert_eq!(
        index.agent_window(OWNER_B, "contract"),
        Located::At(b.clone())
    );
    assert_eq!(
        index.agent_window(OWNER_C, "contract").pane(),
        None,
        "a row with no window claimed a namesake stamped for another"
    );

    // The exact-name lookup reports every window of the name.
    let mut named: Vec<String> = backend
        .window_panes("tb-contract")
        .expect("window_panes")
        .into_iter()
        .map(|(pane, _)| pane)
        .collect();
    named.sort();
    let mut expected = vec![a.clone(), b.clone()];
    expected.sort();
    assert_eq!(named, expected);

    // Killing is idempotent: a pane already gone is what a kill wanted.
    backend.kill(&a).expect("kill");
    backend
        .kill(&a)
        .expect("a second kill of the same pane is not an error");
    let after = backend.discover().expect("discover");
    assert!(
        after.iter().all(|w| w.backend_id != a),
        "a killed window is still listed"
    );
    assert!(
        after.iter().any(|w| w.backend_id == b),
        "killing one window took its namesake down too"
    );

    backend.kill(&b).expect("kill the namesake");
}

/// The headless half of the contract: what `session_ops` drives a session's
/// lifecycle with, through a backend nothing has attached to — so, for a
/// multiplexer, without opening a connection that would bring a server into
/// being where a teardown found none.
pub fn lifecycle(backend: &dyn SessionBackend) {
    let env = HashMap::new();
    let args = ["300".to_string()];
    let owner = Owner::new(OWNER_A, "headless");
    let spec = |owner| WindowSpec {
        owner,
        role: WindowRole::Agent,
        command: "sleep",
        args: &args,
        cwd: None,
        env: &env,
    };

    // Created stamped for its owner, and named by thurbox's convention.
    let pane = backend.create_window(&spec(owner)).expect("create_window");
    let listed = backend.discover().expect("discover");
    let window = listed
        .iter()
        .find(|w| w.backend_id == pane)
        .unwrap_or_else(|| panic!("the created window {pane} is not listed"));
    assert_eq!(window.name, "tb-headless");
    assert_eq!(window.session, OWNER_A);
    assert_eq!(window.role, WindowRole::Agent);
    assert_eq!(
        backend.locate(owner).expect("locate"),
        Placed {
            agent: Located::At(pane.clone()),
            shell: Located::Absent,
        }
    );
    assert!(
        backend.pane_pid(&pane).expect("pane_pid").is_some(),
        "a running pane has a pid"
    );

    // Another row is never handed this one's window, namesake or not.
    let stranger = Owner::new(OWNER_B, "headless");
    assert_eq!(
        backend.locate(stranger).expect("locate a stranger").agent,
        Located::Absent
    );

    // A rename follows the owner's window and keeps its stamp.
    backend
        .rename_windows(owner, "moved")
        .expect("rename_windows");
    let renamed = Owner::new(OWNER_A, "moved");
    assert_eq!(
        backend.locate(renamed).expect("locate").agent,
        Located::At(pane.clone())
    );
    assert!(backend
        .discover()
        .expect("discover")
        .iter()
        .any(|w| w.backend_id == pane && w.name == "tb-moved" && w.session == OWNER_A));

    // A second window for the same owner and role keeps one of the two, and
    // the owner resolves to it: one session, one window per role (ADR-25).
    let again = backend.create_window(&spec(renamed)).expect("create again");
    assert_eq!(
        backend.locate(renamed).expect("locate").agent,
        Located::At(again.clone()),
        "the newer window keeps the identity"
    );

    // Killing is idempotent without an attachment too.
    backend.kill(&again).expect("kill");
    backend.kill(&again).expect("a second kill is not an error");
    backend
        .kill(&pane)
        .expect("kill the older window, if it is still there");
    assert_eq!(
        backend.locate(renamed).expect("locate"),
        Placed {
            agent: Located::Absent,
            shell: Located::Absent,
        }
    );
}

/// Pane I/O: what reaches a pane through the verbs every caller shares —
/// located by its owner, then addressed by pane.
pub fn pane_io(backend: &dyn SessionBackend) {
    let env = HashMap::new();
    let owner = Owner::new(OWNER_C, "io");
    let open = |command: &'static str, args: &[String]| {
        backend
            .create_window(&WindowSpec {
                owner,
                role: WindowRole::Agent,
                command,
                args,
                cwd: None,
                env: &env,
            })
            .expect("create_window")
    };
    let pane = open("cat", &[]);
    assert_eq!(
        backend.locate(owner).expect("locate").agent,
        Located::At(pane.clone())
    );

    // Text lands literally; submitted, it is one line; unsubmitted, it waits.
    backend
        .send_text(&pane, "-first line", true)
        .expect("send_text");
    backend
        .send_text(&pane, "second", false)
        .expect("send_text unsubmitted");
    let key = Key::parse("enter").expect("enter");
    assert!(
        !backend.send_key(&pane, &key).expect("send_key").is_empty(),
        "a key is reported as the backend spelled it"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut screen = String::new();
    while std::time::Instant::now() < deadline {
        screen = backend.capture(&pane, 50, false).expect("capture");
        if screen.contains("-first line") && screen.contains("second") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        screen.contains("-first line") && screen.contains("second"),
        "capture reads back what was typed: {screen:?}"
    );

    let state = backend.pane_state(&pane).expect("pane_state");
    assert_eq!(state.dead, Some(false), "a running pane is not dead");
    assert!(
        backend.pane_path(&pane).is_ok(),
        "a present pane's PATH is answered, known or not"
    );

    // A pane gone is not somebody else's to answer for.
    backend.kill(&pane).expect("kill");
    assert_eq!(
        backend.pane_state(&pane).unwrap_or_default(),
        thurbox::backend::PaneState::default(),
        "a gone pane's state is no answer"
    );
    assert!(
        backend.send_text(&pane, "nowhere", true).is_err(),
        "a send to a gone pane reported success"
    );
}

/// Status delivery and the heartbeat, for a backend that has a status
/// channel: a state recorded on a pane is what the headless listing reads back
/// for that pane and no other, and the hook command it hands out can be
/// spliced into a hook file. The heartbeat is kept, found, never listed as a
/// session, and stopped — once.
pub fn status(backend: &dyn SessionBackend) {
    let env = HashMap::new();
    let open = |owner: &'static str| {
        backend
            .create_window(&WindowSpec {
                owner: Owner::new(owner, owner),
                role: WindowRole::Agent,
                command: "sleep",
                args: &["300".to_string()],
                cwd: None,
                env: &env,
            })
            .expect("create_window")
    };
    let reporting = open(OWNER_C);
    let quiet = open(OWNER_D);

    let command = backend
        .hook_signal_command()
        .expect("a backend with a status channel names its hook command");
    assert!(
        !command.contains(['"', '\\']) && command.ends_with(' '),
        "a hook command must splice into a JSON string, the state after it: {command:?}"
    );

    backend
        .record_hook_state(&reporting, "blocked")
        .expect("record_hook_state");
    let states = backend.hook_states().expect("hook_states");
    assert!(
        states.contains(&(reporting.clone(), "blocked".to_string())),
        "the listing reads back the recorded state: {states:?}"
    );
    assert!(
        !states
            .iter()
            .any(|(pane, state)| *pane == quiet && !state.is_empty()),
        "a pane that reported nothing has no state: {states:?}"
    );
    assert!(
        backend.record_hook_state("%999999", "done").is_err(),
        "a state recorded on no pane reported success"
    );

    let program = std::path::Path::new("/bin/true");
    let args = ["automation".to_string(), "tick".to_string()];
    let every = std::time::Duration::from_secs(60);
    assert!(!backend.heartbeat_running().expect("asked"));
    backend
        .ensure_heartbeat(program, &args, every)
        .expect("ensure_heartbeat");
    backend
        .ensure_heartbeat(program, &args, every)
        .expect("ensure_heartbeat again");
    assert!(backend.heartbeat_running().expect("asked"));
    let names: Vec<String> = backend
        .discover()
        .expect("discover")
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert!(
        names.iter().all(|name| name.starts_with("tb")),
        "the heartbeat is not a session: {names:?}"
    );
    assert!(
        backend.stop_heartbeat().expect("stop"),
        "there was one to stop"
    );
    assert!(!backend.heartbeat_running().expect("asked"));
    assert!(!backend.stop_heartbeat().expect("stop"), "stopped twice");

    for pane in [reporting, quiet] {
        backend.kill(&pane).expect("kill");
    }
}

/// Shutdown is final: a worker still holding the registry when the process
/// quits must not open a connection `shutdown_all` just closed.
pub fn shutdown_is_final(backend: &dyn SessionBackend) {
    backend.ensure_ready().expect("the backend readies");
    backend.shutdown();
    assert!(
        backend.ensure_ready().is_err(),
        "a backend readied itself again after shutdown"
    );
    backend.shutdown();
}
