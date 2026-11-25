#![cfg(unix)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::Env;

fn missing(what: &str) {
    assert!(
        std::env::var_os("CI").is_none(),
        "{what} under CI, where the runtime protocol tests must run"
    );
    eprintln!("skipping: {what}");
}

fn runtime_mux() -> Option<PathBuf> {
    let found = std::env::var_os("RUNTIME_MUX_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            Path::new(env!("CARGO_BIN_EXE_fleet"))
                .parent()
                .map(|dir| dir.join("runtime-mux"))
        })
        .filter(|p| p.is_file());
    if found.is_none() {
        missing("no runtime-mux binary; build it with `cargo build -p runtime-mux`");
    }
    found
}

fn tmux() -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("tmux"))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        missing("tmux is not installed");
    }
    found
}

struct Sockets(PathBuf);

impl Sockets {
    fn new(name: &str) -> Sockets {
        Sockets(short_private_dir(name))
    }

    fn tmux(&self, args: &[&str]) -> std::process::Output {
        Command::new("tmux")
            .env("TMUX_TMPDIR", &self.0)
            .env_remove("TMUX")
            .args(["-L", "runtime-mux"])
            .args(args)
            .output()
            .expect("tmux")
    }
}

fn short_private_dir(name: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .unwrap_or_else(short_temp_dir);
    let dir = base.join(format!("fx-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("socket dir");
    dir
}

fn short_temp_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    }
}

impl Drop for Sockets {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
        let _ = Command::new("tmux")
            .env("TMUX_TMPDIR", &self.0)
            .args(["-L", "runtime-tmux-sh", "kill-server"])
            .output();
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn warning_env(name: &str, program: &Path, sockets: &Sockets, telemetry: &str) -> Env {
    let env = mux_env(name, program, sockets);
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port()
        .to_string();
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        format!(
            "{cfg}\n[runtimes.mux.places.local]\n\n[runtimes.mux.places.devbox]\nkind = \"ssh\"\ndestination = \"me@127.0.0.1\"\nssh_opts = [\"-F\", \"/dev/null\", \"-p\", \"{closed}\"]\n{telemetry}"
        ),
    )
    .expect("config");
    let mut task = support::task("t1", true);
    task["spec"]["host"] = json!("mux/devbox");
    env.apply(json!([task])).ok();
    env
}

#[test]
fn a_runtime_mux_warning_is_exported_when_fleets_config_turns_logs_on() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let collector = support::otlp::Collector::start();
    let sockets = Sockets::new("otel-logs");
    let env = warning_env(
        "runtime-mux-otel-logs",
        &program,
        &sockets,
        &format!(
            "\n[telemetry]\nendpoint = \"{}\"\nlogs = true\n",
            collector.endpoint
        ),
    );
    reconcile_until(&env, "a WARN record from runtime-mux", |_| {
        collector.logs().into_iter().any(|l| {
            l.resource["service.name"] == "runtime-mux"
                && l.severity == "WARN"
                && l.body.contains("devbox")
        })
    });
}

#[test]
fn an_inherited_otel_logs_exporter_turns_on_no_runtime_mux_logs() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let collector = support::otlp::Collector::start();
    let sockets = Sockets::new("otel-nologs");
    let mut env = warning_env("runtime-mux-otel-nologs", &program, &sockets, "");
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint);
    env.set_var("OTEL_LOGS_EXPORTER", "otlp");
    reconcile_until(&env, "a start on devbox and its span", |env| {
        env.get("t1")["status"].to_string().contains("devbox")
            && collector
                .spans()
                .iter()
                .any(|s| s.name == "runtime-mux start")
    });
    assert!(
        !collector.paths().iter().any(|p| p == "/v1/logs"),
        "{:#?}",
        collector.logs()
    );
}

#[test]
fn the_reference_script_passes_the_conformance_suite() {
    if tmux().is_none() {
        return;
    }
    if Command::new("jq").arg("--version").output().is_err() {
        missing("jq is not installed");
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new("script");
    let mut env = Env::new("runtime-check-script");
    env.set_var("TMUX_TMPDIR", &sockets.0.to_string_lossy());
    let run = env.fleet(&["runtime", "check", "--", script.to_str().unwrap()]);
    run.ok()
        .stdout_has("ok   two starts with one key leave one process")
        .stdout_has("ok   concurrent starts with one key leave one process")
        .stdout_has("ok   an exited process answers Exited with its code, or None")
        .stdout_has("speaks runtime protocol 1");
    assert!(!run.stdout.contains("FAIL"), "{}", run.stdout);
    assert!(
        !run.stdout.contains("manifest reports each place"),
        "a version 1 runtime is asked about no place: {}",
        run.stdout
    );
}

fn mux_env(name: &str, program: &Path, sockets: &Sockets) -> Env {
    let mut env = Env::new(name);
    fs::write(
        &env.config,
        format!(
            "version = 1\n\n[data]\ndir = \"data\"\n\n[runtimes.mux]\nprogram = '{}'\ncallTimeoutMs = 10000\n",
            program.display()
        ),
    )
    .expect("config");
    env.set_var("TMUX_TMPDIR", &sockets.0.to_string_lossy());
    env
}

#[test]
fn doctor_reads_runtime_mux_s_version_from_its_manifest() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("version");
    let env = mux_env("runtime-version", &program, &sockets);
    let cli = env.fleet(&["version", "--json"]).ok().json()["cli"]["version"].clone();
    let report = env.fleet(&["doctor", "--json"]).json();
    let versions = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|c| c["area"] == "version")
        .unwrap_or_else(|| panic!("no version check: {report:#}"))
        .clone();
    assert_eq!(versions["level"], "ok", "{report:#}");
    assert_eq!(
        versions["detail"],
        format!("cli {0}, runtime mux {0}", cli.as_str().expect("cli")),
        "{report:#}"
    );
}

#[test]
fn runtime_mux_passes_the_conformance_suite() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("check");
    let env = mux_env("runtime-check", &program, &sockets);
    let run = env.fleet(&["runtime", "check", "--", program.to_str().unwrap()]);
    run.ok()
        .stdout_has("ok   two starts with one key leave one process")
        .stdout_has("ok   concurrent starts with one key leave one process")
        .stdout_has("ok   a protocol mismatch answers Unknown")
        .stdout_has("ok   an error answers Unknown, never None")
        .stdout_has("ok   a stop of an absent process succeeds")
        .stdout_has("ok   workspace: idempotent by key")
        .stdout_has("ok   workspace: release never removes what it did not make")
        .stdout_has("ok   manifest reports each place it was asked about")
        .stdout_has("ok   a version 2 request without a place answers Unknown")
        .stdout_has("ok   browse: lists the subdirectories and marks a repository")
        .stdout_has("ok   browse: a missing directory answers failed")
        .stdout_has("ok   branches: lists a repository's branches and its default")
        .stdout_has("ok   branches: a directory that is no repository answers failed")
        .stdout_has("ok   shell: a key no process holds answers none")
        .stdout_has("ok   shell: a running key answers how to open its shell, on each call")
        .stdout_has("ok   shell: a stop leaves nothing to open")
        .stdout_has("ok   pause: a key no process holds answers none")
        .stdout_has("ok   pause: stops the process and keeps the key until a start or a stop")
        .stdout_has("speaks runtime protocol 2");
    assert!(!run.stdout.contains("FAIL"), "{}", run.stdout);
}

fn logged_env(name: &str, program: &Path, sockets: &Sockets) -> (Env, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let env = mux_env(name, program, sockets);
    let log = env.root.join("calls.log");
    let script = env.root.join("logged.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nreq=$(cat)\nprintf '%s %s\\n' \"$1\" \"$req\" >> '{}'\nprintf '%s' \"$req\" | exec '{}' \"$@\"\n",
            log.display(),
            program.display()
        ),
    )
    .expect("script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
    let config = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        config.replace(&*program.to_string_lossy(), &script.to_string_lossy()),
    )
    .expect("config");
    (env, log)
}

fn calls(log: &Path) -> Vec<(String, Value)> {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let (verb, request) = line.split_once(' ').expect("verb and request");
            let request = serde_json::from_str(request)
                .unwrap_or_else(|e| panic!("{verb} request is not JSON ({e}): {request}"));
            (verb.to_string(), request)
        })
        .collect()
}

#[test]
fn fleet_names_the_local_place_in_each_request_to_a_version_2_runtime() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("place-v2");
    let (mut env, log) = logged_env("runtime-mux-place-v2", &program, &sockets);
    waiting_task(&mut env);
    let calls = calls(&log);
    assert!(calls.iter().any(|(v, _)| v == "start"), "{calls:?}");
    let local = json!({"name": "local", "config": {}});
    for (verb, request) in &calls {
        assert_eq!(request["protocol"], 2, "{verb} {request}");
        if verb == "manifest" {
            assert_eq!(request["places"], json!([local]), "{verb} {request}");
        } else {
            assert_eq!(request["place"], local, "{verb} {request}");
        }
    }
    assert_eq!(env.status("t1")["sessionState"], "Running");
    let mut long = support::task("t3", true);
    long["spec"]["host"] = json!("mux/local");
    env.apply(json!([long])).ok();
    env.reconcile().ok();
    assert_eq!(env.status("t3")["sessionState"], "Running");
}

#[test]
fn a_version_1_runtime_gets_version_1_requests() {
    if !reference_script_runs() {
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new("place-v1");
    let (mut env, log) = logged_env("runtime-script-place-v1", &script, &sockets);
    waiting_task(&mut env);
    let calls = calls(&log);
    assert!(calls.iter().any(|(v, _)| v == "start"), "{calls:?}");
    for (verb, request) in calls.iter().filter(|(v, _)| v != "manifest") {
        assert_eq!(request["protocol"], 1, "{verb} {request}");
        assert!(request.get("place").is_none(), "{verb} {request}");
    }
    assert_eq!(env.status("t1")["sessionState"], "Running");
}

#[test]
fn a_runtime_that_answers_none_to_everything_fails_the_check() {
    let env = Env::new("runtime-check-broken");
    let script = env.root.join("broken.sh");
    fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\nif [ \"$1\" = manifest ]; then\n  echo '{\"known\":{\"protocol\":1,\"verbs\":[\"manifest\",\"start\",\"inspect\",\"stop\"]}}'\nelse\n  echo '{\"known\":\"none\"}'\nfi\n",
    )
    .expect("script");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
    let run = env.fleet(&["runtime", "check", "--", script.to_str().unwrap()]);
    run.code(1)
        .stdout_has("FAIL a protocol mismatch answers Unknown")
        .stdout_has("FAIL an error answers Unknown, never None")
        .stdout_has("ok   inspect of an absent key answers None");
}

#[test]
fn a_missing_multiplexer_is_reported_by_name_at_the_first_start() {
    let Some(program) = runtime_mux() else { return };
    let sockets = Sockets::new("missing");
    let mut env = mux_env("runtime-mux-missing", &program, &sockets);
    let empty = env.root.join("empty-path");
    fs::create_dir_all(&empty).expect("empty PATH");
    env.only_on_path(&empty);
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", env.root.display())]);
    env.apply(json!([task])).ok();
    env.reconcile();
    let status = env.get("t1")["status"].clone();
    assert!(
        status.to_string().contains("tmux"),
        "the status names the missing multiplexer: {status:#}"
    );
    assert!(status["session"].is_null(), "{status:#}");
}

#[test]
fn a_scheme_the_runtime_does_not_list_is_refused_before_any_spawn() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("scheme");
    let mut env = mux_env("runtime-mux-scheme", &program, &sockets);
    env.set_var("RUNTIME_MUX_CONFIG", &mux_config(&env).to_string_lossy());
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!(["hg+https://example.test/app"]);
    env.apply(json!([task])).ok();
    env.reconcile();
    let status = env.get("t1")["status"].clone();
    assert_eq!(status["phase"], "Pending", "{status:#}");
    support::assert_condition(&status, "WorkspaceReady", "False", "WorkspaceUnsupported");
    assert!(
        status.to_string().contains("hg+https://example.test/app"),
        "the status names the URL: {status:#}"
    );
    assert!(status["session"].is_null(), "{status:#}");
    let key = format!(
        "fleet.{}",
        env.get("t1")["metadata"]["uid"].as_str().unwrap()
    );
    assert_eq!(
        mux_call(
            &program,
            &sockets,
            "inspect",
            &json!({"protocol": 1, "key": key})
        ),
        json!({"known": "none"}),
        "nothing was started"
    );
}

fn mux_config(env: &Env) -> PathBuf {
    let config = env.root.join("runtime-mux.toml");
    fs::write(
        &config,
        format!("[workspace]\nroot = '{}'\n", env.root.join("ws").display()),
    )
    .expect("runtime-mux config");
    config
}

fn mux_call(program: &Path, sockets: &Sockets, verb: &str, request: &Value) -> Value {
    mux_call_env(program, sockets, verb, request, &[])
}

fn mux_call_env(
    program: &Path,
    sockets: &Sockets,
    verb: &str,
    request: &Value,
    envs: &[(&str, &Path)],
) -> Value {
    use std::io::Write;
    let out = Command::new(program)
        .arg(verb)
        .env("TMUX_TMPDIR", &sockets.0)
        .envs(envs.iter().copied())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(request.to_string().as_bytes())?;
            child.wait_with_output()
        })
        .expect(verb);
    serde_json::from_slice(&out.stdout).expect("JSON")
}

#[test]
fn the_hosts_and_the_harness_profiles_are_listed_for_a_view() {
    let env = Env::new("runtime-hosts");
    fs::write(
        &env.config,
        "version = 1\n\n[data]\ndir = \"data\"\n\n[runtimes.mux]\nprogram = \"runtime-mux\"\n\n[runtimes.mux.places.local]\n\n[runtimes.mux.places.devbox]\nkind = \"ssh\"\ndestination = \"me@devbox\"\n",
    )
    .expect("config");
    let hosts = env.fleet(&["runtime", "hosts", "--json"]).ok().json();
    assert_eq!(hosts, json!({"hosts": ["local", "mux/devbox"]}));

    let two = Env::new("runtime-hosts-two");
    fs::write(
        &two.config,
        "version = 1\n\n[data]\ndir = \"data\"\n\n[runtimes.mux]\nprogram = \"runtime-mux\"\ndefault = true\n\n[runtimes.vm]\nprogram = \"runtime-vm\"\n\n[runtimes.vm.places.sandbox]\nimage = \"agent-base\"\n",
    )
    .expect("config");
    let hosts = two.fleet(&["runtime", "hosts", "--json"]).ok().json();
    assert_eq!(hosts, json!({"hosts": ["local", "vm/sandbox"]}));

    let single = Env::new("runtime-hosts-single");
    let any = single.fleet(&["runtime", "hosts", "--json"]).ok().json();
    assert_eq!(any, json!({"hosts": null}));

    let version = env.fleet(&["version", "--json"]).ok().json();
    assert_eq!(
        version["harnesses"],
        json!([
            "claude",
            "claude-code",
            "codex",
            "pi",
            "opencode",
            "kilo",
            "omp",
            "qwen",
            "copilot",
            "kimi"
        ])
    );
}

#[test]
fn a_config_whose_hosts_are_ambiguous_is_refused_at_load() {
    let head = "version = 1\n\n[data]\ndir = \"data\"\n\n";
    for (name, body, why) in [
        (
            "no-default",
            "[runtimes.mux]\nprogram = \"runtime-mux\"\n\n[runtimes.vm]\nprogram = \"runtime-vm\"\n",
            "several runtimes and no default",
        ),
        (
            "two-defaults",
            "[runtimes.mux]\nprogram = \"runtime-mux\"\ndefault = true\n\n[runtimes.vm]\nprogram = \"runtime-vm\"\ndefault = true\n",
            "several runtimes and no default",
        ),
        (
            "old-hosts",
            "[runtimes.mux]\nprogram = \"runtime-mux\"\n\n[hosts.local]\nruntime = \"mux\"\n",
            "[hosts] is gone",
        ),
        (
            "bad-place",
            "[runtimes.mux]\nprogram = \"runtime-mux\"\n\n[runtimes.mux.places.Dev_Box]\n",
            "runtimes.mux.places.Dev_Box",
        ),
        (
            "bad-runtime",
            "[runtimes.Mux]\nprogram = \"runtime-mux\"\n",
            "runtimes.Mux",
        ),
    ] {
        let env = Env::new(&format!("runtime-config-{name}"));
        fs::write(&env.config, format!("{head}{body}")).expect("config");
        env.fleet(&["list", "task"]).code(1).stderr_has(why);
    }
}

#[test]
fn a_default_runtime_without_a_local_place_loads_and_local_names_nothing() {
    let env = Env::new("runtime-no-local");
    fs::write(
        &env.config,
        "version = 1\n\n[data]\ndir = \"data\"\n\n[runtimes.vm]\nprogram = \"runtime-vm\"\n\n[runtimes.vm.places.sandbox]\nimage = \"agent-base\"\n",
    )
    .expect("config");
    env.fleet(&["list", "task"]).ok();
    let hosts = env.fleet(&["runtime", "hosts", "--json"]).ok().json();
    assert_eq!(hosts, json!({"hosts": ["vm/sandbox"]}));
    let places = env.fleet(&["runtime", "places", "--json"]).ok().json();
    assert!(places["local"].is_null(), "{places:#}");
    let why = "no `local` place: the default runtime vm has none; add [runtimes.vm.places.local] or name a place";
    assert_eq!(places["localWhy"], why, "{places:#}");
    env.fleet(&["runtime", "places"]).ok().stdout_has(why);
    let mut named = support::task("t1", true);
    named["spec"]["host"] = json!("local");
    let mut unnamed = support::task("t2", true);
    unnamed["spec"]
        .as_object_mut()
        .expect("spec")
        .remove("host");
    for task in [named, unnamed] {
        env.apply(task.clone()).code(1).stderr_has(why);
    }
    let mut placed = support::task("t3", true);
    placed["spec"]["host"] = json!("vm/sandbox");
    env.apply(placed).ok();
    let config = support::object("TaskConfig", "on-vm", json!({"host": "vm/sandbox"}));
    let mut inherits = support::task("t4", true);
    inherits["spec"]
        .as_object_mut()
        .expect("spec")
        .remove("host");
    inherits["spec"]["configRef"] = json!({"name": "on-vm"});
    env.apply(json!([config, inherits])).ok();
    let mut later = inherits.clone();
    later["metadata"]["name"] = json!("t5");
    env.apply(later).ok();
    let bare = support::object("TaskConfig", "bare", json!({}));
    let mut on_bare = support::task("t6", true);
    on_bare["spec"]
        .as_object_mut()
        .expect("spec")
        .remove("host");
    on_bare["spec"]["configRef"] = json!({"name": "bare"});
    env.apply(json!([bare, on_bare])).code(1).stderr_has(why);
    let first = support::object("TaskConfig", "twice", json!({"host": "vm/sandbox"}));
    let last = support::object("TaskConfig", "twice", json!({}));
    let mut on_twice = support::task("t7", true);
    on_twice["spec"]
        .as_object_mut()
        .expect("spec")
        .remove("host");
    on_twice["spec"]["configRef"] = json!({"name": "twice"});
    env.apply(json!([first, last, on_twice]))
        .code(1)
        .stderr_has(why);
    let default = support::object("TaskConfig", "default", json!({"host": "vm/sandbox"}));
    let mut plain = support::task("t8", true);
    plain["spec"].as_object_mut().expect("spec").remove("host");
    env.apply(json!([default, plain])).ok();
    let mut later = plain.clone();
    later["metadata"]["name"] = json!("t9");
    env.apply(later).ok();
}

#[test]
fn the_places_are_listed_with_their_readiness() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("places");
    let env = mux_env("runtime-places", &program, &sockets);
    let v1 = env.root.join("v1.sh");
    fs::write(
        &v1,
        "#!/bin/sh\ncat >/dev/null\necho '{\"known\":{\"protocol\":1,\"verbs\":[\"manifest\",\"start\",\"inspect\",\"stop\"]}}'\n",
    )
    .expect("v1 runtime");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&v1, fs::Permissions::from_mode(0o755)).expect("chmod");
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        format!(
            "{cfg}default = true\n\n[runtimes.mux.places.local]\n\n[runtimes.mux.places.devbox]\nkind = \"ssh\"\ndestination = \"me@devbox\"\n\n[runtimes.old]\nprogram = '{}'\n\n[runtimes.old.places.local]\n\n[runtimes.old.places.far]\n\n[runtimes.gone]\nprogram = '{}'\n",
            v1.display(),
            env.root.join("no-such-runtime").display()
        ),
    )
    .expect("config");
    let places = env.fleet(&["runtime", "places", "--json"]).ok().json();
    let rows = places["places"].as_array().expect("places");
    let row = |host: &str| {
        rows.iter()
            .find(|r| r["place"] == host)
            .unwrap_or_else(|| panic!("no {host} in {places:#}"))
            .clone()
    };
    assert_eq!(rows.len(), 5, "{places:#}");
    let local = row("mux/local");
    assert_eq!(local["ready"], true, "{places:#}");
    assert_eq!(local["platform"], "posix", "{places:#}");
    assert_eq!(local["alias"], "local", "{places:#}");
    let devbox = row("mux/devbox");
    assert_eq!(devbox["ready"], false, "{places:#}");
    assert!(devbox["why"].is_string(), "{places:#}");
    assert_eq!(row("old/local")["ready"], true, "{places:#}");
    let far = row("old/far");
    assert_eq!(far["ready"], false, "{places:#}");
    assert!(
        far["why"]
            .as_str()
            .is_some_and(|w| w.contains("protocol 1")),
        "{places:#}"
    );
    let gone = row("gone/local");
    assert!(gone["ready"].is_null(), "unknown, not absent: {places:#}");
    assert!(gone["why"].is_string(), "{places:#}");
}

#[test]
fn a_task_on_a_place_passes_its_config_to_the_runtime() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("place-config");
    let (env, log) = logged_env("runtime-place-config", &program, &sockets);
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port()
        .to_string();
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        format!(
            "{cfg}\n[runtimes.mux.places.local]\n\n[runtimes.mux.places.devbox]\nkind = \"ssh\"\ndestination = \"me@127.0.0.1\"\nssh_opts = [\"-F\", \"/dev/null\", \"-p\", \"{closed}\"]\n"
        ),
    )
    .expect("config");
    let mut task = support::task("t1", true);
    task["spec"]["host"] = json!("mux/devbox");
    env.apply(json!([task])).ok();
    env.reconcile();
    let calls = calls(&log);
    let devbox = json!({"name": "devbox", "config": {
        "kind": "ssh",
        "destination": "me@127.0.0.1",
        "ssh_opts": ["-F", "/dev/null", "-p", closed],
    }});
    let starts: Vec<&Value> = calls
        .iter()
        .filter(|(v, _)| v == "start")
        .map(|(_, r)| r)
        .collect();
    assert!(!starts.is_empty(), "{calls:?}");
    for start in starts {
        assert_eq!(start["place"], devbox, "{start}");
    }
    let status = env.get("t1")["status"].clone();
    assert!(status["session"].is_null(), "{status:#}");
    assert!(status.to_string().contains("devbox"), "{status:#}");
}

#[test]
fn a_task_on_an_unknown_place_waits_unreachable() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("place-unknown");
    let env = mux_env("runtime-place-unknown", &program, &sockets);
    let mut task = support::task("t1", true);
    task["spec"]["host"] = json!("mux/nowhere");
    env.apply(json!([task])).ok();
    env.reconcile();
    let status = env.get("t1")["status"].clone();
    support::assert_condition(&status, "HostReachable", "False", "Unreachable");
    assert!(status["session"].is_null(), "{status:#}");
}

#[test]
fn a_task_starts_and_is_released_through_runtime_mux() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("task");
    let mut env = mux_env("runtime-mux-task", &program, &sockets);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let harness = bin.join("claude");
    fs::write(&harness, "#!/bin/sh\nexec sleep 300\n").expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", bin.display())));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");

    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    env.reconcile().ok();

    let session = env.get("t1")["status"]["session"].clone();
    let uid = env.get("t1")["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let key = format!("fleet.{uid}");
    assert_eq!(session["id"], key.as_str(), "{session:#}");
    assert_eq!(session["host"], "local", "{session:#}");
    let panes = |format: &str| {
        String::from_utf8_lossy(&sockets.tmux(&["list-panes", "-a", "-F", format]).stdout)
            .into_owned()
    };
    let listed = panes("#{pane_id} #{pane_current_path}");
    let id = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();

    env.reconcile().ok();
    assert_eq!(env.get("t1")["status"]["session"]["id"], key.as_str());
    // tmux keeps the server alive on the session's own first window.
    let listed = panes("#{pane_id} #{window_name}");
    assert_eq!(
        listed.lines().filter(|l| l.contains(" tb-")).count(),
        1,
        "{listed}"
    );

    env.fleet(&["delete", "task", "t1"]).ok();
    env.reconcile().ok();
    assert_eq!(env.get("t1")["status"]["phase"], "Releasing");
    sockets.tmux(&["send-keys", "-t", &id, "C-c"]);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        env.reconcile();
        let gone = env.fleet(&["get", "task", "t1"]).code != Some(0);
        if gone {
            break;
        }
        assert!(Instant::now() < deadline, "{:#}", env.get("t1"));
        std::thread::sleep(Duration::from_millis(200));
    }
    let inspect = Command::new(&program)
        .arg("inspect")
        .env("TMUX_TMPDIR", &sockets.0)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(json!({"protocol": 1, "key": key}).to_string().as_bytes())?;
            child.wait_with_output()
        })
        .expect("inspect");
    let answer: Value = serde_json::from_slice(&inspect.stdout).expect("JSON");
    assert_eq!(answer, json!({"known": "none"}), "the release stopped it");
}

