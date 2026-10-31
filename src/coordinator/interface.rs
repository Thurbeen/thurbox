//! Reloading the interface, and the user's decisions about it.
//!
//! Trust, the disabled set and a switch slot's selection are all *user*
//! decisions (`ui.json`), distinct from the delivery facts in `.bundled.json`.
//! Each of the four Interface-tab keys reloads, because a plugin's capabilities
//! are installed when its environment is built: revoking has to rebuild it, so
//! the capability is *absent* on the next frame rather than present and
//! refusing.

use std::time::{Duration, Instant};

use thurbox::kernel::bands::Level;
use thurbox::kernel::modals::Modals;
use thurbox::kernel::perf::Counters;

use crate::{App, DEBOUNCE};

/// How soon after a memory rebuild pressure again means the rebuild did not
/// help. Well under the time a leaking plugin takes to refill the VM — about
/// 30 s at 2 MiB a render — and far over the few frames an interface takes to
/// reach its resident size.
const MEMORY_REFILL: Duration = Duration::from_secs(10);

impl App {
    /// Rebuild the interface from the directory the user actually edits, dropping
    /// to the bundled copy only for as long as that one will not load.
    ///
    /// Every reload goes through here rather than `host.reload()` because the host
    /// rebuilds from whichever directory it was last built from. Once the floor had
    /// been installed, that was `/tmp`: the watcher fired on the user's fix, the
    /// fallback was rebuilt from itself, nothing changed, and no error appeared —
    /// while `ui_dir`, the watcher, the inventory and every Interface-tab action
    /// still pointed at the user's copy, so `r`/`d`/`space`/`t` wrote the right
    /// file to no visible effect. Only a restart recovered. It also cost a plugin
    /// author their edit-reload loop, since a `./ui` checkout takes the same path.
    pub(crate) fn reload_interface(&mut self, reason: super::events::ReloadReason) {
        let memory = reason == super::events::ReloadReason::Memory;
        if memory {
            // Transactional edit reloads retain the last good VM. An exhausted
            // VM cannot be that fallback: even reporting its error allocates.
            self.host = self.host.fresh_from(&self.ui_dir);
        } else {
            self.host.reload_from(&self.ui_dir);
            // The window measures the VM a memory rebuild produced; one the user
            // asked for is a fresh start.
            self.memory_rebuilt = None;
        }
        Counters::bump(&self.perf.reloads);
        if self.host.error.is_none() {
            self.floor_cause = None;
            if self.floor.take().is_some() {
                self.toast(format!("{} loaded again", self.ui_dir.display()));
            }
        } else if memory || std::env::var_os("THURBOX_UI_DIR").is_none() {
            self.install_bundled_floor();
        }
        self.settle_reload(reason);
    }

    /// What every reload ends with, whichever interface it put in.
    fn settle_reload(&mut self, reason: super::events::ReloadReason) {
        let memory = reason == super::events::ReloadReason::Memory;
        // A failed memory rebuild retired every plugin without loading any.
        // Clear old events, but do not announce a successful interface reload.
        self.note_reload(if memory && self.host.error.is_some() {
            super::events::ReloadReason::Boot
        } else {
            reason
        });
        // Plugin indices are positions in a vector the rebuild just replaced, and
        // `grabbed` and every click target are ones recorded during the previous
        // paint. A reload between a paint and an input — a watcher firing on a
        // deleted file — would leave them pointing past the end, or at whichever
        // plugin moved into that position, which then receives a press on a node
        // it never painted (#1118). The next paint records them again.
        self.grabbed = None;
        self.holds.clear();
        self.click_targets.clear();
        self.last_floats.clear();
        self.drawn_floats.clear();
        self.failing.clear();
        self.forget_unloaded_plugins();
    }

