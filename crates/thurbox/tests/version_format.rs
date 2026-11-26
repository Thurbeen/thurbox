const VERSION: &str = env!("THURBOX_VERSION");

#[test]
fn version_starts_with_digit() {
    assert!(
        VERSION.starts_with(|c: char| c.is_ascii_digit()),
        "THURBOX_VERSION must start with a digit, never 'v' (got {VERSION:?}); \
         the UI already prepends the prefix"
    );
}

#[test]
fn dev_build_version_matches_cargo_toml() {
    if VERSION.contains("-dev") {
        assert_eq!(
            VERSION,
            env!("CARGO_PKG_VERSION"),
            "without THURBOX_RELEASE_VERSION, build.rs falls back to CARGO_PKG_VERSION"
        );
    }
}
