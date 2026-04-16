//! Key/value settings stored in the `metadata` table.

use rusqlite::{params, OptionalExtension};

use super::Database;

const EDITOR_COMMAND_KEY: &str = "editor_command";
const THEME_KEY: &str = "active_theme";
const NERD_FONT_KEY: &str = "nerd_font_glyphs";

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

    /// Whether nerd-font glyphs are enabled. Defaults to `false` when unset.
    pub fn get_nerd_font_enabled(&self) -> rusqlite::Result<bool> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![NERD_FONT_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value.as_deref() == Some("1"))
    }

    /// Toggle nerd-font glyphs. Stored as `"1"` / `"0"`.
    pub fn set_nerd_font_enabled(&self, enabled: bool) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![NERD_FONT_KEY, if enabled { "1" } else { "0" }],
        )?;
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
    fn nerd_font_round_trip() {
        let db = Database::open_in_memory().unwrap();
        assert!(!db.get_nerd_font_enabled().unwrap());

        db.set_nerd_font_enabled(true).unwrap();
        assert!(db.get_nerd_font_enabled().unwrap());

        db.set_nerd_font_enabled(false).unwrap();
        assert!(!db.get_nerd_font_enabled().unwrap());
    }
}
