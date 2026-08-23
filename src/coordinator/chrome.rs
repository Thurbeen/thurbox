//! The chrome the kernel paints itself, and the terminal the whole interface
//! sits in: the host-service helpers (`snapshots_db`, the URL opener, the
//! editor launcher), the terminal-mode setup and teardown the loop and the
//! panic hook share, and the rects and renderers for the error panel and the
//! perf HUD.

use super::*;

/// A connection for reading the persisted theme choice at startup.
///
/// Separate from the snapshot store's: this is read once, and opening a second
/// short-lived connection is cheaper than threading one through construction.
pub(crate) fn snapshots_db() -> Option<thurbox::storage::Database> {
    thurbox::paths::database_file().and_then(|path| thurbox::storage::Database::open(&path).ok())
}

/// Can a link actually be opened here?
///
/// On a remote session or a bare tty there is no browser, and spawning an
/// opener goes nowhere silently — which is why v1 learned to copy instead and
/// say so. Published to plugins so a pane can label its key "open" or "copy"
/// *before* you press it.
///
/// v1's `has_browser_target`: any of the three set and non-blank is enough, and
/// `BROWSER` overrides the display check because a terminal browser is a valid
/// target on a machine with no X or Wayland session.
pub(crate) fn browser_available() -> bool {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return true;
    }
    ["BROWSER", "DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
}

/// Hand a URL to the platform's opener.
///
/// v1's `helpers::open_url`. The child is spawned and **not** waited on: a
/// launcher can take seconds to come up and the render loop must not park in
/// `waitpid`, so a successful spawn is reported as opened.
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

/// Turn a written chord back into the keystroke it names.
///
/// The inverse of `registry::canonical_chord`, needed only by
/// [`ClickVerb::Key`]: replaying a click as a real key event is what stops a
/// modal button and its letter from ever diverging. Built on
/// `registry::normalise_chord` so the spellings a plugin may write (`Ctrl+D`,
/// `command+j`) stay the registry's vocabulary rather than becoming a second
/// one.
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

/// The editor to open a session's directory with.
///
/// v1's chain, from `resolve_editor` (`src/cli/config.rs`): the DB setting
/// `thurbox-cli editor set` writes, then `$VISUAL`, then `$EDITOR`.
pub(crate) fn editor_command() -> Option<String> {
    snapshots_db()
        .and_then(|db| db.get_editor_command().ok().flatten())
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|command| !command.trim().is_empty())
}

/// How the editor should be launched, as configured (`thurbox-cli editor mode`).
///
/// `Auto` — the default — leaves the decision to the name-based classification.
pub(crate) fn editor_mode() -> thurbox::session::settings::EditorMode {
    snapshots_db()
        .and_then(|db| db.get_editor_mode().ok())
        .unwrap_or_default()
}

/// Run the configured editor over a session's directories.
///
/// Every directory, not just the first: a multi-repo session's whole point is
/// that its repositories are worked together, and opening one of them is the
/// same bug as forgetting the others exist.
///
/// A **terminal** editor gets a real tty, which mirrors v1's
/// `run_pending_editor` (`src/main.rs`): inside tmux it floats in a
/// `display-popup` with a pty of its own and the TUI keeps its screen
/// underneath; elsewhere the terminal is handed over for the editor's lifetime
/// and taken back afterwards — the git/sudoedit pattern. Blocking the render
/// loop while someone edits is correct, not a bug.
///
/// A **GUI** editor is spawned detached instead. Handing one a tty is not
/// harmless: a launcher told to wait (`code --wait`) holds the terminal for the
/// whole editing session with nothing drawn in it.
pub(crate) fn open_editor(
    terminal: &mut DefaultTerminal,
    dirs: &[std::path::PathBuf],
) -> Result<String, String> {
    let first = dirs
        .first()
        .ok_or("that session has no directory to open")?;
    let configured = editor_command()
        .ok_or("no editor configured — set one with `thurbox-cli editor set <command>`")?;
    let (program, mut args) = super::editor::parse_editor_command(&configured)
        .map_err(|e| format!("the configured editor command is unusable: {e}"))?;
    let terminal_editor = super::editor::is_terminal_editor(&program, &args, editor_mode());
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
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    );
    let _ = crossterm::terminal::disable_raw_mode();
    let status = std::process::Command::new(&program).args(&args).status();
    let _ = crossterm::terminal::enable_raw_mode();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen);
    // Mirrors the disable above; harmless when the feature is off, since the
    // loop drops mouse events either way.
    enable_mouse_clicks();
    let _ = terminal.clear();

    match status {
        Ok(_) => Ok(format!("closed {program}")),
        Err(e) => Err(format!("could not run {program}: {e}")),
    }
}

/// Undo everything boot did to the terminal, in reverse.
///
/// Safe to call twice and safe to call when some of it was never enabled —
/// every step is best-effort, because this runs on the panic path where
/// failing to clean up is worse than a redundant escape.
pub(crate) fn restore_terminal() {
    // Reverse order of setup, so the kitty flags come off while raw mode is still
    // on -- popping them after `ratatui::restore()` would write the escape to a
    // cooked terminal.
    pop_keyboard_enhancement();
    // Unconditional: cheaper than tracking whether capture was on, and a
    // terminal left reporting is the failure this exists to prevent.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste
    );
    ratatui::restore();
}

