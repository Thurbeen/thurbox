use std::time::Duration;
use thurbox::kernel::host::KeyReleases;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::{DefaultTerminal, Frame};

use thurbox::kernel::host::KeyPress;

pub(crate) fn browser_available() -> bool {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return true;
    }
    ["BROWSER", "DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
}

pub(crate) fn open_url(url: &str) -> Result<(), String> {
    let (program, args) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        // The empty string is `start`'s window-title argument; without it
        // `start` swallows the URL as the title and opens nothing.
        ("cmd", vec!["/C", "start", "", url])
    } else {
        if !browser_available() {
            return Err("No display to open a browser on".to_string());
        }
        ("xdg-open", vec![url])
    };

    std::process::Command::new(program)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("No URL opener ({program} not installed)"),
            _ => format!("Could not run {program}: {e}"),
        })
}

pub(crate) fn key_event_from_chord(chord: &str) -> Option<KeyEvent> {
    let normalised = thurbox::kernel::registry::normalise_chord(chord);
    let mut modifiers = KeyModifiers::NONE;
    let mut name = "";
    for part in normalised.split('+') {
        match part {
            "ctrl" => modifiers |= KeyModifiers::CONTROL,
            "alt" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "cmd" => modifiers |= KeyModifiers::SUPER,
            other => name = other,
        }
    }

    let code = match name {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        other => match other.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            Some(n) if (1..=12).contains(&n) => KeyCode::F(n),
            // A bare character, which is most of them. `chars().count()`
            // rather than `len()`, so a non-ASCII key is not read as several.
            _ => {
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => KeyCode::Char(c),
                    _ => return None,
                }
            }
        },
    };
    Some(KeyEvent::new(code, modifiers))
}

pub(crate) fn editor_command(stored: Option<String>) -> Option<String> {
    stored
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|command| !command.trim().is_empty())
}

pub(crate) fn open_editor(
    terminal: &mut DefaultTerminal,
    dirs: &[std::path::PathBuf],
    command: Option<String>,
    mode: thurbox::session::settings::EditorMode,
) -> Result<String, String> {
    let first = dirs
        .first()
        .ok_or("that session has no directory to open")?;
    let configured = editor_command(command)
        .ok_or("no editor configured — set one with `thurbox-cli editor set <command>`")?;
    let (program, mut args) = super::editor::parse_editor_command(&configured)
        .map_err(|e| format!("the configured editor command is unusable: {e}"))?;
    let terminal_editor = super::editor::is_terminal_editor(&program, &args, mode);
    args.extend(dirs.iter().map(|dir| dir.display().to_string()));

    let opened = if dirs.len() == 1 {
        format!("opened {}", first.display())
    } else {
        format!("opened {} directories", dirs.len())
    };

    if !terminal_editor {
        // Detached: no tty, no wait, and deliberately no report of how it went —
        // a GUI editor's exit status arrives long after anyone is looking.
        return match std::process::Command::new(&program).args(&args).spawn() {
            Ok(_) => Ok(format!("{opened} in {program}")),
            Err(e) => Err(format!("could not run {program}: {e}")),
        };
    }

    if std::env::var_os("TMUX").is_some() {
        // Quoted and run through tmux's shell, so a path or flag with a space
        // in it survives being flattened into one command string.
        let mut script = thurbox::shell::posix_quote(&program);
        for arg in &args {
            script.push(' ');
            script.push_str(&thurbox::shell::posix_quote(arg));
        }
        // `-E` closes the popup when the editor exits; the editor's own exit
        // code is ignored, since a non-zero edit must not trigger a retry.
        // A tmux that would not run at all falls through to the suspend path
        // rather than leaving the key doing nothing.
        let launched = std::process::Command::new("tmux")
            .args([
                "display-popup",
                "-E",
                "-w",
                "90%",
                "-h",
                "90%",
                "-T",
                "thurbox editor",
            ])
            .arg(&script)
            .status();
        if launched.is_ok() {
            return Ok(format!("{opened} in {program}"));
        }
    }

    // Stand the interface down so the editor inherits a normal cooked
    // terminal, then put everything back and force a full repaint — the
    // editor overwrote the cells ratatui thinks are on screen.
    //
    // The kitty flags come off first and go back on last, mirroring boot: they
    // were pushed on the alternate screen, and a terminal keeps a flag stack
    // per screen, so a pop written after leaving it would miss them and an
    // editor opening the alternate screen would be sent an escape sequence for
    // every key it is let go of.
    let popped = pop_keyboard_enhancement();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    );
    let _ = crossterm::terminal::disable_raw_mode();
    let status = std::process::Command::new(&program).args(&args).status();
    let _ = crossterm::terminal::enable_raw_mode();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen);
    if popped {
        repush_keyboard_enhancement();
    }
    // Mirrors the disable above; harmless when the feature is off, since the
    // loop drops mouse events either way.
    enable_mouse_clicks();
    let _ = terminal.clear();

    match status {
        Ok(_) => Ok(format!("closed {program}")),
        Err(e) => Err(format!("could not run {program}: {e}")),
    }
}

