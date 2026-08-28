//! Rebuilding the `thurbox.*` tables plugins read.
//!
//! `republish` runs once per painted frame and once per input *batch*, not once
//! per event — a held-down key otherwise paid for it per repeat. Within it every
//! group is gated on a change-signal, so a group whose inputs did not move is
//! not rebuilt at all (ADR-P16), and the three reads that touch a screen or the
//! disk carry an age rather than a "we have an answer" flag (ADR-P14).

use super::*;

impl App {
    /// Rebuild everything a plugin can read.
    ///
    /// Called just before a paint and before dispatching input — the only two
    /// moments Lua runs — rather than once per loop iteration.
    pub(crate) fn republish(&mut self) {
        self.advance_animation();
        let sessions: Vec<String> = self
            .snapshots
            .current()
            .sessions
            .iter()
            .map(|row| row.id.clone())
            .collect();
        self.refresh_links(&sessions);
        self.refresh_search_content(&sessions);
        // Generation-gated, so an idle session costs one atomic load. The
        // mutating half runs here; the map itself is borrowed below, after the
        // last `&mut self` call — cloning it to end this borrow cost two
        // `String`s per live session per publish.
        self.terminals.sync_meta();
        let attach_errors = self.terminals.failures();
        let inflight = self.commands.inflight();
        let focus = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.plugins.get(*index))
            .map(|plugin| plugin.name.to_string());
        // What is on screen is known from the frame just painted, which is what
        // makes "its slot is not in the arrangement" an observation rather than
        // a guess: placement depends on the terminal's current size.
        let visible: std::collections::HashSet<usize> = (0..self.host.plugins.len())
            .filter(|index| self.is_visible_plugin(*index))
            .collect();
        // Before the borrows below: it re-reads the directory when something says
        // the answer moved, which needs `self` mutably.
        self.refresh_trust();
        let ui_dir = self.ui_dir.clone();
        let registry = &self.registry;
        let trust = &self.trust;
        self.inventory = thurbox::kernel::inventory::rows(
            &self.host.plugins,
            &self.sources,
            &visible,
            &self.visible_slots,
            self.host.error.as_deref(),
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
        if let Err(e) = self.host.publish(&thurbox::kernel::host::Published {
            epoch: thurbox::kernel::host::Epoch {
                snapshot: self.snapshots.version(),
                themes: self.themes.version(),
                registry: self.registry.version(),
                meta: self.terminals.meta_version(),
                failed: self.terminals.failed_version(),
                data: self.data_epoch,
                animation: self.animation_tick,
            },
            snapshot: self.snapshots.current(),
            attach_errors: &attach_errors,
            inflight: &inflight,
            themes: &self.themes,
            registry: &self.registry,
            diffs: &self.diffs,
            links: &self.links,
            content: &self.content,
            meta,
            metrics: &self.metrics,
            status_rows: self.status_rows(),
            can_open: browser_available(),
            focus: focus.as_deref(),
            hovered: self.hovered.as_ref(),
            inventory: &inventory,
            ui_dir: &ui_dir,
            settings: self.config.in_force(),
            repos: &self.repos,
            wants: &self.repo_wants(),
        }) {
            self.layout_error = Some(format!("publishing the snapshot failed: {e}"));
        }
        // Put it back: the settings modal's Interface tab lists the same rows,
        // and recomputing them there would mean a second join against a frame
        // that has already been painted.
        self.inventory = inventory;
    }

    /// Publish the world for this batch of input, unless it already was.
    ///
    /// See the drain loop for why once per batch is enough.
    pub(crate) fn publish_for_batch(&mut self, published: &mut bool) {
        if !*published {
            self.republish();
            *published = true;
        }
    }

    /// Re-read where each file of the interface came from.
    ///
    /// On reload, and after this process edits one — the two moments the answer
    /// can have changed. Not per frame: it digests every file.
    pub(crate) fn refresh_sources(&mut self) {
        self.sources = thurbox::kernel::bundled::sources(&self.ui_dir);
        // Delivery just re-read the directory, so whatever the cached trust
        // answers were digested from is no longer the file on disk.
        self.trust_stale = true;
    }

    /// Rescan for the links on each session's screen, where that screen moved.
    ///
    /// Part of publishing rather than a standing cost of the loop, because the
    /// answer is only ever read by a plugin — and gated on the session's
    /// `output_stamp`, because finding them walks every cell of its grid building
    /// a `String` per row. Ungated, a held-down key rescanned every terminal on
    /// screen per repeat for answers that could not have changed.
    pub(crate) fn refresh_links(&mut self, sessions: &[String]) {
        for id in sessions {
            // Only surfaces that are ON SCREEN. Extracting links walks the whole
            // vt100 grid and URL-scans every row, and doing that for every
            // session with a live pane cost ~1.2ms a frame with three of them —
            // for answers nothing could use, since a link that is not painted
            // can be neither clicked nor handed to the outer terminal. v1 asked
            // only the active session for the same reason.
            //
            // The rects are last frame's (this runs before the paint that
            // clears them), so a surface that has just appeared gets its links
            // on the following frame rather than this one.
            if self.terminals.last_rect(id).is_none() {
                self.link_stamps.remove(id);
                if self.links.remove(id).is_some() {
                    self.note_published_change();
                }
                continue;
            }
            match self.terminals.output_stamp(id) {
                // Unchanged since the last scan: the answer stands.
                Some(stamp) if self.link_stamps.get(id) == Some(&stamp) => {}
                Some(stamp) => {
                    self.link_stamps.insert(id.clone(), stamp);
                    let found = self.terminals.links(id);
                    // Absent rather than empty when there are none, which is the
                    // shape a plugin reads: `thurbox.links[id]` is nil for a
                    // session with no links, not a table with nothing in it.
                    //
                    // Compared before storing: a printing agent moves its stamp
                    // every frame while the links on screen usually stay put, and
                    // treating a re-scan as a change would move the epoch every
                    // frame for nothing.
                    let changed = if found.is_empty() {
                        self.links.remove(id).is_some()
                    } else if self.links.get(id) == Some(&found) {
                        false
                    } else {
                        self.links.insert(id.clone(), found);
                        true
                    };
                    if changed {
                        self.note_published_change();
                    }
                }
                // No live pane, so no screen and no links.
                None => {
                    self.link_stamps.remove(id);
                    self.links.remove(id);
                }
            }
        }
        // A session that left the snapshot takes its cached answer with it.
        let known: std::collections::HashSet<&str> = sessions.iter().map(String::as_str).collect();
        self.links.retain(|id, _| known.contains(id.as_str()));
        self.link_stamps.retain(|id, _| known.contains(id.as_str()));
    }

