use std::collections::HashMap;

use mlua::{Lua, Table, Value};

use super::{LuaHost, Published};
use crate::kernel::command::InFlight;
use crate::kernel::diff::DiffStore;
use crate::kernel::metrics::Metrics;
use crate::kernel::registry::{Registry, Value as SettingValue};
use crate::kernel::snapshot::{HostRow, Snapshot};
use crate::kernel::theme::Themes;

/// Set one key on a Lua table, mapping the error to a String — the shape
/// `publish` otherwise repeats per field.
fn set(table: &Table, key: &str, value: impl mlua::IntoLua) -> Result<(), String> {
    table.raw_set(key, value).map_err(|e| e.to_string())
}

/// FNV-1a over a sequence of byte strings, for group keys whose input has no
/// version counter of its own (the interface inventory, a parameterised want).
///
/// The group cache compares keys exactly, so a collision here would hand back a
/// stale table — but these keys pair the hash with a real version (or hash
/// inputs a handful of short strings), which keeps the exposure to the same
/// order as the `always_fresh` counter wrapping.
fn fnv64<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.iter().chain(&[0xff]) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

impl LuaHost {
    pub fn publish(&self, world: &Published) -> Result<(), String> {
        let Published {
            epoch,
            hovered,
            focus,
            selection,
            snapshot,
            attach_errors,
            starts,
            inflight,
            themes,
            registry,
            diffs,
            links,
            search,
            meta,
            metrics,
            status_rows,
            can_open,
            inventory,
            ui_dir,
            settings,
            repos: repo_store,
            wants,
            printing,
        } = world;

        let table = self.lua.create_table().map_err(|e| e.to_string())?;
        // The largest group by a distance — a table per session with ~30 named
        // fields and two nested tables — and one whose inputs move rarely: the
        // snapshot's own generation, what each agent reported about itself, and
        // whether an attach failed.
        let sessions = self.group(
            "sessions",
            [epoch.snapshot, epoch.meta, epoch.failed, 0],
            || build_sessions(&self.lua, snapshot, attach_errors, starts, meta),
        )?;
        // A file read is rooted at a session's directory, so the roots follow
        // the snapshot rather than being asked for separately — and only when
        // it moved: rebuilding cloned every id and cwd per publish.
        if self.roots_snapshot.get() != Some(epoch.snapshot) {
            self.roots_snapshot.set(Some(epoch.snapshot));
            let mut roots = self.roots.borrow_mut();
            roots.clear();
            // Files are read with this machine's filesystem, so only a
            // directory on this machine is a root: a remote session's path
            // names a directory elsewhere, which a local one of the same
            // name is not.
            for row in &snapshot.sessions {
                match &row.cwd {
                    Some(cwd) if row.workspace_on_this_machine() => {
                        roots.insert(row.id.clone(), cwd.clone());
                    }
                    _ => {}
                }
            }
        }

        set(&table, "sessions", sessions)?;

        // Which sessions are producing output, as a set keyed by session id.
        //
        // A group of its own, and a deliberately tiny one: the answer moves
        // with an agent's output, so gating the `sessions` group on it would
        // rebuild a ~30-field table per session every time a pane started or
        // stopped printing. One id per printing session is the whole payload,
        // and it is the only thing that lets a pane animate `running` on
        // evidence rather than assert a turn it cannot see.
        let printing_value = self.group("printing", [epoch.printing, 0, 0, 0], || {
            let table = self
                .lua
                .create_table_with_capacity(0, printing.len())
                .map_err(|e| e.to_string())?;
            for session in printing.iter() {
                set(&table, session.as_str(), true)?;
            }
            Ok(Value::Table(table))
        })?;
        set(&table, "printing", printing_value)?;

        let repos = self.group("repos", [epoch.snapshot, 0, 0, 0], || {
            build_repos(&self.lua, snapshot)
        })?;
        set(&table, "repos", repos)?;

        // Built from the snapshot alone, so it changes when the database does
        // and not once a frame.
        let agents = self.group("agents", [epoch.snapshot, 0, 0, 0], || {
            build_agents(&self.lua, snapshot)
        })?;
        set(&table, "agents", agents)?;

        // `name` identifies, `detail` distinguishes on screen, `backend` is what
        // a command and a bookmark scope are keyed by — a flow needs all three,
        // and none follows from another.
        // Built from the snapshot alone, so it changes when the database does
        // and not once a frame.
        let hosts = self.group("hosts", [epoch.snapshot, 0, 0, 0], || {
            build_hosts(&self.lua, snapshot)
        })?;
        set(&table, "hosts", hosts)?;

        // Built from the snapshot alone, like the agents and places above.
        let fleets = self.group("fleets", [epoch.snapshot, 0, 0, 0], || {
            build_fleets(&self.lua, snapshot)
        })?;
        set(&table, "fleets", fleets)?;

        self.publish_repo_reads(&table, repo_store, wants, epoch)?;
        self.publish_settings(&table, settings)?;

        set(&table, "taken_at_ms", snapshot.taken_at_ms)?;
        set(&table, "version", env!("THURBOX_VERSION"))?;
        set(&table, "api", crate::kernel::lua_api::API)?;
        let hover = match hovered {
            None => self.group("hover", [0, 0, 0, 0], || {
                self.lua
                    .create_table()
                    .map(Value::Table)
                    .map_err(|e| e.to_string())
            })?,
            Some(identity) => {
                let hover = self.lua.create_table().map_err(|e| e.to_string())?;
                if let Some(id) = &identity.id {
                    set(&hover, "id", id.clone())?;
                }
                if let Some(role) = &identity.role {
                    set(&hover, "role", role.clone())?;
                }
                Value::Table(hover)
            }
        };
        set(&table, "hover", hover)?;

        // Which pane holds focus, by name. `ctx.focused` answers "am I?", but
        // the footer has to name whoever IS and is not focusable itself.
        set(&table, "focus", focus.unwrap_or(""))?;
        // The mouse text selection, empty when nothing is selected. A bare scalar
        // like `focus`: a pane painting it live must be impure, since it moves no
        // epoch — see `Published::selection`.
        set(&table, "selection", selection.unwrap_or(""))?;
        set(&table, "reloads", self.reloads)?;

        let commands = self.group("commands", [epoch.data, 0, 0, 0], || {
            build_commands(&self.lua, inflight)
        })?;
        set(&table, "commands", commands)?;

        let theme = self.group("theme", [epoch.themes, 0, 0, 0], || {
            build_theme(&self.lua, themes)
        })?;
        set(&table, "theme", theme)?;

        let reg = self.group("registry", [epoch.registry, 0, 0, 0], || {
            build_registry(&self.lua, registry)
        })?;
        set(&table, "registry", reg)?;

        // The selected session's changes, when any have been asked for. The
        // *absence* of an entry is "not requested"; `pending` is "asked, not
        // finished" — a slow diff must not read as a clean worktree.
        //
        // Gated because this was the heaviest ungated block by a distance: a
        // Ready diff can carry up to MAX_DIFF_BYTES of body, published line by
        // line — a Rust String clone plus a Lua string per line, every frame,
        // forever once computed. The store only changes through its worker
        // landing a changed answer or an invalidate command. Its own version
        // keeps unrelated workers from copying the body again; the snapshot
        // version covers the session set.
        let diffs_value = self.group("diffs", [epoch.snapshot, diffs.version(), 0, 0], || {
            build_diffs(&self.lua, snapshot, diffs)
        })?;
        set(&table, "diffs", diffs_value)?;

        // The links map is maintained compare-before-store (refresh_links), and
        // an actual change moves the data epoch — so the rebuilt table is owed
        // only then, not on every frame a linked agent prints.
        let links_value = self.group("links", [epoch.snapshot, epoch.data, 0, 0], || {
            build_links(&self.lua, links)
        })?;
        set(&table, "links", links_value)?;

        // The content search's answer. Absent unless something asked, so an
        // interface that never searches never pays for it — and gated on the
        // answer's own serial, not the data epoch. Every worker moves that
        // epoch (links, diffs, metrics), eight times a second under load, and
        // each move rebuilt up to 200 hit tables and handed the strip a new
        // table, which is how it tells that its answer changed. An answer made
        // outside a store (a test's) has no serial and keeps the epoch.
        let search_key = match search {
            None => [0, 0, 0, 0],
            Some(answer) if answer.serial > 0 => [1, answer.serial, 0, 0],
            Some(_) => [2, epoch.snapshot, epoch.data, 0],
        };
        let search_value = self.group("search", search_key, || build_search(&self.lua, *search))?;
        set(&table, "search", search_value)?;

        // Machine, per-agent and account metrics. Each is absent rather than
        // zero when it has not been sampled, so a panel can distinguish "no
        // reading yet" from "nothing spent" — v1's info panel draws the same
        // distinction by omitting the row. A sample can only land through
        // Metrics::poll, which moves its own sample serial, so the tables are owed
        // only then — they were rebuilt per frame for readings that move once
        // a second at most.
        let metrics_value = self.group("metrics", [epoch.snapshot, epoch.metrics, 0, 0], || {
            build_metrics(&self.lua, snapshot, metrics)
        })?;
        set(&table, "metrics", metrics_value)?;

        // What the arrangement needs to know about the chrome bands, and nothing
        // more: whether the message band wants a row. The message itself is not
        // here, because placing a band and filling it are different jobs.
        let chrome = self.lua.create_table().map_err(|e| e.to_string())?;
        set(&chrome, "status_rows", *status_rows)?;
        set(&table, "chrome", chrome)?;
        // The one arrangement input published as a bare scalar; recorded so
        // the arrangement cache can key on it (see `LayoutKey`).
        self.last_status_rows.set(*status_rows);

        let inventory_key = fnv64(inventory.iter().flat_map(|row| {
            [
                row.path.as_bytes(),
                row.name.as_bytes(),
                row.kind.as_str().as_bytes(),
                row.slot.as_bytes(),
                row.source.as_str().as_bytes(),
                row.state.as_str().as_bytes(),
                row.error.as_deref().unwrap_or("").as_bytes(),
            ]
        }));
        let inventory_value = self.group("plugins", [inventory_key, 0, 0, 0], || {
            build_inventory(&self.lua, inventory)
        })?;
        set(&table, "mods", inventory_value.clone())?;
        set(&table, "plugins", inventory_value)?;
        set(&table, "ui_dir", to_lua_string(&self.lua, ui_dir)?)?;

        let platform = self.group("platform", [0, 0, 0, 0], || build_platform(&self.lua))?;
        set(&table, "platform", platform)?;

        // Whether a key's release reaches a binding that asked for it (see
        // `Binding::release`) — what a pane offering press-and-hold reads to
        // decide whether to offer it or to fall back to press-to-toggle.
        let releases = self.key_releases.get();
        let keyboard = self.group("keyboard", [releases as u64 + 1, 0, 0, 0], || {
            let keyboard = self.lua.create_table().map_err(|e| e.to_string())?;
            set(&keyboard, "releases", releases.as_str())?;
            Ok(Value::Table(keyboard))
        })?;
        set(&table, "keyboard", keyboard)?;

        set(&table, "can_open_links", *can_open)?;
        set(
            &table,
            "error",
            opt_lua_string(&self.lua, snapshot.error.as_deref())?,
        )?;

        *self.epoch.borrow_mut() = Some(*epoch);
        super::api::freeze_fields(&self.lua, &table).map_err(|e| e.to_string())?;
        let observed = super::api::observe_samples(&self.lua, table, self.sample_read.clone())
            .map_err(|e| e.to_string())?;
        set(&self.lua.globals(), "thurbox", observed)
    }

