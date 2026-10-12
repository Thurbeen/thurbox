//! Images dropped onto a remote session's pane.
//!
//! Dragging a file onto a terminal makes the terminal *paste its local path*;
//! the agent then reads that path off disk. Locally that works. On an
//! `ssh:`/`wsl:` session the agent runs on the host, where the path does not
//! exist, so the drop silently did nothing. [`dropped_images`] recognises such
//! a paste, [`upload`] copies exactly those files to a per-session directory on
//! the host over the host's own launcher (the system `ssh`, so thurbox handles
//! no credentials), and [`cleanup`] removes the directory with the session.
//!
//! Only a paste that is *entirely* existing local image files is touched; any
//! other text goes through unchanged, so ordinary pastes never reach this path.

use crate::session::HostDef;
use crate::shell::{posix_quote, HostLauncher};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Refuse anything larger: a drop is a screenshot or photo, and the copy runs
/// over the user's ssh link.
const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

const IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "heic"];

/// Parent of every session's drop directory on the host.
const REMOTE_ROOT: &str = "/tmp/thurbox-drop";

/// The directory holding one session's dropped files on its host.
///
/// The id is reduced to path-safe characters: it is persisted data, and this
/// string ends up inside a remote `rm -rf`.
fn remote_dir(session_id: &str) -> String {
    format!("{REMOTE_ROOT}/{}", sanitize(session_id))
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Whether uploads are supported for `host`: a POSIX host, since the copy and
/// the cleanup are `sh` scripts.
pub fn supported(host: &HostDef) -> bool {
    !host.is_windows()
}

/// The local image files `text` consists of, or `None` when it is anything else.
///
/// Terminals spell a dropped file as a bare path, a quoted or
/// backslash-escaped one, or a `file://` URI, and several files as several
/// such words. Every word must resolve to an existing image file within
/// `MAX_IMAGE_BYTES`; one that does not makes the whole paste ordinary text.
pub fn dropped_images(text: &str) -> Option<Vec<PathBuf>> {
    let words = split_words(text.trim())?;
    if words.is_empty() {
        return None;
    }
    words
        .iter()
        .map(|word| {
            let path = PathBuf::from(file_uri_path(word)?);
            is_image_file(&path).then_some(path)
        })
        .collect()
}

fn is_image_file(path: &Path) -> bool {
    let image = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()));
    image
        && path.is_absolute()
        && std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.len() <= MAX_IMAGE_BYTES)
            .unwrap_or(false)
}

/// Split like a shell would for the three spellings a terminal uses: single
/// quotes, double quotes and backslash escapes. Backslashes are path
/// separators on Windows, so they escape nothing there. `None` on an
/// unterminated quote.
fn split_words(text: &str) -> Option<Vec<String>> {
    let escapes = !cfg!(windows);
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') if escapes => current.push(chars.next()?),
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(ch);
                in_word = true;
            }
            (None, '\\') if escapes => {
                current.push(chars.next()?);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        words.push(current);
    }
    Some(words)
}

