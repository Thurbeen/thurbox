use std::process::Command;

use crate::Platform;

pub const SETUP_FAILURE_MARKER: &str = "thurbox: setup step ";

const SETUP_ARGV0: &str = "thurbox-setup";

pub fn wrap_with_setup(setup: &[String], command: &str, args: &[String]) -> (String, Vec<String>) {
    if setup.is_empty() {
        return (command.to_string(), args.to_vec());
    }
    let mut script = String::from(
        "thurbox_step=0\n\
         trap 'thurbox_status=$?; if [ \"$thurbox_status\" -ne 0 ]; then \
         printf \"\\n%s%s failed (exit %s)\\n\" \"$thurbox_marker\" \"$thurbox_step\" \
         \"$thurbox_status\" >&2; fi' EXIT\n",
    );
    script.push_str(&format!(
        "thurbox_marker='{SETUP_FAILURE_MARKER}'\nset -e\n"
    ));
    for (i, step) in setup.iter().enumerate() {
        script.push_str(&format!("thurbox_step={}\n{step}\n", i + 1));
    }
    script.push_str("trap - EXIT\nexec \"$@\"\n");
    let wrapped = [
        "-c".to_string(),
        script,
        SETUP_ARGV0.to_string(),
        command.to_string(),
    ]
    .into_iter()
    .chain(args.iter().cloned())
    .collect();
    ("sh".to_string(), wrapped)
}

pub fn local_platform() -> Platform {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(simulated) = SIMULATED_LOCAL.with(std::cell::Cell::get) {
        return simulated;
    }
    Platform::local()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static SIMULATED_LOCAL: std::cell::Cell<Option<Platform>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn simulate_local<T>(platform: Platform, f: impl FnOnce() -> T) -> T {
    let saved = SIMULATED_LOCAL.with(|s| s.replace(Some(platform)));
    let out = f();
    SIMULATED_LOCAL.with(|s| s.set(saved));
    out
}

fn is_safe_shell_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | ',')
}

pub fn posix_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(is_safe_shell_char) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

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

pub const SSH_MULTIPLEX_OPTS: [&str; 6] = [
    "-o",
    "ControlMaster=auto",
    "-o",
    "ControlPersist=60",
    "-o",
    "ControlPath=~/.ssh/thurbox-%C",
];

fn ssh_dir_exists() -> bool {
    static EXISTS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *EXISTS.get_or_init(|| {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| std::path::Path::new(&home).join(".ssh").is_dir())
            .unwrap_or(false)
    })
}

pub fn ssh_appended_opts() -> Vec<&'static str> {
    let mut opts: Vec<&'static str> = SSH_HARDENING_OPTS.to_vec();
    if ssh_dir_exists() {
        opts.extend(SSH_MULTIPLEX_OPTS);
    }
    opts
}

pub fn ssh_command(destination: &str, ssh_opts: &[String]) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(ssh_opts);
    cmd.args(ssh_appended_opts());
    cmd.arg(destination);
    cmd
}

pub fn powershell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub fn powershell_launcher(script: &str) -> Result<String, CommandLineTooLong> {
    encoded_powershell("-NoLogo", script)
}

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

pub const WINDOWS_CMD_LINE_MAX: usize = 8191;

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

pub fn strip_telemetry_env(cmd: &mut Command) {
    for (var, _) in std::env::vars_os() {
        if is_telemetry_var(&var.to_string_lossy()) {
            cmd.env_remove(var);
        }
    }
}

pub(crate) fn is_telemetry_var(var: &str) -> bool {
    var.get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("OTEL_"))
        || var.eq_ignore_ascii_case("TRACEPARENT")
        || var.eq_ignore_ascii_case("TRACESTATE")
}

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
    pub fn for_host(host: &crate::HostDef) -> Self {
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

    pub fn program(&self) -> &'static str {
        match self {
            Self::Ssh { .. } => "ssh",
            Self::Wsl { .. } => "wsl.exe",
        }
    }

    pub fn command(&self) -> Command {
        let mut cmd = match self {
            Self::Ssh {
                destination,
                ssh_opts,
            } => ssh_command(destination, ssh_opts),
            Self::Wsl { distro } => wsl_command(distro),
        };
        strip_telemetry_env(&mut cmd);
        cmd
    }

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

pub const WSL_PROFILE_EXEC: &str =
    "[ -r /etc/profile ] && . /etc/profile </dev/null >/dev/null 2>&1; \
     [ -r \"$HOME/.profile\" ] && . \"$HOME/.profile\" </dev/null >/dev/null 2>&1; exec \"$@\"";

