//! `runtime-mux <verb>`: the runtime protocol's program (docs/runtime/README.md).
//! Reads one JSON request on stdin and prints one answer on stdout. The
//! composition root: it alone builds the backend, on the program's own tmux
//! socket ([`runtime_mux::protocol::SOCKET`]), on this machine or on the
//! remote place the request names.
//!
//! `runtime-mux view [--socket S] [--tmux-tmpdir D] [--scrollback N]
//! [--place JSON] <key | --pane P>` is the
//! other shape: one key's pane, or a pane by its id, as a terminal stream on
//! stdin and stdout, for as long as the view stays. `attach` answers with it;
//! thurbox's stream panes name the pane. With `--place`, the request's place
//! as JSON, the pane is the remote place's, read through that place's
//! backend over its launcher.

use std::io::Read;
use std::sync::{Arc, Mutex};

use runtime_mux::protocol::{self, SOCKET, SOCKET_ENV};
use runtime_mux::{BackendRegistry, HostDef, HostRegistry, MuxSettings, SessionBackend};

fn main() {
    let verb = std::env::args().nth(1).unwrap_or_default();
    if verb == "view" {
        if let Err(why) = view(std::env::args().skip(2).collect()) {
            eprintln!("runtime-mux view: {why}");
            std::process::exit(1);
        }
        return;
    }
    // Each verb is one span, `runtime-mux VERB`, under the caller's
    // TRACEPARENT; off unless the OTEL_* variables ask (docs/observability.md).
    // A view runs as long as a viewer watches, so it records nothing.
    let telemetry = otel_env::init("runtime-mux", env!("CARGO_PKG_VERSION"), &[], false)
        .unwrap_or_else(|why| {
            eprintln!("warning: telemetry is off: {why}");
            otel_env::Telemetry::default()
        });
    let answer = {
        let root = otel_env::root(
            "runtime-mux",
            format!("runtime-mux {verb}"),
            vec![otel_env::KeyValue::new("runtime.verb", verb.clone())],
        );
        let answer = serve(&verb);
        let known = answer.get("known").is_some();
        root.set(otel_env::KeyValue::new(
            "runtime.outcome",
            if known { "known" } else { "unknown" },
        ));
        if !known {
            root.fail("unknown".into());
        }
        answer
    };
    // The answer first: its caller waits for this process to exit, and
    // the flush below is bounded far inside that caller's deadline, so a
    // stalled collector never turns an answer into a timeout.
    println!("{answer}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    telemetry.shutdown_within(otel_env::CALL_SHUTDOWN_BOUND);
}

/// Answer one protocol verb with the request on stdin.
fn serve(verb: &str) -> serde_json::Value {
    let mut request = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut request) {
        return serde_json::json!({ "unknown": { "why": format!("cannot read the request: {e}") } });
    }
    let socket = std::env::var(SOCKET_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| SOCKET.to_string());
    // A remote place's server is not this machine's: its locks and pause
    // marks name the place, so a slow place never holds up a local start.
    let place = protocol::place_of(&request);
    let server = match &place {
        Some(host) => format!("{socket}@{}", host.name),
        None => socket.clone(),
    };
    // A start looks the key up and then makes its window, so two at once
    // could each make one; a stop between them could remove the one the
    // other just adopted. Both run one at a time per server. A workspace
    // call makes or adopts by key the same way, but may take minutes (a
    // first clone, setup steps), so it holds only its key's lock, never the
    // server's. A stop that releases the workspace takes the key's lock
    // first, then the server's: no call takes them the other way round.
    let key_lock = match verb {
        "workspace" => true,
        "stop" => released(&request),
        _ => false,
    };
    let mut _key_turn = None;
    if key_lock {
        if let Some(key) = request_key(&request) {
            match take(&format!("runtime-mux-{server}.key-{key}.lock")) {
                Ok(file) => _key_turn = Some(file),
                Err(answer) => return answer,
            }
        }
    }
    // A releasing stop lets go of the server's lock once the process is
    // stopped: the workspace's teardown and release hold the key only.
    let mut server_turn = None;
    // A shell call makes the key's shell window the way a start makes its
    // agent's, so it takes the same turn.
    // A pause stops the process as a stop does.
    if matches!(verb, "start" | "stop" | "shell" | "pause") {
        match take(&format!("runtime-mux-{server}.lock")) {
            Ok(file) => server_turn = Some(file),
            Err(answer) => return answer,
        }
    }
    let kept = kept(&server);
    // Each remote place's login `PATH` is cached beside the locks, so a call
    // does not read it again over ssh.
    keep_cache(&socket);
    let settings = MuxSettings {
        socket: Some(socket),
        ..MuxSettings::default()
    };
    let built = Mutex::new(Vec::new());
    let backends = |place: Option<&HostDef>| backend_for(&settings, place, &built);
    let answer = protocol::serve_with(
        &backends,
        &kept,
        verb,
        &request,
        &view_argv(&settings),
        &mut || drop(server_turn.take()),
    );
    for registry in built.into_inner().unwrap_or_default() {
        registry.shutdown_all();
    }
    // A WSL place's server lives only while a `wsl.exe` holds the distro up.
    if let (Some(host), "start" | "shell") = (place.as_ref().filter(|h| h.is_wsl()), verb) {
        let built = Mutex::new(Vec::new());
        if let Some(waiter) = backend_for(&settings, Some(host), &built)
            .ok()
            .and_then(|backend| backend.server_waiter())
        {
            runtime_mux::place::run_detached(waiter);
        }
        for registry in built.into_inner().unwrap_or_default() {
            registry.shutdown_all();
        }
    }
    answer
}

