//! An edited layout keeps working across an upgrade.
//!
//! Delivery never overwrites a file the user edited (`bundled::decide` →
//! *Preserve*), but it keeps updating everything that file depends on: the
//! kernel, and every untouched file under `lib/`. So an edited `layout.lua` or
//! pane is frozen while its dependencies move, and "intact" is not "working".
//! These tests hold the second half.
//!
//! `tests/fixtures/edited_interface/<release>/` is what such a user has on
//! disk: files as that release shipped them, each with an edit so delivery
//! treats it as the user's. `v2.22.4` is the arrangement, `10_sessions.lua` and
//! `20_agent.lua` of the release the `thurbox-files` fork is pinned to;
//! `v2.32.0` is the arrangement and the agent pane of the release that briefly
//! shipped layout presets, whose agent pane asks `panels.placed`. They are FROZEN on
//! purpose — never refresh them to follow a change in `ui/`, because a user's
//! preserved file does not follow it either. If one stops loading, the change
//! that broke it breaks every user with that edit; keep the old shape working in
//! `lib/` instead (the contract in `ui/AGENTS.md`).
//!
//! `lib_surface.txt` beside them is that contract as data: every name each
//! `lib/` module exports, and what kind of value it is.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command as Process;

use ratatui::layout::Rect;

use thurbox::kernel::bundled;
use thurbox::kernel::command::Command;
use thurbox::kernel::host::{LuaHost, Published, RenderContext};
use thurbox::kernel::layout::resolve;
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::session::SessionState;

/// Every release a user may have edited files from, each with what they edited.
const RELEASES: &[(&str, &[&str])] = &[
    (
        "v2.22.4",
        &[
            "layout.lua",
            "plugins/10_sessions.lua",
            "plugins/20_agent.lua",
        ],
    ),
    ("v2.32.0", &["layout.lua", "plugins/20_agent.lua"]),
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/edited_interface")
}

fn fixture(relative: &str) -> String {
    std::fs::read_to_string(fixtures().join(relative)).expect("fixture")
}

/// A delivered interface the user then edited, upgraded once more.
///
/// The second `materialize` is the upgrade: it is what runs on every start of a
/// new release, and it must hand the edits back untouched.
fn upgraded_edited_interface(release: &str, edited: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = bundled::materialize(dir.path());
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    for relative in edited {
        let frozen = fixture(&format!("{release}/{relative}"));
        std::fs::write(dir.path().join(relative), frozen).expect("edit");
    }

    let upgrade = bundled::materialize(dir.path());
    assert!(upgrade.errors.is_empty(), "{:?}", upgrade.errors);
    for relative in edited {
        assert!(
            upgrade.preserved.iter().any(|p| p == relative),
            "{release}: {relative} was not preserved: {upgrade:?}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(relative)).expect("read"),
            fixture(&format!("{release}/{relative}")),
            "{release}: the upgrade wrote into the edited {relative}"
        );
    }
    dir
}

