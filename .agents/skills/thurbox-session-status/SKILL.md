---
name: thurbox-session-status
description: Session status in thurbox: the SessionState vocabulary, where a row's state comes from (Fleet's status.sessionState, mapped in kernel/snapshot/fleet.rs), its glyphs, colours and spinner, and OS desktop notifications with backend detection and click-to-focus. Use when working on session status, the status dot, or notifications.
---

# Thurbox session status and notifications

*Working reference indexed by `AGENTS.md`. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Where a session's state comes from

Fleet owns a session's state (ADR-41). Every session is a Fleet Task, and the
snapshot maps the Task's `status.agentState` and `status.sessionState` onto
`SessionState` (`src/session/hook_status.rs`) in `row_of`
(`src/kernel/snapshot/fleet.rs`), first match wins:

| Task status | `SessionState` |
|---|---|
| `sessionState: Paused` | `stopped` (the row's `stopped` is set too) |
| `sessionState: Lost` or `Gone` | `failed` |
| `agentState: working` | `working` |
| `agentState: attention` | `blocked` |
| `agentState: idle` | `idle` |
| `sessionState: Running` | `running` |
| `sessionState: Idle` | `idle` |
| anything else, or absent | `unreported` |

Fleet derives both from the agent's own hooks: `agentState` is the agent's
last report (`working`, `attention`, `idle`, or `unknown` before it reports).
Below the hooks, Fleet reads `agentState` off the screen with the harness's
screen rules (`crates/fleet-api/src/screen.rs`), and `status.activity.source`
says which (`hook`, `screen`, `none`). `row_of` carries it as the row's
`status_source`, published as `status_source`, `status_rule` and
`status_since_ms`; `ui.status` draws a `screen` state dimmed (blocked as `◇`)
and `none` as `?`, and the sessions pane names the source after the name.
Only an agent can be unknown: Fleet writes no `activity` for a harness with
no hooks and no screen rules, such as a shell, so its row keeps the dot of
what the runtime proved and no source text.
Claude Code's `Notification` hook, matched to `permission_prompt`, reports
`blocked`, which is `attention`; its `idle_prompt` notification is idle.
opencode's and kilo's plugins report `blocked` while a permission or a
question is open, omp's while a tool waits on its approval, and qwen's
`PermissionRequest` hook reports it; pi asks a person nothing, so it never
reports `attention`.
Each hook reports
through `fleet signal`, and Fleet's harness profiles wire those hooks at launch
(`crates/fleet-api/src/harness.rs`). thurbox installs nothing into an agent's
config and derives no state of its own.

A Task marked for deletion (`metadata.deletionTimestamp`) is not listed while
Fleet releases its session. `FleetWatch::poll` lists it again once
`DELETION_STUCK_AFTER` (three minutes) has passed since the mark, as the row's
`deleting`, published as `deleting_since_ms` and `deleting_why` (the Task's
`Released` condition); the sessions pane reads it as `deletion stuck`.

## The vocabulary

`SessionState` is the one vocabulary every surface prints. Five words are an
agent's own hook words (`working`, `blocked`, `background`, `done`, `idle`);
`SessionState::from_hook_state` parses them. The rest are inferred. Each
inferred word is spelled apart from `idle`, so "nothing reported" never reads
as "the agent says it is at rest". The Fleet mapping above produces
`working`, `blocked`, `idle`, `running`, `stopped`, `failed` and
`unreported`; a plugin still draws every word.

| State | Theme role | Glyph |
|-------|------------|-------|
| `working` | `status_working` | braille spinner (`⠋⠙⠹…`); static `◐` |
| `blocked` | `status_blocked` | `◆` |
| `done` | `status_done` | `●` |
| `background` | `status_background` | `◎` |
| `idle` | `status_idle` | `○` |
| `unreachable` | `status_unreachable` | `⊘` |
| `running` | `status_running` | `◉`; the spinner while its pane prints |
| `uncovered` | `status_unknown` | `◌` |
| `unreported` | `status_unknown` | `◌` |
| `failed` | `status_error` | `✗` |
| `stopped` | `status_idle` (fallback) | `○` (fallback) |

The glyphs and roles live in `ui/lib/theme.lua` (`STATUS_GLYPHS`,
`STATUS_ROLES`, `theme.status`). A word without a row there falls back to
`idle`'s; only `stopped` relies on that fallback. The colours are theme fields,
so each preset and a custom theme set them.

## Animation

`ui.status(status, elapsed, printing)` (`ui/lib/ui.lua`) picks the glyph the
sessions pane draws (`ui/plugins/10_sessions.lua`). Two words animate:

- `working` always animates.
- `running` animates only while its pane prints. `running` says an agent holds
  the pane; it cannot tell a turn in flight from a prompt waiting for input.
  Terminal output can, so a quiet pane keeps the static `◉`.

`printing` comes from `thurbox.printing`, a set of session ids that
`Terminals::sync_printing` (`src/kernel/terminal/mod.rs`) fills. It is
published in a group of its own, `printing_version`, which moves only when the
set gains or loses a member. A pane that prints within `WORKING_QUIET_MS`
(10 s, `src/session/hook_status.rs`) counts as printing. A surface that holds no
terminals publishes no set, and `ui.status` then returns the static glyph.

The frames are `theme.spinner`; `theme.spinner_frame` steps them at the
kernel's `ANIMATION_HZ` (8). The loop advances the animation tick only while
something animates, so an idle frame invalidates no `pure` pane (ADR-P16).

## Where status renders

- Each session row carries its own dot, and the ` Sessions ` panel border shows
  one dot per session.
- A `blocked` row is tinted in `status_blocked` and its title takes the dot's
  colour.
- Repo-group headers carry no status.
- Status only recolours rows. It never reorders them.

## OS notifications

`kernel::notify::Notifier` watches each snapshot for state edges.
`src/notifications.rs` is the delivery layer, a leaf over `session` and `paths`.

- **Edges.** A session that moves into `blocked` raises a notification. With
  `also_on_waiting`, a move into `done` does too, unless the previous state was
  `blocked`. The first sight of a session is not an edge.
- **Settings** (`[notifications]`): `min_interval_secs` is a per-session floor
  between two notifications. `suppress_for_active` skips the focused session.
  `sound` plays the OS sound. `backend` picks the delivery path.
- **Gate.** `[features] notifications` (default on) starts the dispatcher. With
  it off, nothing is attempted. `backend = "off"` is a separate delivery
  switch.
- **Backend.** `notifications::detect_backend` resolves `backend` plus host
  probing into `Dbus`, `WindowsToast`, `Macos` or `None`, through the pure,
  table-driven `resolve_backend`. `auto` picks dbus on a Linux desktop. It picks
  the Windows toast on native Windows, and under WSL when no dbus answers and
  `powershell.exe` is on `PATH`. It picks the macOS banner on macOS. Delivery
  runs on a dispatcher thread, so a slow daemon never stalls the loop. A
  delivery error is kept in `LAST_ERROR`.
- **Diagnostic.** `thurbox-cli notify` prints the detected backend, whether it
  can deliver, click-to-focus support and the last error. `--test` fires a
  sample synchronously (`send_blocking`).
- **Headless sends.** `thurbox-cli notify send` (`cli::notify::send`) checks the
  off switches first. It then takes a dedupe hold in `notify-dedupe.json` under
  a file lock, which a failed delivery gives back, and delivers with a 10 s
  bound. It names no session, so a click focuses nothing.
- **Click-to-focus.** On dbus, the action callback writes the session id to a
  process-wide slot, and the loop takes it with
  `notifications::take_focus_request` and focuses that session. The Windows
  toast and the macOS banners show the notification and do not focus a
  session. Raising the terminal window is not implemented: thurbox runs inside
  an emulator it does not own.