    /// Swap in the bundled interface because the user's will not load, saying why.
    /// Whether it went in.
    fn install_bundled_floor(&mut self) -> bool {
        let Ok(dir) = thurbox::kernel::bundled::fallback_dir() else {
            return false;
        };
        let embedded = self.host.fresh_from(&dir);
        if embedded.error.is_some() {
            return false;
        }
        // Recorded before the swap: the reason is the *user's* error, and the
        // host that carries it is about to be replaced by one with none.
        self.floor_cause = self.host.error.clone();
        self.floor = Some(format!(
            "using the bundled interface — {} failed to load: {}",
            self.ui_dir.display(),
            self.floor_cause.clone().unwrap_or_default()
        ));
        self.host = embedded;
        true
    }

    /// Replace a full VM with the bundled interface without building the
    /// user's again: it filled the last fresh VM it was given (`cause`).
    fn floor_refilled_interface(&mut self, cause: String) {
        self.host.error = Some(cause.clone());
        if !self.install_bundled_floor() {
            // The full VM must not stay, and the user's interface is what filled
            // it: an empty VM, with the recovery keys in the kernel's chrome.
            self.host = self.host.retired(cause);
        }
        Counters::bump(&self.perf.reloads);
        self.settle_reload(super::events::ReloadReason::Memory);
    }

    /// Drop what plugins that are no longer loaded were holding.
    fn forget_unloaded_plugins(&mut self) {
        // A plugin that was edited away, renamed, removed or turned off must not
        // leave its answers behind to accumulate across reloads — and must not be
        // able to read them again if a file of that name comes back.
        let live: Vec<String> = self
            .host
            .plugins
            .iter()
            .map(|plugin| plugin.path.clone())
            .collect();
        self.runs.retain_plugins(&live);
        // The same rule for a program it was holding, and here it matters more: an
        // answer left behind is a few bytes, a program left behind is a process
        // nothing on screen can ever reach again. A plugin that is still loaded
        // keeps its pane — a reload is an edit to a file, and losing your game to
        // one would make reloading unusable.
        self.terminals.retain_program_plugins(&live);
    }

    /// Turn one interface file off, or back on, and reload.
    ///
    /// The reload is the mechanism, not a side effect: a disabled plugin is one
    /// `build` did not read, so the decision only becomes true when the VM is
    /// built again (design D2/D3). No confirmation — the action is reversible by
    /// the same key, and confirming a reversible act is what teaches people to
    /// confirm without reading.
    pub(crate) fn apply_switch(&mut self, file: &str, off: bool) {
        let absolute = self.ui_dir.join(file);
        let key = absolute.to_string_lossy().into_owned();
        match self.registry.set_disabled(&key, off) {
            Ok(now_off) => {
                self.publish_disabled();
                self.reload_interface(super::events::ReloadReason::Settings);
                self.collect_declarations();
                self.clamp_focus();
                self.refresh_sources();
                self.toast(if now_off {
                    format!("{file} turned off")
                } else {
                    format!("{file} turned on")
                });
            }
            Err(e) => self.report(e, Level::Error),
        }
    }

    /// Record the user's decision about one interface file, and reload.
    ///
    /// The reload is not incidental: a plugin's capabilities are installed when
    /// its environment is built, so revoking has to rebuild it — that is what
    /// makes the capability *absent* on the next frame rather than present and
    /// refusing (design D7).
    pub(crate) fn apply_trust(&mut self, file: &str, trusted: bool) {
        let absolute = self.ui_dir.join(file);
        let key = absolute.to_string_lossy().into_owned();
        let outcome = if trusted {
            match std::fs::read_to_string(&absolute) {
                // Trusted as it is *now*: the digest recorded here is what makes
                // "trusted, and since modified" a state the list can report.
                //
                // For an INSTALLED file the `src@version` is recorded beside it, so
                // the grant is a statement the user could actually have meant — "I
                // trust atlas v0.3.1" — and lapses when the pin moves instead of
                // reading as tampering after every ordinary release.
                Ok(contents) => {
                    let lock =
                        thurbox::kernel::packages::read_lock(&self.ui_dir).unwrap_or_default();
                    match lock.covering(file) {
                        Some(entry) => self
                            .registry
                            .trust_installed(&key, &entry.pin_key(), &contents)
                            .map(|()| format!("trusting {file} at {}", entry.pin_key())),
                        None => self
                            .registry
                            .trust(&key, &contents)
                            .map(|()| format!("trusting {file}")),
                    }
                }
                Err(e) => Err(format!("{}: {e}", absolute.display())),
            }
        } else {
            self.registry
                .revoke(&key)
                .map(|()| format!("no longer trusting {file}"))
        };
        match outcome {
            Ok(message) => {
                self.reload_interface(super::events::ReloadReason::Settings);
                self.collect_declarations();
                self.clamp_focus();
                self.refresh_sources();
                self.toast(message);
            }
            Err(e) => self.report(e, Level::Error),
        }
    }

