use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use mlua::{Function, Lua, StdLib, Table, Value};
use ratatui::layout::Rect;

use super::command::{Command, InFlight};
use super::convert;
use super::events::{Event, Field};
use super::layout::{Region, SlotMode};
use super::node::{Node, Size};
use super::registry::{ActionDecl, Binding, CommandDecl, Pill, Registry, Setting};
use super::snapshot::Snapshot;
use super::theme::Themes;

mod api;
mod load;
mod publish;

use self::api::{clean_error, install_api, Env, EnvParts};
pub(crate) use self::api::{columns, take_left};
use self::load::{load_arrangement, load_mod, new_vm, read_float, Budget};
use self::publish::run_to_lua;
use super::perf::Hook;

pub const INSTRUCTION_BUDGET: u32 = 20_000_000;

static BUDGET_OVERRIDE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn set_instruction_budget(instructions: u32) {
    BUDGET_OVERRIDE.store(instructions, std::sync::atomic::Ordering::Relaxed);
}

pub fn instruction_budget() -> u32 {
    match BUDGET_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => INSTRUCTION_BUDGET,
        set => set,
    }
}

static MEMORY_OVERRIDE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn set_memory_limit(bytes: usize) {
    MEMORY_OVERRIDE.store(bytes, std::sync::atomic::Ordering::Relaxed);
}

pub fn memory_limit() -> usize {
    match MEMORY_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => MEMORY_LIMIT,
        set => set,
    }
}

pub const MEMORY_LIMIT: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryPressure {
    Exhausted,
    Grown,
}

fn mod_stdlib() -> StdLib {
    // UTF8 is pure computation with no capability risk, and text handling
    // needs it: Lua's `#` counts bytes, so without it a mod measuring a name
    // like "Rosé Pine" pads it one column short.
    StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::COROUTINE | StdLib::UTF8
}

#[derive(Clone, Debug, PartialEq)]
enum Persisted {
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Table(Vec<(Persisted, Persisted)>),
}

type Shared = Rc<RefCell<BTreeMap<String, Persisted>>>;
type WriteCount = Rc<std::cell::Cell<(u64, u64)>>;
type StateVersion = Rc<std::cell::Cell<u64>>;
type Private = Rc<RefCell<BTreeMap<(String, String), Persisted>>>;
type Queue = Rc<RefCell<Vec<(Vec<String>, Command)>>>;

pub type RunAnswers = std::collections::HashMap<String, Vec<(String, super::runs::Run)>>;
type Roots = Rc<RefCell<BTreeMap<String, PathBuf>>>;
type FileWants = Rc<RefCell<BTreeMap<String, std::collections::HashSet<super::files::ReadKey>>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Load,
    Render,
    Key,
    Event,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Load => "load",
            Phase::Render => "render",
            Phase::Key => "key",
            Phase::Event => "event",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModError {
    pub owner: String,
    pub phase: Phase,
    pub message: String,
}

impl std::fmt::Display for ModError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            self.owner,
            self.phase.as_str(),
            self.message
        )
    }
}

enum Arrangement {
    Missing,
    Static(Region),
    Dynamic(Function),
}

pub struct Mod {
    pub name: String,
    pub file: String,
    pub path: String,
    pub slot: String,
    pub focusable: bool,
    pub pure: bool,
    pub session_input: bool,
    pub size: Size,
    pub decorates: Option<String>,
    pub floats: bool,
    pub strip: bool,
    pub bindings: Vec<Binding>,
    pub settings: Vec<Setting>,
    pub pills: Vec<Pill>,
    pub capabilities: Vec<Capability>,
    pub events: Vec<String>,
    pub commands: Vec<CommandDecl>,
    pub actions: Vec<ActionDecl>,
    order: f64,
    def: Table,
    env: Env,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    Run,
    Program,
    Sessions,
    Files,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Run => "run",
            Capability::Program => "program",
            Capability::Sessions => "sessions",
            Capability::Files => "files",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Capability::Run => "runs programs and reads their output",
            Capability::Program => "runs a program you interact with",
            Capability::Sessions => "starts, types into and deletes sessions, edits the interface",
            Capability::Files => "reads the files in a session's directory",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "run" => Some(Capability::Run),
            "program" => Some(Capability::Program),
            "sessions" => Some(Capability::Sessions),
            "files" => Some(Capability::Files),
            _ => None,
        }
    }

    pub const ALL: [Capability; 4] = [
        Capability::Run,
        Capability::Program,
        Capability::Sessions,
        Capability::Files,
    ];
}

