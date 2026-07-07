//! Native code-review view: state + behavior. A scrollable diff of the active
//! session's worktree — the branch (`<base>..HEAD`), the uncommitted working
//! changes, or a single commit (see [`ReviewTarget`]) — with classified
//! comments, a review summary, and per-file/hunk "reviewed" marks — all
//! rendered natively (see
//! [`crate::ui::code_review`]) and persisted in SQLite
//! ([`crate::storage::review`]). Comment composition is an in-view sub-mode
//! (no separate modal), so the whole feature lives under
//! [`InputFocus::CodeReview`].

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyModifiers};

use std::path::{Path, PathBuf};

use crate::session::review::{
    pair_hunk, parse_unified_diff, Classification, CommentAnchor, DiffFile, ReviewComment, Side,
};
use crate::session::{HostDef, SessionId};

use super::modals::TextArea;
use super::{App, InputFocus, StatusLevel};

/// One repository under review — a session worktree. A single-repo session has
/// one; a multi-repo session has several (each on the shared branch). `base` is
/// that repo's review base (`<base>..HEAD`), resolved when the review opens.
#[derive(Debug, Clone)]
pub(crate) struct ReviewRepo {
    /// Display name used to namespace file paths + group the changed-files list
    /// in a multi-repo review.
    pub label: String,
    pub dir: PathBuf,
    pub base: Option<String>,
}

/// What the review is showing: the whole branch, the uncommitted working
/// changes, or a single commit. Mirrors tuicr's review targets (`-r`, `-w`, a
/// commit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReviewTarget {
    /// `<base>..HEAD` of every repo.
    Branch,
    /// Uncommitted changes vs `HEAD` (staged + unstaged) of every repo.
    Working,
    /// A single commit in one repo (`repo` indexes [`CodeReviewState::repos`]).
    Commit { repo: usize, sha: String },
}

/// In-view picker for choosing the [`ReviewTarget`].
pub(crate) struct TargetPickerState {
    pub entries: Vec<ReviewTarget>,
    pub selected: usize,
}

/// A clickable button in the review view footer. Index-free so the renderer and
/// click dispatch agree by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewButton {
    Comment,
    FileComment,
    Summary,
    MarkReviewed,
    Copy,
    SendToAgent,
    Close,
    Save,
    Cancel,
    CycleClass,
    /// Toggle unified ↔ side-by-side.
    ToggleView,
    /// Toggle soft-wrap of long diff lines (unified layout).
    ToggleWrap,
    /// Open the review-target picker (branch / working / per-commit).
    Target,
    /// Open the find-in-diff search.
    Find,
}

/// One rendered row in the flattened review view (diff + interleaved comments +
/// summary). Selection, scroll, and clicks all operate on this list.
#[derive(Debug, Clone)]
pub(crate) enum ReviewRow {
    FileHeader(usize),
    HunkHeader(usize, usize),
    Line(usize, usize, usize),
    /// A line/file-level comment, by its id.
    Comment(i64),
    SummaryHeader,
    /// A review-level (summary) comment, by its id.
    Summary(i64),
    /// A non-selectable informational line ("No changes", hints).
    Info(String),
}

impl ReviewRow {
    pub(crate) fn is_selectable(&self) -> bool {
        !matches!(self, ReviewRow::Info(_))
    }
}

/// In-review text search (the `/`-triggered find-in-diff sub-mode), mirroring
/// the file viewer's find. Matches rows whose text — file paths, hunk headers,
/// diff line bodies, and comment bodies — contains the query (case-insensitive).
/// While [`Self::editing`] every key edits the query (the selection jumps to the
/// first match as you type); `Enter`/`↓`/`Ctrl+N` step to the next match, `↑`/
/// `Ctrl+P` the previous, and `Tab` commits — after which the bar stays for
/// highlighting and `n`/`N` step matches just like the file viewer.
pub(crate) struct ReviewSearch {
    /// The query being typed (append-only editing, like the file viewer).
    pub query: String,
    /// Whether the query line is still being typed (captures all keys).
    pub editing: bool,
    /// Matching row indices (into [`CodeReviewState::rows`]), in row order. The
    /// "current" position shown in the bar is derived from the selection, so
    /// `n`/`N` always step relative to where the cursor actually is.
    pub matches: Vec<usize>,
}

/// In-progress comment composition (an in-view sub-mode of the review).
pub(crate) struct ComposeState {
    pub anchor: CommentAnchor,
    pub classification: Classification,
    pub body: TextArea,
    /// `Some` when editing an existing comment rather than creating one.
    pub editing_id: Option<i64>,
}

/// The open code-review view for the active session (rebuilt per toggle).
pub(crate) struct CodeReviewState {
    pub session_id: SessionId,
    /// The repos under review (one per worktree; ≥2 = multi-repo). Cached so
    /// switching targets doesn't re-resolve the session.
    pub repos: Vec<ReviewRepo>,
    /// Whether this is a multi-repo review (`repos.len() > 1`) — drives path
    /// namespacing + the changed-files labels.
    pub multi: bool,
    /// Combined diff across all repos. In a multi-repo review each file's `path`
    /// is namespaced `"<repo>/<path>"` so files, comments, and marks stay
    /// unambiguous across repos.
    pub files: Vec<DiffFile>,
    pub comments: Vec<ReviewComment>,
    pub reviewed_files: HashSet<String>,
    pub reviewed_hunks: HashSet<(String, usize)>,
    /// Files whose fold state is flipped from the default. A file folds (its
    /// diff collapses to just the header, tree-view style) once reviewed; this
    /// set lets the user manually expand a reviewed file to peek (or fold an
    /// unreviewed one) without changing its reviewed mark. Transient (not
    /// persisted): `is_file_folded` = `reviewed XOR fold_override`.
    pub fold_override: HashSet<String>,
    pub rows: Vec<ReviewRow>,
    pub selected: usize,
    pub scroll: usize,
    pub compose: Option<ComposeState>,
    /// Side-by-side (old | new) vs unified diff layout. Toggled with `v`. In
    /// this layout a deletion and its aligned addition share one selectable row
    /// (see [`crate::session::review::pair_hunk`]); comments still anchor to a
    /// single side.
    pub side_by_side: bool,
    /// The side a mouse click landed on, scoped to the row it selected
    /// (`(row, side)`). Lets a click on the old/new column of a paired
    /// side-by-side row steer a subsequent comment to that side; any keyboard
    /// move changes `selected` so the stale entry no longer matches and the
    /// anchor falls back to its default (New). `None` = keyboard-driven.
    pub click_side: Option<(usize, Side)>,
    /// Horizontal column offset of the diff body (the line-number gutter stays
    /// pinned). Slides long lines into view; toggled with `Left`/`Right`.
    /// Ignored while `wrap` is on and in the side-by-side layout.
    pub h_scroll: usize,
    /// Soft-wrap long diff lines onto extra screen rows instead of truncating
    /// (unified layout only). Toggled with `w`; forces `h_scroll = 0` when on.
    pub wrap: bool,
    /// What the diff currently shows (branch / working / a commit).
    pub target: ReviewTarget,
    /// Commits for the target picker as `(repo_index, short-sha, subject)`.
    pub commits: Vec<(usize, String, String)>,
    pub host: Option<HostDef>,
    /// The open target picker, if any.
    pub target_picker: Option<TargetPickerState>,
    /// The open find-in-diff search, if any (see [`ReviewSearch`]).
    pub search: Option<ReviewSearch>,
}

impl ReviewTarget {
    /// Display label for the picker / title, given the repos + loaded commits.
    pub(crate) fn label(
        &self,
        repos: &[ReviewRepo],
        commits: &[(usize, String, String)],
    ) -> String {
        match self {
            ReviewTarget::Branch => {
                if repos.len() == 1 {
                    let base = repos[0].base.as_deref().unwrap_or("base");
                    format!("All branch changes ({base}..HEAD)")
                } else {
                    format!("All branch changes · {} repos", repos.len())
                }
            }
            ReviewTarget::Working => "Working changes (uncommitted)".to_string(),
            ReviewTarget::Commit { repo, sha } => {
                let subject = commits
                    .iter()
                    .find(|(r, s, _)| r == repo && s == sha)
                    .map(|(_, _, subj)| subj.as_str())
                    .unwrap_or("");
                // Prefix the repo name only when multiple repos are in play.
                let prefix = if repos.len() > 1 {
                    repos
                        .get(*repo)
                        .map(|r| format!("{}: ", r.label))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                format!("{prefix}{sha}  {subject}")
            }
        }
    }
}

impl CodeReviewState {
    /// The diff-file index the current selection sits in (for the changed-files
    /// list highlight), if the selected row belongs to a file. A line/file-level
    /// comment resolves to its anchored file; the summary section and its
    /// comments belong to no file (`None`).
    pub(crate) fn current_file(&self) -> Option<usize> {
        match self.rows.get(self.selected)? {
            ReviewRow::FileHeader(fi)
            | ReviewRow::HunkHeader(fi, _)
            | ReviewRow::Line(fi, _, _) => Some(*fi),
            ReviewRow::Comment(id) => {
                let path = self.comment(*id)?.anchor.file()?;
                self.files.iter().position(|f| f.path == path)
            }
            _ => None,
        }
    }
}

impl CodeReviewState {
    /// The comment with `id`, if loaded.
    pub(crate) fn comment(&self, id: i64) -> Option<&ReviewComment> {
        self.comments.iter().find(|c| c.id == id)
    }

    /// The comment id of the selected row, if it is a comment / summary row.
    pub(crate) fn selected_comment_id(&self) -> Option<i64> {
        match self.rows.get(self.selected)? {
            ReviewRow::Comment(id) | ReviewRow::Summary(id) => Some(*id),
            _ => None,
        }
    }

    /// The file path the selected row belongs to (a line/hunk/header, or a
    /// comment anchored to a file) — so file-scoped actions (mark reviewed,
    /// fold) work from anywhere inside the file, not just its header.
    pub(crate) fn selected_file_path(&self) -> Option<String> {
        match self.rows.get(self.selected)? {
            ReviewRow::FileHeader(fi)
            | ReviewRow::HunkHeader(fi, _)
            | ReviewRow::Line(fi, _, _) => self.files.get(*fi).map(|f| f.path.clone()),
            ReviewRow::Comment(id) => self
                .comment(*id)
                .and_then(|c| c.anchor.file().map(str::to_string)),
            _ => None,
        }
    }

