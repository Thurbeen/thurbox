//! Where a program would be found on this machine, without running it: what
//! `fleet doctor` reports for each harness's program.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The file `program` names: itself when it holds a path separator, else
/// the first match in `path` (the `PATH` value), with each `PATHEXT`
/// extension on Windows.
pub fn find(program: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 || candidate.is_absolute() {
        return runnable(candidate).then(|| candidate.to_path_buf());
    }
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_string)
            .chain(std::iter::once(String::new()))
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(path?).find_map(|dir| {
        extensions.iter().find_map(|ext| {
            let file = dir.join(format!("{program}{ext}"));
            runnable(&file).then_some(file)
        })
    })
}

#[cfg(unix)]
fn runnable(file: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    file.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn runnable(file: &Path) -> bool {
    file.is_file()
}
