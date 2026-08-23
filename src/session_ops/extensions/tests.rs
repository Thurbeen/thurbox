//! `extensions`'s tests, kept together (the `git/tests.rs` pattern): one
//! sibling module of the split files, so their private items stay reachable.

use std::path::Path;

use crate::session::{
    AutomationAction, ExtensionAutomation, ExtensionDef, ExtensionSession, SessionId,
};
use crate::storage::Database;
use crate::sync::SharedSession;

use super::fs::*;
use super::install::*;
use super::lifecycle::*;

fn insert_session(db: &Database, name: &str) -> SessionId {
    let id = SessionId::default();
    let shared = SharedSession {
        id,
        name: name.into(),
        agent: "flow".into(),
        backend_id: String::new(),
        backend_type: "local-tmux".into(),
        agent_session_id: Some(uuid::Uuid::new_v4().to_string()),
        cwd: None,
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    };
    db.upsert_session(&shared).unwrap();
    id
}

/// A manifest whose single session already exists, so ensure never has to
/// spawn (which would need tmux) — only the automation is created.
fn flow_def() -> ExtensionDef {
    ExtensionDef {
        name: "flow".into(),
        description: None,
        config_version: Some(1),
        version: None,
        min_thurbox_version: None,
        installed_with: None,
        source: None,
        home: None,
        agents: Vec::new(),
        files: Vec::new(),
        external_files: Vec::new(),
        agent_patches: Vec::new(),
        config_merges: Vec::new(),
        symlinks: Vec::new(),
        sessions: vec![ExtensionSession {
            name: "flow".into(),
            agent: "flow".into(),
            repo_path: "/tmp/flow".into(),
        }],
        automations: vec![ExtensionAutomation {
            name: "flow-tick".into(),
            trigger: "cron:*/5 * * * *".into(),
            session_ref: Some("flow".into()),
            prompt: Some("tick".into()),
            command: None,
        }],
    }
}

#[test]
fn ensure_reuses_existing_session_and_creates_automation() {
    let db = Database::open_in_memory().unwrap();
    insert_session(&db, "flow");

    let report = ensure_extension(&db, &flow_def()).unwrap();
    assert!(report.sessions_created.is_empty(), "session was reused");
    assert_eq!(report.automations_created, ["flow-tick"]);

    let autos = db.list_automations().unwrap();
    assert_eq!(autos.len(), 1);
    assert_eq!(autos[0].name, "flow-tick");
    assert!(matches!(autos[0].action, AutomationAction::Send { .. }));
}

#[test]
fn ensure_is_idempotent() {
    let db = Database::open_in_memory().unwrap();
    insert_session(&db, "flow");
    let def = flow_def();

    ensure_extension(&db, &def).unwrap();
    let second = ensure_extension(&db, &def).unwrap();
    assert!(!second.created_anything(), "second pass creates nothing");
    assert_eq!(db.list_automations().unwrap().len(), 1);
}

#[test]
fn ensure_relinks_stale_send_target_after_session_recreated() {
    let db = Database::open_in_memory().unwrap();
    let old_id = insert_session(&db, "flow");
    let def = flow_def();

    // First pass binds the automation to the original session id.
    ensure_extension(&db, &def).unwrap();
    let auto = &db.list_automations().unwrap()[0];
    assert_eq!(auto.action, AutomationAction::Send { session_id: old_id });

    // The session is recreated under the same name with a fresh id (the
    // shape that orphaned the automation: soft-delete + new row).
    db.soft_delete_session(old_id).unwrap();
    let new_id = insert_session(&db, "flow");
    assert_ne!(old_id, new_id);

    // Self-heal re-links the existing automation to the live id rather than
    // leaving it pointing at the dead one.
    let report = ensure_extension(&db, &def).unwrap();
    assert!(report.automations_created.is_empty(), "no new automation");
    assert_eq!(report.automations_relinked, ["flow-tick"]);
    assert!(report.created_anything(), "a relink counts as repair");

    let auto = &db.list_automations().unwrap()[0];
    assert_eq!(auto.action, AutomationAction::Send { session_id: new_id });

    // A subsequent pass is a no-op now that the link is correct.
    let again = ensure_extension(&db, &def).unwrap();
    assert!(!again.created_anything(), "relink is idempotent");
}

#[test]
fn unknown_session_ref_errors() {
    let db = Database::open_in_memory().unwrap();
    insert_session(&db, "flow");
    let mut def = flow_def();
    def.automations[0].session_ref = Some("ghost".into());
    let err = ensure_extension(&db, &def).unwrap_err();
    assert!(err.contains("ghost"), "got: {err}");
}

#[test]
fn activate_records_active_set() {
    let db = Database::open_in_memory().unwrap();
    insert_session(&db, "flow");
    activate_extension(&db, &flow_def()).unwrap();
    assert_eq!(db.get_active_extensions().unwrap(), ["flow"]);
}

