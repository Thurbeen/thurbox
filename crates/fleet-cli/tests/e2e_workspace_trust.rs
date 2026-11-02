//! Workspace trust at agent startup, end to end: the real `fleet` binary, its
//! file store, TaskController and the thurbox adapter against the bounded
//! `thurbox-cli` stand-in. The rows and screens below are what thurbox-cli
//! 2.76.1, Codex 0.164.0 and Claude Code 2.1.312 showed in an isolated,
//! disposable instance. No real trust store or session is touched.

mod support;

use serde_json::{Value, json};
use support::{Env, assert_condition, runtime_config};

fn path(unix: &str) -> String {
    if cfg!(windows) {
        format!("C:{}", unix.replace('/', "\\"))
    } else {
        unix.to_string()
    }
}

/// The Codex launch override for one trust root: one `projects` key whose
/// value is a TOML inline table, since Codex splits a dotted `-c` key on
/// every `.` a path may contain.
fn codex_trust(root: &str) -> String {
    let escaped = root.replace('\\', "\\\\").replace('"', "\\\"");
    format!("projects={{\"{escaped}\"={{trust_level=\"trusted\"}}}}")
}

fn task(name: &str, harness: &str) -> Value {
    let mut t = support::task(name, true);
    t["spec"]["harness"] = json!(harness);
    t["spec"]["workspace"]["dir"] = json!(path(&format!("/work/{name}")));
    t
}

fn repositories_task(name: &str, harness: &str) -> Value {
    let mut t = task(name, harness);
    t["spec"]["workspace"] = json!({"repositories": [
        {"name": "fleet", "url": "https://example.invalid/org/fleet", "ref": "main"},
        {"name": "docs", "url": "https://example.invalid/org/docs"}
    ]});
    t
}

fn env(name: &str, extra_config: &str) -> Env {
    let env = Env::thurbox(name);
    let (fleet, docs) = (path("/src/fleet"), path("/src/docs"));
    let mut cfg = std::fs::read_to_string(&env.config).expect("config");
    cfg.push_str(&format!(
        "\n[runtime.repositories]\n\"https://example.invalid/org/fleet\" = '{fleet}'\n\"https://example.invalid/org/docs\" = '{docs}'\n{extra_config}"
    ));
    std::fs::write(&env.config, cfg).expect("config");
    env.apply(runtime_config()).ok();
    env
}

