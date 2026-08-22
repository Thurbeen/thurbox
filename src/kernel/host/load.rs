use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use mlua::{Lua, Table, Value, VmState};

use super::api::clean_error;
use super::{
    instruction_budget, memory_limit, plugin_stdlib, Arrangement, Capability, Float, Plugin,
};
use crate::kernel::node::Size;
use crate::kernel::registry::{binding_from, Binding, Pill, Setting, Value as SettingValue};

/// Build a VM with the deliberate stdlib set and the memory ceiling.
pub(super) fn new_vm() -> mlua::Result<Lua> {
    let lua = Lua::new_with(plugin_stdlib(), mlua::LuaOptions::default())?;
    lua.set_memory_limit(memory_limit())?;
    Ok(lua)
}

/// How often the count hook fires. The budget is checked per batch rather than
/// per instruction, because a hook on every instruction would dominate the
/// frame cost.
const HOOK_INTERVAL: u32 = 100_000;

/// Marker in the abort error, so [`clean_error`] can recognise its own work
/// rather than pattern-matching on VM prose.
pub(super) const BUDGET_EXCEEDED: &str = "thurbox: instruction budget exceeded";

/// Arms the instruction-count hook for the duration of one plugin call.
///
/// This is the Lua 5.4 half of design.md D8. Luau donates `set_interrupt`;
/// stock Lua does not, so the equivalent is a count hook that raises an error
/// once the budget is spent — which unwinds the plugin call and surfaces as an
/// ordinary plugin failure. It is what stops `while true do end` from taking
/// the render loop with it.
///
/// Disarmed on drop, so the budget is per call rather than per VM.
pub(super) struct Budget<'a> {
    lua: &'a Lua,
}

impl<'a> Budget<'a> {
    pub(super) fn arm(lua: &'a Lua) -> Self {
        let batches = Rc::new(RefCell::new(0u32));
        let budget = instruction_budget();
        let limit = (budget / HOOK_INTERVAL).max(1);
        // A hook that cannot be installed must not silently disable the guard,
        // but it also must not stop the frame: an unbudgeted call is still
        // better than a black screen, and the failure is visible in the log.
        if let Err(e) = lua.set_hook(
            mlua::HookTriggers::default().every_nth_instruction(HOOK_INTERVAL),
            move |_, _| {
                let mut spent = batches.borrow_mut();
                *spent += 1;
                if *spent > limit {
                    return Err(mlua::Error::runtime(BUDGET_EXCEEDED));
                }
                Ok(VmState::Continue)
            },
        ) {
            tracing::warn!("could not arm the plugin instruction budget: {e}");
        }
        Self { lua }
    }
}

impl Drop for Budget<'_> {
    fn drop(&mut self) {
        self.lua.remove_hook();
    }
}

/// Read `ui/layout.lua`, if there is one.
///
/// A parse error here fails the whole reload, exactly like a broken plugin —
/// so the previous arrangement keeps running rather than the screen collapsing.
pub(super) fn load_arrangement(lua: &Lua, ui_dir: &Path) -> Result<Arrangement, String> {
    let path = ui_dir.join("layout.lua");
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Arrangement::Missing),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let value: Value = lua
        .load(&source)
        .set_name("layout.lua")
        .eval()
        .map_err(|e| format!("layout.lua: {}", clean_error(&e)))?;
    match value {
        Value::Function(arrange) => Ok(Arrangement::Dynamic(arrange)),
        other => Ok(Arrangement::Static(crate::kernel::layout::region_from_lua(
            &other, "layout",
        )?)),
    }
}

