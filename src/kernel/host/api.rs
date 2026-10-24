use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mlua::{Lua, Table, Value};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::load::{BUDGET_EXCEEDED, BUDGET_UNARMED};
use super::{
    instruction_budget, memory_limit, Persisted, Private, Queue, Roots, Shared, StateVersion,
};
use crate::kernel::command::{Args, Command, ExtraMember};
use crate::kernel::events::Field;

/// Install what every plugin shares into a fresh VM: `store`, `text`, the
/// clock, and the sealed standard library.
///
/// What is bound to one plugin — `command`, `run`, `run_forget`, `files`, `state`, `require`
/// — is not here. It goes into that plugin's own environment
/// ([`plugin_env`]), because every plugin runs in this one Lua state.
pub(super) fn install_api(
    lua: &Lua,
    store: Shared,
    state_version: StateVersion,
    clock: Rc<std::cell::Cell<f64>>,
    clock_read: Rc<std::cell::Cell<bool>>,
    sample_read: Rc<std::cell::Cell<u8>>,
    store_writes: super::WriteCount,
) -> mlua::Result<()> {
    scrub_globals(lua)?;
    install_store(lua, "store", store, state_version, store_writes, None)?;
    install_store_writer(lua)?;
    install_clock(lua, clock, clock_read)?;
    install_sample_reads(lua, sample_read)?;
    install_text(lua)?;
    seal_shared(lua)?;
    Ok(())
}

/// What a plugin's own environment is built from: the host's handles, shared by
/// every environment, which each one binds to its own plugin.
#[derive(Clone)]
pub(super) struct EnvParts {
    pub ui_dir: PathBuf,
    pub state: Private,
    pub queue: Queue,
    pub roots: Roots,
    pub runs: Rc<RefCell<Vec<(String, crate::kernel::runs::Request)>>>,
    pub version: StateVersion,
    pub writes: super::WriteCount,
    /// The plugin whose request a handler is running for, while one is: see
    /// `LuaHost::acting_for`.
    pub behalf: Rc<RefCell<Option<Vec<String>>>>,
    /// The path of the plugin the kernel is calling into, which is the
    /// handler `behalf` applies to.
    pub entered: Rc<RefCell<String>>,
}

/// One plugin's environment, and the capability it holds only while granted.
pub(super) struct Env {
    /// The `_ENV` its chunk, its modules and every function they define run in.
    pub table: Table,
    /// `run`, bound to this plugin. Kept out of `table` until the plugin is
    /// granted it — `LuaHost::enter` puts it there per call.
    pub run: mlua::Function,
    pub run_forget: mlua::Function,
    /// `files`, kept out of `table` the same way until granted.
    pub files: Table,
    /// This plugin's `thurbox`: the published data, read through, plus the
    /// values that are this plugin's alone (`runs`, `granted`), which
    /// `LuaHost::enter` sets here and nowhere every plugin reads.
    pub thurbox: Table,
}

/// Build the environment the plugin at `path` runs in.
///
/// Every plugin shares one Lua state, so "the plugin running now" is not who
/// issued a call: a pane that replaced a global another pane calls would run
/// its own code inside that pane's call, with its grant. So nothing here asks
/// who is running. Each plugin's chunk gets its own `_ENV`, and the functions
/// that act for a plugin — `command`, `run`, `run_forget`, `files`, `state`, `require` — are created
/// for it, with its identity bound in. Code a plugin defines resolves those
/// names through its own environment wherever it is called from, so a
/// hijacking function acts as its author and nobody else.
///
/// Reads fall through to the shared globals, which hold only what every plugin
/// may have; the library tables and `store` are shadowed by views of the
/// plugin's own ([`seal_shared`]). Writes land in the plugin's own table. `_G`
/// is that table, and its metatable is sealed, so the shared one is not
/// reachable to write to. `ns` is the key `state` is kept under.
pub(super) fn plugin_env(lua: &Lua, parts: &EnvParts, path: &str, ns: &str) -> mlua::Result<Env> {
    let table = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.set("__index", lua.globals())?;
    meta.set("__metatable", false)?;
    table.set_metatable(Some(meta))?;
    table.set("_G", table.clone())?;
    sealed_views(lua, &table, path, parts)?;
    table.set(
        "command",
        command_for(
            lua,
            parts.queue.clone(),
            path.to_string(),
            parts.behalf.clone(),
            parts.entered.clone(),
        )?,
    )?;
    table.set(
        "state",
        state_for(
            lua,
            parts.state.clone(),
            ns.to_string(),
            parts.version.clone(),
            parts.writes.clone(),
        )?,
    )?;
    table.set("require", require_for(lua, &parts.ui_dir, table.clone())?)?;
    let thurbox = thurbox_view(lua)?;
    table.raw_set("thurbox", thurbox.clone())?;
    Ok(Env {
        thurbox,
        run: run_for(lua, parts.runs.clone(), path.to_string())?,
        run_forget: forget_for(lua, parts.runs.clone(), path.to_string())?,
        files: files_for(lua, parts.roots.clone())?,
        table,
    })
}

/// One plugin's `thurbox`: reads fall through to whatever is published now,
/// a write is refused, and the fields only this plugin may see are its own.
///
/// Per plugin because what the kernel sets per call — a plugin's own run
/// answers, its grants — must never sit in a table another plugin's code can
/// read: a finalizer runs inside whichever call the collector interrupts, and
/// it reads through its own environment, so it finds only its own plugin's.
fn thurbox_view(lua: &Lua) -> mlua::Result<Table> {
    let view = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.raw_set(
        "__index",
        lua.create_function(|lua, (_, key): (Value, Value)| {
            match lua.globals().get::<Value>("thurbox")? {
                Value::Table(published) => published.get::<Value>(key),
                _ => Ok(Value::Nil),
            }
        })?,
    )?;
    meta.raw_set(
        "__newindex",
        lua.create_function(|_, (_, key): (Value, Value)| -> mlua::Result<()> {
            Err(mlua::Error::runtime(format!(
                "thurbox is published by the kernel and cannot be changed (assigning {})",
                key.to_string().unwrap_or_default()
            )))
        })?,
    )?;
    meta.raw_set(
        "__pairs",
        lua.create_function(|lua, _: Value| {
            let pairs: mlua::Function = lua.globals().get("pairs")?;
            match lua.globals().get::<Value>("thurbox")? {
                Value::Table(published) => pairs.call::<mlua::MultiValue>(published),
                _ => pairs.call::<mlua::MultiValue>(lua.create_table()?),
            }
        })?,
    )?;
    meta.raw_set("__metatable", false)?;
    view.set_metatable(Some(meta))?;
    let views: Table = lua.named_registry_value(VIEWS)?;
    views.raw_set(view.clone(), true)?;
    Ok(view)
}

/// Make the shared library tables read-only, by building the metatable each
/// plugin's view of them carries ([`sealed_views`]).
///
/// A plugin's own `_ENV` keeps a replaced global to itself, but `string.upper =
/// …` writes into a table every plugin reads — and so does a string method,
/// through the one metatable all strings share. So no plugin is handed the real
/// table: each gets an empty proxy of its own that reads through and refuses a
/// write. Of its own, because `rawset` writes past `__newindex` into the proxy
/// itself, and a proxy every plugin shared would carry that write to all of them.
/// The real tables stay in the shared globals, which no plugin can reach, and
/// the string metatable is sealed so `getmetatable("")` cannot hand one out.
///
/// Nothing on a view leads back to the table behind it: `__index` is followed
/// by Lua and never returned, `__pairs` iterates through a closure rather than
/// returning `next` and the table as the iteration state, and `__metatable`
/// hides the metatable itself.
fn seal_shared(lua: &Lua) -> mlua::Result<()> {
    lua.load("getmetatable('').__metatable = false")
        .set_name("seal")
        .exec()?;
    let globals = lua.globals();
    let metas = lua.create_table()?;
    for name in SEALED {
        let Value::Table(inner) = globals.get::<Value>(name)? else {
            continue;
        };
        metas.set(name, read_only_meta(lua, inner, name)?)?;
    }
    lua.set_named_registry_value(SEALED_METAS, metas)
}