fn argv_of(env: &Env, sub: &str) -> Vec<Vec<String>> {
    env.stub_calls()
        .iter()
        .filter(|c| support::is_call(c, sub))
        .map(|c| {
            c["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap().to_string())
                .collect()
        })
        .collect()
}

fn agent_args(create: &[String]) -> Vec<String> {
    create
        .iter()
        .filter_map(|a| a.strip_prefix("--agent-arg=").map(str::to_string))
        .collect()
}

fn thurbox_key(env: &Env, task: &str) -> String {
    let uid = env.get(task)["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    format!("fleet.{uid}")
}

fn condition(status: &Value, type_: &str) -> Value {
    status["conditions"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| c["type"] == type_))
        .cloned()
        .unwrap_or(Value::Null)
}

/// A fresh Codex with its empty initial composer at the cursor, before its
/// first lifecycle hook.
fn initial_codex(env: &Env) {
    env.edit_stub(|s| {
        s["spawnState"] = json!("running");
        s["spawnFields"] = json!({
            "agent": "codex", "state_source": "process", "hook_reported": false,
            "hook_state": null, "hook_state_contradicted": false,
            "hook_corroboration": "agent", "foreground_process": "/bin/codex"
        });
        s["capture"] = json!({
            "ansi": false, "lines": 0, "cursor_row": 3, "cursor_col": 2,
            "foreground_process": "/bin/codex",
            "output": "  >_ OpenAI Codex (v0.164.0)\n  To get started, describe a task or try one of these commands:\n\n› Ask Codex to do anything\n  ? for shortcuts\n"
        });
    });
}

/// Claude Code at its workspace-trust dialog: no hook has reported, and the
/// dialog's default choice is to exit.
fn claude_at_trust_dialog(env: &Env, cwd: &str) {
    env.edit_stub(|s| {
        s["spawnState"] = json!("running");
        s["spawnFields"] = json!({
            "state_source": "process", "hook_reported": false, "hook_state": null,
            "hook_state_contradicted": false, "hook_corroboration": "agent",
            "foreground_process": "/home/u/.local/bin/claude"
        });
        s["capture"] = json!({
            "ansi": false, "lines": 0, "cursor_row": 9, "cursor_col": 1,
            "foreground_process": "/home/u/.local/bin/claude", "foreground_cwd": cwd,
            "output": format!(concat!(
                "────────────────────────────────────────\n",
                " Accessing workspace:\n",
                "\n",
                " {}\n",
                "\n",
                " Quick safety check: Is this a project you created or one you trust?\n",
                " Claude Code'll be able to read, edit, and execute files here.\n",
                "\n",
                " Security guide\n",
                " ❯ No, exit\n",
                "   Yes, I trust this folder\n",
                "\n",
                " Enter to confirm · Esc to cancel\n"), cwd)
        });
    });
}

/// What thurbox reports once Claude passed its startup dialogs: its hooks.
fn claude_reported_idle(env: &Env) {
    env.edit_stub(|s| {
        for sess in s["sessions"].as_array_mut().unwrap() {
            sess["state"] = json!("idle");
            sess["state_source"] = json!("hook");
            sess["hook_reported"] = json!(true);
            sess["hook_state"] = json!("idle");
        }
    });
}

#[test]
fn codex_starts_with_session_scoped_trust_for_exactly_its_directory() {
    let env = env("trust-codex-dir", "");
    initial_codex(&env);
    env.apply(task("c1", "codex")).ok();
    env.reconcile().ok();

    let create = &argv_of(&env, "create")[0];
    assert_eq!(
        agent_args(create),
        ["-c".to_string(), codex_trust(&path("/work/c1"))],
        "one launch override for the workspace, nothing broader: {create:?}"
    );
    let st = env.status("c1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "WorkspaceTrusted", "True", "Trusted");
    assert_eq!(argv_of(&env, "message send").len(), 1);
}

#[test]
fn codex_on_repositories_is_trusted_on_its_primary_source_checkout_only() {
    let env = env("trust-codex-repos", "");
    initial_codex(&env);
    env.apply(repositories_task("r1", "codex")).ok();
    env.reconcile().ok();

    // Codex keys a worktree's trust on the main checkout of its repository;
    // only the primary is the session's cwd, so only its source is named.
    let create = &argv_of(&env, "create")[0];
    assert_eq!(
        agent_args(create),
        ["-c".to_string(), codex_trust(&path("/src/fleet"))],
        "{create:?}"
    );
    assert_eq!(env.status("r1")["phase"], "Briefed");
    assert_condition(&env.status("r1"), "WorkspaceTrusted", "True", "Trusted");
}

#[test]
fn a_codex_session_without_fleets_trust_is_never_briefed_and_says_why() {
    // An adopted session Fleet did not start, one trusting another path, one
    // trusting twice, and one in another directory: each fails closed.
    let cases: [(&str, Value); 4] = [
        ("adopted", json!([])),
        ("other", json!(["-c", codex_trust(&path("/work/other"))])),
        (
            "twice",
            json!([
                "-c",
                codex_trust(&path("/work/c1")),
                "-c",
                codex_trust(&path("/work/c1"))
            ]),
        ),
        ("cwd", json!(null)),
    ];
    for (name, args) in cases {
        let env = env(&format!("trust-codex-mismatch-{name}"), "");
        initial_codex(&env);
        let mut t = task("c1", "codex");
        t["spec"]["dispatch"] = json!(false);
        env.apply(t).ok();
        let key = thurbox_key(&env, "c1");
        let (cwd, args) = if name == "cwd" {
            (
                path("/work/elsewhere"),
                json!(["-c", codex_trust(&path("/work/c1"))]),
            )
        } else {
            (path("/work/c1"), args)
        };
        env.edit_stub(|s| {
            let mut row = s["spawnFields"].clone();
            let extra = json!({
                "id": "00000000-0000-4000-8000-000000000099", "key": key, "host": "local",
                "name": "c1", "state": "running", "stopped": false, "cwd": cwd,
                "worktrees": [], "agent_args": args, "meta": {}, "sent": [], "env": []
            });
            row.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            s["sessions"] = json!([row]);
        });
        env.apply(task("c1", "codex")).ok();
        env.reconcile().ok();

        let st = env.status("c1");
        assert_eq!(argv_of(&env, "message send").len(), 0, "{name}: {st:#}");
        assert_eq!(st["phase"], "Spawned", "{name}");
        assert_condition(&st, "WorkspaceTrusted", "False", "Mismatch");
        assert_condition(&st, "NeedsAttention", "True", "WorkspaceUntrusted");
        assert_eq!(
            argv_of(&env, "create").len(),
            1,
            "{name}: adopted, never twinned"
        );
        // The way out it names is one the controller allows: deleting only
        // the session would leave the Task WorkerGone, never respawned.
        let message = condition(&st, "WorkspaceTrusted")["message"].to_string();
        assert!(message.contains("delete the Task"), "{name}: {message}");
        assert!(!message.contains("delete it,"), "{name}: {message}");
    }
}

#[test]
fn codex_showing_its_own_trust_dialog_is_never_trusted() {
    // The override reached Codex, yet Codex keys its cwd on another path
    // (a symlink on a remote host, say) and draws its dialog over the
    // composer. The dialog's default trusts; a brief's Enter would answer it.
    let env = env("trust-codex-overlay", "");
    initial_codex(&env);
    env.edit_stub(|s| {
        s["capture"]["output"] = json!(concat!(
            "  >_ OpenAI Codex (v0.164.0)\n",
            "\n",
            "› Ask Codex to do anything\n",
            "  Folder access\n",
            "  /work/t1\n",
            "  Trust this folder? Codex can read, edit, and run files here, subject to your permission settings.\n",
            "› 1. Trust and continue\n",
            "  2. Back to Agent Command Center\n",
            "  enter continue · esc back\n"
        ));
        s["capture"]["cursor_row"] = json!(2);
    });
    env.apply(task("t1", "codex")).ok();
    env.reconcile().ok();
    let st = env.status("t1");
    assert_eq!(argv_of(&env, "message send").len(), 0, "{st:#}");
    assert_condition(&st, "WorkspaceTrusted", "False", "Mismatch");
    assert_condition(&st, "NeedsAttention", "True", "WorkspaceUntrusted");
    // Codex keeps only the last `-c projects=`: an agent entry or wrapper
    // passing its own after Fleet's replaces it. The message names both
    // causes, not only a symlink.
    let message = condition(&st, "WorkspaceTrusted")["message"].to_string();
    assert!(message.contains("-c projects="), "{message}");
    assert!(message.contains("config.toml"), "{message}");
}

#[test]
fn a_codex_screen_that_cannot_be_read_is_unknown_not_trusted() {
    for (name, field, value) in [
        ("no-output", "output", Value::Null),
        ("other-session", "session_id", json!("someone-else")),
        ("scrollback", "lines", json!(200)),
    ] {
        let env = env(&format!("trust-codex-unreadable-{name}"), "");
        initial_codex(&env);
        env.edit_stub(|s| s["capture"][field] = value);
        env.apply(task("t1", "codex")).ok();
        env.reconcile().ok();
        let st = env.status("t1");
        assert_eq!(argv_of(&env, "message send").len(), 0, "{name}: {st:#}");
        assert_condition(&st, "WorkspaceTrusted", "Unknown", "TrustUnknown");
    }
}

#[test]
fn a_remote_trust_root_is_normalised_or_refused() {
    let env = env("trust-codex-remote", "");
    env.edit_stub(|s| s["hosts"] = json!({"remote": "up"}));
    initial_codex(&env);
    // A trailing separator is the same directory: the override names it the
    // way Codex spells its cwd.
    let mut t = task("r1", "codex");
    t["spec"]["host"] = json!("remote");
    t["spec"]["workspace"]["dir"] = json!(path("/work/r1/"));
    env.apply(t).ok();
    env.reconcile().ok();
    let create = &argv_of(&env, "create")[0];
    assert_eq!(
        agent_args(create),
        ["-c".to_string(), codex_trust(&path("/work/r1"))],
        "{create:?}"
    );
    // `..` needs the host's filesystem to resolve: the spawn is refused.
    let mut t = task("r2", "codex");
    t["spec"]["host"] = json!("remote");
    t["spec"]["workspace"]["dir"] = json!(path("/work/x/../r2"));
    env.apply(t).ok();
    env.reconcile().ok();
    assert_eq!(argv_of(&env, "create").len(), 1, "{:?}", env.stub_calls());
    assert!(
        env.get("r2")["status"].to_string().contains("resolve"),
        "{:#}",
        env.get("r2")["status"]
    );
}

#[test]
fn claude_at_its_trust_dialog_is_never_briefed_and_asks_for_one_time_trust() {
    let env = env("trust-claude", "");
    claude_at_trust_dialog(&env, &path("/stub/worktrees/1-0/fleet-r1"));
    env.apply(repositories_task("r1", "claude")).ok();
    env.reconcile().ok();

    let st = env.status("r1");
    assert_eq!(st["phase"], "Spawned", "{st:#}");
    assert_eq!(argv_of(&env, "message send").len(), 0);
    assert!(
        argv_of(&env, "key").is_empty(),
        "never a keystroke into a dialog"
    );
    assert!(
        agent_args(&argv_of(&env, "create")[0]).is_empty(),
        "Claude has no per-session trust interface; nothing is invented"
    );
    assert_condition(&st, "WorkspaceTrusted", "False", "Unsupported");
    assert_condition(&st, "NeedsAttention", "True", "WorkspaceUntrusted");
    // Actionable: Claude keys a worktree's trust on its source checkout.
    let message = condition(&st, "WorkspaceTrusted")["message"].to_string();
    assert!(
        message.contains(&path("/src/fleet").replace('\\', "\\\\")),
        "{message}"
    );
    // Thurbox's own way to trust ahead of launch, for every later session.
    assert!(message.contains("session.pre_launch"), "{message}");

    // Looked at again after the retry wait, not in a hot loop.
    assert!(env.get("r1")["status"]["notBefore"].is_u64());
    env.reconcile().ok();
    assert_eq!(argv_of(&env, "message send").len(), 0);

    // A person trusts the repository once; Claude passes its dialog and
    // reports. The same Task now gets its brief, without a respawn.
    claude_reported_idle(&env);
    env.expire_wait("r1");
    env.reconcile().ok();
    let st = env.status("r1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "WorkspaceTrusted", "True", "Trusted");
    assert_condition(&st, "NeedsAttention", "False", "Resolved");
    assert_eq!(argv_of(&env, "create").len(), 1);
    assert_eq!(argv_of(&env, "message send").len(), 1);
}

#[test]
fn trust_that_cannot_be_read_is_unknown_never_a_brief_and_recovers() {
    let env = env("trust-unknown", "");
    claude_at_trust_dialog(&env, &path("/work/u1"));
    env.stub_fault("capture", "fail", 10);
    env.apply(task("u1", "claude")).ok();
    env.reconcile().ok();
    let st = env.status("u1");
    assert_eq!(argv_of(&env, "message send").len(), 0);
    assert_condition(&st, "WorkspaceTrusted", "Unknown", "TrustUnknown");
    assert!(
        env.get("u1")["status"]["notBefore"].is_null(),
        "Unknown is probed again on the next pass, without a retry wait"
    );
    assert_ne!(
        condition(&st, "NeedsAttention")["reason"],
        "WorkspaceUntrusted",
        "Unknown is not a known refusal: {st:#}"
    );

    // A thurbox that does not report agent arguments cannot prove Codex's
    // trust either: Unknown, never a guess.
    let codex = self::env("trust-unknown-codex", "");
    initial_codex(&codex);
    codex.edit_stub(|s| s["spawnFields"]["agent_args"] = Value::Null);
    codex.apply(task("u2", "codex")).ok();
    codex.reconcile().ok();
    assert_eq!(argv_of(&codex, "send").len(), 0);
    assert_condition(
        &codex.status("u2"),
        "WorkspaceTrusted",
        "Unknown",
        "TrustUnknown",
    );

    claude_reported_idle(&env);
    env.edit_stub(|s| s["faults"] = json!([]));
    env.expire_wait("u1");
    env.reconcile().ok();
    assert_eq!(env.status("u1")["phase"], "Briefed");
}

#[test]
fn a_harness_without_a_known_trust_interface_starts_nothing_until_configured() {
    let env = env("trust-unsupported", "");
    env.apply(task("a1", "aider")).ok();
    env.reconcile().ok();
    // A configuration gap, refused before any session exists: no session is
    // left behind without the trust a later mapping would have added.
    assert!(argv_of(&env, "create").is_empty(), "{:?}", env.stub_calls());
    let st = env.status("a1");
    assert_eq!(st["phase"], "Pending", "{st:#}");
    assert_condition(&st, "Spawned", "False", "TrustUnmapped");
    assert_condition(&st, "NeedsAttention", "True", "TrustUnmapped");
    assert!(
        condition(&st, "NeedsAttention")["message"]
            .to_string()
            .contains("runtime.workspaceTrust.aider"),
        "{st:#}"
    );
    // The gap spends no spawn attempt: waiting on it never stalls the Task.
    for _ in 0..4 {
        env.expire_wait("a1");
        env.reconcile().ok();
    }
    assert_eq!(env.status("a1")["phase"], "Pending");
    assert!(argv_of(&env, "create").is_empty());

    // The operator declares the harness has no workspace-trust gate.
    let mut cfg = std::fs::read_to_string(&env.config).expect("config");
    cfg.push_str("\n[runtime.workspaceTrust]\naider = \"none\"\n");
    std::fs::write(&env.config, cfg).expect("config");
    env.expire_wait("a1");
    env.reconcile().ok();
    // Trusted now, the brief goes out once. thurbox knows no agent inbox
    // for this harness and keeps it in its mailbox: not a delivery.
    let st = env.status("a1");
    assert_condition(&st, "WorkspaceTrusted", "True", "Trusted");
    assert_condition(&st, "Briefed", "False", "BriefUndelivered");
    assert_eq!(argv_of(&env, "message send").len(), 1, "{st:#}");

    let bad = Env::thurbox("trust-bad-config");
    let cfg = std::fs::read_to_string(&bad.config).expect("config");
    std::fs::write(
        &bad.config,
        format!("{cfg}\n[runtime.workspaceTrust]\naider = \"yes\"\n"),
    )
    .expect("config");
    bad.fleet(&["reconcile"])
        .code(1)
        .stderr_has("runtime.workspaceTrust");
}

#[test]
fn an_aliased_codex_harness_waits_for_its_mapping_then_starts_trusted() {
    // A thurbox agent entry such as `codex-coder` runs Codex under another
    // name. Fleet cannot tell its family, so it says so instead of starting
    // a session Codex would never trust.
    let env = env("trust-alias", "");
    initial_codex(&env);
    env.edit_stub(|s| s["spawnFields"]["agent"] = json!("codex-coder"));
    env.apply(task("c1", "codex-coder")).ok();
    env.reconcile().ok();
    assert!(argv_of(&env, "create").is_empty(), "{:?}", env.stub_calls());
    assert_condition(&env.status("c1"), "NeedsAttention", "True", "TrustUnmapped");

    let mut cfg = std::fs::read_to_string(&env.config).expect("config");
    cfg.push_str("\n[runtime.workspaceTrust]\ncodex-coder = \"codex\"\n");
    std::fs::write(&env.config, cfg).expect("config");
    env.expire_wait("c1");
    env.reconcile().ok();
    let create = &argv_of(&env, "create")[0];
    assert_eq!(
        agent_args(create),
        ["-c".to_string(), codex_trust(&path("/work/c1"))],
        "{create:?}"
    );
    let st = env.status("c1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "WorkspaceTrusted", "True", "Trusted");
    assert_condition(&st, "NeedsAttention", "False", "Resolved");
}

#[test]
fn a_home_or_filesystem_root_is_never_trusted_automatically() {
    let env = env("trust-root", "");
    initial_codex(&env);
    let mut t = task("root", "codex");
    t["spec"]["workspace"]["dir"] = json!(if cfg!(windows) { "C:\\" } else { "/" });
    env.apply(t).ok();
    env.reconcile().ok();
    assert!(argv_of(&env, "create").is_empty(), "{:?}", env.stub_calls());
    assert_eq!(argv_of(&env, "message send").len(), 0);
    assert!(
        env.get("root")["status"]
            .to_string()
            .contains("never trusted automatically"),
        "{:#}",
        env.get("root")["status"]
    );
}

#[test]
fn codex_declared_ungated_adds_no_trust_and_is_still_briefed() {
    // The operator trusts Codex's folders in Codex's own config and says so.
    let env = env(
        "trust-codex-none",
        "[runtime.workspaceTrust]\ncodex = \"none\"\n",
    );
    initial_codex(&env);
    env.apply(task("n1", "codex")).ok();
    env.reconcile().ok();
    assert!(agent_args(&argv_of(&env, "create")[0]).is_empty());
    let st = env.status("n1");
    assert_eq!(st["phase"], "Briefed", "{st:#}");
    assert_condition(&st, "WorkspaceTrusted", "True", "Trusted");
}
