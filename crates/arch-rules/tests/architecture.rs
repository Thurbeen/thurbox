use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use arch_rules::{Code, Violation, Workspace, check, check_uncached, load_rules};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

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
        "otel-env",
        "fleet-dev",
    ] {
        assert!(
            names.contains(expected),
            "{expected} missing from {names:?}"
        );
    }
    // Only `check`: resolving every component name afresh takes ten times as
    // long, and the fixtures already compare it with the cache.
    let violations = check(&ws, &rules);
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
applies_to = ["pure", "adapter", "orchestration", "edge"]
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

#[test]
fn a_function_does_not_shadow_a_glob_imported_module() {
    let src = ctl("use std::*;\nfn fs() {}\npub fn x() { let _ = fs::read(\"p\"); }\n");
    expect("value-shadow", &[(CTL, &src)], &[("pure-io", "fx-ctl")]);
}

#[test]
fn a_module_shadows_a_glob_imported_module() {
    let src = ctl("use std::*;\nmod fs { pub fn read() {} }\npub fn x() { fs::read(); }\n");
    expect("type-shadow", &[(CTL, &src)], &[]);
}

#[test]
fn a_resolution_cut_short_is_not_remembered() {
    let mut body = String::from("pub mod m0 { pub use std::*; }\n");
    for level in 1..30 {
        body.push_str(&format!(
            "pub mod m{level} {{ pub use super::m{}::*; }}\n",
            level - 1
        ));
    }
    body.push_str("pub fn deep() { let _ = crate::m29::fs::read(\"p\"); }\n");
    for level in 0..=22 {
        body.push_str(&format!(
            "pub fn at{level}() {{ let _ = crate::m{level}::fs::read(\"p\"); }}\n"
        ));
    }
    let src = ctl(&body);
    expect("cut-short", &[(CTL, &src)], &[("pure-io", "fx-ctl")]);
}

#[test]
fn a_binary_module_does_not_shadow_a_library_glob() {
    let lib = ctl("use std::*;\npub fn x() { let _ = fs::read(\"p\"); }\n");
    expect(
        "bin-shadow",
        &[
            (CTL, &lib),
            ("crates/fx-ctl/src/main.rs", "mod fs {}\nfn main() {}\n"),
        ],
        &[("pure-io", "fx-ctl")],
    );
}

#[test]
fn a_binary_submodule_does_not_shadow_a_library_submodule() {
    let lib =
        ctl("pub mod net {\n    use std::*;\n    pub fn x() { let _ = fs::read(\"p\"); }\n}\n");
    expect(
        "bin-submodule-shadow",
        &[
            (CTL, &lib),
            ("crates/fx-ctl/src/main.rs", "mod net;\nfn main() {}\n"),
            ("crates/fx-ctl/src/net.rs", "mod fs {}\n"),
        ],
        &[("pure-io", "fx-ctl")],
    );
}

#[test]
fn a_cfg_gated_module_does_not_shadow_a_glob() {
    let src =
        ctl("use std::*;\n#[cfg(test)]\nmod fs {}\npub fn x() { let _ = fs::read(\"p\"); }\n");
    expect("cfg-shadow", &[(CTL, &src)], &[("pure-io", "fx-ctl")]);
}

#[test]
fn a_file_two_targets_declare_is_indexed_in_each() {
    expect(
        "shared-file",
        &[
            (CTL, &ctl("pub mod cli;\n")),
            ("crates/fx-ctl/src/cli.rs", "pub use std::*;\n"),
            (
                "crates/fx-ctl/src/main.rs",
                "mod cli;\nfn main() { let _ = cli::fs::read(\"p\"); }\n",
            ),
        ],
        &[("pure-io", "fx-ctl")],
    );
}

#[test]
fn a_path_attribute_module_is_followed() {
    expect(
        "path-attr",
        &[
            ("crates/fx-ctl/src/other/x.rs", "pub use std::*;\n"),
            (
                "crates/fx-ctl/src/main.rs",
                "#[path = \"other/x.rs\"]\nmod cli;\nfn main() { let _ = cli::fs::read(\"p\"); }\n",
            ),
        ],
        &[("pure-io", "fx-ctl")],
    );
}

#[test]
fn a_function_local_import_does_not_hide_a_module_read() {
    let src = ctl(
        "use std::*;\npub mod m { pub fn read(_: &str) {} }\nfn a() { use crate::m as fs; }\npub fn b() { let _ = fs::read(\"p\"); }\n",
    );
    expect("fn-local-use", &[(CTL, &src)], &[("pure-io", "fx-ctl")]);
}

#[test]
fn cfg_alternative_imports_are_both_judged() {
    let rules = format!(
        "{RULES}\n[[restricted]]\napplies_to = [\"pure\", \"adapter\", \"orchestration\", \"edge\"]\npath = \"fx_api::danger\"\nallowed = []\nwhy = \"fixture\"\n"
    );
    expect(
        "cfg-alternatives",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-store/src/lib.rs",
                "pub mod m { pub fn danger() {} }\n#[cfg(unix)]\nuse fx_api as sys;\n#[cfg(not(unix))]\nuse crate::m as sys;\npub fn b() { sys::danger(); }\n",
            ),
        ],
        &[("restricted", "fx-store")],
    );
}