    /// Publish the settings in force.
    ///
    /// Every switch is published, including ones the kernel does not act on: a
    /// pane that owns its own surface is the only thing that can honour its own
    /// switch, which is the point of publishing rather than gating centrally.
    fn publish_settings(
        &self,
        table: &Table,
        settings: &crate::session::settings::Settings,
    ) -> Result<(), String> {
        let features = self.lua.create_table().map_err(|e| e.to_string())?;
        let flags = &settings.features;
        for (name, value) in [
            ("shell_pane", flags.shell_pane),
            ("perf_hud", flags.perf_hud),
            ("mouse", flags.mouse),
            ("notifications", flags.notifications),
            ("version_check", flags.version_check),
            ("auto_update", flags.auto_update),
        ] {
            features.set(name, value).map_err(|e| e.to_string())?;
        }

        let published = self.lua.create_table().map_err(|e| e.to_string())?;
        published
            .set("features", features)
            .map_err(|e| e.to_string())?;
        published
            .set("two_panel_min_cols", settings.two_panel_min_cols)
            .map_err(|e| e.to_string())?;
        published
            .set("scrollback_lines", settings.scrollback_lines)
            .map_err(|e| e.to_string())?;
        table
            .set("settings", published)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn publish_repo_reads(
        &self,
        table: &Table,
        store: &crate::kernel::repos::RepoStore,
        wants: &crate::kernel::repos::Wants,
        epoch: &super::Epoch,
    ) -> Result<(), String> {
        // Each read is gated on the data epoch (an answer landing moves it)
        // paired with a digest of the question: the flow re-states its wants
        // through `store`, which moves no version, so the question itself has
        // to be part of the key. While the flow is open the bookmark rows —
        // potentially hundreds, six fields each — were otherwise rebuilt and
        // recrossed into Lua on every frame; this is also what gives the rows
        // a stable table identity for the flow's own memoization.
        let bookmarks_key = fnv64([
            &b"bookmarks"[..],
            wants.bookmarks.as_deref().unwrap_or("\x01none").as_bytes(),
        ]);
        let bookmarks_value = self.group("bookmarks", [epoch.data, bookmarks_key, 0, 0], || {
            self.build_bookmarks(store, wants).map(Value::Table)
        })?;
        set(table, "bookmarks", bookmarks_value)?;

        let browse_key = fnv64([
            &b"browse"[..],
            wants
                .browse
                .as_ref()
                .map(|(host, dir)| format!("{host}\0{dir}"))
                .unwrap_or_else(|| "\x01none".to_string())
                .as_bytes(),
        ]);
        let browse_value = self.group("browse", [epoch.data, browse_key, 0, 0], || {
            self.build_browse(store, wants).map(Value::Table)
        })?;
        set(table, "browse", browse_value)?;

        let branches_key = fnv64([
            &b"repo_branches"[..],
            wants.branches.as_deref().unwrap_or("\x01none").as_bytes(),
        ]);
        let branches_value =
            self.group("repo_branches", [epoch.data, branches_key, 0, 0], || {
                self.build_repo_branches(store, wants).map(Value::Table)
            })?;
        set(table, "repo_branches", branches_value)?;

        Ok(())
    }

    /// The branches of the repository `store.want_branches` names: empty
    /// while nothing is asked, `loading` until git answers, and `is_git`
    /// false for a directory that is no repository.
    fn build_repo_branches(
        &self,
        store: &crate::kernel::repos::RepoStore,
        wants: &crate::kernel::repos::Wants,
    ) -> Result<Table, String> {
        use crate::kernel::repos::BranchListing;

        let out = self.lua.create_table().map_err(|e| e.to_string())?;
        let Some(repo) = &wants.branches else {
            return Ok(out);
        };
        // A remote place's repository is asked as `<host>\0<path>`; the flow
        // matches the read by its path.
        let path = repo
            .split_once('\0')
            .map_or(repo.as_str(), |(_, path)| path);
        out.set("repo", path.to_string())
            .map_err(|e| e.to_string())?;
        let listing = store.branches(repo);
        out.set(
            "loading",
            matches!(listing, None | Some(BranchListing::Pending)),
        )
        .map_err(|e| e.to_string())?;
        out.set("is_git", !matches!(listing, Some(BranchListing::NotGit)))
            .map_err(|e| e.to_string())?;
        if let Some(BranchListing::Failed(why)) = listing {
            out.set("error", why.clone()).map_err(|e| e.to_string())?;
        }
        if let Some(why) = store.fetch_note(repo) {
            out.set("fetch_error", why).map_err(|e| e.to_string())?;
        }
        let names = self.lua.create_table().map_err(|e| e.to_string())?;
        let worktrees = self.lua.create_table().map_err(|e| e.to_string())?;
        if let Some(BranchListing::Ready(found)) = listing {
            for (index, tree) in found.worktrees.iter().enumerate() {
                let item = self.lua.create_table().map_err(|e| e.to_string())?;
                item.set("path", tree.path.clone())
                    .map_err(|e| e.to_string())?;
                item.set("branch", opt_lua_string(&self.lua, tree.branch.as_deref())?)
                    .map_err(|e| e.to_string())?;
                worktrees.set(index + 1, item).map_err(|e| e.to_string())?;
            }
            out.set(
                "default",
                opt_lua_string(&self.lua, found.default.as_deref())?,
            )
            .map_err(|e| e.to_string())?;
            for (index, name) in found.names.iter().enumerate() {
                names
                    .set(index + 1, name.clone())
                    .map_err(|e| e.to_string())?;
            }
        }
        out.set("names", names).map_err(|e| e.to_string())?;
        out.set("worktrees", worktrees).map_err(|e| e.to_string())?;
        Ok(out)
    }

    fn build_bookmarks(
        &self,
        store: &crate::kernel::repos::RepoStore,
        wants: &crate::kernel::repos::Wants,
    ) -> Result<Table, String> {
        let bookmarks = self.lua.create_table().map_err(|e| e.to_string())?;
        if let Some(host) = &wants.bookmarks {
            bookmarks
                .set("host", host.clone())
                .map_err(|e| e.to_string())?;
            // Absent rows and no rows are different states: the first is "still
            // reading", which a flow renders rather than calling it empty.
            bookmarks
                .set("loading", store.bookmarks(host).is_none())
                .map_err(|e| e.to_string())?;
            let rows = self.lua.create_table().map_err(|e| e.to_string())?;
            for (index, row) in store.bookmarks(host).into_iter().flatten().enumerate() {
                let item = self.lua.create_table().map_err(|e| e.to_string())?;
                item.set("path", row.path.clone())
                    .map_err(|e| e.to_string())?;
                item.set("name", row.name.clone())
                    .map_err(|e| e.to_string())?;
                item.set("parent", opt_lua_string(&self.lua, row.parent.as_deref())?)
                    .map_err(|e| e.to_string())?;
                // `nil` unless the row carries a name of its own: a bookmark the
                // user labelled, or one the flow offers where the path is an
                // implementation detail rather than something they typed.
                item.set("label", opt_lua_string(&self.lua, row.label.as_deref())?)
                    .map_err(|e| e.to_string())?;
                item.set("is_parent", row.is_parent)
                    .map_err(|e| e.to_string())?;
                item.set("offered", row.offered)
                    .map_err(|e| e.to_string())?;
                // `nil` means never established, which is a third state beside
                // true and false: it stays worktree-capable.
                item.set(
                    "is_git",
                    match row.is_git {
                        Some(value) => Value::Boolean(value),
                        None => Value::Nil,
                    },
                )
                .map_err(|e| e.to_string())?;
                rows.set(index + 1, item).map_err(|e| e.to_string())?;
            }
            bookmarks.set("rows", rows).map_err(|e| e.to_string())?;
        }
        Ok(bookmarks)
    }

    fn build_browse(
        &self,
        store: &crate::kernel::repos::RepoStore,
        wants: &crate::kernel::repos::Wants,
    ) -> Result<Table, String> {
        use crate::kernel::repos::Listing;

        let browse = self.lua.create_table().map_err(|e| e.to_string())?;
        if let Some((host, dir)) = &wants.browse {
            browse
                .set("host", host.clone())
                .map_err(|e| e.to_string())?;
            // A remote place's `~` is published as the home the runtime
            // listed, so an entry joined to it is an absolute path there.
            let shown = store.resolved_dir(host, dir).unwrap_or(dir);
            // Kept for every later listing of the place, so a typed `~/…`
            // there is expanded to the home its runtime named.
            if let Some(home) = store.resolved_dir(host, "~") {
                browse
                    .set("home", home.to_string())
                    .map_err(|e| e.to_string())?;
            }
            browse
                .set("dir", shown.to_string())
                .map_err(|e| e.to_string())?;
            let listing = store.listing(host, dir);
            browse
                .set("loading", matches!(listing, None | Some(Listing::Pending)))
                .map_err(|e| e.to_string())?;
            browse
                .set(
                    "error",
                    match listing {
                        Some(Listing::Failed(message)) => to_lua_string(&self.lua, message)?,
                        _ => Value::Nil,
                    },
                )
                .map_err(|e| e.to_string())?;
            let entries = self.lua.create_table().map_err(|e| e.to_string())?;
            if let Some(Listing::Ready(found)) = listing {
                for (index, entry) in found.iter().enumerate() {
                    let item = self.lua.create_table().map_err(|e| e.to_string())?;
                    item.set("name", entry.name.clone())
                        .map_err(|e| e.to_string())?;
                    item.set("is_git", entry.is_git)
                        .map_err(|e| e.to_string())?;
                    entries.set(index + 1, item).map_err(|e| e.to_string())?;
                }
            }
            browse.set("entries", entries).map_err(|e| e.to_string())?;
        }
        Ok(browse)
    }
}

fn build_sessions(
    lua: &Lua,
    snapshot: &Snapshot,
    attach_errors: &HashMap<String, String>,
    starts: &HashMap<String, crate::kernel::terminal::Start>,
    meta: &HashMap<String, crate::kernel::terminal::AgentMeta>,
) -> Result<Value, String> {
    let sessions = lua.create_table().map_err(|e| e.to_string())?;
    for (index, row) in snapshot.sessions.iter().enumerate() {
        // Pre-sized: this row takes ~30 named fields, and a table grown one
        // field at a time rehashes itself on the way there — per session,
        // per frame.
        let entry = lua
            .create_table_with_capacity(0, 32)
            .map_err(|e| e.to_string())?;
        set(&entry, "id", to_lua_string(lua, &row.id)?)?;
        set(&entry, "name", to_lua_string(lua, &row.name)?)?;
        set(&entry, "fleet", opt_lua_string(lua, row.fleet())?)?;
        set(&entry, "agent", to_lua_string(lua, &row.agent)?)?;
        // Three names, three fields, because they are three different facts and
        // no two of them can stand in for each other: what the row was created
        // as, what a driver declared it runs (`session reports-as`), and what is
        // observably in the pane right now. A row created as `zsh` with claude
        // running in it is otherwise a session the interface can only call
        // `zsh`.
        set(&entry, "key", opt_lua_string(lua, row.key.as_deref())?)?;
        set(
            &entry,
            "reports_as",
            opt_lua_string(lua, row.reports_as.as_deref())?,
        )?;
        set(
            &entry,
            "detected_agent",
            opt_lua_string(lua, row.detected_agent.as_deref())?,
        )?;
        set(&entry, "status", to_lua_string(lua, row.status.as_str())?)?;
        // Where a Fleet row's status came from, so a pane draws a hook's
        // report apart from a screen rule's reading and from neither.
        let source = row.status_source.as_ref();
        set(
            &entry,
            "status_source",
            opt_lua_string(lua, source.map(|s| s.source.as_str()))?,
        )?;
        set(
            &entry,
            "status_rule",
            opt_lua_string(lua, source.and_then(|s| s.rule.as_deref()))?,
        )?;
        set(&entry, "status_since_ms", source.and_then(|s| s.since_ms))?;
        let deleting = row.deleting.as_ref();
        set(&entry, "deleting_since_ms", deleting.map(|d| d.since_ms))?;
        set(
            &entry,
            "deleting_why",
            opt_lua_string(lua, deleting.and_then(|d| d.why.as_deref()))?,
        )?;
        set(
            &entry,
            "state_why",
            opt_lua_string(lua, row.state_why.as_deref())?,
        )?;
        let attach_error = attach_errors.get(&row.id).map(String::as_str);
        set(&entry, "backend", to_lua_string(lua, &row.backend)?)?;
        set(&entry, "repo", opt_lua_string(lua, row.repo.as_deref())?)?;
        set(
            &entry,
            "branch",
            opt_lua_string(lua, row.branch.as_deref())?,
        )?;
        // What the diff is taken against, so a pane can name the range it is
        // showing rather than guessing. Distinct from `branch` above.
        set(
            &entry,
            "base_branch",
            opt_lua_string(lua, row.base_branch.as_deref())?,
        )?;
        set(
            &entry,
            "host",
            opt_lua_string(lua, row.remote_host.as_deref())?,
        )?;
        set(
            &entry,
            "parent",
            opt_lua_string(lua, row.parent_id.as_deref())?,
        )?;
        set(
            &entry,
            "cwd",
            opt_lua_string(
                lua,
                row.cwd.as_ref().map(|p| p.to_string_lossy()).as_deref(),
            )?,
        )?;
        set(&entry, "worktrees", row.worktree_count)?;
        // Every repo the session spans, so a group can be labelled with all
        // of them rather than just the primary.
        let repos = lua.create_table().map_err(|e| e.to_string())?;
        for (position, name) in row.repos.iter().enumerate() {
            repos
                .raw_set(position + 1, to_lua_string(lua, name)?)
                .map_err(|e| e.to_string())?;
        }
        set(&entry, "repos", Value::Table(repos))?;
        // What the agent said about itself over its own terminal. Absent
        // when it has said nothing, which a row must tell apart from an
        // empty message.
        let agent_meta = meta.get(&row.id);
        set(
            &entry,
            "activity",
            opt_lua_string(lua, agent_meta.and_then(|m| m.activity.as_deref()))?,
        )?;
        set(
            &entry,
            "notification",
            opt_lua_string(lua, agent_meta.and_then(|m| m.notification.as_deref()))?,
        )?;
        // What whoever drives the session wrote under `ui.*` (issue #1523):
        // a status note, a group label. Already cleaned on the snapshot's
        // worker, and absent rather than empty so a row with none costs no
        // table.
        if !row.meta.is_empty() {
            let meta = lua
                .create_table_with_capacity(0, row.meta.len())
                .map_err(|e| e.to_string())?;
            for (key, value) in &row.meta {
                set(&meta, key, to_lua_string(lua, value)?)?;
            }
            set(&entry, "meta", meta)?;
        }
        set(&entry, "display_order", row.display_order)?;
        if let Some(git) = row.git {
            let stats = lua.create_table().map_err(|e| e.to_string())?;
            set(&stats, "files", git.files_changed)?;
            set(&stats, "insertions", git.insertions)?;
            set(&stats, "deletions", git.deletions)?;
            set(&stats, "untracked", git.untracked)?;
            set(&stats, "dirty", git.dirty)?;
            set(&stats, "ahead", git.ahead)?;
            set(&stats, "behind", git.behind)?;
            set(&stats, "merged", git.merged)?;
            set(&entry, "git", stats)?;
        }
        // Why this session's terminal is not live, when it is not.
        set(&entry, "attach_error", opt_lua_string(lua, attach_error)?)?;
        let start = starts.get(&row.id);
        set(
            &entry,
            "starting",
            start.is_some_and(|start| *start == crate::kernel::terminal::Start::Pending),
        )?;
        let start_error = match start {
            Some(crate::kernel::terminal::Start::Failed(error)) => Some(error.as_str()),
            _ => None,
        };
        set(&entry, "start_error", opt_lua_string(lua, start_error)?)?;
        sessions
            .raw_set(index + 1, entry)
            .map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(sessions))
}

fn build_repos(lua: &Lua, snapshot: &Snapshot) -> Result<Value, String> {
    let repos = lua.create_table().map_err(|e| e.to_string())?;
    for (index, repo) in snapshot.repos.iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "path", repo.path.clone())?;
        set(&item, "name", repo.name.clone())?;
        repos.raw_set(index + 1, item).map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(repos))
}