    /// Take a saved draft into force and write it to the file.
    ///
    /// Two halves, in this order: the live flags apply now (that is what makes
    /// them live), and the file is written on a worker like everything else that
    /// touches the world — so a read-only filesystem surfaces as a reported
    /// failure rather than a silent one.
    pub(crate) fn apply_settings(&mut self, draft: thurbox::session::settings::Settings) {
        let outcome = self.config.adopt(draft.clone());
        self.config.mark_saved();
        self.commands
            .dispatch(thurbox::kernel::command::Command::Configure {
                settings: Box::new(draft),
            });
        if outcome == thurbox::kernel::config::Reloaded::NeedsRestart {
            self.toast("saved — some changes apply on restart".to_string());
        }
        self.dirty = true;
    }

    /// Restore or remove one interface file, and ask for the reload.
    ///
    /// One implementation for both callers — a plugin's `command("plugin", …)`
    /// and the settings modal's Interface tab — so the two cannot come to mean
    /// different things about what restoring is.
    pub(crate) fn apply_plugin_edit(
        &mut self,
        file: &str,
        edit: thurbox::kernel::command::PluginEdit,
    ) {
        let outcome = match edit {
            thurbox::kernel::command::PluginEdit::Restore => {
                thurbox::kernel::bundled::restore(&self.ui_dir, file).map(|backup| match backup {
                    Some(backup) => format!(
                        "restored {file} — your edited copy is kept as {}",
                        backup.display()
                    ),
                    None => format!("restored {file}"),
                })
            }
            thurbox::kernel::command::PluginEdit::Remove => {
                thurbox::kernel::bundled::remove(&self.ui_dir, file)
                    .map(|()| format!("removed {file}"))
            }
        };
        match outcome {
            Ok(message) => {
                // Removal changes no file the watcher is waiting on, so the
                // reload is asked for here.
                self.refresh_sources();
                self.reload_at = Some(Instant::now() + DEBOUNCE);
                self.toast(message);
            }
            Err(e) => self.report(e, Level::Error),
        }
    }

