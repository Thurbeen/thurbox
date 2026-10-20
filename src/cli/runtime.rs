//! `thurbox-cli runtime` — the processes thurbox runs that are *not* sessions.
//!
//! One exists today: the automation heartbeat, which this machine's backend
//! (the registry's default) keeps running — `automation tick` on a loop, so
//! schedules fire with no interface attached. `status` reports the last tick
//! too, since the keeper throws the tick's own output away. It
//! is created implicitly by anything that arms an automation, and until this
//! command it appeared in no listing and no teardown reclaimed it — a session
//! delete cannot, because it is not a session. Anything thurbox puts on a
//! multiplexer server should be visible and stoppable from the CLI; this is
//! that noun.

use clap::Subcommand;
use serde_json::json;

use super::output::CommandOutput;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// What thurbox is running besides sessions, and on which server.
    Status,
    /// Stop the automation heartbeat keeper.
    ///
    /// Automations stop firing headlessly until it is armed again, which the
    /// next `automation` write does on its own.
    Stop,
}

pub fn run(action: Action, backends: &super::Backends<'_>) -> CommandOutput {
    let socket = crate::backend::instance::local_socket_name();
    let registry = backends.get();
    let here = registry.default_backend();
    match action {
        Action::Status => {
            // `null` when the backend could not be asked: not running is an
            // answer, and this is not one.
            let running = here.heartbeat_running().ok();
            // Which of this machine's backends carry hook status from a pane —
            // each one's own answer, which is also what the Windows harness
            // holds psmux's measured behaviour against.
            let hook_status: serde_json::Map<String, serde_json::Value> = registry
                .all_backends()
                .filter(|(route, _)| !route.is_remote())
                .map(|(route, backend)| {
                    (
                        route.format(),
                        json!(backend.hook_signal_command().is_some()),
                    )
                })
                .collect();
            let last_tick = crate::session_ops::automations::tick_log::last();
            CommandOutput::new(
                json!({
                    "tmux_socket": socket,
                    "backend": here.name(),
                    "automation_heartbeat": running,
                    "hook_status": hook_status,
                    "last_tick": last_tick.as_ref().map(last_tick_json),
                }),
                format!(
                    "socket: {socket}\nbackend: {}\nautomation heartbeat: {}\nlast tick: {}",
                    here.name(),
                    match running {
                        Some(true) => "running",
                        Some(false) => "not running",
                        None => "unknown (the backend did not answer)",
                    },
                    last_tick
                        .as_ref()
                        .map_or_else(|| "never".to_string(), describe),
                ),
            )
            .help([
                "thurbox-cli runtime stop   stop the heartbeat keeper",
                "thurbox-cli automation tick   fire what is due, once",
            ])
        }
        Action::Stop => match here.stop_heartbeat() {
            Ok(stopped) => CommandOutput::new(
                json!({ "tmux_socket": socket, "stopped": stopped }),
                if stopped {
                    "Stopped the automation heartbeat keeper.".to_string()
                } else {
                    "No automation heartbeat keeper was running.".to_string()
                },
            ),
            Err(e) => CommandOutput::failed(
                json!({ "tmux_socket": socket, "stopped": false }),
                format!("Could not stop the automation heartbeat: {e:#}"),
                format!("could not stop the automation heartbeat: {e:#}"),
            ),
        },
    }
}

/// The last tick as `runtime status --json` reports it: what the tick log
/// recorded, plus how long ago, so a driver can tell a heartbeat that stopped
/// from one that is quiet.
fn last_tick_json(tick: &crate::session_ops::automations::tick_log::LastTick) -> serde_json::Value {
    json!({
        "at": tick.at,
        "age_secs": age_secs(tick.at),
        "ok": tick.ok,
        "error": tick.error,
        "fired": tick.fired,
        "failed": tick.failed,
        "errors": tick.errors,
        "log": crate::session_ops::automations::tick_log::log_path()
            .map(|p| p.display().to_string()),
    })
}

fn age_secs(at: u64) -> u64 {
    crate::sync::current_time_millis().saturating_sub(at) / 1000
}

fn describe(tick: &crate::session_ops::automations::tick_log::LastTick) -> String {
    let age = age_secs(tick.at);
    match (&tick.error, tick.ok) {
        (Some(error), false) => format!("{age}s ago, failed: {error}"),
        _ => format!(
            "{age}s ago, ok ({} fired, {} failed, {} could not fire)",
            tick.fired,
            tick.failed,
            tick.errors.len()
        ),
    }
}
