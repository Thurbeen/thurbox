//! `runtime-mux <verb>`: the runtime protocol's program (docs/runtime/README.md).
//! Reads one JSON request on stdin and prints one answer on stdout. The
//! composition root: it alone builds the backend, on the program's own tmux
//! socket ([`runtime_mux::protocol::SOCKET`]).

use std::io::Read;

use runtime_mux::protocol::{self, SOCKET, SOCKET_ENV};
use runtime_mux::MuxSettings;

fn main() {
    let verb = std::env::args().nth(1).unwrap_or_default();
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
    let answer = protocol::serve(registry.default_backend().as_ref(), &verb, &request);
    registry.shutdown_all();
    println!("{answer}");
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
