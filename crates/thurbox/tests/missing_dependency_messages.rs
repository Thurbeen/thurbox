#![cfg(unix)]

use std::collections::HashMap;

fn without_a_multiplexer<T>(f: impl FnOnce() -> T) -> T {
    let dir = tempfile::tempdir().expect("tempdir");
    let saved = std::env::var_os("PATH");
    std::env::set_var("PATH", dir.path());
    let out = f();
    match saved {
        Some(v) => std::env::set_var("PATH", v),
        None => std::env::remove_var("PATH"),
    }
    out
}

#[test]
fn a_spawn_with_no_multiplexer_installed_names_it_the_search_and_the_fix() {
    let message = without_a_multiplexer(|| {
        let err = thurbox::backend::WindowLifecycle::create_window(
            &thurbox::backend::tmux::TmuxBackend::local(&thurbox::backend::mux_settings(
                &Default::default(),
            )),
            &thurbox::backend::WindowSpec {
                owner: thurbox::backend::Owner::new(
                    "00000000-0000-0000-0000-000000000000",
                    "test01",
                ),
                role: thurbox::backend::WindowRole::Agent,
                command: "some-agent",
                args: &[],
                cwd: None,
                env: &HashMap::new(),
            },
        )
        .expect_err("no multiplexer is installed, so this cannot succeed");
        format!("{err:#}")
    });

    let mux = "tmux";
    assert!(
        message.contains(mux),
        "the message never names the missing binary: {message}"
    );
    assert!(
        message.contains("Looked in"),
        "the message never says where thurbox looked: {message}"
    );
    assert!(
        message.contains("http"),
        "the message offers nowhere to get it: {message}"
    );
    assert!(
        !message.contains("os error 2"),
        "the message still leans on a raw errno: {message}"
    );
}
