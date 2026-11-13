//! The architecture rules hold for this workspace, and every rule is proven
//! to catch what it forbids: each negative fixture below is a small
//! workspace that breaks exactly one rule, and the checker must report
//! exactly that rule and nothing else.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use arch_rules::{Violation, Workspace, check, check_uncached, load_rules};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// [`check`], asserting that resolving every name afresh finds the same:
/// the resolution cache may only save work. A fresh resolution that runs
/// out of budget is the case the cache exists for, so it is not compared.
fn check_agreeing_with_uncached(
    ws: &Workspace,
    rules: &arch_rules::Rules,
    case: &str,
) -> Vec<Violation> {
    let cached = check(ws, rules);
    let fresh = check_uncached(ws, rules);
    if !fresh.iter().any(|v| v.code == "unresolvable") {
        assert_eq!(cached, fresh, "the cache changed what {case} reports");
    }
    cached
}

#[test]
fn this_workspace_obeys_its_architecture() {
    let root = repo_root();
    let ws = Workspace::load(&root, "Cargo.toml").expect("workspace");
    let rules = load_rules(&root.join("crates").join("architecture.toml")).expect("rules");
    let names: BTreeSet<_> = ws.crates.iter().map(|c| c.name.as_str()).collect();
    for expected in [
        "fleet-api",
        "fleet-controllers",
        "fleet-store",
        "fleet-runtime",
        "fleet-manager",
        "fleet-cli",
        "arch-rules",
        "fleet-exec",
        "fleet-forge",
        "fleet-plugin",
        "fleet-gateway",
        "fleet-telemetry",
        "fleet-dev",
    ] {
        assert!(
            names.contains(expected),
            "{expected} missing from {names:?}"
        );
    }
    let violations = check_agreeing_with_uncached(&ws, &rules, "workspace");
    let shown: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(
        violations.is_empty(),
        "architecture violations:\n{}",
        shown.join("\n")
    );
}

static SEQ: AtomicU32 = AtomicU32::new(0);

const RULES: &str = r#"
[crates.fx-api]
role = "pure"
deps = []
external = ["serde"]
probes = ["fx-store"]

[crates.fx-ctl]
role = "pure"
deps = ["fx-api"]
probes = ["fx-store", "fx-mgr"]

[crates.fx-store]
role = "adapter"
deps = ["fx-api"]

[crates.fx-mgr]
role = "orchestration"
deps = ["fx-api", "fx-ctl"]

[crates.fx-cli]
role = "edge"
deps = ["fx-api", "fx-mgr", "fx-store"]
forbidden_methods = ["update_status", "append_inbox"]

[pure]
forbidden_paths = ["std::fs", "std::env", "std::process", "std::time::Instant", "std::println"]

[status_writers]
"fx_api::store::FieldManager::Ctl" = ["fx-api", "fx-ctl"]
"fx_api::store::FieldManager::Cli" = ["fx-api", "fx-mgr"]

[[restricted]]
path = "std::process::Command"
allowed = []
why = "no external programs"
"#;

fn manifest(name: &str, deps: &[&str]) -> String {
    let mut s = format!("[package]\nname = \"{name}\"\n\n[dependencies]\n");
    for d in deps {
        s.push_str(&format!("{d} = {{ path = \"../{d}\" }}\n"));
    }
    s
}

/// The clean base: one crate per role, every rule satisfied.
fn base() -> Vec<(String, String)> {
    let files = [
        ("fixture.toml", "[workspace]\nmembers = [\"crates/*\"]\n".to_string()),
        ("architecture.toml", RULES.to_string()),
        ("crates/fx-api/fixture.toml", manifest("fx-api", &["serde"])),
        (
            "crates/fx-api/src/lib.rs",
            "//! ```compile_fail\n//! use fx_store::Store;\n//! ```\npub mod store {\n    pub enum FieldManager { Ctl, Worker, Cli }\n}\npub use store::FieldManager;\nuse serde::Serialize;\n".into(),
        ),
        ("crates/fx-ctl/fixture.toml", manifest("fx-ctl", &["fx-api"])),
        (
            "crates/fx-ctl/src/lib.rs",
            "//! ```compile_fail\n//! use fx_store::Store;\n//! ```\n//!\n//! ```compile_fail\n//! use fx_mgr::run;\n//! ```\nuse fx_api::FieldManager;\npub fn plan() -> FieldManager { FieldManager::Ctl }\n".into(),
        ),
        ("crates/fx-store/fixture.toml", manifest("fx-store", &["fx-api"])),
        ("crates/fx-store/src/lib.rs", "use std::fs;\npub struct S;\npub fn read() { let _ = fs::read(\"x\"); }\n".into()),
        ("crates/fx-mgr/fixture.toml", manifest("fx-mgr", &["fx-api", "fx-ctl"])),
        ("crates/fx-mgr/src/lib.rs", "pub fn run() { let _ = fx_ctl::plan(); let _ = fx_api::FieldManager::Worker; }\n".into()),
        ("crates/fx-cli/fixture.toml", manifest("fx-cli", &["fx-api", "fx-mgr", "fx-store"])),
        ("crates/fx-cli/src/main.rs", "fn main() { fx_mgr::run(); fx_store::read(); println!(\"ok\"); }\n".into()),
    ];
    files.into_iter().map(|(p, c)| (p.to_string(), c)).collect()
}