    pub(crate) fn with_modal_world<T>(
        &mut self,
        act: impl FnOnce(&mut Modals, &mut thurbox::kernel::modals::World<'_>) -> T,
    ) -> T {
        // The settings the modal shows, and the slot a save comes back through.
        // What the *file* holds rather than what is in force: the panel edits the
        // file, and a restart-only change lives only there until the next launch
        // (`Config::on_disk`). Cloned because the modal needs it while
        // `self.config` is borrowed mutably to apply whatever comes back.
        let on_disk = self.config.on_disk().clone();
        let mut saved = None;
        let mut edit = None;
        let mut run = None;
        // Same reason as the settings clone: the modal reads it while `self` is
        // borrowed mutably to apply whatever comes back.
        let inventory = std::mem::take(&mut self.inventory);
        let outcome = {
            let mut world = thurbox::kernel::modals::World {
                registry: &mut self.registry,
                themes: &mut self.themes,
                settings_on_disk: &on_disk,
                save_settings: &mut saved,
                inventory: &inventory,
                interface_edit: &mut edit,
                run_action: &mut run,
            };
            act(&mut self.modals, &mut world)
        };
        self.inventory = inventory;
        self.store_chosen_theme();
        if let Some(draft) = saved {
            self.apply_settings(draft);
        }
        // The palette's choice, run after the modal has closed itself.
        if let Some(dispatch) = run {
            self.run_action(&dispatch.plugin, &dispatch.action);
        }
        match edit {
            Some(thurbox::kernel::modals::interface::Edit::File { file, kind }) => {
                self.apply_plugin_edit(&file, kind);
            }
            Some(thurbox::kernel::modals::interface::Edit::Trust { file, trusted }) => {
                self.apply_trust(&file, trusted);
            }
            Some(thurbox::kernel::modals::interface::Edit::Switch { file, off }) => {
                self.apply_switch(&file, off);
            }
            None => {}
        }
        outcome
    }

    /// Report what just happened, for [`STATUS_TTL`], at informational level.
    pub(crate) fn toast(&mut self, message: impl Into<String>) {
        self.report(message, Level::Info);
    }

    /// Rebuild the interface once its VM has filled up — see
    /// [`thurbox::kernel::host::LuaHost::memory_pressure`]. Said out loud,
    /// because the panes lose whatever they held in memory and the cause is a
    /// plugin holding too much.
    ///
    /// A rebuild that did not help is not repeated: pressure again within
    /// [`MEMORY_REFILL`] of the last one means the interface, not a leak, is
    /// what fills the VM. Grown back is its size, so it is kept; exhausted
    /// again, it cannot run in the VM at all, so the bundled interface runs
    /// until the user's next reload. Either way the user's interface is
    /// rebuilt at most once per window, never on every loop iteration.
    pub(crate) fn rebuild_full_vm(&mut self) {
        use thurbox::kernel::host::MemoryPressure;
        let Some(pressure) = self.host.memory_pressure() else {
            return;
        };
        let used = self.host.used_memory() >> 20;
        let limit = thurbox::kernel::host::memory_limit() >> 20;
        let again = self
            .memory_rebuilt
            .is_some_and(|at| at.elapsed() < MEMORY_REFILL);
        if again && pressure == MemoryPressure::Grown {
            tracing::info!(
                "the plugin VM grew back to {used} of its {limit} MiB after a rebuild; kept as the interface's size"
            );
            self.host.accept_resident_memory();
            return;
        }
        if again {
            self.floor_refilled_interface(format!(
                "it filled a new plugin VM to its {limit} MiB limit within {} s of the last rebuild",
                MEMORY_REFILL.as_secs()
            ));
        } else {
            self.reload_interface(super::events::ReloadReason::Memory);
        }
        // From when the replacement is in, not when it was asked for: a slow
        // load must not use up the window before the new VM has run at all.
        self.memory_rebuilt = Some(Instant::now());
        self.collect_declarations();
        self.clamp_focus();
        if self.host.error.is_none() && again && self.floor.is_some() {
            tracing::warn!(
                "the plugin VM held {used} of its {limit} MiB again right after a rebuild; using the bundled interface"
            );
            self.report(
                format!(
                    "the interface filled its {limit} MiB plugin memory again right after a rebuild; using the bundled interface"
                ),
                Level::Error,
            );
        } else if self.host.error.is_none() {
            tracing::warn!("the plugin VM held {used} of its {limit} MiB; rebuilt the interface");
            self.report(
                format!(
                    "the interface reached its {limit} MiB plugin memory limit and was rebuilt"
                ),
                Level::Error,
            );
        } else {
            // An empty interface has no status band. Keep the outcome and the
            // recovery keys in the kernel error panel as well as the log.
            let cause = self.host.error.take().unwrap_or_default();
            self.host.error = Some(format!(
                "plugin VM replaced; interface failed to load\n{cause}\nFix the interface and press F10 to reload; F6 opens settings."
            ));
            tracing::warn!(
                "the plugin VM held {used} of its {limit} MiB; plugin VM replaced; interface failed to load"
            );
            self.report(
                "plugin VM replaced; interface failed to load — fix the interface and reload",
                Level::Error,
            );
        }
    }

    /// Report at a chosen severity. The message band badges the three
    /// differently, as v1 does — an error that reads like a confirmation is an
    /// error nobody notices.
    pub(crate) fn report(&mut self, message: impl Into<String>, level: Level) {
        self.status = Some((message.into(), level, Instant::now()));
        self.dirty = true;
    }
}
