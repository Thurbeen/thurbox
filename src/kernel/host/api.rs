use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mlua::{Lua, Table, Value};

use super::load::BUDGET_EXCEEDED;
use super::{
    instruction_budget, memory_limit, Persisted, Private, Queue, Roots, Shared, StateVersion,
};
use crate::kernel::command::{Args, Command, ExtraMember};
use crate::kernel::events::Field;

/// Install `require`, `state` and `store` into a fresh VM.
#[allow(clippy::too_many_arguments)]
pub(super) fn install_api(
    lua: &Lua,
    ui_dir: &Path,
    store: Shared,
    state: Private,
    current: Rc<RefCell<String>>,
    queue: Queue,
    roots: Roots,
    runs: Rc<RefCell<Vec<(String, crate::kernel::runs::Ask)>>>,
    current_path: Rc<RefCell<String>>,
    state_version: StateVersion,
    clock: Rc<std::cell::Cell<f64>>,
    clock_read: Rc<std::cell::Cell<bool>>,
) -> mlua::Result<()> {
    scrub_globals(lua)?;
    install_require(lua, ui_dir)?;
    install_store(lua, "store", store, state_version.clone(), None)?;
    install_private(lua, state, current, state_version)?;
    install_command(lua, queue, current_path.clone())?;
    install_files(lua, roots)?;
    install_run(lua, runs, current_path)?;
    install_clock(lua, clock, clock_read)?;
    Ok(())
}

/// The registry name of the metatable every render context carries.
///
/// See [`install_clock`]. In the registry rather than in globals for the same
/// reason [`RUN_IMPL`] is: a plugin chunk's `_ENV` *is* the globals table, so
/// anything parked there is reachable by name from every plugin.
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
/// One visible consequence: `elapsed` is not a key of the ctx table, so it does
/// not appear in `pairs(ctx)`. Nothing iterates a render context — a plugin asks
/// it for named facts — and the alternative is to give up knowing who reads the
/// clock.
fn install_clock(
    lua: &Lua,
    clock: Rc<std::cell::Cell<f64>>,
    read: Rc<std::cell::Cell<bool>>,
) -> mlua::Result<()> {
    let meta = lua.create_table()?;
    meta.set(
        "__index",
        lua.create_function(move |_, (_, key): (Table, String)| {
            if key == "elapsed" {
                read.set(true);
                return Ok(Value::Number(clock.get()));
            }
            Ok(Value::Nil)
        })?,
    )?;
    lua.set_named_registry_value(CTX_META, meta)
}

/// Give one render context its clock.
pub(super) fn attach_clock(lua: &Lua, ctx: &Table) -> mlua::Result<()> {
    let meta: Table = lua.named_registry_value(CTX_META)?;
    ctx.set_metatable(Some(meta))
}

/// The name the `run` implementation is parked under.
///
/// Installed once per VM and handed to `run` only while a plugin that may use it
/// is executing — see `LuaHost::enter`.
///
/// It lives in the VM's **registry**, not in globals. A plugin chunk's `_ENV` is
/// the globals table, so anything parked there is reachable by name from every
/// plugin whether or not it was granted — a leading `__` is a naming convention,
/// not a boundary, and `scrub_globals` can only remove names it lists. The
/// registry is not addressable from Lua at all (`debug` is withheld), and it
/// still dies with the VM, so a reload cannot leave a stale handle behind.
pub(super) const RUN_IMPL: &str = "__run_impl";

/// `run(key, program, opts)` — ask for a program and read the answer later.
///
/// Queued, never executed here: the whole point is that a plugin cannot call
/// anything that waits. The asking plugin is stamped from `current`, so a run
/// is attributed without the plugin naming itself (and without being able to
/// claim another's key).
fn install_run(
    lua: &Lua,
    runs: Rc<RefCell<Vec<(String, crate::kernel::runs::Ask)>>>,
    current_path: Rc<RefCell<String>>,
) -> mlua::Result<()> {
    let implementation = lua.create_function(
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
            let ask = crate::kernel::runs::Ask {
                key,
                program,
                session: opts
                    .as_ref()
                    .and_then(|t| t.get::<Option<String>>("session").ok().flatten())
                    .unwrap_or_default(),
                ttl: seconds("ttl").unwrap_or(crate::kernel::runs::DEFAULT_TTL),
                timeout: seconds("timeout").unwrap_or(crate::kernel::runs::DEFAULT_TIMEOUT),
                refresh: opts
                    .as_ref()
                    .and_then(|t| t.get::<Option<bool>>("refresh").ok().flatten())
                    .unwrap_or(false),
            };
            runs.borrow_mut().push((current_path.borrow().clone(), ask));
            Ok(())
        },
    )?;
    lua.set_named_registry_value(RUN_IMPL, implementation)?;
    // Absent until a plugin that may use it runs.
    lua.globals().set("run", Value::Nil)?;
    Ok(())
}

/// `files.list(session, path)` and `files.read(session, path)`.
///
/// The capability that is *not* granted here is the point: a plugin gets a
/// directory listing and a file's text, both rooted at that session's working
/// directory and refusing anything outside it. It never gets a filesystem.
fn install_files(lua: &Lua, roots: Roots) -> mlua::Result<()> {
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

    lua.globals().set("files", files)?;
    Ok(())
}

