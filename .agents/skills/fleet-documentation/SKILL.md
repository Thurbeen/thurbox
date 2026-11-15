---
name: fleet-documentation
description: Use when creating, moving, or updating Fleet documentation or its SVG diagrams, including architecture and controller guides.
---

# Maintain Fleet documentation

Fleet's documentation is the `docs/fleet/` directory of this repository; the
[overview](../../../docs/README.md) says how it sits beside thurbox's and the
runtimes'. Start from the document that owns the subject:

- [The Fleet index](../../../docs/fleet/README.md) owns the model, the
  table of resource kinds, the table of controllers, the reconcile rules
  and the walkthrough. A new kind gets a row there and a page under
  `docs/fleet/resources/`; `crates/fleet-api/tests/docs_index.rs` fails
  until the row exists.
- [Architecture](../../../docs/fleet/architecture.md) owns components,
  boundaries, ports and execution flow.
- Each page under `docs/fleet/controllers/` owns one controller: its
  resource, interface and state machine.
- Each page under `docs/fleet/resources/` owns one kind's manifest fields
  and semantics.
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
