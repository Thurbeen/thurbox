use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use term_stream::{Capability, Down, Exit, Hello, Size, Up};

use crate::core::PROGRAM;

const HISTORY_LINES: u64 = 2000;
const CONTROL: u32 = 0;
const DATA: u8 = 0;
const CREDIT: u8 = 1;
const CLOSE: u8 = 2;
const RESIZE: u8 = 3;
const BRACKETED_ON: &[u8] = b"\x1b[?2004h";
const BRACKETED_OFF: &[u8] = b"\x1b[?2004l";

type Upstream = Arc<Mutex<ChildStdin>>;

/// `runtime-herdr view [--session S] <terminal>`: the terminal stream
/// (docs/runtime/README.md, "The terminal stream") over herdr-core's framed
/// `terminal.attach`.
pub fn run(args: &[String]) -> i32 {
    match stream(args) {
        Ok(()) => 0,
        Err(why) => {
            let mut stdout = std::io::stdout().lock();
            let _ = term_stream::write(&mut stdout, &Down::Exit(Exit::Unknown { why }));
            let _ = stdout.flush();
            1
        }
    }
}

fn stream(args: &[String]) -> Result<(), String> {
    let (session, terminal) = match args {
        [flag, session, terminal] if flag == "--session" => (Some(session.as_str()), terminal),
        [terminal] => (None, terminal),
        _ => return Err("usage: runtime-herdr view [--session S] <terminal>".to_string()),
    };
    let mut down = std::io::stdout().lock();
    write_down(
        &mut down,
        &Down::Hello(Hello {
            version: term_stream::VERSION,
            capabilities: vec![Capability::Resize],
        }),
    )?;
    let mut view = std::io::stdin();
    let size = loop {
        match term_stream::read::<Up>(&mut view) {
            Ok(Some(Up::Resize(size))) => break size,
            Ok(Some(_)) => {}
            Ok(None) => return Ok(()),
            Err(e) => return Err(format!("the view's first frame: {e}")),
        }
    };
    let mut command = Command::new(PROGRAM);
    if let Some(session) = session {
        command.env("HC_SESSION", session);
    }
    let mut core = command
        .arg("connect")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run {PROGRAM} connect: {e}"))?;
    let (Some(mut requests), Some(replies)) = (core.stdin.take(), core.stdout.take()) else {
        return Err(format!("{PROGRAM} connect has no pipes"));
    };
    let mut replies = BufReader::new(replies);
    let hello = json!({ "jsonrpc": "2.0", "id": 0, "method": "hello", "params": { "generation": 1, "framed": true } });
    requests
        .write_all(format!("{hello}\n").as_bytes())
        .and_then(|()| requests.flush())
        .map_err(|e| format!("herdr-core connect: {e}"))?;
    let mut line = String::new();
    replies
        .read_line(&mut line)
        .map_err(|e| format!("herdr-core connect: {e}"))?;
    let greeted: Value =
        serde_json::from_str(&line).map_err(|e| format!("herdr-core hello: {e}"))?;
    if greeted.get("error").is_some() {
        return Err(format!("herdr-core hello: {}", greeted["error"]));
    }
    let upstream: Upstream = Arc::new(Mutex::new(requests));
    let attach = json!({ "jsonrpc": "2.0", "id": 1, "method": "terminal.attach", "params": {
        "terminal": terminal, "cols": size.cols, "rows": size.rows, "history": HISTORY_LINES,
    } });
    send(&upstream, CONTROL, DATA, attach.to_string().as_bytes())?;
    let channel = await_channel(&mut replies)?;
    let bracketed = Arc::new(AtomicBool::new(false));
    relay_view(
        view_frames(),
        Arc::clone(&upstream),
        channel,
        Arc::clone(&bracketed),
    );
    relay_core(&mut replies, &mut down, &upstream, channel, &bracketed)
}

fn view_frames() -> impl Iterator<Item = Up> + Send + 'static {
    let mut view = std::io::stdin();
    std::iter::from_fn(move || term_stream::read::<Up>(&mut view).ok().flatten())
}

fn relay_view(
    frames: impl Iterator<Item = Up> + Send + 'static,
    upstream: Upstream,
    channel: u32,
    bracketed: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        for frame in frames {
            let sent = match frame {
                Up::Input(bytes) => send(&upstream, channel, DATA, &bytes),
                Up::Paste(text) if bracketed.load(Ordering::Relaxed) => {
                    let wrapped = format!("\x1b[200~{text}\x1b[201~");
                    send(&upstream, channel, DATA, wrapped.as_bytes())
                }
                Up::Paste(text) => send(&upstream, channel, DATA, text.as_bytes()),
                Up::Resize(Size { rows, cols }) => {
                    let mut size = cols.to_le_bytes().to_vec();
                    size.extend_from_slice(&rows.to_le_bytes());
                    send(&upstream, channel, RESIZE, &size)
                }
                Up::Detach => break,
            };
            if sent.is_err() {
                return;
            }
        }
        let _ = send(&upstream, channel, CLOSE, &[]);
    });
}

