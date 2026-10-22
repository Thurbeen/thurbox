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

fn isolate(home: &std::path::Path) {
    thurbox::paths::set_test_dir(home);
    std::env::set_var(thurbox::paths::CONFIG_DIR_OVERRIDE_ENV, home);
    std::env::set_var(thurbox::paths::DATA_DIR_OVERRIDE_ENV, home);
    // No credentials anywhere: the fetch answers "not logged in" locally.
    std::env::set_var("HOME", home);
    std::env::set_var("CLAUDE_CONFIG_DIR", home.join("claude"));
    std::fs::write(
        home.join("agents.toml"),
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
}

/// Poll until `done` holds or a deadline passes, folding fetches in.
async fn settle(metrics: &mut Metrics, done: impl Fn(&Metrics) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(metrics) && Instant::now() < deadline {
        metrics.poll();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custom_agent_asserting_claude_reads_claude_usage() {
    let home = tempfile::tempdir().expect("tempdir");
    isolate(home.path());

    let mut metrics = Metrics::new();
    metrics.sample(vec![subject("a", "claude-work"), subject("b", "mine")]);
    settle(&mut metrics, |m| m.usage("claude-work", None).is_some()).await;

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

/// A remote host launches its agents from its own definitions, so this
/// machine's `hook_schema` says nothing about a session there: a custom name
/// on a host gets no family, as before family matching existed.
#[tokio::test(flavor = "multi_thread")]
async fn a_remote_custom_agent_does_not_borrow_the_local_family() {
    let home = tempfile::tempdir().expect("tempdir");
    isolate(home.path());

    let mut metrics = Metrics::new();
    let mut remote = subject("r", "claude-work");
    remote.host = Some("elsewhere".into());
    let mut builtin = subject("b", "claude");
    builtin.host = Some("elsewhere".into());
    metrics.sample(vec![remote, builtin]);
    settle(&mut metrics, |m| {
        m.usage("claude", Some("elsewhere")).is_some()
    })
    .await;

    // The built-in name on the host still has its usage scope (here: a host
    // with no definition, reported as such).
    assert!(metrics.usage("claude", Some("elsewhere")).is_some());
    assert!(
        metrics.usage("claude-work", Some("elsewhere")).is_none(),
        "a remote custom agent must not read a family from local agents.toml"
    );
}
