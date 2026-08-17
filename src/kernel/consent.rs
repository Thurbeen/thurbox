//! The one-time gate a v1 profile passes on its first v2 launch.
//!
//! v2 replaces v1 under the same binary name, which means auto-update moves
//! people to a different interface without their having asked — and `auto_update`
//! defaults to `true`, so most of them never read a release note first. Several
//! surfaces they may use every day are simply gone (see [`GONE`]).
//!
//! So the first launch on a profile that has v1 history stops and asks. It is a
//! plain-stdout screen rather than a modal, deliberately: it runs *before* the
//! interface takes the terminal, so it appears even if the interface would fail to
//! build, and it cannot be dismissed by a stray keystroke queued for a pane.
//!
//! Declining cannot load v1 — it is not in this binary any more. What it does is
//! turn `auto_update` **off** and print how to reinstall the 1.x line, because a
//! downgrade that leaves auto-update on would be undone on the next launch.

use std::io::Write;

use crate::storage::Database;

/// The v1 surfaces v2 has no equivalent for: what it was, the chord that opened
/// it, and where it still lives (empty = nowhere yet).
///
/// Stated here rather than in prose so the gate, the release notes and the docs
/// cannot drift: this is the list, and it is the only list.
pub const GONE: [(&str, &str, &str); 7] = [
    ("code review", "Ctrl+X / F7", ""),
    ("file viewer", "Ctrl+E / F3", ""),
    ("info panel", "Ctrl+B / F2", ""),
    ("tasks panel", "F5", "thurbox-cli task"),
    ("automations pane", "Ctrl+P", "thurbox-cli automation"),
    ("restore list", "Ctrl+U", "thurbox-cli session restore"),
    ("perf HUD", "F12", "thurbox-cli perf"),
];

/// thurbox's mark, as `scripts/install.sh` prints it. Shared shape on purpose:
/// this screen and the installer are the two places a person meets the project
/// outside the interface itself.
const BANNER: [&str; 6] = [
    r"   _____ _   _ _   _____________  _______   __",
    r"  |_   _| | | | | | | ___ \ ___ \|  _  \ \ / /",
    r"    | | | |_| | | | | |_/ / |_/ /| | | |\ V / ",
    r"    | | |  _  | | | |    /| ___ \| | | |/   \ ",
    r"    | | | | | | |_| | |\ \| |_/ /\ \_/ / /^\ \",
    r"    \_/ \_| |_/\___/\_| \_\____/  \___/\/   \/",
];

/// How wide the framed heading is drawn.
///
/// Wide enough to be no narrower than the paragraph beneath it — a frame that the
/// text overhangs reads as a mistake — and narrow enough that the whole screen
/// fits an 80-column terminal, which `the_screen_fits_eighty_columns` holds.
const WIDTH: usize = 70;

/// The palette roles this screen paints with, resolved to escape sequences once.
///
/// Built from the user's *own* active theme rather than hardcoded colours: they
/// chose it, the interface behind this screen is about to use it, and a gate in
/// somebody else's colours reads like a different program. Every field is empty
/// when colour is off, so the same format strings serve both and there is no
/// second code path to keep correct.
pub struct Skin {
    accent: String,
    bright: String,
    muted: String,
    text: String,
    gone: String,
    moved: String,
    safe: String,
    bold: String,
    reset: String,
}

impl Skin {
    /// Colour only when it will be read by a terminal.
    ///
    /// The three conditions `scripts/install.sh` already honours: a TTY, no
    /// `NO_COLOR`, and a `TERM` that is not `dumb`. Piping this screen into a file
    /// or a pager must not fill it with escapes.
    pub fn detect(palette: &crate::session::theme_config::ThemePalette) -> Self {
        let colour = std::io::IsTerminal::is_terminal(&std::io::stdout())
            && std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true);
        if !colour {
            return Self::plain();
        }
        Self {
            accent: fg(palette.accent),
            bright: fg(palette.accent_bright),
            muted: fg(palette.text_muted),
            text: fg(palette.text_primary),
            gone: fg(palette.status_blocked),
            moved: fg(palette.status_done),
            safe: fg(palette.status_idle),
            bold: "\x1b[1m".to_string(),
            reset: "\x1b[0m".to_string(),
        }
    }

    /// Every role empty, so the text stands on its own.
    pub fn plain() -> Self {
        Self {
            accent: String::new(),
            bright: String::new(),
            muted: String::new(),
            text: String::new(),
            gone: String::new(),
            moved: String::new(),
            safe: String::new(),
            bold: String::new(),
            reset: String::new(),
        }
    }
}

