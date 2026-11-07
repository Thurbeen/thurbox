# ChangeRequest state machine

`status.phase` records how closely the change is watched. The snapshot fields
record what the forge last said. The [interface](interface.md) defines the one
probe used here.

![ChangeRequest lifecycle: Watching polls every interval; merged settles; closed is polled slowly for a grace period and either reopens into Watching or settles; a settled object is deleted after its retention](state-machine.svg)

## Transition rules

The manager only reconciles a ChangeRequest whose `notBefore` is absent or
due. Deletion by a person removes the record from any phase. Every poll
writes the bookkeeping fields, `observedAt` and `notBefore`; only a changed
snapshot moves `revision`.

| Current phase | Observation | Next phase | Decision |
| --- | --- | --- | --- |
| Watching | Known, nothing differs from the snapshot | Watching | Bookkeeping only; `notBefore = now + pollMs`. |
| Watching | Known, something differs; state draft or open | Watching | `revision += 1`; assign `seq` to what changed; set `everReady` if the state is not draft; write; wait `pollMs`. |
| Watching | Known, state merged | Settled | Write the final snapshot; `settledAt = now`; `Settled=True/Merged`; no further polls. |
| Watching | Known, state closed | Closed | Write the snapshot; `closedAt = now`; wait `10 × pollMs`. |
| Watching or Closed | Known, the change is proven absent | Settled | `Settled=True/Gone`; no further polls. |
| Watching or Closed | Unknown | unchanged | `Reachable=Unknown` with the reason; snapshot untouched; wait the phase's interval, or the answer's `retryAfter` when it carries one. |
| Watching, no `status.ref` yet | The URL could not be parsed by the configured adapter | Watching | `Reachable=Unknown/UrlNotParsed`; wait `pollMs`. |
| Closed | Known, state draft or open | Watching | `revision += 1` through `state.seq` even if nothing else differs; clear `closedAt`; wait `pollMs`. |
| Closed | Known, state merged | Settled | As from Watching. |
| Closed | Known, still closed, within `closedGraceMs` of `closedAt` | Closed | Write only if something differs; wait `10 × pollMs`. |
| Closed | Known, still closed, past `closedGraceMs` | Settled | `settledAt = now`; `Settled=True/ClosedGraceExpired`; no further polls. |
| Settled | none asked; `settledTtlMs` not yet passed since `settledAt` | Settled | Wait until it has. |
| Settled | none asked; `settledTtlMs` passed | Record removed | Request deletion of this object. A change reopened after the grace is a new ChangeRequest. |

Proven absence is the port's `Known(None)`: the forge answered and said the
change does not exist. A refused login, a missing permission, a partial
answer or an ambiguous not-found is Unknown, never gone.

## Sequence numbers

`revision` only moves when the snapshot changes, so a quiet change costs no
snapshot writes. Each note carries the `revision` that first showed it, moved
again by an edit or an unresolve; `state`, `reviewDecision` and `head` carry
the `revision` at which their value last changed. The newest 100 notes are
kept; `droppedBefore` rises to the `seq` of any note rotated out, so a
consumer whose cursor is below it knows it has a gap rather than silently
missing a note. The [ChangeRequest resource](../../resources/change-request.md#sequence-numbers)
says why the counter is Fleet's and not the forge's clock, and what makes an
observation complete.

## Conditions

| Type | True means / relevant reason |
| --- | --- |
| Reachable | The last poll read the change completely (`Observed`). Unknown with the reason after a timeout (`Timeout`), an incomplete read (`Incomplete`), an unparseable answer (`Unreadable`), a throttled answer (`Throttled`), a missing login (`NotLoggedIn`), a refusal that is not proven absence (`Refused`), a CLI that could not start (`CliUnavailable`) or a URL the adapter could not place (`UrlNotParsed`). |
| Mergeable | The forge's own answer, as `status.mergeable`: True (`Mergeable`) when the forge says the change can be merged now, False with the forge's reason as a CamelCase word (`Conflicts`, `Blocked`, `CiMustPass`, `Draft`, ...; `NotMergeable` when the forge gives none) and the raw reason in the message, Unknown (`Computing`) while the forge is still computing it or has no such answer. Fleet reports it and never acts on it. |
| Settled | `Merged`, `ClosedGraceExpired` or `Gone`. |

Conditions keep their `lastTransitionTime` unless their status changes, so a
pass that sees nothing new writes nothing to them.

## Retention

A ChangeRequest is tied to no Task. Settled ones would otherwise accumulate
one per change ever opened, so the controller deletes its own object once
`settledTtlMs` has passed since `settledAt`, thirty days by default. This is
the pattern of a Kubernetes Job with a TTL after finishing: the object's own
controller removes it a configured time after it is done, and nothing else
has to know. The delete is fenced like every controller write: it names the
object's `uid` and `resourceVersion` and runs under the lease, so a stale
plan cannot remove a recreated mirror. A follower that still names a deleted
ChangeRequest sees readable absence and raises `ChangeGone`.

An open ChangeRequest nobody follows, such as one left by a crash between
the two steps of `fleet declare`, is polled until it settles like any other,
then retained and removed. Nothing collects open orphans; they cost one
bounded call per interval and are visible in `fleet list changerequest`.

## Deletion

`fleet delete changerequest NAME` removes the record on the next pass, at any
phase. There is no finalizer because nothing on the forge is held, and no
owner, so no other object's deletion reaches a ChangeRequest.
