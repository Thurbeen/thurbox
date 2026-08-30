# Design Documentation

This directory holds the **rationale** behind Thurbox's design decisions. For
operational guidance (build commands, module layout, event loop), see
[`CLAUDE.md`](../CLAUDE.md) and the skills it indexes under
[`.claude/skills/`](../.claude/skills/).

## Documents

| Document | Purpose | Update when... |
|---|---|---|
| [CONSTITUTION.md](CONSTITUTION.md) | Core principles | Adding/removing an enforced invariant |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Architecture decisions | Changing a technology or structural pattern |
| [FEATURES.md](FEATURES.md) | Feature-level design | Altering keybindings, lifecycle, layout, or UX |
| [PERFORMANCE.md](PERFORMANCE.md) | Render/tick performance decisions | Changing the render loop, a cache, or a worker cadence |
| [V2-KERNEL.md](V2-KERNEL.md) | The plugin kernel: its shape, rules and traps | Changing the kernel's contract with Lua |
| [PLUGINS.md](PLUGINS.md) | Writing an interface plugin | Changing the plugin API or the `thurbox.*` shape |
| [ORCHESTRATION.md](ORCHESTRATION.md) | The control-plane pattern for running sessions across many repos | Changing the session/message/extension surface the pattern relies on |
| [AGENTS.md](AGENTS.md) | Each built-in agent's config and status-hook mechanism | Adding or changing a built-in agent |
| [CONFIG.md](CONFIG.md) | Thurbox's own config files / env vars / DB settings in one place | Adding/changing a config file, env var, or DB setting |
| [RELEASING.md](RELEASING.md) | What a release may and may not change about the artifacts | Changing the release workflow or a published artifact |

Two files here are not rationale: [TUTORIAL.md](TUTORIAL.md), the onboarding
walkthrough (its screenshots are generated — see
`scripts/demo/record-tutorial.sh`), and [DEVELOPMENT.md](DEVELOPMENT.md), the
dev-environment guide.

## Keeping docs current

**Rule**: If a code change invalidates or extends a documented decision, update
the relevant doc in the same PR.

- Operational changes (new commands, module moves) go in `CLAUDE.md` or the
  per-subsystem skill under `.claude/skills/` that owns the subject
- Decisional changes (why we chose X over Y) go in `docs/`
- Don't duplicate content between the two
