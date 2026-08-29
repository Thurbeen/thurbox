//! What a frame actually costs, measured against the real interface.
//!
//! Not a gate — ADR-P5 keeps timing out of CI, and `tests/kernel_frame_cost.rs`
//! holds the assertions that *are* deterministic. This is the instrument those
//! assertions cannot be: it drives the bundled `ui/` with a synthetic snapshot
//! and reports where the time in one frame goes, so a change can be measured
//! rather than argued about.
//!
//! ```sh
//! cargo bench --bench frame_cost                 # the default sweep
//! THURBOX_BENCH_SESSIONS=1,20,100 cargo bench --bench frame_cost
//! ```
//!
//! Every phase is named after the thing the loop does, so a row here maps onto
//! a line in `docs/PERFORMANCE.md` rather than onto a function name that may
//! move.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use thurbox::kernel::host::{Epoch, LuaHost, Published, RenderContext};
use thurbox::kernel::inventory::Trust;
use thurbox::kernel::paint::PlaceholderSurfaces;
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;

static SCREEN: std::sync::OnceLock<(u16, u16)> = std::sync::OnceLock::new();
#[allow(non_snake_case)]
fn WIDTH() -> u16 {
    SCREEN.get_or_init(screen).0
}
#[allow(non_snake_case)]
fn HEIGHT() -> u16 {
    SCREEN.get_or_init(screen).1
}

/// The screen the frame is measured at. Overridable because "the session list
/// costs 435us" and "a visible row costs 9us" are different claims, and only a
/// height sweep tells them apart.
fn screen() -> (u16, u16) {
    let parse = |name: &str, fallback: u16| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    (
        parse("THURBOX_BENCH_WIDTH", 200),
        parse("THURBOX_BENCH_HEIGHT", 50),
    )
}

// --- the world --------------------------------------------------------------

fn ui_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui")
}

fn host() -> LuaHost {
    let host = LuaHost::new(ui_dir());
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

/// The `default` preset by name, never the environment's active choice — a
/// measurement that moves with whichever palette the machine happens to have
/// selected is not a measurement.
fn themes() -> Themes {
    let mut themes = Themes::load(None);
    themes.preview("default").expect("the default preset");
    themes
}

fn registry(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    registry
}

/// Rows shaped like the ones a working machine has: several repos, a parent
/// per group, live panes, and a git stat block — the shape the session list
/// spends its time on.
fn snapshot(sessions: usize) -> Snapshot {
    let repos = ["/src/thurbox", "/src/thurlab", "/src/thurspace"];
    let rows = (0..sessions)
        .map(|nth| {
            let repo = repos[nth % repos.len()];
            SessionRow {
                id: format!("session-{nth:04}"),
                name: format!("fix-the-thing-number-{nth}"),
                agent: "claude".into(),
                status: ["idle", "working", "blocked", "done"][nth % 4].into(),
                cwd: Some(format!("{repo}/worktrees/w{nth}").into()),
                repo: Some(repo.into()),
                repos: vec![repo.into()],
                branch: Some(format!("feat/thing-{nth}")),
                base_branch: Some("main".into()),
                backend: "local-tmux".into(),
                backend_id: Some(format!("%{nth}")),
                remote_host: None,
                agent_session_id: Some(format!("00000000-0000-0000-0000-{nth:012}")),
                // Every fourth row nests under the one before it, so the
                // session model's tree walk is exercised rather than skipped.
                parent_id: (nth % 4 == 3).then(|| format!("session-{:04}", nth - 1)),
                display_order: Some(nth as i64),
                worktree_count: 1,
                git: None,
                hook_state: Some("idle".into()),
                shell_backend_id: None,
                member_dirs: vec![format!("{repo}/worktrees/w{nth}").into()],
            }
        })
        .collect();
    Snapshot {
        sessions: rows,
        ..Snapshot::default()
    }
}

struct World {
    themes: Themes,
    registry: Registry,
    diffs: thurbox::kernel::diff::DiffStore,
    repos: thurbox::kernel::repos::RepoStore,
    inventory: Vec<thurbox::kernel::inventory::Row>,
}

impl World {
    fn new(host: &LuaHost) -> Self {
        let sources = thurbox::kernel::bundled::sources(&ui_dir());
        let visible: HashSet<usize> = (0..host.plugins.len()).collect();
        let placed: HashSet<String> = host.plugins.iter().map(|p| p.slot.clone()).collect();
        let inventory = thurbox::kernel::inventory::rows(
            &host.plugins,
            &sources,
            &visible,
            &placed,
            None,
            &|_| Trust::NotAsked,
            &|_| false,
        );
        World {
            themes: themes(),
            registry: registry(host),
            diffs: thurbox::kernel::diff::DiffStore::new(),
            repos: thurbox::kernel::repos::RepoStore::with_hosts(Default::default()),
            inventory,
        }
    }

    fn publish(&self, host: &LuaHost, epoch: Epoch, snapshot: &Snapshot) {
        host.publish(&Published {
            epoch,
            snapshot,
            attach_errors: &HashMap::new(),
            inflight: &[],
            themes: &self.themes,
            registry: &self.registry,
            diffs: &self.diffs,
            links: &Default::default(),
            content: &Default::default(),
            meta: &Default::default(),
            metrics: &Default::default(),
            status_rows: 0,
            can_open: true,
            inventory: &self.inventory,
            ui_dir: "ui",
            settings: &Default::default(),
            repos: &self.repos,
            wants: &Default::default(),
            focus: None,
            hovered: None,
        })
        .expect("publish");
    }
}

// --- timing -----------------------------------------------------------------

/// Median of `runs` timed passes, each of `inner` repetitions.
///
/// Median rather than mean: one scheduler hiccup in a hundred passes should not
/// move the number a reader compares against last week's.
fn measure(runs: usize, inner: usize, mut body: impl FnMut()) -> Duration {
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        for _ in 0..inner {
            body();
        }
        samples.push(started.elapsed() / inner as u32);
    }
    samples.sort_unstable();
    samples[samples.len() / 2]
}

