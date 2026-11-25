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
use crate::kernel::command::{Args, Command};
use crate::kernel::events::Field;

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

#[derive(Clone)]
pub(super) struct EnvParts {
    pub ui_dir: PathBuf,
    pub state: Private,
    pub queue: Queue,
    pub roots: Roots,
    pub file_reads: Rc<RefCell<crate::kernel::files::ReadStore>>,
    pub file_wants: super::FileWants,
    pub runs: Rc<RefCell<Vec<(String, crate::kernel::runs::Request)>>>,
    pub version: StateVersion,
    pub writes: super::WriteCount,
    pub behalf: Rc<RefCell<Option<Vec<String>>>>,
    pub entered: Rc<RefCell<String>>,
}

pub(super) struct Env {
    pub table: Table,
    pub run: mlua::Function,
    pub run_forget: mlua::Function,
    pub files: Table,
    pub thurbox: Table,
}

pub(super) fn mod_env(lua: &Lua, parts: &EnvParts, path: &str, ns: &str) -> mlua::Result<Env> {
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
        files: files_for(
            lua,
            parts.roots.clone(),
            parts.file_reads.clone(),
            parts.file_wants.clone(),
            path.to_string(),
        )?,
        table,
    })
}

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

const SEALED_METAS: &str = "__sealed_metas";

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

const STORE_WRITERS: &str = "__store_writers";

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

const SEALED: [&str; 6] = ["string", "table", "math", "coroutine", "utf8", "text"];

