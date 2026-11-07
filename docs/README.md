# Design Documentation

This directory holds the **rationale** behind Thurbox's design decisions. For
operational guidance (build commands, module layout, event loop), see
[`AGENTS.md`](../AGENTS.md) and the skills it indexes under
[`.agents/skills/`](../.agents/skills/).

## Documents

| Document | Purpose | Update when... |
|---|---|---|
| [CONSTITUTION.md](CONSTITUTION.md) | Core principles | Adding/removing an enforced invariant |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Architecture decisions | Changing a technology or structural pattern |
| [FEATURES.md](FEATURES.md) | Feature-level design | Altering keybindings, lifecycle, layout, or UX |
| [PERFORMANCE.md](PERFORMANCE.md) | Render/tick performance decisions | Changing the render loop, a cache, or a worker cadence |
| [KERNEL.md](KERNEL.md) | The plugin kernel: its shape, rules and traps | Changing the kernel's contract with Lua |
| [PLUGINS.md](PLUGINS.md) | Writing an interface plugin | Changing the plugin API or the `thurbox.*` shape |
| [ORCHESTRATION.md](ORCHESTRATION.md) | The control-plane pattern for running sessions across many repos | Changing the session/message/extension surface the pattern relies on |
| [FLEET-ENGINE.md](FLEET-ENGINE.md) | Fleet as thurbox's engine: the components, the three contracts, workspaces by recipe and the migration; diagrams in [`fleet-engine/`](fleet-engine/) | Changing the split between thurbox, Fleet and a runtime, a contract, or a migration step |
| [fleet/](fleet/README.md) | Fleet's own documentation: architecture, contracts, controllers, resources, configuration | Changing Fleet; its index says what each page owns |
| [AGENTS.md](AGENTS.md) | Each built-in agent's config and status-hook mechanism | Adding or changing a built-in agent |
| [CONFIG.md](CONFIG.md) | Thurbox's own config files / env vars / DB settings in one place | Adding/changing a config file, env var, or DB setting |
| [RELEASING.md](RELEASING.md) | What a release may and may not change about the artifacts | Changing the release workflow or a published artifact |
| [REVIEW.md](REVIEW.md) | The per-path house rules a change is reviewed against, and which document owns which class of fact | Encoding a new house rule, or moving what a document owns |

Three files here are not rationale: [TUTORIAL.md](TUTORIAL.md), the onboarding
walkthrough (its screenshots are generated — see
`scripts/demo/record-tutorial.sh`), [DEVELOPMENT.md](DEVELOPMENT.md), the
dev-environment guide, and
[BENCHMARK-MULTIPLEXERS.md](BENCHMARK-MULTIPLEXERS.md), the results and method
of `just bench-multiplexers`.

## Layout

```text
docs/
├── README.md                     this index
│
│   thurbox                       one upper-case file per subject
├── CONSTITUTION.md               core principles
├── ARCHITECTURE.md               architectural decisions, ADR-1 … ADR-41
├── FEATURES.md                   feature-level design
├── PERFORMANCE.md                render-loop and tick decisions, ADR-P*
├── KERNEL.md                     the plugin kernel
├── PLUGINS.md                    writing an interface plugin
├── AGENTS.md                     the built-in coding agents
├── CONFIG.md                     thurbox's own configuration
├── ORCHESTRATION.md              the control-plane pattern
├── RELEASING.md                  what a release may change
├── REVIEW.md                     house rules and documentation ownership
├── DEVELOPMENT.md                the dev environment
├── TUTORIAL.md                   the onboarding walkthrough
├── BENCHMARK-MULTIPLEXERS.md     thurbox against raw tmux and Herdr
├── benchmark-multiplexers/       its raw samples
│
│   the engine                    the design, and the engine's own docs
├── FLEET-ENGINE.md               Fleet as thurbox's engine
├── fleet-engine/                 its diagrams
│   ├── components.svg            three components, two process contracts
│   ├── terminal-stream.svg       contract 3: attaching to any runtime
│   ├── workspaces.svg            Fleet decides, a recipe prepares, the runtime places
│   └── migration.svg             the nine steps
└── fleet/                        Fleet's documentation, Fleet's layout
    ├── README.md                 its index and layout
    ├── ARCHITECTURE.md · CONTRACTS.md · migrating-from-v1.md
    ├── configuration/            fleet-toml · init · environment-json · telemetry
    ├── controllers/<name>/       README · interface · state-machine (+ .svg) · adapters/
    ├── resources/                one page per kind
    └── media/
```

Naming: a subject is one upper-case file at the level that owns it
(`FEATURES.md`, `fleet/ARCHITECTURE.md`); a subject's assets sit in a
lower-case directory of the same name beside it (`fleet-engine/`,
`benchmark-multiplexers/`); pages inside a directory are lower-case and
hyphenated; a directory with more than one page has a `README.md` index; a
diagram is an SVG with light and dark palettes beside the page that embeds
it. The next component to join this repository gets a directory like
`fleet/`, with its own index.

## Keeping docs current

**Rule**: If a code change invalidates or extends a documented decision, update
the relevant doc in the same PR.

- Operational changes (new commands, module moves) go in `AGENTS.md` or the
  per-subsystem skill under `.agents/skills/` that owns the subject
- Decisional changes (why we chose X over Y) go in `docs/`
- Don't duplicate content between the two