/// Where [`seal_shared`] keeps the metatable of each sealed library table, out
/// of every plugin's reach.
const SEALED_METAS: &str = "__sealed_metas";

/// One plugin's views of the shared tables: a fresh empty table per sealed
/// library carrying the shared, sealed metatable, and one for `store`.
fn sealed_views(lua: &Lua, env: &Table, path: &str, parts: &EnvParts) -> mlua::Result<()> {
    let metas: Table = lua.named_registry_value(SEALED_METAS)?;
    for name in SEALED {
        if let Ok(Value::Table(meta)) = metas.get::<Value>(name) {
            let view = lua.create_table()?;
            view.set_metatable(Some(meta))?;
            env.raw_set(name, view)?;
        }
    }
    env.raw_set("store", store_view(lua, path, parts)?)
}

/// One plugin's view of `store`: reads what every plugin reads, and records
/// this plugin as the last writer of each key it writes ([`STORE_WRITERS`]) —
/// or, writing for another plugin's request, that request's chain
/// ([`requester`]), so the pane a request came from is the one named.
///
/// The record is made here, by a function built for this plugin, so it cannot
/// be claimed for another: a `rawset` on the view lands in the plugin's own
/// table and never reaches `store`.
/// Who a request from the plugin at `owner` is made by: the chain of plugins
/// it passed through, the one that first asked first.
///
/// A handler running at another plugin's request (`behalf`, while `entered` is
/// that handler) adds itself to that request's chain, and the gate allows the
/// request only with what every plugin in it holds. Anything else — the
/// handler's key the user pressed, another plugin's finalizer the collector
/// runs meanwhile — is its author's own request.
pub(super) fn requester(
    behalf: &RefCell<Option<Vec<String>>>,
    entered: &RefCell<String>,
    owner: &str,
) -> Vec<String> {
    match behalf.borrow().as_ref() {
        Some(chain) if *entered.borrow() == owner => {
            let mut chain = chain.clone();
            if chain.last().map(String::as_str) != Some(owner) {
                chain.push(owner.to_string());
            }
            chain
        }
        _ => vec![owner.to_string()],
    }
}

fn store_view(lua: &Lua, path: &str, parts: &EnvParts) -> mlua::Result<Table> {
    let shared: Table = lua.globals().get("store")?;
    let Some(shared) = shared.metatable() else {
        return Err(mlua::Error::runtime("store has no metatable"));
    };
    let write: mlua::Function = shared.get("__newindex")?;
    let writers: Table = lua.named_registry_value(STORE_WRITERS)?;
    let owner = path.to_string();
    let (behalf, entered) = (parts.behalf.clone(), parts.entered.clone());
    let meta = lua.create_table()?;
    meta.set("__index", shared.get::<Value>("__index")?)?;
    meta.set(
        "__newindex",
        lua.create_function(move |_, (view, key, value): (Table, String, Value)| {
            write.call::<()>((view, key.clone(), value))?;
            writers.raw_set(key, requester(&behalf, &entered, &owner).join(" via "))
        })?,
    )?;
    meta.set("__metatable", false)?;
    let view = lua.create_table()?;
    view.set_metatable(Some(meta))?;
    Ok(view)
}

/// Who last wrote each `store` key, by plugin path: what `store_writer` reads.
const STORE_WRITERS: &str = "__store_writers";

/// `store_writer(key)` — the path of the plugin that last wrote `store[key]`,
/// or nil when nothing has, or the kernel or `layout.lua` did.
///
/// `store` is how one pane hands another a request, and the one acting on it
/// may need to say whose request it is: the shipped confirmation names the
/// pane that asked, because the question it was handed is that pane's words.
fn install_store_writer(lua: &Lua) -> mlua::Result<()> {
    let writers = lua.create_table()?;
    lua.set_named_registry_value(STORE_WRITERS, writers.clone())?;
    lua.globals().set(
        "store_writer",
        lua.create_function(move |_, key: String| {
            Ok(writers
                .raw_get::<Option<String>>(key)?
                .filter(|path| !path.is_empty()))
        })?,
    )
}

/// The library tables every plugin reads, sealed by [`seal_shared`].
const SEALED: [&str; 6] = ["string", "table", "math", "coroutine", "utf8", "text"];

pub(super) const READ_TAKEN_AT: u8 = 1;
pub(super) const READ_METRICS: u8 = 2;
/// Not a cache key: read by the loop, which fetches a remote session's diff
/// only while some pane drawn on screen reads `thurbox.diffs`.
pub(super) const READ_DIFFS: u8 = 4;
/// What iterating the published table reads: all of it.
const READ_ALL: u8 = READ_TAKEN_AT | READ_METRICS | READ_DIFFS;
const PUBLISHED_TABLES: &str = "__published_tables";

fn note_sample_read(read: &std::cell::Cell<u8>, key: &Value) {
    let flag = match key.as_string() {
        Some(key) if key == "taken_at_ms" => READ_TAKEN_AT,
        Some(key) if key == "metrics" => READ_METRICS,
        Some(key) if key == "diffs" => READ_DIFFS,
        _ => 0,
    };
    read.set(read.get() | flag);
}

/// Preserve raw reads and iteration of the published table while observing
/// the volatile inputs. Weak keys let a plugin retain a snapshot without
/// retaining every later publication in the VM.
///
/// `rawget` and `next` read through to what a published proxy stands for —
/// the top-level table ([`PUBLISHED_TABLES`]) or any table under it
/// ([`FROZEN`]) — so code written against plain tables keeps working. There
/// is no `rawset` to route: it is withheld, being the one way past a
/// metatable's refusal to write.
fn install_sample_reads(lua: &Lua, read: Rc<std::cell::Cell<u8>>) -> mlua::Result<()> {
    let weak = || -> mlua::Result<Table> {
        let table = lua.create_table()?;
        let meta = lua.create_table()?;
        meta.raw_set("__mode", "k")?;
        table.set_metatable(Some(meta))?;
        Ok(table)
    };
    let tables = weak()?;
    lua.set_named_registry_value(PUBLISHED_TABLES, tables.clone())?;
    let frozen = weak()?;
    lua.set_named_registry_value(FROZEN, frozen.clone())?;
    let views = weak()?;
    lua.set_named_registry_value(VIEWS, views.clone())?;
    // A plugin's `thurbox` view reads as the table published now.
    let published = |lua: &Lua, tables: &Table| -> mlua::Result<Option<Table>> {
        match lua.globals().get::<Value>("thurbox")? {
            Value::Table(proxy) => tables.raw_get::<Option<Table>>(proxy),
            _ => Ok(None),
        }
    };

    let original_get: mlua::Function = lua.globals().get("rawget")?;
    let (map, nested, plugin_views) = (tables.clone(), frozen.clone(), views.clone());
    let reads = read.clone();
    lua.globals().set(
        "rawget",
        lua.create_function(move |lua, (table, key): (Table, Value)| {
            if plugin_views.raw_get::<bool>(table.clone()).unwrap_or(false) {
                let own = table.raw_get::<Value>(key.clone())?;
                if !own.is_nil() {
                    return Ok(own);
                }
                note_sample_read(&reads, &key);
                return match published(lua, &map)? {
                    Some(backing) => backing.raw_get::<Value>(key),
                    None => Ok(Value::Nil),
                };
            }
            if let Some(backing) = map.raw_get::<Option<Table>>(table.clone())? {
                note_sample_read(&reads, &key);
                return backing.raw_get::<Value>(key);
            }
            if let Some(inner) = nested.raw_get::<Option<Table>>(table.clone())? {
                return inner.raw_get::<Value>(key);
            }
            original_get.call::<Value>((table, key))
        })?,
    )?;

    let original_next: mlua::Function = lua.globals().get("next")?;
    lua.globals().set(
        "next",
        lua.create_function(move |lua, (table, key): (Table, Value)| {
            let table = if views.raw_get::<bool>(table.clone()).unwrap_or(false) {
                read.set(read.get() | READ_ALL);
                match published(lua, &tables)? {
                    Some(backing) => backing,
                    None => return Ok(mlua::MultiValue::new()),
                }
            } else if let Some(backing) = tables.raw_get::<Option<Table>>(table.clone())? {
                read.set(read.get() | READ_ALL);
                backing
            } else if let Some(inner) = frozen.raw_get::<Option<Table>>(table.clone())? {
                inner
            } else {
                table
            };
            original_next.call::<mlua::MultiValue>((table, key))
        })?,
    )?;
    install_frozen_meta(lua)
}

