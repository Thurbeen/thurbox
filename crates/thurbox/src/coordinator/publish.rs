use std::time::Instant;

use thurbox::kernel::metrics::Subject;

use super::browser_available;
use crate::{App, LINK_SCAN_INTERVAL};

impl App {
    pub(crate) fn republish(&mut self) {
        self.terminals.sync_printing();
        self.advance_animation();
        let sessions: Vec<String> = self
            .snapshots
            .current()
            .sessions
            .iter()
            .map(|row| row.id.clone())
            .collect();
        let surfaces: Vec<String> = sessions
            .iter()
            .flat_map(|id| [id.clone(), thurbox::kernel::terminal::shell_surface(id)])
            .collect();
        self.refresh_links(&surfaces);
        self.terminals.sync_meta();
        let attach_errors = self.terminals.failures();
        let starts = self.terminals.starts();
        let inflight = self.commands.inflight();
        let focus = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.mods.get(*index))
            .map(|loaded| loaded.name.to_string());
        let visible: std::collections::HashSet<usize> = (0..self.host.mods.len())
            .filter(|index| self.is_visible_mod(*index))
            .collect();
        self.refresh_trust();
        let ui_dir = self.ui_dir.clone();
        let registry = &self.registry;
        let trust = &self.trust;
        self.inventory = thurbox::kernel::inventory::rows(
            &self.host.mods,
            &self.sources,
            &visible,
            &self.visible_slots,
            self.host.error.as_deref().or(self.floor_cause.as_deref()),
            &|path| {
                trust
                    .get(path)
                    .copied()
                    .unwrap_or(thurbox::kernel::inventory::Trust::NotAsked)
            },
            &|path| registry.is_disabled(&ui_dir.join(path).to_string_lossy()),
        );
        let inventory = std::mem::take(&mut self.inventory);
        let ui_dir = self.ui_dir.display().to_string();
        let meta = self.terminals.meta_map();
        self.host.set_key_releases(self.holds.releases);
        if let Err(e) = self.host.publish(&thurbox::kernel::host::Published {
            epoch: thurbox::kernel::host::Epoch {
                snapshot: self.snapshots.version(),
                themes: self.themes.version(),
                registry: self.registry.version(),
                meta: self.terminals.meta_version(),
                failed: self.terminals.failed_version(),
                data: self.data_epoch,
                animation: self.animation_tick,
                printing: self.terminals.printing_version(),
                taken_at: self.snapshots.current().taken_at_ms as u64,
                metrics: self.metrics.version(),
            },
            snapshot: self.snapshots.current(),
            attach_errors: &attach_errors,
            starts: &starts,
            printing: self.terminals.printing(),
            inflight: &inflight,
            themes: &self.themes,
            registry: &self.registry,
            diffs: &self.diffs,
            links: &self.links,
            search: self.search.answer(),
            meta,
            metrics: &self.metrics,
            status_rows: self.status_rows(),
            can_open: browser_available(),
            focus: focus.as_deref(),
            selection: self.selected_text.as_deref(),
            hovered: self.hovered.as_ref(),
            inventory: &inventory,
            ui_dir: &ui_dir,
            settings: self.config.in_force(),
            repos: &self.repos,
            wants: &self.repo_wants(),
        }) {
            self.layout_error = Some(format!("publishing the snapshot failed: {e}"));
        }
        self.inventory = inventory;
    }

    pub(crate) fn publish_for_batch(&mut self, published: &mut bool) {
        if !*published {
            self.republish();
            *published = true;
        }
    }

    pub(crate) fn refresh_sources(&mut self) {
        self.sources = thurbox::kernel::bundled::sources(&self.ui_dir);
        self.trust_stale = true;
    }

    fn scan_links(&mut self, id: &str, stamp: u64, now: Instant) {
        self.link_stamps.insert(id.to_string(), stamp);
        self.link_scans.insert(id.to_string(), now);
        let found = self.terminals.links(id);
        let changed = if found.is_empty() {
            self.links.remove(id).is_some()
        } else if self.links.get(id) == Some(&found) {
            false
        } else {
            self.links.insert(id.to_string(), found);
            true
        };
        if changed {
            self.note_published_change();
        }
    }

    pub(crate) fn refresh_links(&mut self, surfaces: &[String]) {
        let now = Instant::now();
        for id in surfaces {
            if self.terminals.last_rect(id).is_none() {
                self.link_stamps.remove(id);
                self.link_scans.remove(id);
                if self.links.remove(id).is_some() {
                    self.note_published_change();
                }
                continue;
            }
            let stamp = self.terminals.output_stamp(id);
            match plan_link_scan(id, stamp, &self.link_stamps, &self.link_scans, now) {
                LinkScan::Keep => {}
                LinkScan::Scan(stamp) => self.scan_links(id, stamp, now),
                LinkScan::Gone => {
                    self.link_stamps.remove(id);
                    self.link_scans.remove(id);
                    self.links.remove(id);
                }
            }
        }
        let known: std::collections::HashSet<&str> = surfaces.iter().map(String::as_str).collect();
        self.links.retain(|id, _| known.contains(id.as_str()));
        self.link_stamps.retain(|id, _| known.contains(id.as_str()));
        self.link_scans.retain(|id, _| known.contains(id.as_str()));
    }

    pub(crate) fn serve_search(&mut self) {
        use thurbox::kernel::search::{Request, WANT_CONTENT, WANT_SESSIONS};
        let request = self.host.shared_string(WANT_CONTENT).map(|query| Request {
            query,
            sessions: self
                .host
                .shared_string(WANT_SESSIONS)
                .map(|ids| ids.split_whitespace().map(str::to_string).collect()),
        });
        let generation = self.terminals.output_generation();
        let (snapshots, terminals) = (&self.snapshots, &self.terminals);
        let dropped = self.search.serve(request, generation, |request| {
            let wanted: Vec<String> = snapshots
                .current()
                .sessions
                .iter()
                .map(|row| &row.id)
                .filter(|id| {
                    request
                        .sessions
                        .as_ref()
                        .is_none_or(|only| only.contains(id))
                })
                .cloned()
                .collect();
            terminals.search_sources(&wanted)
        });
        if dropped {
            self.note_data_change();
        }
        if self.search.poll() {
            self.note_data_change();
        }
    }

    pub(crate) fn refresh_trust(&mut self) {
        if !self.trust_stale {
            return;
        }
        let lock = thurbox::kernel::packages::read_lock(&self.ui_dir).unwrap_or_default();
        self.trust = self
            .sources
            .keys()
            .map(|path| {
                (
                    path.clone(),
                    thurbox::kernel::packages::trust_of(&self.ui_dir, path, &lock, &self.registry),
                )
            })
            .collect();
        self.trust_stale = false;
    }

    pub(crate) fn publish_trust(&self) {
        self.host.set_trusted(thurbox::kernel::packages::granted(
            &self.ui_dir,
            self.host.mods.iter().map(|loaded| loaded.path.as_str()),
            &self.registry,
        ));
    }

    pub(crate) fn publish_disabled(&self) {
        let disabled: Vec<String> = self
            .registry
            .disabled()
            .filter_map(|absolute| thurbox::kernel::bundled::relative_to(&self.ui_dir, absolute))
            .collect();
        self.host.set_disabled(disabled);
    }

    pub(crate) fn repo_wants(&self) -> thurbox::kernel::repos::Wants {
        use thurbox::kernel::repos::{Wants, WANT_BOOKMARKS, WANT_BRANCHES, WANT_BROWSE};
        Wants::new(
            self.host.shared_string(WANT_BOOKMARKS),
            self.host.shared_string(WANT_BROWSE),
        )
        .with_branches(self.host.shared_string(WANT_BRANCHES))
    }

    pub(crate) fn metric_subjects(&self) -> Vec<Subject> {
        self.snapshots
            .current()
            .sessions
            .iter()
            .map(|row| Subject {
                session: row.id.clone(),
                agent_session_id: row.agent_session_id.clone(),
                pane: self.terminals.backend_handle(&row.id),
            })
            .collect()
    }

    pub(crate) fn collect_declarations(&mut self) {
        thurbox::kernel::declare_interface(&mut self.registry, &self.host);
        self.publish_trust();
    }
}