/// `nil` until Fleet has said which harnesses it has: a flow must tell a
/// pending list from an empty one.
fn build_agents(lua: &Lua, snapshot: &Snapshot) -> Result<Value, String> {
    let Some(known) = &snapshot.agents else {
        return Ok(Value::Nil);
    };
    let agents = lua.create_table().map_err(|e| e.to_string())?;
    for (index, agent) in known.iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "name", agent.name.clone())?;
        agents.raw_set(index + 1, item).map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(agents))
}

/// `nil` until Fleet has said which places it defines: a flow must tell a
/// pending list from one with no remote place.
fn build_hosts(lua: &Lua, snapshot: &Snapshot) -> Result<Value, String> {
    hosts_table(lua, snapshot.hosts.as_deref())
}

fn hosts_table(lua: &Lua, known: Option<&[HostRow]>) -> Result<Value, String> {
    let Some(known) = known else {
        return Ok(Value::Nil);
    };
    let hosts = lua.create_table().map_err(|e| e.to_string())?;
    for (index, host) in known.iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "name", host.name.clone())?;
        set(&item, "detail", host.detail.clone())?;
        set(&item, "backend", host.backend.clone())?;
        set(&item, "ready", host.ready)?;
        set(&item, "why", host.why.clone())?;
        hosts.raw_set(index + 1, item).map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(hosts))
}

