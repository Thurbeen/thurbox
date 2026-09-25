---
name: thurbox-ui-surfaces
description: Thurbox's interface surfaces and their contracts: the keybinding registry (reserved vs kernel-owned chords, terminal passthrough, macOS kitty/Cmd and Windows AltGr handling), the 36 themes and theme roles, the settings panel with its live-vs-restart-only rule and Interface recovery tab, global search, and where the code-review view went. Use when changing keybindings, themes, the settings modal or search.
---

# Thurbox keybindings, themes, settings and search

*Working reference indexed by `AGENTS.md`. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Keybindings

Every chord goes through **one registry** (`kernel::registry`). Plugins *declare*
keys in their declaration table; the kernel resolves a press to an action and hands
it back to the plugin that claimed it. There is no hardcoded table to keep in step
with a help screen, because the help modal renders the registry.

Global chords (kernel-owned):

| Key | Action |
|-----|--------|
| `Ctrl+Q` | Quit (detach sessions) |
| `Ctrl+N` | New session (the creation flow) |
| `Ctrl+H` / `Ctrl+L` | Focus previous / next pane |
| `Ctrl+J` / `Ctrl+K` | Select next / previous session |
| `Ctrl+,` / `F6` | Settings (`]` for the Interface tab) |
| `Ctrl+Y` / `F4` | Theme picker |
| `Ctrl+P` | Command palette — every action, filtered as you type |
| `F1` / `Ctrl+G` | Keybindings help |
| `F10` | Reload the interface from disk |
| `F12` | Perf HUD |
| `Ctrl+C` / `Ctrl+V` (+ `Cmd+C` / `Cmd+V` on macOS) | Copy the selection / paste |

Everything else belongs to a plugin and is listed in `F1`. Rebindings persist to
`ui.json` beside trust and the disabled set — a *user decision*, distinct from the
delivery facts in `.bundled.json`.

