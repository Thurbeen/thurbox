use std::sync::Arc;

use anyhow::{anyhow, bail, Result};

use crate::contract::SessionBackend;
use crate::settings::MuxSettings;
use crate::shell::HostLauncher;
use crate::tmux_compat::control_mode::ControlPolicy;
use crate::tmux_compat::server::{parse_version, ConfigOption, Server, TmuxCompatible};
use crate::tmux_compat::transport::TmuxTransport;
use crate::{HostDef, Multiplexer, Platform};

pub struct Tmux;

pub type TmuxBackend = Server<Tmux>;

pub fn local(settings: &MuxSettings) -> Arc<dyn SessionBackend> {
    Arc::new(TmuxBackend::local(settings))
}

pub fn on_host(
    host: &HostDef,
    launcher: HostLauncher,
    platform: Platform,
    settings: &MuxSettings,
) -> Arc<dyn SessionBackend> {
    Arc::new(TmuxBackend::on_host(host, launcher, platform, settings))
}

impl TmuxCompatible for Tmux {
    const MULTIPLEXER: Multiplexer = Multiplexer::Tmux;
    const WINDOW_OPTIONS: bool = true;
    const WINDOW_SETTINGS: bool = true;
    const WINDOW_EVENTS: bool = true;
    const PANE_MONITORING: bool = true;
    const SNAPSHOTS: bool = true;
    const CURSOR_ROW_CAPTURE: bool = true;
    const COMMAND_LISTS: bool = true;
    const COMMAND_LIST_SINGLE_REPLY: bool = false;
    const ONE_SHOT_SPAWN_ANSWERS: bool = true;
    const CONDITIONAL_RESIZE: bool = true;
    const SERVER_SCOPE: &str = "-s";
    const SOCKET_DIR: bool = true;
    const DISPLAY_FLAGS: &[&str] = &[PANE_STATE_UTF8_FLAG];
    const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>> = None;

    fn check_banner(banner: &str, _socket: &str) -> Result<()> {
        if banner
            .lines()
            .any(|line| line.trim_start().starts_with("psmux "))
        {
            bail!(
                "this `tmux` is psmux ({}); choose the psmux multiplexer for it",
                banner.trim().replace('\n', ", ")
            );
        }
        check_min_version(banner)
    }

    fn session_config(session: &str) -> Vec<ConfigOption> {
        let mut config = Vec::new();
        // `set-clipboard external` stops tmux answering an app's OSC 52 read from 3.2 up; `get-clipboard off` only from 3.7.
        config.push(ConfigOption::set(
            &["-s", "set-clipboard", "external"],
            false,
        ));

        // Apps inside tmux read the `mouse` option before they request mouse reports.
        config.push(ConfigOption::set(&["-t", session, "mouse", "on"], true));

        // tmux 3.2 expands `#{terminal-features}` to "" but reads one index, and `-a` fills the first free index.
        let slot = format!("#{{{CLIPBOARD_FEATURE_SLOT}}}");
        let write = format!("set-option -qs {CLIPBOARD_FEATURE_SLOT} *:clipboard");
        config.push(ConfigOption {
            args: vec!["if-shell".into(), "-F".into(), slot, String::new(), write],
            fatal: false,
        });
        config
    }

    fn control_policy(_transport: &TmuxTransport, _session: &str) -> ControlPolicy {
        ControlPolicy {
            flow_control_command: Some("refresh-client -f pause-after=5"),
            implicit_attach_reply: true,
            tagged_blocks: true,
            command_list_single_reply: Self::COMMAND_LIST_SINGLE_REPLY,
            subscriptions: true,
            status_poll: None,
            confirm_close: None,
        }
    }

    const HOOK_STATUS: bool = true;
}

const PANE_STATE_UTF8_FLAG: &str = "-u";

const CLIPBOARD_FEATURE_SLOT: &str = "terminal-features[100]";

const MIN_TMUX_VERSION: (u32, u32) = (3, 2);

