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
//!   this; on Lua 5.4 it is ours to build, so every call runs
//!   under an instruction-count hook and a memory ceiling.
//!
//! The VM is deliberately not `Send` — mlua's `send` feature stays off, which
//! makes "plugins never touch the render thread" a compile error.

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
use super::registry::{Binding, CommandDecl, Pill, Registry, Setting};
use super::snapshot::Snapshot;
use super::theme::Themes;

mod api;
mod load;
mod publish;

use self::api::{clean_error, install_api, RUN_IMPL};
use self::load::{load_arrangement, load_plugin, new_vm, read_float, Budget};
use self::publish::run_to_lua;

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
/// capability model is enforced by absence, not by a binding
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
/// How many times plugin state — shared or private — has been written.
///
/// A pure pane's tree may legitimately depend on `store` or `state`: the agent
/// pane remembers which tab each session is on, and one pane can read a value
/// another wrote. Neither is a published source, so without this a tree cached
/// before a keypress would survive it (`frame-cost`). Coarse on purpose — any
/// write invalidates every cached tree, which is right for something that
/// happens on input rather than per frame.
type StateVersion = Rc<std::cell::Cell<u64>>;
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
    /// An `on_event` handler — off the render path, so its failure is reported
    /// once per event rather than painted into a pane every frame.
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
    /// Declared `pure = true`: this pane's render is a function of the published
    /// tables and its render context, and of nothing else.
    ///
    /// An assertion the author makes, not a property the kernel can check — a
    /// render may write to `store` or read a per-frame clock, and neither is
    /// visible from outside the VM. Declaring it buys not being called on a
    /// frame where nothing it can read has changed; declaring it wrongly buys a
    /// pane painted from a stale tree. Opt-in for exactly that reason: a pane
    /// that says nothing behaves as it always has (`frame-cost`).
    pub pure: bool,
    /// Declared `input = "session"`: while this plugin holds focus, keys it
    /// does not handle are forwarded to the session its surface names.
    ///
    /// This is how the terminal pane stays an ordinary plugin. The kernel does
    /// not know which plugin "is" the terminal — it knows this one asked for
    /// raw input, and which session the tree it just returned pointed at.
    pub session_input: bool,
    /// Space this plugin asks of its slot. Declared **statically** here rather
    /// than in render output — that is what lets the layout pass resolve rects
    /// before any plugin runs.
    pub size: Size,
    /// Slot whose rendered tree this plugin may transform.
    ///
    /// Cross-plugin decoration: a decorator receives another plugin's tree
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
    /// Events this plugin subscribed to, each validated at load against
    /// [`super::events::KERNEL_EVENTS`] or the `user.` form.
    ///
    /// Data, like `keys`: a handler with no list receives nothing, so what a
    /// plugin listens for is enumerable without calling it.
    pub events: Vec<String>,
    /// Actions this plugin wants reachable without a chord — the palette's rows.
    pub commands: Vec<CommandDecl>,
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
    /// The macOS Command key (crossterm's `SUPER`).
    ///
    /// Carried rather than dropped, which it was until issue #1024: a `Cmd`
    /// chord arrived as the bare letter, so `cmd+c` could be written in a
    /// binding and never fire. It reaches a terminal at all only under the
    /// kitty keyboard protocol — see [`super::clipboard`].
    pub cmd: bool,
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
///
/// The tree is behind an `Rc` so a pure-pane cache *hit* costs a refcount bump
/// rather than a deep clone of the whole node tree — and so the settle diff
/// can short-circuit on pointer identity: the same `Rc` handed out twice IS
/// the same tree, with no per-node comparison needed (ADR-P16's cache saved
/// the Lua call and the conversion but still paid two tree-sized clones per
/// pane per frame).
#[derive(Debug, Clone)]
pub struct Rendered {
    pub node: std::rc::Rc<Node>,
    /// Set when the plugin asked to float above the arrangement.
    pub float: Option<Float>,
}

