//! Acceptance tests exercise the actual tooling binary against private files.
use fleet_exec::{Outcome, Spec, run};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
static SEQ: AtomicU32 = AtomicU32::new(0);

/// The variables that point git at another repository than its working
/// directory. A commit hook exports them, and a child keeping them would run
/// its `git config` on this repository instead of its scratch one.
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

/// A child of these tests: `program`, blind to git's location variables.
fn scratch(program: impl Into<std::ffi::OsString>, timeout: Duration) -> Spec {
    let mut s = Spec::new(program, timeout);
    s.env_remove = GIT_LOCATION.iter().map(|v| v.to_string()).collect();
    s
}
struct Env(PathBuf);
impl Env {
    fn new() -> Self {
        let p = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "tooling-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn write(&self, p: &str, text: &str) {
        let p = self.0.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    fn call(&self, args: &[&str], env: &[(&str, &str)]) -> (bool, String) {
        let mut s = scratch(env!("CARGO_BIN_EXE_fleet-dev"), Duration::from_secs(120))
            .args(args.iter().copied());
        s.cwd = Some(self.0.clone());
        s.env = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        match run(&s) {
            Outcome::Exited {
                code,
                stdout,
                stderr,
            } => (
                code == Some(0),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr)
                ),
            ),
            o => panic!("{o:?}"),
        }
    }
    fn git(&self, args: &[&str]) {
        let mut s = scratch("git", Duration::from_secs(10)).args(args.iter().copied());
        s.cwd = Some(self.0.clone());
        s.env = vec![
            (
                "GIT_CONFIG_GLOBAL".into(),
                self.0.join("no-config").to_string_lossy().into(),
            ),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ];
        let o = run(&s);
        assert!(o.success_text().is_some(), "{o:?}");
    }
    fn repo(&self) {
        self.git(&["init", "-b", "main"]);
        self.git(&["config", "user.name", "Fixture"]);
        self.git(&["config", "user.email", "fixture@example.invalid"]);
        self.git(&["config", "commit.gpgsign", "false"]);
        // The release decision is cocogitto's, read from the repository's own
        // configuration.
        let cog = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cog.toml");
        self.write("cog.toml", &fs::read_to_string(cog).unwrap());
        self.write("crates/demo.rs", "pub fn shipped() {}\n");
        self.commit("feat: first release");
        self.write(
            ".github/release-readiness.toml",
            "ready = false\nreason = 'integration pending'\n",
        );
    }
    fn commit(&self, title: &str) {
        self.git(&["add", "."]);
        self.git(&["commit", "-m", title]);
    }
}
impl Drop for Env {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

const CI: &str = "on: {pull_request: {branches: [main]}}\njobs:\n  rust:\n    strategy: {matrix: {os: [ubuntu-latest, ubuntu-26.04-arm]}}\n    runs-on: ${{ matrix.os }}\n    timeout-minutes: 30\n    steps: [{run: 'cargo xtask check'}]\n  all-checks:\n    if: always()\n    needs: [rust]\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    steps: [{run: 'cargo xtask verdict'}]\n";

#[test]
fn workflow_and_verdict_fail_closed_on_mutated_jobs() {
    let e = Env::new();
    e.write(".github/workflows/ci.yml", CI);
    assert!(e.call(&["workflow"], &[]).0);
    assert!(
        e.call(
            &["verdict"],
            &[("NEEDS", r#"{"rust":{"result":"success"}}"#)]
        )
        .0
    );
    for result in ["skipped", "cancelled", "failure", ""] {
        let n = json!({"rust":{"result":result}}).to_string();
        let (ok, out) = e.call(&["verdict"], &[("NEEDS", &n)]);
        assert!(!ok && out.contains("rust"), "{out}");
    }
    let (ok, out) = e.call(&["verdict"], &[("NEEDS", "{}")]);
    assert!(!ok && out.contains("missing"), "{out}");
    for (from, to, why) in [
        ("if: always()", "if: success()", "always"),
        ("needs: [rust]", "needs: []", "needs"),
        ("timeout-minutes: 30", "name: No timeout", "timeout"),
        (
            "cargo xtask check",
            "cargo xtask check rust-fmt",
            "rust-test",
        ),
        ("ubuntu-latest, ", "", "ubuntu-latest"),
    ] {
        e.write(".github/workflows/ci.yml", &CI.replace(from, to));
        let (ok, out) = e.call(&["workflow"], &[]);
        assert!(!ok && out.contains(why), "{why}: {out}");
    }
}

#[test]
fn every_workflow_rejects_injection_and_unpinned_actions() {
    let e = Env::new();
    e.write(".github/workflows/ci.yml", CI);
    for expr in [
        "github.event.pull_request.title",
        "inputs['version']",
        "github['event']['pull_request']['title']",
        "github [ 'event' ] [ 'comment' ] [ 'body' ]",
        "iNpUtS.version",
        "github.event.pull_request.body",
        "github.head_ref",
        "github.event.head_commit.message",
        "inputs.version",
        "github.event.comment.body",
        "format('{0}', github.event.pull_request.title)",
    ] {
        e.write(
            ".github/workflows/extra.yml",
            &format!("jobs:\n  extra:\n    steps:\n      - run: echo ${{{{ {expr} }}}}\n"),
        );
        let (ok, out) = e.call(&["workflow"], &[]);
        assert!(!ok && out.contains("interpolation"), "{out}");
    }
    e.write(
        ".github/workflows/extra.yml",
        "jobs:\n  extra:\n    steps: [{uses: 'actions/checkout@main'}]\n",
    );
    let (ok, out) = e.call(&["workflow"], &[]);
    assert!(!ok && out.contains("SHA"), "{out}");
}

#[test]
fn archives_are_reproducible_and_missing_targets_fail_checksums() {
    let e = Env::new();
    e.write("LICENSE", "fixture licence\n");
    e.write("binary", "binary fixture\n");
    let targets = [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "aarch64-apple-darwin",
    ];
    for t in targets {
        let args = [
            "release",
            "package",
            "--version",
            "v1.2.3",
            "--target",
            t,
            "--binary",
            "binary",
            "--out",
            "dist",
        ];
        let (ok, out) = e.call(&args, &[]);
        assert!(ok, "{out}");
        let p = e.0.join(format!("dist/fleet-v1.2.3-{t}.tar.gz"));
        let a = fs::read(&p).unwrap();
        assert!(e.call(&args, &[]).0);
        assert_eq!(a, fs::read(&p).unwrap());
        use std::io::Read;
        let gz = flate2::read::GzDecoder::new(a.as_slice());
        let mut archive = tar::Archive::new(gz);
        let entries = archive
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                let name = entry.path().unwrap().to_string_lossy().into_owned();
                let mode = entry.header().mode().unwrap();
                let mut bytes = String::new();
                entry.read_to_string(&mut bytes).unwrap();
                (name, mode, bytes)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            vec![
                ("fleet".into(), 0o755, "binary fixture\n".into()),
                ("LICENSE".into(), 0o644, "fixture licence\n".into())
            ]
        );
        if t != targets[targets.len() - 1] {
            assert!(
                !e.call(
                    &["release", "checksums", "--version", "v1.2.3", "dist"],
                    &[]
                )
                .0
            );
        }
    }
    let (ok, out) = e.call(
        &["release", "checksums", "--version", "v1.2.3", "dist"],
        &[],
    );
    assert!(ok, "{out}");
    let sums = fs::read_to_string(e.0.join("dist/fleet-v1.2.3-checksums.txt")).unwrap();
    assert_eq!(sums.lines().count(), 3);
}

#[test]
fn windows_is_not_a_release_target_and_no_workflow_runs_on_it() {
    let supported = [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "aarch64-apple-darwin",
    ];
    let e = Env::new();
    e.write("LICENSE", "fixture licence\n");
    e.write("binary", "binary fixture\n");
    let package = |target: &str| {
        e.call(
            &[
                "release",
                "package",
                "--version",
                "v1.2.3",
                "--target",
                target,
                "--binary",
                "binary",
                "--out",
                "dist",
            ],
            &[],
        )
    };
    let (ok, out) = package("x86_64-pc-windows-msvc");
    assert!(!ok && out.contains("unsupported release target"), "{out}");
    for t in supported {
        let (ok, out) = package(t);
        assert!(ok, "{out}");
    }
    // All supported archives are required for a complete release.
    let (ok, out) = e.call(
        &["release", "checksums", "--version", "v1.2.3", "dist"],
        &[],
    );
    assert!(ok, "{out}");
    let sums = fs::read_to_string(e.0.join("dist/fleet-v1.2.3-checksums.txt")).unwrap();
    assert_eq!(sums.lines().count(), supported.len(), "{sums}");
    // A Linux-only CI satisfies the workflow gate.
    e.write(".github/workflows/ci.yml", CI);
    let (ok, out) = e.call(&["workflow"], &[]);
    assert!(ok, "{out}");
    // Fleet's release workflows build exactly the supported targets and run
    // no job on Windows (thurbox's own CI does; Fleet is not tested there).
    let workflows = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workflows");
    let cd: serde_json::Value =
        serde_saphyr::from_str(&fs::read_to_string(workflows.join("cd.yml")).unwrap()).unwrap();
    let targets: Vec<_> = cd["jobs"]["build"]["strategy"]["matrix"]["include"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["target"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(targets, supported);
    let darwin = &cd["jobs"]["build"]["strategy"]["matrix"]["include"][2];
    assert_eq!(darwin["os"], "macos-15");
    let ci: serde_json::Value =
        serde_saphyr::from_str(&fs::read_to_string(workflows.join("ci.yml")).unwrap()).unwrap();
    assert!(
        ci["jobs"]["rust"]["strategy"]["matrix"]["os"]
            .as_array()
            .unwrap()
            .contains(&json!("macos-15"))
    );
    for entry in fs::read_dir(&workflows).unwrap() {
        let path = entry.unwrap().path();
        if !matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy();
        let doc: serde_json::Value =
            serde_saphyr::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for (job, body) in doc["jobs"].as_object().unwrap() {
            let runners = [&body["runs-on"], &body["strategy"]["matrix"]];
            assert!(
                !runners.iter().any(|r| r.to_string().contains("windows")),
                "{name}/{job} runs on Windows"
            );
        }
    }
}

#[test]
fn title_and_suite_validation_are_executable() {
    let e = Env::new();
    for t in ["feat: accept", "fix(cli)!: break"] {
        assert!(e.call(&["release", "title", t, "12"], &[]).0);
    }
    for t in ["oops", "feat: double (#12)", "feat: ", "unknown: hello"] {
        assert!(!e.call(&["release", "title", t, "12"], &[]).0);
    }
    let listing = json!({"rust-suites": {"a":{"binary-name":"architecture","kind":"test","testcases":{"holds":{"ignored":false,"filter-match":{"status":"matches"}}}},"e":{"binary-name":"e2e_demo","kind":"test","testcases":{"runs":{"ignored":false,"filter-match":{"status":"matches"}}}}}});
    e.write("listing.json", &listing.to_string());
    assert!(e.call(&["suites", "listing.json"], &[]).0);
    for field in ["ignored", "filtered"] {
        let mut bad = listing.clone();
        if field == "ignored" {
            bad["rust-suites"]["e"]["testcases"]["runs"]["ignored"] = json!(true);
        } else {
            bad["rust-suites"]["e"]["testcases"]["runs"]["filter-match"]["status"] =
                json!("mismatch");
        }
        e.write("listing.json", &bad.to_string());
        let (ok, out) = e.call(&["suites", "listing.json"], &[]);
        assert!(!ok && out.contains("e2e_"), "{out}");
    }
}

fn installer(e: &Env, args: &[&str]) -> (bool, String) {
    installer_with(e, args, Some("v1.2.3"))
}
fn installer_with(e: &Env, args: &[&str], version: Option<&str>) -> (bool, String) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut s = scratch("sh", Duration::from_secs(30))
        .arg(
            repo.join("scripts/fleet/install-release.sh")
                .into_os_string(),
        )
        .args(args.iter().copied());
    s.cwd = Some(e.0.clone());
    s.env = vec![
        (
            "FLEET_RELEASE_BASE".into(),
            e.0.join("releases").to_string_lossy().into(),
        ),
        (
            "FLEET_INSTALL_DIR".into(),
            e.0.join("bin").to_string_lossy().into(),
        ),
        ("TMPDIR".into(), e.0.to_string_lossy().into()),
        ("TEMP".into(), e.0.to_string_lossy().into()),
    ];
    if e.0.join("tools/uname").exists() {
        s.env.push((
            "PATH".into(),
            format!(
                "{}:{}",
                e.0.join("tools").display(),
                std::env::var("PATH").unwrap()
            ),
        ));
    }
    if let Some(v) = version {
        s.env.push(("FLEET_VERSION".into(), v.into()));
    }
    match run(&s) {
        Outcome::Exited {
            code,
            stdout,
            stderr,
        } => (
            code == Some(0),
            format!(
                "{}{}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            ),
        ),
        o => panic!("{o:?}"),
    }
}
fn install_fixture(e: &Env) {
    e.write("LICENSE", "fixture\n");
    for t in [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "aarch64-apple-darwin",
    ] {
        let (ok, out) = e.call(
            &[
                "release",
                "package",
                "--version",
                "v1.2.3",
                "--target",
                t,
                "--binary",
                env!("CARGO_BIN_EXE_fleet-install-fixture"),
                "--out",
                "releases/v1.2.3",
            ],
            &[],
        );
        assert!(ok, "{out}");
    }
    let (ok, out) = e.call(
        &[
            "release",
            "checksums",
            "--version",
            "v1.2.3",
            "releases/v1.2.3",
        ],
        &[],
    );
    assert!(ok, "{out}");
}
#[cfg(unix)]
fn platform(e: &Env, system: &str, machine: &str) {
    use std::os::unix::fs::PermissionsExt;
    e.write("tools/uname", &format!("#!/bin/sh\ncase \"$1\" in -s) echo {system} ;; -m) echo {machine} ;; *) exit 1 ;; esac\n"));
    fs::set_permissions(e.0.join("tools/uname"), fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn installer_selects_native_archives_and_refuses_unsupported_platforms() {
    let e = Env::new();
    install_fixture(&e);
    // The fixture executable is native to the test host. Only uname is stubbed.
    for (system, machine, target) in [
        ("Linux", "x86_64", "x86_64-unknown-linux-musl"),
        ("Linux", "amd64", "x86_64-unknown-linux-musl"),
        ("Linux", "aarch64", "aarch64-unknown-linux-musl"),
        ("Linux", "arm64", "aarch64-unknown-linux-musl"),
        ("Darwin", "arm64", "aarch64-apple-darwin"),
    ] {
        platform(&e, system, machine);
        let (ok, out) = installer(&e, &[]);
        assert!(ok && out.contains(target), "{system}/{machine}: {out}");
    }
    let before = fs::read(e.0.join("bin/fleet")).unwrap();
    for (system, machine) in [
        ("Darwin", "x86_64"),
        ("Darwin", "aarch64"),
        ("Linux", "riscv64"),
        ("FreeBSD", "arm64"),
    ] {
        platform(&e, system, machine);
        let (ok, out) = installer(&e, &[]);
        assert!(
            !ok && out.contains("unsupported platform"),
            "{system}/{machine}: {out}"
        );
        assert_eq!(before, fs::read(e.0.join("bin/fleet")).unwrap());
        assert!(!e.0.join("bin/.fleet-install.lock").exists());
    }
    platform(&e, "Darwin", "arm64");
    e.write(
        "releases/v1.2.3/fleet-v1.2.3-aarch64-apple-darwin.tar.gz",
        "tampered",
    );
    let (ok, out) = installer(&e, &[]);
    assert!(!ok && out.contains("checksum mismatch"), "{out}");
    assert_eq!(before, fs::read(e.0.join("bin/fleet")).unwrap());
}

#[test]
fn offline_install_update_tamper_refusal_and_uninstall() {
    let e = Env::new();
    install_fixture(&e);
    let (ok, out) = installer(&e, &[]);
    assert!(ok, "{out}");
    let binary = e.0.join("bin/fleet");
    let first = fs::read(&binary).unwrap();
    let (ok, out) = installer(&e, &[]);
    assert!(ok, "{out}");
    assert_eq!(first, fs::read(&binary).unwrap());
    e.write(
        "releases/v1.2.3/fleet-v1.2.3-checksums.txt",
        "tampered checksums\n",
    );
    assert!(!installer(&e, &[]).0);
    assert_eq!(first, fs::read(&binary).unwrap());
    let (ok, out) = installer(&e, &["--uninstall"]);
    assert!(ok && !binary.exists(), "{out}");
}
#[test]
fn concurrent_install_and_wrong_version_preserve_existing_binary() {
    let e = Env::new();
    install_fixture(&e);
    let (ok, out) = installer(&e, &[]);
    assert!(ok, "{out}");
    let binary = e.0.join("bin/fleet");
    let before = fs::read(&binary).unwrap();
    fs::create_dir(e.0.join("bin/.fleet-install.lock")).unwrap();
    let (ok, out) = installer(&e, &[]);
    assert!(!ok && out.contains("lock"), "{out}");
    assert_eq!(before, fs::read(&binary).unwrap());
    fs::remove_dir(e.0.join("bin/.fleet-install.lock")).unwrap();
    // A correctly checksummed archive carrying the wrong version is still refused.
    fs::rename(e.0.join("releases/v1.2.3"), e.0.join("releases/v2.0.0")).unwrap();
    for t in [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "aarch64-apple-darwin",
    ] {
        assert!(
            e.call(
                &[
                    "release",
                    "package",
                    "--version",
                    "v2.0.0",
                    "--target",
                    t,
                    "--binary",
                    env!("CARGO_BIN_EXE_fleet-install-fixture"),
                    "--out",
                    "releases/v2.0.0"
                ],
                &[]
            )
            .0
        );
    }
    assert!(
        e.call(
            &[
                "release",
                "checksums",
                "--version",
                "v2.0.0",
                "releases/v2.0.0"
            ],
            &[]
        )
        .0
    );
    fs::rename(e.0.join("releases/v2.0.0"), e.0.join("releases/v1.2.3")).unwrap();
    // Installer below is explicitly asked for a different release via direct fixture call.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut s = scratch("sh", Duration::from_secs(30)).arg(
        repo.join("scripts/fleet/install-release.sh")
            .into_os_string(),
    );
    fs::rename(e.0.join("releases/v1.2.3"), e.0.join("releases/v2.0.0")).unwrap();
    s.env = vec![
        ("FLEET_VERSION".into(), "v2.0.0".into()),
        (
            "FLEET_RELEASE_BASE".into(),
            e.0.join("releases").to_string_lossy().into(),
        ),
        (
            "FLEET_INSTALL_DIR".into(),
            e.0.join("bin").to_string_lossy().into(),
        ),
    ];
    assert!(run(&s).success_text().is_none());
    assert_eq!(before, fs::read(binary).unwrap());
}

#[test]
fn native_gate_executes_fixture_tests_doctests_lint_docs_and_format() {
    let e = Env::new();
    e.write(
        "Cargo.toml",
        "[package]\nname = 'gate-fixture'\nversion = '0.0.0'\nedition = '2024'\n[workspace]\n",
    );
    let lib = "/// One more.\n/// ```\n/// assert_eq!(gate_fixture::inc(1), 2);\n/// ```\npub fn inc(x: u32) -> u32 {\n    x + 1\n}\n";
    e.write("src/lib.rs", lib);
    e.write(
        "tests/architecture.rs",
        "#[test]\nfn holds() {\n    assert_eq!(gate_fixture::inc(1), 2);\n}\n",
    );
    let e2e = "#[test]\nfn runs() {\n    assert_eq!(gate_fixture::inc(2), 3);\n}\n";
    e.write("tests/e2e_demo.rs", e2e);
    e.write(".config/nextest.toml", "[profile.ci]\nfail-fast = false\n");
    let mut s = scratch("cargo", Duration::from_secs(30)).args(["generate-lockfile", "--offline"]);
    s.cwd = Some(e.0.clone());
    assert!(run(&s).success_text().is_some());
    let target = e.0.join("target").to_string_lossy().into_owned();
    let env = [
        ("CARGO_TARGET_DIR", target.as_str()),
        ("CARGO_NET_OFFLINE", "true"),
    ];
    for area in ["rust-fmt", "rust-test", "rust-lint", "rust-doc"] {
        let (ok, out) = e.call(&["check", area], &env);
        assert!(ok, "{out}");
    }
    e.write(
        "tests/e2e_demo.rs",
        &e2e.replace("#[test]", "#[test]\n#[ignore]"),
    );
    let (ok, out) = e.call(&["check", "rust-test"], &env);
    assert!(!ok && out.contains("e2e_"), "{out}");
    e.write("tests/e2e_demo.rs", e2e);
    e.write(
        ".config/nextest.toml",
        "[profile.ci]\ndefault-filter = 'not binary(e2e_demo)'\n",
    );
    let (ok, out) = e.call(&["check", "rust-test"], &env);
    assert!(!ok && out.contains("e2e_"), "{out}");
    e.write(".config/nextest.toml", "[profile.ci]\nfail-fast = false\n");
    fs::remove_file(e.0.join("tests/architecture.rs")).unwrap();
    let (ok, out) = e.call(&["check", "rust-test"], &env);
    assert!(!ok && out.contains("architecture"), "{out}");
    e.write(
        "tests/architecture.rs",
        "#[test]\nfn holds() { assert!(false); }\n",
    );
    let (ok, out) = e.call(&["check", "rust-test"], &env);
    assert!(!ok && out.contains("cargo nextest run"), "{out}");
    e.write("tests/architecture.rs", "#[test]\nfn holds() {}\n");
    e.write("src/lib.rs", &lib.replace("inc(1), 2", "inc(1), 3"));
    let (ok, out) = e.call(&["check", "rust-test"], &env);
    assert!(!ok && out.contains("cargo test --doc"), "{out}");
    e.write("src/lib.rs", &lib.replace("One more.", "See [`Missing`]."));
    let (ok, out) = e.call(&["check", "rust-doc"], &env);
    assert!(!ok && out.contains("cargo doc"), "{out}");
    e.write("src/lib.rs", &lib.replace("x + 1", "x+1"));
    assert!(!e.call(&["check", "rust-fmt"], &env).0);
    assert!(e.call(&["check", "--fix", "rust-fmt"], &env).0);
    e.write(
        "src/lib.rs",
        &lib.replace("    x + 1", "    let unused = 1;\n    x + 1"),
    );
    let (ok, out) = e.call(&["check", "rust-lint"], &env);
    assert!(!ok && out.contains("cargo clippy"), "{out}");
    fs::remove_file(e.0.join("Cargo.toml")).unwrap();
    let (ok, out) = e.call(&["check", "rust-test"], &env);
    assert!(!ok && out.contains("Cargo.toml"), "{out}");
}

const CD: &str = "defaults: {run: {shell: bash}}\njobs:\n  build:\n    runs-on: ubuntu-26.04-arm\n    steps:\n      - run: cargo xtask check rust-test\n      - run: cargo xtask release package\n  publish:\n    environment: release\n    steps: []\n";
const TITLE: &str = "on: {pull_request: {types: [opened, edited, synchronize, reopened]}}\njobs:\n  title:\n    steps: [{run: 'cargo xtask release title \"$PR_TITLE\" \"$PR_NUMBER\"'}]\n";
#[test]
fn release_and_title_workflows_keep_native_platform_and_trust_promises() {
    let e = Env::new();
    e.write(".github/workflows/ci.yml", CI);
    e.write(".github/workflows/cd.yml", CD);
    e.write(".github/workflows/pr-title.yml", TITLE);
    let (ok, out) = e.call(&["workflow"], &[]);
    assert!(ok, "{out}");
    for (from, to, why) in [
        ("shell: bash", "shell: sh", "pipefail"),
        (
            "environment: release",
            "environment: production",
            "environment",
        ),
        ("cargo xtask check rust-test", "cargo build", "rust-test"),
        ("ubuntu-26.04-arm", "macos-latest", "untested runner"),
    ] {
        e.write(".github/workflows/cd.yml", &CD.replace(from, to));
        let (ok, out) = e.call(&["workflow"], &[]);
        assert!(!ok && out.contains(why), "{why}: {out}");
    }
    e.write(".github/workflows/cd.yml", CD);
    e.write(
        ".github/workflows/pr-title.yml",
        &TITLE.replace("opened, edited,", "opened,"),
    );
    let (ok, out) = e.call(&["workflow"], &[]);
    assert!(!ok && out.contains("edited"), "{out}");
    e.write(".github/workflows/pr-title.yml", TITLE);
    e.write(
        ".github/workflows/ci.yml",
        &CI.replace("cargo xtask check'", "echo cargo xtask check'"),
    );
    assert!(!e.call(&["workflow"], &[]).0);
    e.write(
        ".github/workflows/ci.yml",
        &CI.replace(
            "steps: [{run: 'cargo xtask check'}]",
            "steps: [{if: success(), run: 'cargo xtask check'}]",
        ),
    );
    assert!(!e.call(&["workflow"], &[]).0);
    // An additional runner doing only Rust still owes every Rust check.
    let rust = "rust-fmt rust-lint rust-test rust-doc rust-deny";
    let extra = format!(
        "  arm:\n    runs-on: ubuntu-26.04-arm\n    timeout-minutes: 20\n    steps: [{{run: 'cargo xtask check {rust}'}}]\n"
    );
    let complete = CI
        .replace("ubuntu-latest, ubuntu-26.04-arm", "ubuntu-latest")
        .replace("needs: [rust]", "needs: [rust, arm]")
        + &extra;
    e.write(".github/workflows/ci.yml", &complete);
    assert!(e.call(&["workflow"], &[]).0);
    e.write(
        ".github/workflows/ci.yml",
        &complete.replace(rust, "rust-fmt rust-lint rust-test rust-doc"),
    );
    let (ok, out) = e.call(&["workflow"], &[]);
    assert!(!ok && out.contains("rust-deny"), "{out}");
}

#[test]
fn the_gate_refuses_missing_tools() {
    let e = Env::new();
    e.write("Cargo.toml", "[workspace]\nmembers=[]\n");
    let path = e.0.join("empty-path");
    fs::create_dir(&path).unwrap();
    let path = path.to_string_lossy();
    let (ok, out) = e.call(&["check", "rust-test"], &[("PATH", path.as_ref())]);
    assert!(!ok && out.contains("could not start"), "{out}");
}

#[test]
fn docs_gate_rejects_broken_links_malformed_svg_and_inconsistent_structure() {
    let e = Env::new();
    e.repo();
    e.write("Cargo.toml", "[workspace]\nmembers=[]\n");
    e.write("README.md", "# Fixture\n\n![Diagram](diagram.svg)\n");
    e.write(
        "diagram.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><style>@media (prefers-color-scheme: light) { :root { color: #000; } } @media (prefers-color-scheme: dark) { :root { color: #fff; } }</style><path/></svg>",
    );
    e.git(&["add", "."]);
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(ok, "{out}");
    e.write("diagram.svg", "<svg><broken></svg>");
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(!ok && out.contains("SVG"), "{out}");
    fs::remove_file(e.0.join("diagram.svg")).unwrap();
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(!ok && out.contains("missing link"), "{out}");

    e.write(
        "diagram.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><path/></svg>",
    );
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(!ok && out.contains("light and dark modes"), "{out}");

    e.write("docs/task-controller/README.md", "# Task controller\n");
    e.git(&["add", "."]);
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(!ok && out.contains("docs/controllers/task"), "{out}");

    fs::remove_dir_all(e.0.join("docs/task-controller")).unwrap();
    e.write(
        "diagram.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><style>@media (prefers-color-scheme: light) { :root { color: #000; } } @media (prefers-color-scheme: dark) { :root { color: #fff; } }</style><path/></svg>",
    );
    e.write("docs/controllers/README.md", "# Controllers\n");
    e.write("docs/controllers/task/README.md", "# Task controller\n");
    e.write(
        "AGENTS.md",
        "Read the [Task controller](docs/controllers/task/README.md).\n",
    );
    e.git(&["add", "-A"]);
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(!ok && out.contains("controller index"), "{out}");

    e.write("docs/controllers/plugin/README.md", "# Plugin controller\n");
    e.write(
        "docs/architecture.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><style>@media (prefers-color-scheme: light) { :root { color: #000; } } @media (prefers-color-scheme: dark) { :root { color: #fff; } }</style><text>three controllers</text></svg>",
    );
    e.write(
        "AGENTS.md",
        "Read the [controller index](docs/controllers/README.md).\n",
    );
    e.git(&["add", "-A"]);
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(!ok && out.contains("PluginController"), "{out}");
    e.write("docs/architecture.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"><style>@media (prefers-color-scheme: light) { :root { color: #000; } } @media (prefers-color-scheme: dark) { :root { color: #fff; } }</style><text>TaskController</text><text>PluginController host</text></svg>");
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(ok, "{out}");

    e.write(
        "docs/controllers/gateway/README.md",
        "# Gateway controller\n\nDesign, not implemented.\n",
    );
    e.git(&["add", "-A"]);
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(!ok && out.contains("GatewayController"), "{out}");
    e.write("docs/architecture.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"><style>@media (prefers-color-scheme: light) { :root { color: #000; } } @media (prefers-color-scheme: dark) { :root { color: #fff; } }</style><text>TaskController</text><text>GatewayController</text><text>PluginController host</text></svg>");
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(!ok && out.contains("GatewayController"), "{out}");
    e.write("docs/architecture.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"><style>@media (prefers-color-scheme: light) { :root { color: #000; } } @media (prefers-color-scheme: dark) { :root { color: #fff; } }</style><text>TaskController</text><text>planned GatewayController</text><text>PluginController host</text></svg>");
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(ok, "{out}");

    e.write("docs/architecture.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"><style>@media (prefers-color-scheme: light) { :root { color: #000; } } @media (prefers-color-scheme: dark) { :root { color: #fff; } }</style><text>TaskController</text><text>planned GatewayController</text><text>planned PluginController host</text></svg>");
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(!ok && out.contains("PluginController"), "{out}");
    e.write(
        "docs/controllers/gateway/README.md",
        "# Gateway controller\n\nShipped.\n",
    );
    e.write("docs/architecture.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"><style>@media (prefers-color-scheme: light) { :root { color: #000; } } @media (prefers-color-scheme: dark) { :root { color: #fff; } }</style><text>TaskController</text><text>planned GatewayController</text><text>PluginController host</text></svg>");
    let (ok, out) = e.call(&["check", "docs"], &[]);
    assert!(!ok && out.contains("GatewayController"), "{out}");
}
#[cfg(unix)]
#[test]
fn workflow_symlink_outside_checkout_is_refused() {
    let e = Env::new();
    let outside = Env::new();
    outside.write("ci.yml", CI);
    fs::create_dir_all(e.0.join(".github/workflows")).unwrap();
    std::os::unix::fs::symlink(
        outside.0.join("ci.yml"),
        e.0.join(".github/workflows/ci.yml"),
    )
    .unwrap();
    let (ok, out) = e.call(&["workflow"], &[]);
    assert!(!ok && out.contains("outside checkout"), "{out}");
}

#[test]
fn swallowed_ci_and_release_test_failures_cannot_count_as_validation() {
    let e = Env::new();
    e.write(".github/workflows/cd.yml", CD);
    for replacement in ["cargo xtask check || true", "cargo xtask check; true"] {
        e.write(
            ".github/workflows/ci.yml",
            &CI.replace("cargo xtask check'", &format!("{replacement}'")),
        );
        let (ok, out) = e.call(&["workflow"], &[]);
        assert!(!ok, "swallowed CI failure accepted: {out}");
    }
    for replacement in [
        "continue-on-error: true\n    timeout-minutes: 30",
        "timeout-minutes: 30\n    continue-on-error: ${{ true }}",
    ] {
        e.write(
            ".github/workflows/ci.yml",
            &CI.replace("timeout-minutes: 30", replacement),
        );
        let (ok, out) = e.call(&["workflow"], &[]);
        assert!(!ok, "ignored CI failure accepted: {out}");
    }
    e.write(".github/workflows/ci.yml", CI);
    for replacement in [
        "if: false\n        run: cargo xtask check rust-test",
        "continue-on-error: true\n        run: cargo xtask check rust-test",
        "run: cargo xtask check rust-test || true",
    ] {
        e.write(
            ".github/workflows/cd.yml",
            &CD.replace("run: cargo xtask check rust-test", replacement),
        );
        let (ok, out) = e.call(&["workflow"], &[]);
        assert!(!ok, "skipped or swallowed release test accepted: {out}");
    }
}

#[test]
fn the_gate_never_rebuilds_the_binary_it_runs_from() {
    // `cargo xtask check` runs fleet-dev while its own `cargo test` rebuilds
    // the workspace, fleet-dev included. Windows refuses to replace a running
    // executable, so the alias builds fleet-dev in a target dir of its own.
    let config = include_str!("../../../.cargo/config.toml");
    let alias = config
        .lines()
        .find(|l| l.trim_start().starts_with("xtask"))
        .expect("an xtask alias");
    assert!(alias.contains("--target-dir target/xtask"), "{alias}");
}

#[test]
fn v2_installs_only_when_a_version_is_named() {
    // Fleet v1 stays frozen; v2 is installed by explicitly naming
    // its version. The installer never resolves "latest", which could be v1.
    let e = Env::new();
    install_fixture(&e);
    let (ok, out) = installer_with(&e, &[], None);
    assert!(!ok, "{out}");
    assert!(out.contains("FLEET_VERSION"), "{out}");
    assert!(!out.contains("latest"), "{out}");
    let binary = e.0.join("bin/fleet");
    assert!(!binary.exists());
    // Removing an install needs no version.
    let (ok, out) = installer_with(&e, &["--uninstall"], None);
    assert!(ok, "{out}");
}

#[test]
fn the_tree_declares_fleet_major_2() {
    // Python v1 clones read this file from a fetched ref before they update,
    // and refuse anything that is not their own major version. This tree is
    // v2: it must say so exactly, and never inherit v1's `1`.
    let marker = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../FLEET_MAJOR");
    let text = fs::read_to_string(&marker).expect("a root FLEET_MAJOR file");
    assert_eq!(text, "2\n", "FLEET_MAJOR must be exactly \"2\\n\"");
}

#[test]
fn installed_binary_has_path_guidance() {
    let e = Env::new();
    install_fixture(&e);
    let (ok, out) = installer(&e, &[]);
    assert!(ok && out.contains("PATH"), "{out}");
}

#[test]
fn release_smoke_requires_offline_bootstrap_from_the_installed_binary() {
    let e = Env::new();
    let (ok, out) = e.call(
        &[
            "release",
            "smoke",
            "--binary",
            env!("CARGO_BIN_EXE_fleet-install-fixture"),
            "--version",
            "v1.2.3",
            "--first-run-dir",
            "clean-user",
        ],
        &[],
    );
    assert!(!ok && out.contains("bootstrap missing"), "{out}");
}

#[test]
fn release_checksums_include_the_published_installer() {
    let e = Env::new();
    install_fixture(&e);
    e.write("releases/v1.2.3/install-release.sh", "release metadata");
    let (ok, out) = e.call(
        &[
            "release",
            "checksums",
            "--version",
            "v1.2.3",
            "releases/v1.2.3",
        ],
        &[],
    );
    assert!(ok, "{out}");
    let sums = fs::read_to_string(e.0.join("releases/v1.2.3/fleet-v1.2.3-checksums.txt")).unwrap();
    assert!(sums.contains("  install-release.sh\n"), "{sums}");
}

#[test]
fn release_fixture_checksum_recipe_installs() {
    let e = Env::new();
    install_fixture(&e);
    let fixture = e.0.join("installer-fixture/v1.2.3");
    fs::create_dir_all(&fixture).unwrap();
    for item in fs::read_dir(e.0.join("releases/v1.2.3")).unwrap() {
        let item = item.unwrap();
        if item
            .file_name()
            .to_string_lossy()
            .ends_with("checksums.txt")
        {
            continue;
        }
        fs::copy(item.path(), fixture.join(item.file_name())).unwrap();
    }
    let recipe = include_str!("fixtures/workflows/cd.yml")
        .lines()
        .find(|l| l.contains("shasum -a 256") && l.contains("checksums.txt"))
        .expect("native release fixture checksum recipe")
        .trim();
    let mut spec = scratch("bash", Duration::from_secs(30)).args(["-eo", "pipefail", "-c", recipe]);
    spec.cwd = Some(e.0.clone());
    spec.env.push(("VERSION".into(), "v1.2.3".into()));
    let result = run(&spec);
    assert!(result.success_text().is_some(), "{result:?}");
    fs::copy(
        fixture.join("fleet-v1.2.3-checksums.txt"),
        e.0.join("releases/v1.2.3/fleet-v1.2.3-checksums.txt"),
    )
    .unwrap();
    let (ok, out) = installer(&e, &[]);
    assert!(ok, "native release checksum fixture: {out}");
}

#[test]
fn published_installer_installs_its_own_release() {
    // The README's one-liner fetches the latest release's installer with no
    // FLEET_VERSION. The copy the release publishes names that release, so
    // the version stays explicit while the repository script still requires one.
    let e = Env::new();
    install_fixture(&e);
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::create_dir_all(e.0.join("scripts")).unwrap();
    fs::create_dir_all(e.0.join("dist")).unwrap();
    fs::copy(
        repo.join("scripts/fleet/install-release.sh"),
        // Where the release recipe, written for Fleet's own repository, finds it.
        e.0.join("scripts/install-release.sh"),
    )
    .unwrap();
    let recipe = include_str!("fixtures/workflows/cd.yml")
        .lines()
        .find(|l| l.contains("scripts/install-release.sh") && l.contains("dist/"))
        .expect("the release step that publishes the installer")
        .trim();
    let mut spec = scratch("bash", Duration::from_secs(30)).args(["-eo", "pipefail", "-c", recipe]);
    spec.cwd = Some(e.0.clone());
    spec.env.push(("VERSION".into(), "v1.2.3".into()));
    let result = run(&spec);
    assert!(result.success_text().is_some(), "{result:?}");
    let mut s = scratch("sh", Duration::from_secs(30))
        .arg(e.0.join("dist/install-release.sh").into_os_string());
    s.env = vec![
        (
            "FLEET_RELEASE_BASE".into(),
            e.0.join("releases").to_string_lossy().into(),
        ),
        (
            "FLEET_INSTALL_DIR".into(),
            e.0.join("bin").to_string_lossy().into(),
        ),
    ];
    let out = run(&s);
    assert!(out.success_text().is_some(), "{out:?}");
    assert!(e.0.join("bin/fleet").exists());
}

/// Runs one `run:` step of the repository's own cd.yml under bash, the way
/// the runner does, with the real git and cog and a local fake for `gh`.
#[cfg(unix)]
fn release_step(e: &Env, job: &str, step: &str, env: &[(&str, &str)]) -> (bool, String) {
    use std::os::unix::fs::PermissionsExt;
    let cd = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workflows/cd.yml");
    let doc: serde_json::Value = serde_saphyr::from_str(&fs::read_to_string(cd).unwrap()).unwrap();
    let script = doc["jobs"][job]["steps"]
        .as_array()
        .unwrap_or_else(|| panic!("cd.yml has no {job} job"))
        .iter()
        .find(|s| s["name"] == step)
        .and_then(|s| s["run"].as_str())
        .unwrap_or_else(|| panic!("cd.yml has no {job}/{step} step"))
        .to_string();
    // The fake forge behaves like `gh release create --verify-tag`: the tag
    // must already exist.
    e.write(
        "fakes/gh",
        "#!/bin/sh\nv=$3\ngit rev-parse -q --verify \"refs/tags/$v\" >/dev/null || { echo \"tag $v does not exist\" >&2; exit 1; }\ncase \" $* \" in *\" --verify-tag \"*) ;; *) echo 'gh would create the tag' >&2; exit 1 ;; esac\necho \"released $v\"\n",
    );
    fs::set_permissions(e.0.join("fakes/gh"), fs::Permissions::from_mode(0o755)).unwrap();
    e.write("output", "");
    let mut paths = vec![e.0.join("fakes")];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut s = scratch("bash", Duration::from_secs(120)).args(["-eo", "pipefail", "-c", &script]);
    s.cwd = Some(e.0.clone());
    s.env = [
        (
            "PATH",
            std::env::join_paths(paths)
                .unwrap()
                .to_string_lossy()
                .to_string(),
        ),
        (
            "GIT_CONFIG_GLOBAL",
            e.0.join("no-config").to_string_lossy().to_string(),
        ),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        (
            "GITHUB_OUTPUT",
            e.0.join("output").to_string_lossy().to_string(),
        ),
        (
            "GITHUB_STEP_SUMMARY",
            e.0.join("summary").to_string_lossy().to_string(),
        ),
        ("GITHUB_REPOSITORY", "owner/fleet".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .chain(env.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    .collect();
    match run(&s) {
        Outcome::Exited {
            code,
            stdout,
            stderr,
        } => (
            code == Some(0),
            format!(
                "{}{}{}",
                fs::read_to_string(e.0.join("output")).unwrap(),
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            ),
        ),
        o => panic!("{o:?}"),
    }
}

#[cfg(unix)]
#[test]
fn a_push_to_main_tags_and_publishes_what_cocogitto_decides() {
    let cd = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workflows/cd.yml");
    let doc: serde_json::Value = serde_saphyr::from_str(&fs::read_to_string(cd).unwrap()).unwrap();
    assert!(
        doc["on"]["push"]["branches"]
            .as_array()
            .is_some_and(|b| b.iter().any(|b| b == "main")),
        "cd.yml must run on every push to main: {}",
        doc["on"]
    );
    let e = Env::new();
    e.repo();
    // Runner files stay out of the status cog checks, as in a clean checkout.
    e.write(
        ".git/info/exclude",
        "fakes/\noutput\nsummary\norigin.git/\ndist/\nno-config\nonly-git/\n",
    );
    e.write(".github/release-readiness.toml", "ready = true\n");
    e.commit("chore: ready");
    e.git(&["tag", "v2.1.0"]);
    e.git(&["init", "-q", "--bare", "origin.git"]);
    e.git(&["remote", "add", "origin", "origin.git"]);
    e.write("crates/demo.rs", "pub fn shipped() {}\npub fn more() {}\n");
    e.commit("feat: more");
    let main = [("GITHUB_REF", "refs/heads/main"), ("FORCED_VERSION", "")];
    let step = |name: &str, env: &[(&str, &str)]| {
        let env: Vec<_> = main.iter().chain(env).copied().collect();
        release_step(&e, "check-release", name, &env)
    };
    let (ok, out) = step("Parse release decision", &[]);
    assert!(
        ok && out.contains("should_release=true")
            && out.contains("version=v2.2.0\n")
            && out.contains("semver=2.2.0\n")
            && out.contains("bump_args=--auto --disable-bump-commit\n")
            && out.contains("prerelease=false"),
        "parse: {out}"
    );
    let (ok, out) = step(
        "Check the release touches a shipped artifact",
        &[("VERSION", "v2.2.0")],
    );
    assert!(ok && out.contains("shipped=true"), "shipped: {out}");
    let (ok, out) = step("Check publication is allowed", &[]);
    assert!(ok && out.contains("allowed=true"), "allowed: {out}");
    let newest = [("TARGET", "v2.2.0"), ("PRERELEASE", "false")];
    let (ok, out) = step("Is this the highest version ever tagged", &newest);
    assert!(ok && out.contains("newest=true"), "newest: {out}");
    // Only main publishes, and readiness fails closed.
    let (ok, out) = release_step(
        &e,
        "check-release",
        "Check publication is allowed",
        &[("GITHUB_REF", "refs/heads/feature")],
    );
    assert!(ok && out.contains("allowed=false"), "branch: {out}");
    for text in [
        "",
        "reason = 'waiting'\n",
        "ready = 'true'\n",
        "ready = false\n",
        "invalid TOML",
    ] {
        e.write(".github/release-readiness.toml", text);
        let (ok, out) = step("Check publication is allowed", &[]);
        assert!(ok && out.contains("allowed=false"), "{text:?}: {out}");
    }
    fs::remove_file(e.0.join(".github/release-readiness.toml")).unwrap();
    let (ok, out) = step("Check publication is allowed", &[]);
    assert!(
        ok && out.contains("allowed=false"),
        "missing readiness: {out}"
    );
    e.git(&["checkout", "-q", "--", ".github/release-readiness.toml"]);
    // create-tag: the action runs `cog bump <bump_args>`, then the tag is pushed.
    let mut bump =
        scratch("cog", Duration::from_secs(60)).args(["bump", "--auto", "--disable-bump-commit"]);
    bump.cwd = Some(e.0.clone());
    assert!(run(&bump).success_text().is_some(), "cog bump");
    // The create-tag job's checkout is thrown away, with the changelog cog
    // staged there; later jobs check out clean.
    e.git(&["reset", "-q", "--hard"]);
    let (ok, out) = release_step(
        &e,
        "create-tag",
        "Push tag to repository",
        &[("VERSION", "v2.2.0")],
    );
    assert!(ok, "push tag: {out}");
    let rev = |args: &[&str]| {
        let mut s = scratch("git", Duration::from_secs(10)).args(args.iter().copied());
        s.cwd = Some(e.0.clone());
        run(&s).success_text().map(|s| s.trim().to_string())
    };
    let head = rev(&["rev-parse", "HEAD"]);
    assert_eq!(
        rev(&["--git-dir", "origin.git", "rev-parse", "v2.2.0^{commit}"]),
        head
    );
    let mut notes = scratch("cog", Duration::from_secs(60)).args(["changelog", "--at", "v2.2.0"]);
    notes.cwd = Some(e.0.clone());
    let notes = run(&notes).success_text().unwrap_or_default();
    assert!(
        notes.contains("v2.2.0") && notes.contains("more"),
        "notes: {notes}"
    );
    e.write("dist/fleet-v2.2.0-checksums.txt", "");
    let (ok, out) = release_step(
        &e,
        "publish",
        "Create GitHub Release",
        &[
            ("VERSION", "v2.2.0"),
            ("NEWEST", "true"),
            ("PRERELEASE", "false"),
            ("CHANGELOG", &notes),
            ("GH_TOKEN", "fake"),
        ],
    );
    assert!(ok && out.contains("released v2.2.0"), "publish: {out}");
    // Re-running the released push releases nothing, and so does a push
    // with no releasable commit.
    let (ok, out) = step("Parse release decision", &[]);
    assert!(ok && out.contains("should_release=false"), "rerun: {out}");
    e.write("docs/notes.md", "# Notes\n");
    e.commit("docs: comment");
    let (ok, out) = step("Parse release decision", &[]);
    assert!(
        ok && out.contains("should_release=false"),
        "docs push: {out}"
    );
    // A releasable type that ships nothing is held by the relevance gate.
    e.write("README.md", "docs only");
    e.commit("fix: prose");
    let (ok, out) = step("Parse release decision", &[]);
    assert!(ok && out.contains("version=v2.2.1\n"), "fix: {out}");
    let (ok, out) = step(
        "Check the release touches a shipped artifact",
        &[("VERSION", "v2.2.1")],
    );
    assert!(ok && out.contains("shipped=false"), "docs only: {out}");
    // A forced prerelease is marked as one and never latest.
    let (ok, out) = release_step(
        &e,
        "check-release",
        "Parse release decision",
        &[
            ("GITHUB_REF", "refs/heads/main"),
            ("FORCED_VERSION", "3.0.0-rc.1"),
        ],
    );
    assert!(
        ok && out.contains("version=v3.0.0-rc.1\n")
            && out.contains("bump_args=--version 3.0.0-rc.1 --disable-bump-commit\n")
            && out.contains("prerelease=true"),
        "forced: {out}"
    );
    let (ok, out) = step(
        "Is this the highest version ever tagged",
        &[("TARGET", "v3.0.0-rc.1"), ("PRERELEASE", "true")],
    );
    assert!(
        ok && out.contains("newest=false"),
        "prerelease newest: {out}"
    );
    let (ok, out) = step(
        "Is this the highest version ever tagged",
        &[("TARGET", "v2.1.1"), ("PRERELEASE", "false")],
    );
    assert!(
        ok && out.contains("newest=false"),
        "maintenance newest: {out}"
    );
    // A missing cog is an unknown decision, never "no release".
    fs::create_dir_all(e.0.join("only-git")).unwrap();
    for tool in ["bash", "git"] {
        let found = run(
            &scratch("sh", Duration::from_secs(10)).args(["-c", &format!("command -v {tool}")])
        )
        .success_text()
        .unwrap();
        std::os::unix::fs::symlink(found.trim(), e.0.join("only-git").join(tool)).unwrap();
    }
    let path = e.0.join("only-git").to_string_lossy().to_string();
    let (ok, out) = step("Parse release decision", &[("PATH", &path)]);
    assert!(!ok && !out.contains("should_release"), "without cog: {out}");
}

#[test]
fn claude_code_finds_the_repository_skills_through_a_tracked_link() {
    // Codex reads project skills from .agents/skills, Claude Code only from
    // .claude/skills: a session on either harness must find the same skills.
    // Each skill is linked one by one, so a link is tracked per skill.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut skills = 0;
    for entry in fs::read_dir(root.join(".agents/skills")).unwrap() {
        let name = entry.unwrap().file_name();
        let claude = root.join(".claude/skills").join(&name).join("SKILL.md");
        assert!(claude.is_file(), "Claude Code finds no {name:?}");
        let link = Path::new(".claude/skills").join(&name);
        let mut s = scratch("git", Duration::from_secs(10)).args([
            std::ffi::OsStr::new("ls-files"),
            std::ffi::OsStr::new("--stage"),
            std::ffi::OsStr::new("--"),
            link.as_os_str(),
        ]);
        s.cwd = Some(root.clone());
        let o = run(&s);
        let listing = o.success_text().unwrap_or_else(|| panic!("{o:?}"));
        assert!(
            listing.starts_with("120000 "),
            "{link:?} is not a tracked symlink: {listing:?}"
        );
        skills += 1;
    }
    assert!(skills > 0, "the repository has no skills");
}

/// Run in a child by
/// `a_git_helper_never_touches_the_repository_its_parent_names`, which
/// passes it a `GIT_DIR` as a commit hook would.
#[test]
#[ignore = "run in a child by a_git_helper_never_touches_the_repository_its_parent_names"]
fn git_helper_under_an_inherited_git_dir() {
    let e = Env::new();
    e.git(&["init", "-q", "-b", "main"]);
    e.git(&["config", "user.name", "Fixture"]);
    assert!(e.0.join(".git/config").is_file());
}

#[test]
fn a_git_helper_never_touches_the_repository_its_parent_names() {
    let other = Env::new();
    other.git(&["init", "-q", "-b", "main"]);
    let config = other.0.join(".git/config");
    let before = fs::read_to_string(&config).unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "git_helper_under_an_inherited_git_dir",
            "--ignored",
        ])
        .env("GIT_DIR", other.0.join(".git"))
        .output()
        .unwrap();
    assert!(child.status.success(), "{child:?}");
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
}
