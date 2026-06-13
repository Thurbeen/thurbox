//! Key/value settings stored in the `metadata` table.

use rusqlite::{params, OptionalExtension};

use super::Database;

const EDITOR_COMMAND_KEY: &str = "editor_command";
const THEME_KEY: &str = "active_theme";
const ACTIVE_EXTENSIONS_KEY: &str = "active_extensions";

impl Database {
    /// Get the configured editor command (e.g. `code`, `nvim --remote-tab`).
    pub fn get_editor_command(&self) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![EDITOR_COMMAND_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
    }

    /// Set the editor command. Pass an empty string to clear.
    pub fn set_editor_command(&self, command: &str) -> rusqlite::Result<()> {
        if command.is_empty() {
            self.conn.execute(
                "DELETE FROM metadata WHERE key = ?1",
                params![EDITOR_COMMAND_KEY],
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![EDITOR_COMMAND_KEY, command],
            )?;
        }
        Ok(())
    }

    /// Get the active theme preset name (e.g. `default`, `catppuccin-mocha`).
    pub fn get_active_theme(&self) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![THEME_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
    }

    /// Set the active theme preset name. Pass an empty string to reset to default.
    pub fn set_active_theme(&self, name: &str) -> rusqlite::Result<()> {
        if name.is_empty() {
            self.conn
                .execute("DELETE FROM metadata WHERE key = ?1", params![THEME_KEY])?;
        } else {
            self.conn.execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![THEME_KEY, name],
            )?;
        }
        Ok(())
    }

    /// The set of currently-active extension names (e.g. `["flow"]`), stored as
    /// a JSON array under the `active_extensions` metadata key. Drives self-heal:
    /// thurbox re-ensures each active extension's resources on startup and tick.
    /// A malformed/missing value reads as an empty set rather than erroring.
    pub fn get_active_extensions(&self) -> rusqlite::Result<Vec<String>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![ACTIVE_EXTENSIONS_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(raw
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default())
    }

    /// Persist the active-extension set as a JSON array. An empty set deletes the
    /// key (mirrors the editor/theme reset-on-empty convention).
    fn set_active_extensions(&self, names: &[String]) -> rusqlite::Result<()> {
        if names.is_empty() {
            self.conn.execute(
                "DELETE FROM metadata WHERE key = ?1",
                params![ACTIVE_EXTENSIONS_KEY],
            )?;
        } else {
            let json = serde_json::to_string(names).unwrap_or_else(|_| "[]".into());
            self.conn.execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![ACTIVE_EXTENSIONS_KEY, json],
            )?;
        }
        Ok(())
    }

    /// Mark an extension active (idempotent). Returns `true` if it was newly
    /// added, `false` if already active.
    pub fn add_active_extension(&self, name: &str) -> rusqlite::Result<bool> {
        let mut names = self.get_active_extensions()?;
        if names.iter().any(|n| n == name) {
            return Ok(false);
        }
        names.push(name.to_string());
        self.set_active_extensions(&names)?;
        Ok(true)
    }

    /// Mark an extension inactive (idempotent). Returns `true` if it was removed,
    /// `false` if it wasn't active. Self-heal will no longer resurrect it.
    pub fn remove_active_extension(&self, name: &str) -> rusqlite::Result<bool> {
        let mut names = self.get_active_extensions()?;
        let before = names.len();
        names.retain(|n| n != name);
        if names.len() == before {
            return Ok(false);
        }
        self.set_active_extensions(&names)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_command_round_trip() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.get_editor_command().unwrap(), None);

        db.set_editor_command("code --wait").unwrap();
        assert_eq!(
            db.get_editor_command().unwrap().as_deref(),
            Some("code --wait")
        );

        db.set_editor_command("nvim").unwrap();
        assert_eq!(db.get_editor_command().unwrap().as_deref(), Some("nvim"));

        db.set_editor_command("").unwrap();
        assert_eq!(db.get_editor_command().unwrap(), None);
    }

    #[test]
    fn active_theme_round_trip() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.get_active_theme().unwrap(), None);

        db.set_active_theme("catppuccin-mocha").unwrap();
        assert_eq!(
            db.get_active_theme().unwrap().as_deref(),
            Some("catppuccin-mocha")
        );

        db.set_active_theme("tokyo-night").unwrap();
        assert_eq!(
            db.get_active_theme().unwrap().as_deref(),
            Some("tokyo-night")
        );

        db.set_active_theme("").unwrap();
        assert_eq!(db.get_active_theme().unwrap(), None);
    }

    #[test]
    fn active_extensions_round_trip() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.get_active_extensions().unwrap().is_empty());

        // Add is idempotent and preserves order.
        assert!(db.add_active_extension("flow").unwrap());
        assert!(!db.add_active_extension("flow").unwrap());
        assert!(db.add_active_extension("other").unwrap());
        assert_eq!(db.get_active_extensions().unwrap(), ["flow", "other"]);

        // Remove is idempotent; removing the last entry clears the key.
        assert!(db.remove_active_extension("flow").unwrap());
        assert!(!db.remove_active_extension("flow").unwrap());
        assert_eq!(db.get_active_extensions().unwrap(), ["other"]);
        assert!(db.remove_active_extension("other").unwrap());
        assert!(db.get_active_extensions().unwrap().is_empty());
    }

    #[test]
    fn malformed_active_extensions_reads_as_empty() {
        let db = Database::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO metadata (key, value) VALUES ('active_extensions', 'not json')",
                [],
            )
            .unwrap();
        assert!(db.get_active_extensions().unwrap().is_empty());
    }
}
