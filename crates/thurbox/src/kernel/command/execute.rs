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
    /// Nothing, and rightly: a repair Fleet makes itself. The interface
    /// announces it as neither a success nor a failure.
    Nothing,
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
/// world: expanding a `~` on an ssh host is a round trip, so is establishing
/// whether a path is a repository, and scanning a folder is another. The
/// refusal of a missing path is the point — catching a typo now beats failing
/// minutes later inside `git worktree add`.
fn bookmark(host: &str, path: &str, edit: &BookmarkEdit) -> Result<(), String> {
    // Removal names a path that is already remembered — an absolute one, since
    // the rows the flow offers come from the file — so it needs neither the
    // filesystem nor the host. Done BEFORE the host is resolved, which is what
    // lets a bookmark be forgotten after its host has been taken out of
    // `hosts.toml`.
    if *edit == BookmarkEdit::Remove {
        return bookmark_remove(host, path);
    }

    // `""` is local; the flow's key is the same string the bookmarks file
    // stores, so nothing is translated here.
    let remote = match host.is_empty() {
        true => None,
        false => {
            let (registry, _warnings) = crate::config::hosts::cached_registry();
            Some(
                registry
                    .resolve(host)
                    .cloned()
                    .ok_or_else(|| format!("no such host: {host}"))?,
            )
        }
    };

    let expanded = match remote.as_ref() {
        Some(host) => std::path::PathBuf::from(
            crate::git::expand_remote_tilde(host, path).map_err(|e| format!("{e:#}"))?,
        ),
        None => crate::paths::expand_tilde(path),
    };

    let fill = match edit {
        BookmarkEdit::Parent => return bookmark_parent(host, remote.as_ref(), &expanded),
        BookmarkEdit::Create => crate::git::NewRepo::Empty,
        BookmarkEdit::Init => crate::git::NewRepo::Init,
        BookmarkEdit::Clone { url } => crate::git::NewRepo::Clone { url: url.clone() },
        BookmarkEdit::Add | BookmarkEdit::Remove => {
            return bookmark_add(host, remote.as_ref(), &expanded)
        }
    };
    crate::git::create_repo_dir(remote.as_ref(), &expanded, &fill).map_err(|e| format!("{e:#}"))?;
    // Remembered through the same door as a typed path, so the git-ness is
    // observed rather than assumed from which verb was asked for.
    bookmark_add(host, remote.as_ref(), &expanded)
}

/// Forget a remembered path.
///
/// Removal names a path that is already remembered — an absolute one, since the
/// rows the flow offers come from the file — so it needs neither the
/// filesystem nor the host. Done BEFORE the host is resolved, which is what lets
/// a bookmark be forgotten after its host has been taken out of `hosts.toml`.
fn bookmark_remove(host: &str, path: &str) -> Result<(), String> {
    let removed = crate::kernel::bookmarks::forget(host, &crate::paths::expand_tilde(path))?;
    match removed {
        true => Ok(()),
        false => Err(format!("not a remembered repository: {path}")),
    }
}

/// Import a folder of repositories: remember the folder, then its members.
///
/// The members are not fixed here. A local folder is re-scanned on every read of
/// the bookmark list and a remote one on its own interval, so this is the first
/// scan rather than the only one — nobody has to re-import a folder to see a
/// repository they cloned into it.
fn bookmark_parent(
    host: &str,
    remote: Option<&crate::session::HostDef>,
    expanded: &std::path::Path,
) -> Result<(), String> {
    let children = match remote {
        Some(host) => crate::git::scan_child_repos_on(host, &expanded.to_string_lossy())
            .map_err(|e| format!("{e:#}"))?,
        None => {
            if !expanded.is_dir() {
                return Err(format!("Path not found: {}", expanded.display()));
            }
            crate::git::scan_child_repos(expanded)
        }
    };
    crate::kernel::bookmarks::remember_folder(host, expanded)?;
    // Replace rather than merge: these members are a scan, and a scan is the
    // whole truth about the folder, so a repository that has since been deleted
    // must stop being offered. The same write is what a rescan makes later
    // (`kernel::repos::rescan_folder`) — importing is only the first one.
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
fn bookmark_add(
    host: &str,
    remote: Option<&crate::session::HostDef>,
    expanded: &std::path::Path,
) -> Result<(), String> {
    let is_git = match remote {
        Some(host) => match crate::git::classify_path_on(host, &expanded.to_string_lossy())
            .map_err(|e| format!("{e:#}"))?
        {
            crate::git::PathClass::Git => Some(true),
            crate::git::PathClass::Dir => Some(false),
            crate::git::PathClass::Missing => {
                return Err(format!(
                    "Path not found on '{}': {}",
                    host.name,
                    expanded.display()
                ))
            }
        },
        None => {
            if !expanded.is_dir() {
                return Err(format!("Path not found: {}", expanded.display()));
            }
            Some(crate::git::is_git_repo(expanded))
        }
    };

    // Touches recency as well as adding, which is what lets the flow re-select a
    // path that was already remembered: the row it asked for is the newest one.
    crate::kernel::bookmarks::remember(host, expanded, is_git)
}
