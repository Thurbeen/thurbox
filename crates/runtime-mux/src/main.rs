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
    let telemetry = otel_env::init(
        "runtime-mux",
        env!("CARGO_PKG_VERSION"),
        &[],
        otel_env::Logs::Environment,
    )
    .unwrap_or_else(|why| {
        eprintln!("warning: telemetry is off: {why}");
        otel_env::Telemetry::default()
    });
    if let Some(provider) = telemetry.logs() {
        export_warnings(provider);
    }
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
    println!("{answer}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    telemetry.shutdown_within(otel_env::CALL_SHUTDOWN_BOUND);
}

fn export_warnings(provider: &otel_env::SdkLoggerProvider) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer;
    let filter = tracing_subscriber::filter::Targets::new()
        .with_target("runtime_mux", tracing::level_filters::LevelFilter::WARN);
    let bridge = opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(provider)
        .with_filter(filter);
    let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(bridge));
}

fn serve(verb: &str) -> serde_json::Value {
    let mut request = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut request) {
        return serde_json::json!({ "unknown": { "why": format!("cannot read the request: {e}") } });
    }
    let socket = std::env::var(SOCKET_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| SOCKET.to_string());
    let place = protocol::place_of(&request);
    let server = match &place {
        Some(host) => format!("{socket}@{}", host.name),
        None => socket.clone(),
    };
    // MUST KILL: type — a lock guard type that can only take the key lock before the server lock would make the order impossible to break
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
    let mut server_turn = None;
    if matches!(verb, "start" | "stop" | "shell" | "pause") {
        match take(&format!("runtime-mux-{server}.lock")) {
            Ok(file) => server_turn = Some(file),
            Err(answer) => return answer,
        }
    }
    let kept = kept(&server);
    let marked = match verb {
        "list" => Some(None),
        "start" | "stop" | "pause" | "inspect" => request_key(&request).map(Some),
        _ => None,
    };
    if let Some(key) = marked {
        let holding_the_lock = server_turn.is_some();
        if let Err(answer) =
            copy_older_pause_marks(&server, &kept, key.as_deref(), holding_the_lock)
        {
            return answer;
        }
    }
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

fn keep_cache(socket: &str) {
    if let (Ok(dir), prefix) = private_dir() {
        runtime_mux::place::keep_cache_in(dir, format!("{prefix}runtime-mux-{socket}@"));
    }
}

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

fn take(name: &str) -> Result<std::fs::File, serde_json::Value> {
    lock(name).map_err(|e| {
        serde_json::json!({ "unknown": { "why": format!("cannot take the lock {name}: {e}") } })
    })
}

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

fn released(request: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(request)
        .ok()
        .and_then(|v| v.get("release").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

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
                // tmux finds its socket under TMUX_TMPDIR; set it before any thread starts.
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
    let beside_start_records = |what: &str| match &sockets {
        Ok(sockets) => runtime_mux::workspace::data_dir()
            .map(|data| {
                data.join(what)
                    .join(sockets_digest(sockets))
                    .join(server_dir(server))
            })
            .ok_or_else(|| format!("no HOME to keep the {what} keys in")),
        Err(e) => Err(format!("no directory for the tmux sockets: {e}")),
    };
    protocol::Kept {
        paused: protocol::Marks::new(
            beside_start_records("paused"),
            "key.".to_string(),
            "pause mark",
        ),
        started: protocol::Marks::new(
            beside_start_records("started"),
            "key.".to_string(),
            "start record",
        ),
        paused_beside_the_locks: protocol::Marks::new(
            sockets.map_err(|e| format!("no directory for the tmux sockets: {e}")),
            format!("{prefix}runtime-mux-{server}.paused-"),
            "pause mark",
        ),
    }
}

fn copy_older_pause_marks(
    server: &str,
    kept: &protocol::Kept,
    key: Option<&str>,
    holding_the_lock: bool,
) -> Result<(), serde_json::Value> {
    let unknown = |why: String| serde_json::json!({ "unknown": { "why": why } });
    if kept.pause_marks_to_copy(key).map_err(unknown)?.is_empty() {
        return Ok(());
    }
    let _turn = match holding_the_lock {
        true => None,
        false => Some(take(&format!("runtime-mux-{server}.lock"))?),
    };
    kept.copy_pause_marks(key).map_err(unknown)
}

const SERVER_DIR_COMPONENT: usize = 128;

fn server_dir(server: &str) -> std::path::PathBuf {
    let hex: String = server.bytes().map(|b| format!("{b:02x}")).collect();
    hex.as_bytes()
        .chunks(SERVER_DIR_COMPONENT)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect()
}

fn sockets_digest(sockets: &std::path::Path) -> String {
    format!(
        "{:08x}",
        runtime_mux::instance::fnv1a32(sockets.to_string_lossy().as_bytes())
    )
}

fn lock(name: &str) -> std::io::Result<std::fs::File> {
    let (dir, prefix) = private_dir();
    let path = dir?.join(format!("{prefix}{name}"));
    let mut options = std::fs::OpenOptions::new();
    // Windows locks only a handle opened to read or write.
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
    let file = options.open(path)?;
    file.lock()?;
    Ok(file)
}

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

#[cfg(not(unix))]
fn private_dir() -> (std::io::Result<std::path::PathBuf>, String) {
    let user = std::env::var("USERNAME").unwrap_or_default();
    (Ok(std::env::temp_dir()), format!("{user}-"))
}

#[cfg(test)]
mod tests {
    use super::server_dir;

    #[test]
    fn a_long_server_name_fits_in_filenames_and_stays_distinct() {
        let socket = "s".repeat(64);
        let long = format!("{socket}@{}", "p".repeat(64));
        let longer = format!("{long}q");
        let dir = server_dir(&long);
        assert!(
            dir.components().all(|c| c.as_os_str().len() <= 255),
            "{}",
            dir.display()
        );
        assert_ne!(dir, server_dir(&longer));
        assert_ne!(server_dir("ab"), server_dir("a/b"));
    }
}
