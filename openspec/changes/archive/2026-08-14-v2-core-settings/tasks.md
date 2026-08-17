## 1. The owner of the live settings (`kernel::config`)

- [x] 1.1 Add `kernel::config::Config`: the settings in force, loaded at startup through `settings_config::load_or_seed_with_warnings`, with warnings surfaced as startup notices
- [x] 1.2 Expose the live reads the loop and the modal need (features, notification knobs, scalars), leaving restart-only values to `settings::global()`
- [x] 1.3 Poll the file's mtime and re-apply on an external change, reporting only when a restart-only value moved (`Settings::restart_only_differs`)
- [x] 1.4 Ignore our own write, so saving from the modal does not report itself as an external change
- [x] 1.5 Keep the settings in force when the file will not parse, and report the problem
- [x] 1.6 Assert the live/restart classification against `restart_only_differs`, so a field cannot be misclassified quietly

## 2. Honouring what is already read, properly (`[notifications]`)

- [x] 2.1 Pass every notification knob to `kernel::notify`: the finish edge, the focused-session skip, the sound, the per-session interval, the backend
- [x] 2.2 Notify on the finish edge only when it is asked for, and never twice inside the configured interval
- [x] 2.3 Deliver nothing when the backend selects no delivery, leaving the feature switch alone

## 3. The switches that gate nothing today

- [x] 3.1 `perf_hud`: `F12` does nothing when it is off
- [x] 3.2 `shell_pane`: no shell tab and no shell command when it is off
- [x] 3.3 `soft_delete`: off means the session list hard-deletes, after a confirmation that itemises what would be lost from the session's git state
- [x] 3.4 `scrollback_lines`: the number of lines each adopted pane's parser keeps
- [x] 3.5 `audit_retention_days`: pruned at startup, as v1 prunes it

## 4. Updates (`version_check`, `auto_update`)

- [x] 4.1 Read the cached update status at startup — no network on the render path — and feed the header band's update notice
- [x] 4.2 Refresh the cache on a worker when it is stale, and only while the check is enabled
- [x] 4.3 Run the silent update on a worker at startup when it is enabled, skipped for a dev build, reporting "restart to apply" through the message band
- [x] 4.4 With either switch off, make no network call and replace nothing

## 5. Editing them (`kernel::modals::settings`)

- [x] 5.1 Declare the core rows as data under a kernel-owned owner: id, label, description, kind, and whether it needs a restart — v1's wording, ported
- [x] 5.2 Render them as their own section beside the plugin rows, with `⟳` on the restart-only ones
- [x] 5.3 Edit the core section as a draft: `Esc` discards, save applies
- [x] 5.4 Add `Command::Configure` carrying the draft, writing `settings.toml` on a worker through `save_settings` so comments and unknown keys survive
- [x] 5.5 Apply the live half of a saved draft at once, and say when a saved change waits for the next launch
- [x] 5.6 Leave plugin rows writing through as they do today, and make the difference visible on screen

## 6. What plugins can read

- [x] 6.1 Publish the settings in force as `thurbox.settings`, and declare it in `thurbox.yml`
- [x] 6.2 `ui/layout.lua` reads the two-column threshold from it, falling back to its current constant when nothing was published
- [x] 6.3 The session list honours `soft_delete`; the agent pane honours `shell_pane`

## 7. Proof

- [x] 7.1 A disabled switch removes its surface and its key does nothing; re-enabling restores it
- [x] 7.2 Deleting with soft delete off confirms first and tears down; with it on, it soft-deletes and can be undone
- [x] 7.3 Each notification knob changes delivery: the finish edge, the interval, the focused-session skip, no-delivery
- [x] 7.4 A raised column threshold leaves only the central pane at a width that used to show two
- [x] 7.5 A live change on disk applies without a restart; a restart-only change is reported instead of implied; an unparseable file keeps what was in force
- [x] 7.6 A saved core edit reaches `settings.toml` with its comments intact, and an abandoned one writes nothing
- [x] 7.7 With the update switches off, nothing is fetched and nothing is replaced
- [x] 7.8 Lint clean: clippy, fmt, selene, stylua, rustdoc, architecture rules

## 8. Documentation

- [x] 8.1 `docs/CONFIG.md`: what v2 honours, what it does not yet, and where the difference is tracked
- [x] 8.2 `docs/V2-KERNEL.md` (the module list and the settings modal's two halves) and `docs/PLUGINS.md` (`thurbox.settings`)
- [x] 8.3 Record what implementing this got wrong under "Findings from implementing" in `design.md`