    /// The hunk index the selected row belongs to (a line or hunk header), for
    /// hunk-level reviewed marks.
    pub(crate) fn selected_hunk_index(&self) -> Option<usize> {
        match self.rows.get(self.selected)? {
            ReviewRow::Line(_, hi, _) | ReviewRow::HunkHeader(_, hi) => Some(*hi),
            _ => None,
        }
    }

    /// The comment anchor for the selected row, if it can carry one (a diff line
    /// or a file/hunk header). `file_level` forces a file anchor even on a line.
    ///
    /// On a line row the side is resolved so a paired side-by-side row (a
    /// deletion aligned with an addition) anchors sensibly: a mouse click that
    /// hit a specific column ([`Self::click_side`], scoped to this row) wins when
    /// that side exists; otherwise it defaults to New (the addition), falling
    /// back to Old for a pure deletion — matching the unified layout's
    /// prefer-new rule. Pure so the side logic is unit-testable without an
    /// [`App`].
    pub(crate) fn selected_anchor(&self, file_level: bool) -> Option<CommentAnchor> {
        match self.rows.get(self.selected)? {
            ReviewRow::Line(fi, hi, li) => {
                let file = self.files.get(*fi)?;
                if file_level {
                    return Some(CommentAnchor::File {
                        file: file.path.clone(),
                    });
                }
                let hunk = file.hunks.get(*hi)?;
                // Resolve the old-side / new-side DiffLines this row stands for.
                // Unified: a single line, treated as whichever side it carries.
                // Paired side-by-side: the whole SidePair (a deletion aligned
                // with an addition), so both sides may be present.
                let (old_line, new_line) = if self.side_by_side {
                    let pair = pair_hunk(hunk)
                        .into_iter()
                        .find(|p| p.old == Some(*li) || p.new == Some(*li))?;
                    (
                        pair.old.and_then(|i| hunk.lines.get(i)),
                        pair.new.and_then(|i| hunk.lines.get(i)),
                    )
                } else {
                    let line = hunk.lines.get(*li)?;
                    (
                        line.old_no.is_some().then_some(line),
                        line.new_no.is_some().then_some(line),
                    )
                };
                let want = self
                    .click_side
                    .filter(|(row, _)| *row == self.selected)
                    .map(|(_, s)| s);
                let (side, line) = match want {
                    Some(Side::Old) if old_line.is_some() => (Side::Old, old_line),
                    Some(Side::New) if new_line.is_some() => (Side::New, new_line),
                    _ if new_line.is_some() => (Side::New, new_line),
                    _ => (Side::Old, old_line),
                };
                let line = line?;
                let ln = match side {
                    Side::New => line.new_no?,
                    Side::Old => line.old_no?,
                };
                Some(CommentAnchor::Line {
                    file: file.path.clone(),
                    side,
                    line: ln,
                })
            }
            ReviewRow::FileHeader(fi) | ReviewRow::HunkHeader(fi, _) => {
                let file = self.files.get(*fi)?;
                Some(CommentAnchor::File {
                    file: file.path.clone(),
                })
            }
            _ => None,
        }
    }

    /// Whether `path`'s diff is folded (collapsed to just its header). A file
    /// folds once reviewed; [`Self::fold_override`] flips that per file so the
    /// user can peek at a reviewed file (or collapse an unreviewed one) without
    /// touching its reviewed mark.
    pub(crate) fn is_file_folded(&self, path: &str) -> bool {
        self.reviewed_files.contains(path) != self.fold_override.contains(path)
    }

    /// Rebuild [`Self::rows`] from the diff + loaded comments + marks. A folded
    /// file contributes only its header row (its hunks, lines, and comments are
    /// hidden until it is expanded), like a collapsed tree node.
    pub(crate) fn rebuild_rows(&mut self) {
        let mut rows: Vec<ReviewRow> = Vec::new();
        if self.files.is_empty() {
            rows.push(ReviewRow::Info(
                "No changes to show for this target.".to_string(),
            ));
        }
        for (fi, file) in self.files.iter().enumerate() {
            self.push_file_rows(&mut rows, fi, file);
        }
        // Summary section (always present so a summary can be added).
        rows.push(ReviewRow::SummaryHeader);
        for c in &self.comments {
            if c.anchor == CommentAnchor::Review {
                rows.push(ReviewRow::Summary(c.id));
            }
        }
        self.rows = rows;
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
        // Row indices shifted (a fold toggle, a new comment) — refresh any open
        // search's match set so `n`/`N` and the highlight stay anchored to live
        // rows. Selection is left where the caller put it.
        self.refresh_search_matches();
    }

    /// Append one file's rows: the header, then (unless folded) its file-level
    /// comments and each hunk's header + lines with their interleaved line
    /// comments.
    fn push_file_rows(&self, rows: &mut Vec<ReviewRow>, fi: usize, file: &DiffFile) {
        rows.push(ReviewRow::FileHeader(fi));
        if self.is_file_folded(&file.path) {
            return;
        }
        // File-level comments directly under the header.
        for c in &self.comments {
            if c.anchor.anchors_file(&file.path) {
                rows.push(ReviewRow::Comment(c.id));
            }
        }
        for (hi, hunk) in file.hunks.iter().enumerate() {
            rows.push(ReviewRow::HunkHeader(fi, hi));
            if self.side_by_side {
                // Paired layout: a deletion and its aligned addition share one
                // selectable row, keyed by the old (or, if absent, the new) line
                // — the renderer re-derives the pair from the same `pair_hunk`.
                // Comments for either side interleave after the shared row.
                for pair in pair_hunk(hunk) {
                    let rep = pair.old.or(pair.new).expect("a pair has ≥1 side");
                    rows.push(ReviewRow::Line(fi, hi, rep));
                    let mut prev = None;
                    for li in [pair.old, pair.new].into_iter().flatten() {
                        // A context pair points both sides at the same line;
                        // don't interleave its comments twice.
                        if Some(li) == prev {
                            continue;
                        }
                        prev = Some(li);
                        let line = &hunk.lines[li];
                        for c in &self.comments {
                            if c.anchor.anchors_line(&file.path, line.old_no, line.new_no) {
                                rows.push(ReviewRow::Comment(c.id));
                            }
                        }
                    }
                }
            } else {
                for (li, line) in hunk.lines.iter().enumerate() {
                    rows.push(ReviewRow::Line(fi, hi, li));
                    // Line comments anchored to this line (either side).
                    for c in &self.comments {
                        if c.anchor.anchors_line(&file.path, line.old_no, line.new_no) {
                            rows.push(ReviewRow::Comment(c.id));
                        }
                    }
                }
            }
        }
    }

    /// Number of `+`/`-` lines across all files (for the title).
    pub(crate) fn totals(&self) -> (usize, usize) {
        let add = self.files.iter().map(DiffFile::added_count).sum();
        let del = self.files.iter().map(DiffFile::deleted_count).sum();
        (add, del)
    }

    /// The searchable text of a row: the file path, the hunk section heading,
    /// the diff line body, or the comment body. Drives both `/` search matching
    /// and the in-row match highlight, so the two never disagree about what a
    /// row "contains".
    pub(crate) fn row_text(&self, row: &ReviewRow) -> Option<String> {
        match row {
            ReviewRow::FileHeader(fi) => self.files.get(*fi).map(|f| f.path.clone()),
            ReviewRow::HunkHeader(fi, hi) => self
                .files
                .get(*fi)
                .and_then(|f| f.hunks.get(*hi))
                .map(|h| h.header.clone()),
            ReviewRow::Line(fi, hi, li) => self
                .files
                .get(*fi)
                .and_then(|f| f.hunks.get(*hi))
                .and_then(|h| h.lines.get(*li))
                .map(|l| l.text.clone()),
            ReviewRow::Comment(id) | ReviewRow::Summary(id) => {
                self.comment(*id).map(|c| c.body.clone())
            }
            ReviewRow::SummaryHeader | ReviewRow::Info(_) => None,
        }
    }

    /// Row indices (into [`Self::rows`], in order) whose [`Self::row_text`]
    /// contains `query` case-insensitively. Empty/whitespace query → no matches.
    /// Pure so the search is unit-testable independent of [`App`].
    pub(crate) fn search_matches(&self, query: &str) -> Vec<usize> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        (0..self.rows.len())
            .filter(|&i| {
                self.row_text(&self.rows[i])
                    .is_some_and(|t| t.to_lowercase().contains(&q))
            })
            .collect()
    }

    /// Refresh the open search's match set against the current [`Self::rows`]
    /// (no-op when no search is open). Called after the query changes and after
    /// the rows are rebuilt so `n`/`N` + the highlight stay anchored to live rows.
    pub(crate) fn refresh_search_matches(&mut self) {
        let Some(query) = self.search.as_ref().map(|s| s.query.clone()) else {
            return;
        };
        let matches = self.search_matches(&query);
        if let Some(s) = self.search.as_mut() {
            s.matches = matches;
        }
    }
}

impl App {
    /// Toggle the native code-review view for the active session. Building it
    /// runs `git diff <base>..HEAD` synchronously (normally fast); a huge-repo
    /// async build is a follow-up.
    pub(crate) fn toggle_code_review(&mut self) {
        if self.active_review().is_some() {
            self.close_code_review();
            return;
        }
        // Measured as a slow op: the build shells out to git (over SSH for a
        // remote session), and the duration attributes any perceived stall.
        self.time_op("code_review_build", |s| s.open_code_review());
    }