/// Write the base with `changes` applied (replacing or adding files) and
/// return every (code, crate) the checker reports.
fn report(case: &str, changes: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("arch")
        .join(format!("{case}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut files = base();
    for (p, c) in changes {
        match files.iter_mut().find(|(fp, _)| fp == p) {
            Some(f) => f.1 = c.to_string(),
            None => files.push((p.to_string(), c.to_string())),
        }
    }
    for (p, c) in &files {
        let path = root.join(p);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, c).unwrap();
    }
    let ws = Workspace::load(&root, "fixture.toml").expect("fixture workspace");
    let rules = load_rules(&root.join("architecture.toml")).expect("fixture rules");
    let found = check_agreeing_with_uncached(&ws, &rules, case);
    let _ = fs::remove_dir_all(&root);
    found
        .into_iter()
        .map(|v| (v.code.to_string(), v.krate))
        .collect()
}

fn expect(case: &str, changes: &[(&str, &str)], want: &[(&str, &str)]) {
    let got = report(case, changes);
    let want: BTreeSet<(String, String)> = want
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    assert_eq!(got, want, "fixture {case}");
}

const CTL: &str = "crates/fx-ctl/src/lib.rs";
const PROBES: &str = "//! ```compile_fail\n//! use fx_store::Store;\n//! ```\n//! ```compile_fail\n//! use fx_mgr::run;\n//! ```\n";

fn ctl(body: &str) -> String {
    format!("{PROBES}{body}")
}

#[test]
fn the_clean_fixture_passes() {
    expect("clean", &[], &[]);
}

#[test]
fn a_pure_crate_reaching_io_through_an_alias_is_caught() {
    expect(
        "alias",
        &[(
            CTL,
            &ctl("use std::fs as f;\npub fn x() { let _ = f::read(\"p\"); }\n"),
        )],
        &[("pure-io", "fx-ctl")],
    );
}

#[test]
fn a_pure_crate_reaching_io_through_a_reexport_chain_is_caught() {
    let src = ctl(
        "mod a { pub use std::fs; }\nmod b { pub use super::a::fs as files; }\npub fn x() { let _ = b::files::read(\"p\"); }\n",
    );
    expect("reexport-chain", &[(CTL, &src)], &[("pure-io", "fx-ctl")]);
}

#[test]
fn a_pure_crate_reaching_io_through_a_glob_is_caught() {
    expect(
        "glob",
        &[(
            CTL,
            &ctl("use std::*;\npub fn x() { let _ = fs::read(\"p\"); }\n"),
        )],
        &[("pure-io", "fx-ctl")],
    );
}

#[test]
fn a_pure_crate_reading_the_environment_or_clock_is_caught() {
    expect(
        "env-macro",
        &[(CTL, &ctl("pub fn x() -> &'static str { env!(\"HOME\") }\n"))],
        &[("pure-io", "fx-ctl")],
    );
    expect(
        "clock",
        &[(
            CTL,
            &ctl("pub fn x() { let _ = ::std::time::Instant::now(); }\n"),
        )],
        &[("pure-io", "fx-ctl")],
    );
    expect(
        "print",
        &[(CTL, &ctl("pub fn x() { println!(\"side effect\"); }\n"))],
        &[("pure-io", "fx-ctl")],
    );
}

#[test]
fn a_pure_crate_using_an_unlisted_external_crate_is_caught() {
    expect(
        "external",
        &[
            (
                "crates/fx-ctl/fixture.toml",
                &manifest("fx-ctl", &["fx-api", "rand"]),
            ),
            (
                CTL,
                &ctl("use rand::random;\npub fn x() -> u8 { random() }\n"),
            ),
        ],
        &[("pure-external", "fx-ctl")],
    );
}

#[test]
fn an_adapter_depending_on_a_controller_is_caught_in_manifest_and_source() {
    expect(
        "adapter-edge",
        &[
            (
                "crates/fx-store/fixture.toml",
                &manifest("fx-store", &["fx-api", "fx-ctl"]),
            ),
            (
                "crates/fx-store/src/lib.rs",
                "pub fn y() { let _ = fx_ctl::plan(); }\n",
            ),
        ],
        &[("undeclared-edge", "fx-store")],
    );
}

#[test]
fn a_renamed_dependency_and_extern_crate_alias_are_caught() {
    expect(
        "rename",
        &[
            (
                "crates/fx-store/fixture.toml",
                "[package]\nname = \"fx-store\"\n\n[dependencies]\nfx-api = { path = \"../fx-api\" }\npolicy = { package = \"fx-ctl\", path = \"../fx-ctl\" }\n",
            ),
            (
                "crates/fx-store/src/lib.rs",
                "extern crate policy as p;\npub fn y() { let _ = p::plan(); }\n",
            ),
        ],
        &[("undeclared-edge", "fx-store")],
    );
}

#[test]
fn the_cli_reaching_a_controller_through_a_manager_reexport_is_caught() {
    expect(
        "cli-reexport",
        &[
            (
                "crates/fx-mgr/src/lib.rs",
                "pub use fx_ctl as policy;\npub fn run() {}\n",
            ),
            (
                "crates/fx-cli/src/main.rs",
                "fn main() { let _ = fx_mgr::policy::plan(); fx_store::read(); }\n",
            ),
        ],
        &[("reexport-edge", "fx-cli")],
    );
    expect(
        "cli-glob-reexport",
        &[
            (
                "crates/fx-mgr/src/lib.rs",
                "pub use fx_ctl::*;\npub fn run() {}\n",
            ),
            (
                "crates/fx-cli/src/main.rs",
                "fn main() { let _ = fx_mgr::plan(); }\n",
            ),
        ],
        &[("reexport-edge", "fx-cli")],
    );
}

#[test]
fn naming_another_controllers_field_manager_is_caught() {
    expect(
        "status-writer",
        &[(
            "crates/fx-cli/src/main.rs",
            "use fx_api::FieldManager as F;\nfn main() { let _ = F::Ctl; }\n",
        )],
        &[("status-writer", "fx-cli")],
    );
    expect(
        "status-writer-via-module",
        &[(
            "crates/fx-mgr/src/lib.rs",
            "use fx_api::store;\npub fn run() { let _ = store::FieldManager::Ctl; }\n",
        )],
        &[("status-writer", "fx-mgr")],
    );
}

/// The typed inbox mutations name their caller the way status writes do:
/// only the manager's verb speaks as `Cli`, and the CLI never appends itself.
#[test]
fn an_inbox_mutation_caller_is_a_status_writer_too() {
    expect(
        "inbox-caller-positive",
        &[(
            "crates/fx-mgr/src/lib.rs",
            "pub fn run() { let _ = fx_ctl::plan(); let _ = fx_api::FieldManager::Cli; }\n",
        )],
        &[],
    );
    expect(
        "inbox-caller-cli",
        &[(
            "crates/fx-cli/src/main.rs",
            "fn main() { let _ = fx_api::FieldManager::Cli; fx_mgr::run(); }\n",
        )],
        &[("status-writer", "fx-cli")],
    );
    expect(
        "inbox-caller-store",
        &[(
            "crates/fx-store/src/lib.rs",
            "use std::fs;\npub struct S;\npub fn read() { let _ = fs::read(\"x\"); let _ = fx_api::FieldManager::Cli; }\n",
        )],
        &[("status-writer", "fx-store")],
    );
    expect(
        "inbox-append-from-cli",
        &[(
            "crates/fx-cli/src/main.rs",
            "fn main() { let s = fx_store::S; s.append_inbox(); fx_mgr::run(); }\n",
        )],
        &[("cli-method", "fx-cli")],
    );
}

#[test]
fn the_cli_writing_status_itself_is_caught() {
    expect(
        "cli-method",
        &[(
            "crates/fx-cli/src/main.rs",
            "fn main() { let s = fx_store::S; s.update_status(); }\n",
        )],
        &[("cli-method", "fx-cli")],
    );
}

#[test]
fn spawning_an_external_program_is_caught() {
    expect(
        "command",
        &[(
            "crates/fx-store/src/lib.rs",
            "use std::process::Command as C;\npub fn z() { let _ = C::new(\"thurbox-cli\"); }\n",
        )],
        &[("restricted", "fx-store")],
    );
}

#[test]
fn cycles_in_the_declared_or_actual_graph_are_caught() {
    let rules = RULES.replace(
        "[crates.fx-api]\nrole = \"pure\"\ndeps = []",
        "[crates.fx-api]\nrole = \"pure\"\ndeps = [\"fx-ctl\"]",
    );
    expect(
        "declared-cycle",
        &[("architecture.toml", &rules)],
        &[("cycle", "fx-api")],
    );
    expect(
        "actual-cycle",
        &[(
            "crates/fx-api/fixture.toml",
            &manifest("fx-api", &["serde", "fx-ctl"]),
        )],
        &[("cycle", "fx-api"), ("undeclared-edge", "fx-api")],
    );
}

#[test]
fn an_undeclared_dev_dependency_is_caught() {
    let m = format!(
        "{}\n[dev-dependencies]\nfx-mgr = {{ path = \"../fx-mgr\" }}\n",
        manifest("fx-store", &["fx-api"])
    );
    expect(
        "dev-edge",
        &[("crates/fx-store/fixture.toml", &m)],
        &[("undeclared-dev-edge", "fx-store")],
    );
}

#[test]
fn an_ungoverned_crate_is_caught() {
    expect(
        "ungoverned",
        &[
            (
                "crates/fx-extra/fixture.toml",
                "[package]\nname = \"fx-extra\"\n",
            ),
            ("crates/fx-extra/src/lib.rs", "pub fn e() {}\n"),
        ],
        &[("ungoverned-crate", "fx-extra")],
    );
}

#[test]
fn a_pure_crate_without_its_compile_fail_probes_is_caught() {
    expect(
        "missing-probe",
        &[(
            CTL,
            "use fx_api::FieldManager;\npub fn plan() -> FieldManager { FieldManager::Ctl }\n",
        )],
        &[("missing-probe", "fx-ctl")],
    );
}

#[test]
fn a_glob_of_a_local_module_resolves_without_expanding_through_itself() {
    // The shape of fleet-controllers' `use conditions::*`: a glob of a sibling
    // module, used from the module that globs it. Resolution must terminate
    // within its budget and find nothing to report.
    let src = ctl(
        "pub mod conditions { pub const A: &str = \"a\"; }\nuse conditions::*;\npub fn f() -> &'static str { let _ = conditions::A; A }\n",
    );
    expect("local-glob", &[(CTL, &src)], &[]);
}

/// Nested `use super::*` modules that each glob a sibling too, the shape of
/// a component's `#[cfg(test)] mod tests` trees: every bare name may come
/// through every glob on the way up, so resolution that recomputes each
/// branch runs out of budget, while one that remembers what it resolved
/// finds nothing to report.
#[test]
fn nested_glob_imports_resolve_within_the_budget() {
    let mut body = String::from("pub mod helpers { pub fn h() {} }\n");
    let depth = 12;
    for level in 0..depth {
        body.push_str(&format!(
            "pub mod m{level} {{ use super::*; use crate::helpers::*; pub fn f{level}() {{ h(); }}\n"
        ));
    }
    body.push_str("pub fn deepest() { h(); f0(); }\n");
    body.push_str(&"}\n".repeat(depth));
    let src = ctl(&body);
    expect("nested-globs", &[(CTL, &src)], &[]);
}

/// A function only takes a value name: `fs::read` still reaches the
/// glob-imported `std::fs` module, so the read is caught.
#[test]
fn a_function_does_not_shadow_a_glob_imported_module() {
    let src = ctl("use std::*;\nfn fs() {}\npub fn x() { let _ = fs::read(\"p\"); }\n");
    expect("value-shadow", &[(CTL, &src)], &[("pure-io", "fx-ctl")]);
}

/// A module of the same name does shadow the glob: `fs::read` is the local
/// module's function, so nothing is reported.
#[test]
fn a_module_shadows_a_glob_imported_module() {
    let src = ctl("use std::*;\nmod fs { pub fn read() {} }\npub fn x() { fs::read(); }\n");
    expect("type-shadow", &[(CTL, &src)], &[]);
}

#[test]
fn io_inside_a_macro_argument_is_caught() {
    let src = ctl("pub fn x() -> String { format!(\"{:?}\", std::fs::remove_file(\"x\")) }\n");
    expect("macro-arg", &[(CTL, &src)], &[("pure-io", "fx-ctl")]);
}

#[test]
fn a_forbidden_method_called_as_a_path_is_caught() {
    expect(
        "cli-ufcs",
        &[(
            "crates/fx-cli/src/main.rs",
            "fn main() { let s = fx_store::S; fx_store::S::update_status(&s); }\n",
        )],
        &[("cli-method", "fx-cli")],
    );
}

/// The runtime follow-on's rule: one crate may start processes, and every
/// other adapter reaches programs only through it.
#[test]
fn only_the_exec_crate_spawns_and_others_reach_programs_through_it() {
    let rules = format!(
        "{}\n[crates.fx-exec]\nrole = \"adapter\"\ndeps = []\n",
        RULES
            .replace(
                "deps = [\"fx-api\"]\n\n[crates.fx-mgr]",
                "deps = [\"fx-api\", \"fx-exec\"]\n\n[crates.fx-mgr]"
            )
            .replace(
                "path = \"std::process::Command\"\nallowed = []",
                "path = \"std::process::Command\"\nallowed = [\"fx-exec\"]"
            )
    );
    let exec_src = "use std::process::Command;\npub fn run(p: &str) { let _ = Command::new(p); }\n";
    let base = [
        ("architecture.toml", rules.as_str()),
        (
            "crates/fx-exec/fixture.toml",
            "[package]\nname = \"fx-exec\"\n",
        ),
        ("crates/fx-exec/src/lib.rs", exec_src),
    ];
    let store_manifest = manifest("fx-store", &["fx-api", "fx-exec"]);
    let through = "pub struct S;\npub fn read() { fx_exec::run(\"thurbox-cli\"); }\n";
    let mut clean: Vec<(&str, &str)> = base.to_vec();
    clean.push(("crates/fx-store/fixture.toml", &store_manifest));
    clean.push(("crates/fx-store/src/lib.rs", through));
    expect("exec-clean", &clean, &[]);

    let direct = "use std::process::Command;\npub struct S;\npub fn read() { let _ = Command::new(\"thurbox-cli\"); }\n";
    let mut dirty: Vec<(&str, &str)> = base.to_vec();
    dirty.push(("crates/fx-store/fixture.toml", &store_manifest));
    dirty.push(("crates/fx-store/src/lib.rs", direct));
    expect("exec-bypass", &dirty, &[("restricted", "fx-store")]);
}

/// One crate starts processes: every way to spawn one is restricted to it.
#[test]
fn the_rules_keep_spawning_in_fleet_exec() {
    let rules = load_rules(&repo_root().join("crates").join("architecture.toml")).expect("rules");
    for path in [
        "std::process::Command",
        "std::os::unix::process",
        "std::os::windows::process",
        "process_wrap",
    ] {
        let r = rules
            .restricted
            .iter()
            .find(|r| r.path == path)
            .unwrap_or_else(|| panic!("{path} is not restricted"));
        assert_eq!(r.allowed, vec!["fleet-exec".to_string()], "{path}");
    }
}

/// Telemetry's rules, against the real rules file: fleet-telemetry is an
/// adapter on no other Fleet crate, the CLI alone wires it, both pure
/// crates probe its edge, and the SDK and the OTLP exporter are named by it
/// alone.
#[test]
fn the_rules_keep_the_sdk_and_exporter_in_fleet_telemetry() {
    let rules = load_rules(&repo_root().join("crates").join("architecture.toml")).expect("rules");
    let telemetry = rules
        .crates
        .get("fleet-telemetry")
        .expect("fleet-telemetry is governed");
    assert_eq!(telemetry.role, arch_rules::Role::Adapter);
    assert!(telemetry.deps.is_empty());
    let dependants: Vec<&String> = rules
        .crates
        .iter()
        .filter(|(_, r)| r.deps.iter().any(|d| d == "fleet-telemetry"))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        dependants,
        vec!["fleet-cli"],
        "only the edge wires telemetry"
    );
    for pure in ["fleet-api", "fleet-controllers"] {
        assert!(
            rules.crates[pure]
                .probes
                .iter()
                .any(|p| p == "fleet-telemetry"),
            "{pure} probes the telemetry edge"
        );
    }
    for path in ["opentelemetry_sdk", "opentelemetry_otlp"] {
        let r = rules
            .restricted
            .iter()
            .find(|r| r.path == path)
            .unwrap_or_else(|| panic!("{path} is not restricted"));
        assert_eq!(r.allowed, vec!["fleet-telemetry".to_string()], "{path}");
    }
}

