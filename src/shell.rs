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
/// quoting — see [`crate::backend::tmux_compat::control_mode::shell_escape`].
pub fn posix_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(is_safe_shell_char) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Defensive `ssh` options thurbox appends to **every** ssh invocation so a
/// broken, unreachable, or password-only host fails fast and non-interactively
/// instead of freezing the single-threaded TUI.
///
/// - `BatchMode=yes` — never fall back to a password/keyboard-interactive
///   prompt. Such a prompt reads from the controlling `/dev/tty` (the terminal
///   ratatui owns), so it would corrupt the display and steal keystrokes; with
///   BatchMode ssh fails immediately instead.
/// - `ConnectTimeout=5` — bound the connect phase for an unreachable host.
/// - `ServerAliveInterval=5` + `ServerAliveCountMax=1` — bound a *hung
///   established* connection (e.g. a mid-session network drop on the long-lived
///   control-mode ssh), so it is detected in ~5s rather than hanging until the
///   OS TCP timeout.
///
/// These are appended **after** the caller's own `ssh_opts`; ssh honors the
/// first occurrence of each option, so a user-configured value still wins.
pub const SSH_HARDENING_OPTS: [&str; 8] = [
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=5",
    "-o",
    "ServerAliveInterval=5",
    "-o",
    "ServerAliveCountMax=1",
];

/// Connection-multiplexing options, appended with the hardening set when the
/// user's `~/.ssh` directory exists (that is where the control socket lives).
///
/// Without a master connection every remote git call, probe and provisioning
/// step pays a full TCP + auth handshake — ~300 ms to a LAN host, measured in
/// `docs/PERFORMANCE.md` — and several paths make many such calls in a row.
/// With one, the second and later round trips cost single-digit milliseconds.
///
/// `%C` is ssh's hash of host+port+user, so the socket path stays short
/// (`sun_path` caps at ~104 bytes) and collision-free; the `~` is expanded by
/// ssh itself. `ControlMaster=auto` degrades to a plain connection when the
/// socket cannot be created, and — like the hardening set — these are appended
/// after the caller's `ssh_opts`, so a user-configured `ControlMaster`/
/// `ControlPath`/`ControlPersist` still wins.
pub const SSH_MULTIPLEX_OPTS: [&str; 6] = [
    "-o",
    "ControlMaster=auto",
    "-o",
    "ControlPersist=60",
    "-o",
    "ControlPath=~/.ssh/thurbox-%C",
];

/// Whether `~/.ssh` exists, probed once per process.
///
/// The multiplexing socket lives there; on a machine without the directory the
/// options are omitted entirely rather than risking a per-connection
/// `muxserver_listen` warning on stderr, which remote error reporting would
/// otherwise have to filter.
fn ssh_dir_exists() -> bool {
    static EXISTS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *EXISTS.get_or_init(|| {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| std::path::Path::new(&home).join(".ssh").is_dir())
            .unwrap_or(false)
    })
}

/// The options [`ssh_command`] appends after the caller's own: the hardening
/// set, plus the multiplexing set when `~/.ssh` exists to hold the control
/// socket. Public so tests can build exact expectations without re-deriving
/// the directory probe.
pub fn ssh_appended_opts() -> Vec<&'static str> {
    let mut opts: Vec<&'static str> = SSH_HARDENING_OPTS.to_vec();
    if ssh_dir_exists() {
        opts.extend(SSH_MULTIPLEX_OPTS);
    }
    opts
}

/// Build an `ssh <opts> <hardening> <destination>` [`Command`], ready for the
/// caller to append the remote command and its arguments.
///
/// Every thurbox ssh use is non-interactive, so [`SSH_HARDENING_OPTS`] is always
/// applied (after the caller's `ssh_opts`, which therefore take precedence),
/// and [`SSH_MULTIPLEX_OPTS`] follows whenever `~/.ssh` exists to hold the
/// control socket.
pub fn ssh_command(destination: &str, ssh_opts: &[String]) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(ssh_opts);
    cmd.args(ssh_appended_opts());
    cmd.arg(destination);
    cmd
}

