//! Whether the binaries a session needs are actually installed.
//!
//! thurbox deliberately starts with no multiplexer and no coding agent on the
//! machine (`docs/CONSTITUTION.md`) — browsing, reading and configuring must
//! work on a fresh box. The cost is that the check used to happen at the worst
//! possible moment: the user committed to creating a session and got a number
//! back (`tmux new-window exited exit status: 127`), which names neither the
//! binary, nor where thurbox looked, nor what to install.
//!
//! This module is the one answer to "is the thing that would run this actually
//! there?", asked in three places:
//!
//! - the create-session flow, so the answer arrives *before* the user commits
//!   ([`crate::kernel::snapshot`] publishes it; probed on a TTL, never per
//!   frame);
//! - the spawn error, so a failure names the binary, the directories searched
//!   and the fix ([`crate::agent::tmux`]);
//! - `thurbox-cli doctor`, so it can be asked directly.
//!
//! It never blocks anything. A `Missing` answer is a warning on the choice
//! being made, not a refusal: an agent `command` can be a shell function, an
//! alias or something installed a second later, and treating "not on `PATH`"
//! as fatal would turn an improvement into a new way to fail — the same rule
//! `crate::agent::tmux::resolve_local_program` is written under.

use std::path::Path;

use crate::agent::transport::{TmuxTransport, DEFAULT_MUX};

/// How many search directories a one-line message names before it summarizes
/// the rest. A `PATH` of thirty entries is ordinary; a message that prints all
/// of them is unreadable in a TUI's one-line message row, and `thurbox-cli
/// doctor` prints the full list for the case where every entry matters.
const DIRS_IN_A_MESSAGE: usize = 6;

/// What thurbox found when it looked for a binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// A file that can be executed is there.
    Present,
    /// Every directory on `PATH` was searched and nothing runnable matched.
    Missing,
    /// Not answerable from here. A remote host's binaries live on the *host*,
    /// and asking would mean a round trip on every keystroke; a command with no
    /// name at all is nothing to look for. Deliberately distinct from
    /// [`Presence::Missing`] — reporting "not installed" about a machine that
    /// was never looked at is the conflation this whole module exists to end.
    Unknown,
}

impl Presence {
    /// The stable word a plugin and `--json` branch on.
    pub fn as_str(self) -> &'static str {
        match self {
            Presence::Present => "present",
            Presence::Missing => "missing",
            Presence::Unknown => "unknown",
        }
    }
}

/// Whether `exe` is runnable, looked up the way this platform's loader would.
///
/// Unix: the absolute-only `PATH` walk [`crate::paths::resolve_on_path`] does.
/// Windows: the same walk, but a bare name is also tried with each extension in
/// `PATHEXT`, because that is how the loader finds `psmux.exe` given `psmux`.
/// The *spawn* path deliberately has no such munging (see
/// `crate::agent::tmux::resolve_local_program`) — this is detection, and a
/// detector that called psmux missing on every Windows machine would be worse
/// than no detector at all.
///
/// A command spelled as a path is not a `PATH` lookup: it is checked where the
/// user pointed, which is the only place it could come from.
pub fn look_up(exe: &str) -> Presence {
    if exe.is_empty() {
        return Presence::Unknown;
    }
    if exe.contains('/') || exe.contains(std::path::MAIN_SEPARATOR) {
        return present_if(crate::paths::is_executable_file(Path::new(exe)));
    }
    let names = names_to_try(exe);
    for dir in crate::paths::path_dirs() {
        for name in &names {
            if crate::paths::is_executable_file(&dir.join(name)) {
                return Presence::Present;
            }
        }
    }
    Presence::Missing
}

fn present_if(found: bool) -> Presence {
    if found {
        Presence::Present
    } else {
        Presence::Missing
    }
}

/// The file names a bare `exe` could be on disk.
///
/// One on Unix. On Windows the loader also tries each `PATHEXT` suffix, so a
/// registry entry of `psmux` has to match `psmux.exe`; the default list is
/// used when the variable is unset, which is what a stripped environment
/// (a service, a CI runner) leaves behind.
fn names_to_try(exe: &str) -> Vec<String> {
    if !cfg!(windows) {
        return vec![exe.to_string()];
    }
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut names = vec![exe.to_string()];
    names.extend(
        pathext
            .split(';')
            .map(str::trim)
            .filter(|ext| ext.starts_with('.'))
            .map(|ext| format!("{exe}{ext}")),
    );
    names
}

