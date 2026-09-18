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

/// Metadata key holding the teardown sweep's backoff for one host, as
/// `<attempted_at_millis>:<consecutive failures>`.
///
/// Durable rather than process-local because the sweep has two drivers and one
/// of them is not a long-lived process: the interface's `Command::Reap` runs
/// for as long as thurbox does, but `thurbox-cli automation tick` is started
/// afresh by the heartbeat every minute, and a backoff held in memory is
/// forgotten by every one of those ticks. Only a host that failed has a row,
/// and its first answer deletes it.
fn host_probe_backoff_key(backend_type: &str) -> String {
    format!("host_probe_backoff:{backend_type}")
}

/// Metadata key holding the teardown sweep's backoff for one soft-deleted
/// session, in the same `<attempted_at_millis>:<consecutive failures>` shape
/// and durable for the same reason as [`host_probe_backoff_key`].
///
/// Keyed per row rather than per host because that is the granularity the
/// retry has: a host answers `list-windows` while its own `thurbox-cli` does
/// not run, so the row is overdue and still owns its windows on every pass
/// while the host itself looks perfectly reachable (issue #1193).
fn session_reap_backoff_key(id: SessionId) -> String {
    format!("session_reap_backoff:{id}")
}

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

    /// Claim the right to probe `backend_type` at `now_ms`, or refuse because
    /// its last attempt is too recent to repeat.
    ///
    /// One `BEGIN IMMEDIATE` around the read and the write, which is what makes
    /// this a claim rather than a check somebody else can overtake: the sweep
    /// runs on a fresh thread every few seconds in the interface and in a fresh
    /// process every minute headlessly, so several of them are inside one slow
    /// listing at a time. The stamp is written **before** the probe is made, so
    /// the rest collide with it instead of each starting a process of their own.
    ///
    /// `retry_after_ms` is asked how long a host with that many consecutive
    /// failures is left alone. The curve is the caller's — `storage` owns the
    /// atomicity and the row, not the policy.
    pub fn claim_host_probe(
        &self,
        backend_type: &str,
        now_ms: u64,
        retry_after_ms: impl Fn(u32) -> u64,
    ) -> rusqlite::Result<bool> {
        self.claim_backoff(
            &host_probe_backoff_key(backend_type),
            now_ms,
            retry_after_ms,
        )
    }

    /// Record that probing `backend_type` failed at `now_ms`, which spaces the
    /// next attempt further out than the last.
    pub fn note_host_probe_failed(&self, backend_type: &str, now_ms: u64) -> rusqlite::Result<()> {
        self.note_backoff_failed(&host_probe_backoff_key(backend_type), now_ms)
    }

    /// Forget `backend_type`'s backoff, so a host that has answered is asked at
    /// the base cadence again rather than at the interval its outage earned.
    pub fn clear_host_probe_backoff(&self, backend_type: &str) -> rusqlite::Result<()> {
        self.clear_backoff(&host_probe_backoff_key(backend_type))
    }

    /// Claim the right to attempt the reap of soft-deleted session `id` at
    /// `now_ms`, or refuse because its last attempt is too recent to repeat.
    /// [`Self::claim_host_probe`]'s claim, on a row rather than on a host.
    pub fn claim_session_reap(
        &self,
        id: SessionId,
        now_ms: u64,
        retry_after_ms: impl Fn(u32) -> u64,
    ) -> rusqlite::Result<bool> {
        self.claim_backoff(&session_reap_backoff_key(id), now_ms, retry_after_ms)
    }

    /// Record that `id`'s reap failed at `now_ms`, spacing the next attempt
    /// further out than the last.
    pub fn note_session_reap_failed(&self, id: SessionId, now_ms: u64) -> rusqlite::Result<()> {
        self.note_backoff_failed(&session_reap_backoff_key(id), now_ms)
    }

    /// Forget `id`'s reap backoff, once its reap has come good — so no stamp
    /// outlives the row's teardown, and a session deleted again after a
    /// restore starts at the base cadence.
    pub fn clear_session_reap_backoff(&self, id: SessionId) -> rusqlite::Result<()> {
        self.clear_backoff(&session_reap_backoff_key(id))
    }

    /// The claim above, over whichever `metadata` key the caller names.
    fn claim_backoff(
        &self,
        key: &str,
        now_ms: u64,
        retry_after_ms: impl Fn(u32) -> u64,
    ) -> rusqlite::Result<bool> {
        let tx = self.write_transaction()?;
        let recorded: Option<String> = tx
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        // The count is carried through a claim, not reset: a host that keeps
        // failing keeps backing off rather than starting over at the first
        // interval every time it is let through.
        let failures = match recorded.as_deref().and_then(parse_backoff) {
            Some((at, failures)) => {
                // A stamp in the future is a wall clock that moved backwards
                // under us — an NTP correction, or one made by hand. Elapsed
                // time would read as zero until the clock caught up, which is
                // an unbounded backoff rather than the one that was asked for,
                // so such a stamp is stale and the host is asked.
                if at <= now_ms && now_ms - at < retry_after_ms(failures) {
                    return Ok(false);
                }
                failures
            }
            None => 0,
        };
        tx.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, format!("{now_ms}:{failures}")],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// The failure count above, bumped on whichever `metadata` key the caller
    /// names.
    fn note_backoff_failed(&self, key: &str, now_ms: u64) -> rusqlite::Result<()> {
        let tx = self.write_transaction()?;
        let recorded: Option<String> = tx
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let failures = recorded
            .as_deref()
            .and_then(parse_backoff)
            .map_or(0, |(_, failures)| failures)
            .saturating_add(1);
        tx.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, format!("{now_ms}:{failures}")],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Drop whichever backoff `metadata` key the caller names.
    fn clear_backoff(&self, key: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM metadata WHERE key = ?1", params![key])?;
        Ok(())
    }
}

