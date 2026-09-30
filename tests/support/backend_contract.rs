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
use thurbox::backend::{SessionBackend, WindowRole};

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
