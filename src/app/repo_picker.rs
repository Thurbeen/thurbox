//! Repo-picker path entry: the path-browser dropdown, Enter-commit
//! validation, and parent import — local inline, remote via workers.
//!
//! Remote (`ssh:`/`wsl:`) targets never touch the UI thread with a round
//! trip: listings, path checks, and parent scans run on `spawn_blocking`
//! workers (the `branch_list` pattern, ADR-P12) and land through
//! [`App::poll_repo_picker`] each tick. Supersession is two-layered: starting
//! a [`BackgroundTask`](super::background::BackgroundTask) over an in-flight
//! job orphans the old receiver (its send fails harmlessly), and every result
//! carries the `repo_picker_gen` stamp of the picker instance that requested
//! it, so a result outliving its modal (Esc + reopen) is dropped on arrival.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};
use tracing::error;

use super::background::TaskPoll;
use super::modals::{BrowseEntry, Modal, RepoPickerFocus, RepoPickerModal};
use super::{App, StatusLevel};
use crate::paths;

/// A finished directory listing for the path browser.
pub(crate) struct DirListingResult {
    pub gen: u64,
    /// The browsed dir exactly as typed — the `dir_cache` key and the match
    /// against the browser's current dir.
    pub dir: String,
    pub result: Result<Vec<BrowseEntry>, String>,
}

/// A finished Enter-commit validation of a typed remote path.
pub(crate) struct PathCheckResult {
    pub gen: u64,
    /// What was typed (matched against `pending_commit`).
    pub raw: String,
    /// The tilde-expanded path and what it turned out to be.
    pub result: Result<(PathBuf, crate::git::PathClass), String>,
}

/// A finished remote parent scan (Ctrl+P import).
pub(crate) struct ParentImportResult {
    pub gen: u64,
    /// The expanded parent folder and its child git repos.
    pub result: Result<(PathBuf, Vec<PathBuf>), String>,
}

/// Split the typed path into `(dir to browse, prefix to filter by)`. The dir
/// is everything up to the last `/` (kept in its typed, tilde-unexpanded
/// form); with no `/` at all the home dir is browsed, filtered by the whole
/// input.
pub(crate) fn split_browse_dir(input: &str) -> (String, String) {
    let input = input.trim();
    if input == "~" {
        // A bare `~` means "browse home", not "filter home by the literal ~".
        return ("~".to_string(), String::new());
    }
    match input.rsplit_once('/') {
        Some(("", prefix)) => ("/".to_string(), prefix.to_string()),
        Some((dir, prefix)) => (dir.to_string(), prefix.to_string()),
        None => ("~".to_string(), input.to_string()),
    }
}

/// Join a browsed dir and a selected entry into the descended input value
/// (trailing `/` so the next listing targets the entry itself).
fn join_browse_dir(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}/")
    } else {
        format!("{dir}/{name}/")
    }
}

/// Convert a [`crate::git::DirListing`] into browser entries, mapping
/// `Missing` to a user-facing error.
fn listing_to_entries(
    dir: &str,
    listing: crate::git::DirListing,
) -> Result<Vec<BrowseEntry>, String> {
    match listing {
        crate::git::DirListing::Missing => Err(format!("No such directory: {dir}")),
        crate::git::DirListing::Entries(entries) => Ok(entries
            .into_iter()
            .map(|(name, is_git)| BrowseEntry { name, is_git })
            .collect()),
    }
}

impl App {
    /// The new-session wizard's target host (`None` = local), owned so a
    /// worker can take it.
    fn repo_picker_target_host(&self) -> Option<crate::session::HostDef> {
        self.host_for_backend(self.new_session.backend.as_deref())
            .cloned()
    }

    /// Open the path-browser dropdown for the current input (Tab with nothing
    /// to inline-complete).
    pub(crate) fn repo_picker_open_browser(&mut self) {
        let Modal::RepoPicker(ref rp) = self.modal else {
            return;
        };
        let (dir, prefix) = split_browse_dir(rp.path_input.value());
        self.repo_picker_fetch_listing(dir, prefix, false);
    }

    /// Keep an open dropdown in sync after an input edit: a changed dir
    /// component refetches (cache-first), a changed prefix just re-filters.
    pub(crate) fn repo_picker_sync_browser(&mut self) {
        let Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        if !rp.browser.open {
            return;
        }
        let (dir, prefix) = split_browse_dir(rp.path_input.value());
        if dir == rp.browser.dir {
            rp.browser.recompute_filter(&prefix);
        } else {
            self.repo_picker_fetch_listing(dir, prefix, false);
        }
    }

