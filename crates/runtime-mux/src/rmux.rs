use std::sync::Arc;

use anyhow::{bail, Result};

use crate::contract::SessionBackend;
use crate::settings::MuxSettings;
use crate::shell::HostLauncher;
use crate::tmux_compat::control_mode::{ControlPolicy, Missing};
use crate::tmux_compat::server::{
    parse_version, tmux_answered_absent, ConfigOption, Server, TmuxCompatible,
};
use crate::tmux_compat::transport::TmuxTransport;
use crate::{HostDef, Multiplexer, Platform};

pub struct Rmux;
pub type RmuxBackend = Server<Rmux>;

pub fn local(settings: &MuxSettings) -> Arc<dyn SessionBackend> {
    Arc::new(RmuxBackend::local(settings))
}

pub fn on_host(
    host: &HostDef,
    launcher: HostLauncher,
    platform: Platform,
    settings: &MuxSettings,
) -> Arc<dyn SessionBackend> {
    Arc::new(RmuxBackend::on_host(host, launcher, platform, settings))
}

impl TmuxCompatible for Rmux {
    const MULTIPLEXER: Multiplexer = Multiplexer::Rmux;
    const WINDOW_OPTIONS: bool = true;
    const WINDOW_SETTINGS: bool = true;
    // RMUX does not end a pane stream when its window is killed externally.
    const WINDOW_EVENTS: bool = false;
    const PANE_MONITORING: bool = false;
    const SNAPSHOTS: bool = true;
    const COMMAND_LISTS: bool = false;
    const COMMAND_LIST_SINGLE_REPLY: bool = true;
    const ONE_SHOT_SPAWN_ANSWERS: bool = true;
    const CONDITIONAL_RESIZE: bool = true;
    const SERVER_SCOPE: &str = "-s";
    const DISPLAY_FLAGS: &[&str] = &[];
    // RMUX 0.10.0 rejects a 0.9.1 daemon's wire, and `#{version}` reports tmux 3.4, so no floor reads it.
    const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>> = None;

    fn check_banner(banner: &str, _socket: &str) -> Result<()> {
        let version = Self::banner_version(banner)
            .ok_or_else(|| anyhow::anyhow!("expected an RMUX binary, got {banner:?}"))?;
        let version = parse_version(version)
            .ok_or_else(|| anyhow::anyhow!("cannot parse RMUX version {version:?}"))?;
        if version < (0, 10, 0) {
            bail!("RMUX 0.10.0 or newer is required");
        }
        Ok(())
    }

    fn session_config(_session: &str) -> Vec<ConfigOption> {
        Vec::new()
    }

    fn answered_absent(answer: &str) -> Option<Missing> {
        let Some((target, reply)) = answer
            .strip_prefix("invalid target '")
            .and_then(|wrapped| wrapped.split_once("': "))
        else {
            return rmux_answered_absent(answer);
        };
        reply
            .rsplit_once(": ")
            .filter(|(_, named)| *named == target)
            .and_then(|_| rmux_answered_absent(reply))
    }

    fn control_policy(transport: &TmuxTransport, session: &str) -> ControlPolicy {
        ControlPolicy {
            flow_control_command: None,
            implicit_attach_reply: true,
            tagged_blocks: true,
            command_list_single_reply: Self::COMMAND_LIST_SINGLE_REPLY,
            // RMUX 0.10.0 refuses `refresh-client -B`.
            subscriptions: false,
            status_poll: (transport.is_remote() && Self::HOOK_STATUS)
                .then(|| Self::hook_poll_command(session)),
            confirm_close: None,
        }
    }

    const HOOK_STATUS: bool = true;
}

fn rmux_answered_absent(answer: &str) -> Option<Missing> {
    tmux_answered_absent(answer).or_else(|| {
        answer
            .strip_prefix("session not found: ")
            .filter(|session| !session.is_empty())
            .map(|_| Missing::Server)
    })
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

    #[cfg(unix)]
    #[test]
    fn rmuxs_own_absence_replies_read_as_absence() {
        crate::tmux_compat::server::assert_absence_replies::<Rmux>(&[
            (
                "error connecting to /tmp/rmux-1000/thurbox (No such file or directory)",
                true,
            ),
            ("can't find session: thurbox", true),
            (
                "invalid target 'thurbox': can't find session: thurbox",
                true,
            ),
            ("can't find pane: %99", false),
            ("invalid target '%99': can't find pane: %99", false),
            ("can't find window: =tb-nope", false),
            (
                "invalid target 'tb-nope': can't find window: tb-nope",
                false,
            ),
        ]);
    }
}