#[test]
fn deactivate_tears_down_and_clears_active_set() {
    let db = Database::open_in_memory().unwrap();
    insert_session(&db, "flow");
    let def = flow_def();
    activate_extension(&db, &def).unwrap();

    let report = deactivate_extension(&db, &def, false).unwrap();
    assert!(report.was_active);
    assert_eq!(report.automations_deleted, ["flow-tick"]);
    assert_eq!(report.sessions_deleted, ["flow"]);
    assert!(db.list_automations().unwrap().is_empty());
    assert!(
        db.get_active_extensions().unwrap().is_empty(),
        "self-heal must not resurrect a deactivated extension"
    );
}

#[test]
fn deactivate_is_idempotent() {
    let db = Database::open_in_memory().unwrap();
    let def = flow_def();
    // Nothing exists / not active — deactivate is a clean no-op.
    let report = deactivate_extension(&db, &def, false).unwrap();
    assert!(!report.was_active);
    assert!(report.automations_deleted.is_empty());
    assert!(report.sessions_deleted.is_empty());
}

#[test]
fn install_lays_files_registers_agents_and_activates() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();
    // Pre-create the session so activate reuses it (no tmux spawn in tests).
    insert_session(&db, "flow");

    // A local source dir with a manifest + payload.
    let src = tempfile::TempDir::new().unwrap();
    let home = temp.path().join("flowhome");
    std::fs::write(
        src.path().join("extension.toml"),
        format!(
            r#"name = "flow"
home = '{}'

[[agents]]
name = "flow"
command = "claude"
args = ["--model", "haiku"]

[[files]]
path = "FLOW.md"

[[files]]
path = "scripts/do.sh"
executable = true

[[files]]
path = "repos.md"
if_absent = true

[[files]]
path = ".claude/settings.json"
source = "settings.tmpl"
substitute = true

[[symlinks]]
link = "CLAUDE.md"
target = "FLOW.md"

[[sessions]]
name = "flow"
agent = "flow"
repo_path = "{{home}}"

[[automations]]
name = "flow-tick"
trigger = "cron:*/5 * * * *"
session_ref = "flow"
prompt = "tick"
"#,
            home.display()
        ),
    )
    .unwrap();
    std::fs::write(src.path().join("FLOW.md"), "spec").unwrap();
    std::fs::create_dir_all(src.path().join("scripts")).unwrap();
    std::fs::write(src.path().join("scripts/do.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(src.path().join("repos.md"), "seed table").unwrap();
    std::fs::write(src.path().join("settings.tmpl"), "perm {home}/x").unwrap();

    let target = src.path().to_string_lossy().to_string();
    let report = install_extension(&db, &target, None, false).unwrap();

    assert!(home.join("FLOW.md").exists());
    assert!(home.join("scripts/do.sh").exists());
    assert!(home.join("repos.md").exists());
    // {home} substituted in the settings template.
    let settings = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
    assert_eq!(settings, format!("perm {}/x", home.display()));
    assert!(std::fs::symlink_metadata(home.join("CLAUDE.md"))
        .unwrap()
        .file_type()
        .is_symlink());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join("scripts/do.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o100, 0o100, "do.sh should be executable");
    }

    assert_eq!(report.agents_added, ["flow"]);
    let reg = crate::agent::agent_config::load_or_seed();
    assert_eq!(reg.get("flow").unwrap().args, ["--model", "haiku"]);
    assert_eq!(db.get_active_extensions().unwrap(), ["flow"]);
    let stored = crate::agent::extension_config::load_manifest("flow").unwrap();
    // repo_path was resolved from {home} to the absolute home.
    assert_eq!(stored.sessions[0].repo_path, home);
    // Install provenance is stamped into the discovery manifest.
    assert_eq!(
        stored.installed_with.as_deref(),
        Some(crate::agent::extension_config::binary_version())
    );
    assert_eq!(stored.source.as_deref(), Some(target.as_str()));
    assert_eq!(report.ensure.automations_created, ["flow-tick"]);

    // Re-install is idempotent: repos.md kept (if_absent), no new agents.
    std::fs::write(home.join("repos.md"), "user edited").unwrap();
    let again = install_extension(&db, &target, None, false).unwrap();
    assert!(again.files_skipped.contains(&"repos.md".to_string()));
    assert_eq!(
        std::fs::read_to_string(home.join("repos.md")).unwrap(),
        "user edited",
        "if_absent file must not be clobbered on reinstall"
    );
    assert!(again.agents_added.is_empty());
}

#[test]
fn install_applies_agent_patches_and_external_files() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();

    // An out-of-home dir that exists (the "agent is installed" case) and a
    // plugin destination under it — kept inside the tempdir so the test
    // never touches the real home.
    let plugin_dir = temp.path().join("opencode");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let plugin_dest = plugin_dir.join("plugin/status.js");
    let home = temp.path().join("hookshome");

    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(
        src.path().join("extension.toml"),
        format!(
            r#"name = "hooks"
home = '{home}'

[[agent_patches]]
name = "claude"
append_args = ["--settings", "{{home}}/claude.json"]

[[external_files]]
path = '{plugin}'
source = "status.js"
requires_dir = '{plugin_dir}'
"#,
            home = home.display(),
            plugin = plugin_dest.display(),
            plugin_dir = plugin_dir.display(),
        ),
    )
    .unwrap();
    // The plugin payload carries the managed marker so uninstall can remove it.
    std::fs::write(
        src.path().join("status.js"),
        "// thurbox `extension install` managed\n",
    )
    .unwrap();

    let target = src.path().to_string_lossy().to_string();
    let report = install_extension(&db, &target, None, false).unwrap();

    // The built-in claude agent's args gained the --settings flag, resolved.
    assert_eq!(report.agents_patched, ["claude"]);
    let reg = crate::agent::agent_config::load_or_seed();
    let args = &reg.get("claude").unwrap().args;
    let expected = format!("{}/claude.json", home.display());
    assert!(
        args.windows(2)
            .any(|w| w == ["--settings".to_string(), expected.clone()]),
        "claude args carry the resolved --settings: {args:?}"
    );

    // The external plugin file landed in the out-of-home config dir.
    assert!(plugin_dest.is_file());
    assert!(report
        .external_files_written
        .iter()
        .any(|p| p == &plugin_dest.to_string_lossy()));

    // Uninstall reverses both: the patch is removed and the plugin deleted.
    let un = uninstall_extension(&db, "hooks", false).unwrap();
    assert_eq!(un.agents_unpatched, ["claude"]);
    assert!(!plugin_dest.exists(), "managed plugin removed on uninstall");
    let reg = crate::agent::agent_config::load_or_seed();
    assert!(!reg
        .get("claude")
        .unwrap()
        .args
        .contains(&"--settings".to_string()));
}

