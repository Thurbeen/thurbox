use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// A single architecture violation: a forbidden import found in a source file.
struct Violation {
    file: PathBuf,
    line_number: usize,
    line: String,
}

/// Recursively collect all `.rs` files under `dir`.
fn collect_rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_rs_files(&path));
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files
}

/// Scan all `.rs` files in `module_dir` for `use crate::{denied}::` imports.
/// Returns a list of violations with file path, line number, and line content.
fn check_no_imports(module_dir: &Path, denied_modules: &[&str]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let files = collect_rs_files(module_dir);

    for file in files {
        let content = fs::read_to_string(&file).unwrap_or_default();
        for (i, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            for denied in denied_modules {
                let pattern = format!("use crate::{denied}::");
                if trimmed.starts_with(&pattern) {
                    violations.push(Violation {
                        file: file.clone(),
                        line_number: i + 1,
                        line: line.to_string(),
                    });
                }
            }
        }
    }

    violations
}

fn format_violations(module_name: &str, violations: &[Violation]) -> String {
    let mut msg = format!(
        "\n{} architecture violation(s) in `{module_name}/`:\n",
        violations.len()
    );
    for v in violations {
        writeln!(
            msg,
            "  {}:{}: {}",
            v.file.display(),
            v.line_number,
            v.line.trim()
        )
        .unwrap();
    }
    msg
}

#[test]
fn ui_layer_isolation() {
    let module_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
    let violations = check_no_imports(&module_dir, &["claude", "git"]);
    assert!(
        violations.is_empty(),
        "{}",
        format_violations("ui", &violations)
    );
}

#[test]
fn git_module_independence() {
    let module_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/git");
    let violations = check_no_imports(&module_dir, &["ui"]);
    assert!(
        violations.is_empty(),
        "{}",
        format_violations("git", &violations)
    );
}

#[test]
fn claude_module_isolation() {
    let module_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/claude");
    let violations = check_no_imports(&module_dir, &["ui", "git"]);
    assert!(
        violations.is_empty(),
        "{}",
        format_violations("claude", &violations)
    );
}

#[test]
fn project_isolation() {
    let module_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/project");
    let violations = check_no_imports(&module_dir, &["claude", "ui", "git", "app"]);
    assert!(
        violations.is_empty(),
        "{}",
        format_violations("project", &violations)
    );
}

#[test]
fn sync_module_isolation() {
    let module_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sync");
    let violations = check_no_imports(&module_dir, &["claude", "ui", "git", "app"]);
    assert!(
        violations.is_empty(),
        "{}",
        format_violations("sync", &violations)
    );
}

#[test]
fn app_module_structure() {
    // Verify that app/ module can be split into multiple files
    // Each file should maintain proper module organization
    let app_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app");

    // All app submodules should exist as .rs files or be re-exported from mod.rs
    let expected_files = vec!["mod.rs"];

    for file in expected_files {
        let path = app_dir.join(file);
        assert!(
            path.exists(),
            "Expected app module file not found: {}",
            file
        );
    }
}