pub fn launch(launcher: Option<&HostLauncher>, program: &str, args: &[&str]) -> Command {
    match launcher {
        None => {
            let mut cmd = Command::new(program);
            cmd.args(args);
            cmd
        }
        // `wsl.exe --exec` hands argv over verbatim; a small `sh` first reads the login profile for its `PATH`.
        Some(launcher @ HostLauncher::Wsl { .. }) => {
            let mut cmd = launcher.command();
            cmd.args(["--exec", "sh", "-c", WSL_PROFILE_EXEC, "sh", program])
                .args(args);
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
    let script = format!(
        "$ProgressPreference = 'SilentlyContinue'\n{}",
        start_process_script(&powershell_quote(program), args, "")
    );
    cmd.args(encoded_powershell("-NoLogo -NoProfile -NonInteractive", &script)?.split(' '));
    Ok(cmd)
}

pub fn windows_program_script<S: AsRef<str>>(
    program: &str,
    args: &[S],
    dir: Option<&str>,
    path_prepend: &[String],
    prelude: &str,
    after: &str,
) -> String {
    let argv = args
        .iter()
        .map(|a| powershell_quote(a.as_ref()))
        .collect::<Vec<_>>()
        .join(", ");
    let dir = dir
        .map(|d| format!("$s.WorkingDirectory={}\n", powershell_quote(d)))
        .unwrap_or_default();
    // The script rides the command line base64'd from UTF-16, inside WINDOWS_CMD_LINE_MAX.
    format!(
        "$ProgressPreference='SilentlyContinue'\n\
         {path}{prelude}\
         {WINDOWS_QUOTERS}\
         $v=@({argv})\n\
         try{{$global:LASTEXITCODE=$null\n\
         $p=(Get-Command {program} -CommandType Application,ExternalScript -EA Stop|select -First 1).Source\n\
         $e=[IO.Path]::GetExtension($p)\n\
         if($e-in'.cmd','.bat'){{$f=$env:ComSpec;$a='/d /v:off /s /c \"\"'+$p+'\" '+(($v|%{{&$k $_}})-join' ')+'\"'}}\
         elseif($e-eq'.ps1'){{$f=(Get-Command powershell.exe).Source;$a='-NoProfile -ExecutionPolicy Bypass -File '+(&$q $p)+' '+(($v|%{{&$q $_}})-join' ')}}\
         else{{$f=$p;$a=($v|%{{&$q $_}})-join' '}}\n\
         $s=New-Object Diagnostics.ProcessStartInfo $f,$a\n\
         $s.UseShellExecute=$false\n\
         {dir}\
         $x=[Diagnostics.Process]::Start($s)\n\
         $x.WaitForExit()\n\
         $global:LASTEXITCODE=$x.ExitCode\n\
         }}finally{{\n\
         {after}}}\n\
         exit $x.ExitCode",
        path = windows_path_prepend(path_prepend),
        program = powershell_quote(program),
    )
}

pub const WINDOWS_QUOTERS: &str = "$q={param($a)if($a-and$a-notmatch'[\\s\"]'){return $a};$o='\"';$b=0;foreach($h in $a.ToCharArray()){if($h-eq'\\'){$b++}elseif($h-eq'\"'){$o+='\\'*(2*$b+1)+'\"';$b=0}else{$o+='\\'*$b+$h;$b=0}};$o+'\\'*(2*$b)+'\"'}
$k={param($a)$o='\"';$b=0;foreach($h in $a.ToCharArray()){if($h-eq'\\'){$b++;$o+='\\';continue};if($h-eq'\"'){$o+='\\'*$b+'\"'}elseif($h-eq'%'-or$h-eq[char]13){$o+='%%cd:~,'};$b=0;if($h-eq[char]10){$o+=' '}else{$o+=$h}};$o+'\\'*$b+'\"'}
";

pub fn windows_path_prepend(dirs: &[String]) -> String {
    if dirs.is_empty() {
        return String::new();
    }
    let items: Vec<String> = dirs
        .iter()
        .map(|d| match d.strip_prefix('~') {
            Some(rest) => format!("($env:USERPROFILE + {})", powershell_quote(rest)),
            None => powershell_quote(d),
        })
        .collect();
    format!(
        "$env:Path = (@({}) -join ';') + ';' + $env:Path\n",
        items.join(", ")
    )
}

pub fn local_powershell(script: &str) -> Command {
    use base64::Engine as _;
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut cmd = Command::new("powershell.exe");
    off_the_terminal(&mut cmd);
    cmd.args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
        .arg(base64::engine::general_purpose::STANDARD.encode(utf16));
    strip_telemetry_env(&mut cmd);
    cmd
}

fn start_process_script<S: AsRef<str>>(file_name: &str, args: &[S], extra: &str) -> String {
    let line = args
        .iter()
        .map(|a| windows_arg_quote(a.as_ref()))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "$s = New-Object System.Diagnostics.ProcessStartInfo\n\
         $s.FileName = {file_name}\n\
         $s.Arguments = {}\n\
         $s.UseShellExecute = $false\n\
         {extra}\
         $p = [System.Diagnostics.Process]::Start($s)\n\
         $p.WaitForExit()\n\
         exit $p.ExitCode",
        powershell_quote(&line)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostShell {
    Posix,
    PowerShell,
}

impl HostShell {
    pub fn for_platform(platform: crate::Platform) -> Self {
        match platform {
            crate::Platform::Posix => Self::Posix,
            crate::Platform::Windows => Self::PowerShell,
        }
    }

    pub fn for_host(host: &crate::HostDef) -> Self {
        Self::for_platform(host.platform())
    }

    pub fn local() -> Self {
        Self::for_platform(local_platform())
    }

    pub fn quote(self, word: &str) -> String {
        match self {
            Self::Posix => posix_quote(word),
            Self::PowerShell => powershell_quote(word),
        }
    }

    pub fn run_script(self, launcher: &HostLauncher, script: &str) -> Command {
        match self {
            Self::Posix => launcher.shell_c(script),
            Self::PowerShell => {
                use base64::Engine as _;
                let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
                let mut cmd = launcher.command();
                cmd.args(["powershell", "-NoProfile", "-EncodedCommand"])
                    .arg(base64::engine::general_purpose::STANDARD.encode(utf16));
                cmd
            }
        }
    }

    pub fn run_on_host(host: &crate::HostDef, script: &str) -> Command {
        Self::for_host(host).run_script(&HostLauncher::for_host(host), script)
    }

    pub fn launch(
        self,
        launcher: &HostLauncher,
        program: &str,
        args: &[&str],
    ) -> Result<Command, CommandLineTooLong> {
        match self {
            Self::Posix => Ok(launch(Some(launcher), program, args)),
            Self::PowerShell => launch_on_windows(launcher, program, args),
        }
    }

    pub fn default_shell(self, remote: bool) -> String {
        match (remote, self) {
            (false, Self::PowerShell) => {
                std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
            }
            (false, Self::Posix) => {
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
            }
            (true, Self::PowerShell) => "powershell".to_string(),
            (true, Self::Posix) => "/bin/sh".to_string(),
        }
    }

    pub fn wrap_setup(
        self,
        setup: &[String],
        command: &str,
        args: &[String],
    ) -> Result<(String, Vec<String>), CommandLineTooLong> {
        if setup.is_empty() || self == Self::Posix {
            return Ok(wrap_with_setup(setup, command, args));
        }
        let marker = SETUP_FAILURE_MARKER;
        let mut script = format!(
            "$ErrorActionPreference = 'Stop'\n\
             $global:thurbox_step = 0\n\
             $global:thurbox_reported = $false\n\
             function thurbox_fail($code) {{ $global:thurbox_reported = $true; \
             [Console]::Error.WriteLine(\"`n{marker}$global:thurbox_step failed (exit $code)\"); \
             exit $code }}\n\
             try {{\n"
        );
        for (i, step) in setup.iter().enumerate() {
            let n = i + 1;
            script.push_str(&format!(
                "$global:thurbox_step = {n}\n\
                 $global:LASTEXITCODE = 0\n\
                 try {{\n{step}\n}} catch {{ [Console]::Error.WriteLine($_); thurbox_fail 1 }}\n\
                 if ($LASTEXITCODE) {{ thurbox_fail $LASTEXITCODE }}\n"
            ));
        }
        script.push_str(&format!(
            "$global:thurbox_step = 0\n\
             }} finally {{\n\
             if ($global:thurbox_step -and -not $global:thurbox_reported) {{ \
             [Console]::Error.WriteLine(\"`n{marker}$global:thurbox_step failed (exit $LASTEXITCODE)\") }}\n\
             }}\n\
             $ErrorActionPreference = 'Continue'\n\
             $thurbox_cmd = Get-Command -Name {} -CommandType Application, ExternalScript \
             -ErrorAction Stop | Select-Object -First 1\n\
             $thurbox_agent = $thurbox_cmd.Source\n\
             $thurbox_lead = ''\n\
             if ($thurbox_cmd.CommandType -eq 'ExternalScript') {{ \
             $thurbox_lead = '-NoLogo -File \"' + $thurbox_agent + '\" '; \
             $thurbox_agent = (Get-Command -Name 'powershell.exe' -CommandType Application \
             | Select-Object -First 1).Source }}\n{}",
            powershell_quote(command),
            start_process_script(
                "$thurbox_agent",
                args,
                "$s.Arguments = $thurbox_lead + $s.Arguments\n\
                 $s.WorkingDirectory = (Get-Location -PSProvider FileSystem).ProviderPath\n",
            )
        ));
        let line = powershell_launcher(&script)?;
        let mut words = line.split(' ').map(str::to_string);
        let program = words.next().unwrap_or_default();
        Ok((program, words.collect()))
    }

    pub fn in_home(self, command: &str) -> String {
        match self {
            Self::Posix => format!("cd \"$HOME\" && {command}"),
            Self::PowerShell => format!(
                "Set-Location -LiteralPath $HOME -ErrorAction Stop\n{command}\n\
                 if (-not $?) {{ if ($LASTEXITCODE) {{ exit $LASTEXITCODE }}; exit 1 }}\n\
                 exit $LASTEXITCODE"
            ),
        }
    }

    pub fn hook_script(self, command: &str, cwd: Option<&str>, env: &[(String, String)]) -> String {
        let mut script = String::new();
        match self {
            Self::Posix => {
                for (k, v) in env {
                    script.push_str(&format!("export {k}={}\n", posix_quote(v)));
                }
                if let Some(dir) = cwd {
                    script.push_str(&format!("cd {} || exit\n", posix_quote(dir)));
                }
                script.push_str(command);
            }
            Self::PowerShell => {
                for (k, v) in env {
                    script.push_str(&format!("$env:{k} = {}\n", powershell_quote(v)));
                }
                if let Some(dir) = cwd {
                    script.push_str(&format!(
                        "Set-Location -LiteralPath {} -ErrorAction Stop\n",
                        powershell_quote(dir)
                    ));
                }
                script.push_str(&format!(
                    "$global:LASTEXITCODE = 0\n{command}\n\
                     if (-not $?) {{ if ($LASTEXITCODE) {{ exit $LASTEXITCODE }}; exit 1 }}\n\
                     exit $LASTEXITCODE"
                ));
            }
        }
        script
    }
}

fn is_bare_windows_word(word: &str) -> bool {
    !word.is_empty()
        && word.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '+' | '%')
        })
}

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