/// The telemetry rule on a fixture: the crate that owns the SDK may name
/// it; a manager that installs its own provider is caught.
#[test]
fn only_the_telemetry_crate_names_the_sdk() {
    let rules = format!(
        "{RULES}\n[crates.fx-tel]\nrole = \"adapter\"\ndeps = []\n\n[[restricted]]\npath = \"fx_sdk\"\nallowed = [\"fx-tel\"]\nwhy = \"one crate exports telemetry\"\n"
    );
    let base = [
        ("architecture.toml", rules.as_str()),
        (
            "crates/fx-tel/fixture.toml",
            "[package]\nname = \"fx-tel\"\n\n[dependencies]\nfx-sdk = \"1\"\n",
        ),
        (
            "crates/fx-tel/src/lib.rs",
            "pub fn init() { fx_sdk::install(); }\n",
        ),
    ];
    expect("telemetry-clean", &base, &[]);

    let mgr_manifest = format!(
        "{}fx-sdk = \"1\"\n",
        manifest("fx-mgr", &["fx-api", "fx-ctl"])
    );
    let mut dirty: Vec<(&str, &str)> = base.to_vec();
    dirty.push(("crates/fx-mgr/fixture.toml", &mgr_manifest));
    dirty.push((
        "crates/fx-mgr/src/lib.rs",
        "pub fn run() { let _ = fx_ctl::plan(); let _ = fx_api::FieldManager::Worker; fx_sdk::install(); }\n",
    ));
    expect("telemetry-bypass", &dirty, &[("restricted", "fx-mgr")]);
}