/// PowerShell single-quote escaping for one argument.
///
/// Inside single quotes PowerShell interprets nothing except a doubled quote,
/// so this is the whole rule — unlike [`posix_quote`] there is no safe-token
/// fast path, because the callers embed the result in larger PowerShell
/// expressions where a bare token could be re-parsed. One implementation for
/// what had grown three (`backend::tmux`, `git::remote`, and a near-copy in
/// `notifications`).
pub fn powershell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A Windows PowerShell script as one program invocation:
/// `powershell.exe -NoLogo -EncodedCommand <base64 of its UTF-16LE text>`.
///
/// A psmux window command is a single string run by whichever shell psmux
/// resolves — since psmux 3.4.0 (#683) `$SHELL` first, which Windows OpenSSH
/// sets to `cmd.exe` for every process it starts, so a server started over
/// ssh hands a bare PowerShell script to `cmd.exe /C` and the window dies.
/// Naming the interpreter makes the script run in PowerShell whatever that
/// shell is. Encoded, the string is letters, digits, `+`, `/` and `=` only,
/// which no layer between here and the pane — ssh, a `cmd.exe` login shell,
/// psmux's tokenizer, a shell psmux may still route it through — reads as
/// syntax. No `-NoProfile`: psmux ran the script as
/// `powershell -NoLogo -Command`, which loads the profile, and still does.
///
/// Encoding makes the line `39 + 4 * ceil(2n / 3)` chars for a script of `n`
/// UTF-16 units, about 2.7 times the script, so it is measured here, the one
/// place it is built: a line past [`WINDOWS_CMD_LINE_MAX`] is refused with
/// [`CommandLineTooLong`] rather than handed to a host that fails to start it
/// without saying why.
pub fn powershell_launcher(script: &str) -> Result<String, CommandLineTooLong> {
    encoded_powershell("-NoLogo", script)
}

/// `powershell.exe <switches> -EncodedCommand <base64>`, refused past
/// [`WINDOWS_CMD_LINE_MAX`].
fn encoded_powershell(switches: &str, script: &str) -> Result<String, CommandLineTooLong> {
    use base64::Engine as _;
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let line = format!(
        "powershell.exe {switches} -EncodedCommand {}",
        base64::engine::general_purpose::STANDARD.encode(utf16)
    );
    if line.len() > WINDOWS_CMD_LINE_MAX {
        return Err(CommandLineTooLong {
            len: line.len(),
            max: WINDOWS_CMD_LINE_MAX,
        });
    }
    Ok(line)
}

/// The longest command line `cmd.exe` runs (it answers "The command line is
/// too long." past it), and the bound used for every Windows host rather than
/// `CreateProcess`'s 32,767: Windows OpenSSH runs a one-shot ssh command
/// through its default login shell, `cmd.exe`, and a psmux server started
/// over ssh has `$SHELL=cmd.exe` for any window it routes through a shell. A
/// line that only fits a local server would fail the same agent on a host.
pub const WINDOWS_CMD_LINE_MAX: usize = 8191;

/// A command line longer than a Windows host will run, refused before it is
/// sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLineTooLong {
    pub len: usize,
    pub max: usize,
}

impl std::fmt::Display for CommandLineTooLong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "command line is {} chars, Windows limit is {}",
            self.len, self.max
        )
    }
}

impl std::error::Error for CommandLineTooLong {}

/// A launcher for a command on an off-local host: `ssh <opts> <dest>` or
/// `wsl.exe -d <distro>`.
///
/// One implementation of a construction that had grown four copies —
/// `git::command::host_launcher`, `git::remote::host_shell_c`,
/// `usage::remote_read_command` and the transport's prefix branch — each one
/// more place for the two quoting rules below to drift apart. It launches, and
/// nothing else: it names no multiplexer and adds no `-L`, so the tmux
/// transport and a plain `git` or `sh -c` call build on the same one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostLauncher {
    Ssh {
        destination: String,
        ssh_opts: Vec<String>,
    },
    Wsl {
        distro: String,
    },
}

impl HostLauncher {
    /// How `host` is reached: its WSL distro, else its ssh destination. The
    /// one conversion from a host entry to a launcher.
    pub fn for_host(host: &crate::session::HostDef) -> Self {
        if host.is_wsl() {
            Self::Wsl {
                distro: host.distro_name(),
            }
        } else {
            Self::Ssh {
                destination: host.destination.clone(),
                ssh_opts: host.ssh_opts.clone(),
            }
        }
    }

