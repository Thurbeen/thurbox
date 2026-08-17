## Why

Once every v1 surface has a plugin equivalent, the v1 TUI is 50,327 lines of
duplicated behaviour that must be maintained alongside its replacement. This
change removes it — and exists as its own change precisely so that removal is a
decision with prerequisites, not a side effect of the kernel landing.

## What Changes

- **BREAKING** — `src/app/` (33,485 lines) and `src/ui/` (16,842) are deleted,
  along with the v1 event loop in `src/main.rs`.
- `thurbox2` becomes `thurbox`; the second binary target goes away.
- `tests/architecture_rules.rs` drops its `ui` and `app` entries.
- `docs/ARCHITECTURE.md`, `docs/FEATURES.md`, `docs/PERFORMANCE.md` and the
  affected parts of `CLAUDE.md` are rewritten around the kernel.

## Prerequisites — none of this may start until all are complete

- `v2-plugin-kernel` — kernel, bare core, theme system
- `v2-session-flows` — create, fork, sync, restore
- `v2-workflow-panes` — tasks, automations
- `v2-navigation-panes` — info panel, file viewer, global search
- `v2-code-review` — the review surface
- `v2-terminal-affordances` — mouse, links, notifications, shell pane

Plus: every golden recording captured from v1 passes against its plugin
equivalent, with no assertion weakened to make it pass.

## Capabilities

### Modified Capabilities

- `bundled-plugins`: the bundled set becomes the whole interface rather than a
  bare core, since there is no longer a v1 to fall back to.

## Impact

Deletes ~50,300 lines. `thurbox-cli`, `extensions/`, and the whole lower half of
the crate are untouched. Rollback is a revert of the deletion commits, which is
why they are kept separate and last.

**This change is the reason the others exist.** If it cannot run because a
prerequisite is unmet, that is the gate working.
