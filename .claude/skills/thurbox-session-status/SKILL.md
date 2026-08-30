---
name: thurbox-session-status
description: Hooks-driven session status in thurbox: the six SessionStatus states and their glyphs/colours, the session signal callback and its persistence columns, derivation including Unreachable remote hosts and the output-quiescence stuck-working fallback, done-vs-seen acknowledgment, and OS desktop notifications with backend detection and click-to-focus. Use when working on session status, the status dot, hook state, or notifications.
---

# Thurbox session status and notifications

*Working reference extracted from `CLAUDE.md`, which indexes it. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Session status (hooks-driven)

The session list shows, at a glance, which agents are blocked, working,
or done. `SessionStatus` (`src/session/mod.rs`) has six states — five driven by
**agent hooks**, not heuristics, plus `Unreachable` for a down remote host:

| State | Colour | Glyph | Meaning |
|-------|--------|-------|---------|
| `Working` | yellow | animated braille spinner (`⠋⠙⠹…`; static `◐`) | agent is actively running |
| `Blocked` | red | `◆` | agent needs input or approval |
| `Done` | blue | `●` (filled) | a turn just finished; shown until you switch away |
| `Idle` | green | `○` (hollow) | acknowledged (you moved off a Done), never active, or at rest |
| `Error` | red | `✗` | reserved for a crashed agent — **not derived yet** (no exit-code signal; exited → `Idle`) |
| `Unreachable` | muted grey | `⊘` | remote host down/offline; the ordinary row, derived from a live attach failure, awaiting reconnect |

**Unreachable sessions.** A persisted **remote** session whose host cannot be
reached **always appears in the list**, tagged `Unreachable`, rather than
silently vanishing. v2 gets there by derivation rather than by a synthetic row:
`kernel::terminal` records the attach failure for that session, and
`snapshot::with_reachability` folds "a remote backend plus a live attach error"
into the published status. So the row is the ordinary row — no second `Session`
kind, no dead input channel to guard, and nothing on the loop that can block on
ssh. (v1 inserted a `Session::placeholder`; the constructor is still in
`src/agent/backend.rs` and has no caller.)

The attach worker owns the retry: the same failed attempt is left alone for
`ATTACH_RETRY_INTERVAL` (20 s) and then made again, so a host that was offline at
startup recovers on its own instead of staying dead for the life of the process.
Recovery replaces nothing — the next attach simply succeeds and the derived
status goes back to what the hooks say.

The same treatment covers **mid-session host loss**: `drop_lost_panes` (per tick)
spots a *live* remote session whose pane is gone, lets it go, and clears the
readied-backend cache — the connection that session died with is the one every
other session on that host shares. The reliable signal is `has_exited()`: with
`remain-on-exit=on` a clean agent exit keeps its pane alive (no reader EOF), so a
remote reader hitting EOF means the host/SSH connection dropped. This composes
with the fail-fast SSH hardening (`crate::shell::SSH_HARDENING_OPTS` =
`BatchMode=yes` + `ConnectTimeout` + `ServerAlive*`; plus
`SSH_MULTIPLEX_OPTS` — `ControlMaster=auto` with a socket under `~/.ssh` —
whenever that directory exists, so repeated probes and git calls reuse one
connection; both sets are appended after the user's `ssh_opts`, whose first
occurrence wins), which stops a broken host
from prompting for a password on the TUI's terminal or hanging the render loop.

The live session list **animates** the `Working` spinner. The frames are
`theme.spinner` in `ui/lib/theme.lua` and the pane picks one from the elapsed
time it is handed (`status_glyph` in `10_sessions.lua`); the clock behind that is
the kernel's shared **animation tick** (`kernel::host::ANIMATION_HZ` = 8), which
the loop advances **only while something is actually animating** — a free-running
one invalidated every `pure` pane on every idle frame (ADR-P16). The filled `●`
(Done) vs hollow `○` (Idle) pair reads done-vs-seen at a glance;
`SessionStatus::icon()` is the static glyph, for contexts with no clock.