/// The forge follow-on's rules, against the real rules file: fleet-forge is
/// an adapter reaching only the API and the spawner, the CLI is the only
/// crate that wires it, and ChangeRequestController's field manager is
/// named by the API and the controllers alone.
#[test]
fn the_rules_keep_forges_below_the_port() {
    let rules = load_rules(&repo_root().join("crates").join("architecture.toml")).expect("rules");
    let forge = rules
        .crates
        .get("fleet-forge")
        .expect("fleet-forge is governed");
    assert_eq!(forge.role, arch_rules::Role::Adapter);
    assert_eq!(
        forge.deps,
        vec!["fleet-api".to_string(), "fleet-exec".to_string()]
    );
    let dependants: Vec<&String> = rules
        .crates
        .iter()
        .filter(|(_, r)| r.deps.iter().any(|d| d == "fleet-forge"))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(dependants, vec!["fleet-cli"], "only the edge wires a forge");
    for pure in ["fleet-api", "fleet-controllers"] {
        assert!(
            rules.crates[pure].probes.iter().any(|p| p == "fleet-forge"),
            "{pure} probes the forge edge"
        );
    }
    assert_eq!(
        rules
            .status_writers
            .get("fleet_api::store::FieldManager::ChangeRequestController"),
        Some(&vec![
            "fleet-api".to_string(),
            "fleet-controllers".to_string()
        ])
    );
    let cli = &rules.crates["fleet-cli"];
    for method in ["delete_fenced", "change", "mark_ready"] {
        assert!(
            cli.forbidden_methods.iter().any(|m| m == method),
            "the CLI may not call {method}"
        );
    }
}

/// A forge adapter is below the controllers and above nothing else: it may
/// not reach a controller, and a controller may not reach it. The same
/// shape as the runtime boundary, proven for the second adapter kind.
#[test]
fn a_forge_adapter_and_the_controllers_cannot_reach_each_other() {
    let mut rules = RULES.to_string();
    rules.push_str("\n[crates.fx-forge]\nrole = \"adapter\"\ndeps = [\"fx-api\"]\n");
    let forge_manifest = manifest("fx-forge", &["fx-api"]);
    expect(
        "forge-clean",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-forge/fixture.toml", &forge_manifest),
            (
                "crates/fx-forge/src/lib.rs",
                "pub fn change() -> fx_api::FieldManager { fx_api::FieldManager::Worker }\n",
            ),
        ],
        &[],
    );
    expect(
        "forge-reaches-controller",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-forge/fixture.toml",
                &manifest("fx-forge", &["fx-api", "fx-ctl"]),
            ),
            (
                "crates/fx-forge/src/lib.rs",
                "pub fn change() { let _ = fx_ctl::plan(); }\n",
            ),
        ],
        &[("undeclared-edge", "fx-forge")],
    );
    expect(
        "controller-reaches-forge",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-forge/fixture.toml", &forge_manifest),
            ("crates/fx-forge/src/lib.rs", "pub fn change() {}\n"),
            (
                "crates/fx-ctl/fixture.toml",
                &manifest("fx-ctl", &["fx-api", "fx-forge"]),
            ),
            (CTL, &ctl("pub fn x() { fx_forge::change(); }\n")),
        ],
        &[("undeclared-edge", "fx-ctl")],
    );
}