pub(crate) fn restore_terminal() {
    // Reverse order of setup, so the kitty flags come off while raw mode is still
    // on -- popping them after `ratatui::restore()` would write the escape to a
    // cooked terminal.
    pop_keyboard_enhancement();
    // Unconditional: cheaper than tracking whether capture was on, and a
    // terminal left reporting is the failure this exists to prevent. The
    // cursor is shown here too, not left to `Terminal`'s drop: a signal exits
    // from the runtime's thread and drops nothing, and a shell with no cursor
    // is the leftover that path had.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        crossterm::cursor::Show
    );
    // The console's own mouse handling is a mode, not an escape, so the line
    // above does not give it back. Idempotent, and a no-op if we never took it.
    windows_console::release_mouse();
    ratatui::restore();
}

const KEYBOARD_FLAGS: crossterm::event::KeyboardEnhancementFlags =
    crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        .union(crossterm::event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES);

static KEYBOARD_ENHANCEMENT_PUSHED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) fn push_keyboard_enhancement() -> KeyReleases {
    use crossterm::event::PushKeyboardEnhancementFlags;
    if matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    ) && crossterm::execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KEYBOARD_FLAGS)
    )
    .is_ok()
    {
        KEYBOARD_ENHANCEMENT_PUSHED.store(true, std::sync::atomic::Ordering::SeqCst);
        return KeyReleases::Negotiated;
    }
    if cfg!(windows) {
        KeyReleases::Negotiated
    } else {
        KeyReleases::Unsupported
    }
}

pub(crate) fn pop_keyboard_enhancement() -> bool {
    let pushed = KEYBOARD_ENHANCEMENT_PUSHED.swap(false, std::sync::atomic::Ordering::SeqCst);
    if pushed {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags
        );
    }
    pushed
}

