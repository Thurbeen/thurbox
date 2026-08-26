//! Loading and seeding of the session lifecycle hooks file.
//!
//! `~/.config/thurbox/hooks.toml` declares the user's own commands to run
//! before and after thurbox creates, deletes, restarts or restores a session.
//! It is the reverse of the `hooks/` directory beside it, which is where the
//! built-in `hooks` *extension* keeps the status-hook files it installs into
//! the agent CLIs. On first run the file is seeded fully commented-out, so a
//! fresh install runs nothing. If it exists but cannot be read or parsed, no
//! hooks run and a warning says why — the same degrade `hosts.toml` chose,
//! since a broken config file must never block a session operation.
//!
//! The file is read each time an event fires, on the worker firing it: one
//! small read per session operation buys an edit that is in force at once and
//! no cache to give an age to.

use std::path::PathBuf;

use crate::session::{HookEvent, HooksFile, LifecycleHook};

/// Seed contents for `hooks.toml` on first run: full documentation plus one
/// commented-out example per event, but no active hooks.
pub const SEED_HOOKS_TOML: &str = r#"# Thurbox session lifecycle hooks  —  ~/.config/thurbox/hooks.toml
#
# Each [[hooks]] entry runs a shell command of yours at one moment in a
# session's life: before or after thurbox creates, deletes, restarts or
# restores it. Hooks run on the machine thurbox runs on, whichever interface
# asked for the operation — the TUI, `thurbox-cli`, an automation, an
# extension — once per operation.
#
# NOT the same thing as the `hooks/` directory next to this file: that is
# where the built-in `hooks` extension keeps the status-hook files it installs
# INTO the agent CLIs (so an agent can tell thurbox it is working/blocked/
# done). This file is the other direction — thurbox telling your scripts what
# it is doing.
#
# Events (pre = before the operation has any effect; post = after it fully
# succeeded, and never after a failure):
#
#   session.pre_create    session.post_create
#   session.pre_delete    session.post_delete
#   session.pre_restart   session.post_restart
#   session.pre_restore   session.post_restore
#
# A `pre_*` hook can REFUSE the operation: exit non-zero (or exceed the
# timeout) and nothing happens — no worktree, no process, no row changed —
# and its stderr is the reported reason. Later hooks for that event do not
# run. A `post_*` hook is informational: every one runs, a failure is logged
# and reported but never fails the operation.
#
# What a hook receives, as environment variables (unset — not empty — when
# the fact is not known at that moment):
#
#   THURBOX_HOOK_EVENT       the event name, e.g. "session.post_create"
#   THURBOX_SESSION          the thurbox session id (at pre_create: the id it
#                            will have if creation succeeds)
#   THURBOX_SESSION_ID       the agent's own conversation id
#   THURBOX_SESSION_NAME     the session name
#   THURBOX_AGENT            the agent, e.g. "claude"
#   THURBOX_REPO             the primary repository path
#   THURBOX_CWD              the directory the agent runs in (the worktree, or
#                            the symlink workspace of a multi-repo session);
#                            unset at pre_create, before the worktree exists
#   THURBOX_BRANCH           the worktree branch, when there is one
#   THURBOX_BASE_BRANCH      the branch it was created from (create events)
#   THURBOX_HOST             the remote host name; unset for a local session.
#                            When set, THURBOX_REPO/THURBOX_CWD are paths on
#                            that host — the hook itself still runs locally
#   THURBOX_PARENT_SESSION   the parent session id (a fork, or --parent)
#   THURBOX_TASK             the originating task id, for a task-spawned session
#   THURBOX_CONFIG_DIR / THURBOX_DATA_DIR
#                            so a `thurbox-cli` you run inside the hook hits the
#                            same database as the thurbox that fired it
#
# The same facts — plus `worktrees` (repo_path, worktree_path, branch), the
# additional directories, and `force`/`force_deleted` for delete/restore —
# arrive as ONE JSON object on stdin, for anything structured:
#   jq -r .cwd
#
# The hook runs through `sh -c` (`cmd /C` on Windows), in the primary
# repository when that is a directory on this machine (otherwise in thurbox's
# own working directory), with no terminal: stdout/stderr are captured and only
# their tail is reported. It is killed after `timeout_secs` (default 30).
#
# Unknown keys are reported on startup (and fail `thurbox-cli config
# validate`) but don't break the load. A file that fails to parse means NO
# hooks run, with a warning — never a blocked session.
#
# Fields per [[hooks]] entry:
#
#   event          (string, required)   one of the eight event names above
#   command        (string, required)   the shell command
#   timeout_secs   (integer, optional, default: 30)
#
config_version = 1

