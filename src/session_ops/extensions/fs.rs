//! The filesystem safety kit the installer leans on: resilient recursive
//! removal, the guards that refuse dangerous deletes and path traversal, the
//! managed-file ownership marker, and the platform symlink/exec-bit shims.

use std::path::Path;

// The path-traversal guard is the shared implementation in `paths` (see its
// doc for why `session::plugin_spec` keeps a stricter variant of its own);
// re-exported here so the installer and its tests keep their historical names.
pub(super) use crate::paths::{ensure_safe_relative, safe_join};

/// Remove a directory tree. On Windows a just-written payload file can be held
/// transiently by the search indexer / antivirus, so `remove_dir_all` fails with
/// `ERROR_SHARING_VIOLATION` (os error 32); retry with a short backoff until the
/// handle is released. Unix removes in one shot.
pub(super) fn remove_dir_all_resilient(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let mut last: std::io::Result<()> = Ok(());
        // Rides out a *transient* hold on a just-written payload file by the
        // search indexer / antivirus (ERROR_SHARING_VIOLATION), retrying ~1.4s.
        // It does NOT help when the dir is held persistently — e.g. psmux's
        // server-level handle on a deleted session's pane cwd, which only
        // `kill-server` frees (a documented Windows limitation).
        for attempt in 0..5u64 {
            match std::fs::remove_dir_all(path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Err(e);
                    std::thread::sleep(std::time::Duration::from_millis(100 * (attempt + 1)));
                }
            }
        }
        last
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_dir_all(path)
    }
}

/// Refuse to recursively delete obviously-dangerous paths (root, `$HOME`
/// itself, or a shallow path) — a guard before `remove_dir_all` on a
/// manifest-supplied home.
pub(super) fn guard_removable_dir(path: &Path) -> Result<(), String> {
    let depth = path
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count();
    if depth < 2 {
        return Err(format!(
            "refusing to remove '{}' (too shallow); remove it by hand",
            path.display()
        ));
    }
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    if let Some(home) = std::env::var_os(home_var) {
        if path == Path::new(&home) {
            return Err("refusing to remove the user's home directory".into());
        }
    }
    Ok(())
}

/// Marker an installer-managed `substitute` file carries (in the template
/// content) so reinstall can overwrite *its own* file but not one the user has
/// edited (or whose marker they removed). The remote provisioning
/// (`remote_hooks`) applies the same rule to files it ships to a host.
pub(crate) const MANAGED_MARKER: &str = "thurbox `extension install`";

/// Whether `dest` is a `substitute` file the user has taken ownership of: it
/// exists but no longer carries the managed marker. A missing file (fresh
/// install) or one still carrying the marker is ours to (over)write.
pub(super) fn is_user_modified(dest: &Path) -> bool {
    match std::fs::read_to_string(dest) {
        Ok(content) => !content.contains(MANAGED_MARKER),
        Err(_) => false,
    }
}

#[cfg(unix)]
pub(super) fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
pub(super) fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
pub(super) fn make_symlink(target: &str, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link)
        .map_err(|e| format!("symlink {} -> {target}: {e}", link.display()))
}

#[cfg(windows)]
pub(super) fn make_symlink(target: &str, link: &Path) -> Result<(), String> {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    // `target` is relative to the link's parent; resolve it to choose the right
    // symlink flavour (Windows distinguishes file vs directory symlinks).
    let resolved = link
        .parent()
        .map(|p| p.join(target))
        .unwrap_or_else(|| std::path::PathBuf::from(target));
    let is_dir = resolved.is_dir();
    let primary = if is_dir {
        symlink_dir(target, link)
    } else {
        symlink_file(target, link)
    };
    if let Err(err) = primary {
        // Symlink creation needs privilege (admin or Developer Mode). Fall back
        // to a privilege-free equivalent that keeps the payload reachable at
        // `link`: an NTFS junction for directories, a hard link for files.
        let recovered = if is_dir {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(&resolved)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        } else {
            std::fs::hard_link(&resolved, link).is_ok()
        };
        if !recovered {
            return Err(format!("symlink {} -> {target}: {err}", link.display()));
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(super) fn make_symlink(_target: &str, _link: &Path) -> Result<(), String> {
    Err("symlinks are not supported on this platform".into())
}