/// Cache each remote place's login `PATH` beside the locks of `socket`, so
/// a call does not read it again over ssh.
fn keep_cache(socket: &str) {
    if let (Ok(dir), prefix) = private_dir() {
        runtime_mux::place::keep_cache_in(dir, format!("{prefix}runtime-mux-{socket}@"));
    }
}

/// The backend serving `place`: this machine's default multiplexer, or the
/// place's own on the host it names. A remote place's server runs on this
/// program's socket unless the place names another. Each registry built is
/// kept in `built`, for the caller to shut down.
fn backend_for(
    settings: &MuxSettings,
    place: Option<&HostDef>,
    built: &Mutex<Vec<BackendRegistry>>,
) -> Result<Arc<dyn SessionBackend>, String> {
    let mut host = place.cloned();
    if let Some(host) = &mut host {
        if host.socket.is_none() {
            host.socket.clone_from(&settings.socket);
        }
    }
    let hosts = HostRegistry {
        config_version: None,
        hosts: host.iter().cloned().collect(),
    };
    let registry = runtime_mux::wiring::for_hosts(&hosts, settings);
    let backend = match &host {
        None => Some(registry.default_backend().clone()),
        Some(host) => registry.get(&host.default_route()).cloned(),
    };
    let found = backend.ok_or_else(|| {
        let route = host.as_ref().map(HostDef::default_route);
        format!(
            "place {:?}: no multiplexer runtime-mux drives is {}",
            host.as_ref().map(|h| h.name.as_str()).unwrap_or_default(),
            route.map(|r| r.mux.name().to_string()).unwrap_or_default()
        )
    });
    if let Ok(mut built) = built.lock() {
        built.push(registry);
    }
    found
}

/// The lock file `name`, or the Unknown answer when it cannot be taken.
fn take(name: &str) -> Result<std::fs::File, serde_json::Value> {
    lock(name).map_err(|e| {
        serde_json::json!({ "unknown": { "why": format!("cannot take the lock {name}: {e}") } })
    })
}

/// The request's key when it names a valid one: the runtime protocol's
/// key, letters, digits, '.', '-' and '_', which a file name holds as it is.
fn request_key(request: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(request).ok()?;
    let key = value.get("key")?.as_str()?;
    let valid = !key.is_empty()
        && key.len() <= 128
        && !key.starts_with('.')
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    valid.then(|| key.to_string())
}