/// A palette colour as an SGR foreground sequence.
///
/// `Reset` yields the terminal default rather than a colour, which is what the
/// ANSI-based Default preset wants — it deliberately keeps the user's own palette.
fn fg(colour: ratatui::style::Color) -> String {
    use ratatui::style::Color;
    match colour {
        Color::Reset => "\x1b[39m".to_string(),
        Color::Rgb(r, g, b) => format!("\x1b[38;2;{r};{g};{b}m"),
        Color::Indexed(i) => format!("\x1b[38;5;{i}m"),
        Color::Black => "\x1b[30m".to_string(),
        Color::Red => "\x1b[31m".to_string(),
        Color::Green => "\x1b[32m".to_string(),
        Color::Yellow => "\x1b[33m".to_string(),
        Color::Blue => "\x1b[34m".to_string(),
        Color::Magenta => "\x1b[35m".to_string(),
        Color::Cyan => "\x1b[36m".to_string(),
        Color::Gray => "\x1b[37m".to_string(),
        Color::DarkGray => "\x1b[90m".to_string(),
        Color::LightRed => "\x1b[91m".to_string(),
        Color::LightGreen => "\x1b[92m".to_string(),
        Color::LightYellow => "\x1b[93m".to_string(),
        Color::LightBlue => "\x1b[94m".to_string(),
        Color::LightMagenta => "\x1b[95m".to_string(),
        Color::LightCyan => "\x1b[96m".to_string(),
        Color::White => "\x1b[97m".to_string(),
    }
}

/// What the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run v2, and never ask again on this profile.
    Continue,
    /// Do not run. Auto-update has been turned off and instructions printed.
    Declined,
}

/// Whether this profile has to be asked.
///
/// Two conditions, and the second is what keeps a fresh install quiet: there is
/// no v1 to warn somebody about who has never run thurbox before.
pub fn required(db: &Database) -> bool {
    if db.v2_acknowledged().unwrap_or(false) {
        return false;
    }
    db.has_session_history().unwrap_or(false)
}

