use std::collections::HashMap;

use mlua::{Lua, Table, Value};

use super::{LuaHost, Published};
use crate::kernel::command::InFlight;
use crate::kernel::diff::DiffStore;
use crate::kernel::metrics::Metrics;
use crate::kernel::registry::{Registry, Value as SettingValue};
use crate::kernel::snapshot::Snapshot;
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
            snapshot,
            attach_errors,
            inflight,
            themes,
            registry,
            diffs,
            links,
            content,
            meta,
            metrics,
            status_rows,
            can_open,
            inventory,
            ui_dir,
            settings,
            repos: repo_store,
            wants,
        } = world;

        let table = self.lua.create_table().map_err(|e| e.to_string())?;
        // The largest group by a distance — a table per session with ~30 named
        // fields and two nested tables — and one whose inputs move rarely: the
        // snapshot's own generation, what each agent reported about itself, and
        // whether an attach failed.
        let sessions = self.group(
            "sessions",
            [epoch.snapshot, epoch.meta, epoch.failed, 0],
            || build_sessions(&self.lua, snapshot, attach_errors, meta),
        )?;
        // A file read is rooted at a session's directory, so the roots follow
        // the snapshot rather than being asked for separately — and only when
        // it moved: rebuilding cloned every id and cwd per publish.
        if self.roots_snapshot.get() != Some(epoch.snapshot) {
            self.roots_snapshot.set(Some(epoch.snapshot));
            let mut roots = self.roots.borrow_mut();
            roots.clear();
            for row in &snapshot.sessions {
                if let Some(cwd) = &row.cwd {
                    roots.insert(row.id.clone(), cwd.clone());
                }
            }
        }

        set(&table, "sessions", sessions)?;

        // What a creation flow can choose among. Reads, so picking is plugin
        // state and only committing is a command.
        // Built from the snapshot alone, so it changes when the database does
        // and not once a frame.
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
        // What a bare launch would use, so a flow preselects it rather than
        // whichever agent happens to be first.
        set(&table, "agent_default", snapshot.agent_default.clone())?;

        // `name` identifies, `detail` distinguishes on screen, `backend` is what
        // a command and a bookmark scope are keyed by — a flow needs all three,
        // and none follows from another.
        // Built from the snapshot alone, so it changes when the database does
        // and not once a frame.
        let hosts = self.group("hosts", [epoch.snapshot, 0, 0, 0], || {
            build_hosts(&self.lua, snapshot)
        })?;
        set(&table, "hosts", hosts)?;

        self.publish_repo_reads(&table, repo_store, wants, epoch)?;
        self.publish_settings(&table, settings)?;

        // Built from the snapshot alone, so it changes when the database does
        // and not once a frame.
        let deleted = self.group("deleted", [epoch.snapshot, 0, 0, 0], || {
            build_deleted(&self.lua, snapshot)
        })?;
        set(&table, "deleted", deleted)?;

        // Built from the snapshot alone, so it changes when the database does
        // and not once a frame.
        let tasks = self.group("tasks", [epoch.snapshot, 0, 0, 0], || {
            build_tasks(&self.lua, snapshot)
        })?;
        set(&table, "tasks", tasks)?;

        // Built from the snapshot alone, so it changes when the database does
        // and not once a frame.
        let automations = self.group("automations", [epoch.snapshot, 0, 0, 0], || {
            build_automations(&self.lua, snapshot)
        })?;
        set(&table, "automations", automations)?;
        set(&table, "taken_at_ms", snapshot.taken_at_ms)?;
        // The running release, for the header banner. v1 baked it into
        // `ui::status_bar::render_header` at compile time; a plugin cannot read
        // an env var, and hardcoding it in Lua would leave every shipped copy
        // claiming whatever version it was written against.
        set(&table, "version", env!("THURBOX_VERSION"))?;
        // What the pointer is over, as `id` and `role`, so a plugin can match
        // whichever it used to mark the affordance. The common case is nothing
        // hovered, which is one shared empty table (a constant key never
        // moves); only while the pointer sits on an affordance is a fresh
        // table built per publish.
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
        // Published so a plugin can render how many times it has been reloaded
        // — the feedback that tells you a save actually took effect.
        set(&table, "reloads", self.reloads)?;

        // Work accepted but not yet visible in the rows above. Published so a
        // plugin can draw it rather than leaving an unexplained gap — what v1
        // needed `PendingSpawn` for. Gated on the data epoch: accepting a
        // command, every phase move and its completion all bump it (that is
        // the ADR-P16 follow-up), and agent output deliberately does not — so
        // a streaming turn reuses this table instead of rebuilding it per
        // frame.
        let commands = self.group("commands", [epoch.data, 0, 0, 0], || {
            build_commands(&self.lua, inflight)
        })?;
        set(&table, "commands", commands)?;

        // The active palette, as role -> colour. Plugins name roles and never
        // colours, so this is the only place a literal enters the UI — and
        // swapping it restyles every pane, including ones the theme's author
        // never saw (design.md D14).
        // Thirty-six palettes, each a table, rebuilt for every frame that was
        // painted — and changed only when someone picks a different one.
        let theme = self.group("theme", [epoch.themes, 0, 0, 0], || {
            build_theme(&self.lua, themes)
        })?;
        set(&table, "theme", theme)?;

        // The registry, so help and settings can be plugins rendering what the
        // kernel collected — including declarations from plugins they have
        // never heard of.
        // Every declared binding and setting. Moves when a plugin is (re)declared,
        // a chord is rebound or a setting is set — never within a frame.
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
        // landing an answer or an invalidate command, both of which move the
        // data epoch; the snapshot version covers the session set.
        let diffs_value = self.group("diffs", [epoch.snapshot, epoch.data, 0, 0], || {
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

        // What each terminal is showing, for a content search. Empty unless
        // something asked, so an interface that never searches never pays for
        // it — and gated on the data epoch, which refresh_search_content bumps
        // exactly when the scanned text actually changed: each entry is a full
        // copy of a screen, up to CONTENT_LINE_CAP lines per session.
        let content_value = self.group("content", [epoch.snapshot, epoch.data, 0, 0], || {
            build_content(&self.lua, content)
        })?;
        set(&table, "content", content_value)?;

        // Machine, per-agent and account metrics. Each is absent rather than
        // zero when it has not been sampled, so a panel can distinguish "no
        // reading yet" from "nothing spent" — v1's info panel draws the same
        // distinction by omitting the row. A sample can only land through
        // Metrics::poll, which moves the data epoch, so the tables are owed
        // only then — they were rebuilt per frame for readings that move once
        // a second at most.
        let metrics_value = self.group("metrics", [epoch.snapshot, epoch.data, 0, 0], || {
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

        // The interface's own files. One row per file rather than per loaded
        // plugin, because the ones worth acting on are exactly those that did
        // NOT load — a removed pane, or one whose error is why the screen still
        // shows the last good version. There is no version counter behind the
        // rows (they are re-derived each republish), so the key is a digest of
        // what they say — cheap against a per-frame Lua conversion of seven
        // strings per file.
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
        set(&table, "plugins", inventory_value)?;
        set(&table, "ui_dir", to_lua_string(&self.lua, ui_dir)?)?;

        // The machine this is running on, so a plugin delivering more than one
        // build can choose between them itself.
        //
        // Published rather than expressed in a package manifest, deliberately: a
        // substitution template states one rule, while a pane that can read this
        // states every rule it actually needs — prefer a binary already on `PATH`,
        // fall back to a portable build, distinguish a libc variant, or say politely
        // that it has nothing for you. The kernel models none of that.
        // Constant for the life of the process, so it is built once and then
        // handed back — an all-zero key never moves.
        let platform = self.group("platform", [0, 0, 0, 0], || build_platform(&self.lua))?;
        set(&table, "platform", platform)?;

        // So a plugin can say "open" or "copy" before you press it.
        set(&table, "can_open_links", *can_open)?;
        set(
            &table,
            "error",
            opt_lua_string(&self.lua, snapshot.error.as_deref())?,
        )?;

        *self.epoch.borrow_mut() = Some(*epoch);
        set(&self.lua.globals(), "thurbox", table)
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
            ("tasks", flags.tasks),
            ("automations", flags.automations),
            ("file_viewer", flags.file_viewer),
            ("global_search", flags.global_search),
            ("info_panel", flags.info_panel),
            ("shell_pane", flags.shell_pane),
            ("code_review", flags.code_review),
            ("perf_hud", flags.perf_hud),
            ("mouse", flags.mouse),
            ("notifications", flags.notifications),
            ("soft_delete", flags.soft_delete),
            ("version_check", flags.version_check),
            ("auto_update", flags.auto_update),
        ] {
            features.set(name, value).map_err(|e| e.to_string())?;
        }

        let published = self.lua.create_table().map_err(|e| e.to_string())?;
        published
            .set("features", features)
            .map_err(|e| e.to_string())?;
        // The two the arrangement needs, and the one a terminal pane may want to
        // show. The notification knobs are deliberately absent: nothing a plugin
        // draws depends on them.
        published
            .set("two_panel_min_cols", settings.two_panel_min_cols)
            .map_err(|e| e.to_string())?;
        published
            .set("three_panel_min_cols", settings.three_panel_min_cols)
            .map_err(|e| e.to_string())?;
        published
            .set("scrollback_lines", settings.scrollback_lines)
            .map_err(|e| e.to_string())?;
        table
            .set("settings", published)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Publish the creation flow's three parameterised reads.
    ///
    /// Only what was asked for this frame: a flow that is closed asks nothing,
    /// so all three are empty tables. Each carries its own request back
    /// (`host`, `dir`, `repo`) so a plugin can tell an answer to its current
    /// question from one still in flight for the previous.
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
            &b"branches"[..],
            wants
                .branches
                .as_ref()
                .map(|(host, repo)| format!("{host}\0{repo}"))
                .unwrap_or_else(|| "\x01none".to_string())
                .as_bytes(),
        ]);
        let branches_value = self.group("branches", [epoch.data, branches_key, 0, 0], || {
            self.build_branches(store, wants).map(Value::Table)
        })?;
        set(table, "branches", branches_value)?;

        Ok(())
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
                // True only for a row the flow offers on its own account. It
                // leads the list by construction, so a plugin reading the list
                // as most-recent-first has to be able to step over it.
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
            browse.set("dir", dir.clone()).map_err(|e| e.to_string())?;
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

    fn build_branches(
        &self,
        store: &crate::kernel::repos::RepoStore,
        wants: &crate::kernel::repos::Wants,
    ) -> Result<Table, String> {
        use crate::kernel::repos::Branches;

        let branches = self.lua.create_table().map_err(|e| e.to_string())?;
        if let Some((host, repo)) = &wants.branches {
            branches
                .set("host", host.clone())
                .map_err(|e| e.to_string())?;
            branches
                .set("repo", repo.clone())
                .map_err(|e| e.to_string())?;
            let known = store.branches(host, repo);
            branches
                .set("loading", matches!(known, None | Some(Branches::Pending)))
                .map_err(|e| e.to_string())?;
            branches
                .set(
                    "error",
                    match known {
                        Some(Branches::Failed(message)) => to_lua_string(&self.lua, message)?,
                        _ => Value::Nil,
                    },
                )
                .map_err(|e| e.to_string())?;
            let list = self.lua.create_table().map_err(|e| e.to_string())?;
            if let Some(Branches::Ready(names)) = known {
                for (index, name) in names.iter().enumerate() {
                    list.set(index + 1, name.clone())
                        .map_err(|e| e.to_string())?;
                }
            }
            branches.set("list", list).map_err(|e| e.to_string())?;
        }
        Ok(branches)
    }
}