#[test]
fn an_env_provider_command_source_reaches_a_task_on_runtime_mux() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("env-from");
    let mut env = mux_env("runtime-mux-env-from", &program, &sockets);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let harness = bin.join("claude");
    fs::write(
        &harness,
        "#!/bin/sh\nenv > \"$PWD/env.log.tmp\"\nmv \"$PWD/env.log.tmp\" \"$PWD/env.log\"\nexec sleep 300\n",
    )
    .expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", bin.display())));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");

    env.apply(json!([
        {"apiVersion": "fleet/v1alpha1", "kind": "EnvProvider", "metadata": {"name": "x"},
         "spec": {"sources": [{"type": "command", "program": "/bin/sh",
                               "args": ["-c", r#"printf '{"FLEET_ENV_FROM_PROBE":"from-command"}'"#],
                               "cwd": env.root, "timeoutMs": 5000}]}},
        {"apiVersion": "fleet/v1alpha1", "kind": "TaskConfig", "metadata": {"name": "shared"},
         "spec": {"envFrom": [{"name": "x"}]}},
    ]))
    .ok();
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    task["spec"]["configRef"] = json!({"name": "shared"});
    env.apply(json!([task])).ok();

    let logged = work.join("env.log");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !logged.is_file() {
        let pass = env.reconcile();
        assert!(
            Instant::now() < deadline,
            "the agent never started: {}{}",
            pass.stdout,
            pass.stderr
        );
        std::thread::sleep(Duration::from_millis(300));
    }
    let seen = fs::read_to_string(&logged).expect("env.log");
    assert!(
        seen.lines()
            .any(|l| l == "FLEET_ENV_FROM_PROBE=from-command"),
        "{seen}"
    );
}

const CLAUDE: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$PWD/argv.log"
printf '%s' "$FLEET_SIGNAL_FILE" > "$PWD/signal-file"
fleet signal idle
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  fleet signal prompt "{\"prompt\":\"$line\",\"session_id\":\"conv-1\"}"
  fleet signal idle
done
"#;

fn reconcile_until(env: &Env, what: &str, done: impl Fn(&Env) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(env) {
        assert!(
            Instant::now() < deadline,
            "{what}: {:#}",
            env.get("t1")["status"]
        );
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
}

fn starts(work: &Path) -> Vec<Vec<String>> {
    fs::read_to_string(work.join("argv.log"))
        .unwrap_or_default()
        .split("--\n")
        .filter(|s| !s.is_empty())
        .map(|s| s.lines().map(str::to_string).collect())
        .collect()
}

#[test]
fn a_task_on_runtime_mux_reports_through_fleet_signal_and_revives_on_its_conversation() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("signal");
    let mut env = mux_env("runtime-mux-signal", &program, &sockets);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, CLAUDE).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on the prompt hook's proof", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let first = &starts(&work)[0];
    let settings: Vec<&String> = first
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "--settings")
        .map(|(i, _)| &first[i + 1])
        .collect();
    // Claude Code keeps the last of several --settings.
    assert_eq!(settings.len(), 1, "{first:?}");
    let settings: Value = serde_json::from_str(settings[0]).expect("settings JSON");
    for event in ["SessionStart", "UserPromptSubmit", "PreToolUse", "Stop"] {
        let command = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_default();
        assert!(
            command.starts_with("fleet signal "),
            "{event}: {settings:#}"
        );
    }
    let file = fs::read_to_string(work.join("signal-file")).expect("FLEET_SIGNAL_FILE");
    assert!(Path::new(&file).is_file(), "the agent reported to {file:?}");
    reconcile_until(&env, "idle, as its hooks report", |env| {
        env.get("t1")["status"]["sessionState"] == "Idle"
    });
    assert_eq!(env.get("t1")["status"]["conversation"], "conv-1");

    let id = env.get("t1")["status"]["session"]["id"].clone();
    let panes = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&panes.stdout).into_owned();
    let pane = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    sockets.tmux(&["send-keys", "-t", &pane, "C-c"]);
    reconcile_until(&env, "revived", |_| starts(&work).len() >= 2);
    let second = &starts(&work)[1];
    assert_eq!(
        &second[second.len() - 2..],
        ["--resume", "conv-1"],
        "{second:?}"
    );
    reconcile_until(&env, "the window back", |env| {
        support::condition(&env.get("t1")["status"], "SessionLost")["status"] == "False"
    });
    let status = env.get("t1")["status"].clone();
    assert_eq!(status["session"]["id"], id, "{status:#}");
    assert!(
        status["conditions"].as_array().is_some_and(|cs| !cs
            .iter()
            .any(|c| c["type"] == "WorkerGone" && c["status"] == "True")),
        "{status:#}"
    );
}

const PASTING_CLAUDE: &str = r#"#!/bin/sh
fleet signal idle
stty raw -echo
printf '\033[>4;2m\033[?2004h'
perl -e '
$/ = "\r";
while (defined(my $text = <STDIN>)) {
  chomp $text;
  $text =~ s/\e\[20[01]~//g;
  open(my $log, ">", "typed.log") or die; print $log $text; close $log;
  my $json = "\n\n<pasted_content id=\"ab12\">\n$text\n</pasted_content id=\"ab12\">\n";
  $json =~ s/(["\\])/\\$1/g;
  $json =~ s/\n/\\n/g;
  system("fleet", "signal", "prompt", "{\"prompt\":\"$json\",\"session_id\":\"conv-1\"}");
  system("fleet", "signal", "idle");
}'
"#;

#[test]
fn a_multi_line_brief_reaches_claude_code_whole_and_its_wrapped_paste_proves_it() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("paste");
    let mut env = mux_env("runtime-mux-paste", &program, &sockets);
    stand_in(&mut env, "claude", "claude", PASTING_CLAUDE);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let brief = "Find the cause.\n\nThen fix it:\n1. a test\n2. the fix";
    let mut task = support::task("t1", true);
    task["spec"]["brief"] = json!(brief);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "typed", |_| work.join("typed.log").is_file());
    let typed = fs::read_to_string(work.join("typed.log")).unwrap_or_default();
    assert_eq!(typed, brief, "the agent reads the brief's line breaks");
    reconcile_until(&env, "briefed on the wrapped paste's proof", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
}

const TIMING_CLAUDE: &str = r#"#!/bin/sh
fleet signal idle
stty raw -echo
printf '\033[>4;2m\033[?2004h'
perl -e '
my ($typed, $paste, $buf, $in, $split) = ("", "", "", 0, 0);
my ($open, $close) = ("\e[200~", "\e[201~");
sub submit {
  open(my $log, ">", "typed.log") or die; print $log $typed . $paste; close $log;
  if ($split) { open(my $s, ">", "split.log") or die; print $s $typed; close $s; }
  my $prompt = $typed;
  $prompt .= "\n\n<pasted_content id=\"ab12\">\n$paste\n</pasted_content id=\"ab12\">\n" if length $paste;
  $prompt =~ s/(["\\])/\\$1/g;
  $prompt =~ s/\n/\\n/g;
  system("fleet", "signal", "prompt", "{\"prompt\":\"$prompt\",\"session_id\":\"conv-1\"}");
  system("fleet", "signal", "idle");
  ($typed, $paste, $split) = ("", "", 0);
}
while (1) {
  my $rin = ""; vec($rin, 0, 1) = 1;
  if (!select(my $rout = $rin, undef, undef, $in ? 0.005 : undef)) {
    $typed .= $paste; $paste = ""; $split = 1;
    next;
  }
  sysread(STDIN, my $chunk, 65536) or last;
  $buf .= $chunk;
  while (length $buf) {
    if ($in) {
      my $at = index($buf, $close);
      if ($at < 0) {
        my $keep = length($buf) < 5 ? length($buf) : 5;
        $paste .= substr($buf, 0, length($buf) - $keep, "");
        last;
      }
      $paste .= substr($buf, 0, $at);
      $buf = substr($buf, $at + length $close);
      $in = 0;
    } elsif (index($buf, $open) == 0) {
      $buf = substr($buf, length $open);
      $in = 1;
    } elsif (index($open, $buf) == 0) {
      last;
    } elsif (substr($buf, 0, 1) eq "\r") {
      $buf = substr($buf, 1);
      submit();
    } else {
      $typed .= substr($buf, 0, 1, "");
    }
  }
}'
"#;

fn long_brief() -> String {
    (1..=75)
        .map(|n| format!("- **Step {n}**: check `#{{pane}}` and $HOME; then write it down."))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_long_brief_reaches_claude_code_as_one_paste() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("long-paste");
    let mut env = mux_env("runtime-mux-long-paste", &program, &sockets);
    stand_in(&mut env, "claude", "claude", TIMING_CLAUDE);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let brief = long_brief();
    assert!(brief.len() > 4000, "{}", brief.len());
    let mut task = support::task("t1", true);
    task["spec"]["brief"] = json!(brief);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "typed", |_| work.join("typed.log").is_file());
    let split = fs::read_to_string(work.join("split.log"));
    assert!(
        split.is_err(),
        "the paste paused after {} bytes, so the agent took them as typed text",
        split.map_or(0, |s| s.len())
    );
    let typed = fs::read_to_string(work.join("typed.log")).unwrap_or_default();
    assert_eq!(typed, brief, "the agent reads the brief whole");
    reconcile_until(&env, "briefed", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
}

#[test]
fn a_task_on_runtime_mux_is_revived_on_its_conversation_after_a_reboot() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("reboot");
    let mut env = mux_env("runtime-mux-reboot", &program, &sockets);
    stand_in(&mut env, "claude", "claude", CLAUDE);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "the conversation its hooks report", |env| {
        env.get("t1")["status"]["conversation"] == "conv-1"
    });
    let id = env.get("t1")["status"]["session"]["id"].clone();

    sockets.tmux(&["kill-server"]);
    for entry in fs::read_dir(&sockets.0).expect("socket dir") {
        fs::remove_dir_all(entry.expect("entry").path()).expect("tmux's private directory");
    }

    reconcile_until(&env, "revived after the reboot", |_| {
        starts(&work).len() >= 2
    });
    let second = &starts(&work)[1];
    assert_eq!(
        &second[second.len() - 2..],
        ["--resume", "conv-1"],
        "{second:?}"
    );
    reconcile_until(&env, "the window back", |env| {
        support::condition(&env.get("t1")["status"], "SessionLost")["status"] == "False"
    });
    let status = env.get("t1")["status"].clone();
    assert_eq!(status["session"]["id"], id, "{status:#}");
    assert!(
        status["conditions"].as_array().is_some_and(|cs| !cs
            .iter()
            .any(|c| c["type"] == "WorkerGone" && c["status"] == "True")),
        "{status:#}"
    );
}

const WORKING_CLAUDE: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$PWD/argv.log"
fleet signal idle
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  printf '%s\n' "$line" >> "$PWD/typed.log"
  fleet signal prompt "{\"prompt\":\"$line\",\"session_id\":\"conv-1\"}"
  fleet signal working
done
"#;

#[test]
fn a_shipped_task_lost_in_a_reboot_reads_lost_and_an_attach_revives_it_on_its_conversation() {
    use support::view::View;
    use term_stream::{Down, Size, Up};
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("shipped");
    let mut env = mux_env("runtime-mux-shipped", &program, &sockets);
    stand_in(&mut env, "claude", "claude", WORKING_CLAUDE);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "working on its conversation", |env| {
        let status = &env.get("t1")["status"];
        status["conversation"] == "conv-1" && status["agentState"] == "working"
    });
    env.fleet(&[
        "result",
        "t1",
        "--generation",
        "1",
        "--outcome",
        "shipped",
        "--summary",
        "done",
        "--output",
        "prUrl=\"https://example.test/pr/1\"",
    ])
    .ok();
    reconcile_until(&env, "concluded", |env| {
        env.get("t1")["status"]["phase"] == "Concluded"
    });
    let key = env.get("t1")["status"]["session"]["id"]
        .as_str()
        .expect("a session id")
        .to_string();

    sockets.tmux(&["kill-server"]);
    for entry in fs::read_dir(&sockets.0).expect("socket dir") {
        fs::remove_dir_all(entry.expect("entry").path()).expect("tmux's private directory");
    }
    env.expire_session_check("t1");
    reconcile_until(&env, "read as lost", |env| {
        env.get("t1")["status"]["sessionState"] == "Lost"
    });
    let status = env.get("t1")["status"].clone();
    assert_eq!(status["agentState"], Value::Null, "{status:#}");
    support::assert_condition(&status, "SessionLost", "True", "WindowGone");
    env.reconcile().ok();
    assert_eq!(
        starts(&work).len(),
        1,
        "a concluded worker at rest stays at rest"
    );

    let attach = std::cell::RefCell::new(env.spawn(&["attach", "t1", "--json"]));
    reconcile_until(&env, "revived for the view", |_| {
        attach.borrow_mut().try_wait().expect("attach").is_some()
    });
    let run = support::wait(attach.into_inner());
    let answer = run.ok().json();
    let argv: Vec<String> = serde_json::from_value(answer["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer:#}"));
    assert_eq!(
        argv.last().map(String::as_str),
        Some(key.as_str()),
        "{argv:?}"
    );
    let all = starts(&work);
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(
        &all[1][all[1].len() - 2..],
        ["--resume", "conv-1"],
        "{all:?}"
    );
    let status = env.get("t1")["status"].clone();
    assert_eq!(status["phase"], "Concluded", "{status:#}");
    support::assert_condition(&status, "SessionLost", "False", "Revived");
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile().ok();
    }
    let typed = fs::read_to_string(work.join("typed.log")).unwrap_or_default();
    assert_eq!(
        typed.lines().count(),
        1,
        "only the brief, nothing at the revive: {typed:?}"
    );

    let mut view = View::argv(&argv);
    assert!(matches!(view.next(), Down::Hello(_)), "no hello first");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until("a snapshot", |f| matches!(f, Down::Snapshot(_)));
    view.send(&Up::Detach);
    assert!(view.close().success());
}

const UNSAVED_CLAUDE: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$PWD/argv.log"
case " $* " in *" --resume "*)
  echo "No conversation found with session ID: conv-1"
  exit 1 ;;
esac
fleet signal idle
while IFS= read -r line; do
  fleet signal prompt "{\"prompt\":\"x\",\"session_id\":\"conv-1\"}"
  fleet signal idle
done
"#;

#[test]
fn a_session_that_cannot_resume_is_marked_so_and_neither_revived_nor_viewed_again() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("unsaved");
    let mut env = mux_env("runtime-mux-unsaved", &program, &sockets);
    stand_in(&mut env, "claude", "claude", UNSAVED_CLAUDE);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "the conversation its hooks report", |env| {
        env.get("t1")["status"]["conversation"] == "conv-1"
    });

    sockets.tmux(&["kill-server"]);
    for entry in fs::read_dir(&sockets.0).expect("socket dir") {
        fs::remove_dir_all(entry.expect("entry").path()).expect("tmux's private directory");
    }
    reconcile_until(&env, "marked as one that cannot resume", |env| {
        env.expire_wait("t1");
        env.get("t1")["status"]["conditions"]
            .as_array()
            .is_some_and(|cs| {
                cs.iter()
                    .any(|c| c["type"] == "SessionLost" && c["reason"] == "CannotResume")
            })
    });
    let revives = starts(&work).len();
    for _ in 0..5 {
        env.expire_wait("t1");
        env.reconcile().ok();
    }
    assert_eq!(
        starts(&work).len(),
        revives,
        "no revive after it cannot resume"
    );
    let status = env.get("t1")["status"].clone();
    assert_eq!(status["sessionState"], "Lost", "{status:#}");
    support::assert_condition(&status, "NeedsAttention", "True", "CannotResume");

    env.fleet(&["attach", "t1", "--json"])
        .code(3)
        .stderr_has("cannot resume");
}

const SLOW_CLAUDE: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$PWD/argv.log"
case " $* " in *" --resume "*) ;; *) fleet signal idle ;; esac
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  printf '%s\n' "$line" >> "$PWD/typed.log"
  fleet signal prompt "{\"prompt\":\"$line\",\"session_id\":\"conv-1\"}"
  fleet signal idle
done
"#;

#[test]
fn a_revived_agent_is_ready_only_once_it_reports_again() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("restart");
    let mut env = mux_env("runtime-mux-restart", &program, &sockets);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, SLOW_CLAUDE).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "briefed", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let typed = || {
        fs::read_to_string(work.join("typed.log"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    assert_eq!(typed(), 1, "the brief");

    let panes = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&panes.stdout).into_owned();
    let pane = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    sockets.tmux(&["send-keys", "-t", &pane, "C-c"]);
    reconcile_until(&env, "revived", |_| starts(&work).len() >= 2);
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
    assert_eq!(typed(), 1, "nothing typed before the agent reports");

    let file = env
        .data
        .join("signals")
        .join(format!("{}.jsonl", env.session_key("t1")));
    let mut cmd = env.command(&["signal", "idle"]);
    cmd.env("FLEET_SIGNAL_FILE", &file);
    support::wait(cmd.spawn().expect("fleet signal")).ok();
    reconcile_until(&env, "the catch-up note typed", |_| typed() >= 2);
}

const TYPING_CLAUDE: &str = r#"#!/bin/bash
{ printf '%s\n' "$@"; echo --; } >> "$PWD/argv.log"
fleet signal idle
while IFS= read -r line; do
  text=$line
  while IFS= read -r -t 0.5 more; do text=$text$'\n'$more; done
  text=$(printf '%s' "$text" | tr -d '\033' | sed 's/\[20[01]~//g')
  printf '%s\n' "$text" >> "$PWD/typed.log"
  json=$(printf '%s' "$text" | awk 'BEGIN { ORS = "" } { gsub(/\\/, "\\\\"); gsub(/"/, "\\\""); if (NR > 1) print "\\n"; print }')
  fleet signal prompt "{\"prompt\":\"$json\",\"session_id\":\"conv-1\"}"
  fleet signal idle
done
"#;

fn ask(env: &Env, program: &Path, sockets: &Sockets, verb: &str, request: Value) -> Value {
    let out = Command::new(program)
        .arg(verb)
        .env("TMUX_TMPDIR", &sockets.0)
        .env("HOME", &env.root)
        .env_remove("XDG_DATA_HOME")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(request.to_string().as_bytes())?;
            child.wait_with_output()
        })
        .expect(verb);
    serde_json::from_slice(&out.stdout).expect("JSON")
}

fn paused_on_mux(
    name: &str,
    program: &Path,
    sockets: &Sockets,
    harness: &str,
) -> (Env, String, PathBuf) {
    let mut env = mux_env(name, program, sockets);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, harness).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    task["spec"]["configRef"] = json!({"name": "pausing"});
    let config = support::object("TaskConfig", "pausing", json!({"pauseAfterMs": 1}));
    env.apply(json!([config, task])).ok();

    reconcile_until(&env, "paused while idle", |env| {
        let status = env.get("t1")["status"].clone();
        status["conditions"].as_array().is_some_and(|cs| {
            cs.iter()
                .any(|c| c["type"] == "Paused" && c["status"] == "True")
        })
    });
    let status = env.get("t1")["status"].clone();
    support::assert_condition(&status, "Paused", "True", "Idle");
    assert_eq!(status["conversation"], "conv-1", "{status:#}");
    let key = status["session"]["id"].as_str().unwrap().to_string();
    (env, key, work)
}

#[test]
fn an_idle_session_on_runtime_mux_is_stopped_and_a_message_resumes_its_conversation() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("pause");
    let (env, key, work) = paused_on_mux("runtime-mux-pause", &program, &sockets, TYPING_CLAUDE);

    let listed = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(
        !listed.lines().any(|l| l == work.to_string_lossy()),
        "no pane runs in the workspace: {listed}"
    );
    assert_eq!(
        ask(
            &env,
            &program,
            &sockets,
            "inspect",
            json!({"protocol": 1, "key": key})
        ),
        json!({"known": "paused"})
    );
    let keys = ask(&env, &program, &sockets, "list", json!({"protocol": 1}));
    assert!(
        keys["known"]["keys"]
            .as_array()
            .is_some_and(|ks| ks.iter().any(|k| *k == key.as_str())),
        "{keys}"
    );
    env.reconcile().ok();
    let status = env.get("t1")["status"].clone();
    assert_eq!(status["sessionState"], "Paused", "{status:#}");
    for c in ["WorkerGone", "SessionLost"] {
        assert!(
            !status["conditions"]
                .as_array()
                .is_some_and(|cs| cs.iter().any(|x| x["type"] == c && x["status"] == "True")),
            "{c} in {status:#}"
        );
    }
    assert_eq!(starts(&work).len(), 1, "nothing restarted it");

    env.fleet(&["send", "t1", "next message"]).ok();
    let typed = || fs::read_to_string(work.join("typed.log")).unwrap_or_default();
    reconcile_until(&env, "the message typed", |_| {
        typed().lines().any(|l| l == "next message")
    });
    let lines = typed();
    assert_eq!(
        lines.lines().collect::<Vec<_>>()[1..],
        ["next message"],
        "the brief, then the message and nothing of Fleet's own: {lines}"
    );
    let all = starts(&work);
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(
        &all[1][all[1].len() - 2..],
        ["--resume", "conv-1"],
        "{all:?}"
    );
    let status = env.get("t1")["status"].clone();
    assert_eq!(status["session"]["id"], key.as_str(), "{status:#}");
    let paused = support::condition(&status, "Paused");
    assert!(
        paused["reason"] == "Resumed" || paused["reason"] == "Idle",
        "{status:#}"
    );
}

#[test]
fn attaching_to_a_paused_session_on_runtime_mux_resumes_it_and_streams_it() {
    use support::view::View;
    use term_stream::{Down, Size, Up};
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("attach-paused");
    let (env, key, work) = paused_on_mux(
        "runtime-mux-attach-paused",
        &program,
        &sockets,
        &TYPING_CLAUDE.replace(
            "fleet signal idle",
            r#"case " $* " in *" --resume "*) fleet signal working ;; *) fleet signal idle ;; esac"#,
        ),
    );

    let attach = std::cell::RefCell::new(env.spawn(&["attach", "t1", "--wake", "--json"]));
    reconcile_until(&env, "resumed for the view", |env| {
        attach.borrow_mut().try_wait().expect("attach").is_some()
            || support::condition(&env.get("t1")["status"], "Paused")["status"] == "False"
    });
    let run = support::wait(attach.into_inner());
    let answer = run.ok().json();
    let argv: Vec<String> = serde_json::from_value(answer["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer:#}"));
    assert_eq!(
        argv.last().map(String::as_str),
        Some(key.as_str()),
        "{argv:?}"
    );

    let all = starts(&work);
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(
        &all[1][all[1].len() - 2..],
        ["--resume", "conv-1"],
        "{all:?}"
    );
    let status = env.get("t1")["status"].clone();
    support::assert_condition(&status, "Paused", "False", "Resumed");
    assert_eq!(status["session"]["id"], key.as_str(), "{status:#}");

    let mut view = View::argv(&argv);
    assert!(matches!(view.next(), Down::Hello(_)), "no hello first");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until("a snapshot", |f| matches!(f, Down::Snapshot(_)));
    view.send(&Up::Detach);
    assert!(view.close().success());

    let file = env
        .data
        .join("signals")
        .join(format!("{}.jsonl", env.session_key("t1")));
    let mut cmd = env.command(&["signal", "idle"]);
    cmd.env("FLEET_SIGNAL_FILE", &file);
    support::wait(cmd.spawn().expect("fleet signal")).ok();
    let typed = || fs::read_to_string(work.join("typed.log")).unwrap_or_default();
    reconcile_until(&env, "paused again, or something typed", |env| {
        typed().lines().count() > 1
            || support::condition(&env.get("t1")["status"], "Paused")["status"] == "True"
    });
    let typed = typed();
    assert_eq!(
        typed.lines().count(),
        1,
        "only the brief was typed: {typed}"
    );
}

const STUB_AGENT: &str = r#"#!/bin/sh
printf '%s' "$FLEET_HARNESS" > "$PWD/harness"
fleet signal working
printf '%s' '{"session_id":"s1"}' | fleet signal idle
exec sleep 300
"#;

const STUB_PROFILE: &str = r#"name = "stub"
program = "stub-agent"
authority = "full"

[payload]
conversation = ["session_id"]
"#;

#[test]
fn a_harness_that_is_only_a_user_profile_runs_and_reports_on_runtime_mux() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("stub");
    let mut env = mux_env("runtime-mux-stub", &program, &sockets);
    let harnesses = env.config.parent().unwrap().join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses dir");
    fs::write(harnesses.join("stub.toml"), STUB_PROFILE).expect("profile");
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let agent = bin.join("stub-agent");
    fs::write(&agent, STUB_AGENT).expect("agent");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let version = env.fleet(&["version", "--json"]).ok().json();
    assert_eq!(
        version["harnesses"],
        json!([
            "stub",
            "claude",
            "claude-code",
            "codex",
            "pi",
            "opencode",
            "kilo",
            "omp",
            "qwen",
            "copilot",
            "kimi"
        ]),
        "a view offers the user's harness"
    );
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("stub");
    task["spec"].as_object_mut().unwrap().remove("brief");
    task["spec"].as_object_mut().unwrap().remove("outputs");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "idle on conversation s1", |env| {
        let status = env.get("t1")["status"].clone();
        status["agentState"] == "idle" && status["conversation"] == "s1"
    });
    assert_eq!(
        fs::read_to_string(work.join("harness")).unwrap_or_default(),
        "stub",
        "Fleet names the harness to its hooks"
    );
}

