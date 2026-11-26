use std::path::Path;
use std::sync::mpsc::{channel, Receiver};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

pub struct Watcher {
    _watcher: RecommendedWatcher,
    events: Receiver<notify::Result<Event>>,
}

impl Watcher {
    pub fn new(dir: &Path) -> notify::Result<Self> {
        let (tx, events) = channel();
        let mut watcher = notify::recommended_watcher(tx)?;
        // notify resolves a relative path against the cwd, and its events then fail to match.
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        watcher.watch(&dir, RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: watcher,
            events,
        })
    }

    pub fn changed(&self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            match event {
                Ok(event) if is_noise(&event) => {}
                _ => changed = true,
            }
        }
        changed
    }
}

fn is_noise(event: &Event) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return true;
    }
    if event.paths.is_empty() {
        return false;
    }
    event.paths.iter().all(|path| {
        // The watch is recursive, so every git operation under it (index lock, ref update, pack) emits events.
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
    fn a_mod_edit_is_not_noise() {
        assert!(!is_noise(&event(&["/ui/mods/session_list.lua"])));
    }

    #[test]
    fn editor_scratch_files_are_noise() {
        assert!(is_noise(&event(&["/ui/mods/session_list.lua~"])));
        assert!(is_noise(&event(&["/ui/mods/.#session_list.lua"])));
    }

    #[test]
    fn version_control_bookkeeping_is_noise() {
        for path in [
            "/ui/.git/index.lock",
            "/ui/.git/refs/heads/main",
            "/ui/.git/objects/pack/pack-abc.pack",
            "/ui/doom/.git/FETCH_HEAD",
        ] {
            assert!(is_noise(&event(&[path])), "{path}");
        }
    }

    #[test]
    fn a_file_merely_named_like_git_is_not_noise() {
        assert!(!is_noise(&event(&["/ui/mods/.gitignore.lua"])));
        assert!(!is_noise(&event(&["/ui/mygit/mods/a.lua"])));
        assert!(!is_noise(&event(&["/ui/lib/.gitkeep.lua"])));
    }

    #[test]
    fn a_real_edit_beside_git_churn_still_wakes_it() {
        assert!(!is_noise(&event(&[
            "/ui/.git/index",
            "/ui/mods/session_list.lua",
        ])));
    }

    #[test]
    fn reading_a_file_is_not_a_change() {
        // inotify reports each read of layout.lua as an access event.
        let read = Event {
            kind: EventKind::Access(AccessKind::Read),
            paths: vec![PathBuf::from("/ui/layout.lua")],
            attrs: Default::default(),
        };
        assert!(is_noise(&read));
    }

    #[test]
    fn a_mixed_batch_is_not_noise() {
        assert!(!is_noise(&event(&["/ui/mods/a.lua~", "/ui/mods/a.lua"])));
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
