mod coordinator;

use std::error::Error;

use clap::Parser as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;

use thurbox::kernel::bands::{Band, Level};
use thurbox::kernel::command::CommandBus;
use thurbox::kernel::diff::DiffStore;
use thurbox::kernel::host::{LuaHost, ModError};
use thurbox::kernel::metrics::Metrics;
use thurbox::kernel::modals::Modals;
use thurbox::kernel::node::Identity;
use thurbox::kernel::notify::Notifier;
use thurbox::kernel::perf::Counters;
use thurbox::kernel::registry::Registry;
use thurbox::kernel::selection::Selection;
use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::kernel::terminal::Terminals;
use thurbox::kernel::theme::Themes;
use thurbox::kernel::watch::Watcher;
use thurbox::ui_control::Server as UiControlServer;

const TICK: Duration = Duration::from_millis(10);

const IDLE_TICK: Duration = Duration::from_millis(50);

const QUIESCENT_AFTER: Duration = Duration::from_millis(500);
const DEBOUNCE: Duration = Duration::from_millis(120);
const FORCE_REDRAW_INTERVAL: Duration = Duration::from_millis(250);

const LINK_SCAN_INTERVAL: Duration = Duration::from_millis(250);

const PERF_WINDOW_TICKS: u64 = 1000;

const PERF_PUBLISH_INTERVAL: Duration = Duration::from_secs(5);

const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(16);

const FAILURE_HOLD: Duration = Duration::from_secs(5);

const OUTPUT_FRAME_INTERVAL: Duration = Duration::from_millis(33);

const ECHO_WINDOW: Duration = Duration::from_millis(150);

const ECHO_HOLD: Duration = MIN_FRAME_INTERVAL;

const KEEP_FRAME_WHILE_TYPING: Duration = Duration::from_secs(2);

const ECHO_POLL: Duration = Duration::from_millis(1);

const INPUT_FAILURE_LIMIT: u32 = 64;
const STATUS_TTL: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    Args::parse();
    require_terminal();
    coordinator::boot::run().await
}
#[derive(clap::Parser)]
#[command(
    about = "The interface takes no options: `--version`, `--help`, and a usage line for anything else. Scripts drive thurbox through `thurbox-cli`"
)]
#[command(
    name = "thurbox",
    version = env!("THURBOX_VERSION"),
    about = "The thurbox terminal interface: every session of every Fleet, in one screen.",
    after_help = "Run it in a terminal. To script thurbox or check this machine, use thurbox-cli \
                  (`thurbox-cli doctor`)."
)]
struct Args {}

fn require_terminal() {
    use std::io::IsTerminal;
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        eprintln!(
            "thurbox: needs a terminal on stdin and stdout. To script thurbox, use thurbox-cli."
        );
        std::process::exit(1);
    }
}

#[derive(Debug, Clone)]
struct ClickTarget {
    mod_index: usize,
    rect: Rect,
    identity: Identity,
}

#[derive(Debug, Clone)]
struct PointerGrab {
    mod_index: usize,
    rect: Rect,
    identity: Identity,
}

#[derive(Clone)]
struct TrackedCommand {
    kind: &'static str,
    session: String,
    label: Option<String>,
    failed: bool,
    name: Option<String>,
    created: Option<String>,
    force: bool,
}

