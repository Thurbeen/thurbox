//! Activation-event matching for process plugins.
//!
//! A process plugin lists `activation_events` in its manifest. The
//! [`PluginRuntime`](super::PluginRuntime) keeps the plugin dormant until one
//! of its events fires — at which point it spawns the executable and runs the
//! handshake. Once spawned, a plugin stays running until shutdown or
//! `disable_plugin`; re-firing an event does not re-spawn.
//!
//! These helpers are pure: they answer "does this firing event match this
//! plugin's declared list?" and have no IO. The runtime drives the spawn
//! decision based on the answer.

use std::path::Path;

use crate::session::ActivationEvent;

/// A concrete event that just happened in the host. Match it against a
/// plugin's declared `activation_events` with [`event_matches`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FiredEvent<'a> {
    StartupFinished,
    BackendSelected(&'a str),
    Command(&'a str),
    Role(&'a str),
    /// A session opened on a workspace at this path. The matcher checks
    /// whether the relative path declared by the plugin exists under it.
    WorkspaceOpened(&'a Path),
}

/// True if `fired` would activate a plugin that declared `declared` in its manifest.
///
/// Matching is exact for everything except `OnWorkspaceContains`, which checks
/// whether the relative path joins to an existing entry under the workspace
/// root. This mirrors VSCode's `onWorkspaceContains` semantics.
pub fn event_matches(fired: &FiredEvent<'_>, declared: &ActivationEvent) -> bool {
    match (fired, declared) {
        (FiredEvent::StartupFinished, ActivationEvent::OnStartupFinished) => true,
        (FiredEvent::BackendSelected(name), ActivationEvent::OnBackendSelected(decl)) => {
            *name == decl.as_str()
        }
        (FiredEvent::Command(name), ActivationEvent::OnCommand(decl)) => *name == decl.as_str(),
        (FiredEvent::Role(name), ActivationEvent::OnRole(decl)) => *name == decl.as_str(),
        (FiredEvent::WorkspaceOpened(root), ActivationEvent::OnWorkspaceContains(decl)) => {
            root.join(decl).exists()
        }
        _ => false,
    }
}

/// True if any declared event in `declared` matches `fired` — this is the
/// OR-semantics the manifest documents.
pub fn any_event_matches(fired: &FiredEvent<'_>, declared: &[ActivationEvent]) -> bool {
    declared.iter().any(|d| event_matches(fired, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_finished_matches_only_itself() {
        assert!(event_matches(
            &FiredEvent::StartupFinished,
            &ActivationEvent::OnStartupFinished,
        ));
        assert!(!event_matches(
            &FiredEvent::StartupFinished,
            &ActivationEvent::OnRole("anything".into()),
        ));
    }

    #[test]
    fn backend_selected_requires_name_match() {
        assert!(event_matches(
            &FiredEvent::BackendSelected("gastown"),
            &ActivationEvent::OnBackendSelected("gastown".into()),
        ));
        assert!(!event_matches(
            &FiredEvent::BackendSelected("gastown"),
            &ActivationEvent::OnBackendSelected("local-tmux".into()),
        ));
    }

    #[test]
    fn command_requires_name_match() {
        assert!(event_matches(
            &FiredEvent::Command("publish"),
            &ActivationEvent::OnCommand("publish".into()),
        ));
        assert!(!event_matches(
            &FiredEvent::Command("publish"),
            &ActivationEvent::OnCommand("rollback".into()),
        ));
    }

    #[test]
    fn role_requires_name_match() {
        assert!(event_matches(
            &FiredEvent::Role("reviewer"),
            &ActivationEvent::OnRole("reviewer".into()),
        ));
        assert!(!event_matches(
            &FiredEvent::Role("reviewer"),
            &ActivationEvent::OnRole("polecat".into()),
        ));
    }

    #[test]
    fn workspace_contains_checks_real_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".beads")).unwrap();

        assert!(event_matches(
            &FiredEvent::WorkspaceOpened(dir.path()),
            &ActivationEvent::OnWorkspaceContains(".beads".into()),
        ));
        assert!(!event_matches(
            &FiredEvent::WorkspaceOpened(dir.path()),
            &ActivationEvent::OnWorkspaceContains("nonexistent".into()),
        ));
    }

    #[test]
    fn any_event_matches_or_semantics() {
        let declared = vec![
            ActivationEvent::OnStartupFinished,
            ActivationEvent::OnBackendSelected("gastown".into()),
        ];
        assert!(any_event_matches(&FiredEvent::StartupFinished, &declared));
        assert!(any_event_matches(
            &FiredEvent::BackendSelected("gastown"),
            &declared
        ));
        assert!(!any_event_matches(
            &FiredEvent::BackendSelected("nope"),
            &declared
        ));
    }

    #[test]
    fn cross_kind_never_matches() {
        assert!(!event_matches(
            &FiredEvent::Role("reviewer"),
            &ActivationEvent::OnCommand("reviewer".into()),
        ));
    }
}
