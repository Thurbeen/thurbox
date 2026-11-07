# Task state machine

`status.phase` records execution progress. Conditions describe facts and
problems within that phase; an effect intent or a worker's result is not
proof of an external artifact landing. The
[controller interface](interface.md) defines the probes and effects used here.

![Task lifecycle from Pending through Spawning, Spawned, Briefed and Concluded; bounded spawn and send failures lead to Stalled; deletion from any phase enters Releasing and removes the record only after release](state-machine.svg)

## Transition rules

The manager only reconciles Tasks whose `notBefore` is absent or due. It
reconciles a Concluded Task only in these cases:

- Its inbox is nonempty.
- It is being deleted.
- Its session recheck is due: `status.sessionCheckedAt` is absent or 5
  minutes old.
- Its session may be paused: see
  [pausing an idle session](#pausing-an-idle-session). The manager then
  reconciles it on every sweep, so that its activity is watched.

Deletion takes precedence over normal progress, including Stalled and
Concluded. With no deletion, Stalled is terminal. Concluded is terminal
until a delivered amendment re-opens it. A Concluded Task is due again for
delivery and for the session recheck only.
A recorded worker result for `status.brief.generation` concludes an active
Task before its ordinary phase logic runs. A result for an older generation
concludes nothing.

| Current phase | Observation or input | Next phase | Decision |
| --- | --- | --- | --- |
| Pending | `dispatch: false` | Pending | Set `Spawned=False/AwaitingDispatch`; do not spawn. |
| Pending | Dispatched; host false, Unknown or unanswered | Pending | Report host condition; do not spawn. |
| Pending | Dispatched; host reachable; Fleet makes the workspace and `status.workspace` is empty; no materialize in flight | Pending | Increment materialize attempt; set `WorkspaceReady=Unknown/Materializing`; persist the intent and plan `Materialize` under the workspace key. |
| Pending | Known `Materialized` receipt, or a missing/Unknown receipt with the workspace key found | Spawning | Record `status.workspace`; set `WorkspaceReady=True` (`Materialized` or `Adopted`); plan Spawn with the workspace as `ready`. |
| Pending | Known failure, or a missing/Unknown receipt with the workspace key proven absent; attempts below 3 | Pending | Set `WorkspaceReady=False/MaterializeRetry`; wait, then materialize again under the next key. |
| Pending | Known `Unmapped` materialize receipt | Pending | Give the attempt back; raise `NeedsAttention/RepositoryUnmapped`; wait, then materialize again. |
| Pending | Same failure with 3 materialize attempts spent | Stalled | Set `WorkspaceReady=False/MaterializeFailed`; raise `Stalled/MaterializeAttemptsExhausted` and NeedsAttention. |
| Pending | Missing/Unknown materialize receipt; workspace key unreadable | Pending | Set `WorkspaceReady=Unknown/WorkspaceUnreadable`; never materialize again blindly. |
| Pending | Dispatched; host reachable | Spawning | Increment spawn attempt; persist spawn intent and plan Spawn. |
| Spawning | Known Spawned receipt | Spawned | Record returned SessionRef. |
| Spawning | Missing/Unknown receipt; session key found | Spawned | Adopt the session and recover its receipt. |
| Spawning | Known failure, or ambiguous outcome with key proven absent; attempts below 3 | Pending | Schedule a wait before another spawn. |
| Spawning | Known `Unmapped` receipt | Pending | Give the attempt back; set `Spawned=False` and NeedsAttention, both `RepositoryUnmapped`; schedule a wait, then spawn again under the same key. |
| Spawning | Known `TrustUnmapped` receipt | Pending | As for `Unmapped`, with reason `TrustUnmapped`. |
| Spawning | Same failure with 3 spawn attempts spent | Stalled | Raise `Stalled/SpawnAttemptsExhausted` and NeedsAttention. |
| Spawning | Missing/Unknown receipt; session list unreadable | Spawning | Report Unknown; never infer absence or respawn blindly. |
| Spawned | Readable lookup lacks the recorded session identity | Spawned | Raise WorkerGone and NeedsAttention; do not send. |
| Spawned | Known Queued receipt for the brief | Spawned | Set `Briefed=False/BriefUndelivered` and raise `NeedsAttention/BriefUndelivered` with the runtime's note. Never send the brief again, and deliver no inbox entry. |
| Spawned | Known Delivered receipt, or journal proves delivered | Briefed | Record the brief key and the generation of the brief intent; recovered journal answers become receipts. |
| Spawned | Workspace trust Known untrusted | Spawned | Set `WorkspaceTrusted=False` with the gap as reason, raise `NeedsAttention/WorkspaceUntrusted`, wait, then probe again. Do not send. |
| Spawned | Workspace trust Unknown | Spawned | Set `WorkspaceTrusted=Unknown/TrustUnknown`. Do not send and raise no attention. Probe again on the next pass, without a wait. |
| Spawned | Workspace trust unanswered | Spawned | Do not send. Change nothing. |
| Spawned | No brief intent yet, or delivery proven absent; workspace trusted; fresh readiness true; fewer than 3 sends spent | Spawned | Set `WorkspaceTrusted=True/Trusted` and clear its attention. Persist brief intent and plan Brief, with the brief and every amendment so far as one message, for the current generation. A later pass consumes its receipt. |
| Spawned | Session not ready or readiness Unknown | Spawned | Do not send, including on a retry. Receipt recovery may still proceed. |
| Spawned | Delivery unreadable after an ambiguous send | Spawned | Wait, then probe again; after 3 unreadable probes raise NeedsAttention. |
| Spawned | Another send would exceed the 3-send bound | Stalled | Raise `Stalled/BriefAttemptsExhausted` and NeedsAttention. |
| Briefed | No result for `status.brief.generation`; recorded session present or lookup unreadable | Briefed | Wait for result; Unknown does not mean WorkerGone. |
| Briefed | No result for `status.brief.generation`; readable lookup lacks recorded identity | Briefed | Raise WorkerGone and NeedsAttention; never abandon or respawn. |
| Spawned, Briefed, or Concluded with a delivery due | Session lost: the runtime holds the session, but its agent's window is gone | unchanged | Set `SessionLost=True/WindowGone`. Send nothing. With `dispatch: true`, persist a revive intent and plan `Revive` under `revive/<n>`. A held Task is not revived. |
| Spawned, Briefed, or Concluded with a delivery due | Still lost after a revive | unchanged | Wait 30 seconds from the last revive, then revive again. After three revives have left the session lost, each further revive also raises `NeedsAttention/ReviveFailed`. |
| Spawned, Briefed, or Concluded with a delivery due | Not lost while `SessionLost` is True | unchanged | Set `SessionLost=False/Revived`. When the brief has a receipt, owe a catch-up note under `catchup/<n>`. In Spawned, the brief protocol continues. |
| Briefed or Concluded | A catch-up note is owed | unchanged | Send the note as an inbox entry is sent, before any inbox entry. In Briefed, the note repeats the brief, the amendments the worker has, and their generation. |
| Briefed, or Concluded and watched | A pause setting; activity Known Idle without a break for `pauseAfterMs`; the session observed running and not known lost; nothing to deliver or in flight | unchanged | Persist a pause intent and plan `Pause` under `pause/<n>`. See [pausing an idle session](#pausing-an-idle-session). |
| Briefed or Concluded | Known Paused receipt, or a missing/Unknown receipt with the session observed stopped | unchanged | Set `Paused=True/Idle`. Raise no WorkerGone, SessionLost or revive. |
| Briefed or Concluded | Failed pause receipt, or a missing/Unknown receipt with the session observed running | unchanged | Set `Paused=False/PauseFailed`. The idle time starts again. |
| Briefed, or Concluded with a delivery due or watched | Session observed stopped and Fleet did not stop it | unchanged | Set `Paused=True/External`. |
| Briefed or Concluded | Paused; an inbox entry to deliver; `dispatch: true` | unchanged | Persist a resume intent and plan `Resume` under `resume/<n>`. Send nothing. |
| Briefed or Concluded | Known Resumed receipt, a missing/Unknown receipt with the session observed running, or a paused session observed running | unchanged | Set `Paused=False/Resumed`. When the brief has a receipt, owe a catch-up note under `catchup/<n>`. |
| Briefed or Concluded | Failed resume receipt, or a missing/Unknown receipt with the session observed stopped | unchanged | Wait 30 seconds from the last resume, then resume again. After three resumes have left the session stopped, each further resume also raises `NeedsAttention/ResumeFailed`. |
| Active phase | Accepted worker result for `status.brief.generation` is stored | Concluded | Set `Concluded=True/ResultRecorded`; keep the session. |
| Spawned, Briefed or Concluded | Oldest inbox entry has no receipt; session ready; nothing else sent this pass | unchanged | Persist the entry's intent and plan `Brief` under its key; one entry per pass. In Spawned the brief goes first: nothing is delivered before the brief was sent, or while a brief send is unconfirmed. |
| Spawned, Briefed or Concluded | Oldest inbox entry; session not ready or Unknown | unchanged | Do not send. Receipt recovery through `BriefDelivered` may still proceed. |
| Spawned, Briefed or Concluded | Known Delivered receipt, or journal proves delivered | unchanged | Record the receipt in `status.deliveries` with the runtime's route, and remove the entry, in one write; its intent, receipt and attempt records go with it. |
| Spawned, Briefed or Concluded | Known Queued receipt for an entry or a catch-up note | unchanged | Mark it failed and raise `NeedsAttention/DeliveryQueued` with the runtime's note. Never send it again. The entry stays in `status.inbox`, and later entries are still delivered. |
| Spawned, Briefed or Concluded | Oldest entry is an amendment at or below `status.brief.generation`, or one never sent with a newer amendment queued behind it | unchanged | Record its receipt without a send. The worker has it already, or the newer message carries it. |
| Briefed | Amendment entry delivered | Briefed | Set `status.brief` to the entry's key and generation. |
| Concluded | Amendment entry delivered | Briefed | Set `status.brief` to the entry's key and generation, and `Concluded=False/Amended`. Keep the old result. |
| Spawned, Briefed or Concluded | Entry send refused (Known failure) | unchanged | Wait, then send again under the same key to a session observed ready; at most 3 sends. |
| Spawned, Briefed or Concluded | Entry delivery unreadable after an ambiguous send | unchanged | Wait, then read the journal again. After 3 unreadable reads, raise `NeedsAttention/DeliveryUnconfirmed` naming the entry. The entry stays, marked failed, and is never sent again. Later entries are still delivered. |
| Spawned, Briefed or Concluded | A fourth send of one entry would be needed | unchanged | `NeedsAttention/DeliveryFailed` naming the entry; the entry stays, marked failed, and later entries are still delivered. |
| Spawned, Briefed or Concluded | More than 20 entries still to deliver, not counting entries given up on | unchanged | `NeedsAttention/InboxFull`; cleared once the count is back within the bound. |
| Concluded | Empty inbox; session recheck due; lookup answered | Concluded | Record `sessionCheckedAt`; the next recheck is due 5 minutes later. |
| Concluded | Readable lookup lacks the recorded session | Concluded | `WorkerGone` and `NeedsAttention`; never a respawn. |
| Stalled | Any inbox entry | Stalled | Nothing is delivered; Stalled has no session to deliver to. |
| Any phase | Task marked deleting | Releasing | Resolve spawn ambiguity if needed; release only once at rest or proven absent. A deleting Task delivers nothing. |
| Releasing | Busy session or unreadable rest/lookup evidence | Releasing | Keep finalizer; wait for safe evidence. |
| Releasing | Failed, missing or Unknown release receipt | Releasing | Wait and retry; after 3 attempts also raise NeedsAttention. |
| Releasing | Known Released receipt, or nothing was spawned and nothing remains to release | Record removed | Remove finalizer; removal is not another phase. |

A Spawned phase without a SessionRef is an invalid internal combination; the
controller returns it to Pending instead of sending to nobody. Normal store
and controller paths record identity before entering Spawned.

## Retries and recovery

`MAX_ATTEMPTS=3` bounds spawn effects and brief sends. `RETRY_DELAY_MS=30000`
is a fixed wait, persisted as `status.notBefore` across process restarts.
These are controller constants, not extra configuration resources.

| Situation | Evidence required before proceeding | Wait and exhaustion behavior |
| --- | --- | --- |
| Spawn failed or was proven absent | A failure receipt, or readable key lookup after an ambiguous spawn | Wait before the next attempt; after the third failed attempt enter Stalled. |
| Workspace not trusted | A Trusted answer on a later pass | Wait before each probe. A held brief is not a send and spends no attempt; the Task never stalls on it. |
| Brief send refused | Known failure before confirmed delivery | Wait before retry; every retry needs fresh readiness; at most 3 sends. |
| Brief send answer missing/Unknown | Read the session's journal under the original brief key | Delivered recovers without sending; proven not delivered permits a ready-session send; Unknown waits and probes again. |
| Delivery journal unreadable | A later readable journal answer | After 3 failures raise NeedsAttention, but keep probing after each wait. Recovery clears BriefUnconfirmed attention. |
| Release answer missing/Unknown or failed | Recorded session at rest, or key proven absent; adapter preserves exact identity | Wait once per failed attempt, then retry. After 3 attempts raise NeedsAttention but continue; success releases the finalizer. |
| Session lookup unreadable | A later readable lookup | Unknown is not absence and does not by itself trigger WorkerGone or permit a blind spawn. |
| Session lost | A Known lost answer; Unknown revives nothing | Revive at once, then wait before each further revive. After three revives have left the session lost, raise NeedsAttention, and continue: a revive leaves a running session alone. |
| Pause answer missing/Unknown | Whether the session is stopped | Stopped records the pause. Running records a failed pause. Unknown waits; the stop is never repeated blindly. |
| Resume refused, or answer missing/Unknown | Whether the session is stopped | Running records the resume. Stopped resumes again after a wait. Unknown waits. After three resumes have left the session stopped, raise NeedsAttention, and continue: a resume leaves a running session alone. |
| Catch-up note send refused or answer missing/Unknown | As for an inbox entry, under the note's key | At most 3 sends, then `DeliveryFailed` naming the note. After 3 unreadable reads, `DeliveryUnconfirmed` naming it. Either way the inbox goes next. |
| Inbox entry send refused or answer missing/Unknown | As for the brief, under the entry's key: a failure receipt, or the journal's answer | Wait before retry; every retry needs fresh readiness; at most 3 sends per entry, then `DeliveryFailed` and the next entry. Unknown journal reads continue after each wait; the third raises `DeliveryUnconfirmed` and the next entry goes. |

A single brief key is reused so retrying a proven-undelivered brief does not
create a new delivery identity, and each inbox entry's key is reused the same
way. The thurbox pending journal prevents another
send when the first might already have typed text. A key reused by a different
session is not proof that the original worker is alive and does not transfer
at-rest proof to that replacement.

`fleet reconcile` exits at a fixed point; it does not wait for `notBefore`.
Invoke it again after the deadline. Stalled Tasks do not retry automatically;
unreadable-delivery and release recovery continue on later due invocations.
A different brief or a fresh run requires a new Task. An addition to the
brief is an amendment, through `fleet amend`; a Stalled or deleting Task
refuses one. Changing `dispatch` does not clear attempt counters or revive a
Stalled Task.

## Conditions

Each condition carries `status` (True, False or Unknown), `reason`, `message`,
`observedGeneration` and `lastTransitionTime`. The transition timestamp is
retained when its status has not changed against stored state. Conditions
are last recorded observations; absence of a condition is not a True value.

| Type | True means / relevant reason |
| --- | --- |
| HostReachable | The host answered reachable during the dispatch check. |
| WorkspaceReady | The workspace Fleet makes is whole: `Materialized` or `Adopted`. Unknown while `Materializing`, `WorkspaceUnreadable`, `Cleaning` or `CleanRetry`. False with `MaterializeRetry`, `MaterializeFailed`, `RepositoryUnmapped` or, after the release, `Cleaned`. Absent when the runtime makes the workspace. |
| Spawned | The spawn returned a session or the controller adopted its key. |
| Briefed | The brief has a `ReceiptWritten` or `ReceiptRecovered` record. |
| Stalled | `MaterializeAttemptsExhausted`, `SpawnAttemptsExhausted` or `BriefAttemptsExhausted`. |
| WorkerGone | A readable lookup no longer shows the recorded identity under its key (`SessionGone`). |
| Concluded | A worker result for `status.brief.generation` was accepted and observed (`ResultRecorded`). False with `Amended` after a delivered amendment re-opened the Task. |
| Released | A known release completed, or there was nothing to release. |
| WorkspaceTrusted | The session's agent trusts its workspace (`Trusted`). False reasons are `Unsupported` and `Mismatch`; Unknown is `TrustUnknown`. |
| SessionLost | The runtime holds the session, but its agent's window is gone (`WindowGone`). False after a lost session is back (`Revived`). |
| Paused | The runtime holds the session with its agent stopped. `Idle` when Fleet paused it; `External` when Fleet found it stopped. False with `Resumed` once the agent runs again, and with `PauseFailed` after a pause that did not happen. The phase does not change. |
| NeedsAttention | Reason is Stalled, WorkerGone, BriefUnconfirmed, ReleaseFailed, DeliveryFailed, DeliveryUnconfirmed, DeliveryQueued, BriefUndelivered, InboxFull, RepositoryUnmapped, TrustUnmapped, WorkspaceUntrusted, ReviveFailed, ResumeFailed or CleanFailed. |

WorkerGone clears when the exact recorded session is seen again.
RepositoryUnmapped clears when a later spawn does not answer Unmapped.
TrustUnmapped clears when a later spawn does not answer TrustUnmapped. A successful
delivery recovery clears attention whose reason is BriefUnconfirmed. A
delivery clears attention whose reason is DeliveryUnconfirmed while no
entry is marked failed. An inbox drained back within its bound clears InboxFull.
A Trusted answer clears WorkspaceUntrusted.
BriefUndelivered and DeliveryQueued stay until a person acts on the Task.
ReviveFailed clears when the session is seen not lost.
ResumeFailed clears when the session is seen running.
DeliveryFailed and DeliveryUnconfirmed stay while a failed entry does. A
successful release removes the Task; a worker result alone is not a general
reset of previous conditions. NeedsAttention does not universally stop the
controller: its reason and phase determine what continues. Delivering an
inbox entry changes no phase and no condition other than `NeedsAttention`
(`InboxFull`, `DeliveryFailed`, `DeliveryUnconfirmed`, `DeliveryQueued`). A
delivered amendment also re-opens a Concluded Task, with `Concluded=False/Amended`.

## After a host reboot

A host reboot ends every agent process. The Thurbox runtime keeps each
session's row, id, key, worktree and conversation. The Task keeps its
session, brief record and counters. Fleet resumes the work in the same
session:

```text
fleet up starts ──> sweep ──> Lost? ─yes─> Revive (same id, same worktree, conversation resumed)
                                │                │
                                no               v
                                │          next pass: not lost ──> SessionLost=False/Revived
                                v                                    │
                       normal protocol                 Briefed: catch-up note + brief, generation unchanged
                                                       Concluded: catch-up note, then the waiting inbox
```

- Spawned, Briefed and unconcluded Tasks are revived on the first sweep.
- A Concluded Task is revived only when its inbox has an entry for it.
  Change requests the worker declared are still followed. Their feedback
  arrives as inbox entries.
- A held Task (`dispatch: false`) is never revived.
- A revive never creates a second session. The brief key is never sent
  again. The catch-up note has its own key and journal entry.
- The brief record keeps its generation. A result for another generation is
  still refused. An amendment that waits is delivered after the catch-up
  note.
- A deleting Task is released, not revived. A session with no window is at
  rest.
- A Task whose session row was deleted is `WorkerGone`, as before. Fleet
  does not respawn it.

The manager must run again after the reboot. See
[running `fleet up`](../../configuration/init.md#running-fleet-up).

## Pausing an idle session

An idle session costs a process and a terminal while its Task waits. Fleet
pauses the session through the runtime and resumes it when work arrives. The
Task, its workspace and the agent's conversation stay.

A pause ends background work in the agent. A resumed agent does not start
its background shells, Monitors or loops again. For this reason, Fleet
pauses only on a Known Idle answer from the runtime.

```text
Briefed/Concluded ──Idle for pauseAfterMs──> Pause ──> Paused=True/Idle
        ^                                                   │
        │                                            inbox entry
        │                                                   v
  catch-up note, then the inbox <── Paused=False/Resumed <── Resume
```

The controller plans a pause when all of these hold:

- `pauseAfterMs` is set, on the Task or on its TaskConfig.
- `dispatch` is true, and the Task is not deleting.
- The phase is Briefed, or Concluded.
- The inbox holds no entry to deliver, and no catch-up note is owed.
- No brief, revive or resume waits for its outcome.
- The runtime can pause.
- The runtime reported Known Idle on every pass for `pauseAfterMs`, and on
  the pass that plans the pause. `status.idleSince` records the first Idle
  answer. Active, Unknown, a pass that sent something, a recorded result or
  a pass that does not watch the session clears it.
- On that pass, the session is observed running and is not known lost.

These rules apply to a paused session:

- An inbox entry plans one resume. Entries that arrive meanwhile wait in the
  inbox. Delivery then waits for readiness, as before.
- A resumed worker is owed the catch-up note that a revived worker gets,
  before its inbox.
- A session stopped outside Fleet sets `Paused=True/External`. The next
  inbox entry resumes it.
- A paused session found running again sets `Paused=False/Resumed`.
- A pause never raises WorkerGone or SessionLost, and never revives.
- Stalled, held and deleting Tasks are never resumed.
- Deleting a paused Task releases it: a stopped session is at rest.
- A Concluded Task owed a catch-up note is not paused again until its next
  inbox entry delivers the note. A revived Concluded Task behaves the same
  way.

`pauseAfterMs` absent means that Fleet never pauses the session.

The Thurbox runtime reports activity only from a thurbox that has
[Thurbeen/thurbox#1665](https://github.com/Thurbeen/thurbox/pull/1665).
With an older thurbox, activity is Unknown, so Fleet never pauses. See
[the runtime port](interface.md#runtime-port).

## Deletion protocol

The [store](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-store/src/lib.rs) adds `fleet.dev/release` at
Task creation. `fleet delete` marks the Task, then reconciliation takes over.
No session ever spawned means nothing needs releasing. If a spawn was
interrupted before its receipt, the controller first looks up its key so it
cannot orphan an unknown session.

For a recorded session, the controller requires Known at-rest or readable
absence before planning Release. Thurbox checks that the current key holder
still has the recorded id before deleting it; a replacement yields Unknown.
The finalizer remains while release is failed or ambiguous. It is removed
only after Known Released, or when there is no session to release.

When Fleet made the workspace, the release is followed by `CleanWorkspace`:

- The finalizer stays until the clean has a `Deleted` receipt.
- A failed or unknown clean is repeated after each wait. Three raise
  `NeedsAttention/CleanFailed`.
- A Task deleted before its materialize has a Known receipt looks the
  workspace key up first. An unreadable key holds the finalizer. The
session and its conversation/workspace may be deleted by the runtime, so a
worker result does not itself authorize release before Task deletion.

See [implementation and tests](README.md#implementation-and-evidence) for the
fault, lease-loss and identity regressions that exercise these paths.
