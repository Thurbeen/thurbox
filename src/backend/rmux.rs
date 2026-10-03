//! The opt-in RMUX adapter over the shared tmux-compatible server.

use std::sync::Arc;

use anyhow::{bail, Result};

use crate::backend::contract::SessionBackend;
use crate::backend::tmux_compat::control_mode::{
    hex_send_keys_commands, shell_escape, ControlPolicy, PaneInput,
};
use crate::backend::tmux_compat::server::{ConfigOption, Server, TmuxCompatible};
use crate::backend::tmux_compat::transport::TmuxTransport;
use crate::session::{HostDef, Multiplexer, Platform};
use crate::shell::HostLauncher;

pub struct Rmux;
pub type RmuxBackend = Server<Rmux>;

pub fn local() -> Arc<dyn SessionBackend> {
    Arc::new(RmuxBackend::local())
}

pub fn on_host(
    host: &HostDef,
    launcher: HostLauncher,
    platform: Platform,
) -> Arc<dyn SessionBackend> {
    Arc::new(RmuxBackend::on_host(host, launcher, platform))
}

impl TmuxCompatible for Rmux {
    const MULTIPLEXER: Multiplexer = Multiplexer::Rmux;
    const WINDOW_OPTIONS: bool = true;
    const WINDOW_SETTINGS: bool = true;
    // RMUX does not end a pane stream when its window is killed externally.
    // Poll liveness so an open TUI can relaunch that pane.
    const WINDOW_EVENTS: bool = false;
    const PANE_MONITORING: bool = false;
    const SNAPSHOTS: bool = true;
    const COMMAND_LISTS: bool = false;
    const COMMAND_LIST_SINGLE_REPLY: bool = true;
    const ONE_SHOT_SPAWN_ANSWERS: bool = true;
    const CONDITIONAL_RESIZE: bool = true;
    const SERVER_SCOPE: &str = "-s";
    const DISPLAY_FLAGS: &[&str] = &[];
    // RMUX 0.9.1 speaks wire version 5, which the 0.10.0 client (wire 8)
    // rejects before it can ask that daemon to create a pane. `#{version}`
    // reports tmux compatibility 3.4, so it cannot be used as an RMUX floor.
    const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>> = None;

    fn check_banner(banner: &str, _socket: &str) -> Result<()> {
        let version = banner
            .trim()
            .strip_prefix("rmux ")
            .ok_or_else(|| anyhow::anyhow!("expected an RMUX binary, got {banner:?}"))?;
        let mut parts = version.split('.');
        let parse = |part: Option<&str>| -> Result<u32> {
            Ok(part
                .ok_or_else(|| anyhow::anyhow!("cannot parse RMUX version {version:?}"))?
                .parse()?)
        };
        let version = (
            parse(parts.next())?,
            parse(parts.next())?,
            parse(parts.next())?,
        );
        if version < (0, 10, 0) {
            bail!("RMUX 0.10.0 or newer is required");
        }
        Ok(())
    }

    fn session_config(_session: &str) -> Vec<ConfigOption> {
        Vec::new()
    }

    fn paste_args(target: &str, text: &str) -> Vec<String> {
        vec![
            "send-keys".into(),
            "-t".into(),
            target.into(),
            "-l".into(),
            format!("\x1b[200~{text}\x1b[201~"),
        ]
    }

    fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> String {
        let (mux, socket, target) = (
            shell_escape(mux),
            shell_escape(socket),
            shell_escape(target),
        );
        let text = shell_escape(&format!("\x1b[200~{text}\x1b[201~"));
        format!(
            "{mux} -L {socket} send-keys -t {target} -l {text}; \
             sleep 0.2; {mux} -L {socket} send-keys -t {target} Enter"
        )
    }

    fn pane_input(_transport: &TmuxTransport, _socket: &str) -> Arc<dyn PaneInput> {
        Arc::new(RmuxInput)
    }

    fn control_policy(transport: &TmuxTransport, session: &str) -> ControlPolicy {
        ControlPolicy {
            flow_control_command: None,
            implicit_attach_reply: true,
            tagged_blocks: true,
            command_list_single_reply: Self::COMMAND_LIST_SINGLE_REPLY,
            // RMUX 0.10.0 refuses `refresh-client -B`.
            subscriptions: false,
            status_poll: (transport.is_remote() && Self::HOOK_STATUS).then(|| {
                format!(
                    "list-panes -s -t {} -F '#{{pane_id}} #{{{}}}'",
                    shell_escape(session),
                    crate::backend::tmux_compat::control_mode::REMOTE_HOOK_STATE_OPTION
                )
            }),
        }
    }

    const HOOK_STATUS: bool = true;

    // In a pane RMUX resolves `set-option -p` to that pane. Grok rejects a
    // hook file with a bare `$VAR`, so the command needs no shell variable.
    fn hook_signal_command(_server: &Server<Self>) -> String {
        format!(
            "rmux set-option -p {} ",
            crate::backend::tmux_compat::control_mode::REMOTE_HOOK_STATE_OPTION
        )
    }
}

struct RmuxInput;

impl PaneInput for RmuxInput {
    fn send_keys(&self, pane_id: &str, buf: &[u8]) -> Vec<String> {
        hex_send_keys_commands(pane_id, buf)
    }

    fn paste(&self, _pane_id: &str, _text: &str) -> Option<Result<()>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tested_rmux_protocol_has_a_version_floor() {
        assert!(Rmux::check_banner("rmux 0.9.1", "test").is_err());
        assert!(Rmux::check_banner("rmux 0.10.0", "test").is_ok());
        assert!(Rmux::check_banner("rmux 0.11.0", "test").is_ok());
        assert!(Rmux::check_banner("tmux 3.4", "test").is_err());
    }

    #[test]
    fn the_remote_control_policy_polls_hooks_without_subscriptions() {
        let remote = TmuxTransport::remote(
            HostLauncher::Ssh {
                destination: "example.invalid".into(),
                ssh_opts: Vec::new(),
            },
            "rmux",
        );
        let policy = Rmux::control_policy(&remote, "thurbox");
        assert!(!policy.subscriptions);
        assert!(policy.command_list_single_reply);
        assert_eq!(
            policy.status_poll.as_deref(),
            Some("list-panes -s -t thurbox -F '#{pane_id} #{@thurbox_state}'")
        );
        assert!(
            Rmux::control_policy(&TmuxTransport::local("rmux"), "thurbox")
                .status_poll
                .is_none()
        );
    }
}
