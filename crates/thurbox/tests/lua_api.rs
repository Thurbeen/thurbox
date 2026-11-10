//! The plugin API against its one declaration, `kernel::lua_api` (issue #1476).
//!
//! Two directions. The declaration files are what `lua_api` renders — the
//! generated region of `ui/lib/thurbox.d.lua`, the `thurbox.*` entries of
//! `thurbox.yml` and the whole `tests/fixtures/lua_std/reads.lua` — and a real
//! publish serves no key `lua_api` does not declare. Either one alone lets a
//! field drift: the first is satisfied by a schema missing the field, the second
//! by declaration files nobody regenerated.
//!
//! `THURBOX_REGENERATE=1` (what `just lua-api` sets) rewrites the files instead
//! of comparing them.
//!
//! `meta` drives the first field added under the versioned API, `session.meta`,
//! from the real `thurbox-cli` to the bundled session list.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use thurbox::kernel::host::{LuaHost, Published, RenderContext};
use thurbox::kernel::lua_api::{self, Class, Ty, API, API_ROOT};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{AutomationRow, GitState, HostRow, RunRow, SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::session::SessionState;

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

/// Compare `path` with `expected`, or write it when regenerating.
fn holds(path: &Path, expected: &str) -> Result<(), String> {
    if std::env::var_os("THURBOX_REGENERATE").is_some() {
        std::fs::write(path, expected).expect("write a generated file");
        return Ok(());
    }
    // Line ends aside: a Windows checkout may carry `\r\n` (see `splice`).
    let actual = std::fs::read_to_string(path)
        .expect("read a generated file")
        .replace("\r\n", "\n");
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{} is not what src/kernel/lua_api.rs renders; run `just lua-api`",
            path.display()
        ))
    }
}

fn spliced(path: &Path, comment: &str, body: &str) -> String {
    let text = std::fs::read_to_string(path).expect("read");
    lua_api::splice(&text, comment, body)
        .unwrap_or_else(|| panic!("{} lost its generated-region markers", path.display()))
}

