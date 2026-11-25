#![allow(dead_code)]

use std::collections::HashMap;

use runtime_mux::identity::{Located, WindowIndex};
use runtime_mux::{Key, Owner, Placed, SessionBackend, WindowRole, WindowSpec};

const OWNER_A: &str = "00000000-0000-4000-8000-00000000000a";
const OWNER_B: &str = "00000000-0000-4000-8000-00000000000b";
const OWNER_C: &str = "00000000-0000-4000-8000-00000000000c";
const OWNER_D: &str = "00000000-0000-4000-8000-00000000000d";

pub fn suite(backend: &dyn SessionBackend) {
    backend.ensure_ready().expect("the backend readies");
    let env = HashMap::new();
    let spawn = |name: &str| {
        backend
            .spawn(name, "sleep", &["300".to_string()], None, &env, 24, 80)
            .unwrap_or_else(|e| panic!("spawn {name}: {e:#}"))
            .backend_id
    };

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

    let pane: String = backend
        .create_window(&spec(owner))
        .expect("create_window")
        .into();
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

    let stranger = Owner::new(OWNER_B, "headless");
    assert_eq!(
        backend.locate(stranger).expect("locate a stranger").agent,
        Located::Absent
    );

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

    let again: String = backend
        .create_window(&spec(renamed))
        .expect("create again")
        .into();
    assert_eq!(
        backend.locate(renamed).expect("locate").agent,
        Located::At(again.clone()),
        "the newer window keeps the identity"
    );

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
            .to_string()
    };
    let pane = open("cat", &[]);
    assert_eq!(
        backend.locate(owner).expect("locate").agent,
        Located::At(pane.clone())
    );

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

    backend.kill(&pane).expect("kill");
    assert_eq!(
        backend.pane_state(&pane).unwrap_or_default(),
        runtime_mux::PaneState::default(),
        "a gone pane's state is no answer"
    );
    assert!(
        backend.send_text(&pane, "nowhere", true).is_err(),
        "a send to a gone pane reported success"
    );
}

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
            .to_string()
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

    for pane in [reporting, quiet] {
        backend.kill(&pane).expect("kill");
    }
}

pub fn shutdown_is_final(backend: &dyn SessionBackend) {
    backend.ensure_ready().expect("the backend readies");
    backend.shutdown();
    assert!(
        backend.ensure_ready().is_err(),
        "a backend readied itself again after shutdown"
    );
    backend.shutdown();
}
