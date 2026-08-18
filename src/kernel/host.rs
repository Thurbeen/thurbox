//! Owns the Lua VM, the plugins loaded into it, and the slots they compose into.
//!
//! A reload throws the whole VM away and builds a fresh one. If the new VM
//! fails to build, the previous one keeps running and the error is surfaced —
//! so a typo costs a red pane, never the session you were watching.
//!
//! Two isolation rules make this safe to work in, and both are spec
//! requirements rather than niceties:
//!
//! - A plugin whose `render` throws is replaced by an error panel **in its own
//!   rect**. Its neighbours keep drawing and keep their state.
//! - A plugin that never returns is *interrupted*. Luau would have donated
//!   this; on Lua 5.4 it is ours to build (design.md D8), so every call runs
//!   under an instruction-count hook and a memory ceiling.
//!
//! The VM is deliberately not `Send` — mlua's `send` feature stays off, which
//! makes "plugins never touch the render thread" a compile error (D10).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use mlua::{Function, Lua, StdLib, Table, Value, VmState};
use ratatui::layout::Rect;

use super::command::{Args, Command, ExtraMember, InFlight};
use super::convert;
use super::layout::{Region, SlotMode};
use super::node::{Node, Size};
use super::registry::{binding_from, Binding, Pill, Registry, Setting, Value as SettingValue};
use super::snapshot::Snapshot;
use super::theme::Themes;

/// Instructions a single plugin call may execute before it is interrupted.
///
/// Generous — a session list formatting a few hundred rows uses a tiny
/// fraction — but finite, so `while true do end` costs one red pane instead of
/// the application.
pub const INSTRUCTION_BUDGET: u32 = 20_000_000;

/// Override for [`INSTRUCTION_BUDGET`], so the bound can be raised for a heavy
/// pane or lowered to prove the guard fires without waiting 20M instructions.
///
/// A setting rather than a constant because the right number depends on what
/// plugins you run, and discovering it means measuring — see task 2.12.
static BUDGET_OVERRIDE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Set the per-call instruction budget. Zero restores the default.
pub fn set_instruction_budget(instructions: u32) {
    BUDGET_OVERRIDE.store(instructions, std::sync::atomic::Ordering::Relaxed);
}

/// The budget in force.
pub fn instruction_budget() -> u32 {
    match BUDGET_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => INSTRUCTION_BUDGET,
        set => set,
    }
}

/// Override for [`MEMORY_LIMIT`], for the same reasons.
static MEMORY_OVERRIDE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Set the plugin VM's memory ceiling. Zero restores the default.
pub fn set_memory_limit(bytes: usize) {
    MEMORY_OVERRIDE.store(bytes, std::sync::atomic::Ordering::Relaxed);
}

/// The memory ceiling in force.
pub fn memory_limit() -> usize {
    match MEMORY_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => MEMORY_LIMIT,
        set => set,
    }
}

/// Ceiling on the plugin VM's heap. Exceeding it fails the allocation as a
/// plugin error rather than aborting the process.
pub const MEMORY_LIMIT: usize = 256 * 1024 * 1024;

/// Which standard libraries plugins get.
///
/// Chosen deliberately, not inherited: `io`, `os` and `debug` are **absent**,
/// so a plugin has no filesystem, process or clock-tampering access — the
/// capability model is enforced by absence (design.md D9), not by a binding
/// that refuses. `package` is withheld too; `require` is ours (see
/// [`install_require`]), scoped to the plugin directory.
fn plugin_stdlib() -> StdLib {
    // UTF8 is pure computation with no capability risk, and text handling
    // needs it: Lua's `#` counts bytes, so without it a plugin measuring a name
    // like "Rosé Pine" pads it one column short.
    StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::COROUTINE | StdLib::UTF8
}

/// A value that survives a reload, mirrored on the Rust side.
#[derive(Clone, Debug, PartialEq)]
enum Persisted {
    Bool(bool),
    /// Integers stay integers, or `state.n = 1` would come back as `1.0`.
    Int(i64),
    Num(f64),
    Str(String),
    Table(Vec<(Persisted, Persisted)>),
}

/// `store` is shared by every plugin; `state` is keyed by plugin file too, so
/// two plugins can both use `state.index` without colliding.
type Shared = Rc<RefCell<BTreeMap<String, Persisted>>>;
type Private = Rc<RefCell<BTreeMap<(String, String), Persisted>>>;
/// Commands a plugin issued this frame, drained by the loop.
type Queue = Rc<RefCell<Vec<Command>>>;

/// Finished runs, keyed by the plugin that asked and then by its own key.
pub type RunAnswers = std::collections::HashMap<String, Vec<(String, super::runs::Run)>>;
/// Each session's working directory, so a file read can be rooted at one.
/// Refreshed with the snapshot; a session not in here cannot be browsed.
type Roots = Rc<RefCell<BTreeMap<String, PathBuf>>>;

/// Which phase a plugin failed in — required so every failure is attributable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Load,
    Render,
    Key,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Load => "load",
            Phase::Render => "render",
            Phase::Key => "key",
        }
    }
}

/// A failure attributed to a plugin and a phase.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginError {
    pub plugin: String,
    pub phase: Phase,
    pub message: String,
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            self.plugin,
            self.phase.as_str(),
            self.message
        )
    }
}

/// The arrangement, resolved once per reload rather than per frame.
enum Arrangement {
    /// No `ui/layout.lua` — the kernel supplies a default.
    Missing,
    /// The file returned an arrangement directly.
    Static(Region),
    /// The file returned a function of the available size. This is what makes
    /// the arrangement responsive, so it is called per frame — but *loaded*
    /// once.
    Dynamic(Function),
}

/// A loaded plugin.
pub struct Plugin {
    pub name: String,
    pub file: String,
    /// Path relative to the interface directory — the identity every command
    /// about this file uses, since the file name is the only name a plugin
    /// always has (it may declare no `name`, and two may declare the same one).
    pub path: String,
    pub slot: String,
    pub focusable: bool,
    /// Declared `input = "session"`: while this plugin holds focus, keys it
    /// does not handle are forwarded to the session its surface names.
    ///
    /// This is how the terminal pane stays an ordinary plugin. The kernel does
    /// not know which plugin "is" the terminal — it knows this one asked for
    /// raw input, and which session the tree it just returned pointed at.
    pub session_input: bool,
    /// Space this plugin asks of its slot. Declared **statically** here rather
    /// than in render output — that is what lets the layout pass resolve rects
    /// before any plugin runs (design.md D2).
    pub size: Size,
    /// Slot whose rendered tree this plugin may transform.
    ///
    /// The answer to design.md D6: a decorator receives another plugin's tree
    /// and returns a modified one, matching on the identity nodes already
    /// carry. No selector engine — that was the largest available instance of
    /// the mistake this whole design exists to avoid.
    pub decorates: Option<String>,
    /// Whether this plugin is allowed to float. Declared statically so the
    /// kernel knows to render it *after* the arrangement, without having to
    /// render it once to find out.
    pub floats: bool,
    /// Keys this plugin declared, as data — enumerable without invoking it.
    pub bindings: Vec<Binding>,
    /// Settings this plugin accepts.
    pub settings: Vec<Setting>,
    /// Action-band entries this plugin contributes.
    pub pills: Vec<Pill>,
    /// Capabilities this plugin declared it needs.
    ///
    /// Declaring one is not being granted it: the kernel installs a capability
    /// only for a plugin the user has *trusted* with it. Kept as data so the
    /// interface's own file list can say which files ask to run programs
    /// without anyone reading their source.
    pub capabilities: Vec<Capability>,
    order: f64,
    def: Table,
}

/// Something a plugin may ask to be able to do that ordinary Lua here cannot.
///
/// The enum exists so a third is a compile error at every place that decides
/// about the first two, rather than a string compared in four places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// Run a program on the user's behalf and read its output.
    Run,
    /// Keep an interactive program in a pane and feed it the user's keystrokes.
    ///
    /// Deliberately **not** part of [`Capability::Run`]. `run` is bounded on every
    /// axis that matters — a capped amount of output, a timeout, a limit on how
    /// many run at once — and an interactive program has none of those by design,
    /// and holds the keyboard as well. Someone who trusted a pane to poll `top`
    /// every few seconds did not agree to "may hold a process open indefinitely
    /// and feed it what I type", so the grant is asked for separately.
    Program,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Run => "run",
            Capability::Program => "program",
        }
    }

    /// What granting this lets a file do, in words, for the list where the user
    /// decides about it.
    ///
    /// Distinguishing the two is the whole point: "runs programs" would describe
    /// both, and the difference — whether it also takes your keystrokes and stays
    /// running — is exactly what a reader needs before pressing `t`.
    pub fn describe(self) -> &'static str {
        match self {
            Capability::Run => "runs programs and reads their output",
            Capability::Program => "runs a program you interact with",
        }
    }

    /// Parse a declared name, or refuse it.
    ///
    /// Refused rather than ignored: a plugin declaring `capabilities = { "rnu" }`
    /// would otherwise load, be granted nothing, and look like the capability is
    /// broken.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "run" => Some(Capability::Run),
            "program" => Some(Capability::Program),
            _ => None,
        }
    }

    /// Every capability, for a message naming what was available.
    pub const ALL: [Capability; 2] = [Capability::Run, Capability::Program];
}

