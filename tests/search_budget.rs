//! What a frame of the open search strip may cost the render thread.
//!
//! The strip is not `pure`, so it renders on every frame it is open, and while
//! agents print that is every frame there is. Two things made each of those
//! frames expensive, and each is pinned here:
//!
//! * it re-matched every session and rebuilt a row per text hit: ~2.4ms a
//!   frame with twenty sessions. A frame whose inputs did not move now reuses
//!   the last answer;
//! * re-stating its own state (a table) counted as a change on most frames, so
//!   every pure pane's cached tree was dropped with it and the whole interface
//!   re-rendered for as long as search was open.
//!
//! Asserted on work done rather than on a clock (ADR-P5): calls into a counting
//! `lib.fuzzy`, and renders the kernel served from cache. Instruction counts
//! would not do — matching is C string functions and table allocation, one VM
//! instruction apiece.

use thurbox::kernel::host::{Epoch, KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::search::{Answer, Hit, Request, MAX_HITS};
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::session::SessionState;

const PLUGIN: &str = "search";
const SESSIONS: usize = 24;

/// Where the counting `lib.fuzzy` leaves its tally.
const CALLS: &str = "test.fuzzy_calls";

/// `lib.fuzzy`, with every function counting its calls into `store`.
const COUNTING_FUZZY: &str = r#"
local real = require("lib.fuzzy_real")
local calls = 0
local counted = {}
for name, value in pairs(real) do
  if type(value) == "function" then
    counted[name] = function(...)
      calls = calls + 1
      store["test.fuzzy_calls"] = tostring(calls)
      return value(...)
    end
  else
    counted[name] = value
  end
end
return counted
"#;

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("entry");
        let path = entry.path();
        if path.is_dir() {
            copy_dir(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).expect("copy");
        }
    }
}

/// The real interface, with `lib.fuzzy` counting.
fn counting_interface() -> (tempfile::TempDir, LuaHost) {
    let dir = tempfile::tempdir().expect("tempdir");
    copy_dir(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"),
        dir.path(),
    );
    let lib = dir.path().join("lib");
    std::fs::rename(lib.join("fuzzy.lua"), lib.join("fuzzy_real.lua")).expect("rename");
    std::fs::write(lib.join("fuzzy.lua"), COUNTING_FUZZY).expect("write");
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    (dir, host)
}

fn calls(host: &LuaHost) -> usize {
    host.shared_string(CALLS)
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn row(n: usize) -> SessionRow {
    SessionRow {
        id: format!("id-{n}"),
        name: format!("worker-{n}-feature-branch"),
        agent: "claude".into(),
        status: SessionState::Idle,
        cwd: Some(std::path::PathBuf::from("/src/thurbox")),
        repo: Some("thurbox".into()),
        repos: vec!["thurbox".into()],
        branch: Some(format!("feat/thing-{n}")),
        base_branch: None,
        backend: "local-tmux".into(),
        backend_id: Some(format!("%{n}")),
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

/// A full answer: as many hits as the kernel ever publishes.
fn answer(query: &str) -> Answer {
    Answer {
        request: Request {
            query: query.into(),
            sessions: None,
        },
        hits: (0..MAX_HITS)
            .map(|n| Hit {
                session: format!("id-{}", n % SESSIONS),
                shell: false,
                text: format!("error: compile failed in src/main.rs at line {n}"),
                ranges: vec![(0, 5)],
                back: n,
                scroll: n,
                row: 3,
                exact: true,
                score: 130,
            })
            .collect(),
        total: 5_000,
        sessions: SESSIONS,
        lines: 240_000,
        // As a store hands it out: what the published table is gated on.
        serial: 1,
        ..Answer::default()
    }
}

fn publish(host: &LuaHost, epoch: Epoch, snapshot: &Snapshot, search: &Answer) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch,
        snapshot,
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &Default::default(),
        search: Some(search),
        meta: &Default::default(),
        metrics: &Default::default(),
        status_rows: 0,
        can_open: true,
        inventory: &[],
        ui_dir: "ui",
        settings: &Default::default(),
        repos: &repos,
        wants: &Default::default(),
        focus: None,
        selection: None,
        hovered: None,
        printing: &Default::default(),
    })
    .expect("publish");
}