fn repush_keyboard_enhancement() {
    if crossterm::execute!(
        std::io::stdout(),
        crossterm::event::PushKeyboardEnhancementFlags(KEYBOARD_FLAGS)
    )
    .is_ok()
    {
        KEYBOARD_ENHANCEMENT_PUSHED.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(crate) fn enable_mouse_clicks() -> bool {
    use std::io::Write;
    // The escape asks the TERMINAL to report. On Windows something else has to
    // be asked as well, or the console keeps every drag for itself.
    windows_console::capture_mouse();
    let mut out = std::io::stdout();
    out.write_all(b"\x1b[?1000h\x1b[?1003h\x1b[?1006h").is_ok() && out.flush().is_ok()
}

#[cfg(windows)]
mod windows_console {
    use crossterm_winapi::{ConsoleMode, Handle};
    use std::sync::atomic::{AtomicU32, Ordering};

    const ENABLE_MOUSE_INPUT: u32 = 0x0010;
    const ENABLE_QUICK_EDIT_MODE: u32 = 0x0040;
    const ENABLE_EXTENDED_FLAGS: u32 = 0x0080;
    const NOT_SAVED: u32 = u32::MAX;

    static SAVED_MODE: AtomicU32 = AtomicU32::new(NOT_SAVED);

    pub(super) fn capture_mouse() {
        let Ok(handle) = Handle::current_in_handle() else {
            return;
        };
        let console = ConsoleMode::from(handle);
        let Ok(current) = console.mode() else {
            return;
        };
        let wanted =
            (current | ENABLE_MOUSE_INPUT | ENABLE_EXTENDED_FLAGS) & !ENABLE_QUICK_EDIT_MODE;
        match console.set_mode(wanted) {
            // Saved only once it is really ours, so a failed take restores
            // nothing on the way out.
            Ok(()) => SAVED_MODE.store(current, Ordering::SeqCst),
            Err(e) => tracing::warn!("could not take the console's mouse input: {e}"),
        }
    }

    pub(super) fn release_mouse() {
        let saved = SAVED_MODE.swap(NOT_SAVED, Ordering::SeqCst);
        if saved == NOT_SAVED {
            return;
        }
        if let Ok(handle) = Handle::current_in_handle() {
            let _ = ConsoleMode::from(handle).set_mode(saved);
        }
    }
}

#[cfg(not(windows))]
mod windows_console {
    pub(super) fn capture_mouse() {}
    pub(super) fn release_mouse() {}
}

pub(crate) fn next_event(timeout: Duration) -> std::io::Result<Option<Event>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    event::read().map(Some)
}

pub(crate) fn clamp_span(value: u16, floor: u16, cap: u16) -> u16 {
    value.clamp(floor.min(cap), cap)
}

pub(crate) fn error_area(area: Rect) -> Rect {
    let cap = (area.height / 2).max(3).min(area.height);
    let height = clamp_span(area.height.saturating_sub(2), 3, cap);
    Rect {
        x: area.x,
        y: area.y.saturating_add(area.height.saturating_sub(height)),
        width: area.width,
        height: height.min(area.height),
    }
}

pub(crate) fn hud_area(area: Rect) -> Rect {
    let width = 34.min(area.width);
    let height = 15.min(area.height);
    Rect {
        x: area.x + area.width - width,
        y: area.y,
        width,
        height,
    }
}

pub(crate) fn read_cells(frame: &mut Frame, rect: Rect) -> u64 {
    use std::hash::{Hash, Hasher};
    // `Frame` exposes only `buffer_mut`, hence the mutable borrow for a read.
    // Taken once rather than per cell: this runs for every band on every
    // painted frame.
    let buffer = frame.buffer_mut();
    let rect = rect.intersection(buffer.area);
    let mut hasher = std::hash::DefaultHasher::new();
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            if let Some(cell) = buffer.cell(ratatui::layout::Position::new(x, y)) {
                cell.symbol().hash(&mut hasher);
                cell.fg.hash(&mut hasher);
                cell.bg.hash(&mut hasher);
                cell.modifier.hash(&mut hasher);
                cell.underline_color.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

pub(crate) fn fmt_hud_us(us: u64) -> String {
    if us < 1_000 {
        format!("{us}us")
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.1}s", us as f64 / 1_000_000.0)
    }
}

pub(crate) fn render_hud(
    frame: &mut Frame,
    area: Rect,
    counters: &thurbox::kernel::perf::Snapshot,
    timings: &thurbox::kernel::perf::Timings,
) {
    use ratatui::style::{Color, Style};
    use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

    if area.width == 0 || area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" perf ");
    let inner = block.inner(area);
    // Counters first — they are the exact half. The timings below them are
    // wall-clock and so only ever indicative (ADR-P11).
    let mut text = format!(
        "iterations {}\nframes     {}\nskipped    {}\nrenders    {}\nreused r/g {}/{}\nechoes e/f {}/{}\nfailures   {}\nreloads    {}\n",
        counters.iterations,
        counters.frames,
        counters.skipped,
        counters.renders,
        counters.renders_skipped,
        counters.groups_reused,
        counters.echoes,
        counters.echo_frames,
        counters.failures,
        counters.reloads,
    );
    for (label, histogram) in [
        ("frame", &timings.frame),
        ("republ", &timings.republish),
        ("tick", &timings.tick),
    ] {
        text.push_str(&format!(
            "{label:<6} {} / {}\n",
            fmt_hud_us(histogram.percentile_us(50)),
            fmt_hud_us(histogram.max_us()),
        ));
    }
    // The pane when there is one: it is what the reader goes to fix, and the
    // op name alone does not fit beside it.
    match timings.slow_ops.iter_recent().next() {
        Some(op) => match &op.owner {
            Some(owner) => text.push_str(&format!("slow   {}ms {owner}", op.ms)),
            None => text.push_str(&format!("slow   {}:{}ms", op.name, op.ms)),
        },
        None => text.push_str("slow   none"),
    }
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::Yellow)),
        inner,
    );
}

