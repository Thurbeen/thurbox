//! Implementations of the forge contract ([`fleet_api::forge::Forge`]).
//!
//! - [`GithubForge`]: pull requests through `gh`, on github.com or an
//!   enterprise host.
//! - [`GitlabForge`]: merge requests through `glab`, on gitlab.com or a
//!   self-hosted instance.
//! - [`AzureDevopsForge`]: pull requests through `az`, on Azure DevOps Services.
//! - [`BitbucketCloudForge`]: pull requests on bitbucket.org through an
//!   externally authenticated `curl`.
//! - [`FakeForge`]: a file-backed forge for tests and trials. It runs no
//!   process.
//! - [`ForgeHosts`]: the adapters of a configuration's `[forge.hosts]`,
//!   chosen by the host a URL or a reference names.
//!
//! This is the only crate that knows the forge clients (`az`, `curl`, `gh`
//! and `glab`) exist. Every call is one bounded process through fleet-exec,
//! its tree killed past the bound, and the clients' own configuration is the
//! only credential source: Fleet passes none and stores none. Adapters that
//! implement the one write, `mark_ready`, re-read the change's head and state
//! immediately before mutation and refuse with `Moved` when either differs
//! from what the caller expected. Providers without that guarantee fail
//! closed.

pub mod azure_devops;
pub mod bitbucket;
pub mod cli;
pub mod fake;
pub mod github;
pub mod gitlab;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

pub use azure_devops::AzureDevopsForge;
pub use bitbucket::BitbucketCloudForge;
pub use fake::{FakeForge, FakeForgeState, ForgeFault};
use fleet_api::forge::{ChangeObservation, ChangeRef, Expected, Forge, Uncertainty};
use fleet_api::plan::EffectOutcome;
use fleet_api::{Millis, Observation};
pub use github::GithubForge;
pub use gitlab::GitlabForge;

/// Which adapter serves a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    AzureDevops,
    BitbucketCloud,
    Github,
    Gitlab,
    Fake,
}

impl HostKind {
    pub fn parse(kind: &str) -> Option<HostKind> {
        match kind {
            azure_devops::KIND => Some(HostKind::AzureDevops),
            bitbucket::KIND => Some(HostKind::BitbucketCloud),
            github::KIND => Some(HostKind::Github),
            gitlab::KIND => Some(HostKind::Gitlab),
            fake::KIND => Some(HostKind::Fake),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            HostKind::AzureDevops => azure_devops::KIND,
            HostKind::BitbucketCloud => bitbucket::KIND,
            HostKind::Github => github::KIND,
            HostKind::Gitlab => gitlab::KIND,
            HostKind::Fake => fake::KIND,
        }
    }
}

/// One `[forge.hosts]` entry.
#[derive(Debug, Clone)]
pub struct HostConfig {
    pub kind: HostKind,
    /// The CLI executable, a path or a name on PATH; the kind's own name by
    /// default. Unused by the fake.
    pub cli: Option<OsString>,
    /// The fake's directory.
    pub dir: Option<PathBuf>,
}

/// The configured forges, one adapter per host.
pub struct ForgeHosts {
    hosts: BTreeMap<String, Box<dyn Forge>>,
}