    fn open_code_review(&mut self) {
        let Some(session) = self.sessions.get(self.active_index) else {
            return;
        };
        let session_id = session.info.id;
        // Remote sessions run `git` over SSH against their host.
        let host = session
            .info
            .remote_host
            .as_deref()
            .and_then(|name| self.host_for_backend(Some(&format!("ssh:{name}"))).cloned());
        // The persisted fork point (the primary worktree's base) is the default
        // base for every repo; each repo falls back to its own default branch.
        let session_base = self
            .db
            .get_session_base_branch(session_id)
            .ok()
            .flatten()
            .filter(|b| !b.trim().is_empty());

        // One review repo per worktree (multi-repo sessions have several); a
        // session with no worktree reviews its bare cwd. Attached `additional_dirs`
        // are reference-only (no branch) and are not reviewed.
        let worktrees = session.info.worktrees.clone();
        let mut repos: Vec<ReviewRepo> = if worktrees.is_empty() {
            let Some(cwd) = session.info.cwd.clone() else {
                self.set_error("This session has no working directory to review");
                return;
            };
            let base = resolve_repo_base(session_base.as_deref(), &cwd, host.as_ref());
            vec![ReviewRepo {
                label: String::new(),
                dir: cwd,
                base,
            }]
        } else {
            worktrees
                .iter()
                .map(|w| {
                    let label = crate::git::repo_display_name(&w.repo_path).unwrap_or_else(|| {
                        w.worktree_path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    });
                    let base =
                        resolve_repo_base(session_base.as_deref(), &w.worktree_path, host.as_ref());
                    ReviewRepo {
                        label,
                        dir: w.worktree_path.clone(),
                        base,
                    }
                })
                .collect()
        };
        // Disambiguate repos that share a display name, so the `"<label>/<path>"`
        // namespacing can't collide and cross-contaminate comments/marks.
        dedup_repo_labels(&mut repos);
        let multi = repos.len() > 1;

        // Commits across every repo for the target picker (tagged by repo index).
        let mut commits: Vec<(usize, String, String)> = Vec::new();
        for (i, r) in repos.iter().enumerate() {
            if let Some(b) = r.base.as_deref() {
                for (sha, subj) in crate::git::list_commits_on(host.as_ref(), &r.dir, b) {
                    commits.push((i, sha, subj));
                }
            }
        }
        // Default to the branch diff when any repo has a base; otherwise the
        // uncommitted changes (a bare / unknown-base session still reviews its tree).
        let target = if repos.iter().any(|r| r.base.is_some()) {
            ReviewTarget::Branch
        } else {
            ReviewTarget::Working
        };
        let files = build_files(&repos, &target, host.as_ref(), multi);

        let state = CodeReviewState {
            session_id,
            repos,
            multi,
            files,
            comments: Vec::new(),
            reviewed_files: HashSet::new(),
            reviewed_hunks: HashSet::new(),
            fold_override: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            compose: None,
            side_by_side: false,
            click_side: None,
            h_scroll: 0,
            wrap: false,
            target,
            commits,
            host,
            target_picker: None,
            search: None,
        };
        // Install the bare state for this session, then load comments + marks +
        // build rows through the single shared path (`reload_review_data`).
        self.code_reviews.insert(session_id, state);
        self.reload_review_data();
        self.focus = InputFocus::CodeReview;
    }

    /// Reload comments + marks from the DB into the open review and rebuild rows.
    pub(crate) fn reload_review_data(&mut self) {
        self.time_op("code_review_reload", |s| s.reload_review_data_inner());
    }

    fn reload_review_data_inner(&mut self) {
        let Some(cr) = self.active_review() else {
            return;
        };
        let sid = cr.session_id;
        let comments = self.db.list_review_comments(sid).unwrap_or_default();
        let marks = self.db.list_review_marks(sid).unwrap_or_default();
        let mut reviewed_files = HashSet::new();
        let mut reviewed_hunks = HashSet::new();
        for (file, hunk) in marks {
            match hunk {
                None => {
                    reviewed_files.insert(file);
                }
                Some(h) => {
                    reviewed_hunks.insert((file, h));
                }
            }
        }
        if let Some(cr) = self.active_review_mut() {
            cr.comments = comments;
            cr.reviewed_files = reviewed_files;
            cr.reviewed_hunks = reviewed_hunks;
            cr.rebuild_rows();
        }
    }

    pub(crate) fn close_code_review(&mut self) {
        if let Some(sid) = self.active_session_id() {
            self.code_reviews.remove(&sid);
        }
        if matches!(self.focus, InputFocus::CodeReview | InputFocus::ReviewFiles) {
            self.focus = InputFocus::Terminal;
        }
    }

    // ── Navigation ───────────────────────────────────────────────────────────

    /// Move the selection by `delta` rows, skipping non-selectable info rows,
    /// and keep it on screen.
    pub(crate) fn cr_move(&mut self, delta: isize) {
        let Some(cr) = self.active_review_mut() else {
            return;
        };
        if cr.rows.is_empty() {
            return;
        }
        let step = delta.signum();
        if step == 0 {
            return;
        }
        let mut idx = cr.selected as isize;
        let len = cr.rows.len() as isize;
        loop {
            idx += step;
            if idx < 0 || idx >= len {
                return; // hit an edge; leave selection put
            }
            if cr.rows[idx as usize].is_selectable() {
                cr.selected = idx as usize;
                break;
            }
        }
        cr.ensure_visible();
    }

    pub(crate) fn cr_page(&mut self, down: bool) {
        let page = self.code_review_viewport().max(1) as isize;
        let step = if down { 1 } else { -1 };
        for _ in 0..page {
            self.cr_move(step);
        }
    }

    pub(crate) fn cr_home_end(&mut self, end: bool) {
        let Some(cr) = self.active_review_mut() else {
            return;
        };
        if end {
            cr.selected = cr.rows.len().saturating_sub(1);
            while cr.selected > 0 && !cr.rows[cr.selected].is_selectable() {
                cr.selected -= 1;
            }
        } else {
            cr.selected = 0;
            while cr.selected + 1 < cr.rows.len() && !cr.rows[cr.selected].is_selectable() {
                cr.selected += 1;
            }
        }
        cr.ensure_visible();
    }

    /// Jump to the next/previous file header.
    pub(crate) fn cr_jump_file(&mut self, forward: bool) {
        let Some(cr) = self.active_review_mut() else {
            return;
        };
        let len = cr.rows.len();
        if len == 0 {
            return;
        }
        let range: Vec<usize> = if forward {
            (cr.selected + 1..len).collect()
        } else {
            (0..cr.selected).rev().collect()
        };
        for i in range {
            if matches!(
                cr.rows[i],
                ReviewRow::FileHeader(_) | ReviewRow::SummaryHeader
            ) {
                cr.selected = i;
                cr.ensure_visible();
                return;
            }
        }
    }

    /// Jump to the next/previous hunk header (tuicr's `[`/`]`).
    pub(crate) fn cr_jump_hunk(&mut self, forward: bool) {
        let Some(cr) = self.active_review_mut() else {
            return;
        };
        let len = cr.rows.len();
        let range: Vec<usize> = if forward {
            (cr.selected + 1..len).collect()
        } else {
            (0..cr.selected).rev().collect()
        };
        for i in range {
            if matches!(cr.rows[i], ReviewRow::HunkHeader(_, _)) {
                cr.selected = i;
                cr.ensure_visible();
                return;
            }
        }
    }

    /// Jump the diff to a file's header by diff-file index (a click in the
    /// changed-files list).
    pub(crate) fn cr_jump_to_file(&mut self, file_idx: usize) {
        let Some(cr) = self.active_review_mut() else {
            return;
        };
        if let Some(pos) = cr
            .rows
            .iter()
            .position(|r| matches!(r, ReviewRow::FileHeader(fi) if *fi == file_idx))
        {
            cr.selected = pos;
            cr.ensure_visible();
        }
    }

    /// Toggle the unified ↔ paired side-by-side diff layout (tuicr's
    /// `diff_view`). The two layouts have different row sets — side-by-side
    /// merges each aligned deletion+addition into one row — so the rows are
    /// rebuilt; `rebuild_rows` clamps the selection if it fell off the end.
    pub(crate) fn cr_toggle_side_by_side(&mut self) {
        if let Some(cr) = self.active_review_mut() {
            cr.side_by_side = !cr.side_by_side;
            // Horizontal scroll is unified-only; leaving it set would strand the
            // side-by-side cells at a nonzero offset once you switch back.
            if cr.side_by_side {
                cr.h_scroll = 0;
            }
            cr.click_side = None;
            cr.rebuild_rows();
            cr.ensure_visible();
        }
    }

    /// Toggle soft-wrap of long diff lines (unified body, or each paired
    /// side-by-side half independently). Wrapping and horizontal scroll are
    /// mutually exclusive, so turning wrap on resets the column offset.
    pub(crate) fn cr_toggle_wrap(&mut self) {
        if let Some(cr) = self.active_review_mut() {
            cr.wrap = !cr.wrap;
            if cr.wrap {
                cr.h_scroll = 0;
            }
        }
    }

    /// Scroll the diff body horizontally by `delta` columns (positive = right).
    /// No-op while wrapped or in side-by-side; otherwise clamped so you can't
    /// scroll past the longest line (the exact body width isn't known here, so
    /// `render_rows` applies a final clamp against the drawn `avail`).
    pub(crate) fn cr_scroll_h(&mut self, delta: isize) {
        let Some(cr) = self.active_review_mut() else {
            return;
        };
        if cr.wrap || cr.side_by_side {
            return;
        }
        let max = cr.max_line_width().saturating_sub(1);
        let next = (cr.h_scroll as isize + delta).clamp(0, max as isize);
        cr.h_scroll = next as usize;
    }

    /// Open the review-target picker (branch / working / per-commit).
    pub(crate) fn cr_open_target_picker(&mut self) {
        let Some(cr) = self.active_review_mut() else {
            return;
        };
        let mut entries = vec![ReviewTarget::Working];
        if cr.repos.iter().any(|r| r.base.is_some()) {
            entries.push(ReviewTarget::Branch);
        }
        for (repo, sha, _) in &cr.commits {
            entries.push(ReviewTarget::Commit {
                repo: *repo,
                sha: sha.clone(),
            });
        }
        let selected = entries.iter().position(|t| *t == cr.target).unwrap_or(0);
        cr.target_picker = Some(TargetPickerState { entries, selected });
    }

    /// Switch the diff to a different target, recomputing it from git.
    pub(crate) fn cr_set_target(&mut self, target: ReviewTarget) {
        if self.active_review().is_none() {
            return;
        }
        // Same git-subprocess cost profile as opening the review.
        self.time_op("code_review_retarget", |s| {
            let Some(cr) = s.active_review() else {
                return;
            };
            let repos = cr.repos.clone();
            let host = cr.host.clone();
            let multi = cr.multi;
            let files = build_files(&repos, &target, host.as_ref(), multi);
            if let Some(cr) = s.active_review_mut() {
                cr.target = target;
                cr.files = files;
                cr.selected = 0;
                cr.scroll = 0;
                cr.target_picker = None;
                cr.rebuild_rows();
            }
        });
    }

