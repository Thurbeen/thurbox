#![cfg(debug_assertions)]

fn default_debug_policy() -> bool {
    ![
        "CARGO_PROFILE_DEV_DEBUG",
        "CARGO_PROFILE_TEST_DEBUG",
        "CARGO_PROFILE_DEV_STRIP",
        "CARGO_PROFILE_TEST_STRIP",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some())
}

#[cfg(target_os = "linux")]
#[test]
fn default_debug_cli_stays_within_artifact_budget() {
    if !default_debug_policy() {
        return;
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .arg("--version")
        .output()
        .expect("run the built CLI");
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains(env!("THURBOX_VERSION")));
    let bytes = std::fs::metadata(env!("CARGO_BIN_EXE_thurbox-cli"))
        .expect("Cargo built the CLI")
        .len();
    let line_tables_cli_budget = 160 * 1024 * 1024;
    assert!(
        bytes < line_tables_cli_budget,
        "default CLI is {bytes} bytes; budget is 160 MiB, which full DWARF debuginfo overshoots"
    );
}

#[inline(never)]
fn diagnostic_backtrace() -> String {
    std::backtrace::Backtrace::force_capture().to_string()
}

#[test]
fn default_test_backtrace_retains_source_locations() {
    if !default_debug_policy() {
        return;
    }
    let trace = diagnostic_backtrace();
    assert!(trace.contains("diagnostic_backtrace"), "{trace}");
    assert!(trace.contains("build_artifacts.rs:"), "{trace}");
}