fn build_sessions(
    lua: &Lua,
    snapshot: &Snapshot,
    attach_errors: &HashMap<String, String>,
    meta: &HashMap<String, crate::kernel::terminal::AgentMeta>,
) -> Result<Value, String> {
    let sessions = lua.create_table().map_err(|e| e.to_string())?;
    for (index, row) in snapshot.sessions.iter().enumerate() {
        // Pre-sized: this row takes ~28 named fields, and a table grown one
        // field at a time rehashes itself on the way there — per session,
        // per frame.
        let entry = lua
            .create_table_with_capacity(0, 30)
            .map_err(|e| e.to_string())?;
        set(&entry, "id", to_lua_string(lua, &row.id)?)?;
        set(&entry, "name", to_lua_string(lua, &row.name)?)?;
        set(&entry, "agent", to_lua_string(lua, &row.agent)?)?;
        let attach_error = attach_errors.get(&row.id).map(String::as_str);
        set(
            &entry,
            "status",
            to_lua_string(
                lua,
                &crate::kernel::snapshot::with_reachability(
                    &row.status,
                    &row.backend,
                    attach_error,
                ),
            )?,
        )?;
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
        // Manual position. Without this a plugin cannot honour the order a
        // reorder command just wrote, and the list silently ignores it.
        set(&entry, "display_order", row.display_order)?;
        // Absent means "not computed yet", which a plugin must be able to
        // tell apart from a clean tree.
        if let Some(git) = row.git {
            let stats = lua.create_table().map_err(|e| e.to_string())?;
            set(&stats, "files", git.files_changed)?;
            set(&stats, "insertions", git.insertions)?;
            set(&stats, "deletions", git.deletions)?;
            set(&stats, "untracked", git.untracked)?;
            set(&stats, "dirty", git.dirty)?;
            set(&stats, "ahead", git.ahead)?;
            set(&stats, "behind", git.behind)?;
            set(&entry, "git", stats)?;
        }
        // Why this session's terminal is not live, when it is not.
        set(&entry, "attach_error", opt_lua_string(lua, attach_error)?)?;
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

fn build_agents(lua: &Lua, snapshot: &Snapshot) -> Result<Value, String> {
    let agents = lua.create_table().map_err(|e| e.to_string())?;
    for (index, agent) in snapshot.agents.iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "name", agent.name.clone())?;
        // v1's picker labels a row `name  (command)` when the two differ, so
        // the command travels with the name.
        set(&item, "command", agent.command.clone())?;
        agents.raw_set(index + 1, item).map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(agents))
}