    /// Apply the target-picker entry at `idx` (a click), mirroring the keyboard
    /// Enter path. Out-of-range indices are ignored.
    pub(crate) fn cr_select_target(&mut self, idx: usize) {
        let chosen = self
            .active_review()
            .and_then(|cr| cr.target_picker.as_ref())
            .and_then(|picker| picker.entries.get(idx).cloned());
        if let Some(target) = chosen {
            self.cr_set_target(target);
        }
    }

    /// Key handling while the target picker is open.
    fn handle_target_picker_key(&mut self, code: KeyCode) {
        let chosen = {
            let Some(cr) = self.active_review_mut() else {
                return;
            };
            let Some(picker) = cr.target_picker.as_mut() else {
                return;
            };
            let len = picker.entries.len();
            match code {
                KeyCode::Esc => {
                    cr.target_picker = None;
                    None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if len > 0 {
                        picker.selected = (picker.selected + 1).min(len - 1);
                    }
                    None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    picker.selected = picker.selected.saturating_sub(1);
                    None
                }
                KeyCode::Enter => picker.entries.get(picker.selected).cloned(),
                _ => None,
            }
        };
        if let Some(target) = chosen {
            self.cr_set_target(target);
        }
    }

    /// Set the selection directly (a column-less click / scrollbar drag), if the
    /// row is selectable. Clears any recorded click side (no column context).
    pub(crate) fn cr_select_row(&mut self, idx: usize) {
        if let Some(cr) = self.active_review_mut() {
            if cr.rows.get(idx).is_some_and(ReviewRow::is_selectable) {
                cr.selected = idx;
                cr.click_side = None;
                cr.ensure_visible();
            }
        }
    }

    /// Select a row from a mouse click, recording which column (old | new) the
    /// click hit so a follow-up comment on a paired side-by-side row attaches to
    /// that side. `rel_x` is the click offset within the `width`-wide row; the
    /// paired layout splits at its center separator. In the unified layout the
    /// column carries no side, so nothing is recorded.
    pub(crate) fn cr_click_row(&mut self, idx: usize, rel_x: u16, width: u16) {
        if let Some(cr) = self.active_review_mut() {
            if cr.rows.get(idx).is_some_and(ReviewRow::is_selectable) {
                cr.selected = idx;
                cr.click_side = cr.side_by_side.then(|| {
                    let side = if rel_x < width / 2 {
                        Side::Old
                    } else {
                        Side::New
                    };
                    (idx, side)
                });
                cr.ensure_visible();
            }
        }
    }

    /// Rows that fit in the diff viewport (excluding the title + footer chrome,
    /// and the search bar when it is open).
    fn code_review_viewport(&self) -> usize {
        // Central pane height minus the block border (2) and the footer bar (1),
        // minus the search bar's row when a search is open.
        let (rows, _) = self.content_area_size();
        let search = usize::from(self.active_review().is_some_and(|cr| cr.search.is_some()));
        (rows as usize).saturating_sub(3 + search)
    }

    // ── Search ───────────────────────────────────────────────────────────────

    /// Open the find-in-diff search (`/`), capturing keystrokes into the query
    /// until `Tab`/`Esc`. A fresh query starts empty.
    pub(crate) fn cr_start_search(&mut self) {
        if let Some(cr) = self.active_review_mut() {
            cr.search = Some(ReviewSearch {
                query: String::new(),
                editing: true,
                matches: Vec::new(),
            });
        }
    }

    /// Close the search entirely (drops the query + highlights).
    pub(crate) fn cr_close_search(&mut self) {
        if let Some(cr) = self.active_review_mut() {
            cr.search = None;
        }
    }

    /// `Tab`: commit the query — stop editing but keep the search open so the
    /// matches stay highlighted and `n`/`N` step. An empty query just closes it
    /// (mirrors the file viewer hiding its bar on an empty commit).
    fn cr_commit_search(&mut self) {
        let Some(cr) = self.active_review_mut() else {
            return;
        };
        match cr.search.as_mut() {
            Some(s) if s.query.trim().is_empty() => cr.search = None,
            Some(s) => s.editing = false,
            None => {}
        }
    }

    /// Edit the query (append a char or backspace), then re-match and jump the
    /// selection to the first match — the file viewer's incremental search.
    fn cr_edit_search(&mut self, push: Option<char>) {
        if let Some(cr) = self.active_review_mut() {
            if let Some(s) = cr.search.as_mut() {
                match push {
                    Some(c) => s.query.push(c),
                    None => {
                        s.query.pop();
                    }
                }
            }
            cr.refresh_search_matches();
        }
        self.cr_jump_first_match();
    }

    /// Move the selection to the first match, if any.
    fn cr_jump_first_match(&mut self) {
        if let Some(cr) = self.active_review_mut() {
            if let Some(&first) = cr.search.as_ref().and_then(|s| s.matches.first()) {
                cr.selected = first;
                cr.ensure_visible();
            }
        }
    }

    /// Step to the next/previous match (`n`/`N`, `Enter`/arrows while typing),
    /// scanning from the current selection and wrapping — so it always moves
    /// relative to the cursor, exactly like the file viewer's `next_match`.
    pub(crate) fn cr_search_step(&mut self, forward: bool) {
        let Some(cr) = self.active_review_mut() else {
            return;
        };
        let Some(s) = cr.search.as_ref() else {
            return;
        };
        if s.matches.is_empty() {
            return;
        }
        let next = if forward {
            // First match strictly after the selection, else wrap to the first.
            s.matches
                .iter()
                .find(|&&i| i > cr.selected)
                .copied()
                .or_else(|| s.matches.first().copied())
        } else {
            // Last match strictly before the selection, else wrap to the last.
            s.matches
                .iter()
                .rev()
                .find(|&&i| i < cr.selected)
                .copied()
                .or_else(|| s.matches.last().copied())
        };
        if let Some(sel) = next {
            cr.selected = sel;
            cr.ensure_visible();
        }
    }

