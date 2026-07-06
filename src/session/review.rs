//! Native code-review data model + a unified-diff parser.
//!
//! Pure data (no local crate imports beyond `super`), matching the architecture
//! rule for `session`. The `ui` layer renders these types directly (it may
//! import `session` but never `git`); the `git diff` invocation lives in
//! [`crate::git`], and persistence in [`crate::storage::review`].

use super::SessionId;

/// Maximum byte length of a review comment / summary body.
pub const MAX_REVIEW_BODY_LEN: usize = 16 * 1024;

/// Which side of the diff a line-anchored comment sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Old,
    New,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Old => "old",
            Side::New => "new",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "old" => Some(Side::Old),
            "new" => Some(Side::New),
            _ => None,
        }
    }
}

/// A review comment's classification — the colored "type" badge. Mirrors
/// tuicr's set (issue / suggestion / note / praise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Classification {
    Issue,
    Suggestion,
    #[default]
    Note,
    Praise,
}

impl Classification {
    /// In selector / cycle order.
    pub const ALL: [Classification; 4] = [
        Classification::Issue,
        Classification::Suggestion,
        Classification::Note,
        Classification::Praise,
    ];

    /// Stable token used in storage and markdown export.
    pub fn as_str(self) -> &'static str {
        match self {
            Classification::Issue => "issue",
            Classification::Suggestion => "suggestion",
            Classification::Note => "note",
            Classification::Praise => "praise",
        }
    }

    /// Human-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Classification::Issue => "Issue",
            Classification::Suggestion => "Suggestion",
            Classification::Note => "Note",
            Classification::Praise => "Praise",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Next classification in [`Self::ALL`] order, wrapping — drives the
    /// in-modal cycle.
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|&c| c == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// Previous classification, wrapping.
    pub fn prev(self) -> Self {
        let i = Self::ALL.iter().position(|&c| c == self).unwrap_or(0);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Where a review comment is anchored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentAnchor {
    /// A specific line on one side of a file's diff.
    Line { file: String, side: Side, line: u32 },
    /// The file as a whole.
    File { file: String },
    /// The review as a whole — the summary.
    Review,
}

impl CommentAnchor {
    /// The file this comment concerns, if any (`None` for the review summary).
    pub fn file(&self) -> Option<&str> {
        match self {
            CommentAnchor::Line { file, .. } | CommentAnchor::File { file } => Some(file),
            CommentAnchor::Review => None,
        }
    }

    /// Whether this is a file-level comment on `file`.
    pub fn anchors_file(&self, file: &str) -> bool {
        matches!(self, CommentAnchor::File { file: f } if f == file)
    }

    /// Whether this line-level comment anchors to a diff line in `file` with the
    /// given old/new line numbers (matched against the comment's side).
    pub fn anchors_line(&self, file: &str, old_no: Option<u32>, new_no: Option<u32>) -> bool {
        match self {
            CommentAnchor::Line {
                file: f,
                side,
                line,
            } if f == file => match side {
                Side::New => new_no == Some(*line),
                Side::Old => old_no == Some(*line),
            },
            _ => false,
        }
    }
}

/// A persisted review comment. The summary is just a [`CommentAnchor::Review`]
/// comment, so one type/table covers line, file, and review-level remarks.
#[derive(Debug, Clone)]
pub struct ReviewComment {
    pub id: i64,
    pub session_id: SessionId,
    pub anchor: CommentAnchor,
    pub classification: Classification,
    pub body: String,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Validate a comment/summary body against the length bound. Pure so the UI and
/// storage layers share one rule; returns a human-readable reason on rejection.
pub fn validate_body(body: &str) -> Result<(), String> {
    if body.trim().is_empty() {
        return Err("comment body must not be empty".into());
    }
    if body.len() > MAX_REVIEW_BODY_LEN {
        return Err(format!(
            "comment body too long ({} bytes; max {})",
            body.len(),
            MAX_REVIEW_BODY_LEN
        ));
    }
    Ok(())
}

// ── Diff model ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Add,
    Del,
}

/// A single line within a diff hunk, with its 1-based line numbers on each side
/// (the side that doesn't contain the line is `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub new_start: u32,
    /// The section heading after the `@@ … @@` ranges (often a function name).
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
}

