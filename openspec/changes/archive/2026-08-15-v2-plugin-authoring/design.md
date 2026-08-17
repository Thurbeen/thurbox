## Context

See `proposal.md` — Why. Three existing facts shape this.

`resolve_ui_dir` lives in `src/bin/thurbox2.rs`, so nothing but the interface can
answer "which directory". `LuaHost::new` already loads every `plugins/*.lua` and
records a per-file error without throwing, which is exactly what a check wants.
`kernel::inventory::rows` already computes origin-and-visibility for `F11`.

The architecture rules are the real constraint: `cli` may reference `session`,
`storage`, `session_ops`, `sync`, `paths`, `notifications`, and `agent` by
fully-qualified path only. It may not reference `kernel` at all, and that is not
an oversight — the headless surface has had no reason to host a Lua VM.

## Goals / Non-Goals

**Goals:**

- Every question a plugin author asks before their first save, answerable from a
  pipe.
- One directory resolution, one example, one loader — so the CLI cannot report
  something the interface would contradict.
- The traps written down where they are hit, not discovered again.

**Non-Goals:**

- A plugin *manager* — install, update, registry. A plugin is a file you drop in;
  making it a package would be the opposite of this change.
- Linting. `selene` and `lua-language-server` already do that better, and the
  guide points at them. `check` answers a different question: does the kernel
  accept it.
- Changing how plugins are written. Nothing here alters the API.

## Decisions

### D1 — The directory resolution moves into the library, and the CLI reports the rule

`kernel::bundled` already owns `user_ui_dir`, `fallback_dir` and `materialize`, so
resolution joins them and both binaries call it. It returns the path *and* which
rule chose it, because "where" without "why" is what makes the wrong-directory
mistake so easy to repeat: a session that sees `/home/me/.config/thurbox/ui` and
expected the checkout's `./ui` learns nothing from the path alone.

*Alternative considered.* Leaving resolution in the binary and re-implementing it
in the CLI. Rejected outright: two implementations of "which directory is live" is
the bug the command exists to prevent.

### D2 — `check` loads the real host, and that is why `cli` gains `kernel`

The failures worth catching are declaration-shaped: a plugin with no `render`, a
slot nothing places, a key that clashes with another plugin's, a `float` that is
not a table. A syntax check would pass all of them. `LuaHost::new` already
produces exactly the per-file report wanted, so `check` builds one and prints what
it found.

That requires `cli → kernel`, declared **path-only** in
`tests/architecture_rules.rs`, the same treatment `cli` already gives `agent`: the
crossing stays visible at each call site. The alternative — a `--check` flag on
`thurbox2` — would keep the dependency inside the binary that already has it, but
it puts a headless operation on the interactive binary, away from every other
headless command, and an agent looking for it would not find it there. Recorded
because it is a close call and the allowlist edit is the cost.

### D3 — The starter and the guide's example are one file

`docs/examples/plugin.lua` is embedded with `include_str!`. `plugin new` writes
it, the guide shows it, and a test builds a host from it alone and renders it. One
artifact, so the example cannot be correct in the guide and broken in the
scaffold; and it lives under `docs/` rather than `ui/` because `ui/plugins/*.lua`
is *loaded*, and an example that ships as a pane is a pane nobody asked for.

### D4 — `new` writes a numbered file, and refuses rather than sanitises

Bundled panes are ordered by their filename prefix (`10_sessions`, `20_agent`), so
a starter gets one too — high enough not to collide with the shipped set. A name
containing a separator is refused, not cleaned: a scaffold that quietly writes
somewhere other than where it said is worse than one that stops.

### D5 — The guide leads with the path, and the traps come from what actually broke

The fast path is: where the file goes → the smallest plugin → see it → check it.
Everything currently in the guide stays, below it. The traps section carries the
five that cost time in the last two changes, each with the symptom rather than the
rule:

1. `state`/`store` reads return a **fresh table**; mutating it and not writing it
   back is silent — the value is simply the old one next frame.
2. A local helper used before its `local function` is a global `nil` at call time.
   `selene` catches it; the guide says so, since the runtime error is unhelpful.
3. `searching and match(...) or {}` turns a *miss* into an empty table, which then
   reads as a match. Lua's `and`/`or` cannot carry a nil.
4. A floating pane must name a slot the arrangement never places, or it also
   competes for the centre.
5. `on_action` must return `false` while a text field has focus, or the pane's own
   letter keys swallow typing.

## Risks / Trade-offs

**`cli` gaining `kernel` widens the headless surface.** → Path-only and declared,
so every crossing is visible; and it buys the one thing an agent cannot otherwise
do — verify before running.

**A starter that stops working.** → It is loaded and rendered by a test, so it
fails CI rather than a user's first attempt.

**`check` runs user Lua.** → It is the same code the interface would run a moment
later, under the same instruction and memory bounds; a plugin that hangs the check
would have hung the interface.

**Docs restructuring can bury what experienced readers use.** → Nothing is
removed; the fast path is added above it, and the reference keeps its order.
