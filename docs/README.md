# Design Documentation

This directory contains the **rationale** behind Thurbox's design decisions. For operational guidance (build commands, module layout, event loop), see [`CLAUDE.md`](../CLAUDE.md).

## Documents

| Document | Purpose | Update when... |
|----------|---------|----------------|
| [CONSTITUTION.md](CONSTITUTION.md) | Core principles and non-negotiable rules | Adding/removing an enforced invariant |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Architectural decisions with rationale | Changing a technology choice or structural pattern |
| [FEATURES.md](FEATURES.md) | Feature-level design choices | Altering keybindings, lifecycle, layout, or UX behavior |

## Keeping Docs Current

**Rule**: If a code change invalidates or extends a documented decision, update the relevant doc in the same PR.

- Operational changes (new commands, module moves) go in `CLAUDE.md`
- Decisional changes (why we chose X over Y) go in `docs/`
- Don't duplicate content between the two