impl FileStatus {
    pub fn glyph(self) -> &'static str {
        match self {
            FileStatus::Modified => "M",
            FileStatus::Added => "A",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    /// New path (or the old path for a deletion).
    pub path: String,
    /// Old path, when it differs from `path` (a rename).
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub hunks: Vec<DiffHunk>,
}

impl DiffFile {
    pub fn added_count(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffLineKind::Add)
            .count()
    }

    pub fn deleted_count(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffLineKind::Del)
            .count()
    }
}

/// Parse `git diff` unified output into a list of [`DiffFile`]s. Tolerant of the
/// metadata lines git emits (index / mode / rename / binary); unknown lines are
/// skipped. Pure and unit-tested.
pub fn parse_unified_diff(input: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut cur: Option<DiffFile> = None;
    let mut hunk: Option<DiffHunk> = None;
    let mut old_no = 0u32;
    let mut new_no = 0u32;

    for line in input.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // New file boundary: flush the pending hunk + file.
            push_hunk(&mut cur, &mut hunk);
            if let Some(f) = cur.take() {
                files.push(f);
            }
            let (old, new) = split_diff_git_paths(rest);
            let path = new.clone().or_else(|| old.clone()).unwrap_or_default();
            cur = Some(DiffFile {
                path,
                old_path: None,
                status: FileStatus::Modified,
                hunks: Vec::new(),
            });
            continue;
        }

        let Some(f) = cur.as_mut() else {
            continue;
        };

        if line.starts_with("@@") {
            push_hunk(&mut cur, &mut hunk);
            let (os, ns, header) = parse_hunk_header(line);
            old_no = os;
            new_no = ns;
            hunk = Some(DiffHunk {
                old_start: os,
                new_start: ns,
                header,
                lines: Vec::new(),
            });
        } else if apply_file_metadata(f, line, hunk.is_some()) {
            // A header/metadata line (mode / rename / `---` / `+++`) was consumed.
        } else if let Some(h) = hunk.as_mut() {
            push_body_line(h, line, &mut old_no, &mut new_no);
        }
    }

    push_hunk(&mut cur, &mut hunk);
    if let Some(f) = cur.take() {
        files.push(f);
    }
    files
}

/// Push the pending hunk into the current file, if both are present.
fn push_hunk(cur: &mut Option<DiffFile>, hunk: &mut Option<DiffHunk>) {
    if let (Some(f), Some(h)) = (cur.as_mut(), hunk.take()) {
        f.hunks.push(h);
    }
}

/// Apply a file-header metadata line (mode / rename / `---` / `+++`) to `f`,
/// returning whether it was consumed. `in_hunk` gates the `---`/`+++` arms to
/// the pre-hunk region: inside a hunk a removed/added line whose *content*
/// starts with `-- `/`++ ` (SQL/Lua/Haskell comment, signature delimiter)
/// renders as `--- …`/`+++ …` and must stay a Del/Add body line.
fn apply_file_metadata(f: &mut DiffFile, line: &str, in_hunk: bool) -> bool {
    if line.starts_with("new file mode") {
        f.status = FileStatus::Added;
    } else if line.starts_with("deleted file mode") {
        f.status = FileStatus::Deleted;
    } else if let Some(p) = line.strip_prefix("rename from ") {
        f.status = FileStatus::Renamed;
        f.old_path = Some(p.to_string());
    } else if let Some(p) = line.strip_prefix("rename to ") {
        f.status = FileStatus::Renamed;
        f.path = p.to_string();
    } else if let Some(p) = line.strip_prefix("--- ").filter(|_| !in_hunk) {
        let p = p.trim();
        if p != "/dev/null" {
            let old = p.strip_prefix("a/").unwrap_or(p).to_string();
            // Only record as old_path when it differs from the new path
            // (renames already set it; identical paths leave it None).
            if f.old_path.is_none() && f.path != old {
                f.old_path = Some(old);
            }
        }
    } else if let Some(p) = line.strip_prefix("+++ ").filter(|_| !in_hunk) {
        let p = p.trim();
        if p != "/dev/null" {
            f.path = p.strip_prefix("b/").unwrap_or(p).to_string();
        }
    } else {
        return false;
    }
    true
}

