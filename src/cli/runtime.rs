//! `thurbox-cli runtime` — the processes thurbox runs that are *not* sessions.
//!
//! One exists today: the automation heartbeat, which this machine's backend
//! (the registry's default) keeps running — `automation tick` on a loop, so
//! schedules fire with no interface attached. It
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
            CommandOutput::new(
                json!({
                    "tmux_socket": socket,
                    "backend": here.name(),
                    "automation_heartbeat": running,
                    "hook_status": hook_status,
                }),
                format!(
                    "socket: {socket}\nbackend: {}\nautomation heartbeat: {}",
                    here.name(),
                    match running {
                        Some(true) => "running",
                        Some(false) => "not running",
                        None => "unknown (the backend did not answer)",
                    }
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
