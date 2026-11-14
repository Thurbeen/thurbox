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
/// `[runtimes]` that `init` writes.
fn set_runtime(config: &mut toml::Value, runtime: toml::Value) {
    let table = config.as_table_mut().unwrap();
    table.remove("runtimes");
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

/// `fleet init`, then point the generated config at the fake runtime and
/// the test store.
fn fake_bootstrap(name: &str) -> Env {
    let mut env = Env::new(name);
    env.set_var(
        "LOCALAPPDATA",
        &env.root.join("local-data").to_string_lossy(),
    );
    env.set_var("APPDATA", &env.root.join("config").to_string_lossy());
    // An explicit fresh config uses the same discovery and bootstrap machinery.
    fs::remove_file(&env.config).unwrap();
    env.fleet(&["init"]).ok();
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
    env.apply(support::task("first", true)).ok();
    until(|| env.status("first")["phase"] == "Briefed");
    env.host()
        .set_at_rest(&env.session_key("first"), false)
        .unwrap();
    let original = env.get("first");
    env.apply(support::task("while-first-busy", true)).ok();
    until(|| env.status("while-first-busy")["phase"] == "Briefed");
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
        env.get("first")["metadata"]["uid"],
        original["metadata"]["uid"]
    );
    assert_eq!(env.calls("spawn"), 3, "restart adopts the existing session");
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
    env.apply(support::task("first", true)).ok();
    until(|| env.status("first")["phase"] == "Briefed");
    let original = env.get("first");
    assert_eq!(env.calls("spawn"), 1);
    // An unreadable host is explicit Unknown, never evidence of absence.
    env.host().set_host("local", "unknown").unwrap();
    thread::sleep(Duration::from_millis(600));
    assert_eq!(env.calls("spawn"), 1);
    let now = env.get("first");
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

    // First run a Task through the CLI and the real controller.
    let env = fake_bootstrap("up-lease-clock");
    env.apply(support::task("first", true)).ok();
    env.reconcile().ok();
    let session: SessionRef =
        serde_json::from_value(env.get("first")["status"]["session"].clone()).unwrap();

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
    env.apply(support::task("first", true)).ok();
    until(|| {
        env.status("first")["conditions"]
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
fn a_healthy_briefed_task_does_not_report_a_startup_failure() {
    let env = fake_bootstrap("up-healthy-diagnostic");
    let path = env.root.join("stderr");
    let mut command = env.command(&["up", "--poll-ms", "100"]);
    command.stderr(fs::File::create(&path).unwrap());
    let _resident = Running(command.spawn().unwrap());
    env.apply(support::task("first", true)).ok();
    until(|| env.status("first")["phase"] == "Briefed");
    let before = env.calls("find");
    until(|| env.calls("find") >= before + 3);
    assert!(
        fs::read_to_string(path).unwrap().is_empty(),
        "a healthy running Task is not a startup failure"
    );
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
    env.apply(support::task("first", true)).ok();
    until(|| env.status("first")["phase"] == "Briefed");
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
            .any(|s| s.name == "effect spawn" && s.attrs["fleet.name"] == "first"),
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
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(env.root.join("config/fleet/config.toml")).unwrap())
            .unwrap();
    assert!(
        config["data"]["dir"]
            .as_str()
            .unwrap()
            .starts_with(other.to_str().unwrap()),
        "{config}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn graceful_shutdown_releases_the_lease_and_reconnects_without_waiting_for_expiry() {
    let env = fake_bootstrap("up-graceful");
    let mut first = Running(env.spawn(&["up", "--poll-ms", "100"]));
    env.apply(support::task("first", true)).ok();
    until(|| env.status("first")["phase"] == "Briefed");
    let session = env.get("first")["status"]["session"].clone();
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
    assert_eq!(env.get("first")["status"]["session"], session);
    assert_eq!(env.calls("spawn"), 2);
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

/// `fleet up` is a plain manager: a config with no starter runs it, and it
/// reconciles the Tasks applied to its store.
#[test]
fn up_runs_without_a_starter_and_reconciles_a_task() {
    let env = Env::new("up-no-starter");
    let _resident = Running(env.spawn(&["up", "--poll-ms", "100"]));
    env.apply(support::task("plain", true)).ok();
    until(|| env.status("plain")["phase"] == "Briefed");
    assert_eq!(env.calls("spawn"), 1, "up creates no Task of its own");
    env.fleet(&["get", "task", "vega"]).code(3);
}

/// `init` writes the native config and nothing else: no `[starter]`, no
/// bundled workspace.
#[test]
fn init_writes_the_config_and_no_starter_workspace() {
    let env = Env::without_tools("init-config-only");
    wait(installed(&env, &["init"]).spawn().unwrap()).ok();
    let config = env.root.join("config/fleet/config.toml");
    let text = fs::read_to_string(&config).unwrap();
    assert!(!text.contains("[starter]"), "{text}");
    assert!(
        text.contains("[runtimes.mux]") && !text.contains("[hosts"),
        "{text}"
    );
    assert!(!env.root.join("local-data/fleet/vega").exists());
    // The config is the user's once written: a second init keeps it.
    let customized = customize_config(&config);
    wait(installed(&env, &["init"]).spawn().unwrap())
        .ok()
        .stdout_has("kept");
    assert_eq!(fs::read(&config).unwrap(), customized);
    // No runtime-mux on PATH: `up` names the program it could not start.
    wait(installed(&env, &["up"]).spawn().unwrap())
        .code(1)
        .stderr_has("runtime-mux");
}

/// Two inits racing write one config, never two halves.
#[test]
fn concurrent_init_writes_one_config() {
    let env = Env::without_tools("concurrent-init");
    let one = installed(&env, &["init"]).spawn().unwrap();
    let two = installed(&env, &["init"]).spawn().unwrap();
    wait(one).ok();
    wait(two).ok();
    let config = env.root.join("config/fleet/config.toml");
    let text = fs::read_to_string(&config).unwrap();
    toml::from_str::<toml::Value>(&text).unwrap();
    assert_eq!(text.matches("[runtimes.mux]").count(), 1, "{text}");
}

#[test]
fn separate_configurations_get_separate_sessions_and_stores() {
    let env = Env::without_tools("separate-instances");
    let one = env.root.join("one.toml");
    let two = env.root.join("two.toml");
    for path in [&one, &two] {
        wait(
            installed(&env, &["--config", path.to_str().unwrap(), "init"])
                .spawn()
                .unwrap(),
        )
        .ok();
    }
    let a: toml::Value = toml::from_str(&fs::read_to_string(&one).unwrap()).unwrap();
    let b: toml::Value = toml::from_str(&fs::read_to_string(&two).unwrap()).unwrap();
    assert_ne!(
        a["runtimes"]["mux"]["keyPrefix"], b["runtimes"]["mux"]["keyPrefix"],
        "separate configs must not share session namespace"
    );
    assert_ne!(a["data"]["dir"], b["data"]["dir"]);
}

/// A config written for the starter is refused by name, not as an unknown
/// field, and the refusal names no page: the docs keep no starter section.
#[test]
fn a_starter_section_is_refused_at_load() {
    let env = Env::new("starter-refused");
    let mut text = fs::read_to_string(&env.config).unwrap();
    text.push_str("\n[starter]\nworkspace = \"vega\"\n");
    fs::write(&env.config, text).unwrap();
    let run = env.fleet(&["list", "task"]);
    run.code(1).stderr_has("[starter] is no longer read");
    assert!(!run.stderr.contains("configuration page"), "{}", run.stderr);
}

/// `fleet version --json` reports the manager's lease under `manager`, and
/// only reads it: no store is created, and the lease is neither taken nor
/// renewed.
#[cfg(target_os = "linux")]
#[test]
fn version_reports_the_manager_lease_without_taking_it() {
    let env = fake_bootstrap("version-manager");
    let manager = || env.fleet(&["version", "--json"]).ok().json()["manager"].clone();
    let lease = env.data.join("lease.json");

    let none = manager();
    assert_eq!(none["held"], false, "{none:#}");
    assert!(none["holder"].is_null(), "{none:#}");
    assert!(!lease.exists(), "version wrote no lease");
    assert!(
        !env.data.join("store.json").exists(),
        "version opened no store"
    );

    let mut up = Running(env.spawn(&["up", "--poll-ms", "100"]));
    until(|| manager()["held"] == true);
    let held = manager();
    assert!(
        held["holder"]
            .as_str()
            .is_some_and(|h| h.starts_with("fleet-up-")),
        "{held:#}"
    );
    assert!(held["expiresAt"].as_u64().is_some(), "{held:#}");

    // A released lease is no manager; version leaves the record as it is.
    assert!(
        Command::new("/bin/kill")
            .args(["-TERM", &up.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    until(|| up.0.try_wait().unwrap().is_some());
    let released = fs::read(&lease).unwrap();
    let after = manager();
    assert_eq!(after["held"], false, "{after:#}");
    assert_eq!(after["holder"], held["holder"], "{after:#}");
    assert_eq!(fs::read(&lease).unwrap(), released, "version took no lease");

    // A lease that cannot be read is unknown, never absent.
    fs::write(&lease, b"{not json").unwrap();
    let unknown = manager();
    assert!(unknown["held"].is_null(), "{unknown:#}");
    assert!(
        unknown["why"].as_str().is_some_and(|w| w.contains("lease")),
        "{unknown:#}"
    );
}
