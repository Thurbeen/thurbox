use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use tracing::warn;

use crate::contract::{SessionBackend, WindowRole};
use crate::settings::MuxSettings;
use crate::shell::{
    powershell_launcher, powershell_quote, CommandLineTooLong, HostLauncher, WINDOWS_CMD_LINE_MAX,
};
use crate::tmux_compat::control_mode::{ControlPolicy, Missing, PaneInput, SEND_KEYS_CHUNK_BYTES};
use crate::tmux_compat::server::{
    named_version, parse_version, tmux_answered_absent, ConfigOption, Server, TmuxCompatible,
};
use crate::tmux_compat::transport::TmuxTransport;
use crate::{HostDef, Multiplexer, Platform};

pub struct Psmux;

pub type PsmuxBackend = Server<Psmux>;

pub fn local(settings: &MuxSettings) -> Arc<dyn SessionBackend> {
    Arc::new(PsmuxBackend::local(settings))
}

pub fn on_host(
    host: &HostDef,
    launcher: HostLauncher,
    platform: Platform,
    settings: &MuxSettings,
) -> Arc<dyn SessionBackend> {
    Arc::new(PsmuxBackend::on_host(host, launcher, platform, settings))
}

impl TmuxCompatible for Psmux {
    const MULTIPLEXER: Multiplexer = Multiplexer::Psmux;

    const WINDOW_OPTIONS: bool = false;

    const WINDOW_SETTINGS: bool = false;

    const WINDOW_EVENTS: bool = false;

    const EXIT_ZERO_ERRORS: bool = true;

    const SESSION_SCOPED_IDS: bool = true;

    const KILL_BY_PANE: bool = true;
    const PANE_MONITORING: bool = true;

    const SNAPSHOTS: bool = false;

    const COMMAND_LISTS: bool = false;
    const COMMAND_LIST_SINGLE_REPLY: bool = false;

    // A cold psmux server can refuse `new-session` or disappear before the
    // first `set-option`, even when its `new-session` client exited successfully.
    const COLD_START_ATTEMPTS: usize = 3;

    const COLD_START_GRACE: std::time::Duration = if cfg!(test) {
        std::time::Duration::from_secs(2)
    } else {
        std::time::Duration::from_secs(20)
    };

    const RETRY_NO_SERVER_ERROR: bool = true;

    const BOOTSTRAP_SIZE_ARGS: &[&str] = &[];

    const SESSION_PROBE_COMMAND: &str = "list-windows";

    const ONE_SHOT_SPAWN_ANSWERS: bool = false;
    const SPAWN_ON_STDIN: bool = false;

    const SPAWN_SETTLE: Option<std::time::Duration> = Some(if cfg!(test) {
        std::time::Duration::from_millis(20)
    } else {
        std::time::Duration::from_millis(1500)
    });

    const CONDITIONAL_RESIZE: bool = false;

    const SERVER_SCOPE: &str = "-g";

    const SERVER_OPTIONS_NEED_TARGET: bool = true;

    const DISPLAY_FLAGS: &[&str] = &[];

    const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>> = Some(check_psmux_version);

    fn check_banner(banner: &str, _socket: &str) -> Result<()> {
        check_psmux_binary(banner)
    }

    fn banner_version(banner: &str) -> Option<&str> {
        named_version(banner, &["psmux", "tmux"])
    }

    fn check_listing(listing: &str) -> Result<()> {
        if let Some(error) = listing.lines().find(|line| line.starts_with("ERROR:")) {
            bail!("psmux listing failed: {error}");
        }
        Ok(())
    }

    fn session_config(_session: &str) -> Vec<ConfigOption> {
        Vec::new()
    }

    fn quote(arg: &str) -> String {
        psmux_quote(arg)
    }

    fn answered_absent(answer: &str) -> Option<Missing> {
        const NO_SERVER: &str = "psmux: no server running on session";
        if answer == NO_SERVER {
            return Some(Missing::Server);
        }
        if let Some(socket) = answer
            .strip_prefix(NO_SERVER)
            .and_then(|rest| rest.strip_prefix(" '"))
        {
            return socket
                .strip_suffix('\'')
                .filter(|socket| !socket.is_empty() && !socket.contains('\''))
                .map(|_| Missing::Server);
        }
        if answer
            .strip_prefix("session not found: ")
            .is_some_and(|session| !session.is_empty())
        {
            return Some(Missing::Server);
        }
        tmux_answered_absent(answer)
    }

    fn env_flags(_env: &HashMap<String, String>) -> String {
        String::new()
    }

    fn private_env(
        server: &Server<Self>,
        env: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        if env.is_empty() {
            return Ok(HashMap::new());
        }
        let file = stash_env(&server.located_transport(), env)?;
        Ok(HashMap::from([(ENV_FILE.to_string(), file)]))
    }

    fn discard_private_env(server: &Server<Self>, env: &HashMap<String, String>) {
        if let Some(file) = env.get(ENV_FILE) {
            remove_env_file(&server.located_transport(), file);
        }
    }