/// The metamethods every [`freeze`] proxy shares, so a published table costs
/// one metatable rather than three Rust closures.
///
/// Each finds the table behind its proxy through [`FROZEN`] instead of
/// capturing it. A captured table is a registry reference — a GC root the
/// collector cannot see through — so a proxy could only free what it stood for
/// one full collection after the proxy itself went, one nesting level per
/// collection. A deep published group rebuilt every pass then outran the
/// collector, and the emergency collection at the memory ceiling, which runs
/// once, freed one level and failed the allocation anyway.
fn install_frozen_meta(lua: &Lua) -> mlua::Result<()> {
    let shared = lua.create_table()?;
    shared.raw_set(
        "__newindex",
        lua.create_function(|_, (_, key): (Value, Value)| -> mlua::Result<()> {
            Err(mlua::Error::runtime(format!(
                "thurbox is shared by every plugin and cannot be changed (assigning {})",
                key.to_string().unwrap_or_default()
            )))
        })?,
    )?;
    shared.raw_set(
        "__len",
        lua.create_function(|lua, (proxy, _): (Table, Value)| {
            let frozen: Table = lua.named_registry_value(FROZEN)?;
            Ok(frozen
                .raw_get::<Option<Table>>(proxy)?
                .map_or(0, |inner| inner.raw_len()))
        })?,
    )?;
    // The proxy itself is the iteration state: the `next` installed above
    // reads a frozen proxy as what it stands for, so the table behind it is
    // never handed out.
    shared.raw_set(
        "__pairs",
        lua.create_function(|lua, proxy: Table| {
            let next: mlua::Function = lua.globals().get("next")?;
            Ok((next, proxy, Value::Nil))
        })?,
    )?;
    lua.set_named_registry_value(FROZEN_META, shared)
}

/// Where [`install_frozen_meta`] keeps the shared metamethods.
const FROZEN_META: &str = "__frozen_meta";

/// Every plugin's `thurbox` view ([`thurbox_view`]), so `rawget` and `next`
/// read one as the table published now.
const VIEWS: &str = "__thurbox_views";

/// Every read-only proxy [`freeze`] made, mapped to the table it stands for.
const FROZEN: &str = "__frozen";

/// The metatable of a read-only view of `inner`: reads through, refuses a
/// write naming `what`, iterates without handing out `inner`, and hides
/// itself.
fn read_only_meta(lua: &Lua, inner: Table, what: &str) -> mlua::Result<Table> {
    let meta = lua.create_table()?;
    meta.raw_set("__index", inner.clone())?;
    let what = what.to_string();
    meta.raw_set(
        "__newindex",
        lua.create_function(move |_, (_, key): (Value, Value)| -> mlua::Result<()> {
            Err(mlua::Error::runtime(format!(
                "{what} is shared by every plugin and cannot be changed (assigning {})",
                key.to_string().unwrap_or_default()
            )))
        })?,
    )?;
    let length = inner.clone();
    meta.raw_set(
        "__len",
        lua.create_function(move |_, _: Value| Ok(length.raw_len()))?,
    )?;
    let next: mlua::Function = lua.globals().get("next")?;
    meta.raw_set(
        "__pairs",
        lua.create_function(move |lua, _: Value| {
            let (walk, inner) = (next.clone(), inner.clone());
            let step = lua.create_function(move |_, (_, key): (Value, Value)| {
                walk.call::<mlua::MultiValue>((inner.clone(), key))
            })?;
            Ok((step, Value::Nil, Value::Nil))
        })?,
    )?;
    meta.raw_set("__metatable", false)?;
    Ok(meta)
}

/// Make published data read-only: `value`, and every table under it, is
/// replaced by a proxy that reads through and refuses a write.
///
/// The published tables are one set every plugin reads, so a write into one —
/// a session's id a trusted pane builds a command from — would be one pane
/// steering another. The proxies are shared, which is safe only because
/// `rawset` is withheld: nothing else writes past `__newindex`. A table already
/// frozen is returned as it is, so a cached group is not wrapped twice.
pub(super) fn freeze(lua: &Lua, value: Value) -> mlua::Result<Value> {
    let Value::Table(table) = value else {
        return Ok(value);
    };
    let frozen: Table = lua.named_registry_value(FROZEN)?;
    if frozen.raw_get::<Option<Table>>(table.clone())?.is_some() {
        return Ok(Value::Table(table));
    }
    freeze_fields(lua, &table)?;
    let shared: Table = lua.named_registry_value(FROZEN_META)?;
    let meta = lua.create_table()?;
    for name in ["__newindex", "__len", "__pairs"] {
        meta.raw_set(name, shared.raw_get::<Value>(name)?)?;
    }
    meta.raw_set("__index", table.clone())?;
    meta.raw_set("__metatable", false)?;
    let proxy = lua.create_table()?;
    proxy.set_metatable(Some(meta))?;
    frozen.raw_set(proxy.clone(), table)?;
    Ok(Value::Table(proxy))
}

/// [`freeze`] every table held by `table`, leaving `table` itself writable —
/// for the top-level published table, which the kernel still fills.
pub(super) fn freeze_fields(lua: &Lua, table: &Table) -> mlua::Result<()> {
    let entries = table
        .pairs::<Value, Value>()
        .collect::<mlua::Result<Vec<_>>>()?;
    for (key, value) in entries {
        if matches!(value, Value::Table(_)) {
            table.raw_set(key, freeze(lua, value)?)?;
        }
    }
    Ok(())
}

/// The table behind the published `thurbox` proxy, for the kernel's own
/// in-place patches — plugins cannot write there.
pub(super) fn published_backing(lua: &Lua) -> Option<Table> {
    let proxy: Table = lua.globals().get("thurbox").ok()?;
    let tables: Table = lua.named_registry_value(PUBLISHED_TABLES).ok()?;
    tables.raw_get::<Option<Table>>(proxy).ok().flatten()
}