#[test]
fn a_function_local_module_does_not_shadow_a_module_of_the_same_name() {
    let src = ctl(
        "pub mod x {\n    use std::*;\n    pub fn r() { let _ = fs::read(\"p\"); }\n}\nfn f() { mod x { mod fs {} } }\n",
    );
    expect("fn-local-mod", &[(CTL, &src)], &[("pure-io", "fx-ctl")]);
}

#[test]
fn a_path_attribute_cycle_terminates() {
    let src = ctl("#[cfg(any())]\n#[path = \"lib.rs\"]\nmod disabled;\npub fn x() {}\n");
    expect("path-cycle", &[(CTL, &src)], &[]);
}

#[test]
fn an_unconditional_import_still_shadows_a_glob_beside_a_weak_one() {
    let src = ctl(
        "use std::*;\npub mod safe { pub fn read() {} }\nuse crate::safe as fs;\nfn a() { use crate::safe as fs; }\npub fn b() { fs::read(); }\n",
    );
    expect("weak-beside-strong", &[(CTL, &src)], &[]);
}

#[test]
fn a_weak_import_alone_keeps_the_glob_meaning() {
    let src = ctl(
        "pub mod n { pub fn read(_: &str) {} }\npub mod m {\n    pub use std::*;\n    fn f() { use crate::n as fs; }\n}\npub fn x() { let _ = m::fs::read(\"p\"); }\n",
    );
    expect("weak-alone", &[(CTL, &src)], &[("pure-io", "fx-ctl")]);
}

#[test]
fn an_unconditional_reexport_still_shadows_a_glob_beside_a_weak_one() {
    let src = ctl(
        "pub mod safe { pub fn read() {} }\npub mod m {\n    pub use std::*;\n    pub use crate::safe as fs;\n    fn f() { use crate::safe as fs; }\n}\npub fn x() { m::fs::read(); }\n",
    );
    expect("weak-beside-strong-reexport", &[(CTL, &src)], &[]);
}

#[test]
fn a_type_imported_under_a_macro_name_does_not_hide_the_macro() {
    let src = ctl("use std::fmt::Error as println;\npub fn b() { println!(\"x\"); }\n");
    expect("type-as-macro", &[(CTL, &src)], &[("pure-io", "fx-ctl")]);
}

#[test]
fn a_reexported_type_named_like_a_macro_does_not_hide_it() {
    let src = ctl(
        "pub mod m {\n    pub use std::*;\n    pub use std::fmt::Error as println;\n}\npub fn b() { m::println!(\"x\"); }\n",
    );
    expect(
        "reexport-type-as-macro",
        &[(CTL, &src)],
        &[("pure-io", "fx-ctl")],
    );
}

#[test]
fn a_renamed_import_of_a_reexport_read_as_a_macro_is_judged() {
    let src = ctl(
        "pub mod m {\n    pub use std::*;\n    pub use std::fmt::Error as println;\n}\nuse crate::m::println as say;\npub fn b() { say!(\"x\"); }\n",
    );
    expect(
        "renamed-reexport-macro",
        &[(CTL, &src)],
        &[("pure-io", "fx-ctl")],
    );
}

#[test]
fn a_reexported_renamed_macro_read_through_a_path_is_judged() {
    let src = ctl(
        "pub mod m {\n    pub use std::*;\n    pub use std::fmt::Error as println;\n}\npub mod n {\n    pub use crate::m::println as say;\n}\npub fn b() { n::say!(\"x\"); }\n",
    );
    expect(
        "path-renamed-reexport-macro",
        &[(CTL, &src)],
        &[("pure-io", "fx-ctl")],
    );
}

#[test]
fn a_macro_imported_under_another_name_is_judged() {
    let src = ctl("use std::println as say;\npub fn b() { say!(\"x\"); }\n");
    expect("macro-alias", &[(CTL, &src)], &[("pure-io", "fx-ctl")]);
}

#[test]
fn a_struct_does_not_shadow_a_glob_imported_macro() {
    let rules = format!(
        "{RULES}\n[[restricted]]\napplies_to = [\"pure\", \"adapter\", \"orchestration\", \"edge\"]\npath = \"fx_api::danger\"\nallowed = []\nwhy = \"fixture\"\n"
    );
    expect(
        "macro-shadow",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-store/src/lib.rs",
                "use fx_api::*;\npub struct danger;\npub fn x() { danger!(); }\n",
            ),
        ],
        &[("restricted", "fx-store")],
    );
}

#[test]
fn a_module_does_not_shadow_a_glob_imported_macro() {
    let rules = format!(
        "{RULES}\n[[restricted]]\napplies_to = [\"pure\", \"adapter\", \"orchestration\", \"edge\"]\npath = \"fx_api::danger\"\nallowed = []\nwhy = \"fixture\"\n"
    );
    expect(
        "macro-module-shadow",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-store/src/lib.rs",
                "use fx_api::*;\npub mod danger {}\npub fn x() { danger!(); }\n",
            ),
        ],
        &[("restricted", "fx-store")],
    );
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

