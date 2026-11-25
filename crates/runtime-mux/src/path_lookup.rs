#[cfg(test)]
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub fn which_on_path(exe: &str) -> bool {
    resolve_on_path(exe).is_some()
}

pub fn resolve_on_path(exe: &str) -> Option<PathBuf> {
    resolve_on(exe, &std::env::var_os("PATH")?)
}

pub fn resolve_on(exe: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    if exe.is_empty() || exe.contains(std::path::MAIN_SEPARATOR) || exe.contains('/') {
        return None;
    }
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(exe))
        .find(|candidate| is_executable_file(candidate))
}

pub fn path_dirs() -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .collect()
}

pub fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

pub const WSL_DISTRO_NAME_VAR: &str = "WSL_DISTRO_NAME";

pub fn current_wsl_distro() -> Option<String> {
    std::env::var(WSL_DISTRO_NAME_VAR)
        .ok()
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
}

#[cfg(test)]
pub(crate) fn with_path<T>(path: impl AsRef<OsStr>, f: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A test that panicked while holding it poisoned the lock; the value is
    // `()`, so there is nothing to protect against and the next test may run.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os("PATH");
    std::env::set_var("PATH", path.as_ref());
    let out = f();
    match saved {
        Some(v) => std::env::set_var("PATH", v),
        None => std::env::remove_var("PATH"),
    }
    out
}
