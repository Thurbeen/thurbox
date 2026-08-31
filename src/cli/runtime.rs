//! `thurbox-cli runtime` — the processes thurbox runs that are *not* sessions.
//!
//! One exists today: the automation heartbeat keeper, a detached tmux window
//! looping `automation tick` so schedules fire with no interface attached. It
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

pub fn run(action: Action) -> CommandOutput {
    let socket = crate::agent::tmux::local_socket_name();
    match action {
        Action::Status => {
            let running = crate::agent::tmux::automation_heartbeat_running();
            CommandOutput::new(
                json!({
                    "tmux_socket": socket,
                    "automation_heartbeat": running,
                }),
                format!(
                    "socket: {socket}\nautomation heartbeat: {}",
                    if running { "running" } else { "not running" }
                ),
            )
            .help([
                "thurbox-cli runtime stop   stop the heartbeat keeper",
                "thurbox-cli automation tick   fire what is due, once",
            ])
        }
        Action::Stop => {
            let stopped = crate::agent::tmux::stop_automation_heartbeat();
            CommandOutput::new(
                json!({ "tmux_socket": socket, "stopped": stopped }),
                if stopped {
                    "Stopped the automation heartbeat keeper.".to_string()
                } else {
                    "No automation heartbeat keeper was running.".to_string()
                },
            )
        }
    }
}