#[test]
fn a_restriction_binds_only_the_roles_and_crates_it_names() {
    let store = "pub fn s() { let _: Option<fx_api::store::FieldManager> = None; }\n";
    for (scope, want) in [("adapter", "fx-store"), ("fx-mgr", "fx-mgr")] {
        let rules = format!(
            "{RULES}\n[[restricted]]\napplies_to = [\"{scope}\"]\npath = \"fx_api::store\"\nallowed = []\nwhy = \"fixture\"\n"
        );
        expect(
            &format!("restricted-scope-{scope}"),
            &[
                ("architecture.toml", &rules),
                ("crates/fx-store/src/lib.rs", store),
            ],
            &[("restricted", want)],
        );
    }
}

#[test]
fn a_restriction_must_apply_to_a_known_role_or_crate() {
    for scope in ["[\"adapters\"]", "[]"] {
        let rules = format!(
            "{RULES}\n[[restricted]]\napplies_to = {scope}\npath = \"fx_api::FieldManager\"\nallowed = []\nwhy = \"fixture\"\n"
        );
        expect(
            "restricted-unknown-scope",
            &[("architecture.toml", &rules)],
            &[("restricted-scope", "fx_api::FieldManager")],
        );
    }
}

#[test]
fn every_fleet_restriction_binds_every_fleet_crate() {
    let rules = load_rules(&repo_root().join("crates").join("architecture.toml")).expect("rules");
    let mut unbound = Vec::new();
    for r in &rules.restricted {
        for (name, crate_rule) in &rules.crates {
            if crate_rule.role == arch_rules::Role::Component || r.allowed.contains(name) {
                continue;
            }
            if !r
                .applies_to
                .iter()
                .any(|s| s == name || s == crate_rule.role.name())
            {
                unbound.push(format!("{} does not bind {name}", r.path));
            }
        }
    }
    assert!(unbound.is_empty(), "{unbound:#?}");
}

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

#[test]
fn the_rules_keep_the_sdk_and_exporter_in_otel_env() {
    let rules = load_rules(&repo_root().join("crates").join("architecture.toml")).expect("rules");
    let telemetry = rules.crates.get("otel-env").expect("otel-env is governed");
    assert_eq!(telemetry.role, arch_rules::Role::Component);
    assert!(telemetry.deps.is_empty());
    let dependants: Vec<&String> = rules
        .crates
        .iter()
        .filter(|(_, r)| r.deps.iter().any(|d| d == "otel-env"))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        dependants,
        vec![
            "fleet-cli",
            "fleet-runtime",
            "runtime-mux",
            "thurbox",
            "workspace-git"
        ],
        "each program, and the runtime adapter that carries trace context"
    );
    for pure in ["fleet-api", "fleet-controllers"] {
        assert!(
            rules.crates[pure].probes.iter().any(|p| p == "otel-env"),
            "{pure} probes the telemetry edge"
        );
    }
    for path in ["opentelemetry_sdk", "opentelemetry_otlp"] {
        let r = rules
            .restricted
            .iter()
            .find(|r| r.path == path)
            .unwrap_or_else(|| panic!("{path} is not restricted"));
        assert_eq!(r.allowed, vec!["otel-env".to_string()], "{path}");
    }
}