/// A second controller's field manager is as guarded as the first: only the
/// crates its rule lists may name it, whatever path they reach it by.
#[test]
fn only_the_listed_crates_name_a_second_controllers_field_manager() {
    let rules = format!(
        "{}\n[status_writers]\n\"fx_api::store::FieldManager::Ctl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::CrCtl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::Cli\" = [\"fx-api\", \"fx-mgr\"]\n",
        RULES.replace(
            "[status_writers]\n\"fx_api::store::FieldManager::Ctl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::Cli\" = [\"fx-api\", \"fx-mgr\"]\n",
            ""
        )
    );
    let api = "//! ```compile_fail\n//! use fx_store::Store;\n//! ```\npub mod store {\n    pub enum FieldManager { Ctl, CrCtl, Worker }\n}\npub use store::FieldManager;\nuse serde::Serialize;\n";
    expect(
        "second-writer-clean",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                CTL,
                &ctl(
                    "use fx_api::FieldManager;\npub fn plan() -> FieldManager { FieldManager::CrCtl }\n",
                ),
            ),
        ],
        &[],
    );
    expect(
        "second-writer-from-store",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-store/src/lib.rs",
                "pub struct S;\npub fn read() { let _ = fx_api::FieldManager::CrCtl; }\n",
            ),
        ],
        &[("status-writer", "fx-store")],
    );
    expect(
        "second-writer-from-manager",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-mgr/src/lib.rs",
                "use fx_api::store::FieldManager as F;\npub fn run() { let _ = F::CrCtl; }\n",
            ),
        ],
        &[("status-writer", "fx-mgr")],
    );
}

#[test]
fn a_forbidden_adapter_method_is_caught() {
    let rules = RULES.replace(
        "[crates.fx-store]\nrole = \"adapter\"\ndeps = [\"fx-api\"]",
        "[crates.fx-store]\nrole = \"adapter\"\ndeps = [\"fx-api\"]\nforbidden_methods = [\"expose\"]",
    );
    assert_ne!(rules, RULES);
    expect(
        "expose",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-store/src/lib.rs",
                "pub struct S;\npub fn read() {}\npub fn leak(v: &String) { let _ = v.expose(); }\n",
            ),
        ],
        &[("cli-method", "fx-store")],
    );
}

#[test]
fn core_controllers_cannot_reach_a_runtime_adapter() {
    let mut rules = RULES.to_string();
    rules.push_str("\n[crates.fx-runtime]\nrole = \"adapter\"\ndeps = [\"fx-api\"]\n");
    expect(
        "runtime-core-boundary",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-runtime/fixture.toml",
                &manifest("fx-runtime", &["fx-api"]),
            ),
            ("crates/fx-runtime/src/lib.rs", "pub fn call() {}"),
            (
                "crates/fx-ctl/fixture.toml",
                &manifest("fx-ctl", &["fx-api", "fx-runtime"]),
            ),
            (CTL, &ctl("pub fn x(){ fx_runtime::call(); }")),
        ],
        &[("undeclared-edge", "fx-ctl")],
    );
}

#[test]
fn a_runtime_adapter_cannot_claim_the_tasks_status_identity() {
    let mut rules = RULES.to_string();
    rules.push_str("\n[crates.fx-runtime]\nrole = \"adapter\"\ndeps = [\"fx-api\"]\n");
    expect(
        "runtime-status-capability",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-runtime/fixture.toml",
                &manifest("fx-runtime", &["fx-api"]),
            ),
            (
                "crates/fx-runtime/src/lib.rs",
                "use fx_api::FieldManager; pub fn x(){let _ = FieldManager::Ctl;}",
            ),
        ],
        &[("status-writer", "fx-runtime")],
    );
}

#[test]
fn runtime_effect_capability_cannot_leak_to_an_observer_adapter() {
    let rules = format!(
        "{RULES}\n[[restricted]]\npath = \"fx_api::Runtime\"\nallowed = [\"fx-api\", \"fx-ctl\", \"fx-mgr\"]\nwhy = \"only the manager performs runtime effects\"\n"
    );
    expect(
        "runtime-effect-negative",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-store/src/lib.rs",
                "use fx_api::Runtime;\npub fn mutate(p: &dyn Runtime) {}\n",
            ),
        ],
        &[("restricted", "fx-store")],
    );
    expect(
        "runtime-effect-positive",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-mgr/src/lib.rs",
                "use fx_api::Runtime;\npub fn perform(p: &dyn Runtime) {}\n",
            ),
        ],
        &[],
    );
}

/// The mark-ready step's rule, against the real rules file: the Forge port,
/// which carries Fleet's one forge write, is held by the API that defines
/// it, the adapters that implement it, the CLI that wires it and the
/// manager that performs it, and by nothing else.
#[test]
fn the_rules_keep_the_forge_port_and_its_one_write_with_the_manager() {
    let rules = load_rules(&repo_root().join("crates").join("architecture.toml")).expect("rules");
    let port = rules
        .restricted
        .iter()
        .find(|r| r.path == "fleet_api::forge::Forge")
        .expect("the Forge port is restricted");
    assert_eq!(
        port.allowed,
        vec![
            "fleet-api".to_string(),
            "fleet-manager".to_string(),
            "fleet-forge".to_string(),
            "fleet-cli".to_string(),
        ]
    );
    for excluded in ["fleet-controllers", "fleet-store", "fleet-runtime"] {
        assert!(
            !port.allowed.iter().any(|c| c == excluded),
            "{excluded} must not hold the port"
        );
    }
}

/// A forge effect capability is held like the runtime's: an observer
/// adapter that takes the port is caught, the manager that performs the
/// effect is not, and a pure controller cannot hold it either.
#[test]
fn forge_effect_capability_cannot_leak_to_an_observer_adapter_or_a_controller() {
    let rules = format!(
        "{RULES}\n[[restricted]]\npath = \"fx_api::Forge\"\nallowed = [\"fx-api\", \"fx-mgr\"]\nwhy = \"only the manager performs the mark-ready effect\"\n"
    );
    let api = "//! ```compile_fail\n//! use fx_store::Store;\n//! ```\npub mod store {\n    pub enum FieldManager { Ctl, Worker, Cli }\n}\npub use store::FieldManager;\npub trait Forge { fn mark_ready(&self); }\nuse serde::Serialize;\n";
    expect(
        "forge-effect-from-store",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-store/src/lib.rs",
                "use fx_api::Forge;\npub struct S;\npub fn read() {}\npub fn mutate(p: &dyn Forge) { p.mark_ready(); }\n",
            ),
        ],
        &[("restricted", "fx-store")],
    );
    expect(
        "forge-effect-from-controller",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                CTL,
                &ctl("use fx_api::Forge;\npub fn plan(p: &dyn Forge) { p.mark_ready(); }\n"),
            ),
        ],
        &[("restricted", "fx-ctl")],
    );
    expect(
        "forge-effect-from-manager",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-mgr/src/lib.rs",
                "use fx_api::Forge;\npub fn run(p: &dyn Forge) { let _ = fx_ctl::plan(); p.mark_ready(); }\n",
            ),
        ],
        &[],
    );
}