pub(super) fn observe_samples(
    lua: &Lua,
    backing: Table,
    read: Rc<std::cell::Cell<u8>>,
) -> mlua::Result<Table> {
    let proxy = lua.create_table()?;
    let tables: Table = lua.named_registry_value(PUBLISHED_TABLES)?;
    tables.raw_set(proxy.clone(), backing)?;
    // Neither closure captures `backing`: each finds it through
    // `PUBLISHED_TABLES`, for the reason `install_frozen_meta` gives.
    let meta = lua.create_table()?;
    let reads = read.clone();
    meta.raw_set(
        "__index",
        lua.create_function(move |lua, (proxy, key): (Table, Value)| {
            note_sample_read(&reads, &key);
            let tables: Table = lua.named_registry_value(PUBLISHED_TABLES)?;
            match tables.raw_get::<Option<Table>>(proxy)? {
                Some(backing) => backing.raw_get::<Value>(key),
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    meta.raw_set(
        "__newindex",
        lua.create_function(move |_, (_, key): (Table, Value)| -> mlua::Result<()> {
            Err(mlua::Error::runtime(format!(
                "thurbox is published by the kernel and cannot be changed (assigning {})",
                key.to_string().unwrap_or_default()
            )))
        })?,
    )?;
    // The proxy is the iteration state: the installed `next` reads it as the
    // backing table and notes the read, so the table itself is never handed
    // out.
    meta.raw_set(
        "__pairs",
        lua.create_function(move |lua, proxy: Table| {
            read.set(read.get() | READ_ALL);
            let next: mlua::Function = lua.globals().get("next")?;
            Ok((next, proxy, Value::Nil))
        })?,
    )?;
    meta.raw_set("__metatable", false)?;
    proxy.set_metatable(Some(meta))?;
    Ok(proxy)
}

/// The registry name of the metatable every render context carries.
///
/// See [`install_clock`]. In the registry rather than in globals because the
/// shared globals are every plugin's fallback `_ENV`, so anything parked there
/// is reachable by name from every plugin.
pub(super) const CTX_META: &str = "__ctx_meta";

/// Serve `ctx.elapsed` through a metatable, and record that it was asked for.
///
/// The animation clock is the one render input whose reader the kernel cannot
/// otherwise name, and it invalidates every pure pane eight times a second while
/// any agent is working. Making it a *lookup* rather than a field is what lets
/// the kernel see which panes actually depend on it — the coupling Textual gets
/// from `set_interval` on a widget and Bubble Tea from a spinner returning its
/// own tick. See `CachedTree`.
///
/// `__index` fires only for keys the table does not have, so every ordinary
/// field (`width`, `height`, `focused`, `frame`, `name`, `slot`) is still a raw
/// read and pays nothing. Built once per VM and shared by every render, because
/// a closure per render would cost more than the field it replaces.
///
/// The key is taken as a `Value` rather than a `String`: `__index` also fires
/// for `ctx[nil]` or `ctx[true]`, which Lua defines as nil, and coercing would
/// raise out of the index and fail the whole render instead.
///
/// One visible consequence: `elapsed` is not a key of the ctx table, so it does
/// not appear in `pairs(ctx)`. Nothing iterates a render context — a plugin asks
/// it for named facts — and the alternative is to give up knowing who reads the
/// clock.
///
/// `__metatable` seals it, because `getmetatable`/`setmetatable` are both in the
/// plugin sandbox and this one table is shared by every pane: without the seal a
/// plugin could replace the `__index` behind every other pane's clock, freezing
/// their spinners and making the read-detection above silently answer "no".
fn install_clock(
    lua: &Lua,
    clock: Rc<std::cell::Cell<f64>>,
    read: Rc<std::cell::Cell<bool>>,
) -> mlua::Result<()> {
    let meta = lua.create_table()?;
    meta.set(
        "__index",
        lua.create_function(move |_, (_, key): (Table, Value)| {
            if key.as_string().is_some_and(|key| key == "elapsed") {
                read.set(true);
                return Ok(Value::Number(clock.get()));
            }
            Ok(Value::Nil)
        })?,
    )?;
    meta.set("__metatable", false)?;
    lua.set_named_registry_value(CTX_META, meta)
}

/// Give one render context its clock.
pub(super) fn attach_clock(lua: &Lua, ctx: &Table) -> mlua::Result<()> {
    let meta: Table = lua.named_registry_value(CTX_META)?;
    ctx.set_metatable(Some(meta))
}

/// `run(key, program, opts)` — ask for a program and read the answer later.
///
/// Queued, never executed here: the whole point is that a plugin cannot call
/// anything that waits. Created for one plugin, with its path bound in, so a
/// run is attributed without the plugin naming itself (and without being able
/// to claim another's key).
fn run_for(
    lua: &Lua,
    runs: Rc<RefCell<Vec<(String, crate::kernel::runs::Request)>>>,
    owner: String,
) -> mlua::Result<mlua::Function> {
    lua.create_function(
        move |_, (key, program, opts): (String, String, Option<Table>)| {
            if key.is_empty() || program.is_empty() {
                return Err(mlua::Error::runtime(
                    "run(key, program): both a key and a program are required",
                ));
            }
            let seconds = |name: &str| -> Option<std::time::Duration> {
                opts.as_ref()
                    .and_then(|t| t.get::<Option<f64>>(name).ok().flatten())
                    .filter(|n| *n > 0.0)
                    .map(std::time::Duration::from_secs_f64)
            };
            let session_target = || {
                let session = match &opts {
                    Some(t) => t.get::<Value>("session")?,
                    None => Value::Nil,
                };
                let session = match session {
                    Value::String(s) if !s.as_bytes().is_empty() => s.to_string_lossy(),
                    _ => {
                        return Err(mlua::Error::runtime(
                            "run(key, program, opts): opts.session is required unless opts.machine is \"local\"",
                        ));
                    }
                };
                Ok(crate::kernel::runs::Target::Session(session))
            };
            // A fallback could run the program on a machine the pane did not
            // ask for, so invalid routing options are refused before queueing.
            let machine = match &opts {
                Some(t) => t.get::<Value>("machine")?,
                None => Value::Nil,
            };
            let target = match &machine {
                Value::Nil => session_target()?,
                Value::String(s) if s == "session" => session_target()?,
                Value::String(s) if s == "local" => crate::kernel::runs::Target::Local,
                Value::String(s) => {
                    let value = s.to_str()?;
                    let name = value.strip_prefix("host:")
                        .filter(|name| !name.is_empty() && !name.contains(':'))
                        .ok_or_else(|| mlua::Error::runtime(
                            "run: machine must be session, local or host:<exact-configured-name>"
                        ))?;
                    crate::kernel::runs::Target::Host(name.to_string())
                },
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "run: machine must be session, local or host:<exact-configured-name>, not a {}",
                        other.type_name()
                    )));
                }
            };
            let ask = crate::kernel::runs::Ask {
                key,
                program,
                target,
                ttl: seconds("ttl").unwrap_or(crate::kernel::runs::DEFAULT_TTL),
                timeout: seconds("timeout").unwrap_or(crate::kernel::runs::DEFAULT_TIMEOUT),
                refresh: opts
                    .as_ref()
                    .and_then(|t| t.get::<Option<bool>>("refresh").ok().flatten())
                    .unwrap_or(false),
            };
            runs.borrow_mut().push((owner.clone(), crate::kernel::runs::Request::Ask(ask)));
            Ok(())
        },
    )
}