#[test]
fn config_merge_installs_and_reverts_without_clobbering_user_config() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();

    // The agent's config dir (gates the merge) + its shared settings file,
    // seeded with the user's own settings incl. a pre-existing empty map.
    let agent_dir = temp.path().join("dotgemini");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let settings = agent_dir.join("settings.json");
    std::fs::write(
        &settings,
        r#"{"theme":"dark","mcpServers":{},"hooks":{"BeforeTool":[{"command":"user"}]}}"#,
    )
    .unwrap();
    let home = temp.path().join("hookshome");

    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(
        src.path().join("extension.toml"),
        format!(
            r#"name = "hooks"
home = '{home}'

[[config_merges]]
path = '{settings}'
source = "gemini-hooks.json"
requires_dir = '{agent_dir}'
"#,
            home = home.display(),
            settings = settings.display(),
            agent_dir = agent_dir.display(),
        ),
    )
    .unwrap();
    std::fs::write(
            src.path().join("gemini-hooks.json"),
            r#"{"hooks":{"BeforeTool":[{"hooks":[{"type":"command","command":"thurbox-cli session signal --state working || true"}]}],"AfterAgent":[{"hooks":[{"type":"command","command":"thurbox-cli session signal --state done || true"}]}]}}"#,
        )
        .unwrap();

    let target = src.path().to_string_lossy().to_string();
    let report = install_extension(&db, &target, None, false).unwrap();
    assert_eq!(report.config_merges_applied, [settings.to_string_lossy()]);

    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    // User settings preserved (incl. the empty map) ...
    assert_eq!(merged["theme"], serde_json::json!("dark"));
    assert_eq!(merged["mcpServers"], serde_json::json!({}));
    // ... the user's own BeforeTool hook survived, ours was unioned in ...
    assert_eq!(merged["hooks"]["BeforeTool"].as_array().unwrap().len(), 2);
    assert_eq!(
        merged["hooks"]["BeforeTool"][0],
        serde_json::json!({"command": "user"})
    );
    // ... and our AfterAgent hook was added.
    assert!(merged["hooks"]["AfterAgent"].is_array());

    // A re-install is a no-op write (skipped, not re-applied).
    let again = install_extension(&db, &target, None, false).unwrap();
    assert!(again.config_merges_applied.is_empty());
    assert_eq!(again.config_merges_skipped, [settings.to_string_lossy()]);

    // Uninstall prunes exactly our entries; the user's config is restored.
    let un = uninstall_extension(&db, "hooks", false).unwrap();
    assert_eq!(un.config_merges_reverted, [settings.to_string_lossy()]);
    let restored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(
        restored,
        serde_json::json!({"theme":"dark","mcpServers":{},"hooks":{"BeforeTool":[{"command":"user"}]}})
    );
}