#[test]
fn the_declaration_files_are_what_the_schema_renders() {
    let types = root().join("ui/lib/thurbox.d.lua");
    let selene = root().join("thurbox.yml");
    let probe = root().join("crates/thurbox/tests/fixtures/lua_std/reads.lua");
    let failures: Vec<String> = [
        holds(&types, &spliced(&types, "-- ", &lua_api::declarations())),
        holds(
            &selene,
            &spliced(&selene, "  # ", &lua_api::selene_globals()),
        ),
        holds(&probe, &lua_api::std_probe()),
    ]
    .into_iter()
    .filter_map(Result::err)
    .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn granted_declares_exactly_the_capabilities() {
    let declared: Vec<&str> = lua_api::GRANTED.fields.iter().map(|f| f.name).collect();
    let capabilities: Vec<&str> = thurbox::kernel::host::Capability::ALL
        .iter()
        .map(|capability| capability.as_str())
        .collect();
    assert_eq!(declared, capabilities);
}

// ── The walk ──────────────────────────────────────────────────────────────

/// A class as a Lua table literal the probe below walks: each field names its
/// kind and, below a record, list or map, the fields of its rows.
fn shape(class: &Class, without: Option<&str>) -> String {
    let fields: Vec<String> = class
        .fields
        .iter()
        .filter(|field| Some(field.name) != without)
        .map(|field| {
            let (kind, inner) = match &field.ty {
                Ty::Leaf(_) => ("leaf", None),
                Ty::Record(inner) => ("record", Some(*inner)),
                Ty::List(inner) => ("list", Some(*inner)),
                Ty::Map(inner) | Ty::OwnKeys(inner) => ("map", Some(*inner)),
                Ty::MapOfLists(inner) => ("maplist", Some(*inner)),
                Ty::Roles => ("roles", None),
            };
            let fields = inner
                .map(|inner| format!(", fields = {}", shape(inner, None)))
                .unwrap_or_default();
            format!("[\"{}\"] = {{ kind = \"{kind}\"{fields} }}", field.name)
        })
        .collect();
    format!("{{ {} }}", fields.join(", "))
}

/// A pane that walks every key of `thurbox`, and of every row beneath it,
/// against `api` and renders `declared` or the first key that is not.
fn walker(api: &str) -> String {
    let roles: Vec<String> = lua_api::roles()
        .iter()
        .map(|role| format!("[\"{role}\"] = true"))
        .collect();
    format!(
        r#"
        local api = {api}
        local roles = {{ {roles} }}
        local walk
        local function check(value, fields, path)
          if type(value) ~= "table" then return end
          for key, item in pairs(value) do
            local field = fields[key]
            if not field then
              error(path .. "." .. tostring(key) .. " is not declared", 0)
            end
            walk(item, field, path .. "." .. key)
          end
        end
        walk = function(value, field, path)
          if field.kind == "record" then
            check(value, field.fields, path)
          elseif field.kind == "list" then
            for index, row in pairs(value) do
              assert(math.type(index) == "integer", path .. " is not a list")
              check(row, field.fields, path .. "[]")
            end
          elseif field.kind == "map" then
            for _, row in pairs(value) do check(row, field.fields, path .. "[]") end
          elseif field.kind == "maplist" then
            for _, list in pairs(value) do
              for _, row in pairs(list) do check(row, field.fields, path .. "[][]") end
            end
          elseif field.kind == "roles" then
            for role in pairs(value) do
              if not roles[role] then error(path .. "." .. role .. " is not declared", 0) end
            end
          end
        end
        return {{
          name = "walk", slot = "a", capabilities = {{ "run" }},
          render = function()
            local ok, reason = pcall(check, thurbox, api, "thurbox")
            return {{ text = ok and "declared" or reason }}
          end,
        }}
        "#,
        roles = roles.join(", "),
    )
}

fn row(name: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.to_string(),
        agent: "claude".to_string(),
        status: SessionState::from_hook_state("working").expect("a hook state"),
        cwd: Some(PathBuf::from("/repo")),
        repo: Some("thurbox".to_string()),
        repos: vec!["thurbox".to_string()],
        branch: Some(format!("feat/{name}")),
        base_branch: Some("main".to_string()),
        backend: "local:tmux".to_string(),
        backend_id: Some("%1".to_string()),
        agent_session_id: None,
        remote_host: Some("box".to_string()),
        parent_id: Some("parent".to_string()),
        display_order: Some(3),
        worktree_count: 1,
        git: Some(GitState {
            files_changed: 1,
            insertions: 2,
            deletions: 3,
            untracked: 0,
            dirty: true,
            ahead: 1,
            behind: 0,
            merged: Some(false),
        }),
        stopped: false,
        hook_state: None,
        key: Some("fleet.lead".to_string()),
        reports_as: Some("claude".to_string()),
        detected_agent: Some("codex".to_string()),
        shell_backend_id: None,
        member_dirs: Vec::new(),
        meta: BTreeMap::from([("ui.note".to_string(), "waiting for CI".to_string())]),
    }
}

/// A snapshot with every optional field of every row set, so a key the
/// declaration lacks is one the walk meets.
fn full() -> Snapshot {
    Snapshot {
        sessions: vec![row("lead")],
        automations: vec![AutomationRow {
            id: 1,
            name: "nightly".into(),
            schedule: "0 3 * * *".into(),
            action: "spawn".into(),
            enabled: true,
            last_outcome: Some("success".into()),
            last_detail: Some("ok".into()),
            runs: vec![RunRow {
                id: 1,
                started_at: 1,
                status: "success".into(),
                detail: "ok".into(),
                session: Some("lead".into()),
            }],
            timezone: Some("UTC".into()),
            session: Some("lead".into()),
            repo: Some("/repo".into()),
            branch: Some("b".into()),
            base: Some("main".into()),
            agent: Some("claude".into()),
            command: Some("true".into()),
            last_run_at: Some(1),
            next_run_at: Some(2),
            ..AutomationRow::default()
        }],
        repos: vec![thurbox::kernel::snapshot::RepoRow {
            path: "/repo".into(),
            name: "thurbox".into(),
        }],
        agents: Some(vec![thurbox::kernel::snapshot::AgentRow {
            name: "claude".into(),
        }]),
        hosts: vec![HostRow {
            name: "box".into(),
            detail: "ssh".into(),
            backend: "ssh:box".into(),
            platform: "posix".into(),
            multiplexer: Some("tmux".into()),
            available_multiplexers: vec!["tmux".into()],
            probing: false,
        }],
        taken_at_ms: 1_700_000_000_000,
        error: Some("a failed refresh".into()),
    }
}