struct Table {
    rows: Vec<(String, Vec<Duration>)>,
    columns: Vec<String>,
}

impl Table {
    fn print(&self, title: &str) {
        println!("\n{title}");
        let width = self
            .rows
            .iter()
            .map(|(name, _)| name.len())
            .chain(std::iter::once(6))
            .max()
            .unwrap_or(6);
        print!("{:<width$}", "phase", width = width);
        for column in &self.columns {
            print!(" {column:>12}");
        }
        println!();
        println!("{}", "-".repeat(width + 13 * self.columns.len()));
        for (name, values) in &self.rows {
            print!("{name:<width$}", width = width);
            for value in values {
                print!(" {:>10.1}us", value.as_secs_f64() * 1e6);
            }
            println!();
        }
    }
}

// --- a frame, as the loop actually draws one ---------------------------------

/// The panes an arrangement of this size actually places, at their real rects.
///
/// This is the difference between a measurement and a fiction: a closed search
/// strip occupies no slot, so `draw_slots` never reaches it and it costs a real
/// frame nothing. Rendering every plugin regardless reported it as the second
/// most expensive pane in the interface.
fn placed_panes(host: &LuaHost) -> Vec<(usize, String, ratatui::layout::Rect)> {
    let area = ratatui::layout::Rect::new(0, 0, WIDTH(), HEIGHT());
    let region = host.arrangement(WIDTH(), HEIGHT()).expect("arrangement");
    let mut out = Vec::new();
    for slot in thurbox::kernel::layout::resolve(&region, area) {
        for &index in host.in_slot(&slot.slot) {
            let Some(plugin) = host.plugins.get(index) else {
                continue;
            };
            out.push((index, plugin.name.clone(), slot.rect));
            // A switch slot shows one occupant; a stack shows them all. The
            // bundled interface has one occupant per slot either way, so the
            // distinction costs nothing to ignore here — but say so, because an
            // interface with two panes in `center` would need it.
            if host.slot_mode(&slot.slot) == thurbox::kernel::layout::SlotMode::Switch {
                break;
            }
        }
    }
    out
}

/// The float probe every frame pays: `draw_floats` renders each floating plugin
/// to discover whether it is floating *this* frame. Three of the bundled panes
/// are floats, and all three declare `pure`, so this is a cache hit — which is
/// exactly the claim worth measuring.
fn float_probe(host: &LuaHost, frame: u64) {
    for &index in host.floating() {
        let _ = host.render(
            index,
            RenderContext {
                width: WIDTH(),
                height: HEIGHT(),
                focused: true,
                elapsed: 0.0,
                frame,
            },
        );
    }
}

