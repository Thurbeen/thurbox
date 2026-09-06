---
name: thurbox-session-status
description: Hooks-driven session status in thurbox: the SessionState vocabulary and its glyphs/colours, the session signal callback and its persistence columns, derivation including Unreachable remote hosts and the output-quiescence stuck-working fallback, done-vs-seen acknowledgment, and OS desktop notifications with backend detection and click-to-focus. Use when working on session status, the status dot, hook state, or notifications.
---

# Thurbox session status and notifications

*Working reference extracted from `CLAUDE.md`, which indexes it. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Session status (hooks-driven)

The session list shows, at a glance, which agents are blocked, working,
or done. **`SessionState` (`src/session/hook_status.rs`) is the whole
vocabulary — one enum, every surface.** Four of its words are the agent's own
hook report; the rest are thurbox's own, and each is spelled apart from `idle`
on purpose, because collapsing "nothing here is wired to report" or "the host
is gone" into "the agent says it is at rest" is exactly the conflation the
module exists to prevent:

| State | Colour | Glyph | Meaning |
|-------|--------|-------|---------|
| `working` | yellow | animated braille spinner (`⠋⠙⠹…`; static `◐`) | agent is actively running (hook) |
| `blocked` | red | `◆` | agent needs input or approval (hook) |
| `done` | blue | `●` (filled) | a turn just finished; shown until you switch away (hook) |
| `idle` | green | `○` (hollow) | acknowledged (you moved off a Done), never active, or at rest |
| `unreachable` | muted grey | `⊘` | remote host down/offline; the ordinary row, derived from a live attach failure, awaiting reconnect |
| `running` | `status_running` (accent) | `◉` | an agent holds the pane and nothing has signalled — an observation, never a claim about what it is doing |
| `uncovered` | `status_unknown` (muted) | `◌` | this agent is wired to report nothing, so its silence means nothing |
| `unreported` | `status_unknown` (muted) | `◌` | the agent *can* report and has not yet |
| `stopped` | — | — | parked by `session stop`: no process at all, which is why it outranks whatever the hook columns still hold |

**Only `stopped` carries no dot**, because a parked session is at rest by
definition and `idle` describes it without lying. The three above it each have
one, and that is a correction: the interface used to derive its status from the
hook columns alone (`derive_state`), so `hook_state = NULL` answered `Idle` and
`theme.lua`'s `or STATUS_GLYPHS.idle` drew the green hollow circle — for a
session a harness had launched an agent into, mid-turn, with nothing wired to
report it. `idle` means *the agent said it is at rest*; none of these three
does, and spelling them apart is the whole reason the vocabulary has nine words
instead of five.