/// The `<attempted_at_millis>:<failures>` a backoff row holds, or `None` for a
/// value this build did not write — which is read as "never asked" rather than
/// as an error, so a row hand-edited or left by a future format costs one extra
/// probe instead of failing a teardown.
fn parse_backoff(value: &str) -> Option<(u64, u32)> {
    let (at, failures) = value.split_once(':')?;
    Some((at.parse().ok()?, failures.parse().ok()?))
}

#[cfg(test)]
mod tests {

    /// The teardown sweep's host backoff has to outlive the process that
    /// recorded it: the headless heartbeat runs `automation tick` as a fresh
    /// process every minute, so a backoff kept in memory is forgotten on every
    /// tick and an unreachable host is probed again regardless of what the
    /// last one learned. It lives in `metadata`, the store both drivers of the
    /// sweep already share.
    #[test]
    fn a_host_probe_backoff_outlives_the_process_that_recorded_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("thurbox.db");
        // One minute flat, which is what the sweep's own curve starts at.
        let retry = |_failures: u32| 60_000_u64;
        let backend = "ssh:unreachable";
        let now = 1_700_000_000_000_u64;

        let first = Database::open(&path).unwrap();
        assert!(
            first.claim_host_probe(backend, now, retry).unwrap(),
            "a host nobody has asked about is asked"
        );
        first.note_host_probe_failed(backend, now).unwrap();
        drop(first);

        // A second connection stands in for the next `automation tick`.
        let second = Database::open(&path).unwrap();
        assert!(
            !second
                .claim_host_probe(backend, now + 5_000, retry)
                .unwrap(),
            "the next tick must be held to the backoff the last one recorded"
        );
        assert!(
            second
                .claim_host_probe(backend, now + 61_000, retry)
                .unwrap(),
            "and let through once it has run out"
        );
        // An answer clears it, so a host that comes back is asked at once.
        second.clear_host_probe_backoff(backend).unwrap();
        assert!(second
            .claim_host_probe(backend, now + 61_000, retry)
            .unwrap());
    }

    /// A wall clock that moves backwards must not strand a host: elapsed time
    /// then reads as zero, and a backoff computed from it would last until the
    /// clock caught up rather than the interval it was given.
    #[test]
    fn a_backoff_stamped_in_the_future_is_not_honoured() {
        let db = Database::open_in_memory().unwrap();
        let retry = |_failures: u32| 60_000_u64;
        let backend = "ssh:devbox";
        let stamped = 1_700_000_000_000_u64;

        assert!(db.claim_host_probe(backend, stamped, retry).unwrap());
        db.note_host_probe_failed(backend, stamped).unwrap();
        // The clock jumps an hour back, so every stamp is now in the future.
        let after_rollback = stamped - 3_600_000;
        assert!(
            db.claim_host_probe(backend, after_rollback, retry).unwrap(),
            "a host must not be held until the clock catches up"
        );
    }

    /// Consecutive failures are what the caller's curve is asked about, so the
    /// count has to survive the same way the stamp does.
    #[test]
    fn consecutive_host_probe_failures_are_counted_across_connections() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("thurbox.db");
        let backend = "wsl:ubuntu";
        let seen = std::sync::Mutex::new(Vec::new());
        let retry = |failures: u32| {
            seen.lock().unwrap().push(failures);
            60_000_u64
        };

        let now = 1_700_000_000_000_u64;
        let db = Database::open(&path).unwrap();
        db.claim_host_probe(backend, now, retry).unwrap();
        db.note_host_probe_failed(backend, now).unwrap();
        db.note_host_probe_failed(backend, now).unwrap();
        drop(db);

        let next = Database::open(&path).unwrap();
        next.claim_host_probe(backend, now + 1_000, retry).unwrap();
        assert_eq!(
            seen.lock().unwrap().last().copied(),
            Some(2),
            "the curve is asked about both failures, not a count reset by the \
             new connection"
        );
    }
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