/// Every listed Fleet, each with its own agents and places, `nil` until
/// that Fleet has answered.
fn build_fleets(lua: &Lua, snapshot: &Snapshot) -> Result<Value, String> {
    let fleets = lua.create_table().map_err(|e| e.to_string())?;
    for (index, fleet) in snapshot.fleets.iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "name", fleet.name.clone())?;
        set(&item, "transport", fleet.transport.clone())?;
        set(&item, "remote", fleet.remote)?;
        set(&item, "state", fleet.state.as_str())?;
        set(&item, "why", fleet.why.clone())?;
        set(&item, "home", fleet.home.clone())?;
        let agents = match &fleet.agents {
            None => Value::Nil,
            Some(known) => {
                let agents = lua.create_table().map_err(|e| e.to_string())?;
                for (at, agent) in known.iter().enumerate() {
                    let row = lua.create_table().map_err(|e| e.to_string())?;
                    set(&row, "name", agent.name.clone())?;
                    agents.raw_set(at + 1, row).map_err(|e| e.to_string())?;
                }
                Value::Table(agents)
            }
        };
        set(&item, "agents", agents)?;
        set(&item, "places", hosts_table(lua, fleet.places.as_deref())?)?;
        fleets.raw_set(index + 1, item).map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(fleets))
}

