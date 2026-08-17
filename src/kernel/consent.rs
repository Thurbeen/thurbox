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

/// The v1 surfaces v2 has no equivalent for, and where each one still lives.
///
/// Stated here rather than in prose so the gate, the release notes and the docs
/// cannot drift: this is the list, and it is the only list.
pub const GONE: [(&str, &str); 7] = [
    ("code review (Ctrl+X / F7)", "no replacement yet"),
    ("file viewer (Ctrl+E / F3)", "no replacement yet"),
    ("info panel (Ctrl+B / F2)", "no replacement yet"),
    ("tasks panel (F5)", "thurbox-cli task"),
    ("automations pane (Ctrl+P)", "thurbox-cli automation"),
    ("restore list (Ctrl+U)", "thurbox-cli session restore"),
    ("perf HUD (F12)", "thurbox-cli perf"),
];

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
pub fn notice(version: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "\n  thurbox {version} replaces the interface you were using.\n\n"
    ));
    out.push_str(
        "  Every pane is now a plugin you can edit, and the interface was cut back\n\
         \x20 to its core to get there. These are gone from the TUI:\n\n",
    );
    let width = GONE.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    for (name, where_now) in GONE {
        out.push_str(&format!("    {name:<width$}   {where_now}\n"));
    }
    out.push_str(
        "\n  Your sessions, worktrees, tasks and automations are untouched: both\n\
         \x20 interfaces use the same database, and nothing is migrated. Extensions\n\
         \x20 keep running.\n\n",
    );
    out
}

/// What to do instead, printed when the answer is no.
///
/// `auto_update` is turned off by the caller before this is shown, so the two
/// halves match: reinstalling 1.x is pointless while something will replace it
/// again on the next launch.
pub fn downgrade_instructions(last_v1: &str) -> String {
    format!(
        "\n  Staying on v1. Auto-update has been turned off so nothing moves you\n\
         \x20 again. To reinstall it:\n\n\
         \x20   export VERSION={last_v1}\n\
         \x20   curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh\n\n\
         \x20 `export` is required -- in `VERSION=x curl ... | sh` the assignment\n\
         \x20 applies to curl, and the sh on the right of the pipe never sees it.\n\n\
         \x20 Newer 1.x patches: https://github.com/Thurbeen/thurbox/releases\n\
         \x20 To come back to v2 later, run thurbox again and answer yes.\n\n"
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

    let decision = ask(env!("THURBOX_VERSION"))?;
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
            let _ = write!(out, "{}", downgrade_instructions(LAST_V1_RELEASE));
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
pub fn ask(version: &str) -> std::io::Result<Decision> {
    use crossterm::event::{read, Event, KeyCode, KeyModifiers};

    let mut out = std::io::stdout();
    write!(out, "{}", notice(version))?;
    write!(
        out,
        "  [Enter] continue to v2 (asked once)      [q] stay on v1\n\n  > "
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

    #[test]
    fn the_notice_names_every_dropped_surface() {
        // The gate is the only place a user is told, so a surface missing from it
        // is a surface they discover is gone by looking for it.
        let text = notice("2.0.0");
        for (name, where_now) in GONE {
            assert!(text.contains(name), "{name} missing from the notice");
            assert!(text.contains(where_now), "{where_now} missing");
        }
        assert!(text.contains("2.0.0"));
        assert!(
            text.contains("same database"),
            "it has to say sessions are safe, or the list reads as data loss"
        );
    }

    #[test]
    fn the_downgrade_instruction_exports_and_disables_auto_update() {
        let text = downgrade_instructions("v1.8.6");
        assert!(
            text.contains("export VERSION=v1.8.6"),
            "a bare `VERSION=x curl | sh` sets it for curl, not for sh: {text}"
        );
        assert!(
            text.contains("turned off"),
            "reinstalling 1.x is pointless if auto-update will undo it"
        );
    }
}