/// Append a hunk body line (`+`/`-`/` `-prefixed) to `hunk`, advancing the
/// per-side line counters. `\ No newline …` and stray metadata are ignored.
fn push_body_line(hunk: &mut DiffHunk, line: &str, old_no: &mut u32, new_no: &mut u32) {
    let (kind, old, new) = match line.as_bytes().first() {
        Some(b'+') => (DiffLineKind::Add, None, Some(*new_no)),
        Some(b'-') => (DiffLineKind::Del, Some(*old_no), None),
        Some(b' ') => (DiffLineKind::Context, Some(*old_no), Some(*new_no)),
        _ => return,
    };
    hunk.lines.push(DiffLine {
        kind,
        old_no: old,
        new_no: new,
        text: line[1..].to_string(),
    });
    if old.is_some() {
        *old_no += 1;
    }
    if new.is_some() {
        *new_no += 1;
    }
}

/// Split `a/<old> b/<new>` (the tail of a `diff --git` line). Best-effort: git
/// quotes paths with special characters, but the `---`/`+++` lines refine the
/// paths afterward, so this only needs to handle the common unquoted case.
fn split_diff_git_paths(rest: &str) -> (Option<String>, Option<String>) {
    if let Some(idx) = rest.find(" b/") {
        let old = rest[..idx].strip_prefix("a/").map(str::to_string);
        let new = rest[idx + 1..].strip_prefix("b/").map(str::to_string);
        (old, new)
    } else {
        (None, None)
    }
}

/// Parse a `@@ -o,os +n,ns @@ header` line into the two start lines + heading.
fn parse_hunk_header(line: &str) -> (u32, u32, String) {
    let mut old_start = 0;
    let mut new_start = 0;
    let mut header = String::new();
    if let Some(after) = line.strip_prefix("@@ ") {
        if let Some(end) = after.find(" @@") {
            let ranges = &after[..end];
            header = after[end + 3..].trim().to_string();
            for tok in ranges.split_whitespace() {
                if let Some(o) = tok.strip_prefix('-') {
                    old_start = parse_start(o);
                } else if let Some(n) = tok.strip_prefix('+') {
                    new_start = parse_start(n);
                }
            }
        }
    }
    (old_start, new_start, header)
}

/// Parse the start line of a `start,count` (or bare `start`) range token.
fn parse_start(s: &str) -> u32 {
    s.split(',')
        .next()
        .and_then(|x| x.parse().ok())
        .unwrap_or(0)
}

// ── Side-by-side pairing ─────────────────────────────────────────────────────

/// One visual row of the paired (true side-by-side) layout: the old-side and
/// new-side line indices within a hunk (`None` = a blank half-cell). A context
/// line pairs with itself (`old == new`); within a change block a deletion and
/// an addition align positionally (`del[k] ↔ add[k]`), and any uneven remainder
/// is left half-blank. Every hunk line appears in exactly one [`SidePair`] on
/// exactly one side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidePair {
    pub old: Option<usize>,
    pub new: Option<usize>,
}

