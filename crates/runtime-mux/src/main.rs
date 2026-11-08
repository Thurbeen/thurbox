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
    let settings = MuxSettings {
        socket: Some(socket),
        ..MuxSettings::default()
    };
    let registry = runtime_mux::wiring::local_only(&settings);
    let answer = protocol::serve(registry.default_backend().as_ref(), &verb, &request);
    registry.shutdown_all();
    println!("{answer}");
}
