# Design Documentation

This directory contains the **rationale** behind Thurbox's design
decisions. For operational guidance (build commands, module layout,
event loop), see [`CLAUDE.md`](../CLAUDE.md).

## Documents

| Document | Purpose | Update when... |
|---|---|---|
| [CONSTITUTION.md](CONSTITUTION.md) | Core principles | Adding/removing an enforced invariant |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Architecture decisions | Changing a technology or structural pattern |
| [FEATURES.md](FEATURES.md) | Feature-level design | Altering keybindings, lifecycle, layout, or UX |

## Keeping Docs Current

**Rule**: If a code change invalidates or extends a documented decision,
update the relevant doc in the same PR.

- Operational changes (new commands, module moves) go in `CLAUDE.md`
- Decisional changes (why we chose X over Y) go in `docs/`
- Don't duplicate content between the two