# ──────────────────────────────────────────────────────────────────────────
# Examples — every entry below is commented out. Uncomment and edit to use.
# ──────────────────────────────────────────────────────────────────────────

# Refuse a session on a branch nobody should work on directly.
# [[hooks]]
# event = "session.pre_create"
# command = 'case "$THURBOX_BRANCH" in main|master) echo "refusing: protected branch" >&2; exit 1;; esac'

# Copy a local env file and warm the dependencies in a fresh worktree.
# [[hooks]]
# event = "session.post_create"
# command = '[ -n "$THURBOX_CWD" ] && cp -n .env.local "$THURBOX_CWD/.env" 2>/dev/null; true'
# timeout_secs = 120

# Ask before deleting a session that still has a running build.
# [[hooks]]
# event = "session.pre_delete"
# command = 'test ! -f "$THURBOX_CWD/.build-lock"'

# Tell a channel a session is gone.
# [[hooks]]
# event = "session.post_delete"
# command = 'notify-send "thurbox" "deleted $THURBOX_SESSION_NAME"'

# [[hooks]]
# event = "session.pre_restart"
# command = 'echo "restarting $THURBOX_SESSION_NAME" >> ~/thurbox-hooks.log'

# [[hooks]]
# event = "session.post_restart"
# command = 'echo "restarted $THURBOX_SESSION_NAME" >> ~/thurbox-hooks.log'

# [[hooks]]
# event = "session.pre_restore"
# command = 'jq -e ".force_deleted | not"'   # refuse a lossy restore

# [[hooks]]
# event = "session.post_restore"
# command = 'notify-send "thurbox" "restored $THURBOX_SESSION_NAME"'
"#;

/// Path to the lifecycle hooks file: `~/.config/thurbox/hooks.toml` (sibling
/// of `config.toml`; `~/.config/thurbox-dev/hooks.toml` on a dev build).
pub fn hooks_config_path() -> Option<PathBuf> {
    crate::paths::config_file().map(|p| p.with_file_name("hooks.toml"))
}

/// Load the hooks file, seeding it commented-out when absent. Any read/parse
/// error degrades to no hooks; the warnings are logged here (headless callers)
/// — [`load_or_seed_with_warnings`] returns them for a caller that surfaces
/// them itself.
pub fn load_or_seed() -> HooksFile {
    let (file, warnings) = load_or_seed_with_warnings();
    for w in &warnings {
        tracing::warn!("{w}");
    }
    file
}

/// [`load_or_seed`], also returning user-facing warnings for anything that
/// silently degraded (parse error → no hooks, seed failure, unknown keys).
pub fn load_or_seed_with_warnings() -> (HooksFile, Vec<String>) {
    let Some(path) = hooks_config_path() else {
        return (
            HooksFile::default(),
            vec!["Could not resolve hooks.toml path; no lifecycle hooks".into()],
        );
    };

    if !path.exists() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return (
                    HooksFile::default(),
                    vec![format!("Failed to create config dir for hooks.toml: {e}")],
                );
            }
        }
        if let Err(e) = std::fs::write(&path, SEED_HOOKS_TOML) {
            return (
                HooksFile::default(),
                vec![format!("Failed to seed hooks.toml: {e}")],
            );
        }
        tracing::info!(path = %path.display(), "Seeded hooks.toml (no active hooks)");
        return (HooksFile::default(), Vec::new());
    }

    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            match super::agent_config::parse_toml_reporting_unknown::<HooksFile>(
                &contents,
                "hooks.toml",
            ) {
                Ok((file, warnings)) => (file, warnings),
                Err(e) => (
                    HooksFile::default(),
                    vec![format!(
                        "hooks.toml: {}; no lifecycle hooks",
                        super::agent_config::compact_toml_error(&e.to_string())
                    )],
                ),
            }
        }
        Err(e) => (
            HooksFile::default(),
            vec![format!("Failed to read hooks.toml: {e}")],
        ),
    }
}