fn host_with(source: &str) -> (tempfile::TempDir, LuaHost) {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(plugins.join("10_probe.lua"), source).expect("write");
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    (dir, host)
}

fn publish(host: &LuaHost, snapshot: &Snapshot) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let session = snapshot.sessions[0].id.clone();
    let mut diffs = thurbox::kernel::diff::DiffStore::new();
    diffs.set_for_test(
        &session,
        thurbox::kernel::diff::Diff::Ready {
            files: vec![thurbox::kernel::diff::FileSummary {
                path: "b.rs".into(),
                added: 1,
                removed: 2,
                status: "R",
                old_path: Some("a.rs".into()),
            }],
            body: vec!["+x".into()],
            truncated: true,
            raw_bytes: 3,
            untracked_omitted: 1,
        },
    );
    // Every creation-flow read served at once, `branches` as a failure so the
    // `error` field is published too.
    let mut repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    repos.set_bookmarks_for_test(
        "",
        vec![thurbox::kernel::repos::BookmarkRow {
            path: "/src/a".into(),
            name: "a".into(),
            parent: Some("/src".into()),
            is_parent: false,
            is_git: Some(true),
            label: Some("A".into()),
            offered: true,
        }],
    );
    repos.set_listing_for_test(
        "",
        "/src",
        thurbox::kernel::repos::Listing::Ready(vec![thurbox::kernel::repos::BrowseEntry {
            name: "a".into(),
            is_git: true,
        }]),
    );
    let wants = thurbox::kernel::repos::Wants {
        bookmarks: Some(String::new()),
        browse: Some((String::new(), "/src".into())),
    };
    let search = thurbox::kernel::search::Answer {
        request: thurbox::kernel::search::Request {
            query: "x".into(),
            sessions: Some(vec![session.clone()]),
        },
        hits: vec![thurbox::kernel::search::Hit {
            session: session.clone(),
            shell: false,
            text: "x".into(),
            ranges: vec![(0, 1)],
            back: 1,
            scroll: 0,
            row: 2,
            exact: true,
            score: 3,
        }],
        total: 1,
        error: Some("bad regex".into()),
        ..Default::default()
    };
    let inventory = [thurbox::kernel::inventory::Row {
        path: "plugins/10_x.lua".into(),
        name: "x".into(),
        kind: thurbox::kernel::inventory::Kind::Pane,
        slot: "a".into(),
        source: thurbox::kernel::bundled::Source::User,
        state: thurbox::kernel::inventory::State::Failed,
        error: Some("10_x: broken".into()),
        capabilities: Vec::new(),
        trust: thurbox::kernel::inventory::Trust::NotAsked,
    }];
    let links: HashMap<String, Vec<(String, usize, usize)>> = HashMap::from([(
        "lead#shell".to_string(),
        vec![("https://x".to_string(), 1, 2)],
    )]);
    let inflight = [thurbox::kernel::command::InFlight {
        id: 1,
        kind: "create",
        session: "lead".into(),
        subject: Some("thurbox".into()),
        host: Some("box".into()),
        phase: thurbox::kernel::command::Phase::Failed,
        error: Some("no".into()),
    }];
    let attach_errors = HashMap::from([(snapshot.sessions[0].id.clone(), "gone".to_string())]);
    let printing: HashSet<String> = HashSet::from([snapshot.sessions[0].id.clone()]);
    let hovered = thurbox::kernel::node::Identity {
        id: Some("row".into()),
        classes: Vec::new(),
        role: Some("session".into()),
    };
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot,
        attach_errors: &attach_errors,
        inflight: &inflight,
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &links,
        search: Some(&search),
        meta: &Default::default(),
        metrics: &Default::default(),
        status_rows: 0,
        can_open: true,
        inventory: &inventory,
        ui_dir: "ui",
        settings: &Default::default(),
        repos: &repos,
        wants: &wants,
        focus: Some("walk"),
        selection: Some("text"),
        hovered: Some(&hovered),
        printing: &printing,
    })
    .expect("publish");
}

