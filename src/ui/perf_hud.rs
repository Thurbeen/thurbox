//! Perf HUD overlay: a small right-anchored box with the live perf counters,
//! frame/tick timing percentiles, and the most recent slow ops. Toggled by
//! `Action::TogglePerfHud` (F12); opening it also switches on wall-clock
//! timing collection (`App::perf_timing_active`). Unlike a modal it never dims
//! or captures input — it floats above the normal panes so the app stays fully
//! drivable while the numbers update (~4 fps via the forced-redraw floor).

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::metrics_state::MetricsState;

use super::theme::Theme;

/// Borrowed snapshot of everything the HUD shows.
pub(crate) struct PerfHudView<'a> {
    pub(crate) metrics: &'a MetricsState,
    pub(crate) session_count: usize,
}

/// Preferred HUD width/height; clamped to the frame on small terminals.
const HUD_WIDTH: u16 = 46;
const HUD_HEIGHT: u16 = 18;

/// Format a µs value compactly: `420µs`, `3.2ms`, `1.8s`.
fn fmt_us(us: u64) -> String {
    if us < 1_000 {
        format!("{us}µs")
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.1}s", us as f64 / 1_000_000.0)
    }
}

fn row<'a>(label: &'a str, value: String) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{label:<14}"),
            Style::default().fg(Theme::text_muted()),
        ),
        Span::styled(value, Style::default().fg(Theme::text_primary())),
    ])
}

/// Render the HUD into the top-right corner of `area` (the full frame).
pub(crate) fn render_perf_hud(frame: &mut Frame, area: Rect, view: &PerfHudView<'_>) {
    let w = HUD_WIDTH.min(area.width);
    let h = HUD_HEIGHT.min(area.height.saturating_sub(1));
    if w < 20 || h < 6 {
        return;
    }
    // Top-right, below the header row, so it covers neither the session list
    // nor the footer hints.
    let hud = Rect::new(area.x + area.width - w, area.y + 1, w, h);
    frame.render_widget(Clear, hud);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Theme::border_unfocused()))
        .title(Span::styled(" Perf ", Style::default().fg(Theme::accent())))
        .style(Style::default().bg(Theme::modal_bg()));
    let inner = block.inner(hud);
    frame.render_widget(block, hud);

    let p = view.metrics.perf;
    let t = &view.metrics.timings;
    // Idle efficiency: the share of loop iterations that skipped the paint.
    let attempts = p.redraws_requested + p.redraws_skipped;
    let skip_pct = if attempts == 0 {
        0.0
    } else {
        p.redraws_skipped as f64 * 100.0 / attempts as f64
    };

    let mut lines: Vec<Line> = vec![
        row("sessions", view.session_count.to_string()),
        row("ticks", view.metrics.tick_count.to_string()),
        row("frames", p.frames_rendered.to_string()),
        row(
            "idle skips",
            format!("{} ({skip_pct:.0}%)", p.redraws_skipped),
        ),
        row("order builds", p.ordered_sessions_rebuilds.to_string()),
        row("hook loads", p.hook_state_loads.to_string()),
        row(
            "ext polls",
            format!(
                "{} ({} reloads)",
                p.external_poll_checks, p.external_poll_reloads
            ),
        ),
        row(
            "frame p50/p95",
            format!(
                "{} / {} (max {})",
                fmt_us(t.frame.percentile_us(50)),
                fmt_us(t.frame.percentile_us(95)),
                fmt_us(t.frame.max_us()),
            ),
        ),
        row(
            "tick p50/p95",
            format!(
                "{} / {} (max {})",
                fmt_us(t.tick.percentile_us(50)),
                fmt_us(t.tick.percentile_us(95)),
                fmt_us(t.tick.max_us()),
            ),
        ),
    ];

    lines.push(Line::from(Span::styled(
        "slow ops",
        Style::default().fg(Theme::text_muted()),
    )));
    let mut any = false;
    for op in t.slow_ops.iter_recent().take(4) {
        any = true;
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<18}", op.name),
                Style::default().fg(Theme::text_secondary()),
            ),
            Span::styled(
                format!("{}ms", op.ms),
                Style::default().fg(Theme::status_working()),
            ),
        ]));
    }
    if !any {
        lines.push(Line::from(Span::styled(
            "  none ≥5ms",
            Style::default().fg(Theme::text_muted()),
        )));
    }

    // Discoverability: the log-based counterpart of this overlay.
    lines.push(Line::from(Span::styled(
        "THURBOX_PERF_LOG=1 logs windows",
        Style::default().fg(Theme::keybind_hint()),
    )));

    frame.render_widget(Paragraph::new(lines), inner);
}