    /// Key handling while the search query line is being typed, mirroring the
    /// file viewer: `Enter`/`↓`/`Ctrl+N` next match, `↑`/`Ctrl+P` previous (all
    /// stay in the input), `Tab` commits, `Esc` cancels, `Backspace`/chars edit.
    fn handle_review_search_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Esc => self.cr_close_search(),
            KeyCode::Enter | KeyCode::Down => self.cr_search_step(true),
            KeyCode::Up => self.cr_search_step(false),
            KeyCode::Tab => self.cr_commit_search(),
            KeyCode::Char('n') if ctrl => self.cr_search_step(true),
            KeyCode::Char('p') if ctrl => self.cr_search_step(false),
            KeyCode::Backspace => self.cr_edit_search(None),
            KeyCode::Char(c) if !ctrl => self.cr_edit_search(Some(c)),
            _ => {}
        }
    }

    // ── Comments ─────────────────────────────────────────────────────────────

    /// The anchor for a line/file comment on the current selection, if any.
    fn cr_selected_anchor(&self, file_level: bool) -> Option<CommentAnchor> {
        self.active_review()?.selected_anchor(file_level)
    }

    /// Begin composing a comment at the selected line (or the file).
    pub(crate) fn cr_start_comment(&mut self, file_level: bool) {
        let Some(anchor) = self.cr_selected_anchor(file_level) else {
            self.set_error("Select a diff line or file to comment on");
            return;
        };
        if let Some(cr) = self.active_review_mut() {
            cr.compose = Some(ComposeState {
                anchor,
                classification: Classification::default(),
                body: TextArea::new(),
                editing_id: None,
            });
        }
    }

    /// Begin composing the review summary.
    pub(crate) fn cr_start_summary(&mut self) {
        if let Some(cr) = self.active_review_mut() {
            cr.compose = Some(ComposeState {
                anchor: CommentAnchor::Review,
                classification: Classification::default(),
                body: TextArea::new(),
                editing_id: None,
            });
        }
    }

    /// Edit the comment under the selection (if a comment row is selected).
    pub(crate) fn cr_edit_selected(&mut self) {
        let Some(cr) = self.active_review() else {
            return;
        };
        let Some(id) = cr.selected_comment_id() else {
            return;
        };
        let Some(c) = cr.comment(id) else { return };
        let mut body = TextArea::new();
        body.set(&c.body);
        let compose = ComposeState {
            anchor: c.anchor.clone(),
            classification: c.classification,
            body,
            editing_id: Some(id),
        };
        if let Some(cr) = self.active_review_mut() {
            cr.compose = Some(compose);
        }
    }

    /// Delete the comment under the selection.
    pub(crate) fn cr_delete_selected(&mut self) {
        let Some(cr) = self.active_review() else {
            return;
        };
        let Some(id) = cr.selected_comment_id() else {
            return;
        };
        if let Err(e) = self.db.delete_review_comment(id) {
            self.set_error(format!("Failed to delete comment: {e}"));
            return;
        }
        self.reload_review_data();
        self.set_status(StatusLevel::Info, "Comment deleted");
    }

    pub(crate) fn cr_compose_cancel(&mut self) {
        if let Some(cr) = self.active_review_mut() {
            cr.compose = None;
        }
    }

    pub(crate) fn cr_compose_cycle_class(&mut self, forward: bool) {
        if let Some(cr) = self.active_review_mut() {
            if let Some(comp) = cr.compose.as_mut() {
                comp.classification = if forward {
                    comp.classification.next()
                } else {
                    comp.classification.prev()
                };
            }
        }
    }

    /// Persist the in-progress comment, then reload.
    pub(crate) fn cr_compose_save(&mut self) {
        let Some(cr) = self.active_review() else {
            return;
        };
        let Some(comp) = cr.compose.as_ref() else {
            return;
        };
        let body = comp.body.value().trim().to_string();
        if let Err(e) = crate::session::review::validate_body(&body) {
            self.set_error(e);
            return;
        }
        let sid = cr.session_id;
        let anchor = comp.anchor.clone();
        let class = comp.classification;
        let editing = comp.editing_id;

        let result = match editing {
            Some(id) => self.db.update_review_comment(id, class, &body).map(|_| id),
            None => self.db.add_review_comment(sid, &anchor, class, &body),
        };
        match result {
            Ok(_) => {
                if let Some(cr) = self.active_review_mut() {
                    cr.compose = None;
                }
                self.reload_review_data();
                self.set_status(StatusLevel::Success, "Comment saved");
            }
            Err(e) => self.set_error(format!("Failed to save comment: {e}")),
        }
    }

    // ── Reviewed marks ─────────────────────────────────────────────────────────

    /// Toggle the "reviewed" mark for the selected file (or its hunk). Resolves
    /// the file from whatever row is selected (line, hunk header, file header, or
    /// a comment within the file) so it works anywhere inside the file.
    pub(crate) fn cr_toggle_reviewed(&mut self, hunk_level: bool) {
        let Some(cr) = self.active_review() else {
            return;
        };
        let file = cr.selected_file_path();
        let hunk = if hunk_level {
            cr.selected_hunk_index()
        } else {
            None
        };
        let Some(file) = file else {
            return;
        };
        let sid = cr.session_id;
        if let Err(e) = self.db.toggle_review_mark(sid, &file, hunk) {
            self.set_error(format!("Failed to update reviewed mark: {e}"));
            return;
        }
        // A file-level toggle returns the file to its default fold state
        // (reviewed → folded, unreviewed → expanded) by dropping any manual
        // override, so marking reviewed collapses the diff tree-style.
        if hunk.is_none() {
            if let Some(cr) = self.active_review_mut() {
                cr.fold_override.remove(&file);
            }
        }
        self.reload_review_data();
        // Land the cursor on the (possibly now-folded) file header rather than a
        // row that just disappeared.
        if hunk.is_none() {
            self.cr_select_file_header(&file);
        }
    }

    /// Move the selection to `path`'s file-header row, if present.
    fn cr_select_file_header(&mut self, path: &str) {
        if let Some(cr) = self.active_review_mut() {
            if let Some(pos) = cr.rows.iter().position(|r| {
                matches!(r, ReviewRow::FileHeader(fi) if cr.files.get(*fi).is_some_and(|f| f.path == path))
            }) {
                cr.selected = pos;
                cr.ensure_visible();
            }
        }
    }

    /// Toggle the fold of the file under the selection (manual tree expand /
    /// collapse). Works from any row inside the file; keeps the cursor on the
    /// file header.
    pub(crate) fn cr_toggle_fold(&mut self) {
        let Some(file) = self.active_review().and_then(|cr| cr.selected_file_path()) else {
            return;
        };
        if let Some(cr) = self.active_review_mut() {
            // Flip the fold override: `remove` returns whether it was present, so
            // a failed remove means it wasn't folded → insert it.
            if !cr.fold_override.remove(&file) {
                cr.fold_override.insert(file.clone());
            }
            cr.rebuild_rows();
        }
        self.cr_select_file_header(&file);
    }

    // ── Export ───────────────────────────────────────────────────────────────

    /// Compile the review (comments grouped by file + summary) to markdown, or
    /// `None` when there are no comments yet.
    pub(crate) fn cr_review_markdown(&self) -> Option<String> {
        let cr = self.active_review()?;
        review_markdown(&cr.files, &cr.comments)
    }

    /// Copy the compiled review to the clipboard.
    pub(crate) fn cr_copy_markdown(&mut self) {
        let Some(md) = self.cr_review_markdown() else {
            self.set_status(StatusLevel::Info, "No review comments yet");
            return;
        };
        match self.clipboard.as_mut() {
            Some(cb) => match cb.set_text(&md) {
                Ok(_) => self.set_status(StatusLevel::Success, "Review copied to clipboard"),
                Err(e) => self.set_error(format!("Clipboard write failed: {e}")),
            },
            None => self.set_error("Clipboard not available"),
        }
    }

    /// Paste the compiled review into the session's agent as a prompt to address
    /// it — the review → agent → re-review loop.
    pub(crate) fn cr_send_to_agent(&mut self) {
        let Some(md) = self.cr_review_markdown() else {
            self.set_status(StatusLevel::Info, "No review comments to send");
            return;
        };
        let Some(cr) = self.active_review() else {
            return;
        };
        let sid = cr.session_id;
        let prompt = format!("Please address the following code review:\n\n{md}");
        self.close_code_review();
        self.send_prompt_to_session(sid, &prompt, 0);
        self.set_status(StatusLevel::Success, "Review sent to agent");
    }

    /// Dispatch a footer-button click in the review view.
    pub(crate) fn cr_button(&mut self, button: ReviewButton) {
        match button {
            ReviewButton::Comment => self.cr_start_comment(false),
            ReviewButton::FileComment => self.cr_start_comment(true),
            ReviewButton::Summary => self.cr_start_summary(),
            ReviewButton::MarkReviewed => self.cr_toggle_reviewed(false),
            ReviewButton::Copy => self.cr_copy_markdown(),
            ReviewButton::SendToAgent => self.cr_send_to_agent(),
            ReviewButton::Close => self.close_code_review(),
            ReviewButton::Save => self.cr_compose_save(),
            ReviewButton::Cancel => self.cr_compose_cancel(),
            ReviewButton::CycleClass => self.cr_compose_cycle_class(true),
            ReviewButton::ToggleView => self.cr_toggle_side_by_side(),
            ReviewButton::ToggleWrap => self.cr_toggle_wrap(),
            ReviewButton::Target => self.cr_open_target_picker(),
            ReviewButton::Find => self.cr_start_search(),
        }
    }

    /// Whether `code`/`mods` is a global chord the review panes must let through
    /// to the global path so the user can always leave: the focus cycle, quit,
    /// the review toggle itself (so its key closes the pane like every other
    /// toggleable pane), and the overlay openers (help, settings, theme, search)
    /// so those modals stay reachable while a review is open. None collide with
    /// the panes' own keys (plain letters + nav). Shared by the diff pane and the
    /// changed-files pane so the two never drift.
    fn review_escape_chord(&self, code: KeyCode, mods: KeyModifiers) -> bool {
        matches!(
            self.keybindings.lookup(code, mods),
            Some(
                crate::session::Action::FocusForward
                    | crate::session::Action::FocusBackward
                    | crate::session::Action::QuitApp
                    | crate::session::Action::ToggleReview
                    // The sibling central-pane view: `ToggleShell` (F8) leaves
                    // the review straight to the shell, mirroring how the
                    // Shell tab does — so the F-keys switch views from anywhere.
                    | crate::session::Action::ToggleShell
                    | crate::session::Action::ToggleHelp
                    | crate::session::Action::OpenSettings
                    | crate::session::Action::OpenThemePicker
                    | crate::session::Action::ToggleInfoPanel
                    | crate::session::Action::GlobalSearch
            )
        )
    }

    /// Key capture for the code-review view (called before the global keybinding
    /// lookup). Returns `true` when consumed. Focus/quit chords pass through so
    /// the user can always leave.
    pub(crate) fn handle_code_review_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        if self.focus != InputFocus::CodeReview {
            return false;
        }
        // The review's `Ctrl+D`/`Ctrl+U` paging is deliberately not an escape
        // chord, so it keeps paging rather than deleting/restoring sessions.
        if self.review_escape_chord(code, mods) {
            return false;
        }

        // Sub-modes capture all keys: the target picker, then the compose box.
        if self
            .active_review()
            .is_some_and(|cr| cr.target_picker.is_some())
        {
            self.handle_target_picker_key(code);
            return true;
        }
        let composing = self.active_review().is_some_and(|cr| cr.compose.is_some());
        if composing {
            self.handle_review_compose_key(code, mods);
            return true;
        }
        // The search query line, while being typed, captures all keys.
        let searching = self
            .active_review()
            .is_some_and(|cr| cr.search.as_ref().is_some_and(|s| s.editing));
        if searching {
            self.handle_review_search_key(code, mods);
            return true;
        }

        let ctrl = mods.contains(KeyModifiers::CONTROL);
        // Ctrl+D / Ctrl+U half-page (pager convention). Handled before the guard
        // below since they are the only Ctrl chords this view acts on.
        if ctrl {
            match code {
                KeyCode::Char('d') => self.cr_page(true),
                KeyCode::Char('u') => self.cr_page(false),
                _ => {}
            }
            return true;
        }
        // Swallow any other Ctrl/Alt chord so it can't trip a plain-letter
        // command (Ctrl+F must not start a file comment, etc.). The global
        // escape chords already fell through above.
        if mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return true;
        }
        match code {
            // Esc clears an active (confirmed) search before closing the review,
            // so a stray `/` is one keystroke to undo.
            KeyCode::Esc if self.active_review().is_some_and(|cr| cr.search.is_some()) => {
                self.cr_close_search()
            }
            KeyCode::Esc => self.close_code_review(),
            KeyCode::Char('/') => self.cr_start_search(),
            KeyCode::Char('n') => self.cr_search_step(true),
            KeyCode::Char('N') => self.cr_search_step(false),
            KeyCode::Down | KeyCode::Char('j') => self.cr_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.cr_move(-1),
            KeyCode::PageDown => self.cr_page(true),
            KeyCode::PageUp => self.cr_page(false),
            KeyCode::Home | KeyCode::Char('g') => self.cr_home_end(false),
            KeyCode::End | KeyCode::Char('G') => self.cr_home_end(true),
            // tuicr file/hunk jumps: `}`/`{` files, `]`/`[` hunks. Tab/BackTab
            // mirror the file jump for keyboards where braces are awkward.
            KeyCode::Tab | KeyCode::Char('}') => self.cr_jump_file(true),
            KeyCode::BackTab | KeyCode::Char('{') => self.cr_jump_file(false),
            KeyCode::Char(']') => self.cr_jump_hunk(true),
            KeyCode::Char('[') => self.cr_jump_hunk(false),
            // Horizontal scroll of the body (gutter stays pinned). `h`/`l` are
            // free in the diff pane (they mean "open" only in the files pane).
            KeyCode::Left | KeyCode::Char('h') => self.cr_scroll_h(-8),
            KeyCode::Right | KeyCode::Char('l') => self.cr_scroll_h(8),
            KeyCode::Char('v') => self.cr_toggle_side_by_side(),
            KeyCode::Char('w') => self.cr_toggle_wrap(),
            KeyCode::Char('t') => self.cr_open_target_picker(),
            KeyCode::Char('c') => self.cr_start_comment(false),
            KeyCode::Char('f') => self.cr_start_comment(true),
            KeyCode::Char('s') => self.cr_start_summary(),
            KeyCode::Char('r') => self.cr_toggle_reviewed(false),
            KeyCode::Char('R') => self.cr_toggle_reviewed(true),
            KeyCode::Char('y') => self.cr_copy_markdown(),
            KeyCode::Char('e') => self.cr_send_to_agent(),
            KeyCode::Char('x') | KeyCode::Delete => self.cr_delete_selected(),
            KeyCode::Enter => self.cr_enter(),
            _ => {}
        }
        true
    }

    /// Handle keys while the review's **changed-files list** (file-viewer column)
    /// is focused. Mirrors the file viewer's options over the diff's files: `j`/`k`
    /// (and arrows) walk file to file with the diff following, `g`/`G` jump to the
    /// first/last file, `Enter`/`l` drop into the diff at the selected file, and
    /// `r`/`R` toggle the file/hunk reviewed mark. Returns `true` when consumed;
    /// focus/quit chords fall through (like [`Self::handle_code_review_key`]) so
    /// the user can always leave.
    pub(crate) fn handle_review_files_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        if self.focus != InputFocus::ReviewFiles {
            return false;
        }
        if self.review_escape_chord(code, mods) {
            return false;
        }

        let ctrl = mods.contains(KeyModifiers::CONTROL);
        if ctrl {
            // Half-page paging, matching the diff pane's `Ctrl+D`/`Ctrl+U`.
            match code {
                KeyCode::Char('d') => self.cr_page(true),
                KeyCode::Char('u') => self.cr_page(false),
                _ => {}
            }
            return true;
        }
        // Swallow any other Ctrl/Alt chord so it can't leak to the PTY or trip a
        // plain-letter command (the global escape chords already fell through).
        if mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return true;
        }
        match code {
            KeyCode::Esc => self.close_code_review(),
            KeyCode::Down | KeyCode::Char('j') => self.cr_jump_file(true),
            KeyCode::Up | KeyCode::Char('k') => self.cr_jump_file(false),
            KeyCode::PageDown => self.cr_page(true),
            KeyCode::PageUp => self.cr_page(false),
            // First / last *file* (not the trailing summary row, which the diff
            // pane's `g`/`G` would land on).
            KeyCode::Home | KeyCode::Char('g') => self.cr_jump_to_file(0),
            KeyCode::End | KeyCode::Char('G') => {
                if let Some(last) = self
                    .active_review()
                    .map(|cr| cr.files.len().saturating_sub(1))
                {
                    self.cr_jump_to_file(last);
                }
            }
            // Open the selected file: drop focus into the diff, which is already
            // scrolled to that file (mirrors the file viewer's "open" on a file).
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                self.focus = InputFocus::CodeReview;
            }
            KeyCode::Char('r') => self.cr_toggle_reviewed(false),
            KeyCode::Char('R') => self.cr_toggle_reviewed(true),
            // `/` searches the diff: open the find sub-mode and drop into the
            // diff pane, which the search input owns.
            KeyCode::Char('/') => {
                self.cr_start_search();
                self.focus = InputFocus::CodeReview;
            }
            _ => {}
        }
        true
    }

    /// `Enter`: edit the comment under the selection, or fold/unfold the file the
    /// selection sits in (collapsing its diff like a tree node).
    fn cr_enter(&mut self) {
        let on_comment = self
            .active_review()
            .and_then(|cr| cr.rows.get(cr.selected))
            .is_some_and(|r| matches!(r, ReviewRow::Comment(_) | ReviewRow::Summary(_)));
        if on_comment {
            self.cr_edit_selected();
        } else {
            self.cr_toggle_fold();
        }
    }

    /// Key handling while composing a comment in the review view.
    fn handle_review_compose_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Esc => self.cr_compose_cancel(),
            KeyCode::Char('s') if ctrl => self.cr_compose_save(),
            KeyCode::Tab => self.cr_compose_cycle_class(true),
            KeyCode::BackTab => self.cr_compose_cycle_class(false),
            _ => {
                if let Some(cr) = self.active_review_mut() {
                    if let Some(comp) = cr.compose.as_mut() {
                        apply_textarea_key(&mut comp.body, code, mods);
                    }
                }
            }
        }
    }
}

