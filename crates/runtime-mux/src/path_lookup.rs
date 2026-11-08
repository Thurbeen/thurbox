//! What this process can find on `PATH`, and which WSL distro it runs in:
//! the lookups every launch and every preflight share.

#[cfg(test)]
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Whether `exe` resolves on `PATH`. A minimal lookup that avoids pulling in a
/// `which` crate for a one-off probe (used to detect optional helper binaries
/// like `wsl.exe` / `powershell.exe`); cheap PATH scan, no process spawn.
pub fn which_on_path(exe: &str) -> bool {
    resolve_on_path(exe).is_some()
}

/// Where `exe` resolves on `PATH`, as a path a caller can hand to something
/// that does *not* share this process's `PATH` — `tmux_compat::server`'s
/// `resolve_local_program`, which hands a local window command to the
/// multiplexer, is why this exists rather than [`which_on_path`] alone.
///
/// `None` when nothing matches, and also when `exe` already carries a path
/// separator: a caller-supplied path is the caller's decision, and re-resolving
/// it against `PATH` would be wrong (`./agent` is not `bin/./agent`).
///
/// The match is the file being **executable** on Unix, not merely present: a
/// directory named `claude` on `PATH` is not the agent, and a mode-644 file is
/// not something `execvp` will run. Verbatim, with no `.exe`/`PATHEXT` munging
/// — the same lookup `which_on_path` has always done.
///
/// A `PATH` component that is not **absolute** is skipped, which includes the
/// empty one POSIX reads as "the current directory" (`:/usr/bin`, or a stray
/// trailing colon — an ordinary accident in a shell config). Whose current
/// directory is the entire question: the answer here is handed to a consumer
/// with a working directory of its own, so a relative one would be resolved
/// there — reintroducing exactly the dependence this exists to remove, and
/// letting a `claude` sitting in the repo being worked on shadow the real one.
/// Skipping is also the safe half of that choice: declining leaves the caller
/// with the command it already had, where honouring it would silently launch a
/// different binary.
pub fn resolve_on_path(exe: &str) -> Option<PathBuf> {
    if exe.is_empty() || exe.contains(std::path::MAIN_SEPARATOR) || exe.contains('/') {
        return None;
    }
    path_dirs()
        .into_iter()
        .map(|dir| dir.join(exe))
        .find(|candidate| is_executable_file(candidate))
}

/// The directories [`resolve_on_path`] searches, in the order it searches them.
///
/// Public because a lookup that finds nothing has to be able to say *where it
/// looked*: an error naming only the binary leaves the reader unable to tell a
/// missing install from a `PATH` the process was started without. Deriving that
/// list a second time in the message would let the two drift, so the resolver
/// and the message read the same function.
///
/// Empty when `PATH` is unset — which is itself the answer, and reads that way.
pub fn path_dirs() -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .collect()
}

/// Whether `p` is a file this process could `exec`.
pub fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The environment variable every WSL2 distro's init sets to that distro's own
/// name. Present only *inside* a distro — not on Windows, not on a plain Linux
/// or macOS host.
pub const WSL_DISTRO_NAME_VAR: &str = "WSL_DISTRO_NAME";

/// The WSL distro this process is itself running inside, if any.
///
/// Read from the environment rather than probed for: `wsl.exe` is on `PATH`
/// inside a distro (interop), so its presence says a distro is *reachable* and
/// never which one we are already in. Read here, not in `host_def`: a
/// `HostDef` is told the distro (`HostDef::is_wsl_loopback`) rather than
/// reading process state itself.
pub fn current_wsl_distro() -> Option<String> {
    std::env::var(WSL_DISTRO_NAME_VAR)
        .ok()
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
}

/// Run `f` with [`WSL_DISTRO_NAME_VAR`] set to `distro` (or unset, for
/// `None`), restoring what was there before.
///
/// Serialized on a process-wide lock, and the **only** way a test may set it,
/// for the reason [`with_path`] gives.
#[cfg(test)]
pub(crate) fn with_wsl_distro<T>(distro: Option<&str>, f: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os(WSL_DISTRO_NAME_VAR);
    match distro {
        Some(d) => std::env::set_var(WSL_DISTRO_NAME_VAR, d),
        None => std::env::remove_var(WSL_DISTRO_NAME_VAR),
    }
    let out = f();
    match saved {
        Some(v) => std::env::set_var(WSL_DISTRO_NAME_VAR, v),
        None => std::env::remove_var(WSL_DISTRO_NAME_VAR),
    }
    out
}

/// Run `f` with `PATH` set to `path`, restoring what was there before.
///
/// Serialized on a process-wide lock, and the **only** way a test may set
/// `PATH`: it is process state, and under plain `cargo test` the unit tests
/// that need it run concurrently in one process, where interleaved writes make
/// one test observe another's directory or restore a stale value. (`nextest`,
/// the repo's gate, gives each test its own process — this is what keeps the
/// other entry point honest.)
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
