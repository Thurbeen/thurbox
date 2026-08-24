//! Key/value settings stored in the `metadata` table.

use rusqlite::{params, OptionalExtension};

use super::Database;

use crate::session::{SessionId, PENDING_FOCUS_SESSION_ID_KEY};

const EDITOR_COMMAND_KEY: &str = "editor_command";
const EDITOR_MODE_KEY: &str = "editor_mode";
const THEME_KEY: &str = "active_theme";
/// Set once a profile has answered the v2 interface gate. Owned here because
/// `storage` owns the `metadata` table; the kernel reads it through the
/// accessors below rather than the other way round.
const V2_ACK_KEY: &str = "v2_interface_acknowledged";
/// Set when the gate's decline branch is what turned `auto_update` off, so a
/// later accept can tell its own doing from a preference the user set. Owned
/// here for the same reason as [`V2_ACK_KEY`]: `storage` owns `metadata`.
const AUTO_UPDATE_OFF_BY_GATE_KEY: &str = "auto_update_disabled_by_consent_gate";
const ACTIVE_EXTENSIONS_KEY: &str = "active_extensions";
const PERF_SNAPSHOT_KEY: &str = "perf_snapshot";

/// Metadata key recording an opt-out of the built-in extension `name`. The
/// format is load-bearing rather than cosmetic: `hooks` must keep producing
/// `builtin_hooks_optout`, the key written before there was more than one
/// built-in, or every existing opt-out silently reverses on upgrade.
fn builtin_optout_key(name: &str) -> String {
    format!("builtin_{name}_optout")
}

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

    /// Get the configured editor launch mode (`auto`/`terminal`/`gui`),
    /// tolerating a missing/blank/corrupt row (defaults to `Auto`).
    pub fn get_editor_mode(&self) -> rusqlite::Result<crate::session::settings::EditorMode> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![EDITOR_MODE_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(crate::session::settings::EditorMode::parse_stored(
            stored.as_deref(),
        ))
    }

    /// Set the editor launch mode. Pass [`crate::session::settings::EditorMode::Auto`]
    /// (or an empty-ish reset) to clear the row back to the default.
    pub fn set_editor_mode(
        &self,
        mode: crate::session::settings::EditorMode,
    ) -> rusqlite::Result<()> {
        if mode == crate::session::settings::EditorMode::Auto {
            self.conn.execute(
                "DELETE FROM metadata WHERE key = ?1",
                params![EDITOR_MODE_KEY],
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![EDITOR_MODE_KEY, mode.as_db_value()],
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

    /// Whether this profile has answered the v2 interface gate.
    ///
    /// Asked by the v2 interface gate. Recorded in `metadata` rather than
    /// `settings.toml` because it is a fact about this machine's history, not a
    /// preference the user would edit or copy between machines.
    pub fn v2_acknowledged(&self) -> rusqlite::Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![V2_ACK_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .is_some())
    }

    /// Record that the gate has been answered, so it is asked once.
    pub fn acknowledge_v2(&self) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![V2_ACK_KEY, "1"],
        )?;
        Ok(())
    }

    /// Record that the consent gate is what turned `auto_update` off.
    ///
    /// The gate disables auto-update when someone declines v2, so a downgrade to
    /// the 1.x line is not undone on the next launch. Without this marker the
    /// accept branch cannot undo that: a `false` in `settings.toml` looks the
    /// same whether the gate wrote it or the user did, and re-enabling
    /// unconditionally would silently overturn a deliberate preference.
    pub fn note_auto_update_disabled_by_gate(&self) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![AUTO_UPDATE_OFF_BY_GATE_KEY, "1"],
        )?;
        Ok(())
    }

    /// Read **and clear** that marker: did the gate turn `auto_update` off?
    ///
    /// One shot, in one statement, mirroring
    /// [`Self::take_pending_focus_session_id`] — the flag exists only to be
    /// acted on once, and leaving it behind would re-enable auto-update again
    /// on some later launch the user had since opted out of.
    pub fn take_auto_update_disabled_by_gate(&self) -> rusqlite::Result<bool> {
        Ok(self
            .conn
            .query_row(
                "DELETE FROM metadata WHERE key = ?1 RETURNING value",
                params![AUTO_UPDATE_OFF_BY_GATE_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .is_some())
    }

    /// Whether this profile has ever had a session.
    ///
    /// The test for "came from v1" rather than "fresh install": soft-deleted rows
    /// count, because having deleted a session is still history. A profile with
    /// none has nothing to be warned about losing.
    pub fn has_session_history(&self) -> rusqlite::Result<bool> {
        self.conn
            .query_row("SELECT EXISTS(SELECT 1 FROM sessions)", [], |row| {
                row.get::<_, bool>(0)
            })
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

    /// Publish the TUI's latest perf snapshot (a JSON blob) for
    /// `thurbox-cli perf` to read. Written only while perf timing is active
    /// (THURBOX_PERF_LOG or an open perf HUD) — each write bumps other
    /// connections' `data_version`, so an idle default-config TUI must never
    /// churn this row.
    pub fn set_perf_snapshot(&self, json: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![PERF_SNAPSHOT_KEY, json],
        )?;
        Ok(())
    }

    /// The last published perf snapshot, if any (see [`Self::set_perf_snapshot`]).
    pub fn get_perf_snapshot(&self) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![PERF_SNAPSHOT_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
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

    /// Whether the user has opted out of the auto-activated built-in extension
    /// `name`. Set when they `extension deactivate <name>`, so startup self-heal
    /// won't resurrect it.
    pub fn builtin_extension_opted_out(&self, name: &str) -> rusqlite::Result<bool> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![builtin_optout_key(name)],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(raw.as_deref() == Some("1"))
    }

    /// Record (or clear) the opt-out of the built-in extension `name`.
    pub fn set_builtin_extension_optout(&self, name: &str, optout: bool) -> rusqlite::Result<()> {
        let key = builtin_optout_key(name);
        if optout {
            self.conn.execute(
                "INSERT INTO metadata (key, value) VALUES (?1, '1') \
                 ON CONFLICT(key) DO UPDATE SET value = '1'",
                params![key],
            )?;
        } else {
            self.conn
                .execute("DELETE FROM metadata WHERE key = ?1", params![key])?;
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

    /// Atomically read + clear the pending "focus this session" request that
    /// the notifications click handler writes. Returns the raw UUID string the
    /// click handler stored, or `None` when no click is pending. Done under
    /// SQLite's writer serialization so a concurrent click can't be lost:
    /// the `RETURNING value` clause yields the value being deleted in the
    /// same statement.
    pub fn take_pending_focus_session_id(&self) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "DELETE FROM metadata WHERE key = ?1 RETURNING value",
                params![PENDING_FOCUS_SESSION_ID_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
    }

    /// Record a "focus this session" request for the running TUI to consume.
    /// Used by the macOS click-to-focus CLI (`thurbox-cli session focus`) —
    /// the symmetric writer to [`Self::take_pending_focus_session_id`].
    /// Linux dispatches in-process from the dbus action callback and writes
    /// the metadata row directly; the CLI path needs this helper because it
    /// runs in a separate process spawned by `terminal-notifier -execute`.
    pub fn set_pending_focus_session_id(&self, session_id: SessionId) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![PENDING_FOCUS_SESSION_ID_KEY, session_id.to_string()],
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
    fn the_v2_gate_answer_round_trips_and_history_is_detected() {
        let db = Database::open_in_memory().expect("in-memory");
        assert!(
            !db.v2_acknowledged().expect("read"),
            "a fresh profile has not answered"
        );
        assert!(
            !db.has_session_history().expect("read"),
            "a fresh profile has no history, so it must not be prompted"
        );

        db.acknowledge_v2().expect("write");
        assert!(
            db.v2_acknowledged().expect("read"),
            "the answer has to persist, or the gate asks on every launch"
        );
    }

    #[test]
    fn the_gates_auto_update_marker_is_one_shot() {
        let db = Database::open_in_memory().expect("in-memory");
        assert!(
            !db.take_auto_update_disabled_by_gate().expect("read"),
            "a profile the gate never touched must not have its auto_update \
             re-enabled: that `false` would be the user's own setting"
        );

        db.note_auto_update_disabled_by_gate().expect("write");
        assert!(
            db.take_auto_update_disabled_by_gate().expect("read"),
            "the decline branch's doing has to be readable by a later accept"
        );
        assert!(
            !db.take_auto_update_disabled_by_gate().expect("read"),
            "taking it clears it -- left behind, it would re-enable auto-update \
             on some later launch the user had since opted out of"
        );
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
    fn pending_focus_session_id_round_trip() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.take_pending_focus_session_id().unwrap(), None);

        // The notifications click handler writes via raw SQL using the same
        // `PENDING_FOCUS_SESSION_ID_KEY`; this mirrors that.
        db.conn
            .execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
                params![
                    PENDING_FOCUS_SESSION_ID_KEY,
                    "deadbeef-0000-0000-0000-000000000000"
                ],
            )
            .unwrap();

        // First take returns + clears it; second returns None.
        assert_eq!(
            db.take_pending_focus_session_id().unwrap().as_deref(),
            Some("deadbeef-0000-0000-0000-000000000000")
        );
        assert_eq!(db.take_pending_focus_session_id().unwrap(), None);
    }

    #[test]
    fn set_pending_focus_session_id_is_idempotent_and_overwrites() {
        let db = Database::open_in_memory().unwrap();
        let first = SessionId::default();
        let second = SessionId::default();
        // Two distinct ids (UUIDs are unique).
        assert_ne!(first, second);

        db.set_pending_focus_session_id(first).unwrap();
        // A second set overwrites — the latest click wins, no stacking.
        db.set_pending_focus_session_id(second).unwrap();

        assert_eq!(
            db.take_pending_focus_session_id().unwrap().as_deref(),
            Some(second.to_string().as_str())
        );
        assert_eq!(db.take_pending_focus_session_id().unwrap(), None);
    }

    #[test]
    fn perf_snapshot_round_trips_and_overwrites() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.get_perf_snapshot().unwrap(), None);
        db.set_perf_snapshot(r#"{"frames":1}"#).unwrap();
        db.set_perf_snapshot(r#"{"frames":2}"#).unwrap();
        assert_eq!(
            db.get_perf_snapshot().unwrap().as_deref(),
            Some(r#"{"frames":2}"#),
            "the latest snapshot wins"
        );
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