#[test]
fn only_the_telemetry_crate_names_the_sdk() {
    let rules = format!(
        "{RULES}\n[crates.fx-tel]\nrole = \"adapter\"\ndeps = []\n\n[[restricted]]\napplies_to = [\"pure\", \"adapter\", \"orchestration\", \"edge\"]\npath = \"fx_sdk\"\nallowed = [\"fx-tel\"]\nwhy = \"one crate exports telemetry\"\n"
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
        "{RULES}\n[[restricted]]\napplies_to = [\"pure\", \"adapter\", \"orchestration\", \"edge\"]\npath = \"fx_api::Runtime\"\nallowed = [\"fx-api\", \"fx-ctl\", \"fx-mgr\"]\nwhy = \"only the manager performs runtime effects\"\n"
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

#[test]
fn forge_effect_capability_cannot_leak_to_an_observer_adapter_or_a_controller() {
    let rules = format!(
        "{RULES}\n[[restricted]]\napplies_to = [\"pure\", \"adapter\", \"orchestration\", \"edge\"]\npath = \"fx_api::Forge\"\nallowed = [\"fx-api\", \"fx-mgr\"]\nwhy = \"only the manager performs the mark-ready effect\"\n"
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

#[test]
fn gateway_effect_capability_cannot_leak_to_an_observer_adapter_or_a_controller() {
    let rules = format!(
        "{RULES}\n[[restricted]]\napplies_to = [\"pure\", \"adapter\", \"orchestration\", \"edge\"]\npath = \"fx_api::Gateway\"\nallowed = [\"fx-api\", \"fx-mgr\"]\nwhy = \"only the manager performs the post effect\"\n"
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

#[test]
fn the_rules_keep_board_appends_with_the_publish_verb() {
    let rules = load_rules(&repo_root().join("crates").join("architecture.toml")).expect("rules");
    assert!(
        rules.crates["fleet-cli"]
            .forbidden_methods
            .iter()
            .any(|m| m == "append_events"),
        "the CLI publishes through the manager's verb, never the store's append"
    );
}

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

#[test]
fn a_component_reaching_another_crate_through_a_reexport_is_caught() {
    let rules = format!("{RULES}\n[crates.fx-ui]\nrole = \"component\"\ndeps = [\"fx-ctl\"]\n");
    let ctl = format!("{PROBES}pub use fx_api::FieldManager;\n");
    expect(
        "component-reexport",
        &[
            ("architecture.toml", &rules),
            (CTL, &ctl),
            ("crates/fx-ui/fixture.toml", &manifest("fx-ui", &["fx-ctl"])),
            (
                "crates/fx-ui/src/main.rs",
                "fn main() { let _ = fx_ctl::FieldManager::Worker; let _ = std::process::Command::new(\"x\"); }\n",
            ),
        ],
        &[("reexport-edge", "fx-ui")],
    );
}

const MODULES: &str = "\n[crates.fx-mod]\nrole = \"component\"\ndeps = []\nexempt = [\"lib\"]\ngoverned = [\"c\"]\n\n[crates.fx-mod.modules]\na = { use = [\"b\"] }\nb = { use = [] }\nc = { use = [] }\n\"c::d\" = { use = [\"b\"] }\n";

fn modules(case: &str, changes: &[(&str, &str)], want: &[(&str, &str)]) {
    let rules = format!("{RULES}{MODULES}");
    let mut files: Vec<(&str, &str)> = vec![
        ("architecture.toml", &rules),
        (
            "crates/fx-mod/fixture.toml",
            "[package]\nname = \"fx-mod\"\n",
        ),
        (
            "crates/fx-mod/src/lib.rs",
            "pub mod a;\npub mod b;\npub mod c;\n",
        ),
        ("crates/fx-mod/src/a.rs", "pub fn a() { crate::b::b(); }\n"),
        ("crates/fx-mod/src/b.rs", "pub fn b() {}\n"),
        ("crates/fx-mod/src/c/mod.rs", "pub mod d;\n"),
        (
            "crates/fx-mod/src/c/d.rs",
            "pub fn d() { crate::b::b(); }\n",
        ),
    ];
    for (p, c) in changes {
        match files.iter_mut().find(|(fp, _)| fp == p) {
            Some(f) => f.1 = c,
            None => files.push((p, c)),
        }
    }
    expect(case, &files, want);
}

#[test]
fn a_crate_that_keeps_its_module_rules_passes() {
    modules("modules-clean", &[], &[]);
}

#[test]
fn a_module_reaching_what_its_rule_does_not_name_is_caught() {
    let bad = [("module-edge", "fx-mod")];
    modules(
        "modules-edge",
        &[("crates/fx-mod/src/b.rs", "pub fn b() { crate::a::a(); }\n")],
        &bad,
    );
    modules(
        "modules-edge-test",
        &[(
            "crates/fx-mod/src/b.rs",
            "pub fn b() {}\n#[cfg(test)]\nmod tests { fn t() { crate::a::a(); } }\n",
        )],
        &bad,
    );
    modules(
        "modules-edge-alias",
        &[
            (
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub type Seen = crate::c::d::Hidden;\n",
            ),
            (
                "crates/fx-mod/src/c/d.rs",
                "pub fn d() { crate::b::b(); }\npub struct Hidden;\n",
            ),
            (
                "crates/fx-mod/src/a.rs",
                "pub fn a() { crate::b::b(); let _: Option<crate::b::Seen> = None; }\n",
            ),
        ],
        // `b` declaring the alias reaches `c::d` itself, and `a` through it.
        &bad,
    );
}

#[test]
fn every_module_is_governed_and_every_rule_names_one() {
    modules(
        "modules-ungoverned",
        &[
            (
                "crates/fx-mod/src/lib.rs",
                "pub mod a;\npub mod b;\npub mod c;\npub mod e;\n",
            ),
            ("crates/fx-mod/src/e.rs", ""),
        ],
        &[("module-ungoverned", "fx-mod")],
    );
    modules(
        "modules-governed",
        &[
            ("crates/fx-mod/src/c/mod.rs", "pub mod d;\npub mod f;\n"),
            ("crates/fx-mod/src/c/f.rs", ""),
        ],
        &[("module-ungoverned", "fx-mod")],
    );
    modules(
        "modules-test-file",
        &[
            (
                "crates/fx-mod/src/c/mod.rs",
                "pub mod d;\n#[cfg(test)]\nmod tests;\n",
            ),
            ("crates/fx-mod/src/c/tests.rs", ""),
        ],
        &[],
    );
    let unknown = format!("{RULES}{MODULES}gone = {{ use = [] }}\n");
    modules(
        "modules-unknown",
        &[("architecture.toml", &unknown)],
        &[("module-unknown", "fx-mod")],
    );
    let typo = format!(
        "{RULES}{}",
        MODULES.replace("governed = [\"c\"]", "governed = [\"cc\"]")
    );
    modules(
        "modules-governed-typo",
        &[("architecture.toml", &typo)],
        &[("module-unknown", "fx-mod")],
    );
}

#[test]
fn module_rules_allow_no_cycle_and_no_unused_grant() {
    let cycle = MODULES.replace("b = { use = [] }", "b = { use = [\"a\"] }");
    let rules = format!("{RULES}{cycle}");
    modules(
        "modules-cycle",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-mod/src/b.rs",
                "pub fn b() { let _ = crate::a::a; }\n",
            ),
        ],
        &[("module-cycle", "fx-mod")],
    );
    let unused = [("module-unused", "fx-mod")];
    modules(
        "modules-unused-inline",
        &[(
            "crates/fx-mod/src/a.rs",
            "pub fn a() {}\n#[cfg(test)]\nmod tests { fn t() { crate::b::b(); } }\n",
        )],
        &unused,
    );
    modules(
        "modules-unused-file",
        &[
            (
                "crates/fx-mod/src/a.rs",
                "pub fn a() {}\n#[cfg(test)]\nmod tests;\n",
            ),
            (
                "crates/fx-mod/src/a/tests.rs",
                "fn t() { crate::b::b(); }\n",
            ),
        ],
        &unused,
    );
}

#[test]
fn a_restricted_path_binds_every_module_it_does_not_allow() {
    let restricted = |extra: &str| {
        format!(
            "{RULES}{MODULES}\n[[crates.fx-mod.restricted]]\npaths = [\"fx_mod::b::b\"]\nallowed = [\"c::d\"]\nwhy = \"fixture\"\n{extra}"
        )
    };
    let hit = [("module-restricted", "fx-mod")];
    let rules = restricted("");
    modules("restricted-module", &[("architecture.toml", &rules)], &hit);
    // An exempt root has no module rule, but a restriction binds it.
    modules(
        "restricted-root",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-mod/src/a.rs",
                "pub fn a() { let _ = crate::b::B; }\n",
            ),
            ("crates/fx-mod/src/b.rs", "pub fn b() {}\npub struct B;\n"),
            (
                "crates/fx-mod/src/lib.rs",
                "pub mod a;\npub mod b;\npub mod c;\npub fn root() { crate::b::b(); }\n",
            ),
        ],
        &hit,
    );
    modules(
        "restricted-test-code",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-mod/src/a.rs",
                "pub fn a() { let _ = crate::b::B; }\n#[cfg(test)]\nmod tests { fn t() { crate::b::b(); } }\n",
            ),
            ("crates/fx-mod/src/b.rs", "pub fn b() {}\npub struct B;\n"),
        ],
        &hit,
    );
    let production = restricted("production_only = true\n");
    modules(
        "restricted-production-only",
        &[
            ("architecture.toml", &production),
            (
                "crates/fx-mod/src/a.rs",
                "pub fn a() { let _ = crate::b::B; }\n#[cfg(test)]\nmod tests { fn t() { crate::b::b(); } }\n",
            ),
            ("crates/fx-mod/src/b.rs", "pub fn b() {}\npub struct B;\n"),
        ],
        &[],
    );
    let two_calls = "pub fn a() { crate::b::b(); crate::b::b(); }\n";
    for (sites, want) in [
        (2, &[][..]),
        (1, &hit[..]),
        (3, &[("module-restricted-stale", "fx-mod")][..]),
    ] {
        let rules = restricted(&format!(
            "exceptions = [{{ file = \"a.rs\", sites = {sites}, why = \"fixture\" }}]\n"
        ));
        modules(
            &format!("restricted-exception-{sites}"),
            &[
                ("architecture.toml", &rules),
                ("crates/fx-mod/src/a.rs", two_calls),
            ],
            want,
        );
    }
}

