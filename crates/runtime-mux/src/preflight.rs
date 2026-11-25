use std::path::Path;

use crate::Multiplexer;

const DIRS_IN_A_MESSAGE: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Present,
    Missing,
    Unknown,
}

impl Presence {
    pub fn as_str(self) -> &'static str {
        match self {
            Presence::Present => "present",
            Presence::Missing => "missing",
            Presence::Unknown => "unknown",
        }
    }
}

pub fn look_up(exe: &str) -> Presence {
    if exe.is_empty() {
        return Presence::Unknown;
    }
    if exe.contains('/') || exe.contains(std::path::MAIN_SEPARATOR) {
        let path = Path::new(exe);
        if !path.is_absolute() {
            return Presence::Unknown;
        }
        return present_if(crate::path_lookup::is_executable_file(path));
    }
    let names = names_to_try(exe);
    for dir in crate::path_lookup::path_dirs() {
        for name in &names {
            if crate::path_lookup::is_executable_file(&dir.join(name)) {
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

#[derive(Debug, Clone, Copy)]
pub enum Dependency<'a> {
    LocalMultiplexer,
    Multiplexer(&'a str),
    Launcher(&'a str),
    Agent {
        name: &'a str,
        command: &'a str,
        registry: Option<&'a Path>,
    },
}

impl Dependency<'_> {
    pub fn binary(&self) -> &str {
        match self {
            Dependency::LocalMultiplexer => local_multiplexer(),
            Dependency::Multiplexer(bin) | Dependency::Launcher(bin) => bin,
            Dependency::Agent { command, .. } => command,
        }
    }

    fn role(&self) -> String {
        match self {
            Dependency::LocalMultiplexer | Dependency::Multiplexer(_) => {
                format!("{} (thurbox's multiplexer)", self.binary())
            }
            Dependency::Launcher(bin) => format!("{bin} (how thurbox reaches a host)"),
            Dependency::Agent { name, command, .. } if name == command => {
                format!("{name} (a coding agent)")
            }
            Dependency::Agent { name, command, .. } => {
                format!("{command} (the CLI the coding agent {name} runs)")
            }
        }
    }

    pub fn fix(&self) -> String {
        match self {
            Dependency::LocalMultiplexer | Dependency::Multiplexer(_) => {
                match Multiplexer::parse(self.binary()) {
                    Ok(mux) => mux.install_hint(),
                    Err(_) => format!("install {}", self.binary()),
                }
            }
            Dependency::Launcher(bin) if bin.starts_with("wsl") => {
                "wsl.exe comes with the Windows Subsystem for Linux: \
                 https://learn.microsoft.com/windows/wsl/install"
                    .to_string()
            }
            Dependency::Launcher(_) => {
                "install an OpenSSH client: https://www.openssh.com/".to_string()
            }
            Dependency::Agent { name, registry, .. } => {
                let registry = registry
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "agents.toml".to_string());
                format!(
                    "install its CLI, or point the `command` of the `{name}` entry in {registry} \
                     at the binary"
                )
            }
        }
    }

    pub fn missing_summary(&self) -> String {
        format!("`{}` was not found on PATH; {}", self.binary(), self.fix())
    }

    pub fn missing_message(&self) -> String {
        format!(
            "{} is not installed, or not on thurbox's own PATH. Fix: {}. {}",
            self.role(),
            self.fix(),
            searched(self.binary()),
        )
    }
}

fn searched(binary: &str) -> String {
    let dirs = crate::path_lookup::path_dirs();
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

pub fn launch_failure(
    remote_launcher: Option<&str>,
    mux: &str,
    context: &str,
    err: std::io::Error,
) -> anyhow::Error {
    if err.kind() != std::io::ErrorKind::NotFound {
        return anyhow::Error::new(err).context(context.to_string());
    }
    let dependency = match remote_launcher {
        Some(launcher) => Dependency::Launcher(launcher),
        None => Dependency::Multiplexer(mux),
    };
    anyhow::Error::new(MissingDependency(dependency.missing_message()))
}

#[derive(Debug)]
pub struct MissingDependency(pub String);

impl std::fmt::Display for MissingDependency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MissingDependency {}

pub fn is_missing_dependency(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.is::<MissingDependency>())
}

pub fn local_multiplexers() -> Vec<Multiplexer> {
    let found: Vec<Multiplexer> = Multiplexer::ALL
        .into_iter()
        .filter(|mux| look_up(mux.name()) == Presence::Present)
        .collect();
    Multiplexer::available_on(crate::shell::local_platform(), Some(&found), None)
}

pub fn local_multiplexer() -> &'static str {
    Multiplexer::platform_default().name()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_command_is_unknown_because_its_directory_is_not_ours() {
        assert_eq!(look_up("./bin/agent"), Presence::Unknown);
        assert_eq!(look_up("bin/agent"), Presence::Unknown);
    }

    #[test]
    fn an_absolute_command_is_still_answered_where_the_user_pointed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent = dir.path().join("agent");
        std::fs::write(&agent, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700))
                .expect("chmod");
        }
        assert_eq!(look_up(&agent.display().to_string()), Presence::Present);
    }

    #[test]
    fn a_command_spelled_as_a_path_is_checked_where_the_user_pointed() {
        // On Windows a root without a prefix is not `Path::is_absolute`.
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("definitely-not-here").join("agent-xyz");
        assert_eq!(
            look_up(&missing.display().to_string()),
            Presence::Missing,
            "a path that is not there is missing, whatever PATH holds"
        );
    }

    #[test]
    fn an_empty_command_is_unknown_rather_than_missing() {
        assert_eq!(look_up(""), Presence::Unknown);
    }

    #[test]
    fn a_missing_binary_names_itself_the_search_and_a_fix() {
        let dep = Dependency::Agent {
            name: "claude",
            command: "claude",
            registry: None,
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
        let phrase = crate::path_lookup::with_path("", || searched("tmux"));
        assert!(phrase.contains("PATH is unset"), "{phrase}");
    }

    #[test]
    fn the_search_phrase_summarizes_a_long_path_instead_of_printing_it() {
        // On Windows the PATH separator is `;` and a prefix-less `/opt/dirN` is not absolute.
        let many: Vec<std::path::PathBuf> = (0..20)
            .map(|i| {
                if cfg!(windows) {
                    std::path::PathBuf::from(format!("C:\\dir{i}"))
                } else {
                    std::path::PathBuf::from(format!("/opt/dir{i}"))
                }
            })
            .collect();
        let path = std::env::join_paths(&many).unwrap();
        let phrase = crate::path_lookup::with_path(path, || searched("tmux"));
        assert!(phrase.contains(&many[0].display().to_string()), "{phrase}");
        assert!(
            phrase.contains(&format!("and {} more", 20 - DIRS_IN_A_MESSAGE)),
            "{phrase}"
        );
        assert!(
            !phrase.contains(&many[19].display().to_string()),
            "the whole PATH landed in a one-line message: {phrase}"
        );
    }
}
