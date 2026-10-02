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
}

/// `program` with `args`: run on this machine when there is no `launcher`, or
/// on the host it reaches.
///
/// Through a launcher the tokens are re-split by the host's login shell, so
/// each one is POSIX-quoted to arrive intact (a simple token passes unchanged).
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
            let n = usize::try_from(n).ok().filter(|n| (1..=list.len()).contains(n))?;
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
        let mut sharing = start(false);
        let mut kept_off = start(true);
        let list = attached().expect("the console lists its processes");
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