#[test]
fn a_restriction_binds_a_crate_without_module_rules() {
    let rules = format!(
        "{RULES}\n[crates.fx-flat]\nrole = \"component\"\ndeps = []\n\n[[crates.fx-flat.restricted]]\npaths = [\"fx_flat::b::b\"]\nallowed = [\"b\"]\nwhy = \"fixture\"\n"
    );
    expect(
        "restricted-flat",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-flat/fixture.toml",
                "[package]\nname = \"fx-flat\"\n",
            ),
            ("crates/fx-flat/src/lib.rs", "pub mod a;\npub mod b;\n"),
            ("crates/fx-flat/src/a.rs", "pub fn a() { crate::b::b(); }\n"),
            ("crates/fx-flat/src/b.rs", "pub fn b() {}\n"),
        ],
        &[("module-restricted", "fx-flat")],
    );
}

#[test]
fn a_file_two_targets_include_counts_once_against_an_exception() {
    let rules = format!(
        "{RULES}{MODULES}\n[[crates.fx-mod.restricted]]\npaths = [\"std::fs::write\"]\nallowed = [\"c::d\"]\nwhy = \"fixture\"\nexceptions = [{{ file = \"a.rs\", sites = 1, why = \"fixture\" }}]\n"
    );
    let main_rules = rules.replace("exempt = [\"lib\"]", "exempt = [\"lib\", \"main\"]");
    modules(
        "restricted-shared-file",
        &[
            ("architecture.toml", &main_rules),
            (
                "crates/fx-mod/src/a.rs",
                "pub fn a() { crate::b::b(); let _ = std::fs::write(\"x\", \"y\"); }\n",
            ),
            (
                "crates/fx-mod/src/main.rs",
                "#[path = \"a.rs\"]\nmod a;\nfn main() {}\n",
            ),
        ],
        &[],
    );
}

fn restriction(fields: &str) -> String {
    format!("{RULES}{MODULES}\n[[crates.fx-mod.restricted]]\nwhy = \"fixture\"\n{fields}")
}