const PATH_AGENT: &str = r#"#!/bin/sh
printf '%s' "$0" > "$PWD/agent"
printf '%s' "$PATH" > "$PWD/path"
fake-tool > "$PWD/tool"
exec sleep 300
"#;

#[test]
fn a_task_path_reaches_its_pane_on_runtime_mux() {
    let (Some(built), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &Path, script: &str| {
        fs::write(path, script).expect("script");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
    };
    let sockets = Sockets::new("taskpath");
    let mut env = mux_env("runtime-mux-task-path", &built, &sockets);
    let mux_dir = env.root.join("mux");
    fs::create_dir_all(&mux_dir).expect("mux dir");
    let program = mux_dir.join("runtime-mux");
    fs::hard_link(&built, &program)
        .or_else(|_| fs::copy(&built, &program).map(drop))
        .expect("runtime-mux");
    executable(&mux_dir.join("thurbox-cli"), "#!/bin/sh\n");
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        cfg.replace(&*built.to_string_lossy(), &program.to_string_lossy()),
    )
    .expect("config");
    let harnesses = env.config.parent().unwrap().join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses dir");
    fs::write(harnesses.join("stub.toml"), STUB_PROFILE).expect("profile");
    let bin = env.root.join("bin");
    let task_bin = env.root.join("task-bin");
    fs::create_dir_all(&bin).expect("bin");
    fs::create_dir_all(&task_bin).expect("task bin");
    executable(&bin.join("stub-agent"), PATH_AGENT);
    executable(&bin.join("fake-tool"), "#!/bin/sh\necho runtime\n");
    executable(&task_bin.join("fake-tool"), "#!/bin/sh\necho task\n");
    executable(&task_bin.join("stub-agent"), PATH_AGENT);
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let inherited = format!(
        "{}:{}:{}",
        bin.display(),
        fleet_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    env.only_on_path(Path::new(&inherited));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("stub");
    task["spec"].as_object_mut().unwrap().remove("brief");
    task["spec"].as_object_mut().unwrap().remove("outputs");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    task["spec"]["env"] = json!({"PATH": format!("{}:{inherited}", task_bin.display())});
    env.apply(json!([task])).ok();

    reconcile_until(&env, "the agent ran its tool", |_| {
        fs::read_to_string(work.join("tool")).is_ok_and(|t| !t.is_empty())
    });
    assert_eq!(
        fs::read_to_string(work.join("tool")).unwrap().trim(),
        "task",
        "the pane ran the Task's tool"
    );
    assert_eq!(
        Path::new(&fs::read_to_string(work.join("agent")).unwrap_or_default()),
        task_bin.join("stub-agent"),
        "the pane ran the Task's agent"
    );
    let path = fs::read_to_string(work.join("path")).unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    assert!(
        dirs.contains(&mux_dir),
        "runtime-mux's CLI directory is on the pane's PATH: {path}"
    );
    assert!(
        dirs.iter().position(|d| *d == task_bin) < dirs.iter().position(|d| *d == bin),
        "the Task's PATH keeps its order: {path}"
    );
}

const SCREEN_AGENT: &str = r#"#!/bin/sh
echo start >> "$PWD/starts"
if [ "$(wc -l < "$PWD/starts")" -gt 1 ]; then exec sleep 300; fi
printf '\033]2;[ . ] Action Required | stub\007'
printf '  Would you like to run the following command?\n\n  $ touch x\n\n'
printf '\342\200\272 1. Yes, proceed (y)\n\n  Press enter to confirm or esc to cancel\n'
while [ ! -e "$PWD/go" ]; do sleep 0.2; done
fleet signal idle
exec sleep 300
"#;

const SCREEN_PROFILE: &str = r#"name = "stub"
program = "stub-agent"
authority = "full"

[screen]
rules = "codex"
"#;

#[test]
fn an_agent_state_comes_from_the_screen_below_the_hooks_on_runtime_mux() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("screen");
    let mut env = mux_env("runtime-mux-screen", &program, &sockets);
    let harnesses = env.config.parent().unwrap().join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses dir");
    fs::write(harnesses.join("stub.toml"), SCREEN_PROFILE).expect("profile");
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let agent = bin.join("stub-agent");
    fs::write(&agent, SCREEN_AGENT).expect("agent");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("stub");
    task["spec"].as_object_mut().unwrap().remove("brief");
    task["spec"].as_object_mut().unwrap().remove("outputs");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    let shows = |env: &Env, state: &str, source: &str| {
        let status = env.get("t1")["status"].clone();
        status["agentState"] == state && status["activity"]["source"] == source
    };
    reconcile_until(&env, "blocked, from the screen", |env| {
        shows(env, "attention", "screen")
    });
    assert_eq!(
        env.get("t1")["status"]["activity"]["rule"],
        "codex/action-required"
    );
    let shown = env.fleet(&["get", "task", "t1"]).ok().json();
    assert_eq!(
        shown["status"]["activity"]["source"], "screen",
        "fleet get task prints it"
    );

    fs::write(work.join("go"), "").expect("go");
    reconcile_until(&env, "idle, from the hooks", |env| {
        shows(env, "idle", "hook")
    });
    assert!(env.get("t1")["status"]["activity"]["rule"].is_null());

    let panes = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&panes.stdout).into_owned();
    let pane = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    sockets.tmux(&["send-keys", "-t", &pane, "C-c"]);
    reconcile_until(&env, "unknown, from neither", |env| {
        shows(env, "unknown", "none")
            && fs::read_to_string(work.join("starts")).is_ok_and(|s| s.lines().count() >= 2)
    });
}

fn path_without_flock(dir: &Path) -> String {
    fs::create_dir_all(dir).expect("PATH dir");
    let path = std::env::var_os("PATH").unwrap_or_default();
    for tool in [
        "sh", "tmux", "jq", "id", "perl", "cat", "awk", "grep", "sleep", "tail", "kill", "rm",
        "env", "ls", "mkdir",
    ] {
        let found = std::env::split_paths(&path)
            .map(|d| d.join(tool))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("no {tool} on PATH"));
        let _ = std::os::unix::fs::symlink(found, dir.join(tool));
    }
    dir.to_string_lossy().into_owned()
}

fn reference_script_runs() -> bool {
    if tmux().is_none() {
        return false;
    }
    for tool in ["jq", "perl"] {
        if Command::new(tool).arg("--version").output().is_err() {
            missing(&format!("{tool} is not installed"));
            return false;
        }
    }
    true
}

#[test]
fn the_reference_script_passes_the_conformance_suite_with_its_perl_lock() {
    if !reference_script_runs() {
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new("perl");
    let mut env = Env::new("runtime-check-script-perl");
    env.set_var("TMUX_TMPDIR", &sockets.0.to_string_lossy());
    let path = path_without_flock(&env.root.join("no-flock"));
    env.only_on_path(Path::new(&path));
    let run = env.fleet(&["runtime", "check", "--", script.to_str().unwrap()]);
    run.ok()
        .stdout_has("ok   concurrent starts with one key leave one process")
        .stdout_has("ok   two starts with one key leave one process");
    assert!(!run.stdout.contains("FAIL"), "{}", run.stdout);
}

fn a_killed_holder_releases_the_lock(name: &str, path: &str) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new(name);
    let hang = sockets.0.join("hang");
    fs::create_dir_all(&hang).expect("hang dir");
    fs::write(
        hang.join("tmux"),
        "#!/bin/sh\n: > \"$TMUX_TMPDIR/hung\"\nexec sleep 30\n",
    )
    .expect("fake tmux");
    fs::set_permissions(hang.join("tmux"), fs::Permissions::from_mode(0o755)).expect("chmod");
    let request = json!({"protocol": 1, "key": "k.one", "argv": ["sleep", "300"], "env": {}});
    let start = |path: &str| {
        let mut child = Command::new(&script)
            .process_group(0)
            .arg("start")
            .env("TMUX_TMPDIR", &sockets.0)
            .env("PATH", path)
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("tmux.sh");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(request.to_string().as_bytes())
            .expect("request");
        child
    };
    let mut holder = start(&format!("{}:{path}", hang.display()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !sockets.0.join("hung").exists() {
        assert!(Instant::now() < deadline, "the first start never hung");
        std::thread::sleep(Duration::from_millis(20));
    }
    let waiters: Vec<_> = (0..4).map(|_| start(path)).collect();
    std::thread::sleep(Duration::from_millis(300));
    // SIGKILL runs no trap.
    Command::new("kill")
        .args(["-KILL", &format!("-{}", holder.id())])
        .status()
        .expect("SIGKILL");
    holder.wait().expect("reaped");

    let begun = Instant::now();
    let answers: Vec<Value> = waiters
        .into_iter()
        .map(|w| {
            serde_json::from_slice(&w.wait_with_output().expect("start").stdout).expect("JSON")
        })
        .collect();
    assert!(
        begun.elapsed() < Duration::from_secs(5),
        "waited {:?} for a dead holder",
        begun.elapsed()
    );
    let started: Vec<&Value> = answers.iter().map(|a| &a["known"]["started"]).collect();
    assert!(
        started.iter().all(|s| s["id"] == started[0]["id"]),
        "{answers:?}"
    );
    assert_eq!(
        started.iter().filter(|s| s["adopted"] == false).count(),
        1,
        "{answers:?}"
    );
}

#[test]
fn a_reference_script_killed_holding_its_flock_does_not_block_the_next_starts() {
    if !reference_script_runs() {
        return;
    }
    if Command::new("flock").arg("--version").output().is_err() {
        missing("flock is not installed");
        return;
    }
    a_killed_holder_releases_the_lock("killed-flock", &std::env::var("PATH").unwrap_or_default());
}

#[test]
fn a_reference_script_killed_holding_its_perl_lock_does_not_block_the_next_starts() {
    if !reference_script_runs() {
        return;
    }
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("no-flock-{}", std::process::id()));
    a_killed_holder_releases_the_lock("killed-perl", &path_without_flock(&dir));
}

const CODEX: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$PWD/argv.log"
printf '>_ OpenAI Codex (v0.164.0)\n\nTo get started, describe a task or try one of these commands:\n\n'
printf '\342\200\272 Ask Codex to do anything\r\033[2C'
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  printf '%s\n' "$line" >> "$PWD/typed.log"
  fleet signal idle "{\"type\":\"agent-turn-complete\",\"thread-id\":\"t9\",\"input-messages\":[\"$line\"]}"
done
"#;

#[test]
fn a_codex_task_on_runtime_mux_is_briefed_before_its_first_turn_ends() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("codex");
    let mut env = mux_env("runtime-mux-codex", &program, &sockets);
    stand_in(&mut env, "codex", "codex", CODEX);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("codex");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on its notify's proof", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let typed = fs::read_to_string(work.join("typed.log")).unwrap_or_default();
    assert_eq!(typed, "Do the work for t1.\n");
    assert_eq!(env.get("t1")["status"]["conversation"], "t9");
}

fn stand_in(env: &mut Env, program: &str, argv0: &str, script: &str) {
    let bin = env.root.join("bin");
    let shell = env.root.join("shell");
    fs::create_dir_all(&bin).expect("bin");
    fs::create_dir_all(&shell).expect("shell dir");
    std::os::unix::fs::symlink("/bin/sh", shell.join(argv0)).expect("shell");
    let body = env.root.join(format!("{program}.sh"));
    fs::write(&body, script).expect("harness");
    let launcher = bin.join(program);
    fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nexec '{}' '{}' \"$@\"\n",
            shell.join(argv0).display(),
            body.display()
        ),
    )
    .expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
}

const KIMI: &str = r#"#!/bin/sh
printf ' No session yet \342\200\224 one will be created on your first message.\n\n'
printf ' \342\225\255\342\224\200\342\224\200\342\225\256\n'
printf ' \342\224\202 >    \342\224\202\n'
printf ' \342\225\260\342\224\200\342\224\200\342\225\257\n'
printf '              context: 0%% (0/256k)'
printf '\033[2A\r\033[5C'
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  printf '%s\n' "$line" >> "$PWD/typed.log"
  fleet signal prompt --harness kimi "{\"session_id\":\"session_k1\",\"prompt\":[{\"type\":\"text\",\"text\":\"$line\"}]}"
done
"#;

#[test]
fn a_kimi_task_on_runtime_mux_is_briefed_before_its_first_report() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("kimi-stand-in");
    let mut env = mux_env("runtime-mux-kimi-stand-in", &program, &sockets);
    stand_in(&mut env, "kimi", "kimi-code", KIMI);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("kimi");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on its prompt hook's proof", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let typed = fs::read_to_string(work.join("typed.log")).unwrap_or_default();
    assert_eq!(typed, "Do the work for t1.\n");
    assert_eq!(env.get("t1")["status"]["conversation"], "session_k1");
}

const SILENT_CLAUDE: &str = r#"#!/bin/sh
rule=$(printf '\342\224\200%.0s' $(seq 1 80))
printf ' Claude Code v2.1.322\n\n'
printf '%s\n' "$rule"
printf '\342\235\257 Try "create a util logging.py that..."\n'
printf '%s\n' "$rule"
printf '  ? for shortcuts'
printf '\033[2A\r\033[2C'
while IFS= read -r line; do :; done
"#;

#[test]
fn a_claude_composer_on_screen_reads_as_a_trusted_workspace() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("claude-composer");
    let mut env = mux_env("runtime-mux-claude-composer", &program, &sockets);
    stand_in(&mut env, "claude", "claude", SILENT_CLAUDE);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "trusted on its composer", |env| {
        env.get("t1")["status"]["conditions"]
            .as_array()
            .is_some_and(|cs| {
                cs.iter()
                    .any(|c| c["type"] == "WorkspaceTrusted" && c["status"] == "True")
            })
    });
    let status = env.get("t1")["status"].clone();
    support::assert_condition(&status, "WorkspaceTrusted", "True", "Trusted");

    support::assert_condition(&status, "Briefed", "Unknown", "ReadyUnknown");
    env.backdate_condition("t1", "Briefed", 61_000);
    env.reconcile();
    let status = env.get("t1")["status"].clone();
    let message = support::condition(&status, "Briefed")["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("no hook report yet for harness claude-code")
            && message.contains("check `fleet doctor` for its integration"),
        "a silent Claude waits on readiness, and the wait names its cause: {message}"
    );
}

const COPILOT: &str = r#"#!/bin/sh
printf ' ~/work\n'
printf '\342\224\200\342\224\200\342\224\200\342\224\200\n'
printf '\342\235\257\n'
printf '\342\224\200\342\224\200\342\224\200\342\224\200\n'
printf ' Interactive \302\267 / commands'
printf '\033[2A\r\033[2C'
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  printf '%s\n' "$line" >> "$PWD/typed.log"
  printf '{"sessionId":"s9","prompt":"%s"}' "$line" | fleet signal prompt
done
"#;

#[test]
fn a_copilot_task_on_runtime_mux_is_briefed_before_its_first_report() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("copilot-stand-in");
    let mut env = mux_env("runtime-mux-copilot-stand-in", &program, &sockets);
    stand_in(&mut env, "copilot", "copilot", COPILOT);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("copilot");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on its prompt hook's proof", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let typed = fs::read_to_string(work.join("typed.log")).unwrap_or_default();
    assert_eq!(typed, "Do the work for t1.\n");
    assert_eq!(env.get("t1")["status"]["conversation"], "s9");
}

const GIT_LOCATION: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
];

fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    for var in GIT_LOCATION {
        cmd.env_remove(var);
    }
    let out = cmd
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.test"])
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn preserved_refs(repository: &Path) -> Vec<String> {
    git(
        repository,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/fleet/preserved",
        ],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

fn git_url_task_on_runtime_mux(
    name: &str,
    program: &Path,
    sockets: &Sockets,
    claude_script: &str,
) -> (Env, PathBuf) {
    let mut env = mux_env(name, program, sockets);
    env.set_var("RUNTIME_MUX_CONFIG", &mux_config(&env).to_string_lossy());
    let src = env.root.join("src");
    fs::create_dir_all(&src).expect("source");
    git(&src, &["init", "-q", "-b", "main"]);
    git(&src, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, claude_script).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("git+file://{}#main", src.display())]);
    env.apply(json!([task])).ok();
    (env, src)
}

#[test]
fn a_git_url_task_on_runtime_mux_runs_in_a_worktree_and_its_release_keeps_the_work() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("repo");
    let (env, src) = git_url_task_on_runtime_mux("runtime-mux-repo", &program, &sockets, CLAUDE);

    reconcile_until(&env, "briefed in its worktree", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let status = env.get("t1")["status"].clone();
    let dir = PathBuf::from(
        status["workspace"]["dirs"][0]
            .as_str()
            .unwrap_or_else(|| panic!("no workspace dirs: {status:#}")),
    );
    let root = env.root.join("ws");
    assert!(
        dir.starts_with(&root),
        "{} under {}",
        dir.display(),
        root.display()
    );
    assert!(dir.join(".git").is_file(), "a worktree: {status:#}");
    assert_ne!(dir, src, "never the checkout itself");
    assert_eq!(starts(&dir).len(), 1, "the agent started in the worktree");

    fs::write(dir.join("notes.txt"), "kept\n").expect("work");
    env.fleet(&["delete", "task", "t1"]).ok();
    let deadline = Instant::now() + Duration::from_secs(60);
    while env.fleet(&["get", "task", "t1"]).code == Some(0) {
        assert!(Instant::now() < deadline, "{:#}", env.get("t1"));
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
    assert!(!dir.exists(), "the release removed the worktree");
    let refs = preserved_refs(&src);
    assert_eq!(refs.len(), 1, "one preserved ref: {refs:?}");
    assert_eq!(
        git(&src, &["show", &format!("{}:notes.txt", refs[0])]),
        "kept\n"
    );
}

const CLAUDE_GONE_QUIET: &str = r#"#!/bin/sh
fleet signal idle
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  fleet signal prompt "{\"prompt\":\"$line\",\"session_id\":\"conv-1\"}"
  fleet signal working
done
"#;

fn backdate_deletion(env: &Env, name: &str, by: Duration) {
    let path = env.object_path("Task", name);
    let mut task: Value = serde_json::from_slice(&fs::read(&path).expect("record")).expect("json");
    let deleted = task["metadata"]["deletionTimestamp"]
        .as_u64()
        .expect("a deleting Task");
    task["metadata"]["deletionTimestamp"] = json!(deleted - by.as_millis() as u64);
    fs::write(&path, task.to_string()).expect("record");
}

#[test]
fn a_deleted_task_whose_agent_last_said_working_is_released_after_the_grace() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("quiet");
    let (env, src) =
        git_url_task_on_runtime_mux("runtime-mux-quiet", &program, &sockets, CLAUDE_GONE_QUIET);
    reconcile_until(&env, "briefed, its agent then working", |env| {
        let st = &env.get("t1")["status"];
        st["phase"] == "Briefed" && st["agentState"] == "working"
    });
    let dir = PathBuf::from(
        env.get("t1")["status"]["workspace"]["dirs"][0]
            .as_str()
            .expect("a workspace")
            .to_string(),
    );
    fs::write(dir.join("notes.txt"), "kept\n").expect("work");

    env.fleet(&["delete", "task", "t1"]).ok();
    reconcile_until(&env, "held while the agent may be working", |env| {
        env.get("t1")["status"]["conditions"]
            .as_array()
            .is_some_and(|cs| {
                cs.iter()
                    .any(|c| c["type"] == "Released" && c["reason"] == "SessionBusy")
            })
    });
    env.reconcile();
    assert_eq!(
        env.fleet(&["get", "task", "t1"]).code,
        Some(0),
        "a session that just reported working is not released at once"
    );

    backdate_deletion(&env, "t1", Duration::from_secs(24 * 3600));
    let deadline = Instant::now() + Duration::from_secs(60);
    while env.fleet(&["get", "task", "t1"]).code == Some(0) {
        assert!(Instant::now() < deadline, "{:#}", env.get("t1")["status"]);
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
    assert!(!dir.exists(), "the release removed the worktree");
    let refs = preserved_refs(&src);
    assert_eq!(refs.len(), 1, "one preserved ref: {refs:?}");
    assert_eq!(
        git(&src, &["show", &format!("{}:notes.txt", refs[0])]),
        "kept\n"
    );
}

#[test]
fn a_release_removes_a_worktree_with_a_read_only_directory_and_keeps_the_work() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    if uid() == "0" {
        eprintln!("skipping: root removes a read-only directory's files");
        return;
    }
    let sockets = Sockets::new("read-only");
    let (env, src) =
        git_url_task_on_runtime_mux("runtime-mux-read-only", &program, &sockets, CLAUDE);
    reconcile_until(&env, "briefed in its worktree", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let dir = PathBuf::from(
        env.get("t1")["status"]["workspace"]["dirs"][0]
            .as_str()
            .expect("a workspace")
            .to_string(),
    );
    fs::write(dir.join("notes.txt"), "kept\n").expect("work");
    let build = dir.join("target").join("debug").join("build");
    fs::create_dir_all(&build).expect("build output");
    fs::write(build.join("thurbox"), "binary").expect("build output");
    let _writable_again = Chmod::hold(&build, 0o555, 0o755);

    env.fleet(&["delete", "task", "t1"]).ok();
    let deadline = Instant::now() + Duration::from_secs(60);
    while env.fleet(&["get", "task", "t1"]).code == Some(0) {
        assert!(Instant::now() < deadline, "{:#}", env.get("t1")["status"]);
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
    assert!(!dir.exists(), "the release removed the worktree");
    let refs = preserved_refs(&src);
    assert_eq!(refs.len(), 1, "one preserved ref: {refs:?}");
    assert_eq!(
        git(&src, &["show", &format!("{}:notes.txt", refs[0])]),
        "kept\n"
    );
}

struct Chmod {
    path: PathBuf,
    restore: u32,
}

impl Chmod {
    fn hold(path: &Path, mode: u32, restore: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
        Self {
            path: path.to_path_buf(),
            restore,
        }
    }
}

impl Drop for Chmod {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.restore));
    }
}