    /// Read what each terminal is showing, while something is searching them.
    ///
    /// The scan is the same walk [`Self::refresh_links`] makes, so serving it
    /// always would not be ruinous — it would simply be paid by every interface
    /// that never searches, which is the argument for asking. Gated a second time
    /// on the screens having *moved*: a query is asked for once and then read on
    /// every frame and every keystroke of it being narrowed, and re-reading every
    /// grid for each of those is the whole cost of typing in the search strip.
    pub(crate) fn refresh_search_content(&mut self, sessions: &[String]) {
        let asked = self
            .host
            .shared_string(thurbox::kernel::terminal::WANT_CONTENT)
            .is_some_and(|query| !query.is_empty());
        if !asked {
            // Dropped the moment nothing is searching, so a closed strip is not
            // still holding every screen it scanned.
            if self.content_generation.is_some() {
                self.content = std::collections::HashMap::new();
                self.content_generation = None;
                self.note_published_change();
            }
            return;
        }
        let generation = self.terminals.output_generation();
        if self.content_generation != Some(generation) {
            let scanned = self.terminals.screens(sessions);
            if scanned != self.content {
                self.content = scanned;
                self.note_published_change();
            }
            self.content_generation = Some(generation);
        }
    }

    /// Re-read where each file of the interface stands with the user.
    ///
    /// Answering it reads and digests every file in the directory and parses
    /// `plugins.lock`, so it is done when something says the answer moved rather
    /// than per publish. Trust is keyed by absolute path, and drift is answered by
    /// reading the file; an INSTALLED file is judged differently (its
    /// `src@version` as well as its contents), and `trust_of` owns that split so
    /// no caller can check only one half of it.
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

    /// Tell the host which plugins the user trusted, by the path it knows them by.
    ///
    /// Relative, because that is `Plugin::path`; trust is stored absolute so two
    /// interface directories cannot share it, so this is where the two meet.
    pub(crate) fn publish_trust(&self) {
        let trusted: Vec<String> = self
            .host
            .plugins
            .iter()
            .map(|plugin| plugin.path.clone())
            .filter(|path| {
                let absolute = self.ui_dir.join(path);
                self.registry.is_trusted(&absolute.to_string_lossy())
            })
            .collect();
        self.host.set_trusted(trusted);
    }

    pub(crate) fn publish_disabled(&self) {
        let disabled: Vec<String> = self
            .registry
            .disabled()
            .filter_map(|absolute| thurbox::kernel::bundled::relative_to(&self.ui_dir, absolute))
            .collect();
        self.host.set_disabled(disabled);
    }

    /// What the creation flow is asking about, read off the shared `store`.
    ///
    /// Absent means not asking, which is why the local machine is an *empty*
    /// host rather than an absent one: a closed flow must cost nothing, and
    /// asking about local has to be expressible (`kernel::repos::Wants`).
    pub(crate) fn repo_wants(&self) -> thurbox::kernel::repos::Wants {
        use thurbox::kernel::repos::{Wants, WANT_BOOKMARKS, WANT_BRANCHES, WANT_BROWSE};
        Wants::new(
            self.host.shared_string(WANT_BOOKMARKS),
            self.host.shared_string(WANT_BROWSE),
            self.host.shared_string(WANT_BRANCHES),
        )
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
                agent: row.agent.clone(),
                host: row.remote_host.clone(),
            })
            .collect()
    }

    /// Hand the registry what every loaded plugin declared, plus the kernel's
    /// own chords.
    ///
    /// The system modals and the clipboard pair go through the same registry as
    /// everything else, which is what makes them listable in help, conflict-checked
    /// against a plugin's keys, and rebindable — they simply have no Lua plugin
    /// behind them.
    pub(crate) fn collect_declarations(&mut self) {
        let (mut bindings, settings, mut pills) = self.host.all_declarations();
        bindings.extend(thurbox::kernel::modals::bindings());
        bindings.extend(thurbox::kernel::clipboard::bindings());
        pills.extend(thurbox::kernel::modals::pills());
        self.registry.declare_all(bindings, settings, pills);
        self.registry.declare_commands(self.host.commands());
        // Trust is read against the plugin set that just loaded, so a reload —
        // including the one a trust change triggers — lands both together.
        self.publish_trust();
    }
}
