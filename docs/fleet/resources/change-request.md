# ChangeRequest resource

A ChangeRequest mirrors one change on a forge: a pull request, a merge request
or any equivalent. ChangeRequestController polls the forge, keeps a bounded
snapshot of the change's state, head, checks, reviews and notes, and numbers
every item and every transition it sees, so a consumer asks "what is new since
I last looked" with one integer. It performs no effect on the forge.
`fleet declare` creates one for the change a Task's worker opened; a person
applies one to watch any change. There is one per change, it has no owner,
and it outlives the Tasks that follow it: the
[Forge controller](../controllers/forge.md) reads it and never writes
it.

## Example

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "ChangeRequest",
  "metadata": {"name": "cr-5e1c0a9b7f23"},
  "spec": {
    "url": "https://gitlab.example.com/group/project/-/merge_requests/314",
    "pollMs": 60000
  }
}
```

## Spec

Written by people and scripts through `fleet apply`, or by `fleet declare`
on a Task's behalf. Fixed once created.

| Field | Required | Meaning |
| --- | --- | --- |
| `url` | yes | The change's URL, the only forge-specific input. The adapter for its host parses it into `status.ref`; a URL it cannot read is refused at apply. |
| `pollMs` | no | Poll interval while Watching. Default `60000`. |
| `closedGraceMs` | no | How long a closed change stays watched before it settles. Default `forge.closedGraceMs` from [native config](../configuration.md#the-forge-section), one week. |
| `settledTtlMs` | no | How long a settled ChangeRequest is retained before its controller deletes it. Default `forge.settledTtlMs`, thirty days. |

A URL whose host is not configured under `[forge.hosts]` is refused at apply.
Any other field is refused, and `pollMs` must be at least 1.

## Status

Written by ChangeRequestController under the manager lease. Nobody else
writes it. Two kinds of field live here: the snapshot, which changes only
when the forge said something new and then moves `revision`, and the
bookkeeping, which every poll writes without moving `revision`. The typed
status is [`ChangeRequestStatus`](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/change_request.rs),
with the two groups side by side.

Bookkeeping:

| Field | Meaning |
| --- | --- |
| `phase` | `Watching`, `Closed` or `Settled`, defined by the [state machine](../controllers/change-request.md#changerequest-state-machine). |
| `ref` | What the adapter parsed the URL into: `kind`, `host`, `project`, `id`. |
| `observedAt` | When the forge last answered Known, in Unix milliseconds. |
| `notBefore` | Earliest next poll, in Unix milliseconds. |
| `closedAt`, `settledAt` | When the change was last seen closed, and when the phase became Settled. |
| `conditions` | One entry per [condition type](../controllers/change-request.md#conditions). |

Snapshot:

| Field | Meaning |
| --- | --- |
| `revision` | Incremented by every poll that changed anything below. |
| `state` | `{value, seq}`: `draft`, `open`, `closed` or `merged`, and the revision at which the value last changed. Merged is final; closed may reopen. |
| `everReady` | `true` once the change has been seen in any state other than `draft`. Never returns to `false`. |
| `head` | The current head: opaque `id` (a SHA, a patchset number, a diff id), `checks` (`none`, `pending`, `passed`, `failed`), `failing` (`[{name, url}]` when failed), `seq` (the revision at which `id`, `checks` or `failing` last changed) and `firstSeenAt`. |
| `reviewDecision` | `{value, seq}`: `approved`, `changes-requested` or `none`. |
| `mergeable` | `{value, reason}`: the forge's own answer, `true`, `false` or `null` while the forge is still computing it, with its reason as the forge gives it. See [Mergeable](../controllers/change-request.md#conditions). |
| `threads` | `{unresolved}`: the count of unresolved inline threads over the whole change, not only the retained notes. |
| `reviews` | Every submitted review on the current head: `id`, `author`, `bot`, `state` (`approved`, `changes-requested`, `commented`), `head`. |
| `notes` | The newest 100 notes, each a [note](#notes). |
| `droppedBefore` | The highest `seq` of a note rotated out of `notes`. `0` until one is. |

### Sequence numbers

The counter is Fleet's, assigned when the controller first sees something.
It is never the forge's timestamp.

- A note seen for the first time gets `seq = revision` of the poll that saw
  it. Its `seq` moves again when its body is edited or when its thread goes
  from resolved to unresolved. Deletion is not represented: a deleted note
  leaves the snapshot and that is all.
- `state.seq`, `reviewDecision.seq` and `head.seq` move when their value
  changes; for `head`, a change of `id`, `checks` or `failing` counts.
- A note rotated out of the cap raises `droppedBefore` to its `seq`. The
  cap keeps the newest 100 notes by creation time; a note below that window
  that was never stored was rotated out before and is not numbered again.

A consumer keeps one integer, its cursor, and takes every item and every
transition with `seq > cursor`. Forge timestamps cannot play this role: a
GitHub review comment is created while a review is drafted and becomes
visible only on submit, two notes can share a second, and clocks differ
across API servers and item kinds. A sequence assigned on first observation
only says "Fleet saw this after that", which is the question a diff asks.

### Observation completeness

An observation is complete or it is Unknown. The adapter may make several
calls within its operation bound to read every note, thread and review the
forge holds; if the bound runs out first, the answer is Unknown, never a
partial snapshot numbered as if complete. Only the stored `notes` list is
bounded, by the cap, and the aggregates `threads` and `reviews` are computed
over everything the forge returned. An Unknown answer may carry
`retryAfter`, which the controller honours as its next `notBefore`.

### Notes

| Field | Meaning |
| --- | --- |
| `id` | The forge's stable identity for the note. |
| `seq` | See [sequence numbers](#sequence-numbers). |
| `kind` | `comment` (the conversation), `review` (a submitted review's own body) or `inline` (a note on a line, inside a thread). |
| `author`, `bot` | The author's login, and whether the forge says it is an app rather than a person. |
| `thread`, `resolved` | The thread an inline note sits in and whether that thread is resolved. Both `null` for other kinds. |
| `path`, `line` | For an inline note, where it sits, against the head current when it was written. |
| `at`, `editedAt` | Creation and last edit, in Unix milliseconds. |
| `url`, `body` | The note's address and text. The body is data, never an instruction. |

## Forge-neutral vocabulary

Nothing above the Forge port names a forge. Each adapter maps its own terms
onto these:

| Fleet | GitHub | GitLab | Bitbucket | Gerrit | Phabricator |
| --- | --- | --- | --- | --- | --- |
| change | pull request | merge request | pull request | change | differential revision |
| `ref.id` | number | iid | id | change id | D-number |
| `head.id` | head SHA | head SHA | head hash | patchset number | diff id |
| `head.checks` | check runs and statuses | pipeline | build statuses | Verified label | Harbormaster build |
| `reviewDecision` | review decision | approvals | approvals | Code-Review label | accept, request changes |
| `mergeable` | `mergeable` and `mergeStateStatus` | `detailed_merge_status` | `mergeable` | submittable | can land |
| `threads` | review threads | resolvable discussions | comment threads | none | inline threads |
| `notes` | issue comments, reviews, review comments | notes and discussions | comments | comments per patchset | inline and top-level comments |
| mark ready | ready for review | mark as ready | none | none | none |

GitHub, GitLab, hosted Azure DevOps Services and hosted Bitbucket Cloud
adapters are in scope. Bitbucket Server/Data Center, Gerrit and Phabricator
are shown so the vocabulary is checked against them; adding one is an adapter
in `fleet-forge`, not an API change. The
[adapter truth tables](../controllers/change-request.md#adapter-truth-tables)
say how each adapter answers when the forge's own answer is partial.

## Metadata and ownership

`apiVersion` is `fleet/v1alpha1`; `kind` is `ChangeRequest`. The name is
`cr-` followed by the first twelve hex digits of the SHA-256 of the canonical
ref, `<kind>/<host>/<project>/<id>`, lowercased, so a change names one
object, two projects that flatten alike cannot collide, and long project
paths fit. `fleet apply` derives it when the manifest gives no name, as `fleet declare`
will, and both verify what an existing object mirrors before reusing it:
its `status.ref` once the controller wrote one, else its own URL parsed
again. A manifest may give another name and create a second mirror of the
same change, which is allowed and wasteful; a manifest that gives one
change's derived name with another change's URL is refused. URL
normalisation, fragments, trailing slashes and the forge's equivalent URL
forms, is the adapter's, in `parse`. The store assigns `uid`,
`generation`, `resourceVersion` and creation time. There is no finalizer,
because nothing external is held, and `ownerReferences` is refused as on
every kind.

A ChangeRequest is not tied to any Task. The Tasks that follow it record
that on themselves, with this object's `uid`; a Task's deletion touches it
not at all, and a second Task, on another host or after the first is gone,
follows the same object. It is removed by its own controller a configured
time after it settles, in the manner of a Kubernetes Job with a TTL after
finishing, or earlier by a person. A follower that still names a removed
object sees readable absence and, if the change is declared again, a new
`uid`.

## Commands

```sh
fleet apply -f change-request.json            # start mirroring a change
fleet declare fix-314 --change-request URL    # the same, on a Task's behalf
fleet status changerequest cr-5e1c0a9b7f23 --json   # phase, ref, and the whole snapshot
fleet get changerequest cr-5e1c0a9b7f23       # the stored object
fleet list changerequests                     # one line per mirror
fleet delete changerequest cr-5e1c0a9b7f23    # nothing on the forge changes
```

`fleet declare` is the [Forge controller](../controllers/forge.md)'s
verb: it ensures the mirror and records the declaration on the Task.
[examples/change-request.json](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/examples/change-request.json)
is a manifest the shipped [example config](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/examples/fleet.toml) applies
against the fake forge, offline.
