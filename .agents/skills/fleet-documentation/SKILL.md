---
name: fleet-documentation
description: Use when creating, moving, or updating Fleet documentation or its SVG diagrams, including architecture and controller guides.
---

# Maintain Fleet documentation

Start from the document that owns the subject:

- [Architecture](../../../docs/ARCHITECTURE.md) owns components, boundaries,
  ports and execution flow.
- [Controllers](../../../docs/controllers/README.md) indexes each controller's
  resource, interface and state machine.
- [Resources](../../../docs/resources/) owns manifest fields and semantics.
- [Configuration](../../../docs/configuration/) owns user configuration.
- [README](../../../README.md) owns installation, examples and cutover.
- [Migrating from v1](../../../docs/migrating-from-v1.md) owns the mapping
  from v1 concepts, configuration and commands to v2.
- [Contracts](../../../crates/CONTRACTS.md) owns integration guarantees and
  implementation evidence.

Keep controller material under `docs/controllers/<controller>/`. When moving a
document, update every tracked relative link and the owning indexes. Check an
architecture claim against `crates/architecture.toml`, the relevant port and
manager code, and the contracts rather than inferring it from an old diagram.

Every tracked SVG must parse as XML and define explicit light and dark palettes
with `prefers-color-scheme`. Use direct CSS colors for renderer compatibility;
render the result locally and inspect it before publishing. Preserve `<title>`,
`<desc>`, `role="img"` and `aria-labelledby` accessibility metadata.

For a documentation defect, first add an end-to-end gate case and observe the
intended failure. During editing run `cargo xtask check markdown docs`. Before
publication run the full `cargo xtask check`; do not weaken the gate to accept
the new structure.