/// The Gateway's first step, against the real rules file: fleet-gateway is
/// an adapter reaching only the API and the spawner, the CLI is the only
/// crate that wires it, both pure crates probe its edge, the Gateway port
/// with its two writes is held by the API, the adapters, the manager and
/// the CLI alone, and the CLI may not call its reads or its post itself.
#[test]
fn the_rules_keep_the_gateway_below_its_port_and_its_writes_with_the_manager() {
    let rules = load_rules(&repo_root().join("crates").join("architecture.toml")).expect("rules");
    let gateway = rules
        .crates
        .get("fleet-gateway")
        .expect("fleet-gateway is governed");
    assert_eq!(gateway.role, arch_rules::Role::Adapter);
    assert_eq!(
        gateway.deps,
        vec!["fleet-api".to_string(), "fleet-exec".to_string()]
    );
    let dependants: Vec<&String> = rules
        .crates
        .iter()
        .filter(|(_, r)| r.deps.iter().any(|d| d == "fleet-gateway"))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        dependants,
        vec!["fleet-cli"],
        "only the edge wires a gateway"
    );
    for pure in ["fleet-api", "fleet-controllers"] {
        assert!(
            rules.crates[pure]
                .probes
                .iter()
                .any(|p| p == "fleet-gateway"),
            "{pure} probes the gateway edge"
        );
    }
    let port = rules
        .restricted
        .iter()
        .find(|r| r.path == "fleet_api::gateway::Gateway")
        .expect("the Gateway port is restricted");
    assert_eq!(
        port.allowed,
        vec![
            "fleet-api".to_string(),
            "fleet-manager".to_string(),
            "fleet-gateway".to_string(),
            "fleet-cli".to_string(),
        ]
    );
    for excluded in [
        "fleet-controllers",
        "fleet-store",
        "fleet-runtime",
        "fleet-forge",
    ] {
        assert!(
            !port.allowed.iter().any(|c| c == excluded),
            "{excluded} must not hold the port"
        );
    }
    let cli = &rules.crates["fleet-cli"];
    for method in ["history", "capabilities", "post"] {
        assert!(
            cli.forbidden_methods.iter().any(|m| m == method),
            "the CLI may not call {method}"
        );
    }
}

/// A chat write capability is held like the forge's: an observer adapter or
/// a pure controller that takes the Gateway port is caught, the manager
/// that performs the effect is not, and a second adapter cannot reach it
/// through the first.
#[test]
fn gateway_effect_capability_cannot_leak_to_an_observer_adapter_or_a_controller() {
    let rules = format!(
        "{RULES}\n[[restricted]]\npath = \"fx_api::Gateway\"\nallowed = [\"fx-api\", \"fx-mgr\"]\nwhy = \"only the manager performs the post effect\"\n"
    );
    let api = "//! ```compile_fail\n//! use fx_store::Store;\n//! ```\npub mod store {\n    pub enum FieldManager { Ctl, Worker, Cli }\n}\npub use store::FieldManager;\npub trait Gateway { fn post(&self); fn open(&self); }\nuse serde::Serialize;\n";
    expect(
        "gateway-effect-from-store",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-store/src/lib.rs",
                "use fx_api::Gateway;\npub struct S;\npub fn read() {}\npub fn mutate(p: &dyn Gateway) { p.post(); }\n",
            ),
        ],
        &[("restricted", "fx-store")],
    );
    expect(
        "gateway-effect-from-controller",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                CTL,
                &ctl("use fx_api::Gateway;\npub fn plan(p: &dyn Gateway) { p.open(); }\n"),
            ),
        ],
        &[("restricted", "fx-ctl")],
    );
    expect(
        "gateway-effect-from-manager",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-mgr/src/lib.rs",
                "use fx_api::Gateway;\npub fn run(p: &dyn Gateway) { let _ = fx_ctl::plan(); p.post(); }\n",
            ),
        ],
        &[],
    );
    // A gateway adapter beside the forge one: each is below the controllers
    // and above nothing else, and neither reaches the other.
    let mut rules = RULES.to_string();
    rules.push_str(
        "\n[crates.fx-forge]\nrole = \"adapter\"\ndeps = [\"fx-api\"]\n\n[crates.fx-gateway]\nrole = \"adapter\"\ndeps = [\"fx-api\"]\n",
    );
    expect(
        "gateway-reaches-forge",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-forge/fixture.toml",
                &manifest("fx-forge", &["fx-api"]),
            ),
            ("crates/fx-forge/src/lib.rs", "pub fn change() {}\n"),
            (
                "crates/fx-gateway/fixture.toml",
                &manifest("fx-gateway", &["fx-api", "fx-forge"]),
            ),
            (
                "crates/fx-gateway/src/lib.rs",
                "pub fn post() { fx_forge::change(); }\n",
            ),
        ],
        &[("undeclared-edge", "fx-gateway")],
    );
    expect(
        "controller-reaches-gateway",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-forge/fixture.toml",
                &manifest("fx-forge", &["fx-api"]),
            ),
            ("crates/fx-forge/src/lib.rs", "pub fn change() {}\n"),
            (
                "crates/fx-gateway/fixture.toml",
                &manifest("fx-gateway", &["fx-api"]),
            ),
            ("crates/fx-gateway/src/lib.rs", "pub fn post() {}\n"),
            (
                "crates/fx-ctl/fixture.toml",
                &manifest("fx-ctl", &["fx-api", "fx-gateway"]),
            ),
            (CTL, &ctl("pub fn x() { fx_gateway::post(); }\n")),
        ],
        &[("undeclared-edge", "fx-ctl")],
    );
}

/// The Forge controller's rules, against the real rules file: its field
/// manager is named by the API and the controllers alone, and the relay
/// write and the login read are the manager's, never the CLI's.
#[test]
fn the_rules_keep_the_forge_controllers_writes_in_the_controllers() {
    let rules = load_rules(&repo_root().join("crates").join("architecture.toml")).expect("rules");
    assert_eq!(
        rules
            .status_writers
            .get("fleet_api::store::FieldManager::ForgeController"),
        Some(&vec![
            "fleet-api".to_string(),
            "fleet-controllers".to_string()
        ])
    );
    assert_eq!(
        rules
            .status_writers
            .get("fleet_api::store::FieldManager::Worker"),
        Some(&vec!["fleet-api".to_string(), "fleet-manager".to_string()]),
        "declare, like result, is the manager's verb"
    );
    let cli = &rules.crates["fleet-cli"];
    for method in ["record_relay", "whoami", "append_inbox"] {
        assert!(
            cli.forbidden_methods.iter().any(|m| m == method),
            "the CLI may not call {method}"
        );
    }
}

