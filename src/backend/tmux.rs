//! The tmux adapter: tmux's answers to [`TmuxCompatible`].
//!
//! What tmux shares with the other multiplexers that speak its protocol is
//! [`Server`]; every way tmux differs from them is a body here — its `-H` key
//! encoding, its bracketed paste, its clipboard and mouse options, its version
//! floor. Nothing here is psmux's, and nothing in the shared code asks whether
//! a server is tmux.

use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::backend::contract::SessionBackend;
use crate::backend::tmux_compat::control_mode::{
    hex_send_keys_commands, shell_escape, ControlPolicy, PaneInput,
};
use crate::backend::tmux_compat::server::{ConfigOption, Server, TmuxCompatible};
use crate::backend::tmux_compat::transport::TmuxTransport;
use crate::session::{HostDef, Multiplexer, Platform};
use crate::shell::HostLauncher;

/// tmux.
pub struct Tmux;

/// A tmux server's session backend.
pub type TmuxBackend = Server<Tmux>;

/// tmux on this machine, whatever OS it is.
pub fn local() -> Arc<dyn SessionBackend> {
    Arc::new(TmuxBackend::local())
}

/// tmux on `host`, reached through `launcher` on a machine of `platform`.
pub fn on_host(
    host: &HostDef,
    launcher: HostLauncher,
    platform: Platform,
) -> Arc<dyn SessionBackend> {
    Arc::new(TmuxBackend::on_host(host, launcher, platform))
}

impl TmuxCompatible for Tmux {
    const MULTIPLEXER: Multiplexer = Multiplexer::Tmux;
    const WINDOW_OPTIONS: bool = true;
    const WINDOW_SETTINGS: bool = true;
    const WINDOW_EVENTS: bool = true;
    const PANE_MONITORING: bool = true;
    const SNAPSHOTS: bool = true;
    const COMMAND_LISTS: bool = true;
    const COMMAND_LIST_SINGLE_REPLY: bool = false;
    const ONE_SHOT_SPAWN_ANSWERS: bool = true;
    const CONDITIONAL_RESIZE: bool = true;
    const SERVER_SCOPE: &str = "-s";
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

    /// The bracketed-paste-wrapped bytes, taken literally (`send-keys -l`).
    fn paste_args(target: &str, text: &str) -> Vec<String> {
        vec![
            "send-keys".to_string(),
            "-t".to_string(),
            target.to_string(),
            "-l".to_string(),
            bracketed_paste(text),
        ]
    }

    fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> String {
        deferred_paste_script(mux, socket, target, text)
    }

    fn pane_input(_transport: &TmuxTransport, _socket: &str) -> Arc<dyn PaneInput> {
        Arc::new(HexKeys)
    }

    fn control_policy(_transport: &TmuxTransport, _session: &str) -> ControlPolicy {
        ControlPolicy {
            flow_control: true,
            implicit_attach_reply: true,
            tagged_blocks: true,
            subscriptions: true,
            status_poll: None,
        }
    }

    const HOOK_STATUS: bool = true;

    /// Inside a pane tmux finds its own server and pane from `$TMUX` and
    /// `$TMUX_PANE`, so the command names neither — and works whichever
    /// socket the pane's server is on.
    fn hook_signal_command(_server: &Server<Self>) -> String {
        format!(
            "{} set-option -p {} ",
            Self::MULTIPLEXER.name(),
            crate::backend::tmux_compat::control_mode::REMOTE_HOOK_STATE_OPTION
        )
    }
}

/// tmux's input: every keystroke as `send-keys -H` hex. A paste takes no other
/// way in: the writer pastes it through control mode (`set-buffer` and
/// `paste-buffer -p`), so tmux frames it only for an app that asked.
struct HexKeys;

impl PaneInput for HexKeys {
    fn send_keys(&self, pane_id: &str, buf: &[u8]) -> Vec<String> {
        hex_send_keys_commands(pane_id, buf)
    }

