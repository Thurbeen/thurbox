## Why

`settings.toml` is thurbox's configuration, and v2 reads five things out of it:
`features.mouse`, `features.notifications`, `features.automations` (only to arm
the heartbeat), `notifications.backend` and `clipboard.provider`. Everything else
in the file is silently ignored — so a v2 user who turned mouse capture off gets
what they asked for, while one who set `soft_delete = false`, `perf_hud = false`,
`sound = false`, `min_interval_secs = 30`, `scrollback_lines = 50000` or
`two_panel_min_cols = 100` is quietly overridden. `auto_update` and
`version_check` are worse than ignored: the header band has an update notice with
nothing behind it, so v2 never tells you a release exists and never installs one,
whatever the file says.

Configuration that is read in some places and not others is worse than
configuration that is absent: the file becomes something you cannot trust. And
v2's settings modal edits *plugin* settings only, so there is no way to see —
let alone change — the core switches from inside the interface.

## What Changes

- The kernel gains a **live copy of `Settings`**, applied at startup and
  re-applied when `settings.toml` changes on disk (v1's mtime poll), with v1's
  live-vs-restart split preserved (`Settings::restart_only_differs`).
- Every setting in scope **actually gates v2's behaviour**:
  - `soft_delete` — off means the session list hard-deletes after a confirmation
    that itemises what would be lost, instead of soft-deleting with an undo.
  - `perf_hud` — off means `F12` does nothing.
  - `shell_pane` — off means no shell tab, and the chord does nothing.
  - `mouse` / `notifications` — already honoured; kept, and now editable.
  - `version_check` — the header band's update notice is fed from the cached
    check, refreshed on a worker when stale.
  - `auto_update` — a silent update runs on startup exactly as v1's does
    (skipped for a dev build), reporting through the message band.
  - `notifications.{also_on_waiting, suppress_for_active, sound,
    min_interval_secs, backend}` — all five reach the notifier; today only two do.
  - `scrollback_lines` — the vt100 parser of every adopted pane.
  - `two_panel_min_cols` / `three_panel_min_cols` — published, and read by
    `ui/layout.lua` instead of its hardcoded `80`.
  - `audit_retention_days` — pruned at startup, as v1 prunes it.
- The **settings modal edits core settings beside plugin ones**: kernel-declared
  rows with v1's keyword/description text, a `⟳` marker on restart-only rows,
  edited as a draft and written to `settings.toml` on save — v1's semantics,
  because these knobs are a file and some of them cannot take effect until the
  next launch. Plugin settings keep their existing write-through behaviour.
- The effective settings are **published to plugins** (`thurbox.settings`), so a
  pane can honour a flag the kernel knows nothing about.
- **Out of scope, by decision**: `features.automations` (v2 does not fire
  schedules at all — parity-gap #5 owns that), and the flags whose panes v2 does
  not have yet (`tasks`, `file_viewer`, `info_panel`, `global_search`,
  `code_review`). Neither is listed in the modal, because a row that gates
  nothing reads as broken.

## Capabilities

### New Capabilities

- `core-settings`: what `settings.toml` must control in the interface, how a
  change to it takes effect (immediately, or at the next launch, and how the
  difference is made visible), and what a plugin may read of it.

### Modified Capabilities

None. The settings *modal* is specified under `v2-system-modals`, which has no
archived main spec to delta against; the core rows it grows are specified here
and fold in at archive time — the same treatment `v2-new-session-flow` gave the
create command.

## Impact

- **Kernel**: new `src/kernel/config.rs` (the live settings, the reload poll, the
  live/restart application); `src/kernel/modals/settings.rs` grows a core section
  with draft/save; `src/kernel/notify.rs` takes the full knob set;
  `src/kernel/terminal.rs` passes `scrollback_lines`; `src/kernel/host.rs`
  publishes `thurbox.settings`; `src/bin/thurbox2.rs` wires the poll, the update
  worker, the audit prune, and gates `F12`.
- **Command bus**: writing a core setting is a command (file I/O off the render
  path), distinct from the in-process `Command::Setting` a plugin uses.
- **Interface**: `ui/layout.lua` reads the panel breakpoints; the session list
  honours `soft_delete`; the agent pane honours `shell_pane`; `thurbox.yml` gains
  the published field.
- **Reuse, not reimplementation**: `session::settings::Settings` (including
  `restart_only_differs`), `agent::settings_config::{load_or_seed_with_warnings,
  save_settings}` (which already preserves the seed's comments via `toml_edit`),
  `agent::version_check::{read_cached_status, cache_is_stale, refresh_cache}` and
  `agent::self_update::perform_update` all exist and are used as they are.
- **Docs**: `docs/CONFIG.md` gains what v2 honours; `docs/V2-KERNEL.md` and
  `docs/PLUGINS.md` gain the published settings and the new module.
