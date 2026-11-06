//! The operating system a machine runs, as its own dimension: independent of
//! how thurbox reaches it (the route's place) and of which multiplexer serves
//! it (the route's multiplexer).
//!
//! It decides shell and path semantics — `sh -c` or PowerShell, `$HOME` or
//! `%USERPROFILE%`, whether `/bin/sh` exists to pin as a server's
//! `default-command`. What a multiplexer can do (close events, hex keys) is
//! that backend's own capability and is never read off a platform, nor a
//! platform off a multiplexer's name.

use serde::{Deserialize, Serialize};

/// A machine's OS family, as far as thurbox's shell and path handling cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// Linux, macOS, a WSL distro: `/bin/sh`, `$HOME`, `/` paths.
    Posix,
    /// Native Windows: PowerShell, `%USERPROFILE%`, `\` paths.
    Windows,
}

impl Platform {
    pub const ALL: [Self; 2] = [Self::Posix, Self::Windows];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Posix => "posix",
            Self::Windows => "windows",
        }
    }

    /// The OS this thurbox was built for, which is the machine it runs on.
    ///
    /// A build fact, so it answers the same in a test: a decision that a test
    /// must be able to run as the other platform reads the local platform
    /// through `shell::local_platform`, which a test can simulate.
    pub const fn local() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_platform_is_the_build_os() {
        let built = if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Posix
        };
        assert_eq!(Platform::local(), built);
    }
}