#[test]
fn config_merge_skipped_when_requires_dir_absent() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();

    // requires_dir points at a path that does NOT exist (agent not installed).
    let missing_dir = temp.path().join("not-installed");
    let settings = missing_dir.join("settings.json");
    let home = temp.path().join("hookshome");

    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(
        src.path().join("extension.toml"),
        format!(
            r#"name = "hooks"
home = '{home}'

[[config_merges]]
path = '{settings}'
source = "gemini-hooks.json"
requires_dir = '{missing}'
"#,
            home = home.display(),
            settings = settings.display(),
            missing = missing_dir.display(),
        ),
    )
    .unwrap();
    std::fs::write(src.path().join("gemini-hooks.json"), "{}").unwrap();

    let target = src.path().to_string_lossy().to_string();
    let report = install_extension(&db, &target, None, false).unwrap();
    assert_eq!(report.config_merges_skipped, [settings.to_string_lossy()]);
    assert!(report.config_merges_applied.is_empty());
    assert!(
        !settings.exists(),
        "no file created when the agent is absent"
    );
}

#[test]
fn config_merge_soft_skips_a_malformed_user_target() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();

    // The agent is installed (dir exists) but the user's settings file is
    // broken JSON. Install must NOT abort — it runs every startup + tick.
    let agent_dir = temp.path().join("dotgemini");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let settings = agent_dir.join("settings.json");
    std::fs::write(&settings, "{ this is not valid json").unwrap();
    let home = temp.path().join("hookshome");

    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(
        src.path().join("extension.toml"),
        format!(
            r#"name = "hooks"
home = '{home}'

[[config_merges]]
path = '{settings}'
source = "gemini-hooks.json"
requires_dir = '{agent_dir}'
"#,
            home = home.display(),
            settings = settings.display(),
            agent_dir = agent_dir.display(),
        ),
    )
    .unwrap();
    std::fs::write(src.path().join("gemini-hooks.json"), r#"{"hooks":{}}"#).unwrap();

    let target = src.path().to_string_lossy().to_string();
    // Install succeeds despite the broken target; the merge is soft-skipped...
    let report = install_extension(&db, &target, None, false).unwrap();
    assert_eq!(report.config_merges_skipped, [settings.to_string_lossy()]);
    assert!(report.config_merges_applied.is_empty());
    // ...and the user's (broken) file is left exactly as-is, not overwritten.
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        "{ this is not valid json"
    );
}

#[test]
fn config_merge_revert_soft_skips_a_malformed_target() {
    // If the user corrupts settings.json AFTER install, uninstall must not
    // abort on the unparseable file — it leaves it and reverts nothing.
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();

    let agent_dir = temp.path().join("dotgemini");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let settings = agent_dir.join("settings.json");
    std::fs::write(&settings, "{}").unwrap();
    let home = temp.path().join("hookshome");

    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(
        src.path().join("extension.toml"),
        format!(
            r#"name = "hooks"
home = '{home}'

[[config_merges]]
path = '{settings}'
source = "gemini-hooks.json"
requires_dir = '{agent_dir}'
"#,
            home = home.display(),
            settings = settings.display(),
            agent_dir = agent_dir.display(),
        ),
    )
    .unwrap();
    std::fs::write(
            src.path().join("gemini-hooks.json"),
            r#"{"hooks":{"AfterAgent":[{"hooks":[{"type":"command","command":"thurbox-cli session signal --state done || true"}]}]}}"#,
        )
        .unwrap();

    let target = src.path().to_string_lossy().to_string();
    install_extension(&db, &target, None, false).unwrap();
    // The user corrupts the file after install.
    std::fs::write(&settings, "}{ broken").unwrap();

    // Uninstall succeeds, reverts nothing, leaves the broken file untouched.
    let un = uninstall_extension(&db, "hooks", false).unwrap();
    assert!(un.config_merges_reverted.is_empty());
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), "}{ broken");
}

#[test]
fn external_file_skipped_when_requires_dir_absent() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();

    let missing_dir = temp.path().join("not-installed");
    let dest = missing_dir.join("plugin/status.js");
    let home = temp.path().join("h");
    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(
        src.path().join("extension.toml"),
        format!(
            r#"name = "hooks"
home = '{home}'

[[external_files]]
path = '{dest}'
source = "status.js"
requires_dir = '{req}'
"#,
            home = home.display(),
            dest = dest.display(),
            req = missing_dir.display(),
        ),
    )
    .unwrap();
    std::fs::write(
        src.path().join("status.js"),
        "// thurbox `extension install`\n",
    )
    .unwrap();

    let report = install_extension(&db, &src.path().to_string_lossy(), None, false).unwrap();
    assert!(!dest.exists(), "skipped because requires_dir is absent");
    assert!(report
        .external_files_skipped
        .iter()
        .any(|p| p == &dest.to_string_lossy()));
}