    /// The program this launcher runs on this machine.
    pub fn program(&self) -> &'static str {
        match self {
            Self::Ssh { .. } => "ssh",
            Self::Wsl { .. } => "wsl.exe",
        }
    }

    /// The bare launcher, ready for the caller to append the remote command.
    /// Both transports join and shell-interpret whitespace-free trailing
    /// tokens identically, so callers append the same POSIX-quoted words.
    pub fn command(&self) -> Command {
        match self {
            Self::Ssh {
                destination,
                ssh_opts,
            } => ssh_command(destination, ssh_opts),
            Self::Wsl { distro } => wsl_command(distro),
        }
    }

    /// Run a multi-statement POSIX script via `sh -c` on the host.
    ///
    /// The two branches encode the transports' one real difference:
    ///
    /// - **`wsl.exe`** needs `--exec` (`-e`), which skips its command-line
    ///   processing entirely: argv reaches the in-distro process verbatim, so
    ///   the script travels as one **unquoted** arg and `sh -c` parses the
    ///   `posix_quote`d paths inside it exactly like the ssh path. Without
    ///   `-e`, `wsl.exe` mangles the script — it substitutes `$…` even inside
    ///   a preserved argument, and pre-quoting makes the in-distro shell treat
    ///   the quoted blob as one command word ("not found").
    /// - **`ssh`** space-joins its trailing args into one string the remote
    ///   login shell re-splits, so the script must be POSIX-quoted to survive
    ///   as a single `sh -c` argument.
    ///
    /// POSIX hosts only — a native-Windows host has no `sh`; see
    /// `git::remote::host_powershell_c`.
    pub fn shell_c(&self, script: &str) -> Command {
        let mut cmd = self.command();
        match self {
            Self::Wsl { .. } => {
                cmd.arg("-e").arg("sh").arg("-c").arg(script);
            }
            Self::Ssh { .. } => {
                cmd.arg(posix_quote("sh"))
                    .arg(posix_quote("-c"))
                    .arg(posix_quote(script));
            }
        }
        cmd
    }

    /// [`Self::shell_c`] with positional parameters: `sh -c <script> <args…>`,
    /// so the first of `args` is the script's `$0`. Each one arrives as the
    /// single word it is here, by the same rule as the script.
    pub fn shell_c_args(&self, script: &str, args: &[&str]) -> Command {
        let mut cmd = self.shell_c(script);
        for arg in args {
            match self {
                Self::Wsl { .. } => cmd.arg(arg),
                Self::Ssh { .. } => cmd.arg(posix_quote(arg)),
            };
        }
        cmd
    }
}

/// `program` with `args`: run on this machine when there is no `launcher`, or
/// on the host it reaches.
///
/// Through a launcher the tokens are re-split by the host's login shell, so
/// each one is POSIX-quoted to arrive intact (a simple token passes unchanged);
/// a native-Windows host's shell is not POSIX, see [`launch_on_windows`].
/// Nothing is added: a multiplexer's own flags — tmux's `-L <socket>` among
/// them — are its adapter's to write, so a launcher carries any adapter's
/// command line exactly as the adapter wrote it.
pub fn launch(launcher: Option<&HostLauncher>, program: &str, args: &[&str]) -> Command {
    match launcher {
        None => {
            let mut cmd = Command::new(program);
            cmd.args(args);
            cmd
        }
        Some(launcher) => {
            let mut cmd = launcher.command();
            for token in std::iter::once(&program).chain(args) {
                cmd.arg(posix_quote(token));
            }
            cmd
        }
    }
}

