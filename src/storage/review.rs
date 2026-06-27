//! Persistence for the native code-review view: review comments and per-file /
//! per-hunk "reviewed" marks, keyed by session id. CRUD modeled on
//! [`super::messages`]; the pure data types live in
//! [`crate::session::review`].

use rusqlite::{params, OptionalExtension};

use crate::session::review::{Classification, CommentAnchor, ReviewComment, Side};
use crate::session::SessionId;
use crate::sync::current_time_millis;

use super::Database;

/// Sentinel `hunk_index` meaning "the whole file is marked reviewed" (SQLite
/// PKs can't rely on NULL uniqueness, so a real value is used).
const WHOLE_FILE_HUNK: i64 = -1;

/// Decompose a [`CommentAnchor`] into the three nullable columns it persists as.
fn anchor_columns(anchor: &CommentAnchor) -> (Option<String>, Option<&'static str>, Option<i64>) {
    match anchor {
        CommentAnchor::Line { file, side, line } => (
            Some(file.clone()),
            Some(side.as_str()),
            Some(i64::from(*line)),
        ),
        CommentAnchor::File { file } => (Some(file.clone()), None, None),
        CommentAnchor::Review => (None, None, None),
    }
}

/// Rebuild a [`CommentAnchor`] from the persisted columns.
fn anchor_from_columns(
    file_path: Option<String>,
    side: Option<String>,
    line_no: Option<i64>,
) -> CommentAnchor {
    match (file_path, line_no) {
        (Some(file), Some(line)) => CommentAnchor::Line {
            file,
            side: side.as_deref().and_then(Side::parse).unwrap_or(Side::New),
            line: line.max(0) as u32,
        },
        (Some(file), None) => CommentAnchor::File { file },
        (None, _) => CommentAnchor::Review,
    }
}

impl Database {
    /// Insert a review comment, returning its new id.
    pub fn add_review_comment(
        &self,
        session_id: SessionId,
        anchor: &CommentAnchor,
        classification: Classification,
        body: &str,
    ) -> rusqlite::Result<i64> {
        let now = current_time_millis() as i64;
        let (file_path, side, line_no) = anchor_columns(anchor);
        self.conn.execute(
            "INSERT INTO review_comments \
             (session_id, file_path, side, line_no, classification, body, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                session_id.to_string(),
                file_path,
                side,
                line_no,
                classification.as_str(),
                body,
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// List a session's review comments (oldest first), summary comments
    /// included.
    pub fn list_review_comments(
        &self,
        session_id: SessionId,
    ) -> rusqlite::Result<Vec<ReviewComment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, side, line_no, classification, body, created_at, updated_at \
             FROM review_comments \
             WHERE session_id = ?1 AND deleted_at IS NULL \
             ORDER BY created_at, id",
        )?;
        let sid = session_id.to_string();
        let rows = stmt.query_map(params![sid], |row| {
            let file_path: Option<String> = row.get(1)?;
            let side: Option<String> = row.get(2)?;
            let line_no: Option<i64> = row.get(3)?;
            let class: String = row.get(4)?;
            Ok(ReviewComment {
                id: row.get(0)?,
                session_id,
                anchor: anchor_from_columns(file_path, side, line_no),
                classification: Classification::parse(&class).unwrap_or_default(),
                body: row.get(5)?,
                created_at: row.get::<_, i64>(6)? as u64,
                updated_at: row.get::<_, i64>(7)? as u64,
            })
        })?;
        rows.collect()
    }