#[test]
fn ensure_safe_relative_rejects_traversal_and_absolute() {
    assert!(ensure_safe_relative("FLOW.md").is_ok());
    assert!(ensure_safe_relative("scripts/do.sh").is_ok());
    assert!(ensure_safe_relative("./a/b").is_ok());
    assert!(ensure_safe_relative("/etc/passwd").is_err());
    assert!(ensure_safe_relative("../escape").is_err());
    assert!(ensure_safe_relative("a/../../b").is_err());
}

#[test]
fn install_rejects_path_traversal_in_manifest() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();

    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(
        src.path().join("extension.toml"),
        format!(
            "name = \"evil\"\nhome = '{}'\n[[files]]\npath = \"../../pwned\"\n",
            temp.path().join("h").display()
        ),
    )
    .unwrap();
    std::fs::write(src.path().join("../../pwned"), "x").ok();

    let target = src.path().to_string_lossy().to_string();
    let err = install_extension(&db, &target, None, false).unwrap_err();
    assert!(err.contains("must not contain '..'"), "got: {err}");
}

#[test]
fn install_skips_user_modified_substitute_file() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();
    insert_session(&db, "flow");

    let src = tempfile::TempDir::new().unwrap();
    let home = temp.path().join("h");
    std::fs::write(
            src.path().join("extension.toml"),
            format!(
                "name = \"flow\"\nhome = '{}'\n[[files]]\npath = \"settings.json\"\nsubstitute = true\n[[sessions]]\nname = \"flow\"\nagent = \"flow\"\nrepo_path = \"{{home}}\"\n",
                home.display()
            ),
        )
        .unwrap();
    // Template carries the managed marker so a fresh install owns it.
    std::fs::write(
        src.path().join("settings.json"),
        "thurbox `extension install` managed {home}",
    )
    .unwrap();
    let target = src.path().to_string_lossy().to_string();

    // First install writes it.
    let r1 = install_extension(&db, &target, None, false).unwrap();
    assert!(r1.files_written.contains(&"settings.json".to_string()));

    // User edits it (drops the marker) → reinstall must not clobber it.
    std::fs::write(home.join("settings.json"), "MY CUSTOM PERMS").unwrap();
    let r2 = install_extension(&db, &target, None, false).unwrap();
    assert!(r2.files_skipped.contains(&"settings.json".to_string()));
    assert_eq!(
        std::fs::read_to_string(home.join("settings.json")).unwrap(),
        "MY CUSTOM PERMS"
    );

    // --force overrides and rewrites from the template.
    let r3 = install_extension(&db, &target, None, true).unwrap();
    assert!(r3.files_written.contains(&"settings.json".to_string()));
}

#[test]
fn install_defaults_home_under_extensions_dir() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();

    // Manifest with NO `home` field → falls through to the derived default.
    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(
        src.path().join("extension.toml"),
        "name = \"demo\"\n[[files]]\npath = \"NOTES.md\"\n",
    )
    .unwrap();
    std::fs::write(src.path().join("NOTES.md"), "hello\n").unwrap();

    let target = src.path().to_string_lossy().to_string();
    let report = install_extension(&db, &target, None, false).unwrap();

    // The Override path strategy maps the config dir under the test base, so
    // the default home is `<base>/extensions/demo` (sibling of demo.toml).
    let expected = temp.path().join("extensions").join("demo");
    assert_eq!(report.home, expected.to_string_lossy());
    assert!(
        expected.join("NOTES.md").exists(),
        "payload lands in the default home, not $HOME"
    );
}

#[test]
fn install_home_override_and_manifest_home_beat_default() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();

    // (a) A manifest-pinned `home` wins over the derived default.
    let pinned = temp.path().join("pinned");
    let src = tempfile::TempDir::new().unwrap();
    std::fs::write(
        src.path().join("extension.toml"),
        format!(
            "name = \"demo\"\nhome = '{}'\n[[files]]\npath = \"NOTES.md\"\n",
            pinned.display()
        ),
    )
    .unwrap();
    std::fs::write(src.path().join("NOTES.md"), "hi\n").unwrap();
    let target = src.path().to_string_lossy().to_string();
    let report = install_extension(&db, &target, None, false).unwrap();
    assert_eq!(report.home, pinned.to_string_lossy());

    // (b) `--home` beats both the manifest home and the default.
    let override_home = temp.path().join("override");
    let report =
        install_extension(&db, &target, Some(&override_home.to_string_lossy()), false).unwrap();
    assert_eq!(report.home, override_home.to_string_lossy());
}

