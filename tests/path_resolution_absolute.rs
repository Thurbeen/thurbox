//! `resolve_on_path` must never answer with a **relative** path.
//!
//! It exists so a local window command does not depend on the multiplexer's
//! `PATH` or working directory. An empty `PATH` component — an ordinary
//! accident, a stray leading or trailing colon in a shell config — is read by
//! POSIX as "the current directory", so joining it produces a bare name again:
//! the very thing tmux would then resolve from the *window's* `-c` directory,
//! which is the bug this function was added to prevent.
//!
//! Its own test binary because it changes the process working directory, which
//! a sibling test in the same binary would see.

#![cfg(unix)]

use std::path::PathBuf;

/// An executable file in `dir`, the way a `PATH` lookup expects to find one.
fn executable_marker(dir: &std::path::Path, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
    p
}

#[test]
fn an_empty_path_component_never_resolves_to_a_relative_program() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = "tbx_empty_component_probe";
    executable_marker(dir.path(), marker);

    let saved_cwd = std::env::current_dir().expect("cwd");
    // The current directory *is* the directory holding the marker, so an empty
    // component would match — and match first.
    std::env::set_current_dir(dir.path()).expect("chdir");
    let saved_path = std::env::var_os("PATH");
    std::env::set_var("PATH", format!(":{}", dir.path().to_string_lossy()));

    let found = thurbox::paths::resolve_on_path(marker);

    match saved_path {
        Some(v) => std::env::set_var("PATH", v),
        None => std::env::remove_var("PATH"),
    }
    std::env::set_current_dir(&saved_cwd).expect("restore cwd");

    let found = found.expect("the real directory on PATH still resolves it");
    assert!(
        found.is_absolute(),
        "resolved to {found:?}, which the consumer would resolve from its own \
         working directory rather than thurbox's"
    );
    assert_eq!(found.file_name().unwrap(), marker);
}