/// A key, flattened to what Lua needs to know about it.
#[derive(Debug, Clone, Default)]
pub struct KeyPress {
    pub name: String,
    pub ch: Option<char>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// A click resolved onto a node one plugin painted.
///
/// The counterpart to [`KeyPress`], and deliberately shaped like one: the
/// kernel hit-tests the tree it just painted, attributes the node to the plugin
/// that returned it, and hands over the identity plus where inside the node the
/// press landed. It carries no rect — a plugin that knows *which* node was hit
/// does not need to be told again where that node was.
#[derive(Debug, Clone, Default)]
pub struct Click {
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub role: Option<String>,
    /// Column and row within the clicked node's own rect. What v1 needed for a
    /// side-by-side diff row, where which half you clicked is the answer.
    pub x: u16,
    pub y: u16,
}

/// A plugin that is floating this frame, and how much room it wants.
///
/// Floating is a property of what the plugin *returned*, not of the plugin — a
/// modal appears by returning a float node and disappears by not doing so, with
/// no separate open/close channel for the kernel to keep in sync.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Float {
    /// Share of the screen, 0..100. Clamped when rendered.
    pub width_pct: f64,
    pub height_pct: f64,
    /// Exact columns, when the plugin knows them. Wins over the percentage.
    ///
    /// A modal framing a list of a known length wants a size in cells, not a
    /// share of the screen: sized by percentage its frame drifts away from its
    /// content as the terminal grows. v1's modals are a fixed width with the
    /// height fitted to what is inside them, and this is how a plugin says so.
    pub cols: Option<u16>,
    pub rows: Option<u16>,
}

impl Default for Float {
    fn default() -> Self {
        Self {
            width_pct: 60.0,
            height_pct: 60.0,
            cols: None,
            rows: None,
        }
    }
}

/// What a plugin produced this frame.
#[derive(Debug, Clone)]
pub struct Rendered {
    pub node: Node,
    /// Set when the plugin asked to float above the arrangement.
    pub float: Option<Float>,
}

/// Everything published to plugins in one frame.
///
/// A struct rather than a parameter list because this grew from two to six in
/// the course of one change, and each growth churned every call site. Adding
/// something plugins can read should not be an edit to every test.
pub struct Published<'a> {
    pub snapshot: &'a Snapshot,
    /// Why a session's terminal is not live, keyed by session.
    pub attach_errors: &'a std::collections::HashMap<String, String>,
    pub inflight: &'a [InFlight],
    pub themes: &'a Themes,
    pub registry: &'a Registry,
    pub diffs: &'a super::diff::DiffStore,
    /// Links visible in each session's terminal, keyed by session.
    pub links: &'a std::collections::HashMap<String, Vec<(String, usize, usize)>>,
    /// What each session's terminal is showing, keyed by session — empty unless
    /// a plugin asked (`kernel::terminal::WANT_CONTENT`). Serving it only on
    /// demand is what keeps every agent's screen off every frame.
    pub content: &'a std::collections::HashMap<String, String>,
    /// Machine, per-agent and account metrics, as far as they have been sampled.
    pub metrics: &'a super::metrics::Metrics,
    /// Rows the message band needs — 0 while it has nothing to say.
    ///
    /// Published because the ARRANGEMENT needs it: whether a band takes a row is
    /// placement, which `ui/layout.lua` decides. What the band then shows is not,
    /// so the message itself stays kernel-side.
    pub status_rows: u16,
    /// What each live agent reported over its own terminal — the activity line
    /// and the attention message. Keyed by session; absent for a session with
    /// no live pane.
    pub meta: &'a std::collections::HashMap<String, super::terminal::AgentMeta>,
    /// Whether opening a link will actually open anything here.
    pub can_open: bool,
    /// The identity under the pointer, so a pane can light its own affordance.
    ///
    /// Published rather than resolved in the kernel because only the pane that
    /// drew a thing knows how it should look highlighted — the kernel knows
    /// where it is, not what it means.
    pub hovered: Option<&'a super::node::Identity>,
    /// The focused plugin's name, so a pane can report focus it does not hold.
    ///
    /// `ctx.focused` is per-plugin and answers "am I focused?"; the footer has
    /// to name whoever IS, and it is not focusable itself. v1 reads the same
    /// thing off `App::focus`.
    pub focus: Option<&'a str>,
    /// The interface's own files: where each came from and which are running.
    ///
    /// Published rather than drawn by the kernel, so the pane that lists the
    /// panes is an ordinary plugin holding no capability a user's own could not
    /// have — which is the standing rule for bundled plugins, and the only
    /// honest test of it (design D4).
    pub inventory: &'a [super::inventory::Row],
    /// The directory the interface was loaded from.
    ///
    /// A `./ui` beside the working directory wins over the user's own copy, so
    /// edits that "did nothing" are usually edits to a file that is not the one
    /// running. Reporting it is the whole fix (design D7).
    pub ui_dir: &'a str,
    /// The settings in force — the live half as last read, the restart-only half
    /// as published at startup (`kernel::config`).
    ///
    /// Published because the arrangement needs some of it *before* any plugin
    /// runs (the column thresholds), and because a pane must be able to honour a
    /// feature switch the kernel has no knowledge of.
    pub settings: &'a crate::session::settings::Settings,
    /// What the creation flow asked about, and the answers so far.
    ///
    /// Published as a pair rather than the whole store because only what is
    /// *currently* asked for is published: a flow that is closed asks nothing, so
    /// these three tables are empty and cost a table each.
    pub repos: &'a super::repos::RepoStore,
    pub wants: &'a super::repos::Wants,
}

/// What a plugin is told about the frame it is rendering into.
#[derive(Debug, Clone, Copy)]
pub struct RenderContext {
    /// The plugin's **own** resolved width, not the screen's.
    pub width: u16,
    pub height: u16,
    pub focused: bool,
    pub elapsed: f64,
    pub frame: u64,
}

pub struct LuaHost {
    lua: Lua,
    ui_dir: PathBuf,
    store: Shared,
    state: Private,
    /// The plugin currently being called, so `state` can namespace itself.
    ///
    /// This is the file *stem*, which is what `state` has always keyed by.
    current: Rc<RefCell<String>>,
    /// The same plugin, by its path.
    ///
    /// Kept beside `current` rather than replacing it: a run is attributed by
    /// path because that is the identity trust and the inventory use, and
    /// changing what `state` keys by would silently move every plugin's stored
    /// state on upgrade.
    current_path: Rc<RefCell<String>>,
    /// Session working directories, the roots a file read is confined to.
    roots: Roots,
    /// Commands issued by plugins, drained once per frame by the loop.
    ///
    /// Queued rather than executed inline: `command()` must return instantly,
    /// and a plugin must never be able to run a database write from inside a
    /// render call (design.md D5).
    queue: Queue,
    /// Runs plugins asked for, drained once per frame like `queue`.
    ///
    /// Separate from `queue` because a run carries the plugin that asked —
    /// answers are namespaced by it — and because the command bus dispatches
    /// writes to workers, while this is a read whose worker the store owns.
    runs: Rc<RefCell<Vec<(String, super::runs::Ask)>>>,
    /// Paths the user trusted, as `set_trusted` last recorded them.
    trusted: Rc<RefCell<Vec<String>>>,
    /// Paths the user turned off. Read by `build`, which simply does not load
    /// them — which is the whole implementation of being disabled (design D2).
    disabled: Rc<RefCell<Vec<String>>>,
    /// Finished runs, per plugin. Published into `thurbox.runs` when that plugin
    /// is entered, so a plugin sees its own answers and no one else's.
    run_answers: Rc<RefCell<RunAnswers>>,
    pub plugins: Vec<Plugin>,
    layout: Arrangement,
    /// Set when the *last* reload attempt failed; `plugins` are still the ones
    /// from the last good build.
    pub error: Option<String>,
    pub reloads: u32,
}

impl LuaHost {
    pub fn new(ui_dir: impl Into<PathBuf>) -> Self {
        let ui_dir = ui_dir.into();
        let mut host = Self {
            lua: new_vm().unwrap_or_else(|_| Lua::new()),
            ui_dir,
            store: Shared::default(),
            state: Private::default(),
            current: Rc::new(RefCell::new(String::new())),
            current_path: Rc::new(RefCell::new(String::new())),
            queue: Queue::default(),
            runs: Rc::new(RefCell::new(Vec::new())),
            trusted: Rc::new(RefCell::new(Vec::new())),
            disabled: Rc::new(RefCell::new(Vec::new())),
            run_answers: Rc::new(RefCell::new(std::collections::HashMap::new())),
            roots: Roots::default(),
            plugins: Vec::new(),
            layout: Arrangement::Missing,
            error: None,
            reloads: 0,
        };
        host.reload();
        host
    }

    /// Rebuild the VM from disk. On failure the previous VM keeps running.
    /// Rebuild from a different directory than the one this host was built from.
    ///
    /// The recovery floor needs it. [`Self::reload`] rebuilds from `self.ui_dir`,
    /// so a host built from the bundled fallback rebuilds the *fallback* — the
    /// user's fix to their own copy would change nothing, with nothing on screen
    /// to say why. Pointing the host back at the authoritative directory on every
    /// reload is what makes the floor a state rather than a one-way door.
    pub fn reload_from(&mut self, ui_dir: impl Into<PathBuf>) {
        self.ui_dir = ui_dir.into();
        self.reload();
    }

