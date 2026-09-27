//! The multiplexer choice is independent of the machine that runs a session.

use super::HostDef;

/// A name accepted in settings, hosts, and per-create commands. Implementations
/// register separately; accepting a name here never claims its binary is usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    Tmux,
    Psmux,
    Rmux,
    Herdr,
}

impl Multiplexer {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Psmux => "psmux",
            Self::Rmux => "rmux",
            Self::Herdr => "herdr",
        }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "tmux" => Ok(Self::Tmux),
            "psmux" => Ok(Self::Psmux),
            "rmux" => Ok(Self::Rmux),
            "herdr" => Ok(Self::Herdr),
            _ => Err(format!(
                "Unknown multiplexer '{name}'. Choose tmux, psmux, rmux, or herdr."
            )),
        }
    }

    pub const fn platform_default() -> Self {
        if cfg!(windows) {
            Self::Psmux
        } else {
            Self::Tmux
        }
    }
}

/// What a create will use, before any worktree or pane is made. The host name
/// stays separate from the multiplexer choice in both the API and the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendChoice {
    pub host: Option<HostDef>,
    pub multiplexer: Multiplexer,
    /// The stable session routing key. Unsuffixed keys retain legacy meaning.
    pub backend_type: String,
}

impl BackendChoice {
    pub fn resolve(
        host: Option<HostDef>,
        explicit: Option<&str>,
        local_default: Option<&str>,
    ) -> Result<Self, String> {
        let fallback = match &host {
            Some(host) if host.is_windows() => Multiplexer::Psmux,
            Some(_) => Multiplexer::Tmux,
            None => Multiplexer::platform_default(),
        };
        let configured = match &host {
            Some(host) => host.multiplexer.as_deref(),
            None => local_default,
        };
        let name = explicit.or(configured).unwrap_or("default");
        let multiplexer = if name == "default" {
            fallback
        } else {
            Multiplexer::parse(name)?
        };
        let backend_type = match &host {
            Some(host) if multiplexer == Multiplexer::Psmux => {
                format!("{}:psmux", host.backend_name())
            }
            Some(host) if multiplexer == fallback => host.backend_name(),
            Some(host) => format!("{}:{}", host.backend_name(), multiplexer.name()),
            None if multiplexer == Multiplexer::Psmux => "local-psmux".to_string(),
            None if multiplexer == fallback => super::LOCAL_BACKEND_TYPE.to_string(),
            None => format!("local-{}", multiplexer.name()),
        };
        Ok(Self {
            host,
            multiplexer,
            backend_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_beats_host_or_local_defaults() {
        let host = HostDef {
            name: "example".into(),
            multiplexer: Some("rmux".into()),
            ..Default::default()
        };
        let chosen = BackendChoice::resolve(Some(host), Some("herdr"), None).unwrap();
        assert_eq!(chosen.backend_type, "ssh:example:herdr");
        assert_eq!(chosen.multiplexer, Multiplexer::Herdr);
        let local = BackendChoice::resolve(None, Some("herdr"), Some("rmux")).unwrap();
        assert_eq!(local.backend_type, "local-herdr");
    }

    #[test]
    fn legacy_keys_keep_their_routing() {
        let host = HostDef {
            name: "example".into(),
            ..Default::default()
        };
        let chosen = BackendChoice::resolve(Some(host), None, None).unwrap();
        assert_eq!(chosen.backend_type, "ssh:example");
        assert_eq!(
            BackendChoice::resolve(None, None, None)
                .unwrap()
                .backend_type,
            super::super::LOCAL_BACKEND_TYPE
        );
    }

    #[test]
    fn wsl_uses_its_host_preference_and_explicit_choice_wins() {
        let mut host = HostDef::wsl("example");
        host.multiplexer = Some("rmux".into());
        let configured = BackendChoice::resolve(Some(host.clone()), None, None).unwrap();
        assert_eq!(configured.backend_type, "wsl:example:rmux");
        let overridden = BackendChoice::resolve(Some(host), Some("tmux"), None).unwrap();
        assert_eq!(overridden.backend_type, "wsl:example");
    }
}