struct App {
    control: Option<UiControlServer>,
    control_revision: u64,
    control_observed: Option<serde_json::Value>,
    control_events: std::collections::VecDeque<serde_json::Value>,
    control_tickets: std::collections::HashMap<String, PendingConfirmation>,
    control_state_version: u64,
    control_registry_version: u64,
    control_placed: Vec<thurbox::kernel::layout::SlotRect>,
    control_floats: std::collections::HashSet<usize>,
    control_focus: usize,
    control_modal: (
        Option<thurbox::kernel::modals::ModalKind>,
        Option<usize>,
        Option<String>,
    ),
    host: LuaHost,
    ui_dir: PathBuf,
    sources: std::collections::BTreeMap<String, thurbox::kernel::bundled::Source>,
    watcher: Watcher,
    snapshots: SnapshotStore,
    terminals: Terminals,
    commands: CommandBus,
    diffs: DiffStore,
    repos: thurbox::kernel::repos::RepoStore,
    metrics: Metrics,
    clipboard: Option<arboard::Clipboard>,
    image_probe: thurbox::clipboard::ImageProbe,
    paste_targets: Vec<String>,
    held_input: coordinator::input::HeldInput,
    probed_presses: usize,
    notifier: Notifier,
    perf: Counters,
    timings: thurbox::kernel::perf::Timings,
    startup: thurbox::kernel::perf::Startup,
    perf_log: bool,
    perf_window_base: thurbox::kernel::perf::Snapshot,
    perf_window_tick: u64,
    perf_published_at: Option<Instant>,
    first_frame_logged: bool,
    process_start: Instant,
    input_dirty: bool,
    echo: std::collections::VecDeque<coordinator::EchoWait>,
    echo_due: Option<String>,
    last_frame: Option<ratatui::buffer::Buffer>,
    last_keystroke: Option<Instant>,
    last_activity: Instant,
    animation_tick: u64,
    animation_step: u64,
    data_epoch: u64,
    selection: Option<Selection>,
    wheel_notch: coordinator::mouse::WheelNotch,
    click_train: coordinator::mouse::ClickTrain,
    themes: Themes,
    slot_selection: std::collections::HashMap<String, usize>,
    updates: thurbox::kernel::updates::Updates,
    config: thurbox::kernel::config::Config,
    registry: Registry,
    modals: Modals,
    visible_slots: std::collections::HashSet<String>,
    pending_focus: Option<usize>,
    editor_request: Option<String>,
    click_targets: Vec<ClickTarget>,
    last_area: Rect,
    screen_size: (u16, u16),
    selected_text: Option<String>,
    copy_after_paint: bool,
    hovered: Option<Identity>,
    mouse: bool,
    paste_burst: coordinator::paste::PasteBurst,
    holds: coordinator::input::Holds,
    focused_session: Option<String>,
    diffs_shown: bool,
    focused_surface: Option<String>,
    focused_typing: bool,
    focus: usize,
    focus_return: usize,
    reload_at: Option<Instant>,
    errors: Vec<ModError>,
    failing: std::collections::HashMap<usize, (ModError, Instant)>,
    layout_error: Option<String>,
    floor: Option<String>,
    floor_cause: Option<String>,
    memory_rebuilt: Option<Instant>,
    status: Option<(String, Level, Instant)>,
    startup_notices: std::collections::VecDeque<String>,
    startup_notice_due: Option<Instant>,
    reported_failures: std::collections::HashSet<u64>,
    tracked_commands: std::collections::HashMap<u64, TrackedCommand>,
    band_targets: Vec<thurbox::kernel::bands::Hit>,
    started: Instant,
    frames: u64,
    last_trees: Vec<Option<std::rc::Rc<thurbox::kernel::node::Node>>>,
    last_bands: std::collections::HashMap<Band, (Rect, u64)>,
    last_floats: std::collections::HashMap<usize, (Rect, std::rc::Rc<thurbox::kernel::node::Node>)>,
    drawn_floats: std::collections::HashSet<usize>,
    last_paint: Instant,
    last_placed: Vec<thurbox::kernel::layout::SlotRect>,
    dirty: bool,
    changed_this_frame: bool,
    last_output_painted: std::collections::HashMap<String, u64>,
    last_output_gen: u64,
    grabbed: Option<usize>,
    pointer_grab: Option<PointerGrab>,
    pty_pointer: Option<String>,
    runs: thurbox::kernel::runs::RunStore,
    inventory: Vec<thurbox::kernel::inventory::Row>,
    bookmark_in_flight: bool,
    report_after: Option<u64>,
    links: std::collections::HashMap<String, Vec<(String, usize, usize)>>,
    link_stamps: std::collections::HashMap<String, u64>,
    last_link_paints: Vec<thurbox::kernel::terminal::HyperlinkPaint>,
    link_scans: std::collections::HashMap<String, Instant>,
    search: thurbox::kernel::search::SearchStore,
    trust: std::collections::HashMap<String, thurbox::kernel::inventory::Trust>,
    trust_stale: bool,
    hud: bool,
    events: coordinator::events::Events,
    quit: bool,
}

struct PendingConfirmation {
    action: String,
    args: serde_json::Value,
    target: String,
    backend_id: Option<String>,
    cwd: Option<PathBuf>,
    member_dirs: Vec<PathBuf>,
    registry_version: u64,
    peer: u32,
    expires: std::time::Instant,
}

#[cfg(test)]
mod tests {
    use super::*;

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