/// A binary thurbox needs, and what a user does about it not being there.
///
/// Carries the *role* rather than the bare name because the fix depends on it:
/// a multiplexer is a package to install, an agent is a CLI thurbox knows
/// nothing about beyond the `command` the registry gives it.
#[derive(Debug, Clone, Copy)]
pub enum Dependency<'a> {
    /// The multiplexer a *local* session's window lives in: `tmux`, or `psmux`
    /// on native Windows.
    LocalMultiplexer,
    /// What reaches a remote host's multiplexer from this machine: `ssh`, or
    /// `wsl.exe` for a WSL distro.
    Launcher(&'a str),
    /// A coding agent's CLI, as its `agents.toml` entry spells `command`.
    Agent { name: &'a str, command: &'a str },
}

impl Dependency<'_> {
    /// The binary this is about.
    pub fn binary(&self) -> &str {
        match self {
            Dependency::LocalMultiplexer => DEFAULT_MUX,
            Dependency::Launcher(bin) => bin,
            Dependency::Agent { command, .. } => command,
        }
    }

    /// How the user thinks of it, for the front of a sentence.
    ///
    /// Short on purpose, apposition included: this leads a message row that is
    /// as wide as the terminal and no wider, so every word here is a word the
    /// fix behind it does not get.
    fn role(&self) -> String {
        match self {
            Dependency::LocalMultiplexer => {
                format!("{DEFAULT_MUX} (thurbox's multiplexer)")
            }
            Dependency::Launcher(bin) => format!("{bin} (how thurbox reaches a host)"),
            Dependency::Agent { name, command } if name == command => {
                format!("{name} (a coding agent)")
            }
            Dependency::Agent { name, command } => {
                format!("{command} (the CLI the coding agent {name} runs)")
            }
        }
    }

    /// The one thing to do about it, on *this* platform.
    ///
    /// Never a package-manager line thurbox has not verified: where the command
    /// depends on a distribution, this names the package and links the
    /// project's own install page instead of guessing an invocation. thurbox
    /// bakes in no agent knowledge either (`docs/AGENTS.md`), so a missing
    /// agent is answered with the registry entry that decides what gets run
    /// rather than with an install command for a CLI thurbox does not know.
    pub fn fix(&self) -> String {
        match self {
            Dependency::LocalMultiplexer if cfg!(windows) => {
                "install psmux, the Windows multiplexer: https://github.com/psmux/psmux".to_string()
            }
            Dependency::LocalMultiplexer if cfg!(target_os = "macos") => {
                "install tmux 3.2 or newer (`brew install tmux`), or see \
                 https://github.com/tmux/tmux/wiki/Installing"
                    .to_string()
            }
            Dependency::LocalMultiplexer => {
                "install tmux 3.2 or newer — the package is called `tmux` on every major \
                 distribution; see https://github.com/tmux/tmux/wiki/Installing"
                    .to_string()
            }
            Dependency::Launcher(bin) if bin.starts_with("wsl") => {
                "wsl.exe comes with the Windows Subsystem for Linux: \
                 https://learn.microsoft.com/windows/wsl/install"
                    .to_string()
            }
            Dependency::Launcher(_) => {
                "install an OpenSSH client: https://www.openssh.com/".to_string()
            }
            Dependency::Agent { name, .. } => {
                let registry = crate::agent::agent_config::agents_config_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "agents.toml".to_string());
                format!(
                    "install its CLI, or point the `command` of the `{name}` entry in {registry} \
                     at the binary"
                )
            }
        }
    }

    /// The short form, for a report that prints the search path once of its
    /// own accord. Repeating twenty absolute directories on every row of a
    /// ten-agent registry buries the one word that differs between them.
    pub fn missing_summary(&self) -> String {
        format!("`{}` was not found on PATH; {}", self.binary(), self.fix())
    }

    /// The whole sentence: what is missing, the fix, and where thurbox looked.
    ///
    /// One line, and in that order deliberately. It is rendered in a TUI
    /// message row, which is as wide as the terminal and no wider, so what
    /// comes last is what a narrow screen loses — and of the three, the list of
    /// directories is both the longest and the one `thurbox-cli doctor` prints
    /// in full anyway. A reader who sees only the first clause still knows what
    /// is missing and roughly what to do.
    pub fn missing_message(&self) -> String {
        format!(
            "{} is not installed, or not on thurbox's own PATH. Fix: {}. {}",
            self.role(),
            self.fix(),
            searched(self.binary()),
        )
    }
}

/// Where thurbox looked for `binary`, as a phrase.
///
/// Reads [`crate::paths::path_dirs`] rather than re-deriving the list, so the
/// message can never name a search the resolver did not do.
fn searched(binary: &str) -> String {
    let dirs = crate::paths::path_dirs();
    if dirs.is_empty() {
        return format!("There was nowhere to look for `{binary}`: PATH is unset.");
    }
    let shown: Vec<String> = dirs
        .iter()
        .take(DIRS_IN_A_MESSAGE)
        .map(|d| d.display().to_string())
        .collect();
    let rest = dirs.len().saturating_sub(shown.len());
    let tail = if rest > 0 {
        format!(" (and {rest} more)")
    } else {
        String::new()
    };
    format!("Looked in {}{tail}.", shown.join(", "))
}