pub fn wsl_command(distro: &str) -> Command {
    let mut cmd = wsl_exe();
    cmd.arg("-d").arg(distro);
    #[cfg(unix)]
    cmd.arg("--cd").arg("/");
    cmd
}

pub fn wsl_exe() -> Command {
    let mut cmd = Command::new("wsl.exe");
    off_the_terminal(&mut cmd);
    cmd
}

#[cfg(windows)]
fn off_the_terminal(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
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
    #[test]
    fn a_command_on_a_place_carries_no_telemetry() {
        std::env::set_var("OTEL_SERVICE_NAME", "fleet");
        std::env::set_var(
            "TRACEPARENT",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        );
        for launcher in [
            HostLauncher::Ssh {
                destination: "me@devbox".into(),
                ssh_opts: Vec::new(),
            },
            HostLauncher::Wsl {
                distro: "Ubuntu".into(),
            },
        ] {
            let cmd = launcher.shell_c("true");
            let removed: Vec<String> = cmd
                .get_envs()
                .filter(|(_, value)| value.is_none())
                .map(|(name, _)| name.to_string_lossy().into_owned())
                .collect();
            for var in ["OTEL_SERVICE_NAME", "TRACEPARENT"] {
                assert!(
                    removed.iter().any(|r| r == var),
                    "{launcher:?}: {removed:?}"
                );
            }
        }
    }

    #[test]
    fn a_wsl_launch_hands_each_word_to_the_profile_script() {
        let launcher = HostLauncher::Wsl {
            distro: "Ubuntu".into(),
        };
        let cmd = launch(
            Some(&launcher),
            "/opt/my tools/fleet",
            &["send", "it's done"],
        );
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let exec = argv.iter().position(|a| a == "--exec").expect("--exec");
        assert_eq!(
            &argv[exec + 1..],
            [
                "sh",
                "-c",
                WSL_PROFILE_EXEC,
                "sh",
                "/opt/my tools/fleet",
                "send",
                "it's done"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_wsl_profile_script_lends_the_path_and_keeps_stdout_clean() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().expect("home");
        let bin = home.path().join("my bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let tool = bin.join("say");
        std::fs::write(&tool, "#!/bin/sh\nprintf '%s|' \"$@\"\n").expect("tool");
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::write(
            home.path().join(".profile"),
            format!(
                "echo profile-noise\necho profile-err >&2\nPATH=\"{}:$PATH\"\n",
                bin.display()
            ),
        )
        .expect("profile");
        let out = Command::new("sh")
            .args(["-c", WSL_PROFILE_EXEC, "sh", "say", "a b", "it's"])
            .env("HOME", home.path())
            .env("PATH", "/usr/bin:/bin")
            .output()
            .expect("sh");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "a b|it's|");
        assert!(out.stderr.is_empty(), "{out:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_wsl_profile_script_needs_no_etc_profile_and_leaves_stdin_alone() {
        use std::io::Write;
        let home = tempfile::tempdir().expect("home");
        std::fs::write(home.path().join(".profile"), "read eaten\n").expect("profile");
        let without_etc = WSL_PROFILE_EXEC.replace("/etc/profile", "/nonexistent/etc/profile");
        let mut child = Command::new("sh")
            .args(["-c", &without_etc, "sh", "cat"])
            .env("HOME", home.path())
            .env("PATH", "/usr/bin:/bin")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("sh");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(b"first frame\nsecond frame\n")
            .expect("write");
        let out = child.wait_with_output().expect("sh");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "first frame\nsecond frame\n"
        );
    }
    use super::*;

    #[test]
    fn a_windows_path_prepend_reads_tilde_as_the_profile() {
        assert_eq!(windows_path_prepend(&[]), "");
        assert_eq!(
            windows_path_prepend(&["~/rp2/win".to_string(), "C:/it's/bin".to_string()]),
            "$env:Path = (@(($env:USERPROFILE + '/rp2/win'), 'C:/it''s/bin') -join ';') + ';' + $env:Path\n"
        );
    }

    #[test]
    fn a_windows_program_script_quotes_its_program_directory_and_arguments() {
        let script = windows_program_script(
            "o'prog",
            &["it's", "two words"],
            Some("C:/a b/it's"),
            &["~/bin".to_string()],
            "Prelude-Here\n",
            "",
        );
        assert!(script.contains("Get-Command 'o''prog'"), "{script}");
        assert!(script.contains("$v=@('it''s', 'two words')"), "{script}");
        assert!(
            script.contains("$s.WorkingDirectory='C:/a b/it''s'"),
            "{script}"
        );
        assert!(script.contains("($env:USERPROFILE + '/bin')"), "{script}");
        let prelude = script.find("Prelude-Here").expect("prelude");
        assert!(
            prelude < script.find("Get-Command").expect("lookup"),
            "{script}"
        );
    }

    #[test]
    fn each_kind_of_windows_program_gets_its_own_quoting() {
        let script = windows_program_script("shim", &["left&right", "%PATH%"], None, &[], "", "");
        assert!(script.contains("$v=@('left&right', '%PATH%')"), "{script}");
        assert!(
            script.contains("$f=$env:ComSpec;$a='/d /v:off /s /c \"\"'+$p+'\" '+(($v|%{&$k $_})"),
            "{script}"
        );
        assert!(script.contains("else{$f=$p;$a=($v|%{&$q $_})"), "{script}");
        assert!(
            WINDOWS_QUOTERS.contains("$o+='%%cd:~,'"),
            "{WINDOWS_QUOTERS}"
        );
        assert!(
            !WINDOWS_QUOTERS.contains('`'),
            "no PowerShell escape in the blocks"
        );
    }

    fn a_powershell() -> Option<&'static str> {
        if cfg!(windows) {
            return Some("powershell.exe");
        }
        let found = std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("pwsh").is_file()));
        found.then_some("pwsh")
    }

    fn crt_argv(line: &str) -> Vec<String> {
        let chars: Vec<char> = line.chars().collect();
        let (mut args, mut arg, mut inside, mut any, mut i) =
            (Vec::new(), String::new(), false, false, 0);
        while i < chars.len() {
            match chars[i] {
                '\\' => {
                    let mut n = 0;
                    while chars.get(i) == Some(&'\\') {
                        n += 1;
                        i += 1;
                    }
                    if chars.get(i) == Some(&'"') {
                        arg.push_str(&"\\".repeat(n / 2));
                        if n % 2 == 1 {
                            arg.push('"');
                            i += 1;
                        }
                    } else {
                        arg.push_str(&"\\".repeat(n));
                    }
                    continue;
                }
                '"' if inside && chars.get(i + 1) == Some(&'"') => {
                    arg.push('"');
                    i += 1;
                }
                '"' => {
                    inside = !inside;
                    any = true;
                }
                ' ' | '\t' if !inside => {
                    if any || !arg.is_empty() {
                        args.push(std::mem::take(&mut arg));
                    }
                    any = false;
                }
                c => arg.push(c),
            }
            i += 1;
        }
        if any || !arg.is_empty() {
            args.push(arg);
        }
        args
    }

    fn cmd_reads(line: &str) -> (Vec<char>, Vec<String>, String) {
        let (mut outside, mut looked_up, mut out) = (Vec::new(), Vec::new(), String::new());
        let chars: Vec<char> = line.chars().collect();
        let (mut i, mut inside) = (0, false);
        while i < chars.len() {
            match chars[i] {
                '"' => {
                    inside = !inside;
                    out.push('"');
                }
                '%' => match chars[i + 1..].iter().position(|c| *c == '%') {
                    Some(0) | None => out.push('%'),
                    Some(n) => {
                        let name: String = chars[i + 1..i + 1 + n].iter().collect();
                        looked_up.push(name.clone());
                        if name == "cd:~," {
                            i += n + 2;
                        } else {
                            out.push('%');
                            i += 1;
                        }
                        continue;
                    }
                },
                c @ ('&' | '|' | '<' | '>' | '^') if !inside => {
                    outside.push(c);
                    out.push(c);
                }
                c => out.push(c),
            }
            i += 1;
        }
        (outside, looked_up, out)
    }

    #[test]
    fn the_windows_quoters_round_trip_awkward_arguments_in_powershell() {
        use base64::Engine as _;
        let Some(ps) = a_powershell() else {
            eprintln!("skipping: no PowerShell here (pwsh is not on PATH)");
            return;
        };
        let args = [
            "x\" & echo pwned & \"y",
            "%PATH%",
            "!x!",
            "a|b",
            "trail\\",
            "",
            "\"q\"",
            "left&right",
            "with space",
            "a\\\"b",
            "%%cd%%",
            "new\nline",
            "{\"k\":\"v w\",\"p\":\"C:\\\\x\\\\\"}",
        ];
        let script = format!(
            "{WINDOWS_QUOTERS}$v=@({})\nforeach($x in $v){{$e=[Text.Encoding]::UTF8;[Console]::Out.WriteLine([Convert]::ToBase64String($e.GetBytes((& $q $x)))+' '+[Convert]::ToBase64String($e.GetBytes((& $k $x))))}}",
            args.iter().map(|a| powershell_quote(a)).collect::<Vec<_>>().join(", ")
        );
        let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let out = Command::new(ps)
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
            .arg(base64::engine::general_purpose::STANDARD.encode(utf16))
            .output()
            .expect("PowerShell runs");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let lines: Vec<(String, String)> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let (a, b) = l.trim().split_once(' ')?;
                let text = |w: &str| {
                    String::from_utf8(base64::engine::general_purpose::STANDARD.decode(w).ok()?)
                        .ok()
                };
                Some((text(a)?, text(b)?))
            })
            .collect();
        assert_eq!(
            lines.len(),
            args.len(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        for (arg, (q, k)) in args.iter().zip(&lines) {
            assert_eq!(crt_argv(q), [*arg], "$q {arg:?} -> {q}");
            let (outside, looked_up, after) = cmd_reads(k);
            assert!(
                outside.is_empty(),
                "$k {arg:?} -> {k}: {outside:?} outside quotes"
            );
            assert!(
                looked_up.iter().all(|n| n == "cd:~,"),
                "$k {arg:?} -> {k}: looks up {looked_up:?}"
            );
            assert_eq!(
                crt_argv(&after),
                [arg.replace('\n', " ")],
                "$k {arg:?} -> {k}"
            );
        }
    }

    #[test]
    fn the_after_step_runs_even_when_no_program_starts() {
        use base64::Engine as _;
        let Some(ps) = a_powershell() else {
            eprintln!("skipping: no PowerShell here (pwsh is not on PATH)");
            return;
        };
        let after = "[Console]::Out.Write('after-ran:' + (Get-Variable LASTEXITCODE -ValueOnly -ErrorAction Ignore) + ';')\n";
        let run = |program: &str| {
            let script = windows_program_script::<&str>(program, &[], None, &[], "", after);
            let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            Command::new(ps)
                .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
                .arg(base64::engine::general_purpose::STANDARD.encode(utf16))
                .output()
                .expect("PowerShell runs")
        };
        let missing = run("no-such-program-runtime-mux");
        let stdout = String::from_utf8_lossy(&missing.stdout);
        assert!(
            stdout.contains("after-ran:;"),
            "{stdout}{}",
            String::from_utf8_lossy(&missing.stderr)
        );
        assert!(
            !missing.status.success(),
            "a missing program fails the script"
        );
        let found = if cfg!(windows) { "whoami" } else { "true" };
        let ran = run(found);
        let stdout = String::from_utf8_lossy(&ran.stdout);
        assert!(
            stdout.contains("after-ran:0;"),
            "{stdout}{}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }

    #[test]
    fn a_long_argument_rides_the_windows_script_once() {
        let long = "x".repeat(1500);
        let script = windows_program_script("tool.exe", &[long.as_str()], None, &[], "", "");
        assert_eq!(script.matches(long.as_str()).count(), 1, "{script}");
        let launcher = HostLauncher::Ssh {
            destination: "me@win".into(),
            ssh_opts: vec![],
        };
        let cmd = HostShell::PowerShell.run_script(&launcher, &script);
        let line: usize = cmd.get_args().map(|a| a.len() + 1).sum();
        assert!(line <= WINDOWS_CMD_LINE_MAX, "{line} chars");
    }

    #[test]
    fn a_windows_program_is_resolved_and_run_by_its_kind() {
        let script = windows_program_script("codex", &["exec"], None, &[], "", "");
        assert!(script.contains("Get-Command 'codex'"), "{script}");
        for kind in ["'.cmd'", "'.bat'", "'.ps1'"] {
            assert!(script.contains(kind), "{kind}: {script}");
        }
        assert!(
            script.contains("$env:ComSpec") && script.contains("/v:off"),
            "{script}"
        );
    }

    #[test]
    fn the_local_platform_is_the_build_os_unless_simulated() {
        assert_eq!(local_platform(), Platform::local());
        for platform in Platform::ALL {
            assert_eq!(simulate_local(platform, local_platform), platform);
        }
        assert_eq!(local_platform(), Platform::local());
    }

    #[test]
    fn a_powershell_launch_is_39_plus_the_base64_of_its_utf16_script() {
        for n in [0, 1, 2, 3, 462, 1000, 3000] {
            let line = powershell_launcher(&"a".repeat(n)).expect("under the limit");
            assert_eq!(line.len(), 39 + 4 * (2 * n).div_ceil(3), "n = {n}");
        }
    }

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

    #[test]
    fn a_launcher_follows_the_hosts_kind_alone() {
        use crate::{HostDef, HostKind, Multiplexer, Platform};
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
        let mut expected: Vec<&str> = vec!["-p", "2222"];
        expected.extend(ssh_appended_opts());
        expected.push("me@box");
        assert_eq!(args, expected);
    }

    #[test]
    fn ssh_command_hardening_follows_user_opts_so_user_wins() {
        // ssh honors the first occurrence of an option.
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
        // A Windows child joins the console after `spawn` has returned.
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
        // wsl.exe back-translates the caller's cwd through the distro name; `--cd /` bypasses that.
        #[cfg(unix)]
        assert_eq!(args, ["-d", "Ubuntu", "--cd", "/"]);
        #[cfg(windows)]
        assert_eq!(args, ["-d", "Ubuntu"]);
        assert_eq!(cmd.get_current_dir(), None);
    }

    const QUOTING_TABLE: &[&str] = &[
        "plain",
        "",
        "two words",
        "a|b",
        "it's",
        "say \"hi\"",
        "%PATH%",
        "$HOME",
        "`tick`",
        "semi;colon & amp",
        "C:\\dir with space\\",
        "née",
        "line\nbreak",
    ];

    fn ssh() -> HostLauncher {
        HostLauncher::Ssh {
            destination: "me@box".into(),
            ssh_opts: vec![],
        }
    }

    fn remote_line(cmd: &Command) -> String {
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = args
            .iter()
            .position(|a| a == "me@box")
            .expect("destination");
        args[at + 1..].join(" ")
    }

    #[cfg(unix)]
    #[test]
    fn the_quoting_table_survives_a_posix_host_shell() {
        let launch = HostShell::Posix
            .launch(&ssh(), "printf", &[&["%s\\0"], QUOTING_TABLE].concat())
            .expect("POSIX lines are not bounded");
        let out = Command::new("sh")
            .args(["-c", &remote_line(&launch)])
            .output()
            .expect("sh");
        let words: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .split_terminator('\0')
            .map(str::to_string)
            .collect();
        assert_eq!(words, QUOTING_TABLE, "launch");

        for word in QUOTING_TABLE {
            let script = format!("printf %s {}", HostShell::Posix.quote(word));
            let out = Command::new("sh").args(["-c", &script]).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), *word, "quote");
        }
    }

    #[test]
    fn the_quoting_table_survives_a_windows_host_shell() {
        let launch = HostShell::PowerShell
            .launch(&ssh(), "psmux", QUOTING_TABLE)
            .expect("fits a Windows line");
        let line = remote_line(&launch);
        let encoded = line
            .split(' ')
            .skip_while(|w| *w != "-EncodedCommand")
            .nth(1)
            .unwrap_or_else(|| panic!("not encoded: {line}"));
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')),
            "nothing a login shell reads as syntax: {encoded}"
        );
        let script = decode_powershell(encoded);
        let arguments = script
            .split_once("$s.Arguments = ")
            .and_then(|(_, rest)| rest.split_once("\n$s.UseShellExecute"))
            .map(|(arguments, _)| arguments)
            .expect("the argument line");
        assert_eq!(
            crt_split(&powershell_unquote(arguments)),
            QUOTING_TABLE,
            "{script}"
        );

        for word in QUOTING_TABLE {
            assert_eq!(
                powershell_unquote(&HostShell::PowerShell.quote(word)),
                *word
            );
        }
    }

    #[test]
    fn bare_words_reach_a_windows_host_unencoded() {
        let launch = HostShell::PowerShell
            .launch(&ssh(), "psmux", &["-L", "thurbox", "list-windows"])
            .unwrap();
        assert_eq!(remote_line(&launch), "psmux -L thurbox list-windows");
    }

    #[cfg(unix)]
    #[test]
    fn a_posix_hook_script_carries_env_and_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let odd = dir.path().join("it's $here");
        std::fs::create_dir(&odd).unwrap();
        let env: Vec<(String, String)> = QUOTING_TABLE
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("THURBOX_T{i}"), v.to_string()))
            .collect();
        let probe = (0..env.len())
            .map(|i| format!("\"$THURBOX_T{i}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let script = HostShell::Posix.hook_script(
            &format!("printf '%s\\0' \"$PWD\" {probe}"),
            Some(&odd.to_string_lossy()),
            &env,
        );
        let out = Command::new("sh").args(["-c", &script]).output().unwrap();
        let words: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .split_terminator('\0')
            .map(str::to_string)
            .collect();
        assert_eq!(words[0], odd.to_string_lossy());
        assert_eq!(&words[1..], QUOTING_TABLE);
    }

    #[test]
    fn a_windows_setup_past_the_line_limit_is_refused() {
        let step = "x".repeat(WINDOWS_CMD_LINE_MAX);
        let err = HostShell::PowerShell
            .wrap_setup(&[step], "agent", &[])
            .unwrap_err();
        assert!(err.len > err.max, "{err}");
    }

    #[test]
    fn a_windows_setup_hands_the_agent_its_exact_arguments() {
        let args: Vec<String> = ["", "say \"hi\"", "two words", "C:\\dir\\"]
            .map(String::from)
            .to_vec();
        let (program, words) = HostShell::PowerShell
            .wrap_setup(&["prepare.exe".into()], "agent.exe", &args)
            .unwrap();
        assert_eq!(program, "powershell.exe");
        let script = decode_powershell(words.last().unwrap());
        let arguments = script
            .split_once("$s.Arguments = ")
            .and_then(|(_, rest)| rest.split_once("\n$s.UseShellExecute"))
            .map(|(arguments, _)| arguments)
            .expect(&script);
        assert_eq!(crt_split(&powershell_unquote(arguments)), args, "{script}");
    }

    #[test]
    fn a_windows_setup_keeps_the_location_and_runs_a_script_agent() {
        let (_, words) = HostShell::PowerShell
            .wrap_setup(&["Set-Location sub".into()], "agent", &[])
            .unwrap();
        let script = decode_powershell(words.last().unwrap());
        assert!(
            script.contains(
                "$s.WorkingDirectory = (Get-Location -PSProvider FileSystem).ProviderPath"
            ),
            "{script}"
        );
        assert!(
            script.contains("-CommandType Application, ExternalScript"),
            "{script}"
        );
        assert!(script.contains("'ExternalScript'"), "{script}");
        assert!(script.contains("-NoLogo -File"), "{script}");
    }

    #[test]
    fn a_windows_setup_marks_a_step_that_exits_on_its_own() {
        let (_, words) = HostShell::PowerShell
            .wrap_setup(&["exit 3".into()], "agent.exe", &[])
            .unwrap();
        let script = decode_powershell(words.last().unwrap());
        let body = script.find("exit 3").expect(&script);
        let finally = script.find("} finally {").expect(&script);
        let agent = script.find("ProcessStartInfo").expect(&script);
        assert!(body < finally && finally < agent, "{script}");
        let tail = &script[finally..agent];
        assert!(tail.contains(SETUP_FAILURE_MARKER), "{script}");
    }

    fn decode_powershell(encoded: &str) -> String {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        String::from_utf16(&units).unwrap()
    }

    fn powershell_unquote(s: &str) -> String {
        s.strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .expect("single-quoted")
            .replace("''", "'")
    }

    fn crt_split(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = line.chars().peekable();
        loop {
            while matches!(chars.peek(), Some(' ' | '\t')) {
                chars.next();
            }
            if chars.peek().is_none() {
                return out;
            }
            let (mut word, mut quoted) = (String::new(), false);
            while let Some(&c) = chars.peek() {
                match c {
                    ' ' | '\t' if !quoted => break,
                    '\\' => {
                        let mut n = 0;
                        while chars.peek() == Some(&'\\') {
                            chars.next();
                            n += 1;
                        }
                        if chars.peek() == Some(&'"') {
                            word.push_str(&"\\".repeat(n / 2));
                            if n % 2 == 1 {
                                word.push('"');
                                chars.next();
                            }
                        } else {
                            word.push_str(&"\\".repeat(n));
                        }
                        continue;
                    }
                    '"' => quoted = !quoted,
                    c => word.push(c),
                }
                chars.next();
            }
            out.push(word);
        }
    }
}
