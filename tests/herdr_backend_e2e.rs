#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::os::unix::fs::PermissionsExt;
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
            ("TMUX_TMPDIR", Some(root.as_os_str())),
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

fn contains_terminal_text(bytes: &[u8], text: &[u8]) -> bool {
    strip_terminal_controls(bytes)
        .windows(text.len())
        .any(|part| part == text)
}

fn strip_terminal_controls(bytes: &[u8]) -> Vec<u8> {
    let mut text = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b || index + 1 == bytes.len() {
            text.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        match bytes[index] {
            b'[' => {
                index += 1;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            b']' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    text
}

#[test]
fn herdr_backend_can_be_selected_and_discovers_a_real_isolated_session() -> Result<()> {
    let version = match Command::new("herdr").arg("--version").output() {
        Ok(version) => version,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping real Herdr E2E: herdr binary is not installed");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if !version.status.success() {
        anyhow::bail!("Herdr CLI version command failed: {version:?}");
    }
    let temp = TempDir::new()?;
    let config = temp.path().join("config.toml");
    let _environment = EnvironmentGuard::isolate(temp.path(), &config);
    thurbox::paths::set_test_dir(temp.path().join("thurbox-home"));
    // A fake transport executes SSH/WSL commands on this isolated host. The
    // Herdr server is real; only the remote launcher is substituted.
    let transport_bin = temp.path().join("remote-bin");
    std::fs::create_dir(&transport_bin)?;
    let ssh = transport_bin.join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\nwhile [ \"$1\" = -o ]; do shift 2; done\n[ \"$1\" = fake-host ] || exit 98\nshift\nexec /bin/sh -c \"$*\"\n",
    )?;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))?;
    let wsl = transport_bin.join("wsl.exe");
    std::fs::write(
        &wsl,
        "#!/bin/sh\nif [ \"$1\" = -l ]; then printf 'FakeWSL\\n'; exit 0; fi\n[ \"$1\" = -d ] || exit 98\nshift 2\nif [ \"$1\" = --cd ]; then shift 2; fi\n[ \"$1\" = --exec ] && shift\nif [ \"$1\" = herdr ]; then\n  if [ \"$2\" = pane ] && [ \"$3\" = run ]; then printf '%s\\n' \"$5\" >> \"$HERDR_CONFIG_PATH.wsl-pane-run\"; fi\n  exec \"$@\"\nfi\nexec /bin/sh -c \"$*\"\n",
    )?;
    std::fs::set_permissions(&wsl, std::fs::Permissions::from_mode(0o755))?;
    let hosts_path = thurbox::agent::host_config::hosts_config_path().context("hosts path")?;
    if let Some(parent) = hosts_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &hosts_path,
        "[[hosts]]\nname = \"probe-ssh\"\ndestination = \"fake-host\"\nshare_sessions = false\nmultiplexer = \"herdr\"\n\
         [[hosts]]\nname = \"probe-wsl\"\nkind = \"wsl\"\ndistro = \"FakeWSL\"\nshare_sessions = false\nmultiplexer = \"herdr\"\n",
    )?;
    let old_path = std::env::var_os("PATH").context("PATH missing")?;
    let remote_path = std::env::join_paths(
        std::iter::once(transport_bin.as_path()).chain(
            std::env::split_paths(&old_path)
                .collect::<Vec<_>>()
                .iter()
                .map(std::path::PathBuf::as_path),
        ),
    )?;
    std::env::set_var("PATH", &remote_path);
    let settings_path = thurbox::agent::settings_config::settings_config_path()
        .context("settings path for default-change probe")?;
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&settings_path, "multiplexer = \"herdr\"\n")?;
    let (initial_settings, warnings) =
        thurbox::agent::settings_config::load_or_seed_with_warnings();
    anyhow::ensure!(
        warnings.is_empty(),
        "invalid initial local default: {warnings:?}"
    );
    thurbox::session::settings::init(initial_settings);
    anyhow::ensure!(
        thurbox::session::settings::global().multiplexer.as_deref() == Some("herdr"),
        "settings.toml did not select Herdr for new local sessions"
    );
    std::fs::write(
        &config,
        "[terminal]\ndefault_shell = \"/bin/sh\"\nshell_mode = \"non_login\"\n\
         [update]\nversion_check = false\nmanifest_check = false\n",
    )?;
    let phases = std::sync::Mutex::new(Vec::new());
    let absent_server = thurbox::session_ops::spawn::spawn_session_headless_with_progress(
        &thurbox::storage::Database::open_in_memory()?,
        thurbox::session_ops::spawn::SpawnRequest {
            name: "herdr-not-ready".into(),
            repo_path: temp.path().to_owned(),
            command: Some("/bin/sh".into()),
            multiplexer: Some("herdr".into()),
            ..Default::default()
        },
        Some(&|phase| phases.lock().unwrap().push(phase)),
    );
    anyhow::ensure!(
        absent_server
            .as_ref()
            .is_err_and(|error| error.contains("Herdr server is not running")),
        "server readiness was not checked before create: {absent_server:?}"
    );
    anyhow::ensure!(
        *phases.lock().unwrap() == vec![thurbox::session_ops::spawn::SpawnPhase::Resolving],
        "Herdr create reached hooks or worktrees before checking readiness: {phases:?}"
    );
    let _server = HerdrServer::start(config.clone())?;

    let mut status_command = Command::new("herdr");
    status_command
        .env("HERDR_CONFIG_PATH", &config)
        .args(["status", "server", "--json"]);
    let status = output_with_timeout(status_command, Duration::from_secs(1))?;
    let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert!(
        status["version"].as_str().is_some(),
        "missing Herdr version: {status}"
    );
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
    // Force the first rename to fail after workspace creation. The pane must
    // be closed even though spawn has not yet received its pane id.
    let real_herdr = std::env::split_paths(&std::env::var_os("PATH").context("PATH missing")?)
        .map(|dir| dir.join("herdr"))
        .find(|path| path.is_file())
        .context("Herdr binary missing from PATH")?;
    let wrapper_dir = temp.path().join("wrapper-bin");
    std::fs::create_dir(&wrapper_dir)?;
    let wrapper = wrapper_dir.join("herdr");
    let quoted_binary = real_herdr.to_string_lossy().replace('\'', "'\\''");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nif [ \"$1\" = pane ] && [ \"$2\" = rename ]; then\n  if [ \"$4\" = thurbox-rename-failure ]; then\n    echo 'forced initial rename failure' >&2\n    exit 42\n  fi\n  if [ \"$HERDR_E2E_FAIL_STAMP\" = 1 ]; then\n    case \"$4\" in thurbox:*) echo 'forced stamp failure' >&2; exit 42;; esac\n  fi\nfi\nexec '{quoted_binary}' \"$@\"\n"),
    )?;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
    let old_path = std::env::var_os("PATH").context("PATH missing")?;
    let wrapped_path = std::env::join_paths(
        std::iter::once(wrapper_dir.as_path()).chain(
            std::env::split_paths(&old_path)
                .collect::<Vec<_>>()
                .iter()
                .map(std::path::PathBuf::as_path),
        ),
    )?;
    std::env::set_var("PATH", &wrapped_path);
    let failed_rename = backend.spawn(
        "thurbox-rename-failure",
        "/bin/true",
        &[],
        Some(temp.path()),
        &HashMap::new(),
        24,
        80,
    );
    std::env::set_var("PATH", &old_path);
    anyhow::ensure!(
        failed_rename
            .as_ref()
            .is_err_and(|error| error.to_string().contains("forced initial rename failure")),
        "expected injected rename failure, got {:?}",
        failed_rename.as_ref().err()
    );
    anyhow::ensure!(
        isolated_json(&config, &["pane", "list"])?["result"]["panes"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "failed initial rename leaked a Herdr pane"
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
    assert!(
        session.size.is_none(),
        "Herdr delegates grid size to the UI rect"
    );
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
    assert!(
        session.size.is_none(),
        "resize keeps the UI rect authoritative"
    );
    session.input.write_all(&[0xe2])?;
    session.input.write_all(&[0x82])?;
    session.input.write_all(&[0xac])?;
    session.input.write_all(&[0xff])?;
    session.input.write_all(b"\n")?;
    session.input.flush()?;
    let expected_bytes = b"e2 82 ac ff 0a";
    let observed = output_until(&output, Duration::from_secs(10), |bytes| {
        contains_terminal_text(bytes, b"30")
            && contains_terminal_text(bytes, b"90")
            && (contains_terminal_text(bytes, expected_bytes)
                || contains_terminal_text(bytes, b"e282acff0a"))
    })
    .context("waiting for resized dimensions and exact raw input bytes")?;
    assert!(
        contains_terminal_text(&observed, b"30") && contains_terminal_text(&observed, b"90"),
        "PTY size output did not contain 30 rows and 90 columns: {:?}",
        String::from_utf8_lossy(&observed)
    );
    assert!(
        contains_terminal_text(&observed, expected_bytes)
            || contains_terminal_text(&observed, b"e282acff0a"),
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
    assert!(
        adopted.size.is_none(),
        "adopted Herdr panes follow the UI rect"
    );
    assert!(adopted.seed_len > 0);
    let output = std::mem::replace(&mut adopted.output, Box::new(std::io::empty()));
    let output = pump_one_byte_reads(output);
    adopted.input.write_all(b"restart-input\n")?;
    adopted.input.flush()?;
    let observed = output_until(&output, Duration::from_secs(10), |bytes| {
        bytes
            .windows(b"restart-input".len())
            .any(|part| part == b"restart-input")
    })
    .context("waiting for live output after adopting the pane")?;
    assert!(observed
        .windows(b"restart-input".len())
        .any(|part| part == b"restart-input"));
    restarted
        .kill(&backend_id)
        .context("closing the directly spawned Herdr probe pane")?;
    drop(output);
    drop(adopted);

    // Exercise Thurbox's headless create/restart path as well as the backend
    // contract directly. The isolated TMUX_TMPDIR makes an accidental tmux
    // respawn fail without reaching a user's server.
    let repo = temp.path().join("restart-repo");
    std::fs::create_dir_all(&repo)?;
    std::fs::write(repo.join("README.md"), "Herdr restart probe\n")?;
    for args in [
        vec!["init", "-b", "main"],
        vec!["add", "README.md"],
        vec![
            "-c",
            "user.name=Herdr E2E",
            "-c",
            "user.email=herdr-e2e@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    ] {
        let output = Command::new("git").args(args).current_dir(&repo).output()?;
        anyhow::ensure!(
            output.status.success(),
            "git setup failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let failed_db = thurbox::storage::Database::open_in_memory()?;
    failed_db.conn_ref().execute_batch(
        "CREATE TRIGGER herdr_e2e_reject_insert BEFORE INSERT ON sessions \
         BEGIN SELECT RAISE(ABORT, 'forced Herdr E2E insert failure'); END;",
    )?;
    let panes_before = isolated_json(&config, &["pane", "list"])?["result"]["panes"]
        .as_array()
        .context("Herdr panes missing before forced persistence failure")?
        .iter()
        .filter_map(|pane| pane["pane_id"].as_str().map(str::to_owned))
        .collect::<std::collections::HashSet<_>>();
    let failure = thurbox::session_ops::spawn::spawn_session_headless(
        &failed_db,
        thurbox::session_ops::spawn::SpawnRequest {
            name: "herdr-persistence-failure-e2e".into(),
            repo_path: repo.clone(),
            command: Some("/bin/sh".into()),
            args: vec!["-c".into(), "sleep 30".into()],
            multiplexer: Some("herdr".into()),
            ..Default::default()
        },
    );
    anyhow::ensure!(
        failure
            .as_ref()
            .is_err_and(|error| error.contains("forced Herdr E2E insert failure")),
        "expected the injected persistence failure, got {failure:?}"
    );
    let panes_after = isolated_json(&config, &["pane", "list"])?["result"]["panes"]
        .as_array()
        .context("Herdr panes missing after forced persistence failure")?
        .iter()
        .filter_map(|pane| pane["pane_id"].as_str().map(str::to_owned))
        .collect::<std::collections::HashSet<_>>();
    anyhow::ensure!(
        panes_after == panes_before,
        "failed DB upsert leaked a Herdr pane: before {panes_before:?}, after {panes_after:?}"
    );
    let db = thurbox::storage::Database::open_in_memory()?;
    let spawned = thurbox::session_ops::spawn::spawn_session_headless(
        &db,
        thurbox::session_ops::spawn::SpawnRequest {
            name: "herdr-restart-e2e".into(),
            repo_path: repo.clone(),
            worktree_branch: None,
            base_branch: None,
            existing_worktree: None,
            agent: None,
            command: Some("/bin/sh".into()),
            args: vec!["-c".into(), "while :; do sleep 1; done".into()],
            env: HashMap::new().into_iter().collect(),
            resume_session_id: None,
            agent_session_id: None,
            host: None,
            multiplexer: None,
            parent_session_id: None,
            task_id: None,
            extra_repos: Vec::new(),
            fork_session_id: None,
            inherit_worktrees: Vec::new(),
        },
    )
    .map_err(anyhow::Error::msg)?;
    std::fs::write(&settings_path, "multiplexer = \"tmux\"\n")?;
    let (new_settings, warnings) = thurbox::agent::settings_config::load_or_seed_with_warnings();
    anyhow::ensure!(
        warnings.is_empty(),
        "invalid new local default: {warnings:?}"
    );
    anyhow::ensure!(
        new_settings.multiplexer.as_deref() == Some("tmux"),
        "settings.toml did not switch the future local default"
    );
    let restarted_session =
        thurbox::session_ops::restart::restart_session_headless(&db, spawned.session_id)
            .map_err(anyhow::Error::msg)?;
    let row = db
        .get_session_by_id(spawned.session_id)?
        .context("restarted Herdr session disappeared")?;
    anyhow::ensure!(
        row.backend_type == BACKEND_TYPE && row.backend_id != spawned.backend_id,
        "headless restart did not replace the Herdr pane: {row:?}"
    );
    anyhow::ensure!(
        restarted.discover()?.iter().any(|pane| {
            pane.backend_id == row.backend_id
                && pane.session == spawned.session_id.to_string()
                && pane.name == "herdr-restart-e2e"
        }),
        "headless restart persisted a pane id that Herdr cannot discover"
    );
    anyhow::ensure!(
        restarted_session.hook_failures.is_empty(),
        "headless restart hooks failed: {:?}",
        restarted_session.hook_failures
    );
    restarted
        .kill(&row.backend_id)
        .context("closing the restarted Herdr lifecycle pane")?;
    let panes_before = isolated_json(&config, &["pane", "list"])?["result"]["panes"]
        .as_array()
        .context("Herdr panes missing before forced restart stamp failure")?
        .iter()
        .filter_map(|pane| pane["pane_id"].as_str().map(str::to_owned))
        .collect::<std::collections::HashSet<_>>();
    let wrapper_path = std::env::var_os("PATH").context("PATH missing")?;
    std::env::set_var("PATH", &wrapped_path);
    std::env::set_var("HERDR_E2E_FAIL_STAMP", "1");
    let stamp_failure =
        thurbox::session_ops::restart::restart_session_headless(&db, spawned.session_id);
    std::env::remove_var("HERDR_E2E_FAIL_STAMP");
    std::env::set_var("PATH", wrapper_path);
    anyhow::ensure!(
        stamp_failure
            .as_ref()
            .is_err_and(|error| error.contains("forced stamp failure")),
        "expected injected restart stamp failure, got {stamp_failure:?}"
    );
    let panes_after = isolated_json(&config, &["pane", "list"])?["result"]["panes"]
        .as_array()
        .context("Herdr panes missing after forced restart stamp failure")?
        .iter()
        .filter_map(|pane| pane["pane_id"].as_str().map(str::to_owned))
        .collect::<std::collections::HashSet<_>>();
    anyhow::ensure!(
        panes_after == panes_before,
        "failed restart stamp leaked a Herdr pane: before {panes_before:?}, after {panes_after:?}"
    );

    for (index, host_name) in ["probe-ssh", "probe-wsl"].into_iter().enumerate() {
        if index == 1 {
            std::fs::write(
                &hosts_path,
                "[[hosts]]\nname = \"probe-ssh\"\ndestination = \"fake-host\"\nshare_sessions = false\nmultiplexer = \"tmux\"\n\
                 [[hosts]]\nname = \"probe-wsl\"\nkind = \"wsl\"\ndistro = \"FakeWSL\"\nshare_sessions = false\nmultiplexer = \"herdr\"\n",
            )?;
        }
        let db = thurbox::storage::Database::open_in_memory()?;
        let spawned = thurbox::session_ops::spawn::spawn_session_headless(
            &db,
            thurbox::session_ops::spawn::SpawnRequest {
                name: format!("herdr-{host_name}"),
                repo_path: repo.clone(),
                command: Some("/bin/sh".into()),
                args: vec!["-c".into(), "echo remote-ready; cat".into()],
                host: Some(host_name.into()),
                multiplexer: None,
                ..Default::default()
            },
        )
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("create Herdr session on {host_name}"))?;
        if host_name == "probe-wsl" {
            let log = std::fs::read_to_string(format!("{}.wsl-pane-run", config.display()))?;
            anyhow::ensure!(
                log.lines()
                    .any(|arg| arg == "'/bin/sh' '-c' 'echo remote-ready; cat'"),
                "WSL changed Herdr pane run's command argument: {log:?}"
            );
        }
        let row = db
            .get_session_by_id(spawned.session_id)?
            .context("remote Herdr row missing")?;
        anyhow::ensure!(
            row.backend_type.ends_with(":herdr"),
            "remote Herdr choice was not persisted: {}",
            row.backend_type
        );
        // Each host chose Herdr through hosts.toml. Change its default before
        // adopting or restarting; the persisted suffix must keep Herdr.
        std::fs::write(
            &hosts_path,
            "[[hosts]]\nname = \"probe-ssh\"\ndestination = \"fake-host\"\nshare_sessions = false\nmultiplexer = \"tmux\"\n\
             [[hosts]]\nname = \"probe-wsl\"\nkind = \"wsl\"\ndistro = \"FakeWSL\"\nshare_sessions = false\nmultiplexer = \"tmux\"\n",
        )?;
        // A fresh process reloads hosts.toml before constructing its backend.
        // This in-process test must use the cold loader because the TUI's
        // configured-host registry intentionally caches its startup snapshot.
        let hosts = thurbox::agent::host_config::load_all();
        let host = hosts
            .resolved_by_backend(&row.backend_type)
            .with_context(|| format!("host {} was not resolved", row.backend_type))?;
        let remote = HerdrBackend::from_host(&host, row.backend_type.clone());
        anyhow::ensure!(
            remote.discover()?.iter().any(|pane| {
                pane.backend_id == row.backend_id
                    && pane.name == row.name
                    && pane.session == spawned.session_id.to_string()
            }),
            "fresh remote Herdr backend did not rediscover the original name on {host_name}"
        );
        let mut adopted = remote.adopt(&row.backend_id, 24, 80, None)?;
        let output = std::mem::replace(&mut adopted.output, Box::new(std::io::empty()));
        let output = pump_one_byte_reads(output);
        adopted.input.write_all(b"remote-control\n")?;
        let observed = output_until(&output, Duration::from_secs(10), |bytes| {
            contains_terminal_text(bytes, b"remote-control")
        })?;
        anyhow::ensure!(
            contains_terminal_text(&observed, b"remote-control"),
            "remote Herdr control stream did not echo input"
        );
        drop(adopted);
        drop(output);
        thurbox::session_ops::restart::restart_session_headless(&db, spawned.session_id)
            .map_err(anyhow::Error::msg)?;
        let restarted = db
            .get_session_by_id(spawned.session_id)?
            .context("restarted remote Herdr row missing")?;
        anyhow::ensure!(
            restarted.backend_id != row.backend_id,
            "remote Herdr restart reused the old pane"
        );
        let captured = remote.capture_history(&restarted.backend_id)?;
        anyhow::ensure!(
            !captured.is_empty(),
            "remote Herdr capture returned no pane data on {host_name}"
        );
        thurbox::session_ops::delete_session_headless(&db, spawned.session_id, false)
            .map_err(anyhow::Error::msg)?;
        let restored =
            thurbox::session_ops::restore_session_headless(&db, spawned.session_id, false)
                .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            restored.respawn_error.is_none(),
            "remote Herdr restore failed: {:?}",
            restored.respawn_error
        );
        let restored_row = db
            .get_session_by_id(spawned.session_id)?
            .context("restored remote Herdr row missing")?;
        anyhow::ensure!(
            restored_row.backend_type == row.backend_type,
            "remote restore changed the persisted backend"
        );
        let deleted = thurbox::session_ops::delete_session_headless(&db, spawned.session_id, true)
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            deleted.killed_window,
            "remote Herdr force-delete did not close pane"
        );
        remote
            .kill(&restored_row.backend_id)
            .context("closing an already absent Herdr pane should be idempotent")?;
    }
    let override_db = thurbox::storage::Database::open_in_memory()?;
    let overridden = thurbox::session_ops::spawn::spawn_session_headless(
        &override_db,
        thurbox::session_ops::spawn::SpawnRequest {
            name: "herdr-explicit-remote".into(),
            repo_path: repo.clone(),
            command: Some("/bin/sh".into()),
            args: vec!["-c".into(), "sleep 30".into()],
            host: Some("probe-wsl".into()),
            multiplexer: Some("herdr".into()),
            ..Default::default()
        },
    )
    .map_err(anyhow::Error::msg)?;
    let overridden_row = override_db
        .get_session_by_id(overridden.session_id)?
        .context("explicit remote override row missing")?;
    anyhow::ensure!(
        overridden_row.backend_type == "wsl:probe-wsl:herdr",
        "explicit Herdr did not override hosts.toml: {}",
        overridden_row.backend_type
    );
    thurbox::session_ops::delete_session_headless(&override_db, overridden.session_id, true)
        .map_err(anyhow::Error::msg)?;
    let mirrored_db = thurbox::storage::Database::open_in_memory()?;
    let mirrored_id: thurbox::session::SessionId = uuid::Uuid::new_v4().to_string().parse()?;
    let listed = serde_json::json!([{
        "id": mirrored_id.to_string(),
        "name": "herdr-shared-host",
        "backend_type": "herdr",
        "backend_id": "pane-from-host"
    }]);
    let first_mirror = thurbox::session_ops::mirror::reconcile_with(
        &mirrored_db,
        "ssh:probe-ssh",
        &listed,
        &serde_json::json!([]),
        thurbox::session_ops::mirror::Transitive::Hide,
    );
    anyhow::ensure!(first_mirror.unknown_local.is_empty());
    let mirrored = mirrored_db
        .get_session_by_id(mirrored_id)?
        .context("shared host's Herdr row was not mirrored")?;
    anyhow::ensure!(
        mirrored.backend_type == "ssh:probe-ssh:herdr",
        "shared-host mirror lost Herdr choice: {}",
        mirrored.backend_type
    );
    // A later sync must still recognize the host's Herdr row. It must not
    // report the same live session as unknown just because the default mux
    // pass also ran.
    let second_mirror = thurbox::session_ops::mirror::reconcile_with(
        &mirrored_db,
        "ssh:probe-ssh:herdr",
        &listed,
        &serde_json::json!([]),
        thurbox::session_ops::mirror::Transitive::Hide,
    );
    anyhow::ensure!(
        second_mirror.unknown_local.is_empty(),
        "repeat Herdr mirror reported a known session as unknown: {:?}",
        second_mirror.unknown_local
    );
    let mirrored_again = mirrored_db
        .get_session_by_id(mirrored_id)?
        .context("repeat mirror lost the Herdr session")?;
    anyhow::ensure!(
        mirrored_again.backend_type == "ssh:probe-ssh:herdr",
        "repeat mirror changed the persisted mux: {}",
        mirrored_again.backend_type
    );
    std::env::set_var("PATH", &old_path);
    Ok(())
}