/// Whether we pushed the kitty flags, so only we pop them.
static KEYBOARD_ENHANCEMENT_PUSHED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Ask for `DISAMBIGUATE_ESCAPE_CODES`, if the terminal supports it.
pub(crate) fn push_keyboard_enhancement() {
    use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
    if matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    ) && crossterm::execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok()
    {
        KEYBOARD_ENHANCEMENT_PUSHED.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Pop them if and only if we pushed.
///
/// `swap` so a second restore -- the panic hook racing the normal path -- cannot
/// pop a level we never pushed.
pub(crate) fn pop_keyboard_enhancement() {
    if KEYBOARD_ENHANCEMENT_PUSHED.swap(false, std::sync::atomic::Ordering::SeqCst) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags
        );
    }
}

/// Ask the terminal for clicks, motion, and SGR coordinates.
///
/// Three modes, each earning its keep:
///
/// * `?1000` — presses and releases, what the click registry needs.
/// * `?1003` — motion, **whether or not a button is down**. Both of thurbox's
///   pointer features need it and neither worked without it: a drag reports
///   nothing between press and release (so dragging a selection selected
///   nothing), and a *hover* highlight has no event at all to fire on, which is
///   why every button stayed unlit. `?1002` covers only the first of those, so it
///   is not enough. The flood is real — one report per cell crossed — and is
///   absorbed where it arrives rather than by asking for less: the loop drains
///   every queued event per iteration, and a `Moved` that does not change the
///   identity under the pointer is dropped without touching `dirty`.
/// * `?1006` — SGR coordinates, so columns past 223 survive.
///
/// v1 asks for `?1000`+`?1002` (crossterm's `EnableMouseCapture`), which is why
/// its hover highlight is likewise limited to drags.
///
/// `DisableMouseCapture` still turns everything off, so teardown is unchanged.
pub(crate) fn enable_mouse_clicks() -> bool {
    use std::io::Write;
    let mut out = std::io::stdout();
    out.write_all(b"\x1b[?1000h\x1b[?1003h\x1b[?1006h").is_ok() && out.flush().is_ok()
}

/// The next terminal event, or `None` if none arrived within `timeout`.
///
/// The two crossterm calls belong together: a `poll` that says yes is what makes
/// the `read` non-blocking, and either can fail the same way.
pub(crate) fn next_event(timeout: Duration) -> std::io::Result<Option<Event>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    event::read().map(Some)
}

/// Clamp a span into `floor..=cap`, tolerating a `cap` below the `floor`.
///
/// `u16::clamp` asserts `min <= max`, and every rect below takes its cap from the
/// space available — which on a short terminal is smaller than the floor the
/// content wants. The cap wins, because a rect must never exceed its parent.
pub(crate) fn clamp_span(value: u16, floor: u16, cap: u16) -> u16 {
    value.clamp(floor.min(cap), cap)
}

/// Where the reload-failure panel goes: the bottom of the screen, sized to the
/// message but never more than half the height.
pub(crate) fn error_area(area: Rect) -> Rect {
    // Half the screen, but never less than the three rows the message needs and
    // never more than the screen has. This panel is what a broken plugin shows
    // through, so it is the last thing that may itself panic.
    let cap = (area.height / 2).max(3).min(area.height);
    let height = clamp_span(area.height.saturating_sub(2), 3, cap);
    Rect {
        x: area.x,
        y: area.y.saturating_add(area.height.saturating_sub(height)),
        width: area.width,
        height: height.min(area.height),
    }
}

/// Where the perf HUD sits: the top-right corner, clamped to what there is.
///
/// The corner the session list is not in, so the pane you are most likely to be
/// watching while measuring is the one it does not cover.
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

/// A digest of one rect of the frame, for comparing against the last one.
///
/// A hash of the cells rather than a clone of them: the settle diff only asks
/// "same as last frame?", and storing the cells cost a `Cell` clone per band
/// cell per painted frame. Hashing keeps the property that made cells the
/// right input — exact, and immune to a new `BandState` field being forgotten
/// — at zero retained allocation. Clipped to the buffer's own area: a rect the
/// arrangement produced is trusted to be inside the frame, but reading out of
/// bounds would panic rather than merely mis-compare.
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

/// Compact µs for the HUD's narrow columns; `cli::perf` formats the same way.
pub(crate) fn fmt_hud_us(us: u64) -> String {
    if us < 1_000 {
        format!("{us}us")
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.1}s", us as f64 / 1_000_000.0)
    }
}

/// Paint the counters.
///
/// Counts rather than timings, which is the whole point of `kernel::perf`: a
/// number that says "an idle loop painted no frames" is exact, where one that
/// says "idle was fast" is a coin toss on shared hardware.
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
        "iterations {}\nframes     {}\nskipped    {}\nrenders    {}\nreused r/g {}/{}\nfailures   {}\nreloads    {}\n",
        counters.iterations,
        counters.frames,
        counters.skipped,
        counters.renders,
        counters.renders_skipped,
        counters.groups_reused,
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
    match timings.slow_ops.iter_recent().next() {
        Some(op) => text.push_str(&format!("slow   {}:{}ms", op.name, op.ms)),
        None => text.push_str("slow   none"),
    }
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::Yellow)),
        inner,
    );
}

/// Flatten a crossterm key into what Lua is told about it.
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thurbox::kernel::host::Float;

    /// Every chrome rect is derived from the space available, so each one has to
    /// hold at a size smaller than its own content floor. The error panel matters
    /// most: it is what a broken plugin shows through, and a five-row pane plus a
    /// syntax error under `ui/` is the ordinary state while authoring one.
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

    /// The cap wins over the floor, so a rect never exceeds the space it was
    /// given even when the content wants more.
    #[test]
    fn clamp_span_prefers_the_cap_to_the_floor() {
        assert_eq!(clamp_span(9, 3, 5), 5);
        assert_eq!(clamp_span(1, 3, 5), 3);
        assert_eq!(clamp_span(9, 3, 2), 2);
        assert_eq!(clamp_span(9, 3, 0), 0);
    }
}