- **Reading it headlessly.** `session get`/`list --json` report the raw
  `hook_state` **plus what it takes to judge it**: `hook_state_at` /
  `hook_state_age_secs`, `hook_reported` (silence is not `idle`),
  `hook_coverage` / `hook_states_reportable` / `hook_delivery` /
  `hook_blocked_is_heuristic` (from `session::hook_status::
  AGENT_HOOK_COVERAGE`, asserted against the shipped payloads by a test), and
  `state` / `state_source` (`state` is always a word — `uncovered` when the
  agent reports nothing and `unreported` when it has not yet — and it is what
  the piped TOON `session list` shows, so a person and an agent reading the
  same call see the same word). There is deliberately **no staleness timeout**
  here — a turn may run for an hour, so a guessed bound would report live work
  as finished; the age is published and the policy is the consumer's. The
  decisive check is the pane: `session get` resolves the foreground process
  (`agent::tmux::pane_state`, one `display-message` plus one `ps`) and reports
  `hook_corroboration` and `hook_state_contradicted`, **never** overwriting
  `hook_state` with the inference. `session list` skips the probe unless
  `--verify`; a remote session is never probed and answers `unavailable`.
  `session doctor` is the same picture as a verdict plus the wiring checks
  (extension active, payload on disk carrying the signal marker, `thurbox-cli`
  resolvable on `PATH`), exiting non-zero when a session's wiring is broken —
  an agent thurbox ships no hooks for but which is *signalling anyway* warns
  rather than fails, since state is demonstrably arriving — the answer to
  "every hook ends in `|| true`, so how do I know it fired?"
- **Agents thurbox did not launch.** A harness that owns the agent launch asks
  for a bare interactive shell and starts the agent in that pane, so nothing is
  wired and nothing signals. Two answers, both additive: `THURBOX_SESSION` is in
  the pane env and inherited by every child, so anything in there can call
  `thurbox-cli session signal --state <s>` with **no arguments** (documented as
  a stable contract in `docs/CONFIG.md`); and failing that, a pane whose
  foreground is an agent the registry knows reports `state: "running"` /
  `state_source: "process"` / `hook_corroboration: "foreign-agent"` —
  deliberately coarse, since process inspection cannot say what an agent is
  *doing*.
- **The callback.** Agents report transitions with
  `thurbox-cli session signal --state <working|blocked|done|idle>`
  (`cli::sessions::Action::Signal`). Identity is the injected
  `THURBOX_SESSION` (falling back to a lookup by `agent_session_id` /
  `THURBOX_SESSION_ID`), so a hook passes no id. It writes the persisted
  state and the TUI picks it up via `PRAGMA data_version` — works headless.
  (`idle` = agent at rest, e.g. a boot-time hook; `done` = a turn just finished.)
  **Remote** sessions use a different callback with the same downstream:
  the materialized hook file sets a tmux pane user option instead, delivered
  over the control-mode subscription into the same hook columns (see the
  Remote-session-status bullet in the Remote SSH & WSL section).
- **Persistence.** `sessions.hook_state` / `hook_state_at` / `seen_at`
  (schema **v34**), with targeted-UPDATE accessors `set_hook_state` /
  `mark_session_seen` / `load_hook_states` (`storage/sessions.rs`).
  `upsert_session` deliberately **never** lists the hook columns, so the
  TUI's full-row write-back can't clobber a state a headless hook set. A
  fresh spawn seeds **nothing** — a never-reported session is `Idle`, and the
  agent's hooks drive it from there (so an idle, just-booted agent doesn't look
  stuck working).
- **Derivation.** The snapshot carries each session's `hook_state` and folds
  attach state into the published status — a *remote* session with no live pane
  → `Unreachable` (`with_reachability`); a `working` one gone quiet → `Idle`
  (the fallback below, which subsumes exited → `Idle`); else the persisted state
  (`working`/`blocked`; `idle`/none → `Idle`). A local session is never
  unreachable: this is its machine, and a missing pane there means the agent was
  not launched. The rows are read on the snapshot's own schedule rather than per
  frame, gated on `PRAGMA data_version` moving (see `docs/PERFORMANCE.md`
  ADR-P6) — but the quiescence fallback is re-derived every tick, since output
  moves between reads. `done` shows as `Done` (blue)
  **whether focused or not** — so a turn you're watching visibly completes — and
  becomes `Idle` only when you **move focus off it** (acknowledge it): the focus
  change vs. `last_active_session_id` marks the just-left `done` session `seen`
  (persists `seen_at`, one-shot). A single focused session therefore reads
  `working ↔ done`; `idle` is the at-rest/acknowledged state.