/// Everything published to plugins in one frame.
///
/// A struct rather than a parameter list because this grew from two to six in
/// the course of one change, and each growth churned every call site. Adding
/// something plugins can read should not be an edit to every test.
/// The version each published source stood at when a frame was published.
///
/// A group rebuilt at one epoch and asked for again at the same one cannot have
/// differed, which is the whole of the gate (`frame-cost`). Each group names the
/// fields it is built from rather than taking the whole struct, so a source that
/// moves every frame only invalidates what actually reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Epoch {
    /// `SnapshotStore::version`.
    pub snapshot: u64,
    /// `Themes::version`.
    pub themes: u64,
    /// `Registry::version`.
    pub registry: u64,
    /// `Terminals::meta_version`.
    pub meta: u64,
    /// `Terminals::failed_version`.
    pub failed: u64,
    /// The loop's own `data_epoch` — the worker stores, links, screen text.
    pub data: u64,
    /// The shared animation clock, which advances **only while something is
    /// actually animating** (a working session, a command in flight).
    ///
    /// A free-running clock here was a bug worth naming: it moved 8 times a
    /// second whether or not anything on screen was moving, so at the 4fps idle
    /// floor every pure pane missed its cache on every frame and re-ran for a
    /// byte-identical tree. `frame-cost` requires an idle interface to rebuild
    /// neither its groups nor its panes; a clock that never stops makes that
    /// impossible to satisfy.
    pub animation: u64,
}

/// The versions one group is built from, compared exactly.
///
/// A fixed array rather than a hash of them: a hash collision here would reuse a
/// stale group, and "astronomically unlikely" is the wrong guarantee for a
/// wrong answer nobody can see. Unused slots stay zero.
type GroupKey = [u64; 4];

/// How often the shared animation clock may advance, in Hz.
///
/// Set to the rate `theme.spinner_frame` advances the working spinner at
/// (`math.floor(elapsed * 8)`), so a cached tree is dropped exactly when the
/// spinner would move to its next frame and never merely because time passed.
/// Changing one without the other either freezes the animation or re-renders
/// for nothing. The loop owns the counter — see [`Epoch::animation`].
pub const ANIMATION_HZ: f64 = 8.0;

/// What a pure pane's cached tree was built for: the publish epoch, the parts of
/// the render context it may depend on, and the plugin-state version.
///
/// `ctx.frame` and `ctx.elapsed` are deliberately absent. They move every frame,
/// so reading either here would mean never reusing anything. A pane may still
/// animate and be pure, but only from the shared clock in [`Epoch::animation`],
/// which advances at [`ANIMATION_HZ`] *and only while something is animating* —
/// which is why a pane animating per frame, or on a schedule of its own, may not
/// declare `pure`.
///
/// The last field is [`StateVersion`]. A pure render may read `store` or
/// `state` — the agent pane remembers a tab per session — and those are written
/// by handlers, not by anything published, so without it a tree cached before a
/// keypress would outlive it.
type TreeKey = (Epoch, u16, u16, bool, u64);

/// A pure pane's cached tree, and whether the clock is part of what keyed it.
///
/// [`Epoch::animation`] advances eight times a second for as long as any session
/// is `working`, which is most of the time on a machine with an agent running.
/// It is in [`TreeKey`], so every pure pane used to be re-rendered at that rate
/// however little it had to do with a spinner — measured at +51% CPU under load
/// (`docs/PERFORMANCE.md`, ADR-P21).
///
/// Every other TUI scopes animation to the thing that animates, and gets the
/// coupling for free because the animating widget is the one that asks to be
/// redrawn: a Textual widget calls `self.set_interval(1/60, self.refresh)` on
/// *itself*, a Bubble Tea spinner returns its own tick command, fidget.nvim's
/// `Anime` is the closure that reads `now`, and lualine redraws the statusline
/// alone. thurbox's panes do not ask — the kernel calls them — so the coupling
/// has to be observed instead: a tree can only depend on the clock if the render
/// that built it read `ctx.elapsed`, and the ctx table's metatable is what
/// notices. A pane that did not read it is served across an animation tick.
///
/// Deliberately detected rather than declared. A declaration defaulting to "does
/// not animate" freezes any pane whose author never read the release note, with
/// no error anywhere; one defaulting to "does" recovers almost nothing. Detection
/// cannot be wrong in either direction: the flag is recorded from the render that
/// produced the very tree being cached, and a pane that starts or stops reading
/// the clock re-keys itself on the render where it does.
struct CachedTree {
    key: TreeKey,
    rendered: Rendered,
    /// Whether the render that produced `rendered` read `ctx.elapsed`.
    reads_clock: bool,
}

