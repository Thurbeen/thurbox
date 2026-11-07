# ChangeRequest controller

A ChangeRequest mirrors one change on a forge. Its controller decides, from
the stored snapshot and one forge observation, whether anything changed and
when to look again. It performs no effect: the forge is read, never written,
from here.

| Read | Answers |
| --- | --- |
| [ChangeRequest](../../resources/change-request.md) | What do I apply, and what does the snapshot contain? |
| [State machine](state-machine.md) | How does a change move between Watching, Closed and Settled? |
| [Interface](interface.md) | What are the probes, the Forge port and the write rules? |
| [Forge controller](../forge/README.md) | Who consumes the snapshot, and how? |
| [Native config](../../configuration/fleet-toml.md#the-forge-section) | How are forge hosts and adapters configured? |
| [Overall architecture](../../ARCHITECTURE.md) | Where does this controller fit in Fleet? |

## One run

1. A ChangeRequest is applied, by a person or by `fleet declare` for the
   change a worker opened. The adapter for its host
   parses the URL at apply, and again before the first poll, when the
   controller records it as `status.ref`.
2. On each due pass the controller asks one probe, `Change { change }`. The
   manager answers it through the Forge port with one complete observation
   or Unknown.
3. A Known answer is compared with the stored snapshot. If anything differs,
   `revision` is incremented, every new or changed item gets that `seq`, and
   the snapshot is written. If nothing differs, only the bookkeeping fields,
   `observedAt` and `notBefore`, are written.
4. An Unknown answer sets `Reachable=Unknown`, leaves the snapshot untouched
   and waits one poll interval, or the `retryAfter` the forge gave. Unknown
   never means closed, merged or empty.
5. A merged change settles. A closed change is watched more slowly for a
   grace period, then settles; reopened within it, it is watched again.
6. A settled ChangeRequest is retained for `settledTtlMs`, then the
   controller deletes it. A person may delete one at any time. There is no
   finalizer and no owner: no Task's deletion reaches it.

Polling cadence is `status.notBefore`, which the manager already honours for
Tasks. One ChangeRequest costs one bounded observation per interval, a few
forge calls when the change has many notes, and one small bookkeeping write.

## Why a resource of its own

- One poller per change, however many consumers care. The Forge controller,
  a reviewer and a conductor read the same snapshot instead of each calling
  the forge.
- The forge's vocabulary is translated once, in the adapter. No consumer sees
  a GitHub or GitLab field.
- An unreachable forge is this controller's condition. No consumer can
  mistake it for a state of the change.
- It outlives its followers. A Task that declared the change can be deleted
  and another can take over on a different host; the mirror and its
  sequence numbers stay, so the newcomer's cursor means the same thing.

## Implementation and evidence

| Responsibility | Source | Tests |
| --- | --- | --- |
| Pure decisions, numbering, cadence and retention | [ChangeRequestController](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-controllers/src/change_request.rs) | [Controller transitions](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-controllers/tests/change_request_controller.rs) |
| Resource, status and port types | [ChangeRequest types](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/change_request.rs), [Forge port](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/forge.rs), [plans](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/plan.rs) | [Forge E2E](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-cli/tests/e2e_forge.rs) |
| Sweep by kind, host-ordered deferral, fenced delete | [Manager](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-manager/src/lib.rs) | [Forge E2E](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-cli/tests/e2e_forge.rs), [Store contract](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-store/tests/store_contract.rs) |
| Apply by URL, the derived name, ref verification | [Verbs](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-manager/src/verbs.rs) | [Forge E2E](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-cli/tests/e2e_forge.rs) |
| GitHub through `gh`, GitLab through `glab`, Azure DevOps Services through `az`, Bitbucket Cloud through authenticated `curl`, the fake | [fleet-forge](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-forge/src/lib.rs): [github](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-forge/src/github.rs), [gitlab](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-forge/src/gitlab.rs), [azure-devops](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-forge/src/azure_devops.rs), [bitbucket-cloud](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-forge/src/bitbucket.rs), [fake](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-forge/src/fake.rs) | Truth tables in each adapter's unit tests; [Forge E2E](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-cli/tests/e2e_forge.rs) against the stub CLIs |
| `[forge]` configuration | [CLI config](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-cli/src/config.rs) | [Forge E2E](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-cli/tests/e2e_forge.rs) |
| Crate edges and the field manager | [architecture.toml](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/architecture.toml) | [Architecture suite](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-arch/tests/architecture.rs) |

The forge E2E suite drives the built CLI through apply by URL, idempotent
re-apply, the aliasing refusal, bookkeeping-only polls, numbering of notes,
edits, unresolves, head and state transitions, the 100-note cap, Unknown
answers and `retryAfter`, host-wide deferral, the closed grace and reopen,
merged settling, retention and the fenced delete, against the fake forge; and
pagination, truth tables, rate limits, missing logins, a hung CLI's killed
process tree and proven absence against the stub `gh`, `glab`, `az` and
authenticated HTTP client. The
[Forge controller guide](../forge/README.md#implementation-and-evidence)
lists the steps that follow this one.