fn build_theme(lua: &Lua, themes: &Themes) -> Result<Value, String> {
    let roles = lua.create_table().map_err(|e| e.to_string())?;
    for (role, colour) in themes.roles() {
        set(&roles, role, colour)?;
    }
    let theme = lua.create_table().map_err(|e| e.to_string())?;
    set(&theme, "name", themes.active_name())?;
    set(
        &theme,
        "nerd_font",
        themes.active().palette.nerd_font_enabled,
    )?;
    set(&theme, "roles", roles)?;

    let choices = lua.create_table().map_err(|e| e.to_string())?;
    for (index, choice) in themes.choices().iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "name", choice.name.clone())?;
        set(&item, "display_name", choice.display_name.clone())?;
        set(&item, "light", choice.is_light)?;
        set(&item, "custom", choice.is_custom)?;
        choices
            .raw_set(index + 1, item)
            .map_err(|e| e.to_string())?;
    }
    set(&theme, "choices", choices)?;
    Ok(Value::Table(theme))
}

fn build_registry(lua: &Lua, registry: &Registry) -> Result<Value, String> {
    let keys = lua.create_table().map_err(|e| e.to_string())?;
    for (index, binding) in registry.bindings().iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "mod", binding.owner.clone())?;
        set(&item, "plugin", binding.owner.clone())?;
        set(&item, "action", binding.action.clone())?;
        set(&item, "key", binding.chord.clone())?;
        set(&item, "default_key", binding.default_chord.clone())?;
        set(&item, "desc", binding.description.clone())?;
        set(&item, "scope", binding.scope.as_str())?;
        set(&item, "rebound", binding.chord != binding.default_chord)?;
        set(&item, "group", binding.group.clone())?;
        keys.raw_set(index + 1, item).map_err(|e| e.to_string())?;
    }
    let settings = lua.create_table().map_err(|e| e.to_string())?;
    for (index, setting) in registry.settings().iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "mod", setting.owner.clone())?;
        set(&item, "plugin", setting.owner.clone())?;
        set(&item, "id", setting.id.clone())?;
        set(&item, "desc", setting.description.clone())?;
        set(&item, "type", setting.value.type_name())?;
        set_value(&item, "value", &setting.value)?;
        set_value(&item, "default", &setting.default)?;
        if !setting.choices.is_empty() {
            let choices = lua.create_table().map_err(|e| e.to_string())?;
            for (at, choice) in setting.choices.iter().enumerate() {
                choices
                    .raw_set(at + 1, choice.as_str())
                    .map_err(|e| e.to_string())?;
            }
            set(&item, "choices", choices)?;
        }
        settings
            .raw_set(index + 1, item)
            .map_err(|e| e.to_string())?;
    }
    let commands = lua.create_table().map_err(|e| e.to_string())?;
    for (index, command) in registry.commands().iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "mod", command.owner.clone())?;
        set(&item, "plugin", command.owner.clone())?;
        set(&item, "action", command.action.clone())?;
        set(&item, "desc", command.description.clone())?;
        commands
            .raw_set(index + 1, item)
            .map_err(|e| e.to_string())?;
    }
    let reg = lua.create_table().map_err(|e| e.to_string())?;
    set(&reg, "keys", keys)?;
    set(&reg, "commands", commands)?;
    set(&reg, "settings", settings)?;
    let sections = lua.create_table().map_err(|e| e.to_string())?;
    for (index, name) in crate::kernel::registry::HELP_SECTIONS.iter().enumerate() {
        sections
            .raw_set(index + 1, *name)
            .map_err(|e| e.to_string())?;
    }
    set(&reg, "sections", sections)?;
    Ok(Value::Table(reg))
}

