//! thurbox v2 — a session engine with a Lua-driven renderer.
//!
//! The kernel owns no pane. It resolves rects, calls plugins, paints what they
//! return, and refreshes a snapshot of the session engine on its own schedule.
//! Every surface you see — the session list included — is a file under `ui/`
//! that you can edit while this is running.
//!
//! This file holds the loop's state — `App`, whose every field is documented
//! with what broke without it — plus the loop's constants. Startup lives in
//! `coordinator::boot`, the terminal/chrome helpers in `coordinator::chrome`,
//! and `App`'s behaviour in the rest of [`coordinator`], split by what each
//! group of methods is for. See `openspec/changes/archive/*-v2-plugin-kernel/`.

mod coordinator;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::{DefaultTerminal, Frame};

use thurbox::agent::input::key_to_bytes;
use thurbox::kernel::bands::{self, Band, BandState, Level};
use thurbox::kernel::command::CommandBus;
use thurbox::kernel::diff::DiffStore;
use thurbox::kernel::host::{Click, KeyPress, LuaHost, PluginError, RenderContext};
use thurbox::kernel::layout::{resolve, SlotMode};
use thurbox::kernel::metrics::{Metrics, Subject};
use thurbox::kernel::modals::{ModalKind, Modals};
use thurbox::kernel::node::{Axis, ClickVerb, Identity};
use thurbox::kernel::notify::Notifier;
use thurbox::kernel::paint;
use thurbox::kernel::perf::Counters;
use thurbox::kernel::registry::{canonical_chord, is_ctrl_letter_chord, Registry};
use thurbox::kernel::selection::{PaneBounds, Selection, TermPos};
use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::kernel::terminal::Terminals;
use thurbox::kernel::theme::Themes;
use thurbox::kernel::watch::Watcher;

/// How long the loop blocks waiting for input.
///
/// v1 uses 10ms (`src/main.rs`). At 50ms a keystroke could sit unnoticed for a
/// twentieth of a second before the loop even looked at it, which is felt as
/// lag however fast the frame that follows is. Polling this often is only
/// affordable because the expensive per-frame work below is gated on a paint
/// actually being due.
const TICK: Duration = Duration::from_millis(10);

/// The input poll's timeout once nothing has happened for [`QUIESCENT_AFTER`].
///
/// `event::poll` returns the instant an event arrives, so lengthening this costs
/// **no** input latency — a keystroke wakes the thread either way. What it slows
/// is noticing things that do not wake it: new agent output, a worker result, a
/// row another process wrote. At rest there is by definition none of the first,
/// and a 50ms delay on the others is not perceptible; the first sign of activity
/// puts the loop straight back on [`TICK`].
///
/// Worth 94 wakes a second against 20 on an idle interface, which was half its
/// entire cost.
const IDLE_TICK: Duration = Duration::from_millis(50);

/// How long nothing must happen before the loop slows its poll to
/// [`IDLE_TICK`]. Longer than a keypress-to-repaint round trip, so typing never
/// crosses into the slow poll and back.
const QUIESCENT_AFTER: Duration = Duration::from_millis(500);
/// Editors save in bursts (write, rename, chmod); wait for the dust to settle.
const DEBOUNCE: Duration = Duration::from_millis(120);
/// Longest a frame may go unpainted when nothing has changed.
///
/// Covers time-driven content the diff cannot see — a spinner, a clock — and is
/// what turns an idle app from ~20 fps into ~4. v1 uses the same floor.
const FORCE_REDRAW_INTERVAL: Duration = Duration::from_millis(250);

/// Iterations between two `perf_window` log lines (~10s at the 10ms tick).
const PERF_WINDOW_TICKS: u64 = 1000;

/// How often the JSON snapshot is written while timing is active. Slower than
/// the log line because it is a database write every other thurbox connection
/// pays for with a `data_version` bump.
const PERF_PUBLISH_INTERVAL: Duration = Duration::from_secs(5);

