use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Posix,
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