fn a_release_finishes_a_half_removed_worktree(name: &str, git_file_spared: bool) {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    if uid() == "0" {
        eprintln!("skipping: root removes a read-only directory's files");
        return;
    }
    let sockets = Sockets::new(name);
    let (env, src) =
        git_url_task_on_runtime_mux(&format!("runtime-mux-{name}"), &program, &sockets, CLAUDE);
    reconcile_until(&env, "briefed in its worktree", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let dir = PathBuf::from(
        env.get("t1")["status"]["workspace"]["dirs"][0]
            .as_str()
            .expect("a workspace")
            .to_string(),
    );
    let git_file = dir.join(".git");
    let gitfile = fs::read_to_string(&git_file).expect("a linked worktree's gitfile");
    let gitdir = PathBuf::from(gitfile.trim_end().strip_prefix("gitdir: ").expect("gitdir"));
    fs::write(dir.join("notes.txt"), "kept\n").expect("work");
    let build = dir.join("target").join("debug");
    fs::create_dir_all(&build).expect("build output");
    fs::write(build.join("thurbox"), "binary").expect("build output");
    let unreadable = Chmod::hold(&build, 0o000, 0o755);

    env.fleet(&["delete", "task", "t1"]).ok();
    reconcile_until(&env, "the first release fails part way", |env| {
        env.get("t1")["status"]["releaseAttempts"].as_u64() > Some(0)
    });
    assert!(!gitdir.exists(), "git removes the git directory anyway");
    if git_file_spared {
        fs::write(&git_file, &gitfile).expect("the gitfile the order spared");
    } else if git_file.exists() {
        fs::remove_file(&git_file).expect("the gitfile the order removed");
    }
    drop(unreadable);

    let deadline = Instant::now() + Duration::from_secs(60);
    while env.fleet(&["get", "task", "t1"]).code == Some(0) {
        assert!(Instant::now() < deadline, "{:#}", env.get("t1")["status"]);
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
    assert!(!dir.exists(), "the release removed what was left");
    let refs = preserved_refs(&src);
    assert_eq!(refs.len(), 1, "one preserved ref: {refs:?}");
    assert_eq!(
        git(&src, &["show", &format!("{}:notes.txt", refs[0])]),
        "kept\n"
    );
}

#[test]
fn a_release_finishes_a_worktree_an_earlier_attempt_removed_the_gitfile_of() {
    a_release_finishes_a_half_removed_worktree("half-removed", false);
}

#[test]
fn a_release_finishes_a_worktree_an_earlier_attempt_left_a_dangling_gitfile_in() {
    a_release_finishes_a_half_removed_worktree("dangling-gitfile", true);
}

#[test]
fn a_release_keeps_a_worktree_whose_gitfile_it_cannot_read() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    if uid() == "0" {
        eprintln!("skipping: root reads an unreadable file");
        return;
    }
    let sockets = Sockets::new("unreadable-gitfile");
    let (env, src) =
        git_url_task_on_runtime_mux("runtime-mux-unreadable-gitfile", &program, &sockets, CLAUDE);
    reconcile_until(&env, "briefed in its worktree", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let dir = PathBuf::from(
        env.get("t1")["status"]["workspace"]["dirs"][0]
            .as_str()
            .expect("a workspace")
            .to_string(),
    );
    fs::write(dir.join("notes.txt"), "kept\n").expect("work");
    let git_file = dir.join(".git");
    let readable_again = Chmod::hold(&git_file, 0o000, 0o644);

    env.fleet(&["delete", "task", "t1"]).ok();
    reconcile_until(&env, "a release attempt answered", |env| {
        env.fleet(&["get", "task", "t1"]).code != Some(0)
            || env.get("t1")["status"]["releaseAttempts"].as_u64() > Some(0)
    });
    assert_eq!(
        fs::read_to_string(dir.join("notes.txt")).ok().as_deref(),
        Some("kept\n"),
        "the work outlives a gitfile release could not read"
    );
    drop(readable_again);

    let deadline = Instant::now() + Duration::from_secs(60);
    while env.fleet(&["get", "task", "t1"]).code == Some(0) {
        assert!(Instant::now() < deadline, "{:#}", env.get("t1")["status"]);
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
    let refs = preserved_refs(&src);
    assert_eq!(refs.len(), 1, "one preserved ref: {refs:?}");
    assert_eq!(
        git(&src, &["show", &format!("{}:notes.txt", refs[0])]),
        "kept\n"
    );
}

fn uid() -> String {
    let out = Command::new("id").arg("-u").output().expect("id -u");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn script_call(tmpdir: &Path, verb: &str, request: &Value) -> Value {
    use std::io::Write;
    use std::process::Stdio;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let mut child = Command::new(script)
        .arg(verb)
        .env("TMUX_TMPDIR", tmpdir)
        .env_remove("TMUX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("tmux.sh");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .expect("request");
    serde_json::from_slice(&child.wait_with_output().expect("answer").stdout).expect("JSON")
}

#[test]
fn the_reference_script_truncates_nothing_through_a_planted_link() {
    if !reference_script_runs() {
        return;
    }
    let sockets = Sockets::new("planted");
    let sentinel = sockets.0.join("sentinel");
    fs::write(&sentinel, "keep\n").expect("sentinel");
    std::os::unix::fs::symlink(
        &sentinel,
        sockets
            .0
            .join(format!("runtime-tmux-sh-{}-runtime-tmux-sh.lock", uid())),
    )
    .expect("link");
    let request = json!({"protocol": 1, "key": "k.one", "argv": ["sleep", "300"], "env": {}});
    let answer = script_call(&sockets.0, "start", &request);
    assert!(answer["known"]["started"].is_object(), "{answer}");
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep\n");
}

#[test]
fn the_reference_script_refuses_a_socket_directory_others_can_write() {
    if !reference_script_runs() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let sockets = Sockets::new("foreign");
    let dir = sockets.0.join(format!("tmux-{}", uid()));
    fs::create_dir_all(&dir).expect("socket dir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).expect("chmod");
    let request = json!({"protocol": 1, "key": "k.one", "argv": ["sleep", "300"], "env": {}});
    let answer = script_call(&sockets.0, "start", &request);
    let why = answer["unknown"]["why"].as_str().unwrap_or_default();
    assert!(why.contains("writable by others"), "{answer}");
}

fn waiting_task(env: &mut Env) -> PathBuf {
    task_running(env, "#!/bin/sh\nexec sleep 300\n")
}

fn task_running(env: &mut Env, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let harness = bin.join("claude");
    fs::write(&harness, script).expect("harness");
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", bin.display())));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    env.apply(json!([
        support::task("t1", true),
        support::task("t2", false)
    ]))
    .ok();
    env.reconcile().ok();
    work
}

#[test]
fn fleet_attach_wraps_a_tty_answer_in_its_bridge() {
    if tmux().is_none() {
        return;
    }
    if Command::new("jq").arg("--version").output().is_err() {
        missing("jq is not installed");
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new("attach-tty");
    let mut env = mux_env("attach-tty", &script, &sockets);
    waiting_task(&mut env);

    env.fleet(&["attach", "t2", "--json"])
        .code(3)
        .stderr_has("no session");

    let answer = env.fleet(&["attach", "t1", "--json"]).ok().json();
    let argv: Vec<&str> = answer["stream"]["argv"]
        .as_array()
        .unwrap_or_else(|| panic!("a stream: {answer:#}"))
        .iter()
        .map(|a| a.as_str().expect("a string"))
        .collect();
    assert_eq!(argv[0], env!("CARGO_BIN_EXE_fleet"), "{argv:?}");
    assert_eq!(&argv[1..4], ["term-bridge", "--", "tmux"], "{argv:?}");
    assert!(argv.contains(&"attach-session"), "{argv:?}");
}

#[test]
fn fleet_attach_streams_a_runtime_mux_session_and_a_detach_leaves_it_running() {
    use support::view::View;
    use term_stream::{Capability, Down, Size, Up};
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("attach-mux");
    let mut env = mux_env("attach-mux", &program, &sockets);
    task_running(
        &mut env,
        "#!/bin/sh\necho ready-marker\nwhile IFS= read -r l; do echo \"pong-$l\"; done\n",
    );

    let answer = env.fleet(&["attach", "t1", "--json"]).ok().json();
    let argv: Vec<String> = serde_json::from_value(answer["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer:#}"));
    assert_eq!(Path::new(&argv[0]), program, "{argv:?}");
    assert_eq!(argv[1], "view", "{argv:?}");

    let mut view = View::argv(&argv);
    let Down::Hello(hello) = view.next() else {
        panic!("no hello first")
    };
    assert!(hello.has(Capability::Snapshot), "{hello:?}");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until("a snapshot", |f| matches!(f, Down::Snapshot(_)));
    view.until_text("ready-marker");
    view.send(&Up::Input(b"ping\r".to_vec()));
    view.until_text("pong-ping");
    view.send(&Up::Detach);
    assert!(view.close().success());

    env.reconcile().ok();
    assert_eq!(env.status("t1")["sessionState"], "Running");
}

#[test]
fn fleet_shell_opens_a_shell_in_the_tasks_directory_on_runtime_mux() {
    use std::os::unix::fs::PermissionsExt;
    use support::view::View;
    use term_stream::{Down, Size, Up};
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("shell-mux");
    let mut env = mux_env("shell-mux", &program, &sockets);
    env.set_var("SHELL", "/bin/sh");
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let harness = bin.join("claude");
    fs::write(&harness, "#!/bin/sh\nexec sleep 300\n").expect("harness");
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", bin.display())));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let work = work.canonicalize().expect("canonical workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task, support::task("t2", false)])).ok();
    env.reconcile().ok();

    env.fleet(&["shell", "t2", "--json"])
        .code(3)
        .stderr_has("no session");

    let answer = env.fleet(&["shell", "t1", "--json"]).ok().json();
    let argv: Vec<String> = serde_json::from_value(answer["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer:#}"));
    assert_eq!(Path::new(&argv[0]), program, "{argv:?}");
    assert_eq!(argv[1], "view", "{argv:?}");

    let mut view = View::argv(&argv);
    assert!(matches!(view.next(), Down::Hello(_)), "no hello first");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until("a snapshot", |f| matches!(f, Down::Snapshot(_)));
    view.send(&Up::Input(b"pwd > shell-pwd\r".to_vec()));
    let out = work.join("shell-pwd");
    let deadline = Instant::now() + Duration::from_secs(10);
    while fs::read_to_string(&out).unwrap_or_default().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    view.send(&Up::Detach);
    assert!(view.close().success());
    assert_eq!(
        fs::read_to_string(&out).unwrap_or_default().trim(),
        work.display().to_string(),
        "the shell opens in the Task's directory"
    );
}

#[test]
fn fleet_shell_on_a_runtime_without_the_verb_exits_3() {
    if tmux().is_none() {
        return;
    }
    if Command::new("jq").arg("--version").output().is_err() {
        missing("jq is not installed");
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new("shell-none");
    let mut env = mux_env("shell-none", &script, &sockets);
    waiting_task(&mut env);
    env.fleet(&["shell", "t1", "--json"])
        .code(3)
        .stderr_has("offers no shell");
}

#[test]
fn term_bridge_relays_a_raw_command_as_a_stream() {
    use std::process::Stdio;
    use support::view::View;
    use term_stream::{Capability, Down, Exit, Size, Up};
    let env = Env::new("term-bridge");
    let mut cmd = env.command(&[
        "term-bridge",
        "--",
        "sh",
        "-c",
        "echo bridged; read l; echo got-$l; exit 3",
    ]);
    cmd.stderr(Stdio::inherit());
    let mut view = View::open(cmd);
    let Down::Hello(hello) = view.next() else {
        panic!("no hello first")
    };
    assert!(!hello.has(Capability::Snapshot), "{hello:?}");
    view.send(&Up::Resize(Size {
        rows: 30,
        cols: 100,
    }));
    view.until("the size", |f| {
        *f == Down::Resized(Size {
            rows: 30,
            cols: 100,
        })
    });
    view.until_text("bridged");
    view.send(&Up::Input(b"x\r".to_vec()));
    view.until_text("got-x");
    let exit = view.until("the exit", |f| matches!(f, Down::Exit(_)));
    assert_eq!(exit, Down::Exit(Exit::Known { code: Some(3) }));
}

fn bridge(env: &Env, script: &str, ready: &str) -> support::view::View {
    use term_stream::{Down, Size, Up};
    let mut cmd = env.command(&["term-bridge", "--", "sh", "-c", script]);
    cmd.stderr(std::process::Stdio::inherit());
    let mut view = support::view::View::open(cmd);
    assert!(matches!(view.next(), Down::Hello(_)), "no hello first");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until_text(ready);
    view
}

#[test]
fn term_bridge_brackets_a_paste_the_program_asked_for() {
    use term_stream::Up;
    let env = Env::new("term-bridge-paste");
    let mut view = bridge(
        &env,
        r#"stty -icanon -echo; printf '\033[?2004h'; echo ready; head -c 20 | od -An -c"#,
        "ready",
    );
    // Bracketed paste adds six bytes on each side.
    view.send(&Up::Paste("abcdefgh".into()));
    view.until_text("2   0   0   ~   a   b");
}

#[test]
fn term_bridge_lets_go_of_a_command_that_closed_its_terminal() {
    use term_stream::Up;
    let env = Env::new("term-bridge-detach");
    let mut view = bridge(
        &env,
        "echo started; exec </dev/null >/dev/null 2>&1; exec sleep 300",
        "started",
    );
    std::thread::sleep(Duration::from_millis(300));
    view.send(&Up::Detach);
    view.close();
}

#[test]
fn a_tty_attach_runs_through_the_bridge_end_to_end() {
    use support::view::View;
    use term_stream::{Down, Size, Up};
    if tmux().is_none() {
        return;
    }
    if Command::new("jq").arg("--version").output().is_err() {
        missing("jq is not installed");
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/runtimes/tmux.sh");
    let sockets = Sockets::new("attach-bridge");
    let mut env = mux_env("attach-bridge", &script, &sockets);
    task_running(
        &mut env,
        "#!/bin/sh\necho ready-marker\nwhile IFS= read -r l; do echo \"pong-$l\"; done\n",
    );
    let answer = env.fleet(&["attach", "t1", "--json"]).ok().json();
    let argv: Vec<String> = serde_json::from_value(answer["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer:#}"));
    assert_eq!(argv[1], "term-bridge", "{argv:?}");

    // The tmux client needs a terminal type.
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env("TERM", "xterm-256color")
        .env_remove("TMUX");
    let mut view = View::open(cmd);
    assert!(matches!(view.next(), Down::Hello(_)), "no hello first");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until_text("ready-marker");
    view.send(&Up::Input(b"ping\r".to_vec()));
    view.until_text("pong-ping");
    view.send(&Up::Resize(Size {
        rows: 30,
        cols: 100,
    }));
    view.until("the new size", |f| {
        *f == Down::Resized(Size {
            rows: 30,
            cols: 100,
        })
    });
    view.send(&Up::Detach);
    view.close();

    env.reconcile().ok();
    assert_eq!(env.status("t1")["sessionState"], "Running");
}

#[test]
fn the_lease_outlasts_one_whole_runtime_operation() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("lease");
    let env = mux_env("runtime-mux-lease", &program, &sockets);
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        cfg.replace("callTimeoutMs = 10000", "callTimeoutMs = 2000"),
    )
    .expect("config");
    env.apply(support::task("t1", false)).ok();
    env.fleet(&["reconcile", "--lease-ttl-ms", "5000"])
        .code(1)
        .stderr_has("lease");
    assert!(env.get("t1")["status"]["phase"].is_null(), "nothing ran");
    env.reconcile().ok();
    assert_eq!(env.get("t1")["status"]["phase"], "Pending");
}

#[test]
fn a_message_typed_into_the_agent_is_recorded_as_one_the_route_may_hold() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("may-hold");
    let mut env = mux_env("runtime-mux-may-hold", &program, &sockets);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, CLAUDE).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "briefed", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });

    env.fleet(&["send", "t1", "are you there?"]).ok();
    reconcile_until(&env, "the message delivered", |env| {
        env.get("t1")["status"]["deliveries"]["receipts"]
            .as_array()
            .is_some_and(|r| !r.is_empty())
    });
    let status = env.get("t1")["status"].clone();
    let receipt = &status["deliveries"]["receipts"][0];
    assert_eq!(receipt["mayHold"], true, "{status:#}");
    assert_eq!(receipt["deliveredVia"], "input", "{status:#}");
    assert!(
        status["conditions"].as_array().is_some_and(|cs| !cs
            .iter()
            .any(|c| c["type"] == "NeedsAttention" && c["status"] == "True")),
        "{status:#}"
    );
}

const INBOX_CLAUDE: &str = r#"#!/bin/bash
export CLAUDE_CODE_MESSAGING_SOCKET="/tmp/fleet-inbox-$$.sock"
trap 'rm -f "$CLAUDE_CODE_MESSAGING_SOCKET"' EXIT
perl -MIO::Socket::UNIX -MJSON::PP -e '
my $s = IO::Socket::UNIX->new(Type => SOCK_STREAM(), Local => $ARGV[0], Listen => 5) or die;
while (my $c = $s->accept) {
  while (my $line = <$c>) {
    my $text = decode_json($line)->{message}{content};
    open(my $log, ">>", "inbox.log") or die; print $log "$text\n"; close $log;
    system("fleet", "signal", "prompt", encode_json({prompt => $text, session_id => "conv-1"}));
    system("fleet", "signal", "idle");
  }
}' "$CLAUDE_CODE_MESSAGING_SOCKET" &
while [ ! -S "$CLAUDE_CODE_MESSAGING_SOCKET" ]; do sleep 0.05; done
fleet signal idle
while IFS= read -r line; do
  text=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  printf '%s\n' "$text" >> "$PWD/typed.log"
  fleet signal prompt "{\"prompt\":\"$text\",\"session_id\":\"conv-1\"}"
  fleet signal idle
done
"#;

#[test]
fn a_message_reaches_claude_code_through_its_messaging_socket_not_its_composer() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("inbox");
    let mut env = mux_env("runtime-mux-inbox", &program, &sockets);
    stand_in(&mut env, "claude", "claude", INBOX_CLAUDE);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["brief"] = json!("Find the cause.");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "briefed", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });

    env.fleet(&["send", "t1", "- are you there?"]).ok();
    reconcile_until(&env, "the message delivered", |env| {
        env.get("t1")["status"]["deliveries"]["receipts"]
            .as_array()
            .is_some_and(|r| !r.is_empty())
    });
    let read = |name: &str| fs::read_to_string(work.join(name)).unwrap_or_default();
    assert_eq!(
        read("typed.log"),
        "Find the cause.\n",
        "only the brief is typed"
    );
    assert_eq!(
        read("inbox.log"),
        "- are you there?\n",
        "the message is in the inbox"
    );
    let status = env.get("t1")["status"].clone();
    let receipt = &status["deliveries"]["receipts"][0];
    assert_eq!(receipt["deliveredVia"], "inbox", "{status:#}");
    assert_ne!(receipt["mayHold"], true, "{status:#}");
}

const DEAF_CLAUDE: &str = r#"#!/bin/bash
export CLAUDE_CODE_MESSAGING_SOCKET="/tmp/fleet-inbox-gone-$$.sock"
fleet signal idle
while IFS= read -r line; do
  text=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  printf '%s\n' "$text" >> "$PWD/typed.log"
  fleet signal prompt "{\"prompt\":\"$text\",\"session_id\":\"conv-1\"}"
  fleet signal idle
done
"#;

#[test]
fn a_message_claude_codes_socket_refuses_is_typed_instead() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("deaf");
    let mut env = mux_env("runtime-mux-deaf", &program, &sockets);
    stand_in(&mut env, "claude", "claude", DEAF_CLAUDE);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["brief"] = json!("Find the cause.");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "briefed", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });

    env.fleet(&["send", "t1", "are you there?"]).ok();
    reconcile_until(&env, "the message delivered", |env| {
        env.get("t1")["status"]["deliveries"]["receipts"]
            .as_array()
            .is_some_and(|r| !r.is_empty())
    });
    let typed = fs::read_to_string(work.join("typed.log")).unwrap_or_default();
    assert_eq!(typed, "Find the cause.\nare you there?\n");
    let status = env.get("t1")["status"].clone();
    assert_eq!(
        status["deliveries"]["receipts"][0]["deliveredVia"], "input",
        "{status:#}"
    );
}

fn agent(name: &str) -> Option<PathBuf> {
    if std::env::var_os("FLEET_REAL_AGENTS").is_none_or(|v| v != "1") {
        eprintln!("skipping: drives the real {name}; set FLEET_REAL_AGENTS=1 to run it");
        return None;
    }
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        eprintln!("skipping: {name} is not installed");
    }
    found
}

fn snapshot(dir: &Path, skip: &[&str]) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(dir).unwrap().to_path_buf();
            if skip.iter().any(|s| rel.starts_with(s)) {
                continue;
            }
            match entry.file_type() {
                Ok(t) if t.is_dir() => todo.push(path),
                Ok(t) if t.is_file() => {
                    files.insert(rel, fs::read(&path).unwrap_or_default());
                }
                _ => {}
            }
        }
    }
    files
}

fn reported(env: &Env, task: &str) -> Vec<(String, Option<String>)> {
    let file = env
        .data
        .join("signals")
        .join(format!("{}.jsonl", env.session_key(task)));
    fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|s| {
            (
                s["state"].as_str().unwrap_or_default().to_string(),
                s["conversation"].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn in_order(states: &[(String, Option<String>)], want: &[&str]) -> bool {
    let mut want = want.iter().peekable();
    for (s, _) in states {
        if want.peek().is_some_and(|w| *w == s) {
            want.next();
        }
    }
    want.peek().is_none()
}

fn agent_env(name: &str, program: &Path, sockets: &Sockets) -> Env {
    let mut env = mux_env(name, program, sockets);
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", fleet_dir.display())));
    env
}

fn pane_in(sockets: &Sockets, work: &Path) -> String {
    let panes = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&panes.stdout).into_owned();
    listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string()
}

fn allow_each_request_until(
    env: &Env,
    sockets: &Sockets,
    work: &Path,
    what: &str,
    done: impl Fn(&Env) -> bool,
) {
    let pane = pane_in(sockets, work);
    reconcile_until(env, what, |env| {
        if reported(env, "t1")
            .last()
            .is_some_and(|(s, _)| s == "blocked")
        {
            sockets.tmux(&["send-keys", "-t", &pane, "Enter"]);
        }
        done(env)
    });
}

const ASKING_AGENT: &str = r#"#!/bin/sh
printf '%s' '{"session_id":"s1"}' | fleet signal idle
fleet signal working
for request in 1 2 3; do
  fleet signal blocked
  read -r answer
done
fleet signal working
fleet signal idle
exec sleep 300
"#;

#[test]
fn every_request_of_a_turn_is_allowed_before_it_ends() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("asking");
    let mut env = mux_env("runtime-mux-asking", &program, &sockets);
    let harnesses = env.config.parent().unwrap().join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses dir");
    fs::write(harnesses.join("stub.toml"), STUB_PROFILE).expect("profile");
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let agent = bin.join("stub-agent");
    fs::write(&agent, ASKING_AGENT).expect("agent");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("stub");
    task["spec"].as_object_mut().unwrap().remove("brief");
    task["spec"].as_object_mut().unwrap().remove("outputs");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "waiting on the first request", |env| {
        env.get("t1")["status"]["agentState"] == "attention"
    });
    allow_each_request_until(&env, &sockets, &work, "the turn ended", |env| {
        in_order(&reported(env, "t1"), &["blocked", "working", "idle"])
            && env.get("t1")["status"]["agentState"] == "idle"
    });
    let blocked = reported(&env, "t1")
        .iter()
        .filter(|(s, _)| s == "blocked")
        .count();
    assert_eq!(blocked, 3, "{:?}", reported(&env, "t1"));
}

fn end_agent(sockets: &Sockets, work: &Path) {
    let panes = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_pid} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&panes.stdout).into_owned();
    let pid = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    let _ = Command::new("pkill").args(["-TERM", "-P", &pid]).status();
    let _ = Command::new("kill").args(["-TERM", &pid]).status();
}

#[test]
fn pi_reports_its_state_through_the_extension_fleet_passes_at_launch() {
    let (Some(program), Some(_), Some(_)) = (runtime_mux(), tmux(), agent("pi")) else {
        return;
    };
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let config = std::env::var_os("PI_CODING_AGENT_DIR")
        .map_or_else(|| home.join(".pi/agent"), PathBuf::from);
    let before = snapshot(&config, &["sessions"]);
    let sockets = Sockets::new("pi");
    let env = agent_env("runtime-mux-pi", &program, &sockets);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let sessions = env.root.join("pi-sessions");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("pi");
    task["spec"]["brief"] = json!("Reply with the single word ok.");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    task["spec"]["env"] = json!({
        "HOME": home.to_string_lossy(),
        "PI_OFFLINE": "1",
        "PI_CODING_AGENT_SESSION_DIR": sessions.to_string_lossy(),
    });
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on pi's prompt hook", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    reconcile_until(&env, "the turn settled", |env| {
        in_order(&reported(env, "t1"), &["idle", "prompt", "working", "idle"])
            && env.get("t1")["status"]["agentState"] == "idle"
    });
    let conversation = env.get("t1")["status"]["conversation"]
        .as_str()
        .expect("a conversation")
        .to_string();
    let transcript = snapshot(&sessions, &[])
        .keys()
        .any(|f| f.to_string_lossy().contains(&conversation));
    assert!(
        transcript,
        "pi's session {conversation} is the conversation"
    );

    end_agent(&sockets, &work);
    reconcile_until(&env, "resumed on its session", |env| {
        let states = reported(env, "t1");
        states
            .iter()
            .rposition(|(s, _)| s == "started")
            .is_some_and(|at| {
                states[at..]
                    .iter()
                    .any(|(s, c)| s == "idle" && c.as_deref() == Some(&*conversation))
            })
    });
    end_agent(&sockets, &work);
    assert!(
        snapshot(&config, &["sessions"]) == before,
        "pi's config directory {} changed",
        config.display()
    );
}

#[test]
fn opencode_reports_its_state_through_the_plugin_fleet_names_at_launch() {
    let (Some(program), Some(_), Some(_)) = (runtime_mux(), tmux(), agent("opencode")) else {
        return;
    };
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map_or_else(|| home.join(".config"), PathBuf::from)
        .join("opencode");
    let before = snapshot(&config, &[]);
    let sockets = Sockets::new("opencode");
    let env = agent_env("runtime-mux-opencode", &program, &sockets);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    fs::write(
        work.join("opencode.jsonc"),
        r#"{"permissions":[{"action":"shell","resource":"*","effect":"ask"}]}"#,
    )
    .expect("project config");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("opencode");
    task["spec"]["env"] = json!({"HOME": home.to_string_lossy()});
    task["spec"]["brief"] =
        json!("Run the shell command `echo fleet` yourself, with your shell tool, then stop.");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on opencode's prompt hook", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    reconcile_until(&env, "waiting on the permission", |env| {
        env.get("t1")["status"]["agentState"] == "attention"
    });
    let conversation = env.get("t1")["status"]["conversation"]
        .as_str()
        .expect("a conversation")
        .to_string();
    assert!(conversation.starts_with("ses_"), "{conversation}");
    allow_each_request_until(&env, &sockets, &work, "the turn ended", |env| {
        in_order(
            &reported(env, "t1"),
            &["idle", "prompt", "working", "blocked", "working", "idle"],
        ) && env.get("t1")["status"]["agentState"] == "idle"
    });

    end_agent(&sockets, &work);
    reconcile_until(&env, "resumed on its session", |env| {
        let states = reported(env, "t1");
        states
            .iter()
            .rposition(|(s, _)| s == "started")
            .is_some_and(|at| {
                states[at..]
                    .iter()
                    .any(|(s, c)| s == "prompt" && c.as_deref() == Some(&*conversation))
            })
    });
    end_agent(&sockets, &work);
    assert!(
        snapshot(&config, &[]) == before,
        "opencode's config directory {} changed",
        config.display()
    );
}

