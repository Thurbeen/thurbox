//! `runtime-mux <verb>`: the runtime protocol's program (docs/runtime/README.md).
//! Reads one JSON request on stdin and prints one answer on stdout. The
//! composition root: it alone builds the backend, on the program's own tmux
//! socket ([`runtime_mux::protocol::SOCKET`]).
//!
//! `runtime-mux view [--socket S] [--tmux-tmpdir D] [--scrollback N]
//! <key | --pane P>` is the
//! other shape: one key's pane, or a pane by its id, as a terminal stream on
//! stdin and stdout, for as long as the view stays. `attach` answers with it;
//! thurbox's stream panes name the pane.

use std::io::Read;

use runtime_mux::protocol::{self, SOCKET, SOCKET_ENV};
use runtime_mux::MuxSettings;

fn main() {
    let verb = std::env::args().nth(1).unwrap_or_default();
    if verb == "view" {
        if let Err(why) = view(std::env::args().skip(2).collect()) {
            eprintln!("runtime-mux view: {why}");
            std::process::exit(1);
        }
        return;
    }
    let mut request = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut request) {
        println!(
            "{}",
            serde_json::json!({ "unknown": { "why": format!("cannot read the request: {e}") } })
        );
        return;
    }
    let socket = std::env::var(SOCKET_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| SOCKET.to_string());
    // A start looks the key up and then makes its window, so two at once
    // could each make one; a stop between them could remove the one the
    // other just adopted. Both run one at a time per server.
    let mut _turn = None;
    if matches!(verb.as_str(), "start" | "stop") {
        match lock(&socket) {
            Ok(file) => _turn = Some(file),
            Err(e) => {
                println!(
                    "{}",
                    serde_json::json!({ "unknown": { "why": format!("cannot take the start lock: {e}") } })
                );
                return;
            }
        }
    }
    let settings = MuxSettings {
        socket: Some(socket),
        ..MuxSettings::default()
    };
    let registry = runtime_mux::wiring::local_only(&settings);
    let answer = protocol::serve(
        registry.default_backend().as_ref(),
        &verb,
        &request,
        &view_argv(&settings),
    );
    registry.shutdown_all();
    println!("{answer}");
}

/// How a view reaches this server: this program's `view`, naming the socket
/// and the directory tmux keeps it in, as the view runs with the viewer's
/// environment.
fn view_argv(settings: &MuxSettings) -> Vec<String> {
    let program = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "runtime-mux".into());
    let mut argv = vec![program, "view".into()];
    if let Some(socket) = &settings.socket {
        argv.extend(["--socket".into(), socket.clone()]);
    }
    if let Some(dir) = std::env::var_os("TMUX_TMPDIR").filter(|d| !d.is_empty()) {
        argv.extend(["--tmux-tmpdir".into(), dir.to_string_lossy().into_owned()]);
    }
    argv
}

/// `view`: the key's pane as a terminal stream until the view lets go.
fn view(args: Vec<String>) -> Result<(), String> {
    let mut socket = SOCKET.to_string();
    let mut key = None;
    let mut pane = None;
    let mut scrollback = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = args.next().ok_or("--socket takes a name")?,
            "--pane" => pane = Some(args.next().ok_or("--pane takes a pane id")?),
            // The history a snapshot carries: the viewer's own setting.
            "--scrollback" => {
                let lines = args.next().ok_or("--scrollback takes a line count")?;
                scrollback = Some(
                    lines
                        .parse()
                        .map_err(|e| format!("--scrollback {lines}: {e}"))?,
                );
            }
            "--tmux-tmpdir" => {
                let dir = args.next().ok_or("--tmux-tmpdir takes a directory")?;
                // Before any thread starts: tmux finds the socket there.
                std::env::set_var("TMUX_TMPDIR", dir);
            }
            _ if key.is_none() => key = Some(arg),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let defaults = MuxSettings::default();
    let settings = MuxSettings {
        socket: Some(socket),
        scrollback_lines: scrollback.unwrap_or(defaults.scrollback_lines),
        ..defaults
    };
    let registry = runtime_mux::wiring::local_only(&settings);
    let backend = registry.default_backend().clone();
    let pane = match (pane, key) {
        (Some(pane), None) => pane,
        (None, Some(key)) => protocol::pane_of(backend.as_ref(), &key)?
            .ok_or_else(|| format!("no process holds key {key}"))?,
        _ => {
            return Err(
                "usage: runtime-mux view [--socket S] [--tmux-tmpdir D] [--scrollback N] <key | --pane P>".into(),
            )
        }
    };
    backend.ensure_ready().map_err(|e| e.to_string())?;
    let served = runtime_mux::view::serve(backend, &pane, std::io::stdin(), &mut std::io::stdout());
    registry.shutdown_all();
    served
}

/// The lock serializing starts and stops on `socket`'s server, held until
/// the returned file is dropped. It sits in the private directory tmux keeps
/// its sockets in, `<TMUX_TMPDIR or /tmp>/tmux-<uid>` and never under
/// `TMPDIR`, so every caller reaching one server takes one lock, a server in
/// another directory has its own, and no other user can plant a link at its
/// name. The directory is made `0700` when missing and refused, as tmux
/// refuses it, when another user owns it or can write to it.
#[cfg(unix)]
fn lock(socket: &str) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    let base = std::env::var_os("TMUX_TMPDIR")
        .filter(|d| !d.is_empty())
        .map_or_else(
            || std::path::PathBuf::from("/tmp"),
            std::path::PathBuf::from,
        );
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let dir = base.join(format!("tmux-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    let meta = std::fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return Err(std::io::Error::other(format!(
            "{} is not a directory only this user can write to",
            dir.display()
        )));
    }
    // Appending never truncates, and a link at the name is refused.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(format!("runtime-mux-{socket}.lock")))?;
    file.lock()?;
    Ok(file)
}

/// The lock on a host without tmux's private socket directory.
#[cfg(not(unix))]
fn lock(socket: &str) -> std::io::Result<std::fs::File> {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::temp_dir().join(format!("runtime-mux-{user}-{socket}.lock")))?;
    file.lock()?;
    Ok(file)
}