    pub fn reload(&mut self) {
        match self.build() {
            Ok((lua, plugins, layout)) => {
                self.lua = lua;
                self.plugins = plugins;
                self.layout = layout;
                self.error = None;
                self.reloads += 1;
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn build(&self) -> Result<(Lua, Vec<Plugin>, Arrangement), String> {
        let lua = new_vm().map_err(|e| e.to_string())?;
        install_api(
            &lua,
            &self.ui_dir,
            self.store.clone(),
            self.state.clone(),
            self.current.clone(),
            self.queue.clone(),
            self.roots.clone(),
            self.runs.clone(),
            self.current_path.clone(),
        )
        .map_err(|e| e.to_string())?;

        let dir = self.ui_dir.join("plugins");
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "lua"))
            .collect();
        // Sorted so `order` ties break by filename, deterministically.
        files.sort();

        // Panes the spec names outside `plugins/`. A plugin obtained as a repository
        // keeps its author's layout, so its pane sits at `<name>/…` — invisible to
        // the scan above, which reads only the top level of `plugins/`.
        //
        // Its place in the load order is unaffected: `order` comes from the
        // *basename*'s numeric prefix, so `40_x.lua` sorts as `40_x.lua` wherever it
        // lives.
        let nested: Vec<String> = super::packages::read_spec(&self.ui_dir)
            .map(|spec| {
                spec.plugins
                    .into_iter()
                    .map(|entry| entry.file)
                    .filter(|file| crate::session::plugin_spec::is_nested_pane(file))
                    .collect()
            })
            // A malformed spec must not cost the whole interface: the panes in
            // `plugins/` still load, and `plugin check` reports the spec separately.
            .unwrap_or_default();

        let mut plugins = Vec::new();
        let disabled = self.disabled.borrow();
        for relative in nested {
            let path = self.ui_dir.join(&relative);
            if !path.is_file() || disabled.iter().any(|off| off == &relative) {
                continue;
            }
            plugins.push(load_plugin(&lua, &path, &relative)?);
        }
        for path in files {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            let relative = format!("plugins/{name}");
            // A plugin the user turned off is simply not read. That is the
            // entire implementation: it is absent from `plugins`, so it declares
            // no key, occupies no slot, is granted no capability and cannot fail
            // to load — five properties that would otherwise each need enforcing
            // somewhere else (design D2).
            if disabled.iter().any(|off| off == &relative) {
                continue;
            }
            plugins.push(load_plugin(&lua, &path, &relative)?);
        }
        drop(disabled);
        // An EMPTY directory is not an error: removing every pane is something
        // a user may do, and faulting here would summon the recovery floor,
        // which would deliver the bundled interface again — the system undoing
        // the removal it was just asked to make (design D6). A *missing*
        // directory still is one: delivery always creates it, so its absence
        // means the interface directory is not one.
        plugins.sort_by(|a, b| a.order.total_cmp(&b.order));

        let layout = load_arrangement(&lua, &self.ui_dir)?;
        Ok((lua, plugins, layout))
    }

    /// Every declaration from every loaded plugin, for the registry.
    pub fn declarations(&self) -> (Vec<Binding>, Vec<Setting>) {
        let (bindings, settings, _) = self.all_declarations();
        (bindings, settings)
    }

    /// [`Self::declarations`] including the action-band entries.
    ///
    /// Every declaration is collected the same way and at the same moment — at
    /// load and reload — which is what lets a band paint from data already in
    /// hand rather than by asking a plugin while it draws.
    pub fn all_declarations(&self) -> (Vec<Binding>, Vec<Setting>, Vec<Pill>) {
        let mut bindings = Vec::new();
        let mut settings = Vec::new();
        let mut pills = Vec::new();
        for plugin in &self.plugins {
            bindings.extend(plugin.bindings.iter().cloned());
            settings.extend(plugin.settings.iter().cloned());
            pills.extend(plugin.pills.iter().cloned());
        }
        (bindings, settings, pills)
    }

    /// Call a plugin's `on_action` with a declared action id.
    ///
    /// Separate from `on_key`: a declared key is routed by the registry, which
    /// is what lets it be rebound, listed in help, and conflict-checked. Raw
    /// `on_key` stays for panes that need every keystroke (the terminal).
    pub fn on_action(&self, index: usize, action: &str) -> Result<bool, PluginError> {
        let Some(plugin) = self.plugins.get(index) else {
            return Ok(false);
        };
        let fail = |message: String| PluginError {
            plugin: plugin.name.clone(),
            phase: Phase::Key,
            message,
        };
        let handler: Value = plugin
            .def
            .get("on_action")
            .map_err(|e| fail(e.to_string()))?;
        let Value::Function(handler) = handler else {
            return Ok(false);
        };

        self.enter(plugin);
        let guard = Budget::arm(&self.lua);
        let handled: Result<bool, mlua::Error> = handler.call(action.to_string());
        drop(guard);
        handled.map_err(|e| fail(clean_error(&e)))
    }

    /// Index of a plugin by name.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.plugins.iter().position(|p| p.name == name)
    }

    /// Indices of the plugins occupying one slot, in render order.
    pub fn in_slot(&self, slot: &str) -> Vec<usize> {
        self.plugins
            .iter()
            .enumerate()
            // A decorator is not an occupant: it draws INTO another plugin's
            // tree. Without this it would take the default slot and compete for
            // the centre with the pane it exists to decorate.
            .filter(|(_, plugin)| plugin.decorates.is_none())
            .filter(|(_, plugin)| plugin.slot == slot)
            .map(|(index, _)| index)
            .collect()
    }

    /// Indices of plugins decorating `slot`, in render order.
    ///
    /// Render order is the deterministic order the spec requires: it follows
    /// each plugin's declared `order`, so two decorators on one slot apply the
    /// same way every run.
    pub fn decorators_of(&self, slot: &str) -> Vec<usize> {
        self.plugins
            .iter()
            .enumerate()
            .filter(|(_, plugin)| plugin.decorates.as_deref() == Some(slot))
            .map(|(index, _)| index)
            .collect()
    }

    /// Ask a decorator to transform a tree.
    ///
    /// Returns the original on failure rather than propagating: a decorator
    /// that throws must cost its decoration, not the pane it was decorating.
    pub fn decorate(
        &self,
        index: usize,
        node: &Node,
        ctx: RenderContext,
    ) -> Result<Node, PluginError> {
        let Some(plugin) = self.plugins.get(index) else {
            return Ok(node.clone());
        };
        let fail = |message: String| PluginError {
            plugin: plugin.name.clone(),
            phase: Phase::Render,
            message,
        };
        let handler: Value = plugin
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

        self.enter(plugin);
        let guard = Budget::arm(&self.lua);
        let result: Result<Value, mlua::Error> = handler.call((tree, table));
        drop(guard);

        let value = result.map_err(|e| fail(clean_error(&e)))?;
        // The DECORATOR owns what it returns: a program surface it names is one of
        // its own panes, not one belonging to the plugin whose tree it decorated.
        convert::to_node(&value, &plugin.path).map_err(fail)
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
        store: &super::repos::RepoStore,
        wants: &super::repos::Wants,
    ) -> Result<(), String> {
        use super::repos::{Branches, Listing};

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
        table
            .set("bookmarks", bookmarks)
            .map_err(|e| e.to_string())?;

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
        table.set("browse", browse).map_err(|e| e.to_string())?;

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
        table.set("branches", branches).map_err(|e| e.to_string())?;

        Ok(())
    }

    /// A string a plugin left in the shared `store`, if it put one there.
    ///
    /// The loop needs exactly one of these — `store.selected`, the session the
    /// list has selected — to know which session focus just left. Reading the
    /// store rather than tracking it kernel-side keeps the selection owned by the
    /// pane that moves it, which is what makes the session list a plugin.
    pub fn shared_string(&self, key: &str) -> Option<String> {
        match self.store.borrow().get(key) {
            Some(Persisted::Str(text)) => Some(text.clone()),
            _ => None,
        }
    }

    /// Read a boolean out of the shared `store`.
    ///
    /// The panel flags live there (`panels.<name>`) because the arrangement has
    /// to read them before any plugin runs, so this is how a caller outside Lua
    /// asks whether a column is open.
    pub fn shared_bool(&self, key: &str) -> Option<bool> {
        match self.store.borrow().get(key) {
            Some(Persisted::Bool(value)) => Some(*value),
            _ => None,
        }
    }

    /// Put a string into the shared `store`, for a request that arrives from
    /// outside any plugin.
    ///
    /// The one caller is a focus request landing from another process — a clicked
    /// notification, or `thurbox-cli session focus`. It is written into the store
    /// rather than applied directly because the *selection* belongs to the
    /// session list, which republishes it every frame; anything the kernel set
    /// behind its back would be overwritten immediately.
    pub fn set_shared_string(&self, key: &str, value: &str) {
        self.store
            .borrow_mut()
            .insert(key.to_string(), Persisted::Str(value.to_string()));
    }

    /// Put a boolean into the shared `store`.
    ///
    /// The counterpart to [`Self::shared_bool`], and used for the same panel
    /// flags: [`Self::placed_slots`] opens every column before resolving the
    /// arrangement, because a pane behind a closed toggle is not a missing pane.
    pub fn set_shared_bool(&self, key: &str, value: bool) {
        self.store
            .borrow_mut()
            .insert(key.to_string(), Persisted::Bool(value));
    }

    /// Indices of plugins that may float, in render order.
    pub fn floating(&self) -> Vec<usize> {
        self.plugins
            .iter()
            .enumerate()
            .filter(|(_, plugin)| plugin.floats)
            .map(|(index, _)| index)
            .collect()
    }