/// [`launch`] on a native-Windows host, whose login shell — `cmd.exe` by
/// Windows OpenSSH's default, PowerShell when configured — re-reads the line
/// ssh hands it. No one quoting survives both: `cmd.exe` ignores the single
/// quotes [`posix_quote`] writes, so a `|` inside them starts a pipe (issue
/// #1460).
///
/// Words every such shell passes through unchanged (`is_bare_windows_word`)
/// go as they are, which keeps a control-mode attach and a `send-paste`'s
/// base64 at their own length. Any other line runs as an encoded PowerShell
/// script, whose base64 no shell reads as syntax. The script starts `program`
/// itself with the arguments as one command line quoted for the C runtime
/// ([`windows_arg_quote`]): PowerShell 5.1's own native-argument quoting drops
/// an empty argument and splits one at an embedded `"`. The child inherits
/// stdin, stdout and stderr, so its bytes reach this machine untouched and its
/// exit status is the script's. Progress output is silenced, since PowerShell
/// writes it to stderr as a CLIXML document that a multiplexer whose stderr
/// is its failure channel would read as an error.
pub fn launch_on_windows(
    launcher: &HostLauncher,
    program: &str,
    args: &[&str],
) -> Result<Command, CommandLineTooLong> {
    let mut cmd = launcher.command();
    let words = || std::iter::once(&program).chain(args);
    // `cmd.exe` expands a `%name%` pair, so a line has at most one `%` bare.
    let percents: usize = words().map(|w| w.matches('%').count()).sum();
    if percents <= 1 && words().all(|w| is_bare_windows_word(w)) {
        cmd.args(words());
        return Ok(cmd);
    }
    let line = args
        .iter()
        .map(|a| windows_arg_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "$ProgressPreference = 'SilentlyContinue'\n\
         $s = New-Object System.Diagnostics.ProcessStartInfo\n\
         $s.FileName = {}\n\
         $s.Arguments = {}\n\
         $s.UseShellExecute = $false\n\
         $p = [System.Diagnostics.Process]::Start($s)\n\
         $p.WaitForExit()\n\
         exit $p.ExitCode",
        powershell_quote(program),
        powershell_quote(&line)
    );
    cmd.args(encoded_powershell("-NoLogo -NoProfile -NonInteractive", &script)?.split(' '));
    Ok(cmd)
}

/// Whether `word` reaches a program unchanged, unquoted, through each login
/// shell a Windows OpenSSH server may run a command in: `cmd.exe`,
/// PowerShell and a POSIX `sh`. Not `,`, which PowerShell reads as an array
/// and passes as two arguments.
fn is_bare_windows_word(word: &str) -> bool {
    !word.is_empty()
        && word.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '+' | '%')
        })
}

