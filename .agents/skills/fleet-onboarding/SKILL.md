---
name: fleet-onboarding
description: Use when explicitly asked to set up a fresh Fleet deployment.
---

# Install and configure Fleet

Read [the configuration page](../../../docs/fleet/configuration.md) first.
Install Fleet only when the user asks for it. Check the release's supported
target and checksum. Download that release's `install-release.sh` into a new
directory, inspect it, then run it with `FLEET_VERSION` set to the tag the
user chose. The repository's `scripts/fleet/install-release.sh` has no
default release and refuses to run without `FLEET_VERSION`.
`FLEET_RELEASE_BASE` supports an offline release base.

For a fresh deployment, run `fleet init` to create the native per-user
config without Git or network. Read
[Initialization and `fleet up`](../../../docs/fleet/configuration.md#initialization-and-fleet-up)
for config selection and paths. Put `runtime-mux` on PATH, install tmux, and
authenticate each harness your Tasks name before `fleet up`; that foreground
manager keeps all due Tasks progressing and creates no Task of its own.
Ctrl-C retains sessions and releases its lease; after a crash, wait for lease
expiry before reconnecting. A repeated `fleet init` keeps the existing config.
To run VEGA, follow
[Running VEGA](../../../docs/fleet/configuration.md#running-vega): it is an
ordinary Task applied from its own repository.

Run `fleet version --json` before applying Tasks. Apply each Task with
`fleet apply -f FILE`, keeping `dispatch` false until it has been inspected.
Use the fake-runtime examples for an isolated trial. Real sessions use the
runtime that `[hosts.<name>]` names for the host each Task names:
`runtime-mux` by default (`fleet runtime check -- <program>` checks another).
Workers must use the same config as the Task that dispatches them; persist its data.dir instead of a differing
`up --data-dir` override. `fleet reconcile` is a bounded diagnostic command
for use while the resident manager is stopped.

Installation never starts a manager. Preserve existing deployments and
active sessions. Never configure
credentials by copying secret values into manifests or argv.