fn relay_core(
    replies: &mut impl Read,
    down: &mut impl Write,
    upstream: &Upstream,
    channel: u32,
    bracketed: &AtomicBool,
) -> Result<(), String> {
    let mut tail: Vec<u8> = Vec::new();
    loop {
        let Some((on, kind, payload)) = read_frame(replies)? else {
            return Err("herdr-core connect closed the stream".to_string());
        };
        if on != channel {
            continue;
        }
        match kind {
            DATA => {
                tail.extend_from_slice(&payload);
                if let Some(on) = last_bracketed_mode(&tail) {
                    bracketed.store(on, Ordering::Relaxed);
                }
                let keep = tail.len().saturating_sub(BRACKETED_ON.len());
                tail.drain(..keep);
                let consumed = u32::try_from(payload.len()).unwrap_or(u32::MAX);
                write_down(down, &Down::Output(payload))?;
                send(upstream, channel, CREDIT, &consumed.to_le_bytes())?;
            }
            CLOSE => {
                if let Ok(reason) = serde_json::from_slice::<Value>(&payload) {
                    let code = reason["exit_code"].as_i64();
                    write_down(down, &Down::Exit(Exit::Known { code }))?;
                }
                return Ok(());
            }
            RESIZE if payload.len() == 4 => {
                let cols = u16::from_le_bytes([payload[0], payload[1]]);
                let rows = u16::from_le_bytes([payload[2], payload[3]]);
                write_down(down, &Down::Resized(Size { rows, cols }))?;
            }
            _ => {}
        }
    }
}

fn last_bracketed_mode(bytes: &[u8]) -> Option<bool> {
    let find = |needle: &[u8]| bytes.windows(needle.len()).rposition(|w| w == needle);
    match (find(BRACKETED_ON), find(BRACKETED_OFF)) {
        (Some(on), Some(off)) => Some(on > off),
        (Some(_), None) => Some(true),
        (None, Some(_)) => Some(false),
        (None, None) => None,
    }
}

fn await_channel(replies: &mut impl Read) -> Result<u32, String> {
    loop {
        let Some((channel, kind, payload)) = read_frame(replies)? else {
            return Err("herdr-core connect closed before it answered terminal.attach".to_string());
        };
        if channel != CONTROL || kind != DATA {
            continue;
        }
        let reply: Value = serde_json::from_slice(&payload)
            .map_err(|e| format!("herdr-core terminal.attach: {e}"))?;
        if reply["id"] != 1 {
            continue;
        }
        if let Some(error) = reply.get("error") {
            return Err(format!("herdr-core terminal.attach: {error}"));
        }
        return reply["result"]["channel"]
            .as_u64()
            .and_then(|c| u32::try_from(c).ok())
            .ok_or_else(|| format!("herdr-core terminal.attach named no channel: {reply}"));
    }
}

fn read_frame(replies: &mut impl Read) -> Result<Option<(u32, u8, Vec<u8>)>, String> {
    let mut header = [0u8; 9];
    match replies.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(format!("herdr-core connect: {e}")),
    }
    let channel = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let length = u32::from_le_bytes([header[5], header[6], header[7], header[8]]) as usize;
    let mut payload = vec![0u8; length];
    replies
        .read_exact(&mut payload)
        .map_err(|e| format!("herdr-core connect: {e}"))?;
    Ok(Some((channel, header[4], payload)))
}

fn send(upstream: &Upstream, channel: u32, kind: u8, payload: &[u8]) -> Result<(), String> {
    let length = u32::try_from(payload.len()).map_err(|_| "a frame over 4 GiB".to_string())?;
    let mut frame = Vec::with_capacity(9 + payload.len());
    frame.extend_from_slice(&channel.to_le_bytes());
    frame.push(kind);
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(payload);
    let mut upstream = upstream
        .lock()
        .map_err(|_| "the upstream lock is poisoned".to_string())?;
    upstream
        .write_all(&frame)
        .and_then(|()| upstream.flush())
        .map_err(|e| format!("herdr-core connect: {e}"))
}

fn write_down(down: &mut impl Write, frame: &Down) -> Result<(), String> {
    term_stream::write(down, frame)
        .and_then(|()| down.flush())
        .map_err(|e| format!("the view: {e}"))
}
