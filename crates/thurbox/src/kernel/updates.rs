use std::sync::mpsc::{channel, Receiver, Sender};

use crate::session::settings::FeatureFlags;

fn log_next_major(next: Option<&str>) {
    if let Some(next) = next {
        tracing::info!(
            "auto-update: v{next} is a new major line — not installed. \
             Moving to it is a reinstall: {}",
            crate::update::version_check::MIGRATION_NOTES_URL
        );
    }
}

enum Done {
    Checked(Option<String>),
    Installed { to: String, fleet: String },
}

#[derive(Default)]
pub struct Updates {
    latest: Option<String>,
    channel: Option<(Sender<Done>, Receiver<Done>)>,
}

impl Updates {
    pub fn start(features: &FeatureFlags, fleets: Vec<crate::fleet_link::FleetLink>) -> Self {
        let mut updates = Self::default();
        if !features.version_check && !features.auto_update {
            return updates;
        }

        if features.version_check {
            // Instant, and usually enough: the badge is right from the first
            // frame when the cache is warm.
            updates.latest = crate::update::version_check::read_cached_status().map(|s| s.latest);
            if crate::update::version_check::cache_is_stale() {
                let tx = updates.sender();
                std::thread::spawn(move || {
                    let latest = match crate::update::version_check::refresh_cache() {
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
                match crate::update::self_update::perform_update(false) {
                    Ok(crate::update::self_update::UpdateOutcome::Updated {
                        to,
                        next_major,
                        ..
                    }) => {
                        log_next_major(next_major.as_deref());
                        // The check's cache is refreshed with it, so the badge
                        // agrees with what is now installed on the next launch.
                        let _ = crate::update::version_check::refresh_cache();
                        let fleet = crate::kernel::snapshot::fleet::stale_managers(&fleets, &to);
                        let _ = tx.send(Done::Installed { to, fleet });
                    }
                    Ok(crate::update::self_update::UpdateOutcome::UpToDate {
                        next_major, ..
                    }) => log_next_major(next_major.as_deref()),
                    // Up to date, or a dev build — `perform_update` skips those
                    // itself, which is why nothing is decided here.
                    Ok(_) => {}
                    Err(e) => tracing::warn!("auto-update failed: {e}"),
                }
            });
        }
        updates
    }

    pub fn available(&self) -> Option<&str> {
        self.latest.as_deref()
    }

    pub fn poll(&mut self) -> Option<String> {
        let mut message = None;
        let Some((_, rx)) = &self.channel else {
            return None;
        };
        while let Ok(done) = rx.try_recv() {
            match done {
                Done::Checked(latest) => self.latest = latest,
                Done::Installed { to, fleet } => {
                    let mut text = format!("Updated to v{to} — restart thurbox to apply.");
                    if !fleet.is_empty() {
                        text.push(' ');
                        text.push_str(&fleet);
                    }
                    self.latest = Some(to);
                    message = Some(text);
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
    #[test]
    fn only_this_module_installs_a_release() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let allowed = [
            "kernel/updates.rs",
            "cli/update.rs",
            "update/self_update.rs",
        ];
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
        let updates = Updates::start(&features(false, false), Vec::new());
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
        tx.send(Done::Installed {
            to: "9.9.9".into(),
            fleet: String::new(),
        })
        .expect("send");
        let message = updates.poll().unwrap_or_default();
        assert!(message.contains("9.9.9"), "{message}");
        assert!(
            message.contains("restart"),
            "the new version applies on the next launch, and has to say so: {message}"
        );
        assert_eq!(updates.available(), Some("9.9.9"));
    }

    #[test]
    fn an_install_says_how_to_restart_the_fleet_manager() {
        let mut updates = Updates::default();
        let tx = updates.sender();
        let fleet = "Fleet local's manager still runs the old release · systemctl --user restart fleet.service.".to_string();
        tx.send(Done::Installed {
            to: "9.9.9".into(),
            fleet: fleet.clone(),
        })
        .expect("send");
        let message = updates.poll().unwrap_or_default();
        assert!(message.ends_with(&fleet), "{message}");
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