/// A third controller over an existing kind is as guarded as the first two:
/// only the listed crates name its manager, and its typed write is a
/// manager method the CLI may not call.
#[test]
fn a_second_controller_over_one_kind_is_named_by_the_controllers_alone() {
    let rules = format!(
        "{}\n[status_writers]\n\"fx_api::store::FieldManager::Ctl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::FgCtl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::Cli\" = [\"fx-api\", \"fx-mgr\"]\n",
        RULES
            .replace(
                "[status_writers]\n\"fx_api::store::FieldManager::Ctl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::Cli\" = [\"fx-api\", \"fx-mgr\"]\n",
                ""
            )
            .replace(
                "forbidden_methods = [\"update_status\", \"append_inbox\"]",
                "forbidden_methods = [\"update_status\", \"append_inbox\", \"record_relay\"]"
            )
    );
    let api = "//! ```compile_fail\n//! use fx_store::Store;\n//! ```\npub mod store {\n    pub enum FieldManager { Ctl, FgCtl, Worker, Cli }\n}\npub use store::FieldManager;\nuse serde::Serialize;\n";
    expect(
        "forge-writer-clean",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                CTL,
                &ctl(
                    "use fx_api::FieldManager;\npub fn plan() -> FieldManager { FieldManager::Ctl }\npub fn relay() -> FieldManager { FieldManager::FgCtl }\n",
                ),
            ),
        ],
        &[],
    );
    expect(
        "forge-writer-from-store",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-store/src/lib.rs",
                "pub struct S;\npub fn read() { let _ = fx_api::FieldManager::FgCtl; }\n",
            ),
        ],
        &[("status-writer", "fx-store")],
    );
    expect(
        "forge-writer-from-manager",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-mgr/src/lib.rs",
                "use fx_api::store::FieldManager as F;\npub fn run() { let _ = F::FgCtl; }\n",
            ),
        ],
        &[("status-writer", "fx-mgr")],
    );
    expect(
        "relay-from-cli",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-cli/src/main.rs",
                "fn main() { let s = fx_store::S; s.record_relay(); fx_mgr::run(); }\n",
            ),
        ],
        &[("cli-method", "fx-cli")],
    );
}

#[test]
fn plugin_identity_and_execution_stay_behind_the_gate_and_manager() {
    let rules = RULES.replace("forbidden_methods = [\"update_status\", \"append_inbox\"]", "forbidden_methods = [\"update_status\", \"append_inbox\", \"call\", \"admit\", \"plugin_mutate\"]")
        .replace("[status_writers]", "[status_writers]\n\"fx_api::store::FieldManager::Plugin\" = [\"fx-api\", \"fx-ctl\"]");
    let api = "//! ```compile_fail\n//! use fx_store::Store;\n//! ```\npub mod store { pub enum FieldManager { Ctl, Worker, Cli, Plugin(String) } }\npub use store::FieldManager;\nuse serde::Serialize;\n";
    expect(
        "plugin-writer-from-manager",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-mgr/src/lib.rs",
                "pub fn run() { let _ = fx_api::FieldManager::Plugin(\"issues\".into()); }\n",
            ),
        ],
        &[("status-writer", "fx-mgr")],
    );
    for method in ["call", "admit", "plugin_mutate"] {
        expect(
            &format!("plugin-cli-{method}"),
            &[
                ("architecture.toml", &rules),
                ("crates/fx-api/src/lib.rs", api),
                (
                    "crates/fx-cli/src/main.rs",
                    &format!("fn main() {{ let s = fx_store::S; s.{method}(); fx_mgr::run(); }}\n"),
                ),
            ],
            &[("cli-method", "fx-cli")],
        );
    }
}

/// The Gateway mirror's rules, against the real rules file: the two new
/// controllers' field managers are named by the API and the controllers
/// alone, the listener's by the API, the manager's verb and the store that
/// checks it, and the four typed mutations are manager methods the CLI may
/// not call.
#[test]
fn the_rules_keep_the_mirrors_writes_with_the_controllers_and_the_receive_verb() {
    let rules = load_rules(&repo_root().join("crates").join("architecture.toml")).expect("rules");
    for writer in [
        "fleet_api::store::FieldManager::ConversationController",
        "fleet_api::store::FieldManager::ListenerController",
    ] {
        assert_eq!(
            rules.status_writers.get(writer),
            Some(&vec![
                "fleet-api".to_string(),
                "fleet-controllers".to_string()
            ]),
            "{writer}"
        );
    }
    assert_eq!(
        rules
            .status_writers
            .get("fleet_api::store::FieldManager::Listener"),
        Some(&vec![
            "fleet-api".to_string(),
            "fleet-manager".to_string(),
            "fleet-store".to_string()
        ]),
        "the receive verb is the manager's, and the store checks its caller"
    );
    let cli = &rules.crates["fleet-cli"];
    for method in [
        "receive_event",
        "record_received",
        "record_beat",
        "record_refusal",
        // The conversation Task's create, fenced by the lease: the
        // manager's, performing ConversationController's plan.
        "create_fenced",
    ] {
        assert!(
            cli.forbidden_methods.iter().any(|m| m == method),
            "the CLI may not call {method}"
        );
    }
}

/// The fenced create is a store write like the typed mutations: a CLI that
/// calls it is caught by the method rule, with the real rules' spelling.
#[test]
fn the_cli_may_not_create_fenced() {
    let rules = RULES.replace(
        "forbidden_methods = [\"update_status\", \"append_inbox\"]",
        "forbidden_methods = [\"update_status\", \"append_inbox\", \"create_fenced\"]",
    );
    expect(
        "cli-create-fenced",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-cli/src/main.rs",
                "fn main() { let s = fx_store::S; s.create_fenced(); fx_mgr::run(); }\n",
            ),
        ],
        &[("cli-method", "fx-cli")],
    );
}

/// A listener-shaped writer, one the manager's verb speaks as without a
/// lease, is guarded like `Cli`: the manager and the store may name it,
/// the CLI and an adapter may not; and a fourth controller's manager is
/// the controllers' alone, with its typed drain a method the CLI may not
/// call.
#[test]
fn a_verb_writer_and_a_mirror_controller_are_named_by_the_listed_crates_alone() {
    let rules = format!(
        "{}\n[status_writers]\n\"fx_api::store::FieldManager::Ctl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::ConvCtl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::Cli\" = [\"fx-api\", \"fx-mgr\"]\n\"fx_api::store::FieldManager::Listener\" = [\"fx-api\", \"fx-mgr\", \"fx-store\"]\n",
        RULES
            .replace(
                "[status_writers]\n\"fx_api::store::FieldManager::Ctl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::Cli\" = [\"fx-api\", \"fx-mgr\"]\n",
                ""
            )
            .replace(
                "forbidden_methods = [\"update_status\", \"append_inbox\"]",
                "forbidden_methods = [\"update_status\", \"append_inbox\", \"receive_event\", \"record_received\"]"
            )
    );
    let api = "//! ```compile_fail\n//! use fx_store::Store;\n//! ```\npub mod store {\n    pub enum FieldManager { Ctl, ConvCtl, Worker, Cli, Listener(String) }\n}\npub use store::FieldManager;\nuse serde::Serialize;\n";
    expect(
        "mirror-writers-clean",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                CTL,
                &ctl(
                    "use fx_api::FieldManager;\npub fn plan() -> FieldManager { FieldManager::Ctl }\npub fn mirror() -> FieldManager { FieldManager::ConvCtl }\n",
                ),
            ),
            (
                "crates/fx-mgr/src/lib.rs",
                "pub fn run() { let _ = fx_ctl::plan(); let _ = fx_api::FieldManager::Listener(\"a\".into()); }\n",
            ),
            (
                "crates/fx-store/src/lib.rs",
                "use std::fs;\npub struct S;\npub fn read() { let _ = fs::read(\"x\"); let _ = fx_api::FieldManager::Listener(\"a\".into()); }\n",
            ),
        ],
        &[],
    );
    expect(
        "listener-writer-from-cli",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-cli/src/main.rs",
                "fn main() { let _ = fx_api::FieldManager::Listener(\"a\".into()); fx_mgr::run(); }\n",
            ),
        ],
        &[("status-writer", "fx-cli")],
    );
    expect(
        "mirror-writer-from-manager",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-mgr/src/lib.rs",
                "use fx_api::store::FieldManager as F;\npub fn run() { let _ = F::ConvCtl; }\n",
            ),
        ],
        &[("status-writer", "fx-mgr")],
    );
    expect(
        "listener-writer-from-controller",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                CTL,
                &ctl(
                    "use fx_api::FieldManager;\npub fn plan() -> FieldManager { FieldManager::Listener(\"a\".into()) }\n",
                ),
            ),
        ],
        &[("status-writer", "fx-ctl")],
    );
    for method in ["receive_event", "record_received"] {
        expect(
            &format!("mirror-{method}-from-cli"),
            &[
                ("architecture.toml", &rules),
                ("crates/fx-api/src/lib.rs", api),
                (
                    "crates/fx-cli/src/main.rs",
                    &format!("fn main() {{ let s = fx_store::S; s.{method}(); fx_mgr::run(); }}\n"),
                ),
            ],
            &[("cli-method", "fx-cli")],
        );
    }
}