fn press(host: &LuaHost, chord: &str) {
    let index = host.index_of(PLUGIN).expect("no search plugin");
    let mut key = KeyPress {
        name: chord.to_string(),
        ..KeyPress::default()
    };
    if chord.chars().count() == 1 {
        key.ch = chord.chars().next();
    }
    if let Some(rest) = chord.strip_prefix("ctrl+") {
        key.ctrl = true;
        key.name = rest.to_string();
        key.ch = rest.chars().next().filter(|_| rest.chars().count() == 1);
    }
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    if let Some(binding) = registry.resolve(&key, Some(PLUGIN)) {
        let action = binding.action.clone();
        if host.on_action(index, &action).expect("on_action") {
            return;
        }
    }
    host.on_key(index, &key).expect("on_key");
}

fn render(host: &LuaHost) -> Result<(), String> {
    let index = host.index_of(PLUGIN).expect("no search plugin");
    host.render(
        index,
        RenderContext {
            width: 120,
            height: 16,
            focused: true,
            elapsed: 0.0,
            frame: 0,
        },
    )
    .map(|_| ())
    .map_err(|e| format!("{e:?}"))
}

#[test]
fn a_frame_that_changed_nothing_matches_nothing() {
    let (_dir, host) = counting_interface();
    let snapshot = Snapshot {
        sessions: (0..SESSIONS).map(row).collect(),
        ..Snapshot::default()
    };
    let found = answer("error");
    // One epoch, as the loop holds between changes: the kernel hands back the
    // same published tables, which is what lets the strip see nothing moved.
    let settled = Epoch::always_fresh();

    publish(&host, settled, &snapshot, &found);
    press(&host, "ctrl+/");
    for ch in "error".chars() {
        press(&host, &ch.to_string());
    }
    publish(&host, settled, &snapshot, &found);
    render(&host).expect("render");
    let after_keystroke = calls(&host);
    assert!(after_keystroke > 0, "the counting lib.fuzzy is not in use");

    for _ in 0..5 {
        publish(&host, settled, &snapshot, &found);
        render(&host).expect("render");
    }
    assert_eq!(
        calls(&host),
        after_keystroke,
        "a frame whose query, scope and published tables stood still matched again"
    );

    // Another worker landing moves the data epoch — links, diffs and metrics
    // do, several times a second under load. The answer did not change, so
    // neither does anything the strip matches.
    for data in 1..=5 {
        let other_worker = Epoch {
            data: settled.data + data,
            ..settled
        };
        publish(&host, other_worker, &snapshot, &found);
        render(&host).expect("render");
    }
    assert_eq!(
        calls(&host),
        after_keystroke,
        "another worker's result re-matched a search whose answer had not changed"
    );

    // The control: moved inputs are matched afresh, or the count above could
    // stand still for a reason that has nothing to do with the memo.
    publish(&host, Epoch::always_fresh(), &snapshot, &found);
    render(&host).expect("render");
    assert!(
        calls(&host) > after_keystroke,
        "a moved input was not matched"
    );
}

#[test]
fn an_open_strip_that_changed_nothing_leaves_every_pure_pane_cached() {
    // The strip re-states its own state on every frame — the query field, a
    // table — and the kernel compares a write with what is held so an unmoved
    // value is no change. A table used to compare in whatever order `pairs`
    // happened to walk it, which differs between two copies of the same table,
    // so the field "moved" on most frames and dropped every pure pane's cached
    // tree with it: the session list re-rendered on every frame search was open.
    //
    // Lua seeds its string hash per VM, so which order `pairs` walks a table in
    // is decided when the VM is made; several are tried, or the one this run
    // happened to draw could hide the bug.
    for _ in 0..32 {
        settled_strip_keeps_the_list_cached();
    }
}

fn settled_strip_keeps_the_list_cached() {
    let host = LuaHost::new(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
    assert!(host.error.is_none(), "{:?}", host.error);
    let snapshot = Snapshot {
        sessions: (0..SESSIONS).map(row).collect(),
        ..Snapshot::default()
    };
    let found = answer("error");
    let settled = Epoch::always_fresh();
    let sessions = host.index_of("sessions").expect("no sessions pane");
    let list = |host: &LuaHost| {
        host.render(
            sessions,
            RenderContext {
                width: 40,
                height: 30,
                focused: false,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render sessions");
    };

    publish(&host, settled, &snapshot, &found);
    press(&host, "ctrl+/");
    for ch in "error".chars() {
        press(&host, &ch.to_string());
    }
    // Let the first frames after the keystrokes settle what they write.
    for _ in 0..3 {
        publish(&host, settled, &snapshot, &found);
        render(&host).expect("render");
        list(&host);
    }

    let before = host.skipped_renders();
    for _ in 0..20 {
        publish(&host, settled, &snapshot, &found);
        render(&host).expect("render");
        list(&host);
    }
    assert_eq!(
        host.skipped_renders() - before,
        20,
        "the session list was re-rendered while nothing it reads changed"
    );
}