fn build_hosts(lua: &Lua, snapshot: &Snapshot) -> Result<Value, String> {
    let hosts = lua.create_table().map_err(|e| e.to_string())?;
    for (index, host) in snapshot.hosts.iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "name", host.name.clone())?;
        set(&item, "detail", host.detail.clone())?;
        set(&item, "backend", host.backend.clone())?;
        hosts.raw_set(index + 1, item).map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(hosts))
}

fn build_deleted(lua: &Lua, snapshot: &Snapshot) -> Result<Value, String> {
    let deleted = lua.create_table().map_err(|e| e.to_string())?;
    for (index, row) in snapshot.deleted.iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "id", row.id.clone())?;
        set(&item, "name", row.name.clone())?;
        set(&item, "agent", row.agent.clone())?;
        // Epoch millis, so `widgets.time_ago` can measure it against
        // `taken_at_ms` — the instant these rows were read.
        set(&item, "deleted_at", row.deleted_at)?;
        set(&item, "worktrees", row.worktrees)?;
        // Restoring this one recovers committed work only.
        set(&item, "partial", row.partial)?;
        deleted
            .raw_set(index + 1, item)
            .map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(deleted))
}

fn build_tasks(lua: &Lua, snapshot: &Snapshot) -> Result<Value, String> {
    let tasks = lua.create_table().map_err(|e| e.to_string())?;
    for (index, row) in snapshot.tasks.iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "id", row.id)?;
        set(&item, "title", row.title.clone())?;
        set(
            &item,
            "description",
            opt_lua_string(lua, row.description.as_deref())?,
        )?;
        set(&item, "status", row.status.clone())?;
        set(&item, "source", row.source.clone())?;
        set(
            &item,
            "url",
            opt_lua_string(lua, row.external_url.as_deref())?,
        )?;
        // Epoch seconds, so the detail view can date a task.
        set(&item, "created_at", row.created_at)?;
        set(&item, "updated_at", row.updated_at)?;
        tasks.raw_set(index + 1, item).map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(tasks))
}