pub(super) const READ_TAKEN_AT: u8 = 1;
pub(super) const READ_METRICS: u8 = 2;
pub(super) const READ_DIFFS: u8 = 4;
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
    let published = |lua: &Lua, tables: &Table| -> mlua::Result<Option<Table>> {
        match lua.globals().get::<Value>("thurbox")? {
            Value::Table(proxy) => tables.raw_get::<Option<Table>>(proxy),
            _ => Ok(None),
        }
    };

    let original_get: mlua::Function = lua.globals().get("rawget")?;
    let (map, nested, mod_views) = (tables.clone(), frozen.clone(), views.clone());
    let reads = read.clone();
    lua.globals().set(
        "rawget",
        lua.create_function(move |lua, (table, key): (Table, Value)| {
            if mod_views.raw_get::<bool>(table.clone()).unwrap_or(false) {
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

fn install_frozen_meta(lua: &Lua) -> mlua::Result<()> {
    let shared = lua.create_table()?;
    shared.raw_set(
        "__newindex",
        lua.create_function(|_, (_, key): (Value, Value)| -> mlua::Result<()> {
            Err(mlua::Error::runtime(format!(
                "thurbox is shared by every mod and cannot be changed (assigning {})",
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

const FROZEN_META: &str = "__frozen_meta";

const VIEWS: &str = "__thurbox_views";

const FROZEN: &str = "__frozen";

fn read_only_meta(lua: &Lua, inner: Table, what: &str) -> mlua::Result<Table> {
    let meta = lua.create_table()?;
    meta.raw_set("__index", inner.clone())?;
    let what = what.to_string();
    meta.raw_set(
        "__newindex",
        lua.create_function(move |_, (_, key): (Value, Value)| -> mlua::Result<()> {
            Err(mlua::Error::runtime(format!(
                "{what} is shared by every mod and cannot be changed (assigning {})",
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

pub(super) const CTX_META: &str = "__ctx_meta";

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

pub(super) fn attach_clock(lua: &Lua, ctx: &Table) -> mlua::Result<()> {
    let meta: Table = lua.named_registry_value(CTX_META)?;
    ctx.set_metatable(Some(meta))
}

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

fn files_for(
    lua: &Lua,
    roots: Roots,
    reads: Rc<RefCell<crate::kernel::files::ReadStore>>,
    wants: super::FileWants,
    owner: String,
) -> mlua::Result<Table> {
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
            let key = (root, path);
            {
                let mut wants = wants.borrow_mut();
                let keys = wants.entry(owner.clone()).or_default();
                if keys.len() >= crate::kernel::files::MAX_READ_KEYS && !keys.contains(&key) {
                    return Err(mlua::Error::runtime(
                        "files.read accepts at most 16 distinct paths per mod render",
                    ));
                }
                keys.insert(key.clone());
            }
            match reads.borrow_mut().request(key) {
                None => Ok(None),
                Some(answer) => answer
                    .as_ref()
                    .clone()
                    .map(Some)
                    .map_err(mlua::Error::runtime),
            }
        })?,
    )?;

    Ok(files)
}

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
            owner: owner.clone(),
            argv: opts
                .as_ref()
                .and_then(|t| t.get::<Option<Vec<String>>>("args").ok().flatten())
                .unwrap_or_default(),
            session: get_string("session").unwrap_or_default(),
            target: get_string("target"),
            text: get_string("text"),
            value: get_string("value"),
            force: get_bool("force").unwrap_or(false),
            fresh: get_bool("fresh").unwrap_or(false),
            toggle: get_bool("toggle").unwrap_or(false),
            flag: get_bool("flag"),
            number: opts
                .as_ref()
                .and_then(|t| t.get::<Option<f64>>("number").ok().flatten()),
            reset: get_bool("reset").unwrap_or(false),
            repo: get_string("repo"),
            agent: get_string("agent"),
            host: get_string("host"),
            fleet: get_string("fleet"),
            worktree: get_bool("worktree"),
            branch: get_string("branch"),
            base: get_string("base"),
            // `create`'s other repositories, each `{ path, worktree }`. An
            // entry with no path is dropped rather than guessed at.
            extras: opts
                .as_ref()
                .and_then(|t| t.get::<Option<Table>>("extras").ok().flatten())
                .map(|table| {
                    table
                        .sequence_values::<Table>()
                        .filter_map(Result::ok)
                        .filter_map(|entry| {
                            let path: String = entry.get("path").ok()?;
                            (!path.is_empty()).then(|| crate::kernel::command::ExtraMember {
                                path,
                                worktree: entry.get::<Option<bool>>("worktree").ok().flatten()
                                    == Some(true),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            level: get_string("level"),
            file: get_string("file"),
            action: get_string("action"),
            keys: opts
                .as_ref()
                .and_then(|t| t.get::<Option<mlua::LuaString>>("keys").ok().flatten())
                .map(|keys| keys.as_bytes().to_vec()),
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

const WITHHELD_GLOBALS: [&str; 8] = [
    "dofile",     // reads and runs an arbitrary path
    "loadfile",   // reads an arbitrary path
    "load",       // loads code, including bytecode that can crash the VM
    "loadstring", // Lua 5.1 spelling of the same
    "print",      // stdout is the TUI's
    "warn",       // and stderr is usually the same terminal
    "collectgarbage",
    "rawset",
];

fn scrub_globals(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    for name in WITHHELD_GLOBALS {
        globals.set(name, Value::Nil)?;
    }
    Ok(())
}

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
                "require({name:?}): only modules inside the mod directory can be required"
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

    meta.set("__metatable", false)?;
    table.set_metatable(Some(meta))?;
    lua.globals().set(global, table)?;
    Ok(())
}

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
        return format!("exceeded the mod memory limit ({} bytes)", memory_limit());
    }
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(&text)
        .trim()
        .to_string()
}

// ── Display width ───────────────────────────────────────────────────────────

const ELLIPSIS: &str = "…";

pub(crate) fn columns(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

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

const MAX_COLS: usize = 1 << 20;

fn cols_arg(n: f64) -> usize {
    if n.is_finite() && n > 0.0 {
        (n.floor() as usize).min(MAX_COLS)
    } else {
        0
    }
}

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

    #[test]
    fn width_is_columns_and_not_bytes_or_characters() {
        assert_eq!(columns("修复终端宽度"), 12);
        assert_eq!("修复终端宽度".len(), 18);
        assert_eq!("修复终端宽度".chars().count(), 6);
    }

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

    #[test]
    fn an_unknown_side_or_align_is_refused() {
        assert!(Side::parse("centre").is_err());
        assert!(Align::parse("middle").is_err());
        assert!(Align::parse("centre").is_ok());
    }

    #[test]
    fn a_huge_cols_is_capped_rather_than_allocated_in_full() {
        assert_eq!(cols_arg(f64::MAX), MAX_COLS);
        assert_eq!(cols_arg(1e18), MAX_COLS);
        assert_eq!(pad("x", cols_arg(1e18), &Align::Left).len(), MAX_COLS);
    }
}
