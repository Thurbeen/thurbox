//! Optional local Herdr backend, using Herdr's CLI for pane lifecycle and its
//! documented terminal-session control stream for live PTY bytes.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine;

use crate::agent::backend::{AdoptedSession, DiscoveredSession, SessionBackend, SpawnedSession};
use crate::agent::tmux::WindowRole;

fn parse_version(output: &str) -> Result<(u64, u64, u64)> {
    let token = output
        .split_whitespace()
        .find(|part| {
            part.trim_start_matches('v')
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
        .context("version number missing")?;
    let token = token.trim_start_matches('v');
    let token = token.split('+').next().unwrap_or(token);
    if token.contains('-') {
        bail!("pre-release Herdr versions are not supported");
    }
    let mut parts = token.split('.');
    let major = parts.next().context("major version missing")?.parse()?;
    let minor = parts.next().context("minor version missing")?.parse()?;
    let patch = parts.next().context("patch version missing")?.parse()?;
    Ok((major, minor, patch))
}

pub const BACKEND_TYPE: &str = "herdr";
const LABEL_PREFIX: &str = "thurbox";
const MINIMUM_VERSION: (u64, u64, u64) = (0, 9, 1);

struct TerminalStream {
    child: Arc<Mutex<Child>>,
    input: ChildStdin,
    lines: Receiver<std::io::Result<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn herdr_version_parser_accepts_minimum_and_newer_versions() {
        assert_eq!(parse_version("herdr 0.9.1").unwrap(), (0, 9, 1));
        assert_eq!(parse_version("herdr v0.10.0+build.2").unwrap(), (0, 10, 0));
        assert_eq!(parse_version("herdr 1.0.0").unwrap(), (1, 0, 0));
    }

    #[test]
    fn herdr_version_parser_rejects_old_and_malformed_versions() {
        assert!(parse_version("herdr 0.9.0").unwrap() < MINIMUM_VERSION);
        assert!(parse_version("herdr 0.9.1-rc.1").is_err());
        assert!(parse_version("herdr unknown").is_err());
    }

    #[test]
    fn reader_skips_empty_frames_and_reads_the_next_frame() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(r#"{"type":"terminal.frame","data":""}"#.into()))
            .unwrap();
        sender
            .send(Ok(r#"{"type":"terminal.frame","data":"eA=="}"#.into()))
            .unwrap();
        let child = Command::new("sh").arg("-c").arg("sleep 5").spawn().unwrap();
        let mut reader = StreamReader {
            child: Arc::new(Mutex::new(child)),
            lines: receiver,
            pending_line: None,
            pending: VecDeque::new(),
        };
        let mut byte = [0];
        assert_eq!(reader.read(&mut byte).unwrap(), 1);
        assert_eq!(byte, [b'x']);
    }
}

#[derive(Default)]
pub struct HerdrBackend {
    streams: Mutex<HashMap<String, Arc<Mutex<ChildStdin>>>>,
}

impl HerdrBackend {
    fn close_pane(&self, pane: &str) {
        if let Err(error) = self.cli(&["pane", "close", pane]) {
            tracing::warn!(
                pane,
                "could not close Herdr pane after spawn failure: {error:#}"
            );
        }
    }

    fn cli(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new("herdr")
            .args(args)
            .output()
            .context("Herdr CLI is unavailable; install Herdr and add `herdr` to PATH")?;
        if !output.status.success() {
            bail!(
                "Herdr command failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output)
    }

    fn json(&self, args: &[&str]) -> Result<serde_json::Value> {
        let output = self.cli(args)?;
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).context("invalid JSON from Herdr CLI")?;
        if let Some(error) = value.get("error") {
            bail!("Herdr API error: {error}");
        }
        Ok(value)
    }

    fn terminal(&self, pane: &str, cols: u16, rows: u16) -> Result<TerminalStream> {
        let mut child = Command::new("herdr")
            .args([
                "terminal",
                "session",
                "control",
                pane,
                "--cols",
                &cols.to_string(),
                "--rows",
                &rows.to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start Herdr terminal control stream")?;
        let input = child.stdin.take().context("Herdr control stdin")?;
        let output = child.stdout.take().context("Herdr control stdout")?;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut lines = BufReader::new(output);
            loop {
                let mut line = String::new();
                match lines.read_line(&mut line) {
                    Ok(0) => return,
                    Ok(_) => {
                        if sender.send(Ok(line)).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        return;
                    }
                }
            }
        });
        Ok(TerminalStream {
            child: Arc::new(Mutex::new(child)),
            input,
            lines: receiver,
        })
    }

    fn wait_initial_frame(
        lines: &Receiver<std::io::Result<String>>,
        timeout: Duration,
    ) -> Result<String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                bail!("timed out waiting for Herdr's initial terminal frame");
            }
            let line = lines
                .recv_timeout(remaining)
                .context("waiting for Herdr's initial terminal frame")??;
            let record: serde_json::Value = serde_json::from_str(&line)?;
            if record["type"] == "terminal.frame" {
                return Ok(line);
            }
            if record["type"] == "terminal.closed" {
                bail!("Herdr closed the control stream before its initial frame");
            }
        }
    }

    fn start_command(&self, pane: &str, command: &str, args: &[String]) -> Result<()> {
        let mut invocation = vec![command.to_string()];
        invocation.extend(args.iter().cloned());
        if invocation.is_empty() {
            bail!("empty Herdr command");
        }
        let shell_command = invocation
            .iter()
            .map(|token| format!("'{}'", token.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ");
        let mut argv = vec!["pane".to_string(), "run".to_string(), pane.to_string()];
        argv.push(shell_command);
        let mut cmd = Command::new("herdr");
        cmd.args(argv);
        let out = cmd.output().context("launch command in Herdr pane")?;
        if !out.status.success() {
            bail!(
                "Herdr pane run failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    fn create_pane(
        &self,
        label: &str,
        cwd: Option<&Path>,
        rows: u16,
        cols: u16,
        env: &HashMap<String, String>,
    ) -> Result<String> {
        let cwd = cwd.unwrap_or_else(|| Path::new("."));
        let cwd = cwd.to_string_lossy();
        let mut create = vec![
            "workspace".to_string(),
            "create".to_string(),
            "--cwd".to_string(),
            cwd.to_string(),
            "--label".to_string(),
            label.to_string(),
        ];
        for (key, value) in env {
            create.push("--env".into());
            create.push(format!("{key}={value}"));
        }
        let refs = create.iter().map(String::as_str).collect::<Vec<_>>();
        let created = self.json(&refs)?;
        let pane = created["result"]["root_pane"]["pane_id"]
            .as_str()
            .context("Herdr root pane id missing")?
            .to_string();
        self.cli(&["pane", "rename", &pane, label])?;
        let _ = (rows, cols);
        Ok(pane)
    }
}

struct StreamReader {
    child: Arc<Mutex<Child>>,
    lines: Receiver<std::io::Result<String>>,
    pending_line: Option<String>,
    pending: VecDeque<u8>,
}
impl Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.pending.is_empty() {
            let count = buf.len().min(self.pending.len());
            for (dest, byte) in buf[..count].iter_mut().zip(self.pending.drain(..count)) {
                *dest = byte;
            }
            return Ok(count);
        }
        loop {
            let line = match self.pending_line.take() {
                Some(line) => line,
                None => match self.lines.recv() {
                    Ok(Ok(line)) => line,
                    Ok(Err(error)) => return Err(error),
                    Err(_) => return Ok(0),
                },
            };
            let record: serde_json::Value =
                serde_json::from_str(&line).map_err(std::io::Error::other)?;
            if record["type"] == "terminal.closed" {
                return Ok(0);
            }
            if record["type"] != "terminal.frame" {
                continue;
            }
            let encoded = record
                .get("data")
                .or_else(|| record.get("bytes"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| std::io::Error::other("Herdr frame has no byte payload"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(std::io::Error::other)?;
            if bytes.is_empty() {
                continue;
            }
            let count = bytes.len().min(buf.len());
            buf[..count].copy_from_slice(&bytes[..count]);
            self.pending.extend(bytes.into_iter().skip(count));
            return Ok(count);
        }
    }
}
impl Drop for StreamReader {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct StreamWriter {
    child: Arc<Mutex<Child>>,
    input: Arc<Mutex<ChildStdin>>,
}
impl Write for StreamWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let bytes_base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let payload = serde_json::json!({"type":"terminal.input","bytes":bytes_base64});
        let mut input = self
            .input
            .lock()
            .map_err(|_| std::io::Error::other("Herdr input lock poisoned"))?;
        serde_json::to_writer(&mut *input, &payload).map_err(std::io::Error::other)?;
        input.write_all(b"\n")?;
        input.flush()?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.input
            .lock()
            .map_err(|_| std::io::Error::other("Herdr input lock poisoned"))?
            .flush()
    }
}
impl Drop for StreamWriter {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl SessionBackend for HerdrBackend {
    fn name(&self) -> &str {
        BACKEND_TYPE
    }
    fn check_available(&self) -> Result<()> {
        if !cfg!(any(target_os = "linux", target_os = "macos")) {
            bail!("Herdr backend is supported only on Linux and macOS");
        }
        let out = Command::new("herdr")
            .arg("--version")
            .output()
            .context("Herdr CLI is unavailable; install Herdr and add `herdr` to PATH")?;
        if !out.status.success() {
            bail!("Herdr CLI is unavailable");
        }
        let version_output = String::from_utf8_lossy(&out.stdout);
        let version =
            parse_version(&version_output).context("could not parse Herdr CLI version")?;
        if version < MINIMUM_VERSION {
            bail!(
                "Herdr 0.9.1 or newer is required; found {}.{}.{}",
                version.0,
                version.1,
                version.2
            );
        }
        Ok(())
    }
    fn ensure_ready(&self) -> Result<()> {
        self.check_available()?;
        let out = self.cli(&["status", "server", "--json"])?;
        let status: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        if status["running"] != true {
            bail!("Herdr server is not running; start it with `herdr server`");
        }
        Ok(())
    }
    fn spawn(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> Result<SpawnedSession> {
        self.ensure_ready()?;
        let pane = self.create_pane(window_name, cwd, rows, cols, env)?;
        let TerminalStream {
            child,
            input,
            lines,
        } = match self.terminal(&pane, cols, rows) {
            Ok(stream) => stream,
            Err(error) => {
                self.close_pane(&pane);
                return Err(error);
            }
        };
        let initial_frame = match Self::wait_initial_frame(&lines, Duration::from_secs(5)) {
            Ok(frame) => frame,
            Err(error) => {
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                self.close_pane(&pane);
                bail!("Herdr terminal did not become ready: {error:#}");
            }
        };
        let input = Arc::new(Mutex::new(input));
        self.streams
            .lock()
            .map_err(|_| anyhow::anyhow!("Herdr stream lock poisoned"))?
            .insert(pane.clone(), input.clone());
        if let Err(error) = self.start_command(&pane, command, args) {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
                let _ = child.wait();
            }
            self.streams
                .lock()
                .map_err(|_| anyhow::anyhow!("Herdr stream lock poisoned"))?
                .remove(&pane);
            self.close_pane(&pane);
            return Err(error);
        }
        let output = StreamReader {
            child: child.clone(),
            lines,
            pending_line: Some(initial_frame),
            pending: VecDeque::new(),
        };
        let writer = StreamWriter { child, input };
        Ok(SpawnedSession {
            backend_id: pane,
            output: Box::new(output),
            input: Box::new(writer),
            size: None,
        })
    }
    fn adopt(
        &self,
        backend_id: &str,
        rows: u16,
        cols: u16,
        seed: Option<Vec<u8>>,
    ) -> Result<AdoptedSession> {
        let TerminalStream {
            child,
            input,
            lines,
        } = self.terminal(backend_id, cols, rows)?;
        let input = Arc::new(Mutex::new(input));
        self.streams
            .lock()
            .map_err(|_| anyhow::anyhow!("Herdr stream lock poisoned"))?
            .insert(backend_id.to_string(), input.clone());
        let output = StreamReader {
            child: child.clone(),
            lines,
            pending_line: None,
            pending: VecDeque::new(),
        };
        let writer = StreamWriter { child, input };
        let seed = seed.unwrap_or_default();
        let seed_len = seed.len();
        Ok(AdoptedSession {
            output: Box::new(std::io::Cursor::new(seed).chain(output)),
            input: Box::new(writer),
            seed_len,
            size: None,
        })
    }
    fn capture_history(&self, backend_id: &str) -> Result<Vec<u8>> {
        let out = self.cli(&[
            "pane", "read", backend_id, "--source", "visible", "--ansi", "--raw",
        ])?;
        Ok(out.stdout)
    }
    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        self.ensure_ready()?;
        let value = self.json(&["pane", "list"])?;
        let panes = value["result"]["panes"]
            .as_array()
            .context("Herdr pane list missing")?;
        Ok(panes
            .iter()
            .filter_map(|p| {
                let label = p["label"].as_str()?;
                let (session, role, name) = parse_label(label)?;
                Some(DiscoveredSession {
                    backend_id: p["pane_id"].as_str()?.to_string(),
                    name,
                    is_alive: p["exited"].as_bool() != Some(true),
                    session: session.to_string(),
                    role,
                })
            })
            .collect())
    }
    fn stamp_window(&self, backend_id: &str, session_id: &str, role: WindowRole) -> Result<()> {
        let pane = self.json(&["pane", "get", backend_id])?;
        let name = pane["result"]["pane"]["label"]
            .as_str()
            .context("Herdr pane label missing")?;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(name);
        let label = format!("{LABEL_PREFIX}:{session_id}:{}:{encoded}", role.as_str());
        self.cli(&["pane", "rename", backend_id, &label])?;
        Ok(())
    }
    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        let streams = self
            .streams
            .lock()
            .map_err(|_| anyhow::anyhow!("Herdr stream lock poisoned"))?;
        let input = streams
            .get(backend_id)
            .context("Herdr pane has no active control stream")?;
        let mut input = input
            .lock()
            .map_err(|_| anyhow::anyhow!("Herdr input lock poisoned"))?;
        serde_json::to_writer(
            &mut *input,
            &serde_json::json!({"type":"terminal.resize","cols":cols,"rows":rows}),
        )?;
        input.write_all(b"\n")?;
        input.flush()?;
        Ok(())
    }
    fn is_dead(&self, backend_id: &str) -> Result<bool> {
        let value = self.json(&["pane", "get", backend_id])?;
        Ok(value["result"]["pane"]["exited"] == true)
    }
    fn kill(&self, backend_id: &str) -> Result<()> {
        self.cli(&["pane", "close", backend_id])?;
        Ok(())
    }
    fn detach(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        let value = self.json(&["pane", "process-info", backend_id])?;
        Ok(value["result"]["process_info"]["shell_pid"]
            .as_u64()
            .map(|n| n as u32))
    }
}

fn parse_label(label: &str) -> Option<(&str, WindowRole, String)> {
    let mut parts = label.splitn(4, ':');
    if parts.next()? != LABEL_PREFIX {
        return None;
    }
    let session = parts.next()?;
    let role = match parts.next()? {
        "agent" => WindowRole::Agent,
        "shell" => WindowRole::Shell,
        "program" => WindowRole::Program,
        _ => return None,
    };
    let name = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts.next()?)
        .ok()?;
    Some((session, role, String::from_utf8(name).ok()?))
}