/// What a failure to *launch* the multiplexer means, in words a user can act on.
///
/// [`std::process::Command`]'s `spawn`/`output` fails before anything has run,
/// so a `NotFound` here is exactly one thing: the program named is not
/// installed, or is not on the `PATH` thurbox was started with. That is the
/// whole content of the report this module exists for, and it used to reach the
/// user as `No such file or directory (os error 2)` under a context line that
/// named tmux without saying tmux was the thing that was missing.
///
/// Every other io error keeps `context` and its own text: a permission error or
/// a broken pipe is not something an install fixes, and dressing one as a
/// missing binary sends the reader somewhere there is nothing to find.
pub fn launch_failure(
    transport: &TmuxTransport,
    context: &'static str,
    err: std::io::Error,
) -> anyhow::Error {
    if err.kind() != std::io::ErrorKind::NotFound {
        return anyhow::Error::new(err).context(context);
    }
    let launcher = transport.launcher();
    let dependency = if transport.is_remote() {
        Dependency::Launcher(launcher)
    } else {
        Dependency::LocalMultiplexer
    };
    anyhow::Error::new(MissingDependency(dependency.missing_message()))
}

/// A binary a session needs is not installed.
///
/// A type rather than a plain message so the callers between here and the
/// screen can *recognise* it: its `Display` is already the whole story, and
/// every context line one of them would otherwise add ("Failed to spawn tmux
/// window", "Failed to create tmux session") pushes the part that names the
/// binary and the fix off the end of a one-line message row. See
/// [`is_missing_dependency`].
#[derive(Debug)]
pub struct MissingDependency(pub String);

impl std::fmt::Display for MissingDependency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MissingDependency {}

/// Whether `err`, or anything it wraps, is a [`MissingDependency`].
///
/// What a caller checks before adding a context line of its own, and before
/// rendering the chain rather than the message.
pub fn is_missing_dependency(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.is::<MissingDependency>())
}

/// The multiplexer a local session would run in on this platform.
pub fn local_multiplexer() -> &'static str {
    DEFAULT_MUX
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_spelled_as_a_path_is_checked_where_the_user_pointed() {
        // Not a PATH lookup at all: no directory on PATH could make this true
        // or false, so answering from PATH would answer about the wrong file.
        assert_eq!(
            look_up("/definitely/not/here/agent-xyz"),
            Presence::Missing,
            "a path that is not there is missing, whatever PATH holds"
        );
    }

    #[test]
    fn an_empty_command_is_unknown_rather_than_missing() {
        // There is nothing to look for, which is not the same claim as having
        // looked and found nothing.
        assert_eq!(look_up(""), Presence::Unknown);
    }

    #[test]
    fn a_missing_binary_names_itself_the_search_and_a_fix() {
        let dep = Dependency::Agent {
            name: "claude",
            command: "claude",
        };
        let message = dep.missing_message();
        assert!(message.contains("claude"), "{message}");
        assert!(message.contains("Looked in"), "{message}");
        assert!(message.contains("agents.toml"), "{message}");
    }

    #[test]
    fn the_multiplexer_fix_is_the_one_for_this_platform() {
        let fix = Dependency::LocalMultiplexer.fix();
        if cfg!(windows) {
            assert!(fix.contains("psmux"), "{fix}");
            assert!(
                !fix.contains("tmux"),
                "a Windows user is never sent to tmux"
            );
        } else {
            assert!(fix.contains("tmux"), "{fix}");
            assert!(
                !fix.contains("apt install"),
                "the distribution is not thurbox's to guess: {fix}"
            );
        }
    }

    #[test]
    fn an_unset_path_says_so_rather_than_listing_nothing() {
        let phrase = crate::paths::with_path("", || searched("tmux"));
        assert!(phrase.contains("PATH is unset"), "{phrase}");
    }

    #[test]
    fn the_search_phrase_summarizes_a_long_path_instead_of_printing_it() {
        let many: Vec<String> = (0..20).map(|i| format!("/opt/dir{i}")).collect();
        let phrase = crate::paths::with_path(many.join(":"), || searched("tmux"));
        assert!(phrase.contains("/opt/dir0"), "{phrase}");
        assert!(
            phrase.contains(&format!("and {} more", 20 - DIRS_IN_A_MESSAGE)),
            "{phrase}"
        );
        assert!(
            !phrase.contains("/opt/dir19"),
            "the whole PATH landed in a one-line message: {phrase}"
        );
    }
}