    /// Indices of every focusable plugin, in render order.
    pub fn focusable(&self) -> Vec<usize> {
        self.plugins
            .iter()
            .enumerate()
            .filter(|(_, plugin)| plugin.focusable)
            .map(|(index, _)| index)
            .collect()
    }

    /// Take the commands plugins issued since the last drain.
    /// Make `plugin` the current one, and give it exactly the capabilities it
    /// has been granted.
    ///
    /// Capabilities are **installed and removed per call** rather than once at
    /// load, because every plugin shares one Lua state: a global installed for
    /// one would be reachable by all. Setting it to nil for a plugin that was
    /// not granted it is what makes "capabilities are absent rather than
    /// blocked" true here — `run` is not a function that refuses, it is not a
    /// function (design D7).
    /// The other half of [`Self::enter`]: withdraw every per-plugin capability.
    ///
    /// For Lua that runs outside any plugin. `enter` installs `run` for the plugin
    /// about to be called and nothing takes it away afterwards, so code entered
    /// without it would inherit the last grant — and be attributed to the last
    /// plugin, since that is what stamps the asking path.
    fn enter_nothing(&self) {
        self.current.borrow_mut().clear();
        self.current_path.borrow_mut().clear();
        let _ = self.lua.globals().set("run", Value::Nil);
    }

    /// May this plugin use `capability`?
    ///
    /// Two conditions, and both are needed: the file **declared** it, and the user
    /// **trusted** that file. One predicate rather than the check written out at
    /// each site, because the capabilities must not be able to drift apart — a
    /// grant for one is not a grant for the other, and the way that breaks is a
    /// second site that checks trust and forgets to check which capability was
    /// asked for.
    pub fn may(&self, plugin: &Plugin, capability: Capability) -> bool {
        plugin.capabilities.contains(&capability) && self.trusted.borrow().contains(&plugin.path)
    }

    /// [`Self::may`], for a plugin named by its path — the identity a queued
    /// command carries, since the command is honoured after the call that made it.
    pub fn may_path(&self, path: &str, capability: Capability) -> bool {
        self.plugins
            .iter()
            .find(|plugin| plugin.path == path)
            .is_some_and(|plugin| self.may(plugin, capability))
    }

    fn enter(&self, plugin: &Plugin) {
        *self.current.borrow_mut() = plugin.file.clone();
        *self.current_path.borrow_mut() = plugin.path.clone();
        let granted = self.may(plugin, Capability::Run);
        // Only a plugin that asked for a capability can have answers, so every
        // other one skips the rest. Worth the check: `enter` runs on every call
        // to every plugin, and building a table per call for the panes that can
        // never have runs would be a per-frame allocation for nothing.
        if plugin.capabilities.is_empty() {
            let _ = self.lua.globals().set("run", Value::Nil);
            return;
        }

        // This plugin's own answers, and nothing else's. Set per call rather
        // than at publish because `thurbox` is one shared table: publishing
        // every plugin's runs into it would let any pane read another's output.
        //
        // The read surface is created if it does not exist yet, so an answer is
        // readable without depending on a publish having happened first.
        let globals = self.lua.globals();
        let surface = match globals.get::<Table>("thurbox") {
            Ok(table) => Some(table),
            // Not `Option::inspect`: that is stable since 1.76 and the MSRV is
            // 1.75.
            Err(_) => match self.lua.create_table() {
                Ok(fresh) => {
                    let _ = globals.set("thurbox", fresh.clone());
                    Some(fresh)
                }
                Err(_) => None,
            },
        };
        if let (Some(surface), Ok(table)) = (surface, self.lua.create_table()) {
            if let Some(answers) = self.run_answers.borrow().get(&plugin.path) {
                for (key, run) in answers {
                    if let Ok(entry) = run_to_lua(&self.lua, run) {
                        let _ = table.set(key.clone(), entry);
                    }
                }
            }
            let _ = surface.set("runs", table);
        }

        // What this plugin has actually been granted, as `thurbox.granted.<name>`.
        //
        // Needed because not every capability can be withheld by absence. `run` is
        // a global, so a plugin checks `if not run then` and draws an honest hint —
        // that IS the absence. `program` is asked for through `command`, which
        // every plugin has, so absence cannot express it and a pane would have no
        // way to tell "not trusted" from "still starting". This is that answer, and
        // it grants nothing: it is a boolean about a decision the user already made.
        if let (Ok(surface), Ok(table)) = (
            self.lua.globals().get::<Table>("thurbox"),
            self.lua.create_table(),
        ) {
            for capability in Capability::ALL {
                if self.may(plugin, capability) {
                    let _ = table.set(capability.as_str(), true);
                }
            }
            let _ = surface.set("granted", table);
        }

        // Resolved out of the VM rather than held in Rust, because a reload
        // replaces the VM and a cached handle would outlive the state it came
        // from. Out of the *registry* rather than globals, because globals is
        // every plugin's `_ENV` — see `RUN_IMPL`.
        let globals = self.lua.globals();
        match granted
            .then(|| self.lua.named_registry_value::<Value>(RUN_IMPL))
            .transpose()
        {
            Ok(Some(implementation)) => {
                let _ = globals.set("run", implementation);
            }
            _ => {
                let _ = globals.set("run", Value::Nil);
            }
        }
    }

    /// Hand over the answers to every run asked for so far.
    ///
    /// Keyed by plugin, and published per plugin in `enter` rather than into one
    /// shared table: a plugin reading another's `docker` output would be a
    /// capability nobody granted.
    pub fn set_runs(&self, answers: RunAnswers) {
        *self.run_answers.borrow_mut() = answers;
    }

    /// Record which plugins the user has turned off, by their path.
    ///
    /// Handed in for the same reason trust is: the decision is persisted with
    /// the interface's other user preferences, and the host has no business
    /// opening that file. Takes effect on the next `reload`.
    pub fn set_disabled(&self, paths: Vec<String>) {
        *self.disabled.borrow_mut() = paths;
    }

    /// Record which plugins the user has trusted, by their path.
    ///
    /// Handed in rather than read here: trust is persisted with the interface's
    /// other user decisions, and the host has no business opening that file.
    pub fn set_trusted(&self, paths: Vec<String>) {
        *self.trusted.borrow_mut() = paths;
    }

    /// Take the runs plugins asked for this frame.
    pub fn drain_runs(&self) -> Vec<(String, super::runs::Ask)> {
        std::mem::take(&mut *self.runs.borrow_mut())
    }

    pub fn drain_commands(&self) -> Vec<Command> {
        std::mem::take(&mut *self.queue.borrow_mut())
    }

    /// Publish the current snapshot so plugins can read it.
    ///
    /// Called once per frame from the event loop — never from inside a plugin,
    /// which is what keeps every read immediate (design.md D5).
    pub fn publish(&self, world: &Published) -> Result<(), String> {
        let Published {
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
        let sessions = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, row) in snapshot.sessions.iter().enumerate() {
            let entry = self.lua.create_table().map_err(|e| e.to_string())?;
            let set = |key: &str, value: Value| -> Result<(), String> {
                entry.set(key, value).map_err(|e| e.to_string())
            };
            set("id", to_lua_string(&self.lua, &row.id)?)?;
            set("name", to_lua_string(&self.lua, &row.name)?)?;
            set("agent", to_lua_string(&self.lua, &row.agent)?)?;
            let attach_error = attach_errors.get(&row.id).map(String::as_str);
            set(
                "status",
                to_lua_string(
                    &self.lua,
                    &super::snapshot::with_reachability(&row.status, &row.backend, attach_error),
                )?,
            )?;
            set("backend", to_lua_string(&self.lua, &row.backend)?)?;
            set("repo", opt_lua_string(&self.lua, row.repo.as_deref())?)?;
            set("branch", opt_lua_string(&self.lua, row.branch.as_deref())?)?;
            // What the diff is taken against, so a pane can name the range it is
            // showing rather than guessing. Distinct from `branch` above.
            set(
                "base_branch",
                opt_lua_string(&self.lua, row.base_branch.as_deref())?,
            )?;
            set(
                "host",
                opt_lua_string(&self.lua, row.remote_host.as_deref())?,
            )?;
            set(
                "parent",
                opt_lua_string(&self.lua, row.parent_id.as_deref())?,
            )?;
            set(
                "cwd",
                opt_lua_string(
                    &self.lua,
                    row.cwd.as_ref().map(|p| p.to_string_lossy()).as_deref(),
                )?,
            )?;
            entry
                .set("worktrees", row.worktree_count)
                .map_err(|e| e.to_string())?;
            // Every repo the session spans, so a group can be labelled with all
            // of them rather than just the primary.
            let repos = self.lua.create_table().map_err(|e| e.to_string())?;
            for (position, name) in row.repos.iter().enumerate() {
                repos
                    .set(position + 1, to_lua_string(&self.lua, name)?)
                    .map_err(|e| e.to_string())?;
            }
            set("repos", Value::Table(repos))?;
            // What the agent said about itself over its own terminal. Absent
            // when it has said nothing, which a row must tell apart from an
            // empty message.
            let agent_meta = meta.get(&row.id);
            set(
                "activity",
                opt_lua_string(&self.lua, agent_meta.and_then(|m| m.activity.as_deref()))?,
            )?;
            set(
                "notification",
                opt_lua_string(
                    &self.lua,
                    agent_meta.and_then(|m| m.notification.as_deref()),
                )?,
            )?;
            // Manual position. Without this a plugin cannot honour the order a
            // reorder command just wrote, and the list silently ignores it.
            entry
                .set("display_order", row.display_order)
                .map_err(|e| e.to_string())?;
            // Absent means "not computed yet", which a plugin must be able to
            // tell apart from a clean tree.
            if let Some(git) = row.git {
                let stats = self.lua.create_table().map_err(|e| e.to_string())?;
                stats
                    .set("files", git.files_changed)
                    .map_err(|e| e.to_string())?;
                stats
                    .set("insertions", git.insertions)
                    .map_err(|e| e.to_string())?;
                stats
                    .set("deletions", git.deletions)
                    .map_err(|e| e.to_string())?;
                stats
                    .set("untracked", git.untracked)
                    .map_err(|e| e.to_string())?;
                stats.set("dirty", git.dirty).map_err(|e| e.to_string())?;
                stats.set("ahead", git.ahead).map_err(|e| e.to_string())?;
                stats.set("behind", git.behind).map_err(|e| e.to_string())?;
                entry.set("git", stats).map_err(|e| e.to_string())?;
            }
            // Why this session's terminal is not live, when it is not.
            set("attach_error", opt_lua_string(&self.lua, attach_error)?)?;
            sessions.set(index + 1, entry).map_err(|e| e.to_string())?;
        }
        // A file read is rooted at a session's directory, so the roots follow
        // the snapshot rather than being asked for separately.
        {
            let mut roots = self.roots.borrow_mut();
            roots.clear();
            for row in &snapshot.sessions {
                if let Some(cwd) = &row.cwd {
                    roots.insert(row.id.clone(), cwd.clone());
                }
            }
        }

        table.set("sessions", sessions).map_err(|e| e.to_string())?;

        // What a creation flow can choose among. Reads, so picking is plugin
        // state and only committing is a command.
        let repos = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, repo) in snapshot.repos.iter().enumerate() {
            let item = self.lua.create_table().map_err(|e| e.to_string())?;
            item.set("path", repo.path.clone())
                .map_err(|e| e.to_string())?;
            item.set("name", repo.name.clone())
                .map_err(|e| e.to_string())?;
            repos.set(index + 1, item).map_err(|e| e.to_string())?;
        }
        table.set("repos", repos).map_err(|e| e.to_string())?;