/// `command("delete", { session = id })` — the only way a plugin changes state.
///
/// It enqueues and returns; it never runs the operation. That is the whole of
/// the write side, and it is why a plugin cannot stall the render loop with a
/// database write or an unreachable host.
///
/// A malformed command raises immediately, because the mistake is in the
/// plugin's own call and there is nothing to report asynchronously about.
fn install_command(lua: &Lua, queue: Queue, current_path: Rc<RefCell<String>>) -> mlua::Result<()> {
    let command = lua.create_function(move |_, (kind, opts): (String, Option<Table>)| {
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
            // Stamped from the plugin currently executing, NEVER read from the
            // options table: a plugin that could name its own owner could act as
            // another one. Same reasoning as `run`'s attribution.
            owner: current_path.borrow().clone(),
            argv: opts
                .as_ref()
                .and_then(|t| t.get::<Option<Vec<String>>>("args").ok().flatten())
                .unwrap_or_default(),
            session: get_string("session").unwrap_or_default(),
            text: get_string("text"),
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
            agent: get_string("agent"),
            host: get_string("host"),
            status: get_string("status"),
            file: get_string("file"),
            action: get_string("action"),
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
        queue.borrow_mut().push(parsed);
        Ok(())
    })?;
    lua.globals().set("command", command)?;
    Ok(())
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
const WITHHELD_GLOBALS: [&str; 7] = [
    "dofile",     // reads and runs an arbitrary path
    "loadfile",   // reads an arbitrary path
    "load",       // loads code, including bytecode that can crash the VM
    "loadstring", // Lua 5.1 spelling of the same
    "print",      // stdout is the TUI's
    "warn",       // and stderr is usually the same terminal
    "collectgarbage",
];

fn scrub_globals(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    for name in WITHHELD_GLOBALS {
        globals.set(name, Value::Nil)?;
    }
    Ok(())
}

/// Where `require`'s module cache lives, in the registry rather than globals.
const MODULE_CACHE: &str = "__modules";

/// `require("lib.theme")` loads `ui/lib/theme.lua`.
///
/// Ours rather than Lua's, because `package` is withheld: this resolves only
/// inside the plugin directory, so `require` cannot reach the filesystem at
/// large. The module cache lives in the VM, so it dies with the VM — which is
/// what makes shared libraries reload alongside their plugins. It lives in the
/// VM's *registry* rather than its globals for the reason [`RUN_IMPL`] does: a
/// global is reachable from every plugin, and one able to rewrite the cache could
/// hand every other plugin a replaced `lib.theme`.
fn install_require(lua: &Lua, ui_dir: &Path) -> mlua::Result<()> {
    let root = ui_dir.to_path_buf();
    let cache = lua.create_table()?;
    lua.set_named_registry_value(MODULE_CACHE, cache)?;

    let require = lua.create_function(move |lua, name: String| {
        let cache: Table = lua.named_registry_value(MODULE_CACHE)?;
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
        let value: Value = lua.load(&source).set_name(name.clone()).eval()?;
        cache.set(name, value.clone())?;
        Ok(value)
    })?;
    lua.globals().set("require", require)?;
    Ok(())
}

/// Install a persisted table under `global`, optionally namespaced per plugin.
fn install_store(
    lua: &Lua,
    global: &str,
    store: Shared,
    version: StateVersion,
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
        lua.create_function(move |_, (_, key, value): (Table, String, Value)| {
            let mut slot = write.borrow_mut();
            // Compared before storing. A pane may write the same value on every
            // frame — the search strip re-states how many panes it is showing —
            // and treating that as a change would move the version 40 times a
            // second and invalidate every cached tree, which is the difference
            // between this mechanism working and doing nothing at all.
            let moved = match from_lua(&value) {
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

    table.set_metatable(Some(meta))?;
    lua.globals().set(global, table)?;
    Ok(())
}

/// `state` — private to the plugin currently being called, and **in memory only**.
///
/// It outlives a reload because [`LuaHost::build`] hands the same map to the new VM,
/// which is what "persistent" in the plugin docs used to mean and read as more than
/// it was. Nothing serialises it: a plugin's `state` dies with the process, and so
/// does `store`. Anything a plugin needs after a restart has nowhere to go today —
/// worth knowing before this word is chosen again.
fn install_private(
    lua: &Lua,
    state: Private,
    current: Rc<RefCell<String>>,
    version: StateVersion,
) -> mlua::Result<()> {
    let table = lua.create_table()?;
    let meta = lua.create_table()?;

    let read = state.clone();
    let read_ns = current.clone();
    meta.set(
        "__index",
        lua.create_function(move |lua, (_, key): (Table, String)| {
            let ns = read_ns.borrow().clone();
            match read.borrow().get(&(ns, key)) {
                Some(value) => to_lua(lua, value),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    let write = state;
    let write_ns = current;
    meta.set(
        "__newindex",
        lua.create_function(move |_, (_, key, value): (Table, String, Value)| {
            let ns = write_ns.borrow().clone();
            let mut slot = write.borrow_mut();
            // Same rule as `store`: only a value that actually moved counts.
            let moved = match from_lua(&value) {
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

    table.set_metatable(Some(meta))?;
    lua.globals().set("state", table)?;
    Ok(())
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

fn from_lua(value: &Value) -> Option<Persisted> {
    match value {
        Value::Nil => None,
        Value::Boolean(b) => Some(Persisted::Bool(*b)),
        Value::Integer(n) => Some(Persisted::Int(*n)),
        Value::Number(n) => Some(Persisted::Num(*n)),
        Value::String(s) => Some(Persisted::Str(s.to_string_lossy())),
        Value::Table(table) => {
            let mut entries = Vec::new();
            for pair in table.pairs::<Value, Value>() {
                let (key, entry) = pair.ok()?;
                if let (Some(key), Some(entry)) = (from_lua(&key), from_lua(&entry)) {
                    entries.push((key, entry));
                }
            }
            Some(Persisted::Table(entries))
        }
        _ => None,
    }
}

/// Trim mlua's wrapper noise so the pane shows the plugin's own message.
pub(super) fn clean_error(error: &mlua::Error) -> String {
    let text = error.to_string();
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