#[test]
fn a_direct_restriction_judges_what_a_module_names_itself() {
    let rules = restriction("paths = [\"std::fs\"]\nallowed = [\"b\"]\ndirect = true\n");
    let arch = ("architecture.toml", rules.as_str());
    let hit = [("module-restricted", "fx-mod")];
    modules(
        "direct-alias",
        &[
            arch,
            (
                "crates/fx-mod/src/a.rs",
                "use std::fs as f;\npub fn a() { crate::b::b(); let _ = f::read(\"x\"); }\n",
            ),
        ],
        &hit,
    );
    for (case, b) in [
        ("direct-reexport", "pub fn b() {}\npub use std::fs::read;\n"),
        ("direct-glob", "pub fn b() {}\npub use std::fs::*;\n"),
    ] {
        modules(
            case,
            &[
                arch,
                (
                    "crates/fx-mod/src/a.rs",
                    "pub fn a() { crate::b::b(); let _ = crate::b::read; }\n",
                ),
                ("crates/fx-mod/src/b.rs", b),
            ],
            &[],
        );
    }
}

#[test]
fn a_restriction_binds_only_where_it_applies_and_frees_whole_files() {
    let hit = [("module-restricted", "fx-mod")];
    let scoped = restriction("paths = [\"fx_mod::b::b\"]\nallowed = []\napplies_to = [\"c\"]\n");
    // `c::d` reaches `b::b`; `a` does too, but `a` is outside the scope.
    modules("applies-to", &[("architecture.toml", &scoped)], &hit);
    // A reach through a glob import is resolved, so it is caught too.
    modules(
        "applies-to-glob",
        &[
            ("architecture.toml", &scoped),
            (
                "crates/fx-mod/src/c/d.rs",
                "use crate::b::*;\npub fn d() { b(); }\n",
            ),
        ],
        &hit,
    );
    let freed = restriction(
        "paths = [\"fx_mod::b::b\"]\nallowed = []\napplies_to = [\"c\"]\nallowed_files = [\"c/d.rs\"]\n",
    );
    modules("allowed-files", &[("architecture.toml", &freed)], &[]);
    let gone = restriction(
        "paths = [\"fx_mod::b::b\"]\nallowed = []\napplies_to = [\"c\"]\nallowed_files = [\"c/d.rs\", \"c/gone.rs\"]\n",
    );
    modules(
        "allowed-files-stale",
        &[("architecture.toml", &gone)],
        &[("module-restricted-stale", "fx-mod")],
    );
}

#[test]
fn a_module_names_only_the_external_crates_its_rule_lists() {
    let manifest = (
        "crates/fx-mod/fixture.toml",
        "[package]\nname = \"fx-mod\"\n\n[dependencies]\nserde = \"1\"\n",
    );
    let a = (
        "crates/fx-mod/src/a.rs",
        "pub fn a() { crate::b::b(); let _: Option<serde::de::IgnoredAny> = None; let _ = std::mem::size_of::<u8>(); }\n",
    );
    for (case, external, want) in [
        ("external-none", "[]", &[("module-external", "fx-mod")][..]),
        ("external-listed", "[\"serde\"]", &[][..]),
    ] {
        let rules = format!(
            "{RULES}{}",
            MODULES.replace(
                "a = { use = [\"b\"] }",
                &format!("a = {{ use = [\"b\"], external = {external} }}")
            )
        );
        modules(case, &[("architecture.toml", &rules), manifest, a], want);
    }
}

#[test]
fn static_mut_and_build_target_cfg_can_be_restricted() {
    let hit = [("module-restricted", "fx-mod")];
    let state = restriction("paths = [\"#static_mut\"]\nallowed = []\napplies_to = [\"b\"]\n");
    modules(
        "static-mut",
        &[
            ("architecture.toml", &state),
            (
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\nstatic mut N: u8 = 0;\n",
            ),
        ],
        &hit,
    );
    let os = restriction(
        "paths = [\"#cfg::windows\", \"#cfg::unix\"]\nallowed = []\napplies_to = [\"b\"]\nproduction_only = true\n",
    );
    for (case, b) in [
        (
            "cfg-attr",
            "pub fn b() {}\n#[cfg(windows)]\npub fn w() {}\n",
        ),
        ("cfg-macro", "pub fn b() { if cfg!(unix) {} }\n"),
        (
            "cfg-not",
            "pub fn b() {}\n#[cfg(not(windows))]\npub fn w() {}\n",
        ),
        ("cfg-mod", "pub fn b() {}\n#[cfg(windows)]\nmod w;\n"),
        (
            "cfg-inline-mod",
            "pub fn b() {}\n#[cfg(windows)]\nmod w {}\n",
        ),
        ("cfg-use", "pub fn b() {}\n#[cfg(unix)]\nuse std::fs;\n"),
        ("cfg-file", "#![cfg(unix)]\npub fn b() {}\n"),
        (
            "cfg-extern-crate",
            "pub fn b() {}\n#[cfg(windows)]\nextern crate alloc;\n",
        ),
        (
            "cfg-impl",
            "pub fn b() {}\npub struct S;\n#[cfg(windows)]\nimpl S {}\n",
        ),
        (
            "cfg-const",
            "pub fn b() {}\n#[cfg(unix)]\nconst C: u8 = 0;\n",
        ),
        (
            "cfg-attr-cfg",
            "pub fn b() {}\n#[cfg_attr(windows, inline)]\npub fn w() {}\n",
        ),
    ] {
        modules(
            case,
            &[("architecture.toml", &os), ("crates/fx-mod/src/b.rs", b)],
            &hit,
        );
    }
    // A test-only item's own `cfg` is test code: it is not production code
    // deciding by a build target.
    modules(
        "cfg-all-test",
        &[
            ("architecture.toml", &os),
            (
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\n#[cfg(all(test, unix))]\nfn check() {}\n",
            ),
        ],
        &[],
    );
    modules(
        "cfg-test-then-unix",
        &[
            ("architecture.toml", &os),
            (
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\n#[cfg(test)]\n#[cfg(unix)]\nfn check() {}\n",
            ),
        ],
        &[],
    );
    modules(
        "cfg-test-only",
        &[
            ("architecture.toml", &os),
            (
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\n#[cfg(test)]\nmod tests { #[cfg(unix)] fn u() {} }\n",
            ),
        ],
        &[],
    );
}