/// Whether a stop request releases the key's workspace.
fn released(request: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(request)
        .ok()
        .and_then(|v| v.get("release").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
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
    let mut place = None;
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
            "--place" => {
                let named = args.next().ok_or("--place takes the place as JSON")?;
                let named: serde_json::Value =
                    serde_json::from_str(&named).map_err(|e| format!("--place: {e}"))?;
                place = protocol::check_place(Some(&named))?;
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
    keep_cache(&socket);
    let defaults = MuxSettings::default();
    let settings = MuxSettings {
        socket: Some(socket),
        scrollback_lines: scrollback.unwrap_or(defaults.scrollback_lines),
        ..defaults
    };
    if let Some(host) = &place {
        runtime_mux::place::warm(host);
    }
    let built = Mutex::new(Vec::new());
    let served = view_pane(&settings, place.as_ref(), &built, pane, key);
    for registry in built.into_inner().unwrap_or_default() {
        registry.shutdown_all();
    }
    served
}

/// The view of `pane`, or of the pane holding `key`, on the backend of
/// `place`.
fn view_pane(
    settings: &MuxSettings,
    place: Option<&HostDef>,
    built: &Mutex<Vec<BackendRegistry>>,
    pane: Option<String>,
    key: Option<String>,
) -> Result<(), String> {
    let backend = backend_for(settings, place, built)?;
    let pane = match (pane, key) {
        (Some(pane), None) => pane,
        (None, Some(key)) => protocol::pane_of(backend.as_ref(), &key)?
            .ok_or_else(|| format!("no process holds key {key}"))?,
        _ => {
            return Err(
                "usage: runtime-mux view [--socket S] [--tmux-tmpdir D] [--scrollback N] [--place JSON] <key | --pane P>".into(),
            )
        }
    };
    backend.ensure_ready().map_err(|e| e.to_string())?;
    runtime_mux::view::serve(backend, &pane, std::io::stdin(), &mut std::io::stdout())
}

fn kept(server: &str) -> protocol::Kept {
    let (sockets, prefix) = private_dir();
    let started = match &sockets {
        Ok(sockets) => runtime_mux::workspace::data_dir()
            .map(|data| data.join("started").join(sockets_digest(sockets)))
            .ok_or_else(|| "no HOME to keep the started keys in".to_string()),
        Err(e) => Err(format!("no directory for the tmux sockets: {e}")),
    };
    protocol::Kept {
        paused: protocol::Marks::new(
            sockets.map_err(|e| format!("no directory for the pause marks: {e}")),
            format!("{prefix}runtime-mux-{server}.paused-"),
            "pause mark",
        ),
        started: protocol::Marks::new(started, format!("{server}."), "start record"),
    }
}

fn sockets_digest(sockets: &std::path::Path) -> String {
    format!(
        "{:08x}",
        runtime_mux::instance::fnv1a32(sockets.to_string_lossy().as_bytes())
    )
}

/// The lock file `name`, held until the returned file is dropped: the one
/// serializing starts and stops on a server, or one key's.
fn lock(name: &str) -> std::io::Result<std::fs::File> {
    let (dir, prefix) = private_dir();
    let path = dir?.join(format!("{prefix}{name}"));
    let mut options = std::fs::OpenOptions::new();
    // Appending never truncates. Windows locks only a handle opened to read
    // or write, and an append-only one is neither.
    options.create(true).read(true).append(true);
    // A link at the name is refused.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
    let file = options.open(path)?;
    file.lock()?;
    Ok(file)
}

/// The directory the locks and the pause marks sit in, and the prefix of
/// their names. It is the private directory tmux keeps its sockets in,
/// `<TMUX_TMPDIR or /tmp>/tmux-<uid>` and never under `TMPDIR`, so every
/// caller reaching one server takes one lock, a server in another directory
/// has its own, and no other user can plant a link at a name. The directory
/// is made `0700` when missing and refused, as tmux refuses it, when another
/// user owns it or can write to it.
#[cfg(unix)]
fn private_dir() -> (std::io::Result<std::path::PathBuf>, String) {
    (tmux_dir(), String::new())
}

#[cfg(unix)]
fn tmux_dir() -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
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
    Ok(dir)
}

/// On a machine without tmux's private socket directory: the temporary
/// directory, each name prefixed with the user's.
#[cfg(not(unix))]
fn private_dir() -> (std::io::Result<std::path::PathBuf>, String) {
    let user = std::env::var("USERNAME").unwrap_or_default();
    (Ok(std::env::temp_dir()), format!("{user}-"))
}