The interface now derives through the same `Assessment` the CLI does
(`kernel::snapshot::assess`), which is what makes `running` reachable there —
and `running` needs the pane, so see **Reading the pane from the interface**
below. The `Error` state v1 reserved for a crashed agent is gone with
`SessionStatus`: it was never derived (process exit carries no failure signal),
and a word no surface can produce is one more thing for a driver to handle for
nothing.

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
(Done) vs hollow `○` (Idle) pair reads done-vs-seen at a glance; the glyphs
themselves live in `ui/lib/theme.lua`, whose `or STATUS_GLYPHS.idle` fallback
now catches only `stopped` — every other word in the table has a row of its
own, which is the point.

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
  Without the probe those fields are `null`, meaning **not checked** — a
  different answer from `false`, and the one reason `state` can read
  `unreported` on `list` where `get` reads `running`. That is the only
  difference between the two verbs' answers for one row, and `session list
  --help` says so.
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
  `state_source: "process"` / `hook_corroboration: "foreign-agent"` /
  `detected_agent: "<name>"` — deliberately coarse, since process inspection
  cannot say what an agent is *doing*.
  **Three names, three fields.** `agent` is what the row was created as,
  `reports_as` what a driver *declared* (`session reports-as`), and
  `detected_agent` what is observably in the pane — the registry **name**, so
  `antigravity` rather than the `agy` its argv spells.
  **Only when the observation determines one.** `ps` reports the executable,
  not the profile, so an executable that more than one registered profile
  claims yields no name at all — `hook_corroboration` still reads
  `foreign-agent` and the state still reads `running`, because the *presence*
  is observed, but `detected_agent` is null. This is a live shape, not a
  theoretical one: the shipped `agents.toml` walks the user through building it
  ("Pin a model" adds `claude-opus` on the same `command` as `claude`). Taking
  the first matching entry published whichever the file happened to list first
  — a confident name nothing had determined, and worse on screen than the bare
  `shell` label this vocabulary exists to improve on, because a specific name
  invites trust. Deliberately not resolved by a tie-break on argv either: the
  worth of the field is that it is never wrong. Detection is never
  written back as `reports_as`: a declaration is durable and an observation is
  not, and deriving one from the other would make a passing process permanent.
  The interface publishes all three onto its session rows, and shows the third
  as `zsh → claude` in the terminal pane's title and `claude · no status
  reported` beside the row.
  **Matching is in command position only** (`runs_program`): argv0, a shell's
  `-c` operand or its script, and past `exec`/`env`/`VAR=value` prefixes. It
  used to match a bare token *anywhere* in the command line, and the shape a
  harness produces — a multi-kilobyte prose brief in argv, printed back by `ps`
  as one long line — made `perl -e 'sleep 300' claude` a claude agent holding
  the pane. A missed identity is a blank; a wrong one is on screen.
- **Reading the pane from the interface.** The probe costs one
  `display-message` plus one `ps`, which the render loop may not pay, so
  `kernel::snapshot::PaneProbe` runs it on a worker thread and the verdict is
  folded in when it lands (`Assessment::with_corroboration`). It is asked
  **only** about rows whose `hook_state` is null, is cached for
  `PANE_PROBE_TTL` (2 s), and a moved verdict forces the snapshot rebuild —
  `PRAGMA data_version` cannot see a worker's answer. A session whose hooks
  work costs nothing at all. **A pane verdict is published only for a row
  nothing has reported for**: `kernel::snapshot::assess` folds one in only
  when `hook.state` is `None`, so a verdict cached before the hook onset
  cannot reach the screen even if it races a rebuild and outlives the poll
  that would otherwise have evicted it. `poll_pane_probes` retaining just the
  still-probed ids and `apply_hook_states` clearing `detected_agent` in place
  are the supporting hygiene, not the guarantee — the latter is load-bearing
  only because that path corrects its row directly and never reaches `assess`.
  See `docs/PERFORMANCE.md` → *Freshness is a property of a cached answer*.
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
  fresh spawn seeds **nothing** — a never-reported session reads `unreported`
  (or `running`, once the pane probe finds its agent), never `idle`, and the
  agent's hooks drive it from there (so an idle, just-booted agent doesn't look
  stuck working).
- **A parked session takes no state.** `set_hook_state` returns `false` for a
  row with `stopped_at` set, and `set_session_stopped(true)` clears the hook
  columns in the same transaction as the mark. `session stop` killed the pane,
  so a heartbeat's pane-option poll or a mirror pass carrying a host's last word
  would be writing a turn onto a session with no process to be in one — and
  `thurbox-cli watch` would publish a transition that did not happen. The
  callers treat `Ok(false)` as "not written", never as a failure; `session
  signal` reports it, saying to `session start` first.
- **Transitions are logged.** Each write that moves the state appends to
  `session_events` (schema **v43**) inside the same transaction, carrying
  `from_state` → `to_state`. That log is what `thurbox-cli watch` streams —
  see the `thurbox-cli` skill — and it exists because sampling the columns
  collapses two transitions that land between two samples.
- **One derivation, in `session::hook_status`.** All three read-time rules live
  in the pure module both the kernel and the CLI may import (`derive_state`,
  `with_output_quiescence`, `with_reachability`, all returning `SessionState`),
  because the interface and `thurbox-cli` were each folding the same three
  columns their own way and answering different words for one row. A caller
  applies the folds whose inputs it can actually observe:

  | fold | input | who can supply it |
  |---|---|---|
  | `derive_state` (incl. the `done → idle` acknowledgment) | `hook_state`, `hook_state_at`, `seen_at` — all stored | everyone |
  | `classify_foreground` → `Assessment::with_corroboration` | the pane's foreground process group | anyone who can run `ps` |
  | `with_output_quiescence` (incl. the latched-`blocked` fold) | terminal output age, against `hook_state_at` | the interface only |
  | `with_reachability` | a live attach error | the interface only |

  `seen_at` being a **stored fact** rather than a timeout is why the CLI applies
  it too: the CLI rightly refuses to guess a staleness bound, but this column is
  simply there to be read, and until it was, a turn the interface had already
  acknowledged reported `done` on `session get`/`list`/`watch` for the rest of
  the session's life. The two folds the CLI cannot make it does not fake.
  A local session is never unreachable: this is its machine, and a missing pane
  there means the agent was not launched. The rows are read on the snapshot's
  own schedule rather than per frame, gated on `PRAGMA data_version` moving (see
  `docs/PERFORMANCE.md` ADR-P6) — but the quiescence fallback is re-derived
  every tick, since output moves between reads. `done` shows as `Done` (blue)
  **whether focused or not** — so a turn you're watching visibly completes — and
  becomes `Idle` only when you **move focus off it** (acknowledge it): the focus
  change vs. `last_active_session_id` marks the just-left `done` session `seen`
  (persists `seen_at`, one-shot). A single focused session therefore reads
  `working ↔ done`; `idle` is the at-rest/acknowledged state.
- **Stuck-`working` fallback.** Hooks can miss the turn-end edge: Claude Code
  fires **no hook on interrupt** (Esc/Ctrl+C) nor when it returns to the idle
  prompt, so an interrupted (or crashed) turn would leave `hook_state = working`
  forever. `session::with_output_quiescence` guards with an **output-quiescence
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
- **Latched-`blocked` fallback.** `blocked` is still never time-gated: a session
  waiting on you is quiet for exactly as long as it waits, so no clock may end
  one and an hour-long block stays `blocked`. What ends one is evidence —
  `session::outlived_by_output`, in the same tick pass. A block edge stops the
  pane printing, so a real block keeps `millis_since_output` within measurement
  slop of `hook_state_age`; a block the agent resolved by itself leaves the two
  diverging, because the turn goes on printing. When the pane has printed more
  than `WORKING_QUIET_MS` past the edge the block is over, and what is left runs
  through the `working` rule above: still printing reads `working`, gone quiet
  reads `idle`. The margin is `WORKING_QUIET_MS` rather than a new constant
  because it is the same claim that one already makes. Why it is needed at all:
  claude's `blocked` is a **text match on a `Notification` body**
  (`blocked_is_heuristic`), that hook also fires for advisories an autonomous
  agent answers on its own, and — unlike kimi's `PermissionRequest` /
  `PermissionResult` pair — nothing in the payload clears it. A false block that
  landed as the newest word therefore stood for the rest of the session's life.
  Only the interface can apply this: `session get` has no terminal to ask and
  keeps reporting the latched word, which is the same line the CLI already draws
  at the two folds it cannot make.
- **Per-session only.** Status renders on the session's own row (and in the
  ` Sessions ` panel border title, one dot per session). Repo-group headers
  (`group_header_line` in `10_sessions.lua`) carry **no** status — a rolled-up
  group dot would restate what every member row shows. Status only recolors — it
  **never** reorders rows (the order is status-independent).
- **Colours** are tunable theme fields: `status_working` / `status_blocked`
  / `status_done` / `status_idle` / `status_unreachable` / `status_running` /
  `status_unknown`
  (`session::theme_config`, all 36 presets + custom-theme overrides), published
  to Lua as theme roles and read by the pane. `status_error` is a separate
  role (a failed *command*, e.g. `bands.rs`'s `Level::Error` — not a session
  state, and untouched by `SessionState` dropping `Error`).
- **Wiring the hooks** is the job of the built-in **hooks extension**
  (auto-activated; see the Extensions section) — core thurbox only knows
  the generic `session signal` command.

## OS notifications

When a session goes to `SessionState::Blocked` (the agent needs you, reported by a
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