fn main() {
    let counts: Vec<usize> = std::env::var("THURBOX_BENCH_SESSIONS")
        .ok()
        .map(|spec| {
            spec.split(',')
                .filter_map(|n| n.trim().parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![1, 10, 50]);

    let host = host();
    let world = World::new(&host);
    let panes = placed_panes(&host);

    println!(
        "thurbox frame cost — {}x{}, sessions: {counts:?}",
        WIDTH(),
        HEIGHT()
    );
    println!(
        "placed: {}   floats probed: {}   (of {} loaded)",
        panes
            .iter()
            .map(|(_, name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        host.floating().len(),
        host.plugins.len()
    );

    let columns: Vec<String> = counts.iter().map(|n| format!("{n} sessions")).collect();
    let mut rows: Vec<(String, Vec<Duration>)> = Vec::new();

    // --- whole frames -------------------------------------------------------

    // The frame the loop spends most of its life on: the 250ms forced redraw
    // with nothing moved. Every group reused, every pure pane cached. This is
    // the number that must stay flat in the session count.
    rows.push((
        "frame: settled".into(),
        counts
            .iter()
            .map(|&n| {
                let snapshot = snapshot(n);
                let epoch = Epoch::default();
                let mut terminal =
                    Terminal::new(TestBackend::new(WIDTH(), HEIGHT())).expect("term");
                let mut once = || {
                    world.publish(&host, epoch, &snapshot);
                    let trees = render_panes(&host, &panes, 0);
                    float_probe(&host, 0);
                    paint(&mut terminal, &trees);
                };
                once();
                measure(9, 20, once)
            })
            .collect(),
    ));

    // A frame after the snapshot moved: the whole publish, every pane run
    // again, and the paint. What a session appearing, a status changing or a
    // branch landing costs.
    rows.push((
        "frame: snapshot moved".into(),
        counts
            .iter()
            .map(|&n| {
                let snapshot = snapshot(n);
                let mut epoch = Epoch::default();
                let mut terminal =
                    Terminal::new(TestBackend::new(WIDTH(), HEIGHT())).expect("term");
                measure(9, 20, || {
                    epoch.snapshot += 1;
                    world.publish(&host, epoch, &snapshot);
                    let trees = render_panes(&host, &panes, 0);
                    float_probe(&host, 0);
                    paint(&mut terminal, &trees);
                })
            })
            .collect(),
    ));

    // The animation clock ticking, which is what a `working` session does eight
    // times a second: nothing else moved, but every pure pane is invalidated.
    rows.push((
        "frame: animation tick".into(),
        counts
            .iter()
            .map(|&n| {
                let snapshot = snapshot(n);
                let mut epoch = Epoch::default();
                let mut terminal =
                    Terminal::new(TestBackend::new(WIDTH(), HEIGHT())).expect("term");
                measure(9, 20, || {
                    epoch.animation += 1;
                    world.publish(&host, epoch, &snapshot);
                    let trees = render_panes(&host, &panes, 0);
                    float_probe(&host, 0);
                    paint(&mut terminal, &trees);
                })
            })
            .collect(),
    ));

    // --- the parts ----------------------------------------------------------

    rows.push((
        "  publish (reused)".into(),
        counts
            .iter()
            .map(|&n| {
                let snapshot = snapshot(n);
                let epoch = Epoch::default();
                world.publish(&host, epoch, &snapshot);
                measure(9, 50, || world.publish(&host, epoch, &snapshot))
            })
            .collect(),
    ));

    rows.push((
        "  publish (rebuilt)".into(),
        counts
            .iter()
            .map(|&n| {
                let snapshot = snapshot(n);
                measure(9, 20, || {
                    world.publish(&host, Epoch::always_fresh(), &snapshot);
                })
            })
            .collect(),
    ));

    rows.push((
        "  arrangement".into(),
        counts
            .iter()
            .map(|_| {
                measure(9, 200, || {
                    host.arrangement(WIDTH(), HEIGHT()).expect("arrangement");
                })
            })
            .collect(),
    ));

    rows.push((
        "  render placed panes".into(),
        counts
            .iter()
            .map(|&n| {
                let snapshot = snapshot(n);
                let mut epoch = Epoch::default();
                measure(9, 20, || {
                    // A moved epoch, so this is the cost of actually running
                    // them rather than of the cache answering.
                    epoch.snapshot += 1;
                    world.publish(&host, epoch, &snapshot);
                    let _ = render_panes(&host, &panes, 0);
                })
            })
            .collect(),
    ));

    rows.push((
        "  float probe (cached)".into(),
        counts
            .iter()
            .map(|&n| {
                let snapshot = snapshot(n);
                world.publish(&host, Epoch::default(), &snapshot);
                float_probe(&host, 0);
                measure(9, 200, || float_probe(&host, 0))
            })
            .collect(),
    ));

    rows.push((
        "  paint".into(),
        counts
            .iter()
            .map(|&n| {
                let snapshot = snapshot(n);
                world.publish(&host, Epoch::always_fresh(), &snapshot);
                let trees = render_panes(&host, &panes, 0);
                let mut terminal =
                    Terminal::new(TestBackend::new(WIDTH(), HEIGHT())).expect("term");
                measure(9, 20, || paint(&mut terminal, &trees))
            })
            .collect(),
    ));

    rows.push((
        "  normalize width".into(),
        counts
            .iter()
            .map(|_| {
                let mut buffer = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(
                    0,
                    0,
                    WIDTH(),
                    HEIGHT(),
                ));
                measure(9, 200, || {
                    thurbox::kernel::paint::normalize_ambiguous_width(&mut buffer);
                })
            })
            .collect(),
    ));

    let sources = thurbox::kernel::bundled::sources(&ui_dir());
    let visible: HashSet<usize> = (0..host.plugins.len()).collect();
    let placed: HashSet<String> = host.plugins.iter().map(|p| p.slot.clone()).collect();
    rows.push((
        "  inventory rows".into(),
        counts
            .iter()
            .map(|_| {
                measure(9, 200, || {
                    let _ = thurbox::kernel::inventory::rows(
                        &host.plugins,
                        &sources,
                        &visible,
                        &placed,
                        None,
                        &|_| Trust::NotAsked,
                        &|_| false,
                    );
                })
            })
            .collect(),
    ));

    Table { rows, columns }.print("per frame");

    output_frame();

    per_pane(&host, &world, &panes, &counts);
    caching(&host, &world, &panes, counts.last().copied().unwrap_or(50));
}

fn render_panes(
    host: &LuaHost,
    panes: &[(usize, String, ratatui::layout::Rect)],
    frame: u64,
) -> Vec<(
    std::rc::Rc<thurbox::kernel::node::Node>,
    ratatui::layout::Rect,
)> {
    panes
        .iter()
        .filter_map(|(index, _, rect)| {
            host.render(
                *index,
                RenderContext {
                    width: rect.width,
                    height: rect.height,
                    focused: false,
                    elapsed: 0.0,
                    frame,
                },
            )
            .ok()
            .map(|rendered| (rendered.node, *rect))
        })
        .collect()
}

fn paint(
    terminal: &mut Terminal<TestBackend>,
    trees: &[(
        std::rc::Rc<thurbox::kernel::node::Node>,
        ratatui::layout::Rect,
    )],
) {
    terminal
        .draw(|frame| {
            for (tree, rect) in trees {
                thurbox::kernel::paint::render(frame, *rect, tree, &PlaceholderSurfaces);
            }
            thurbox::kernel::paint::normalize_ambiguous_width(frame.buffer_mut());
        })
        .expect("draw");
}

/// Which pane the time is in, at the largest session count — the question
/// "the frame got slower" always turns into.
///
/// Driven by the ANIMATION epoch, not the snapshot: that invalidates every pure
/// pane while leaving every published group reusable, so the publish beside each
/// render is the ~45us cache-hit one rather than the ~1ms rebuild. Subtracting a
/// millisecond from a millisecond measures the subtraction, not the pane.
fn per_pane(
    host: &LuaHost,
    world: &World,
    panes: &[(usize, String, ratatui::layout::Rect)],
    counts: &[usize],
) {
    let mut rows: Vec<(String, Vec<Duration>)> = panes
        .iter()
        .map(|(_, name, rect)| {
            (
                format!("{name} ({}x{})", rect.width, rect.height),
                Vec::new(),
            )
        })
        .collect();
    let mut baselines = Vec::new();
    for &sessions in counts {
        let snapshot = snapshot(sessions);
        let mut epoch = Epoch::default();
        let baseline = measure(9, 50, || {
            epoch.animation += 1;
            world.publish(host, epoch, &snapshot);
        });
        for (nth, (index, _, rect)) in panes.iter().enumerate() {
            let each = measure(9, 50, || {
                epoch.animation += 1;
                world.publish(host, epoch, &snapshot);
                let _ = host.render(
                    *index,
                    RenderContext {
                        width: rect.width,
                        height: rect.height,
                        focused: false,
                        elapsed: 0.0,
                        frame: 0,
                    },
                );
            });
            rows[nth].1.push(each.saturating_sub(baseline));
        }
        baselines.push(baseline);
    }
    rows.push(("(reused publish, subtracted)".into(), baselines));
    Table {
        rows,
        columns: counts.iter().map(|n| format!("{n} sessions")).collect(),
    }
    .print("per placed pane, on an animation tick");
}

/// What the caches actually did — the counters the perf HUD reads.
///
/// A time that looks fine because a cache answered is a different fact from a
/// time that is fine, and only these tell them apart.
fn caching(
    host: &LuaHost,
    world: &World,
    panes: &[(usize, String, ratatui::layout::Rect)],
    sessions: usize,
) {
    let snapshot = snapshot(sessions);
    let epoch = Epoch::default();
    world.publish(host, epoch, &snapshot);
    let _ = render_panes(host, panes, 0);
    float_probe(host, 0);

    let skipped = host.skipped_renders();
    let reused = host.reused_groups();
    let frames = 100;
    for _ in 0..frames {
        world.publish(host, epoch, &snapshot);
        let _ = render_panes(host, panes, 0);
        float_probe(host, 0);
    }
    let renders = panes.len() + host.floating().len();
    println!("\nover {frames} settled frames");
    println!(
        "  pane renders served from the cache: {} of {}",
        host.skipped_renders() - skipped,
        frames * renders
    );
    println!(
        "  published groups reused:            {}",
        host.reused_groups() - reused
    );
}

// --- the streaming-agent frame ----------------------------------------------

/// A screen with the shape an agent's output has: mostly text, a few URLs, some
/// styling. Filled to the full grid, because both costs below are per cell.
fn agent_screen(rows: u16, cols: u16) -> vt100::Parser {
    let mut parser = vt100::Parser::new(rows, cols, 0);
    for nth in 0..rows {
        let line = if nth % 12 == 0 {
            format!("\x1b[33m  see https://github.com/Thurbeen/thurbox/pull/{nth} for the rest\x1b[0m\r\n")
        } else if nth % 3 == 0 {
            format!("\x1b[1m  * step {nth}\x1b[0m: rewrote src/kernel/host/publish.rs and re-ran the suite\r\n")
        } else {
            format!("    {nth:>4} | the quick brown fox jumps over the lazy dog, repeatedly and at length\r\n")
        };
        parser.process(line.as_bytes());
    }
    parser
}

/// What a frame costs *because an agent is printing* — the two per-frame costs
/// a `PlaceholderSurfaces` harness cannot see.
///
/// Both are paid once per painted frame per visible printing surface, at the
/// 33ms output floor, so the per-second figure beside each is the one that
/// matters. The link scan is keyed on the surface's output stamp, which a
/// printing agent moves every frame — so its cache never hits while output is
/// arriving, which is exactly when it runs.
fn output_frame() {
    let (width, height) = (WIDTH() * 3 / 4, HEIGHT() - 2);
    let parser = agent_screen(height, width);
    let mut terminal = Terminal::new(TestBackend::new(WIDTH(), HEIGHT())).expect("term");

    let extract = measure(9, 100, || {
        let _ = thurbox::kernel::terminal::links::extract_screen_rows(parser.screen());
    });
    let rows = thurbox::kernel::terminal::links::extract_screen_rows(parser.screen());
    let detect = measure(9, 100, || {
        let _ = thurbox::kernel::terminal::links::detect_urls(&rows);
    });
    let surface = measure(9, 100, || {
        terminal
            .draw(|frame| {
                let area = ratatui::layout::Rect::new(0, 0, width, height);
                frame.render_widget(tui_term::widget::PseudoTerminal::new(parser.screen()), area);
            })
            .expect("draw");
    });

    let per_second = |d: Duration| d.as_secs_f64() * 1e6 * 30.0 / 1000.0;
    println!("\nwhile one agent prints — {width}x{height} grid, at the 33ms output floor");
    println!("phase                          per frame     per second");
    println!("-------------------------------------------------------");
    for (name, each) in [
        ("extract screen rows", extract),
        ("detect urls", detect),
        ("paint the vt100 surface", surface),
        ("(link scan = extract+detect)", extract + detect),
    ] {
        println!(
            "{name:<30} {:>7.1}us {:>10.2}ms",
            each.as_secs_f64() * 1e6,
            per_second(each)
        );
    }
}
