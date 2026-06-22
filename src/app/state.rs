// Safe state accessors for App struct.
// This module provides bounds-safe accessors to replace direct array indexing,
// preventing panics from out-of-bounds access.

use crate::agent::Session;

use super::App;

impl App {
    /// Get the currently active session, or None if index is out of bounds.
    /// Replaces: `&self.sessions[self.active_index]`
    pub fn active_session(&self) -> Option<&Session> {
        self.sessions.get(self.active_index)
    }

    /// Get a mutable reference to the currently active session, or None if out of bounds.
    /// Replaces: `&mut self.sessions[self.active_index]`
    pub fn active_session_mut(&mut self) -> Option<&mut Session> {
        self.sessions.get_mut(self.active_index)
    }

    /// Get the number of sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Check if the active session index is valid.
    pub fn has_active_session(&self) -> bool {
        self.active_index < self.sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::App;
    use crate::agent::backend::SpawnedSession;
    use crate::agent::{AgentProvider, BackendRegistry, GenericProvider, Session, SessionBackend};
    use crate::storage::Database;
    use std::path::Path;
    use std::sync::Arc;

    /// Inert backend so a real [`App`] can be built without touching tmux.
    struct StubBackend;
    impl SessionBackend for StubBackend {
        fn name(&self) -> &str {
            "stub"
        }
        fn check_available(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn ensure_ready(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn spawn(
            &self,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<&Path>,
            _: &std::collections::HashMap<String, String>,
            _: u16,
            _: u16,
        ) -> anyhow::Result<SpawnedSession> {
            anyhow::bail!("stub backend does not spawn")
        }
        fn adopt(
            &self,
            _: &str,
            _: u16,
            _: u16,
        ) -> anyhow::Result<crate::agent::backend::AdoptedSession> {
            anyhow::bail!("stub backend does not adopt")
        }
        fn discover(&self) -> anyhow::Result<Vec<crate::agent::backend::DiscoveredSession>> {
            Ok(vec![])
        }
        fn resize(&self, _: &str, _: u16, _: u16) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_dead(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        fn kill(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn detach(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn pane_pid(&self, _: &str) -> anyhow::Result<Option<u32>> {
            Ok(None)
        }
    }

    /// Build a real [`App`] seeded with `count` stub sessions (`active_index` at
    /// 0 when non-empty), hermetic via a [`TestPathGuard`] tempdir the caller
    /// must keep alive for the `App`'s lifetime.
    fn app_with_sessions(count: usize) -> (App, crate::paths::TestPathGuard, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let guard = crate::paths::TestPathGuard::new(tmp.path());
        let backend: Arc<dyn SessionBackend> = Arc::new(StubBackend);
        let provider: Arc<dyn AgentProvider> = Arc::new(GenericProvider::new(
            crate::agent::agent_config::builtin_registry()
                .default_agent()
                .unwrap()
                .clone(),
        ));
        let mut app = App::new(
            24,
            120,
            BackendRegistry::new(Arc::clone(&backend)),
            crate::agent::agent_config::builtin_registry(),
            Database::open_in_memory().unwrap(),
        );
        for i in 0..count {
            app.sessions
                .push(Session::stub(&format!("session-{i}"), &backend, &provider));
        }
        if count > 0 {
            app.active_index = 0;
        }
        (app, guard, tmp)
    }

    #[test]
    fn active_session_returns_the_session_at_active_index() {
        let (mut app, _g, _t) = app_with_sessions(3);

        // Default selection points at the first session.
        assert_eq!(
            app.active_session().map(|s| s.info.name.as_str()),
            Some("session-0")
        );

        app.active_index = 2;
        assert_eq!(
            app.active_session().map(|s| s.info.name.as_str()),
            Some("session-2")
        );
    }

    #[test]
    fn active_session_is_none_when_no_sessions() {
        let (app, _g, _t) = app_with_sessions(0);
        assert!(app.active_session().is_none());
    }

    #[test]
    fn active_session_is_none_when_index_out_of_bounds() {
        let (mut app, _g, _t) = app_with_sessions(2);
        // An index past the end must not panic — the accessor guards it.
        app.active_index = 5;
        assert!(app.active_session().is_none());
        assert!(!app.has_active_session());
    }

    #[test]
    fn active_session_mut_yields_the_active_session() {
        let (mut app, _g, _t) = app_with_sessions(2);
        app.active_index = 1;

        // The mutable accessor reaches the same session and allows mutation.
        let session = app.active_session_mut().expect("session-1 is active");
        session.info.name = "renamed".to_string();

        assert_eq!(
            app.active_session().map(|s| s.info.name.as_str()),
            Some("renamed")
        );
    }

    #[test]
    fn active_session_mut_is_none_when_index_out_of_bounds() {
        let (mut app, _g, _t) = app_with_sessions(1);
        app.active_index = 9;
        assert!(app.active_session_mut().is_none());
    }

    #[test]
    fn session_count_tracks_the_session_list() {
        let (app0, _g0, _t0) = app_with_sessions(0);
        assert_eq!(app0.session_count(), 0);

        let (app3, _g3, _t3) = app_with_sessions(3);
        assert_eq!(app3.session_count(), 3);
    }

    #[test]
    fn has_active_session_reflects_index_validity() {
        let (mut app, _g, _t) = app_with_sessions(2);

        // In-bounds index → valid.
        app.active_index = 0;
        assert!(app.has_active_session());
        app.active_index = 1;
        assert!(app.has_active_session());

        // Index equal to len (one past the last) and beyond → invalid.
        app.active_index = 2;
        assert!(!app.has_active_session());
        app.active_index = 100;
        assert!(!app.has_active_session());
    }

    #[test]
    fn has_active_session_is_false_for_empty_app() {
        let (app, _g, _t) = app_with_sessions(0);
        assert!(!app.has_active_session());
        assert_eq!(app.session_count(), 0);
    }
}
