//! The commands no session owns, run on a command worker: the settings file,
//! the session list's manual order
//! and the creation flow's repository memory. Sessions are Fleet's
//! ([`super::fleet`]).

use super::{BookmarkEdit, Command};

/// What a command that succeeded did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// What it was asked to do.
    Done,
}

/// Run `command` when it is one no session owns; `None` for any other.
pub(super) fn local(command: &Command) -> Option<Result<Outcome, String>> {
    let done = match command {
        Command::Bookmark { host, path, edit } => bookmark(host, path, edit),
        Command::Configure { settings } => crate::config::settings::save_settings(settings)
            .map_err(|e| format!("write settings.toml: {e}")),
        // The interface's own order (`interface.json`): Fleet keeps no order
        // of Tasks.
        Command::Order { list } => {
            crate::config::interface_state::update(|state| state.order = list.clone())
                .map_err(|e| format!("write interface.json: {e}"))
        }
        _ => return None,
    };
    Some(done.map(|()| Outcome::Done))
}

/// Remember, forget or import a repository path.
///
/// Runs here rather than in the flow because every branch of it touches the
/// filesystem: establishing whether a path is a repository, and scanning a
/// folder. The refusal of a missing path is the point — catching a typo now
/// beats failing minutes later when the session starts.
fn bookmark(host: &str, path: &str, edit: &BookmarkEdit) -> Result<(), String> {
    // Removal names a path that is already remembered — an absolute one, since
    // the rows the flow offers come from the file — so it needs neither the
    // filesystem nor the host, so a bookmark a 2.x thurbox remembered on another
    // host can still be forgotten.
    if *edit == BookmarkEdit::Remove {
        return bookmark_remove(host, path);
    }

    // `""` is this machine. A path on another Fleet host is remembered as
    // typed: thurbox reaches no other machine to check it, and the session
    // runs in that directory there.
    if !host.is_empty() {
        return match edit {
            BookmarkEdit::Add => {
                crate::kernel::bookmarks::remember(host, std::path::Path::new(path), None)
            }
            _ => Err(format!(
                "{host} is another machine; thurbox creates and imports folders on this one only"
            )),
        };
    }
    let expanded = crate::paths::expand_tilde(path);

    let fill = match edit {
        BookmarkEdit::Parent => return bookmark_parent(host, &expanded),
        BookmarkEdit::Create => crate::git::NewRepo::Empty,
        BookmarkEdit::Init => crate::git::NewRepo::Init,
        BookmarkEdit::Clone { url } => crate::git::NewRepo::Clone { url: url.clone() },
        BookmarkEdit::Add | BookmarkEdit::Remove => return bookmark_add(host, &expanded),
    };
    crate::git::create_repo_dir(&expanded, &fill).map_err(|e| format!("{e:#}"))?;
    // Remembered through the same door as a typed path, so the git-ness is
    // observed rather than assumed from which verb was asked for.
    bookmark_add(host, &expanded)
}

/// Forget a remembered path.
///
/// Removal names a path that is already remembered — an absolute one, since the
/// rows the flow offers come from the file — so it needs neither the
/// filesystem nor the host. Done BEFORE the host is resolved, which is what lets
/// a bookmark be forgotten after its host has been taken out of `hosts.toml`.
fn bookmark_remove(host: &str, path: &str) -> Result<(), String> {
    // Another host's paths were remembered as typed, so they are forgotten
    // as typed: this machine's home says nothing about that host's `~`.
    let path_here = match host.is_empty() {
        true => crate::paths::expand_tilde(path),
        false => std::path::PathBuf::from(path),
    };
    let removed = crate::kernel::bookmarks::forget(host, &path_here)?;
    match removed {
        true => Ok(()),
        false => Err(format!("not a remembered repository: {path}")),
    }
}

/// Import a folder of repositories: remember the folder, then its members.
///
/// The members are not fixed here: the folder is re-scanned on every read of
/// the bookmark list, so this is the first scan rather than the only one —
/// nobody has to re-import a folder to see a repository they cloned into it.
fn bookmark_parent(host: &str, expanded: &std::path::Path) -> Result<(), String> {
    if !expanded.is_dir() {
        return Err(format!("Path not found: {}", expanded.display()));
    }
    let children = crate::git::scan_child_repos(expanded);
    crate::kernel::bookmarks::remember_folder(host, expanded)?;
    // Replace rather than merge: these members are a scan, and a scan is the
    // whole truth about the folder, so a repository that has since been deleted
    // must stop being offered.
    crate::kernel::bookmarks::replace_members(host, expanded, &children)?;
    if children.is_empty() {
        // Not a failure — the folder is remembered — but the user asked a
        // question and "none" is the answer, so it is reported rather than
        // looking like an import that silently did nothing.
        return Err(format!(
            "No repositories found under {}",
            expanded.display()
        ));
    }
    Ok(())
}

/// Remember one path: establish what it is, refuse what is not there, and record
/// the git-ness so the flow knows whether worktree mode is even possible.
fn bookmark_add(host: &str, expanded: &std::path::Path) -> Result<(), String> {
    if !expanded.is_dir() {
        return Err(format!("Path not found: {}", expanded.display()));
    }
    let is_git = Some(crate::git::is_git_repo(expanded));

    // Touches recency as well as adding, which is what lets the flow re-select a
    // path that was already remembered: the row it asked for is the newest one.
    crate::kernel::bookmarks::remember(host, expanded, is_git)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path remembered for another Fleet host is kept as typed, so it is
    /// forgotten as typed: expanding its `~` against this machine's home
    /// would name a path that was never remembered.
    #[test]
    fn a_path_remembered_for_another_host_is_forgotten_as_typed() {
        bookmark("devbox", "~/repo", &BookmarkEdit::Add).expect("remembered");
        bookmark("devbox", "~/repo", &BookmarkEdit::Remove).expect("forgotten");
        assert!(crate::kernel::bookmarks::list("devbox").is_empty());
    }
}