/// The floor between two paints.
///
/// The poll above runs every 10ms so input is noticed at once, but a frame here
/// costs far more than v1's -- every visible pane is rebuilt through Lua and
/// converted back. Without a cap, an agent streaming output marks the screen
/// dirty on every poll (`Terminals::output_generation`, checked in the loop) and
/// drives 100 paints a second for a terminal nobody can read that fast. 60fps
/// keeps typing and output feeling immediate while bounding the cost of a chatty
/// agent.
///
/// This cap only bites once output *causes* a frame at all. It did not until
/// the generation check was hoisted into the loop — before that a printing agent
/// was drawn at the `FORCE_REDRAW_INTERVAL` floor, four times a second, which is
/// what made v2 feel less responsive than v1.
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// The floor when the only thing owed a frame is new agent output.
///
/// Typing has to feel instant; watching a log scroll does not, and applying the
/// 16ms floor to both meant a chatty agent drove ~60 paints a second to show 30
/// lines. Measured across the interval (`docs/PERFORMANCE.md`, ADR-P17): 62fps
/// costs 21.2% of a core, 30fps costs 14.4% and 20fps 13.1% — most of the saving
/// arrives by 30, and below it the scroll starts to look stepped rather than
/// smooth. So: 30fps for output, and a keystroke still repaints on the next
/// frame.
const OUTPUT_FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// Consecutive input-read failures tolerated before the loop gives up.
///
/// One is a terminal handing crossterm bytes it cannot parse, which is a
/// keystroke to drop rather than a reason to quit. A run of them is a stream
/// that has gone away, and polling it forever would spin at full speed.
const INPUT_FAILURE_LIMIT: u32 = 64;
/// How long an outcome message stays up. v1's `STATUS_MESSAGE_TTL`.
const STATUS_TTL: Duration = Duration::from_secs(5);

// A tokio runtime is required, not decorative: adopting a session spawns its
// reader on `spawn_blocking` and its writer on `tokio::spawn`. The render loop
// below stays synchronous — as v1's does — and those tasks run on the worker
// pool.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    coordinator::boot::run().await
}

/// A hitbox from the frame just painted, and the plugin that painted it.
///
/// v1's `App::ClickTarget`, with the crucial difference that its `action` is
/// not an enum the kernel has to know: the identity travels as the plugin wrote
/// it, and only the handful of [`ClickVerb`]s are the kernel's business.
///
/// An **empty** identity is the plugin's own rect — v1's `FocusPane` fallback,
/// recorded before the tree so anything inside it wins.
#[derive(Debug, Clone)]
struct ClickTarget {
    plugin: usize,
    rect: Rect,
    identity: Identity,
}

/// A command seen in flight, remembered so its outcome can be reported and its
/// session let go of when it finishes.
///
/// Remembered rather than read back at the end because a finished command simply
/// leaves the list, taking what it was about with it — a deleted session's row
/// is already gone by the time its delete reports.
#[derive(Clone)]
struct TrackedCommand {
    kind: &'static str,
    session: String,
    label: Option<String>,
    failed: bool,
}