fn forget_for(
    lua: &Lua,
    runs: Rc<RefCell<Vec<(String, crate::kernel::runs::Request)>>>,
    owner: String,
) -> mlua::Result<mlua::Function> {
    lua.create_function(move |_, key: Value| {
        let key = match key {
            Value::String(key) if !key.as_bytes().is_empty() => key.to_str()?.to_string(),
            _ => {
                return Err(mlua::Error::runtime(
                    "run_forget: a nonempty string key is required",
                ))
            }
        };
        runs.borrow_mut()
            .push((owner.clone(), crate::kernel::runs::Request::Forget(key)));
        Ok(())
    })
}

/// `files.list(session, path)` and `files.read(session, path)`.
///
/// Bounded twice. A plugin gets a directory listing and a file's text, both
/// rooted at that session's working directory and refusing anything outside
/// it — never a filesystem. And it gets even that only when granted `files`
/// (`LuaHost::enter`), because a worktree's `.env` is inside that root.
fn files_for(lua: &Lua, roots: Roots) -> mlua::Result<Table> {
    let files = lua.create_table()?;

    let list_roots = roots.clone();
    files.set(
        "list",
        lua.create_function(move |lua, (session, path): (String, Option<String>)| {
            let root = list_roots.borrow().get(&session).cloned().ok_or_else(|| {
                mlua::Error::runtime(format!("no directory for session {session}"))
            })?;
            let entries = crate::kernel::files::list(&root, path.as_deref().unwrap_or(""))
                .map_err(mlua::Error::runtime)?;
            let out = lua.create_table()?;
            for (index, entry) in entries.into_iter().enumerate() {
                let item = lua.create_table()?;
                item.set("name", entry.name)?;
                item.set("dir", entry.is_dir)?;
                out.set(index + 1, item)?;
            }
            Ok(out)
        })?,
    )?;

    let read_roots = roots;
    files.set(
        "read",
        lua.create_function(move |_, (session, path): (String, String)| {
            let root = read_roots.borrow().get(&session).cloned().ok_or_else(|| {
                mlua::Error::runtime(format!("no directory for session {session}"))
            })?;
            crate::kernel::files::read(&root, &path).map_err(mlua::Error::runtime)
        })?,
    )?;

    Ok(files)
}

