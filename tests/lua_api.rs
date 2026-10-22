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
use thurbox::kernel::snapshot::{
    AutomationRow, DeletedRow, GitState, HostRow, RunRow, SessionRow, Snapshot, TaskRow,
};
use thurbox::kernel::theme::Themes;
use thurbox::session::SessionState;

#[path = "support/tmux_server.rs"]
mod tmux_server;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Compare `path` with `expected`, or write it when regenerating.
fn holds(path: &Path, expected: &str) -> Result<(), String> {
    if std::env::var_os("THURBOX_REGENERATE").is_some() {
        std::fs::write(path, expected).expect("write a generated file");
        return Ok(());
    }
    let actual = std::fs::read_to_string(path).expect("read a generated file");
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
    let probe = root().join("tests/fixtures/lua_std/reads.lua");
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
        backend: "local-tmux".to_string(),
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
        deleted: vec![DeletedRow {
            id: "gone".into(),
            name: "gone".into(),
            agent: "claude".into(),
            deleted_at: 1,
            worktrees: 1,
            partial: true,
            restore_refusal: Some("borrowed directory removed".into()),
        }],
        tasks: vec![TaskRow {
            id: 1,
            title: "t".into(),
            description: Some("d".into()),
            status: "todo".into(),
            source: "local".into(),
            external_url: Some("https://example.com".into()),
            created_at: 1,
            updated_at: 2,
        }],
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
        ..Snapshot::default()
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
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
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

// ── `session.meta`, from the CLI to a pane (issue #1523) ──────────────────

/// `ui.*` session meta, from `thurbox-cli session meta` to the bundled session
/// list: what a driver writes reaches the row on the next read of the
/// database, and nothing outside the prefix ever reaches a plugin.
mod meta {
    use std::path::PathBuf;
    use std::process::{Command, Output};

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use thurbox::kernel::host::{LuaHost, Published, RenderContext};
    use thurbox::kernel::paint::{render, PlaceholderSurfaces};
    use thurbox::kernel::registry::Registry;
    use thurbox::kernel::snapshot::{Snapshot, SnapshotStore};
    use thurbox::kernel::theme::Themes;
    use thurbox::session::SessionId;
    use thurbox::sync::SharedSession;

    use super::tmux_server::TmuxServer;

    /// A throwaway instance: its own config, data, home and multiplexer socket.
    struct Env {
        root: tempfile::TempDir,
        server: TmuxServer,
    }

    impl Env {
        fn new() -> Self {
            let root = tempfile::TempDir::new().expect("tempdir");
            for sub in ["home", "config", "data"] {
                std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
            }
            Self {
                root,
                server: TmuxServer::private("thurbox-session-meta-test"),
            }
        }

        fn path(&self, sub: &str) -> PathBuf {
            self.root.path().join(sub)
        }

        fn db(&self) -> thurbox::storage::Database {
            thurbox::storage::Database::open(&self.path("data").join("thurbox.db")).expect("open")
        }

        fn cli(&self, args: &[&str]) -> Output {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
            cmd.args(args);
            cmd.env("HOME", self.path("home"));
            cmd.env("USERPROFILE", self.path("home"));
            cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
            cmd.env("THURBOX_DATA_DIR", self.path("data"));
            self.server.scope(&mut cmd);
            cmd.env_remove("THURBOX_SESSION");
            cmd.env_remove("THURBOX_SESSION_ID");
            cmd.output().expect("run thurbox-cli")
        }

        fn seed(&self, name: &str, cwd: &str) -> SessionId {
            let row = SharedSession {
                id: SessionId::default(),
                name: name.into(),
                agent: "shell".into(),
                backend_id: String::new(),
                backend_type: "local-tmux".into(),
                agent_session_id: None,
                cwd: Some(PathBuf::from(cwd)),
                additional_dirs: Vec::new(),
                worktrees: Vec::new(),
                shell_backend_id: None,
                parent_session_id: None,
                display_order: None,
                tombstone: false,
                tombstone_at: None,
                key: None,
            };
            self.db().upsert_session(&row).expect("persist");
            row.id
        }
    }

    fn ok(out: &Output) {
        assert!(
            out.status.success(),
            "thurbox-cli failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn bundled() -> LuaHost {
        let host = LuaHost::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
        assert!(host.error.is_none(), "{:?}", host.error);
        host
    }

    fn publish(host: &LuaHost, snapshot: &Snapshot) {
        let themes = Themes::load(None);
        let mut registry = Registry::default();
        let (bindings, settings) = host.declarations();
        registry.declare(bindings, settings);
        let diffs = thurbox::kernel::diff::DiffStore::new();
        let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
        host.publish(&Published {
            epoch: thurbox::kernel::host::Epoch::always_fresh(),
            snapshot,
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
    }

    /// One pane of `host`, painted.
    fn screen(host: &LuaHost, pane: &str, width: u16, height: u16) -> String {
        let index = host.index_of(pane).expect("the pane loaded");
        let ctx = RenderContext {
            width,
            height,
            focused: true,
            elapsed: 1.0,
            frame: 1,
        };
        let node = host.render(index, ctx).expect("render").node;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| render(frame, frame.area(), &node, &PlaceholderSurfaces))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A pane printing what a plugin reads off each row's `meta`, by session name.
    const PROBE: &str = r#"
    return {
      name = "probe", slot = "a",
      render = function()
        local out = {}
        for _, row in ipairs(thurbox.sessions) do
          local pairs_seen = {}
          for key, value in pairs(row.meta or {}) do
            pairs_seen[#pairs_seen + 1] = key .. "=" .. value
          end
          table.sort(pairs_seen)
          out[#out + 1] = row.name .. ":" .. table.concat(pairs_seen, ",")
        end
        table.sort(out)
        return { text = table.concat(out, " ") }
      end,
    }
    "#;

    /// The interface's own store over the instance database: one store for the
    /// whole test, so a later read is the change gate noticing a CLI write, as a
    /// running interface would, not a fresh store reading everything.
    fn store(env: &Env) -> SnapshotStore {
        let mut store =
            SnapshotStore::with_database(env.db(), &thurbox::backend::wiring::configured().0);
        store.refresh();
        store
    }

    /// The rows after the next read of the database.
    fn reread(store: &mut SnapshotStore) -> Snapshot {
        store.refresh();
        store.current().clone()
    }

    #[test]
    fn a_ui_note_and_group_set_from_the_cli_reach_the_row_and_nothing_else_does() {
        let env = Env::new();
        env.seed("worker", "/src/alpha");
        env.seed("other", "/src/alpha");
        let probe_dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(probe_dir.path().join("plugins")).expect("mkdir");
        std::fs::write(probe_dir.path().join("plugins/10_probe.lua"), PROBE).expect("write");
        let probe = LuaHost::new(probe_dir.path());
        assert!(probe.error.is_none(), "{:?}", probe.error);
        let list = bundled();
        let mut rows_store = store(&env);

        let refused = env.cli(&[
            "session",
            "meta",
            "set",
            "worker",
            "thurbox.codex_conversation_id",
            "forged",
        ]);
        assert_eq!(
            refused.status.code(),
            Some(thurbox::cli::EXIT_USAGE),
            "thurbox's own keys are not the CLI's to set"
        );
        ok(&env.cli(&[
            "session",
            "meta",
            "set",
            "worker",
            "ui.note",
            "waiting\nfor CI",
        ]));
        ok(&env.cli(&["session", "meta", "set", "worker", "ui.group", "release"]));
        ok(&env.cli(&[
            "session",
            "meta",
            "set",
            "worker",
            "fm.lease",
            "driver-only",
        ]));

        let rows = reread(&mut rows_store);
        publish(&probe, &rows);
        assert_eq!(
            screen(&probe, "probe", 80, 1).trim_end(),
            "other: worker:ui.group=release,ui.note=waitingfor CI",
            "only `ui.*` keys, each one line"
        );
        publish(&list, &rows);
        let drawn = screen(&list, "sessions", 60, 12);
        assert!(drawn.contains("waitingfor CI"), "{drawn}");
        // Grouped under its label, apart from the repo group the other row keeps.
        assert!(drawn.contains("release"), "{drawn}");
        assert!(drawn.contains("alpha"), "{drawn}");
        assert!(!drawn.contains("driver-only"), "{drawn}");

        ok(&env.cli(&["session", "meta", "unset", "worker", "ui.note"]));
        ok(&env.cli(&["session", "meta", "unset", "worker", "ui.group"]));
        let rows = reread(&mut rows_store);
        publish(&probe, &rows);
        assert_eq!(screen(&probe, "probe", 80, 1).trim_end(), "other: worker:");
        publish(&list, &rows);
        let drawn = screen(&list, "sessions", 60, 12);
        assert!(!drawn.contains("for CI"), "{drawn}");
        assert!(!drawn.contains("release"), "{drawn}");
    }
}