fn build_commands(lua: &Lua, inflight: &[InFlight]) -> Result<Value, String> {
    let commands = lua.create_table().map_err(|e| e.to_string())?;
    for (index, entry) in inflight.iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "id", entry.id)?;
        set(&item, "kind", entry.kind)?;
        set(&item, "session", entry.session.clone())?;
        set(&item, "phase", entry.phase.as_str())?;
        set(
            &item,
            "subject",
            opt_lua_string(lua, entry.subject.as_deref())?,
        )?;
        // The machine a creation will land on, so a list grouped by host draws
        // the placeholder under it rather than under whichever group happens
        // to hold that repository.
        set(&item, "host", opt_lua_string(lua, entry.host.as_deref())?)?;
        set(&item, "error", opt_lua_string(lua, entry.error.as_deref())?)?;
        commands
            .raw_set(index + 1, item)
            .map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(commands))
}

fn build_diffs(lua: &Lua, snapshot: &Snapshot, diffs: &DiffStore) -> Result<Value, String> {
    let diff_table = lua.create_table().map_err(|e| e.to_string())?;
    for row in &snapshot.sessions {
        let Some(diff) = diffs.get(&row.id) else {
            continue;
        };
        let item = lua.create_table().map_err(|e| e.to_string())?;
        match diff {
            crate::kernel::diff::Diff::Pending => {
                set(&item, "state", "pending")?;
            }
            crate::kernel::diff::Diff::Failed(error) => {
                set(&item, "state", "failed")?;
                set(&item, "error", error.clone())?;
            }
            crate::kernel::diff::Diff::Ready {
                files,
                body,
                truncated,
                raw_bytes,
                untracked_omitted,
            } => {
                set(&item, "state", "ready")?;
                set(&item, "truncated", *truncated)?;
                set(&item, "raw_bytes", *raw_bytes as i64)?;
                // A short file list is a different failure from a cut body,
                // so it is its own field rather than folded into `truncated`.
                set(&item, "untracked_omitted", *untracked_omitted as i64)?;
                let list = lua.create_table().map_err(|e| e.to_string())?;
                for (index, file) in files.iter().enumerate() {
                    let entry = lua.create_table().map_err(|e| e.to_string())?;
                    set(&entry, "path", file.path.clone())?;
                    set(&entry, "added", file.added)?;
                    set(&entry, "removed", file.removed)?;
                    // Both already known to the parser; a pane re-reading the
                    // body to recover them is the cost of dropping them here.
                    set(&entry, "status", file.status)?;
                    set(
                        &entry,
                        "old_path",
                        opt_lua_string(lua, file.old_path.as_deref())?,
                    )?;
                    list.raw_set(index + 1, entry).map_err(|e| e.to_string())?;
                }
                set(&item, "files", list)?;
                let lines = lua.create_table().map_err(|e| e.to_string())?;
                for (index, line) in body.iter().enumerate() {
                    lines
                        .raw_set(index + 1, line.clone())
                        .map_err(|e| e.to_string())?;
                }
                set(&item, "body", lines)?;
            }
        }
        diff_table
            .raw_set(row.id.clone(), item)
            .map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(diff_table))
}