#[test]
fn kilo_reports_its_state_through_the_plugins_fleet_names_at_launch() {
    let (Some(program), Some(_), Some(_)) = (runtime_mux(), tmux(), agent("kilo")) else {
        return;
    };
    let sockets = Sockets::new("kilo");
    let env = agent_env("runtime-mux-kilo", &program, &sockets);
    let config = env.root.join("user-config").join("kilo");
    fs::create_dir_all(&config).expect("user config");
    fs::write(
        config.join("kilo.json"),
        r#"{"model":"kilo/cohere/north-mini-code:free","permission":{"bash":"ask"}}"#,
    )
    .expect("user config");
    fs::write(config.join("tui.json"), r#"{"scroll_speed":3}"#).expect("user TUI config");
    // kilo's first start rewrites the user's config, so it runs once without Fleet first.
    let settled = Command::new("kilo")
        .args(["debug", "config"])
        .current_dir(&env.root)
        .env("HOME", &env.root)
        .env("XDG_CONFIG_HOME", env.root.join("user-config"))
        .env("KILO_NO_DAEMON", "1")
        .output()
        .expect("kilo debug config");
    assert!(settled.status.success(), "{settled:?}");
    let before = snapshot(&config, &[]);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("kilo");
    task["spec"]["env"] = json!({
        "XDG_CONFIG_HOME": env.root.join("user-config").to_string_lossy(),
    });
    task["spec"]["brief"] =
        json!("Run the shell command `echo fleet` yourself, with your shell tool, then stop.");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on kilo's prompt hook", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    reconcile_until(&env, "waiting on the permission", |env| {
        env.get("t1")["status"]["agentState"] == "attention"
    });
    let conversation = env.get("t1")["status"]["conversation"]
        .as_str()
        .expect("a conversation")
        .to_string();
    assert!(conversation.starts_with("ses_"), "{conversation}");
    allow_each_request_until(&env, &sockets, &work, "the turn ended", |env| {
        in_order(
            &reported(env, "t1"),
            &["idle", "prompt", "working", "blocked", "working", "idle"],
        ) && env.get("t1")["status"]["agentState"] == "idle"
    });

    end_agent(&sockets, &work);
    reconcile_until(&env, "resumed on its session", |env| {
        let states = reported(env, "t1");
        states
            .iter()
            .rposition(|(s, _)| s == "started")
            .is_some_and(|at| {
                states[at..]
                    .iter()
                    .any(|(s, c)| s == "prompt" && c.as_deref() == Some(&*conversation))
            })
    });
    end_agent(&sockets, &work);
    assert!(
        snapshot(&config, &[]) == before,
        "kilo's config directory {} changed",
        config.display()
    );
}

fn any_file_holds(dir: &Path, text: &str) -> bool {
    snapshot(dir, &[])
        .values()
        .any(|bytes| String::from_utf8_lossy(bytes).contains(text))
}

#[test]
fn omp_reports_its_state_through_the_extension_fleet_passes_at_launch() {
    let (Some(program), Some(_), Some(_)) = (runtime_mux(), tmux(), agent("omp")) else {
        return;
    };
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let agent_dir = std::env::var_os("PI_CODING_AGENT_DIR")
        .map_or_else(|| home.join(".omp/agent"), PathBuf::from);
    // omp rewrites its databases at each start and keeps transcripts per session directory.
    let user_files = |dir: &Path| {
        let skip = [
            "sessions",
            "terminal-sessions",
            "predict",
            "custom-session-files",
        ];
        let mut files = snapshot(dir, &skip);
        files.retain(|f, _| {
            let name = f.to_string_lossy();
            !(name.ends_with(".db")
                || name.ends_with(".db-wal")
                || name.ends_with(".db-shm")
                || name == "last-changelog-version")
        });
        files
    };
    let before = user_files(&agent_dir);
    let sockets = Sockets::new("omp");
    let env = agent_env("runtime-mux-omp", &program, &sockets);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let sessions = env.root.join("omp-sessions");
    let overlay = env.root.join("omp-overlay.yml");
    fs::write(
        &overlay,
        "modelRoles:\n  default: anthropic/claude-haiku-4-5\ntools:\n  approvalMode: always-ask\n",
    )
    .expect("overlay");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("omp");
    task["spec"]["brief"] =
        json!("Run the shell command `echo fleet` yourself, with your bash tool, then stop.");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    task["spec"]["env"] = json!({
        "HOME": env.root.to_string_lossy(),
        "PI_CODING_AGENT_DIR": agent_dir.to_string_lossy(),
        "PI_CONFIG_FILES": overlay.to_string_lossy(),
        "PI_CODING_AGENT_SESSION_DIR": sessions.to_string_lossy(),
    });
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on omp's prompt hook", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    reconcile_until(&env, "waiting on the approval", |env| {
        env.get("t1")["status"]["agentState"] == "attention"
    });
    // omp reports a request before it draws its dialog, and takes keys only once the dialog is drawn.
    let pane = pane_in(&sockets, &work);
    reconcile_until(&env, "the turn ended", |env| {
        let screen = sockets.tmux(&["capture-pane", "-p", "-t", &pane]).stdout;
        if env.get("t1")["status"]["agentState"] == "attention"
            && String::from_utf8_lossy(&screen).contains("Allow tool")
        {
            sockets.tmux(&["send-keys", "-t", &pane, "Enter"]);
        }
        in_order(
            &reported(env, "t1"),
            &["idle", "prompt", "working", "blocked", "working", "idle"],
        ) && env.get("t1")["status"]["agentState"] == "idle"
    });
    let conversation = env.get("t1")["status"]["conversation"]
        .as_str()
        .expect("a conversation")
        .to_string();
    let transcript = snapshot(&sessions, &[])
        .keys()
        .any(|f| f.to_string_lossy().contains(&conversation));
    assert!(
        transcript,
        "omp's session {conversation} is the conversation"
    );

    end_agent(&sockets, &work);
    reconcile_until(&env, "resumed on its session", |env| {
        let states = reported(env, "t1");
        states
            .iter()
            .rposition(|(s, _)| s == "started")
            .is_some_and(|at| {
                states[at..]
                    .iter()
                    .any(|(s, c)| s == "idle" && c.as_deref() == Some(&*conversation))
            })
    });
    end_agent(&sockets, &work);
    let after = user_files(&agent_dir);
    let changed: Vec<&PathBuf> = before
        .keys()
        .chain(after.keys())
        .filter(|f| before.get(*f) != after.get(*f))
        .collect();
    assert!(
        changed.is_empty(),
        "omp's agent directory {} changed: {changed:?}",
        agent_dir.display()
    );
}

struct FakeModel {
    url: String,
}

impl FakeModel {
    fn start() -> FakeModel {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/v1", listener.local_addr().expect("address"));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let _ = FakeModel::answer(stream);
                });
            }
        });
        FakeModel { url }
    }

    fn answer(mut stream: std::net::TcpStream) -> std::io::Result<()> {
        use std::io::{Read, Write};
        let mut bytes = Vec::new();
        let mut buf = [0; 8192];
        let body_at = loop {
            let n = stream.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            bytes.extend_from_slice(&buf[..n]);
            if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let head = String::from_utf8_lossy(&bytes[..body_at]).to_ascii_lowercase();
        let length: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        while bytes.len() < body_at + length {
            let n = stream.read(&mut buf)?;
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buf[..n]);
        }
        let request: Value = serde_json::from_slice(&bytes[body_at..]).unwrap_or(Value::Null);
        let messages = request["messages"].as_array().cloned().unwrap_or_default();
        let shell = request["tools"].as_array().and_then(|tools| {
            tools.iter().find_map(|t| {
                t["function"]["name"].as_str().filter(|n| {
                    let n = n.to_ascii_lowercase();
                    n.contains("shell") || n.contains("bash")
                })
            })
        });
        let asked = messages
            .iter()
            .any(|m| m["role"] == "user" && m["content"].to_string().contains("FLEET-TOOL"));
        let answered = messages.iter().any(|m| m["role"] == "tool");
        let message = match shell {
            Some(name) if asked && !answered => json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {
                    "name": name,
                    "arguments": json!({"command": "touch made-by-fleet"}).to_string(),
                }}],
            }),
            _ => json!({"role": "assistant", "content": "done"}),
        };
        let finish = if message["tool_calls"].is_null() {
            "stop"
        } else {
            "tool_calls"
        };
        let usage = json!({"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2});
        let (kind, body) = if request["stream"] == true {
            let chunk = |choices: Value, extra: Value| {
                let mut c = json!({"id": "c1", "object": "chat.completion.chunk", "created": 1,
                    "model": request["model"], "choices": choices});
                if !extra.is_null() {
                    c["usage"] = extra;
                }
                format!("data: {c}\n\n")
            };
            let body = [
                chunk(
                    json!([{"index": 0, "delta": message, "finish_reason": null}]),
                    Value::Null,
                ),
                chunk(
                    json!([{"index": 0, "delta": {}, "finish_reason": finish}]),
                    Value::Null,
                ),
                chunk(json!([]), usage),
                "data: [DONE]\n\n".to_string(),
            ]
            .concat();
            ("text/event-stream", body)
        } else {
            let body = json!({"id": "c1", "object": "chat.completion", "created": 1,
                "model": request["model"], "usage": usage,
                "choices": [{"index": 0, "message": message, "finish_reason": finish}]});
            ("application/json", body.to_string())
        };
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )?;
        stream.flush()
    }
}

#[test]
fn qwen_reports_its_state_through_the_settings_fleet_names_at_launch() {
    let (Some(program), Some(_), Some(_)) = (runtime_mux(), tmux(), agent("qwen")) else {
        return;
    };
    let model = FakeModel::start();
    let sockets = Sockets::new("qwen");
    let env = agent_env("runtime-mux-qwen", &program, &sockets);
    let config = env.root.join(".qwen");
    fs::create_dir_all(&config).expect("user config");
    let marker = env.root.join("user-hook-ran");
    fs::write(
        config.join("settings.json"),
        json!({
            "$version": 4,
            "tools": {"approvalMode": "default"},
            "hooks": {"UserPromptSubmit": [{"hooks": [{
                "type": "command",
                "command": format!("touch '{}'", marker.display()),
            }]}]},
        })
        .to_string(),
    )
    .expect("user settings");
    let before = fs::read(config.join("settings.json")).expect("user settings");
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("qwen");
    task["spec"]["brief"] = json!("Run the FLEET-TOOL command.");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    task["spec"]["env"] = json!({
        "HOME": env.root.to_string_lossy(),
        "OPENAI_API_KEY": "fake",
        "OPENAI_BASE_URL": model.url,
        "OPENAI_MODEL": "fake-model",
    });
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on qwen's prompt hook", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    reconcile_until(&env, "waiting on the approval", |env| {
        env.get("t1")["status"]["agentState"] == "attention"
    });
    // Allow once: the default choice.
    sockets.tmux(&["send-keys", "-t", &pane_in(&sockets, &work), "Enter"]);
    reconcile_until(&env, "the turn ended", |env| {
        in_order(
            &reported(env, "t1"),
            &["idle", "prompt", "working", "blocked", "working", "idle"],
        ) && env.get("t1")["status"]["agentState"] == "idle"
    });
    assert!(work.join("made-by-fleet").is_file(), "the command ran");
    assert!(marker.is_file(), "the user's own hook ran too");
    let conversation = env.get("t1")["status"]["conversation"]
        .as_str()
        .expect("a conversation")
        .to_string();

    end_agent(&sockets, &work);
    reconcile_until(&env, "resumed on its session", |env| {
        let states = reported(env, "t1");
        states
            .iter()
            .rposition(|(s, _)| s == "started")
            .is_some_and(|at| {
                states[at..]
                    .iter()
                    .any(|(s, c)| s == "idle" && c.as_deref() == Some(&*conversation))
            })
    });
    end_agent(&sockets, &work);
    assert_eq!(
        fs::read(config.join("settings.json")).expect("user settings"),
        before,
        "the user's settings changed"
    );
    assert!(
        !any_file_holds(&config, "fleet signal"),
        "Fleet's hooks were written under {}",
        config.display()
    );
}

fn screen_in(sockets: &Sockets, work: &Path) -> String {
    let panes = sockets.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&panes.stdout).into_owned();
    let Some(pane) = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
    else {
        return String::new();
    };
    let out = sockets.tmux(&["capture-pane", "-p", "-t", pane]);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn kimi_is_briefed_and_reports_its_state_through_the_hooks_fleet_integration_installs() {
    let (Some(program), Some(_), Some(_)) = (runtime_mux(), tmux(), agent("kimi")) else {
        return;
    };
    let model = FakeModel::start();
    let sockets = Sockets::new("kimi");
    let mut env = agent_env("runtime-mux-kimi", &program, &sockets);
    let home = env.root.join("kimi-home");
    fs::create_dir_all(&home).expect("kimi home");
    let marker = env.root.join("user-hook-ran");
    let user = format!(
        "# the user's own\n[[hooks]]\nevent = \"UserPromptSubmit\"\ncommand = \"touch '{}'\"\n",
        marker.display()
    );
    fs::write(home.join("config.toml"), &user).expect("user config");
    env.set_var("KIMI_CODE_HOME", &home.to_string_lossy());
    env.fleet(&["integration", "install", "kimi"]).ok();
    let rows = env
        .fleet(&["integration", "status", "kimi", "--json"])
        .ok()
        .json();
    assert_eq!(rows[0]["state"], "current", "{rows:#}");
    assert_eq!(rows[0]["dir"], json!(home.to_string_lossy()), "{rows:#}");

    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("kimi");
    task["spec"]["brief"] = json!("Run the FLEET-TOOL command.");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    task["spec"]["env"] = json!({
        "HOME": env.root.to_string_lossy(),
        "KIMI_CODE_HOME": home.to_string_lossy(),
        "KIMI_CODE_NO_AUTO_UPDATE": "1",
        "KIMI_MODEL_NAME": "fake-model",
        "KIMI_MODEL_API_KEY": "fake",
        "KIMI_MODEL_BASE_URL": model.url,
        "KIMI_MODEL_PROVIDER_TYPE": "openai",
    });
    env.apply(json!([task])).ok();

    reconcile_until(&env, "kimi asks to trust its folder", |_| {
        screen_in(&sockets, &work).contains("Trust and continue")
    });
    let pane = pane_in(&sockets, &work);
    sockets.tmux(&["send-keys", "-t", &pane, "Enter"]);
    reconcile_until(&env, "briefed on kimi's prompt hook", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    reconcile_until(&env, "waiting on the approval", |env| {
        env.get("t1")["status"]["agentState"] == "attention"
    });
    // Approve once: the default choice.
    sockets.tmux(&["send-keys", "-t", &pane, "Enter"]);
    reconcile_until(&env, "the turn ended", |env| {
        in_order(
            &reported(env, "t1"),
            &["idle", "prompt", "working", "blocked", "working", "idle"],
        ) && env.get("t1")["status"]["agentState"] == "idle"
    });
    assert!(work.join("made-by-fleet").is_file(), "the command ran");
    assert!(marker.is_file(), "the user's own hook ran too");
    let conversation = env.get("t1")["status"]["conversation"]
        .as_str()
        .expect("a conversation")
        .to_string();
    assert!(conversation.starts_with("session_"), "{conversation}");
    end_agent(&sockets, &work);

    env.fleet(&["integration", "uninstall", "kimi"]).ok();
    assert_eq!(
        fs::read_to_string(home.join("config.toml")).expect("user config"),
        user,
        "the user's config changed"
    );
}

#[test]
fn copilot_reports_its_state_through_the_plugin_fleet_passes_at_launch() {
    let (Some(program), Some(_), Some(_)) = (runtime_mux(), tmux(), agent("copilot")) else {
        return;
    };
    let model = FakeModel::start();
    let sockets = Sockets::new("copilot");
    let env = agent_env("runtime-mux-copilot", &program, &sockets);
    let home = env.root.join(".copilot");
    let hooks = home.join("hooks");
    fs::create_dir_all(&hooks).expect("user hooks");
    let marker = env.root.join("user-hook-ran");
    fs::write(
        hooks.join("user.json"),
        json!({"version": 1, "hooks": {"userPromptSubmitted": [{
            "type": "command",
            "bash": format!("touch '{}'", marker.display()),
        }]}})
        .to_string(),
    )
    .expect("user hooks");
    let before = snapshot(&hooks, &[]);
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("copilot");
    task["spec"]["brief"] = json!("Run the FLEET-TOOL command.");
    task["spec"].as_object_mut().unwrap().remove("outputs");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    task["spec"]["env"] = json!({
        "HOME": env.root.to_string_lossy(),
        "COPILOT_HOME": home.to_string_lossy(),
        "COPILOT_AUTO_UPDATE": "false",
        "COPILOT_OFFLINE": "true",
        "COPILOT_PROVIDER_BASE_URL": model.url,
        "COPILOT_PROVIDER_API_KEY": "fake",
        "COPILOT_MODEL": "fake-model",
    });
    env.apply(json!([task])).ok();

    reconcile_until(&env, "copilot asks to trust its folder", |_| {
        screen_in(&sockets, &work).contains("Do you trust the files in this folder?")
    });
    let pane = pane_in(&sockets, &work);
    // Yes, for this session only: the default choice.
    sockets.tmux(&["send-keys", "-t", &pane, "Enter"]);
    reconcile_until(&env, "briefed on copilot's prompt hook", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    reconcile_until(&env, "waiting on the approval", |env| {
        env.get("t1")["status"]["agentState"] == "attention"
    });
    // Yes, once: the default choice.
    sockets.tmux(&["send-keys", "-t", &pane, "Enter"]);
    reconcile_until(&env, "the turn ended", |env| {
        in_order(
            &reported(env, "t1"),
            &["idle", "prompt", "working", "blocked", "working", "idle"],
        ) && env.get("t1")["status"]["agentState"] == "idle"
    });
    assert!(work.join("made-by-fleet").is_file(), "the command ran");
    assert!(marker.is_file(), "the user's own hook ran too");
    let conversation = env.get("t1")["status"]["conversation"]
        .as_str()
        .expect("a conversation")
        .to_string();
    assert!(
        home.join("session-state").join(&conversation).is_dir(),
        "{conversation} is copilot's session id"
    );
    end_agent(&sockets, &work);
    assert_eq!(snapshot(&hooks, &[]), before, "the user's hooks changed");
}

#[test]
fn fleet_runtime_browse_and_branches_read_a_place_through_its_runtime() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("browse");
    let env = mux_env("runtime-browse", &program, &sockets);
    let dir = env.root.join("folders");
    fs::create_dir_all(dir.join("plain")).expect("plain");
    fs::create_dir_all(dir.join("repo")).expect("repo");
    fs::write(dir.join("notes.txt"), "a file").expect("file");
    let repo = dir.join("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "first",
        ],
    );
    git(&repo, &["branch", "feat"]);
    let want = serde_json::json!({
        "dir": dir.display().to_string(),
        "entries": [{"name": "plain", "repo": false}, {"name": "repo", "repo": true}],
    });
    for place in ["local", "mux/local"] {
        let run = env.fleet(&["runtime", "browse", place, dir.to_str().unwrap(), "--json"]);
        run.ok();
        assert_eq!(run.json(), want, "{place}");
    }
    let run = env.fleet(&["runtime", "browse", "local", "--json"]);
    run.ok();
    let home = run.json();
    assert_eq!(home["dir"], env.root.display().to_string(), "{home}");
    assert!(
        home["entries"]
            .as_array()
            .is_some_and(|e| e.contains(&serde_json::json!({"name": "folders", "repo": false}))),
        "{home}"
    );
    let text = env.fleet(&["runtime", "browse", "local", dir.to_str().unwrap()]);
    text.ok().stdout_has("plain/").stdout_has("repo/  (git)");

    let run = env.fleet(&[
        "runtime",
        "branches",
        "local",
        repo.to_str().unwrap(),
        "--json",
    ]);
    run.ok();
    assert_eq!(
        run.json(),
        serde_json::json!({"names": ["feat", "main"], "default": "main"})
    );
    env.fleet(&["runtime", "branches", "local", repo.to_str().unwrap()])
        .ok()
        .stdout_has("* main");

    let missing = dir.join("absent");
    env.fleet(&[
        "runtime",
        "browse",
        "local",
        missing.to_str().unwrap(),
        "--json",
    ])
    .code(1)
    .stderr_has("absent");
    env.fleet(&[
        "runtime",
        "branches",
        "local",
        dir.join("plain").to_str().unwrap(),
    ])
    .code(1)
    .stderr_has("no git repository");
    env.fleet(&["runtime", "browse", "mux/devbox", dir.to_str().unwrap()])
        .code(1)
        .stderr_has("no place mux/devbox");
}

#[test]
fn a_runtime_without_browse_is_refused_by_name() {
    use std::os::unix::fs::PermissionsExt;
    let mut env = Env::new("runtime-no-browse");
    let log = env.root.join("calls");
    let program = env.root.join("runtime.sh");
    fs::write(
        &program,
        format!(
            "#!/bin/sh\ncat >/dev/null\necho \"$1\" >> '{}'\necho '{{\"known\":{{\"protocol\":2,\"verbs\":[\"manifest\",\"start\",\"inspect\",\"stop\"],\"places\":[{{\"name\":\"local\",\"platform\":\"posix\",\"dynamic\":false,\"ready\":true}}]}}}}'\n",
            log.display()
        ),
    )
    .expect("runtime");
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("chmod");
    fs::write(
        &env.config,
        format!(
            "version = 1\n\n[data]\ndir = \"data\"\n\n[runtimes.bare]\nprogram = '{}'\n",
            program.display()
        ),
    )
    .expect("config");
    let home = env.root.to_string_lossy().into_owned();
    env.set_var("HOME", &home);
    env.fleet(&["runtime", "browse", "local", "/", "--json"])
        .code(1)
        .stderr_has("runtime bare does not offer browse");
    env.fleet(&["runtime", "branches", "local", "/", "--json"])
        .code(1)
        .stderr_has("runtime bare does not offer branches");
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.lines().all(|verb| verb == "manifest"),
        "only the manifest is asked: {calls}"
    );
}

#[test]
fn one_otel_setup_traces_a_task_through_fleet_runtime_mux_and_workspace_git() {
    use support::otlp::{Collector, Span};
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let collector = Collector::start();
    let sockets = Sockets::new("otel");
    let mut env = mux_env("runtime-mux-otel", &program, &sockets);
    env.set_var("RUNTIME_MUX_CONFIG", &mux_config(&env).to_string_lossy());
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint);
    env.set_var(
        "OTEL_RESOURCE_ATTRIBUTES",
        "deployment.environment.name=e2e",
    );
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let harness = bin.join("claude");
    fs::write(
        &harness,
        "#!/bin/sh\nenv > \"$PWD/env.log\"\nexec sleep 300\n",
    )
    .expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", bin.display())));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");

    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    env.reconcile().ok();
    let env_log = work.join("env.log");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !env_log.is_file() {
        assert!(Instant::now() < deadline, "the agent never started");
        std::thread::sleep(Duration::from_millis(100));
    }
    let listed = String::from_utf8_lossy(
        &sockets
            .tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"])
            .stdout,
    )
    .into_owned();
    let pane = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    env.fleet(&["delete", "task", "t1"]).ok();
    sockets.tmux(&["send-keys", "-t", &pane, "C-c"]);
    let deadline = Instant::now() + Duration::from_secs(30);
    while env.fleet(&["get", "task", "t1"]).code == Some(0) {
        assert!(Instant::now() < deadline, "{:#}", env.get("t1"));
        std::thread::sleep(Duration::from_millis(200));
        env.reconcile();
    }

    let spans = collector.spans();
    let of = |service: &str, name: &str| -> Vec<Span> {
        spans
            .iter()
            .filter(|s| s.resource["service.name"] == service && s.name == name)
            .cloned()
            .collect()
    };
    let one = |service: &str, name: &str| -> Span {
        of(service, name).into_iter().next().unwrap_or_else(|| {
            let names: std::collections::BTreeSet<_> = spans
                .iter()
                .map(|s| format!("{} {}", s.resource["service.name"], s.name))
                .collect();
            panic!("no `{name}` span from {service} in {names:#?}")
        })
    };
    for span in &spans {
        assert_eq!(
            span.resource["deployment.environment.name"], "e2e",
            "{span:#?}"
        );
    }
    let spawn = one("fleet", "effect spawn");
    let release = one("fleet", "effect release");
    let start = one("fleet", "runtime start");
    let mux_start = one("runtime-mux", "runtime-mux start");
    let mux_workspace = one("runtime-mux", "runtime-mux workspace");
    let make = one("workspace-git", "workspace-git make");
    let mux_stop = one("runtime-mux", "runtime-mux stop");
    let trace = &spawn.trace_id;
    for span in [
        &release,
        &start,
        &mux_start,
        &mux_workspace,
        &make,
        &mux_stop,
    ] {
        assert_eq!(&span.trace_id, trace, "{span:#?}");
    }
    assert_eq!(start.parent, spawn.span_id, "{start:#?}");
    assert_eq!(start.attrs["fleet.runtime.verb"], "start", "{start:#?}");
    assert_eq!(start.attrs["fleet.outcome"], "known", "{start:#?}");
    assert_eq!(mux_start.parent, start.span_id, "{mux_start:#?}");
    let workspace = spans
        .iter()
        .find(|s| s.span_id == mux_workspace.parent)
        .expect("fleet's workspace call");
    assert_eq!(workspace.name, "runtime workspace", "{workspace:#?}");
    assert_eq!(make.parent, mux_workspace.span_id, "{make:#?}");

    let all = collector.points();
    let has = |metric: &str, attr: &str, value: &str| {
        all.iter()
            .any(|p| p.metric == metric && p.attrs.get(attr).is_some_and(|v| v == value))
    };
    for (metric, attr, value) in [
        ("fleet.tasks", "fleet.phase", "Pending"),
        ("fleet.sessions", "fleet.session.state", "Paused"),
        (
            "fleet.reconcile.duration",
            "fleet.controller",
            "TaskController",
        ),
        ("fleet.effects", "fleet.effect", "spawn"),
        ("fleet.runtime.call.duration", "fleet.runtime.verb", "start"),
        ("fleet.runtime.call.duration", "fleet.outcome", "known"),
    ] {
        assert!(
            has(metric, attr, value),
            "{metric} {attr}={value} in {all:#?}"
        );
    }

    let seen = fs::read_to_string(&env_log).expect("the agent's environment");
    let otel: std::collections::BTreeMap<&str, &str> = seen
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| k.starts_with("OTEL_") || k.starts_with("TRACE"))
        .collect();
    assert_eq!(
        otel.keys().copied().collect::<Vec<_>>(),
        [
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_PROTOCOL",
            "OTEL_LOGS_EXPORTER",
            "OTEL_METRICS_EXPORTER",
            "OTEL_RESOURCE_ATTRIBUTES",
            "OTEL_SERVICE_NAME",
        ],
        "{seen}"
    );
    assert_eq!(otel["OTEL_EXPORTER_OTLP_ENDPOINT"], collector.endpoint);
    assert_eq!(otel["OTEL_SERVICE_NAME"], "claude");
    let resource = otel["OTEL_RESOURCE_ATTRIBUTES"];
    assert!(resource.contains("fleet.task=t1"), "{resource}");
    assert!(
        !resource.contains("deployment.environment.name"),
        "{resource}"
    );
    assert!(seen.contains("CLAUDE_CODE_ENABLE_TELEMETRY=1\n"), "{seen}");
}

