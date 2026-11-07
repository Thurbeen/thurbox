# thurbox

This directory holds the **rationale** behind thurbox's design decisions:
the interface, its kernel and plugins, and today's session engine. For
operational guidance (build commands, module layout, event loop), see
[`AGENTS.md`](../../AGENTS.md) and the skills it indexes under
[`.agents/skills/`](../../.agents/skills/).

## Documents

| Document | Purpose | Update when... |
|---|---|---|
| [architecture.md](architecture.md) | Architecture decisions, ADR-1 … ADR-41 | Changing a technology or structural pattern |
| [features.md](features.md) | Feature-level design | Altering keybindings, lifecycle, layout, or UX |
| [performance.md](performance.md) | Render/tick performance decisions | Changing the render loop, a cache, or a worker cadence |
| [kernel.md](kernel.md) | The plugin kernel: its shape, rules and traps | Changing the kernel's contract with Lua |
| [plugins.md](plugins.md) | Writing an interface plugin | Changing the plugin API or the `thurbox.*` shape |
| [orchestration.md](orchestration.md) | The control-plane pattern for running sessions across many repos | Changing the session/message/extension surface the pattern relies on |
| [agents.md](agents.md) | Each built-in agent's config and status-hook mechanism | Adding or changing a built-in agent |
| [config.md](config.md) | Thurbox's own config files / env vars / DB settings in one place | Adding/changing a config file, env var, or DB setting |
| [releasing.md](releasing.md) | What a release may and may not change about the artifacts | Changing the release workflow or a published artifact |
| [review.md](review.md) | The per-path house rules a change is reviewed against, and which document owns which class of fact | Encoding a new house rule, or moving what a document owns |

Three files here are not rationale: [tutorial.md](tutorial.md), the onboarding
walkthrough (its screenshots are generated — see
`scripts/demo/record-tutorial.sh`), [development.md](development.md), the
dev-environment guide, and
[benchmark-multiplexers.md](benchmark-multiplexers.md), the results and method
of `just bench-multiplexers`.

## On Fleet

thurbox is becoming an interface over [Fleet](../fleet/README.md), with sessions on
[runtimes](../runtime/README.md); [the overview](../README.md) is the whole design.
This is what it changes in thurbox.

Less than it looks: the grid, `vt100`, rendering, size arbitration and the
performance gates already consume `PaneChunk`s, today from control mode
through `WiredPane`. They stay. What changes is the source:

- **One pane kind for every session**: a stream pane. It runs the argv from
  `fleet attach`, decodes frames into the `PaneChunk`s `WiredPane` already
  takes, and encodes keys, pastes and resizes as frames. Program panes stay
  as they are.
- **Gone from thurbox**: the control-mode client, the transports, the
  multiplexer dialects, discovery and pane location, all now
  `runtime-mux view`'s.
- **Disconnects** become one case: the stream ends, the pane shows it is
  detached, and thurbox asks `fleet attach` again with backoff. Today that is
  a different path per transport.
- **Capabilities** the interface reads from `hello`: no `scrollback` hides
  copy-mode history, no `viewers` warns before a second view, a bridged stream
  shows that its history starts now.
- **Cost**: one extra process and pipe between the multiplexer and the grid.
  Output frames are raw bytes, never base64; a snapshot is sent only on
  connect and resync. Step 5 measures it against [performance.md](performance.md)'s frame
  floors before anything is deleted.

A plain terminal uses the same stream: `fleet attach <task>` without `--json`
draws the snapshot, then passes output through and input back.

## Keeping docs current

The rule and where operational facts go are in the [documentation
overview](../README.md#keeping-docs-current).