fn build_automations(lua: &Lua, snapshot: &Snapshot) -> Result<Value, String> {
    let automations = lua.create_table().map_err(|e| e.to_string())?;
    for (index, row) in snapshot.automations.iter().enumerate() {
        let item = lua.create_table().map_err(|e| e.to_string())?;
        set(&item, "id", row.id)?;
        set(&item, "name", row.name.clone())?;
        set(&item, "schedule", row.schedule.clone())?;
        set(&item, "action", row.action.clone())?;
        set(&item, "enabled", row.enabled)?;
        set(
            &item,
            "last_outcome",
            opt_lua_string(lua, row.last_outcome.as_deref())?,
        )?;
        set(
            &item,
            "last_detail",
            opt_lua_string(lua, row.last_detail.as_deref())?,
        )?;
        // The whole history, not just the last outcome: v1's run-history
        // pane lists every recent run, and a plugin cannot reconstruct
        // those from `last_outcome` alone.
        let runs = lua.create_table().map_err(|e| e.to_string())?;
        for (position, run) in row.runs.iter().enumerate() {
            let entry = lua.create_table().map_err(|e| e.to_string())?;
            set(&entry, "started_at", run.started_at)?;
            set(&entry, "status", run.status.clone())?;
            set(&entry, "detail", run.detail.clone())?;
            runs.raw_set(position + 1, entry)
                .map_err(|e| e.to_string())?;
        }
        set(&item, "runs", runs)?;
        automations
            .raw_set(index + 1, item)
            .map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(automations))
}

