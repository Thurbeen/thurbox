//! A spawn that fails because a binary is not installed must say which one,
//! where thurbox looked, and what to do about it.
//!
//! The report this pins comes from a user whose local spawn failed with
//! `tmux new-window exited exit status: 127 for window tb-test01`: a number,
//! with nothing naming the missing binary, the directories searched, or the
//! fix. The same setup answered `Failed to create tmux session` through the
//! headless path — no better.
//!
//! Its own test binary because it sets `PATH` for the whole process, which a
//! sibling test in the same binary would see.

#![cfg(unix)]

use std::collections::HashMap;

/// Run `f` with `PATH` pointing at one directory that holds no multiplexer.
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
        let err = thurbox::agent::tmux::spawn_window(
            "00000000-0000-0000-0000-000000000000",
            "test01",
            "some-agent",
            &[],
            None,
            &HashMap::new(),
        )
        .expect_err("no multiplexer is installed, so this cannot succeed");
        format!("{err:#}")
    });

    let mux = thurbox::agent::transport::DEFAULT_MUX;
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
