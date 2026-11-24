//! A message handed to Claude Code through its messaging socket, the path
//! it names to its hooks in `CLAUDE_CODE_MESSAGING_SOCKET`: one stream-json
//! `user` line starts a turn, or waits for the current one to end, and its
//! prompt hook reports the text. Nothing is typed into its composer.
use std::io;
use std::path::Path;

/// The line Claude Code's socket reads as one user turn.
pub fn claude_line(text: &str) -> String {
    let line = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    });
    format!("{line}\n")
}

/// Write `text` to the Claude Code socket at `socket`.
#[cfg(unix)]
pub fn post_claude(socket: &Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let write_bound = std::time::Duration::from_secs(5);
    let mut stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(write_bound))?;
    stream.write_all(claude_line(text).as_bytes())?;
    stream.flush()?;
    stream.shutdown(std::net::Shutdown::Write)
}

#[cfg(not(unix))]
pub fn post_claude(_socket: &Path, _text: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Claude Code's messaging socket is a Unix socket",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    #[test]
    fn a_post_is_one_user_line_on_the_socket() {
        let dir = std::env::temp_dir().join(format!("fleet-inbox-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("s");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        post_claude(&path, "line one\n\"two\"").expect("post");
        let mut got = String::new();
        listener
            .accept()
            .expect("accept")
            .0
            .read_to_string(&mut got)
            .expect("read");
        let _ = std::fs::remove_dir_all(&dir);
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).expect("json");
        assert!(
            got.ends_with('\n') && got.matches('\n').count() == 1,
            "{got:?}"
        );
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["content"], "line one\n\"two\"");
    }

    #[test]
    fn a_post_to_no_socket_fails() {
        assert!(post_claude(Path::new("/nonexistent/fleet-inbox.sock"), "x").is_err());
    }
}