fn row(name: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.into(),
        agent: "claude".into(),
        status: SessionState::Idle,
        cwd: None,
        repo: Some("thurbox".into()),
        repos: vec!["thurbox".into()],
        branch: Some(format!("feat/{name}")),
        base_branch: None,
        backend: "local-tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 1,
        git: None,
        stopped: false,
        hook_state: None,
        reports_as: None,
        detected_agent: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

fn publish(host: &LuaHost) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    let snapshot = Snapshot {
        sessions: vec![row("alpha"), row("beta")],
        ..Snapshot::default()
    };
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot: &snapshot,
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &Default::default(),
        content: &Default::default(),
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

fn ctx(width: u16, height: u16, focused: bool) -> RenderContext {
    RenderContext {
        width,
        height,
        focused,
        elapsed: 0.0,
        frame: 0,
    }
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.index_of(name)
        .unwrap_or_else(|| panic!("{name} did not load"))
}

fn rect_of(host: &LuaHost, width: u16, height: u16, slot: &str) -> Option<Rect> {
    let region = host
        .arrangement(width, height)
        .expect("the edited layout.lua arranges");
    resolve(&region, Rect::new(0, 0, width, height))
        .into_iter()
        .find(|placed| placed.slot == slot)
        .map(|placed| placed.rect)
}

// ── the edited files, against the current lib/ and kernel ───────────────────

#[test]
fn edited_layouts_and_panes_from_old_releases_still_arrange_and_render() {
    for (release, edited) in RELEASES {
        let dir = upgraded_edited_interface(release, edited);
        let host = LuaHost::new(dir.path());
        assert!(host.error.is_none(), "{release}: {:?}", host.error);
        publish(&host);

        // The edit is honoured: the list on the right, wider than it shipped.
        let sessions = rect_of(&host, 160, 48, "sessions").expect("the list is placed");
        let center = rect_of(&host, 160, 48, "center").expect("the agent is placed");
        assert!(sessions.x > center.x, "{release}: {sessions:?} {center:?}");
        assert_eq!(sessions.width, 48, "{release}: 30% of 160");
        assert!(rect_of(&host, 60, 48, "sessions").is_none());
        assert!(rect_of(&host, 60, 48, "center").is_some());

        for focused in [false, true] {
            // The list first: it is what selects a session, which is what gives
            // the agent pane something to draw.
            let list = host
                .render(index_of(&host, "sessions"), ctx(48, 40, focused))
                .unwrap_or_else(|e| panic!("{release}: the session list renders: {e:?}"));
            let drawn = format!("{:?}", list.node);
            assert!(drawn.contains("alpha"), "{release}: no rows: {drawn}");
            if edited.contains(&"plugins/10_sessions.lua") {
                assert!(
                    drawn.contains("My sessions"),
                    "{release}: not the edited pane"
                );
            }

            host.render(index_of(&host, "agent"), ctx(112, 40, focused))
                .unwrap_or_else(|e| panic!("{release}: the edited agent pane renders: {e:?}"));
        }
    }
}

#[test]
fn edited_interfaces_pass_plugin_check() {
    // What the user runs — and what `AGENTS.md` tells an agent to run — after an
    // upgrade. It loads the whole interface at two sizes the way thurbox does.
    for (release, edited) in RELEASES {
        let dir = upgraded_edited_interface(release, edited);
        let output = Process::new(env!("CARGO_BIN_EXE_thurbox-cli"))
            .args(["plugin", "check", "--json"])
            .env("THURBOX_UI_DIR", dir.path())
            .env("THURBOX_CONFIG_DIR", dir.path().join("config"))
            .env("THURBOX_DATA_DIR", dir.path().join("data"))
            .output()
            .expect("run thurbox-cli");
        assert!(
            output.status.success(),
            "{release}: plugin check failed on an edited interface:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

// ── lib/'s published surface ────────────────────────────────────────────────

/// Every `lib/` module that loads into the VM — `thurbox.d.lua` is types only.
fn lib_modules() -> Vec<String> {
    let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/lib");
    let mut names: Vec<String> = std::fs::read_dir(lib)
        .expect("ui/lib")
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            let module = name.strip_suffix(".lua")?;
            (!module.ends_with(".d")).then(|| module.to_string())
        })
        .collect();
    names.sort();
    names
}

fn pinned() -> BTreeSet<String> {
    fixture("lib_surface.txt")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// `module.name kind` for every name each `lib/` module exports, read from
/// inside the sandbox a pane runs in, off a delivered interface.
///
/// Two readings, because a module can answer a name it does not hold:
/// `theme.accent` and the other colour shorthands come from a metatable, so
/// `pairs` never sees them, yet they are what third-party panes use most. So
/// the table's own keys find what is new, and every pinned name is also read
/// the way a pane reads it — with a theme published, as it is on screen.
fn exported_surface(pinned: &BTreeSet<String>) -> BTreeSet<String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let report = bundled::materialize(dir.path());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let modules = lib_modules()
        .iter()
        .map(|module| format!("{module:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let names = pinned
        .iter()
        .filter_map(|line| line.split_once(' ')?.0.split_once('.'))
        .map(|(module, name)| format!("{{ {module:?}, {name:?} }}"))
        .collect::<Vec<_>>()
        .join(", ");
    let probe = format!(
        r#"local MODULES = {{ {modules} }}
local PINNED = {{ {names} }}
return {{
  name = "lib_surface",
  slot = "float",
  render = function()
    local lines = {{}}
    for _, name in ipairs(MODULES) do
      local module = require("lib." .. name)
      if type(module) == "table" then
        for key, value in pairs(module) do
          lines[#lines + 1] = name .. "." .. tostring(key) .. " " .. type(value)
        end
      else
        lines[#lines + 1] = name .. " " .. type(module)
      end
    end
    for _, pin in ipairs(PINNED) do
      local ok, module = pcall(require, "lib." .. pin[1])
      local value = ok and type(module) == "table" and module[pin[2]] or nil
      lines[#lines + 1] = pin[1] .. "." .. pin[2] .. " " .. type(value)
    end
    command("message", {{ text = table.concat(lines, "\n") }})
    return {{ type = "text", text = "" }}
  end,
}}
"#
    );
    std::fs::write(dir.path().join("plugins/99_lib_surface.lua"), probe).expect("probe");

    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    publish(&host);
    host.drain_commands();
    host.render(index_of(&host, "lib_surface"), ctx(10, 1, false))
        .expect("the probe renders");
    host.drain_commands()
        .into_iter()
        .find_map(|command| match command {
            Command::Message { text, .. } => Some(text),
            _ => None,
        })
        .expect("the probe reported")
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn lib_still_exports_every_name_it_ever_published() {
    let pinned = pinned();
    let exported = exported_surface(&pinned);

    let gone: Vec<&String> = pinned.difference(&exported).collect();
    assert!(
        gone.is_empty(),
        "lib/ no longer exports these as it did, so every edited or third-party pane \
         calling them breaks on upgrade. Keep the old name working (the contract in \
         ui/AGENTS.md) rather than editing the pin:\n{gone:#?}"
    );
    let new: Vec<&String> = exported.difference(&pinned).collect();
    assert!(
        new.is_empty(),
        "lib/ exports names the pin does not list. Adding one is fine — it becomes \
         part of the contract, so append it to \
         tests/fixtures/edited_interface/lib_surface.txt:\n{new:#?}"
    );
}

// ── the explicit writers of layout.lua ──────────────────────────────────────

#[test]
fn restoring_an_edited_layout_keeps_the_edit_as_a_backup() {
    // Restore is a user's act (settings → Interface → `r`), but a pane's
    // `command("plugin", …)` can ask for one too, so it may replace the edit
    // but never make it unrecoverable.
    let (release, edited) = RELEASES[0];
    let dir = upgraded_edited_interface(release, edited);
    let backup = bundled::restore(dir.path(), "layout.lua").expect("restore");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("layout.lua")).expect("layout.lua"),
        include_str!("../ui/layout.lua")
    );
    assert_eq!(backup, Some(dir.path().join("layout.lua.bak")));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("layout.lua.bak"))
            .expect("the edited layout is backed up"),
        fixture(&format!("{release}/layout.lua"))
    );

    // Restoring the untouched file again has nothing to keep.
    assert_eq!(
        bundled::restore(dir.path(), "layout.lua").expect("restore"),
        None
    );
    assert!(!dir.path().join("layout.lua.bak.2").exists());
}
