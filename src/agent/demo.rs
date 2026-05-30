//! Internal "demo agent" used by the demo-recording pipeline.
//!
//! This is **not** a user-facing feature. The demo `agents.toml` written by
//! `scripts/demo/record.sh` points an agent's `command` at the thurbox binary
//! itself with the hidden `__demo-agent <scenario>` subcommand. Each spawned
//! session then runs this deterministic, canned transcript instead of a real
//! coding-agent CLI — so demo videos are reproducible with no API key and no
//! network. `src/main.rs` dispatches here before any terminal/TUI setup, so the
//! replay runs inside a thurbox session's tmux pane like a normal agent.

use std::io::{self, Write};
use std::thread::sleep;
use std::time::Duration;

/// Canned transcripts, embedded so there is no runtime file dependency.
const DEFAULT: &str = include_str!("demo_scenarios/default.txt");
const SNAKE: &str = include_str!("demo_scenarios/snake.txt");
const REFACTOR: &str = include_str!("demo_scenarios/refactor.txt");

const RESET: &str = "\x1b[0m";

/// Per-line pacing while streaming the transcript: content lines linger so the
/// output reads like a live agent, blank lines pass quickly.
const CONTENT_LINE_PAUSE: Duration = Duration::from_millis(180);
const BLANK_LINE_PAUSE: Duration = Duration::from_millis(90);

/// Idle tick after the transcript ends. The process must never exit (see
/// [`run_demo_agent`]); it just needs to park without busy-waiting.
const IDLE_TICK: Duration = Duration::from_secs(3600);

/// Pick the transcript for a scenario name (falls back to the default).
fn transcript(scenario: &str) -> &'static str {
    match scenario {
        "snake" => SNAKE,
        "refactor" => REFACTOR,
        _ => DEFAULT,
    }
}

/// Style one transcript line based on a leading marker. The marker is kept in
/// the visible output (it reads naturally: `> `, `$ `, `+ `, `- `, `• `).
fn style(line: &str) -> String {
    // (prefix, ANSI escape)
    let rules: [(&str, &str); 6] = [
        ("> ", "\x1b[1;36m"), // user prompt — bold cyan
        ("# ", "\x1b[1;37m"), // assistant heading — bold white
        ("$ ", "\x1b[33m"),   // shell command — yellow
        ("+ ", "\x1b[32m"),   // diff add — green
        ("- ", "\x1b[31m"),   // diff remove — red
        ("• ", "\x1b[35m"),   // bullet / observation — magenta
    ];
    for (prefix, ansi) in rules {
        if line.starts_with(prefix) {
            return format!("{ansi}{line}{RESET}");
        }
    }
    line.to_string()
}

/// Render the canned transcript for `scenario` to stdout with streaming-style
/// pacing, then idle forever so the tmux pane stays alive while VHS records.
///
/// Diverges: the process must not exit, or the pane would render as "exited"
/// mid-recording. The demo pipeline kills the isolated tmux server on cleanup.
pub fn run_demo_agent(scenario: &str) -> ! {
    let mut out = io::stdout();
    for line in transcript(scenario).lines() {
        let _ = writeln!(out, "{}", style(line));
        let _ = out.flush();
        let pause = if line.trim().is_empty() {
            BLANK_LINE_PAUSE
        } else {
            CONTENT_LINE_PAUSE
        };
        sleep(pause);
    }

    // Leave a final idle prompt and keep the process alive.
    let _ = write!(out, "\n\x1b[1;32m›\x1b[0m ");
    let _ = out.flush();
    loop {
        sleep(IDLE_TICK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_selects_named_scenarios() {
        assert_eq!(transcript("snake"), SNAKE);
        assert_eq!(transcript("refactor"), REFACTOR);
    }

    #[test]
    fn transcript_falls_back_to_default_for_unknown() {
        assert_eq!(transcript("default"), DEFAULT);
        assert_eq!(transcript("does-not-exist"), DEFAULT);
        assert_eq!(transcript(""), DEFAULT);
    }

    #[test]
    fn style_wraps_known_prefix_in_ansi_and_resets() {
        let styled = style("> create a snake game");
        assert!(styled.starts_with("\x1b[1;36m"), "missing prompt color");
        assert!(styled.contains("> create a snake game"), "dropped content");
        assert!(styled.ends_with(RESET), "missing reset");
    }

    #[test]
    fn style_leaves_unmarked_lines_unchanged() {
        // No leading marker → returned verbatim, no escape codes.
        let plain = "  indented body text";
        assert_eq!(style(plain), plain);
        assert_eq!(style(""), "");
    }

    #[test]
    fn blank_lines_pace_faster_than_content() {
        assert!(BLANK_LINE_PAUSE < CONTENT_LINE_PAUSE);
    }
}