    /// Point the browser at `dir` and populate it: from the modal-lifetime
    /// cache, inline for a local target, or via a worker for a remote one
    /// (spinner meanwhile). `bypass_cache` forces a refetch (explicit Tab
    /// refresh inside the browser).
    fn repo_picker_fetch_listing(&mut self, dir: String, prefix: String, bypass_cache: bool) {
        let host = self.repo_picker_target_host();
        let Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        rp.browser.open = true;
        rp.browser.dir = dir.clone();
        rp.browser.error = None;

        if !bypass_cache {
            if let Some(cached) = rp.dir_cache.get(&dir) {
                rp.browser.listing = cached.clone();
                rp.browser.loading = false;
                rp.browser.filtered.clear();
                rp.browser.recompute_filter(&prefix);
                return;
            }
        }

        let Some(host) = host else {
            // Local: std::fs is instant, and the acceptance harness runs
            // without a Tokio runtime — no worker.
            let result = crate::git::list_dir_entries_on(None, &dir)
                .map_err(|e| format!("{e:#}"))
                .and_then(|listing| listing_to_entries(&dir, listing));
            self.apply_repo_dir_listing(dir, result);
            return;
        };

        rp.browser.loading = true;
        rp.browser.listing.clear();
        rp.browser.filtered.clear();
        rp.browser.index = 0;
        let gen = self.repo_picker_gen;
        let tx = self.repo_dir_listing.start();
        tokio::task::spawn_blocking(move || {
            let result = crate::git::list_dir_entries_on(Some(&host), &dir)
                .map_err(|e| format!("{e:#}"))
                .and_then(|listing| listing_to_entries(&dir, listing));
            let _ = tx.send(DirListingResult { gen, dir, result });
        });
    }

    /// Land a listing in the browser (shared by the local inline path and the
    /// worker poll). A listing for a dir the browser has moved away from is
    /// still cached — the user may Backspace right back — but not displayed.
    fn apply_repo_dir_listing(&mut self, dir: String, result: Result<Vec<BrowseEntry>, String>) {
        let Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let current = rp.browser.open && rp.browser.dir == dir;
        match result {
            Ok(entries) => {
                rp.dir_cache.insert(dir, entries.clone());
                if current {
                    rp.browser.loading = false;
                    rp.browser.listing = entries;
                    rp.browser.filtered.clear();
                    let (_, prefix) = split_browse_dir(rp.path_input.value());
                    rp.browser.recompute_filter(&prefix);
                }
            }
            Err(e) => {
                if current {
                    rp.browser.loading = false;
                    rp.browser.error = Some(e);
                    rp.browser.listing.clear();
                    rp.browser.filtered.clear();
                }
            }
        }
    }

    /// Handle a key while the browser dropdown is open. Returns whether the
    /// key was consumed; editing keys fall through to the input (the caller
    /// then runs [`Self::repo_picker_sync_browser`]).
    pub(crate) fn handle_repo_browser_key(&mut self, code: KeyCode, _mods: KeyModifiers) -> bool {
        let Modal::RepoPicker(ref mut rp) = self.modal else {
            return false;
        };
        match code {
            // Esc closes just the dropdown (a second Esc closes the modal);
            // BackTab additionally hands focus to the bookmark list.
            KeyCode::Esc => {
                rp.browser.close();
                true
            }
            KeyCode::BackTab => {
                rp.browser.close();
                rp.focus = RepoPickerFocus::List;
                true
            }
            KeyCode::Up => {
                rp.browser.index = rp.browser.index.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                if rp.browser.index + 1 < rp.browser.filtered.len() {
                    rp.browser.index += 1;
                }
                true
            }
            // Tab inside the open browser is an explicit refresh of the
            // current dir (bypasses the modal-lifetime cache).
            KeyCode::Tab => {
                let (dir, prefix) = split_browse_dir(rp.path_input.value());
                self.repo_picker_fetch_listing(dir, prefix, true);
                true
            }
            KeyCode::Enter => {
                let Some(entry) = rp.browser.selected().cloned() else {
                    // Nothing to act on (empty/missing dir): swallow so a
                    // stray Enter doesn't commit a half-typed path.
                    return true;
                };
                let dir = rp.browser.dir.clone();
                if entry.is_git {
                    // Existence + git-ness were just observed in the listing —
                    // commit directly, skipping the validation round trip.
                    let raw = join_browse_dir(&dir, &entry.name)
                        .trim_end_matches('/')
                        .to_string();
                    self.repo_picker_start_commit(raw, Some(crate::git::PathClass::Git));
                } else {
                    // Descend into the plain dir: update the input and browse it.
                    let value = join_browse_dir(&dir, &entry.name);
                    rp.path_input.set(&value);
                    // The rewritten input invalidates any inline suggestion
                    // computed for the old text (local targets only).
                    self.update_repo_picker_path_suggestion();
                    let (dir, prefix) = split_browse_dir(&value);
                    self.repo_picker_fetch_listing(dir, prefix, false);
                }
                true
            }
            _ => false,
        }
    }