/// `command("delete", { session = id })` — the only way a plugin changes state.
///
/// It enqueues and returns; it never runs the operation. That is the whole of
/// the write side, and it is why a plugin cannot stall the render loop with a
/// database write or an unreachable host.
///
/// A malformed command raises immediately, because the mistake is in the
/// plugin's own call and there is nothing to report asynchronously about.
fn command_for(
    lua: &Lua,
    queue: Queue,
    owner: String,
    behalf: Rc<RefCell<Option<Vec<String>>>>,
    entered: Rc<RefCell<String>>,
) -> mlua::Result<mlua::Function> {
    lua.create_function(move |lua, (kind, opts): (String, Option<Table>)| {
        let opts = opts;
        let get_string = |key: &str| -> Option<String> {
            opts.as_ref()
                .and_then(|t| t.get::<Option<String>>(key).ok().flatten())
        };
        let get_bool = |key: &str| -> Option<bool> {
            opts.as_ref()
                .and_then(|t| t.get::<Option<bool>>(key).ok().flatten())
        };
        let args = Args {
            // Bound when this plugin's environment was built, NEVER read from
            // the options table or from whichever plugin is running: either
            // would let one plugin act as another. Same as `run`'s attribution.
            owner: owner.clone(),
            argv: opts
                .as_ref()
                .and_then(|t| t.get::<Option<Vec<String>>>("args").ok().flatten())
                .unwrap_or_default(),
            session: get_string("session").unwrap_or_default(),
            target: get_string("target"),
            text: get_string("text"),
            value: get_string("value"),
            delta: opts
                .as_ref()
                .and_then(|t| t.get::<Option<i64>>("delta").ok().flatten()),
            force: get_bool("force").unwrap_or(false),
            toggle: get_bool("toggle").unwrap_or(false),
            flag: get_bool("flag"),
            number: opts
                .as_ref()
                .and_then(|t| t.get::<Option<f64>>("number").ok().flatten()),
            reset: get_bool("reset").unwrap_or(false),
            repo: get_string("repo"),
            branch: get_string("branch"),
            base: get_string("base"),
            worktree_path: get_string("worktree_path"),
            agent: get_string("agent"),
            host: get_string("host"),
            multiplexer: get_string("multiplexer"),
            status: get_string("status"),
            level: get_string("level"),
            file: get_string("file"),
            action: get_string("action"),
            // Bytes for a program pane this plugin already started. Read as
            // bytes and never through a Rust `String`: `"\27"` and `"\r"` are the
            // point of the field, and a Lua string may hold a sequence that is
            // not UTF-8 at all — a keyboard escape for a program that speaks its
            // own encoding. Converted, such a field would be dropped whole and
            // silently, which reads as "the editor ignored the file" with no
            // error anywhere. The pane's stdin takes bytes, not text.
            keys: opts
                .as_ref()
                .and_then(|t| t.get::<Option<mlua::LuaString>>("keys").ok().flatten())
                .map(|keys| keys.as_bytes().to_vec()),
            // A Lua array of session ids. Read here rather than as text so a
            // plugin cannot build an order by string concatenation.
            list: opts
                .as_ref()
                .and_then(|t| t.get::<Option<Table>>("list").ok().flatten())
                .map(|table| {
                    table
                        .sequence_values::<String>()
                        .filter_map(Result::ok)
                        .collect()
                })
                .unwrap_or_default(),
            // A Lua array of `{ path = …, worktree = … }`: the further
            // repositories a create spans. A row with no path is dropped rather
            // than creating a member of nowhere.
            extras: opts
                .as_ref()
                .and_then(|t| t.get::<Option<Table>>("extras").ok().flatten())
                .map(|table| {
                    table
                        .sequence_values::<Table>()
                        .filter_map(Result::ok)
                        .filter_map(|entry| {
                            let path: String = entry.get("path").ok()?;
                            (!path.is_empty()).then(|| ExtraMember {
                                path,
                                worktree: entry.get::<Option<bool>>("worktree").ok().flatten()
                                    == Some(true),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            // Every scalar the plugin passed, for an event's payload. `text` is
            // the event's name and is left out; nothing else is interpreted.
            payload: opts
                .as_ref()
                .map(|table| {
                    table
                        .pairs::<String, Value>()
                        .filter_map(Result::ok)
                        .filter(|(key, _)| key != "text")
                        .filter_map(|(key, value)| {
                            let field = match value {
                                Value::String(s) => Field::Text(s.to_string_lossy()),
                                Value::Boolean(b) => Field::Bool(b),
                                Value::Integer(n) => Field::Number(n as f64),
                                Value::Number(n) => Field::Number(n),
                                _ => return None,
                            };
                            Some((key, field))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };

        let parsed = Command::parse(&kind, args).map_err(mlua::Error::runtime)?;
        // Edited confirmation panes forward `options` but may not know the new
        // top-level handoff. Store the undo target when the command is actually
        // issued, so cancelling an old float cannot create a phantom undo.
        if matches!(&parsed, Command::Delete { force: false, .. }) {
            if let Some(remember) = opts
                .as_ref()
                .and_then(|table| table.get::<Option<Table>>("remember").ok().flatten())
            {
                let key: String = remember.get("key")?;
                let value: String = remember.get("value")?;
                let store: Table = lua.globals().get("store")?;
                store.set(key, value)?;
            }
        }
        queue
            .borrow_mut()
            .push((requester(&behalf, &entered, &owner), parsed));
        Ok(())
    })
}

/// Globals that read files, load code, or write to the terminal.
///
/// Withholding `io`/`os`/`debug` at VM construction is not enough: Lua's *base*
/// library is not optional, and it carries `dofile` and `loadfile`, which read
/// arbitrary paths. `print` is just as unwelcome — stdout belongs to the TUI,
/// so a stray `print` would corrupt the screen rather than log anything.
///
/// Removed rather than replaced with a stub that refuses, because the
/// capability model is enforcement by *absence*: a plugin should find nothing
/// there at all.
/// (`tests/kernel_mvp.rs` probes for each of these — that test is what found
/// `dofile`/`loadfile` in the first place.)
const WITHHELD_GLOBALS: [&str; 8] = [
    "dofile",     // reads and runs an arbitrary path
    "loadfile",   // reads an arbitrary path
    "load",       // loads code, including bytecode that can crash the VM
    "loadstring", // Lua 5.1 spelling of the same
    "print",      // stdout is the TUI's
    "warn",       // and stderr is usually the same terminal
    "collectgarbage",
    // The one way to write past a metatable that refuses: with it, a plugin
    // could plant a field in a table every plugin reads.
    "rawset",
];

fn scrub_globals(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    for name in WITHHELD_GLOBALS {
        globals.set(name, Value::Nil)?;
    }
    Ok(())
}

/// `require("lib.theme")` loads `ui/lib/theme.lua`, for one plugin.
///
/// Ours rather than Lua's, because `package` is withheld: this resolves only
/// inside the plugin directory, so `require` cannot reach the filesystem at
/// large. Each plugin has its own, with its own module cache, and a module runs
/// in the requiring plugin's `env`: a module shared between plugins would be a
/// table one of them could rewrite under the other, and a module's
/// `command(…)` has to act as the plugin that required it. The cache is held by
/// this function rather than placed anywhere Lua can name it, and it dies with
/// the VM, which is what makes shared libraries reload alongside their plugins.
fn require_for(lua: &Lua, ui_dir: &Path, env: Table) -> mlua::Result<mlua::Function> {
    let root = ui_dir.to_path_buf();
    let cache = lua.create_table()?;
    lua.create_function(move |lua, name: String| {
        if let Ok(Value::Table(module)) = cache.get::<Value>(name.clone()) {
            return Ok(Value::Table(module));
        }
        // `lib.theme` → `lib/theme.lua`, and nothing may escape the root.
        if name.contains("..") || name.starts_with('/') {
            return Err(mlua::Error::runtime(format!(
                "require({name:?}): only modules inside the plugin directory can be required"
            )));
        }
        let relative: PathBuf = name.split('.').collect::<Vec<_>>().join("/").into();
        let path = root.join(relative).with_extension("lua");
        let source = fs::read_to_string(&path).map_err(|e| {
            mlua::Error::runtime(format!("require({name:?}): {}: {e}", path.display()))
        })?;
        let value: Value = lua
            .load(&source)
            .set_name(name.clone())
            .set_environment(env.clone())
            .eval()?;
        cache.set(name, value.clone())?;
        Ok(value)
    })
}

/// Install a persisted table under `global`, optionally namespaced per plugin.
fn install_store(
    lua: &Lua,
    global: &str,
    store: Shared,
    version: StateVersion,
    writes: super::WriteCount,
    _ns: Option<()>,
) -> mlua::Result<()> {
    let table = lua.create_table()?;
    let meta = lua.create_table()?;

    let read = store.clone();
    meta.set(
        "__index",
        lua.create_function(
            move |lua, (_, key): (Table, String)| match read.borrow().get(&key) {
                Some(value) => to_lua(lua, value),
                None => Ok(Value::Nil),
            },
        )?,
    )?;

    let write = store;
    meta.set(
        "__newindex",
        lua.create_function(move |lua, (_, key, value): (Table, String, Value)| {
            count_write(&writes, &value);
            let mut slot = write.borrow_mut();
            // Compared before storing. A pane may write the same value on every
            // frame — the search strip re-states how many panes it is showing —
            // and treating that as a change would move the version 40 times a
            // second and invalidate every cached tree, which is the difference
            // between this mechanism working and doing nothing at all.
            let moved = match from_lua(lua, &value) {
                Some(persisted) => {
                    if slot.get(&key) == Some(&persisted) {
                        false
                    } else {
                        slot.insert(key, persisted);
                        true
                    }
                }
                None => slot.remove(&key).is_some(),
            };
            if moved {
                version.set(version.get().wrapping_add(1));
            }
            Ok(())
        })?,
    )?;

    // Sealed: every plugin reads this one table, so one able to replace its
    // metatable would decide what `store` is for all of them.
    meta.set("__metatable", false)?;
    table.set_metatable(Some(meta))?;
    lua.globals().set(global, table)?;
    Ok(())
}

/// `state` — private to one plugin (keyed by `ns`), and **in memory only**.
///
/// It outlives a reload because [`LuaHost::build`] hands the same map to the new VM,
/// which is what "persistent" in the plugin docs used to mean and read as more than
/// it was. Nothing serialises it: a plugin's `state` dies with the process, and so
/// does `store`. Anything a plugin needs after a restart has nowhere to go today —
/// worth knowing before this word is chosen again.
fn state_for(
    lua: &Lua,
    state: Private,
    ns: String,
    version: StateVersion,
    writes: super::WriteCount,
) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    let meta = lua.create_table()?;

    let read = state.clone();
    let read_ns = ns.clone();
    meta.set(
        "__index",
        lua.create_function(move |lua, (_, key): (Table, String)| {
            match read.borrow().get(&(read_ns.clone(), key)) {
                Some(value) => to_lua(lua, value),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    let write = state;
    meta.set(
        "__newindex",
        lua.create_function(move |lua, (_, key, value): (Table, String, Value)| {
            count_write(&writes, &value);
            let ns = ns.clone();
            let mut slot = write.borrow_mut();
            // Same rule as `store`: only a value that actually moved counts.
            let moved = match from_lua(lua, &value) {
                Some(persisted) => {
                    if slot.get(&(ns.clone(), key.clone())) == Some(&persisted) {
                        false
                    } else {
                        slot.insert((ns, key), persisted);
                        true
                    }
                }
                None => slot.remove(&(ns, key)).is_some(),
            };
            if moved {
                version.set(version.get().wrapping_add(1));
            }
            Ok(())
        })?,
    )?;

    meta.set("__metatable", false)?;
    table.set_metatable(Some(meta))?;
    Ok(table)
}

/// Count one `store`/`state` assignment, and whether it assigned a table.
///
/// Always counted, since it is one add; the host attributes the count to a
/// plugin by reading it before and after that plugin renders.
fn count_write(writes: &super::WriteCount, value: &Value) {
    let (all, tables) = writes.get();
    let table = u64::from(matches!(value, Value::Table(_)));
    writes.set((all.wrapping_add(1), tables.wrapping_add(table)));
}

fn to_lua(lua: &Lua, value: &Persisted) -> mlua::Result<Value> {
    Ok(match value {
        Persisted::Bool(b) => Value::Boolean(*b),
        Persisted::Int(n) => Value::Integer(*n),
        Persisted::Num(n) => Value::Number(*n),
        Persisted::Str(s) => Value::String(lua.create_string(s)?),
        Persisted::Table(entries) => {
            let table = lua.create_table()?;
            for (key, entry) in entries {
                table.set(to_lua(lua, key)?, to_lua(lua, entry)?)?;
            }
            Value::Table(table)
        }
    })
}

fn from_lua(lua: &Lua, value: &Value) -> Option<Persisted> {
    match value {
        Value::Nil => None,
        Value::Boolean(b) => Some(Persisted::Bool(*b)),
        Value::Integer(n) => Some(Persisted::Int(*n)),
        Value::Number(n) => Some(Persisted::Num(*n)),
        Value::String(s) => Some(Persisted::Str(s.to_string_lossy())),
        Value::Table(table) => {
            let table = contents(lua, table);
            let mut entries = Vec::new();
            for pair in table.pairs::<Value, Value>() {
                let (key, entry) = pair.ok()?;
                if let (Some(key), Some(entry)) = (from_lua(lua, &key), from_lua(lua, &entry)) {
                    entries.push((key, entry));
                }
            }
            // In a canonical order, not the order `pairs` walked it in: that
            // depends on the VM's string-hash seed and on how the table was
            // built, so two copies of one table walk differently — and a write
            // is compared with what is held to decide whether it moved. Held in
            // `pairs` order, a pane re-stating an unchanged table every frame
            // moved the state version on most of them, and every pure pane's
            // cached tree went with it.
            entries.sort_by(|(a, _), (b, _)| canonical(a, b));
            Some(Persisted::Table(entries))
        }
        _ => None,
    }
}

/// The table whose entries `table` holds: itself, or — for a read-only view
/// of published data, which is empty underneath — what it reads through to,
/// and for a plugin's `thurbox` view, all of that plus its own fields.
/// Copied raw, a published table saved into `store` or `state` was empty.
fn contents(lua: &Lua, table: &Table) -> Table {
    let lookup = |name: &str, key: &Table| -> Option<Table> {
        lua.named_registry_value::<Table>(name)
            .ok()?
            .raw_get::<Option<Table>>(key.clone())
            .ok()
            .flatten()
    };
    if let Some(inner) = lookup(FROZEN, table).or_else(|| lookup(PUBLISHED_TABLES, table)) {
        return inner;
    }
    let is_view = lua
        .named_registry_value::<Table>(VIEWS)
        .ok()
        .and_then(|views| views.raw_get::<bool>(table.clone()).ok())
        .unwrap_or(false);
    if is_view {
        if let Ok(copy) = view_contents(lua, table) {
            return copy;
        }
    }
    table.clone()
}

/// A plugin's `thurbox` view as one table, the way a read sees it: every
/// published field — walked through the view's own `pairs`, so the copy counts
/// as reading all of it, as iterating it does — and then this plugin's own
/// `runs` and `granted`, which are the view's and nobody else's.
fn view_contents(lua: &Lua, view: &Table) -> mlua::Result<Table> {
    let copy = lua.create_table()?;
    let pairs: mlua::Function = lua.globals().get("pairs")?;
    let (step, state, mut key): (mlua::Function, Value, Value) = pairs.call(view.clone())?;
    loop {
        let (next, value): (Value, Value) = step.call((state.clone(), key))?;
        if next.is_nil() {
            break;
        }
        copy.raw_set(next.clone(), value)?;
        key = next;
    }
    for pair in view.clone().pairs::<Value, Value>() {
        let (field, value) = pair?;
        copy.raw_set(field, value)?;
    }
    Ok(copy)
}

/// A total order over table keys, for [`from_lua`]'s canonical order. Keys
/// are almost always strings or integers; the rest only need to be consistent.
fn canonical(a: &Persisted, b: &Persisted) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let rank = |p: &Persisted| match p {
        Persisted::Bool(_) => 0,
        Persisted::Int(_) => 1,
        Persisted::Num(_) => 2,
        Persisted::Str(_) => 3,
        Persisted::Table(_) => 4,
    };
    match (a, b) {
        (Persisted::Bool(x), Persisted::Bool(y)) => x.cmp(y),
        (Persisted::Int(x), Persisted::Int(y)) => x.cmp(y),
        (Persisted::Num(x), Persisted::Num(y)) => x.total_cmp(y),
        (Persisted::Str(x), Persisted::Str(y)) => x.cmp(y),
        (Persisted::Table(x), Persisted::Table(y)) => x
            .iter()
            .zip(y)
            .map(|((xk, xv), (yk, yv))| canonical(xk, yk).then_with(|| canonical(xv, yv)))
            .find(|order| *order != Ordering::Equal)
            .unwrap_or_else(|| x.len().cmp(&y.len())),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// Trim mlua's wrapper noise so the pane shows the plugin's own message.
pub(super) fn clean_error(error: &mlua::Error) -> String {
    let text = error.to_string();
    // Before the memory check: arming fails on a full VM, and its cause reads
    // "not enough memory" too, but the call never started.
    if let Some(at) = text.find(BUDGET_UNARMED) {
        let cause = text[at + BUDGET_UNARMED.len()..].trim_start_matches(':');
        let cause = cause.lines().next().unwrap_or_default().trim();
        return format!("not run: its instruction budget could not be armed ({cause})");
    }
    if text.contains(BUDGET_EXCEEDED) {
        return format!(
            "exceeded its instruction budget (~{} instructions) — \
             is there an unterminated loop?",
            instruction_budget()
        );
    }
    if text.contains("not enough memory") {
        return format!(
            "exceeded the plugin memory limit ({} bytes)",
            memory_limit()
        );
    }
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(&text)
        .trim()
        .to_string()
}

// ── Display width ───────────────────────────────────────────────────────────

/// What a cut is marked with when the caller does not say.
const ELLIPSIS: &str = "…";

/// How many terminal columns `s` occupies.
///
/// Not its bytes and not its characters: a CJK glyph takes two columns and a
/// combining mark none, so the three counts disagree on exactly the text a
/// column budget is hardest to get right on. This is the same `unicode-width`
/// the painter measures with, so a plugin that budgets with it agrees with what
/// lands on the screen.
pub(crate) fn columns(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// The longest prefix of `s` that fits in `cols` columns.
///
/// A double-width glyph that would straddle the edge is left out rather than
/// half-drawn, so the result is never *wider* than asked for — which is what
/// lets a caller add its own marker and still fit.
pub(crate) fn take_left(s: &str, cols: usize) -> &str {
    let mut used = 0;
    for (at, ch) in s.char_indices() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > cols {
            return &s[..at];
        }
        used += w;
    }
    s
}

/// The longest suffix of `s` that fits in `cols` columns.
fn take_right(s: &str, cols: usize) -> &str {
    let mut used = 0;
    for (at, ch) in s.char_indices().rev() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > cols {
            return &s[at + ch.len_utf8()..];
        }
        used += w;
    }
    s
}

/// Which end of the text a truncation eats.
enum Side {
    Right,
    Left,
    Middle,
}

impl Side {
    fn parse(name: &str) -> mlua::Result<Self> {
        match name {
            "right" => Ok(Self::Right),
            "left" => Ok(Self::Left),
            "middle" => Ok(Self::Middle),
            other => Err(mlua::Error::runtime(format!(
                "text.truncate: side must be right, left or middle, not {other:?}"
            ))),
        }
    }
}

/// Cut `s` down to `cols` columns, marking the cut with `ellipsis`.
fn truncate(s: &str, cols: usize, ellipsis: &str, side: &Side) -> String {
    if cols == 0 {
        return String::new();
    }
    if columns(s) <= cols {
        return s.to_string();
    }
    let mark = columns(ellipsis);
    // No room for both the mark and any text: the mark alone is the whole
    // answer, itself cut to fit rather than overrunning the budget it was
    // meant to respect.
    if mark >= cols {
        return take_left(ellipsis, cols).to_string();
    }
    let budget = cols - mark;
    match side {
        Side::Right => format!("{}{ellipsis}", take_left(s, budget)),
        Side::Left => format!("{ellipsis}{}", take_right(s, budget)),
        Side::Middle => {
            // The tail gets the larger share of an odd remainder: for a path it
            // carries the leaf, which is the half that identifies the thing.
            let head = budget / 2;
            format!(
                "{}{ellipsis}{}",
                take_left(s, head),
                take_right(s, budget - head)
            )
        }
    }
}

/// Where the short side of a pad goes.
enum Align {
    Left,
    Right,
    Center,
}

impl Align {
    fn parse(name: &str) -> mlua::Result<Self> {
        match name {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "center" | "centre" => Ok(Self::Center),
            other => Err(mlua::Error::runtime(format!(
                "text.pad: align must be left, right or center, not {other:?}"
            ))),
        }
    }
}

/// Spaces `s` out to `cols` columns. Text already that wide is returned as it
/// is — a pad never truncates, because the two answers to "too long" are the
/// caller's to choose between.
fn pad(s: &str, cols: usize, align: &Align) -> String {
    let short = cols.saturating_sub(columns(s));
    if short == 0 {
        return s.to_string();
    }
    match align {
        Align::Left => format!("{s}{}", " ".repeat(short)),
        Align::Right => format!("{}{s}", " ".repeat(short)),
        Align::Center => {
            let before = short / 2;
            format!("{}{s}{}", " ".repeat(before), " ".repeat(short - before))
        }
    }
}

/// No real terminal is remotely this wide; the ceiling exists so that
/// `pad`'s `" ".repeat(cols)` — a Rust-side allocation the VM's own
/// [`super::memory_limit`] cannot see or budget for — stays too small to
/// ever hit the allocator's abort path, whatever a plugin passes in.
const MAX_COLS: usize = 1 << 20;

/// A column count as Lua spells it: any number, floored, never negative,
/// and never past [`MAX_COLS`].
fn cols_arg(n: f64) -> usize {
    if n.is_finite() && n > 0.0 {
        (n.floor() as usize).min(MAX_COLS)
    } else {
        0
    }
}

/// `text.width(s)`, `text.truncate(s, cols, opts)` and `text.pad(s, cols, align)`.
///
/// The one measurement a plugin cannot make for itself. Lua's `#` counts bytes
/// and `utf8.len` counts codepoints; a terminal budget is columns, and no
/// arithmetic over the other two recovers it. Every truncation and every pad in
/// the interface rides on this, so it is the kernel's `unicode-width` — the
/// painter's own measure — rather than an approximation per plugin.
///
/// `opts` is the ellipsis, or `{ ellipsis = "…", side = "right"|"left"|"middle" }`
/// for the cuts that keep the far end. An unknown `side` or `align` raises
/// rather than being ignored: a misspelt option that silently means "left" is
/// the trap `command`'s option list already documents.
fn install_text(lua: &Lua) -> mlua::Result<()> {
    let table = lua.create_table()?;

    table.set(
        "width",
        lua.create_function(|_, s: String| Ok(columns(&s)))?,
    )?;

    table.set(
        "truncate",
        lua.create_function(|_, (s, cols, opts): (String, f64, Option<Value>)| {
            let (ellipsis, side) = match opts {
                Some(Value::String(mark)) => (mark.to_string_lossy(), Side::Right),
                Some(Value::Table(opts)) => (
                    opts.get::<Option<String>>("ellipsis")?
                        .unwrap_or_else(|| ELLIPSIS.to_string()),
                    match opts.get::<Option<String>>("side")? {
                        Some(name) => Side::parse(&name)?,
                        None => Side::Right,
                    },
                ),
                _ => (ELLIPSIS.to_string(), Side::Right),
            };
            Ok(truncate(&s, cols_arg(cols), &ellipsis, &side))
        })?,
    )?;

    table.set(
        "pad",
        lua.create_function(|_, (s, cols, align): (String, f64, Option<String>)| {
            let align = match align {
                Some(name) => Align::parse(&name)?,
                None => Align::Left,
            };
            Ok(pad(&s, cols_arg(cols), &align))
        })?,
    )?;

    lua.globals().set("text", table)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three counts a plugin could reach for disagree here, which is why
    /// this one is the kernel's: six characters, eighteen bytes, twelve columns.
    #[test]
    fn width_is_columns_and_not_bytes_or_characters() {
        assert_eq!(columns("修复终端宽度"), 12);
        assert_eq!("修复终端宽度".len(), 18);
        assert_eq!("修复终端宽度".chars().count(), 6);
    }

    /// A double-width glyph that would straddle the budget's edge is dropped
    /// rather than half-drawn: the result must never be wider than asked for,
    /// or the caller's own marker pushes the row over.
    #[test]
    fn a_cut_never_returns_more_columns_than_it_was_given() {
        assert_eq!(take_left("修复终端", 3), "修");
        assert_eq!(take_right("修复终端", 3), "端");
        assert_eq!(
            truncate("修复终端宽度", 7, ELLIPSIS, &Side::Right),
            "修复终…"
        );
    }

    #[test]
    fn each_side_keeps_the_end_it_names() {
        assert_eq!(truncate("abcdefgh", 5, ELLIPSIS, &Side::Right), "abcd…");
        assert_eq!(truncate("abcdefgh", 5, ELLIPSIS, &Side::Left), "…efgh");
        assert_eq!(truncate("abcdefgh", 5, ELLIPSIS, &Side::Middle), "ab…gh");
        // The tail takes the larger share of an odd remainder.
        assert_eq!(truncate("abcdefgh", 6, ELLIPSIS, &Side::Middle), "ab…fgh");
    }

    /// Below the mark's own width there is nothing to say but the mark, and it
    /// is cut to the budget rather than overrunning the one it protects.
    #[test]
    fn a_budget_too_small_for_the_mark_holds_the_mark() {
        assert_eq!(truncate("abcdefgh", 1, ELLIPSIS, &Side::Right), "…");
        assert_eq!(truncate("abcdefgh", 0, ELLIPSIS, &Side::Right), "");
        assert_eq!(truncate("abcdefgh", 2, "...", &Side::Right), "..");
    }

    #[test]
    fn an_empty_mark_cuts_without_saying_so() {
        assert_eq!(truncate("abcdefgh", 4, "", &Side::Right), "abcd");
        assert_eq!(truncate("abcdefgh", 4, "", &Side::Left), "efgh");
    }

    #[test]
    fn pad_counts_columns_and_never_truncates() {
        assert_eq!(pad("修复", 6, &Align::Left), "修复  ");
        assert_eq!(pad("修复", 6, &Align::Right), "  修复");
        assert_eq!(pad("修复", 7, &Align::Center), " 修复  ");
        assert_eq!(pad("修复终端", 4, &Align::Left), "修复终端");
    }

    /// A misspelt option means the caller asked for something; answering with
    /// the default would draw a plausible frame and report nothing.
    #[test]
    fn an_unknown_side_or_align_is_refused() {
        assert!(Side::parse("centre").is_err());
        assert!(Align::parse("middle").is_err());
        assert!(Align::parse("centre").is_ok());
    }

    /// `pad`'s `" ".repeat(cols)` is a Rust-side allocation the VM's memory
    /// limit cannot see; a plugin passing a huge `cols` must not drive it
    /// past a bounded size regardless of how large the request claims to be.
    #[test]
    fn a_huge_cols_is_capped_rather_than_allocated_in_full() {
        assert_eq!(cols_arg(f64::MAX), MAX_COLS);
        assert_eq!(cols_arg(1e18), MAX_COLS);
        assert_eq!(pad("x", cols_arg(1e18), &Align::Left).len(), MAX_COLS);
    }
}