    fn window_command(
        _server: &Server<Self>,
        window_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<String> {
        psmux_window_command(window_name, command, args, env)
    }

    fn push_window_program(
        cmd: &mut Command,
        window_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<()> {
        cmd.arg(psmux_window_program(window_name, command, args, env)?);
        Ok(())
    }

    fn paste_args(target: &str, text: &str) -> Vec<Vec<String>> {
        paste_chunks(text)
            .into_iter()
            .map(|chunk| send_paste_args(target, chunk))
            .collect()
    }

    fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> Result<String> {
        deferred_paste_script(mux, socket, target, text)
    }

    fn pane_input(transport: &TmuxTransport, socket: &str) -> Arc<dyn PaneInput> {
        Arc::new(PsmuxInput {
            paste: PsmuxPaste::new(transport.clone(), socket.to_string()),
        })
    }

    fn control_policy(transport: &TmuxTransport, session: &str) -> ControlPolicy {
        ControlPolicy {
            flow_control_command: Some("refresh-client -f pause-after=5"),
            implicit_attach_reply: false,
            tagged_blocks: false,
            command_list_single_reply: Self::COMMAND_LIST_SINGLE_REPLY,
            subscriptions: false,
            status_poll: (transport.is_remote() && Self::HOOK_STATUS)
                .then(|| Self::hook_poll_command(session)),
            confirm_close: Some(live_panes_command(session)),
        }
    }

    const HOOK_STATUS: bool = false;

    fn hook_signal_command(server: &Server<Self>) -> String {
        let socket = server.socket();
        let safe: String = socket
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        if safe != socket {
            warn!(
                "{} socket {socket:?} has characters unsafe for a hook command; using {safe:?}",
                server.name
            );
        }
        let socket = if safe.is_empty() {
            crate::instance::TMUX_SOCKET.to_string()
        } else {
            safe
        };
        format!(
            "{} -L {socket} set-option -p {} ",
            Self::MULTIPLEXER.name(),
            crate::tmux_compat::control_mode::REMOTE_HOOK_STATE_OPTION
        )
    }
}

fn live_panes_command(session: &str) -> String {
    format!(
        "list-panes -s -t {} -F {}",
        psmux_quote(session),
        psmux_quote("#{pane_id}")
    )
}

struct PsmuxInput {
    paste: PsmuxPaste,
}

impl PaneInput for PsmuxInput {
    fn send_keys(&self, pane_id: &str, buf: &[u8]) -> Vec<String> {
        psmux_send_keys_commands(pane_id, buf)
    }

    fn paste(&self, pane_id: &str, text: &str) -> Option<Result<()>> {
        Some(self.paste.deliver(pane_id, text))
    }

    fn pastes_out_of_band(&self) -> bool {
        true
    }
}

const MIN_PSMUX_VERSION: (u32, u32, u32) = (3, 3, 8);

fn check_psmux_version(version_output: &str, socket: &str) -> Result<()> {
    match psmux_version(version_output) {
        Some(v) if v < MIN_PSMUX_VERSION => bail!(
            "psmux {}.{}.{} is too old: its server runs a command meant for one pane on the \
             session's active pane — keystrokes typed into another session, the wrong pane \
             read or killed — and, before 3.3.7, starts agents with a stdin that is not their \
             pane. A running server keeps the code it started with: restart it on psmux \
             {}.{}.{} or newer (`psmux -L {socket} kill-server`, which ends the agents running \
             on it)",
            v.0,
            v.1,
            v.2,
            MIN_PSMUX_VERSION.0,
            MIN_PSMUX_VERSION.1,
            MIN_PSMUX_VERSION.2,
        ),
        _ => Ok(()),
    }
}

fn check_psmux_binary(banner: &str) -> Result<()> {
    match psmux_version(banner) {
        Some(v) if v < MIN_PSMUX_VERSION => bail!(
            "the psmux binary is {}.{}.{}, too old to name a pane through: upgrade it to \
             {}.{}.{} or newer, or put a newer psmux first on PATH; restarting a server would \
             not change this",
            v.0,
            v.1,
            v.2,
            MIN_PSMUX_VERSION.0,
            MIN_PSMUX_VERSION.1,
            MIN_PSMUX_VERSION.2,
        ),
        _ => Ok(()),
    }
}

fn psmux_version(output: &str) -> Option<(u32, u32, u32)> {
    output.lines().rev().find_map(|line| {
        let line = line.trim();
        let word = Psmux::banner_version(line).or_else(|| line.split_whitespace().next())?;
        parse_version(word)
    })
}

const EXIT_REPORT: &str =
    "Get-Variable LASTEXITCODE -ValueOnly -ErrorAction Ignore | thurbox-cli session exited";

const ENV_FILE: &str = "=runtime-mux-env-file";

const STASH_SCRIPT: &str = "$ErrorActionPreference = 'Stop'\n\
    $b = [Console]::In.ReadToEnd().Trim()\n\
    $d = Join-Path $env:LOCALAPPDATA 'runtime-mux'\n\
    [void][IO.Directory]::CreateDirectory($d)\n\
    $p = Join-Path $d ('env-' + [guid]::NewGuid().ToString('N') + '.json')\n\
    $me = [Security.Principal.WindowsIdentity]::GetCurrent().User\n\
    $fs = New-Object Security.AccessControl.FileSecurity\n\
    $fs.SetAccessRuleProtection($true, $false)\n\
    $fs.SetOwner($me)\n\
    $fs.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($me, 'FullControl', 'Allow')))\n\
    $bytes = [Convert]::FromBase64String($b)\n\
    $f = [IO.File]::Create($p, 4096, [IO.FileOptions]::None, $fs)\n\
    try { $f.Write($bytes, 0, $bytes.Length) } finally { $f.Close() }\n\
    [Console]::Out.Write($p)";

const STASH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn stash_request(
    transport: &crate::tmux_compat::transport::TmuxTransport,
    env: &HashMap<String, String>,
) -> (Command, Vec<u8>) {
    let json = serde_json::to_vec(env).unwrap_or_default();
    let input = base64::engine::general_purpose::STANDARD.encode(json);
    (
        transport.powershell_command(STASH_SCRIPT),
        input.into_bytes(),
    )
}

fn stash_env(
    transport: &crate::tmux_compat::transport::TmuxTransport,
    env: &HashMap<String, String>,
) -> Result<String> {
    let (cmd, input) = stash_request(transport, env);
    let out = run_fed_bounded(cmd, input, STASH_TIMEOUT)
        .context("the window's environment was not written")?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || path.is_empty() {
        bail!(
            "the window's environment was not written: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(path)
}

fn remove_env_file(transport: &crate::tmux_compat::transport::TmuxTransport, file: &str) {
    let script = format!(
        "Remove-Item -LiteralPath {} -Force -ErrorAction SilentlyContinue",
        powershell_quote(file)
    );
    if let Err(e) = run_fed_bounded(
        transport.powershell_command(&script),
        Vec::new(),
        STASH_TIMEOUT,
    ) {
        warn!("the window's environment file {file} may be left: {e:#}");
    }
}

fn run_fed_bounded(
    mut cmd: Command,
    input: Vec<u8>,
    timeout: std::time::Duration,
) -> Result<std::process::Output> {
    use std::io::{Read as _, Write as _};
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot run {:?}", cmd.get_program()))?;
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let reader = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut out);
            }
            out
        })
    };
    let stdout = reader(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let stderr = reader(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("no answer within {} s", timeout.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    Ok(std::process::Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

fn env_loader(file: &str) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'; $f = {}; \
         (Get-Content -LiteralPath $f -Raw -Encoding UTF8 | ConvertFrom-Json).PSObject.Properties | \
         ForEach-Object {{ Set-Item -LiteralPath ('Env:' + $_.Name) -Value $_.Value }}; \
         Remove-Item -LiteralPath $f; $ErrorActionPreference = 'Continue'; ",
        powershell_quote(file)
    )
}

fn psmux_window_powershell(
    window_name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> String {
    let mut prelude = String::new();
    // Sort for a deterministic command (HashMap iteration order isn't).
    let mut pairs: Vec<_> = env.iter().collect();
    pairs.sort();
    for (k, v) in pairs {
        if k == ENV_FILE {
            prelude.push_str(&env_loader(v));
        } else {
            prelude.push_str(&format!("Set-Item Env:{k} {}; ", powershell_quote(v)));
        }
    }
    if !prelude.is_empty() {
        prelude.push('\n');
    }
    let after = if WindowRole::from_window_name(window_name) == Some(WindowRole::Agent) {
        format!("{EXIT_REPORT}\n")
    } else {
        String::new()
    };
    crate::shell::windows_program_script(command, args, None, &[], &prelude, &after)
}

fn psmux_window_program(
    window_name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<String> {
    powershell_launcher(&psmux_window_powershell(window_name, command, args, env))
        .with_context(|| format!("window {window_name} cannot start on a Windows host"))
}

fn psmux_window_command(
    window_name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<String> {
    Ok(format!(
        "\"{}\"",
        psmux_window_program(window_name, command, args, env)?
    ))
}

fn deferred_paste_script(mux: &str, socket: &str, target: &str, text: &str) -> Result<String> {
    // Only the socket: a quoted program name is a string to PowerShell,
    // not a command, and a multiplexer's binary name needs no quoting.
    let socket = powershell_quote(socket);
    let t = powershell_quote(target);
    let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let script = format!(
        "powershell -NoProfile -Command \"{mux} -L {socket} send-paste -t {t} {payload}; \
         Start-Sleep -Milliseconds 200; \
         {mux} -L {socket} send-keys -t {t} Enter\""
    );
    if script.len() > WINDOWS_CMD_LINE_MAX {
        return Err(CommandLineTooLong {
            len: script.len(),
            max: WINDOWS_CMD_LINE_MAX,
        })
        .context("the deferred prompt cannot reach a Windows host");
    }
    Ok(script)
}

const SHIFT_ENTER: &[u8] = b"\x1b[13;2u";

pub fn psmux_send_keys_commands(pane_id: &str, buf: &[u8]) -> Vec<String> {
    let mut cmds = Vec::new();
    let mut literal: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if buf[i..].starts_with(SHIFT_ENTER) {
            flush_psmux_literal(pane_id, &mut literal, &mut cmds);
            cmds.push(format!("send-keys -t {pane_id} C-j\n"));
            i += SHIFT_ENTER.len();
            continue;
        }
        if let Some((len, name)) = psmux_navigation_key(&buf[i..]) {
            flush_psmux_literal(pane_id, &mut literal, &mut cmds);
            cmds.push(format!("send-keys -t {pane_id} {name}\n"));
            i += len;
            continue;
        }
        if let Some((len, args)) = psmux_escape_sequence(&buf[i..]) {
            flush_psmux_literal(pane_id, &mut literal, &mut cmds);
            cmds.push(format!("send-keys -t {pane_id} -l -N 1 {args}\n"));
            i += len;
            continue;
        }
        let b = buf[i];
        match psmux_key_name(b) {
            Some(name) => {
                flush_psmux_literal(pane_id, &mut literal, &mut cmds);
                cmds.push(format!("send-keys -t {pane_id} {name}\n"));
            }
            None => literal.push(b),
        }
        i += 1;
    }
    flush_psmux_literal(pane_id, &mut literal, &mut cmds);
    cmds
}

fn psmux_navigation_key(buf: &[u8]) -> Option<(usize, String)> {
    let prefix = buf.get(..2)?;
    if prefix != b"\x1b[" && prefix != b"\x1bO" {
        return None;
    }
    let body = &buf[2..];
    let key = |suffix| match suffix {
        b'A' => Some("Up"),
        b'B' => Some("Down"),
        b'C' => Some("Right"),
        b'D' => Some("Left"),
        b'H' => Some("Home"),
        b'F' => Some("End"),
        _ => None,
    };
    if let Some(&suffix) = body.first() {
        if let Some(name) = key(suffix) {
            return Some((3, name.to_string()));
        }
    }
    if prefix[1] != b'[' {
        return None;
    }
    if body.len() >= 2 && body[1] == b'~' {
        let name = match body[0] {
            b'2' => "Insert",
            b'3' => "Delete",
            b'5' => "PageUp",
            b'6' => "PageDown",
            _ => return None,
        };
        return Some((4, name.to_string()));
    }
    // The longest supported form has four bytes after CSI (for example 5;5~).
    let nav = &body[..body.len().min(4)];
    let semicolon = nav.iter().position(|&b| b == b';')?;
    let suffix_pos = nav
        .iter()
        .position(|&b| matches!(b, b'A'..=b'D' | b'H' | b'F' | b'~'))?;
    if suffix_pos <= semicolon || suffix_pos != semicolon + 2 {
        return None;
    }
    let modifier = body[semicolon + 1];
    if !(b'2'..=b'8').contains(&modifier) {
        return None;
    }
    let base = if body[suffix_pos] == b'~' {
        match &body[..semicolon] {
            b"2" => "Insert",
            b"3" => "Delete",
            b"5" => "PageUp",
            b"6" => "PageDown",
            _ => return None,
        }
    } else if &body[..semicolon] == b"1" {
        key(body[suffix_pos])?
    } else {
        return None;
    };
    let bits = modifier - b'1';
    let mut name = String::new();
    if bits & 4 != 0 {
        name.push_str("C-");
    }
    if bits & 2 != 0 {
        name.push_str("M-");
    }
    if bits & 1 != 0 {
        name.push_str("S-");
    }
    name.push_str(base);
    Some((suffix_pos + 3, name))
}

fn psmux_escape_sequence(buf: &[u8]) -> Option<(usize, String)> {
    if buf.first() != Some(&0x1b) {
        return None;
    }
    let (len, rest) = match *buf.get(1)? {
        b'[' | b'O' if buf.len() == 2 => (2, &buf[1..]),
        b'[' => {
            let body = &buf[2..];
            let params = body
                .iter()
                .take_while(|b| (0x30..=0x3f).contains(*b))
                .count();
            let inter = body[params..]
                .iter()
                .take_while(|b| (0x20..=0x2f).contains(*b))
                .count();
            let fin = *body.get(params + inter)?;
            if !(0x40..=0x7e).contains(&fin) {
                return None;
            }
            let end = 2 + params + inter + 1;
            if fin == b'M' && params + inter == 0 {
                // xterm's default/UTF-8 mouse encoding: three fields, each one
                // character offset by 32, after the final byte.
                let (fields, used) = mouse_fields(&buf[end..])?;
                let mut args = vec!["0x1b".to_string(), psmux_quote("[M")];
                args.extend(fields.iter().map(|c| format!("0x{:x}", *c as u32)));
                return Some((end + used, args.join(" ")));
            }
            (end, &buf[1..end])
        }
        b'O' => {
            let key = *buf.get(2)?;
            if !(0x40..=0x7e).contains(&key) {
                return None;
            }
            (3, &buf[1..3])
        }
        // Alt + a printable character (UTF-8 included); Alt + a control byte
        // stays Escape and that byte's key-name.
        first => {
            let end = 1 + utf8_width(first);
            let ch = std::str::from_utf8(buf.get(1..end)?).ok()?.chars().next()?;
            if ch.is_control() {
                return None;
            }
            (end, &buf[1..end])
        }
    };
    let rest = std::str::from_utf8(rest).ok()?;
    Some((len, format!("0x1b {}", psmux_literal_args(rest))))
}

fn mouse_fields(buf: &[u8]) -> Option<([char; 3], usize)> {
    let mut fields = ['\0'; 3];
    let mut used = 0;
    for field in &mut fields {
        let tail = buf.get(used..)?;
        let first = *tail.first()?;
        let width = utf8_width(first);
        match tail.get(..width).and_then(|s| std::str::from_utf8(s).ok()) {
            Some(s) => {
                *field = s.chars().next()?;
                used += width;
            }
            None => {
                *field = char::from(first);
                used += 1;
            }
        }
    }
    Some((fields, used))
}

fn utf8_width(first: u8) -> usize {
    match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn psmux_key_name(b: u8) -> Option<String> {
    Some(match b {
        b'\r' => "Enter".to_string(),
        b'\t' => "Tab".to_string(),
        0x1b => "Escape".to_string(),
        0x7f => "BSpace".to_string(),
        // Ctrl+letter: 0x01..=0x1a → C-a..C-z (covers e.g. LF 0x0a → C-j).
        0x01..=0x1a => format!("C-{}", (b'a' + b - 1) as char),
        _ => return None,
    })
}

fn flush_psmux_literal(pane_id: &str, literal: &mut Vec<u8>, cmds: &mut Vec<String>) {
    if literal.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(literal).into_owned();
    let emit = |chunk: &str, cmds: &mut Vec<String>| {
        cmds.push(format!(
            "send-keys -t {pane_id} -l -N 1 {}\n",
            psmux_literal_args(chunk)
        ));
    };
    let mut chunk = String::new();
    for ch in text.chars() {
        if !chunk.is_empty() && chunk.len() + ch.len_utf8() > SEND_KEYS_CHUNK_BYTES {
            emit(&chunk, cmds);
            chunk.clear();
        }
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        emit(&chunk, cmds);
    }
    literal.clear();
}

fn psmux_literal_args(run: &str) -> String {
    let mut args: Vec<String> = Vec::new();
    let mut rest = run;
    while let Some(ch) = rest.chars().next() {
        if !psmux_arg_is_reinterpreted(rest) {
            break;
        }
        args.push(format!("0x{:x}", ch as u32));
        rest = &rest[ch.len_utf8()..];
    }
    if !rest.is_empty() {
        args.push(psmux_quote(rest));
    }
    args.join(" ")
}

fn psmux_arg_is_reinterpreted(arg: &str) -> bool {
    if arg.starts_with('-') {
        return true;
    }
    arg.strip_prefix("0x")
        .or_else(|| arg.strip_prefix("0X"))
        .is_some_and(|hex| !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

pub fn psmux_quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    )
}

const PASTE_CHUNK_BYTES: usize = (WINDOWS_CMD_LINE_MAX - PASTE_LINE_RESERVE) * 9 / 32;

const PASTE_LINE_RESERVE: usize = 2048;

fn paste_chunks(text: &str) -> Vec<&str> {
    if text.len() <= PASTE_CHUNK_BYTES {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + PASTE_CHUNK_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

fn send_paste_args(pane_id: &str, text: &str) -> Vec<String> {
    vec![
        "send-paste".to_string(),
        "-t".to_string(),
        pane_id.to_string(),
        base64::engine::general_purpose::STANDARD.encode(text.as_bytes()),
    ]
}

#[derive(Debug, Clone)]
struct PsmuxPaste {
    transport: TmuxTransport,
    socket: String,
}

impl PsmuxPaste {
    fn new(transport: TmuxTransport, socket: String) -> Self {
        Self { transport, socket }
    }

    fn deliver(&self, pane_id: &str, text: &str) -> Result<()> {
        for (i, chunk) in paste_chunks(text).into_iter().enumerate() {
            if let Err(e) = self.send_one(pane_id, chunk) {
                if i == 0 {
                    return Err(e);
                }
                warn!("psmux send-paste truncated after {i} chunk(s): {e:#}");
                return Ok(());
            }
        }
        Ok(())
    }

    fn send_one(&self, pane_id: &str, text: &str) -> Result<()> {
        let args = send_paste_args(pane_id, text);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self
            .transport
            .tmux_command(&self.socket, &argv)?
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .context("failed to run psmux send-paste")?;
        if !out.status.success() {
            bail!(
                "psmux send-paste exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(argv: &[String]) -> String {
        argv.iter()
            .flat_map(|a| a.split(' '))
            .filter_map(|word| base64::engine::general_purpose::STANDARD.decode(word).ok())
            .map(|bytes| {
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect();
                String::from_utf16_lossy(&units)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[cfg(unix)]
    #[test]
    fn a_stalled_environment_write_is_killed_at_its_bound() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exec sleep 600"]);
        let why = run_fed_bounded(
            cmd,
            b"input".to_vec(),
            std::time::Duration::from_millis(200),
        )
        .unwrap_err()
        .to_string();
        assert!(why.contains("no answer within"), "{why}");
        let mut cat = Command::new("cat");
        cat.arg("-");
        let out = run_fed_bounded(cat, b"sent".to_vec(), std::time::Duration::from_secs(60))
            .expect("cat answers");
        assert_eq!(out.stdout, b"sent");
    }

    #[test]
    fn the_environment_file_is_created_owner_only() {
        assert!(!STASH_SCRIPT.contains("GetTempPath"), "{STASH_SCRIPT}");
        assert!(STASH_SCRIPT.contains("LOCALAPPDATA"), "{STASH_SCRIPT}");
        assert!(
            STASH_SCRIPT.contains("SetAccessRuleProtection($true, $false)")
                && STASH_SCRIPT
                    .contains("[IO.File]::Create($p, 4096, [IO.FileOptions]::None, $fs)"),
            "{STASH_SCRIPT}"
        );
    }

    fn script_argv(script: &str) -> Vec<String> {
        let rest = script
            .split_once("$v=@(")
            .map(|(_, rest)| rest)
            .unwrap_or_else(|| panic!("no $argv in the script: {script}"));
        let mut args = Vec::new();
        let mut chars = rest.chars().peekable();
        loop {
            match chars.next() {
                Some('\'') => {
                    let mut arg = String::new();
                    while let Some(c) = chars.next() {
                        if c == '\'' {
                            if chars.peek() == Some(&'\'') {
                                chars.next();
                                arg.push('\'');
                            } else {
                                break;
                            }
                        } else {
                            arg.push(c);
                        }
                    }
                    args.push(arg);
                }
                Some(',' | ' ') => {}
                _ => return args,
            }
        }
    }

    const AWKWARD_ARGS: &[&str] = &[
        "plain",
        "with space",
        "\"quoted\"",
        "it's",
        "{}",
        "$HOME",
        "`tick`",
        "{\"model\":\"x y\",\"tags\":[1,\"a\\\"b\"],\"path\":\"C:\\\\x\\\\\"}",
        "",
        "back\\slash\\",
        "%PATH%",
        "a&b|c>d<e^f",
        "trail\\\\",
        "new\nline",
        "x\" & echo pwned & \"y",
        "dir\\\" tail",
        "\"",
    ];

    #[test]
    fn a_psmux_window_hands_a_program_its_arguments_byte_exact() {
        let args: Vec<String> = AWKWARD_ARGS.iter().map(|a| a.to_string()).collect();
        let program =
            psmux_window_program("tb-k-agent", "C:/bin/echo-argv.exe", &args, &HashMap::new())
                .expect("program");
        let script = decoded(std::slice::from_ref(&program));
        assert_eq!(script_argv(&script), args, "{script}");
        assert!(
            !script.contains("& 'C:/bin/echo-argv.exe'"),
            "no `&` launch: {script}"
        );
    }

    #[test]
    fn a_psmux_windows_environment_is_in_no_argv() {
        let secret = "s3cret-value-42";
        let env = HashMap::from([("SECRET_TOKEN".to_string(), secret.to_string())]);
        let transport = crate::tmux_compat::transport::TmuxTransport::remote(
            crate::shell::HostLauncher::Ssh {
                destination: "me@winbox".into(),
                ssh_opts: vec![],
            },
            "psmux",
        )
        .with_shell(crate::shell::HostShell::PowerShell);
        let (stash, input) = stash_request(&transport, &env);
        let argv: Vec<String> = std::iter::once(stash.get_program())
            .chain(stash.get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let text = format!("{argv:?}{}", decoded(&argv));
        assert!(!text.contains(secret), "{text}");
        let sent = base64::engine::general_purpose::STANDARD
            .decode(&input)
            .expect("base64");
        assert!(String::from_utf8_lossy(&sent).contains(secret));

        let file = "C:/Users/me/AppData/Local/Temp/runtime-mux-env-x.json";
        let private = HashMap::from([(ENV_FILE.to_string(), file.to_string())]);
        let program = psmux_window_program("tb-k-agent", "claude", &[], &private).expect("program");
        let script = decoded(std::slice::from_ref(&program));
        assert!(
            script.contains(file) && script.contains("Remove-Item"),
            "{script}"
        );
        assert!(!format!("{program}{script}").contains(secret));
    }
    use crate::StatusChannel;
    #[cfg(unix)]
    use crate::{PaneIo, WindowLifecycle};

    #[cfg(unix)]
    fn cold_start_backend(root: &tempfile::TempDir, scenario: &str) -> PsmuxBackend {
        use std::os::unix::fs::PermissionsExt;

        let mux = root.path().join("psmux-probe");
        let session = root.path().join("session");
        let attempts = root.path().join("attempts");
        let script = format!(
            "#!/bin/sh\n\
             session='{}'\n\
             attempts='{}'\n\
             scenario='{}'\n\
             [ \"$1\" = -L ] && shift 2\n\
             case \"$1\" in\n\
               -V) echo 'psmux 3.3.8' ;;\n\
               has-session)\n\
                 if [ \"$scenario\" = destructive_probe ] && [ -f \"$session\" ]; then\n\
                   rm \"$session\"\n\
                   exit 1\n\
                 fi\n\
                 test -f \"$session\" ;;\n\
               list-windows)\n\
                 test -f \"$session\" || exit 1\n\
                 case \"$scenario\" in\n\
                   namesake_*)\n\
                     echo '%1|tb-check|0'\n\
                     if [ -f \"$attempts.window_created\" ]; then\n\
                       if [ \"$scenario\" = namesake_lives ]; then\n\
                         echo '%2|tb-check|0'\n\
                       elif [ \"$scenario\" = namesake_dies_later ] && \
                            [ ! -f \"$attempts.listed\" ]; then\n\
                         touch \"$attempts.listed\"\n\
                         echo '%2|tb-check|0'\n\
                       fi\n\
                     fi ;;\n\
                   *)\n\
                     if [ -f \"$attempts.window_created\" ] && [ \"$scenario\" != window_dies ]; then\n\
                       echo '%1|tb-check|0'\n\
                     fi ;;\n\
                 esac ;;\n\
               display-message) echo '3.3.8' ;;\n\
               new-session)\n\
                 if [ \"$scenario\" = needs_unsized_bootstrap ]; then\n\
                   for arg do\n\
                     if [ \"$arg\" = -x ] || [ \"$arg\" = -y ]; then\n\
                       echo \"psmux: failed to create session 'thurbox'\" >&2\n\
                       exit 1\n\
                     fi\n\
                   done\n\
                 fi\n\
                 count=0\n\
                 [ -f \"$attempts\" ] && count=$(wc -l < \"$attempts\")\n\
                 echo attempt >> \"$attempts\"\n\
                 if [ \"$scenario\" = delayed_after_refusal ]; then\n\
                   if [ \"$count\" -eq 0 ]; then\n\
                     (sleep 0.3; touch \"$session\") >/dev/null 2>&1 &\n\
                   fi\n\
                   echo \"psmux: failed to create session 'thurbox'\" >&2\n\
                   exit 1\n\
                 fi\n\
                 if [ \"$scenario\" = always_refused ] || \
                    {{ [ \"$scenario\" = refused_once ] && [ \"$count\" -eq 0 ]; }}; then\n\
                   echo \"psmux: failed to create session 'thurbox'\" >&2\n\
                   exit 1\n\
                 fi\n\
                 if [ \"$scenario\" = vanished_once ] && [ \"$count\" -eq 0 ]; then\n\
                   exit 0\n\
                 fi\n\
                 touch \"$session\" ;;\n\
               set-option)\n\
                 if [ ! -f \"$session\" ]; then\n\
                   echo 'psmux: no server running' >&2\n\
                   exit 1\n\
                 fi\n\
                 if [ \"$scenario\" = needs_server_target ]; then\n\
                   global=false; targeted=false; prev=''\n\
                   for arg do\n\
                     [ \"$arg\" = -g ] && global=true\n\
                     [ \"$prev\" = -t ] && [ \"$arg\" = thurbox ] && targeted=true\n\
                     prev=$arg\n\
                   done\n\
                   if [ \"$global\" = true ] && [ \"$targeted\" != true ]; then\n\
                     echo 'psmux: no server running on session private-socket__default' >&2\n\
                     exit 1\n\
                   fi\n\
                 fi\n\
                 if [ \"$scenario\" = transient_option_failure ]; then\n\
                   refused=0\n\
                   [ -f \"$attempts.option_refused\" ] && refused=$(wc -l < \"$attempts.option_refused\")\n\
                   if [ \"$refused\" -lt {option_refusals} ]; then\n\
                     echo refused >> \"$attempts.option_refused\"\n\
                     echo 'psmux: no server running on session' >&2\n\
                     exit 1\n\
                   fi\n\
                 fi\n\
                 if [ \"$scenario\" = live_option_failure ]; then\n\
                   echo 'invalid option' >&2\n\
                   exit 1\n\
                 fi ;;\n\
               new-window)\n\
                 if [ \"$scenario\" = transient_window_failure ] && \
                    [ ! -f \"$attempts.window_ready\" ]; then\n\
                   if [ ! -f \"$attempts.window_started\" ]; then\n\
                     touch \"$attempts.window_started\"\n\
                     (sleep 0.1; touch \"$attempts.window_ready\") >/dev/null 2>&1 &\n\
                   fi\n\
                   echo 'psmux: no server running on session' >&2\n\
                   exit 1\n\
                 fi\n\
                 touch \"$attempts.window_created\" ;;\n\
               *) exit 2 ;;\n\
             esac\n",
            session.display(),
            attempts.display(),
            scenario,
            option_refusals = Psmux::COLD_START_ATTEMPTS + 1,
        );
        std::fs::write(&mux, script).expect("write fake psmux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        PsmuxBackend::with_transport(
            TmuxTransport::local(mux.to_string_lossy()),
            "private-socket",
            "thurbox",
            "local:psmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        )
    }

    #[cfg(unix)]
    fn listing_backend(root: &tempfile::TempDir, stderr: &str, code: i32) -> PsmuxBackend {
        use std::os::unix::fs::PermissionsExt;

        let mux = root.path().join("psmux-listing");
        let script = format!("#!/bin/sh\necho \"{stderr}\" >&2\nexit {code}\n");
        std::fs::write(&mux, script).expect("write fake psmux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        PsmuxBackend::with_transport(
            TmuxTransport::local(mux.to_string_lossy()),
            "private-socket",
            "thurbox",
            "local:psmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        )
    }

    #[cfg(unix)]
    fn recording_backend(root: &tempfile::TempDir) -> (PsmuxBackend, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let mux = root.path().join("psmux-recording");
        let log = root.path().join("lines");
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\n\
             case \"$*\" in *' -V') echo 'psmux 3.3.8'; exit 0 ;; \
             *'#{{version}}'*) echo 3.3.8; exit 0 ;; esac\n\
             [ \"$3\" = display-message ] && echo 0\nexit 0\n",
            log.display()
        );
        std::fs::write(&mux, script).expect("write fake psmux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let backend = PsmuxBackend::with_transport(
            TmuxTransport::local(mux.to_string_lossy()),
            "private-socket",
            "thurbox",
            "local:psmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        );
        (backend, log)
    }

    #[cfg(unix)]
    #[test]
    fn a_long_send_text_goes_out_in_lines_a_windows_host_runs() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, log) = recording_backend(&root);
        let text = "é".repeat(PASTE_CHUNK_BYTES * 2);

        backend.send_text("%1", &text, false).expect("send");

        let lines = std::fs::read_to_string(&log).expect("log");
        let pastes: Vec<&str> = lines.lines().filter(|l| l.contains("send-paste")).collect();
        assert!(pastes.len() > 1, "{} paste line(s)", pastes.len());
        let mut sent = Vec::new();
        for line in &pastes {
            assert!(line.len() <= WINDOWS_CMD_LINE_MAX, "{} chars", line.len());
            let payload = line.rsplit(' ').next().expect("payload");
            sent.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .expect("base64"),
            );
        }
        assert_eq!(String::from_utf8(sent).expect("utf-8"), text);
    }

    #[cfg(unix)]
    #[test]
    fn a_deferred_prompt_past_the_windows_command_line_is_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, log) = recording_backend(&root);
        let prompt = "x".repeat(WINDOWS_CMD_LINE_MAX);

        let error = backend
            .send_text_after("%1", &prompt, std::time::Duration::from_secs(1))
            .unwrap_err();

        assert!(
            format!("{error:#}").contains("Windows limit is 8191"),
            "{error:#}"
        );
        assert!(!log.exists(), "{}", std::fs::read_to_string(&log).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_deferred_prompt_that_fits_is_scheduled() {
        let root = tempfile::tempdir().expect("tempdir");
        let (backend, log) = recording_backend(&root);

        backend
            .send_text_after("%1", "hello", std::time::Duration::from_secs(1))
            .expect("scheduled");

        let lines = std::fs::read_to_string(&log).expect("log");
        assert!(lines.contains("run-shell -b -d 1"), "{lines}");
    }

    #[cfg(unix)]
    #[test]
    fn psmux_no_server_reply_is_an_empty_listing() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = listing_backend(
            &root,
            "psmux: no server running on session 'private-socket__thurbox'",
            1,
        );
        assert!(backend.discover_answered().expect("absence").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn psmux_stderr_behind_a_zero_exit_is_still_an_error() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = listing_backend(&root, "psmux: something went wrong", 0);
        assert!(backend.discover_answered().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn psmux_no_server_reply_with_unexpected_stdout_is_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("tempdir");
        let mux = root.path().join("psmux-listing");
        std::fs::write(
            &mux,
            "#!/bin/sh\nprintf '%s\\n' 'malformed listing'\nprintf '%s\\n' \"psmux: no server running on session 'private-socket__thurbox'\" >&2\nexit 1\n",
        )
        .expect("write fake psmux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let backend = PsmuxBackend::with_transport(
            TmuxTransport::local(mux.to_string_lossy()),
            "private-socket",
            "thurbox",
            "local:psmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        );

        assert!(backend.discover_answered().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn psmux_no_server_reply_with_a_second_error_is_an_error() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = listing_backend(
            &root,
            "psmux: no server running on session 'private-socket__thurbox'\npermission denied",
            1,
        );
        assert!(backend.discover_answered().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn psmux_no_server_reply_mixed_with_a_same_line_error_is_an_error() {
        for stderr in [
            "permission denied: psmux: no server running on session 'private-socket__thurbox'",
            "psmux: no server running on session 'private-socket__thurbox': permission denied",
        ] {
            let root = tempfile::tempdir().expect("tempdir");
            let backend = listing_backend(&root, stderr, 1);
            assert!(backend.discover_answered().is_err(), "accepted: {stderr}");
        }
    }

    #[cfg(unix)]
    fn attempt_count(root: &tempfile::TempDir) -> usize {
        std::fs::read_to_string(root.path().join("attempts"))
            .expect("new-session was attempted")
            .lines()
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn psmux_retries_a_failed_cold_session_bootstrap() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "refused_once");

        backend
            .ensure_session_configured()
            .expect("cold psmux session recovers");
        assert_eq!(attempt_count(&root), 2);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_uses_its_default_initial_window_size() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "needs_unsized_bootstrap");

        backend
            .ensure_session_configured()
            .expect("psmux starts without explicit initial size");
        assert_eq!(attempt_count(&root), 1);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_avoids_a_destructive_has_session_probe() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "destructive_probe");

        backend
            .ensure_session_configured()
            .expect("bootstrap uses a non-destructive session probe");
        assert_eq!(attempt_count(&root), 1);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_waits_for_a_late_server_after_create_refusals() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "delayed_after_refusal");

        backend
            .ensure_session_configured()
            .expect("late server becomes available on the same socket");
        assert!(attempt_count(&root) <= 3);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_retries_when_a_successful_create_left_no_server() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "vanished_once");

        backend
            .ensure_session_configured()
            .expect("cold psmux session recovers");
        assert_eq!(attempt_count(&root), 2);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_does_not_retry_an_option_error_on_a_live_session() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "live_option_failure");

        let error = backend.ensure_session_configured().unwrap_err().to_string();
        assert!(error.contains("invalid option"), "{error}");
        assert_eq!(attempt_count(&root), 1);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_retries_a_transient_option_failure_on_a_live_session() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "transient_option_failure");

        backend
            .ensure_session_configured()
            .expect("psmux recovers after a temporary connection failure");
        assert_eq!(attempt_count(&root), 1);
        let refused = std::fs::read_to_string(root.path().join("attempts.option_refused"))
            .expect("set-option was refused");
        assert_eq!(refused.lines().count(), Psmux::COLD_START_ATTEMPTS + 1);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_targets_global_options_at_its_session() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "needs_server_target");

        backend
            .ensure_session_configured()
            .expect("global options route to the live psmux session");
        assert_eq!(attempt_count(&root), 1);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_retries_a_window_refused_during_cold_start() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "transient_window_failure");
        let env = HashMap::new();
        let spec = crate::contract::WindowSpec {
            owner: crate::contract::Owner::new("session-id", "check"),
            role: crate::contract::WindowRole::Agent,
            command: "cmd.exe",
            args: &[],
            cwd: None,
            env: &env,
        };

        backend
            .create_window(&spec)
            .expect("window appears after the server answers");
        assert!(root.path().join("attempts.window_created").exists());
    }

    #[cfg(unix)]
    #[test]
    fn psmux_refuses_a_window_that_died_at_once() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "window_dies");
        let env = HashMap::new();
        let spec = crate::contract::WindowSpec {
            owner: crate::contract::Owner::new("session-id", "check"),
            role: crate::contract::WindowRole::Agent,
            command: "no-such-agent",
            args: &[],
            cwd: None,
            env: &env,
        };

        let error = backend.create_window(&spec).unwrap_err().to_string();
        assert!(root.path().join("attempts.window_created").exists());
        assert!(error.contains("tb-check exited"), "{error}");
        assert!(error.contains("`no-such-agent`"), "{error}");
    }

    #[cfg(unix)]
    fn check_spec(env: &HashMap<String, String>) -> crate::contract::WindowSpec<'_> {
        crate::contract::WindowSpec {
            owner: crate::contract::Owner::new("session-id", "check"),
            role: crate::contract::WindowRole::Agent,
            command: "no-such-agent",
            args: &[],
            cwd: None,
            env,
        }
    }

    #[cfg(unix)]
    #[test]
    fn psmux_refuses_a_dead_window_next_to_a_live_namesake() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "namesake_dies");
        let env = HashMap::new();

        let error = backend
            .create_window(&check_spec(&env))
            .unwrap_err()
            .to_string();
        assert!(error.contains("tb-check exited"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn psmux_refuses_its_own_pane_dying_while_a_namesake_lives() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "namesake_dies_later");
        let env = HashMap::new();

        let error = backend
            .create_window(&check_spec(&env))
            .unwrap_err()
            .to_string();
        assert!(error.contains("tb-check exited"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn psmux_refuses_a_window_program_past_the_windows_command_line() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "ok");
        let env = HashMap::new();
        let args = vec![format!("--append-system-prompt={}", "x".repeat(3_500))];
        let spec = crate::contract::WindowSpec {
            args: &args,
            ..check_spec(&env)
        };

        let error = format!("{:#}", backend.create_window(&spec).unwrap_err());
        assert!(error.contains("Windows limit is 8191"), "{error}");
        assert!(
            !root.path().join("attempts.window_created").exists(),
            "new-window ran"
        );
    }

    #[cfg(unix)]
    #[test]
    fn psmux_answers_the_new_pane_beside_a_live_namesake() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "namesake_lives");
        let env = HashMap::new();

        let pane = backend.create_window(&check_spec(&env)).expect("created");
        assert_eq!(pane.as_str(), "%2");
    }

    #[cfg(unix)]
    #[test]
    fn psmux_startup_observation_requires_one_live_namesake() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "namesake_lives");
        let env = HashMap::new();
        let result = backend.create_window_observed(&check_spec(&env), std::time::Duration::ZERO);
        assert!(
            result.is_err(),
            "startup accepted ambiguous unstamped identity: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn psmux_startup_observation_confirms_the_created_pane_by_name() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "ok");
        let env = HashMap::new();
        let (pane, state) = backend
            .create_window_observed(&check_spec(&env), std::time::Duration::ZERO)
            .expect("observed created pane");
        assert_eq!(pane.as_str(), "%1");
        assert_eq!(state, crate::contract::BackendLiveness::Live);
    }

    #[cfg(unix)]
    #[test]
    fn psmux_stops_after_three_failed_cold_bootstraps() {
        let root = tempfile::tempdir().expect("tempdir");
        let backend = cold_start_backend(&root, "always_refused");

        let error = backend.ensure_session_configured().unwrap_err().to_string();
        assert!(error.contains("Failed to create psmux session"), "{error}");
        assert_eq!(attempt_count(&root), 3);
    }

    #[test]
    fn psmux_server_options_are_set_in_the_global_scope() {
        assert_eq!(Psmux::SERVER_SCOPE, "-g");
    }

    #[test]
    fn psmux_older_than_the_floor_is_refused_with_the_upgrade() {
        let err = check_psmux_version("3.3.6", "thurbox")
            .unwrap_err()
            .to_string();
        assert!(err.contains("3.3.6"), "{err}");
        assert!(err.contains("3.3.8"), "{err}");
        assert!(err.contains("`psmux -L thurbox kill-server`"), "{err}");
        assert!(check_psmux_version("3.3.6\n", "thurbox").is_err());
        assert!(check_psmux_version("3.3.5", "thurbox").is_err());
        assert!(check_psmux_version("3.2.9", "thurbox").is_err());
    }

    #[test]
    fn an_old_psmux_banner_is_refused_before_a_server_starts() {
        let err = Psmux::check_banner("tmux 3.3.6\n", "thurbox")
            .unwrap_err()
            .to_string();
        assert!(err.contains("psmux binary is 3.3.6, too old"), "{err}");
        assert!(!err.contains("kill-server"), "{err}");
        assert!(Psmux::check_banner("tmux 3.3.8\npsmux 3.3.8 (x)\n", "thurbox").is_ok());
    }

    #[test]
    fn an_old_binary_is_refused_beside_a_new_server() {
        let err = Psmux::check_banner("tmux 3.3.7\npsmux 3.3.7 (x)\n", "thurbox")
            .unwrap_err()
            .to_string();
        assert!(err.contains("psmux binary is 3.3.7, too old"), "{err}");
        assert!(!err.contains("kill-server"), "{err}");
    }

    #[test]
    fn psmux_3_3_8_and_newer_is_accepted() {
        for answer in ["3.3.8", "3.3.8\n", "3.4.0", "4.0"] {
            assert!(check_psmux_version(answer, "thurbox").is_ok(), "{answer}");
        }
        assert!(
            Psmux::check_banner("tmux 3.3.8\npsmux 3.3.8 (66cf613 2026-08-18)\n", "thurbox")
                .is_ok()
        );
    }

    #[test]
    fn psmux_3_3_7_is_refused_for_the_pane_targets_it_misreads() {
        let err = check_psmux_version("3.3.7", "thurbox")
            .unwrap_err()
            .to_string();
        assert!(err.contains("psmux 3.3.7 is too old"), "{err}");
        assert!(err.contains("3.3.8"), "{err}");
        assert!(err.contains("active pane"), "{err}");
        assert!(err.contains("`psmux -L thurbox kill-server`"), "{err}");
    }

    #[cfg(unix)]
    fn psmux_answering(version: &str) -> (tempfile::TempDir, PsmuxBackend, std::path::PathBuf) {
        psmux_asked_its_version(&format!("echo '{version}'"))
    }

    #[cfg(unix)]
    fn psmux_asked_its_version(
        answer: &str,
    ) -> (tempfile::TempDir, PsmuxBackend, std::path::PathBuf) {
        psmux_with_client("printf 'tmux 3.3.8\\npsmux 3.3.8 (test)\\n'", answer)
    }

    #[cfg(unix)]
    fn psmux_with_client(
        banner: &str,
        answer: &str,
    ) -> (tempfile::TempDir, PsmuxBackend, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("tempdir");
        let mux = root.path().join("psmux");
        std::fs::write(
            &mux,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$0.log\"\n\
                 case \"$*\" in\n  *' -V') {banner} ;;\n  *'#{{version}}'*) {answer} ;;\nesac\n"
            ),
        )
        .expect("write fake psmux");
        std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let backend = PsmuxBackend::with_transport(
            TmuxTransport::local(mux.to_string_lossy()),
            "test",
            "thurbox",
            "local:psmux",
            crate::settings::DEFAULT_SCROLLBACK_LINES,
        );
        (root, backend, mux.with_extension("log"))
    }

    #[cfg(unix)]
    #[test]
    fn no_pane_on_a_too_old_psmux_server_is_addressed() {
        let (_root, psmux, log) = psmux_answering("3.3.7");
        let err = psmux
            .send_key("%2", &crate::contract::Key::parse("Enter").unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("psmux 3.3.7 is too old"), "{err}");
        let sent = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!sent.contains("%2"), "a pane was named to it:\n{sent}");
    }

    #[cfg(unix)]
    #[test]
    fn no_pane_on_a_too_old_psmux_server_is_killed() {
        let (_root, psmux, log) = psmux_answering("3.3.7");
        let err = psmux.kill("%2").unwrap_err().to_string();
        assert!(err.contains("psmux 3.3.7 is too old"), "{err}");
        let sent = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!sent.contains("kill"), "a kill was sent:\n{sent}");
    }

    #[cfg(unix)]
    #[test]
    fn a_one_shot_admission_is_asked_for_each_pane() {
        let (_root, psmux, log) = psmux_answering("3.3.8");
        assert_eq!(psmux.target("%2").unwrap(), "thurbox:.%2");
        assert_eq!(psmux.target("%3").unwrap(), "thurbox:.%3");
        let sent = std::fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(sent.matches("#{version}").count(), 2, "{sent}");
    }

    #[cfg(unix)]
    #[test]
    fn an_old_psmux_client_names_no_pane_to_a_new_server() {
        let (_root, psmux, log) =
            psmux_with_client("printf 'tmux 3.3.7\\npsmux 3.3.7 (x)\\n'", "echo 3.3.8");
        let err = psmux.target("%2").unwrap_err().to_string();
        assert!(err.contains("binary is 3.3.7, too old"), "{err}");
        assert!(
            err.contains("restarting a server would not change this"),
            "{err}"
        );
        assert!(!err.contains("kill-server"), "{err}");
        let sent = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!sent.contains("%2"), "a pane was named:\n{sent}");
    }

    #[cfg(unix)]
    #[test]
    fn a_pre_release_psmux_binary_names_no_pane() {
        for banner in [
            "printf 'tmux 3.3.7\\npsmux 3.3.8-dev (x)\\n'",
            "printf 'tmux 3.3.9\\npsmux 3.3.9-dev (x)\\n'",
        ] {
            let (_root, psmux, log) = psmux_with_client(banner, "echo 3.3.8");
            let err = psmux.target("%2").unwrap_err().to_string();
            assert!(
                err.contains("did not give a version thurbox can hold to its floor"),
                "{banner}: {err}"
            );
            let sent = std::fs::read_to_string(&log).unwrap_or_default();
            assert!(!sent.contains("%2"), "a pane was named:\n{sent}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_version_names_no_pane() {
        for answer in ["echo 3", "echo 3.3.7a", "echo 3.4294967296"] {
            let (_root, psmux, _log) = psmux_asked_its_version(answer);
            let err = psmux.target("%2").unwrap_err().to_string();
            assert!(
                err.contains("the server did not say its version"),
                "{answer}: {err}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_server_that_fails_to_say_its_version_is_named_no_pane() {
        let (_root, psmux, log) =
            psmux_asked_its_version("echo 'psmux: command timed out' >&2; exit 1");
        let err = psmux
            .send_key("%2", &crate::contract::Key::parse("Enter").unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("could not ask the server its version"),
            "{err}"
        );
        let sent = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!sent.contains("%2"), "a pane was named to it:\n{sent}");
    }

    #[cfg(unix)]
    #[test]
    fn an_empty_answer_for_the_version_names_no_pane() {
        let (_root, psmux, log) = psmux_asked_its_version("true");
        let err = psmux.target("%2").unwrap_err().to_string();
        assert!(err.contains("the server did not say its version"), "{err}");
        let sent = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!sent.contains("%2"), "a pane was named to it:\n{sent}");
    }

    #[cfg(unix)]
    #[test]
    fn no_server_is_let_through_and_asked_again() {
        let (_root, psmux, log) = psmux_asked_its_version(
            "echo \"psmux: no server running on session 'test__thurbox'\" >&2; exit 1",
        );
        assert_eq!(psmux.target("%2").unwrap(), "thurbox:.%2");
        assert_eq!(psmux.target("%3").unwrap(), "thurbox:.%3");
        let sent = std::fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(sent.matches("#{version}").count(), 2, "{sent}");
    }

    #[cfg(unix)]
    #[test]
    fn a_status_zero_error_for_the_version_lets_through_only_a_missing_session() {
        let (_root, psmux, _log) =
            psmux_asked_its_version("echo \"ERROR: can't find session: thurbox\"");
        assert_eq!(psmux.target("%2").unwrap(), "thurbox:.%2");

        for answer in [
            "echo \"ERROR: can't find window: =tb-demo\"",
            "echo \"ERROR: can't find pane: %2\"",
            "echo 'ERROR: server is busy'",
        ] {
            let (_root, psmux, _log) = psmux_asked_its_version(answer);
            let err = psmux.target("%2").unwrap_err().to_string();
            assert!(
                err.contains("refused the version question"),
                "{answer}: {err}"
            );
        }
    }

    #[test]
    fn a_psmux_kill_reaches_the_pane_it_names_in_thurboxs_session() {
        let psmux = PsmuxBackend::local(&crate::settings::for_tests());
        let session = psmux.session.clone();
        assert_eq!(
            psmux.kill_spelling("%8"),
            ("kill-pane", format!("{session}:.%8").into())
        );
        assert_eq!(
            psmux.kill_spelling("@7"),
            ("kill-window", format!("{session}:@7").into())
        );
        assert_eq!(
            psmux.kill_spelling("thurbox:=tb-x"),
            ("kill-window", "thurbox:=tb-x".into())
        );
    }

    #[test]
    fn a_psmux_pane_target_names_thurboxs_session() {
        let psmux = PsmuxBackend::local(&crate::settings::for_tests());
        assert_eq!(psmux.spell_target("%2"), format!("{}:.%2", psmux.session));
    }

    #[test]
    fn a_psmux_close_is_confirmed_against_the_sessions_panes() {
        let policy = Psmux::control_policy(&TmuxTransport::local("psmux"), "thurbox");
        assert_eq!(
            policy.confirm_close.as_deref(),
            Some("list-panes -s -t \"thurbox\" -F \"#{pane_id}\"")
        );
    }

    #[test]
    fn an_unreadable_psmux_banner_is_not_refused() {
        assert!(Psmux::check_banner("", "thurbox").is_ok());
        assert!(Psmux::check_banner("psmux (dev build)", "thurbox").is_ok());
    }

    #[test]
    fn paste_prompt_args_uses_send_paste_for_psmux() {
        let args = Psmux::paste_args("thurbox:tb-demo", "line one\nline two").concat();
        assert_eq!(
            args,
            vec![
                "send-paste",
                "-t",
                "thurbox:tb-demo",
                "bGluZSBvbmUKbGluZSB0d28=",
            ]
        );
        assert!(!args.iter().any(|a| a.contains('\n') || a.contains('\x1b')));
    }

    fn decoded_script(program: &str) -> String {
        let encoded = program
            .strip_prefix("powershell.exe -NoLogo -EncodedCommand ")
            .unwrap_or_else(|| panic!("not an explicit PowerShell launch: {program}"));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64");
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).expect("UTF-16LE")
    }

    #[test]
    fn psmux_window_command_is_one_double_quoted_token() {
        let args = vec!["--session-id".to_string(), "abc-123".to_string()];
        let cmd = psmux_window_command("tbs-x", "claude", &args, &HashMap::new()).unwrap();
        let program = cmd
            .strip_prefix('"')
            .and_then(|c| c.strip_suffix('"'))
            .expect("double-quoted");
        let script = decoded_script(program);
        assert!(script.contains("Get-Command 'claude'"), "{script}");
        assert_eq!(script_argv(&script), args, "{script}");
    }

    #[test]
    fn the_window_program_names_powershell_and_carries_no_shell_syntax() {
        let mut env = HashMap::new();
        env.insert("THURBOX_SESSION".to_string(), "id-1".to_string());
        let args = vec!["it's".to_string(), "a & b | c; %PATH% $x".to_string()];
        let program = psmux_window_program("tb-x", "C:\\Tools\\claude.exe", &args, &env).unwrap();
        let tokens: Vec<&str> = program.split(' ').collect();
        assert_eq!(
            tokens[..3],
            ["powershell.exe", "-NoLogo", "-EncodedCommand"]
        );
        assert_eq!(tokens.len(), 4, "{program}");
        assert!(tokens[3]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')));
        assert_eq!(
            decoded_script(&program),
            psmux_window_powershell("tb-x", "C:\\Tools\\claude.exe", &args, &env)
        );

        let mut cmd = Command::new("psmux");
        Psmux::push_window_program(&mut cmd, "tb-x", "C:\\Tools\\claude.exe", &args, &env).unwrap();
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv, [program]);
    }

    #[test]
    fn a_typical_agent_window_fits_a_windows_command_line_with_room() {
        let id = "0b9f3c62-7d1e-4a55-9a43-2f6c1e8d7b10";
        let env: HashMap<String, String> = [
            ("THURBOX_SESSION", id),
            ("THURBOX_SOCKET", "thurbox"),
            (
                "THURBOX_METRICS_DIR",
                "C:\\Users\\user\\AppData\\Local\\thurbox\\metrics",
            ),
            (
                "THURBOX_DATA_DIR",
                "C:\\Users\\user\\AppData\\Local\\thurbox",
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let args = vec!["--session-id".to_string(), id.to_string()];

        let program = psmux_window_program("tb-check", "claude", &args, &env).unwrap();
        assert!(
            program.len() < WINDOWS_CMD_LINE_MAX / 2,
            "{}",
            program.len()
        );
    }

    #[test]
    fn psmux_window_script_folds_env_as_set_item() {
        // Sorted for determinism. Values with spaces survive the PS single
        // quotes.
        let mut env = HashMap::new();
        env.insert("THURBOX_SESSION".to_string(), "id-1".to_string());
        env.insert(
            "THURBOX_SESSION_NAME".to_string(),
            "a 'name' λ;$HOME *".to_string(),
        );
        env.insert("B".to_string(), "x y".to_string());
        let script = psmux_window_powershell("tbs-x", "claude", &[], &env);
        assert!(
            script.contains(
                "Set-Item Env:B 'x y'; Set-Item Env:THURBOX_SESSION 'id-1'; Set-Item \
                 Env:THURBOX_SESSION_NAME 'a ''name'' λ;$HOME *'; \n"
            ),
            "{script}"
        );
    }

    #[test]
    fn an_agent_window_reports_its_exit() {
        let script = psmux_window_powershell("tb-x", "claude", &[], &HashMap::new());
        let set = script
            .find("$global:LASTEXITCODE=$x.ExitCode")
            .expect("set");
        let report = script.find(EXIT_REPORT).expect("report");
        assert!(set < report && report < script.find("exit $x.ExitCode").expect("exit"));
        assert!(
            !psmux_window_powershell("tbs-x", "claude", &[], &HashMap::new()).contains(EXIT_REPORT)
        );
        assert!(!EXIT_REPORT.contains(['"', '\n', '$', '{', '}']));
    }

    #[test]
    fn only_an_agent_window_reports_its_exit() {
        for name in ["tbs-x", "tbp-x", "other"] {
            let cmd = psmux_window_powershell(name, "pwsh", &[], &HashMap::new());
            assert!(!cmd.contains("session exited"), "{name}: {cmd}");
        }
    }

    #[test]
    fn psmux_window_script_escapes_and_sanitizes() {
        let args = vec!["it's".to_string(), "say hi\nnow".to_string()];
        let script =
            psmux_window_powershell("tbs-x", "C:\\Tools\\claude.exe", &args, &HashMap::new());
        assert!(
            script.contains("Get-Command 'C:\\Tools\\claude.exe'"),
            "{script}"
        );
        assert!(script.contains("$v=@('it''s', 'say hi\nnow')"), "{script}");
    }

    #[test]
    fn an_env_value_keeps_its_double_quotes() {
        let name = "review \"API\"\nnext";
        let env = HashMap::from([("THURBOX_SESSION_NAME".to_string(), name.to_string())]);
        let script = psmux_window_powershell("tbs-x", "claude", &[], &env);
        assert!(
            script.contains("Set-Item Env:THURBOX_SESSION_NAME 'review \"API\"\nnext'; "),
            "{script}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_native_window_hands_its_child_the_exact_name() {
        if Command::new("node").arg("--version").output().is_err() {
            eprintln!("skipped: no node on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe.js");
        std::fs::write(
            &probe,
            "require('fs').writeFileSync(process.env.PROBE_OUT, JSON.stringify(\
             {env: process.env.THURBOX_SESSION_NAME, args: process.argv.slice(2)}));\n",
        )
        .unwrap();
        let shim = dir.path().join("probe.cmd");
        std::fs::write(&shim, "@node \"%~dp0probe.js\" %*\r\n").unwrap();
        let pwned = dir.path().join("pwned.txt");
        let names = [
            "review \"API\" 'x'".to_string(),
            "a\\\"b".to_string(),
            "trail \"q\" \\".to_string(),
            format!("x\" & echo pwned > \"{}\" & \"y", pwned.display()),
        ];
        let node = probe.display().to_string();
        let shim = shim.display().to_string();
        for (command, lead) in [("node", vec![node]), (shim.as_str(), vec![])] {
            for name in &names {
                let out = dir.path().join("out.json");
                let _ = std::fs::remove_file(&out);
                let env = HashMap::from([
                    ("THURBOX_SESSION_NAME".to_string(), name.clone()),
                    ("PROBE_OUT".to_string(), out.display().to_string()),
                ]);
                let mut args = lead.clone();
                args.push(name.clone());
                let program = psmux_window_program("tbs-x", command, &args, &env).unwrap();
                let tokens: Vec<&str> = program.split(' ').collect();
                let status = Command::new(tokens[0]).args(&tokens[1..]).status().unwrap();
                assert!(status.success(), "{command} {name}: {status}");
                let seen: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
                assert_eq!(
                    seen,
                    serde_json::json!({"env": name, "args": [name]}),
                    "{command}"
                );
                assert!(!pwned.exists(), "{command} ran part of {name:?}");
            }
        }
    }

    #[test]
    fn login_wrap_is_noop_for_psmux_remote() {
        // A Windows SSH host (multiplexer = "psmux") has no `/bin/sh`; wrapping
        // would replace the agent command with one that can't start at all.
        let host = crate::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let backend = PsmuxBackend::for_host(&host, &crate::settings::for_tests());
        assert_eq!(backend.login_wrap_for_remote("claude", None), "claude");
        let cmd = Psmux::window_command(&backend, "tbs-x", "claude", &[], &HashMap::new()).unwrap();
        let script = decoded_script(cmd.trim_matches('"'));
        assert!(
            script.contains("Get-Command 'claude'") && !script.contains("/bin/sh"),
            "the window's command is the PowerShell launch, never a POSIX wrap: {script}"
        );
    }

    #[test]
    fn a_deferred_prompt_names_the_servers_own_mux_and_socket() {
        let psmux = deferred_paste_script("psmux", "sock", "%3", "it's\nhere").expect("fits");
        assert!(psmux.starts_with("powershell -NoProfile -Command \"psmux -L 'sock' send-paste"));
        // Base64: the prompt's newline and quote never reach the script.
        assert!(!psmux.contains("it's"), "{psmux}");
    }

    #[test]
    fn a_deferred_prompt_quotes_a_socket_name_the_host_configured() {
        let psmux = deferred_paste_script("psmux", "my sock", "%3", "hi").expect("fits");
        assert!(psmux.contains("psmux -L 'my sock' send-paste"), "{psmux}");
        assert!(psmux.contains("psmux -L 'my sock' send-keys"), "{psmux}");
    }

    #[test]
    fn a_psmux_connection_expects_only_what_psmux_does() {
        let ssh = TmuxTransport::remote(
            HostLauncher::Ssh {
                destination: "me@winbox".into(),
                ssh_opts: Vec::new(),
            },
            "psmux",
        );
        for transport in [TmuxTransport::local("psmux"), ssh] {
            let policy = Psmux::control_policy(&transport, "thurbox");
            assert!(!policy.implicit_attach_reply);
            assert!(!policy.tagged_blocks);
            assert!(!policy.subscriptions);
            assert_eq!(
                policy.status_poll.is_some(),
                transport.is_remote() && Psmux::HOOK_STATUS
            );
        }
        // The session name is user-authored host entry text, quoted whole
        // rather than stripped of what psmux's tokenizer reads as escapes.
        assert_eq!(
            Psmux::hook_poll_command("a\"b\\c"),
            "list-panes -s -t \"a\\\"b\\\\c\" -F \"#{pane_id} #{@thurbox_state}\""
        );
        assert_eq!(
            live_panes_command("a\"b"),
            "list-panes -s -t \"a\\\"b\" -F \"#{pane_id}\""
        );
    }

    fn host_line(cmd: &Command) -> String {
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let dest = args.iter().position(|a| a == "me@winbox").expect("dest");
        args[dest + 1..].join(" ")
    }

    #[test]
    fn a_one_shot_reaches_a_windows_login_shell_as_plain_words() {
        let host = HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        let backend = PsmuxBackend::for_host(&host, &crate::settings::for_tests());
        let format = "#{pane_id}|#{window_name}|#{pane_dead}";
        let line = host_line(
            &backend
                .mux_command(&["list-windows", "-t", "thurbox", "-F", format])
                .expect("command"),
        );

        let syntax: Vec<char> = line
            .chars()
            .filter(|c| !(c.is_ascii_alphanumeric() || " -_./:=+%".contains(*c)))
            .collect();
        assert!(
            syntax.is_empty(),
            "{syntax:?} reaches the login shell: {line}"
        );

        let encoded = line.rsplit(' ').next().expect("payload");
        let utf16 = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64");
        let units: Vec<u16> = utf16
            .chunks(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let script = String::from_utf16(&units).expect("utf-16");
        assert!(
            script.contains(&format!(
                "$s.Arguments = '-L {} list-windows -t thurbox -F {format}'",
                crate::instance::host_socket(&host)
            )),
            "{script}"
        );
    }

    #[test]
    fn a_full_paste_chunk_fits_a_windows_command_line() {
        for (socket, encoded) in [(None, false), (Some("my sock"), true)] {
            let host = HostDef {
                name: "winbox".into(),
                destination: "me@winbox".into(),
                multiplexer: Some("psmux".into()),
                socket: socket.map(Into::into),
                ..Default::default()
            };
            let backend = PsmuxBackend::for_host(&host, &crate::settings::for_tests());
            for text in ["x".repeat(2500), "é".repeat(PASTE_CHUNK_BYTES)] {
                for paste in Psmux::paste_args("%123", &text) {
                    let argv: Vec<&str> = paste.iter().map(String::as_str).collect();
                    let line = host_line(&backend.mux_command(&argv).expect("fits"));
                    assert_eq!(line.contains("-EncodedCommand"), encoded, "{line}");
                }
            }
        }
    }

    #[test]
    fn psmux_is_never_read_as_stamping_its_windows() {
        let host = crate::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        for backend in [
            PsmuxBackend::local(&crate::settings::for_tests()),
            PsmuxBackend::for_host(&host, &crate::settings::for_tests()),
        ] {
            assert!(!backend.stamps_are_per_window(), "{}", backend.name());
            assert!(
                !backend.capabilities().supports_snapshots,
                "{}",
                backend.name()
            );
        }
    }

    // --- psmux send-keys encoding tests ---
    //
    // Regression: psmux 3.3.6 has no `send-keys -H`, so on Windows the hex path
    // injected the literal text "62" when the user typed `b` (0x62), and Enter /
    // Backspace did nothing. The psmux encoding must use `-l` literals + key-names.

    #[test]
    fn psmux_printable_char_uses_literal_not_hex() {
        // Typing `b` must inject `b`, not the literal text "62".
        assert_eq!(
            psmux_send_keys_commands("%1", b"b"),
            vec!["send-keys -t %1 -l -N 1 \"b\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_printable_run_is_one_literal_command() {
        assert_eq!(
            psmux_send_keys_commands("%1", b"hello world"),
            vec!["send-keys -t %1 -l -N 1 \"hello world\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_enter_backspace_tab_escape_use_key_names() {
        assert_eq!(
            psmux_send_keys_commands("%1", b"\r"),
            vec!["send-keys -t %1 Enter\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x7f]),
            vec!["send-keys -t %1 BSpace\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", b"\t"),
            vec!["send-keys -t %1 Tab\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x1b]),
            vec!["send-keys -t %1 Escape\n".to_string()]
        );
    }

    #[test]
    fn psmux_ctrl_letters_map_to_c_prefix() {
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x03]), // Ctrl+C
            vec!["send-keys -t %1 C-c\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x01]), // Ctrl+A
            vec!["send-keys -t %1 C-a\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x1a]), // Ctrl+Z
            vec!["send-keys -t %1 C-z\n".to_string()]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", &[0x0a]), // LF → Ctrl+J
            vec!["send-keys -t %1 C-j\n".to_string()]
        );
    }

    #[test]
    fn psmux_navigation_sequences_use_one_named_key() {
        for (bytes, name) in [
            (&b"\x1b[A"[..], "Up"),
            (&b"\x1b[B"[..], "Down"),
            (&b"\x1b[C"[..], "Right"),
            (&b"\x1b[D"[..], "Left"),
            (&b"\x1bOA"[..], "Up"),
            (&b"\x1bOB"[..], "Down"),
            (&b"\x1bOC"[..], "Right"),
            (&b"\x1bOD"[..], "Left"),
            (&b"\x1b[H"[..], "Home"),
            (&b"\x1b[F"[..], "End"),
            (&b"\x1b[5~"[..], "PageUp"),
            (&b"\x1b[6~"[..], "PageDown"),
            (&b"\x1b[1;5A"[..], "C-Up"),
            (&b"\x1b[1;2D"[..], "S-Left"),
            (&b"\x1b[5;5~"[..], "C-PageUp"),
            (&b"\x1b[6;2~"[..], "S-PageDown"),
        ] {
            assert_eq!(
                psmux_send_keys_commands("%1", bytes),
                vec![format!("send-keys -t %1 {name}\n")],
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn psmux_navigation_keeps_adjacent_text_and_unrecognized_sequences() {
        assert_eq!(
            psmux_send_keys_commands("%1", b"a\x1b[DZ"),
            vec![
                "send-keys -t %1 -l -N 1 \"a\"\n",
                "send-keys -t %1 Left\n",
                "send-keys -t %1 -l -N 1 \"Z\"\n",
            ]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b[200~"),
            vec!["send-keys -t %1 -l -N 1 0x1b \"[200~\"\n"]
        );
    }

    #[test]
    fn psmux_unnamed_escape_sequences_are_typed_whole() {
        for (bytes, args) in [
            // A mouse report: split, the pane got `[<35;10;5M` and no ESC.
            (&b"\x1b[<35;10;5M"[..], "0x1b \"[<35;10;5M\""),
            (&b"\x1b[<0;10;5m"[..], "0x1b \"[<0;10;5m\""),
            (&b"\x1b[15;5~"[..], "0x1b \"[15;5~\""),
            (&b"\x1bOP"[..], "0x1b \"OP\""),
            (&b"\x1bb"[..], "0x1b \"b\""),
            ("\x1bé".as_bytes(), "0x1b \"é\""),
            // A leading `-` is still kept from reading as a flag.
            (&b"\x1b-"[..], "0x1b 0x2d"),
            (&b"\x1b[M #!"[..], "0x1b \"[M\" 0x20 0x23 0x21"),
        ] {
            assert_eq!(
                psmux_send_keys_commands("%1", bytes),
                vec![format!("send-keys -t %1 -l -N 1 {args}\n")],
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn psmux_shift_enter_is_ctrl_j() {
        // Typed whole, `CSI 13;2u` did nothing in vibe's TUI or PowerShell.
        assert_eq!(
            psmux_send_keys_commands("%1", b"a\x1b[13;2ub"),
            vec![
                "send-keys -t %1 -l -N 1 \"a\"\n",
                "send-keys -t %1 C-j\n",
                "send-keys -t %1 -l -N 1 \"b\"\n",
            ]
        );
    }

    #[test]
    fn psmux_a_lone_or_unfinished_escape_stays_the_escape_key() {
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b\x1b"),
            vec!["send-keys -t %1 Escape\n", "send-keys -t %1 Escape\n"]
        );
        // Alt + a control byte: Escape, then that byte's key-name.
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b\r"),
            vec!["send-keys -t %1 Escape\n", "send-keys -t %1 Enter\n"]
        );
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b[13;2"),
            vec![
                "send-keys -t %1 Escape\n",
                "send-keys -t %1 -l -N 1 \"[13;2\"\n"
            ]
        );
    }

    #[test]
    fn psmux_a_sequence_between_text_keeps_its_neighbours() {
        assert_eq!(
            psmux_send_keys_commands("%1", b"x\x1b[<35;10;5My"),
            vec![
                "send-keys -t %1 -l -N 1 \"x\"\n",
                "send-keys -t %1 -l -N 1 0x1b \"[<35;10;5M\"\n",
                "send-keys -t %1 -l -N 1 \"y\"\n",
            ]
        );
    }

    #[test]
    fn psmux_literal_single_quote_survives() {
        // Regression: psmux's send-coalescing re-quoted literals with the
        // POSIX `'\''` escape its own parser can't read back, so `it's` was
        // typed into the pane as `it\s`. The `-N 1` opts out of coalescing and
        // the double-quote framing passes `'` through untouched.
        assert_eq!(
            psmux_send_keys_commands("%1", b"it's"),
            vec!["send-keys -t %1 -l -N 1 \"it's\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_literal_escapes_backslash_and_double_quote() {
        // psmux's double-quote tokenizer reads exactly `\"` and `\\`; both
        // must be escaped so Windows paths and quoted text round-trip.
        assert_eq!(
            psmux_send_keys_commands("%1", br#"say "hi" C:\p"#),
            vec!["send-keys -t %1 -l -N 1 \"say \\\"hi\\\" C:\\\\p\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_literal_escapes_a_leading_hyphen() {
        // Regression (#920): psmux classifies arguments after tokenizing, so
        // the quotes are gone by the time it drops everything starting with
        // `-` as a flag — a typed `-` was silently swallowed. It comes back as
        // psmux's own `0xNN` codepoint form, which decodes to the same char.
        assert_eq!(
            psmux_send_keys_commands("%1", b"-"),
            vec!["send-keys -t %1 -l -N 1 0x2d\n".to_string()]
        );
        // Only the leading hyphens need escaping; the rest stays one literal.
        assert_eq!(
            psmux_send_keys_commands("%1", b"--flag=a-b"),
            vec!["send-keys -t %1 -l -N 1 0x2d 0x2d \"flag=a-b\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_literal_escapes_a_hex_codepoint_lookalike() {
        // psmux rewrites a `0xNN` argument into the character it names, so a
        // run literally spelling `0x41` would have arrived as `A`.
        assert_eq!(
            psmux_send_keys_commands("%1", b"0x41"),
            vec!["send-keys -t %1 -l -N 1 0x30 \"x41\"\n".to_string()]
        );
        // A hyphen ahead of one still leaves a lookalike behind it.
        assert_eq!(
            psmux_send_keys_commands("%1", b"-0x41"),
            vec!["send-keys -t %1 -l -N 1 0x2d 0x30 \"x41\"\n".to_string()]
        );
        // Not a lookalike: text past the hex digits is ordinary literal text.
        assert_eq!(
            psmux_send_keys_commands("%1", b"0x41z"),
            vec!["send-keys -t %1 -l -N 1 \"0x41z\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_literal_args_escapes_an_all_hyphen_run() {
        assert_eq!(psmux_literal_args("---"), "0x2d 0x2d 0x2d");
        assert_eq!(psmux_literal_args(""), "");
    }

    #[test]
    fn psmux_mixed_text_then_enter() {
        // The common "type a command and submit" path.
        assert_eq!(
            psmux_send_keys_commands("%1", b"ls\r"),
            vec![
                "send-keys -t %1 -l -N 1 \"ls\"\n".to_string(),
                "send-keys -t %1 Enter\n".to_string(),
            ]
        );
    }

    #[test]
    fn psmux_bracketed_paste_keeps_markers_whole() {
        // The markers are typed whole like any other sequence, but a paste
        // still never takes this encoding: each CR in it would be Enter. The
        // writer sends it through `PsmuxPaste` instead.
        assert_eq!(
            psmux_send_keys_commands("%1", b"\x1b[200~hi\x1b[201~"),
            vec![
                "send-keys -t %1 -l -N 1 0x1b \"[200~\"\n".to_string(),
                "send-keys -t %1 -l -N 1 \"hi\"\n".to_string(),
                "send-keys -t %1 -l -N 1 0x1b \"[201~\"\n".to_string(),
            ]
        );
    }

    // --- psmux out-of-band paste ---

    #[test]
    fn paste_chunks_keeps_an_ordinary_paste_whole() {
        assert_eq!(paste_chunks("one\ntwo"), vec!["one\ntwo"]);
        let exact = "x".repeat(PASTE_CHUNK_BYTES);
        assert_eq!(paste_chunks(&exact), vec![exact.as_str()]);
    }

    #[test]
    fn paste_chunks_splits_large_input_on_char_boundaries() {
        // Multi-byte chars straddling the cut: 2 bytes each, odd-sized prefix.
        let text = format!("{}{}", "a", "é".repeat(PASTE_CHUNK_BYTES));
        let chunks = paste_chunks(&text);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= PASTE_CHUNK_BYTES));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn send_paste_args_targets_the_pane_with_a_base64_payload() {
        assert_eq!(
            send_paste_args("%7", "hi\nthere"),
            vec!["send-paste", "-t", "%7", "aGkKdGhlcmU="]
        );
    }

    #[test]
    fn send_paste_args_payload_is_wire_safe() {
        let args = send_paste_args("%1", "first\r\nsecond \"quoted\" \\ '");
        let payload = args.last().unwrap();
        assert!(!payload
            .bytes()
            .any(|b| matches!(b, b'\r' | b'\n' | b'"' | b'\\' | b'\'' | b' ')));
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .unwrap(),
            b"first\r\nsecond \"quoted\" \\ '"
        );
    }

    #[test]
    fn psmux_utf8_char_goes_to_literal() {
        assert_eq!(
            psmux_send_keys_commands("%1", "é".as_bytes()),
            vec!["send-keys -t %1 -l -N 1 \"é\"\n".to_string()]
        );
    }

    #[test]
    fn psmux_long_run_splits_on_char_boundary() {
        let input = "é".repeat(400); // 800 bytes, each char 2 bytes
        let cmds = psmux_send_keys_commands("%1", input.as_bytes());
        assert!(cmds.len() > 1, "expected a long run to span >1 command");
        // Reassemble the quoted literals back into the original text.
        let mut text = String::new();
        for cmd in &cmds {
            let inner = cmd
                .trim_end()
                .strip_prefix("send-keys -t %1 -l -N 1 \"")
                .and_then(|s| s.strip_suffix('"'))
                .expect("literal command shape");
            text.push_str(inner);
        }
        assert_eq!(text, input);
    }

    #[test]
    fn a_closed_status_channel_offers_and_reads_nothing() {
        let host = crate::HostDef {
            name: "winbox".into(),
            destination: "me@winbox.invalid".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        };
        for backend in [
            PsmuxBackend::local(&crate::settings::for_tests()),
            PsmuxBackend::for_host(&host, &crate::settings::for_tests()),
        ] {
            assert_eq!(backend.hook_signal_command(), None);
            assert!(backend.record_hook_state("%1", "working").is_err());
            assert!(backend.hook_states().is_err());
        }
    }

    #[test]
    fn the_hook_command_names_a_safe_socket() {
        let host = |socket: &str| crate::HostDef {
            name: "winbox".into(),
            destination: "me@winbox.invalid".into(),
            multiplexer: Some("psmux".into()),
            socket: Some(socket.into()),
            ..Default::default()
        };
        let command = |socket: &str| {
            Psmux::hook_signal_command(&PsmuxBackend::for_host(
                &host(socket),
                &crate::settings::for_tests(),
            ))
        };
        assert_eq!(
            command("we\"ird sock\\et"),
            "psmux -L weirdsocket set-option -p @thurbox_state "
        );
        assert_eq!(
            command("\"\\ "),
            format!(
                "psmux -L {} set-option -p @thurbox_state ",
                crate::instance::TMUX_SOCKET
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn psmuxs_own_absence_replies_read_as_absence() {
        crate::tmux_compat::server::assert_absence_replies::<Psmux>(&[
            ("psmux: no server running on session", true),
            (
                "psmux: no server running on session 'private-socket__thurbox'",
                true,
            ),
            ("can't find session: thurbox", true),
            ("session not found: thurbox", true),
            ("can't find pane: %2", false),
            ("can't find window: =tb-demo", false),
        ]);
    }

    #[test]
    fn a_banner_names_the_version_on_its_last_line_that_has_one() {
        // psmux's banner, the one the version question is asked of.
        let banner_version = |banner| {
            Psmux::banner_version(banner)
                .filter(|v| crate::tmux_compat::server::is_version_answer(v))
        };
        assert_eq!(
            banner_version("tmux 3.3.7\npsmux 3.3.7 (05cc5d4 2026-07-20)\n"),
            Some("3.3.7")
        );
        assert_eq!(banner_version("tmux 3.3.6\n"), Some("3.3.6"));
        assert_eq!(banner_version("psmux 3.4.0 (20aa1d7)"), Some("3.4.0"));
        for banner in [
            "",
            "3.3.8",
            "psmux",
            "psmux dev",
            "ERROR: x",
            // The last line names a pre-release, and an earlier one is not
            // read in its place.
            "tmux 3.3.7\npsmux 3.3.8-dev (x)\n",
            "tmux 3.3.9\npsmux 3.3.9-dev (x)\n",
        ] {
            assert_eq!(banner_version(banner), None, "{banner:?}");
        }
    }
}
