//! Several processes share one database file — the TUI, every agent hook's
//! `thurbox-cli`, the host CLIs — so the schema pass that runs on open has to
//! hold up with more than one of them at it (issue #1475).
//!
//! Driven through the real `thurbox-cli` binary: the property is about
//! processes, and a process is what races in the field.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::Connection;

struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        Self { root }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn database(&self) -> PathBuf {
        self.path("data").join("thurbox.db")
    }

    /// `thurbox-cli automation list --json` — any command that opens the
    /// database through the ordinary path will do.
    fn cli(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(["automation", "list", "--json"]);
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
        cmd.env("XDG_CONFIG_HOME", self.path("config"));
        cmd.env("XDG_DATA_HOME", self.path("data"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("THURBOX_") {
                cmd.env_remove(&key);
            }
        }
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        cmd
    }
}

fn stored_version(path: &Path) -> u32 {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
        .parse()
        .unwrap()
}

fn output(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A v23 database: the last version before `scheduled_commands` became
/// automations. v24 is a **data** step — it moves each pending row across —
/// so a second process running it again duplicates the row.
fn seed_v23(path: &Path) {
    Connection::open(path)
        .unwrap()
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '23');
             CREATE TABLE scheduled_commands (
                id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                command_text TEXT NOT NULL, scheduled_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL, executed_at INTEGER, cancelled_at INTEGER);
             INSERT INTO scheduled_commands
                (session_id, command_text, scheduled_at, created_at, executed_at, cancelled_at)
                VALUES ('s1', 'pending cmd', 5000, 100, NULL, NULL);",
        )
        .unwrap();
}

#[test]
fn processes_opening_an_old_database_at_once_migrate_it_once() {
    // A race needs more than one try to show up; each round is a fresh file.
    for round in 0..10 {
        let env = Env::new();
        seed_v23(&env.database());

        let children: Vec<_> = (0..8)
            .map(|_| env.cli().spawn().expect("spawn thurbox-cli"))
            .collect();
        for child in children {
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "round {round}: a concurrent open failed:\n{}",
                output(&out)
            );
        }

        let moved: i64 = Connection::open(env.database())
            .unwrap()
            .query_row(
                "SELECT count(*) FROM automations WHERE prompt = 'pending cmd'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(moved, 1, "round {round}: the v24 data step ran twice");
        assert_eq!(
            stored_version(&env.database()),
            thurbox::storage::SCHEMA_VERSION
        );
    }
}

#[test]
fn processes_opening_a_fresh_database_at_once_all_succeed() {
    for round in 0..10 {
        let env = Env::new();
        let children: Vec<_> = (0..8)
            .map(|_| env.cli().spawn().expect("spawn thurbox-cli"))
            .collect();
        for child in children {
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "round {round}: a concurrent open failed:\n{}",
                output(&out)
            );
        }
        assert_eq!(
            stored_version(&env.database()),
            thurbox::storage::SCHEMA_VERSION
        );
        assert_eq!(
            metadata(&env.database(), "min_reader_version"),
            Some(thurbox::storage::MIN_READER_VERSION.to_string()),
            "a new database says which binaries may open it"
        );
    }
}

fn set_metadata(path: &Path, key: &str, value: u32) {
    Connection::open(path)
        .unwrap()
        .execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, value.to_string()),
        )
        .unwrap();
}

fn metadata(path: &Path, key: &str) -> Option<String> {
    use rusqlite::OptionalExtension;
    Connection::open(path)
        .unwrap()
        .query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .unwrap()
}

/// A downgrade keeps working: a newer thurbox's additive migration — here a
/// column and a table this binary has never heard of — leaves a database an
/// older one still opens, without rewinding the version or touching what the
/// newer one added.
#[test]
fn an_older_binary_opens_a_database_a_newer_one_migrated() {
    let env = Env::new();
    let first = env.cli().output().unwrap();
    assert!(first.status.success(), "{}", output(&first));
    let newer = thurbox::storage::SCHEMA_VERSION + 1;
    Connection::open(env.database())
        .unwrap()
        .execute_batch(
            "ALTER TABLE sessions ADD COLUMN from_the_future TEXT;
             CREATE TABLE from_the_future (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
    set_metadata(&env.database(), "schema_version", newer);

    let out = env.cli().output().unwrap();

    assert!(
        out.status.success(),
        "an older thurbox refused a v{newer} database it can read:\n{}",
        output(&out)
    );
    assert_eq!(
        stored_version(&env.database()),
        newer,
        "the stored version was rewound"
    );
    let kept: bool = Connection::open(env.database())
        .unwrap()
        .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'from_the_future'")
        .unwrap()
        .exists([])
        .unwrap();
    assert!(kept, "the newer binary's column was dropped");
    // The older binary runs none of its own steps: its v49 pass would put
    // snapshot triggers on every table it finds, the newer one's included.
    let triggers: i64 = Connection::open(env.database())
        .unwrap()
        .query_row(
            "SELECT count(*) FROM sqlite_master \
             WHERE type = 'trigger' AND tbl_name = 'from_the_future'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        triggers, 0,
        "the older binary re-shaped the newer one's table"
    );
}

/// What a downgrade cannot survive is declared by the database, not guessed
/// from its version: a newer thurbox that made an incompatible change raises
/// `min_reader_version` past this binary, and this binary then refuses rather
/// than writing rows the schema no longer describes.
#[test]
fn an_older_binary_refuses_a_database_it_cannot_read() {
    let env = Env::new();
    let first = env.cli().output().unwrap();
    assert!(first.status.success(), "{}", output(&first));
    let newer = thurbox::storage::SCHEMA_VERSION + 1;
    set_metadata(&env.database(), "schema_version", newer);
    set_metadata(&env.database(), "min_reader_version", newer);

    let out = env.cli().output().unwrap();

    assert!(
        !out.status.success(),
        "opened a database that says v{newer} is the oldest safe reader"
    );
    assert!(
        output(&out).contains("update thurbox"),
        "the refusal says what to do:\n{}",
        output(&out)
    );
    assert_eq!(stored_version(&env.database()), newer);
    assert_eq!(
        metadata(&env.database(), "min_reader_version"),
        Some(newer.to_string()),
        "the refusal rewrote the database"
    );
}