#[test]
fn every_runtime_mux_and_workspace_git_span_has_its_parent_in_its_trace() {
    use support::otlp::{Collector, Span};
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let collector = Collector::start();
    let sockets = Sockets::new("otel-orphans");
    let mut env = mux_env("runtime-mux-otel-orphans", &program, &sockets);
    env.set_var("RUNTIME_MUX_CONFIG", &mux_config(&env).to_string_lossy());
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let harness = bin.join("claude");
    fs::write(&harness, "#!/bin/sh\nexec sleep 300\n").expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", bin.display())));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");

    let mut up = Up(env.spawn(&["up", "--poll-ms", "100"]));
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !env.get("t1")["status"]["session"]["id"].is_string() {
        if Instant::now() > deadline {
            panic!("the Task never started: {:#}", env.get("t1"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    env.fleet(&["attach", "t1", "--json"]).ok();
    env.fleet(&["shell", "t1", "--json"]).ok();
    let listed = String::from_utf8_lossy(
        &sockets
            .tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"])
            .stdout,
    )
    .into_owned();
    let pane = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    env.fleet(&["delete", "task", "t1"]).ok();
    sockets.tmux(&["send-keys", "-t", &pane, "C-c"]);
    let deadline = Instant::now() + Duration::from_secs(30);
    while env.fleet(&["get", "task", "t1"]).code == Some(0) {
        if Instant::now() > deadline {
            panic!("the Task was never released: {:#}", env.get("t1"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Command::new("kill")
        .args(["-INT", &up.0.id().to_string()])
        .status()
        .expect("kill");
    up.0.wait().expect("fleet up stops");

    let spans = collector.spans();
    let named = |service: &str, name: &str| {
        spans
            .iter()
            .any(|s| s.resource["service.name"] == service && s.name == name)
    };
    for (service, name) in [
        ("fleet", "fleet attach"),
        ("fleet", "fleet shell"),
        ("fleet", "runtime start"),
        ("runtime-mux", "runtime-mux start"),
        ("workspace-git", "workspace-git make"),
    ] {
        assert!(named(service, name), "no `{name}` from {service}");
    }
    let parent_of = |span: &Span| {
        spans
            .iter()
            .find(|p| p.trace_id == span.trace_id && p.span_id == span.parent)
    };
    let orphans: Vec<String> = spans
        .iter()
        .filter(|s| {
            let service = &s.resource["service.name"];
            service == "runtime-mux"
                || service == "workspace-git"
                || (service == "fleet" && s.name.starts_with("runtime "))
        })
        .filter(|s| parent_of(s).is_none())
        .map(|s| format!("{} {}", s.resource["service.name"], s.name))
        .collect();
    assert!(
        orphans.is_empty(),
        "spans without their parent: {orphans:#?}"
    );
}

#[test]
fn a_stalled_collector_never_times_out_a_runtime_call() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let stalled = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let endpoint = format!("http://{}", stalled.local_addr().expect("address"));
    let sockets = Sockets::new("otel-stall");
    let mut env = mux_env("runtime-mux-otel-stall", &program, &sockets);
    let config = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        config.replace(
            "callTimeoutMs = 10000",
            "callTimeoutMs = 2000\nworkspaceTimeoutMs = 2000",
        ),
    )
    .expect("config");
    env.set_var("RUNTIME_MUX_CONFIG", &mux_config(&env).to_string_lossy());
    env.set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let harness = bin.join("claude");
    fs::write(&harness, "#!/bin/sh\nexec sleep 300\n").expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", bin.display())));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");

    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    env.reconcile();
    let status = env.get("t1")["status"].clone();
    assert!(
        status["session"]["id"].is_string(),
        "the start answered within the deadline: {status:#}"
    );
    drop(stalled);
}

struct Sshd {
    child: std::process::Child,
    port: u16,
    key: PathBuf,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Sshd {
    fn start(dir: &Path, home: &Path, sockets: &Sockets) -> Option<Sshd> {
        let Some(program) = ["/usr/sbin/sshd", "/usr/bin/sshd"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
        else {
            missing("no sshd; install openssh-server");
            return None;
        };
        fs::create_dir_all(dir).expect("sshd dir");
        fs::create_dir_all(home).expect("place home");
        for name in ["host", "user"] {
            let made = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(dir.join(name))
                .status()
                .expect("ssh-keygen");
            assert!(made.success(), "ssh-keygen {name}");
        }
        fs::copy(dir.join("user.pub"), dir.join("authorized_keys")).expect("authorized_keys");
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        let config = dir.join("sshd_config");
        fs::write(
            &config,
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host\nPidFile {d}/sshd.pid\n\
                 AuthorizedKeysFile {d}/authorized_keys\nStrictModes no\nUsePAM no\n\
                 PasswordAuthentication no\nKbdInteractiveAuthentication no\n\
                 SetEnv HOME={h} TMUX_TMPDIR={s} SHELL=/bin/sh\n",
                d = dir.display(),
                h = home.display(),
                s = sockets.0.display()
            ),
        )
        .expect("sshd_config");
        let log = fs::File::create(dir.join("sshd.log")).expect("sshd log");
        let child = Command::new(&program)
            .args(["-D", "-e", "-f"])
            .arg(&config)
            .stderr(log)
            .spawn()
            .expect("sshd");
        let sshd = Sshd {
            child,
            port,
            key: dir.join("user"),
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let up = Command::new("ssh")
                .args(sshd.opts())
                .args(["127.0.0.1", "true"])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if up {
                return Some(sshd);
            }
            assert!(
                Instant::now() < deadline,
                "sshd did not come up: {}",
                fs::read_to_string(dir.join("sshd.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn opts(&self) -> Vec<String> {
        [
            "-F",
            "/dev/null",
            "-p",
            &self.port.to_string(),
            "-i",
            &self.key.to_string_lossy(),
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "LogLevel=ERROR",
        ]
        .map(String::from)
        .to_vec()
    }

    fn place(&self, name: &str, path: &[&Path]) -> String {
        let quoted = |items: Vec<String>| {
            items
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "\n[runtimes.mux.places.{name}]\nkind = \"ssh\"\ndestination = \"127.0.0.1\"\nssh_opts = [{}]\npath_prepend = [{}]\n",
            quoted(self.opts()),
            quoted(path.iter().map(|p| p.display().to_string()).collect())
        )
    }
}

#[test]
fn a_task_on_an_ssh_place_runs_there_and_reports_its_state() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("ssh-local");
    let remote = Sockets::new("ssh-place");
    let mut env = mux_env("runtime-mux-ssh", &program, &sockets);
    let home = env.root.join("place-home");
    let Some(sshd) = Sshd::start(&env.root.join("sshd"), &home, &remote) else {
        return;
    };
    let bin = env.root.join("place-bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, CLAUDE).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        format!(
            "{cfg}\n[runtimes.mux.places.local]\n{}",
            sshd.place("box", &[&bin, fleet_dir])
        ),
    )
    .expect("config");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&path));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["host"] = json!("mux/box");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on the prompt hook's proof", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let file = fs::read_to_string(work.join("signal-file")).expect("FLEET_SIGNAL_FILE");
    let on_place = file
        .strip_prefix("~/")
        .map(|rest| home.join(rest))
        .unwrap_or_else(|| panic!("a file on the place, under its home: {file:?}"));
    assert!(on_place.is_file(), "the agent reported to {file:?}");
    reconcile_until(&env, "idle, as its hooks report on the place", |env| {
        env.get("t1")["status"]["sessionState"] == "Idle"
    });
    assert_eq!(env.get("t1")["status"]["conversation"], "conv-1");
    assert_eq!(env.get("t1")["status"]["session"]["host"], "mux/box");

    a_place_event_reaches_the_board_as_the_task(&env, &home, &file);

    let listed = remote.tmux(&["list-panes", "-a", "-F", "#{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(
        listed.lines().any(|l| l == work.to_string_lossy()),
        "the agent's pane is on the place: {listed:?}"
    );
    let here = sockets.tmux(&["list-panes", "-a"]);
    assert!(
        here.stdout.is_empty(),
        "nothing runs on this machine's server"
    );

    let place = json!({"name": "box", "config": toml_place(&sshd, &[&bin, fleet_dir])});
    let key = format!(
        "fleet.{}",
        env.get("t1")["metadata"]["uid"].as_str().expect("uid")
    );
    let listed = mux_call(
        &program,
        &sockets,
        "list",
        &json!({"protocol": 2, "place": place}),
    );
    assert!(
        listed["known"]["keys"]
            .as_array()
            .is_some_and(|k| k.iter().any(|k| *k == key.as_str())),
        "{listed}"
    );
    let screen = mux_call(
        &program,
        &sockets,
        "capture",
        &json!({"protocol": 2, "place": place, "key": key}),
    );
    assert!(screen["known"]["screen"]["text"].is_string(), "{screen}");

    let stopped = mux_call(
        &program,
        &sockets,
        "stop",
        &json!({"protocol": 2, "place": place, "key": key}),
    );
    assert_eq!(stopped, json!({"known": "stopped"}));
    let inspected = mux_call(
        &program,
        &sockets,
        "inspect",
        &json!({"protocol": 2, "place": place, "key": key}),
    );
    assert_eq!(inspected, json!({"known": "none"}));
}

fn a_place_event_reaches_the_board_as_the_task(env: &Env, home: &Path, file: &str) {
    env.apply(json!({"apiVersion": "fleet/v1alpha1", "kind": "Board", "metadata": {"name": "shared"}, "spec": {}}))
        .ok();
    let mut publish = env.command(&["event", "publish", "--type", "place.hello", "--data", "1"]);
    publish
        .env("HOME", home)
        .env("FLEET_EVENT_FILE", format!("{file}.events"));
    support::wait(publish.spawn().expect("fleet event publish")).ok();
    reconcile_until(env, "the place's event on the board", |env| {
        env.fleet(&["event", "list", "--json"]).ok().json() != json!([])
    });
    let events = env.fleet(&["event", "list", "--json"]).ok().json();
    assert_eq!(events[0]["type"], "place.hello", "{events}");
    assert_eq!(events[0]["source"]["task"], "t1", "{events}");
}

fn toml_place(sshd: &Sshd, path: &[&Path]) -> Value {
    json!({
        "kind": "ssh",
        "destination": "127.0.0.1",
        "ssh_opts": sshd.opts(),
        "path_prepend": path.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
    })
}

#[test]
fn an_ssh_place_reports_its_readiness() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("ssh-ready");
    let remote = Sockets::new("ssh-ready-place");
    let env = mux_env("runtime-mux-ssh-ready", &program, &sockets);
    let home = env.root.join("place-home");
    let Some(sshd) = Sshd::start(&env.root.join("sshd"), &home, &remote) else {
        return;
    };
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let fleet_only = env.root.join("fleet-only");
    fs::create_dir_all(&fleet_only).expect("fleet-only");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fleet"), fleet_only.join("fleet"))
        .expect("fleet link");
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port();
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        format!(
            "{cfg}\n[runtimes.mux.places.local]\n{}{}\n[runtimes.mux.places.down]\nkind = \"ssh\"\ndestination = \"127.0.0.1\"\nssh_opts = [\"-F\", \"/dev/null\", \"-p\", \"{closed}\"]\n\n[runtimes.mux.places.odd]\nkind = \"ssh\"\ndestination = \"127.0.0.1\"\nruntime = \"mux\"\n",
            sshd.place("box", &[fleet_dir]),
            sshd.place("bare", &[&fleet_only])
        ),
    )
    .expect("config");
    let places = env.fleet(&["runtime", "places", "--json"]).ok().json();
    let rows = places["places"].as_array().expect("places").clone();
    let row = |id: &str| {
        rows.iter()
            .find(|r| r["place"] == id)
            .unwrap_or_else(|| panic!("no {id} in {places:#}"))
            .clone()
    };
    assert_eq!(row("mux/box")["ready"], true, "{places:#}");
    let down = row("mux/down");
    assert_eq!(down["ready"], false, "{places:#}");
    let why = down["why"].as_str().unwrap_or_default();
    assert!(
        why.contains("cannot be reached") && why.contains("ssh -o BatchMode=yes"),
        "{why}"
    );
    let odd = row("mux/odd");
    assert_eq!(odd["ready"], false, "{places:#}");
    assert!(
        odd["why"]
            .as_str()
            .is_some_and(|w| w.contains("unknown field `runtime`")),
        "{places:#}"
    );
    let doctor = env.fleet(&["doctor"]);
    doctor
        .code(1)
        .stdout_has("mux/down")
        .stdout_has("cannot be reached");
    assert_eq!(row("mux/bare")["ready"], true, "{places:#}");
    let bare = json!({"name": "bare", "config": toml_place(&sshd, &[&fleet_only])});
    let dir = env.root.join("on-the-place");
    fs::create_dir_all(&dir).expect("dir");
    let made = mux_call(
        &program,
        &sockets,
        "workspace",
        &json!({"protocol": 2, "place": bare, "key": "k.file",
                "urls": [format!("file://{}", dir.display())]}),
    );
    assert_eq!(
        made["known"]["made"]["dirs"],
        json!([dir.display().to_string()]),
        "{made}"
    );
    let released = mux_call(
        &program,
        &sockets,
        "stop",
        &json!({"protocol": 2, "place": bare, "key": "k.file", "release": true}),
    );
    assert_eq!(released, json!({"known": "stopped"}));
    assert!(dir.is_dir(), "a file:// directory outlives its release");
    let recorded = home.join(".local/share/runtime-mux/workspaces/k.made");
    fs::create_dir_all(&recorded).expect("recorded");
    fs::write(recorded.join(".fleet-workspace.json"), "{}").expect("record");
    let held = mux_call(
        &program,
        &sockets,
        "stop",
        &json!({"protocol": 2, "place": bare, "key": "k.made", "release": true}),
    );
    let why = held["unknown"]["why"].as_str().unwrap_or_default();
    assert!(why.contains("workspace-git"), "{held}");
    assert!(recorded.is_dir(), "nothing was removed");
    let refused = mux_call(
        &program,
        &sockets,
        "workspace",
        &json!({"protocol": 2, "place": bare, "key": "k.git",
                "urls": ["git+https://example.test/app.git"]}),
    );
    let why = refused["known"]["failed"]["why"]
        .as_str()
        .unwrap_or_default();
    assert!(why.contains("workspace-git"), "{refused}");
}

#[test]
fn a_local_task_asks_about_no_other_place() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("place-alone");
    let (mut env, log) = logged_env("runtime-mux-place-alone", &program, &sockets);
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        format!(
            "{cfg}\n[runtimes.mux.places.local]\n\n[runtimes.mux.places.silent]\nkind = \"ssh\"\ndestination = \"silent.invalid\"\nssh_opts = [\"-F\", \"/dev/null\", \"-o\", \"ProxyCommand=sleep 60\"]\n"
        ),
    )
    .expect("config");
    waiting_task(&mut env);
    assert_eq!(env.status("t1")["sessionState"], "Running");
    let local = json!([{"name": "local", "config": {}}]);
    let manifests: Vec<Value> = calls(&log)
        .into_iter()
        .filter(|(v, _)| v == "manifest")
        .map(|(_, r)| r)
        .collect();
    assert!(!manifests.is_empty());
    for request in manifests {
        assert_eq!(request["places"], local, "{request}");
    }
}

const PI: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$PWD/argv.log"
ext=
while [ $# -gt 0 ]; do [ "$1" = -e ] && ext=$2; shift; done
if [ -f "$ext" ]; then printf '%s\n' "$ext" > "$PWD/extension"; fi
printf '%s' "$FLEET_ASSET_DIR" > "$PWD/asset-dir"
fleet signal idle
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  fleet signal prompt "{\"prompt\":\"$line\",\"session_id\":\"conv-pi\"}"
  fleet signal idle
done
"#;

#[test]
fn a_pi_task_on_an_ssh_place_loads_its_extension_from_the_place() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("ssh-pi");
    let remote = Sockets::new("ssh-pi-place");
    let mut env = mux_env("runtime-mux-ssh-pi", &program, &sockets);
    let home = env.root.join("place-home");
    let Some(sshd) = Sshd::start(&env.root.join("sshd"), &home, &remote) else {
        return;
    };
    let bin = env.root.join("place-bin");
    fs::create_dir_all(&bin).expect("bin");
    let pi = bin.join("pi");
    fs::write(&pi, PI).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&pi, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        format!(
            "{cfg}\n[runtimes.mux.places.local]\n{}",
            sshd.place("box", &[&bin, fleet_dir])
        ),
    )
    .expect("config");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&path));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("pi");
    task["spec"]["host"] = json!("mux/box");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on the prompt hook's proof", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let copies = home.join(".local/state/fleet/assets");
    let extension = fs::read_to_string(work.join("extension"))
        .unwrap_or_else(|_| panic!("pi found no extension file: {:?}", starts(&work)));
    assert!(
        Path::new(extension.trim()).starts_with(&copies),
        "the extension is the place's copy: {extension}"
    );
    let assets = fs::read_to_string(work.join("asset-dir")).expect("FLEET_ASSET_DIR");
    assert!(Path::new(&assets).starts_with(&copies), "{assets}");
    assert!(Path::new(&assets).join("fleet-pi.ts").is_file(), "{assets}");

    let key = format!(
        "fleet.{}",
        env.get("t1")["metadata"]["uid"].as_str().expect("uid")
    );
    let place = json!({"name": "box", "config": toml_place(&sshd, &[&bin, fleet_dir])});
    let stopped = mux_call(
        &program,
        &sockets,
        "stop",
        &json!({"protocol": 2, "place": place, "key": key}),
    );
    assert_eq!(stopped, json!({"known": "stopped"}));
    assert!(!Path::new(&assets).exists(), "a stop removes the copy");
}

#[test]
fn a_lost_session_on_a_place_without_exec_is_revived() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("ssh-noexec");
    let remote = Sockets::new("ssh-noexec-place");
    let mut env = mux_env("runtime-mux-ssh-noexec", &program, &sockets);
    let home = env.root.join("place-home");
    let Some(sshd) = Sshd::start(&env.root.join("sshd"), &home, &remote) else {
        return;
    };
    let wrapper = env.root.join("no-exec.sh");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nreq=$(cat)\ncase $1 in\n  exec) echo '{{\"unknown\":{{\"why\":\"no exec here\"}}}}' ;;\n  manifest) printf '%s' \"$req\" | '{p}' \"$@\" | sed 's/\"exec\",//' ;;\n  *) printf '%s' \"$req\" | exec '{p}' \"$@\" ;;\nesac\n",
            p = program.display()
        ),
    )
    .expect("wrapper");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("chmod");
    let bin = env.root.join("place-bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, CLAUDE).expect("harness");
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let cfg = fs::read_to_string(&env.config)
        .expect("config")
        .replace(&*program.to_string_lossy(), &wrapper.to_string_lossy());
    fs::write(
        &env.config,
        format!(
            "{cfg}\n[runtimes.mux.places.local]\n{}",
            sshd.place("box", &[&bin, fleet_dir])
        ),
    )
    .expect("config");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&path));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["host"] = json!("mux/box");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "started on the place", |_| !starts(&work).is_empty());
    let listed = remote.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    let pane = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    remote.tmux(&["send-keys", "-t", &pane, "C-c"]);
    reconcile_until(&env, "revived without exec", |_| starts(&work).len() >= 2);
}

const SKEWED_CLAUDE: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$PWD/argv.log"
file="$HOME/${FLEET_SIGNAL_FILE#\~/}"
mkdir -p "${file%/*}"
idle() { printf '{"at":%s,"state":"idle"}\n' "$(( $(date +%s) * 1000 + 600000 ))" >> "$file"; }
case " $* " in *" --resume "*) ;; *) idle ;; esac
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  printf '%s\n' "$line" >> "$PWD/typed.log"
  fleet signal prompt "{\"prompt\":\"$line\",\"session_id\":\"conv-1\"}"
  idle
done
"#;

#[test]
fn a_revive_on_a_place_with_a_skewed_clock_waits_for_the_new_report() {
    revive_on_a_skewed_place("skew", 0, None);
}

#[test]
fn a_revive_waits_for_the_new_report_past_a_mib_of_old_reports() {
    revive_on_a_skewed_place("skew-big", 1_300_000, None);
}

#[test]
fn a_restart_whose_boundary_cannot_be_read_accepts_no_old_state() {
    let wrapper = |program: &Path, dir: &Path| {
        let script = dir.join("no-length.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nreq=$(cat)\ncase $1:$req in\n  exec:*'\"--length\"'*) echo '{{\"unknown\":{{\"why\":\"no answer\"}}}}' ;;\n  *) printf '%s' \"$req\" | exec '{}' \"$@\" ;;\nesac\n",
                program.display()
            ),
        )
        .expect("wrapper");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
        script
    };
    revive_on_a_skewed_place("skew-nolen", 0, Some(&wrapper));
}

type Wrap = dyn Fn(&Path, &Path) -> PathBuf;

fn revive_on_a_skewed_place(name: &str, pad: usize, wrap: Option<&Wrap>) {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new(&format!("ssh-{name}"));
    let remote = Sockets::new(&format!("ssh-{name}-place"));
    let mut env = mux_env(&format!("runtime-mux-ssh-{name}"), &program, &sockets);
    let home = env.root.join("place-home");
    let Some(sshd) = Sshd::start(&env.root.join("sshd"), &home, &remote) else {
        return;
    };
    let bin = env.root.join("place-bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, SKEWED_CLAUDE).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let mut cfg = fs::read_to_string(&env.config).expect("config");
    if let Some(wrap) = wrap {
        let wrapped = wrap(&program, &env.root);
        cfg = cfg.replace(&*program.to_string_lossy(), &wrapped.to_string_lossy());
    }
    fs::write(
        &env.config,
        format!(
            "{cfg}\n[runtimes.mux.places.local]\n{}",
            sshd.place("box", &[&bin, fleet_dir])
        ),
    )
    .expect("config");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&path));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["host"] = json!("mux/box");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "briefed", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let typed = || {
        fs::read_to_string(work.join("typed.log"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    assert_eq!(typed(), 1, "the brief");
    let file = home
        .join(".local/state/fleet/signals")
        .join(format!("{}.jsonl", env.session_key("t1")));
    let skewed_idle = || {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            + 600_000;
        format!("{{\"at\":{at},\"state\":\"idle\"}}\n")
    };
    use std::io::Write;
    let append = |text: &str| {
        fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .expect("the place's signal file");
    };
    let line = skewed_idle();
    append(&line.repeat(pad / line.len()));
    append(&skewed_idle());

    let listed = remote.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_path}"]);
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    let pane = listed
        .lines()
        .find(|l| l.ends_with(&*work.to_string_lossy()))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no pane in {}: {listed}", work.display()))
        .to_string();
    remote.tmux(&["send-keys", "-t", &pane, "C-c"]);
    if wrap.is_some() {
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(300));
            env.reconcile();
        }
        assert_eq!(typed(), 1, "no old state taken for the new process's");
        assert_eq!(starts(&work).len(), 1, "nothing started without a boundary");
        let status = env.get("t1")["status"].to_string();
        assert!(
            status.contains("signal file"),
            "the status says why: {status}"
        );
        return;
    }
    reconcile_until(&env, "revived", |_| starts(&work).len() >= 2);
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
    assert_eq!(typed(), 1, "nothing typed before the new agent reports");

    append(&skewed_idle());
    reconcile_until(&env, "the catch-up note typed", |_| typed() >= 2);
}