/// The Gateway controller's third step, against the real rules file: its
/// field manager is named by the API and the controllers alone, beside the
/// Forge controller's, and the relay write both share stays a manager
/// method the CLI may not call.
#[test]
fn the_rules_keep_the_gateway_controllers_writes_in_the_controllers() {
    let rules = load_rules(&repo_root().join("crates").join("architecture.toml")).expect("rules");
    for writer in [
        "fleet_api::store::FieldManager::ForgeController",
        "fleet_api::store::FieldManager::GatewayController",
    ] {
        assert_eq!(
            rules.status_writers.get(writer),
            Some(&vec![
                "fleet-api".to_string(),
                "fleet-controllers".to_string()
            ]),
            "{writer}"
        );
    }
    let cli = &rules.crates["fleet-cli"];
    for method in ["record_relay", "append_inbox"] {
        assert!(
            cli.forbidden_methods.iter().any(|m| m == method),
            "the CLI may not call {method}"
        );
    }
}

/// A third controller over one kind, sharing the second's typed write, is
/// as guarded as the first two: only the listed crates name its manager,
/// whatever path they reach it by, and the shared write stays out of the
/// CLI.
#[test]
fn a_third_controller_over_one_kind_is_named_by_the_controllers_alone() {
    let rules = format!(
        "{}\n[status_writers]\n\"fx_api::store::FieldManager::Ctl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::FgCtl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::GwCtl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::Cli\" = [\"fx-api\", \"fx-mgr\"]\n",
        RULES
            .replace(
                "[status_writers]\n\"fx_api::store::FieldManager::Ctl\" = [\"fx-api\", \"fx-ctl\"]\n\"fx_api::store::FieldManager::Cli\" = [\"fx-api\", \"fx-mgr\"]\n",
                ""
            )
            .replace(
                "forbidden_methods = [\"update_status\", \"append_inbox\"]",
                "forbidden_methods = [\"update_status\", \"append_inbox\", \"record_relay\"]"
            )
    );
    let api = "//! ```compile_fail\n//! use fx_store::Store;\n//! ```\npub mod store {\n    pub enum FieldManager { Ctl, FgCtl, GwCtl, Worker, Cli }\n}\npub use store::FieldManager;\nuse serde::Serialize;\n";
    expect(
        "gateway-writer-clean",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                CTL,
                &ctl(
                    "use fx_api::FieldManager;\npub fn plan() -> FieldManager { FieldManager::Ctl }\npub fn relay() -> FieldManager { FieldManager::FgCtl }\npub fn talk() -> FieldManager { FieldManager::GwCtl }\n",
                ),
            ),
        ],
        &[],
    );
    expect(
        "gateway-writer-from-store",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-store/src/lib.rs",
                "pub struct S;\npub fn read() { let _ = fx_api::FieldManager::GwCtl; }\n",
            ),
        ],
        &[("status-writer", "fx-store")],
    );
    expect(
        "gateway-writer-from-manager",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-mgr/src/lib.rs",
                "use fx_api::store::FieldManager as F;\npub fn run() { let _ = F::GwCtl; }\n",
            ),
        ],
        &[("status-writer", "fx-mgr")],
    );
    expect(
        "gateway-writer-from-cli",
        &[
            ("architecture.toml", &rules),
            ("crates/fx-api/src/lib.rs", api),
            (
                "crates/fx-cli/src/main.rs",
                "fn main() { let _ = fx_api::FieldManager::GwCtl; fx_mgr::run(); }\n",
            ),
        ],
        &[("status-writer", "fx-cli")],
    );
}

/// Another component in the repository (thurbox, a runtime) is held to the
/// crate graph, not to Fleet's own source rules: it may start a process or
/// name a status writer, but it reaches no workspace crate its rule does not
/// list.
#[test]
fn a_component_keeps_the_graph_but_not_fleets_source_rules() {
    let rules = format!("{RULES}\n[crates.fx-ui]\nrole = \"component\"\ndeps = []\n");
    let base = [
        ("architecture.toml", rules.as_str()),
        ("crates/fx-ui/fixture.toml", "[package]\nname = \"fx-ui\"\n"),
        (
            "crates/fx-ui/src/main.rs",
            "fn main() { let _ = std::process::Command::new(\"fleet\"); }\n",
        ),
    ];
    expect("component-clean", &base, &[]);

    let manifest = manifest("fx-ui", &["fx-api"]);
    let mut dirty: Vec<(&str, &str)> = base.to_vec();
    dirty.push(("crates/fx-ui/fixture.toml", &manifest));
    dirty.push((
        "crates/fx-ui/src/main.rs",
        "fn main() { let _ = fx_api::FieldManager::Cli; }\n",
    ));
    expect("component-edge", &dirty, &[("undeclared-edge", "fx-ui")]);
}

/// A dependency inherited from `[workspace.dependencies]` under another name
/// is still the workspace crate it names: `mux.workspace = true`, where the
/// root declares `mux = { package = "fx-store" }`, is an edge to fx-store.
#[test]
fn a_renamed_workspace_dependency_is_the_crate_it_names() {
    let root = "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\nmux = { package = \"fx-store\", path = \"crates/fx-store\" }\n";
    let ctl = "[package]\nname = \"fx-ctl\"\n\n[dependencies]\nfx-api = { path = \"../fx-api\" }\nmux.workspace = true\n";
    expect(
        "renamed-workspace-dep",
        &[("fixture.toml", root), ("crates/fx-ctl/fixture.toml", ctl)],
        &[("undeclared-edge", "fx-ctl")],
    );
}