#[test]
// Only ext test that force-deletes then re-spawns a session, so on Windows
// `install#2` spawns a real psmux pane with `cwd = flowhome`. psmux holds an
// OS handle to that working dir and only releases it on `kill-server`
// (`kill-window`/`respawn-pane`/waiting do NOT release it — verified directly
// in the Windows VM), so `remove_dir_all(flowhome)` hits os error 32. This is
// an upstream psmux limitation, not a thurbox bug; `force_teardown`'s
// pane-reap + `remove_dir_all_resilient` are partial mitigations but cannot
// free a *server*-held handle without killing the shared server.
#[cfg_attr(windows, ignore = "psmux leaks the pane cwd handle until kill-server")]
fn uninstall_reverses_install() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();
    insert_session(&db, "flow");

    let src = tempfile::TempDir::new().unwrap();
    let home = temp.path().join("flowhome");
    std::fs::write(
        src.path().join("extension.toml"),
        format!(
            r#"name = "flow"
home = '{}'
[[agents]]
name = "flow"
command = "claude"
[[files]]
path = "FLOW.md"
[[sessions]]
name = "flow"
agent = "flow"
repo_path = "{{home}}"
[[automations]]
name = "flow-tick"
trigger = "cron:*/5 * * * *"
session_ref = "flow"
prompt = "tick"
"#,
            home.display()
        ),
    )
    .unwrap();
    std::fs::write(src.path().join("FLOW.md"), "spec").unwrap();
    let target = src.path().to_string_lossy().to_string();

    install_extension(&db, &target, None, false).unwrap();
    assert!(home.join("FLOW.md").exists());
    assert!(crate::agent::agent_config::load_or_seed()
        .get("flow")
        .is_some());
    assert_eq!(db.get_active_extensions().unwrap(), ["flow"]);

    // Uninstall without --purge keeps the home dir but removes everything else.
    let report = uninstall_extension(&db, "flow", false).unwrap();
    assert_eq!(report.agents_removed, ["flow"]);
    assert!(report.manifest_removed);
    assert!(report.home_removed.is_none());
    assert!(crate::agent::agent_config::load_or_seed()
        .get("flow")
        .is_none());
    assert!(db.get_active_extensions().unwrap().is_empty());
    assert!(crate::agent::extension_config::load_manifest("flow").is_none());
    assert!(home.join("FLOW.md").exists(), "home kept without --purge");

    // Reinstall, then uninstall --purge removes the home dir too.
    install_extension(&db, &target, None, false).unwrap();
    let report = uninstall_extension(&db, "flow", true).unwrap();
    assert_eq!(
        report.home_removed.as_deref(),
        Some(home.to_string_lossy().as_ref())
    );
    assert!(!home.exists(), "home removed with --purge");
}

#[test]
fn update_refetches_from_recorded_source_and_reports_version_move() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();
    insert_session(&db, "flow");

    let src = tempfile::TempDir::new().unwrap();
    let home = temp.path().join("flowhome");
    let manifest = |version: &str| {
        format!(
                "name = \"flow\"\nversion = \"{version}\"\nhome = '{}'\n[[files]]\npath = \"FLOW.md\"\n[[sessions]]\nname = \"flow\"\nagent = \"flow\"\nrepo_path = \"{{home}}\"\n",
                home.display()
            )
    };
    std::fs::write(src.path().join("extension.toml"), manifest("1.0.0")).unwrap();
    std::fs::write(src.path().join("FLOW.md"), "v1 spec").unwrap();
    let target = src.path().to_string_lossy().to_string();

    install_extension(&db, &target, None, false).unwrap();
    let stored = crate::agent::extension_config::load_manifest("flow").unwrap();
    assert_eq!(stored.version.as_deref(), Some("1.0.0"));
    assert_eq!(stored.source.as_deref(), Some(target.as_str()));

    // Author publishes a new version at the same source; update pulls it.
    std::fs::write(src.path().join("extension.toml"), manifest("2.0.0")).unwrap();
    std::fs::write(src.path().join("FLOW.md"), "v2 spec").unwrap();

    let report = update_extension(&db, "flow", false).unwrap();
    assert!(report.changed, "version moved 1.0.0 -> 2.0.0");
    assert_eq!(report.install.previous_version.as_deref(), Some("1.0.0"));
    assert_eq!(report.install.version.as_deref(), Some("2.0.0"));
    assert_eq!(
        std::fs::read_to_string(home.join("FLOW.md")).unwrap(),
        "v2 spec"
    );
    assert_eq!(
        crate::agent::extension_config::load_manifest("flow")
            .unwrap()
            .version
            .as_deref(),
        Some("2.0.0")
    );

    // A no-op update (same source, unchanged) reports changed = false.
    let again = update_extension(&db, "flow", false).unwrap();
    assert!(!again.changed);
}