/// A `file://` URI's path, percent-decoded; any other word unchanged.
fn file_uri_path(word: &str) -> Option<String> {
    let Some(rest) = word.strip_prefix("file://") else {
        return Some(word.to_string());
    };
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The script that stores stdin as `dir/name` on the host, private to the user.
fn store_script(dir: &str, name: &str) -> String {
    format!(
        "umask 077 && mkdir -p {dir} && cat > {file}",
        dir = posix_quote(dir),
        file = posix_quote(&format!("{dir}/{name}")),
    )
}

/// Copy `local` to the session's drop directory on `host`; the remote path.
///
/// The file's bytes travel on the stdin of the host's launcher, so nothing but
/// this one file leaves the machine. The remote name is sanitised and made
/// unique with `seq`, so it needs no quoting where the agent reads it.
pub fn upload(
    host: &HostDef,
    session_id: &str,
    local: &Path,
    seq: usize,
) -> Result<String, String> {
    let name = format!(
        "{}-{seq}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
        sanitize(&local.file_name().map_or_else(
            || "image".to_string(),
            |name| name.to_string_lossy().into_owned()
        )),
    );
    let dir = remote_dir(session_id);
    let cmd = HostLauncher::for_host(host).shell_c(&store_script(&dir, &name));
    pipe_file(cmd, local)?;
    Ok(format!("{dir}/{name}"))
}

fn pipe_file(mut cmd: std::process::Command, local: &Path) -> Result<(), String> {
    let file = std::fs::File::open(local).map_err(|e| format!("{}: {e}", local.display()))?;
    let out = cmd
        .stdin(Stdio::from(file))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run {}: {e}", cmd.get_program().to_string_lossy()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Remove a session's drop directory from its host. Best effort: a host that
/// is down is already reported by the teardown it runs in, and the files live
/// in the host's temp directory either way.
pub fn cleanup(host: &HostDef, session_id: &str) {
    if !supported(host) {
        return;
    }
    let script = format!("rm -rf {}", posix_quote(&remote_dir(session_id)));
    let mut cmd = HostLauncher::for_host(host).shell_c(&script);
    cmd.stdin(Stdio::null()).stdout(Stdio::null());
    if let Err(e) = cmd.output() {
        tracing::debug!("drop cleanup for {session_id} did not run: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"\x89PNG").expect("write");
        path
    }

    #[test]
    fn a_bare_quoted_escaped_or_uri_path_is_recognised() {
        let dir = tempfile::tempdir().expect("tmp");
        let plain = image(dir.path(), "shot.png");
        let spaced = image(dir.path(), "my shot.PNG");
        let p = plain.display().to_string();
        let s = spaced.display().to_string();

        assert_eq!(dropped_images(&p), Some(vec![plain.clone()]));
        assert_eq!(dropped_images(&format!("{p}\n")), Some(vec![plain.clone()]));
        assert_eq!(
            dropped_images(&format!("'{s}'")),
            Some(vec![spaced.clone()])
        );
        assert_eq!(
            dropped_images(&format!("\"{s}\"")),
            Some(vec![spaced.clone()])
        );
        #[cfg(not(windows))]
        {
            assert_eq!(
                dropped_images(&s.replace(' ', "\\ ")),
                Some(vec![spaced.clone()])
            );
            assert_eq!(
                dropped_images(&format!("file://{}", s.replace(' ', "%20"))),
                Some(vec![spaced.clone()])
            );
        }
        assert_eq!(
            dropped_images(&format!("{p} '{s}'")),
            Some(vec![plain, spaced])
        );
    }

    #[test]
    fn anything_that_is_not_only_images_is_ordinary_text() {
        let dir = tempfile::tempdir().expect("tmp");
        let png = image(dir.path(), "shot.png");
        let txt = image(dir.path(), "notes.txt");
        let p = png.display().to_string();

        assert_eq!(dropped_images("hello world"), None);
        assert_eq!(dropped_images(""), None);
        assert_eq!(dropped_images(&txt.display().to_string()), None);
        assert_eq!(dropped_images(&format!("{p} {}", txt.display())), None);
        assert_eq!(dropped_images(&format!("look at {p}")), None);
        assert_eq!(dropped_images("/no/such/file.png"), None);
        assert_eq!(dropped_images("relative.png"), None);
        assert_eq!(dropped_images(&format!("'{p}")), None);
        assert_eq!(dropped_images(&dir.path().display().to_string()), None);
    }

    #[test]
    fn an_oversized_image_is_not_copied() {
        let dir = tempfile::tempdir().expect("tmp");
        let big = dir.path().join("big.png");
        let file = std::fs::File::create(&big).expect("create");
        file.set_len(MAX_IMAGE_BYTES + 1).expect("len");
        assert_eq!(dropped_images(&big.display().to_string()), None);
    }

    #[test]
    fn the_remote_directory_is_path_safe() {
        assert_eq!(remote_dir("abc-123"), "/tmp/thurbox-drop/abc-123");
        assert_eq!(remote_dir("../x y"), "/tmp/thurbox-drop/.._x_y");
    }

    #[cfg(unix)]
    #[test]
    fn the_script_stores_stdin_under_the_directory() {
        let dir = tempfile::tempdir().expect("tmp");
        let src = image(dir.path(), "a.png");
        let target = dir.path().join("host dir");
        let script = store_script(&target.display().to_string(), "1-a.png");

        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(script);
        pipe_file(cmd, &src).expect("stored");

        assert_eq!(
            std::fs::read(target.join("1-a.png")).expect("read"),
            b"\x89PNG"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_command_reports_its_stderr() {
        let dir = tempfile::tempdir().expect("tmp");
        let src = image(dir.path(), "a.png");
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("echo denied >&2; exit 3");
        assert_eq!(pipe_file(cmd, &src), Err("denied".to_string()));
    }

    #[test]
    fn a_windows_host_is_not_supported() {
        let host = HostDef {
            name: "w".into(),
            destination: "w".into(),
            platform: Some(crate::session::Platform::Windows),
            ..Default::default()
        };
        assert!(!supported(&host));
    }
}