    /// Commit the typed path in the repo-picker input: add or re-select the
    /// bookmark, persist it (scoped to the target host), clear the input, and
    /// refresh the filter. A remote path is validated on a worker (exists +
    /// git-ness in one round trip) with a "checking…" state; a local path is
    /// checked inline. A missing path is refused with the input preserved —
    /// catching a typo here beats failing minutes later at branch listing or
    /// worktree creation.
    pub(crate) fn repo_picker_commit_path_input(&mut self) {
        let Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let path = rp.path_input.value().trim().to_string();
        if path.is_empty() {
            self.recompute_repo_filter();
            return;
        }
        self.repo_picker_start_commit(path, None);
    }

    /// Start committing `raw` as a repo path. `known` short-circuits the
    /// classification when the browser just observed it (Enter on a listed
    /// git repo) — the worker then only expands the tilde.
    fn repo_picker_start_commit(&mut self, raw: String, known: Option<crate::git::PathClass>) {
        let Some(host) = self.repo_picker_target_host() else {
            // Local: classification is a cheap stat, done inline.
            let expanded = paths::expand_tilde(&raw);
            if !expanded.is_dir() {
                self.set_error(format!("Path not found: {}", expanded.display()));
                return;
            }
            let is_git = crate::git::is_git_repo(&expanded);
            self.repo_picker_finish_commit(expanded, Some(is_git));
            return;
        };

        if let Modal::RepoPicker(ref mut rp) = self.modal {
            rp.pending_commit = Some(raw.clone());
        }
        let gen = self.repo_picker_gen;
        let tx = self.repo_path_check.start();
        tokio::task::spawn_blocking(move || {
            let result = (|| {
                let expanded = crate::git::expand_remote_tilde(&host, &raw)?;
                let class = match known {
                    Some(class) => class,
                    None => crate::git::classify_path_on(&host, &expanded)?,
                };
                anyhow::Ok((PathBuf::from(expanded), class))
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(PathCheckResult { gen, raw, result });
        });
    }

    /// Apply a validated commit: select-or-add the row, persist the bookmark
    /// with its git-ness, and reset the input/browser.
    fn repo_picker_finish_commit(&mut self, expanded: PathBuf, is_git: Option<bool>) {
        let Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let persist = Self::repo_picker_select_or_add_row(rp, &expanded, is_git);
        if persist {
            if let Err(e) =
                self.db
                    .upsert_repo_bookmark_checked(self.bookmark_host_key(), &expanded, is_git)
            {
                error!("Failed to save repo bookmark: {e}");
                self.set_error(format!("Failed to save repo bookmark: {e}"));
            }
        }
        let Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        rp.path_input.clear();
        rp.path_suggestion = None;
        rp.pending_commit = None;
        rp.browser.close();
        self.recompute_repo_filter();
    }

    /// Select an already-represented bookmark row for `expanded` (upgrading
    /// its git-ness when newly learned), or push a new auto-selected row.
    /// Returns whether the path should be persisted as a standalone bookmark:
    /// a path already shown as a parent's child (or the parent header itself)
    /// is already covered, so it is not re-persisted.
    fn repo_picker_select_or_add_row(
        rp: &mut RepoPickerModal,
        expanded: &std::path::Path,
        is_git: Option<bool>,
    ) -> bool {
        // If already represented, just select it (no duplicate row or DB entry).
        let Some(idx) = rp.bookmarks.iter().position(|p| p == expanded) else {
            rp.push_row(expanded.to_path_buf(), true, false, false, is_git);
            return true;
        };
        let is_child = rp.is_child_row(idx);
        let is_header = rp.is_header_row(idx);
        if !is_header {
            rp.selected[idx] = true;
            rp.is_git[idx] = rp.is_git[idx].or(is_git);
        }
        !is_child && !is_header
    }

    /// Import the typed path as a *parent* folder: persist it as a parent
    /// bookmark and list its child git repos as an indented group. Local
    /// targets scan inline (children stay ephemeral, re-scanned per open);
    /// remote targets scan on a worker and **persist** the children
    /// (`parent_path` rows), refreshed by re-running the import.
    pub(crate) fn repo_picker_import_parent(&mut self) {
        let remote_host = self.repo_picker_target_host();
        let Modal::RepoPicker(ref mut rp) = self.modal else {
            return;
        };
        let path = rp.path_input.value().trim().to_string();
        if path.is_empty() {
            self.set_status(
                StatusLevel::Info,
                "Type a folder path, then Ctrl+P to import its repos as a parent",
            );
            return;
        }

        let Some(host) = remote_host else {
            let expanded = paths::expand_tilde(&path);
            // Equivalent to a literal "" (this branch is local), but keeps the
            // `"" = local` encoding owned by `bookmark_host_key` alone.
            let host_key = self.bookmark_host_key().to_string();
            if let Err(e) = self
                .db
                .upsert_repo_bookmark_kind(&host_key, &expanded, true)
            {
                error!("Failed to save parent bookmark: {e}");
                self.set_error(format!("Failed to save parent bookmark: {e}"));
            }
            self.repo_picker_reset_input_to_list();
            self.refresh_repo_picker_rows();
            return;
        };

        if self.repo_parent_import.in_progress() {
            self.set_status(StatusLevel::Info, "Still scanning — one import at a time");
            return;
        }
        self.set_status(
            StatusLevel::Info,
            format!("Scanning {path} on '{}'…", host.name),
        );
        let gen = self.repo_picker_gen;
        let tx = self.repo_parent_import.start();
        tokio::task::spawn_blocking(move || {
            let result = (|| {
                let expanded = crate::git::expand_remote_tilde(&host, &path)?;
                let children = crate::git::scan_child_repos_on(&host, &expanded)?;
                anyhow::Ok((PathBuf::from(expanded), children))
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(ParentImportResult { gen, result });
        });
    }

    /// Clear the path input (+ suggestion/browser) and focus the list — the
    /// tail shared by both parent-import branches.
    fn repo_picker_reset_input_to_list(&mut self) {
        if let Modal::RepoPicker(ref mut rp) = self.modal {
            rp.path_input.clear();
            rp.path_suggestion = None;
            rp.browser.close();
            rp.focus = RepoPickerFocus::List;
        }
    }

    /// Drain the repo picker's async results (called from `tick_core`). Every
    /// arm guards on the generation stamp *and* the modal still being the repo
    /// picker, so a result outliving its picker instance is dropped — including
    /// a parent import's DB writes (mutating bookmarks after the user Esc'd
    /// would violate least surprise).
    pub(crate) fn poll_repo_picker(&mut self) {
        self.poll_repo_dir_listing();
        self.poll_repo_path_check();
        self.poll_repo_parent_import();
    }

    fn repo_picker_result_is_live(&self, gen: u64) -> bool {
        gen == self.repo_picker_gen && matches!(self.modal, Modal::RepoPicker(_))
    }

    fn poll_repo_dir_listing(&mut self) {
        match self.repo_dir_listing.poll() {
            TaskPoll::Pending => {}
            TaskPoll::Done(res) => {
                if self.repo_picker_result_is_live(res.gen) {
                    self.apply_repo_dir_listing(res.dir, res.result);
                    self.request_redraw();
                }
            }
            TaskPoll::Died => {
                if let Modal::RepoPicker(ref mut rp) = self.modal {
                    if rp.browser.open {
                        rp.browser.loading = false;
                        rp.browser.error = Some("Listing failed (worker died)".to_string());
                        self.request_redraw();
                    }
                }
            }
        }
    }

    fn poll_repo_path_check(&mut self) {
        match self.repo_path_check.poll() {
            TaskPoll::Pending => {}
            TaskPoll::Done(res) => {
                if !self.repo_picker_result_is_live(res.gen) {
                    return;
                }
                // Only the *latest* commit attempt applies; a superseded
                // worker's receiver was already orphaned, but belt-and-braces.
                if let Modal::RepoPicker(ref mut rp) = self.modal {
                    if rp.pending_commit.as_deref() != Some(res.raw.as_str()) {
                        return;
                    }
                    // Every arm below ends the attempt (finish_commit resets
                    // the whole input; the failure arms keep the typed text
                    // so the typo can be fixed in place).
                    rp.pending_commit = None;
                }
                match res.result {
                    Ok((expanded, crate::git::PathClass::Git)) => {
                        self.repo_picker_finish_commit(expanded, Some(true));
                    }
                    Ok((expanded, crate::git::PathClass::Dir)) => {
                        // A plain dir stays selectable (an --add-dir style
                        // member); only the worktree toggle is gated on it.
                        self.repo_picker_finish_commit(expanded, Some(false));
                    }
                    Ok((expanded, crate::git::PathClass::Missing)) => {
                        let host = self.repo_picker_host_name();
                        self.set_error(format!(
                            "Path not found on '{host}': {}",
                            expanded.display()
                        ));
                    }
                    Err(e) => {
                        self.set_error(format!("Path check failed: {e}"));
                    }
                }
                self.request_redraw();
            }
            TaskPoll::Died => {
                if let Modal::RepoPicker(ref mut rp) = self.modal {
                    if rp.pending_commit.take().is_some() {
                        self.set_error("Path check failed (worker died)");
                        self.request_redraw();
                    }
                }
            }
        }
    }

    fn poll_repo_parent_import(&mut self) {
        match self.repo_parent_import.poll() {
            TaskPoll::Pending => {}
            TaskPoll::Done(res) => {
                if !self.repo_picker_result_is_live(res.gen) {
                    return;
                }
                match res.result {
                    Ok((parent, children)) => self.apply_parent_import(parent, children),
                    Err(e) => self.set_error(format!("Parent import failed: {e}")),
                }
                self.request_redraw();
            }
            TaskPoll::Died => {
                if matches!(self.modal, Modal::RepoPicker(_)) {
                    self.set_error("Parent import failed (worker died)");
                    self.request_redraw();
                }
            }
        }
    }

    /// Land a successful parent scan: persist the parent + its children,
    /// report the count, and rebuild the picker rows.
    fn apply_parent_import(&mut self, parent: PathBuf, children: Vec<PathBuf>) {
        let host_key = self.bookmark_host_key().to_string();
        if let Err(e) = self
            .db
            .upsert_repo_bookmark_kind(&host_key, &parent, true)
            .and_then(|()| {
                self.db
                    .replace_parent_children(&host_key, &parent, &children)
            })
        {
            error!("Failed to save parent import: {e}");
            self.set_error(format!("Failed to save parent import: {e}"));
            return;
        }
        let message = match children.len() {
            0 => format!("No git repos found under {}", parent.display()),
            n => format!("Imported {n} repos under {}", parent.display()),
        };
        self.set_status(StatusLevel::Info, message);
        self.repo_picker_reset_input_to_list();
        self.refresh_repo_picker_rows();
    }

    /// The target host's display name for error messages (the wizard is
    /// remote whenever these paths run).
    fn repo_picker_host_name(&self) -> String {
        self.host_for_backend(self.new_session.backend.as_deref())
            .map(|h| h.name.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_browse_dir_cases() {
        // (input, expected dir, expected prefix)
        let cases = [
            ("", "~", ""),
            ("thur", "~", "thur"),
            ("~", "~", ""),
            ("~/", "~", ""),
            ("~/pro", "~", "pro"),
            ("~/projects/", "~/projects", ""),
            ("~/projects/thur", "~/projects", "thur"),
            ("/", "/", ""),
            ("/srv", "/", "srv"),
            ("/srv/repos/", "/srv/repos", ""),
            ("  /srv/x  ", "/srv", "x"),
        ];
        for (input, dir, prefix) in cases {
            assert_eq!(
                split_browse_dir(input),
                (dir.to_string(), prefix.to_string()),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn join_browse_dir_handles_root() {
        assert_eq!(join_browse_dir("/", "srv"), "/srv/");
        assert_eq!(
            join_browse_dir("~/projects", "thurbox"),
            "~/projects/thurbox/"
        );
    }
}