impl CachedTree {
    /// Whether this tree answers for `want`.
    ///
    /// The animation clock is compared only for a tree that read it. Masked on
    /// *both* sides rather than skipped on one, so the comparison stays a plain
    /// equality and cannot drift as `TreeKey` grows.
    fn answers(&self, want: &TreeKey) -> bool {
        if self.reads_clock {
            return self.key == *want;
        }
        let (mut mine, mut theirs) = (self.key, *want);
        mine.0.animation = 0;
        theirs.0.animation = 0;
        mine == theirs
    }
}

impl Epoch {
    /// An epoch that matches no previous one, so every group is rebuilt.
    ///
    /// For a caller that holds no versions to report — a test, or any publish
    /// outside the render loop. Without it such a caller would publish
    /// `Epoch::default()` twice with *different* data and be handed the first
    /// one's groups back, which is the stale answer this whole mechanism exists
    /// to make impossible.
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
        }
    }
}

pub struct Published<'a> {
    /// Where each source stood, so a group that could not have changed is not
    /// rebuilt. See [`Epoch`].
    pub epoch: Epoch,
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
    /// The epoch the last publish ran at, so `render` can key a tree on it
    /// without the loop passing it twice.
    epoch: RefCell<Option<Epoch>>,
    /// How many pane renders and published groups were served from a cache.
    ///
    /// A skip is unobservable in what gets painted — that is the whole
    /// correctness claim — so it is only ever visible as a count. Published
    /// through the perf snapshot, and what `tests/kernel_frame_cost.rs` asserts
    /// on, since it has nothing else to assert on (`frame-cost`).
    skipped_renders: std::cell::Cell<u64>,
    reused_groups: std::cell::Cell<u64>,
    /// A pure pane's last converted tree, with the key it was built under.
    ///
    /// Keyed by plugin index. Dropped whenever the VM is rebuilt, alongside
    /// [`Self::groups`] — a `Node` outlives the Lua it came from, but a tree
    /// built by the previous version of a pane is not that pane's answer.
    trees: RefCell<HashMap<usize, CachedTree>>,
    /// The clock the current render is being handed, and whether it read it.
    ///
    /// `ctx.elapsed` is served through the ctx table's metatable rather than set
    /// as a field, so asking for it is observable. That is the whole mechanism
    /// behind [`CachedTree::reads_clock`] — see it for why the kernel wants to
    /// know.
    clock: Rc<std::cell::Cell<f64>>,
    clock_read: Rc<std::cell::Cell<bool>>,
    /// Published groups, each with the epoch it was built at.
    ///
    /// The outer `thurbox` table is still assembled fresh every frame from
    /// these; only the nested group values are reused. That is deliberate — it
    /// means a gating mistake can produce a stale *group* but never a torn
    /// table, and it keeps the change local to the group builders
    /// (`frame-cost`).
    groups: RefCell<HashMap<&'static str, (GroupKey, Value)>>,
    store: Shared,
    state: Private,
    /// Bumped by every `store`/`state` write; part of a pure pane's tree key.
    state_version: StateVersion,
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
    /// The snapshot version [`Roots`] was last rebuilt from, so publish skips
    /// re-cloning every session's cwd on frames where the snapshot stood still.
    roots_snapshot: std::cell::Cell<Option<u64>>,
    /// Commands issued by plugins, drained once per frame by the loop.
    ///
    /// Queued rather than executed inline: `command()` must return instantly,
    /// and a plugin must never be able to run a database write from inside a
    /// render call.
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
    /// Index lists derived from `plugins`, rebuilt on reload.
    ///
    /// Each answers a per-frame question — which panes are focusable, who
    /// occupies or decorates a slot, a slot's mode — that used to be a scan
    /// with a fresh `Vec` per call (several calls per frame, and `slot_mode` a
    /// Lua table read per member per call) over a set that only changes when
    /// the interface reloads.
    index: PluginIndex,
    layout: Arrangement,
    /// The last resolved arrangement, with everything it was resolved from.
    ///
    /// `layout.lua` runs through the VM and its result is converted node by
    /// node, every frame, from inputs that move rarely: the screen size, the
    /// occupied slots (fixed per reload), `store` (the panel toggles — covered
    /// by the state version), and the published tables it may consult (covered
    /// by the epoch, plus the chrome band's row count, which is published as a
    /// bare scalar and so carries no version of its own).
    layout_cache: RefCell<Option<(LayoutKey, std::rc::Rc<Region>)>>,
    /// The `status_rows` last published, the one arrangement input with no
    /// version — see [`Self::arrangement`]'s cache key.
    last_status_rows: std::cell::Cell<u16>,
    /// Set when the *last* reload attempt failed; `plugins` are still the ones
    /// from the last good build.
    pub error: Option<String>,
    pub reloads: u32,
}

