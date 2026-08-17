//! Whether a newer release exists, and installing it.
//!
//! Both switches from `[features]` land here, and they are separate on purpose:
//! `version_check` only *reports*, `auto_update` also *replaces binaries*. One
//! makes a network call, the other makes a network call and writes to disk, so
//! neither is inferred from the other.
//!
//! Sixth instance of the worker pattern, and the one with the longest tail: a
//! release download can take a minute on a slow link. Nothing here is waited on —
//! the cached answer is read instantly at startup and the network runs on a
//! thread, so a machine with no route to GitHub costs the interface nothing.
//!
//! The work itself is v1's, unchanged: [`crate::agent::version_check`] for the
//! check and its cache, [`crate::agent::self_update`] for the install. What is
//! new is only where the results are kept and when the threads are started.

use std::sync::mpsc::{channel, Receiver, Sender};

use crate::session::settings::FeatureFlags;

/// What a worker came back with.
enum Done {
    /// A refreshed check: the latest release, when it is newer than this build.
    Checked(Option<String>),
    /// The silent update finished and replaced the binaries.
    Installed(String),
}

/// The update state, and the workers feeding it.
#[derive(Default)]
pub struct Updates {
    /// The newer release, when one is known. This is what the header band shows.
    latest: Option<String>,
    channel: Option<(Sender<Done>, Receiver<Done>)>,
}

impl Updates {
    /// Read what is already known and start whatever the switches allow.
    ///
    /// The cache read is a file, so it happens here; everything that touches the
    /// network happens on a thread. With both switches off this does nothing at
    /// all — not a fetch, not a thread, not a file.
    pub fn start(features: &FeatureFlags) -> Self {
        let mut updates = Self::default();
        if !features.version_check && !features.auto_update {
            return updates;
        }

        if features.version_check {
            // Instant, and usually enough: the badge is right from the first
            // frame when the cache is warm.
            updates.latest = crate::agent::version_check::read_cached_status().map(|s| s.latest);
            if crate::agent::version_check::cache_is_stale() {
                let tx = updates.sender();
                std::thread::spawn(move || {
                    let latest = match crate::agent::version_check::refresh_cache() {
                        Ok((_, status)) => status.map(|s| s.latest),
                        Err(e) => {
                            tracing::warn!("update check failed: {e}");
                            return;
                        }
                    };
                    let _ = tx.send(Done::Checked(latest));
                });
            }
        }

        if features.auto_update {
            let tx = updates.sender();
            std::thread::spawn(move || {
                match crate::agent::self_update::perform_update(false) {
                    Ok(crate::agent::self_update::UpdateOutcome::Updated { to, .. }) => {
                        // The check's cache is refreshed with it, so the badge
                        // agrees with what is now installed on the next launch.
                        let _ = crate::agent::version_check::refresh_cache();
                        let _ = tx.send(Done::Installed(to));
                    }
                    // Up to date, or a dev build — `perform_update` skips those
                    // itself, which is why nothing is decided here.
                    Ok(_) => {}
                    Err(e) => tracing::warn!("auto-update failed: {e}"),
                }
            });
        }
        updates
    }

    /// The newer release, when one is known.
    pub fn available(&self) -> Option<&str> {
        self.latest.as_deref()
    }

    /// Fold in whatever the workers finished. Returns a message to report, which
    /// is only ever the one that matters: binaries were replaced.
    pub fn poll(&mut self) -> Option<String> {
        let mut message = None;
        let Some((_, rx)) = &self.channel else {
            return None;
        };
        while let Ok(done) = rx.try_recv() {
            match done {
                Done::Checked(latest) => self.latest = latest,
                Done::Installed(to) => {
                    self.latest = Some(to.clone());
                    message = Some(format!("Updated to v{to} — restart thurbox to apply."));
                }
            }
        }
        message
    }

    fn sender(&mut self) -> Sender<Done> {
        self.channel.get_or_insert_with(channel).0.clone()
    }
}

#[cfg(test)]
mod tests {
    /// Exactly one place in the interface installs a release.
    ///
    /// Not a style rule. `self_update::install_binaries` stages each binary and
    /// then commits it with an atomic rename, so two updates running at once race
    /// over the same files -- and both would toast the same "Updated to v…". A
    /// duplicate is easy to add by accident, because v1 did this in `main` under a
    /// different name and porting it here looks like filling a gap rather than
    /// doubling one.
    ///
    /// `cli::update` is the deliberate other caller: a person typing
    /// `thurbox-cli update` is not the interface updating itself.
    #[test]
    fn only_this_module_installs_a_release() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let allowed = ["kernel/updates.rs", "cli/update.rs", "agent/self_update.rs"];
        let mut offenders = Vec::new();

        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let rel = path
                    .strip_prefix(&root)
                    .expect("under src")
                    .to_string_lossy()
                    .replace('\\', "/");
                if allowed.contains(&rel.as_str()) {
                    continue;
                }
                let body = std::fs::read_to_string(&path).expect("read");
                if body.contains("perform_update(") {
                    offenders.push(rel);
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "these install releases besides kernel::updates, which races it: {offenders:?}"
        );
    }

    use super::*;

    fn features(version_check: bool, auto_update: bool) -> FeatureFlags {
        FeatureFlags {
            version_check,
            auto_update,
            ..FeatureFlags::default()
        }
    }

    #[test]
    fn both_switches_off_does_nothing_at_all() {
        // Not a fetch, not a thread, not a cache read: the point of the switch is
        // that the network is never touched.
        let updates = Updates::start(&features(false, false));
        assert!(updates.available().is_none());
        assert!(
            updates.channel.is_none(),
            "no worker may be started when neither switch is on"
        );
    }

    #[test]
    fn nothing_is_reported_until_something_was_installed() {
        let mut updates = Updates::default();
        assert_eq!(updates.poll(), None);
    }

    #[test]
    fn a_finished_check_updates_the_badge_without_a_message() {
        // A newer release is shown, not announced: v1 puts it in the header and
        // says nothing, because the user did not ask for anything.
        let mut updates = Updates::default();
        let tx = updates.sender();
        tx.send(Done::Checked(Some("9.9.9".into()))).expect("send");
        assert_eq!(updates.poll(), None);
        assert_eq!(updates.available(), Some("9.9.9"));
    }

    #[test]
    fn an_install_is_reported_because_it_changed_the_machine() {
        let mut updates = Updates::default();
        let tx = updates.sender();
        tx.send(Done::Installed("9.9.9".into())).expect("send");
        let message = updates.poll().unwrap_or_default();
        assert!(message.contains("9.9.9"), "{message}");
        assert!(
            message.contains("restart"),
            "the new version applies on the next launch, and has to say so: {message}"
        );
        assert_eq!(updates.available(), Some("9.9.9"));
    }

    #[test]
    fn a_check_that_finds_nothing_clears_the_badge() {
        let mut updates = Updates {
            latest: Some("1.0.0".into()),
            ..Updates::default()
        };
        let tx = updates.sender();
        tx.send(Done::Checked(None)).expect("send");
        updates.poll();
        assert!(updates.available().is_none());
    }
}