/// Collapse a hunk into paired side-by-side rows (see [`SidePair`]). Positional
/// alignment — cheap, deterministic, and dependency-free (matching the
/// heuristic, language-agnostic stance already taken for syntax highlighting).
/// Pure so the row builder ([`crate::app`]) and the renderer ([`crate::ui`])
/// derive the exact same pairing and never disagree on which lines share a row.
pub fn pair_hunk(hunk: &DiffHunk) -> Vec<SidePair> {
    let lines = &hunk.lines;
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            DiffLineKind::Context => {
                pairs.push(SidePair {
                    old: Some(i),
                    new: Some(i),
                });
                i += 1;
            }
            // A change block: the run of deletions, then the run of additions
            // immediately after it (git emits `-` lines before `+` within a
            // contiguous change). Align them positionally.
            _ => {
                let del_start = i;
                while i < lines.len() && lines[i].kind == DiffLineKind::Del {
                    i += 1;
                }
                let del_len = i - del_start;
                let add_start = i;
                while i < lines.len() && lines[i].kind == DiffLineKind::Add {
                    i += 1;
                }
                let add_len = i - add_start;
                for k in 0..del_len.max(add_len) {
                    pairs.push(SidePair {
                        old: (k < del_len).then_some(del_start + k),
                        new: (k < add_len).then_some(add_start + k),
                    });
                }
            }
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_round_trips_and_cycles() {
        for c in Classification::ALL {
            assert_eq!(Classification::parse(c.as_str()), Some(c));
        }
        assert_eq!(Classification::parse("bogus"), None);
        assert_eq!(Classification::Issue.next(), Classification::Suggestion);
        assert_eq!(Classification::Issue.prev(), Classification::Praise);
        assert_eq!(Classification::Praise.next(), Classification::Issue);
    }

    #[test]
    fn anchor_matchers_distinguish_file_side_and_line() {
        let file_anchor = CommentAnchor::File {
            file: "a.rs".into(),
        };
        assert!(file_anchor.anchors_file("a.rs"));
        assert!(!file_anchor.anchors_file("b.rs"));
        // A file-level anchor never matches a specific line.
        assert!(!file_anchor.anchors_line("a.rs", Some(1), Some(1)));

        let new_line = CommentAnchor::Line {
            file: "a.rs".into(),
            side: Side::New,
            line: 5,
        };
        // Matches only the new-side number on the right file.
        assert!(new_line.anchors_line("a.rs", None, Some(5)));
        assert!(!new_line.anchors_line("a.rs", Some(5), None));
        assert!(!new_line.anchors_line("b.rs", None, Some(5)));
        // A line anchor is not a file-level anchor.
        assert!(!new_line.anchors_file("a.rs"));

        let old_line = CommentAnchor::Line {
            file: "a.rs".into(),
            side: Side::Old,
            line: 5,
        };
        assert!(old_line.anchors_line("a.rs", Some(5), None));
        assert!(!old_line.anchors_line("a.rs", None, Some(5)));

        // The review summary anchors to neither a file nor a line.
        assert!(!CommentAnchor::Review.anchors_file("a.rs"));
        assert!(!CommentAnchor::Review.anchors_line("a.rs", Some(1), Some(1)));
    }

    #[test]
    fn validate_body_bounds() {
        assert!(validate_body("ok").is_ok());
        assert!(validate_body("   ").is_err());
        assert!(validate_body(&"x".repeat(MAX_REVIEW_BODY_LEN + 1)).is_err());
    }

    #[test]
    fn parses_a_simple_modification() {
        let diff = "\
diff --git a/src/foo.rs b/src/foo.rs
index 111..222 100644
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -1,3 +1,4 @@ fn foo
 context one
-removed line
+added line
+another added
 context two
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.path, "src/foo.rs");
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!(f.added_count(), 2);
        assert_eq!(f.deleted_count(), 1);
        assert_eq!(f.hunks.len(), 1);
        let h = &f.hunks[0];
        assert_eq!(h.old_start, 1);
        assert_eq!(h.new_start, 1);
        assert_eq!(h.header, "fn foo");
        // First context line is numbered on both sides.
        assert_eq!(h.lines[0].old_no, Some(1));
        assert_eq!(h.lines[0].new_no, Some(1));
        // The removed line has only an old number.
        let removed = h
            .lines
            .iter()
            .find(|l| l.kind == DiffLineKind::Del)
            .unwrap();
        assert_eq!(removed.old_no, Some(2));
        assert_eq!(removed.new_no, None);
        // The first added line has only a new number.
        let added = h
            .lines
            .iter()
            .find(|l| l.kind == DiffLineKind::Add)
            .unwrap();
        assert_eq!(added.new_no, Some(2));
        assert_eq!(added.old_no, None);
    }

    #[test]
    fn parses_added_and_deleted_files() {
        let diff = "\
diff --git a/new.txt b/new.txt
new file mode 100644
index 000..abc
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index abc..000
--- a/gone.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "new.txt");
        assert_eq!(files[0].status, FileStatus::Added);
        assert_eq!(files[0].added_count(), 2);
        assert_eq!(files[1].path, "gone.txt");
        assert_eq!(files[1].status, FileStatus::Deleted);
        assert_eq!(files[1].deleted_count(), 1);
    }

    #[test]
    fn parses_rename_and_multi_hunk() {
        let diff = "\
diff --git a/old/name.rs b/new/name.rs
similarity index 90%
rename from old/name.rs
rename to new/name.rs
index 111..222 100644
--- a/old/name.rs
+++ b/new/name.rs
@@ -1,2 +1,2 @@
 a
-b
+B
@@ -10,2 +10,3 @@
 c
+d
 e
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.status, FileStatus::Renamed);
        assert_eq!(f.old_path.as_deref(), Some("old/name.rs"));
        assert_eq!(f.path, "new/name.rs");
        assert_eq!(f.hunks.len(), 2);
        assert_eq!(f.hunks[1].old_start, 10);
        assert_eq!(f.hunks[1].new_start, 10);
    }

    #[test]
    fn ignores_no_newline_marker() {
        let diff = "\
diff --git a/f b/f
index 1..2 100644
--- a/f
+++ b/f
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].added_count(), 1);
        assert_eq!(files[0].deleted_count(), 1);
        // The `\ No newline` markers are not counted as diff lines.
        assert_eq!(files[0].hunks[0].lines.len(), 2);
    }

    #[test]
    fn body_lines_starting_with_dashes_or_pluses_are_not_misparsed_as_headers() {
        // A removed SQL/Lua comment (`-- …`) renders as `--- …`; an added one as
        // `+++ …`. Inside a hunk these must stay Add/Del body lines and not be
        // mistaken for the file's old/new path header (which would drop the line
        // and desync line numbers for the rest of the file).
        let diff = "\
diff --git a/q.sql b/q.sql
index 1..2 100644
--- a/q.sql
+++ b/q.sql
@@ -1,2 +1,2 @@
 SELECT 1;
-- old comment
++ new comment
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(
            f.path, "q.sql",
            "path comes from the real +++ header, not a body line"
        );
        assert_eq!(f.old_path, None);
        assert_eq!(f.added_count(), 1);
        assert_eq!(f.deleted_count(), 1);
        let h = &f.hunks[0];
        // Context + Del + Add, line numbers intact.
        assert_eq!(h.lines.len(), 3);
        let del = h
            .lines
            .iter()
            .find(|l| l.kind == DiffLineKind::Del)
            .unwrap();
        assert_eq!(del.text, "- old comment");
        assert_eq!(del.old_no, Some(2));
        let add = h
            .lines
            .iter()
            .find(|l| l.kind == DiffLineKind::Add)
            .unwrap();
        assert_eq!(add.text, "+ new comment");
        assert_eq!(add.new_no, Some(2));
    }

    #[test]
    fn empty_input_yields_no_files() {
        assert!(parse_unified_diff("").is_empty());
    }

    /// Build a hunk from a compact `kind` list (`c`/`-`/`+`) for pairing tests.
    fn hunk_of(kinds: &str) -> DiffHunk {
        let (mut o, mut n) = (1u32, 1u32);
        let lines = kinds
            .chars()
            .map(|ch| {
                let (kind, old, new) = match ch {
                    '-' => {
                        let l = (DiffLineKind::Del, Some(o), None);
                        o += 1;
                        l
                    }
                    '+' => {
                        let l = (DiffLineKind::Add, None, Some(n));
                        n += 1;
                        l
                    }
                    _ => {
                        let l = (DiffLineKind::Context, Some(o), Some(n));
                        o += 1;
                        n += 1;
                        l
                    }
                };
                DiffLine {
                    kind,
                    old_no: old,
                    new_no: new,
                    text: ch.to_string(),
                }
            })
            .collect();
        DiffHunk {
            old_start: 1,
            new_start: 1,
            header: String::new(),
            lines,
        }
    }

    #[test]
    fn pairs_context_with_itself() {
        // Two context lines → two rows, each showing the same line on both sides.
        let pairs = pair_hunk(&hunk_of("cc"));
        assert_eq!(
            pairs,
            vec![
                SidePair {
                    old: Some(0),
                    new: Some(0)
                },
                SidePair {
                    old: Some(1),
                    new: Some(1)
                },
            ]
        );
    }

    #[test]
    fn pairs_even_change_block_positionally() {
        // `c - - + + c` → context, then del[0]↔add[0], del[1]↔add[1], context.
        let pairs = pair_hunk(&hunk_of("c--++c"));
        assert_eq!(
            pairs,
            vec![
                SidePair {
                    old: Some(0),
                    new: Some(0)
                },
                SidePair {
                    old: Some(1),
                    new: Some(3)
                },
                SidePair {
                    old: Some(2),
                    new: Some(4)
                },
                SidePair {
                    old: Some(5),
                    new: Some(5)
                },
            ]
        );
    }

    #[test]
    fn uneven_block_leaves_remainder_half_blank() {
        // 3 deletions, 1 addition: del[0]↔add[0], then two del-only rows.
        let pairs = pair_hunk(&hunk_of("---+"));
        assert_eq!(
            pairs,
            vec![
                SidePair {
                    old: Some(0),
                    new: Some(3)
                },
                SidePair {
                    old: Some(1),
                    new: None
                },
                SidePair {
                    old: Some(2),
                    new: None
                },
            ]
        );
    }

    #[test]
    fn pure_additions_and_deletions_pair_against_blanks() {
        // Pure additions: every row is new-only (old blank).
        assert_eq!(
            pair_hunk(&hunk_of("++")),
            vec![
                SidePair {
                    old: None,
                    new: Some(0)
                },
                SidePair {
                    old: None,
                    new: Some(1)
                },
            ]
        );
        // Pure deletions: every row is old-only (new blank).
        assert_eq!(
            pair_hunk(&hunk_of("--")),
            vec![
                SidePair {
                    old: Some(0),
                    new: None
                },
                SidePair {
                    old: Some(1),
                    new: None
                },
            ]
        );
    }

    #[test]
    fn every_line_appears_in_exactly_one_pair() {
        let hunk = hunk_of("c--++c-+c");
        let pairs = pair_hunk(&hunk);
        let mut seen = std::collections::HashSet::new();
        for p in &pairs {
            // A context pair references the same line on both sides (old == new);
            // an add/del pair references two distinct lines. Count distinct.
            let distinct: std::collections::HashSet<usize> =
                [p.old, p.new].into_iter().flatten().collect();
            for li in distinct {
                assert!(seen.insert(li), "line {li} placed twice");
            }
        }
        assert_eq!(
            seen.len(),
            hunk.lines.len(),
            "every hunk line is placed exactly once"
        );
    }
}
