use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_OUTPUT_CAP: usize = 1 << 20;
const READER_GRACE: Duration = Duration::from_millis(500);

type Captured = Arc<Mutex<Vec<u8>>>;

pub fn run(request: &Value) -> Result<Value, String> {
    let argv: Vec<String> = request["argv"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|word| word.as_str().map(String::from))
        .collect::<Option<_>>()
        .ok_or("argv: a list of strings")?;
    let Some((program, args)) = argv.split_first() else {
        return Err("argv names no program".to_string());
    };
    match request["in"].as_str() {
        None | Some("host") => {}
        Some(other) => {
            return Err(format!(
                "in: runtime-herdr runs exec on the host, not {other:?}"
            ));
        }
    }
    let dir = request["dir"].as_str();
    if dir.is_some_and(|d| !d.starts_with('/')) {
        return Err("dir: an absolute path".to_string());
    }
    let deadline =
        Duration::from_millis(request["timeoutMs"].as_u64().unwrap_or(DEFAULT_TIMEOUT_MS));
    let cap = request["outputCap"]
        .as_u64()
        .and_then(|c| usize::try_from(c).ok())
        .unwrap_or(DEFAULT_OUTPUT_CAP);
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return Ok(json!({ "failed": { "why": format!("cannot run {program}: {e}") } })),
    };
    let (stdout, stdout_reader) = drain(child.stdout.take(), cap);
    let (stderr, stderr_reader) = drain(child.stderr.take(), cap);
    let started = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status.code()),
            Ok(None) if started.elapsed() >= deadline => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("{program}: {e}")),
        }
    };
    end_group(&mut child);
    let readers_until = (started + deadline).max(Instant::now() + READER_GRACE);
    for reader in [stdout_reader, stderr_reader] {
        while !reader.is_finished() && Instant::now() < readers_until {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let text = |captured: &Captured| {
        String::from_utf8_lossy(&captured.lock().map(|b| b.clone()).unwrap_or_default())
            .into_owned()
    };
    let (stdout, stderr) = (text(&stdout), text(&stderr));
    Ok(match code {
        Some(code) => json!({ "exited": { "code": code, "stdout": stdout, "stderr": stderr } }),
        None => json!({ "timedOut": { "stdout": stdout, "stderr": stderr } }),
    })
}

fn drain(stream: Option<impl Read + Send + 'static>, cap: usize) -> (Captured, JoinHandle<()>) {
    let captured: Captured = Arc::default();
    let sink = Arc::clone(&captured);
    let reader = std::thread::spawn(move || {
        let Some(mut stream) = stream else { return };
        let mut chunk = [0u8; 8192];
        while let Ok(n) = stream.read(&mut chunk) {
            if n == 0 {
                break;
            }
            if let Ok(mut kept) = sink.lock() {
                let room = cap.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    });
    (captured, reader)
}

fn end_group(child: &mut Child) {
    #[cfg(unix)]
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}
