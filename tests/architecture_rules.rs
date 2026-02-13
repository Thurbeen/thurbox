use cargo_pup_lint_config::{LintBuilder, LintBuilderExt, ModuleLintExt, Severity};

#[test]
#[ignore]
fn ui_layer_isolation() {
    let mut builder = LintBuilder::new();

    builder
        .module_lint()
        .lint_named("ui_no_claude_imports")
        .matching(|m| m.module(".*::ui::.*"))
        .with_severity(Severity::Error)
        .restrict_imports(None, Some(vec![".*::claude::.*".to_string()]))
        .build();

    builder
        .assert_lints(None)
        .expect("UI modules must not import from claude module");
}

#[test]
#[ignore]
fn git_module_independence() {
    let mut builder = LintBuilder::new();

    builder
        .module_lint()
        .lint_named("git_no_ui_imports")
        .matching(|m| m.module(".*::git::.*"))
        .with_severity(Severity::Error)
        .restrict_imports(None, Some(vec![".*::ui::.*".to_string()]))
        .build();

    builder
        .assert_lints(None)
        .expect("Git modules must not import from UI module");
}

#[test]
#[ignore]
fn claude_module_isolation() {
    let mut builder = LintBuilder::new();

    builder
        .module_lint()
        .lint_named("claude_no_ui_or_git_imports")
        .matching(|m| m.module(".*::claude::.*"))
        .with_severity(Severity::Error)
        .restrict_imports(
            None,
            Some(vec![".*::ui::.*".to_string(), ".*::git::.*".to_string()]),
        )
        .build();

    builder
        .assert_lints(None)
        .expect("Claude modules must not import from UI or git modules");
}
