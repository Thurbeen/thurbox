//! Key/value settings stored in the `metadata` table.

use rusqlite::{params, OptionalExtension};

use super::Database;

const EDITOR_COMMAND_KEY: &str = "editor_command";

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
}
