//! The tmux adapter: tmux's answers to [`TmuxCompatible`].
//!
//! What tmux shares with the other multiplexers that speak its protocol is
//! [`Server`], and tmux's own wording is [`TmuxCompatible`]'s defaults — its
//! `-H` key encoding, its bracketed paste, its absence replies. Every way tmux
//! differs from the others is a body here — its clipboard and mouse options,
//! its UTF-8 flag, its version floor. Nothing here is psmux's, and nothing in
//! the shared code asks whether a server is tmux.

use std::sync::Arc;

use anyhow::{anyhow, bail, Result};

use crate::contract::SessionBackend;
use crate::settings::MuxSettings;
use crate::shell::HostLauncher;
use crate::tmux_compat::control_mode::ControlPolicy;
use crate::tmux_compat::server::{parse_version, ConfigOption, Server, TmuxCompatible};
use crate::tmux_compat::transport::TmuxTransport;
use crate::{HostDef, Multiplexer, Platform};

/// tmux.
pub struct Tmux;

/// A tmux server's session backend.
pub type TmuxBackend = Server<Tmux>;

/// tmux on this machine, whatever OS it is.
pub fn local(settings: &MuxSettings) -> Arc<dyn SessionBackend> {
    Arc::new(TmuxBackend::local(settings))
}

/// tmux on `host`, reached through `launcher` on a machine of `platform`.
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

    /// The tmux floor, and never psmux under tmux's name: psmux installs a
    /// `tmux` alias, and from 3.3.7 its banner says what it is. Driven as tmux
    /// it is told `-s` and `-H` and waited on for an attach reply it never
    /// sends. A psmux 3.3.6 prints only `tmux 3.3.6` and cannot be told apart
    /// here.
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
        // An app's OSC 52 copy does not need tmux: control mode hands thurbox
        // the raw bytes in `%output` whatever `set-clipboard` says, and thurbox
        // puts the focused pane's write on the user's clipboard itself
        // (`TermSignals::copy_to_clipboard`). tmux never sends a control-mode
        // client a selection, so `set-clipboard on` never delivered one here.
        //
        // What `on` did do was make tmux parse the write: keep it as a paste
        // buffer, and answer every app's OSC 52 *read* (`?`) with the newest
        // one — so an app in one session could read what an app in another had
        // copied, local or on a remote host. `get-clipboard off` stops the
        // answer only on tmux 3.7+; `external` stops tmux handling an app's
        // OSC 52 at all on every tmux from the 3.2 floor up, and still
        // forwards tmux's own copy-mode yanks to a terminal attached directly
        // (which is what the `*:clipboard` feature below is for).
        config.push(ConfigOption::set(
            &["-s", "set-clipboard", "external"],
            false,
        ));

        // Apps inside tmux can inspect this option before deciding whether to
        // request mouse reports. With it off, a full-screen app may leave wheel
        // capture disabled even though thurbox can forward those reports.
        config.push(ConfigOption::set(&["-t", session, "mouse", "on"], true));

        // The `*:clipboard` feature goes into a fixed slot, and only while that
        // slot is empty. Appending it grew the list by one entry a run, since
        // this runs on every spawn and the server outlives thurbox (#1278); an
        // unconditional write to the slot would overwrite an entry the user's
        // `~/.tmux.conf` put there. Reading the list from Rust first would cost
        // a process per session create, and a format cannot test the whole
        // array on 3.2 (`#{terminal-features}` expands to "") — but it can read
        // one index. `-a` fills the first free index, so appended entries
        // never land on this one.
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

/// tmux's `-u` — "assume the terminal supports UTF-8".
///
/// tmux decides a client speaks UTF-8 from `LC_ALL`/`LC_CTYPE`/`LANG`, and
/// sanitizes what it prints for one that does not: every control byte becomes
/// `_`, the pane-state separator included. Under `LC_ALL=C` or no locale at all — a
/// systemd unit, a cron job, most containers — the whole answer then parses as
/// one field and every pane-state field reports null. `-u` sets the flag
/// outright, so the separator survives whatever the environment says.
const PANE_STATE_UTF8_FLAG: &str = "-u";