#[derive(Debug, Clone, Default)]
pub struct KeyPress {
    pub name: String,
    pub ch: Option<char>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Click {
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub role: Option<String>,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub screen_x: u16,
    pub screen_y: u16,
    pub dragging: bool,
    pub clicks: u8,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Scroll {
    pub up: bool,
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Float {
    pub width_pct: f64,
    pub height_pct: f64,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub at: Option<(u16, u16)>,
}

impl Default for Float {
    fn default() -> Self {
        Self {
            width_pct: 60.0,
            height_pct: 60.0,
            cols: None,
            rows: None,
            at: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Rendered {
    pub node: std::rc::Rc<Node>,
    pub float: Option<Float>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Epoch {
    pub snapshot: u64,
    pub themes: u64,
    pub registry: u64,
    pub meta: u64,
    pub failed: u64,
    pub data: u64,
    pub animation: u64,
    pub printing: u64,
    pub taken_at: u64,
    pub metrics: u64,
}

type GroupKey = [u64; 4];

pub const ANIMATION_HZ: f64 = 8.0;

type TreeKey = (Epoch, u16, u16, bool, u64);

struct CachedTree {
    key: TreeKey,
    rendered: Rendered,
    reads_clock: bool,
    reads_sample: u8,
}

impl CachedTree {
    fn answers(&self, want: &TreeKey) -> bool {
        let (mut mine, mut theirs) = (self.key, *want);
        if !self.reads_clock {
            mine.0.animation = 0;
            theirs.0.animation = 0;
        }
        if self.reads_sample & api::READ_TAKEN_AT == 0 {
            mine.0.taken_at = 0;
            theirs.0.taken_at = 0;
        }
        if self.reads_sample & api::READ_METRICS == 0 {
            mine.0.metrics = 0;
            theirs.0.metrics = 0;
        }
        mine == theirs
    }
}

impl Epoch {
    pub fn always_fresh() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Self {
            snapshot: n,
            themes: n,
            registry: n,
            meta: n,
            failed: n,
            data: n,
            animation: n,
            printing: n,
            taken_at: n,
            metrics: n,
        }
    }
}

pub struct Published<'a> {
    pub epoch: Epoch,
    pub snapshot: &'a Snapshot,
    pub attach_errors: &'a std::collections::HashMap<String, String>,
    pub starts: &'a std::collections::HashMap<String, super::terminal::Start>,
    pub printing: &'a std::collections::HashSet<String>,
    pub inflight: &'a [InFlight],
    pub themes: &'a Themes,
    pub registry: &'a Registry,
    pub diffs: &'a super::diff::DiffStore,
    pub links: &'a std::collections::HashMap<String, Vec<(String, usize, usize)>>,
    pub search: Option<&'a super::search::Answer>,
    pub metrics: &'a super::metrics::Metrics,
    pub status_rows: u16,
    pub meta: &'a std::collections::HashMap<String, super::terminal::AgentMeta>,
    pub can_open: bool,
    pub hovered: Option<&'a super::node::Identity>,
    pub focus: Option<&'a str>,
    pub selection: Option<&'a str>,
    pub inventory: &'a [super::inventory::Row],
    pub ui_dir: &'a str,
    pub settings: &'a crate::session::settings::Settings,
    pub repos: &'a super::repos::RepoStore,
    pub wants: &'a super::repos::Wants,
}

#[derive(Debug, Clone, Copy)]
pub struct RenderContext {
    pub width: u16,
    pub height: u16,
    pub focused: bool,
    pub elapsed: f64,
    pub frame: u64,
}

pub struct LuaHost {
    lua: Lua,
    ui_dir: PathBuf,
    epoch: RefCell<Option<Epoch>>,
    skipped_renders: std::cell::Cell<u64>,
    resident: std::cell::Cell<usize>,
    pressure_collections: std::cell::Cell<u64>,
    collected: std::cell::Cell<usize>,
    reused_groups: std::cell::Cell<u64>,
    perf_timing: std::cell::Cell<bool>,
    mod_perf: RefCell<super::perf::ModTable>,
    idle: std::cell::Cell<bool>,
    op_open: std::cell::Cell<bool>,
    op_heaviest: RefCell<Option<(String, std::time::Duration)>>,
    store_writes: WriteCount,
    trees: RefCell<HashMap<usize, CachedTree>>,
    clock: Rc<std::cell::Cell<f64>>,
    clock_read: Rc<std::cell::Cell<bool>>,
    sample_read: Rc<std::cell::Cell<u8>>,
    rendered_reads: std::cell::Cell<u8>,
    groups: RefCell<HashMap<&'static str, (GroupKey, Value)>>,
    store: Shared,
    state: Private,
    state_version: StateVersion,
    current: Rc<RefCell<String>>,
    current_path: Rc<RefCell<String>>,
    roots: Roots,
    file_reads: Rc<RefCell<super::files::ReadStore>>,
    file_wants: FileWants,
    file_rendered: RefCell<BTreeSet<String>>,
    roots_snapshot: std::cell::Cell<Option<u64>>,
    queue: Queue,
    behalf: Rc<RefCell<Option<Vec<String>>>>,
    runs: Rc<RefCell<Vec<(String, super::runs::Request)>>>,
    trusted: Rc<RefCell<Vec<String>>>,
    disabled: Rc<RefCell<Vec<String>>>,
    run_answers: Rc<RefCell<RunAnswers>>,
    pub mods: Vec<Mod>,
    index: ModIndex,
    layout: Arrangement,
    layout_cache: RefCell<Option<(LayoutKey, std::rc::Rc<Region>)>>,
    last_status_rows: std::cell::Cell<u16>,
    pub error: Option<String>,
    pub reloads: u32,
    key_releases: std::cell::Cell<KeyReleases>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyReleases {
    #[default]
    Unsupported,
    Negotiated,
    Reported,
}

impl KeyReleases {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyReleases::Unsupported => "unsupported",
            KeyReleases::Negotiated => "negotiated",
            KeyReleases::Reported => "reported",
        }
    }
}

type LayoutKey = (u16, u16, u32, Option<Epoch>, u64, u16);

#[derive(Default)]
struct ModIndex {
    focusable: Vec<usize>,
    floating: Vec<usize>,
    slots: HashMap<String, Vec<usize>>,
    decorators: HashMap<String, Vec<usize>>,
    modes: HashMap<String, SlotMode>,
}

impl ModIndex {
    fn build(mods: &[Mod]) -> Self {
        let mut index = Self::default();
        for (i, loaded) in mods.iter().enumerate() {
            if loaded.focusable {
                index.focusable.push(i);
            }
            if loaded.floats {
                index.floating.push(i);
            }
            match &loaded.decorates {
                Some(slot) => index.decorators.entry(slot.clone()).or_default().push(i),
                None => index.slots.entry(loaded.slot.clone()).or_default().push(i),
            }
            // `slot_mode` is a static declaration, read once here instead of
            // through the Lua table on every placement query.
            if let Ok(Value::String(mode)) = loaded.def.get::<Value>("slot_mode") {
                if mode.to_string_lossy() == "switch" {
                    index.modes.insert(loaded.slot.clone(), SlotMode::Switch);
                }
            }
        }
        index
    }
}

impl LuaHost {
    pub fn new(ui_dir: impl Into<PathBuf>) -> Self {
        Self::new_with_disabled(ui_dir, Vec::new())
    }

    pub fn fresh_from(&self, ui_dir: impl Into<PathBuf>) -> Self {
        Self::new_with_disabled(ui_dir, self.disabled.borrow().clone())
    }

    pub fn retired(&self, error: String) -> Self {
        let mut host = Self::unbuilt(self.ui_dir.clone(), self.disabled.borrow().clone());
        host.error = Some(error);
        host
    }

    fn new_with_disabled(ui_dir: impl Into<PathBuf>, disabled: Vec<String>) -> Self {
        let mut host = Self::unbuilt(ui_dir.into(), disabled);
        host.reload();
        host
    }

    fn unbuilt(ui_dir: PathBuf, disabled: Vec<String>) -> Self {
        Self {
            lua: new_vm().unwrap_or_else(|_| Lua::new()),
            ui_dir,
            groups: RefCell::new(HashMap::new()),
            trees: RefCell::new(HashMap::new()),
            clock: Rc::new(std::cell::Cell::new(0.0)),
            clock_read: Rc::new(std::cell::Cell::new(false)),
            sample_read: Rc::new(std::cell::Cell::new(0)),
            rendered_reads: std::cell::Cell::new(0),
            epoch: RefCell::new(None),
            skipped_renders: std::cell::Cell::new(0),
            resident: std::cell::Cell::new(0),
            pressure_collections: std::cell::Cell::new(0),
            collected: std::cell::Cell::new(0),
            reused_groups: std::cell::Cell::new(0),
            key_releases: std::cell::Cell::new(KeyReleases::default()),
            perf_timing: std::cell::Cell::new(false),
            mod_perf: RefCell::new(super::perf::ModTable::default()),
            idle: std::cell::Cell::new(false),
            op_open: std::cell::Cell::new(false),
            op_heaviest: RefCell::new(None),
            store_writes: Rc::new(std::cell::Cell::new((0, 0))),
            store: Shared::default(),
            state: Private::default(),
            state_version: StateVersion::default(),
            current: Rc::new(RefCell::new(String::new())),
            current_path: Rc::new(RefCell::new(String::new())),
            queue: Queue::default(),
            behalf: Rc::new(RefCell::new(None)),
            runs: Rc::new(RefCell::new(Vec::new())),
            trusted: Rc::new(RefCell::new(Vec::new())),
            disabled: Rc::new(RefCell::new(disabled)),
            run_answers: Rc::new(RefCell::new(std::collections::HashMap::new())),
            roots: Roots::default(),
            file_reads: Rc::new(RefCell::new(super::files::store())),
            file_wants: FileWants::default(),
            file_rendered: RefCell::default(),
            roots_snapshot: std::cell::Cell::new(None),
            mods: Vec::new(),
            index: ModIndex::default(),
            layout: Arrangement::Missing,
            layout_cache: RefCell::new(None),
            last_status_rows: std::cell::Cell::new(0),
            error: None,
            reloads: 0,
        }
    }

    pub fn reload_from(&mut self, ui_dir: impl Into<PathBuf>) {
        self.ui_dir = ui_dir.into();
        self.reload();
    }

    pub fn reload(&mut self) {
        match self.build() {
            Ok((lua, mods, layout)) => {
                // Before the VM they point into is dropped: a cached group is a
                // handle into `self.lua`, so carrying one across a reload would
                // hand the next publish a value belonging to a Lua that no
                // longer exists.
                self.forget_groups();
                self.file_wants.borrow_mut().clear();
                self.lua = lua;
                self.index = ModIndex::build(&mods);
                self.mods = mods;
                self.layout = layout;
                self.layout_cache.replace(None);
                self.error = None;
                self.reloads += 1;
                self.accept_resident_memory();
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn build(&self) -> Result<(Lua, Vec<Mod>, Arrangement), String> {
        let lua = new_vm().map_err(|e| e.to_string())?;
        install_api(
            &lua,
            self.store.clone(),
            self.state_version.clone(),
            self.clock.clone(),
            self.clock_read.clone(),
            self.sample_read.clone(),
            self.store_writes.clone(),
        )
        .map_err(|e| e.to_string())?;
        let parts = EnvParts {
            ui_dir: self.ui_dir.clone(),
            state: self.state.clone(),
            queue: self.queue.clone(),
            roots: self.roots.clone(),
            file_reads: self.file_reads.clone(),
            file_wants: self.file_wants.clone(),
            runs: self.runs.clone(),
            version: self.state_version.clone(),
            writes: self.store_writes.clone(),
            behalf: self.behalf.clone(),
            entered: self.current_path.clone(),
        };

        let dir = self.ui_dir.join(crate::session::mod_spec::MODS_DIR);
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "lua"))
            .collect();
        // Sorted so `order` ties break by filename, deterministically.
        files.sort();

        let nested: Vec<String> = super::packages::read_spec(&self.ui_dir)
            .map(|spec| {
                spec.mods
                    .into_iter()
                    .map(|entry| entry.file)
                    .filter(|file| crate::session::mod_spec::is_nested_pane(file))
                    .collect()
            })
            .unwrap_or_default();

        let mut mods = Vec::new();
        let disabled = self.disabled.borrow();
        for relative in nested {
            let path = self.ui_dir.join(&relative);
            if !path.is_file() || disabled.iter().any(|off| off == &relative) {
                continue;
            }
            mods.push(load_mod(&lua, &parts, &path, &relative)?);
        }
        for path in files {
            let name = path
                .file_name()
                .map(std::ffi::OsStr::to_string_lossy)
                .unwrap_or_default();
            let relative = format!("{}/{name}", crate::session::mod_spec::MODS_DIR);
            if disabled.iter().any(|off| off == &relative) {
                continue;
            }
            mods.push(load_mod(&lua, &parts, &path, &relative)?);
        }
        drop(disabled);
        // An EMPTY directory is not an error: removing every pane is something
        // a user may do, and faulting here would summon the recovery floor,
        // which would deliver the bundled interface again — the system undoing
        // the removal it was just asked to make (design D6). A *missing*
        // directory still is one: delivery always creates it, so its absence
        // means the interface directory is not one.
        mods.sort_by(|a, b| a.order.total_cmp(&b.order));
        let mut actions = std::collections::HashMap::<String, (String, String)>::new();
        for loaded in &mods {
            for action in loaded
                .bindings
                .iter()
                .map(|row| &row.action)
                .chain(loaded.commands.iter().map(|row| &row.action))
                .chain(loaded.actions.iter().map(|row| &row.name))
            {
                if let Some((owner, path)) =
                    actions.insert(action.clone(), (loaded.name.clone(), loaded.path.clone()))
                {
                    if owner != loaded.name
                        || (path != loaded.path
                            && super::registry::is_guarded_session_action(action))
                    {
                        return Err(format!(
                            "action {action} is declared by both {owner} and {}",
                            loaded.name
                        ));
                    }
                }
            }
        }

        let layout = load_arrangement(&lua, &parts)?;
        Ok((lua, mods, layout))
    }

    pub fn declarations(&self) -> (Vec<Binding>, Vec<Setting>) {
        let (bindings, settings, _) = self.all_declarations();
        (bindings, settings)
    }

    pub fn all_declarations(&self) -> (Vec<Binding>, Vec<Setting>, Vec<Pill>) {
        let mut bindings = Vec::new();
        let mut settings = Vec::new();
        let mut pills = Vec::new();
        for loaded in &self.mods {
            bindings.extend(loaded.bindings.iter().cloned());
            settings.extend(loaded.settings.iter().cloned());
            pills.extend(loaded.pills.iter().cloned());
        }
        (bindings, settings, pills)
    }

    pub fn on_behalf_of<T>(&self, issuer: &str, f: impl FnOnce() -> T) -> T {
        let previous = self.acting_for(Some(vec![issuer.to_string()]));
        let out = f();
        self.acting_for(previous);
        out
    }

    pub fn acting_for(&self, chain: Option<Vec<String>>) -> Option<Vec<String>> {
        std::mem::replace(&mut *self.behalf.borrow_mut(), chain)
    }

    pub fn on_action(&self, index: usize, action: &str) -> Result<bool, ModError> {
        self.on_action_with_args(index, action, &[])
    }

    pub fn action_handler_present(&self, index: usize) -> bool {
        self.mods.get(index).is_some_and(|loaded| {
            matches!(loaded.def.get::<Value>("on_action"), Ok(Value::Function(_)))
        })
    }

    pub fn raw_gesture_handlers(&self, index: usize) -> Vec<&'static str> {
        let Some(loaded) = self.mods.get(index) else {
            return Vec::new();
        };
        ["on_key", "on_click", "on_scroll"]
            .into_iter()
            .filter(|name| matches!(loaded.def.get::<Value>(*name), Ok(Value::Function(_))))
            .collect()
    }

    pub fn on_action_with_args(
        &self,
        index: usize,
        action: &str,
        args: &[(&str, &str)],
    ) -> Result<bool, ModError> {
        let Some(loaded) = self.mods.get(index) else {
            return Ok(false);
        };
        let fail = |message: String| ModError {
            owner: loaded.name.clone(),
            phase: Phase::Key,
            message,
        };
        let handler: Value = loaded
            .def
            .get("on_action")
            .map_err(|e| fail(e.to_string()))?;
        let Value::Function(handler) = handler else {
            return Ok(false);
        };

        let start = self.call_started();
        self.enter(loaded);
        let guard = Budget::arm(&self.lua);
        let handled: Result<bool, mlua::Error> = if let Err(e) = &guard {
            Err(e.clone())
        } else if args.is_empty() {
            handler.call(action.to_string())
        } else {
            let data = self.lua.create_table().map_err(|e| fail(clean_error(&e)))?;
            for (key, value) in args {
                data.set(*key, *value).map_err(|e| fail(clean_error(&e)))?;
            }
            let arguments = self.lua.create_table().map_err(|e| fail(clean_error(&e)))?;
            let metatable = self.lua.create_table().map_err(|e| fail(clean_error(&e)))?;
            metatable
                .set("__index", data)
                .map_err(|e| fail(clean_error(&e)))?;
            let deny = self
                .lua
                .create_function(|_, (_table, _key, _value): (Value, Value, Value)| {
                    Err::<(), _>(mlua::Error::RuntimeError(
                        "action arguments are read-only".into(),
                    ))
                })
                .map_err(|e| fail(clean_error(&e)))?;
            metatable
                .set("__newindex", deny)
                .map_err(|e| fail(clean_error(&e)))?;
            metatable
                .set("__metatable", false)
                .map_err(|e| fail(clean_error(&e)))?;
            arguments
                .set_metatable(Some(metatable))
                .map_err(|e| fail(clean_error(&e)))?;
            handler.call((action.to_string(), arguments))
        };
        drop(guard);
        self.hook_finished(loaded, start, Hook::Action, handled.is_err());
        handled.map_err(|e| fail(clean_error(&e)))
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.mods.iter().position(|p| p.name == name)
    }

    pub fn name_of_path(&self, path: &str) -> Option<&str> {
        self.mods
            .iter()
            .find(|loaded| loaded.path == path)
            .map(|loaded| loaded.name.as_str())
    }

    pub fn commands(&self) -> Vec<CommandDecl> {
        self.mods
            .iter()
            .flat_map(|loaded| loaded.commands.iter().cloned())
            .collect()
    }

    pub fn action_declarations(&self) -> Vec<ActionDecl> {
        self.mods
            .iter()
            .flat_map(|loaded| loaded.actions.iter().cloned())
            .collect()
    }

    pub fn subscribers(&self, event: &str) -> Vec<usize> {
        self.mods
            .iter()
            .enumerate()
            .filter(|(_, loaded)| loaded.events.iter().any(|name| name == event))
            .map(|(index, _)| index)
            .collect()
    }

    pub fn dispatch_event(&self, event: &Event) -> Vec<ModError> {
        let subscribers = match &event.only {
            None => self.subscribers(&event.name),
            // Addressed. Filtered on `path` because that is the identity a
            // program pane is keyed by — trust, the disabled set and `run`'s
            // attribution all use the same one, and a declared `name` is
            // neither unique nor stable.
            Some(path) => self
                .subscribers(&event.name)
                .into_iter()
                .filter(|index| {
                    self.mods
                        .get(*index)
                        .is_some_and(|loaded| loaded.path == *path)
                })
                .collect(),
        };
        if subscribers.is_empty() {
            return Vec::new();
        }
        let mut failures = Vec::new();
        let payload = match self.payload_table(event) {
            Ok(table) => table,
            Err(message) => {
                failures.push(ModError {
                    owner: "kernel".to_string(),
                    phase: Phase::Event,
                    message: format!("{}: {message}", event.name),
                });
                return failures;
            }
        };
        for index in subscribers {
            if let Err(e) = self.on_event(index, &event.name, &payload) {
                failures.push(e);
            }
        }
        failures
    }

    fn on_event(&self, index: usize, name: &str, payload: &Table) -> Result<(), ModError> {
        let Some(loaded) = self.mods.get(index) else {
            return Ok(());
        };
        let fail = |message: String| ModError {
            owner: loaded.name.clone(),
            phase: Phase::Event,
            message: format!("{name}: {message}"),
        };
        let handler: Value = loaded
            .def
            .get("on_event")
            .map_err(|e| fail(e.to_string()))?;
        let Value::Function(handler) = handler else {
            return Ok(());
        };

        let start = self.call_started();
        self.enter(loaded);
        let outcome: Result<Value, mlua::Error> = Budget::arm(&self.lua)
            .and_then(|_budget| handler.call((name.to_string(), payload.clone())));
        self.hook_finished(loaded, start, Hook::Event, outcome.is_err());
        outcome.map(|_| ()).map_err(|e| fail(clean_error(&e)))
    }

    fn payload_table(&self, event: &Event) -> Result<Table, String> {
        let table = self.lua.create_table().map_err(|e| e.to_string())?;
        for (key, value) in &event.payload {
            let value = match value {
                Field::Text(text) => {
                    Value::String(self.lua.create_string(text).map_err(|e| e.to_string())?)
                }
                Field::Bool(flag) => Value::Boolean(*flag),
                // An integral number goes in as an integer, or `count = 2` reads
                // back as `2.0` — the same care `state` takes with its values.
                Field::Number(n) if n.fract() == 0.0 && n.abs() < (i64::MAX as f64) => {
                    Value::Integer(*n as i64)
                }
                Field::Number(n) => Value::Number(*n),
                Field::List(items) => {
                    let list = self.lua.create_table().map_err(|e| e.to_string())?;
                    for (i, item) in items.iter().enumerate() {
                        list.set(i + 1, item.clone()).map_err(|e| e.to_string())?;
                    }
                    Value::Table(list)
                }
            };
            table.set(key.as_str(), value).map_err(|e| e.to_string())?;
        }
        Ok(table)
    }

    pub fn in_slot(&self, slot: &str) -> &[usize] {
        self.index.slots.get(slot).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn decorators_of(&self, slot: &str) -> &[usize] {
        self.index
            .decorators
            .get(slot)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn decorate(
        &self,
        index: usize,
        node: &Node,
        ctx: RenderContext,
    ) -> Result<Node, ModError> {
        let Some(loaded) = self.mods.get(index) else {
            return Ok(node.clone());
        };
        let fail = |message: String| ModError {
            owner: loaded.name.clone(),
            phase: Phase::Render,
            message,
        };
        let handler: Value = loaded
            .def
            .get("decorate")
            .map_err(|e| fail(e.to_string()))?;
        let Value::Function(handler) = handler else {
            return Ok(node.clone());
        };

        let table = self.lua.create_table().map_err(|e| fail(e.to_string()))?;
        table
            .set("width", ctx.width)
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("height", ctx.height)
            .map_err(|e| fail(e.to_string()))?;

        let tree = convert::to_lua(&self.lua, node).map_err(fail)?;

        let start = self.call_started();
        self.enter(loaded);
        let result: Result<Value, mlua::Error> =
            Budget::arm(&self.lua).and_then(|_budget| handler.call((tree, table)));
        self.hook_finished(loaded, start, Hook::Decorate, result.is_err());

        let value = result.map_err(|e| fail(clean_error(&e)))?;
        convert::to_node(&value, &loaded.path).map_err(fail)
    }

    pub fn shared_string(&self, key: &str) -> Option<String> {
        match self.store.borrow().get(key) {
            Some(Persisted::Str(text)) => Some(text.clone()),
            _ => None,
        }
    }

    pub fn ui_states(&self) -> std::collections::BTreeMap<String, serde_json::Value> {
        let mut states = std::collections::BTreeMap::new();
        for loaded in &self.mods {
            let Ok(Some(project)) = loaded.def.get::<Option<Function>>("ui_state") else {
                continue;
            };
            self.enter(loaded);
            let result: mlua::Result<Table> =
                load::Budget::arm(&self.lua).and_then(|_budget| project.call(()));
            self.enter_nothing();
            let Ok(table) = result else { continue };
            let mut fields = serde_json::Map::new();
            for entry in table.pairs::<String, Value>().take(17) {
                let Ok((key, value)) = entry else { break };
                if fields.len() == 16 || key.len() > 64 {
                    break;
                }
                let value = match value {
                    Value::Boolean(v) => serde_json::Value::Bool(v),
                    Value::Integer(v) => serde_json::json!(v),
                    Value::Number(v) if v.is_finite() => serde_json::json!(v),
                    Value::String(v) if v.as_bytes().len() <= 256 => {
                        serde_json::Value::String(v.to_string_lossy().to_string())
                    }
                    _ => continue,
                };
                fields.insert(key, value);
            }
            states.insert(loaded.path.clone(), serde_json::Value::Object(fields));
        }
        states
    }

    pub fn ui_state_version(&self) -> u64 {
        self.state_version.get()
    }

    pub fn shared_bool(&self, key: &str) -> Option<bool> {
        match self.store.borrow().get(key) {
            Some(Persisted::Bool(value)) => Some(*value),
            _ => None,
        }
    }

    pub fn set_shared_string(&self, key: &str, value: &str) {
        let moved = {
            let mut store = self.store.borrow_mut();
            let next = Persisted::Str(value.to_string());
            if store.get(key) == Some(&next) {
                false
            } else {
                store.insert(key.to_string(), next);
                true
            }
        };
        if moved {
            self.state_version
                .set(self.state_version.get().wrapping_add(1));
        }
    }

    pub fn set_published_selection(&self, selection: &str) {
        // Into the backing table: the published proxy refuses every write.
        if let Some(table) = api::published_backing(&self.lua) {
            let _ = table.raw_set("selection", selection);
        }
    }

    pub fn set_shared_bool(&self, key: &str, value: bool) {
        let moved = {
            let mut store = self.store.borrow_mut();
            let next = Persisted::Bool(value);
            if store.get(key) == Some(&next) {
                false
            } else {
                store.insert(key.to_string(), next);
                true
            }
        };
        if moved {
            self.state_version
                .set(self.state_version.get().wrapping_add(1));
        }
    }

    pub fn floating(&self) -> &[usize] {
        &self.index.floating
    }

    pub fn focusable(&self) -> &[usize] {
        &self.index.focusable
    }

    fn enter_nothing(&self) {
        self.current.borrow_mut().clear();
        self.current_path.borrow_mut().clear();
    }

    #[doc(hidden)]
    pub fn reachable_globals(&self, index: usize) -> Vec<String> {
        let mut names = std::collections::BTreeSet::new();
        let mut collect = |table: Table| {
            for (key, _) in table.pairs::<Value, Value>().flatten() {
                if let Some(name) = key.as_string() {
                    names.insert(name.to_string_lossy());
                }
            }
        };
        if let Some(loaded) = self.mods.get(index) {
            collect(loaded.env.table.clone());
        }
        collect(self.lua.globals());
        names.into_iter().collect()
    }

    pub fn may(&self, loaded: &Mod, capability: Capability) -> bool {
        loaded.capabilities.contains(&capability) && self.trusted.borrow().contains(&loaded.path)
    }

    pub fn may_path(&self, path: &str, capability: Capability) -> bool {
        self.mods
            .iter()
            .find(|loaded| loaded.path == path)
            .is_some_and(|loaded| self.may(loaded, capability))
    }

    fn enter(&self, loaded: &Mod) {
        *self.current.borrow_mut() = loaded.file.clone();
        *self.current_path.borrow_mut() = loaded.path.clone();
        if loaded.capabilities.is_empty() {
            return;
        }
        self.publish_runs(loaded);
        self.publish_granted(loaded);
        let run = if self.may(loaded, Capability::Run) {
            Value::Function(loaded.env.run.clone())
        } else {
            Value::Nil
        };
        let _ = loaded.env.table.raw_set("run", run);
        let forget = if self.may(loaded, Capability::Run) {
            Value::Function(loaded.env.run_forget.clone())
        } else {
            Value::Nil
        };
        let _ = loaded.env.table.raw_set("run_forget", forget);
        let files = if self.may(loaded, Capability::Files) {
            Value::Table(loaded.env.files.clone())
        } else {
            Value::Nil
        };
        let _ = loaded.env.table.raw_set("files", files);
    }

    fn publish_runs(&self, loaded: &Mod) {
        let Ok(table) = self.lua.create_table() else {
            return;
        };
        if let Some(answers) = self.run_answers.borrow().get(&loaded.path) {
            for (key, run) in answers {
                if let Ok(entry) = run_to_lua(&self.lua, run) {
                    let _ = table.set(key.clone(), entry);
                }
            }
        }
        let _ = loaded.env.thurbox.raw_set("runs", table);
    }

    fn publish_granted(&self, loaded: &Mod) {
        let Ok(table) = self.lua.create_table() else {
            return;
        };
        for capability in Capability::ALL {
            if self.may(loaded, capability) {
                let _ = table.set(capability.as_str(), true);
            }
        }
        let _ = loaded.env.thurbox.raw_set("granted", table);
    }

    pub fn set_runs(&self, answers: RunAnswers) {
        *self.run_answers.borrow_mut() = answers;
    }

    pub fn set_disabled(&self, paths: Vec<String>) {
        *self.disabled.borrow_mut() = paths;
    }

    pub fn set_trusted(&self, paths: Vec<String>) {
        *self.trusted.borrow_mut() = paths;
    }

    pub fn poll_files(&self) -> bool {
        let roots = self.roots.borrow();
        let mut wants = self.file_wants.borrow_mut();
        wants.retain(|owner, keys| {
            if !self.may_path(owner, Capability::Files) {
                return false;
            }
            keys.retain(|(root, _)| roots.values().any(|current| current == root));
            !keys.is_empty()
        });
        let mut reads = self.file_reads.borrow_mut();
        let retired = reads.retain(|key| wants.values().any(|keys| keys.contains(key)));
        reads.poll() || retired
    }

    pub fn end_files_frame(&self) {
        let rendered = self.file_rendered.take();
        self.file_wants
            .borrow_mut()
            .retain(|owner, _| rendered.contains(owner));
    }

    pub fn drain_runs(&self) -> Vec<(String, super::runs::Ask)> {
        self.drain_run_requests()
            .into_iter()
            .filter_map(|(owner, request)| match request {
                super::runs::Request::Ask(ask) => Some((owner, ask)),
                super::runs::Request::Forget(_) => None,
            })
            .collect()
    }

    pub fn drain_run_requests(&self) -> Vec<(String, super::runs::Request)> {
        let mut asked = std::mem::take(&mut *self.runs.borrow_mut());
        asked.retain(|(owner, _)| self.mods.iter().any(|loaded| loaded.path == *owner));
        if self.perf_timing.get() {
            let mut table = self.mod_perf.borrow_mut();
            for (path, request) in &asked {
                if matches!(request, super::runs::Request::Ask(_)) {
                    table.stats_mut(path).asked += 1;
                }
            }
        }
        asked
    }

    pub fn drain_commands(&self) -> Vec<Command> {
        self.drain_issued()
            .into_iter()
            .map(|(_, command)| command)
            .collect()
    }

    pub fn drain_issued(&self) -> Vec<(String, Command)> {
        self.drain_requests()
            .into_iter()
            .map(|(mut chain, command)| (chain.swap_remove(0), command))
            .collect()
    }

    pub fn drain_requests(&self) -> Vec<(Vec<String>, Command)> {
        std::mem::take(&mut *self.queue.borrow_mut())
    }

    pub fn set_key_releases(&self, releases: KeyReleases) {
        self.key_releases.set(releases);
    }

    fn group(
        &self,
        name: &'static str,
        key: GroupKey,
        build: impl FnOnce() -> Result<Value, String>,
    ) -> Result<Value, String> {
        // Scoped so `build` can touch the cache without a double borrow.
        if let Some((built_at, value)) = self.groups.borrow().get(name) {
            if *built_at == key {
                self.reused_groups.set(self.reused_groups.get() + 1);
                return Ok(value.clone());
            }
        }
        // Frozen before it is cached: a group is reused across frames, so a
        // write into it would outlive the frame and reach every reader.
        let value = api::freeze(&self.lua, build()?).map_err(|e| e.to_string())?;
        self.groups.borrow_mut().insert(name, (key, value.clone()));
        Ok(value)
    }

    pub fn forget_groups(&self) {
        self.groups.borrow_mut().clear();
        self.trees.borrow_mut().clear();
    }

    pub fn skipped_renders(&self) -> u64 {
        self.skipped_renders.get()
    }

    pub fn used_memory(&self) -> usize {
        self.lua.used_memory()
    }

    pub fn near_memory_limit(&self) -> bool {
        self.memory_pressure().is_some()
    }

    pub fn memory_pressure(&self) -> Option<MemoryPressure> {
        let ceiling = memory_limit();
        let assess = |used: usize| {
            if used >= ceiling - ceiling / 32 {
                Some(MemoryPressure::Exhausted)
            } else if used >= ceiling / 10 * 9
                && used >= self.resident.get().saturating_add(ceiling / 20)
            {
                Some(MemoryPressure::Grown)
            } else {
                None
            }
        };
        let used = self.lua.used_memory();
        assess(used)?;
        // A live heap parked just under the growth threshold would otherwise
        // buy a full collection on every frame: the next waits for a share of
        // new garbage. Never inside the exhaustion margin, though: there a
        // failing allocation leaves no garbage to move the gate, so pacing
        // would hide a full VM for good.
        if used < ceiling - ceiling / 32 && used < self.collected.get().saturating_add(ceiling / 64)
        {
            return None;
        }
        // A collection can itself fail on a full heap; what is left is then
        // what gets measured, which is the answer either way.
        let _ = self.lua.gc_collect();
        self.pressure_collections
            .set(self.pressure_collections.get() + 1);
        let live = self.lua.used_memory();
        self.collected.set(live);
        assess(live)
    }

    pub fn pressure_collections(&self) -> u64 {
        self.pressure_collections.get()
    }

    pub fn accept_resident_memory(&self) {
        self.resident.set(self.lua.used_memory());
    }

    pub fn reused_groups(&self) -> u64 {
        self.reused_groups.get()
    }

    pub fn set_perf_timing(&self, on: bool) {
        if on && !self.perf_timing.get() {
            self.mod_perf.borrow_mut().clear();
        }
        self.perf_timing.set(on);
    }

    pub fn set_idle(&self, idle: bool) {
        self.idle.set(idle);
    }

    pub fn note_frame(&self, took: std::time::Duration) {
        if self.perf_timing.get() {
            self.mod_perf.borrow_mut().note_frame(took);
        }
    }

    pub fn reset_mod_perf(&self) {
        self.mod_perf.borrow_mut().clear();
    }

    pub fn begin_op(&self) {
        self.op_open.set(true);
        self.op_heaviest.replace(None);
    }

    pub fn end_op(&self) -> Option<String> {
        self.op_open.set(false);
        self.op_heaviest.take().map(|(name, _)| name)
    }

    pub fn mod_report(&self) -> super::perf::ModReport {
        super::perf::mod_report(
            &self.mod_perf.borrow(),
            self.mods.iter().map(|loaded| super::perf::ModMeta {
                name: &loaded.name,
                path: &loaded.path,
                pure: loaded.pure,
                floats: loaded.floats,
            }),
        )
    }

    pub fn note_run(&self, event: &super::runs::RunEvent) {
        if self.perf_timing.get() {
            self.mod_perf.borrow_mut().note_run(event);
        }
    }

    fn call_started(&self) -> Option<std::time::Instant> {
        (self.perf_timing.get() || self.op_open.get()).then(std::time::Instant::now)
    }

    fn call_finished(
        &self,
        loaded: &Mod,
        start: Option<std::time::Instant>,
    ) -> Option<std::time::Duration> {
        let took = start?.elapsed();
        if self.op_open.get() {
            let mut heaviest = self.op_heaviest.borrow_mut();
            if heaviest.as_ref().is_none_or(|(_, longest)| took > *longest) {
                *heaviest = Some((loaded.name.clone(), took));
            }
        }
        self.perf_timing.get().then_some(took)
    }

    fn hook_finished(
        &self,
        loaded: &Mod,
        start: Option<std::time::Instant>,
        hook: Hook,
        failed: bool,
    ) {
        let Some(took) = self.call_finished(loaded, start) else {
            return;
        };
        let mut table = self.mod_perf.borrow_mut();
        let stats = table.stats_mut(&loaded.path);
        stats.hooks.stat_mut(hook).record(took);
        stats.failures += u64::from(failed);
    }

    pub fn take_diffs_read(&self) -> bool {
        self.rendered_reads.take() & api::READ_DIFFS != 0
    }

    fn published_epoch(&self) -> Option<Epoch> {
        *self.epoch.borrow()
    }

    pub fn render(&self, index: usize, ctx: RenderContext) -> Result<Rendered, ModError> {
        let loaded = self.mods.get(index).ok_or_else(|| ModError {
            owner: format!("#{index}"),
            phase: Phase::Render,
            message: "mod index out of range".to_string(),
        })?;

        if loaded.capabilities.contains(&Capability::Files) {
            self.file_rendered.borrow_mut().insert(loaded.path.clone());
        }

        // A pure pane asked at the same epoch, in the same rect, with the same
        // focus, would return what it returned last time — so return that,
        // skipping both the Lua call and the conversion. Measured, this is the
        // largest single cost in a frame: the session list rebuilt a
        // byte-identical tree on every frame under load (`frame-cost`).
        let key = self.published_epoch().map(|epoch| {
            (
                epoch,
                ctx.width,
                ctx.height,
                ctx.focused,
                self.state_version.get(),
            )
        });
        if loaded.pure {
            if let Some(key) = key {
                if let Some(cached) = self.trees.borrow().get(&index) {
                    if cached.answers(&key) {
                        self.rendered_reads
                            .set(self.rendered_reads.get() | cached.reads_sample);
                        self.skipped_renders.set(self.skipped_renders.get() + 1);
                        if self.perf_timing.get() {
                            self.mod_perf.borrow_mut().stats_mut(&loaded.path).reused += 1;
                        }
                        return Ok(cached.rendered.clone());
                    }
                }
            }
        }

        let start = self.call_started();
        let writes = self.store_writes.get();
        let result = self.render_lua(index, loaded, ctx, key);
        self.rendered_reads
            .set(self.rendered_reads.get() | self.sample_read.get());
        if let Some(took) = self.call_finished(loaded, start) {
            let (all, tables) = self.store_writes.get();
            let mut table = self.mod_perf.borrow_mut();
            let stats = table.stats_mut(&loaded.path);
            stats.renders += 1;
            stats.render.record(took);
            stats.idle_renders += u64::from(self.idle.get());
            stats.store_writes += all.wrapping_sub(writes.0);
            stats.store_table_writes += tables.wrapping_sub(writes.1);
            match &result {
                Ok(rendered) => {
                    stats.note_tree(&rendered.node);
                    stats.closed_renders += u64::from(loaded.floats && rendered.float.is_none());
                }
                Err(_) => stats.failures += 1,
            }
        }
        result
    }

    fn render_lua(
        &self,
        index: usize,
        loaded: &Mod,
        ctx: RenderContext,
        key: Option<TreeKey>,
    ) -> Result<Rendered, ModError> {
        let fail = |message: String| ModError {
            owner: loaded.name.clone(),
            phase: Phase::Render,
            message,
        };

        let render: Function = loaded
            .def
            .get("render")
            .map_err(|e| fail(format!("no render function: {e}")))?;

        let table = self.lua.create_table().map_err(|e| fail(e.to_string()))?;
        table
            .set("width", ctx.width)
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("height", ctx.height)
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("focused", ctx.focused)
            .map_err(|e| fail(e.to_string()))?;
        // NOT set as a field: `elapsed` is served through the metatable installed
        // below, so that reading it is observable. See `CachedTree`.
        self.clock.set(ctx.elapsed);
        self.clock_read.set(false);
        self.sample_read.set(0);
        api::attach_clock(&self.lua, &table).map_err(|e| fail(e.to_string()))?;
        table
            .set("frame", ctx.frame)
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("name", loaded.name.clone())
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("slot", loaded.slot.clone())
            .map_err(|e| fail(e.to_string()))?;

        if loaded.capabilities.contains(&Capability::Files) {
            self.file_wants.borrow_mut().remove(&loaded.path);
        }
        self.enter(loaded);
        let result: Result<Value, mlua::Error> =
            Budget::arm(&self.lua).and_then(|_budget| render.call(table));

        let value = result.map_err(|e| fail(clean_error(&e)))?;
        let float = read_float(&value).map_err(fail)?;
        let node = convert::to_node(&value, &loaded.path).map_err(fail)?;
        // Behind the `Rc` from birth, so caching it and every later hand-out
        // is a refcount bump — see [`Rendered`].
        let rendered = Rendered {
            node: std::rc::Rc::new(node),
            float,
        };
        if loaded.pure {
            if let Some(key) = key {
                self.trees.borrow_mut().insert(
                    index,
                    CachedTree {
                        key,
                        rendered: rendered.clone(),
                        // Read from the render that just produced this tree, so
                        // the flag and the tree can never describe different
                        // versions of the pane.
                        reads_clock: self.clock_read.get(),
                        reads_sample: self.sample_read.get(),
                    },
                );
            }
        }
        Ok(rendered)
    }

    pub fn on_focus_cycle(&self, index: usize, direction: &str) -> Result<bool, ModError> {
        if !self.mods.get(index).is_some_and(|loaded| loaded.floats) {
            return Ok(false);
        }
        self.boolean_key_hook(index, "on_focus_cycle", |_| Ok(direction))
    }

    pub fn on_key(&self, index: usize, key: &KeyPress) -> Result<bool, ModError> {
        self.boolean_key_hook(index, "on_key", |lua| {
            let table = lua.create_table()?;
            table.set("key", key.name.clone())?;
            table.set("char", key.ch.map(|c| c.to_string()))?;
            table.set("ctrl", key.ctrl)?;
            table.set("alt", key.alt)?;
            table.set("shift", key.shift)?;
            table.set("cmd", key.cmd)?;
            Ok(table)
        })
    }

    fn boolean_key_hook<A: mlua::IntoLuaMulti>(
        &self,
        index: usize,
        name: &str,
        arguments: impl FnOnce(&Lua) -> mlua::Result<A>,
    ) -> Result<bool, ModError> {
        let Some(loaded) = self.mods.get(index) else {
            return Ok(false);
        };
        let fail = |message: String| ModError {
            owner: loaded.name.clone(),
            phase: Phase::Key,
            message,
        };
        let handler: Value = loaded.def.get(name).map_err(|e| fail(e.to_string()))?;
        let Value::Function(handler) = handler else {
            return Ok(false);
        };
        let args = arguments(&self.lua).map_err(|e| fail(clean_error(&e)))?;
        let start = self.call_started();
        self.enter(loaded);
        let handled: Result<bool, mlua::Error> =
            Budget::arm(&self.lua).and_then(|_budget| handler.call(args));
        self.hook_finished(loaded, start, Hook::Key, handled.is_err());
        handled.map_err(|e| fail(clean_error(&e)))
    }

    pub fn on_click(&self, index: usize, click: &Click) -> Result<bool, ModError> {
        self.pointer_hook(index, click, "on_click")
    }

    pub fn on_context(&self, index: usize, click: &Click) -> Result<bool, ModError> {
        self.pointer_hook(index, click, "on_context")
    }

    pub fn on_outside(&self, index: usize, click: &Click) -> Result<bool, ModError> {
        self.pointer_hook(index, click, "on_outside")
    }

    fn pointer_hook(&self, index: usize, click: &Click, hook: &str) -> Result<bool, ModError> {
        let Some(loaded) = self.mods.get(index) else {
            return Ok(false);
        };
        let fail = |message: String| ModError {
            owner: loaded.name.clone(),
            phase: Phase::Key,
            message,
        };

        let handler: Value = loaded.def.get(hook).map_err(|e| fail(e.to_string()))?;
        let Value::Function(handler) = handler else {
            return Ok(false);
        };

        let table = self.lua.create_table().map_err(|e| fail(e.to_string()))?;
        table
            .set("id", click.id.clone())
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("class", click.classes.join(" "))
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("role", click.role.clone())
            .map_err(|e| fail(e.to_string()))?;
        table.set("x", click.x).map_err(|e| fail(e.to_string()))?;
        table.set("y", click.y).map_err(|e| fail(e.to_string()))?;
        table.set("w", click.w).map_err(|e| fail(e.to_string()))?;
        table.set("h", click.h).map_err(|e| fail(e.to_string()))?;
        table
            .set("screen_x", click.screen_x)
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("screen_y", click.screen_y)
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("dragging", click.dragging)
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("clicks", click.clicks)
            .map_err(|e| fail(e.to_string()))?;

        let start = self.call_started();
        self.enter(loaded);
        let handled: Result<bool, mlua::Error> =
            Budget::arm(&self.lua).and_then(|_budget| handler.call(table));
        self.hook_finished(loaded, start, Hook::Click, handled.is_err());

        handled.map_err(|e| fail(clean_error(&e)))
    }

    pub fn on_scroll(&self, index: usize, scroll: &Scroll) -> Result<bool, ModError> {
        let Some(loaded) = self.mods.get(index) else {
            return Ok(false);
        };
        let fail = |message: String| ModError {
            owner: loaded.name.clone(),
            phase: Phase::Key,
            message,
        };

        let handler: Value = loaded
            .def
            .get("on_scroll")
            .map_err(|e| fail(e.to_string()))?;
        let Value::Function(handler) = handler else {
            return Ok(false);
        };

        let table = self.lua.create_table().map_err(|e| fail(e.to_string()))?;
        table
            .set("up", scroll.up)
            .map_err(|e| fail(e.to_string()))?;
        table.set("x", scroll.x).map_err(|e| fail(e.to_string()))?;
        table.set("y", scroll.y).map_err(|e| fail(e.to_string()))?;

        let start = self.call_started();
        self.enter(loaded);
        let handled: Result<bool, mlua::Error> =
            Budget::arm(&self.lua).and_then(|_budget| handler.call(table));
        self.hook_finished(loaded, start, Hook::Scroll, handled.is_err());

        handled.map_err(|e| fail(clean_error(&e)))
    }

    pub fn arrangement(&self, width: u16, height: u16) -> Result<std::rc::Rc<Region>, String> {
        // The dynamic arrangement is a Lua call plus a node-by-node conversion,
        // and the static one a whole-Region clone — per frame, from inputs
        // that move rarely. Everything the Lua function can consult is in the
        // key: `store` through the state version, the published tables through
        // the epoch, `chrome.status_rows` (a bare scalar with no version)
        // recorded at publish, and the occupied slots through the reload
        // counter.
        let key: LayoutKey = (
            width,
            height,
            self.reloads,
            *self.epoch.borrow(),
            self.state_version.get(),
            self.last_status_rows.get(),
        );
        if let Some((built_at, region)) = self.layout_cache.borrow().as_ref() {
            if *built_at == key {
                return Ok(region.clone());
            }
        }
        let region = std::rc::Rc::new(self.resolve_arrangement(width, height)?);
        self.layout_cache.replace(Some((key, region.clone())));
        Ok(region)
    }

    fn resolve_arrangement(&self, width: u16, height: u16) -> Result<Region, String> {
        match &self.layout {
            // No layout.lua: everything to the centre, with a one-line footer.
            // A missing file is a fresh checkout, not an error.
            Arrangement::Missing => Ok(Region {
                children: vec![
                    Region {
                        slot: Some("center".to_string()),
                        ..Region::default()
                    },
                    Region {
                        slot: Some("footer".to_string()),
                        size: Size {
                            len: Some(1),
                            ..Size::default()
                        },
                        ..Region::default()
                    },
                ],
                ..Region::default()
            }),
            Arrangement::Static(region) => Ok(region.clone()),
            Arrangement::Dynamic(arrange) => {
                self.enter_nothing();
                let ctx = self.lua.create_table().map_err(|e| e.to_string())?;
                ctx.set("width", width).map_err(|e| e.to_string())?;
                ctx.set("height", height).map_err(|e| e.to_string())?;
                ctx.set("slots", self.occupied_slots_table()?)
                    .map_err(|e| e.to_string())?;
                ctx.set("strips", self.strips_table()?)
                    .map_err(|e| e.to_string())?;

                let result: Result<Value, mlua::Error> =
                    Budget::arm(&self.lua).and_then(|_budget| arrange.call(ctx));

                let value = result.map_err(|e| format!("layout.lua: {}", clean_error(&e)))?;
                super::layout::region_from_lua(&value, "layout")
            }
        }
    }

    pub fn occupied_slots(&self) -> BTreeSet<&str> {
        self.mods
            .iter()
            .filter(|loaded| loaded.decorates.is_none() && !loaded.floats)
            .map(|loaded| loaded.slot.as_str())
            .collect()
    }

    pub fn unplaced_slots(&self, area: Rect) -> Result<Vec<String>, String> {
        let occupied: BTreeSet<String> = self
            .occupied_slots()
            .into_iter()
            .map(str::to_string)
            .collect();
        let placed = self.placed_slots(area)?;
        Ok(occupied
            .into_iter()
            .filter(|slot| !placed.contains(slot))
            .collect())
    }

    pub fn placed_slots(&self, area: Rect) -> Result<BTreeSet<String>, String> {
        let occupied: Vec<String> = self
            .occupied_slots()
            .into_iter()
            .map(str::to_string)
            .collect();
        for slot in &occupied {
            self.set_shared_bool(&format!("panels.{slot}"), true);
        }
        let region = self.arrangement(area.width, area.height)?;
        Ok(super::layout::placed_slots(&region, area))
    }

    pub fn strips(&self) -> Vec<(&str, u16)> {
        let mut strips: Vec<(&str, u16)> = Vec::new();
        for loaded in &self.mods {
            let slot = loaded.slot.as_str();
            if !loaded.strip
                || loaded.decorates.is_some()
                || loaded.floats
                || strips.iter().any(|(placed, _)| *placed == slot)
            {
                continue;
            }
            let lens = self
                .in_slot(slot)
                .iter()
                .map(|index| &self.mods[*index])
                .filter(|occupant| !occupant.floats)
                .map(|occupant| occupant.size.len.unwrap_or(1).max(1));
            let len = match self.slot_mode(slot) {
                SlotMode::Stack => lens.fold(0u16, u16::saturating_add),
                SlotMode::Switch => lens.max().unwrap_or(1),
            };
            strips.push((slot, len));
        }
        strips
    }

    fn strips_table(&self) -> Result<Table, String> {
        let list = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, (slot, len)) in self.strips().into_iter().enumerate() {
            let entry = self.lua.create_table().map_err(|e| e.to_string())?;
            entry.set("slot", slot).map_err(|e| e.to_string())?;
            entry.set("len", len).map_err(|e| e.to_string())?;
            list.set(index + 1, entry).map_err(|e| e.to_string())?;
        }
        Ok(list)
    }

    fn occupied_slots_table(&self) -> Result<Table, String> {
        let table = self.lua.create_table().map_err(|e| e.to_string())?;
        for slot in self.occupied_slots() {
            table.set(slot, true).map_err(|e| e.to_string())?;
        }
        Ok(table)
    }

    pub fn undiscoverable(&self, index: usize) -> Option<String> {
        let loaded = self.mods.get(index)?;
        if loaded.floats || loaded.decorates.is_some() {
            return None;
        }
        if self.slot_mode(&loaded.slot) != SlotMode::Switch {
            return None;
        }
        // The first occupant of a switch slot is the one that draws.
        if self.in_slot(&loaded.slot).first() == Some(&index) {
            return None;
        }
        if !loaded.pills.is_empty() {
            return None;
        }
        Some(format!(
            "shares the {:?} slot and is not the one shown by default, and declares no \
             pill — so nothing on screen offers it. Declare one \
             (`pills = {{ {{ action = \"…\", label = \"…\" }} }}`) or only a key of its \
             own reaches it — Ctrl+H/Ctrl+L never stop on it.",
            loaded.slot
        ))
    }

    pub fn switch_default(&self, slot: &str) -> Option<usize> {
        self.in_slot(slot).iter().copied().find(|index| {
            let loaded = &self.mods[*index];
            loaded.focusable && !loaded.floats
        })
    }

    pub fn slot_mode(&self, slot: &str) -> SlotMode {
        self.index
            .modes
            .get(slot)
            .copied()
            .unwrap_or(SlotMode::Stack)
    }
}