const MOD_HUD_ROWS: usize = 8;

const MOD_HUD_WIDTH: u16 = 52;

const MOD_HUD_MIN_HEIGHT: u16 = 4;

pub(crate) fn mod_hud_area(area: Rect, hud: Rect, rows: usize) -> Rect {
    // Two borders, the header, the rows and one hint line.
    let wanted = (rows.min(MOD_HUD_ROWS) as u16).saturating_add(4);
    let below = area.bottom().saturating_sub(hud.bottom());
    if below >= wanted.min(MOD_HUD_MIN_HEIGHT) {
        let width = MOD_HUD_WIDTH.min(area.width);
        return Rect {
            x: area.right() - width,
            y: hud.bottom(),
            width,
            height: wanted.min(below),
        };
    }
    let width = MOD_HUD_WIDTH.min(hud.x.saturating_sub(area.x));
    Rect {
        x: hud.x - width,
        y: area.y,
        width,
        height: wanted.min(area.height),
    }
}

pub(crate) fn render_mod_hud(
    frame: &mut Frame,
    area: Rect,
    report: &thurbox::kernel::perf::ModReport,
) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::Line;
    use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

    if area.width < 3 || area.height < 3 {
        return;
    }
    let yellow = Style::default().fg(Color::Yellow);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(yellow)
        .title(" panes ");
    let inner = block.inner(area);
    let mut lines = vec![Line::styled(
        format!(
            "  {:<13} {:>7} {:>6} {:>5} rend/reuse",
            "pane", "total", "p95", "share"
        ),
        yellow,
    )];
    for (rank, row) in report.rows.iter().take(MOD_HUD_ROWS).enumerate() {
        let text = format!(
            "{} {} {:>7} {:>6} {:>4}% {:>5}/{:<5}{}",
            rank + 1,
            thurbox::kernel::perf::fit_columns(&row.name, 13),
            fmt_hud_us(row.total_us),
            fmt_hud_us(row.stats.render.percentile_us(95)),
            (row.frame_share * 100.0).round() as u64,
            row.stats.renders,
            row.stats.reused,
            if row.hints.is_empty() { "" } else { "!" },
        );
        let style = if rank == 0 && row.total_us > 0 {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else if !row.hints.is_empty() {
            Style::default().fg(Color::LightYellow)
        } else {
            yellow
        };
        lines.push(Line::styled(text, style));
    }
    if let Some((name, hint)) = report
        .rows
        .iter()
        .find_map(|row| row.hints.first().map(|hint| (&row.name, hint)))
    {
        lines.push(Line::styled(format!("! {name}: {}", hint.text()), yellow));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

pub(crate) fn to_press(key: &KeyEvent) -> KeyPress {
    let name = match key.code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_lowercase().to_string(),
        KeyCode::F(n) => format!("f{n}"),
        other => format!("{other:?}").to_lowercase(),
    };
    KeyPress {
        name,
        ch: match key.code {
            KeyCode::Char(c) => Some(c),
            _ => None,
        },
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
        // Dropped until issue #1024, which made every `cmd+…` chord
        // unresolvable: `Cmd+C` arrived as a bare `c` and was offered to the
        // focused pane as one. The pty boundary already refuses a SUPER-modified
        // key (`agent::input::key_to_bytes`), so carrying it here costs nothing
        // and is what lets the registry match a Cmd chord.
        cmd: key.modifiers.contains(KeyModifiers::SUPER),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thurbox::kernel::host::Float;

    use crate::App;

    #[test]
    fn chrome_rects_survive_a_terminal_too_small_for_them() {
        for height in 0..=10u16 {
            for width in [0u16, 1, 3, 8, 40] {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };

                let error = error_area(area);
                assert!(
                    error.height <= height && error.bottom() <= area.bottom(),
                    "error_area({width}x{height}) escaped: {error:?}"
                );

                let hud = hud_area(area);
                assert!(
                    hud.right() <= area.right() && hud.bottom() <= area.bottom(),
                    "hud_area({width}x{height}) escaped: {hud:?}"
                );
                let panes = mod_hud_area(area, hud, 20);
                assert!(
                    panes.right() <= area.right() && panes.bottom() <= area.bottom(),
                    "mod_hud_area({width}x{height}) escaped: {panes:?}"
                );

                for float in [
                    Float::default(),
                    Float {
                        cols: Some(200),
                        rows: Some(200),
                        ..Float::default()
                    },
                    Float {
                        cols: Some(0),
                        rows: Some(0),
                        ..Float::default()
                    },
                    Float {
                        cols: Some(20),
                        rows: Some(6),
                        at: Some((width, height)),
                        ..Float::default()
                    },
                    Float {
                        cols: Some(20),
                        rows: Some(6),
                        at: Some((0, 0)),
                        ..Float::default()
                    },
                ] {
                    let rect = App::float_rect(area, float);
                    assert!(
                        rect.right() <= area.right() && rect.bottom() <= area.bottom(),
                        "float_rect({width}x{height}) escaped: {rect:?}"
                    );
                }
            }
        }
    }

    fn anchored(x: u16, y: u16, cols: u16, rows: u16) -> Float {
        Float {
            cols: Some(cols),
            rows: Some(rows),
            at: Some((x, y)),
            ..Float::default()
        }
    }

    #[test]
    fn an_anchored_float_opens_at_its_point() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(
            App::float_rect(area, anchored(10, 5, 20, 6)),
            Rect::new(10, 5, 20, 6)
        );
    }

    #[test]
    fn an_anchored_float_flips_left_at_the_right_edge() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(
            App::float_rect(area, anchored(70, 5, 20, 6)),
            Rect::new(51, 5, 20, 6)
        );
    }

    #[test]
    fn an_anchored_float_flips_up_at_the_bottom() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(
            App::float_rect(area, anchored(10, 22, 20, 6)),
            Rect::new(10, 17, 20, 6)
        );
    }

    #[test]
    fn an_anchored_float_in_the_corner_flips_both_ways() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(
            App::float_rect(area, anchored(79, 23, 20, 6)),
            Rect::new(60, 18, 20, 6)
        );
    }

    #[test]
    fn a_float_wider_than_the_screen_is_pinned_inside_it() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(
            App::float_rect(area, anchored(40, 10, 200, 6)),
            Rect::new(0, 10, 80, 6)
        );
    }

    #[test]
    fn an_anchor_outside_the_area_is_brought_back_into_it() {
        let area = Rect::new(0, 2, 80, 20);
        assert_eq!(
            App::float_rect(area, anchored(10, 0, 20, 6)),
            Rect::new(10, 2, 20, 6)
        );
    }

    #[test]
    fn a_float_with_no_anchor_still_centres() {
        let area = Rect::new(0, 0, 80, 24);
        let float = Float {
            cols: Some(20),
            rows: Some(6),
            ..Float::default()
        };
        assert_eq!(App::float_rect(area, float), Rect::new(30, 9, 20, 6));
    }

    #[test]
    fn clamp_span_prefers_the_cap_to_the_floor() {
        assert_eq!(clamp_span(9, 3, 5), 5);
        assert_eq!(clamp_span(1, 3, 5), 3);
        assert_eq!(clamp_span(9, 3, 2), 2);
        assert_eq!(clamp_span(9, 3, 0), 0);
    }

    #[test]
    fn the_pane_table_moves_beside_the_counters_when_there_is_no_room_below() {
        for (width, height) in [(120u16, 15u16), (120, 17), (100, 12)] {
            let area = Rect {
                x: 0,
                y: 0,
                width,
                height,
            };
            let hud = hud_area(area);
            let panes = mod_hud_area(area, hud, 8);
            assert!(
                panes.height >= 4 && panes.width >= 20,
                "{width}x{height}: no room for a row: {panes:?}"
            );
            assert!(
                panes.intersection(hud).is_empty(),
                "{width}x{height}: covers the counters: {panes:?} / {hud:?}"
            );
        }
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 40,
        };
        let hud = hud_area(area);
        assert_eq!(
            mod_hud_area(area, hud, 8).y,
            hud.bottom(),
            "with room below, it stays under the counters"
        );
    }
}