        let agents = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, agent) in snapshot.agents.iter().enumerate() {
            let item = self.lua.create_table().map_err(|e| e.to_string())?;
            item.set("name", agent.name.clone())
                .map_err(|e| e.to_string())?;
            // v1's picker labels a row `name  (command)` when the two differ, so
            // the command travels with the name.
            item.set("command", agent.command.clone())
                .map_err(|e| e.to_string())?;
            agents.set(index + 1, item).map_err(|e| e.to_string())?;
        }
        table.set("agents", agents).map_err(|e| e.to_string())?;
        // What a bare launch would use, so a flow preselects it rather than
        // whichever agent happens to be first.
        table
            .set("agent_default", snapshot.agent_default.clone())
            .map_err(|e| e.to_string())?;

        // `name` identifies, `detail` distinguishes on screen, `backend` is what
        // a command and a bookmark scope are keyed by — a flow needs all three,
        // and none follows from another.
        let hosts = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, host) in snapshot.hosts.iter().enumerate() {
            let item = self.lua.create_table().map_err(|e| e.to_string())?;
            item.set("name", host.name.clone())
                .map_err(|e| e.to_string())?;
            item.set("detail", host.detail.clone())
                .map_err(|e| e.to_string())?;
            item.set("backend", host.backend.clone())
                .map_err(|e| e.to_string())?;
            hosts.set(index + 1, item).map_err(|e| e.to_string())?;
        }
        table.set("hosts", hosts).map_err(|e| e.to_string())?;

        self.publish_repo_reads(&table, repo_store, wants)?;
        self.publish_settings(&table, settings)?;

        let deleted = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, row) in snapshot.deleted.iter().enumerate() {
            let item = self.lua.create_table().map_err(|e| e.to_string())?;
            item.set("id", row.id.clone()).map_err(|e| e.to_string())?;
            item.set("name", row.name.clone())
                .map_err(|e| e.to_string())?;
            // Restoring this one recovers committed work only.
            item.set("partial", row.partial)
                .map_err(|e| e.to_string())?;
            deleted.set(index + 1, item).map_err(|e| e.to_string())?;
        }
        table.set("deleted", deleted).map_err(|e| e.to_string())?;

        let tasks = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, row) in snapshot.tasks.iter().enumerate() {
            let item = self.lua.create_table().map_err(|e| e.to_string())?;
            item.set("id", row.id).map_err(|e| e.to_string())?;
            item.set("title", row.title.clone())
                .map_err(|e| e.to_string())?;
            item.set(
                "description",
                opt_lua_string(&self.lua, row.description.as_deref())?,
            )
            .map_err(|e| e.to_string())?;
            item.set("status", row.status.clone())
                .map_err(|e| e.to_string())?;
            item.set("source", row.source.clone())
                .map_err(|e| e.to_string())?;
            item.set(
                "url",
                opt_lua_string(&self.lua, row.external_url.as_deref())?,
            )
            .map_err(|e| e.to_string())?;
            // Epoch seconds, so the detail view can date a task.
            item.set("created_at", row.created_at)
                .map_err(|e| e.to_string())?;
            item.set("updated_at", row.updated_at)
                .map_err(|e| e.to_string())?;
            tasks.set(index + 1, item).map_err(|e| e.to_string())?;
        }
        table.set("tasks", tasks).map_err(|e| e.to_string())?;

        let automations = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, row) in snapshot.automations.iter().enumerate() {
            let item = self.lua.create_table().map_err(|e| e.to_string())?;
            item.set("id", row.id).map_err(|e| e.to_string())?;
            item.set("name", row.name.clone())
                .map_err(|e| e.to_string())?;
            item.set("schedule", row.schedule.clone())
                .map_err(|e| e.to_string())?;
            item.set("action", row.action.clone())
                .map_err(|e| e.to_string())?;
            item.set("enabled", row.enabled)
                .map_err(|e| e.to_string())?;
            item.set(
                "last_outcome",
                opt_lua_string(&self.lua, row.last_outcome.as_deref())?,
            )
            .map_err(|e| e.to_string())?;
            item.set(
                "last_detail",
                opt_lua_string(&self.lua, row.last_detail.as_deref())?,
            )
            .map_err(|e| e.to_string())?;
            // The whole history, not just the last outcome: v1's run-history
            // pane lists every recent run, and a plugin cannot reconstruct
            // those from `last_outcome` alone.
            let runs = self.lua.create_table().map_err(|e| e.to_string())?;
            for (position, run) in row.runs.iter().enumerate() {
                let entry = self.lua.create_table().map_err(|e| e.to_string())?;
                entry
                    .set("started_at", run.started_at)
                    .map_err(|e| e.to_string())?;
                entry
                    .set("status", run.status.clone())
                    .map_err(|e| e.to_string())?;
                entry
                    .set("detail", run.detail.clone())
                    .map_err(|e| e.to_string())?;
                runs.set(position + 1, entry).map_err(|e| e.to_string())?;
            }
            item.set("runs", runs).map_err(|e| e.to_string())?;
            automations
                .set(index + 1, item)
                .map_err(|e| e.to_string())?;
        }
        table
            .set("automations", automations)
            .map_err(|e| e.to_string())?;
        table
            .set("taken_at_ms", snapshot.taken_at_ms)
            .map_err(|e| e.to_string())?;
        // The running release, for the header banner. v1 baked it into
        // `ui::status_bar::render_header` at compile time; a plugin cannot read
        // an env var, and hardcoding it in Lua would leave every shipped copy
        // claiming whatever version it was written against.
        table
            .set("version", env!("THURBOX_VERSION"))
            .map_err(|e| e.to_string())?;
        // What the pointer is over, as `id` and `role`, so a plugin can match
        // whichever it used to mark the affordance.
        let hover = self.lua.create_table().map_err(|e| e.to_string())?;
        if let Some(identity) = hovered {
            if let Some(id) = &identity.id {
                hover.set("id", id.clone()).map_err(|e| e.to_string())?;
            }
            if let Some(role) = &identity.role {
                hover.set("role", role.clone()).map_err(|e| e.to_string())?;
            }
        }
        table.set("hover", hover).map_err(|e| e.to_string())?;

        // Which pane holds focus, by name. `ctx.focused` answers "am I?", but
        // the footer has to name whoever IS and is not focusable itself.
        table
            .set("focus", focus.unwrap_or(""))
            .map_err(|e| e.to_string())?;
        // Published so a plugin can render how many times it has been reloaded
        // — the feedback that tells you a save actually took effect.
        table
            .set("reloads", self.reloads)
            .map_err(|e| e.to_string())?;

        // Work accepted but not yet visible in the rows above. Published so a
        // plugin can draw it rather than leaving an unexplained gap — what v1
        // needed `PendingSpawn` for.
        let commands = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, entry) in inflight.iter().enumerate() {
            let item = self.lua.create_table().map_err(|e| e.to_string())?;
            item.set("id", entry.id).map_err(|e| e.to_string())?;
            item.set("kind", entry.kind).map_err(|e| e.to_string())?;
            item.set("session", entry.session.clone())
                .map_err(|e| e.to_string())?;
            item.set("phase", entry.phase.as_str())
                .map_err(|e| e.to_string())?;
            item.set(
                "subject",
                opt_lua_string(&self.lua, entry.subject.as_deref())?,
            )
            .map_err(|e| e.to_string())?;
            item.set("error", opt_lua_string(&self.lua, entry.error.as_deref())?)
                .map_err(|e| e.to_string())?;
            commands.set(index + 1, item).map_err(|e| e.to_string())?;
        }
        table.set("commands", commands).map_err(|e| e.to_string())?;

        // The active palette, as role -> colour. Plugins name roles and never
        // colours, so this is the only place a literal enters the UI — and
        // swapping it restyles every pane, including ones the theme's author
        // never saw (design.md D14).
        let roles = self.lua.create_table().map_err(|e| e.to_string())?;
        for (role, colour) in themes.roles() {
            roles.set(role, colour).map_err(|e| e.to_string())?;
        }
        let theme = self.lua.create_table().map_err(|e| e.to_string())?;
        theme
            .set("name", themes.active_name())
            .map_err(|e| e.to_string())?;
        theme.set("roles", roles).map_err(|e| e.to_string())?;

        // The selectable list, so a picker can be an ordinary plugin.
        let choices = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, choice) in themes.choices().iter().enumerate() {
            let item = self.lua.create_table().map_err(|e| e.to_string())?;
            item.set("name", choice.name.clone())
                .map_err(|e| e.to_string())?;
            item.set("display_name", choice.display_name.clone())
                .map_err(|e| e.to_string())?;
            item.set("light", choice.is_light)
                .map_err(|e| e.to_string())?;
            item.set("custom", choice.is_custom)
                .map_err(|e| e.to_string())?;
            choices.set(index + 1, item).map_err(|e| e.to_string())?;
        }
        theme.set("choices", choices).map_err(|e| e.to_string())?;
        table.set("theme", theme).map_err(|e| e.to_string())?;

        // The registry, so help and settings can be plugins rendering what the
        // kernel collected — including declarations from plugins they have
        // never heard of.
        let keys = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, binding) in registry.bindings().iter().enumerate() {
            let item = self.lua.create_table().map_err(|e| e.to_string())?;
            item.set("plugin", binding.plugin.clone())
                .map_err(|e| e.to_string())?;
            item.set("action", binding.action.clone())
                .map_err(|e| e.to_string())?;
            item.set("key", binding.chord.clone())
                .map_err(|e| e.to_string())?;
            item.set("default_key", binding.default_chord.clone())
                .map_err(|e| e.to_string())?;
            item.set("desc", binding.description.clone())
                .map_err(|e| e.to_string())?;
            item.set("scope", binding.scope.as_str())
                .map_err(|e| e.to_string())?;
            item.set("rebound", binding.chord != binding.default_chord)
                .map_err(|e| e.to_string())?;
            item.set("group", binding.group.clone())
                .map_err(|e| e.to_string())?;
            keys.set(index + 1, item).map_err(|e| e.to_string())?;
        }
        let settings = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, setting) in registry.settings().iter().enumerate() {
            let item = self.lua.create_table().map_err(|e| e.to_string())?;
            item.set("plugin", setting.plugin.clone())
                .map_err(|e| e.to_string())?;
            item.set("id", setting.id.clone())
                .map_err(|e| e.to_string())?;
            item.set("desc", setting.description.clone())
                .map_err(|e| e.to_string())?;
            item.set("type", setting.value.type_name())
                .map_err(|e| e.to_string())?;
            set_value(&item, "value", &setting.value)?;
            set_value(&item, "default", &setting.default)?;
            settings.set(index + 1, item).map_err(|e| e.to_string())?;
        }
        let reg = self.lua.create_table().map_err(|e| e.to_string())?;
        reg.set("keys", keys).map_err(|e| e.to_string())?;
        reg.set("settings", settings).map_err(|e| e.to_string())?;
        // The section order help renders in, so a plugin choosing a `group` can
        // see where it will land without hardcoding the list.
        let sections = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, name) in super::registry::HELP_SECTIONS.iter().enumerate() {
            sections.set(index + 1, *name).map_err(|e| e.to_string())?;
        }
        reg.set("sections", sections).map_err(|e| e.to_string())?;
        table.set("registry", reg).map_err(|e| e.to_string())?;

        // The selected session's changes, when any have been asked for. The
        // *absence* of an entry is "not requested"; `pending` is "asked, not
        // finished" — a slow diff must not read as a clean worktree.
        let diff_table = self.lua.create_table().map_err(|e| e.to_string())?;
        for row in &snapshot.sessions {
            let Some(diff) = diffs.get(&row.id) else {
                continue;
            };
            let item = self.lua.create_table().map_err(|e| e.to_string())?;
            match diff {
                super::diff::Diff::Pending => {
                    item.set("state", "pending").map_err(|e| e.to_string())?;
                }
                super::diff::Diff::Failed(error) => {
                    item.set("state", "failed").map_err(|e| e.to_string())?;
                    item.set("error", error.clone())
                        .map_err(|e| e.to_string())?;
                }
                super::diff::Diff::Ready {
                    files,
                    body,
                    truncated,
                    raw_bytes,
                } => {
                    item.set("state", "ready").map_err(|e| e.to_string())?;
                    item.set("truncated", *truncated)
                        .map_err(|e| e.to_string())?;
                    item.set("raw_bytes", *raw_bytes as i64)
                        .map_err(|e| e.to_string())?;
                    let list = self.lua.create_table().map_err(|e| e.to_string())?;
                    for (index, file) in files.iter().enumerate() {
                        let entry = self.lua.create_table().map_err(|e| e.to_string())?;
                        entry
                            .set("path", file.path.clone())
                            .map_err(|e| e.to_string())?;
                        entry.set("added", file.added).map_err(|e| e.to_string())?;
                        entry
                            .set("removed", file.removed)
                            .map_err(|e| e.to_string())?;
                        // Both already known to the parser; a pane re-reading the
                        // body to recover them is the cost of dropping them here.
                        entry
                            .set("status", file.status)
                            .map_err(|e| e.to_string())?;
                        entry
                            .set(
                                "old_path",
                                opt_lua_string(&self.lua, file.old_path.as_deref())?,
                            )
                            .map_err(|e| e.to_string())?;
                        list.set(index + 1, entry).map_err(|e| e.to_string())?;
                    }
                    item.set("files", list).map_err(|e| e.to_string())?;
                    let lines = self.lua.create_table().map_err(|e| e.to_string())?;
                    for (index, line) in body.iter().enumerate() {
                        lines
                            .set(index + 1, line.clone())
                            .map_err(|e| e.to_string())?;
                    }
                    item.set("body", lines).map_err(|e| e.to_string())?;
                }
            }
            diff_table
                .set(row.id.clone(), item)
                .map_err(|e| e.to_string())?;
        }
        table.set("diffs", diff_table).map_err(|e| e.to_string())?;

        let links_table = self.lua.create_table().map_err(|e| e.to_string())?;
        for (session, found) in links.iter() {
            let list = self.lua.create_table().map_err(|e| e.to_string())?;
            for (index, (url, row, col)) in found.iter().enumerate() {
                let item = self.lua.create_table().map_err(|e| e.to_string())?;
                item.set("url", url.clone()).map_err(|e| e.to_string())?;
                item.set("row", *row).map_err(|e| e.to_string())?;
                item.set("col", *col).map_err(|e| e.to_string())?;
                list.set(index + 1, item).map_err(|e| e.to_string())?;
            }
            links_table
                .set(session.clone(), list)
                .map_err(|e| e.to_string())?;
        }
        table.set("links", links_table).map_err(|e| e.to_string())?;

        // What each terminal is showing, for a content search. Empty unless
        // something asked, so an interface that never searches never pays for it.
        let content_table = self.lua.create_table().map_err(|e| e.to_string())?;
        for (session, text) in content.iter() {
            content_table
                .set(session.clone(), text.clone())
                .map_err(|e| e.to_string())?;
        }
        table
            .set("content", content_table)
            .map_err(|e| e.to_string())?;

        // Machine, per-agent and account metrics. Each is absent rather than
        // zero when it has not been sampled, so a panel can distinguish "no
        // reading yet" from "nothing spent" — v1's info panel draws the same
        // distinction by omitting the row.
        let metrics_table = self.lua.create_table().map_err(|e| e.to_string())?;
        {
            let system = metrics.system();
            let machine = self.lua.create_table().map_err(|e| e.to_string())?;
            machine
                .set("cpu_percent", system.cpu_percent)
                .map_err(|e| e.to_string())?;
            machine
                .set("memory_used", system.memory_used)
                .map_err(|e| e.to_string())?;
            machine
                .set("memory_total", system.memory_total)
                .map_err(|e| e.to_string())?;
            metrics_table
                .set("system", machine)
                .map_err(|e| e.to_string())?;

            let per_session = self.lua.create_table().map_err(|e| e.to_string())?;
            for row in &snapshot.sessions {
                let entry = self.lua.create_table().map_err(|e| e.to_string())?;
                let mut any = false;
                if let Some(resources) = metrics.resources(&row.id) {
                    entry
                        .set("cpu_percent", resources.cpu_percent)
                        .map_err(|e| e.to_string())?;
                    entry
                        .set("memory_bytes", resources.memory_bytes)
                        .map_err(|e| e.to_string())?;
                    any = true;
                }
                if let Some(agent) = metrics.agent(&row.id) {
                    entry
                        .set("agent", agent_metrics_table(&self.lua, agent)?)
                        .map_err(|e| e.to_string())?;
                    any = true;
                }
                if let Some(usage) = metrics.usage(&row.agent, row.remote_host.as_deref()) {
                    if !usage.is_empty() {
                        entry
                            .set("usage", usage_table(&self.lua, usage)?)
                            .map_err(|e| e.to_string())?;
                        any = true;
                    }
                }
                if any {
                    per_session
                        .set(row.id.clone(), entry)
                        .map_err(|e| e.to_string())?;
                }
            }
            metrics_table
                .set("sessions", per_session)
                .map_err(|e| e.to_string())?;
        }
        table
            .set("metrics", metrics_table)
            .map_err(|e| e.to_string())?;

        // What the arrangement needs to know about the chrome bands, and nothing
        // more: whether the message band wants a row. The message itself is not
        // here, because placing a band and filling it are different jobs.
        let chrome = self.lua.create_table().map_err(|e| e.to_string())?;
        chrome
            .set("status_rows", *status_rows)
            .map_err(|e| e.to_string())?;
        table.set("chrome", chrome).map_err(|e| e.to_string())?;

        // The interface's own files. One row per file rather than per loaded
        // plugin, because the ones worth acting on are exactly those that did
        // NOT load — a removed pane, or one whose error is why the screen still
        // shows the last good version.
        let inventory_table = self.lua.create_table().map_err(|e| e.to_string())?;
        for (index, row) in inventory.iter().enumerate() {
            let entry = self.lua.create_table().map_err(|e| e.to_string())?;
            let set = |key: &str, value: Value| -> Result<(), String> {
                entry.set(key, value).map_err(|e| e.to_string())
            };
            set("path", to_lua_string(&self.lua, &row.path)?)?;
            set("name", to_lua_string(&self.lua, &row.name)?)?;
            set("kind", to_lua_string(&self.lua, row.kind.as_str())?)?;
            set("slot", to_lua_string(&self.lua, &row.slot)?)?;
            set("source", to_lua_string(&self.lua, row.source.as_str())?)?;
            set("state", to_lua_string(&self.lua, row.state.as_str())?)?;
            set("error", opt_lua_string(&self.lua, row.error.as_deref())?)?;
            inventory_table
                .set(index + 1, entry)
                .map_err(|e| e.to_string())?;
        }
        table
            .set("plugins", inventory_table)
            .map_err(|e| e.to_string())?;
        table
            .set("ui_dir", to_lua_string(&self.lua, ui_dir)?)
            .map_err(|e| e.to_string())?;

        // The machine this is running on, so a plugin delivering more than one
        // build can choose between them itself.
        //
        // Published rather than expressed in a package manifest, deliberately: a
        // substitution template states one rule, while a pane that can read this
        // states every rule it actually needs — prefer a binary already on `PATH`,
        // fall back to a portable build, distinguish a libc variant, or say politely
        // that it has nothing for you. The kernel models none of that.
        let platform = self.lua.create_table().map_err(|e| e.to_string())?;
        platform
            .set("os", std::env::consts::OS)
            .map_err(|e| e.to_string())?;
        platform
            .set("arch", std::env::consts::ARCH)
            .map_err(|e| e.to_string())?;
        table.set("platform", platform).map_err(|e| e.to_string())?;

        // So a plugin can say "open" or "copy" before you press it.
        table
            .set("can_open_links", *can_open)
            .map_err(|e| e.to_string())?;
        table
            .set(
                "error",
                opt_lua_string(&self.lua, snapshot.error.as_deref())?,
            )
            .map_err(|e| e.to_string())?;

        self.lua
            .globals()
            .set("thurbox", table)
            .map_err(|e| e.to_string())
    }

    /// Render one plugin into its resolved rect.
    ///
    /// Failures are returned rather than propagated, so the caller can paint an
    /// error panel in this plugin's rect and carry on with the others.
    pub fn render(&self, index: usize, ctx: RenderContext) -> Result<Rendered, PluginError> {
        let plugin = self.plugins.get(index).ok_or_else(|| PluginError {
            plugin: format!("#{index}"),
            phase: Phase::Render,
            message: "plugin index out of range".to_string(),
        })?;

        let fail = |message: String| PluginError {
            plugin: plugin.name.clone(),
            phase: Phase::Render,
            message,
        };

        let render: Function = plugin
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
        table
            .set("elapsed", ctx.elapsed)
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("frame", ctx.frame)
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("name", plugin.name.clone())
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("slot", plugin.slot.clone())
            .map_err(|e| fail(e.to_string()))?;

        self.enter(plugin);
        let guard = Budget::arm(&self.lua);
        let result: Result<Value, mlua::Error> = render.call(table);
        drop(guard);

        let value = result.map_err(|e| fail(clean_error(&e)))?;
        let float = read_float(&value).map_err(fail)?;
        let node = convert::to_node(&value, &plugin.path).map_err(fail)?;
        Ok(Rendered { node, float })
    }

    /// Offer a key to one plugin. `Ok(true)` means it consumed the key.
    pub fn on_key(&self, index: usize, key: &KeyPress) -> Result<bool, PluginError> {
        let Some(plugin) = self.plugins.get(index) else {
            return Ok(false);
        };
        let fail = |message: String| PluginError {
            plugin: plugin.name.clone(),
            phase: Phase::Key,
            message,
        };

        let handler: Value = plugin.def.get("on_key").map_err(|e| fail(e.to_string()))?;
        let Value::Function(handler) = handler else {
            return Ok(false);
        };

        let table = self.lua.create_table().map_err(|e| fail(e.to_string()))?;
        table
            .set("key", key.name.clone())
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("char", key.ch.map(|c| c.to_string()))
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("ctrl", key.ctrl)
            .map_err(|e| fail(e.to_string()))?;
        table.set("alt", key.alt).map_err(|e| fail(e.to_string()))?;
        table
            .set("shift", key.shift)
            .map_err(|e| fail(e.to_string()))?;

        self.enter(plugin);
        let guard = Budget::arm(&self.lua);
        let handled: Result<bool, mlua::Error> = handler.call(table);
        drop(guard);

        handled.map_err(|e| fail(clean_error(&e)))
    }

    /// Offer a click to the plugin that painted the node under it.
    ///
    /// Reached only for identity the kernel has no verb for — a list row, say.
    /// The verbs it does know ([`super::node::ClickVerb`]) are answered by the
    /// loop itself, so a footer pill or a modal button needs no `on_click` at
    /// all and cannot behave differently from its key.
    pub fn on_click(&self, index: usize, click: &Click) -> Result<bool, PluginError> {
        let Some(plugin) = self.plugins.get(index) else {
            return Ok(false);
        };
        let fail = |message: String| PluginError {
            plugin: plugin.name.clone(),
            phase: Phase::Key,
            message,
        };

        let handler: Value = plugin
            .def
            .get("on_click")
            .map_err(|e| fail(e.to_string()))?;
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

        self.enter(plugin);
        let guard = Budget::arm(&self.lua);
        let handled: Result<bool, mlua::Error> = handler.call(table);
        drop(guard);

        handled.map_err(|e| fail(clean_error(&e)))
    }

    /// The arrangement at this size.
    ///
    /// `ui/layout.lua` is loaded once per *reload*, not once per frame. Reading
    /// it every frame both wasted a syscall on the render path and — because
    /// inotify reports reads — made the host look like it was editing its own
    /// plugins, which kept the reload debounce rolling forward forever.
    pub fn arrangement(&self, width: u16, height: u16) -> Result<Region, String> {
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
                // `layout.lua` is not a plugin: it declares no capabilities and has
                // no trust record, so it must not execute under whichever grant the
                // last plugin call left installed.
                self.enter_nothing();
                let ctx = self.lua.create_table().map_err(|e| e.to_string())?;
                ctx.set("width", width).map_err(|e| e.to_string())?;
                ctx.set("height", height).map_err(|e| e.to_string())?;
                ctx.set("slots", self.occupied_slots_table()?)
                    .map_err(|e| e.to_string())?;

                let guard = Budget::arm(&self.lua);
                let result: Result<Value, mlua::Error> = arrange.call(ctx);
                drop(guard);

                let value = result.map_err(|e| format!("layout.lua: {}", clean_error(&e)))?;
                super::layout::region_from_lua(&value, "layout")
            }
        }
    }

    /// Slots a loaded plugin would actually paint into, as `{ [slot] = true }`.
    ///
    /// Published to the arrangement so it can decline to reserve a rect nothing
    /// will fill. Without it the two switches that remove a pane disagree: the
    /// panel toggle is arrangement state and the disabled set is delivery state,
    /// and `layout.lua` could only see the first — so turning a plugin off left
    /// its column reserved and empty, which is the opposite of the promise that
    /// the arrangement closes up around what is left.
    ///
    /// Floats and decorators are excluded for the same reason they are excluded
    /// from [`Self::in_slot`]: a float draws *above* the arrangement and a
    /// decorator draws *into* another plugin's tree, so neither one filling a
    /// slot is a reason to carve space out of the screen for it.
    pub fn occupied_slots(&self) -> BTreeSet<&str> {
        self.plugins
            .iter()
            .filter(|plugin| plugin.decorates.is_none() && !plugin.floats)
            .map(|plugin| plugin.slot.as_str())
            .collect()
    }

    /// Slots a loaded plugin occupies that the arrangement places nowhere.
    ///
    /// The failure this answers is invisible to every other check: such a plugin
    /// compiles, declares its keys, appears in the inventory and draws nothing.
    /// Reported by slot; the caller names the files, since it is the one holding
    /// the plugin list the user recognises.
    ///
    /// Two kinds of plugin are excluded before the question is asked, because
    /// neither is broken and reporting them would train the reader to ignore the
    /// answer. [`Self::occupied_slots`] already drops **floats** (they draw above
    /// the arrangement) and **decorators** (they draw into another plugin's
    /// tree); a **disabled** plugin never reaches `plugins` at all, since being
    /// disabled is implemented as not loading the file.
    ///
    /// A third kind is handled by [`Self::placed_slots`], which this defers to: a
    /// pane behind a **closed panel toggle**. `search` starts closed, so the
    /// bundled arrangement legitimately names no `search` slot until something
    /// opens it — which would make the interface we ship fail its own check.
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

    /// Slots the arrangement places at `area`, with every panel toggle opened.
    ///
    /// Extracted so "placed" means one thing: the check that fails an install and
    /// the listing that reports a pane's state resolve the same arrangement at the
    /// same size. Reported as its own answer because *placement is knowable without
    /// a frame* — what needs one is which occupant of a placed slot is in front,
    /// and that is a separate question.
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

    fn occupied_slots_table(&self) -> Result<Table, String> {
        let table = self.lua.create_table().map_err(|e| e.to_string())?;
        for slot in self.occupied_slots() {
            table.set(slot, true).map_err(|e| e.to_string())?;
        }
        Ok(table)
    }

    /// Why this pane may be impossible to find, if it may be.
    ///
    /// The one install that cannot demonstrate itself. A pane whose slot no
    /// arrangement places fails `plugin check` loudly. A pane that is the **alternate
    /// occupant of a `switch` slot** passes every check, reports `installed`, and
    /// shows the user nothing: the slot's first occupant draws, and this one waits to
    /// be focused. Somebody who installs it, follows its README and launches sees an
    /// unchanged screen and reasonably concludes the install failed.
    ///
    /// The kernel already offers four ways to reach it — the action band, the focus
    /// ring, `F1`, and a `focus:<plugin>` click role — but none of them is automatic.
    /// A **pill** is: it is declared data the band enumerates without invoking
    /// anything. So the answer is not new machinery, it is telling the author, and
    /// this is the predicate both the check and the install report consult so they
    /// cannot disagree.
    ///
    /// `None` when the pane draws by default, or when it declares a pill and is
    /// therefore advertised.
    pub fn undiscoverable(&self, index: usize) -> Option<String> {
        let plugin = self.plugins.get(index)?;
        if plugin.floats || plugin.decorates.is_some() {
            return None;
        }
        if self.slot_mode(&plugin.slot) != SlotMode::Switch {
            return None;
        }
        // The first occupant of a switch slot is the one that draws.
        if self.in_slot(&plugin.slot).first() == Some(&index) {
            return None;
        }
        if !plugin.pills.is_empty() {
            return None;
        }
        Some(format!(
            "shares the {:?} slot and is not the one shown by default, and declares no \
             pill — so nothing on screen offers it. Declare one \
             (`pills = {{ {{ action = \"…\", label = \"…\" }} }}`) or it can only be \
             reached by cycling focus.",
            plugin.slot
        ))
    }

    /// Slot mode, declared by any plugin in the slot. Stack unless one says
    /// otherwise, so the common case needs no declaration.
    pub fn slot_mode(&self, slot: &str) -> SlotMode {
        for index in self.in_slot(slot) {
            if let Ok(Value::String(mode)) = self.plugins[index].def.get::<Value>("slot_mode") {
                if mode.to_string_lossy() == "switch" {
                    return SlotMode::Switch;
                }
            }
        }
        SlotMode::Stack
    }
}