/// The `terminal-features` slot thurbox writes `*:clipboard` into — see
/// `session_config`. High enough that neither tmux's defaults nor a
/// hand-appended list reaches it.
const CLIPBOARD_FEATURE_SLOT: &str = "terminal-features[100]";

/// Minimum tmux version required.
const MIN_TMUX_VERSION: (u32, u32) = (3, 2);

/// Enforce the minimum-version gate against a multiplexer's `-V` output.
///
/// The `>= 3.2` floor applies to a `tmux …` banner; any other banner from a
/// binary that answered `-V` as tmux is accepted as-is.
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

    /// A session on a WSL place gets the file its hooks report to and the
    /// place's `PATH` through the `new-window` line itself: `-e` sets the
    /// variable in the window, the login wrap assigns the `PATH`, and the
    /// line travels over the control connection that `wsl.exe` launches, so
    /// nothing depends on what `wsl.exe` passes of this machine's
    /// environment (`WSLENV`).
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
        // A test reads no place's login `PATH`: `path_prepend` leads the
        // inherited one.
        assert!(
            line.ends_with(
                " /bin/sh -lc 'PATH=/opt/fleet/bin\"${PATH:+:$PATH}\"; export PATH; exec claude --resume c1'"
            ),
            "{line}"
        );
        // The connection the line travels over enters the distro.
        let argv: Vec<String> = backend
            .mux_command(&["-C", "attach"])
            .expect("command")
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // `--cd /` only from a Unix machine (`shell::wsl_command`).
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

    /// psmux installs a `tmux` alias, so the tmux adapter on a Windows
    /// machine can reach psmux under tmux's name — and drive it with tmux's
    /// grammar (`-s`, `-H`, an attach reply psmux never sends: the #1168 hang).
    /// A banner that says psmux is refused instead.
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
        // psmux numbers itself independently and may not print a `tmux ` banner;
        // once it answers `-V` it is accepted regardless of its own version.
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

    /// A route's multiplexer is the binary, whatever the host now prefers,
    /// and the host itself stays as configured — its platform is not read off
    /// the route.
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

    /// Whether a backend polls for dead panes is what its multiplexer can
    /// report — tmux announces `%window-close`, psmux does not — not the OS
    /// thurbox was built for, nor the host's.
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

    /// tmux's socket directory is thurbox's to choose on a POSIX machine —
    /// a host's own data dir, resolved there — and not on a Windows one.
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

    /// The text of a long paste is set in pieces, and only the last command
    /// names the pane: an app that times its input sees one paste, not one
    /// per piece with a tmux launch between them (#1901).
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

    /// tmux ends a command at an argument's trailing `;` and drops it, so no
    /// piece but the last ends in one.
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
        // A socket the immediate commands pass as one argument must reach the
        // server's shell as one word too, or the paste runs against no server.
        let tmux = Tmux::deferred_paste_script("tmux", "my sock", "%3", "hi").unwrap();
        assert_eq!(tmux.matches("-L 'my sock' send-keys").count(), 2, "{tmux}");
    }

    /// tmux's `#{@...}` is a window's own, so a listing's stamps are read — on
    /// this machine whatever OS it is, and on a host whatever it prefers.
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

    /// Inside a pane tmux resolves its own server and pane, so the command a
    /// hook runs names neither, and splices into a JSON string as it stands.
    #[test]
    fn the_hook_command_needs_no_socket_or_pane() {
        let command = TmuxBackend::local(&crate::settings::for_tests())
            .hook_signal_command()
            .expect("tmux has a status channel");
        assert_eq!(command, "tmux set-option -p @thurbox_state ");
    }

    /// tmux 3.8's own replies, captured on a scratch socket.
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
