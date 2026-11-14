# Documentation

thurbox, Fleet and the runtimes are three components that talk only by
running each other's programs. Each has its own directory here, and this page
is the whole system: what each owns and how they meet. The decision is
[ADR-41](thurbox/architecture.md#adr-41-fleet-is-thurboxs-engine-sessions-run-on-runtimes).

| Component | Directory | Start at |
| --- | --- | --- |
| thurbox, the interface | [`thurbox/`](thurbox/README.md) | its index: architecture decisions, features, kernel, plugins, configuration |
| Fleet, the engine | [`fleet/`](fleet/README.md) | its index: architecture, contracts, controllers, resources, configuration |
| runtimes | [`runtime/`](runtime/README.md) | the runtime protocol and the terminal stream |

One [constitution](constitution.md) holds for all three: shared rules first,
then each component's own.

## Decision

1. **This repository is the one repository** for thurbox, Fleet and the
   multiplexer runtime. The issue tracker and the release channels
   (Homebrew, AUR, Chocolatey, winget) are `Thurbeen/thurbox`'s.
2. **thurbox requires Fleet.** Fleet is thurbox's orchestration core: every
   session is a Fleet Task. thurbox keeps no session state and no SQLite
   database.
3. **Fleet does not require thurbox.** Fleet reaches sessions only through a
   process protocol that any runtime can implement, down to a shell script.
4. **A runtime is the abstraction**, not a multiplexer. Fleet and thurbox
   know only "a runtime": a program that runs one keyed process somewhere.
   A multiplexer is one way to do that. `runtime-mux` (`crates/runtime-mux`)
   is the runtime thurbox ships by default; it is not special.

## Components

![Three components, two process contracts: thurbox is a Fleet client, Fleet reaches sessions through the runtime protocol, and runtime-mux or any program serves it](components.svg)

```text
thurbox (interface) ──process: fleet watch / apply / send / delete / attach──▶ Fleet (engine)
   │                                                                             │
   │ starts the stream fleet attach names                                        │ process: runtime protocol
   ▼                                                                             ▼
 stream pane ◀──── terminal stream frames, whatever the runtime ──── a runtime: runtime-mux, …
 coding agent ── fleet signal / fleet result ──▶ Fleet
```

| Component | Owns | Never owns |
| --- | --- | --- |
| Fleet (`crates/fleet-*`) | Tasks (one per session), the store, every decision, harness profiles (launch, resume, trust), agent hooks through `fleet signal`, the workspace URLs, and when to make and release them, the inbox, receipts, the runtime protocol and its conformance suite | Any runtime's code; any Rust dependency on a `runtime-*` crate or `thurbox` |
| A runtime (`crates/runtime-*`, or any program) | Processes by key on its places, the workspaces they work in (made from URLs, kept, released), its own place config (hosts), sources and credentials, bounded `exec`, the attach command, whatever keeps a terminal alive there | Tasks, agents, journals, delivery, the interface |
| thurbox (`crates/thurbox`) | The TUI, the Lua kernel, rendering, stream panes, interface state in plain files (order, seen marks, bookmarks, review notes, settings, keybindings) | Sessions, SQLite, any Rust dependency on `fleet-*`, any runtime code but `runtime-mux`, which it links until Fleet drives the runtimes |

Where a fact belongs:

- It must survive an interface restart, or a client other than this interface
  may need it: **Fleet**.
- It is how to reach a terminal or a host: **the runtime**.
- It is how this user looks at things: **thurbox**, in a file.

A host is a place of a runtime
([ADR-42](thurbox/architecture.md#adr-42-hosts-belong-to-runtimes), in progress):
its id is `<runtime>/<place>`, and its config is a table of the runtime's in
Fleet's config, passed to the runtime with each request.
[Where sessions run](where-sessions-run.md) explains Fleets and places, and the
setups they allow.

### Enforcement

One `architecture.toml` covers the whole workspace and `arch-rules` checks it
on every build:

| Crate | May depend on | Must not depend on |
| --- | --- | --- |
| `fleet-*` | `fleet-*`, `term-stream`, crates.io | `runtime-*`, `thurbox` |
| each `runtime-*` | `term-stream`, crates.io | `fleet-*`, `thurbox`, every other `runtime-*` |
| `term-stream` | crates.io | everything else |
| `workspace-git` (makes git workspaces; runtimes run it, never link it) | crates.io | everything else |
| `thurbox` | `term-stream`, crates.io, and `runtime-mux` until Fleet drives the runtimes | `fleet-*`, every other `runtime-*` |

The file is `crates/architecture.toml`, and the `architecture` test of
`arch-rules` checks it. Every crate that is not Fleet's is `role = "component"`: held
to the crate graph from its manifest and its sources, but not to Fleet's own
source rules.

Components talk only through processes; `term-stream` is a wire format, not
a component. A component can therefore later move
to a repository of its own with nothing to untangle.

### The contracts

| Contract | Between | Owner |
| --- | --- | --- |
| Views | thurbox (or any client) → Fleet: `fleet watch`, `apply`, `send`, `delete`, `attach` | [Fleet architecture](fleet/architecture.md#views) |
| The runtime protocol | Fleet → a runtime: `start`, `inspect`, `stop` and the optional verbs | [Runtimes](runtime/README.md#the-runtime-protocol) |
| The terminal stream | a view ↔ the process `fleet attach` names | [Runtimes](runtime/README.md#the-terminal-stream) |
| Workspaces | Fleet → a runtime: a list of URLs in, one directory per URL out | [Runtimes](runtime/README.md#workspaces) |

## Layout

```text
docs/
├── README.md                  the system: this page
├── constitution.md            the rules for every component, and each one's own
├── where-sessions-run.md      Fleets and places: where a session runs
├── components.svg
├── thurbox/                   the interface
│   ├── README.md              its index
│   ├── architecture.md        decisions
│   ├── features.md · performance.md · kernel.md · plugins.md
│   ├── agents.md · config.md · orchestration.md · releasing.md · review.md
│   └── development.md · tutorial.md
├── fleet/                     the engine
│   ├── README.md              its index
│   ├── architecture.md        components, ports, flow, views (+ .svg)
│   ├── contracts.md           guarantees and their tests
│   ├── configuration.md       init, fleet.toml, environment, telemetry
│   ├── controllers/           one page per controller (+ its state-machine .svg)
│   └── resources/             one page per resource kind
└── runtime/                   the runtimes
    ├── README.md              protocol, terminal stream, workspaces (+ .svg)
    └── runtime-mux.md         hosts, psmux, the status channel
```

Naming: one directory per component, each with a `README.md` index. Pages are
lower-case and hyphenated, one per subject; a subject gets a directory only
when it has several pages of one kind (`controllers/`, `resources/`). A
diagram is an SVG with light and dark palettes, named after the page or
section that embeds it and kept beside it. A new component joins as a
directory beside these three.

## Keeping docs current

**Rule**: if a change invalidates or extends a documented decision, update the
owning page in the same change. [Review](thurbox/review.md#documentation-ownership)
says which page owns which class of fact.

- Operational changes (new commands, module moves) go in `AGENTS.md` or the
  per-subsystem skill under `.agents/skills/` that owns the subject.
- Decisional changes (why we chose X over Y) go in `docs/`.
- Don't duplicate content between the two: a duplicate becomes a pointer.