- **Stuck-`working` fallback.** Hooks can miss the turn-end edge: Claude Code
  fires **no hook on interrupt** (Esc/Ctrl+C) nor when it returns to the idle
  prompt, so an interrupted (or crashed) turn would leave `hook_state = working`
  forever. `snapshot::with_output_quiescence` guards with an **output-quiescence
  fallback** (`WORKING_QUIET_MS`, 10 s): a `working` session with no terminal
  output for that long is treated as `Idle`, and so is one with no live pane —
  which is where v1's exited → `Idle` branch lands, since a pane whose stream
  ended leaves the live set. TUI agents animate their progress line (Claude's
  `(Xs · esc to interrupt)` ticks every second) so a genuinely-live turn never
  trips it; only `working` is time-gated. The signal is **terminal output, never
  the age of the hook state**: keyed on `hook_state_at` instead, every turn
  reports itself finished ten seconds in and starts again at the next hook — a
  spinner that stops early and restarts. Applied by `SnapshotStore::
  apply_output_quiescence` on the loop's own tick rather than in `refresh`
  (whose cadence is the database's) and re-derived from `hook_state` each pass,
  so it reverses itself when output resumes. The DB row is left untouched — the
  override is purely per-tick derivation.
- **Per-session only.** Status renders on the session's own row (and in the
  ` Sessions ` panel border title, one dot per session). Repo-group headers
  (`group_header_line` in `10_sessions.lua`) carry **no** status — a rolled-up
  group dot would restate what every member row shows. Status only recolors — it
  **never** reorders rows (the order is status-independent).
- **Colours** are tunable theme fields: `status_working` / `status_blocked`
  / `status_done` / `status_idle` / `status_error`
  (`session::theme_config`, all 36 presets + custom-theme overrides), published
  to Lua as theme roles and read by the pane.
- **Wiring the hooks** is the job of the built-in **hooks extension**
  (auto-activated; see the Extensions section) — core thurbox only knows
  the generic `session signal` command.

## OS notifications

When a session goes to `SessionStatus::Blocked` (the agent needs you, reported by a
hook) thurbox fires an OS desktop notification. `kernel::notify` owns the
per-session bookkeeping and the edge detection; `src/notifications.rs` is still the
leaf side-effect layer that knows only `session` + `paths`.

The trigger is the block edge by default; `also_on_waiting` extends it to
`Working → Done`. Observed once per tick in the same place status is derived, so the
rule cannot drift from the dot in the list. Deduped per session by
`min_interval_secs`, skipped when the session is the focused one
(`suppress_for_active`), body bounded to 200 chars so a huge OSC message cannot
overflow the banner.

- **Backend** (auto-detected): `notifications::detect_backend` resolves
  `[notifications] backend` plus host probing into `Dbus` / `WindowsToast` /
  `Macos` / `None` via the pure, table-driven `resolve_backend`. `auto` picks dbus on
  a Linux desktop, the Windows toast path on native Windows and under WSL when no
  dbus answers (`/proc/version` carries the Microsoft marker and `powershell.exe`
  is on PATH), the macOS banner otherwise. Delivery errors land in a process-wide
  `LAST_ERROR` and are surfaced by the diagnostic — under WSL the dbus path used to
  fail on connect and only `warn!`, so the user saw nothing.
- **Diagnostic**: `thurbox-cli notify` prints the detected backend, whether it can
  deliver, click-to-focus support and the last error; `--test` fires a sample
  *synchronously* (`send_blocking`, since the short-lived CLI has no dispatcher).
- **Click-to-focus** (dbus + macOS `terminal-notifier`): the callback writes a
  session id to the `metadata` row keyed by `PENDING_FOCUS_SESSION_ID_KEY`, and the
  loop reads-and-deletes it atomically (`take_pending_focus_session_id`, one
  `DELETE … RETURNING`) then focuses that session. The Windows toast and macOS
  `osascript` fallback show the banner but ignore clicks. **Raising the terminal
  window is deliberately not implemented** — thurbox runs inside an emulator it does
  not own, and per-emulator window control is fragile, especially on Wayland.
- **Gated by `[features] notifications`** (default on); knobs in `[notifications]`.
  `backend = "off"` is a soft delivery switch distinct from the feature flag.