/// Install a fixture extension from a local source, then bump the source's
/// version so the discovery copy is one release behind. Returns the temp
/// guards (kept alive by the caller), the db, and the home dir.
fn install_then_bump_source(
    temp: &tempfile::TempDir,
    src: &tempfile::TempDir,
) -> (Database, std::path::PathBuf) {
    let db = Database::open_in_memory().unwrap();
    insert_session(&db, "flow");
    let home = temp.path().join("flowhome");
    let manifest = |version: &str| {
        format!(
                "name = \"flow\"\nversion = \"{version}\"\nhome = '{}'\n[[files]]\npath = \"FLOW.md\"\n[[sessions]]\nname = \"flow\"\nagent = \"flow\"\nrepo_path = \"{{home}}\"\n",
                home.display()
            )
    };
    std::fs::write(src.path().join("extension.toml"), manifest("1.0.0")).unwrap();
    std::fs::write(src.path().join("FLOW.md"), "v1 spec").unwrap();
    let target = src.path().to_string_lossy().to_string();
    install_extension(&db, &target, None, false).unwrap();
    // Author publishes a new version at the same recorded source.
    std::fs::write(src.path().join("extension.toml"), manifest("2.0.0")).unwrap();
    std::fs::write(src.path().join("FLOW.md"), "v2 spec").unwrap();
    (db, home)
}

#[test]
fn heal_auto_updates_stale_extension_when_enabled() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let src = tempfile::TempDir::new().unwrap();
    let (db, home) = install_then_bump_source(&temp, &src);

    // A non-dev `current` newer than `installed_with` (= the dev build that
    // installed it) makes the extension stale; auto_update = true refreshes it.
    let def = crate::agent::extension_config::load_manifest("flow").unwrap();
    let mut messages = Vec::new();
    let updated = heal_version_drift(&db, &def, "flow", "9.9.9", true, &mut messages);

    assert!(
        updated,
        "auto-update returns true so the caller skips ensure"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Auto-updated extension 'flow' to v2.0.0")),
        "got: {messages:?}"
    );
    // The discovery copy now carries the new version + this binary's stamp.
    let stored = crate::agent::extension_config::load_manifest("flow").unwrap();
    assert_eq!(stored.version.as_deref(), Some("2.0.0"));
    assert_eq!(
        stored.installed_with.as_deref(),
        Some(crate::agent::extension_config::binary_version())
    );
    assert_eq!(
        std::fs::read_to_string(home.join("FLOW.md")).unwrap(),
        "v2 spec"
    );
}

#[test]
fn heal_nudges_stale_extension_when_auto_update_off() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let src = tempfile::TempDir::new().unwrap();
    let (db, home) = install_then_bump_source(&temp, &src);

    let def = crate::agent::extension_config::load_manifest("flow").unwrap();
    let mut messages = Vec::new();
    let updated = heal_version_drift(&db, &def, "flow", "9.9.9", false, &mut messages);

    assert!(
        !updated,
        "no auto-update, so the caller still ensures resources"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("run `thurbox-cli extension update flow`")),
        "got: {messages:?}"
    );
    // The discovery copy is untouched — still the old version, never fetched.
    assert_eq!(
        crate::agent::extension_config::load_manifest("flow")
            .unwrap()
            .version
            .as_deref(),
        Some("1.0.0")
    );
    assert_eq!(
        std::fs::read_to_string(home.join("FLOW.md")).unwrap(),
        "v1 spec"
    );
}

#[test]
fn heal_does_not_auto_update_a_current_extension() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let src = tempfile::TempDir::new().unwrap();
    let (db, _home) = install_then_bump_source(&temp, &src);

    // `current` equals what installed it → not stale → no fetch, no message,
    // and the caller proceeds to its own ensure (returns false).
    let def = crate::agent::extension_config::load_manifest("flow").unwrap();
    let installed_with = def.installed_with.clone().unwrap();
    let mut messages = Vec::new();
    let updated = heal_version_drift(&db, &def, "flow", &installed_with, true, &mut messages);

    assert!(!updated);
    assert!(messages.is_empty(), "got: {messages:?}");
    assert_eq!(
        crate::agent::extension_config::load_manifest("flow")
            .unwrap()
            .version
            .as_deref(),
        Some("1.0.0"),
        "current source never fetched"
    );
}

