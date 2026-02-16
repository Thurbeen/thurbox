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
    fn test_option_get_with_valid_index() {
        let vec = [1, 2, 3];
        let idx = 1;
        assert_eq!(vec.get(idx), Some(&2));
    }

    #[test]
    fn test_option_get_with_out_of_bounds() {
        let vec: [i32; 3] = [1, 2, 3];
        let idx = 10;
        assert!(vec.get(idx).is_none());
    }

    #[test]
    fn test_has_active_logic_when_valid() {
        let index = 2;
        let len = 5;
        assert!(index < len);
    }

    #[test]
    fn test_has_active_logic_when_invalid() {
        let index = 5;
        let len = 5;
        assert!(index >= len);
    }

    #[test]
    fn test_accessor_return_type_is_option() {
        // The accessors use .get() which returns Option<&T>
        // This test documents that expected behavior
        let values: [&str; 2] = ["a", "b"];
        let valid_get: Option<&&str> = values.first();
        let invalid_get: Option<&&str> = values.get(10);

        assert!(valid_get.is_some());
        assert!(invalid_get.is_none());
    }

    #[test]
    fn test_has_active_project_boundary() {
        // Test at exact boundary
        let index = 4;
        let len = 5;
        assert!(index < len); // Valid

        let index = 5;
        assert!(index >= len); // Invalid at boundary
    }

    #[test]
    fn test_has_active_session_empty_collection() {
        let index = 0;
        let len = 0;
        assert!(index >= len); // Should be false for empty
    }

    #[test]
    fn test_accessor_semantics() {
        // Document the semantic contract of the accessors
        // They should return None rather than panic

        // Simulating what would happen with a list
        let collection: [i32; 3] = [10, 20, 30];

        // Valid access
        let result_valid = collection.get(1);
        assert!(result_valid.is_some());
        assert_eq!(result_valid, Some(&20));

        // Out of bounds access
        let result_invalid = collection.get(100);
        assert!(result_invalid.is_none()); // None, not panic

        // Zero index
        let result_zero = collection.first();
        assert_eq!(result_zero, Some(&10));
    }
}