/// The hooks declared for `event`, in file order, read from disk now.
pub fn hooks_for(event: HookEvent) -> Vec<LifecycleHook> {
    load_or_seed().for_event(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_toml_parses_to_no_hooks() {
        let file: HooksFile = toml::from_str(SEED_HOOKS_TOML).unwrap();
        assert!(file.hooks.is_empty());
        assert_eq!(file.config_version, Some(1));
    }

    /// The seeded file is the documentation users see, so it names every
    /// field, every event, every variable a hook can read, and the line
    /// between it and the extension's `hooks/` directory.
    #[test]
    fn seed_toml_documents_every_field_event_and_variable() {
        for field in ["event", "command", "timeout_secs"] {
            assert!(
                SEED_HOOKS_TOML.contains(field),
                "seed must document '{field}'"
            );
        }
        for event in HookEvent::ALL {
            assert!(
                SEED_HOOKS_TOML.contains(&format!("event = \"{}\"", event.name())),
                "seed must show an example for {event}"
            );
        }
        let ctx = crate::session::HookContext {
            session_id: Some(crate::session::SessionId::default()),
            name: "n".into(),
            agent: "a".into(),
            agent_session_id: Some("c".into()),
            repo: Some("/r".into()),
            cwd: Some("/c".into()),
            branch: Some("b".into()),
            base_branch: Some("m".into()),
            host: Some("h".into()),
            parent_session_id: Some(crate::session::SessionId::default()),
            task_id: Some(1),
            ..Default::default()
        };
        for (name, _) in ctx.env(HookEvent::PostCreate) {
            assert!(SEED_HOOKS_TOML.contains(&name), "seed must document {name}");
        }
        assert!(
            SEED_HOOKS_TOML.contains("hooks/"),
            "seed must draw the line to the extension dir"
        );
    }

    #[test]
    fn load_or_seed_writes_file_when_absent_and_stays_empty() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = hooks_config_path().unwrap();
        assert!(!path.exists());

        let (file, warnings) = load_or_seed_with_warnings();
        assert!(file.hooks.is_empty());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(path.exists(), "hooks.toml should have been seeded");
        assert!(load_or_seed().hooks.is_empty());
    }

    #[test]
    fn load_or_seed_falls_back_on_malformed_file_naming_it() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = hooks_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not = valid toml {{{").unwrap();

        let (file, warnings) = load_or_seed_with_warnings();
        assert!(file.hooks.is_empty());
        assert!(
            warnings.iter().any(|w| w.contains("hooks.toml")),
            "{warnings:?}"
        );
        assert!(hooks_for(HookEvent::PreCreate).is_empty());
    }

    #[test]
    fn an_unknown_event_means_no_hooks_and_a_warning() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = hooks_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[[hooks]]\nevent = \"session.on_create\"\ncommand = \"true\"\n",
        )
        .unwrap();

        let (file, warnings) = load_or_seed_with_warnings();
        assert!(file.hooks.is_empty());
        assert!(!warnings.is_empty());
    }

    #[test]
    fn hooks_for_returns_the_events_entries_in_file_order() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let path = hooks_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[[hooks]]\nevent = \"session.post_create\"\ncommand = \"one\"\n\
             [[hooks]]\nevent = \"session.pre_create\"\ncommand = \"other\"\n\
             [[hooks]]\nevent = \"session.post_create\"\ncommand = \"two\"\n",
        )
        .unwrap();

        let commands: Vec<String> = hooks_for(HookEvent::PostCreate)
            .into_iter()
            .map(|h| h.command)
            .collect();
        assert_eq!(commands, ["one", "two"]);
        assert_eq!(hooks_for(HookEvent::PreDelete).len(), 0);
    }
}