#[test]
fn heal_warns_without_auto_updating_when_binary_too_old() {
    // Binary older than the extension's `min_thurbox_version`: an update
    // can't help (the matching extension version targets a newer binary), so
    // even with auto_update on we only warn and never call update_extension.
    // No install/source needed — the compat branch returns before touching db.
    let db = Database::open_in_memory().unwrap();
    let mut def = flow_def();
    def.min_thurbox_version = Some("5.0.0".into());
    let mut messages = Vec::new();
    let updated = heal_version_drift(&db, &def, "flow", "1.0.0", true, &mut messages);

    assert!(!updated, "compat warning is not an auto-update");
    assert_eq!(messages.len(), 1, "got: {messages:?}");
    assert!(
        messages[0].contains("wants thurbox >= 5.0.0"),
        "got: {messages:?}"
    );
}

#[test]
fn heal_falls_back_to_nudge_when_auto_update_fetch_fails() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let src = tempfile::TempDir::new().unwrap();
    let (db, _home) = install_then_bump_source(&temp, &src);

    // Make the recorded source unreachable so the in-place refresh errors.
    drop(src);

    let def = crate::agent::extension_config::load_manifest("flow").unwrap();
    let mut messages = Vec::new();
    let updated = heal_version_drift(&db, &def, "flow", "9.9.9", true, &mut messages);

    assert!(
        !updated,
        "a failed update returns false so the caller ensures"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("run `thurbox-cli extension update flow`")),
        "falls back to the manual nudge; got: {messages:?}"
    );
    // The discovery copy is untouched — the failed fetch wrote nothing.
    assert_eq!(
        crate::agent::extension_config::load_manifest("flow")
            .unwrap()
            .version
            .as_deref(),
        Some("1.0.0")
    );
}

#[test]
fn update_errors_when_no_recorded_source() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();
    // A manifest installed by an older thurbox carries no `source`.
    crate::agent::extension_config::write_manifest(&ExtensionDef {
        name: "legacy".into(),
        ..Default::default()
    })
    .unwrap();
    let err = update_extension(&db, "legacy", false).unwrap_err();
    assert!(err.contains("no recorded install source"), "got: {err}");
}

#[test]
// Real-spawns a session (ensure_extension → spawn), so it needs a live
// multiplexer. The GH windows-latest runner has no psmux installed; this
// real-spawn path is covered by the dockur VM suite instead.
#[cfg_attr(
    windows,
    ignore = "needs a multiplexer; not installed on the GH windows runner"
)]
fn reinstall_tears_down_then_installs_fresh() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();
    insert_session(&db, "flow");

    let src = tempfile::TempDir::new().unwrap();
    let home = temp.path().join("flowhome");
    std::fs::write(
            src.path().join("extension.toml"),
            format!(
                "name = \"flow\"\nversion = \"1.0.0\"\nhome = '{}'\n[[files]]\npath = \"seed.md\"\nif_absent = true\n[[sessions]]\nname = \"flow\"\nagent = \"flow\"\nrepo_path = \"{{home}}\"\n",
                home.display()
            ),
        )
        .unwrap();
    std::fs::write(src.path().join("seed.md"), "pristine seed").unwrap();
    let target = src.path().to_string_lossy().to_string();

    install_extension(&db, &target, None, false).unwrap();
    // User edits the if_absent seed — update without --force would keep it.
    std::fs::write(home.join("seed.md"), "user edit").unwrap();

    let report = reinstall_extension(&db, "flow", false).unwrap();
    assert_eq!(report.name, "flow");
    assert!(report.uninstall.manifest_removed);
    assert_eq!(report.install.version.as_deref(), Some("1.0.0"));
    // Reinstall forces even the if_absent seed back to pristine.
    assert_eq!(
        std::fs::read_to_string(home.join("seed.md")).unwrap(),
        "pristine seed"
    );
    // The extension is installed + active again afterwards.
    assert!(crate::agent::extension_config::load_manifest("flow").is_some());
}

#[test]
fn reinstall_errors_when_no_recorded_source() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = crate::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();
    crate::agent::extension_config::write_manifest(&ExtensionDef {
        name: "legacy".into(),
        ..Default::default()
    })
    .unwrap();
    let err = reinstall_extension(&db, "legacy", false).unwrap_err();
    assert!(err.contains("no recorded install source"), "got: {err}");
}

#[test]
fn guard_refuses_shallow_dirs() {
    assert!(guard_removable_dir(Path::new("/x")).is_err());
    assert!(guard_removable_dir(Path::new("/home/me/flow")).is_ok());
}

#[test]
fn health_reports_presence_and_active_flag() {
    let db = Database::open_in_memory().unwrap();
    let def = flow_def();

    let before = extension_health(&db, &def).unwrap();
    assert!(!before.active);
    assert_eq!(before.sessions, [("flow".to_string(), false)]);
    assert_eq!(before.automations, [("flow-tick".to_string(), false)]);
    assert!(!before.is_healthy());

    insert_session(&db, "flow");
    activate_extension(&db, &def).unwrap();
    let after = extension_health(&db, &def).unwrap();
    assert!(after.active);
    assert!(after.is_healthy());
}
