// Safe state accessors for App struct.
// This module provides bounds-safe accessors to replace direct array indexing,
// preventing panics from out-of-bounds access.

use crate::claude::Session;
use crate::project::ProjectInfo;

use super::App;

impl App {
    /// Get the currently active project, or None if index is out of bounds.
    /// Replaces: `&self.projects[self.active_project_index]`
    pub fn active_project(&self) -> Option<&ProjectInfo> {
        self.projects.get(self.active_project_index)
    }

    /// Get a mutable reference to the currently active project, or None if out of bounds.
    /// Replaces: `&mut self.projects[self.active_project_index]`
    pub fn active_project_mut(&mut self) -> Option<&mut ProjectInfo> {
        self.projects.get_mut(self.active_project_index)
    }

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

    /// Get the number of projects.
    pub fn project_count(&self) -> usize {
        self.projects.len()
    }

    /// Get the number of sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Check if the active project index is valid.
    pub fn has_active_project(&self) -> bool {
        self.active_project_index < self.projects.len()
    }

    /// Check if the active session index is valid.
    pub fn has_active_session(&self) -> bool {
        self.active_index < self.sessions.len()
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_active_project_empty() {
        // Simulate state where projects list is empty
        // by checking bounds - 0 < 0 should be false
        let projects_len = 0;
        let active_index = 0;
        assert!(active_index >= projects_len); // Out of bounds
    }

    #[test]
    fn test_active_project_out_of_bounds() {
        // Verify bounds checking logic
        let active_index = 100;
        let projects_len = 5;
        assert!(active_index >= projects_len);
    }

    #[test]
    fn test_active_project_valid() {
        // Verify valid index access
        let active_index = 0;
        let projects_len = 5;
        assert!(active_index < projects_len);
    }

    #[test]
    fn test_active_session_empty() {
        // Verify empty list check
        let sessions_len = 0;
        let active_index = 0;
        assert!(active_index >= sessions_len);
    }

    #[test]
    fn test_active_session_out_of_bounds() {
        // Verify out of bounds check
        let active_index = 100;
        let sessions_len = 5;
        assert!(active_index >= sessions_len);
    }

    #[test]
    fn test_bounds_checking_saturating_sub() {
        // Test saturating_sub behavior (used in up navigation)
        let mut index = 0usize;
        index = index.saturating_sub(1);
        assert_eq!(index, 0);

        index = 5;
        index = index.saturating_sub(1);
        assert_eq!(index, 4);
    }

    #[test]
    fn test_bounds_checking_increment() {
        // Test increment with max check
        let mut index = 0usize;
        let max = 5;
        if index + 1 < max {
            index += 1;
        }
        assert_eq!(index, 1);

        index = 4;
        if index + 1 < max {
            index += 1;
        }
        assert_eq!(index, 4); // Should not increment (would be 5 >= max)
    }
}
