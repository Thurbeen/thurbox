use super::Database;

impl Database {
    /// Read `PRAGMA data_version`. The value changes whenever *another*
    /// connection commits (never for this connection's own writes), so a caller
    /// can cheaply gate a per-tick cache reload on it — this is what
    /// `SnapshotStore` and the CLI's watch loops poll. The pragma reads an
    /// in-memory counter (no table access), so it is far cheaper than
    /// re-running a query.
    pub fn data_version(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("PRAGMA data_version", [], |row| row.get(0))
    }
}

#[cfg(test)]
mod tests {
    use crate::session::SessionId;
    use crate::sync::SharedSession;

    use super::*;

    fn make_session(name: &str) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.to_string(),
            agent: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    #[test]
    fn data_version_moves_only_on_another_connections_commit() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let path = temp.path();

        let db1 = Database::open(path).unwrap();
        let db2 = Database::open(path).unwrap();

        let before = db1.data_version().unwrap();

        // Our own write does not move our data_version…
        db1.upsert_session(&make_session("mine")).unwrap();
        assert_eq!(db1.data_version().unwrap(), before);

        // …another connection's commit does.
        db2.upsert_session(&make_session("theirs")).unwrap();
        assert_ne!(db1.data_version().unwrap(), before);
    }
}