/// Resolve a single repo's review base: the session's persisted base when that
/// branch exists in this repo, else the repo's own default branch. Works local
/// or over SSH (`*_on` git helpers).
fn resolve_repo_base(
    session_base: Option<&str>,
    dir: &Path,
    host: Option<&HostDef>,
) -> Option<String> {
    if let Some(b) = session_base {
        if crate::git::branch_exists_on(host, dir, b) {
            return Some(b.to_string());
        }
    }
    let branches = crate::git::list_branches_on(host, dir).unwrap_or_default();
    crate::git::default_branch_on(host, dir, &branches)
}

/// Disambiguate repos that share a display name so the `"<label>/<path>"`
/// namespacing stays unique (two members basenamed `app` would otherwise key
/// comments/marks identically). Colliding labels get a ` (2)`, ` (3)`, … suffix
/// in repo order; unique labels are left untouched.
fn dedup_repo_labels(repos: &mut [ReviewRepo]) {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for r in repos.iter_mut() {
        let n = seen.entry(r.label.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            r.label = format!("{} ({})", r.label, *n);
        }
    }
}

/// Build the combined parsed diff for a review target across every repo. In a
/// multi-repo review each file's path is namespaced `"<repo>/<path>"` so files,
/// comments, and marks stay unambiguous across repos. Per-repo git failures are
/// skipped (that repo contributes no files) rather than aborting the review.
fn build_files(
    repos: &[ReviewRepo],
    target: &ReviewTarget,
    host: Option<&HostDef>,
    multi: bool,
) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    for (i, repo) in repos.iter().enumerate() {
        let raw = match target {
            ReviewTarget::Branch => repo
                .base
                .as_deref()
                .and_then(|b| crate::git::diff_against_on(host, &repo.dir, b)),
            ReviewTarget::Working => crate::git::diff_working_on(host, &repo.dir),
            // A commit target belongs to exactly one repo; the others contribute
            // nothing.
            ReviewTarget::Commit { repo: ri, sha } if *ri == i => {
                crate::git::show_commit_on(host, &repo.dir, sha)
            }
            ReviewTarget::Commit { .. } => None,
        };
        let Some(s) = raw else { continue };
        let mut parsed = parse_unified_diff(&s);
        if multi {
            for f in &mut parsed {
                f.path = format!("{}/{}", repo.label, f.path);
                if let Some(op) = f.old_path.as_mut() {
                    *op = format!("{}/{}", repo.label, op);
                }
            }
        }
        files.extend(parsed);
    }
    files
}

/// Compile a review's comments + summary to markdown, grouped by file in diff
/// order. Comments anchored to a file not in `files` (e.g. after switching the
/// review target) are omitted. Returns `None` when there are no comments. Pure
/// so the export is unit-testable independent of [`App`].
fn review_markdown(files: &[DiffFile], comments: &[ReviewComment]) -> Option<String> {
    if comments.is_empty() {
        return None;
    }
    let mut out = String::from("# Code review\n");
    // Inline comments grouped by file.
    for file in files {
        let file_comments = comments
            .iter()
            .filter(|c| c.anchor.file() == Some(file.path.as_str()));
        let mut wrote_header = false;
        for c in file_comments {
            if !wrote_header {
                out.push_str(&format!("\n## {}\n", file.path));
                wrote_header = true;
            }
            let loc = match &c.anchor {
                CommentAnchor::Line { side, line, .. } => format!("{}:{}", side.as_str(), line),
                _ => "file".to_string(),
            };
            out.push_str(&format!(
                "- **[{}]** ({}) {}\n",
                c.classification.label(),
                loc,
                c.body.replace('\n', "\n  ")
            ));
        }
    }
    // Summary.
    let mut wrote_summary = false;
    for c in comments
        .iter()
        .filter(|c| c.anchor == CommentAnchor::Review)
    {
        if !wrote_summary {
            out.push_str("\n## Summary\n");
            wrote_summary = true;
        }
        out.push_str(&format!(
            "- **[{}]** {}\n",
            c.classification.label(),
            c.body.replace('\n', "\n  ")
        ));
    }
    Some(out)
}

/// Apply a key to a [`TextArea`] (the comment body), mirroring the other
/// multi-line editors: Enter inserts a newline, arrows move, Backspace/Delete
/// edit, printable chars insert.
fn apply_textarea_key(ta: &mut TextArea, code: KeyCode, mods: KeyModifiers) {
    if super::modals::apply_ctrl_line_edit(ta, code, mods) {
        return;
    }
    match code {
        KeyCode::Enter => ta.insert_newline(),
        KeyCode::Backspace => ta.backspace(),
        KeyCode::Delete => ta.delete(),
        KeyCode::Left => ta.move_left(),
        KeyCode::Right => ta.move_right(),
        KeyCode::Up => ta.move_up(),
        KeyCode::Down => ta.move_down(),
        KeyCode::Home => ta.home(),
        KeyCode::End => ta.end(),
        KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => ta.insert(c),
        _ => {}
    }
}

impl CodeReviewState {
    /// Longest diff-line body width (in chars) across every hunk of every file.
    /// Bounds the horizontal scroll offset. O(total chars), so callers keep it
    /// off the hot path: it runs on an h-scroll keypress and in the renderer's
    /// clamp only while `h_scroll > 0` — never on the default unscrolled frame.
    pub(crate) fn max_line_width(&self) -> usize {
        self.files
            .iter()
            .flat_map(|f| f.hunks.iter())
            .flat_map(|h| h.lines.iter())
            .map(|l| l.text.chars().count())
            .max()
            .unwrap_or(0)
    }

    /// Keep the selected row within the scroll window. The viewport height is
    /// applied by the renderer; here we only keep `scroll <= selected`.
    fn ensure_visible(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        // The lower bound is enforced by the renderer (which knows the height);
        // it clamps `scroll` so `selected` stays visible.
    }
}

