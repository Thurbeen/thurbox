//! Reference process plugin contributing a backend named `sample`.
//!
//! Speaks the JSON-RPC protocol described in `docs/PLUGINS.md`:
//! one request per line on stdin, one response per line on stdout, debug
//! prints to stderr (which thurbox captures into the plugin's log file).
//!
//! For each `backend.spawn`/`backend.adopt` call this plugin opens a Unix
//! domain socket under `$XDG_RUNTIME_DIR/thurbox-sample/<id>.sock` (falling
//! back to `/tmp/thurbox-sample/<id>.sock`), accepts one connection, and
//! echoes whatever the host writes back — enough to exercise the full
//! adapter end-to-end without needing a real terminal multiplexer.
//!
//! Real plugins (e.g. the gastown bridge) follow the same shape but back
//! the socket with their own session bytes instead of an echo loop.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{json, Value};

#[derive(Default)]
struct PluginState {
    sessions: HashMap<String, SessionEntry>,
    next_id: AtomicU64,
}

struct SessionEntry {
    socket_path: PathBuf,
    alive: Arc<std::sync::atomic::AtomicBool>,
}

fn main() {
    eprintln!("sample-backend-plugin: starting");

    let state = Arc::new(Mutex::new(PluginState::default()));
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                eprintln!("sample-backend-plugin: stdin closed");
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("sample-backend-plugin: read error: {e}");
                break;
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("sample-backend-plugin: bad request {trimmed}: {e}");
                continue;
            }
        };
        let id = req.get("id").and_then(Value::as_u64).unwrap_or(0);
        let op = req
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        let resp = handle(&state, &op, &params);
        let frame = match resp {
            Ok(result) => json!({"id": id, "ok": true, "result": result}),
            Err(msg) => json!({"id": id, "ok": false, "error": msg}),
        };
        let mut s = serde_json::to_string(&frame).expect("serialize");
        s.push('\n');
        if let Err(e) = stdout.write_all(s.as_bytes()) {
            eprintln!("sample-backend-plugin: write error: {e}");
            break;
        }
        if let Err(e) = stdout.flush() {
            eprintln!("sample-backend-plugin: flush error: {e}");
            break;
        }

        if op == "stop" {
            eprintln!("sample-backend-plugin: stop received; exiting");
            break;
        }
    }
}

fn handle(state: &Arc<Mutex<PluginState>>, op: &str, params: &Value) -> Result<Value, String> {
    match op {
        "handshake" => Ok(json!({
            "api_version": 1,
            "capabilities": ["backend"],
        })),
        "stop" => Ok(Value::Null),
        "config.updated" => {
            eprintln!("sample-backend-plugin: config.updated {params}");
            Ok(Value::Null)
        }
        "backend.spawn" | "backend.adopt" => {
            let id = if op == "backend.adopt" {
                params
                    .get("backend_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing backend_id".to_string())?
                    .to_string()
            } else {
                let n = state
                    .lock()
                    .unwrap()
                    .next_id
                    .fetch_add(1, Ordering::Relaxed);
                format!("sample-{n}")
            };
            let socket_path = socket_path_for(&id);
            let _ = fs::remove_file(&socket_path);
            if let Some(parent) = socket_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
            }
            let listener = UnixListener::bind(&socket_path)
                .map_err(|e| format!("bind {}: {e}", socket_path.display()))?;

            let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let alive_thread = Arc::clone(&alive);
            let id_thread = id.clone();
            thread::spawn(move || serve_session(id_thread, listener, alive_thread));

            state.lock().unwrap().sessions.insert(
                id.clone(),
                SessionEntry {
                    socket_path: socket_path.clone(),
                    alive,
                },
            );
            Ok(json!({
                "backend_id": id,
                "socket_path": socket_path.display().to_string(),
            }))
        }
        "backend.discover" => {
            let s = state.lock().unwrap();
            let entries: Vec<Value> = s
                .sessions
                .iter()
                .map(|(id, e)| {
                    json!({
                        "backend_id": id,
                        "name": id,
                        "is_alive": e.alive.load(Ordering::Relaxed),
                    })
                })
                .collect();
            Ok(Value::Array(entries))
        }
        "backend.resize" => Ok(Value::Null),
        "backend.is_dead" => {
            let id = params
                .get("backend_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing backend_id".to_string())?;
            let s = state.lock().unwrap();
            let dead = !s
                .sessions
                .get(id)
                .map(|e| e.alive.load(Ordering::Relaxed))
                .unwrap_or(false);
            Ok(json!({"dead": dead}))
        }
        "backend.kill" | "backend.detach" => {
            let id = params
                .get("backend_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing backend_id".to_string())?;
            let mut s = state.lock().unwrap();
            if let Some(entry) = s.sessions.remove(id) {
                entry.alive.store(false, Ordering::Relaxed);
                let _ = fs::remove_file(&entry.socket_path);
            }
            Ok(Value::Null)
        }
        "backend.pane_pid" => Ok(json!({"pid": std::process::id()})),
        other => Err(format!("unknown op: {other}")),
    }
}

fn socket_path_for(id: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("thurbox-sample").join(format!("{id}.sock"))
}

fn serve_session(id: String, listener: UnixListener, alive: Arc<std::sync::atomic::AtomicBool>) {
    eprintln!("sample-backend-plugin: serving session {id}");
    let stream = match listener.accept() {
        Ok((s, _)) => s,
        Err(e) => {
            eprintln!("sample-backend-plugin: accept on {id}: {e}");
            alive.store(false, Ordering::Relaxed);
            return;
        }
    };
    // Greeting so the host's reader sees data immediately — useful for tests.
    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sample-backend-plugin: try_clone on {id}: {e}");
            alive.store(false, Ordering::Relaxed);
            return;
        }
    };
    let _ = writer.write_all(b"hello from sample-backend-plugin\n");
    let _ = writer.flush();
    echo_loop(stream);
    alive.store(false, Ordering::Relaxed);
    eprintln!("sample-backend-plugin: session {id} ended");
}

fn echo_loop(mut stream: UnixStream) {
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if stream.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = stream.flush();
            }
            Err(_) => break,
        }
    }
}