/// The notice, built rather than printed so it can be asserted on.
///
/// Laid out like the interface it is introducing: the mark, a framed heading, the
/// dropped surfaces as a glyph-led table, and one reassurance line. The glyphs are
/// the interface's own — `✗` for a surface with nowhere to go and `→` for one that
/// moved, in the same theme roles the session list paints status with, so somebody
/// who has used thurbox recognises the vocabulary before reading a word.
pub fn notice(version: &str, skin: &Skin) -> String {
    let (a, b, m, t) = (&skin.accent, &skin.bright, &skin.muted, &skin.text);
    let (gone, moved, safe) = (&skin.gone, &skin.moved, &skin.safe);
    let (bold, r) = (&skin.bold, &skin.reset);
    let mut out = String::new();

    out.push('\n');
    for line in BANNER {
        out.push_str(&format!("{b}{bold}{line}{r}\n"));
    }
    out.push_str(&format!(
        "{m}        multi-session coding-agent orchestrator{r}\n\n"
    ));

    let rule = "─".repeat(WIDTH - 2);
    let heading = format!("thurbox {version} — a new interface");
    out.push_str(&format!("  {a}╭{rule}╮{r}\n"));
    out.push_str(&format!(
        "  {a}│{r} {t}{bold}{heading}{r}{pad} {a}│{r}\n",
        pad = " ".repeat((WIDTH - 4).saturating_sub(heading.chars().count()))
    ));
    out.push_str(&format!("  {a}╰{rule}╯{r}\n\n"));

    out.push_str(&format!(
        "  {t}Every pane is a plugin you can edit now. Getting there meant cutting{r}\n\
         \x20 {t}the interface back to its core, so these are gone from the TUI:{r}\n\n"
    ));

    let name_w = GONE
        .iter()
        .map(|(n, _, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    let key_w = GONE
        .iter()
        .map(|(_, k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    for (name, key, where_now) in GONE {
        if where_now.is_empty() {
            out.push_str(&format!(
                "    {gone}✗{r} {t}{name}{r}{np}  {m}{key}{r}{kp}   {m}nothing yet{r}\n",
                np = " ".repeat(name_w - name.chars().count()),
                kp = " ".repeat(key_w - key.chars().count())
            ));
        } else {
            out.push_str(&format!(
                "    {moved}→{r} {t}{name}{r}{np}  {m}{key}{r}{kp}   {a}{where_now}{r}\n",
                np = " ".repeat(name_w - name.chars().count()),
                kp = " ".repeat(key_w - key.chars().count())
            ));
        }
    }

    out.push_str(&format!(
        "\n    {safe}●{r} {t}Your sessions, worktrees, tasks and automations are untouched.{r}\n\
         \x20     {m}Both interfaces share one database. Nothing is migrated, and{r}\n\
         \x20     {m}extensions keep running.{r}\n\n"
    ));
    out
}

/// What to do instead, printed when the answer is no.
///
/// `auto_update` is turned off by the caller before this is shown, so the two
/// halves match: reinstalling 1.x is pointless while something will replace it
/// again on the next launch.
///
/// The command exports `VERSION` rather than prefixing the pipeline because a
/// prefix binds to `curl`, not to the `sh` reading from it. That is the reason,
/// not something the prompt explains -- a person at a gate wants the command.
pub fn downgrade_instructions(last_v1: &str, skin: &Skin) -> String {
    let (a, m, t) = (&skin.accent, &skin.muted, &skin.text);
    let (safe, bold, r) = (&skin.safe, &skin.bold, &skin.reset);
    format!(
        "\n  {safe}●{r} {t}Staying on v1. Auto-update is off, so nothing moves you again.{r}\n\n\
         \x20   {m}Reinstall it with:{r}\n\n\
         \x20     {a}{bold}export VERSION={last_v1}{r}\n\
         \x20     {a}{bold}curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh{r}\n\n\
         \x20   {m}Newer 1.x patches   {r}{t}https://github.com/Thurbeen/thurbox/releases{r}\n\
         \x20   {m}Change your mind    {r}{t}run thurbox again and answer yes{r}\n\n"
    )
}

/// The last v1 release at the cutover.
///
/// A concrete version because `install.sh` needs an exact tag; the instructions
/// point at the releases page for any newer 1.x patch, so this going stale costs
/// the reader nothing.
pub const LAST_V1_RELEASE: &str = "v1.8.6";

/// Ask if this profile has to be asked, and act on the answer.
///
/// Returns [`Decision::Continue`] without prompting when there is nothing to warn
/// about — a fresh profile, or one that has already answered.
pub fn consent_gate(db: &Database) -> std::io::Result<Decision> {
    if !required(db) {
        return Ok(Decision::Continue);
    }

    // Nobody to ask. A script, a CI job, a provisioning run or a recording harness
    // launched this, and blocking on a keypress there is not consent -- it is a
    // hang, and `read()` on a closed stdin failed the launch outright with a raw
    // OS error. Continue, because whoever ran this wanted thurbox, and do NOT
    // record the answer: the next interactive launch still asks a person.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        tracing::info!(
            "not a terminal: continuing to the v2 interface without asking. \
             Run `thurbox-cli config accept-interface` to stop asking."
        );
        return Ok(Decision::Continue);
    }

    // The user's own palette: they chose it, and the interface behind this screen
    // is about to use it. Falls back to the default preset when the theme cannot
    // be read -- a gate is not worth failing a launch over.
    let themes = crate::kernel::theme::Themes::load(Some(db));
    let skin = Skin::detect(&themes.active().palette);

    let decision = ask(env!("THURBOX_VERSION"), &skin)?;
    match decision {
        Decision::Continue => {
            // Recorded even if it fails to persist, in the sense that a failed
            // write only costs a second prompt -- never a wrong answer.
            if let Err(e) = db.acknowledge_v2() {
                tracing::warn!("could not record the v2 acknowledgement: {e}");
            }
        }
        Decision::Declined => {
            let disabled = disable_auto_update();
            let mut out = std::io::stdout();
            if let Err(e) = &disabled {
                // Say so rather than claim it: an instruction to downgrade is
                // actively misleading if the thing that would undo it is still on.
                let _ = write!(
                    out,
                    "\n  Could not turn auto-update off ({e}). Set `auto_update = false`\n  \
                     under [features] in your settings.toml before reinstalling, or the\n  \
                     next launch will update you again.\n"
                );
            }
            let _ = write!(out, "{}", downgrade_instructions(LAST_V1_RELEASE, &skin));
            let _ = out.flush();
        }
    }
    Ok(decision)
}

/// Turn `auto_update` off, so a downgrade is not undone on the next launch.
///
/// Written through `settings_config::save_settings`, which edits the file with
/// `toml_edit` and therefore keeps the seed's documentation comments.
fn disable_auto_update() -> Result<(), String> {
    let (mut settings, _) = crate::agent::settings_config::load_or_seed_with_warnings();
    if !settings.features.auto_update {
        return Ok(());
    }
    settings.features.auto_update = false;
    crate::agent::settings_config::save_settings(&settings).map_err(|e| e.to_string())
}

/// Show the notice and read one key.
///
/// Raw mode for the single keypress only, so a `q` cannot be taken from a
/// line-buffered paste and so Ctrl+C still works the way it does at a prompt.
pub fn ask(version: &str, skin: &Skin) -> std::io::Result<Decision> {
    use crossterm::event::{read, Event, KeyCode, KeyModifiers};

    let mut out = std::io::stdout();
    write!(out, "{}", notice(version, skin))?;
    let (a, m, t) = (&skin.accent, &skin.muted, &skin.text);
    let (bold, r) = (&skin.bold, &skin.reset);
    write!(
        out,
        "  {a}{bold}▸{r} {a}{bold}Enter{r} {t}continue to v2{r}   {m}·{r}   \
         {a}{bold}q{r} {t}stay on v1{r}   {m}(asked once){r}\n\n  {a}{bold}>{r} "
    )?;
    out.flush()?;

    crossterm::terminal::enable_raw_mode()?;
    let decision = loop {
        match read() {
            Ok(Event::Key(key)) => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    // Enter, or an explicit yes. `Char` arms test the modifier
                    // rather than ignoring it: matching a bare `q` would make
                    // Ctrl+Q decline, and a chord nobody aimed at this prompt
                    // must not answer it.
                    KeyCode::Enter if !ctrl => break Decision::Continue,
                    KeyCode::Char('y' | 'Y') if !ctrl => break Decision::Continue,
                    KeyCode::Char('q' | 'Q' | 'n' | 'N') if !ctrl => break Decision::Declined,
                    KeyCode::Esc => break Decision::Declined,
                    // Ctrl+C and Ctrl+Q at a prompt mean "I did not choose",
                    // which is not consent -- so they decline.
                    KeyCode::Char('c' | 'q') if ctrl => break Decision::Declined,
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(e) => {
                let _ = crossterm::terminal::disable_raw_mode();
                return Err(e);
            }
        }
    };
    crossterm::terminal::disable_raw_mode()?;
    writeln!(out)?;
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip SGR sequences, so a width can be measured rather than guessed.
    fn visible(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for e in chars.by_ref() {
                    if e == 'm' {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    /// The screen must not wrap in an 80-column terminal.
    ///
    /// It is the first thing a v1 user sees, and it is printed before the
    /// interface takes the terminal — so there is no layout engine to save it, and
    /// a wrapped frame or a broken banner is what the upgrade looks like. Run with
    /// `--no-capture` to eyeball it.
    #[test]
    fn the_screen_fits_eighty_columns() {
        let themes = crate::kernel::theme::Themes::load(None);
        let p = &themes.active().palette;
        let painted = Skin {
            accent: fg(p.accent),
            bright: fg(p.accent_bright),
            muted: fg(p.text_muted),
            text: fg(p.text_primary),
            gone: fg(p.status_blocked),
            moved: fg(p.status_done),
            safe: fg(p.status_idle),
            bold: "\x1b[1m".to_string(),
            reset: "\x1b[0m".to_string(),
        };

        let screen = format!(
            "{}{}",
            notice("2.0.0", &painted),
            downgrade_instructions(LAST_V1_RELEASE, &painted)
        );
        print!("{screen}");

        for line in screen.lines() {
            let shown = visible(line);
            // The installer command is one long URL that cannot be broken, and is
            // meant to be copied rather than read; everything else must fit.
            if shown.contains("install.sh") {
                continue;
            }
            assert!(
                shown.chars().count() <= 80,
                "{} columns: {shown:?}",
                shown.chars().count()
            );
        }

        // Painted output must always close its spans, or the colour bleeds into
        // whatever the shell prints next.
        for line in screen.lines().filter(|l| l.contains('\x1b')) {
            assert!(
                line.ends_with("\x1b[0m") || visible(line).is_empty(),
                "a painted line has to reset: {line:?}"
            );
        }
    }

    #[test]
    fn the_notice_names_every_dropped_surface() {
        // The gate is the only place a user is told, so a surface missing from it
        // is a surface they discover is gone by looking for it.
        let text = notice("2.0.0", &Skin::plain());
        for (name, key, where_now) in GONE {
            assert!(text.contains(name), "{name} missing from the notice");
            assert!(text.contains(key), "{name}'s chord ({key}) missing");
            if !where_now.is_empty() {
                assert!(text.contains(where_now), "{where_now} missing");
            }
        }
        assert!(
            text.contains("nothing yet"),
            "a surface with no replacement has to say so, not just be listed"
        );
        assert!(text.contains("2.0.0"));
        assert!(
            text.contains("share one database") && text.contains("untouched"),
            "it has to say sessions are safe, or the list reads as data loss"
        );
    }

    #[test]
    fn the_downgrade_instruction_exports_and_disables_auto_update() {
        let text = downgrade_instructions("v1.8.6", &Skin::plain());
        assert!(
            text.contains("export VERSION=v1.8.6"),
            "a bare `VERSION=x curl | sh` sets it for curl, not for sh: {text}"
        );
        assert!(
            text.contains("Auto-update is off"),
            "reinstalling 1.x is pointless if auto-update will undo it"
        );
    }
}