fn build_links(
    lua: &Lua,
    links: &HashMap<String, Vec<(String, usize, usize)>>,
) -> Result<Value, String> {
    let links_table = lua.create_table().map_err(|e| e.to_string())?;
    for (session, found) in links.iter() {
        let list = lua.create_table().map_err(|e| e.to_string())?;
        for (index, (url, row, col)) in found.iter().enumerate() {
            let item = lua.create_table().map_err(|e| e.to_string())?;
            set(&item, "url", url.clone())?;
            set(&item, "row", *row)?;
            set(&item, "col", *col)?;
            list.raw_set(index + 1, item).map_err(|e| e.to_string())?;
        }
        links_table
            .raw_set(session.clone(), list)
            .map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(links_table))
}

fn build_search(
    lua: &Lua,
    answer: Option<&crate::kernel::search::Answer>,
) -> Result<Value, String> {
    let Some(answer) = answer else {
        return Ok(Value::Nil);
    };
    let err = |e: mlua::Error| e.to_string();
    let table = lua.create_table().map_err(err)?;
    set(&table, "query", answer.request.query.as_str())?;
    if let Some(ids) = &answer.request.sessions {
        set(&table, "within", ids.join(" "))?;
    }
    set(&table, "total", answer.total)?;
    set(&table, "sessions", answer.sessions)?;
    set(&table, "lines", answer.lines)?;
    set(&table, "ms", answer.elapsed.as_secs_f64() * 1000.0)?;
    if let Some(error) = &answer.error {
        set(&table, "error", error.as_str())?;
    }
    let hits = lua
        .create_table_with_capacity(answer.hits.len(), 0)
        .map_err(err)?;
    for (index, hit) in answer.hits.iter().enumerate() {
        let entry = lua.create_table().map_err(err)?;
        set(&entry, "session", hit.session.as_str())?;
        set(&entry, "shell", hit.shell)?;
        set(&entry, "text", hit.text.as_str())?;
        // 1-based character indices, the shape `lib.fuzzy.spans` lights.
        let positions = lua.create_table().map_err(err)?;
        let mut n = 0;
        for &(start, end) in &hit.ranges {
            for at in start..end {
                n += 1;
                positions.raw_set(n, at + 1).map_err(err)?;
            }
        }
        set(&entry, "positions", positions)?;
        set(&entry, "back", hit.back)?;
        set(&entry, "scroll", hit.scroll)?;
        set(&entry, "row", hit.row)?;
        set(&entry, "exact", hit.exact)?;
        set(&entry, "score", hit.score)?;
        hits.raw_set(index + 1, entry).map_err(err)?;
    }
    set(&table, "hits", hits)?;
    Ok(Value::Table(table))
}

