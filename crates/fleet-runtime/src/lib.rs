//! Implementations of the runtime contract ([`fleet_api::runtime::Runtime`]).
//!
//! - [`ExecRuntime`]: any program speaking the runtime protocol
//!   (docs/runtime/README.md), `runtime-mux` by default, chosen per host.
//! - [`FakeRuntime`]: a file-backed fake host for tests and local trials. It
//!   keeps sessions and workspaces as records, not processes or files, and
//!   runs nothing.
//! - [`FileSignals`]: the signal port's adapter, one file per session on
//!   this machine.
//! - [`assets`]: the plugin files Fleet ships for the harnesses that take
//!   their hooks as a file, written beside the signal files.
//! - [`integration`]: the hooks `fleet integration` installs into an agent's
//!   own config directory, for a harness that takes none at launch.

pub mod assets;
pub mod bridge;
pub mod exec;
pub mod fake;
pub mod integration;
pub mod signals;

pub use exec::{ExecProgram, ExecRuntime};
pub use fake::{FakeHost, FakeRuntime, FakeSession, FakeWorkspace, Fault};
pub use signals::FileSignals;

/// Resolve manager-local explicit sources, never an implicit shell. Errors
/// deliberately omit child output, paths and parser text (all may contain values).
pub(crate) fn resolve_env_source(
    source: &fleet_api::config::EnvSource,
) -> fleet_api::Observation<std::collections::BTreeMap<String, String>> {
    use fleet_api::{
        Observation,
        config::{EnvSource, SOURCE_CAP},
    };
    use std::io::Read;
    let bytes = match source {
        EnvSource::File { path } => {
            if !std::path::Path::new(path).is_absolute() {
                return Observation::unknown(
                    "environment file path must be absolute on the manager",
                );
            }
            let read = || -> std::io::Result<Vec<u8>> {
                let initial = std::fs::metadata(path)?;
                if !initial.is_file() {
                    return Err(std::io::Error::other("not regular"));
                }
                let f = std::fs::File::open(path)?;
                let meta = f.metadata()?;
                if !meta.is_file() || meta.len() > SOURCE_CAP as u64 {
                    return Err(std::io::Error::other("invalid environment file"));
                }
                let mut bytes = Vec::new();
                f.take(SOURCE_CAP as u64 + 1).read_to_end(&mut bytes)?;
                Ok(bytes)
            };
            match read() {
                Ok(b) => b,
                Err(_) => {
                    return Observation::unknown(
                        "environment file unavailable, not regular, or oversized",
                    );
                }
            }
        }
        EnvSource::Command {
            program,
            args,
            cwd,
            timeout_ms,
        } => {
            if !std::path::Path::new(program).is_absolute()
                || !std::path::Path::new(cwd).is_absolute()
            {
                return Observation::unknown(
                    "environment command paths must be absolute on the manager",
                );
            }
            let mut spec =
                fleet_exec::Spec::new(program, std::time::Duration::from_millis(*timeout_ms))
                    .args(args);
            spec.cwd = Some(cwd.into());
            match fleet_exec::run(&spec) {
                fleet_exec::Outcome::Exited {
                    code: Some(0),
                    stdout,
                    ..
                } => stdout,
                fleet_exec::Outcome::TimedOut { .. } => {
                    return Observation::unknown("environment command timed out");
                }
                _ => return Observation::unknown("environment command failed"),
            }
        }
    };
    if bytes.len() > SOURCE_CAP {
        return Observation::unknown("environment source exceeds 64 KiB");
    }
    let map = match serde_json::from_slice::<std::collections::BTreeMap<String, String>>(&bytes) {
        Ok(m) => m,
        Err(_) => return Observation::unknown("environment source must be a JSON string map"),
    };
    if fleet_api::validate::validate_env(&map).is_err() {
        return Observation::unknown("environment source has invalid keys, values, or size");
    }
    Observation::Known(map)
}
