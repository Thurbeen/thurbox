---
name: fleet-onboarding
description: Use when explicitly asked to set up a fresh Fleet deployment.
---

# Install and configure Fleet

Read docs/fleet/configuration.md (initialization and cutover) first.
Fleet v2 is the native binary; Fleet v1 is the frozen Python v1.0.0 that
existing v1 users stay on. Install v2 only when the user asks for it. Check
the release's supported target and checksum. The README's one-line setup
pipes the latest release's installer into a shell; as an agent, download that
release's `install-release.sh` into a new directory, inspect it, then run it
with `FLEET_VERSION` set to the tag the user chose. The repository's
`scripts/fleet/install-release.sh` has no default release and refuses to run
without `FLEET_VERSION`. Windows is not a release target. `FLEET_RELEASE_BASE`
supports an offline release base.

For a fresh v2 deployment, run `fleet init` to create native per-user config
and the bundled editable VEGA workspace without Git or network. Read
[Initialization and the VEGA starter](../../../docs/fleet/configuration.md#initialization-and-the-vega-starter)
for config selection, the harness choice and the managed-file and conflict
rules. Install Thurbox and authenticate the configured harness before
`fleet up`; that foreground manager starts or reconnects VEGA as an ordinary
Task and keeps all due Tasks progressing. Ctrl-C retains sessions and releases
its lease; after a crash, wait for lease expiry before reconnecting.
`fleet init --update` preserves edited files and writes conflict copies for
review.

Run `fleet version --json` before applying Tasks. Apply each Task with
`fleet apply -f FILE`, keeping `dispatch` false until it has been inspected.
Use the fake-runtime examples for an isolated trial. Real sessions use the
thurbox runtime adapter, on the host each Task names. Workers must use the
same config as Mission Control; persist its data.dir instead of a differing
`up --data-dir` override. `fleet reconcile` is a bounded diagnostic command
for use while the resident manager is stopped.

Installation never starts a manager or performs a live migration. Preserve
existing deployments and active sessions. A separate, explicit no-child
cutover is required before changing an operational manager. Never configure
credentials by copying secret values into manifests or argv.