/// What [`LuaHost::arrangement`]'s cache is keyed by. Compared exactly, like a
/// [`GroupKey`]: a stale arrangement is wrong rects, not a slow frame.
type LayoutKey = (u16, u16, u32, Option<Epoch>, u64, u16);

/// See [`LuaHost::index`].
#[derive(Default)]
struct PluginIndex {
    focusable: Vec<usize>,
    floating: Vec<usize>,
    /// Slot -> occupying plugin indices, in render order (decorators excluded).
    slots: HashMap<String, Vec<usize>>,
    /// Slot -> indices of the plugins decorating it, in render order.
    decorators: HashMap<String, Vec<usize>>,
    /// Slot -> declared mode. Absent = stack, so the common case needs no entry.
    modes: HashMap<String, SlotMode>,
}

impl PluginIndex {
    fn build(plugins: &[Plugin]) -> Self {
        let mut index = Self::default();
        for (i, plugin) in plugins.iter().enumerate() {
            if plugin.focusable {
                index.focusable.push(i);
            }
            if plugin.floats {
                index.floating.push(i);
            }
            match &plugin.decorates {
                Some(slot) => index.decorators.entry(slot.clone()).or_default().push(i),
                None => index.slots.entry(plugin.slot.clone()).or_default().push(i),
            }
            // `slot_mode` is a static declaration, read once here instead of
            // through the Lua table on every placement query.
            if let Ok(Value::String(mode)) = plugin.def.get::<Value>("slot_mode") {
                if mode.to_string_lossy() == "switch" {
                    index.modes.insert(plugin.slot.clone(), SlotMode::Switch);
                }
            }
        }
        index
    }
}

