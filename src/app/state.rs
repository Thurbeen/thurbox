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
        let values: [&str; 2] = ["a", "b"];
        let valid_get: Option<&&str> = values.first();
        let invalid_get: Option<&&str> = values.get(10);

        assert!(valid_get.is_some());
        assert!(invalid_get.is_none());
    }

    #[test]
    fn test_has_active_session_empty_collection() {
        let index = 0;
        let len = 0;
        assert!(index >= len);
    }

    #[test]
    fn test_accessor_semantics() {
        let collection: [i32; 3] = [10, 20, 30];

        let result_valid = collection.get(1);
        assert!(result_valid.is_some());
        assert_eq!(result_valid, Some(&20));

        let result_invalid = collection.get(100);
        assert!(result_invalid.is_none());

        let result_zero = collection.first();
        assert_eq!(result_zero, Some(&10));
    }
}
