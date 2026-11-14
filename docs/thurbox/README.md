# thurbox

This directory holds the **rationale** behind thurbox's design decisions:
the interface, its kernel and plugins, and how sessions map to Fleet
Tasks. The whole system, Fleet and the runtimes included, is in
[the overview](../README.md). For
operational guidance (build commands, module layout, event loop), see
[`AGENTS.md`](../../AGENTS.md) and the skills it indexes under
[`.agents/skills/`](../../.agents/skills/).

## Documents

| Document | Purpose | Update when... |
|---|---|---|
| [architecture.md](architecture.md) | Architecture decisions | Changing a technology or structural pattern |
| [features.md](features.md) | Feature-level design | Altering keybindings, lifecycle, layout, or UX |
| [performance.md](performance.md) | Render/tick performance decisions | Changing the render loop, a cache, or a worker cadence |
| [kernel.md](kernel.md) | The plugin kernel: its shape, rules and traps | Changing the kernel's contract with Lua |
| [plugins.md](plugins.md) | Writing an interface plugin | Changing the plugin API or the `thurbox.*` shape |
| [orchestration.md](orchestration.md) | What thurbox adds to Fleet's orchestration: Tasks as sessions, the `fleet` verbs | Changing how sessions map to Fleet Tasks |
| [agents.md](agents.md) | Which agents a session can run: Fleet's harnesses | Adding or changing an agent |
| [Where sessions run](../where-sessions-run.md) | Fleets and places: which Fleet manages a session, and where it runs | Changing how a session picks its Fleet or its place |
| [config.md](config.md) | Thurbox's own config files and env vars in one place | Adding/changing a config file or env var |
| [releasing.md](releasing.md) | What a release may and may not change about the artifacts | Changing the release workflow or a published artifact |
| [review.md](review.md) | The per-path house rules a change is reviewed against, and which document owns which class of fact | Encoding a new house rule, or moving what a document owns |

Two files here are not rationale: [tutorial.md](tutorial.md), the onboarding
walkthrough (its screenshots are generated — see
`scripts/demo/record-tutorial.sh`), and [development.md](development.md), the
dev-environment guide.

## Keeping docs current

The rule and where operational facts go are in the [documentation
overview](../README.md#keeping-docs-current).
