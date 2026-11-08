//! Watches the plugin directory and reports that something changed.
//!
//! Deliberately dumb: it answers "has anything changed since you last asked?"
//! and the caller decides when to act. Editors save in bursts — write, then
//! rename, then chmod — so the caller debounces rather than reloading three
//! times per keystroke.

use std::path::Path;
use std::sync::mpsc::{channel, Receiver};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

pub struct Watcher {
    /// Held so the watcher thread stays alive; never read.
    _watcher: RecommendedWatcher,
    events: Receiver<notify::Result<Event>>,
}

impl Watcher {
    pub fn new(dir: &Path) -> notify::Result<Self> {
        let (tx, events) = channel();
        let mut watcher = notify::recommended_watcher(tx)?;
        // Canonicalized first: a relative path is resolved against the process
        // cwd by the backend, and the events it then reports do not reliably
        // match — so a relative `ui` watches successfully but never fires.
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        watcher.watch(&dir, RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: watcher,
            events,
        })
    }

    /// Drain the queue and report whether anything happened.
    ///
    /// Drains fully rather than taking one event, so a burst collapses into a
    /// single answer and the caller's debounce window starts from the last
    /// event rather than the first.
    pub fn changed(&self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            // A watch error still means "look again" — the alternative is
            // silently going stale.
            match event {
                Ok(event) if is_noise(&event) => {}
                _ => changed = true,
            }
        }
        changed
    }
}

/// Events that never affect what is loaded.
///
/// An event carrying no paths is *not* noise: it is a backend telling us it
/// lost track (an overflow or rescan), which is exactly when a reload matters
/// most. `all()` on an empty iterator is `true`, so that case is handled first.
fn is_noise(event: &Event) -> bool {
    // Reading a file is not changing it. inotify reports plain reads as access
    // events, so without this the host reading its own plugins looks like an
    // edit — and a reader that runs every frame keeps the debounce window
    // rolling forward forever, meaning the reload never actually fires.
    if matches!(event.kind, EventKind::Access(_)) {
        return true;
    }
    if event.paths.is_empty() {
        return false;
    }
    event.paths.iter().all(|path| {
        // Version-control bookkeeping is not an interface edit. The directory is
        // watched RECURSIVELY, so without this every `git` operation anywhere under
        // it — an index lock, a ref update, a pack — looks like an edit.
        //
        // This is not only for installed plugins that arrive as a clone: putting
        // your own interface under version control is a reasonable thing to do, and
        // it currently makes your own commits fight the reload. And the symptom is
        // the surprising one this function's other carve-out documents — a burst of
        // events keeps the debounce window rolling forward, so the reload does not
        // fire *more*, it stops firing at all while git is busy.
        if path
            .components()
            .any(|part| part.as_os_str() == std::ffi::OsStr::new(".git"))
        {
            return true;
        }
        match path.file_name().and_then(|name| name.to_str()) {
            // Editor scratch files: emacs' `.#name`, backups, gio's temp names.
            Some(name) => {
                name.ends_with('~') || name.starts_with(".#") || name.starts_with(".goutputstream")
            }
            None => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, ModifyKind};
    use std::path::PathBuf;

    fn event(paths: &[&str]) -> Event {
        Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        }
    }

    #[test]
    fn a_plugin_edit_is_not_noise() {
        assert!(!is_noise(&event(&["/ui/plugins/session_list.lua"])));
    }

    #[test]
    fn editor_scratch_files_are_noise() {
        assert!(is_noise(&event(&["/ui/plugins/session_list.lua~"])));
        assert!(is_noise(&event(&["/ui/plugins/.#session_list.lua"])));
    }

    /// Version-control bookkeeping is not an interface edit.
    ///
    /// The directory is watched recursively, so every `git` operation under it would
    /// otherwise read as an edit. Two callers depend on this: a plugin installed as
    /// a clone, and a user who versions their own panes — and the second is why this
    /// is a fix rather than a concession.
    #[test]
    fn version_control_bookkeeping_is_noise() {
        for path in [
            "/ui/.git/index.lock",
            "/ui/.git/refs/heads/main",
            "/ui/.git/objects/pack/pack-abc.pack",
            // A clone belonging to an installed plugin, nested deeper.
            "/ui/doom/.git/FETCH_HEAD",
        ] {
            assert!(is_noise(&event(&[path])), "{path}");
        }
    }

    #[test]
    fn a_file_merely_named_like_git_is_not_noise() {
        // The test is a path COMPONENT, not a substring: a pane may legitimately be
        // called `.gitignore.lua`, and a directory `mygit/` is not `.git/`.
        assert!(!is_noise(&event(&["/ui/plugins/.gitignore.lua"])));
        assert!(!is_noise(&event(&["/ui/mygit/plugins/a.lua"])));
        assert!(!is_noise(&event(&["/ui/lib/.gitkeep.lua"])));
    }

    #[test]
    fn a_real_edit_beside_git_churn_still_wakes_it() {
        // `all` over the batch: a commit that also touched a plugin must not be
        // swallowed because most of its paths were bookkeeping.
        assert!(!is_noise(&event(&[
            "/ui/.git/index",
            "/ui/plugins/session_list.lua",
        ])));
    }

    #[test]
    fn reading_a_file_is_not_a_change() {
        // The bug this guards: the host reads layout.lua every frame, inotify
        // reports each read as an access event, and treating those as edits
        // rolls the debounce window forward forever so the reload never fires.
        let read = Event {
            kind: EventKind::Access(AccessKind::Read),
            paths: vec![PathBuf::from("/ui/layout.lua")],
            attrs: Default::default(),
        };
        assert!(is_noise(&read));
    }

    #[test]
    fn a_mixed_batch_is_not_noise() {
        // One real edit among scratch files still has to trigger a reload.
        assert!(!is_noise(&event(&[
            "/ui/plugins/a.lua~",
            "/ui/plugins/a.lua"
        ])));
    }

    #[test]
    fn a_watcher_on_a_real_directory_starts_quiet() {
        let dir = tempfile::tempdir().expect("tempdir");
        let watcher = Watcher::new(dir.path()).expect("watcher starts");
        assert!(!watcher.changed(), "nothing has happened yet");
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Does the watcher actually observe an edit to a nested file?
    #[test]
    fn an_edit_to_a_nested_file_is_observed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = dir.path().join("lib");
        std::fs::create_dir_all(&lib).expect("mkdir");
        let file = lib.join("theme.lua");
        std::fs::write(&file, "return {}").expect("seed");

        let watcher = Watcher::new(dir.path()).expect("watcher");
        std::thread::sleep(Duration::from_millis(150));
        let _ = watcher.changed();

        std::fs::write(&file, "return { changed = true }").expect("edit");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saw = false;
        while Instant::now() < deadline {
            if watcher.changed() {
                saw = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(saw, "the watcher must observe an edit to lib/theme.lua");
    }
}