    fn paste(&self, _pane_id: &str, _text: &str) -> Option<Result<()>> {
        None
    }
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

/// Parse a `tmux -V` version string (e.g. `"tmux 3.4"`, `"tmux 3.3a"`) into a
/// `(major, minor)` pair.
fn parse_tmux_version(version_str: &str) -> Result<(u32, u32)> {
    let version_part = version_str.strip_prefix("tmux ").unwrap_or(version_str);

    let parts: Vec<&str> = version_part.split('.').collect();
    if parts.len() < 2 {
        bail!("Cannot parse tmux version from: {version_str}");
    }

    let major: u32 = parts[0]
        .parse()
        .with_context(|| format!("Cannot parse tmux major version from: {version_str}"))?;
    // Minor might have a trailing letter (e.g., "3a"), strip non-digits.
    let minor_str: String = parts[1].chars().take_while(char::is_ascii_digit).collect();
    let minor: u32 = minor_str
        .parse()
        .with_context(|| format!("Cannot parse tmux minor version from: {version_str}"))?;

    Ok((major, minor))
}

/// Enforce the minimum-version gate against a multiplexer's `-V` output.
///
/// The `>= 3.2` floor applies to a `tmux …` banner; any other banner from a
/// binary that answered `-V` as tmux is accepted as-is.
fn check_min_version(version_output: &str) -> Result<()> {
    let trimmed = version_output.trim();
    if let Some(rest) = trimmed.strip_prefix("tmux ") {
        let (major, minor) = parse_tmux_version(rest)?;
        if (major, minor) < MIN_TMUX_VERSION {
            bail!(
                "tmux {major}.{minor} is too old; thurbox requires >= {}.{}",
                MIN_TMUX_VERSION.0,
                MIN_TMUX_VERSION.1
            );
        }
    }
    Ok(())
}

/// Wrap `text` in the bracketed-paste escape sequences (`ESC[200~ … ESC[201~`)
/// so a multi-line prompt is delivered as a single paste — the embedded
/// newlines insert as text instead of submitting the prompt on the first one.
/// Used by [`SessionBackend::send_text`](crate::backend::SessionBackend::send_text), which is how the TUI reaches this too
/// — the kernel's prompt commands call it rather than framing the paste
/// themselves. The trailing `Enter` is sent separately. tmux delivers
/// these bytes literally via `send-keys -l`.
fn bracketed_paste(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

/// The `run-shell` script that pastes the prompt, waits a beat so the paste is
/// consumed, then presses Enter — a plain `sh` one-liner, which is what tmux's
/// `run-shell` executes it through.
fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> String {
    // Quoted as the immediate commands pass them — one argument each — since a
    // host may configure a socket name with a space in it.
    let (mux, socket) = (shell_escape(mux), shell_escape(socket));
    let escaped_target = shell_escape(target);
    // Bracketed-paste wrap (see `bracketed_paste`) so multi-line prompts don't
    // submit early; `-l` makes the multiplexer deliver the bytes literally.
    let escaped_text = shell_escape(&bracketed_paste(text));
    format!(
        "{mux} -L {socket} send-keys -t {escaped_target} -l {escaped_text}; \
         sleep 0.2; \
         {mux} -L {socket} send-keys -t {escaped_target} Enter"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tmux_version_plain() {
        assert_eq!(parse_tmux_version("tmux 3.4").unwrap(), (3, 4));
    }

    #[test]
    fn parse_tmux_version_trailing_letter() {
        assert_eq!(parse_tmux_version("tmux 3.3a").unwrap(), (3, 3));
    }

    #[test]
    fn parse_tmux_version_without_prefix() {
        assert_eq!(parse_tmux_version("3.2").unwrap(), (3, 2));
    }

    #[test]
    fn parse_tmux_version_rejects_garbage() {
        assert!(parse_tmux_version("not a version").is_err());
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
        let backend = TmuxBackend::new();
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
        let host = crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            multiplexer: Some("rmux".into()),
            ..Default::default()
        };
        let backend = TmuxBackend::for_host(&host);
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
        use crate::session::platform::simulate_local;
        for local in Platform::ALL {
            simulate_local(local, || {
                assert!(
                    !TmuxBackend::local().needs_liveness_poll(),
                    "local tmux on simulated {local:?}"
                );
                assert!(!TmuxBackend::for_host(&windows_host("tmux")).needs_liveness_poll());
                assert!(
                    !TmuxBackend::for_host(&windows_host("psmux")).needs_liveness_poll(),
                    "tmux on a host that prefers psmux is still tmux"
                );
                assert!(!TmuxBackend::for_host(&HostDef::wsl("Ubuntu")).needs_liveness_poll());
            });
        }
    }

    #[test]
    fn paste_prompt_args_wraps_literally_for_tmux() {
        assert_eq!(
            Tmux::paste_args("thurbox:tb-demo", "line one\nline two"),
            vec![
                "send-keys",
                "-t",
                "thurbox:tb-demo",
                "-l",
                "\x1b[200~line one\nline two\x1b[201~",
            ]
        );
    }

    #[test]
    fn a_deferred_prompt_names_the_servers_own_mux_and_socket() {
        let tmux = deferred_paste_script("tmux", "sock", "%3", "it's\nhere");
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
        let tmux = deferred_paste_script("tmux", "my sock", "%3", "hi");
        assert_eq!(tmux.matches("-L 'my sock' send-keys").count(), 2, "{tmux}");
    }

    /// tmux's `#{@...}` is a window's own, so a listing's stamps are read — on
    /// this machine whatever OS it is, and on a host whatever it prefers.
    #[test]
    fn tmux_is_read_as_stamping_its_windows() {
        let winbox = crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        assert!(TmuxBackend::for_host(&winbox).stamps_are_per_window());
        assert!(TmuxBackend::local().stamps_are_per_window());
    }

    /// Inside a pane tmux resolves its own server and pane, so the command a
    /// hook runs names neither, and splices into a JSON string as it stands.
    #[test]
    fn the_hook_command_needs_no_socket_or_pane() {
        let command = TmuxBackend::local()
            .hook_signal_command()
            .expect("tmux has a status channel");
        assert_eq!(command, "tmux set-option -p @thurbox_state ");
    }
}