/// Read one plugin file into a [`Plugin`].
///
/// `relative` is the path within the interface directory, carried through
/// rather than re-derived: the kernel writes and removes files by that name, so
/// it must be the same string everywhere.
pub(super) fn load_plugin(lua: &Lua, path: &Path, relative: &str) -> Result<Plugin, String> {
    let file = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let source = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;

    let value: Value = lua
        .load(&source)
        .set_name(file.clone())
        .eval()
        .map_err(|e| format!("{file}: {}", clean_error(&e)))?;

    let Value::Table(def) = value else {
        return Err(format!("{file}: a plugin must return a table"));
    };

    let name: String = def
        .get::<Option<String>>("name")
        .map_err(|e| format!("{file}.name: {e}"))?
        // Strip a numeric ordering prefix, so `20_session_list.lua` is named
        // `session_list` without the author repeating themselves.
        .unwrap_or_else(|| {
            file.split_once('_')
                .filter(|(prefix, _)| prefix.chars().all(|c| c.is_ascii_digit()))
                .map(|(_, rest)| rest.to_string())
                .unwrap_or_else(|| file.clone())
        });

    let slot: String = def
        .get::<Option<String>>("slot")
        .map_err(|e| format!("{file}.slot: {e}"))?
        .unwrap_or_else(|| "center".to_string());

    let session_input = matches!(
        def.get::<Option<String>>("input")
            .map_err(|e| format!("{file}.input: {e}"))?
            .as_deref(),
        Some("session")
    );

    let focusable = def
        .get::<Option<bool>>("focusable")
        .map_err(|e| format!("{file}.focusable: {e}"))?
        .unwrap_or(false);

    let pure = def
        .get::<Option<bool>>("pure")
        .map_err(|e| format!("{file}.pure: {e}"))?
        .unwrap_or(false);

    let order = def
        .get::<Option<f64>>("order")
        .map_err(|e| format!("{file}.order: {e}"))?
        .unwrap_or(100.0);

    let size = match def.get::<Value>("size") {
        Ok(Value::Table(spec)) => Size {
            len: spec.get::<Option<u16>>("len").unwrap_or(None),
            pct: spec.get::<Option<f64>>("pct").unwrap_or(None),
            fill: spec.get::<Option<f64>>("fill").unwrap_or(None),
            min: spec.get::<Option<u16>>("min").unwrap_or(None),
            max: spec.get::<Option<u16>>("max").unwrap_or(None),
        },
        _ => Size::default(),
    };

    let decorates = def
        .get::<Option<String>>("decorates")
        .map_err(|e| format!("{file}.decorates: {e}"))?;

    let floats = def
        .get::<Option<bool>>("floats")
        .map_err(|e| format!("{file}.floats: {e}"))?
        .unwrap_or(false);

    let bindings = read_bindings(&def, &name)?;
    let settings = read_settings(&def, &name)?;
    let pills = read_pills(&def, &name)?;
    let capabilities = read_capabilities(&def, &file)?;

    // A decorator transforms another pane's tree and draws nothing of its own,
    // so requiring `render` of one would mean writing a stub that returns
    // nothing — which the loader would then treat as an occupant of a slot.
    if !def.contains_key("render").unwrap_or(false) && decorates.is_none() {
        return Err(format!(
            "{file}: a plugin must define a render function, or decorate another"
        ));
    }

    Ok(Plugin {
        name,
        path: relative.to_string(),
        file,
        slot,
        focusable,
        pure,
        session_input,
        size,
        order,
        decorates,
        floats,
        bindings,
        settings,
        pills,
        capabilities,
        def,
    })
}

/// Read `capabilities = { "run" }` off a declaration.
///
/// An unknown name fails the load with the file named, which is the same
/// treatment a malformed key or setting gets: a declaration the kernel cannot
/// honour is a mistake to report, not one to route around.
fn read_capabilities(def: &Table, file: &str) -> Result<Vec<Capability>, String> {
    let declared: Option<Vec<String>> = def
        .get("capabilities")
        .map_err(|e| format!("{file}.capabilities: {e}"))?;
    let mut out = Vec::new();
    for name in declared.unwrap_or_default() {
        let capability = Capability::parse(&name).ok_or_else(|| {
            let known: Vec<&str> = Capability::ALL.iter().map(|c| c.as_str()).collect();
            format!(
                "{file}.capabilities: no capability named {name:?} (available: {})",
                known.join(", ")
            )
        })?;
        if !out.contains(&capability) {
            out.push(capability);
        }
    }
    Ok(out)
}

/// Read the `float` field off whatever a plugin returned.
///
/// `float = true` takes the default size; a table may set `width`/`height` as
/// percentages of the screen, or `cols`/`rows` to ask in cells.
pub(super) fn read_float(value: &Value) -> Result<Option<Float>, String> {
    let Value::Table(table) = value else {
        return Ok(None);
    };
    match table.get::<Value>("float") {
        Ok(Value::Boolean(true)) => Ok(Some(Float::default())),
        Ok(Value::Table(spec)) => {
            let default = Float::default();
            Ok(Some(Float {
                width_pct: spec
                    .get::<Option<f64>>("width")
                    .map_err(|e| format!("float.width: {e}"))?
                    .unwrap_or(default.width_pct),
                height_pct: spec
                    .get::<Option<f64>>("height")
                    .map_err(|e| format!("float.height: {e}"))?
                    .unwrap_or(default.height_pct),
                cols: spec
                    .get::<Option<u16>>("cols")
                    .map_err(|e| format!("float.cols: {e}"))?,
                rows: spec
                    .get::<Option<u16>>("rows")
                    .map_err(|e| format!("float.rows: {e}"))?,
            }))
        }
        Ok(_) => Ok(None),
        Err(e) => Err(format!("float: {e}")),
    }
}