fn render_text(host: &LuaHost) -> String {
    let index = host.index_of("walk").expect("the probe loaded");
    let ctx = RenderContext {
        width: 200,
        height: 1,
        focused: true,
        elapsed: 1.0,
        frame: 1,
    };
    let node = host.render(index, ctx).expect("render").node;
    match &*node {
        thurbox::kernel::node::Node::Text { lines, .. } => lines
            .iter()
            .map(|line| line.iter().map(|run| run.text.as_str()).collect::<String>())
            .collect(),
        other => panic!("the probe renders text, got {other:?}"),
    }
}

#[test]
fn every_key_a_publish_serves_is_declared() {
    let (_dir, host) = host_with(&walker(&shape(&API_ROOT, None)));
    publish(&host, &full());
    assert_eq!(render_text(&host), "declared");
}

#[test]
fn the_walk_names_a_published_key_left_undeclared() {
    // What the walk exists to catch, proven on a field every publish serves.
    let api = shape(&API_ROOT, None).replacen(
        "[\"sessions\"] = { kind = \"list\", fields = { [\"id\"] = { kind = \"leaf\" }, ",
        "[\"sessions\"] = { kind = \"list\", fields = { ",
        1,
    );
    assert_ne!(api, shape(&API_ROOT, None), "the declaration was removed");
    let (_dir, host) = host_with(&walker(&api));
    publish(&host, &full());
    assert_eq!(render_text(&host), "thurbox.sessions[].id is not declared");

    let (_dir, host) = host_with(&walker(&shape(&API_ROOT, Some("api"))));
    publish(&host, &full());
    assert_eq!(render_text(&host), "thurbox.api is not declared");

    // An optional field two levels down, on a row of a list.
    let api = shape(&API_ROOT, None).replacen(", [\"merged\"] = { kind = \"leaf\" }", "", 1);
    assert_ne!(api, shape(&API_ROOT, None), "the declaration was removed");
    let (_dir, host) = host_with(&walker(&api));
    publish(&host, &full());
    assert_eq!(
        render_text(&host),
        "thurbox.sessions[].git.merged is not declared"
    );
}

// ── The contract number ───────────────────────────────────────────────────

#[test]
fn thurbox_api_is_the_contract_number() {
    let (_dir, host) = host_with(
        r#"return {
          name = "walk", slot = "a",
          render = function() return { text = math.type(thurbox.api) .. " " .. thurbox.api } end,
        }"#,
    );
    publish(&host, &full());
    assert_eq!(render_text(&host), format!("integer {API}"));
}

#[test]
fn a_plugin_needing_a_newer_api_is_refused_and_says_why() {
    // A load is all-or-nothing, as for any pane that does not load: the
    // refusal is the load error, and it starts with the file stem, which is
    // what `inventory::rows` pins a failure on.
    let future = API + 1;
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_future.lua"),
        format!(
            r#"return {{
              name = "future", slot = "a", requires_api = {future},
              render = function() error("a refused pane never renders") end,
            }}"#
        ),
    )
    .expect("write");
    let host = LuaHost::new(dir.path());
    assert!(host.index_of("future").is_none(), "refused at load");
    let error = host.error.clone().expect("the refusal is reported");
    assert!(
        error.starts_with(&format!(
            "10_future: requires_api = {future}, but this thurbox serves plugin API {API}"
        )),
        "{error}"
    );

    // The contract it was written against, or an older one, loads.
    let (_dir, host) = host_with(&format!(
        r#"return {{
          name = "walk", slot = "a", requires_api = {API},
          render = function() return {{ text = "current" }} end,
        }}"#
    ));
    publish(&host, &full());
    assert_eq!(render_text(&host), "current");
}
