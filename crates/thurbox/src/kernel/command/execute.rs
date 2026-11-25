use super::{BookmarkEdit, Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    Done,
}

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

    #[test]
    fn a_path_remembered_for_another_host_is_forgotten_as_typed() {
        bookmark("devbox", "~/repo", &BookmarkEdit::Add).expect("remembered");
        bookmark("devbox", "~/repo", &BookmarkEdit::Remove).expect("forgotten");
        assert!(crate::kernel::bookmarks::list("devbox").is_empty());
    }
}
