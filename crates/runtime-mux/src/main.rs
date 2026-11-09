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
/// the returned file is dropped. It sits in the directory tmux derives its
/// socket's from, `TMUX_TMPDIR` or else `/tmp` and never `TMPDIR`, so every
/// caller reaching one server takes one lock, and a server in another
/// directory has its own.
fn lock(socket: &str) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    let fallback = || std::path::PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let fallback = std::env::temp_dir;
    let dir = std::env::var_os("TMUX_TMPDIR")
        .filter(|d| !d.is_empty())
        .map_or_else(fallback, std::path::PathBuf::from);
    #[cfg(unix)]
    // SAFETY: getuid has no preconditions and cannot fail.
    let user = unsafe { libc::getuid() }.to_string();
    #[cfg(not(unix))]
    let user = std::env::var("USERNAME").unwrap_or_default();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("runtime-mux-{user}-{socket}.lock")))?;
    file.lock()?;
    Ok(file)
}