#[test]
fn a_grant_of_a_governed_module_is_caught() {
    let rules = format!(
        "{RULES}{}",
        MODULES.replace("a = { use = [\"b\"] }", "a = { use = [\"b\", \"c\"] }")
    );
    modules(
        "governed-grant",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-mod/src/a.rs",
                "pub fn a() { crate::b::b(); let _ = crate::c::X; }\n",
            ),
            ("crates/fx-mod/src/c/mod.rs", "pub mod d;\npub struct X;\n"),
        ],
        &[("module-governed-grant", "fx-mod")],
    );
}

#[test]
fn a_reexport_of_another_crates_item_is_its_module() {
    let rules = format!(
        "{RULES}{}",
        MODULES.replace("a = { use = [\"b\"] }", "a = { use = [] }")
    );
    modules(
        "local-reexport",
        &[
            ("architecture.toml", &rules),
            (
                "crates/fx-mod/src/a.rs",
                "pub fn a() { let _ = crate::b::read; }\n",
            ),
            (
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub use std::fs::read;\n",
            ),
        ],
        &[("module-edge", "fx-mod")],
    );
}

#[test]
fn a_path_only_grant_admits_paths_but_no_import() {
    let rules = format!(
        "{RULES}{}",
        MODULES.replace(
            "a = { use = [\"b\"] }",
            "a = { use = [], path_only = [\"b\"] }"
        )
    );
    let arch = ("architecture.toml", rules.as_str());
    modules("path-only-clean", &[arch], &[]);
    let edge = [("module-edge", "fx-mod")];
    modules(
        "path-only-use",
        &[
            arch,
            (
                "crates/fx-mod/src/a.rs",
                "use crate::b;\npub fn a() { b::b(); }\n",
            ),
        ],
        &edge,
    );
    modules(
        "path-only-local-use",
        &[
            arch,
            (
                "crates/fx-mod/src/a.rs",
                "pub fn a() { use crate::b::b; b(); }\n",
            ),
        ],
        &edge,
    );
}

#[test]
fn an_inline_module_is_governed_too() {
    modules(
        "modules-inline",
        &[(
            "crates/fx-mod/src/lib.rs",
            "pub mod a;\npub mod b;\npub mod c;\npub mod extra { pub fn e() { crate::b::b(); } }\n",
        )],
        &[("module-ungoverned", "fx-mod")],
    );
    modules(
        "modules-inline-test",
        &[(
            "crates/fx-mod/src/lib.rs",
            "pub mod a;\npub mod b;\npub mod c;\n#[cfg(test)]\nmod tests { fn t() { crate::b::b(); } }\n",
        )],
        &[],
    );
}