/// One argument as a Windows program's C runtime reads it back out of the
/// single command line `CreateProcess` hands it — the MSVC rules psmux, git
/// and every Rust program split argv by. Quoted only when it has to be (empty,
/// whitespace or a `"`); inside the quotes a `"`, and the backslashes before
/// it or before the closing quote, are escaped.
pub fn windows_arg_quote(s: &str) -> String {
    if !s.is_empty() && !s.contains([' ', '\t', '\n', '\x0b', '"']) {
        return s.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in s.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Build a `wsl.exe -d <distro>` [`Command`], ready for the caller to append
/// the in-distro command and its arguments.
///
/// This is the WSL analogue of [`ssh_command`], but `wsl.exe`'s argument
/// forwarding is subtly different from ssh's plain space-join (observed against
/// current `wsl.exe`; callers that need argv to arrive verbatim bypass the
/// shell entirely by appending `-e`/`--exec` — see `git::remote::host_shell_c`):
///
/// - a **whitespace-free** token reaches the in-distro shell for interpretation
///   exactly like over ssh — POSIX-quote it the same way (the shell strips the
///   quotes; metacharacters like `%(…)` survive);
/// - an argument **containing whitespace** is preserved as a single word — do
///   *not* pre-quote a multi-word `sh -c` script for WSL, or the quotes arrive
///   literally and the shell treats the whole blob as one command name (see
///   `git::remote::host_shell_c`, which branches on this).
///
/// No `--` separator is used (none of thurbox's commands start with a `-`,
/// matching the SSH path which also omits it).
///
/// `wsl.exe` inherits the **caller's** current directory and tries to `chdir`
/// to the same path inside the target distro — which fails when thurbox itself
/// runs inside *another* WSL distro (the caller's cwd, e.g. `/home/me/repo`,
/// doesn't exist on the target). That failure prints a `WSL Relay ERROR:
/// CreateProcessCommon chdir(...) failed` on stderr and corrupts the tmux
/// control-mode handshake, so the agent window never launches.
///
/// A Unix caller therefore passes `--cd /` (a landing dir every distro has),
/// which is safe because every caller overrides the real working dir anyway
/// (`git -C <path>`, tmux `new-window -c <path>`). We use `--cd /` rather than
/// only pinning the child's `current_dir("/")`: when the caller distro's name
/// is a **prefix of a sibling distro** (e.g. calling into `MagicDebian` from a
/// host that also has `MagicDebianPerso`), `wsl.exe`'s cwd back-translation
/// mangles the pinned path into `<sibling-suffix>` + cwd — producing
/// `chdir(Perso/home/…)` — so `current_dir("/")` alone does *not* suppress the
/// error. `--cd /` bypasses that translation entirely. `--cd` is a Store/WSL2
/// flag; the Unix-caller case is always WSL2 (thurbox is running inside a
/// distro), so the legacy Windows-10-inbox concern doesn't apply here. A native
/// Windows caller keeps the inherit behavior (its `C:\…` cwd maps to
/// `/mnt/c/…` under default automount; there is no universally-valid Windows
/// pin, and `--cd` would trade this edge case for a hard legacy failure).
pub fn wsl_command(distro: &str) -> Command {
    let mut cmd = wsl_exe();
    cmd.arg("-d").arg(distro);
    #[cfg(unix)]
    cmd.arg("--cd").arg("/");
    cmd
}

/// `wsl.exe` with no arguments yet, kept off the interface's terminal.
///
/// A `wsl.exe` child takes keyboard input from the console it is attached to
/// even when its stdin, stdout and stderr are all redirected. Measured on
/// Windows 11: a console reader received 0 of 8 keys while `wsl.exe -d <distro>
/// sleep 40` ran beside it, and 8 of 8 when the same child was started with
/// `CREATE_NO_WINDOW`. A control-mode connection is such a child for as long as
/// a WSL session is attached, so the interface stopped answering the keyboard
/// the moment one connected. Every `wsl.exe` thurbox starts talks to it over
/// pipes only, so none of them needs the terminal.
///
/// Inside a WSL distro, where interop puts `wsl.exe` on `PATH`, the same child
/// is started in a session of its own, so it has no controlling terminal to
/// read either.
pub fn wsl_exe() -> Command {
    let mut cmd = Command::new("wsl.exe");
    off_the_terminal(&mut cmd);
    cmd
}

#[cfg(windows)]
fn off_the_terminal(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    /// The child gets a console of its own, with no window, instead of ours.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(unix)]
fn off_the_terminal(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `setsid` is async-signal-safe, which is all `pre_exec` asks. It
    // can only fail for a process group leader, which a freshly forked child is
    // not, so its result is not checked.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_powershell_launch_is_39_plus_the_base64_of_its_utf16_script() {
        for n in [0, 1, 2, 3, 462, 1000, 3000] {
            let line = powershell_launcher(&"a".repeat(n)).expect("under the limit");
            assert_eq!(line.len(), 39 + 4 * (2 * n).div_ceil(3), "n = {n}");
        }
    }

    /// The last script that fits, and the first that does not: refused with
    /// the length it would have had.
    #[test]
    fn a_powershell_launch_past_the_windows_limit_is_refused() {
        let fits = (WINDOWS_CMD_LINE_MAX - 39) / 4 * 3 / 2;
        assert!(powershell_launcher(&"a".repeat(fits)).is_ok());
        let error = powershell_launcher(&"a".repeat(fits + 1)).unwrap_err();
        assert_eq!(error.max, WINDOWS_CMD_LINE_MAX);
        assert_eq!(error.len, 39 + 4 * (2 * (fits + 1)).div_ceil(3));
        assert!(error.len > WINDOWS_CMD_LINE_MAX);
        assert_eq!(
            error.to_string(),
            format!("command line is {} chars, Windows limit is 8191", error.len)
        );
    }

    /// The cases PowerShell 5.1's own argument passing gets wrong, measured
    /// against Windows PowerShell 5.1: an empty argument vanished, `a"b`
    /// arrived as `ab`, and `a b\` as `a b"`.
    #[test]
    fn a_windows_argument_is_quoted_for_the_c_runtime() {
        for (arg, quoted) in [
            ("plain", "plain"),
            ("C:\\a\\b", "C:\\a\\b"),
            ("#{pane_id}|#{window_name}", "#{pane_id}|#{window_name}"),
            ("", "\"\""),
            ("a b", "\"a b\""),
            ("a\"b", "\"a\\\"b\""),
            ("a b\\", "\"a b\\\\\""),
            ("a\\\"b", "\"a\\\\\\\"b\""),
        ] {
            assert_eq!(windows_arg_quote(arg), quoted, "{arg:?}");
        }
    }

    fn windows_ssh() -> HostLauncher {
        HostLauncher::Ssh {
            destination: "me@winbox".into(),
            ssh_opts: Vec::new(),
        }
    }

    fn remote_words(cmd: &Command) -> Vec<String> {
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let dest = args.iter().position(|a| a == "me@winbox").expect("dest");
        args[dest + 1..].to_vec()
    }

    /// Words every Windows login shell passes through go unchanged, so a
    /// control-mode attach keeps its stdin on psmux itself and a `send-paste`
    /// keeps its base64 payload at its own length.
    #[test]
    fn a_line_of_plain_words_reaches_a_windows_host_as_it_is() {
        for args in [
            &["-L", "thurbox", "-C", "attach-session", "-t", "thurbox"][..],
            &["-L", "thurbox", "send-paste", "-t", "%3", "aGk+Lz0="][..],
        ] {
            let cmd = launch_on_windows(&windows_ssh(), "psmux", args).expect("fits");
            let mut expected = vec!["psmux".to_string()];
            expected.extend(args.iter().map(|a| a.to_string()));
            assert_eq!(remote_words(&cmd), expected);
        }
    }

    /// `cmd.exe` expands a `%name%` pair, and `,` is a PowerShell array: a
    /// line with either is encoded, never sent bare.
    #[test]
    fn a_line_any_windows_shell_could_read_as_syntax_is_encoded() {
        for args in [
            &["swap-pane", "-s", "%1", "-t", "%2"][..],
            &["display-message", "-p", "a,b"][..],
            &["list-windows", "-F", "#{pane_id}|#{window_name}"][..],
        ] {
            let cmd = launch_on_windows(&windows_ssh(), "psmux", args).expect("fits");
            let words = remote_words(&cmd);
            assert_eq!(
                words[..5],
                [
                    "powershell.exe",
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-EncodedCommand"
                ],
                "{args:?}"
            );
            assert_eq!(words.len(), 6, "{args:?}");
        }
    }

    /// How a host is reached is its kind and nothing else: neither its OS nor
    /// its multiplexer changes the launcher, and the launcher names neither —
    /// no multiplexer binary and no `-L`, which are the transport's to add.
    #[test]
    fn a_launcher_follows_the_hosts_kind_alone() {
        use crate::session::{HostDef, HostKind, Multiplexer, Platform};
        for kind in [HostKind::Ssh, HostKind::Wsl] {
            for platform in [None, Some(Platform::Posix), Some(Platform::Windows)] {
                for mux in std::iter::once(None).chain(Multiplexer::ALL.map(Some)) {
                    let host = HostDef {
                        name: "box".into(),
                        kind,
                        destination: "me@box".into(),
                        ssh_opts: vec!["-p".into(), "2222".into()],
                        platform,
                        multiplexer: mux.map(|m| m.name().to_string()),
                        ..Default::default()
                    };
                    let launcher = HostLauncher::for_host(&host);
                    let expected = match kind {
                        HostKind::Ssh => HostLauncher::Ssh {
                            destination: "me@box".into(),
                            ssh_opts: vec!["-p".into(), "2222".into()],
                        },
                        HostKind::Wsl => HostLauncher::Wsl {
                            distro: "box".into(),
                        },
                    };
                    assert_eq!(launcher, expected);
                    let argv: Vec<String> = launcher
                        .command()
                        .get_args()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect();
                    for word in ["-L"]
                        .into_iter()
                        .chain(Multiplexer::ALL.map(Multiplexer::name))
                    {
                        assert!(!argv.iter().any(|a| a == word), "{argv:?} names {word}");
                    }
                }
            }
        }
    }

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
    fn ssh_command_sets_program_opts_hardening_and_destination() {
        let cmd = ssh_command("me@box", &["-p".into(), "2222".into()]);
        assert_eq!(cmd.get_program().to_string_lossy(), "ssh");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // Caller opts first, the appended set (hardening + multiplexing when
        // the machine has an `~/.ssh`), destination last.
        let mut expected: Vec<&str> = vec!["-p", "2222"];
        expected.extend(ssh_appended_opts());
        expected.push("me@box");
        assert_eq!(args, expected);
    }

    #[test]
    fn ssh_command_hardening_follows_user_opts_so_user_wins() {
        // ssh honors the first occurrence of an option, so a user-set
        // ConnectTimeout must precede ours in the arg list.
        let cmd = ssh_command("me@box", &["-o".into(), "ConnectTimeout=30".into()]);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let user = args.iter().position(|a| a == "ConnectTimeout=30").unwrap();
        let ours = args.iter().position(|a| a == "ConnectTimeout=5").unwrap();
        assert!(user < ours, "user opt must precede hardening opt");
        assert!(args.iter().any(|a| a == "BatchMode=yes"));
    }

    /// The flag [`wsl_exe`] starts every `wsl.exe` with keeps the child out of
    /// this process's console, the one whose keyboard an attached WSL session
    /// used to take. Asked of the console itself: every process attached to it
    /// is in `GetConsoleProcessList`. `ping` stands in for `wsl.exe`, which a
    /// runner need not have. A child started without the flag is checked too,
    /// so a console that lists nothing cannot make this pass.
    #[cfg(windows)]
    #[test]
    fn a_child_kept_off_the_terminal_is_not_attached_to_our_console() {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        }
        fn attached() -> Option<Vec<u32>> {
            let mut list = vec![0u32; 1024];
            // SAFETY: `list` holds the `count` entries the call may write.
            let n = unsafe { GetConsoleProcessList(list.as_mut_ptr(), list.len() as u32) };
            let n = usize::try_from(n)
                .ok()
                .filter(|n| (1..=list.len()).contains(n))?;
            list.truncate(n);
            Some(list)
        }
        if attached().is_none() {
            eprintln!("skipping: this process has no console");
            return;
        }
        let start = |kept_off: bool| {
            let mut cmd = Command::new("ping");
            cmd.args(["-n", "30", "127.0.0.1"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            if kept_off {
                off_the_terminal(&mut cmd);
            }
            cmd.spawn().expect("spawn ping")
        };
        // A child joins the console while it starts up, after `spawn` has
        // returned, so the list is read once the unflagged child is on it —
        // started second, so the flagged one has had at least as long.
        let mut kept_off = start(true);
        let mut sharing = start(false);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let list = loop {
            let list = attached().expect("the console lists its processes");
            if list.contains(&sharing.id()) || std::time::Instant::now() > deadline {
                break list;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        for child in [&mut sharing, &mut kept_off] {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(
            list.contains(&sharing.id()),
            "a child started without the flag should share this console: {list:?}"
        );
        assert!(
            !list.contains(&kept_off.id()),
            "a child kept off the terminal is attached to this console: {list:?}"
        );
    }

    #[test]
    fn wsl_command_sets_program_distro_and_neutral_cwd() {
        let cmd = wsl_command("Ubuntu");
        assert_eq!(cmd.get_program().to_string_lossy(), "wsl.exe");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // A Unix caller passes `--cd /` so wsl.exe doesn't inherit (and fail to
        // chdir to) a caller cwd that doesn't exist in the target distro.
        // Pinning only the child cwd is insufficient — wsl.exe back-translates
        // it through the caller-distro name and, with a prefix-sibling distro,
        // yields a mangled `chdir(<suffix>/…)`; `--cd /` bypasses that.
        #[cfg(unix)]
        assert_eq!(args, ["-d", "Ubuntu", "--cd", "/"]);
        #[cfg(windows)]
        assert_eq!(args, ["-d", "Ubuntu"]);
        // The child cwd is left to the OS default; the translation control is
        // the `--cd` flag, not the caller's process cwd.
        assert_eq!(cmd.get_current_dir(), None);
    }
}
