use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::project;

/// Events emitted when watched config files change on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncEvent {
    ProjectConfigChanged,
    SessionStateChanged,
}

/// Minimum interval (ms) to ignore file events after our own write.
const SELF_WRITE_DEBOUNCE_MS: u64 = 200;

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Watches `config.toml` and `state.toml` for external changes and sends
/// [`SyncEvent`] notifications through an mpsc channel.
pub struct FileWatcher {
    _watcher: RecommendedWatcher,
    last_config_write: Arc<AtomicU64>,
    last_state_write: Arc<AtomicU64>,
}

impl FileWatcher {
    /// Create a new file watcher. Returns the watcher handle and a receiver
    /// for sync events. The watcher must be kept alive for events to flow.
    pub fn new() -> Result<(Self, mpsc::UnboundedReceiver<SyncEvent>)> {
        let (tx, rx) = mpsc::unbounded_channel();

        let config_path = project::config_path();
        let state_path = project::state_path();

        let last_config_write = Arc::new(AtomicU64::new(0));
        let last_state_write = Arc::new(AtomicU64::new(0));

        let config_clone = config_path.clone();
        let state_clone = state_path.clone();
        let lcw = Arc::clone(&last_config_write);
        let lsw = Arc::clone(&last_state_write);

        let mut watcher =
            notify::recommended_watcher(move |res: std::result::Result<Event, notify::Error>| {
                let event = match res {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("File watcher error: {e}");
                        return;
                    }
                };

                // Only react to data-modifying events.
                if !matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    return;
                }

                let now = now_millis();

                for path in &event.paths {
                    if config_clone.as_deref() == Some(path.as_path()) {
                        let last = lcw.load(Ordering::Relaxed);
                        if now.saturating_sub(last) > SELF_WRITE_DEBOUNCE_MS {
                            debug!("Config file changed externally");
                            let _ = tx.send(SyncEvent::ProjectConfigChanged);
                        }
                    } else if state_clone.as_deref() == Some(path.as_path()) {
                        let last = lsw.load(Ordering::Relaxed);
                        if now.saturating_sub(last) > SELF_WRITE_DEBOUNCE_MS {
                            debug!("State file changed externally");
                            let _ = tx.send(SyncEvent::SessionStateChanged);
                        }
                    }
                }
            })
            .context("Failed to create file watcher")?;

        // Watch the parent directories (the files might not exist yet).
        if let Some(ref config) = config_path {
            if let Some(parent) = config.parent() {
                std::fs::create_dir_all(parent).ok();
                watcher
                    .watch(parent, RecursiveMode::NonRecursive)
                    .context("Failed to watch config directory")?;
                debug!("Watching config dir: {}", parent.display());
            }
        }
        if let Some(ref state) = state_path {
            if let Some(parent) = state.parent() {
                std::fs::create_dir_all(parent).ok();
                // Config and state may share the same parent; watching twice is harmless.
                let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
                debug!("Watching state dir: {}", parent.display());
            }
        }

        Ok((
            Self {
                _watcher: watcher,
                last_config_write,
                last_state_write,
            },
            rx,
        ))
    }

    /// Record that we just wrote to the config file so the watcher
    /// ignores the resulting filesystem event.
    pub fn mark_config_write(&self) {
        self.last_config_write
            .store(now_millis(), Ordering::Relaxed);
    }

    /// Record that we just wrote to the state file so the watcher
    /// ignores the resulting filesystem event.
    pub fn mark_state_write(&self) {
        self.last_state_write.store(now_millis(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_event_variants_are_distinct() {
        assert_ne!(
            SyncEvent::ProjectConfigChanged,
            SyncEvent::SessionStateChanged
        );
    }

    #[test]
    fn debounce_constant_is_reasonable() {
        let ms = SELF_WRITE_DEBOUNCE_MS;
        assert!(ms >= 100);
        assert!(ms <= 1000);
    }
}
