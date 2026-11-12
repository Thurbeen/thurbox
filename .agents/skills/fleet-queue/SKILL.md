---
name: fleet-queue
description: Use when inspecting Fleet Tasks or changing their desired spec.
---

# Inspect and update Fleet Tasks

Use `fleet list task --json`, `fleet get task NAME` and
`fleet status task NAME --json` to read work. Task fields are documented in
the [Task resource](../../../docs/fleet/resources/task.md); inspect supported kinds with
`fleet version --json`. Update desired spec with `fleet apply -f FILE`; only
`spec.dispatch` and `spec.forge` may change. `fleet amend TASK` adds to the
brief and reaches the same session; `fleet send TASK TEXT` queues a message
for it. Send only what the requester authorized, and never type into a busy
worker's session. Deletion uses `fleet delete task NAME`; the finalizer holds the record
until its session is released.

A Task is one session. Its worker reports through `fleet result TASK` with
the brief's generation and declared output schema. A result is a claim, not
proof of an artifact landing. The controller owns status; never edit status
directly.
Unknown means the adapter could not tell and blocks the corresponding effect.

Keep `fleet up` running to reconcile all due Tasks under the single manager
lease. Use bounded `fleet reconcile` diagnostics while `up` is stopped. Do not
invent an independent polling program or a second status owner. Use the
requester's config, with its effective data.dir persisted. Keep private
manifests out of public commits. Follow
[the worker policy](../../../examples/fleet/orchestration/queue/POLICY.md) and report the
final outcome with `fleet result`.
