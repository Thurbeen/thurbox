//! Installed-artifact acceptance: no checkout, Git, shell or network tool.
mod support;

use std::fs;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use support::{Env, wait};

fn installed(env: &Env, args: &[&str]) -> Command {
    let bin = env
        .root
        .join(if cfg!(windows) { "fleet.exe" } else { "fleet" });
    if !bin.exists() {
        // A standalone hard link avoids a writable-copy/exec race between
        // parallel test forks; unlinking the build path leaves it runnable.
        fs::hard_link(env!("CARGO_BIN_EXE_fleet"), &bin).unwrap();
    }
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env_clear()
        .current_dir(&env.root)
        .env("HOME", &env.root)
        .env("XDG_CONFIG_HOME", env.root.join("config"))
        .env("XDG_DATA_HOME", env.root.join("local-data"))
        .env("APPDATA", env.root.join("config"))
        .env("LOCALAPPDATA", env.root.join("local-data"))
        .env("PATH", env.root.join("empty-path"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if cfg!(windows) {
        for key in ["SYSTEMROOT", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
    }
    cmd
}

/// `config` with `runtime` as its one `[runtime]`, in place of the
/// `[runtimes]` and `[hosts]` that `init` writes.
fn set_runtime(config: &mut toml::Value, runtime: toml::Value) {
    let table = config.as_table_mut().unwrap();
    table.remove("runtimes");
    table.remove("hosts");
    table.insert("runtime".into(), runtime);
}

fn customize_config(path: &std::path::Path) -> Vec<u8> {
    let mut config: toml::Value = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    config["runtimes"]["mux"]["callTimeoutMs"] = toml::Value::Integer(4321);
    let bytes = format!(
        "{}\n# Private local customization\n",
        toml::to_string(&config).unwrap()
    )
    .into_bytes();
    fs::write(path, &bytes).unwrap();
    bytes
}

fn assert_config_instruction(brief: &str, expected: &std::path::Path) {
    use serde::Deserialize;
    let (_, encoded) = brief
        .rsplit_once("Fleet configuration path (JSON string): ")
        .expect("explicit config instruction");
    let mut parser = serde_json::Deserializer::from_str(encoded);
    let emitted = std::path::PathBuf::deserialize(&mut parser).expect("usable JSON config path");
    assert_eq!(
        fs::canonicalize(emitted).unwrap(),
        fs::canonicalize(expected).unwrap(),
        "the brief must identify the effective config file regardless of native path spelling"
    );
}

struct Running(std::process::Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(40);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "automatic progress did not happen"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn fake_bootstrap(name: &str) -> Env {
    fake_bootstrap_with(name, &["init"])
}

/// Bootstrap with `init_args`, then point the generated config at the fake
/// runtime and the test store.
fn fake_bootstrap_with(name: &str, init_args: &[&str]) -> Env {
    let mut env = Env::new(name);
    env.set_var(
        "LOCALAPPDATA",
        &env.root.join("local-data").to_string_lossy(),
    );
    env.set_var("APPDATA", &env.root.join("config").to_string_lossy());
    // An explicit fresh config uses the same discovery and bootstrap machinery.
    fs::remove_file(&env.config).unwrap();
    env.fleet(init_args).ok();
    let generated = fs::read_to_string(&env.config).unwrap();
    let mut config: toml::Value = toml::from_str(&generated).unwrap();
    // The three shipped ready policies, exactly as the config page shows
    // them, and no default policy: a draft is marked ready only when a
    // Task names one.
    assert!(
        generated.contains(
            "[forge.readyPolicies.checks]\nchecksPassed = true\n\n[forge.readyPolicies.checks-and-bots]\nbotReviewsAddressed = true\nchecksPassed = true\n\n[forge.readyPolicies.strict]\nbotReviewsAddressed = true\nchecksPassed = true\nnoUnresolvedThreads = true\n"
        ),
        "{generated}"
    );
    assert!(config["forge"].get("draftPolicy").is_none(), "{generated}");
    assert!(config["forge"].get("hosts").is_none(), "{generated}");
    set_runtime(
        &mut config,
        toml::Value::Table(toml::map::Map::from_iter([
            ("kind".into(), toml::Value::String("fake".into())),
            (
                "dir".into(),
                toml::Value::String(env.fake.to_string_lossy().into_owned()),
            ),
        ])),
    );
    config["data"]["dir"] = toml::Value::String(env.data.to_string_lossy().into_owned());
    fs::write(&env.config, toml::to_string(&config).unwrap()).unwrap();
    env
}

#[test]
fn up_continues_reconciling_other_tasks_and_reconnects_without_duplicate_sessions() {
    let env = fake_bootstrap("up-progress");
    // Use the normal lease: a short test-only lease can expire during a
    // legitimate filesystem operation on a busy native runner.
    let first = Running(env.spawn(&["up", "--poll-ms", "100"]));
    until(|| env.fleet(&["get", "task", "vega"]).code == Some(0));
    until(|| env.status("vega")["phase"] == "Briefed");
    env.host()
        .set_at_rest(&env.session_key("vega"), false)
        .unwrap();
    let original = env.get("vega");
    env.apply(support::task("while-vega-busy", true)).ok();
    until(|| env.status("while-vega-busy")["phase"] == "Briefed");
    assert_eq!(env.calls("spawn"), 2);
    // A competing startup is refused while the resident owns the lease.
    env.fleet(&["up", "--poll-ms", "100"]).code(6);
    drop(first);
    // Observe the actual crashed lease, rather than assuming its last
    // renewal coincides with the test's kill. Never alter store records.
    let lease: serde_json::Value =
        serde_json::from_slice(&fs::read(env.data.join("lease.json")).unwrap()).unwrap();
    let expires = lease["expiresAt"].as_u64().unwrap();
    until(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            >= u128::from(expires)
    });
    let _restart = Running(env.spawn(&["up", "--poll-ms", "100"]));
    env.apply(support::task("after-restart", true)).ok();
    until(|| env.status("after-restart")["phase"] == "Briefed");
    assert_eq!(
        env.get("vega")["metadata"]["uid"],
        original["metadata"]["uid"]
    );
    assert_eq!(env.calls("spawn"), 3, "restart adopts existing VEGA");
}

#[test]
fn installed_first_run_creates_runnable_config_and_editable_workspace_without_git() {
    let env = Env::without_tools("installed-init");
    wait(installed(&env, &["init"]).spawn().unwrap()).ok();
    let config = env.root.join("config/fleet/config.toml");
    let workspace = env.root.join("local-data/fleet/vega");
    assert!(config.is_file(), "native config discovery path");
    assert!(
        workspace.join("AGENTS.md").is_file(),
        "bundled instructions"
    );
    assert!(
        workspace.join("task.json").is_file(),
        "ordinary Task manifest"
    );
    let customized_config = customize_config(&config);
    fs::write(workspace.join("AGENTS.md"), "custom instructions").unwrap();
    fs::write(workspace.join("private.txt"), "private customization").unwrap();
    wait(installed(&env, &["init"]).spawn().unwrap()).ok();
    wait(installed(&env, &["init", "--update"]).spawn().unwrap()).ok();
    assert_eq!(fs::read(&config).unwrap(), customized_config);

    assert_eq!(
        fs::read_to_string(workspace.join("AGENTS.md")).unwrap(),
        "custom instructions"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("private.txt")).unwrap(),
        "private customization"
    );
    wait(
        installed(
            &env,
            &["apply", "-f", workspace.join("task.json").to_str().unwrap()],
        )
        .spawn()
        .unwrap(),
    )
    .ok()
    .stdout_has("Task/vega created");
    // No runtime-mux on PATH: `up` names the program it could not start.
    wait(installed(&env, &["up"]).spawn().unwrap())
        .code(1)
        .stderr_has("runtime-mux");
}

#[cfg(unix)]
#[test]
fn installed_workspace_exposes_each_skill_to_codex_and_claude_code() {
    // Codex discovers project skills under .agents/skills, Claude Code only
    // under .claude/skills: a VEGA session on either harness must find them.
    let env = Env::without_tools("installed-skills");
    wait(installed(&env, &["init"]).spawn().unwrap()).ok();
    let workspace = env.root.join("local-data/fleet/vega");
    let canonical = workspace.join(".agents/skills");
    let mut names: Vec<_> = fs::read_dir(&canonical)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    assert!(
        names.iter().any(|n| n == "status-digest"),
        "the bundle ships Mission Control's skills: {names:?}"
    );
    for name in names {
        let skill = std::path::Path::new(&name).join("SKILL.md");
        let claude = workspace.join(".claude/skills").join(&skill);
        assert_eq!(
            fs::read_to_string(&claude).ok(),
            Some(fs::read_to_string(canonical.join(&skill)).unwrap()),
            "Claude Code finds no {} under .claude/skills",
            skill.display()
        );
    }
    let link = workspace.join(".claude/skills");
    assert_eq!(
        fs::read_link(&link).unwrap(),
        std::path::Path::new("..").join(".agents").join("skills"),
        "one canonical copy, linked relatively so the workspace can move"
    );

    // A workspace installed before the link existed gains it on update.
    fs::remove_file(&link).unwrap();
    wait(installed(&env, &["init", "--update"]).spawn().unwrap()).ok();
    assert!(link.join("status-digest/SKILL.md").is_file());

    // A user's own skills directory is never replaced.
    fs::remove_file(&link).unwrap();
    fs::create_dir_all(link.join("mine")).unwrap();
    wait(installed(&env, &["init", "--update"]).spawn().unwrap())
        .ok()
        .stdout_has("preserved");
    assert!(link.join("mine").is_dir() && !link.is_symlink());

    // Nor is a user's own `.claude` link: init keeps it and still succeeds.
    let elsewhere = env.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::remove_dir_all(workspace.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, workspace.join(".claude")).unwrap();
    wait(installed(&env, &["init", "--update"]).spawn().unwrap())
        .ok()
        .stdout_has("preserved");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);

    // A link that cannot be made is reported; init still completes.
    use std::os::unix::fs::PermissionsExt;
    fs::remove_file(workspace.join(".claude")).unwrap();
    fs::create_dir(workspace.join(".claude")).unwrap();
    fs::set_permissions(workspace.join(".claude"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::remove_file(workspace.join(".fleet/provenance.json")).unwrap();
    wait(installed(&env, &["init", "--update"]).spawn().unwrap())
        .ok()
        .stdout_has("could not link");
    assert!(workspace.join(".fleet/provenance.json").is_file());
    // Nor does a `.claude` init cannot read.
    fs::set_permissions(workspace.join(".claude"), fs::Permissions::from_mode(0o000)).unwrap();
    fs::remove_file(workspace.join(".fleet/provenance.json")).unwrap();
    wait(installed(&env, &["init", "--update"]).spawn().unwrap())
        .ok()
        .stdout_has("could not inspect");
    assert!(workspace.join(".fleet/provenance.json").is_file());
    fs::set_permissions(workspace.join(".claude"), fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn concurrent_init_preserves_identity_config_and_private_files_and_updates_safely() {
    let env = Env::without_tools("concurrent-init");
    let one = installed(&env, &["init"]).spawn().unwrap();
    let two = installed(&env, &["init"]).spawn().unwrap();
    wait(one).ok();
    wait(two).ok();
    let config = env.root.join("config/fleet/config.toml");
    let workspace = env.root.join("local-data/fleet/vega");
    let newest_config = fs::read(&config).unwrap();
    let original_config = customize_config(&config);
    let original_instance = fs::read(workspace.join(".fleet/instance")).unwrap();
    let original_task = fs::read(workspace.join("task.json")).unwrap();
    fs::write(workspace.join("AGENTS.md"), "user edit").unwrap();
    fs::write(workspace.join("private.txt"), "private").unwrap();
    // Simulate an older pristine managed prompt to exercise an upgrade.
    let prompt = workspace.join("prompts/mission-control.md");
    let newest = fs::read(&prompt).unwrap();
    fs::write(&prompt, "older managed instructions").unwrap();
    let state_path = workspace.join(".fleet/baseline.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    use sha2::{Digest, Sha256};
    state["files"]["prompts/mission-control.md"] = serde_json::json!(
        Sha256::digest(b"older managed instructions")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    // Config remains user-owned even when it matches its older managed
    // baseline; a new binary must offer a conflict instead of upgrading it.
    state["files"]["config"] = serde_json::json!(
        Sha256::digest(&original_config)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    wait(installed(&env, &["init", "--update"]).spawn().unwrap())
        .ok()
        .stdout_has("preserved customization");
    assert_eq!(fs::read(&prompt).unwrap(), newest);
    assert_eq!(
        fs::read_to_string(workspace.join("AGENTS.md")).unwrap(),
        "user edit"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("private.txt")).unwrap(),
        "private"
    );
    assert_eq!(fs::read(&config).unwrap(), original_config);
    let candidate_hash: String = Sha256::digest(&newest_config)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        fs::read(
            workspace
                .join(".fleet/conflicts")
                .join(candidate_hash)
                .join("config")
        )
        .unwrap(),
        newest_config
    );

    assert_eq!(
        fs::read(workspace.join(".fleet/instance")).unwrap(),
        original_instance
    );
    assert_eq!(
        fs::read(workspace.join("task.json")).unwrap(),
        original_task
    );
}

#[test]
fn data_directory_override_and_config_precedence_keep_all_commands_in_one_store() {
    let env = Env::without_tools("config-precedence");
    let path = env.root.join("chosen config.toml");
    let data = env.root.join("chosen-store");
    wait(
        installed(
            &env,
            &[
                "--config",
                path.to_str().unwrap(),
                "--data-dir",
                data.to_str().unwrap(),
                "init",
            ],
        )
        .spawn()
        .unwrap(),
    )
    .ok();
    let mut command = installed(&env, &["list", "task", "--json"]);
    command.env("FLEET_CONFIG", &path);
    wait(command.spawn().unwrap()).ok();
    assert!(
        data.join("store.json").exists(),
        "init's --data-dir must be the directory used by subsequent commands"
    );
    assert!(!env.root.join("config/fleet/config.toml").exists());
    let missing = env.root.join("missing.toml");
    let mut command = installed(
        &env,
        &["--config", missing.to_str().unwrap(), "list", "task"],
    );
    command.env("FLEET_CONFIG", &path);
    wait(command.spawn().unwrap())
        .code(1)
        .stderr_has("cannot read config");
}

#[test]
fn up_never_reads_a_session_it_cannot_see_as_gone() {
    let env = fake_bootstrap("up-unknown-host");
    let _resident = Running(env.spawn(&["up", "--poll-ms", "100"]));
    until(|| env.fleet(&["get", "task", "vega"]).code == Some(0));
    until(|| env.status("vega")["phase"] == "Briefed");
    let original = env.get("vega");
    assert_config_instruction(original["spec"]["brief"].as_str().unwrap(), &env.config);
    assert_eq!(env.calls("spawn"), 1);
    // An unreadable host is explicit Unknown, never evidence of absence.
    env.host().set_host("local", "unknown").unwrap();
    thread::sleep(Duration::from_millis(600));
    assert_eq!(env.calls("spawn"), 1);
    let now = env.get("vega");
    assert_eq!(now["status"]["session"], original["status"]["session"]);
    assert!(
        now["status"]["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["type"] != "WorkerGone"),
        "{:#}",
        now["status"]
    );
}

#[test]
fn up_refuses_a_store_override_that_would_send_workers_to_another_store() {
    let env = fake_bootstrap("up-override");
    let other = env.root.join("other-store");
    let error_path = env.root.join("override-error");
    let mut command = env.command(&["--data-dir", other.to_str().unwrap(), "up"]);
    command.stderr(fs::File::create(&error_path).unwrap());
    let mut process = Running(command.spawn().unwrap());
    thread::sleep(Duration::from_millis(600));
    assert_eq!(
        process.0.try_wait().unwrap().and_then(|s| s.code()),
        Some(1),
        "mismatched store must be refused"
    );
    assert!(
        fs::read_to_string(error_path)
            .unwrap()
            .contains("persist data.dir")
    );
    assert!(!other.exists(), "refuse before writing an unrelated store");
}

#[test]
fn manager_renews_after_the_final_probe_before_waiting_under_its_lease() {
    use fleet_api::plan::{EffectOutcome, SpawnSpec};
    use fleet_api::runtime::Runtime;
    use fleet_api::task::SessionRef;
    use fleet_api::{Clock, Observation};
    use fleet_manager::{Manager, RunOptions};
    use fleet_store::FileStore;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    // First exercise the emitted starter through the CLI and real controller.
    let env = fake_bootstrap("up-lease-clock");
    let cfg: toml::Value = toml::from_str(&fs::read_to_string(&env.config).unwrap()).unwrap();
    let manifest =
        std::path::Path::new(cfg["starter"]["workspace"].as_str().unwrap()).join("task.json");
    env.apply(serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap())
        .ok();
    env.reconcile().ok();
    let session: SessionRef =
        serde_json::from_value(env.get("vega")["status"]["session"].clone()).unwrap();

    #[derive(Debug)]
    struct TestClock(AtomicU64);
    impl Clock for TestClock {
        fn now(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }
    struct SlowLookup {
        clock: Arc<TestClock>,
        session: SessionRef,
    }
    impl Runtime for SlowLookup {
        fn operation_bound(&self) -> u64 {
            1500
        }
        fn host(&self, _: &str) -> Observation<bool> {
            Observation::Known(true)
        }
        fn create_dir(&self, _: &str, _: &str) -> Observation<Result<bool, String>> {
            panic!("no Task is created")
        }
        fn remove_dir(&self, _: &str, _: &str) -> Observation<Result<(), String>> {
            panic!("no thread retires")
        }
        fn find(&self, host: &str, key: &str) -> Observation<Option<SessionRef>> {
            assert_eq!(host, self.session.host);
            assert_eq!(key, self.session.key);
            self.clock.0.fetch_add(1500, Ordering::SeqCst);
            Observation::Known(Some(self.session.clone()))
        }
        fn ready(&self, _: &SessionRef) -> Observation<bool> {
            panic!("already briefed")
        }
        fn workspace_trust(
            &self,
            _: &SessionRef,
            _: &fleet_api::task::Workspace,
            _: Option<&fleet_api::workspace::WorkspaceRef>,
        ) -> Observation<fleet_api::runtime::WorkspaceTrust> {
            panic!("already briefed")
        }
        fn brief_delivered(&self, _: &SessionRef, _: &str) -> Observation<bool> {
            panic!("already briefed")
        }
        fn at_rest(&self, _: &SessionRef) -> Observation<bool> {
            panic!("not deleting")
        }
        fn spawn(&self, _: &SpawnSpec) -> Observation<EffectOutcome> {
            panic!("must retain session")
        }
        fn brief(&self, _: &SessionRef, _: &str, _: &str) -> Observation<EffectOutcome> {
            panic!("must not rebrief")
        }
        fn release(&self, _: &SessionRef) -> Observation<EffectOutcome> {
            panic!("must retain session")
        }
        fn lost(&self, _: &SessionRef) -> Observation<bool> {
            Observation::Known(false)
        }
        fn revive(
            &self,
            _: &SessionRef,
            _: Option<&str>,
            _: &fleet_api::plan::SpawnSpec,
        ) -> Observation<EffectOutcome> {
            panic!("the session is running")
        }
    }
    let clock = Arc::new(TestClock(AtomicU64::new(100_000)));
    let store = FileStore::open_with_clock(&env.data, clock.clone()).unwrap();
    let runtime = SlowLookup {
        clock: clock.clone(),
        session,
    };
    let manager = Manager {
        plugins: None,
        store: &store,
        runtime: &runtime,
        clock: clock.as_ref(),
        holder: "lease-clock-test".into(),
        forge: None,
        signals: None,
        change_request_defaults: Default::default(),
        forge_defaults: Default::default(),
        gateway: None,
    };
    let mut sweeps = 0;
    // Probe 1500ms and wait900ms each fit the2000ms lease, but together
    // do not. Clock values avoid measuring the machine's scheduling/fsync.
    manager
        .serve(
            RunOptions {
                max_passes: 1,
                lease_ttl: 2000,
            },
            |_| {
                sweeps += 1;
                if sweeps == 1 {
                    clock.0.fetch_add(900, Ordering::SeqCst);
                    true
                } else {
                    false
                }
            },
        )
        .expect("renew after the slow operation, before the poll wait");
    assert_eq!(sweeps, 2);
}

#[test]
fn up_reports_known_spawn_failures_without_requiring_a_separate_status_query() {
    let env = fake_bootstrap("up-spawn-diagnostic");
    env.fault(fleet_runtime::Fault::new("spawn", "fail"));
    let error_path = env.root.join("stderr");
    let mut command = env.command(&["up", "--poll-ms", "100"]);
    command.stderr(fs::File::create(&error_path).unwrap());
    let _resident = Running(command.spawn().unwrap());
    until(|| env.fleet(&["get", "task", "vega"]).code == Some(0));
    until(|| {
        env.status("vega")["conditions"]
            .as_array()
            .is_some_and(|cs| cs.iter().any(|c| c["reason"] == "SpawnRetry"))
    });
    until(|| {
        fs::read_to_string(&error_path)
            .unwrap()
            .contains("SpawnRetry")
    });
}

#[test]
fn healthy_briefed_vega_does_not_report_a_startup_failure() {
    let env = fake_bootstrap("up-healthy-diagnostic");
    let path = env.root.join("stderr");
    let mut command = env.command(&["up", "--poll-ms", "100"]);
    command.stderr(fs::File::create(&path).unwrap());
    let _resident = Running(command.spawn().unwrap());
    until(|| env.fleet(&["get", "task", "vega"]).code == Some(0));
    until(|| env.status("vega")["phase"] == "Briefed");
    let before = env.calls("find");
    until(|| env.calls("find") >= before + 3);
    assert!(
        fs::read_to_string(path).unwrap().is_empty(),
        "a healthy running Task is not a startup failure"
    );
}

#[test]
fn reconnect_uses_stored_immutable_inputs_after_the_editable_template_changes() {
    let env = fake_bootstrap("up-retained-inputs");
    let config: toml::Value = toml::from_str(&fs::read_to_string(&env.config).unwrap()).unwrap();
    let path =
        std::path::Path::new(config["starter"]["workspace"].as_str().unwrap()).join("task.json");
    let template: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    env.apply(template).ok();
    env.reconcile().ok();
    assert_eq!(env.status("vega")["phase"], "Briefed");
    let original = env.get("vega");
    let mut changed: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    changed[0]["spec"]["host"] = serde_json::json!("unused-host");
    fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    env.host().set_host("unused-host", "down").unwrap();
    let mut resident = Running(env.spawn(&["up", "--poll-ms", "100"]));
    thread::sleep(Duration::from_millis(600));
    assert!(
        resident.0.try_wait().unwrap().is_none(),
        "reconnect must probe the retained host, not a changed template"
    );
    env.apply(support::task("still-progressing", true)).ok();
    until(|| env.status("still-progressing")["phase"] == "Briefed");
    assert_eq!(env.get("vega")["spec"], original["spec"]);
}

/// `fleet up` exports each sweep as a trace of its own, and Ctrl-C flushes
/// what is still batched before it exits: the run below ends before the
/// batch's first scheduled export.
#[cfg(unix)]
#[test]
fn up_flushes_its_sweeps_to_a_collector_when_interrupted() {
    let collector = support::otlp::Collector::start();
    let mut env = fake_bootstrap("up-telemetry");
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint);
    let mut resident = Running(env.spawn(&["up", "--poll-ms", "100"]));
    until(|| env.fleet(&["get", "task", "vega"]).code == Some(0));
    until(|| env.status("vega")["phase"] == "Briefed");
    let interrupted = Command::new("kill")
        .args(["-INT", &resident.0.id().to_string()])
        .status()
        .unwrap();
    assert!(interrupted.success());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = resident.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "up kept running"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(0));
    let spans = collector.spans();
    let passes: Vec<_> = spans
        .iter()
        .filter(|s| s.name == "reconcile pass")
        .collect();
    assert!(!passes.is_empty(), "{spans:#?}");
    assert_eq!(
        passes[0].resource["service.name"], "fleet",
        "the default name"
    );
    assert!(
        passes.iter().all(|p| p.parent.is_empty()),
        "each sweep is a root: {passes:#?}"
    );
    assert!(
        spans
            .iter()
            .any(|s| s.name == "effect spawn" && s.attrs["fleet.name"] == "vega"),
        "{spans:#?}"
    );
    assert!(!spans.iter().any(|s| s.name == "fleet up"), "{spans:#?}");
    assert!(
        collector
            .points()
            .iter()
            .any(|p| p.metric == "fleet.effects"),
        "metrics flushed too"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn native_config_and_data_can_live_on_different_filesystems() {
    use std::os::unix::fs::MetadataExt;
    let env = Env::without_tools("different-filesystems");
    // A tiny native tmpfs fixture, never a build or shared deployment.
    let other = std::path::PathBuf::from("/dev/shm")
        .join(format!("fleet-bootstrap-{}", std::process::id()));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(other.clone());
    fs::create_dir_all(&other).unwrap();
    assert_ne!(
        fs::metadata(&env.root).unwrap().dev(),
        fs::metadata(&other).unwrap().dev(),
        "cross-volume acceptance requires two filesystems"
    );
    let mut command = installed(&env, &["init"]);
    command.env("XDG_DATA_HOME", &other);
    wait(command.spawn().unwrap()).ok();
    assert!(env.root.join("config/fleet/config.toml").is_file());
    assert!(other.join("fleet/vega/task.json").is_file());
}

#[cfg(target_os = "linux")]
#[test]
fn graceful_shutdown_releases_the_lease_and_reconnects_without_waiting_for_expiry() {
    let env = fake_bootstrap("up-graceful");
    let mut first = Running(env.spawn(&["up", "--poll-ms", "100"]));
    until(|| env.fleet(&["get", "task", "vega"]).code == Some(0));
    until(|| env.status("vega")["phase"] == "Briefed");
    let session = env.get("vega")["status"]["session"].clone();
    assert!(
        Command::new("/bin/kill")
            .args(["-TERM", &first.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let mut status = None;
    until(|| {
        status = first.0.try_wait().unwrap();
        status.is_some()
    });
    assert_eq!(status.unwrap().code(), Some(0));
    let _second = Running(env.spawn(&["up", "--poll-ms", "100"]));
    env.apply(support::task("after-graceful-stop", true)).ok();
    until(|| env.status("after-graceful-stop")["phase"] == "Briefed");
    assert_eq!(env.get("vega")["status"]["session"], session);
    assert_eq!(env.calls("spawn"), 2);
}

#[test]
fn separate_configurations_get_separate_starter_instances_and_boot_instructions() {
    let env = Env::without_tools("separate-instances");
    let one = env.root.join("one.toml");
    let two = env.root.join("two.toml");
    wait(
        installed(&env, &["--config", one.to_str().unwrap(), "init"])
            .spawn()
            .unwrap(),
    )
    .ok();
    wait(
        installed(&env, &["--config", two.to_str().unwrap(), "init"])
            .spawn()
            .unwrap(),
    )
    .ok();
    let a: toml::Value = toml::from_str(&fs::read_to_string(&one).unwrap()).unwrap();
    let b: toml::Value = toml::from_str(&fs::read_to_string(&two).unwrap()).unwrap();
    assert_ne!(
        a["runtimes"]["mux"]["keyPrefix"], b["runtimes"]["mux"]["keyPrefix"],
        "separate configs must not share session namespace"
    );
    assert_ne!(a["starter"]["workspace"], b["starter"]["workspace"]);
    assert_ne!(a["data"]["dir"], b["data"]["dir"]);
    for (config, path) in [(a, one), (b, two)] {
        let task: serde_json::Value = serde_json::from_slice(
            &fs::read(
                std::path::Path::new(config["starter"]["workspace"].as_str().unwrap())
                    .join("task.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_config_instruction(task[0]["spec"]["brief"].as_str().unwrap(), &path);
    }
}

#[test]
fn explicit_config_and_store_work_without_native_home_variables() {
    let env = Env::without_tools("explicit-no-home");
    let config = env.root.join("explicit.toml");
    let store = env.root.join("explicit-store");
    let mut command = installed(
        &env,
        &[
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            store.to_str().unwrap(),
            "init",
        ],
    );
    for key in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "APPDATA",
        "LOCALAPPDATA",
    ] {
        command.env_remove(key);
    }
    wait(command.spawn().unwrap()).ok();
    wait(
        installed(
            &env,
            &[
                "--config",
                config.to_str().unwrap(),
                "list",
                "task",
                "--json",
            ],
        )
        .spawn()
        .unwrap(),
    )
    .ok();
    assert!(store.join("store.json").is_file());
}

fn starter_task(env: &Env) -> (std::path::PathBuf, serde_json::Value) {
    let config: toml::Value = toml::from_str(&fs::read_to_string(&env.config).unwrap()).unwrap();
    let path =
        std::path::Path::new(config["starter"]["workspace"].as_str().unwrap()).join("task.json");
    let task = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    (path, task)
}

/// The coding agent is chosen at `fleet init`, without a manual edit of
/// `task.json`. The choice reaches Mission Control's Task and its brief,
/// which gives the same harness to the workers it dispatches.
#[test]
fn init_harness_flag_reaches_mission_control_and_its_workers() {
    let env = fake_bootstrap_with("init-harness", &["init", "--harness", "claude"]);
    let (path, task) = starter_task(&env);
    assert_eq!(task[0]["spec"]["harness"], "claude");
    let brief = task[0]["spec"]["brief"].as_str().unwrap();
    assert!(
        brief.contains("Give each worker Task the same harness as Task/vega"),
        "{brief}"
    );
    assert_config_instruction(brief, &env.config);

    // Repeating init, or updating it, keeps the choice.
    let chosen = fs::read(&path).unwrap();
    env.fleet(&["init"]).ok().stdout_has("harness claude");
    env.fleet(&["init", "--update"]).ok();
    assert_eq!(fs::read(&path).unwrap(), chosen);

    // Before the first `fleet up`, init can choose again.
    env.fleet(&["init", "--harness", "codex"])
        .ok()
        .stdout_has("harness codex");
    let (_, task) = starter_task(&env);
    assert_eq!(task[0]["spec"]["harness"], "codex");
    env.fleet(&["init", "--harness", "claude"]).ok();

    // The first `fleet up` creates Mission Control on the chosen harness.
    let _up = Running(env.spawn(&["up", "--poll-ms", "100"]));
    until(|| env.fleet(&["get", "task", "vega"]).code == Some(0));
    until(|| env.status("vega")["phase"] == "Briefed");
    assert_eq!(env.get("vega")["spec"]["harness"], "claude");
    let session = env.host().session(&env.session_key("vega")).unwrap();
    assert_eq!(session.unwrap().harness, "claude");
}

#[test]
fn init_without_a_harness_flag_uses_the_bundled_default_and_keeps_edits() {
    let env = fake_bootstrap("init-harness-default");
    let (path, mut task) = starter_task(&env);
    assert_eq!(task[0]["spec"]["harness"], "codex");
    // A hand-edited task.json is the user's: a new choice does not replace it.
    task[0]["spec"]["model"] = serde_json::json!("custom");
    let edited = serde_json::to_vec_pretty(&task).unwrap();
    fs::write(&path, &edited).unwrap();
    let run = env.fleet(&["init", "--harness", "claude"]);
    run.ok().stdout_has("preserved customization");
    assert_eq!(fs::read(&path).unwrap(), edited);
    // The candidate carries the new choice for the user to merge.
    let stdout = run.stdout.clone();
    let (_, candidate) = stdout.split_once("; candidate ").unwrap();
    let candidate: serde_json::Value =
        serde_json::from_slice(&fs::read(candidate.lines().next().unwrap()).unwrap()).unwrap();
    assert_eq!(candidate[0]["spec"]["harness"], "claude");

    // A deleted task.json is not reported as choosing a harness.
    fs::remove_file(&path).unwrap();
    let run = env.fleet(&["init", "--harness", "claude"]);
    let stdout = run.stdout.clone();
    assert!(!stdout.contains("chooses harness"), "{stdout}");
}

/// After the first `fleet up`, Task/vega's harness is immutable, so init
/// refuses another choice instead of reporting one that never applies.
#[test]
fn init_refuses_a_new_harness_once_mission_control_exists() {
    let env = fake_bootstrap("init-harness-after-up");
    let (path, task) = starter_task(&env);
    env.apply(task).ok();
    let before = fs::read(&path).unwrap();
    env.fleet(&["init", "--harness", "claude"])
        .code(1)
        .stderr_has("immutable");
    assert_eq!(fs::read(&path).unwrap(), before);
    // `fleet up` keeps the config's store, so another --data-dir changes nothing.
    let other = env.root.join("other-store");
    env.fleet(&[
        "--data-dir",
        other.to_str().unwrap(),
        "init",
        "--harness",
        "claude",
    ])
    .code(1)
    .stderr_has("immutable");
    assert_eq!(fs::read(&path).unwrap(), before);
    env.fleet(&["init", "--harness", "codex"]).ok();
}

#[test]
fn init_refuses_an_invalid_harness_before_writing_anything() {
    let env = Env::without_tools("init-bad-harness");
    let config = env.root.join("config/fleet/config.toml");
    wait(
        installed(&env, &["init", "--harness", "Not A Harness"])
            .spawn()
            .unwrap(),
    )
    .code(1)
    .stderr_has("harness");
    assert!(!config.exists());
}