#[cfg(test)]
impl CodeReviewState {
    /// Build a minimal review with `n` modified files (`src/f0.rs`…) for
    /// App-level acceptance tests that need an open review without a real git
    /// worktree. Selection starts on the first file header.
    pub(crate) fn for_test(session_id: SessionId, n: usize) -> Self {
        use crate::session::review::{DiffFile, DiffHunk, DiffLine, DiffLineKind, FileStatus};
        let files = (0..n)
            .map(|i| DiffFile {
                path: format!("src/f{i}.rs"),
                old_path: None,
                status: FileStatus::Modified,
                hunks: vec![DiffHunk {
                    old_start: 1,
                    new_start: 1,
                    header: String::new(),
                    lines: vec![DiffLine {
                        kind: DiffLineKind::Add,
                        old_no: None,
                        new_no: Some(1),
                        text: "x".into(),
                    }],
                }],
            })
            .collect();
        let mut s = CodeReviewState {
            session_id,
            repos: vec![ReviewRepo {
                label: String::new(),
                dir: PathBuf::from("/tmp"),
                base: Some("main".into()),
            }],
            multi: false,
            files,
            comments: Vec::new(),
            reviewed_files: HashSet::new(),
            reviewed_hunks: HashSet::new(),
            fold_override: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            compose: None,
            side_by_side: false,
            click_side: None,
            h_scroll: 0,
            wrap: false,
            target: ReviewTarget::Branch,
            commits: Vec::new(),
            host: None,
            target_picker: None,
            search: None,
        };
        s.rebuild_rows();
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::review::{
        Classification, CommentAnchor, DiffFile, DiffHunk, DiffLine, DiffLineKind, FileStatus,
        ReviewComment, Side,
    };

    fn sample_file() -> DiffFile {
        DiffFile {
            path: "src/foo.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: vec![DiffHunk {
                old_start: 1,
                new_start: 1,
                header: String::new(),
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Context,
                        old_no: Some(1),
                        new_no: Some(1),
                        text: "ctx".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Add,
                        old_no: None,
                        new_no: Some(2),
                        text: "added".into(),
                    },
                ],
            }],
        }
    }

    fn state_with(files: Vec<DiffFile>, comments: Vec<ReviewComment>) -> CodeReviewState {
        let mut s = CodeReviewState {
            session_id: SessionId::default(),
            repos: vec![ReviewRepo {
                label: String::new(),
                dir: PathBuf::from("/tmp"),
                base: Some("main".into()),
            }],
            multi: false,
            files,
            comments,
            reviewed_files: HashSet::new(),
            reviewed_hunks: HashSet::new(),
            fold_override: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            compose: None,
            side_by_side: false,
            click_side: None,
            h_scroll: 0,
            wrap: false,
            target: ReviewTarget::Branch,
            commits: Vec::new(),
            host: None,
            target_picker: None,
            search: None,
        };
        s.rebuild_rows();
        s
    }

    /// A file with one change block: 1 context, 2 deletions, 2 additions —
    /// enough to exercise the paired side-by-side pairing (del[k] ↔ add[k]).
    fn change_block_file() -> DiffFile {
        DiffFile {
            path: "src/foo.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: vec![DiffHunk {
                old_start: 1,
                new_start: 1,
                header: String::new(),
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Context,
                        old_no: Some(1),
                        new_no: Some(1),
                        text: "ctx".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Del,
                        old_no: Some(2),
                        new_no: None,
                        text: "old a".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Del,
                        old_no: Some(3),
                        new_no: None,
                        text: "old b".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Add,
                        old_no: None,
                        new_no: Some(2),
                        text: "new a".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Add,
                        old_no: None,
                        new_no: Some(3),
                        text: "new b".into(),
                    },
                ],
            }],
        }
    }

    #[test]
    fn rows_flatten_file_hunk_lines_then_summary() {
        let s = state_with(vec![sample_file()], vec![]);
        assert!(matches!(s.rows[0], ReviewRow::FileHeader(0)));
        assert!(matches!(s.rows[1], ReviewRow::HunkHeader(0, 0)));
        assert!(matches!(s.rows[2], ReviewRow::Line(0, 0, 0)));
        assert!(matches!(s.rows[3], ReviewRow::Line(0, 0, 1)));
        assert!(matches!(s.rows.last(), Some(ReviewRow::SummaryHeader)));
    }

    #[test]
    fn empty_diff_shows_info_row_then_summary() {
        let s = state_with(vec![], vec![]);
        assert!(matches!(s.rows[0], ReviewRow::Info(_)));
        assert!(s.rows.iter().any(|r| matches!(r, ReviewRow::SummaryHeader)));
    }

    #[test]
    fn reviewed_file_folds_to_just_its_header() {
        let mut s = state_with(vec![sample_file()], vec![]);
        // Unreviewed: header + hunk + lines are all present.
        assert!(s
            .rows
            .iter()
            .any(|r| matches!(r, ReviewRow::HunkHeader(0, 0))));

        // Marking the file reviewed folds it — only the header survives.
        s.reviewed_files.insert("src/foo.rs".into());
        s.rebuild_rows();
        assert!(s.is_file_folded("src/foo.rs"));
        assert!(matches!(s.rows[0], ReviewRow::FileHeader(0)));
        assert!(
            !s.rows
                .iter()
                .any(|r| matches!(r, ReviewRow::HunkHeader(..) | ReviewRow::Line(..))),
            "a folded file hides its hunks and lines"
        );

        // The fold override flips it back to expanded without un-reviewing.
        s.fold_override.insert("src/foo.rs".into());
        s.rebuild_rows();
        assert!(!s.is_file_folded("src/foo.rs"));
        assert!(s
            .rows
            .iter()
            .any(|r| matches!(r, ReviewRow::HunkHeader(0, 0))));

        // And an unreviewed file can be manually folded via the override.
        let mut s2 = state_with(vec![sample_file()], vec![]);
        s2.fold_override.insert("src/foo.rs".into());
        s2.rebuild_rows();
        assert!(s2.is_file_folded("src/foo.rs"));
        assert!(!s2.rows.iter().any(|r| matches!(r, ReviewRow::Line(..))));
    }

    #[test]
    fn line_comment_is_interleaved_under_its_anchor() {
        let comment = ReviewComment {
            id: 7,
            session_id: SessionId::default(),
            anchor: CommentAnchor::Line {
                file: "src/foo.rs".into(),
                side: Side::New,
                line: 2,
            },
            classification: Classification::Issue,
            body: "bug".into(),
            created_at: 0,
            updated_at: 0,
        };
        let s = state_with(vec![sample_file()], vec![comment]);
        // The comment row sits right after the added line (new line 2).
        let line_pos = s
            .rows
            .iter()
            .position(|r| matches!(r, ReviewRow::Line(0, 0, 1)))
            .unwrap();
        assert!(matches!(s.rows[line_pos + 1], ReviewRow::Comment(7)));
    }

    /// The paired side-by-side layout collapses an aligned deletion+addition
    /// into ONE selectable row, so a change block of 2 del + 2 add (+1 context)
    /// yields 3 `Line` rows, not 5 — while the enum stays row-granular.
    #[test]
    fn side_by_side_merges_aligned_del_add_into_one_row() {
        let mut s = state_with(vec![change_block_file()], vec![]);
        let unified_lines = s
            .rows
            .iter()
            .filter(|r| matches!(r, ReviewRow::Line(..)))
            .count();
        assert_eq!(unified_lines, 5, "unified: one row per diff line");

        s.side_by_side = true;
        s.rebuild_rows();
        let paired: Vec<_> = s
            .rows
            .iter()
            .filter_map(|r| match r {
                ReviewRow::Line(_, _, li) => Some(*li),
                _ => None,
            })
            .collect();
        // Context (li 0), then del[0]↔add[0] (rep = del li 1), del[1]↔add[1]
        // (rep = del li 2). The addition lines (3, 4) fold into their pair.
        assert_eq!(paired, vec![0, 1, 2]);
    }

    /// A comment on either side of a paired row interleaves right after the
    /// shared row — the addition (New) folds into its deletion's row, so its
    /// comment still appears there.
    #[test]
    fn side_by_side_interleaves_comment_on_folded_addition() {
        // Comment anchored to the New side, new line 2 (the first addition,
        // which pairs with the first deletion).
        let comment = ReviewComment {
            id: 9,
            session_id: SessionId::default(),
            anchor: CommentAnchor::Line {
                file: "src/foo.rs".into(),
                side: Side::New,
                line: 2,
            },
            classification: Classification::Note,
            body: "look here".into(),
            created_at: 0,
            updated_at: 0,
        };
        let mut s = state_with(vec![change_block_file()], vec![comment]);
        s.side_by_side = true;
        s.rebuild_rows();
        // The paired row's representative is the deletion (li 1); the comment
        // sits on the very next row even though its anchor is the addition.
        let pos = s
            .rows
            .iter()
            .position(|r| matches!(r, ReviewRow::Line(0, 0, 1)))
            .unwrap();
        assert!(matches!(s.rows[pos + 1], ReviewRow::Comment(9)));
    }

    /// A paired change row (a deletion aligned with an addition) anchors a
    /// keyboard comment to the New side by default, and to the clicked column
    /// when a mouse click recorded one for that exact row.
    #[test]
    fn paired_row_anchor_defaults_new_and_honors_click_side() {
        let mut s = state_with(vec![change_block_file()], vec![]);
        s.side_by_side = true;
        s.rebuild_rows();
        // Select the first paired change row (rep = deletion li 1).
        s.selected = s
            .rows
            .iter()
            .position(|r| matches!(r, ReviewRow::Line(0, 0, 1)))
            .unwrap();

        // Keyboard default: the New (addition) side, new line 2.
        assert_eq!(
            s.selected_anchor(false),
            Some(CommentAnchor::Line {
                file: "src/foo.rs".into(),
                side: Side::New,
                line: 2,
            })
        );

        // A left-column click on this row steers it to the Old (deletion) side.
        s.click_side = Some((s.selected, Side::Old));
        assert_eq!(
            s.selected_anchor(false),
            Some(CommentAnchor::Line {
                file: "src/foo.rs".into(),
                side: Side::Old,
                line: 2,
            })
        );

        // A stale click side for a *different* row is ignored (falls back to
        // the New default).
        s.click_side = Some((s.selected + 999, Side::Old));
        assert!(matches!(
            s.selected_anchor(false),
            Some(CommentAnchor::Line {
                side: Side::New,
                ..
            })
        ));
    }

    /// A pure-deletion row (no aligned addition) still anchors to the Old side
    /// in the paired layout, even if a click asked for New.
    #[test]
    fn paired_deletion_only_row_anchors_old() {
        let file = DiffFile {
            path: "src/foo.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: vec![DiffHunk {
                old_start: 1,
                new_start: 1,
                header: String::new(),
                lines: vec![DiffLine {
                    kind: DiffLineKind::Del,
                    old_no: Some(1),
                    new_no: None,
                    text: "gone".into(),
                }],
            }],
        };
        let mut s = state_with(vec![file], vec![]);
        s.side_by_side = true;
        s.rebuild_rows();
        s.selected = s
            .rows
            .iter()
            .position(|r| matches!(r, ReviewRow::Line(..)))
            .unwrap();
        s.click_side = Some((s.selected, Side::New));
        assert_eq!(
            s.selected_anchor(false),
            Some(CommentAnchor::Line {
                file: "src/foo.rs".into(),
                side: Side::Old,
                line: 1,
            })
        );
    }

    fn repo(label: &str, base: &str) -> ReviewRepo {
        ReviewRepo {
            label: label.into(),
            dir: PathBuf::from("/tmp"),
            base: Some(base.into()),
        }
    }

    #[test]
    fn target_label_formats_each_variant() {
        let one = vec![repo("", "main")];
        let commits = vec![(0usize, "abc123".to_string(), "fix bug".to_string())];
        assert!(ReviewTarget::Working
            .label(&one, &commits)
            .contains("Working"));
        // Single repo shows the base..HEAD range.
        assert!(ReviewTarget::Branch
            .label(&one, &commits)
            .contains("main..HEAD"));
        assert_eq!(
            ReviewTarget::Commit {
                repo: 0,
                sha: "abc123".into()
            }
            .label(&one, &commits),
            "abc123  fix bug"
        );

        // Multi-repo: branch label counts repos; commit label is repo-prefixed.
        let two = vec![repo("web-app", "main"), repo("api", "main")];
        assert!(ReviewTarget::Branch
            .label(&two, &commits)
            .contains("2 repos"));
        assert_eq!(
            ReviewTarget::Commit {
                repo: 0,
                sha: "abc123".into()
            }
            .label(&two, &commits),
            "web-app: abc123  fix bug"
        );
    }

    #[test]
    fn build_files_combines_and_namespaces_multiple_repos() {
        // `git_program` scrubs inherited `GIT_*` vars so these temp-repo git calls
        // stay hermetic even under the project's pre-commit hook.
        use crate::git::git_program;
        fn git(dir: &std::path::Path, args: &[&str]) {
            let ok = git_program()
                .args([
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?} failed");
        }
        // A repo with a base commit on the default branch + one change on `feat`,
        // so `<base>..HEAD` has a diff. Returns the temp dir + base branch name.
        fn make_repo() -> (tempfile::TempDir, String) {
            let d = tempfile::tempdir().unwrap();
            let p = d.path();
            git(p, &["init", "-q"]);
            std::fs::write(p.join("a.txt"), "one\n").unwrap();
            git(p, &["add", "-A"]);
            git(p, &["commit", "-q", "-m", "init"]);
            let base = String::from_utf8(
                git_program()
                    .args(["symbolic-ref", "--short", "HEAD"])
                    .current_dir(p)
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_string();
            git(p, &["checkout", "-q", "-b", "feat"]);
            std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
            git(p, &["add", "-A"]);
            git(p, &["commit", "-q", "-m", "change"]);
            (d, base)
        }

        let (r1, b1) = make_repo();
        let (r2, b2) = make_repo();
        let repos = vec![
            ReviewRepo {
                label: "alpha".into(),
                dir: r1.path().to_path_buf(),
                base: Some(b1),
            },
            ReviewRepo {
                label: "beta".into(),
                dir: r2.path().to_path_buf(),
                base: Some(b2),
            },
        ];
        // Multi-repo branch diff: both repos contribute, paths namespaced by repo.
        let files = build_files(&repos, &ReviewTarget::Branch, None, true);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"alpha/a.txt"), "got {paths:?}");
        assert!(paths.contains(&"beta/a.txt"), "got {paths:?}");

        // Single-repo (multi=false) leaves paths un-prefixed.
        let single = build_files(&repos[..1], &ReviewTarget::Branch, None, false);
        assert_eq!(
            single.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["a.txt"]
        );

        // A commit target only pulls from its own repo.
        let commit_sha = {
            let out = git_program()
                .args(["rev-parse", "--short", "HEAD"])
                .current_dir(r1.path())
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let commit_files = build_files(
            &repos,
            &ReviewTarget::Commit {
                repo: 0,
                sha: commit_sha,
            },
            None,
            true,
        );
        let cpaths: Vec<&str> = commit_files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            cpaths.iter().all(|p| p.starts_with("alpha/")),
            "got {cpaths:?}"
        );
    }

    #[test]
    fn current_file_tracks_selection() {
        let mut s = state_with(vec![sample_file()], vec![]);
        s.selected = 0; // FileHeader(0)
        assert_eq!(s.current_file(), Some(0));
        // The trailing SummaryHeader belongs to no file.
        s.selected = s.rows.len() - 1;
        assert_eq!(s.current_file(), None);
    }

    fn line_comment(
        id: i64,
        file: &str,
        line: u32,
        class: Classification,
        body: &str,
    ) -> ReviewComment {
        ReviewComment {
            id,
            session_id: SessionId::default(),
            anchor: CommentAnchor::Line {
                file: file.into(),
                side: Side::New,
                line,
            },
            classification: class,
            body: body.into(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn selected_comment_id_reads_comment_and_summary_rows() {
        let s = state_with(
            vec![sample_file()],
            vec![line_comment(3, "src/foo.rs", 2, Classification::Note, "x")],
        );
        // Find the interleaved comment row and select it.
        let pos = s
            .rows
            .iter()
            .position(|r| matches!(r, ReviewRow::Comment(3)))
            .unwrap();
        let mut s = s;
        s.selected = pos;
        assert_eq!(s.selected_comment_id(), Some(3));
        // A diff line row is not a comment.
        s.selected = 2;
        assert_eq!(s.selected_comment_id(), None);
    }

    #[test]
    fn selected_file_path_and_hunk_resolve_from_any_row() {
        // Rows: [FileHeader(0), HunkHeader(0,0), Line(0,0,0), Line(0,0,1), SummaryHeader].
        let mut s = state_with(vec![sample_file()], vec![]);

        // File header → file path, no hunk.
        s.selected = 0;
        assert_eq!(s.selected_file_path().as_deref(), Some("src/foo.rs"));
        assert_eq!(s.selected_hunk_index(), None);

        // Hunk header → file path + hunk index.
        s.selected = 1;
        assert_eq!(s.selected_file_path().as_deref(), Some("src/foo.rs"));
        assert_eq!(s.selected_hunk_index(), Some(0));

        // A diff line → file path + its hunk index.
        s.selected = 2;
        assert_eq!(s.selected_file_path().as_deref(), Some("src/foo.rs"));
        assert_eq!(s.selected_hunk_index(), Some(0));

        // The summary section belongs to no file/hunk.
        s.selected = s.rows.len() - 1;
        assert_eq!(s.selected_file_path(), None);
        assert_eq!(s.selected_hunk_index(), None);
    }

    #[test]
    fn review_markdown_groups_by_file_and_omits_empty() {
        // No comments → None.
        assert!(review_markdown(&[sample_file()], &[]).is_none());

        let comments = vec![
            line_comment(1, "src/foo.rs", 2, Classification::Issue, "bug here"),
            ReviewComment {
                id: 2,
                session_id: SessionId::default(),
                anchor: CommentAnchor::Review,
                classification: Classification::Praise,
                body: "solid work".into(),
                created_at: 0,
                updated_at: 0,
            },
        ];
        let md = review_markdown(&[sample_file()], &comments).unwrap();
        assert!(md.starts_with("# Code review\n"));
        assert!(md.contains("## src/foo.rs"));
        assert!(md.contains("- **[Issue]** (new:2) bug here"));
        assert!(md.contains("## Summary"));
        assert!(md.contains("- **[Praise]** solid work"));

        // A comment on a file not in the diff is omitted (no stray header).
        let orphan = vec![line_comment(9, "gone.rs", 1, Classification::Note, "n")];
        let md = review_markdown(&[sample_file()], &orphan).unwrap();
        assert!(!md.contains("gone.rs"), "orphan file omitted: {md}");
    }

    #[test]
    fn search_matches_diff_text_paths_and_comments() {
        // sample_file: src/foo.rs with a "ctx" context line + an "added" line.
        let s = state_with(
            vec![sample_file()],
            vec![line_comment(
                1,
                "src/foo.rs",
                2,
                Classification::Note,
                "look here",
            )],
        );
        // Diff line body.
        let added: Vec<String> = s
            .search_matches("added")
            .iter()
            .filter_map(|&i| s.row_text(&s.rows[i]))
            .collect();
        assert!(added.iter().any(|t| t == "added"));
        // File path (case-insensitive).
        assert!(!s.search_matches("FOO.RS").is_empty());
        // Comment body.
        assert!(s
            .search_matches("look")
            .iter()
            .any(|&i| matches!(s.rows[i], ReviewRow::Comment(1))));
        // No matches for absent text, and an empty/whitespace query matches nothing.
        assert!(s.search_matches("zzz-nope").is_empty());
        assert!(s.search_matches("   ").is_empty());
    }

    #[test]
    fn refresh_search_matches_reanchors_after_rebuild() {
        // A reviewed file folds to just its header on rebuild, so a match that
        // lived on a now-hidden line must drop out of the refreshed match set.
        let mut s = state_with(vec![sample_file()], vec![]);
        s.search = Some(ReviewSearch {
            query: "added".to_string(),
            editing: false,
            matches: Vec::new(),
        });
        s.refresh_search_matches();
        assert_eq!(
            s.search.as_ref().unwrap().matches.len(),
            1,
            "the 'added' diff line matches while the file is expanded"
        );

        // Folding the file (mark reviewed) hides its lines; rebuild must refresh
        // the matches so none point at a vanished row.
        s.reviewed_files.insert("src/foo.rs".into());
        s.rebuild_rows();
        assert!(
            s.search.as_ref().unwrap().matches.is_empty(),
            "the hidden line no longer matches after the file folds"
        );
    }

    #[test]
    fn search_matches_are_in_row_order() {
        let s = state_with(vec![sample_file()], vec![]);
        // "ctx" and "added" both contain no shared substring; query "d" hits the
        // "added" line. Ensure indices are ascending.
        let m = s.search_matches("d");
        assert!(m.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn summary_comment_lands_in_summary_section() {
        let comment = ReviewComment {
            id: 9,
            session_id: SessionId::default(),
            anchor: CommentAnchor::Review,
            classification: Classification::Praise,
            body: "great".into(),
            created_at: 0,
            updated_at: 0,
        };
        let s = state_with(vec![sample_file()], vec![comment]);
        let hdr = s
            .rows
            .iter()
            .position(|r| matches!(r, ReviewRow::SummaryHeader))
            .unwrap();
        assert!(matches!(s.rows[hdr + 1], ReviewRow::Summary(9)));
    }
}