fn build_metrics(lua: &Lua, snapshot: &Snapshot, metrics: &Metrics) -> Result<Value, String> {
    let metrics_table = lua.create_table().map_err(|e| e.to_string())?;
    let system = metrics.system();
    let machine = lua.create_table().map_err(|e| e.to_string())?;
    set(&machine, "cpu_percent", system.cpu_percent)?;
    set(&machine, "memory_used", system.memory_used)?;
    set(&machine, "memory_total", system.memory_total)?;
    set(&metrics_table, "system", machine)?;

    let per_session = lua.create_table().map_err(|e| e.to_string())?;
    for row in &snapshot.sessions {
        let entry = lua.create_table().map_err(|e| e.to_string())?;
        let mut any = false;
        if let Some(resources) = metrics.resources(&row.id) {
            set(&entry, "cpu_percent", resources.cpu_percent)?;
            set(&entry, "memory_bytes", resources.memory_bytes)?;
            any = true;
        }
        if let Some(agent) = metrics.agent(&row.id) {
            set(&entry, "agent", agent_metrics_table(lua, agent)?)?;
            any = true;
        }
        if any {
            per_session
                .raw_set(row.id.clone(), entry)
                .map_err(|e| e.to_string())?;
        }
    }
    set(&metrics_table, "sessions", per_session)?;
    Ok(Value::Table(metrics_table))
}

fn build_inventory(
    lua: &Lua,
    inventory: &[crate::kernel::inventory::Row],
) -> Result<Value, String> {
    let inventory_table = lua.create_table().map_err(|e| e.to_string())?;
    for (index, row) in inventory.iter().enumerate() {
        let entry = lua.create_table().map_err(|e| e.to_string())?;
        set(&entry, "path", to_lua_string(lua, &row.path)?)?;
        set(&entry, "name", to_lua_string(lua, &row.name)?)?;
        set(&entry, "kind", to_lua_string(lua, row.kind.as_str())?)?;
        set(&entry, "slot", to_lua_string(lua, &row.slot)?)?;
        set(&entry, "source", to_lua_string(lua, row.source.as_str())?)?;
        set(&entry, "state", to_lua_string(lua, row.state.as_str())?)?;
        set(&entry, "error", opt_lua_string(lua, row.error.as_deref())?)?;
        inventory_table
            .raw_set(index + 1, entry)
            .map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(inventory_table))
}

fn build_platform(lua: &Lua) -> Result<Value, String> {
    let platform = lua.create_table().map_err(|e| e.to_string())?;
    set(&platform, "os", std::env::consts::OS)?;
    set(&platform, "arch", std::env::consts::ARCH)?;
    Ok(Value::Table(platform))
}

pub(super) fn run_to_lua(lua: &Lua, run: &crate::kernel::runs::Run) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    match run {
        crate::kernel::runs::Run::Pending => {
            table.set("state", "pending")?;
        }
        crate::kernel::runs::Run::Failed(reason) => {
            table.set("state", "failed")?;
            table.set("error", reason.clone())?;
        }
        crate::kernel::runs::Run::Done(output) => {
            table.set("state", "done")?;
            table.set("stdout", output.stdout.clone())?;
            table.set("stderr", output.stderr.clone())?;
            table.set("status", output.status)?;
            table.set("truncated", output.truncated)?;
            table.set("timed_out", output.timed_out)?;
            // The common question, answered once here rather than by every pane
            // re-deriving it from a nullable status.
            table.set("ok", output.status == Some(0) && !output.timed_out)?;
        }
    }
    Ok(table)
}

/// Set a setting value on a table in whatever Lua type it actually is.
fn set_value(table: &Table, key: &str, value: &SettingValue) -> Result<(), String> {
    match value {
        SettingValue::Bool(b) => table.set(key, *b),
        SettingValue::Number(n) => table.set(key, *n),
        SettingValue::Text(t) => table.set(key, t.clone()),
    }
    .map_err(|e| e.to_string())
}

fn to_lua_string(lua: &Lua, value: &str) -> Result<Value, String> {
    lua.create_string(value)
        .map(Value::String)
        .map_err(|e| e.to_string())
}

fn opt_lua_string(lua: &Lua, value: Option<&str>) -> Result<Value, String> {
    match value {
        Some(text) => to_lua_string(lua, text),
        None => Ok(Value::Nil),
    }
}

/// An agent's statusline metrics as a Lua table.
///
/// Every field is optional on the way in and simply absent on the way out, so a
/// pane renders the rows it has rather than zeroes for the ones it does not.
fn agent_metrics_table(lua: &Lua, m: &crate::session::AgentMetrics) -> Result<Table, String> {
    let table = lua.create_table().map_err(|e| e.to_string())?;
    let set = |key: &str, value: Value| -> Result<(), String> {
        if !matches!(value, Value::Nil) {
            table.set(key, value).map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    set(
        "model",
        opt_lua_string(lua, m.model_display_name.as_deref())?,
    )?;
    set("model_id", opt_lua_string(lua, m.model_id.as_deref())?)?;
    set(
        "cli_version",
        opt_lua_string(lua, m.cli_version.as_deref())?,
    )?;
    let number = |value: Option<f64>| -> Value {
        match value {
            Some(value) => Value::Number(value),
            None => Value::Nil,
        }
    };
    let count = |value: Option<u64>| number(value.map(|v| v as f64));
    set("cost_usd", number(m.total_cost_usd))?;
    set("duration_ms", count(m.total_duration_ms))?;
    set("api_duration_ms", count(m.total_api_duration_ms))?;
    set("lines_added", count(m.total_lines_added))?;
    set("lines_removed", count(m.total_lines_removed))?;
    set("input_tokens", count(m.total_input_tokens))?;
    set("output_tokens", count(m.total_output_tokens))?;
    set("context_window", count(m.context_window_size))?;
    set(
        "context_used_percent",
        count(m.used_percentage.map(u64::from)),
    )?;
    set("current_input_tokens", count(m.current_input_tokens))?;
    set("current_output_tokens", count(m.current_output_tokens))?;
    set(
        "cache_creation_tokens",
        count(m.cache_creation_input_tokens),
    )?;
    set("cache_read_tokens", count(m.cache_read_input_tokens))?;
    Ok(table)
}