fn check_min_version(version_output: &str) -> Result<()> {
    let Some(version) = Tmux::banner_version(version_output) else {
        return Ok(());
    };
    let (major, minor, _) = parse_version(version)
        .ok_or_else(|| anyhow!("Cannot parse tmux version from: {version_output}"))?;
    if (major, minor) < MIN_TMUX_VERSION {
        bail!(
            "tmux {major}.{minor} is too old; thurbox requires >= {}.{}",
            MIN_TMUX_VERSION.0,
            MIN_TMUX_VERSION.1
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StatusChannel;

    #[test]
    fn a_wsl_place_window_gets_the_signal_file_and_the_place_path() {
        let host = crate::HostDef {
            name: "wsl-window-line".into(),
            kind: crate::HostKind::Wsl,
            distro: Some("Ubuntu".into()),
            path_prepend: vec!["/opt/fleet/bin".into()],
            ..Default::default()
        };
        let backend = TmuxBackend::on_host(
            &host,
            crate::shell::HostLauncher::for_host(&host),
            crate::Platform::Posix,
            &crate::settings::for_tests(),
        );
        let env = std::collections::HashMap::from([(
            "FLEET_SIGNAL_FILE".to_string(),
            "~/.local/state/fleet/signals/t1.u1.jsonl".to_string(),
        )]);
        let line = backend
            .new_window_line(
                "tb-k",
                "claude",
                &["--resume".into(), "c1".into()],
                None,
                &env,
            )
            .expect("line");
        assert!(
            line.contains(" -e 'FLEET_SIGNAL_FILE=~/.local/state/fleet/signals/t1.u1.jsonl' "),
            "{line}"
        );
        assert!(
            line.ends_with(
                " /bin/sh -lc 'PATH=/opt/fleet/bin\"${PATH:+:$PATH}\"; export PATH; exec claude --resume c1'"
            ),
            "{line}"
        );
        let argv: Vec<String> = backend
            .mux_command(&["-C", "attach"])
            .expect("command")
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let enter: &[&str] = if cfg!(unix) {
            &["-d", "Ubuntu", "--cd", "/", "-e", "sh"]
        } else {
            &["-d", "Ubuntu", "-e", "sh"]
        };
        assert_eq!(argv[..enter.len()], *enter, "{argv:?}");
        assert_eq!(
            argv[argv.len() - 5..],
            ["tmux", "-L", "thurbox-dev", "-C", "attach"]
        );
    }

    #[test]
    fn a_tmux_banner_names_its_version() {
        assert_eq!(Tmux::banner_version("tmux 3.4\n"), Some("3.4"));
        assert_eq!(parse_version("3.4"), Some((3, 4, 0)));
        assert_eq!(parse_version("3.3a"), Some((3, 3, 0)));
        assert_eq!(parse_version("not a version"), None);
        assert!(check_min_version("tmux next").is_err());
    }

    #[test]
    fn a_psmux_answering_as_tmux_is_refused() {
        let err = Tmux::check_banner("tmux 3.3.8\npsmux 3.3.8 (66cf613 2026-08-18)\n", "thurbox")
            .unwrap_err()
            .to_string();
        assert!(err.contains("psmux"), "{err}");
        assert!(Tmux::check_banner("tmux 3.5a\n", "thurbox").is_ok());
    }

    #[test]
    fn min_version_accepts_recent_tmux() {
        assert!(check_min_version("tmux 3.4").is_ok());
        assert!(check_min_version("tmux 3.2").is_ok());
    }

    #[test]
    fn min_version_rejects_old_tmux() {
        assert!(check_min_version("tmux 2.8").is_err());
    }

    #[test]
    fn min_version_accepts_non_tmux_clone() {
        // psmux numbers its own versions and may print no `tmux ` banner.
        assert!(check_min_version("psmux 0.3.1").is_ok());
        assert!(check_min_version("psmux 1.0").is_ok());
        assert!(check_min_version("pmux 0.1").is_ok());
    }

    #[test]
    fn local_backend_is_named_by_its_local_route_with_local_transport() {
        let backend = TmuxBackend::local(&crate::settings::for_tests());
        assert_eq!(
            backend.name(),
            "local:tmux",
            "on this machine, whatever OS it is"
        );
        assert!(!backend.transport.is_remote());
    }

    #[test]
    fn a_backend_for_a_route_runs_that_routes_binary() {
        let host = crate::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            multiplexer: Some("rmux".into()),
            ..Default::default()
        };
        let backend = TmuxBackend::for_host(&host, &crate::settings::for_tests());
        assert_eq!(backend.name(), "ssh:devbox:tmux");
        assert_eq!(backend.transport.mux(), "tmux");
        assert_eq!(backend.host.as_ref(), Some(&host));
    }

    fn windows_host(mux: &str) -> HostDef {
        HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some(mux.into()),
            platform: Some(Platform::Windows),
            ..Default::default()
        }
    }

    #[test]
    fn liveness_polling_follows_close_events_not_the_build_os() {
        use crate::shell::simulate_local;
        for local in Platform::ALL {
            simulate_local(local, || {
                assert!(
                    !TmuxBackend::local(&crate::settings::for_tests())
                        .capabilities()
                        .needs_liveness_poll,
                    "local tmux on simulated {local:?}"
                );
                assert!(
                    !TmuxBackend::for_host(&windows_host("tmux"), &crate::settings::for_tests())
                        .capabilities()
                        .needs_liveness_poll
                );
                assert!(
                    !TmuxBackend::for_host(&windows_host("psmux"), &crate::settings::for_tests())
                        .capabilities()
                        .needs_liveness_poll,
                    "tmux on a host that prefers psmux is still tmux"
                );
                assert!(
                    !TmuxBackend::for_host(&HostDef::wsl("Ubuntu"), &crate::settings::for_tests())
                        .capabilities()
                        .needs_liveness_poll
                );
            });
        }
    }

    #[test]
    fn a_posix_hosts_socket_goes_in_its_data_dir_and_a_windows_hosts_does_not() {
        use crate::tmux_compat::transport::SocketDir;
        let devbox = HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        assert!(matches!(
            TmuxBackend::for_host(&devbox, &crate::settings::for_tests())
                .transport
                .socket_dir(),
            SocketDir::HostData { .. }
        ));
        assert_eq!(
            TmuxBackend::for_host(&windows_host("tmux"), &crate::settings::for_tests())
                .socket_dir(),
            SocketDir::Inherited
        );
    }

    fn buffer_of(argv: &[String]) -> &str {
        let at = argv.iter().position(|a| a == "-b").expect("a named buffer");
        &argv[at + 1]
    }

    #[test]
    fn a_paste_goes_into_a_buffer_and_reaches_the_pane_in_one_write() {
        let argvs = Tmux::paste_args("thurbox:tb-demo", "a\n\nb");
        assert_eq!(argvs.len(), 2, "{argvs:?}");
        let buffer = buffer_of(&argvs[0]).to_string();
        assert_eq!(
            argvs[0],
            [
                "set-buffer",
                "-b",
                &buffer,
                "--",
                "\x1b[200~a\n\nb\x1b[201~"
            ]
        );
        assert_eq!(
            argvs[1],
            [
                "paste-buffer",
                "-d",
                "-r",
                "-S",
                "-b",
                &buffer,
                "-t",
                "thurbox:tb-demo"
            ]
        );
        assert_ne!(
            buffer,
            buffer_of(&Tmux::paste_args("%3", "a")[0]),
            "each paste has a buffer of its own"
        );
    }

    #[test]
    fn a_long_paste_is_set_in_pieces_and_pasted_once() {
        let text = "x".repeat(2000);
        let argvs = Tmux::paste_args("%3", &text);
        let (paste, sets) = argvs.split_last().expect("commands");
        assert!(sets.len() > 1, "{}", sets.len());
        assert_eq!(paste[0], "paste-buffer");
        let buffer = buffer_of(paste);
        let mut set = String::new();
        for (i, argv) in sets.iter().enumerate() {
            assert_eq!(argv[0], "set-buffer");
            assert_eq!(argv.contains(&"-a".to_string()), i > 0, "{argv:?}");
            assert_eq!(buffer_of(argv), buffer);
            assert!(!argv.contains(&"-t".to_string()), "{argv:?}");
            set.push_str(argv.last().expect("the piece"));
        }
        assert_eq!(set, format!("\x1b[200~{text}\x1b[201~"));
    }

    #[test]
    fn no_piece_of_a_paste_ends_in_a_semicolon() {
        for text in [
            format!("{};b", "a".repeat(505)),
            format!("{}{}b", "a".repeat(500), ";".repeat(20)),
            ";".repeat(1500),
            format!("{}é;;é", "a".repeat(504)),
        ] {
            let argvs = Tmux::paste_args("%3", &text);
            let (_, sets) = argvs.split_last().expect("commands");
            let pieces: Vec<&String> = sets.iter().map(|a| a.last().expect("piece")).collect();
            assert!(
                pieces.iter().all(|p| !p.ends_with(';') && !p.is_empty()),
                "{pieces:?}"
            );
            let joined: String = pieces.iter().map(|p| p.as_str()).collect();
            assert_eq!(joined, format!("\x1b[200~{text}\x1b[201~"));
        }
    }

    #[test]
    fn a_deferred_prompt_names_the_servers_own_mux_and_socket() {
        let tmux = Tmux::deferred_paste_script("tmux", "sock", "%3", "it's\nhere").unwrap();
        assert!(tmux.starts_with("tmux -L sock send-keys -t "), "{tmux}");
        assert!(
            tmux.ends_with("tmux -L sock send-keys -t '%3' Enter"),
            "{tmux}"
        );
    }

    #[test]
    fn a_deferred_prompt_quotes_a_socket_name_the_host_configured() {
        let tmux = Tmux::deferred_paste_script("tmux", "my sock", "%3", "hi").unwrap();
        assert_eq!(tmux.matches("-L 'my sock' send-keys").count(), 2, "{tmux}");
    }

    #[test]
    fn tmux_is_read_as_stamping_its_windows() {
        let winbox = crate::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        assert!(
            TmuxBackend::for_host(&winbox, &crate::settings::for_tests()).stamps_are_per_window()
        );
        assert!(TmuxBackend::local(&crate::settings::for_tests()).stamps_are_per_window());
    }

    #[test]
    fn the_hook_command_needs_no_socket_or_pane() {
        let command = TmuxBackend::local(&crate::settings::for_tests())
            .hook_signal_command()
            .expect("tmux has a status channel");
        assert_eq!(command, "tmux set-option -p @thurbox_state ");
    }

    #[cfg(unix)]
    #[test]
    fn tmuxs_own_absence_replies_read_as_absence() {
        crate::tmux_compat::server::assert_absence_replies::<Tmux>(&[
            (
                "error connecting to /tmp/tmux-1000/thurbox (No such file or directory)",
                true,
            ),
            ("no server running on /tmp/tmux-1000/thurbox", true),
            ("can't find session: thurbox", true),
            ("can't find pane: %99", false),
            ("can't find window: tb-nope", false),
        ]);
    }
}
