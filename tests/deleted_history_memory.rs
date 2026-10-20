//! A long deleted history must not fill the plugin VM.
//!
//! Every plugin shares one VM under [`host::memory_limit`], and the
//! `deleted` group is rebuilt whenever the database moves — every mirror
//! pass of a host with history. Each rebuild must be garbage the collector
//! can take back, or ~1600 mirrored tombstones filled the ceiling within a
//! few hundred publishes and took every pane down with it.

use thurbox::kernel::host::{self, Epoch, LuaHost, Published, RenderContext};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{DeletedRow, Snapshot};
use thurbox::kernel::theme::Themes;

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn snapshot(deleted: usize) -> Snapshot {
    Snapshot {
        deleted: (0..deleted)
            .map(|i| DeletedRow {
                id: format!("00000000-0000-4000-8000-{i:012}"),
                name: format!("worker-session-{i}"),
                agent: "claude".into(),
                deleted_at: 3_600_000,
                worktrees: 1,
                partial: false,
                restore_refusal: None,
            })
            .collect(),
        taken_at_ms: 7_200_000,
        ..Snapshot::default()
    }
}

fn publish(host: &LuaHost, snapshot: &Snapshot, registry: &Registry) {
    let themes = Themes::load(None);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch: Epoch::always_fresh(),
        snapshot,
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry,
        diffs: &diffs,
        links: &Default::default(),
        search: None,
        meta: &Default::default(),
        metrics: &Default::default(),
        status_rows: 0,
        can_open: true,
        inventory: &[],
        ui_dir: "ui",
        settings: &Default::default(),
        repos: &repos,
        wants: &Default::default(),
        focus: None,
        selection: None,
        hovered: None,
        printing: &Default::default(),
    })
    .expect("publish");
}

fn frames(host: &LuaHost, snapshot: &Snapshot, count: usize) -> usize {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let mut peak = 0;
    for frame in 0..count {
        publish(host, snapshot, &registry);
        // The panes that read `thurbox.deleted`.
        for name in ["sessions", "restore", "confirm"] {
            let Some(index) = host.index_of(name) else {
                continue;
            };
            host.render(
                index,
                RenderContext {
                    width: 40,
                    height: 40,
                    focused: false,
                    elapsed: frame as f64,
                    frame: frame as u64,
                },
            )
            .unwrap_or_else(|e| panic!("{name} at frame {frame}: {e}"));
        }
        peak = peak.max(host.used_memory());
    }
    peak
}

#[test]
fn rebuilding_a_long_deleted_history_stays_far_below_the_memory_limit() {
    let host = host();
    let peak = frames(&host, &snapshot(1600), 300);
    // Measured at ~6 MiB; the ceiling leaves room for the collector's pace
    // while staying an order of magnitude under the limit it used to reach.
    let ceiling = 32 * 1024 * 1024;
    assert!(
        peak < ceiling,
        "peak {peak} bytes over 300 publishes (limit {})",
        host::memory_limit()
    );
}