impl std::fmt::Debug for ForgeHosts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForgeHosts")
            .field("hosts", &self.hosts.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ForgeHosts {
    /// Build one adapter per host. `call_timeout` bounds every CLI call.
    pub fn new(
        hosts: BTreeMap<String, HostConfig>,
        call_timeout: Duration,
    ) -> Result<ForgeHosts, String> {
        let mut out: BTreeMap<String, Box<dyn Forge>> = BTreeMap::new();
        for (host, cfg) in hosts {
            let host = host.trim().to_ascii_lowercase();
            if host.is_empty() || host.contains(['/', ' ', '@']) {
                return Err(format!("forge.hosts: {host:?} is not a hostname"));
            }
            let default_cli = |name: &str| -> OsString {
                if cfg!(windows) {
                    format!("{name}.exe").into()
                } else {
                    name.into()
                }
            };
            let adapter: Box<dyn Forge> = match cfg.kind {
                HostKind::AzureDevops => Box::new(AzureDevopsForge::new(
                    cfg.cli.unwrap_or_else(|| default_cli("az")),
                    call_timeout,
                )),
                HostKind::BitbucketCloud => {
                    if host != bitbucket::HOST {
                        return Err(format!(
                            "forge.hosts.{host}: bitbucket-cloud serves only {}",
                            bitbucket::HOST
                        ));
                    }
                    Box::new(BitbucketCloudForge::new(
                        cfg.cli.unwrap_or_else(|| default_cli("curl")),
                        call_timeout,
                    ))
                }
                HostKind::Github => Box::new(GithubForge::new(
                    &host,
                    cfg.cli.unwrap_or_else(|| default_cli("gh")),
                    call_timeout,
                )),
                HostKind::Gitlab => Box::new(GitlabForge::new(
                    &host,
                    cfg.cli.unwrap_or_else(|| default_cli("glab")),
                    call_timeout,
                )),
                HostKind::Fake => {
                    let dir = cfg
                        .dir
                        .ok_or_else(|| format!("forge.hosts.{host}: a fake host needs a dir"))?;
                    Box::new(
                        FakeForge::open(&host, &dir)
                            .map_err(|e| format!("fake forge {}: {e}", dir.display()))?,
                    )
                }
            };
            out.insert(host, adapter);
        }
        Ok(ForgeHosts { hosts: out })
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    fn adapter(&self, host: &str) -> Option<&dyn Forge> {
        self.hosts
            .get(&host.to_ascii_lowercase())
            .or_else(|| {
                host.to_ascii_lowercase()
                    .strip_suffix(".visualstudio.com")
                    .and_then(|_| self.hosts.get(azure_devops::SERVICES_HOST))
            })
            .map(|b| b.as_ref())
    }
}

/// The host part of a URL, lowercased.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?.trim();
    (!host.is_empty() && !host.contains('@')).then(|| host.to_ascii_lowercase())
}

impl Forge for ForgeHosts {
    fn operation_bound(&self) -> Millis {
        self.hosts
            .values()
            .map(|f| f.operation_bound())
            .max()
            .unwrap_or(0)
    }

    fn parse(&self, url: &str) -> Observation<ChangeRef> {
        let Some(host) = host_of(url) else {
            return Observation::unknown("the URL has no host");
        };
        match self.adapter(&host) {
            Some(f) => f.parse(url),
            None => {
                Observation::unknown(format!("host {host} is not configured under [forge.hosts]"))
            }
        }
    }

    fn whoami(&self, host: &str) -> Observation<String> {
        match self.adapter(host) {
            Some(f) => f.whoami(host),
            None => {
                Observation::unknown(format!("host {host} is not configured under [forge.hosts]"))
            }
        }
    }

    fn change(&self, change: &ChangeRef) -> ChangeObservation {
        match self.adapter(&change.host) {
            Some(f) => f.change(change),
            None => ChangeObservation::unknown(
                Uncertainty::Unavailable,
                format!("host {} is not configured under [forge.hosts]", change.host),
            ),
        }
    }

    fn mark_ready(
        &self,
        change: &ChangeRef,
        expect: &Expected,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        match self.adapter(&change.host) {
            Some(f) => f.mark_ready(change, expect, intent_key),
            // Nothing ran: a Known refusal, not a question.
            None => Observation::Known(EffectOutcome::Failed {
                why: format!("host {} is not configured under [forge.hosts]", change.host),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_services_host_routes_legacy_organization_aliases() {
        let hosts = ForgeHosts::new(
            BTreeMap::from([(
                azure_devops::SERVICES_HOST.into(),
                HostConfig {
                    kind: HostKind::AzureDevops,
                    cli: Some("az".into()),
                    dir: None,
                },
            )]),
            Duration::from_secs(1),
        )
        .unwrap();
        let parsed = hosts.parse(
            "https://wingtip.visualstudio.com/DefaultCollection/project/_git/widgets/pullrequest/7",
        );
        assert_eq!(
            parsed,
            Observation::Known(ChangeRef {
                kind: azure_devops::KIND.into(),
                host: "wingtip.visualstudio.com".into(),
                project: "project/widgets".into(),
                id: "7".into(),
            })
        );
    }
}
