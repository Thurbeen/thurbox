---
name: fleet-documentation
description: Use when creating, moving, or updating Fleet documentation or its SVG diagrams, including architecture and controller guides.
---

# Maintain Fleet documentation

Fleet's documentation is the `docs/fleet/` directory of this repository; the
[overview](../../../docs/README.md) says how it sits beside thurbox's and the
runtimes'. Start from the document that owns the subject:

- [Architecture](../../../docs/fleet/architecture.md) owns components,
  boundaries, ports and execution flow.
- [Controllers](../../../docs/fleet/README.md) indexes each controller's page
  under `docs/fleet/controllers/`: its resource, interface and state machine.
- [Resources](../../../docs/fleet/README.md) indexes the pages under
  `docs/fleet/resources/`, which own manifest fields and semantics.
- [Configuration](../../../docs/fleet/configuration.md) owns user
  configuration.
- [Contracts](../../../docs/fleet/contracts.md) owns integration guarantees
  and implementation evidence.

One page per controller, with its state-machine SVG beside it. When moving a
document, update every tracked relative link and the owning index. Check an
architecture claim against `crates/architecture.toml`, the relevant port and
manager code, and the contracts rather than inferring it from an old diagram.

Every tracked SVG must parse as XML and define explicit light and dark palettes
with `prefers-color-scheme`. Use direct CSS colors for renderer compatibility;
render the result locally and inspect it before publishing. Preserve `<title>`,
`<desc>`, `role="img"` and `aria-labelledby` accessibility metadata.

For a documentation defect, first add an end-to-end case and observe the
intended failure. The repository's gate (`.publish.yaml`: `just lint` and the
test suite, whose `tests/doc_references.rs` resolves every relative link)
covers these pages; do not weaken it to accept a new structure.
