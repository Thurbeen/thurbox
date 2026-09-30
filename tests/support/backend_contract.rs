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
use thurbox::backend::{Owner, Placed, SessionBackend, WindowRole, WindowSpec};

/// Three rows, as their stamps. Uuid-shaped, as a real row's are.
const OWNER_A: &str = "00000000-0000-4000-8000-00000000000a";
const OWNER_B: &str = "00000000-0000-4000-8000-00000000000b";
const OWNER_C: &str = "00000000-0000-4000-8000-00000000000c";

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