**Reserved is not the same as kernel-owned.** Five chords are handled before the
registry is consulted and cannot be rebound at all (`registry::RESERVED`: quit,
`F10`, `Ctrl+H`/`Ctrl+L`, `F12`) — they are the escape route out of a pane that
consumes every key. Everything else the kernel owns is an ordinary *binding* with
no Lua plugin behind it: the modal chords (`kernel::modals::bindings`) and copy
and paste (`kernel::clipboard`). Copy and paste used to be literal `KeyCode`
arms in the loop, which is why help listed them as fixed and a Mac user could
not put copy on `Cmd+C` (issue #1024). They resolve through the registry now,
ahead of a float's exclusive grab so they still work from any pane, and copy
*declines* the chord when there is no selection so `Ctrl+C` still interrupts the
agent.

Two properties the registry holds and `tests/keymap.rs` asserts:

- **A plugin-scoped claim does not outrank a global one.** This is why search does
  not take `Ctrl+P`/`Ctrl+N`: doing so would take `Ctrl+N` from new-session
  everywhere.
- **A chord freed by a removed pane stays unbound** rather than being silently
  reused by whatever loads next.

`Registry::detect_conflicts` finds every chord two overlapping scopes both claim,
and `thurbox-cli plugin check` is where that set surfaces — as warnings, naming
both claimants and which one wins. It is the only reporting surface the conflict
set has, and it exists for the plugin author, who cannot know which keys their
users have already spent.

**Terminal passthrough.** thurbox's chords share the `Ctrl+<letter>` namespace with
readline (`Ctrl+A`, `Ctrl+E`, `Ctrl+W`, `Ctrl+U`, `Ctrl+R`, `Ctrl+D`, …). While a
session terminal is focused, a chord a plugin flags as passthrough reaches the agent
instead. Navigation and app-control chords (`Ctrl+H/J/K/L`, `Ctrl+Q`, `Ctrl+N`) are
**never** deferred — they are the way out of a focused terminal.

**macOS.** The kitty keyboard protocol is pushed at startup
(`PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`, gated on
`supports_keyboard_enhancement()`, popped in `restore_terminal` and the panic hook
because `ratatui::restore()` does not). That is what makes `cmd+…` bindable at all
(iTerm2 3.5+, kitty, WezTerm, Ghostty — not Terminal.app) and what separates
`Ctrl+/` from the bytes a legacy terminal sends for it. `Cmd` then reaches the
registry as an ordinary modifier (`KeyPress::cmd`, canonical spelling `cmd+…`),
and the pty boundary refuses a `SUPER`-modified key, so an unbound Cmd chord is
swallowed rather than injected into the agent as a bare letter. The **emulator
still decides**: it applies its own `Cmd+Q/W/N/T/C/V`, `Cmd+K`, … first, and only
what it leaves free arrives. So `Cmd+C`/`Cmd+V` are shipped as macOS defaults
*and* documented as conditional — an emulator that forwards a shortcut it did not
perform (Ghostty's `performable:` keybinds) passes them on, one that swallows its
own does not, and thurbox can do nothing about a key it never receives. F-keys
need `Fn` on Mac laptops unless "Use F1, F2, etc. as standard function keys" is
on.

**Windows.** The console reports AltGr as `Ctrl`+`Alt`, so the pair is dropped at
the input boundary (`coordinator::input::resolve_altgr`) for any character no key
produces unmodified — punctuation, or a non-ASCII letter. Without it every AltGr
character (`\` on AZERTY, `@`/`[`/`]` on QWERTZ) was swallowed by every text
field and reached the agent ESC-wrapped. A `Ctrl+Alt`+letter/digit chord is
untouched, and the rule is Windows-only: elsewhere the terminal composes AltGr
itself before thurbox sees the key.

## Themes

Thirty-six palettes — twenty-eight dark, eight light; the enumeration is in
`session::theme_config` and `docs/FEATURES.md`. Users add their own in
`~/.config/thurbox/themes.toml` (a built-in `base` plus per-colour overrides); they
appear in the picker after the built-ins and persist by name exactly like a preset.

`kernel::theme::Themes` resolves them and publishes **roles** to Lua
(`ui/lib/theme.lua`), so a plugin asks for `theme.accent` or `theme.muted` rather
than a colour — which is what lets one plugin look right under all thirty-six. Pick
one with `Ctrl+Y` (or `F4`, avoiding terminals that take Ctrl+Y as DSUSP); the choice
persists in SQLite under `metadata.active_theme`, and other thurbox processes pick it
up within a tick via `PRAGMA data_version`.

The picker (`kernel::modals::theme`) filters behind `/`, mirroring the file-viewer
and review find so its keys stay consistent: `j`/`k` (+ arrows, `PageUp`/`PageDown`,
`g`/`G`, `Home`/`End`) navigate, and only after `/` do letters append to a query —
matched against display name *and* stable id with a live `matched/total` count.
Entries group under `Dark`/`Light` headers drawn *inside* their entry's row, so
selection, hitboxes and the scrollbar stay in entry space and a header disappears
with its filtered-out section. The index addresses the **match** list, so refining a
query keeps the cursor on the same theme when it survives — narrowing cannot apply a
palette other than the previewed one.

The v1→v2 consent gate paints itself from the user's active palette for the same
reason (`kernel::consent::Skin`): a gate in somebody else's colours reads like a
different program.

## Settings panel

`Ctrl+,` (or `F6`) opens a **kernel-owned modal** (`kernel::modals::settings`) —
chrome about thurbox itself, so it overlays the arrangement, captures input and
stays out of the focus ring. Plugins contribute *data* to it: declare
`{ id, desc, default }` and the modal grows a row.

Two halves on one screen:

- **Plugin settings** go through `Registry::set_setting` — in-process, effective on
  the next frame. Nothing to save, no Cancel.
- **Core settings** are `settings.toml`, written back through a `toml_edit`
  `DocumentMut` so the seed's documentation comments survive.

Whether a core row applies live or waits for a restart is **asked of
`Settings::restart_only_differs`**, the same function `Config::adopt` consults —
never a second list beside the field. A hand-written copy had already drifted from
it, promising both panel-width scalars applied live while `adopt` froze them and
reported `NeedsRestart`. Restart-only rows are marked `⟳`.

The panel is handed **`Config::on_disk`**, not `Config::in_force`. They are
different documents on purpose: a restart-only change is written to the file and
deliberately *not* taken into force, so a panel drafting from what is in force
proposes reverting every such change already saved — one visit saved
`features.mouse = false`, and the next save of anything at all put it back. Two
thirds of the core rows are restart-only, which made it read as "my settings do
not survive a restart".

`]` switches to the **Interface tab** (`kernel::modals::interface`): every file,
grouped `PANES` / `LAYOUT` / `MODULES` / `DOCS`, one state word per row (`failed`,
`deleted`, `not placed`, `off`, `on screen`, `on demand`, `hidden`; no word shared
with a source or a key), then trust, then origin (`edited` / `yours` /
`from <package>` — silent for a shipped, untouched file). Four detail lines under
the list explain the selected row: kind, slot and full source; why it is in that
state; the fix (an unplaced pane gets its `{ slot = "…" }` line, hedged because the
shipped layout places `search` only while it is open); what it asks to run and
whether that is granted. The footer lists only the keys that act on that
row: `r` restore · `d` delete · `space` turn off/on · `t` trust/revoke. `d` always
asks twice; `r` asks twice on an **edited** file (nothing keeps a pane's edits;
`layout.lua` goes to `.bak`). The cursor follows its file by path, since an action
re-sorts the list. It was a pane
once — an honest test of whether the plugin API could build a pane that lists panes
— and is chrome now because a recovery tool must not be the thing that is broken.

It is therefore **the recovery path for a broken interface**, and the shape of that
is not symmetric: a `failed` row sorts to the top with its load error under the list,
but `r` only writes back a copy thurbox *ships* — it refuses for a file the user
wrote ("thurbox ships no version of it") and points an installed pane at
`thurbox-cli plugin sync`. For a pane of the user's own the way back is `space`:
present on disk, not loaded, so nothing tried to load it and its error is silent
until it is switched on. Each of the four keys reloads the interface, so the result
is on the next frame. Documented for users in `docs/PLUGINS.md` → **When something
goes wrong**; the same three answers with no TTY are `plugin list` / `plugin dir` /
`plugin check`.

`settings.toml` is **live-reloaded** (mtime poll): an outside edit re-applies the
live half and toasts, noting a restart when `restart_only_differs` says so.

The `layout` row is the one core row whose save *does* something beyond the file:
`App::apply_settings` runs `presets::apply` on the interface directory — the same
act as `thurbox-cli layout set`, backup of an edited `layout.lua` included — and the
reload puts the new arrangement on screen. An outside edit of `layout` does not
(delivery applies it on the next start), and neither does a save while
`THURBOX_UI_DIR` points at a checkout — `presets::may_switch` is the one rule the
row and `thurbox-cli layout set` share.

> `[features] code_review`, `file_viewer`, `tasks`, `info_panel` and
> `global_search` gated surfaces the interface no longer draws. None of the five
> is read by anything (`three_panel_min_cols`, once in this list, now sizes the
> right-hand column of the `split-shell` and `ide` presets): they are parsed so an existing `settings.toml` keeps loading
> rather than failing on an unknown key, and setting one does nothing in either
> direction. The settings panel does not offer them.


## Global search

`ui/plugins/65_search.lua` — a full-width strip above the chrome bands (v1 floated
it). Two result sections: **sessions** (name, agent, branch, repo — matched in Lua
every frame) and **text** (every line each session's agent pane and shell still
hold, **scrollback included**, matched by the kernel on a worker).

- **Query grammar** is `lib.fuzzy.query`, shared with the session list: words (all
  must match, any order), `"phrase"`, `/regex/` (text only), `in:`/`repo:` filters,
  smart case. The terms half mirrors `kernel::search::Query`; keep the two parsers
  in step. Exact (substring/phrase/regex) ranks above subsequence; terminal
  subsequences must be tight (≤ 2× the word).
- **Terminal text is a *want***: on every change of the ask (no debounce) the pane
  leaves the terms in `store.want_content` (filters resolved to ids in
  `want_content.sessions`) and the kernel answers as `thurbox.search`
  (`kernel::search::SearchStore`). An open strip with nothing typed asks with `""`,
  which reads every history into the cache and matches nothing, so the first
  keystroke is warm. The loop only clones parser handles; reading and matching are
  on a worker (up to 4 threads), a superseded run gives up, each terminal's lock is
  held for at most `CHUNK_ROWS` rows at a time, and a history is cached per
  terminal by output stamp and brought up to date by reading only what was printed
  since — re-run on output at most once a second. Caps: 50 hits per session, 200
  total. The pane memoises `results` on (query, scope, `thurbox.sessions`,
  `thurbox.search`) identity, because it is not pure and renders every frame.
  ADR-P26 has the numbers; `cargo bench --bench search_cost` re-measures them
  (`THURBOX_BENCH_CHECK=1` fails over budget), `tests/search.rs` pins the
  memo, and `scripts/dev/perf-run.sh --search Q [--typing]` measures the whole
  binary with the strip open.
- **Preview and land**: moving onto a text hit scrolls its terminal back while
  focus stays in the strip; `enter`/click opens it scrolled to the line with the
  row marked (`surface.mark`). The strip asks the agent pane to do this — it writes
  `"<surface> <offset> <row>"` (`;`-separated, `-<surface>` resets) to
  `store["terminal.reveal"]` and runs `command("action", {text = "terminal.reveal"})`,
  which reaches `20_agent.lua` because a chord-less palette command now routes to
  its owner like a bound one (`coordinator/mouse.rs::run_clicked_action`).
- **In-place highlighting**: the strip publishes the ids it found as
  `store["search.matches"]`; the session list dims every other row and lights its
  own name matches through the same grammar.
- Keys: `ctrl+/` open (global), `up`/`down`, `pageup`/`pagedown`, `enter`, `tab`
  (everything → text → names), `esc` (puts back selection and scroll). One
  deliberate divergence: no `Ctrl+P`/`Ctrl+N` inside the strip — every chord goes
  through one registry where a plugin-scoped claim does not outrank a global one.

## Code review

**The view is gone from the binary.** v1's native diff reviewer
(`ui/code_review.rs` + `app/code_review.rs`) went with `src/ui`; v1 keeps it on
the `v1.x` branch. It came back **as a pane**, in its own repository:
[`thurbox-code-review`](https://github.com/Thurbeen/thurbox-code-review) —
installed by clone (`thurbox-cli plugin install
git+https://github.com/Thurbeen/thurbox-code-review`), takes the `center` switch
slot beside the agent, reclaims `Ctrl+X`/`F7`, and is the first consumer of
`thurbox.diffs` anywhere. It is not vendored here and not bundled: a change to the
snapshot's diff shape or to `command("focus", { toggle })` breaks it, so treat it as
a downstream consumer of that contract.

What survived, because it is not view code:

- **`session::review`** — the pure diff types and `parse_unified_diff`, which is
  why they live in `session` rather than beside a renderer.
- **`storage::review`** — `review_comments` + `review_marks` (schema v38), keyed on
  the write-once `sessions.base_branch`. Comments already written are still there.
- **`git::diff_against{,_on}`** (a base branch) / **`git::working_diff_on`** (the
  uncommitted changes) and **`kernel::diff`** — diffs are produced on a worker and
  published into the snapshot, bounded at `MAX_DIFF_BYTES`. The working-tree one
  folds in **untracked files** (`git diff --no-index -- /dev/null <path>`, capped at
  `git::UNTRACKED_FILE_CAP`, overflow reported as `untracked_omitted`): `git diff
  HEAD` cannot show a file git has never been told about, which made the default
  target report "no changes" after an agent wrote new ones. There is deliberately no
  body-only `git diff HEAD` helper — having one is how that omission happened.
  Rationale + the rejected temporary-index approach: ADR-P6's diff bullets in
  `docs/PERFORMANCE.md`.

So a review plugin has its data layer waiting for it. Two rules from the v1 design
still apply if you build one: **1 logical diff row = 1 selectable unit** (wrapping
expands only *visual* rows; selection and comment anchoring stay logical), and the
diff types stay in `session` (architecture rule).