fn link_scan_due(last_scan: Option<Instant>, now: Instant) -> bool {
    last_scan.is_none_or(|at| now.duration_since(at) >= LINK_SCAN_INTERVAL)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkScan {
    Keep,
    Scan(u64),
    Gone,
}

fn plan_link_scan(
    id: &str,
    stamp: Option<u64>,
    scanned_stamps: &std::collections::HashMap<String, u64>,
    scanned_at: &std::collections::HashMap<String, Instant>,
    now: Instant,
) -> LinkScan {
    match stamp {
        Some(stamp) if scanned_stamps.get(id) == Some(&stamp) => LinkScan::Keep,
        Some(_) if !link_scan_due(scanned_at.get(id).copied(), now) => LinkScan::Keep,
        Some(stamp) => LinkScan::Scan(stamp),
        None => LinkScan::Gone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::{FORCE_REDRAW_INTERVAL, OUTPUT_FRAME_INTERVAL};

    #[test]
    fn a_screen_is_rescanned_only_once_it_moved_and_the_interval_allows() {
        use std::collections::HashMap;
        let now = Instant::now();
        let later = now + LINK_SCAN_INTERVAL;
        let stamps = HashMap::from([("s".to_string(), 7)]);
        let scanned_now = HashMap::from([("s".to_string(), now)]);
        let never: HashMap<String, Instant> = HashMap::new();

        assert_eq!(
            plan_link_scan("s", None, &stamps, &scanned_now, now),
            LinkScan::Gone
        );
        assert_eq!(
            plan_link_scan("s", Some(7), &stamps, &never, later),
            LinkScan::Keep,
            "an unchanged surface keeps its answer however old"
        );
        assert_eq!(
            plan_link_scan("s", Some(8), &stamps, &scanned_now, now),
            LinkScan::Keep,
            "a moved surface is not scanned again before the interval"
        );
        assert_eq!(
            plan_link_scan("s", Some(8), &stamps, &scanned_now, later),
            LinkScan::Scan(8),
            "a moved surface is scanned once the interval has passed"
        );
        assert_eq!(
            plan_link_scan("s", Some(8), &stamps, &never, now),
            LinkScan::Scan(8),
            "a moved surface never scanned is scanned at once"
        );
        assert_eq!(
            plan_link_scan("new", Some(1), &stamps, &scanned_now, now),
            LinkScan::Scan(1)
        );
    }

    #[test]
    fn a_surface_never_scanned_is_due_at_once() {
        assert!(link_scan_due(None, Instant::now()));
    }

    #[test]
    fn a_surface_just_scanned_is_not_due_again() {
        let now = Instant::now();
        assert!(!link_scan_due(Some(now), now));
        assert!(!link_scan_due(
            Some(now),
            now + LINK_SCAN_INTERVAL - Duration::from_millis(1)
        ));
    }

    #[test]
    fn a_surface_scanned_longer_ago_than_the_interval_is_due() {
        let now = Instant::now();
        assert!(link_scan_due(Some(now - LINK_SCAN_INTERVAL), now));
        assert!(link_scan_due(
            Some(now - LINK_SCAN_INTERVAL - Duration::from_secs(1)),
            now
        ));
    }

    #[test]
    fn the_scan_interval_is_slower_than_the_frames_it_paces() {
        assert!(
            LINK_SCAN_INTERVAL > OUTPUT_FRAME_INTERVAL,
            "the link scan would still run on every output frame"
        );
        assert!(
            LINK_SCAN_INTERVAL <= FORCE_REDRAW_INTERVAL,
            "links could lag a settled screen by more than a forced redraw"
        );
    }
}
