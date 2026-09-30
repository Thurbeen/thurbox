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

    /// The machine this thurbox runs on. The one place the build OS is read
    /// as a platform: every other machine's comes from its host entry.
    pub fn local() -> Self {
        #[cfg(test)]
        if let Some(simulated) = SIMULATED_LOCAL.with(std::cell::Cell::get) {
            return simulated;
        }
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

#[cfg(test)]
thread_local! {
    static SIMULATED_LOCAL: std::cell::Cell<Option<Platform>> =
        const { std::cell::Cell::new(None) };
}

/// Run `f` as though this thurbox were built for `platform`, so a Linux test
/// run covers the decisions a Windows build makes (and the reverse). Only
/// [`Platform::local`] answers differently: code gated on `cfg(windows)`
/// itself is not simulated, which is why no backend decision may be.
#[cfg(test)]
pub(crate) fn simulate_local<T>(platform: Platform, f: impl FnOnce() -> T) -> T {
    let saved = SIMULATED_LOCAL.with(|s| s.replace(Some(platform)));
    let out = f();
    SIMULATED_LOCAL.with(|s| s.set(saved));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_platform_is_the_build_os_unless_simulated() {
        let built = if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Posix
        };
        assert_eq!(Platform::local(), built);
        for platform in Platform::ALL {
            assert_eq!(simulate_local(platform, Platform::local), platform);
        }
        assert_eq!(Platform::local(), built);
    }
}