impl LuaHost {
    pub fn new(ui_dir: impl Into<PathBuf>) -> Self {
        let ui_dir = ui_dir.into();
        let mut host = Self {
            lua: new_vm().unwrap_or_else(|_| Lua::new()),
            ui_dir,
            groups: RefCell::new(HashMap::new()),
            trees: RefCell::new(HashMap::new()),
            clock: Rc::new(std::cell::Cell::new(0.0)),
            clock_read: Rc::new(std::cell::Cell::new(false)),
            epoch: RefCell::new(None),
            skipped_renders: std::cell::Cell::new(0),
            reused_groups: std::cell::Cell::new(0),
            store: Shared::default(),
            state: Private::default(),
            state_version: StateVersion::default(),
            current: Rc::new(RefCell::new(String::new())),
            current_path: Rc::new(RefCell::new(String::new())),
            queue: Queue::default(),
            runs: Rc::new(RefCell::new(Vec::new())),
            trusted: Rc::new(RefCell::new(Vec::new())),
            disabled: Rc::new(RefCell::new(Vec::new())),
            run_answers: Rc::new(RefCell::new(std::collections::HashMap::new())),
            roots: Roots::default(),
            roots_snapshot: std::cell::Cell::new(None),
            plugins: Vec::new(),
            index: PluginIndex::default(),
            layout: Arrangement::Missing,
            layout_cache: RefCell::new(None),
            last_status_rows: std::cell::Cell::new(0),
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
                // Before the VM they point into is dropped: a cached group is a
                // handle into `self.lua`, so carrying one across a reload would
                // hand the next publish a value belonging to a Lua that no
                // longer exists.
                self.forget_groups();
                self.lua = lua;
                self.index = PluginIndex::build(&plugins);
                self.plugins = plugins;
                self.layout = layout;
                self.layout_cache.replace(None);
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
            self.state_version.clone(),
            self.clock.clone(),
            self.clock_read.clone(),
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

    /// The name of the plugin at a path — what an event's `source` carries.
    pub fn name_of_path(&self, path: &str) -> Option<&str> {
        self.plugins
            .iter()
            .find(|plugin| plugin.path == path)
            .map(|plugin| plugin.name.as_str())
    }

    /// Every chord-less command every loaded plugin declared.
    pub fn commands(&self) -> Vec<CommandDecl> {
        self.plugins
            .iter()
            .flat_map(|plugin| plugin.commands.iter().cloned())
            .collect()
    }

    /// Indices of the plugins subscribed to an event, in load order.
    pub fn subscribers(&self, event: &str) -> Vec<usize> {
        self.plugins
            .iter()
            .enumerate()
            .filter(|(_, plugin)| plugin.events.iter().any(|name| name == event))
            .map(|(index, _)| index)
            .collect()
    }

    /// Hand one event to every plugin subscribed to it.
    ///
    /// The payload table is built **once** and shared: a handler that mutates it
    /// changes what a later subscriber reads, which is the same contract a
    /// published table already has — and building it per subscriber would be a
    /// table per plugin per event for a value nobody is meant to write.
    ///
    /// Every subscriber is called whatever the earlier ones did: a handler that
    /// throws or overruns its budget costs itself, reported against its plugin
    /// with the event's name, and the next handler still runs. Return values are
    /// ignored — a handler cannot answer, only enqueue commands and write state.
    pub fn dispatch_event(&self, event: &Event) -> Vec<PluginError> {
        let subscribers = self.subscribers(&event.name);
        if subscribers.is_empty() {
            return Vec::new();
        }
        let mut failures = Vec::new();
        let payload = match self.payload_table(event) {
            Ok(table) => table,
            Err(message) => {
                failures.push(PluginError {
                    plugin: "kernel".to_string(),
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

    /// Call one plugin's `on_event` with a name and a payload table.
    fn on_event(&self, index: usize, name: &str, payload: &Table) -> Result<(), PluginError> {
        let Some(plugin) = self.plugins.get(index) else {
            return Ok(());
        };
        let fail = |message: String| PluginError {
            plugin: plugin.name.clone(),
            phase: Phase::Event,
            message: format!("{name}: {message}"),
        };
        let handler: Value = plugin
            .def
            .get("on_event")
            .map_err(|e| fail(e.to_string()))?;
        let Value::Function(handler) = handler else {
            return Ok(());
        };

        self.enter(plugin);
        let guard = Budget::arm(&self.lua);
        let outcome: Result<Value, mlua::Error> = handler.call((name.to_string(), payload.clone()));
        drop(guard);
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

    /// Indices of the plugins occupying one slot, in render order.
    ///
    /// A decorator is not an occupant: it draws INTO another plugin's tree, so
    /// it is indexed under `PluginIndex::decorators` instead — otherwise it
    /// would take the default slot and compete for the centre with the pane it
    /// exists to decorate.
    pub fn in_slot(&self, slot: &str) -> &[usize] {
        self.index.slots.get(slot).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Indices of plugins decorating `slot`, in render order.
    ///
    /// Render order is the deterministic order the spec requires: it follows
    /// each plugin's declared `order`, so two decorators on one slot apply the
    /// same way every run.
    pub fn decorators_of(&self, slot: &str) -> &[usize] {
        self.index
            .decorators
            .get(slot)
            .map(Vec::as_slice)
            .unwrap_or(&[])
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
    /// Bumps the state version exactly as the Lua `__newindex` path does —
    /// without it, a pure pane consuming the key is served its cached tree on a
    /// frame where nothing else moved, and the request sits unconsumed until
    /// some unrelated signal ticks the epoch (the failure ADR-P16 records for
    /// `thurbox.commands`).
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

    /// Put a boolean into the shared `store`.
    ///
    /// The counterpart to [`Self::shared_bool`], and used for the same panel
    /// flags: [`Self::placed_slots`] opens every column before resolving the
    /// arrangement, because a pane behind a closed toggle is not a missing pane.
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

    /// Indices of plugins that may float, in render order.
    pub fn floating(&self) -> &[usize] {
        &self.index.floating
    }

    /// Indices of every focusable plugin, in render order.
    pub fn focusable(&self) -> &[usize] {
        &self.index.focusable
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
    /// which is what keeps every read immediate.
    /// Build one published group, or hand back the one built at the same key.
    ///
    /// `key` combines only the versions that group actually reads, so a source
    /// moving every frame invalidates what reads it and nothing else. The value
    /// is a Lua reference, so reusing it costs a clone of a registry handle
    /// rather than a rebuild of the table behind it.
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
        let value = build()?;
        self.groups.borrow_mut().insert(name, (key, value.clone()));
        Ok(value)
    }

    /// Forget every cached group, so the next publish rebuilds all of them.
    ///
    /// Called where the VM itself is replaced: the cached values are handles
    /// into the Lua that is going away.
    pub fn forget_groups(&self) {
        self.groups.borrow_mut().clear();
        self.trees.borrow_mut().clear();
    }

    /// Pane renders skipped because a pure pane's tree still stood.
    pub fn skipped_renders(&self) -> u64 {
        self.skipped_renders.get()
    }

    /// Published groups served from the cache rather than rebuilt.
    pub fn reused_groups(&self) -> u64 {
        self.reused_groups.get()
    }

    /// The epoch of the most recent publish, which a pure pane's tree is keyed
    /// on. `None` before anything has been published.
    fn published_epoch(&self) -> Option<Epoch> {
        *self.epoch.borrow()
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
        if plugin.pure {
            if let Some(key) = key {
                if let Some(cached) = self.trees.borrow().get(&index) {
                    if cached.answers(&key) {
                        self.skipped_renders.set(self.skipped_renders.get() + 1);
                        return Ok(cached.rendered.clone());
                    }
                }
            }
        }

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
        // NOT set as a field: `elapsed` is served through the metatable installed
        // below, so that reading it is observable. See `CachedTree`.
        self.clock.set(ctx.elapsed);
        self.clock_read.set(false);
        api::attach_clock(&self.lua, &table).map_err(|e| fail(e.to_string()))?;
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
        // Behind the `Rc` from birth, so caching it and every later hand-out
        // is a refcount bump — see [`Rendered`].
        let rendered = Rendered {
            node: std::rc::Rc::new(node),
            float,
        };
        if plugin.pure {
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
                    },
                );
            }
        }
        Ok(rendered)
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
        table.set("cmd", key.cmd).map_err(|e| fail(e.to_string()))?;

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
    /// otherwise, so the common case needs no declaration. Read off the index
    /// built at load — the declaration is static, and answering it through the
    /// Lua table cost a metamethod-capable read per slot member per placement
    /// query, several queries per frame.
    pub fn slot_mode(&self, slot: &str) -> SlotMode {
        self.index
            .modes
            .get(slot)
            .copied()
            .unwrap_or(SlotMode::Stack)
    }
}
