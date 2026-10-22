//! Account usage for a custom-named agent of a supported family (issue #1527,
//! the family half): `claude-work` with `hook_schema = "claude"` reads claude's
//! usage, where it used to get no usage section at all because support matched
//! the exact agent name.
//!
//! Driven through the kernel's own `Metrics`, the one thing that fetches usage,
//! against an isolated config dir and an empty home, so no real credential is
//! read and no request leaves the machine.

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use thurbox::kernel::metrics::{Metrics, Subject};

fn subject(session: &str, agent: &str) -> Subject {
    Subject {
        session: session.into(),
        agent_session_id: None,
        pane: None,
        agent: agent.into(),
        host: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custom_agent_asserting_claude_reads_claude_usage() {
    let home = tempfile::tempdir().expect("tempdir");
    thurbox::paths::set_test_dir(home.path());
    std::env::set_var(thurbox::paths::CONFIG_DIR_OVERRIDE_ENV, home.path());
    std::env::set_var(thurbox::paths::DATA_DIR_OVERRIDE_ENV, home.path());
    // No credentials anywhere: the fetch answers "not logged in" locally.
    std::env::set_var("HOME", home.path());
    std::env::set_var("CLAUDE_CONFIG_DIR", home.path().join("claude"));
    std::fs::write(
        home.path().join("agents.toml"),
        r#"
[[agents]]
name = "claude-work"
command = "claude"
hook_schema = "claude"

[[agents]]
name = "mine"
command = "mine"
"#,
    )
    .expect("write agents.toml");

    let mut metrics = Metrics::new();
    metrics.sample(vec![subject("a", "claude-work"), subject("b", "mine")]);

    let deadline = Instant::now() + Duration::from_secs(10);
    while metrics.usage("claude-work", None).is_none() && Instant::now() < deadline {
        metrics.poll();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let usage = metrics
        .usage("claude-work", None)
        .expect("a claude-family agent gets a usage fetch");
    assert!(
        usage
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("not logged in"),
        "claude's fetch ran: {usage:?}"
    );
    // The family's own name reads the same fetch.
    assert_eq!(metrics.usage("claude", None), Some(usage));
    assert!(metrics.usage("mine", None).is_none());
}
