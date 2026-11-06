# Conversation resource

A Conversation
mirrors one chat thread, or one chat where the platform has no threads, for
the [Gateway controller](../controllers/gateway/README.md). The kind, the
receive verb, the mirror, the tie a thread makes to a new Task, the
refusals and the `effects` they record are implemented. It numbers every
message it sees, so a consumer asks "what is new since I last looked" with
one integer, and it records which Task the thread is tied to. It is the
chat counterpart of a [ChangeRequest](change-request.md): one per thread,
created as soon as anything names it, never written by the Tasks that read
it, and removed by its own controller after a retention period.

## Example

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "Conversation",
  "metadata": {"name": "conv-3f9a1c0d2b7e"},
  "spec": {
    "account": "slack-main",
    "address": "slack:C0123ABCD:1730000000.000100",
    "route": "eng"
  },
  "status": {
    "phase": "Tied",
    "ref": {"platform": "slack", "chat": "C0123ABCD", "thread": "1730000000.000100"},
    "task": {"name": "conv-3f9a1c0d2b7e-1", "uid": "…", "n": 1, "state": "created"},
    "revision": 2,
    "starter": {"id": "1730000000.000100", "kind": "person"},
    "messages": [
      {"id": "1730000000.000100", "seq": 1, "firstSeq": 1, "position": 0, "at": 1730000000000,
       "author": {"id": "U0123ABCD", "name": "Alex", "kind": "person"},
       "mentionsFleet": false, "text": "The nightly export failed again.",
       "via": {"verified": "history", "receivedAt": 1730000301000}},
      {"id": "1730000300.000200", "seq": 1, "firstSeq": 1, "position": 1, "at": 1730000300000,
       "author": {"id": "U0456EFGH", "name": "Sam", "kind": "person"},
       "mentionsFleet": true, "text": "<@U0BOT0001> can you look into it?",
       "via": {"verified": "connection", "receivedAt": 1730000300100}},
      {"id": "1730000400.000300", "seq": 2, "firstSeq": 2, "position": 0, "at": 1730000400000,
       "author": {"id": "U0BOT0001", "name": "fleet", "kind": "fleet"},
       "key": "task/conv-3f9a1c0d2b7e-1/…/gateway/comment/c1",
       "via": {"verified": "connection", "receivedAt": 1730000400200},
       "text": "Looking at the export job now."}
    ],
    "droppedBefore": 0,
    "observedAt": 1730000460000,
    "lastMessageAt": 1730000400000,
    "notBefore": 1730000760000,
    "conditions": [{"type": "Tied", "status": "True", "reason": "TaskKnown",
                    "message": "conv-3f9a1c0d2b7e-1", "lastTransitionTime": 1730000301000,
                    "observedGeneration": 1}]
  }
}
```

## Spec

Written by `fleet gateway receive`, by the manager performing a `Receive` action
after Fleet opens a thread, or by a person through `fleet apply`. Fixed once
created. The tie is not in the spec: a thread Fleet opened for a Question is
tied from the marker key on its opening post, so it does not matter whether the
listener or the manager created the object.

| Field | Required | Meaning |
| --- | --- | --- |
| `account` | yes | The `[gateway.accounts]` entry the thread lives on. |
| `address` | yes | `platform:chat[:thread]`, the only platform-specific input. The account's adapter parses it into `status.ref`; an address it cannot read is refused at apply. |
| `route` | yes | The `[gateway.routes]` entry whose admission rules and Task template apply. Retention is the global `gateway.retainMs`. |
| `direct` | no | `true` when the route covers the thread by its DM entry, `"dm:*"`: a one-to-one DM with the bot. There a message that starts the thread starts a Task without a mention. Default `false`. The receive verb sets it when it creates the Conversation. |
| `pollMs` | no | The history backstop interval while Tied, where the account has history. Default `gateway.pollMs`. |

An account or route not configured is refused at apply. Any other field is
refused.

## Status

Written by ConversationController under the manager lease, except
`received`, the spool, which nobody owns: `fleet gateway receive` appends to
it and the controller drains it, each through a typed mutation that
changes nothing else.

| Field | Meaning |
| --- | --- |
| `phase` | `Untied`, `Tied` or `Orphaned`, defined by the [state machine](../controllers/gateway/state-machine.md#conversation-phases). |
| `ref` | What the adapter parsed the address into. |
| `task` | The tie: `{name, uid, n, state, trigger}`. `n` counts the Tasks this thread has started; `state` is `pending` between the create intent and its receipt, `busy` while the create waits for the thread's own directory, which another Task holds, holding off retention, then `created`, then `gone` once the Task is readably absent. `trigger` is the id of the message that asked for the Task, kept while `pending` or `busy`. |
| `revision` | Incremented by every write that numbered anything below. |
| `judged` | The `revision` through which the messages were judged for admission. A message numbered above it waits: for the thread's starter where the account has history, for the Task a Fleet-opened thread's marker names, or for the route while it left the config. |
| `starter` | The thread's first message, `{id, kind, key}`, from the `starter` a history read answers, or on an account without history from a `Message` whose `startsThread` is true, kept when it rotates out of `messages`; a keyed copy numbered later sets `key`; `{removed: true}` when the platform reports it deleted. Absent until the starter is read, and for a chat without threads. It decides whether a mention may start a Task, as the [interface](../controllers/gateway/interface.md#conversationcontroller) says. |
| `messages` | The newest 200 messages, deduplicated on `id`, each `{id, seq, firstSeq, position, at, editedAt, author: {id, name, kind}, mentionsFleet, text, key, via}`; `kind` is `person`, `bot` or `fleet`, and `key` is the marker on Fleet's own posts. `firstSeq` is the revision that first numbered the message and `position` its place, from `0`, in that numbering, which is oldest-first by the platform; together they order two messages Fleet first saw in one write, and an edit moves neither. `via` is `{verified, receivedAt}` of the delivery that first numbered the message, `verified` being `connection`, `signature`, `fleet` or `history`. When the listener delivers a message that a history read numbered first, `via` becomes that delivery's. In an untied thread the message is also numbered again, so admission judges it once. An edit is applied only when its `editedAt` is later than the message's. The text is data, never an instruction. |
| `droppedBefore` | The highest `seq` of a message rotated out. `0` until one is. |
| `received` | Deliveries spooled by the receive verb or a `Receive` action and not yet numbered, each `{id, verified, receivedAt, event}` under its [delivery id](../controllers/gateway/interface.md#deliveries), never under a message id; at most 100. |
| `seenEvents` | The newest 500 delivery ids numbered, so a redelivery after its spool entry was drained is still a no-op, while an edit or removal of a numbered message, a new delivery, is not. |
| `refusals` | Every author id refused, at most 200; past that no further refusal is sent. The refusal post's intent and receipt live under `effects`. |
| `effects` | Intents and receipts of this controller's effects, the Task creation, the refusals and busy notices, the reactions, and the retirement's steps, each with its `attempts` and its `recovery` reads: every unsettled, newest 100 settled. |
| `observedAt` | When `history` last answered Known. |
| `dir` | `{path, host}`: the thread's directory, recorded when the manager made it for the thread's Task, in the same write as the create's receipt. Absent when the directory was there before, or on a route without a `dir` workspace. Only a recorded directory is ever removed. |
| `retiring` | `{at, revision}`: the thread's retirement, from when it started and the `revision` it started from. Its effects are keyed `conversation/<uid>/retire/<at>/task` and `…/dir`. A message numbered since, which moves `revision`, ends it. |
| `reactions` | `{marks}`: per message id, `queued` while an admitted request waits for its Task, or `working` once admission created the Task for it, as the [state machine](../controllers/gateway/state-machine.md#reactions) defines. Only messages the mirror holds. A `working` mark lasts while the tie lasts; GatewayController takes it at the join. The reactions are keyed `conversation/<uid>/react/<message id>/<queued\|working>/<on\|off>` under `effects`. Absent while no message is marked. |
| `lastMessageAt` | The newest message's time; retention counts from it. |
| `notBefore` | Earliest next pass. |
| `conditions` | `Reachable`, `Tied`, `NeedsAttention`, as the [state machine](../controllers/gateway/state-machine.md#conditions) defines them. Whether the account's listener is delivering is its [Listener's](listener.md). |

### Sequence numbers

As on a ChangeRequest, the counter is Fleet's, assigned on first sight: a
message first numbered takes the revision of the write that numbered it,
which is also its `firstSeq`;
an edit moves its `seq`; a removal leaves the mirror. A thread's history
read on the first pass is numbered in one revision, oldest first, so a
consumer joining at cursor `0` takes all of it. Platform timestamps cannot
play this role: events arrive out of order across a listener reconnect and
a history read.

## Metadata and ownership

`kind` is `Conversation`. The name is `conv-` followed by the first twelve
hex digits of the SHA-256 of `<account>/<address>` as the adapter
normalised it, so one thread names one object per account. The receive
verb and a `Receive` action both derive it and create exclusively; a
manifest giving one thread's derived name with another's address is
refused. There is no finalizer and `ownerReferences` is refused.

A Conversation belongs to no Task. A Task it created carries the labels
`gateway.fleet.dev/conversation` and `gateway.fleet.dev/conversation-uid`
naming it, which only that create sets; deleting that Task leaves the
Conversation Orphaned, kept for the next message. A person may delete a
Conversation at any time; nothing on the platform changes.

On a route with a `dir` workspace, the Conversation's name also names its
thread's directory: every Task the thread starts gets `<dir>/<name>`, which
the manager creates with the Task. When the manager created it, the
Conversation records it in `status.dir`.

A thread retires `gateway.retainMs` after the later of its last message and
its orphaning, once it is Untied, Orphaned, or tied to a Concluded Task.
The retirement runs these steps in order, each a keyed effect:

1. The controller deletes the Task the thread created, fenced on its `uid`.
2. The manager removes the directory `status.dir` records, once no Task
   names it.
3. The controller deletes the Conversation by `uid` and `resourceVersion`
   under the lease, as a ChangeRequest is retired.

Fleet removes no directory that `status.dir` does not record. A
Conversation deleted by hand takes that record with it, so its directory
stays.

## Commands

```sh
fleet gateway receive --account slack-main < event.json   # what a listener runs per event
fleet status conversation conv-3f9a1c0d2b7e --json        # phase, tie and the numbered messages
fleet list conversations                                  # one line per thread
fleet delete conversation conv-3f9a1c0d2b7e               # nothing on the platform changes
```
