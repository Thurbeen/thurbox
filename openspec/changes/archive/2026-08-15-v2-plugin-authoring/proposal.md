## Why

Adding a plugin is meant to be the easy thing about v2 — one Lua file, no build
step, no restart. For someone working through a terminal it nearly is. For an
**agentic session**, which is how a lot of plugins will now be written, three
things are missing and each of them costs a wrong guess:

- **Which directory is live?** It resolves three ways (`THURBOX_UI_DIR`, a `./ui`
  beside the working directory, the user's own copy) and the only thing that
  reports the answer is `F11` — a key you cannot press without a TTY. The rule is
  documented, but a session that guesses wrong edits a file nothing reads, and
  `docs/PLUGINS.md` already lists that as the most common failure.
- **Is what I wrote valid?** There is no way to find out except launching the
  interface and looking. A plugin that fails to load is *reported* well — in the
  TUI.
- **What does a correct one look like?** The guide is 356 lines with no fast path
  and no runnable example, and it documents `state` as "persistent, private"
  without the rule that reads hand back a **fresh table** — which is exactly the
  bug that shipped in the creation flow (the renderer computed a completion the
  key handler could not see). Four more traps of the same kind cost real time in
  the last two changes and are written down nowhere.

## What Changes

- **`thurbox-cli plugin`**, the headless half of what `F11` shows:
  - `plugin dir` — the directory in force, and *which rule* selected it, so a
    session knows where to write before it writes.
  - `plugin new <name>` — a starter plugin, valid on first load, refusing to
    overwrite an existing file.
  - `plugin check` — load the interface the way the kernel does and report what
    failed, per file, with an exit status a script can gate on.
  - `plugin list` — what is loaded, where each file came from, and whether it is
    on screen: `F11`'s inventory without the TTY.
- **One directory resolution.** `resolve_ui_dir` moves out of `src/bin/thurbox2.rs`
  into the library so the CLI and the interface cannot disagree about which
  directory is live.
- **A runnable example**, embedded once and used twice: it is what `plugin new`
  writes and what the guide shows, so the two cannot drift, and a test loads it so
  it cannot rot.
- **A fast path in `docs/PLUGINS.md`**: where the file goes, the smallest thing
  that works, how to see it, how to check it — before any of the reference
  material. Plus a **traps** section carrying the five that actually cost time,
  starting with the `state` write-back rule.

## Capabilities

### New Capabilities

- `plugin-authoring`: what someone — or an agent — needs to write a plugin
  without launching the interface: finding the directory in force, creating a
  valid starting point, and verifying what they wrote.

### Modified Capabilities

None. `bundled-plugins` and `plugin-host` are still deltas in `v2-plugin-kernel`
and `v2-plugin-lifecycle` with no archived main spec, so the directory-resolution
requirement is specified here and folds in at archive time.

## Impact

- **New**: `src/cli/plugins.rs` and its subcommand on `thurbox-cli`;
  `docs/examples/plugin.lua` (embedded via `include_str!`).
- **Moved**: `resolve_ui_dir` from `src/bin/thurbox2.rs` into
  `kernel::bundled`, which already owns `user_ui_dir` / `fallback_dir` /
  `materialize`.
- **Architecture rules**: `cli` gains `kernel` as a *path-only* reference, the
  same treatment it already gives `agent`. This is a deliberate, declared edit —
  `plugin check` has to load the real host, because the failures worth reporting
  are declaration-shaped (no `render`, an unknown slot, a key that clashes), not
  syntax.
- **Docs**: `docs/PLUGINS.md` restructured around a fast path; `CLAUDE.md` and
  `docs/V2-KERNEL.md` point at it in one line each.
- **Reuse**: `LuaHost` reports per-plugin load errors already; `kernel::inventory`
  already answers "which files, from where, on screen or not" for `F11`.
