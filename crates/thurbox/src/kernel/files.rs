use std::path::{Component, Path, PathBuf};

pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub const MAX_ENTRIES: usize = 2_000;

pub type ReadKey = (PathBuf, String);
pub const MAX_READ_KEYS: usize = 16;
pub type ReadStore = super::worker_store::WorkerStore<ReadKey, Result<String, String>>;

pub fn store() -> ReadStore {
    ReadStore::new(
        2,
        MAX_READ_KEYS,
        std::time::Duration::from_secs(1),
        |(root, path)| read(root, path),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

pub fn resolve(root: &Path, relative: &str) -> Result<PathBuf, String> {
    // Reject the lexical escape first, so a non-existent `../../etc/passwd`
    // fails the same way an existing one does.
    let candidate = Path::new(relative);
    // A ROOT or PREFIX component, not `is_absolute()`.
    //
    // On Windows `is_absolute()` is false for `/etc/passwd`, because absolute
    // there means a drive prefix — so a leading slash slipped past this check and
    // was refused further down, by the resolved-form comparison, with a message
    // about the path not existing. Matching on components refuses it here on
    // both platforms, and `Prefix` additionally covers `C:\…`, `\\?\…` and UNC
    // paths, none of which a session-rooted read may name.
    if candidate
        .components()
        .any(|part| matches!(part, Component::RootDir | Component::Prefix(_)))
    {
        return Err("absolute paths are not allowed".to_string());
    }
    if candidate
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("path escapes the session directory".to_string());
    }

    let joined = root.join(candidate);
    // Then the resolved form, which is what catches a symlink out.
    let real_root = root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    match joined.canonicalize() {
        Ok(real) => {
            if !real.starts_with(&real_root) {
                return Err("path escapes the session directory".to_string());
            }
            Ok(real)
        }
        // Not existing is a different failure from escaping, and the caller
        // wants to tell them apart.
        Err(e) => Err(format!("{}: {e}", joined.display())),
    }
}

pub fn list(root: &Path, relative: &str) -> Result<Vec<Entry>, String> {
    let path = resolve(root, relative)?;
    // Every link's type is answered against this, so the listing cannot report
    // something `resolve` would refuse to open.
    let real_root = root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    let read = std::fs::read_dir(&path).map_err(|e| format!("{}: {e}", path.display()))?;

    let mut entries: Vec<Entry> = Vec::new();
    for item in read.flatten() {
        if entries.len() >= MAX_ENTRIES {
            break;
        }
        let name = item.file_name().to_string_lossy().to_string();
        let is_dir = match item.file_type() {
            Ok(kind) if kind.is_symlink() => links_to_directory_inside(&real_root, &item.path()),
            Ok(kind) => kind.is_dir(),
            Err(_) => false,
        };
        entries.push(Entry { name, is_dir });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    Ok(entries)
}

fn links_to_directory_inside(root: &Path, link: &Path) -> bool {
    match link.canonicalize() {
        Ok(target) => target.starts_with(root) && target.is_dir(),
        Err(_) => false,
    }
}

pub fn read(root: &Path, relative: &str) -> Result<String, String> {
    let path = resolve(root, relative)?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_dir() {
        return Err(format!("{} is a directory", path.display()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} is {} bytes; the viewer reads at most {MAX_FILE_BYTES}",
            path.display(),
            meta.len()
        ));
    }
    // Lossy rather than refusing: a file with one stray byte is still worth
    // looking at, and the viewer is not an editor.
    std::fs::read(&path)
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("README.md"), "# hello\n").expect("write");
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").expect("write");
        dir
    }

    #[test]
    fn entries_list_directories_first() {
        let dir = fixture();
        let entries = list(dir.path(), "").expect("list");
        assert_eq!(entries[0].name, "src");
        assert!(entries[0].is_dir);
        assert!(entries.iter().any(|e| e.name == "README.md" && !e.is_dir));
    }

    #[test]
    fn a_nested_directory_lists() {
        let dir = fixture();
        let entries = list(dir.path(), "src").expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "main.rs");
    }

    #[test]
    fn a_file_reads() {
        let dir = fixture();
        assert_eq!(read(dir.path(), "README.md").expect("read"), "# hello\n");
    }

    #[test]
    fn a_parent_traversal_is_refused() {
        let dir = fixture();
        for attempt in ["..", "../etc/passwd", "src/../../secret", "./../x"] {
            let error = resolve(dir.path(), attempt).unwrap_err();
            assert!(
                error.contains("escapes"),
                "{attempt} should be refused: {error}"
            );
        }
    }

    #[test]
    fn an_absolute_path_is_refused() {
        let dir = fixture();
        // A leading slash is absolute for this purpose on every platform, even
        // where `Path::is_absolute()` disagrees (Windows wants a drive prefix).
        let error = resolve(dir.path(), "/etc/passwd").unwrap_err();
        assert!(error.contains("absolute"), "{error}");

        // The forms only Windows can express. On Unix `C:\Windows` is an
        // ordinary relative filename, so asserting it there would be wrong.
        #[cfg(windows)]
        {
            for attempt in [
                "C:\\Windows\\System32",
                "\\\\?\\C:\\Windows",
                "\\\\server\\share",
            ] {
                let error = resolve(dir.path(), attempt).unwrap_err();
                assert!(error.contains("absolute"), "{attempt}: {error}");
            }
        }
    }

    #[test]
    fn a_symlink_out_of_the_directory_is_refused() {
        // The lexical check alone would pass this, which is why the resolved
        // form is checked too.
        #[cfg(unix)]
        {
            let dir = fixture();
            let outside = tempfile::tempdir().expect("outside");
            std::fs::write(outside.path().join("secret"), "shh").expect("write");
            std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link"))
                .expect("symlink");

            let error = resolve(dir.path(), "link").unwrap_err();
            assert!(error.contains("escapes"), "{error}");
        }
    }

    #[test]
    fn a_symlinked_directory_is_listed_as_a_directory() {
        #[cfg(unix)]
        {
            let dir = fixture();
            std::os::unix::fs::symlink(dir.path().join("src"), dir.path().join("link-to-src"))
                .expect("symlink");

            let entries = list(dir.path(), "").expect("list");
            let link = entries
                .iter()
                .find(|entry| entry.name == "link-to-src")
                .expect("the link is listed");
            assert!(link.is_dir, "a link to a directory is a directory");

            // Pinned because the reordering is a consequence of the fix rather
            // than a regression: the link is a directory now, and directories
            // sort first.
            let order: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
            assert_eq!(order, ["link-to-src", "src", "README.md"]);
        }
    }

    #[test]
    fn a_symlink_out_of_the_directory_does_not_report_its_target() {
        #[cfg(unix)]
        {
            let dir = fixture();
            let outside = tempfile::tempdir().expect("outside");
            std::fs::create_dir_all(outside.path().join("secrets")).expect("mkdir");
            std::os::unix::fs::symlink(outside.path().join("secrets"), dir.path().join("link-out"))
                .expect("symlink");

            let entries = list(dir.path(), "").expect("list");
            let link = entries
                .iter()
                .find(|entry| entry.name == "link-out")
                .expect("the link is listed");
            assert!(!link.is_dir, "an outside target's type is not reported");

            // Indistinguishable from a link whose target is a file: both are
            // refused, and neither says which it was.
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                dir.path().join("link-file"),
            )
            .expect("symlink");
            std::fs::write(outside.path().join("secret.txt"), "shh").expect("write");
            let entries = list(dir.path(), "").expect("list");
            let file_link = entries
                .iter()
                .find(|entry| entry.name == "link-file")
                .expect("the link is listed");
            assert_eq!(link.is_dir, file_link.is_dir);

            for name in ["link-out", "link-file"] {
                for error in [
                    list(dir.path(), name).unwrap_err(),
                    read(dir.path(), name).unwrap_err(),
                ] {
                    assert!(error.contains("escapes"), "{name}: {error}");
                }
            }
        }
    }

    #[test]
    fn a_dangling_symlink_is_listed_and_is_not_a_directory() {
        // There is no target to ask, and a link that resolves to nothing cannot
        // be expanded, so it is listed as a leaf rather than dropped.
        #[cfg(unix)]
        {
            let dir = fixture();
            std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join("dangling"))
                .expect("symlink");

            let entries = list(dir.path(), "").expect("list");
            let link = entries
                .iter()
                .find(|entry| entry.name == "dangling")
                .expect("the link is listed");
            assert!(!link.is_dir);
        }
    }

    #[test]
    fn a_symlink_loop_is_listed_and_is_not_a_directory() {
        // ELOOP rather than "not found", and it must take the same path: the
        // listing neither stalls on it nor drops the entry.
        #[cfg(unix)]
        {
            let dir = fixture();
            std::os::unix::fs::symlink(dir.path().join("ouroboros"), dir.path().join("ouroboros"))
                .expect("symlink");

            let entries = list(dir.path(), "").expect("list");
            let link = entries
                .iter()
                .find(|entry| entry.name == "ouroboros")
                .expect("the link is listed");
            assert!(!link.is_dir);
        }
    }

    #[test]
    fn a_directory_is_not_read_as_a_file() {
        let dir = fixture();
        let error = read(dir.path(), "src").unwrap_err();
        assert!(error.contains("is a directory"), "{error}");
    }

    #[test]
    fn an_oversized_file_is_refused_with_its_size() {
        let dir = fixture();
        let big = dir.path().join("big.bin");
        std::fs::write(&big, vec![0u8; (MAX_FILE_BYTES + 1) as usize]).expect("write");
        let error = read(dir.path(), "big.bin").unwrap_err();
        assert!(error.contains("at most"), "{error}");
    }

    #[test]
    fn a_missing_path_reads_as_missing_not_as_an_escape() {
        let dir = fixture();
        let error = resolve(dir.path(), "nope.txt").unwrap_err();
        assert!(!error.contains("escapes"), "{error}");
    }

    #[test]
    fn invalid_utf8_is_shown_rather_than_refused() {
        let dir = fixture();
        std::fs::write(dir.path().join("odd.txt"), [0x68, 0x69, 0xff]).expect("write");
        let text = read(dir.path(), "odd.txt").expect("read");
        assert!(text.starts_with("hi"), "{text:?}");
    }
}