/// Read a plugin's `keys` declaration.
///
/// Declared as data rather than handled imperatively so the registry can
/// enumerate, conflict-check and rebind them without ever calling the plugin.
fn read_bindings(def: &Table, plugin: &str) -> Result<Vec<Binding>, String> {
    let raw: Value = def.get("keys").map_err(|e| format!("{plugin}.keys: {e}"))?;
    let Value::Table(list) = raw else {
        return Ok(Vec::new());
    };
    let mut bindings = Vec::new();
    for (index, entry) in list.sequence_values::<Table>().enumerate() {
        let entry = entry.map_err(|e| format!("{plugin}.keys[{}]: {e}", index + 1))?;
        let where_ = format!("{plugin}.keys[{}]", index + 1);
        let chord: String = entry
            .get::<Option<String>>("key")
            .map_err(|e| format!("{where_}.key: {e}"))?
            .ok_or_else(|| format!("{where_}: needs a key"))?;
        let action: String = entry
            .get::<Option<String>>("action")
            .map_err(|e| format!("{where_}.action: {e}"))?
            .ok_or_else(|| format!("{where_}: needs an action"))?;
        let description: String = entry
            .get::<Option<String>>("desc")
            .map_err(|e| format!("{where_}.desc: {e}"))?
            .unwrap_or_default();
        let scope: Option<String> = entry
            .get::<Option<String>>("scope")
            .map_err(|e| format!("{where_}.scope: {e}"))?;
        // Declared per key rather than inferred from the chord: whether the
        // agent needs `Ctrl+X` is a fact about the agent, not about the
        // keyboard, and v1 spells it out one action at a time for that reason.
        let passthrough: bool = entry
            .get::<Option<bool>>("passthrough")
            .map_err(|e| format!("{where_}.passthrough: {e}"))?
            .unwrap_or(false);
        let group: Option<String> = entry
            .get::<Option<String>>("group")
            .map_err(|e| format!("{where_}.group: {e}"))?;
        bindings.push(binding_from(
            plugin,
            &chord,
            &action,
            &description,
            scope.as_deref(),
            passthrough,
            group.as_deref(),
        ));
    }
    Ok(bindings)
}

/// Read a plugin's `settings` declaration.
fn read_settings(def: &Table, plugin: &str) -> Result<Vec<Setting>, String> {
    let raw: Value = def
        .get("settings")
        .map_err(|e| format!("{plugin}.settings: {e}"))?;
    let Value::Table(list) = raw else {
        return Ok(Vec::new());
    };
    let mut settings = Vec::new();
    for (index, entry) in list.sequence_values::<Table>().enumerate() {
        let entry = entry.map_err(|e| format!("{plugin}.settings[{}]: {e}", index + 1))?;
        let where_ = format!("{plugin}.settings[{}]", index + 1);
        let id: String = entry
            .get::<Option<String>>("id")
            .map_err(|e| format!("{where_}.id: {e}"))?
            .ok_or_else(|| format!("{where_}: needs an id"))?;
        let description: String = entry
            .get::<Option<String>>("desc")
            .map_err(|e| format!("{where_}.desc: {e}"))?
            .unwrap_or_default();
        let default = match entry.get::<Value>("default") {
            Ok(Value::Boolean(b)) => SettingValue::Bool(b),
            Ok(Value::Integer(n)) => SettingValue::Number(n as f64),
            Ok(Value::Number(n)) => SettingValue::Number(n),
            Ok(Value::String(s)) => SettingValue::Text(s.to_string_lossy()),
            _ => {
                return Err(format!(
                    "{where_}: needs a boolean, number or string default"
                ))
            }
        };
        settings.push(Setting {
            plugin: plugin.to_string(),
            id,
            description,
            default: default.clone(),
            value: default,
        });
    }
    Ok(settings)
}

/// Read a plugin's `pills` declaration — its entries in the action band.
///
/// Data, like `keys` and `settings`, for the same reason: the band must be able
/// to enumerate every entry without invoking any plugin, because drawing chrome
/// that can call into Lua is chrome a plugin can break.
fn read_pills(def: &Table, plugin: &str) -> Result<Vec<Pill>, String> {
    let raw: Value = def
        .get("pills")
        .map_err(|e| format!("{plugin}.pills: {e}"))?;
    let Value::Table(list) = raw else {
        return Ok(Vec::new());
    };
    let mut pills = Vec::new();
    for (index, entry) in list.sequence_values::<Table>().enumerate() {
        let entry = entry.map_err(|e| format!("{plugin}.pills[{}]: {e}", index + 1))?;
        let where_ = format!("{plugin}.pills[{}]", index + 1);
        let action: String = entry
            .get::<Option<String>>("action")
            .map_err(|e| format!("{where_}.action: {e}"))?
            .ok_or_else(|| format!("{where_}: needs an action"))?;
        let label: String = entry
            .get::<Option<String>>("label")
            .map_err(|e| format!("{where_}.label: {e}"))?
            .ok_or_else(|| format!("{where_}: needs a label"))?;
        // Unprioritised entries sort last among themselves, which is a sane
        // default for a pane that does not care where it lands.
        let priority: i64 = entry
            .get::<Option<i64>>("priority")
            .map_err(|e| format!("{where_}.priority: {e}"))?
            .unwrap_or(0);
        pills.push(Pill {
            plugin: plugin.to_string(),
            action,
            label,
            priority,
        });
    }
    Ok(pills)
}