const PLACE_STUB_AGENT: &str = r#"#!/bin/sh
printf '%s' "$FLEET_HARNESS_DIR" > "$PWD/harness-dir"
printf '%s' '{"thread_x":"conv-custom"}' | fleet signal idle
exec sleep 300
"#;

#[test]
fn a_user_profile_reports_its_conversation_from_an_ssh_place() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("ssh-profile");
    let remote = Sockets::new("ssh-profile-place");
    let mut env = mux_env("runtime-mux-ssh-profile", &program, &sockets);
    let home = env.root.join("place-home");
    let Some(sshd) = Sshd::start(&env.root.join("sshd"), &home, &remote) else {
        return;
    };
    let harnesses = env.config.parent().unwrap().join("harnesses");
    fs::create_dir_all(&harnesses).expect("harnesses dir");
    fs::write(
        harnesses.join("stub.toml"),
        STUB_PROFILE.replace("session_id", "thread_x"),
    )
    .expect("profile");
    let bin = env.root.join("place-bin");
    fs::create_dir_all(&bin).expect("bin");
    let agent = bin.join("stub-agent");
    fs::write(&agent, PLACE_STUB_AGENT).expect("agent");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        format!(
            "{cfg}\n[runtimes.mux.places.local]\n{}",
            sshd.place("box", &[&bin, fleet_dir])
        ),
    )
    .expect("config");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&path));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["harness"] = json!("stub");
    task["spec"]["host"] = json!("mux/box");
    task["spec"].as_object_mut().unwrap().remove("brief");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(&env, "the profile's conversation reported", |env| {
        env.get("t1")["status"]["conversation"] == "conv-custom"
    });
    let dir = fs::read_to_string(work.join("harness-dir")).expect("FLEET_HARNESS_DIR");
    assert!(
        Path::new(&dir).starts_with(home.join(".local/state/fleet/harnesses")),
        "the place's copy of the profiles: {dir}"
    );
    assert!(Path::new(&dir).join("stub.toml").is_file(), "{dir}");
}

struct Placed {
    env: Env,
    program: PathBuf,
    home: PathBuf,
    sockets: Sockets,
    remote: Sockets,
    _sshd: Sshd,
}

impl Placed {
    fn new(name: &str, claude: &str) -> Option<Placed> {
        use std::os::unix::fs::PermissionsExt;
        let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
            return None;
        };
        let sockets = Sockets::new(name);
        let remote = Sockets::new(&format!("{name}-place"));
        let mut env = mux_env(&format!("runtime-mux-{name}"), &program, &sockets);
        let home = env.root.join("place-home");
        let sshd = Sshd::start(&env.root.join("sshd"), &home, &remote)?;
        let bin = env.root.join("place-bin");
        fs::create_dir_all(&bin).expect("bin");
        let harness = bin.join("claude");
        fs::write(&harness, claude).expect("harness");
        fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
        let cfg = fs::read_to_string(&env.config).expect("config");
        fs::write(
            &env.config,
            format!(
                "{cfg}\n[runtimes.mux.places.local]\n{}",
                sshd.place("box", &[&bin, Self::build()])
            ),
        )
        .expect("config");
        let path = std::env::var("PATH").unwrap_or_default();
        env.only_on_path(Path::new(&path));
        Some(Placed {
            env,
            program,
            home,
            sockets,
            remote,
            _sshd: sshd,
        })
    }

    fn build() -> &'static Path {
        Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap()
    }

    fn remote_panes(&self) -> Vec<String> {
        let listed = self
            .remote
            .tmux(&["list-panes", "-a", "-F", "#{pane_current_path}"]);
        String::from_utf8_lossy(&listed.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }
}

#[test]
fn a_git_workspace_on_an_ssh_place_is_made_and_released_there() {
    let Some(mut p) = Placed::new("ssh-git", CLAUDE) else {
        return;
    };
    let src = p.env.root.join("src");
    let bad = p.env.root.join("bad");
    let config = p.env.root.join("runtime-mux.toml");
    fs::write(
        &config,
        format!(
            "[[workspace.setup]]\nmatch = 'git+file://{}'\nrun = ['echo ran >> setup.log']\n\n\
             [[workspace.setup]]\nmatch = 'git+file://{}'\nrun = ['echo left > left.txt; exit 3']\n",
            src.display(),
            bad.display()
        ),
    )
    .expect("runtime-mux config");
    p.env
        .set_var("RUNTIME_MUX_CONFIG", &config.to_string_lossy());
    let env = &p.env;
    fs::create_dir_all(&bad).expect("bad");
    git(&bad, &["init", "-q", "-b", "main"]);
    git(
        &bad,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "first",
        ],
    );
    fs::create_dir_all(&src).expect("source");
    git(&src, &["init", "-q", "-b", "main"]);
    git(
        &src,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "first",
        ],
    );
    let mut task = support::task("t1", true);
    task["spec"]["host"] = json!("mux/box");
    task["spec"]["workspace"] = json!([format!("git+file://{}#main", src.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(env, "briefed in its worktree on the place", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    let status = env.get("t1")["status"].clone();
    let dir = PathBuf::from(
        status["workspace"]["dirs"][0]
            .as_str()
            .unwrap_or_else(|| panic!("no workspace dirs: {status:#}")),
    );
    let root = p.home.join(".local/share/runtime-mux/workspaces");
    assert!(
        dir.starts_with(&root),
        "{} under the place's {}",
        dir.display(),
        root.display()
    );
    assert!(dir.join(".git").is_file(), "a worktree: {status:#}");
    assert_eq!(starts(&dir).len(), 1, "the agent started in the worktree");
    assert!(
        p.remote_panes().contains(&dir.display().to_string()),
        "the agent's pane is on the place, in the worktree: {:?}",
        p.remote_panes()
    );
    let branch = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let uid = env.get("t1")["metadata"]["uid"]
        .as_str()
        .expect("uid")
        .to_string();
    assert!(
        branch.trim().ends_with(&uid),
        "a branch of its own: {branch}"
    );
    assert_eq!(
        fs::read_to_string(dir.join("setup.log")).unwrap_or_default(),
        "ran\n",
        "the setup step ran once, in the worktree on the place"
    );

    let place = json!({"name": "box", "config": toml_place(&p._sshd, &[&env.root.join("place-bin"), Placed::build()])});
    let again = mux_call_env(
        &p.program,
        &p.sockets,
        "workspace",
        &json!({"protocol": 2, "place": place, "key": format!("fleet.{uid}"),
                "urls": [format!("git+file://{}#main", src.display())]}),
        &[("RUNTIME_MUX_CONFIG", &config)],
    );
    assert_eq!(again["known"]["made"]["adopted"], true, "{again}");
    assert_eq!(
        fs::read_to_string(dir.join("setup.log")).unwrap_or_default(),
        "ran\n",
        "an adopted workspace's finished setup does not run again"
    );

    let failed = mux_call_env(
        &p.program,
        &p.sockets,
        "workspace",
        &json!({"protocol": 2, "place": place, "key": "k.bad",
                "urls": [format!("git+file://{}#main", bad.display())]}),
        &[("RUNTIME_MUX_CONFIG", &config)],
    );
    let why = failed["known"]["failed"]["why"]
        .as_str()
        .unwrap_or_default();
    assert!(why.contains("exit 3"), "{failed}");
    let kept = preserved_refs(&bad);
    assert_eq!(
        kept.len(),
        1,
        "the failed setup's work is preserved: {kept:?}"
    );
    assert_eq!(
        git(&bad, &["show", &format!("{}:left.txt", kept[0])]),
        "left\n"
    );
    assert!(
        !p.home
            .join(".local/share/runtime-mux/workspaces/k.bad/bad")
            .exists(),
        "the failed workspace is gone from the place"
    );

    fs::write(dir.join("notes.txt"), "kept\n").expect("work");
    env.fleet(&["delete", "task", "t1"]).ok();
    let deadline = Instant::now() + Duration::from_secs(60);
    while env.fleet(&["get", "task", "t1"]).code == Some(0) {
        assert!(Instant::now() < deadline, "{:#}", env.get("t1"));
        std::thread::sleep(Duration::from_millis(300));
        env.reconcile();
    }
    assert!(
        !dir.exists(),
        "the release removed the worktree on the place"
    );
    let refs = preserved_refs(&src);
    assert_eq!(refs.len(), 1, "one preserved ref: {refs:?}");
    assert_eq!(
        git(&src, &["show", &format!("{}:notes.txt", refs[0])]),
        "kept\n"
    );
}

#[test]
fn browse_and_branches_read_an_ssh_place() {
    let Some(p) = Placed::new("ssh-browse", CLAUDE) else {
        return;
    };
    let env = &p.env;
    let dir = env.root.join("folders");
    fs::create_dir_all(dir.join("plain")).expect("plain");
    fs::create_dir_all(dir.join("with space")).expect("spaced");
    fs::create_dir_all(dir.join(".hidden")).expect("hidden");
    fs::write(dir.join("notes.txt"), "a file").expect("file");
    let repo = dir.join("repo");
    fs::create_dir_all(&repo).expect("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "first",
        ],
    );
    git(&repo, &["branch", "feat"]);
    let run = env.fleet(&[
        "runtime",
        "browse",
        "mux/box",
        dir.to_str().unwrap(),
        "--json",
    ]);
    run.ok();
    assert_eq!(
        run.json(),
        json!({
            "dir": dir.display().to_string(),
            "entries": [
                {"name": ".hidden", "repo": false},
                {"name": "plain", "repo": false},
                {"name": "repo", "repo": true},
                {"name": "with space", "repo": false},
            ],
        })
    );
    fs::create_dir_all(p.home.join("on-the-place")).expect("place dir");
    let run = env.fleet(&["runtime", "browse", "mux/box", "--json"]);
    run.ok();
    let home = run.json();
    assert_eq!(home["dir"], p.home.display().to_string(), "{home}");
    assert!(
        home["entries"]
            .as_array()
            .is_some_and(|e| e.contains(&json!({"name": "on-the-place", "repo": false}))),
        "{home}"
    );

    let run = env.fleet(&[
        "runtime",
        "branches",
        "mux/box",
        repo.to_str().unwrap(),
        "--json",
    ]);
    run.ok();
    assert_eq!(
        run.json(),
        json!({"names": ["feat", "main"], "default": "main"})
    );
    env.fleet(&[
        "runtime",
        "browse",
        "mux/box",
        dir.join("absent").to_str().unwrap(),
    ])
    .code(1)
    .stderr_has("absent");
    env.fleet(&[
        "runtime",
        "branches",
        "mux/box",
        dir.join("plain").to_str().unwrap(),
    ])
    .code(1)
    .stderr_has("no git repository");
}

#[test]
fn attach_and_shell_view_a_pane_on_an_ssh_place() {
    use support::view::View;
    use term_stream::{Down, Size, Up};
    let Some(p) = Placed::new(
        "ssh-attach",
        "#!/bin/sh\necho ready-marker\nwhile IFS= read -r l; do echo \"pong-$l\"; done\n",
    ) else {
        return;
    };
    let env = &p.env;
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["host"] = json!("mux/box");
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();
    reconcile_until(env, "running on the place", |env| {
        env.get("t1")["status"]["sessionState"] == "Running"
    });

    let answer = env.fleet(&["attach", "t1", "--json"]).ok().json();
    let argv: Vec<String> = serde_json::from_value(answer["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer:#}"));
    let mut view = View::argv(&argv);
    assert!(matches!(view.next(), Down::Hello(_)), "no hello first");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until("a snapshot", |f| matches!(f, Down::Snapshot(_)));
    view.until_text("ready-marker");
    view.send(&Up::Input(b"ping\r".to_vec()));
    view.until_text("pong-ping");
    view.send(&Up::Detach);
    assert!(view.close().success());
    assert!(p.sockets.tmux(&["list-panes", "-a"]).stdout.is_empty());

    let answer = env.fleet(&["shell", "t1", "--json"]).ok().json();
    let argv: Vec<String> = serde_json::from_value(answer["stream"]["argv"].clone())
        .unwrap_or_else(|e| panic!("a stream ({e}): {answer:#}"));
    let mut view = View::argv(&argv);
    assert!(matches!(view.next(), Down::Hello(_)), "no hello first");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until("a snapshot", |f| matches!(f, Down::Snapshot(_)));
    view.send(&Up::Input(b"echo \"$HOME\" > shell-home\r".to_vec()));
    let out = work.join("shell-home");
    let deadline = Instant::now() + Duration::from_secs(20);
    while fs::read_to_string(&out).unwrap_or_default().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    view.send(&Up::Detach);
    assert!(view.close().success());
    assert_eq!(
        fs::read_to_string(&out).unwrap_or_default().trim(),
        p.home.display().to_string(),
        "the shell runs on the place, in the Task's directory"
    );
}

#[test]
fn a_sessions_environment_is_in_no_argv_here_or_on_an_ssh_place() {
    let (Some(program), Some(real_tmux)) = (runtime_mux(), tmux()) else {
        return;
    };
    const SECRET: &str = "s3cr3t-env-value-4b1f 'quoted' $HOME";
    let sockets = Sockets::new("argv-local");
    let remote = Sockets::new("argv-place");
    let mut env = mux_env("runtime-mux-argv", &program, &sockets);
    let home = env.root.join("place-home");
    let Some(sshd) = Sshd::start(&env.root.join("sshd"), &home, &remote) else {
        return;
    };
    let log = env.root.join("argv.log");
    let shims = env.root.join("shims");
    fs::create_dir_all(&shims).expect("shims");
    let real_ssh = ["/usr/bin/ssh", "/bin/ssh"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .expect("ssh, since sshd started");
    use std::os::unix::fs::PermissionsExt;
    for (name, real) in [("tmux", &real_tmux), ("ssh", &real_ssh)] {
        let shim = shims.join(name);
        fs::write(
            &shim,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done >> '{}'\nexec '{}' \"$@\"\n",
                log.display(),
                real.display()
            ),
        )
        .expect("shim");
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let bin = env.root.join("agent-bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(
        &claude,
        "#!/bin/sh\nenv > \"$PWD/env.log\"\nexec sleep 300\n",
    )
    .expect("agent");
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let cfg = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        format!(
            "{cfg}\n[runtimes.mux.places.local]\n{}",
            sshd.place("box", &[&shims, &bin])
        ),
    )
    .expect("config");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        shims.display(),
        bin.display()
    )));
    let mut tasks = Vec::new();
    let mut logs = Vec::new();
    for (name, host) in [("here", "local"), ("there", "mux/box")] {
        let work = env.root.join("work").join(name);
        fs::create_dir_all(&work).expect("workspace");
        let mut task = support::task(name, true);
        task["spec"]["host"] = json!(host);
        task["spec"]["env"] = json!({"FLEET_SECRET": SECRET});
        task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
        tasks.push(task);
        logs.push(work.join("env.log"));
    }
    env.apply(json!(tasks)).ok();
    reconcile_until(&env, "both agents started", |_| {
        logs.iter().all(|l| l.is_file())
    });
    for l in &logs {
        let seen = fs::read_to_string(l).expect("the agent's environment");
        assert!(
            seen.lines()
                .any(|line| line == format!("FLEET_SECRET={SECRET}")),
            "{}: {seen}",
            l.display()
        );
    }
    let argv = fs::read_to_string(&log).expect("argv log");
    assert!(
        argv.contains("source-file"),
        "the window was made through the shim, its words on stdin: {argv}"
    );
    assert!(!argv.contains("s3cr3t"), "{argv}");
    let ps = Command::new("ps")
        .args(["-eo", "args"])
        .output()
        .expect("ps");
    assert!(!String::from_utf8_lossy(&ps.stdout).contains("s3cr3t"));
}

struct Up(std::process::Child);

impl Drop for Up {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn watch_until(env: &Env, name: &str, what: &str, within: Duration, done: impl Fn(&Value) -> bool) {
    let deadline = Instant::now() + within;
    loop {
        let task = env.get(name);
        if done(&task) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: {:#}", task["status"]);
        std::thread::sleep(Duration::from_millis(100));
    }
}

const BOOTING_CLAUDE: &str = r#"#!/bin/sh
sleep 1
fleet signal idle
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  fleet signal prompt "{\"prompt\":\"$line\",\"session_id\":\"conv-1\"}"
  fleet signal idle
done
"#;

#[test]
fn a_task_dispatched_beside_running_tasks_is_briefed_and_attachable_about_as_soon_as_one_alone() {
    const RUNNING: usize = 12;
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("latency");
    let mut env = mux_env("runtime-mux-latency", &program, &sockets);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, BOOTING_CLAUDE).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let task = |name: &str| {
        let work = env.root.join("work").join(name);
        fs::create_dir_all(&work).expect("workspace");
        let mut task = support::task(name, true);
        task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
        task
    };
    let _up = Up(env.spawn(&["up"]));
    let briefed_and_attachable = |name: &str| {
        let dispatched = Instant::now();
        env.apply(json!([task(name)])).ok();
        watch_until(&env, name, "briefed", Duration::from_secs(20), |t| {
            t["status"]["phase"] == "Briefed"
        });
        env.fleet(&["attach", name, "--json"]).ok();
        dispatched.elapsed()
    };
    let alone = briefed_and_attachable("alone");
    let running: Vec<Value> = (0..RUNNING).map(|i| task(&format!("bg{i}"))).collect();
    env.apply(json!(running)).ok();
    let all_briefed = Instant::now() + Duration::from_secs(180);
    for i in 0..RUNNING {
        watch_until(
            &env,
            &format!("bg{i}"),
            "running Tasks briefed",
            all_briefed.saturating_duration_since(Instant::now()),
            |t| t["status"]["phase"] == "Briefed",
        );
    }

    let beside = briefed_and_attachable("t1");
    assert!(
        beside < alone * 2 + Duration::from_secs(1),
        "dispatch to a briefed, attachable session took {beside:?} beside {RUNNING} running Tasks, {alone:?} alone: {:#}",
        env.get("t1")["status"]["conditions"]
    );
}

#[test]
fn runtime_mux_and_workspace_git_export_to_the_collector_fleets_config_file_names() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("otel-file");
    let collector = support::otlp::Collector::start();
    let mut env = mux_env("runtime-mux-otel-file", &program, &sockets);
    env.set_var("RUNTIME_MUX_CONFIG", &mux_config(&env).to_string_lossy());
    let mut text = fs::read_to_string(&env.config).expect("config");
    text.push_str(&format!(
        "\n[telemetry]\nendpoint = \"{}\"\nresource = {{ \"service.namespace\" = \"fleet-e2e\", \"deployment.environment.name\" = \"e2e\" }}\n",
        collector.endpoint
    ));
    fs::write(&env.config, text).expect("config");
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let harness = bin.join("claude");
    fs::write(&harness, "#!/bin/sh\nexec sleep 300\n").expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", bin.display())));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);

    env.apply(json!([task])).ok();
    env.reconcile().ok();

    let spans = collector.spans();
    for (service, name) in [
        ("runtime-mux", "runtime-mux start"),
        ("runtime-mux", "runtime-mux workspace"),
        ("workspace-git", "workspace-git make"),
    ] {
        let span = spans
            .iter()
            .find(|s| s.resource["service.name"] == service && s.name == name)
            .unwrap_or_else(|| panic!("no `{name}` span from {service} in {spans:#?}"));
        assert_eq!(span.resource["service.namespace"], "fleet-e2e", "{span:#?}");
        assert_eq!(
            span.resource["deployment.environment.name"], "e2e",
            "{span:#?}"
        );
    }
    for span in &spans {
        assert_eq!(span.resource["service.namespace"], "fleet-e2e", "{span:#?}");
    }
}

#[cfg(target_os = "linux")]
struct PrivateUserManager {
    child: std::process::Child,
    dir: PathBuf,
    cgroup: Option<PathBuf>,
}

#[cfg(target_os = "linux")]
impl PrivateUserManager {
    const TARGET: &str = "fleet-e2e.target";