fn build_theme(lua: &Lua, themes: &Themes) -> Result<Value, String> {
    let roles = lua.create_table().map_err(|e| e.to_string())?;
    for (role, colour) in themes.roles() {
        set(&roles, role, colour)?;
    }
    let theme = lua.create_table().map_err(|e| e.to_string())?;
    set(&theme, "name", themes.active_name())?;
    set(&theme, "roles", roles)?;

    // The selectable list, so a picker can be an ordinary plugin.
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
        set(&item, "plugin", binding.plugin.clone())?;
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
        set(&item, "plugin", setting.plugin.clone())?;
        set(&item, "id", setting.id.clone())?;
        set(&item, "desc", setting.description.clone())?;
        set(&item, "type", setting.value.type_name())?;
        set_value(&item, "value", &setting.value)?;
        set_value(&item, "default", &setting.default)?;
        settings
            .raw_set(index + 1, item)
            .map_err(|e| e.to_string())?;
    }
    let reg = lua.create_table().map_err(|e| e.to_string())?;
    set(&reg, "keys", keys)?;
    set(&reg, "settings", settings)?;
    // The section order help renders in, so a plugin choosing a `group` can
    // see where it will land without hardcoding the list.
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

fn build_content(lua: &Lua, content: &HashMap<String, String>) -> Result<Value, String> {
    let content_table = lua.create_table().map_err(|e| e.to_string())?;
    for (session, text) in content.iter() {
        content_table
            .raw_set(session.clone(), text.clone())
            .map_err(|e| e.to_string())?;
    }
    Ok(Value::Table(content_table))
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
        if let Some(usage) = metrics.usage(&row.agent, row.remote_host.as_deref()) {
            if !usage.is_empty() {
                set(&entry, "usage", usage_table(lua, usage)?)?;
                any = true;
            }
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

/// One run as a plugin reads it.
///
/// `state` is the field to branch on — `pending`, `done` or `failed` — because a
/// pane must be able to tell a program that has not answered from one that
/// answered with nothing.
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

/// Account rate-limit windows as a Lua table.
fn usage_table(lua: &Lua, usage: &crate::session::AgentUsage) -> Result<Table, String> {
    let table = lua.create_table().map_err(|e| e.to_string())?;
    let windows = lua.create_table().map_err(|e| e.to_string())?;
    for (index, window) in usage.windows.iter().enumerate() {
        let entry = lua.create_table().map_err(|e| e.to_string())?;
        entry
            .set("label", to_lua_string(lua, &window.label)?)
            .map_err(|e| e.to_string())?;
        entry
            .set("used_percent", window.used_percent)
            .map_err(|e| e.to_string())?;
        if let Some(resets_at) = window.resets_at {
            entry
                .set("resets_at", resets_at)
                .map_err(|e| e.to_string())?;
        }
        windows.set(index + 1, entry).map_err(|e| e.to_string())?;
    }
    table.set("windows", windows).map_err(|e| e.to_string())?;
    if let Some(plan) = &usage.plan {
        table
            .set("plan", to_lua_string(lua, plan)?)
            .map_err(|e| e.to_string())?;
    }
    if let Some(note) = &usage.note {
        table
            .set("note", to_lua_string(lua, note)?)
            .map_err(|e| e.to_string())?;
    }
    Ok(table)
}
