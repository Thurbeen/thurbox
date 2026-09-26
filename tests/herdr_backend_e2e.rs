use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver};
use tempfile::TempDir;
use thurbox::agent::tmux::WindowRole;
use thurbox::agent::{
    herdr::{HerdrBackend, BACKEND_TYPE},
    SessionBackend,
};

struct HerdrServer {
    child: Child,
    config: std::path::PathBuf,
}

struct EnvironmentGuard(Vec<(&'static str, Option<OsString>)>);

impl EnvironmentGuard {
    fn isolate(root: &std::path::Path, config: &std::path::Path) -> Self {
        let home = root.join("home");
        let config_home = home.join(".config");
        let data_home = home.join(".local/share");
        let state_home = home.join(".local/state");
        let socket = home.join("herdr.sock");
        for path in [&config_home, &data_home, &state_home] {
            std::fs::create_dir_all(path).expect("create isolated Herdr home");
        }
        std::fs::write(home.join(".zshrc"), "# isolated Herdr E2E shell\n")
            .expect("disable first-run shell prompt in isolated home");
        let values = [
            ("HOME", Some(home.as_os_str())),
            ("XDG_CONFIG_HOME", Some(config_home.as_os_str())),
            ("XDG_DATA_HOME", Some(data_home.as_os_str())),
            ("XDG_STATE_HOME", Some(state_home.as_os_str())),
            ("HERDR_CONFIG_PATH", Some(config.as_os_str())),
            ("HERDR_SOCKET_PATH", Some(socket.as_os_str())),
        ];
        let previous = values
            .iter()
            .map(|(key, value)| {
                let previous = std::env::var_os(key);
                std::env::set_var(key, value.unwrap_or_else(|| OsStr::new("")));
                (*key, previous)
            })
            .collect();
        Self(previous)
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

impl HerdrServer {
    fn start(config: std::path::PathBuf) -> Result<Self> {
        let child = Command::new("herdr")
            .env("HERDR_CONFIG_PATH", &config)
            .arg("server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start isolated Herdr server")?;
        let mut server = Self { child, config };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = server.child.try_wait()? {
                anyhow::bail!("isolated Herdr server exited early: {status}");
            }
            let mut command = Command::new("herdr");
            command
                .env("HERDR_CONFIG_PATH", &server.config)
                .args(["status", "server", "--json"]);
            let output = output_with_timeout(command, Duration::from_secs(1))?;
            if output.status.success()
                && serde_json::from_slice::<serde_json::Value>(&output.stdout)?["running"] == true
            {
                return Ok(server);
            }
            if Instant::now() >= deadline {
                anyhow::bail!("isolated Herdr server did not become ready within 10 seconds");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for HerdrServer {
    fn drop(&mut self) {
        if let Ok(mut stop) = Command::new("herdr")
            .env("HERDR_CONFIG_PATH", &self.config)
            .args(["server", "stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if matches!(stop.try_wait(), Ok(Some(_))) {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
            if matches!(stop.try_wait(), Ok(None)) {
                let _ = stop.kill();
            }
            let _ = stop.wait();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn isolated_json(config: &std::path::Path, args: &[&str]) -> Result<serde_json::Value> {
    let mut command = Command::new("herdr");
    command.env("HERDR_CONFIG_PATH", config).args(args);
    let output = output_with_timeout(command, Duration::from_secs(3))?;
    anyhow::ensure!(
        output.status.success(),
        "Herdr command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "parse Herdr output for {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stdout)
        )
    })?;
    anyhow::ensure!(value.get("error").is_none(), "Herdr API error: {value}");
    Ok(value)
}

fn output_with_timeout(mut command: Command, timeout: Duration) -> Result<std::process::Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    anyhow::bail!("Herdr command exceeded {timeout:?}")
}

fn pump_one_byte_reads(reader: Box<dyn Read + Send>) -> Receiver<Result<u8, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = reader;
        loop {
            let mut byte = [0u8; 1];
            match reader.read(&mut byte) {
                Ok(0) => return,
                Ok(_) if sender.send(Ok(byte[0])).is_err() => return,
                Ok(_) => {}
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    return;
                }
            }
        }
    });
    receiver
}

fn output_until(
    receiver: &Receiver<Result<u8, String>>,
    timeout: Duration,
    predicate: impl Fn(&[u8]) -> bool,
) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    while !predicate(&bytes) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(
            !remaining.is_zero(),
            "Herdr terminal output deadline expired after receiving {:?}",
            String::from_utf8_lossy(&bytes)
        );
        match receiver.recv_timeout(remaining) {
            Ok(Ok(byte)) => bytes.push(byte),
            Ok(Err(error)) => anyhow::bail!("Herdr terminal read failed: {error}"),
            Err(error) => anyhow::bail!(
                "Herdr terminal output deadline expired: {error}; received {:?}",
                String::from_utf8_lossy(&bytes)
            ),
        }
    }
    Ok(bytes)
}

#[test]
fn herdr_backend_can_be_selected_and_discovers_a_real_isolated_session() -> Result<()> {
    let version = Command::new("herdr").arg("--version").output()?;
    if !version.status.success() {
        anyhow::bail!("Herdr CLI unavailable");
    }
    let temp = TempDir::new()?;
    let config = temp.path().join("config.toml");
    let _environment = EnvironmentGuard::isolate(temp.path(), &config);
    std::fs::write(
        &config,
        "[terminal]\ndefault_shell = \"/bin/sh\"\nshell_mode = \"non_login\"\n\
         [update]\nversion_check = false\nmanifest_check = false\n",
    )?;
    let _server = HerdrServer::start(config.clone())?;

    let mut status_command = Command::new("herdr");
    status_command
        .env("HERDR_CONFIG_PATH", &config)
        .args(["status", "server", "--json"]);
    let status = output_with_timeout(status_command, Duration::from_secs(1))?;
    let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(status["version"], "0.9.1");
    assert_eq!(status["running"], true);
    assert!(
        status["socket"]
            .as_str()
            .is_some_and(|socket| socket.starts_with(temp.path().to_string_lossy().as_ref())),
        "isolated server socket escaped the temporary home: {status}"
    );

    // Drive the actual SessionBackend against the isolated real Herdr server.
    let backend = HerdrBackend::default();
    assert_eq!(backend.name(), BACKEND_TYPE);
    backend.ensure_ready()?;
    assert!(
        isolated_json(&config, &["pane", "list"])?["result"]["panes"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "the isolated server must start without user panes"
    );
    let command = "/bin/sh";
    let shell_arg = "space ; $(must-not-run) 'quoted'";
    let args = vec![
        "-c".to_string(),
        "printf 'herdr-e2e-ready:%s\\n' \"$1\"; stty -echo; od -An -tx1 -N 5; stty size; cat"
            .to_string(),
        "thurbox-child".to_string(),
        shell_arg.to_string(),
    ];
    let mut session = backend.spawn(
        "thurbox-e2e",
        command,
        &args,
        Some(temp.path()),
        &HashMap::new(),
        24,
        80,
    )?;
    let output = std::mem::replace(&mut session.output, Box::new(std::io::empty()));
    let output = pump_one_byte_reads(output);
    let session_id = uuid::Uuid::new_v4().to_string();
    let backend_id = session.backend_id.clone();
    let pane_info = isolated_json(&config, &["pane", "get", &session.backend_id])?;
    assert_eq!(
        pane_info["result"]["pane"]["label"], "thurbox-e2e",
        "pane get result shape: {pane_info}"
    );
    backend.stamp_window(&session.backend_id, &session_id, WindowRole::Agent)?;
    let found = backend.discover()?;
    assert!(
        found
            .iter()
            .any(|pane| pane.backend_id == session.backend_id
                && pane.session == session_id
                && pane.role == WindowRole::Agent),
        "stamped Herdr pane should be discoverable"
    );
    let expected = format!("herdr-e2e-ready:{shell_arg}");
    assert!(
        String::from_utf8(backend.capture_history(&session.backend_id)?)
            .unwrap_or_default()
            .contains(&expected),
        "pane run must preserve each argv token exactly"
    );

    // One-byte reads catch loss of decoded frame tails; every wait is bounded.
    let _initial_frame = output_until(&output, Duration::from_secs(10), |bytes| !bytes.is_empty())
        .context("waiting for the initial terminal frame")?;
    backend.resize(&session.backend_id, 30, 90)?;
    session.input.write_all(&[0xe2])?;
    session.input.write_all(&[0x82])?;
    session.input.write_all(&[0xac])?;
    session.input.write_all(&[0xff])?;
    session.input.write_all(b"\n")?;
    session.input.flush()?;
    let expected_bytes = b"e2 82 ac ff 0a";
    let observed = output_until(&output, Duration::from_secs(10), |bytes| {
        bytes.windows(b"30 90".len()).any(|part| part == b"30 90")
            && bytes
                .windows(expected_bytes.len())
                .any(|part| part == expected_bytes)
    })
    .context("waiting for resized dimensions and exact raw input bytes")?;
    assert!(
        observed
            .windows(b"30 90".len())
            .any(|part| part == b"30 90"),
        "PTY size did not change to 30 rows by 90 columns: {:?}",
        String::from_utf8_lossy(&observed)
    );
    assert!(
        observed
            .windows(expected_bytes.len())
            .any(|part| part == expected_bytes),
        "raw input bytes were not preserved: {:?}",
        String::from_utf8_lossy(&observed)
    );
    let input_history = backend.capture_history(&session.backend_id)?;
    assert!(input_history
        .windows(expected_bytes.len())
        .any(|part| part == expected_bytes));

    // Thurbox restart: close only the controller, then adopt the still-running
    // pane from its persisted Herdr pane id using a fresh backend instance.
    drop(session);
    drop(output);
    let restarted = HerdrBackend::default();
    restarted.ensure_ready()?;
    assert!(
        restarted
            .discover()?
            .iter()
            .any(|pane| pane.backend_id == backend_id
                && pane.session == session_id
                && pane.name == "thurbox-e2e"),
        "fresh backend must recover exact ownership and name"
    );
    let history = restarted.capture_history(
        &found
            .iter()
            .find(|p| p.session == session_id)
            .unwrap()
            .backend_id,
    )?;
    let backend_id = found
        .iter()
        .find(|p| p.session == session_id)
        .unwrap()
        .backend_id
        .clone();
    let mut adopted = restarted.adopt(&backend_id, 30, 90, Some(history))?;
    assert!(adopted.seed_len > 0);
    adopted.input.write_all(b"restart-input\\n")?;
    adopted.input.flush()?;
    restarted.kill(&backend_id)?;
    drop(adopted);
    Ok(())
}