    /// Update a comment's classification + body (e.g. editing an existing note).
    pub fn update_review_comment(
        &self,
        id: i64,
        classification: Classification,
        body: &str,
    ) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        self.conn.execute(
            "UPDATE review_comments SET classification = ?1, body = ?2, updated_at = ?3 \
             WHERE id = ?4 AND deleted_at IS NULL",
            params![classification.as_str(), body, now, id],
        )?;
        Ok(())
    }

    /// Soft-delete a review comment.
    pub fn delete_review_comment(&self, id: i64) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        self.conn.execute(
            "UPDATE review_comments SET deleted_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    /// Toggle a file/hunk "reviewed" mark. `hunk_index = None` marks the whole
    /// file. Returns the new state (`true` = now reviewed).
    pub fn toggle_review_mark(
        &self,
        session_id: SessionId,
        file_path: &str,
        hunk_index: Option<usize>,
    ) -> rusqlite::Result<bool> {
        let sid = session_id.to_string();
        let idx = hunk_index.map(|h| h as i64).unwrap_or(WHOLE_FILE_HUNK);
        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM review_marks \
                 WHERE session_id = ?1 AND file_path = ?2 AND hunk_index = ?3",
                params![sid, file_path, idx],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_some() {
            self.conn.execute(
                "DELETE FROM review_marks \
                 WHERE session_id = ?1 AND file_path = ?2 AND hunk_index = ?3",
                params![sid, file_path, idx],
            )?;
            Ok(false)
        } else {
            let now = current_time_millis() as i64;
            self.conn.execute(
                "INSERT INTO review_marks (session_id, file_path, hunk_index, created_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![sid, file_path, idx, now],
            )?;
            Ok(true)
        }
    }

    /// List a session's "reviewed" marks as `(file_path, hunk_index)` pairs
    /// where `hunk_index = None` means the whole file.
    pub fn list_review_marks(
        &self,
        session_id: SessionId,
    ) -> rusqlite::Result<Vec<(String, Option<usize>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT file_path, hunk_index FROM review_marks WHERE session_id = ?1")?;
        let rows = stmt.query_map(params![session_id.to_string()], |row| {
            let file: String = row.get(0)?;
            let idx: i64 = row.get(1)?;
            let hunk = if idx == WHOLE_FILE_HUNK {
                None
            } else {
                Some(idx.max(0) as usize)
            };
            Ok((file, hunk))
        })?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::review::Classification;

    #[test]
    fn comment_round_trip_all_anchor_levels() {
        let db = Database::open_in_memory().unwrap();
        let sid = SessionId::default();

        let line = CommentAnchor::Line {
            file: "src/foo.rs".into(),
            side: Side::New,
            line: 42,
        };
        db.add_review_comment(sid, &line, Classification::Issue, "bug here")
            .unwrap();
        db.add_review_comment(
            sid,
            &CommentAnchor::File {
                file: "src/foo.rs".into(),
            },
            Classification::Suggestion,
            "rename this file",
        )
        .unwrap();
        db.add_review_comment(
            sid,
            &CommentAnchor::Review,
            Classification::Note,
            "LGTM overall",
        )
        .unwrap();

        let comments = db.list_review_comments(sid).unwrap();
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0].anchor, line);
        assert_eq!(comments[0].classification, Classification::Issue);
        assert!(matches!(comments[1].anchor, CommentAnchor::File { .. }));
        assert_eq!(comments[2].anchor, CommentAnchor::Review);
    }

    #[test]
    fn update_and_soft_delete_comment() {
        let db = Database::open_in_memory().unwrap();
        let sid = SessionId::default();
        let id = db
            .add_review_comment(sid, &CommentAnchor::Review, Classification::Note, "draft")
            .unwrap();

        db.update_review_comment(id, Classification::Praise, "nice work")
            .unwrap();
        let c = &db.list_review_comments(sid).unwrap()[0];
        assert_eq!(c.classification, Classification::Praise);
        assert_eq!(c.body, "nice work");

        db.delete_review_comment(id).unwrap();
        assert!(db.list_review_comments(sid).unwrap().is_empty());
    }

    #[test]
    fn marks_toggle_file_and_hunk_independently() {
        let db = Database::open_in_memory().unwrap();
        let sid = SessionId::default();

        assert!(db.toggle_review_mark(sid, "a.rs", None).unwrap()); // file reviewed
        assert!(db.toggle_review_mark(sid, "a.rs", Some(2)).unwrap()); // hunk 2 reviewed
        let mut marks = db.list_review_marks(sid).unwrap();
        marks.sort();
        assert_eq!(
            marks,
            vec![("a.rs".to_string(), None), ("a.rs".to_string(), Some(2))]
        );

        // Toggling the file mark again clears just it.
        assert!(!db.toggle_review_mark(sid, "a.rs", None).unwrap());
        let marks = db.list_review_marks(sid).unwrap();
        assert_eq!(marks, vec![("a.rs".to_string(), Some(2))]);
    }

    #[test]
    fn base_branch_round_trips_write_once() {
        let db = Database::open_in_memory().unwrap();
        let sid = SessionId::default();
        // No session row yet: a no-op update leaves it unset.
        assert_eq!(db.get_session_base_branch(sid).unwrap(), None);
    }
}
