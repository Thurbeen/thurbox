//! Small shell/SSH command helpers shared across modules.
//!
//! Centralizes two things that would otherwise be duplicated wherever thurbox
//! shells out over SSH: POSIX single-quote escaping for tokens that a remote
//! login shell will re-split, and construction of the `ssh <opts> <dest>`
//! command prefix.

use std::process::Command;

/// Characters that never need quoting in a POSIX shell word.
fn is_safe_shell_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | ',')
}

/// POSIX single-quote escaping for one shell word.
///
/// Simple tokens (paths, branch names, flags) pass through unquoted; anything
/// with whitespace or shell metacharacters is wrapped in single quotes (with
/// embedded quotes escaped). An empty string becomes `''`.
///
/// Note: this does **not** strip newlines. Callers feeding a line-delimited
/// protocol (e.g. tmux control mode) must handle newlines themselves before
/// quoting — see [`crate::agent::control_mode::shell_escape`].
pub fn posix_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(is_safe_shell_char) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build an `ssh <opts> <destination>` [`Command`], ready for the caller to
/// append the remote command and its arguments.
pub fn ssh_command(destination: &str, ssh_opts: &[String]) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(ssh_opts);
    cmd.arg(destination);
    cmd
}

/// Build a `wsl.exe -d <distro>` [`Command`], ready for the caller to append
/// the in-distro command and its arguments.
///
/// This is the WSL analogue of [`ssh_command`], but `wsl.exe`'s argument
/// forwarding is subtly different from ssh's plain space-join (observed against
/// current `wsl.exe`; it only bypasses the shell entirely with `-e`/`--exec`,
/// which we don't pass):
///
/// - a **whitespace-free** token reaches the in-distro shell for interpretation
///   exactly like over ssh — POSIX-quote it the same way (the shell strips the
///   quotes; metacharacters like `%(…)` survive);
/// - an argument **containing whitespace** is preserved as a single word — do
///   *not* pre-quote a multi-word `sh -c` script for WSL, or the quotes arrive
///   literally and the shell treats the whole blob as one command name (see
///   `git::host_shell_c`, which branches on this).
///
/// No `--` separator is used (none of thurbox's commands start with a `-`,
/// matching the SSH path which also omits it).
///
/// `wsl.exe` inherits the **caller's** current directory and tries to `chdir`
/// to the same path inside the target distro — which fails when thurbox itself
/// runs inside *another* WSL distro (the caller's cwd, e.g. `/home/me/repo`,
/// doesn't exist on the target). That failure prints a `WSL Relay ERROR:
/// CreateProcessCommon chdir(...) failed` on stderr and corrupts the tmux
/// control-mode handshake, so the agent window never launches. So on a Unix
/// caller the child's cwd is pinned to `/` — a landing dir every distro has —
/// which is safe because every caller overrides the real working dir anyway
/// (`git -C <path>`, tmux `new-window -c <path>`). Pinning the *caller* cwd
/// (rather than passing `--cd /`) works on every `wsl.exe`; `--cd` is absent
/// from the Windows 10 inbox build, which aborts on unknown flags. A native
/// Windows caller keeps the inherit behavior (its `C:\…` cwd maps to
/// `/mnt/c/…` under default automount; there is no universally-valid Windows
/// pin, and `--cd` would trade this edge case for a hard legacy failure).
pub fn wsl_command(distro: &str) -> Command {
    let mut cmd = Command::new("wsl.exe");
    cmd.arg("-d").arg(distro);
    #[cfg(unix)]
    cmd.current_dir("/");
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_quote_passes_simple_tokens() {
        assert_eq!(posix_quote("feat-x"), "feat-x");
        assert_eq!(posix_quote("/home/me/repo"), "/home/me/repo");
        assert_eq!(posix_quote("-L"), "-L");
    }

    #[test]
    fn posix_quote_wraps_specials_and_empty() {
        assert_eq!(posix_quote("a b"), "'a b'");
        assert_eq!(posix_quote("it's"), "'it'\\''s'");
        assert_eq!(posix_quote(""), "''");
    }

    #[test]
    fn ssh_command_sets_program_opts_and_destination() {
        let cmd = ssh_command("me@box", &["-p".into(), "2222".into()]);
        assert_eq!(cmd.get_program().to_string_lossy(), "ssh");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["-p", "2222", "me@box"]);
    }

    #[test]
    fn wsl_command_sets_program_distro_and_neutral_cwd() {
        let cmd = wsl_command("Ubuntu");
        assert_eq!(cmd.get_program().to_string_lossy(), "wsl.exe");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["-d", "Ubuntu"]);
        // A Unix caller pins its cwd to `/` so wsl.exe doesn't inherit (and
        // fail to chdir to) a caller cwd that doesn't exist in the target
        // distro. Done via the child cwd — not `--cd`, which legacy wsl.exe
        // rejects.
        #[cfg(unix)]
        assert_eq!(cmd.get_current_dir(), Some(std::path::Path::new("/")));
        #[cfg(windows)]
        assert_eq!(cmd.get_current_dir(), None);
    }
}