    fn start(home: &Path, vars: &[(&str, &Path)]) -> Option<PrivateUserManager> {
        let session_manager = std::env::var_os("XDG_RUNTIME_DIR")
            .map(|d| PathBuf::from(d).join("systemd/private"))
            .filter(|p| p.exists());
        let systemd = ["/usr/lib/systemd/systemd", "/lib/systemd/systemd"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file());
        let (Some(_), Some(systemd)) = (session_manager, systemd) else {
            eprintln!("skipping: no systemd user manager to delegate a scope from");
            return None;
        };
        let units = home.join(".config/systemd/user");
        fs::create_dir_all(&units).expect("unit dir");
        fs::write(
            units.join(Self::TARGET),
            "[Unit]\nDescription=fleet e2e\nDefaultDependencies=no\n",
        )
        .expect("target");
        let dir = PathBuf::from(format!("/tmp/fleet-um-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let runtime = dir.join("rt");
        fs::create_dir_all(&runtime).expect("runtime dir");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).expect("chmod");
        let mut assignments = vec![
            format!("HOME={}", home.display()),
            format!("XDG_CONFIG_HOME={}", home.join(".config").display()),
            format!("XDG_DATA_HOME={}", home.join(".local/share").display()),
            format!("XDG_STATE_HOME={}", home.join(".local/state").display()),
            format!("XDG_RUNTIME_DIR={}", runtime.display()),
            format!("PATH={}", std::env::var("PATH").unwrap_or_default()),
        ];
        assignments.extend(vars.iter().map(|(k, v)| format!("{k}={}", v.display())));
        let child = Command::new("systemd-run")
            .args([
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "-p",
                "Delegate=yes",
                "--",
            ])
            .arg("env")
            .arg("-i")
            .args(&assignments)
            .arg(&systemd)
            .arg("--user")
            .arg(format!("--unit={}", Self::TARGET))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(child) = child else {
            eprintln!("skipping: systemd-run is not installed");
            let _ = fs::remove_dir_all(&dir);
            return None;
        };
        let mut manager = PrivateUserManager {
            child,
            dir,
            cgroup: None,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let state = manager.systemctl(&["is-system-running"]);
            if matches!(state.trim(), "running" | "degraded") {
                manager.cgroup = fs::read_to_string(format!("/proc/{}/cgroup", manager.child.id()))
                    .ok()
                    .and_then(|line| {
                        line.trim()
                            .strip_prefix("0::/")
                            .map(|path| Path::new("/sys/fs/cgroup").join(path))
                    })
                    .filter(|init| init.ends_with("init.scope"))
                    .and_then(|init| init.parent().map(Path::to_path_buf));
                return Some(manager);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("skipping: a private systemd --user did not come up");
        None
    }

    fn runtime_dir(&self) -> PathBuf {
        self.dir.join("rt")
    }

    fn systemctl(&self, args: &[&str]) -> String {
        Command::new("systemctl")
            .env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .arg("--user")
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    fn cgroup_empty(cgroup: &Path) -> bool {
        fs::read_to_string(cgroup.join("cgroup.events"))
            .map(|events| events.lines().any(|l| l == "populated 0"))
            .unwrap_or(true)
    }
}

#[cfg(target_os = "linux")]
impl Drop for PrivateUserManager {
    fn drop(&mut self) {
        let _ = self.systemctl(&["exit"]);
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && matches!(self.child.try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Some(cgroup) = &self.cgroup {
            let _ = fs::write(cgroup.join("cgroup.kill"), "1");
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new("chmod")
            .args(["-R", "u+rwx"])
            .arg(&self.dir)
            .output();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_stopped_service_leaves_no_manager_process_but_keeps_the_tmux_server_and_sessions() {
    let (Some(program), Some(_)) = (runtime_mux(), tmux()) else {
        return;
    };
    let sockets = Sockets::new("svc-scope");
    let mut env = mux_env("runtime-service-scope", &program, &sockets);
    let lingering = env.root.join("lingering-runtime.sh");
    fs::write(
        &lingering,
        format!(
            "#!/bin/sh\n(sleep 600 </dev/null >/dev/null 2>&1 &)\nexec '{}' \"$@\"\n",
            program.display()
        ),
    )
    .expect("runtime wrapper");
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&lingering, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let config = fs::read_to_string(&env.config).expect("config");
    fs::write(
        &env.config,
        config.replace(&*program.to_string_lossy(), &lingering.to_string_lossy()),
    )
    .expect("config");
    let Some(manager) = PrivateUserManager::start(&env.root, &[("TMUX_TMPDIR", &sockets.0)]) else {
        return;
    };
    env.set_var("XDG_RUNTIME_DIR", &manager.runtime_dir().to_string_lossy());
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let harness = bin.join("claude");
    fs::write(&harness, "#!/bin/sh\nexec sleep 300\n").expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!("{}:{path}", bin.display())));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");

    env.fleet(&["service", "install", "--now"]).ok();
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    let pane_in_work = || {
        String::from_utf8_lossy(
            &sockets
                .tmux(&["list-panes", "-a", "-F", "#{pane_current_path}"])
                .stdout,
        )
        .lines()
        .any(|l| l == work.to_string_lossy())
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while !pane_in_work() {
        assert!(
            Instant::now() < deadline,
            "no session started: {}\n{:#}",
            manager.systemctl(&["status", "fleet.service", "--no-pager"]),
            env.get("t1")["status"]
        );
        std::thread::sleep(Duration::from_millis(300));
    }
    let server = String::from_utf8_lossy(&sockets.tmux(&["display", "-p", "#{pid}"]).stdout)
        .trim()
        .to_string();
    let unit_cgroup = manager
        .systemctl(&["show", "-p", "ControlGroup", "--value", "fleet.service"])
        .trim()
        .to_string();
    assert!(!unit_cgroup.is_empty(), "the unit runs");
    let server_cgroup = fs::read_to_string(format!("/proc/{server}/cgroup")).expect("cgroup");
    assert!(
        !server_cgroup.contains(&format!("{unit_cgroup}/"))
            && !server_cgroup.trim_end().ends_with(&unit_cgroup),
        "the tmux server runs in the unit's cgroup {unit_cgroup}: {server_cgroup}"
    );

    env.fleet(&["service", "stop"]).ok();

    assert!(
        Path::new(&format!("/proc/{server}")).exists(),
        "the stop took the tmux server"
    );
    assert!(pane_in_work(), "the stop took the session");
    let cgroup = Path::new("/sys/fs/cgroup").join(unit_cgroup.trim_start_matches('/'));
    assert!(
        PrivateUserManager::cgroup_empty(&cgroup),
        "a manager process outlived the stop: {:?}",
        fs::read_to_string(cgroup.join("cgroup.procs"))
    );
}

fn runtime_herdr() -> Option<PathBuf> {
    let found = std::env::var_os("RUNTIME_HERDR_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            Path::new(env!("CARGO_BIN_EXE_fleet"))
                .parent()
                .map(|dir| dir.join("runtime-herdr"))
        })
        .filter(|p| p.is_file());
    if found.is_none() {
        missing("no runtime-herdr binary; build it with `cargo build -p runtime-herdr`");
    }
    found
}

fn herdr_core() -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("herdr-core"))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        missing("herdr-core is not installed");
    }
    found
}

struct HerdrHome(PathBuf);

impl HerdrHome {
    fn new(name: &str) -> HerdrHome {
        HerdrHome(short_private_dir(&format!("hc-{name}")))
    }

    fn vars(&self) -> [(&'static str, PathBuf); 5] {
        [
            ("HOME", self.0.clone()),
            ("XDG_RUNTIME_DIR", self.0.clone()),
            ("XDG_STATE_HOME", self.0.join("state")),
            ("XDG_CONFIG_HOME", self.0.join("config")),
            ("XDG_DATA_HOME", self.0.join("data")),
        ]
    }

    fn hc(&self, args: &[&str]) -> std::process::Output {
        Command::new("herdr-core")
            .env_remove("HC_SESSION")
            .env_remove("HC_SOCKET")
            .envs(self.vars())
            .args(args)
            .output()
            .expect("herdr-core")
    }
}

impl Drop for HerdrHome {
    fn drop(&mut self) {
        let _ = self.hc(&["server", "stop"]);
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn herdr_env(name: &str, program: &Path, home: &HerdrHome) -> Env {
    let mut env = Env::new(name);
    fs::write(
        &env.config,
        format!(
            "version = 1\n\n[data]\ndir = \"data\"\n\n[runtimes.herdr]\nprogram = '{}'\ncallTimeoutMs = 10000\n",
            program.display()
        ),
    )
    .expect("config");
    for (name, value) in home.vars() {
        if name != "HOME" {
            env.set_var(name, &value.to_string_lossy());
        }
    }
    env
}

#[test]
fn runtime_herdr_passes_the_conformance_suite() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("check");
    let env = herdr_env("runtime-check-herdr", &program, &home);
    let run = env.fleet(&["runtime", "check", "--", program.to_str().unwrap()]);
    run.ok()
        .stdout_has("ok   two starts with one key leave one process")
        .stdout_has("ok   concurrent starts with one key leave one process")
        .stdout_has("ok   a protocol mismatch answers Unknown")
        .stdout_has("ok   an error answers Unknown, never None")
        .stdout_has("ok   a stop of an absent process succeeds")
        .stdout_has("ok   a stop ends the process")
        .stdout_has("ok   an exited process answers Exited with its code, or None")
        .stdout_has("ok   workspace: idempotent by key")
        .stdout_has("ok   workspace: release never removes what it did not make")
        .stdout_has("ok   manifest reports each place it was asked about")
        .stdout_has("ok   a version 2 request without a place answers Unknown")
        .stdout_has("ok   shell: a key no process holds answers none")
        .stdout_has("ok   shell: a running key answers how to open its shell, on each call")
        .stdout_has("ok   shell: a stop leaves nothing to open")
        .stdout_has("ok   pause: a key no process holds answers none")
        .stdout_has("ok   pause: stops the process and keeps the key until a start or a stop")
        .stdout_has("speaks runtime protocol 2");
    assert!(!run.stdout.contains("FAIL"), "{}", run.stdout);
}

fn herdr_call(program: &Path, home: &HerdrHome, verb: &str, fields: Value) -> Value {
    herdr_call_at(program, home, "local", verb, fields)
}

fn herdr_call_at(
    program: &Path,
    home: &HerdrHome,
    place: &str,
    verb: &str,
    fields: Value,
) -> Value {
    use std::io::Write;
    let mut request = json!({"protocol": 2, "place": {"name": place, "config": {}}});
    request
        .as_object_mut()
        .expect("an object")
        .extend(fields.as_object().expect("an object").clone());
    let mut child = Command::new(program)
        .arg(verb)
        .env_remove("HC_SESSION")
        .env_remove("HC_SOCKET")
        .envs(home.vars())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("runtime-herdr");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(request.to_string().as_bytes())
        .expect("request");
    let out = child.wait_with_output().expect("answer");
    serde_json::from_slice(&out.stdout).expect("a JSON answer")
}

fn herdr_inspect_until(
    program: &Path,
    home: &HerdrHome,
    key: &str,
    done: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let answer = herdr_call(program, home, "inspect", json!({"key": key}));
        if done(&answer) || Instant::now() >= deadline {
            return answer;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn herdr_start(program: &Path, home: &HerdrHome, key: &str, argv: &[&str]) -> Value {
    herdr_call(
        program,
        home,
        "start",
        json!({"key": key, "argv": argv, "env": {"FLEET_PROBE": "probe"}}),
    )
}

#[test]
fn runtime_herdr_reports_an_exited_process_with_its_code() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("exit");
    let started = herdr_start(&program, &home, "k-exit", &["sh", "-c", "exit 7"]);
    assert!(started["known"]["started"]["id"].is_string(), "{started}");
    let inspected = herdr_inspect_until(&program, &home, "k-exit", |a| {
        a["known"].get("exited").is_some()
    });
    assert_eq!(inspected["known"]["exited"]["code"], 7, "{inspected}");
    let stopped = herdr_call(&program, &home, "stop", json!({"key": "k-exit"}));
    assert_eq!(stopped["known"], "stopped", "{stopped}");
    let after = herdr_call(&program, &home, "inspect", json!({"key": "k-exit"}));
    assert_eq!(after["known"], "none", "{after}");
}

#[test]
fn runtime_herdr_keeps_a_key_running_across_a_core_restart() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("restart");
    let first = herdr_start(&program, &home, "k-restart", &["sleep", "300"]);
    let id = first["known"]["started"]["id"].clone();
    assert!(id.is_string(), "{first}");
    let restarted = home.hc(&["server", "restart"]);
    assert!(restarted.status.success(), "{restarted:?}");
    let running = herdr_call(&program, &home, "inspect", json!({"key": "k-restart"}));
    assert_eq!(running["known"]["running"]["id"], id, "{running}");
    let again = herdr_start(&program, &home, "k-restart", &["sleep", "300"]);
    assert_eq!(again["known"]["started"]["adopted"], true, "{again}");
    assert_eq!(again["known"]["started"]["id"], id, "{again}");
}

#[test]
fn runtime_herdr_opens_a_key_s_shell_in_its_agent_s_space() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("shellspace");
    let started = herdr_start(&program, &home, "k-shellspace", &["sleep", "300"]);
    let agent = started["known"]["started"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{started}"))
        .to_string();
    let relabelled = home.hc(&["label", &agent, "herdr.space=fleet"]);
    assert!(relabelled.status.success(), "{relabelled:?}");
    let shell = herdr_call(&program, &home, "shell", json!({"key": "k-shellspace"}));
    let address = shell["known"]["stream"]["argv"]
        .as_array()
        .and_then(|argv| argv.last())
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{shell}"))
        .to_string();
    let got = home.hc(&["get", &address, "--json"]);
    let terminal: Value = serde_json::from_slice(&got.stdout).expect("a JSON terminal");
    assert_eq!(terminal["labels"]["fleet.role"], "shell", "{terminal}");
    assert_eq!(terminal["labels"]["herdr.space"], "fleet", "{terminal}");
}

#[test]
fn runtime_herdr_groups_a_place_s_keys_in_the_space_it_names() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("grouped");
    let started = herdr_call(
        &program,
        &home,
        "start",
        json!({
            "place": {"name": "local", "config": {"space": "agents"}},
            "key": "k-grouped",
            "argv": ["sleep", "300"],
        }),
    );
    let id = started["known"]["started"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{started}"))
        .to_string();
    let got = home.hc(&["get", &id, "--json"]);
    let terminal: Value = serde_json::from_slice(&got.stdout).expect("a JSON terminal");
    assert_eq!(terminal["labels"]["herdr.space"], "agents", "{terminal}");
    assert_eq!(terminal["labels"]["herdr.tab"], "k-grouped", "{terminal}");
}

#[test]
fn runtime_herdr_reads_a_key_its_core_lost_as_exited() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("lost");
    let first = herdr_start(&program, &home, "k-lost", &["sleep", "300"]);
    assert_eq!(first["known"]["started"]["adopted"], false, "{first}");
    let stopped = home.hc(&["server", "stop"]);
    assert!(stopped.status.success(), "{stopped:?}");
    let lost = herdr_inspect_until(&program, &home, "k-lost", |a| {
        a["known"].get("exited").is_some()
    });
    assert!(lost["known"]["exited"].is_object(), "{lost}");
    let revived = herdr_start(&program, &home, "k-lost", &["sleep", "300"]);
    assert_eq!(revived["known"]["started"]["adopted"], false, "{revived}");
    let running = herdr_call(&program, &home, "inspect", json!({"key": "k-lost"}));
    assert!(running["known"]["running"].is_object(), "{running}");
}

#[test]
fn runtime_herdr_labels_a_key_s_terminal_for_herdr_s_app() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("labels");
    let started = herdr_start(
        &program,
        &home,
        "k-labels",
        &["sh", "-c", "echo probe=$FLEET_PROBE; sleep 300"],
    );
    let id = started["known"]["started"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{started}"))
        .to_string();
    let got = home.hc(&["get", &id, "--json"]);
    let terminal: Value = serde_json::from_slice(&got.stdout).expect("a JSON terminal");
    assert_eq!(
        terminal["labels"],
        json!({
            "fleet.key": "k-labels",
            "fleet.place": "local",
            "herdr.space": "k-labels",
            "herdr.tab": "k-labels"
        }),
        "{terminal}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let screen = String::from_utf8_lossy(&home.hc(&["read", &id]).stdout).into_owned();
        if screen.contains("probe=probe") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the environment never reached it: {screen}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn herdr_keys(program: &Path, home: &HerdrHome, place: &str) -> Value {
    herdr_call_at(program, home, place, "list", json!({}))["known"]["keys"].clone()
}

#[test]
fn runtime_herdr_lists_a_place_s_own_keys() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("places");
    let started = herdr_call_at(
        &program,
        &home,
        "here",
        "start",
        json!({"key": "k-here", "argv": ["sleep", "300"], "env": {}}),
    );
    assert!(started["known"]["started"].is_object(), "{started}");
    assert_eq!(herdr_keys(&program, &home, "here"), json!(["k-here"]));
    assert_eq!(herdr_keys(&program, &home, "there"), json!([]));
}

#[test]
fn runtime_herdr_adopts_the_terminal_of_an_interrupted_start() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("cut");
    let status = home.hc(&["server", "status", "--json"]);
    let core: Value = serde_json::from_slice(&status.stdout).expect("a JSON status");
    let core_id = core["core"]["id"].as_str().expect("a core id");
    let records = home
        .0
        .join("state/runtime-herdr")
        .join(core_id)
        .join("local");
    fs::create_dir_all(&records).expect("records");
    fs::write(
        records.join("k-cut.start"),
        r#"{"terminal":null,"pid":null}"#,
    )
    .expect("record");
    let ran = home.hc(&[
        "run",
        "--bg",
        "--json",
        "--label",
        "fleet.key=k-cut",
        "--label",
        "fleet.place=local",
        "--",
        "sleep",
        "300",
    ]);
    let terminal: Value = serde_json::from_slice(&ran.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&ran.stdout)));
    let address = terminal["address"].clone();
    assert!(address.is_string(), "{terminal}");
    let running = herdr_call(&program, &home, "inspect", json!({"key": "k-cut"}));
    assert_eq!(running["known"]["running"]["id"], address, "{running}");
    let again = herdr_start(&program, &home, "k-cut", &["sleep", "300"]);
    assert_eq!(again["known"]["started"]["adopted"], true, "{again}");
    assert_eq!(again["known"]["started"]["id"], address, "{again}");
}

#[test]
fn runtime_herdr_lists_a_key_that_holds_only_a_workspace() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("ws");
    let dir = home.0.join("notes");
    fs::create_dir_all(&dir).expect("dir");
    let made = herdr_call(
        &program,
        &home,
        "workspace",
        json!({"key": "k-ws", "urls": [format!("file://{}", dir.display())]}),
    );
    assert!(made["known"]["made"].is_object(), "{made}");
    assert_eq!(herdr_keys(&program, &home, "local"), json!(["k-ws"]));
}

#[test]
fn runtime_herdr_keeps_each_place_s_terminals_apart() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("apart");
    let start_at = |place: &str| {
        herdr_call_at(
            &program,
            &home,
            place,
            "start",
            json!({"key": "k-both", "argv": ["sleep", "300"], "env": {}}),
        )
    };
    let here = start_at("here");
    let there = start_at("there");
    assert_eq!(there["known"]["started"]["adopted"], false, "{there}");
    assert_ne!(
        here["known"]["started"]["id"], there["known"]["started"]["id"],
        "{here} {there}"
    );
    let stopped = herdr_call_at(&program, &home, "there", "stop", json!({"key": "k-both"}));
    assert_eq!(stopped["known"], "stopped", "{stopped}");
    let still = herdr_call_at(&program, &home, "here", "inspect", json!({"key": "k-both"}));
    assert_eq!(
        still["known"]["running"]["id"], here["known"]["started"]["id"],
        "{still}"
    );
}

#[test]
fn runtime_herdr_keeps_each_place_s_workspaces_apart() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("wsapart");
    let repo = home.0.join("repo");
    fs::create_dir_all(&repo).expect("repo");
    let git = |args: &[&str]| {
        let mut cmd = Command::new("git");
        for var in GIT_LOCATION {
            cmd.env_remove(var);
        }
        let out = cmd.arg("-C").arg(&repo).args(args).output().expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "first",
    ]);
    let made = herdr_call_at(
        &program,
        &home,
        "here",
        "workspace",
        json!({"key": "k-ws", "urls": [format!("git+file://{}", repo.display())]}),
    );
    let dir = made["known"]["made"]["dirs"][0]
        .as_str()
        .unwrap_or_else(|| panic!("{made}"))
        .to_string();
    assert_eq!(herdr_keys(&program, &home, "here"), json!(["k-ws"]));
    assert_eq!(herdr_keys(&program, &home, "there"), json!([]));
    let released = herdr_call_at(
        &program,
        &home,
        "there",
        "stop",
        json!({"key": "k-ws", "release": true}),
    );
    assert_eq!(released["known"], "stopped", "{released}");
    assert!(Path::new(&dir).is_dir(), "another place released {dir}");
}

#[test]
fn runtime_herdr_types_into_a_key_and_reads_its_screen() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("input");
    let echo = "while IFS= read -r l; do printf 'got[%s]\\n' \"$l\"; done";
    herdr_start(&program, &home, "k-input", &["sh", "-c", echo]);
    let sent = herdr_call(
        &program,
        &home,
        "input",
        json!({"key": "k-input", "text": "hello herdr", "submit": true}),
    );
    assert_eq!(sent["known"], "sent", "{sent}");
    let deadline = Instant::now() + Duration::from_secs(10);
    let screen = loop {
        let captured = herdr_call(&program, &home, "capture", json!({"key": "k-input"}));
        let text = captured["known"]["screen"]["text"]
            .as_str()
            .unwrap_or_default();
        if text.contains("got[hello herdr]") || Instant::now() >= deadline {
            break captured;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        screen["known"]["screen"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("got[hello herdr]")),
        "{screen}"
    );
    for verb in ["input", "capture"] {
        let absent = herdr_call(
            &program,
            &home,
            verb,
            json!({"key": "k-absent", "text": "x", "submit": true}),
        );
        assert_eq!(absent["known"], "none", "{verb}: {absent}");
    }
}

#[test]
fn runtime_herdr_attaches_through_its_own_stream() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("attach");
    let started = herdr_start(&program, &home, "k-attach", &["sleep", "300"]);
    let id = started["known"]["started"]["id"].clone();
    let attach = herdr_call(&program, &home, "attach", json!({"key": "k-attach"}));
    let argv = attach["known"]["stream"]["argv"]
        .as_array()
        .unwrap_or_else(|| panic!("{attach}"))
        .clone();
    assert_eq!(argv[argv.len() - 1], id, "{attach}");
    let absent = herdr_call(&program, &home, "attach", json!({"key": "k-absent"}));
    assert_eq!(absent["known"], "none", "{absent}");
}

#[test]
fn runtime_herdr_runs_a_bounded_command() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("exec");
    let ran = herdr_call(
        &program,
        &home,
        "exec",
        json!({"argv": ["sh", "-c", "echo out; echo err >&2; exit 3"], "in": "host", "timeoutMs": 5000, "outputCap": 1024}),
    );
    assert_eq!(
        ran["known"],
        json!({"exited": {"code": 3, "stdout": "out\n", "stderr": "err\n"}}),
        "{ran}"
    );
    let slow = herdr_call(
        &program,
        &home,
        "exec",
        json!({"argv": ["sleep", "30"], "in": "host", "timeoutMs": 300, "outputCap": 1024}),
    );
    assert!(slow["known"]["timedOut"].is_object(), "{slow}");
}

#[test]
fn a_task_on_runtime_herdr_is_briefed_and_revives_on_its_conversation() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("task");
    let mut env = herdr_env("runtime-herdr-task", &program, &home);
    let bin = env.root.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let claude = bin.join("claude");
    fs::write(&claude, CLAUDE).expect("harness");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fleet_dir = Path::new(env!("CARGO_BIN_EXE_fleet")).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    env.only_on_path(Path::new(&format!(
        "{}:{}:{path}",
        bin.display(),
        fleet_dir.display()
    )));
    let work = env.root.join("work").join("t1");
    fs::create_dir_all(&work).expect("workspace");
    let mut task = support::task("t1", true);
    task["spec"]["workspace"] = json!([format!("file://{}", work.display())]);
    env.apply(json!([task])).ok();

    reconcile_until(&env, "briefed on the prompt hook's proof", |env| {
        env.get("t1")["status"]["phase"] == "Briefed"
    });
    reconcile_until(&env, "idle, as its hooks report", |env| {
        env.get("t1")["status"]["sessionState"] == "Idle"
    });
    assert_eq!(env.get("t1")["status"]["conversation"], "conv-1");

    let id = env.get("t1")["status"]["session"]["id"].clone();
    let listed = home.hc(&["ls", "--json"]);
    let listed: Value = serde_json::from_slice(&listed.stdout).expect("a JSON list");
    let terminal = listed["terminals"]
        .as_array()
        .and_then(|ts| {
            ts.iter()
                .find(|t| t["cwd"] == work.to_string_lossy().as_ref())
        })
        .and_then(|t| t["address"].as_str())
        .unwrap_or_else(|| panic!("no terminal for t1: {listed}"))
        .to_string();
    let killed = home.hc(&["kill", &terminal]);
    assert!(killed.status.success(), "{killed:?}");
    reconcile_until(&env, "revived", |_| starts(&work).len() >= 2);
    let second = &starts(&work)[1];
    assert_eq!(
        &second[second.len() - 2..],
        ["--resume", "conv-1"],
        "{second:?}"
    );
    reconcile_until(&env, "the terminal back", |env| {
        support::condition(&env.get("t1")["status"], "SessionLost")["status"] == "False"
    });
    let status = env.get("t1")["status"].clone();
    assert_eq!(status["session"]["id"], id, "{status:#}");
}

#[test]
fn runtime_herdr_exec_ends_what_a_command_left_running() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("execbg");
    let marker = format!("{}.{}", 301, std::process::id());
    let script = format!("sleep {marker} & echo started");
    let started = Instant::now();
    let ran = herdr_call(
        &program,
        &home,
        "exec",
        json!({"argv": ["sh", "-c", script], "in": "host", "timeoutMs": 10000, "outputCap": 1024}),
    );
    assert_eq!(ran["known"]["exited"]["code"], 0, "{ran}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the answer waited for the background child: {:?}",
        started.elapsed()
    );
    let left = Command::new("pgrep")
        .args(["-f", &format!("sleep {marker}")])
        .output()
        .expect("pgrep");
    assert!(
        String::from_utf8_lossy(&left.stdout).trim().is_empty(),
        "still running: {}",
        String::from_utf8_lossy(&left.stdout)
    );
}

#[test]
fn runtime_herdr_pause_again_closes_a_shell_left_open() {
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("pausecut");
    let started = herdr_start(&program, &home, "k-cut", &["sleep", "300"]);
    let agent = started["known"]["started"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{started}"))
        .to_string();
    let shell = herdr_call(&program, &home, "shell", json!({"key": "k-cut"}));
    assert!(shell["known"].is_object(), "{shell}");
    let status = home.hc(&["server", "status", "--json"]);
    let core: Value = serde_json::from_slice(&status.stdout).expect("a JSON status");
    let records = home
        .0
        .join("state/runtime-herdr")
        .join(core["core"]["id"].as_str().expect("a core id"))
        .join("local");
    fs::write(records.join("k-cut.paused"), "").expect("mark");
    assert!(home.hc(&["kill", &agent]).status.success());
    let live_shells = || {
        let listed = home.hc(&["ls", "--json"]);
        let listed: Value = serde_json::from_slice(&listed.stdout).expect("a JSON list");
        listed["terminals"]
            .as_array()
            .map(|ts| ts.iter().filter(|t| t["state"] == "live").count())
            .unwrap_or(0)
    };
    assert_eq!(live_shells(), 1, "the shell is left open");
    let half = herdr_call(&program, &home, "inspect", json!({"key": "k-cut"}));
    assert!(
        half["unknown"].is_object(),
        "a mark with a shell left: {half}"
    );
    let paused = herdr_call(&program, &home, "pause", json!({"key": "k-cut"}));
    assert_eq!(paused["known"], "paused", "{paused}");
    assert_eq!(live_shells(), 0, "the second pause closed the shell");
}

fn herdr_view(home: &HerdrHome, attach: &Value) -> support::view::View {
    let argv: Vec<String> = attach["known"]["stream"]["argv"]
        .as_array()
        .unwrap_or_else(|| panic!("{attach}"))
        .iter()
        .map(|w| w.as_str().expect("a word").to_string())
        .collect();
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env_remove("HC_SESSION")
        .env_remove("HC_SOCKET")
        .envs(home.vars());
    support::view::View::open(cmd)
}

#[test]
fn runtime_herdr_streams_a_key_s_terminal() {
    use term_stream::{Capability, Down, Size, Up};
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("stream");
    let echo = "printf 'ready\\n'; while IFS= read -r l; do printf 'got[%s]\\n' \"$l\"; done";
    herdr_start(&program, &home, "k-stream", &["sh", "-c", echo]);
    let attach = herdr_call(&program, &home, "attach", json!({"key": "k-stream"}));
    let mut view = herdr_view(&home, &attach);
    let Down::Hello(hello) = view.next() else {
        panic!("the stream did not start with hello")
    };
    assert!(!hello.has(Capability::Snapshot), "{hello:?}");
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.until_text("ready");
    view.send(&Up::Input(b"ping\r".to_vec()));
    view.until_text("got[ping]");
    view.send(&Up::Detach);
    let status = view.child.wait().expect("the stream ends");
    assert!(status.success(), "{status:?}");
    let still = herdr_call(&program, &home, "inspect", json!({"key": "k-stream"}));
    assert!(still["known"]["running"].is_object(), "{still}");
}

#[test]
fn runtime_herdr_stream_reports_the_program_s_exit() {
    use term_stream::{Down, Exit, Size, Up};
    let (Some(program), Some(_)) = (runtime_herdr(), herdr_core()) else {
        return;
    };
    let home = HerdrHome::new("streamexit");
    herdr_start(&program, &home, "k-end", &["sh", "-c", "read x; exit 5"]);
    let attach = herdr_call(&program, &home, "attach", json!({"key": "k-end"}));
    let mut view = herdr_view(&home, &attach);
    view.send(&Up::Resize(Size { rows: 24, cols: 80 }));
    view.send(&Up::Input(b"go\r".to_vec()));
    let exit = view.until("the program's exit", |f| matches!(f, Down::Exit(_)));
    assert_eq!(exit, Down::Exit(Exit::Known { code: Some(5) }));
}