#[test]
fn test_code_anywhere_keeps_no_grant_in_use() {
    let unused = [("module-unused", "fx-mod")];
    for (case, a, extra) in [
        (
            "test-assoc-const",
            "pub struct A;\nimpl A {\n    #[cfg(test)]\n    const CHECK: fn() = crate::b::b;\n}\npub fn a() {}\n",
            None,
        ),
        (
            "test-let",
            "pub fn a() {\n    #[cfg(test)]\n    let _ = crate::b::b;\n}\n",
            None,
        ),
        (
            "test-stmt",
            "pub fn a() {\n    #[cfg(test)]\n    crate::b::b();\n}\n",
            None,
        ),
        (
            "test-file-attr",
            "pub fn a() {}\nmod helper;\n",
            Some((
                "crates/fx-mod/src/a/helper.rs",
                "#![cfg(test)]\nfn t() { crate::b::b(); }\n",
            )),
        ),
        (
            "test-impl-fn",
            "pub struct A;\nimpl A {\n    #[cfg(test)]\n    fn t() { crate::b::b(); }\n}\npub fn a() {}\n",
            None,
        ),
        (
            "test-cfg-all",
            "pub fn a() {}\n#[cfg(all(unix, test))]\nfn t() { crate::b::b(); }\n",
            None,
        ),
        (
            "test-trait-item",
            "pub trait T {\n    #[cfg(test)]\n    const CHECK: fn() = crate::b::b;\n}\npub fn a() {}\n",
            None,
        ),
        (
            "test-arm",
            "pub fn a(x: u8) {\n    match x {\n        #[cfg(test)]\n        0 => crate::b::b(),\n        _ => {}\n    }\n}\n",
            None,
        ),
        (
            "test-stmt-macro",
            "pub fn a() {\n    #[cfg(test)]\n    assert!(crate::b::b() == ());\n}\n",
            None,
        ),
        (
            "test-field",
            "pub struct A {\n    #[cfg(test)]\n    pub f: crate::b::B,\n}\npub fn a() {}\n",
            Some(("crates/fx-mod/src/b.rs", "pub fn b() {}\npub struct B;\n")),
        ),
        (
            "test-variant",
            "pub enum A {\n    #[cfg(test)]\n    V = crate::b::N,\n}\npub fn a() {}\n",
            Some((
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub const N: isize = 0;\n",
            )),
        ),
        (
            "test-fn-param",
            "pub fn a() {}\nfn t(#[cfg(test)] _x: crate::b::B) {}\n",
            Some((
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub struct B;\npub trait Tr {}\npub const C: () = ();\n",
            )),
        ),
        (
            "test-closure-param",
            "pub fn a() {\n    let _f = |#[cfg(test)] _x: crate::b::B| {};\n}\n",
            Some((
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub struct B;\npub trait Tr {}\npub const C: () = ();\n",
            )),
        ),
        (
            "test-receiver",
            "pub struct A;\nimpl A {\n    fn t(#[cfg(test)] self: std::rc::Rc<crate::b::B>) {}\n}\npub fn a() {}\n",
            Some((
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub struct B;\npub trait Tr {}\npub const C: () = ();\n",
            )),
        ),
        (
            "test-generic-param",
            "pub fn a() {}\nfn t<#[cfg(test)] T: crate::b::Tr>() {}\n",
            Some((
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub struct B;\npub trait Tr {}\npub const C: () = ();\n",
            )),
        ),
        (
            "test-struct-expr-field",
            "pub struct S {\n    pub f: (),\n}\npub fn a() {\n    let _ = S { #[cfg(test)] f: crate::b::b() };\n}\n",
            None,
        ),
        (
            "test-field-pattern",
            "pub struct S {\n    pub f: (),\n}\npub fn a(s: S) {\n    let S { #[cfg(test)] f: crate::b::C, .. } = s;\n}\n",
            Some((
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub struct B;\npub trait Tr {}\npub const C: () = ();\n",
            )),
        ),
        (
            "test-pattern",
            "pub fn a() {\n    let _f = |#[cfg(test)] crate::b::C| {};\n}\n",
            Some((
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub struct B;\npub trait Tr {}\npub const C: () = ();\n",
            )),
        ),
        (
            "test-foreign-item",
            "pub fn a() {}\nunsafe extern \"C\" {\n    #[cfg(test)]\n    fn ext(x: crate::b::B);\n}\n",
            Some((
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub struct B;\npub trait Tr {}\npub const C: () = ();\n",
            )),
        ),
        (
            "test-where-predicate",
            "pub fn a() {}\nfn t<T>()\nwhere\n    #[cfg(test)]\n    T: crate::b::Tr,\n{\n}\n",
            Some((
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub struct B;\npub trait Tr {}\npub const C: () = ();\n",
            )),
        ),
        (
            "test-bare-fn-arg",
            "pub fn a() {}\nfn t(_f: fn(#[cfg(test)] crate::b::B)) {}\n",
            Some((
                "crates/fx-mod/src/b.rs",
                "pub fn b() {}\npub struct B;\npub trait Tr {}\npub const C: () = ();\n",
            )),
        ),
    ] {
        let mut changes = vec![("crates/fx-mod/src/a.rs", a)];
        changes.extend(extra);
        modules(case, &changes, &unused);
    }
}

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

#[test]
fn a_missing_crate_an_unparsable_file_and_a_missing_clippy_guard_are_caught() {
    let ghost = format!("{RULES}\n[crates.fx-ghost]\nrole = \"adapter\"\ndeps = []\n");
    expect(
        "missing-crate",
        &[("architecture.toml", &ghost)],
        &[("missing-crate", "fx-ghost")],
    );
    expect(
        "unparsable",
        &[("crates/fx-store/src/broken.rs", "fn (\n")],
        &[("unparsable", "fx-store")],
    );
    let guarded = RULES.replace("[pure]\n", "[pure]\nclippy_guards = [\"std::fs::File\"]\n");
    expect(
        "missing-clippy-guard",
        &[
            ("architecture.toml", &guarded),
            (
                "crates/fx-api/clippy.toml",
                "disallowed-types = [\"std::fs::File\"]\n",
            ),
        ],
        &[("missing-clippy-guard", "fx-ctl")],
    );
}

#[test]
fn every_violation_code_is_in_the_constitution() {
    let constitution = fs::read_to_string(repo_root().join("docs/constitution.md"))
        .expect("read the constitution");
    let start = constitution
        .find("### What `arch-rules` checks")
        .expect("the constitution has the table");
    let section = &constitution[start..];
    let section = &section[..section[4..].find("\n#").map_or(section.len(), |i| i + 4)];
    let missing: Vec<&str> = Code::ALL
        .iter()
        .map(|code| code.as_str())
        .filter(|code| !section.contains(&format!("`{code}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "add these to docs/constitution.md: {missing:?}"
    );
}
