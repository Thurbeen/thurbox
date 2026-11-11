//! `lib.repo_picker`'s calls an edited pane may still make in their older
//! forms: a call that worked keeps working (`ui/AGENTS.md`).

use thurbox::kernel::host::{LuaHost, Published, RenderContext};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::Snapshot;
use thurbox::kernel::theme::Themes;

/// What a probe pane in a copy of the bundled interface stores after running
/// `lua` at load, which sets `store["probe.result"]`.
fn probe(lua: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let report = thurbox::kernel::bundled::materialize(dir.path());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    std::fs::write(
        dir.path().join("plugins").join("95_probe.lua"),
        format!(
            r#"local repo_picker = require("lib.repo_picker")
{lua}
return {{
  name = "probe",
  slot = "center",
  render = function() return {{ type = "text", text = "" }} end,
}}
"#
        ),
    )
    .expect("write the probe");
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::new();
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot: &Snapshot::default(),
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &Default::default(),
        search: None,
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
    let index = host.index_of("probe").expect("no probe");
    host.render(
        index,
        RenderContext {
            width: 20,
            height: 3,
            focused: true,
            elapsed: 0.0,
            frame: 0,
        },
    )
    .expect("render");
    host.shared_string("probe.result")
        .expect("the probe stores its result")
}

/// `chosen(rows)` and `chosen(rows, selected)` were the whole call before
/// worktree mode took a third table: both still answer, with nothing taking
/// a worktree.
#[test]
fn chosen_takes_its_older_forms() {
    let shown = probe(
        r#"local rows = { { path = "/a" }, { path = "/b" } }
local ok1, w1, p1 = pcall(repo_picker.chosen, rows)
local ok2, w2, p2 = pcall(repo_picker.chosen, rows, { ["/b"] = true })
store["probe.result"] = tostring(ok1) .. " " .. (ok1 and (#w1 .. "/" .. #p1) or tostring(w1))
  .. " | " .. tostring(ok2) .. " " .. (ok2 and (#w2 .. "/" .. #p2) or tostring(w2))"#,
    );
    assert_eq!(shown, "true 0/0 | true 0/1");
}