struct App {
    host: LuaHost,
    /// The directory the interface was loaded from. Held because every command
    /// about a plugin file names a path relative to it.
    ui_dir: PathBuf,
    /// Where each file of the interface came from.
    ///
    /// Cached because answering it reads and digests every file: it changes
    /// only when the directory does, which is exactly when the host reloads.
    sources: std::collections::BTreeMap<String, thurbox::kernel::bundled::Source>,
    watcher: Watcher,
    snapshots: SnapshotStore,
    terminals: Terminals,
    commands: CommandBus,
    diffs: DiffStore,
    /// What the creation flow asks about: remembered repositories, directory
    /// listings, branch lists. Requests arrive through `store` and are served
    /// on workers, like every other read that touches the world.
    repos: thurbox::kernel::repos::RepoStore,
    metrics: Metrics,
    /// Native clipboard handle, when the platform has one.
    ///
    /// Built once and kept: `clipboard::copy`/`paste` take it by reference, and
    /// passing `None` means every paste reports an unreachable clipboard — which
    /// is exactly what happened while this field did not exist. v1 holds the
    /// same handle for the same reason.
    clipboard: Option<arboard::Clipboard>,
    notifier: Notifier,
    perf: Counters,
    /// Wall-clock stats, populated only while timing is active (ADR-P11).
    timings: thurbox::kernel::perf::Timings,
    /// How long each startup phase took; published and logged once.
    startup: thurbox::kernel::perf::Startup,
    /// `THURBOX_PERF_LOG` was set, read once at construction. The other half of
    /// [`Self::perf_timing_active`] is the HUD, which can be toggled.
    perf_log: bool,
    /// Counters as they stood when the current perf window opened, so the
    /// `perf_window` line reports deltas rather than lifetime totals.
    perf_window_base: thurbox::kernel::perf::Snapshot,
    /// Iteration count at which the current perf window opened.
    perf_window_tick: u64,
    /// When the JSON snapshot was last written to the database.
    perf_published_at: Option<Instant>,
    /// The one-shot `startup` line is logged after the first painted frame.
    first_frame_logged: bool,
    /// True process start, taken before any startup phase — `started` is taken
    /// during construction and so misses everything before it.
    process_start: Instant,
    /// Whether this frame is owed to something a person did — a keypress, a
    /// resize, a worker result they asked for — rather than to an agent
    /// printing. Only the first kind gets [`MIN_FRAME_INTERVAL`].
    input_dirty: bool,
    /// When anything last happened — input, output, a worker result, a repaint
    /// that changed something. Drives the poll timeout, nothing else.
    last_activity: Instant,
    /// The shared animation clock, advanced only while something is animating.
    ///
    /// Kept here rather than read from `ctx.elapsed` in the render, because
    /// whether anything is animating is the loop's knowledge: a spinner turns
    /// for a session that is *working*, and the creation flow's pending row for
    /// a command in flight. With neither, the clock stands still and a pure
    /// pane's tree survives — which is what lets an idle interface stop
    /// rebuilding anything at all (`frame-cost`).
    animation_tick: u64,
    /// The last `elapsed * ANIMATION_HZ` step the tick was advanced for, so a
    /// step is counted once however many frames fall inside it.
    animation_step: u64,
    /// Moves whenever data the loop owns and publishes does — a worker store
    /// that took a result, the links or screen text just re-scanned, the
    /// in-flight command list, an attach failure.
    ///
    /// The stores already answer "did anything land" from `poll`, so this reads
    /// that rather than duplicating a counter inside each one: a signal derived
    /// from the existing return value cannot drift from it. Combined with the
    /// versions the kernel sources carry into [`Self::publish_epoch`].
    data_epoch: u64,
    /// Active mouse text selection over a terminal surface, if any.
    selection: Option<Selection>,
    themes: Themes,
    /// Which occupant of each `switch` slot is visible, by slot name.
    ///
    /// Focusing a plugin in a switch slot makes it the visible one, which is
    /// both how switching is driven and how the spec's "focus never rests on a
    /// hidden pane" rule is satisfied without a second mechanism.
    slot_selection: std::collections::HashMap<String, usize>,
    /// Whether a newer release exists, and the silent update if it was allowed.
    updates: thurbox::kernel::updates::Updates,
    /// The user's settings: the live half re-read when the file changes, the
    /// restart-only half as published at startup. See `kernel::config`.
    config: thurbox::kernel::config::Config,
    registry: Registry,
    /// Help, settings and the theme picker: kernel-owned, overlaying, and
    /// outside both the layout and the focus ring. See `kernel::modals`.
    modals: Modals,
    /// Slots the arrangement actually placed on the last frame.
    ///
    /// A side column is only in here while it is toggled open, so this is what
    /// keeps Tab from parking focus on a pane nobody can see — v1's rule that a
    /// panel is "a cycle stop only while visible".
    visible_slots: std::collections::HashSet<String>,
    /// A focus request whose slot the arrangement had not placed yet.
    ///
    /// Held for exactly one layout and re-asked there. See
    /// `kernel::focus::defer_until_placed`: a pane that opens its own slot asks
    /// for focus a frame before the slot exists, and judging that request against
    /// the frame that already painted refuses the focus its chord existed to give.
    pending_focus: Option<usize>,
    /// Every identified node of the frame just painted, in paint order.
    ///
    /// Rebuilt each frame and scanned in reverse, so the innermost node under a
    /// point — and, across plugins, the one painted last — wins. That is how a
    /// tab pill on a pane's border beats the pane's own focus fallback.
    click_targets: Vec<ClickTarget>,
    /// The area the last frame was painted into.
    ///
    /// A selection outside every terminal is anchored to this, so a drag over
    /// the session list or a modal has a rect to clamp against.
    last_area: Rect,
    /// The terminal's size, seeded once at startup and updated from
    /// `Event::Resize`.
    ///
    /// Cached because `terminal::size()` is a syscall and its two consumers run
    /// on every iteration of a loop that polls every 10 ms — `readopt_shells`
    /// already refused to pay it per iteration, and this extends the same
    /// reasoning to the attach seed size.
    screen_size: (u16, u16),
    /// The text under the current selection, read while the frame that painted
    /// it is still in hand.
    ///
    /// v1 caches it the same way (`selected_text_cache`) and for the same
    /// reason: a selection outside a terminal can only be read off the painted
    /// buffer, and the buffer is gone by the time `Ctrl+C` arrives.
    selected_text: Option<String>,
    /// The identity under the pointer, for hover highlighting.
    ///
    /// Stored as the identity rather than the position so a move WITHIN the
    /// same affordance is free: the redraw is gated on this changing, not on
    /// the pointer moving. v1 keeps the position instead and re-resolves it
    /// every frame; this way a mouse crossing the screen costs one repaint per
    /// affordance rather than one per cell.
    hovered: Option<Identity>,
    /// `[features] mouse`. Off means no capture escape was ever sent, so the
    /// terminal keeps its native selection and scrolling.
    mouse: bool,
    /// The session shown by the focused plugin's surface, as of the last frame.
    /// Read off the tree that was just painted, so the kernel never needs to
    /// know which plugin is "the terminal".
    focused_session: Option<String>,
    /// The surface the focused pane is showing, of either kind — a session's
    /// terminal or a program a plugin owns.
    ///
    /// Distinct from `focused_session` because the two answer different questions:
    /// that one is "which session am I looking at", which a program pane has no
    /// answer to, and this one is "where do unclaimed keys go".
    focused_surface: Option<String>,
    /// The session the list had selected last frame, so moving off one can
    /// acknowledge the finished turn it was showing.
    last_selected_session: Option<String>,
    /// Index into the host's focusable plugins.
    focus: usize,
    /// Where focus was before the last deliberate move, so `Esc` can go back.
    ///
    /// v1's pickers and panels are modals: `Esc` closes them and focus returns
    /// to what you were doing. v2's are centre-slot occupants, so "closing" one
    /// IS returning focus — without this, `Esc` in the theme picker did nothing
    /// at all once the kernel stopped treating a bare `Esc` as quit.
    focus_return: usize,
    /// Set once a change is seen, fired after the debounce window.
    reload_at: Option<Instant>,
    /// Failures from this frame's render calls, one per failing plugin.
    errors: Vec<PluginError>,
    /// A failure from `ui/layout.lua`, cleared by the next arrangement that
    /// works.
    layout_error: Option<String>,
    /// Why the bundled interface is running instead of the user's copy.
    ///
    /// Sticky, and separate from `layout_error` for a reason that cost the notice
    /// entirely: `layout_error` is cleared on every frame whose arrangement
    /// resolves, and the fallback's arrangement always does — so a message put
    /// there was wiped before it could ever be painted. The floor is a state that
    /// lasts until the user's copy loads again, so it is recorded as one.
    floor: Option<String>,
    /// What just happened, and when. Separate from `layout_error` because that
    /// field is reset by every successful arrangement — which is once a frame —
    /// so a message sharing it was gone before it could be read.
    status: Option<(String, Level, Instant)>,
    /// Commands whose failure has already been reported, so the window in which
    /// a failure lingers for the panes does not re-raise it every poll.
    reported_failures: std::collections::HashSet<u64>,
    /// Commands seen in flight: `id → (verb, session, what it is about, failed)`.
    ///
    /// Kept because a finished command simply leaves the list — there is no
    /// "done" to observe — and because what it was about has to be captured
    /// while it still can be: a deleted session's row is gone by the time its
    /// delete reports.
    tracked_commands: std::collections::HashMap<u64, TrackedCommand>,
    /// Where the chrome bands drew their buttons this frame.
    ///
    /// Kept apart from `click_targets` because a band is not a plugin: a click
    /// on one must not focus a pane, and there is no plugin index to record.
    /// Same reason the system modals keep their own click path.
    band_targets: Vec<thurbox::kernel::bands::Hit>,
    started: Instant,
    frames: u64,
    /// The last painted trees, per plugin index. A frame is skipped when every
    /// plugin returns what it returned last time and nothing else moved — the
    /// plugin-model equivalent of v1's `needs_redraw`.
    last_trees: Vec<Option<std::rc::Rc<thurbox::kernel::node::Node>>>,
    /// The last float each plugin painted, and where.
    ///
    /// What each chrome band painted last frame, and where. Bands have no tree
    /// to diff, so their cells are compared instead — see `render_band`.
    last_bands: std::collections::HashMap<Band, (Rect, u64)>,
    /// Kept apart from `last_trees` because a float is rendered in its own pass at
    /// its own rect, so the two would overwrite each other for a plugin that did
    /// both. Its purpose is the same: settle the loop when nothing moved.
    last_floats: std::collections::HashMap<usize, (Rect, std::rc::Rc<thurbox::kernel::node::Node>)>,
    /// Floats that actually painted on the last frame.
    ///
    /// Distinct from `last_floats`, which is a settle cache and deliberately
    /// KEEPS a closed float's last tree to compare against when it reopens. This
    /// is the live answer to "is it on screen", so it is rebuilt every frame —
    /// reading the cache instead is what reported a closed modal as visible.
    drawn_floats: std::collections::HashSet<usize>,
    last_paint: Instant,
    /// The slot rects the arrangement placed last frame — the signal that the
    /// screen owes a full repaint, because they moved.
    ///
    /// A pane opening or closing reflows every column beside it, and a cell the
    /// diff believes it already printed is a cell it will not print again. That
    /// is fine while ratatui's model of a cell's width matches the terminal's,
    /// and grapheme clusters exist where it cannot: a regional-indicator flag is
    /// two columns to `unicode-width` and a different number to several
    /// emulators, so glyphs from the pane that just closed survive in the column
    /// that replaced it. `normalize_ambiguous_width` removes the one such
    /// disagreement it can (see `kernel::paint`); this covers the rest by
    /// marking the reflowed frame `paint::force_full_repaint`, which prints
    /// every cell of it.
    ///
    /// Deliberately NOT `Terminal::clear`: erasing flushes a blank screen and
    /// leaves the repaint to the next flush, so every toggle blinks the whole
    /// interface. The frame is the same either way — only the empty one in
    /// between is avoided.
    last_placed: Vec<thurbox::kernel::layout::SlotRect>,
    /// Set by anything that invalidates the screen outside the tree diff:
    /// input, a reload, a resize, a completed command.
    dirty: bool,
    /// Set while drawing when any plugin's tree differed from last frame.
    changed_this_frame: bool,
    /// Output stamp each surface was last painted at, keyed by surface name.
    /// What makes a quiet terminal settle rather than repaint every frame.
    last_output_painted: std::collections::HashMap<String, u64>,
    /// Every live pane's last-output stamp, summed, as of the last check.
    ///
    /// Compared each iteration so that new agent output *causes* a frame. The
    /// per-surface map above only decides whether a frame that is already
    /// happening counts as a change — which is why, without this, a printing
    /// agent was drawn at the 250ms floor rather than at once.
    last_output_gen: u64,
    /// Plugin holding an exclusive key grab this frame, if any.
    grabbed: Option<usize>,
    /// Programs plugins asked to be run, and what they printed.
    runs: thurbox::kernel::runs::RunStore,
    /// Every file of the interface, as of the last painted frame.
    ///
    /// Computed for the plugins that used to list it and kept because the
    /// settings modal's Interface tab lists it too — one join per frame, read
    /// by both.
    inventory: Vec<thurbox::kernel::inventory::Row>,
    /// Sessions already asked to relaunch, so a respawn is attempted once per
    /// session per run rather than every frame its window is still missing.
    respawned: std::collections::HashSet<String>,
    /// Watches soft-deleted sessions' undo windows close, so their agents are
    /// let go rather than left running forever.
    reaper: thurbox::kernel::reaper::Reaper,
    /// Whether a bookmark command is still running.
    ///
    /// Repository memory is the one read the flow can *change*, so its cached
    /// rows have to be dropped when a write lands — and only then, since
    /// re-reading while the worker is mid-write would publish the old list and
    /// look like the add did nothing.
    bookmark_in_flight: bool,
    /// Links found on each live session's screen, keyed by session, and the
    /// output stamp each answer was found at.
    ///
    /// Rebuilt for a session only when that session printed something. Finding
    /// them walks the whole grid cell by cell, and this runs on every frame *and*
    /// every input event — so a held-down key used to rescan every terminal on
    /// the screen per repeat, for answers that cannot have changed.
    links: std::collections::HashMap<String, Vec<(String, usize, usize)>>,
    link_stamps: std::collections::HashMap<String, u64>,
    /// What each terminal was showing when a search last asked, and the output
    /// generation it was read at. Empty while nothing is searching.
    content: std::collections::HashMap<String, String>,
    content_generation: Option<u64>,
    /// Where each interface file stands with the user, and the lock the answer
    /// was resolved against.
    ///
    /// Answering it reads and digests every file in the interface directory and
    /// parses `plugins.lock`, which is the wrong price to pay per keystroke: the
    /// answer changes only when the directory or a grant does, and both say so.
    /// The rows themselves are still assembled every publish — those depend on
    /// what is on screen this frame, which is cheap and does change.
    trust: std::collections::HashMap<String, thurbox::kernel::inventory::Trust>,
    /// Set when the directory, a grant or the disabled set moved, so the trust
    /// answers above are re-read.
    trust_stale: bool,
    /// Whether the perf counters are painted over the interface (F12).
    hud: bool,
    quit: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three intervals only mean anything in relation to each other, and
    /// nothing enforces that at the definitions.
    ///
    /// Output must wait longer than input, or the split buys nothing; and both
    /// must stay under the forced-redraw floor, or the floor becomes the real
    /// cadence and the constant above it is silently dead — a setting that
    /// looks tuned while doing nothing (ADR-P17).
    #[test]
    fn the_frame_floors_stand_in_the_right_order() {
        assert!(
            OUTPUT_FRAME_INTERVAL > MIN_FRAME_INTERVAL,
            "output is paced no slower than input, so the split is a no-op"
        );
        assert!(
            OUTPUT_FRAME_INTERVAL < FORCE_REDRAW_INTERVAL,
            "output waits past the forced-redraw floor, which then sets the \
             cadence instead — the constant would be dead"
        );
        assert!(
            MIN_FRAME_INTERVAL < FORCE_REDRAW_INTERVAL,
            "input waits past the forced-redraw floor"
        );
    }
}