/// Build a VM with the deliberate stdlib set and the memory ceiling.
fn new_vm() -> mlua::Result<Lua> {
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
const BUDGET_EXCEEDED: &str = "thurbox: instruction budget exceeded";

/// Arms the instruction-count hook for the duration of one plugin call.
///
/// This is the Lua 5.4 half of design.md D8. Luau donates `set_interrupt`;
/// stock Lua does not, so the equivalent is a count hook that raises an error
/// once the budget is spent — which unwinds the plugin call and surfaces as an
/// ordinary plugin failure. It is what stops `while true do end` from taking
/// the render loop with it.
///
/// Disarmed on drop, so the budget is per call rather than per VM.
struct Budget<'a> {
    lua: &'a Lua,
}

impl<'a> Budget<'a> {
    fn arm(lua: &'a Lua) -> Self {
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
fn load_arrangement(lua: &Lua, ui_dir: &Path) -> Result<Arrangement, String> {
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
        other => Ok(Arrangement::Static(super::layout::region_from_lua(
            &other, "layout",
        )?)),
    }
}

/// Read one plugin file into a [`Plugin`].
///
/// `relative` is the path within the interface directory, carried through
/// rather than re-derived: the kernel writes and removes files by that name, so
/// it must be the same string everywhere.
fn load_plugin(lua: &Lua, path: &Path, relative: &str) -> Result<Plugin, String> {
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
fn read_float(value: &Value) -> Result<Option<Float>, String> {
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

/// Install `require`, `state` and `store` into a fresh VM.
#[allow(clippy::too_many_arguments)]
fn install_api(
    lua: &Lua,
    ui_dir: &Path,
    store: Shared,
    state: Private,
    current: Rc<RefCell<String>>,
    queue: Queue,
    roots: Roots,
    runs: Rc<RefCell<Vec<(String, super::runs::Ask)>>>,
    current_path: Rc<RefCell<String>>,
) -> mlua::Result<()> {
    scrub_globals(lua)?;
    install_require(lua, ui_dir)?;
    install_store(lua, "store", store, None)?;
    install_private(lua, state, current)?;
    install_command(lua, queue, current_path.clone())?;
    install_files(lua, roots)?;
    install_run(lua, runs, current_path)?;
    Ok(())
}

/// One run as a plugin reads it.
///
/// `state` is the field to branch on — `pending`, `done` or `failed` — because a
/// pane must be able to tell a program that has not answered from one that
/// answered with nothing.
fn run_to_lua(lua: &Lua, run: &super::runs::Run) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    match run {
        super::runs::Run::Pending => {
            table.set("state", "pending")?;
        }
        super::runs::Run::Failed(reason) => {
            table.set("state", "failed")?;
            table.set("error", reason.clone())?;
        }
        super::runs::Run::Done(output) => {
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
const RUN_IMPL: &str = "__run_impl";

/// `run(key, program, opts)` — ask for a program and read the answer later.
///
/// Queued, never executed here: the whole point is that a plugin cannot call
/// anything that waits. The asking plugin is stamped from `current`, so a run
/// is attributed without the plugin naming itself (and without being able to
/// claim another's key).
fn install_run(
    lua: &Lua,
    runs: Rc<RefCell<Vec<(String, super::runs::Ask)>>>,
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
            let ask = super::runs::Ask {
                key,
                program,
                session: opts
                    .as_ref()
                    .and_then(|t| t.get::<Option<String>>("session").ok().flatten())
                    .unwrap_or_default(),
                ttl: seconds("ttl").unwrap_or(super::runs::DEFAULT_TTL),
                timeout: seconds("timeout").unwrap_or(super::runs::DEFAULT_TIMEOUT),
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
/// design.md D2 of v2-navigation-panes.
fn install_files(lua: &Lua, roots: Roots) -> mlua::Result<()> {
    let files = lua.create_table()?;

    let list_roots = roots.clone();
    files.set(
        "list",
        lua.create_function(move |lua, (session, path): (String, Option<String>)| {
            let root = list_roots.borrow().get(&session).cloned().ok_or_else(|| {
                mlua::Error::runtime(format!("no directory for session {session}"))
            })?;
            let entries = super::files::list(&root, path.as_deref().unwrap_or(""))
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
            super::files::read(&root, &path).map_err(mlua::Error::runtime)
        })?,
    )?;

    lua.globals().set("files", files)?;
    Ok(())
}

/// `command("delete", { session = id })` — the only way a plugin changes state.
///
/// It enqueues and returns; it never runs the operation. That is the whole of
/// design.md D5's write side, and it is why a plugin cannot stall the render
/// loop with a database write or an unreachable host.
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
/// Removed rather than replaced with a stub that refuses, because design.md D9
/// is enforcement by *absence*: a plugin should find nothing there at all.
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
fn install_store(lua: &Lua, global: &str, store: Shared, _ns: Option<()>) -> mlua::Result<()> {
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
            match from_lua(&value) {
                Some(persisted) => {
                    slot.insert(key, persisted);
                }
                None => {
                    slot.remove(&key);
                }
            }
            Ok(())
        })?,
    )?;

    table.set_metatable(Some(meta))?;
    lua.globals().set(global, table)?;
    Ok(())
}

/// `state` — persistent, and private to the plugin currently being called.
fn install_private(lua: &Lua, state: Private, current: Rc<RefCell<String>>) -> mlua::Result<()> {
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
            match from_lua(&value) {
                Some(persisted) => {
                    slot.insert((ns, key), persisted);
                }
                None => {
                    slot.remove(&(ns, key));
                }
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

/// Trim mlua's wrapper noise so the pane shows the plugin's own message.
fn clean_error(error: &mlua::Error) -> String {
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

/// Wall-clock start, shared by the loop and any plugin animating off `elapsed`.
pub fn started() -> Instant {
    Instant::now()
}
