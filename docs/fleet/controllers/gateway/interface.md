# Gateway controller interface

This page defines the Gateway's APIs: the port that adapters implement, the
CLI verbs that listeners and people run, what a Task declares and receives,
and who may write what. The port, the addresses and admission are
implemented in `fleet_api::gateway`; the three verbs, the Conversation and
Listener kinds, ConversationController, ListenerController,
GatewayController's tie, join, relay, posting, logging and reactions, its
`Open`, `Receive`, `Post`, `Log` and `React` actions, `answers`, the parent
labels and `Probe::List` are implemented. It follows the shape of the
[Forge controller interface](../forge/interface.md). The
[state machine](state-machine.md) defines when each thing happens.

## Addresses

One address format names a conversation everywhere above the port:

```text
<platform>:<chat>[:<thread>]
slack:C0123ABCD:1730000000.000100      a Slack thread: channel, parent ts
slack:C0123ABCD                        a channel, where a thread is opened
telegram:-1001234567890:42             a forum topic: chat id, message_thread_id
discord:1300000000000000000:1300000000000000001   a Discord thread: parent channel, thread
```

Only the adapter reads the parts of an address. A Conversation is named
`conv-<12 hex of sha256(<account>/<address>)>`, so one thread names one
object per account. The account, not the address, carries the platform
workspace, such as a Slack team.

## Gateway port

The port is `fleet_api::gateway::Gateway`. Every answer is Known or
Unknown. A refused write is `Known(EffectOutcome::Failed { why })`.

| Method | Input | Answer |
| --- | --- | --- |
| `operation_bound` | None | The worst-case duration of one call. |
| `parse` | An account and an address | The normalised `ConversationRef`. Unknown for an address the adapter cannot read. |
| `event` | An account's configuration and a [delivery envelope](#deliveries) | A `Delivery`, `None` for a payload the Gateway ignores (a reaction, a presence change), or Unknown. Pure, so the receive verb runs it without the lease. |
| `whoami` | An account | Fleet's bot identity, as the platform reports it. |
| `capabilities` | An account | What the platform offers there: `history`, `marker` (`metadata`, `nonce` or `none`), `unmentioned` (whether messages without a mention are delivered), `threads` and `reactions` (whether Fleet can add and remove its own reactions). |
| `history` | A conversation, and an `after` message id or a `since` time | The messages after that point, oldest first, and the thread's first message as `starter`. Unknown, with `retryAfter` where the platform gives one, when the read fails. `Unsupported` where bots cannot read history. |
| `open` | A channel, text and an intent key | `Opened { conversation, message, at }`: a new thread whose first post is the text, carrying the key as its marker. |
| `post` | A conversation, text and an intent key | `Posted { message, at }`: a post carrying the key as its marker. To a thread address, a reply in the thread. To a channel address, a top-level post in the channel. |
| `react` | A conversation, a message id, a reaction name and `on` | `Reacted { already }`: Fleet's own reaction is added (`on`) or removed. `already` is true when it was already there, or already absent. |
| `link` | An account and a thread address | A URL that opens the thread, or none where the adapter cannot make one without a platform call. Pure: it calls no platform. |

What an adapter must guarantee:

- **`at` is the platform's own time** for the post, such as Slack's `ts` or
  Telegram's `date`. Fleet orders answers on it.
- **A Known `since` read is complete.** Recovery reads `since` the intent
  and treats a missing key as an absent post. A read that cannot page
  through everything answers Unknown (`Incomplete`), never a partial list.
- **An `after` read returns at most `limit` messages**, the newest ones,
  with a count of those left out. The first read of a thread takes the
  newest 200.
- **`starter` is `removed`** when the platform reports the thread's first
  message deleted.
- **An open that takes two platform calls** keys both from the one intent
  key, and finishes a half-done open on retry.
- **With `marker = nonce { windowMs }`,** a retry within the window resends
  the same nonce, and the platform returns the existing post.
- **A reaction is set, not counted.** A repeated add leaves one reaction,
  and removing an absent one changes nothing. Both are Known successes.
- **A reaction name is the platform's emoji name**, as
  `gateway.reactions` gives it. The adapter maps the name to what its
  platform takes. An adapter whose platform has no reactions answers
  `reactions = false`, and Fleet then calls no `react`.
- **`gateway.reactions` names three reactions.** `queued` defaults to
  `hourglass_flowing_sand` (⏳), `working` to `eyes` (👀) and `replied` to
  `white_check_mark` (✅). `reactions = false` turns all three off. The
  config is refused when a name is empty or holds whitespace, when two
  names give the same emoji, and when it has another key.
- **A `post` to a channel address opens no thread.** Every part of a split
  text is a top-level post in the channel. A `since` read of the channel
  address finds them.

Text is Fleet's Markdown subset: paragraphs, emphasis, code, links, lists
and quotes. The adapter converts it to the platform's format, and splits a
long text into consecutive posts under derived keys that name the part and
the count (`<key>/1/3`, `<key>/2/3`, `<key>/3/3`).

There is no edit, delete or merge method. Fleet never changes a post that
it made. `react` changes only Fleet's own reaction on a message. The
[adapters](adapters/README.md#what-the-platforms-changed) say why each
capability exists.

### Events

| Event | Fields |
| --- | --- |
| `Message` | `id` (unique per conversation), `conversation`, `author` (`id`, `name`, `kind`: `person`, `bot` or `fleet`), `mentionsFleet`, `at`, `text`, `key` (the marker on a Fleet post), `startsThread` (true when the payload says the message starts its thread, as a Discord thread's starter or a Telegram topic's first message) |
| `Edited` | `id`, `conversation`, `text`, `editedAt` |
| `Removed` | `id`, `conversation` |

A mention is a `Message` with `mentionsFleet`, not an event of its own. A
newer edit replaces the text. An older edit delivered late changes nothing.

### Deliveries

`event` answers a `Delivery`:

| Field | Meaning |
| --- | --- |
| `id` | The delivery id. It is the same on every redelivery of one event, and different for an edit or removal of the same message. It is the platform's event id where one exists, such as Slack's `event_id`. Otherwise the adapter derives it. |
| `verified` | How the delivery was authenticated: `connection` (an authenticated socket), `signature` (a verified webhook), `fleet` (Fleet's own opening post) or `history` (read by the backstop). |
| `receivedAt` | When the listener received it. |
| `event` | The `Message`, `Edited` or `Removed`. |
| `direct` | `true` when the payload says the chat is a one-to-one DM between Fleet's bot and one other user. Absent means `false`: a channel, a group DM, or a payload that does not say. |

A listener writes one envelope per delivery to the verb's stdin:

```json
{"verified": "connection", "receivedAt": 1730000300100, "payload": {"…": "the platform's raw event"}}
```

The account's `inbound` setting names the one method its listener may
claim. The verb refuses any other. Each mirrored message records how it
arrived, as `via`.

## The receive verb

`fleet gateway receive --account NAME` reads one envelope from stdin and adds
the event to its thread's Conversation. It is a manager verb, like
`fleet send`, and takes no lease.

- An event in a channel that no route covers is ignored. A route covers a
  channel id that it lists. For a delivery with `direct`, the account's DM
  route, with `"dm:*"` in `channels`, covers every other chat. The
  account's wildcard route, with `"*"` in `channels`, covers every other
  channel. Neither covers the `gateway.logs` channel.
- A Conversation that the DM entry covers is created with `spec.direct`.
  An `Edited` or `Removed` event without `direct` is read as `direct` when
  the thread's Conversation records `spec.direct`. Reason: a platform can
  leave the chat's type out of an edit.
- An event in a thread Fleet does not mirror yet creates the Conversation
  only when it could start a Task, earn a refusal, or is Fleet's own
  keyed post. Other chatter is ignored, so a busy channel costs no
  objects. This holds in every channel that a wildcard route covers, and in
  every DM.
- A redelivered event changes nothing.

| Exit | Meaning | The listener |
| --- | --- | --- |
| 0 | Written, a duplicate, or ignored | Acknowledges. |
| 1 | Refused: the thread already holds 100 unread events, the Conversation is being deleted, `verified` is not the account's `inbound`, or the account is not configured | Does not acknowledge. |
| 4 | Unknown: an unreadable envelope, payload or record | Does not acknowledge. The platform redelivers. |
| 5 | A write conflict that the verb's retries did not resolve | Does not acknowledge. The platform redelivers. |

A refused event that the platform stops redelivering is recovered by the
history backstop, for a tied, active thread on an account with history.

### Listener heartbeats

`fleet gateway receive --account NAME --beat` reads one heartbeat instead of
an envelope:

```json
{"instance": "a1", "state": "connected", "digest": "sha256:…", "sentAt": 1730000419990, "lastDeliveryAt": 1730000300100}
```

- `instance` names one listener process, so two processes on one account
  never overwrite each other.
- `state` is `connecting`, `connected` or `closing`.
- `digest` is the account configuration the listener started from.
- A listener beats on every state change and at least every
  `gateway.beatMs`.
- The verb stamps each beat with its own clock, so a listener's wrong clock
  cannot make it look alive.

`--beat` uses the same exit codes. It creates the account's
[Listener](../../resources/listener.md) if the manager has not yet. The verb
also records on the Listener every envelope it refuses or cannot read (exit
1 or 4), counted by reason. An envelope for an account the config lacks is
refused without a record, since there is no Listener; `fleet gateway
status` shows that the account is missing.

### Other verbs

```sh
fleet gateway account NAME --json   # what a listener needs to connect, with the digest
fleet gateway status                # per account: accepted, resolved, inbound, last delivery
```

## Controllers

All three controllers are pure: they read what the manager observes and
return a plan. The manager owns I/O, persistence and process execution, and
performs every effect and every write. Each controller has the shape of the
[Forge controller](../forge/interface.md): `due`, `probes` and `reconcile`.

### ConversationController

```rust
pub struct ConversationInput<'a> {
    pub object: &'a Object,
    pub observations: ConversationObservations,
    pub now: Millis,
    pub defaults: GatewayDefaults,
}

pub fn due(object: &Object, now: Millis) -> bool;
pub fn probes(&self, input: &ConversationInput<'_>) -> Result<Vec<Probe>, StoreError>;
pub fn reconcile(&self, input: &ConversationInput<'_>) -> Result<Plan, StoreError>;
```

- `defaults` carries the route the spec names, its admission rules, the
  people table, the route's brief text and `gateway.retainMs`, as values.
  The manager reads the brief when it loads the config, so the controller
  reads no file.
- `due` holds when the thread has unread events, when `notBefore` passed,
  or when an effect awaits its receipt and a recovery read is due.
- `probes` asks `History` for the first read, the backstop and recovery;
  `Object` for the tied Task, or the Task a Question marker names; and
  `Capabilities` for the account, at most once per account per sweep.
  While reactions are on, it also asks `Capabilities` when a create is
  pending or waits, when a message is marked queued, and when a reaction
  record is left.
- `reconcile` returns:
  - `status`: the whole Conversation status. The same write drains the
    events it numbered.
  - `creates`: at most one Task, from the route, with its intent in the same
    status.
  - `effects`: at most one refusal `Post` per author, one busy notice
    per tie number, the `React` actions of the marks below, and the
    retirement's one `DeleteTask` or `RemoveDir` step.
  - `delete: true`: deletes the object as the last step of its
    retirement, fenced on `uid` and `resourceVersion`.

It works over each [Conversation](../../resources/conversation.md):

- numbers new messages into the mirror, from the receive verb and from a
  `history` backstop while the thread is active,
- starts a Task for an admitted message that meets the route's mention
  rule, unless the thread belongs to a Task already,
- ties a thread Fleet opened to its Question's Task, through the marker on
  the opening post,
- marks the thread Orphaned when its Task is readably gone, or when another
  Task now holds that name,
- sends one polite refusal per author who is not on the allowlist and
  mentions Fleet,
- marks the trigger of a created Task working, and the messages of a
  waiting request queued, then working when its Task is created, under
  `status.reactions`,
- retires the thread after its retention: its Concluded Task, the
  directory the manager made for it, then the Conversation.

A thread that Fleet opened belongs to the Question's Task. While that Task
exists, a mention there starts no other Task. If the opening post's marker
never arrives, the thread ignores mentions, and the person asks again in a
new thread. Fleet does not guess which Task a thread belongs to.

Two limits apply. Once the Question's Task is readably gone, or the opening
post was deleted, a mention may start a Task. On a platform with neither
history nor a marker, a mention that arrives before Fleet records its own
opening post may start a Task; the Question's Task then raises
`ConversationGone`.

The Task it creates:

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "Task",
  "metadata": {
    "name": "conv-3f9a1c0d2b7e-1",
    "labels": {"gateway.fleet.dev/conversation": "conv-3f9a1c0d2b7e",
               "gateway.fleet.dev/conversation-uid": "<conversation uid>",
               "gateway.fleet.dev/route": "eng"}
  },
  "spec": {
    "configRef": {"name": "conversation"},
    "brief": "<the route's brief file, as read when the Task is created>",
    "workspace": {"dir": "/work/conversations/conv-3f9a1c0d2b7e"},
    "dispatch": true
  }
}
```

The name is the Conversation's name with a number that grows each time the
thread starts a new Task. Only this create sets the two `conversation`
labels; `apply` refuses them.

#### Workspaces

A route with a `dir` workspace names a base directory. Each thread gets
its own directory below it: `<dir>/<conversation name>`, for example
`/work/conversations/conv-3f9a1c0d2b7e`. These rules apply:

- Every Task of one thread gets the thread's directory. After an
  orphaning, the next Task gets the same directory, so it finds what the
  earlier Task left.
- ConversationController computes the directory from the route and the
  Conversation's name. It reads no file.
- The create carries an `exclusive: {workspace}` precondition that names
  the thread's directory.
- The manager checks the precondition. Then it creates the directory, with
  its missing parents, on the Task's host. Then it creates the Task.
- A directory that the manager cannot create is a failed create. A
  directory creation with an unknown outcome is an Unknown create. Both
  follow the create receipts in [effect and recovery
  keys](#effect-and-recovery-keys). No Task names a missing directory.
- The directory is created on the Task's host: the Task's `host`, else
  its TaskConfig's `host`, else `local`. A TaskConfig that is absent makes
  the create fail, because the directory's host is not known. The thurbox
  runtime creates directories on the local host only. On another host,
  the create fails and says why.

Threads never wait for each other. The manager refuses the create with
`WorkspaceBusy` only while another Task already has the thread's own
directory. Examples are the previous Task of an Orphaned thread that is
still being deleted, or a Task that someone applied there. The thread
then:

- raises `NeedsAttention/WorkspaceBusy`,
- tells the person once that the request is queued,
- starts the Task, with the whole thread, once the directory is free,
- starts nothing if the message that asked is removed meanwhile.

A route with a `repositories` workspace has no shared directory. Its create
carries no `exclusive` precondition, and each Task gets the workspace the
runtime makes for it.

The manager records the thread's directory on the Conversation, as
`status.dir`, when it created the directory. A directory that existed
before is not recorded. Fleet removes a thread's directory only when the
thread retires, and only a recorded one:

- The thread retires after `gateway.retainMs` with no message, once it is
  Untied, Orphaned, or tied to a Concluded Task.
- The manager removes the directory only while no Task names it.
- Fleet never removes a directory that existed before, a route's base
  directory, or a `repositories` workspace.

The [state machine](state-machine.md#conversation-phases) gives the order
of the retirement's steps.

The config is refused when one route's base directory equals, contains or
lies inside another's. The check does not stop a Task applied or created by
a plugin into a thread's directory later, so no other Task should name a
directory below a route's base directory.

### ListenerController

```rust
pub struct ListenerInput<'a> {
    pub object: &'a Object,
    pub observations: ListenerObservations,
    pub now: Millis,
    pub defaults: GatewayDefaults,
}

pub fn due(object: &Object, now: Millis) -> bool;
pub fn probes(&self, input: &ListenerInput<'_>) -> Result<Vec<Probe>, StoreError>;
pub fn reconcile(&self, input: &ListenerInput<'_>) -> Result<Plan, StoreError>;
```

- `defaults` carries the account's configuration and its digest, the one
  `fleet gateway account` prints.
- `due` holds when `notBefore` passed, or when a heartbeat or refusal was
  recorded since the last pass.
- `probes` asks `Capabilities` and `GatewayWhoami` for the account, every
  `gateway.pollMs`, from the sweep's per-account cache.
- `reconcile` returns status alone: the three conditions below, and
  `delete: true` for a Listener whose account left the config.

It works over each account's [Listener](../../resources/listener.md).
Fleet's calls are outbound, so reaching the platform's API proves nothing
about inbound delivery. ListenerController judges the listener's heartbeats
against the configuration Fleet runs:

| Condition | True when | False reasons |
| --- | --- | --- |
| `Accepted` | The account is valid and some route uses it. | `NoRoute` |
| `Resolved` | The platform's `whoami` is the configured bot, and `capabilities` answered. | `IdentityMismatch`; Unknown with `Unreachable` when `whoami` or `capabilities` is Unknown |
| `InboundReady` | Some instance beat `connected`, on the current digest, within three `gateway.beatMs`, and in that time the account had no envelope refused for the listener's own fault: one unreadable as an envelope, or one whose `verified` is not the account's `inbound`. A full queue or an unreadable payload is one thread's trouble and does not count. | `NoListener`, `Connecting`, `Closing`, `ListenerStale`, `ListenerConfigOld`, `DeliveriesRefused` |

A connected listener whose platform app is not subscribed to messages
delivers nothing. Fleet cannot see that setting, so it shows each instance's
`lastDeliveryAt` instead of guessing.

### GatewayController

```rust
pub struct GatewayInput<'a> {
    pub task: &'a Object,
    pub observations: GatewayObservations,
    pub now: Millis,
    pub defaults: GatewayDefaults,
}

pub fn due(task: &Object, now: Millis) -> bool;
pub fn probes(&self, input: &GatewayInput<'_>) -> Result<Vec<Probe>, StoreError>;
pub fn reconcile(&self, input: &GatewayInput<'_>) -> Result<Plan, StoreError>;
```

- `due` holds when the tied Conversation moved, when the Task declared or
  reported something not yet read, when a child changed, while the tie is
  being set up, or when a post's recovery read is due. A quiet tied Task
  costs nothing.
- `probes` asks `Object` for the Conversation, each followed ChangeRequest
  and the parent; `List` for the children, by the parent labels;
  `Capabilities` per account, also before a relay while reactions are on;
  and `History` to recover a post or an open
  in the thread, or a log post in the log channel. The manager answers
  `List` from one Task listing per sweep.
- `reconcile` returns:
  - `status`: `status.gateway`.
  - `appends`: the inbox entries, through `record_relay`, so the entry and
    the cursor land in one write.
  - `effects`: the `Open`, `Receive`, `Post`, `Log` and `React` actions
    whose intents the status carries. One pass plans at most one thread
    effect and one log effect, and the reactions of its relay and of one
    reply.
  - `remove_finalizer`: always `None`.

It is a second controller over each [Task](../../resources/task.md). It
owns `status.gateway` and:

- joins a newly tied Task with the whole retained thread,
- relays new messages into the inbox, under a cursor,
- posts each `Question` and `Comment` in the thread, and one ready notice
  per followed change,
- logs every Task's Drafts, result and change news in the log channel,
- opens a thread for a Question from an untied Task,
- records answers to Questions,
- routes a child Task's words into its parent's inbox,
- marks each relayed admitted message working, and replied once Fleet
  posted in the thread. Where admission's working reaction on the message
  landed, the join adds no second one.

#### Fields this controller owns

| Field | Meaning |
| --- | --- |
| `conversation` | `{name, uid, state}` of the tied Conversation. `state` is `opening`, `opened`, `waiting`, `tied` or `abandoned`, as the [state machine](state-machine.md#the-tie-on-a-task) defines. |
| `seen` | The Conversation `revision` last read. |
| `cursor` | The Conversation `revision` relayed through. It starts at `0`, so the first relay is the whole retained thread. |
| `declared` | How many items of `status.declared` were read. |
| `result` | The result generation logged, or read while no log channel is configured. |
| `changes` | Per ChangeRequest name, the `state.seq` logged through, or read while no log channel is configured. |
| `children` | Per child Task name, `{uid, seen, declared, result}`. |
| `posts` | Per intent key, `{intent, receipt, attempts, recovery}`, for thread posts, log posts and reactions alike. `intent.action` tells them apart: `post`, `open`, `receive`, `log` or `react`. The newest 100 settled, and every unsettled. A `react` record is dropped once it is settled or given up. |
| `posted` | Every Question and Comment id posted, and every Draft id logged, for the life of the Task. |
| `logged` | How many items of `status.declared` the log lane read for Drafts. |
| `ready` | The `uid` of each followed ChangeRequest whose ready notice was posted in the thread. |
| `questions` | Per Question id, where and when it was posted. |
| `gaps` | How many relays found messages dropped from the mirror. |
| `reactions` | `marks`: per message id in the thread, `working` or `replied`, for the messages the mirror still holds. `replies`: the keys of the thread posts already counted against the working marks. |
| `conditions` | `Tied`, `NeedsAttention`. |

ConversationController owns `status.reactions` on a Conversation:
`marks`, per message id, `queued` or `working`, for the messages the mirror
still holds. A `working` mark there lasts while the tie lasts. Its reaction
records are in the Conversation's `status.effects`, with `intent.action`
`react`.

`status.answered` has no single owner. It maps a Question id to
`{key, from, at}`, and only an inbox append that carries `answers` writes
it.

### Actions

| Action | Performed through | Receipt |
| --- | --- | --- |
| `Open { account, route, channel, text, key }` | `Gateway::open` | `Opened { conversation, message, at }`, Failed, or Unknown |
| `Receive { account, address, route, delivery }`, under `<question key>/receive` | The store, never a platform: it creates the opened thread's Conversation and adds Fleet's opening post, as the receive verb would | `Received` (written or a duplicate), Failed, or Unknown |
| `Post { conversation, text, key }` | `Gateway::post` | `Posted { message, at }`, Failed, or Unknown |
| `React { account, conversation, message, reaction, on, key }` | `Gateway::react` | `Reacted { already }`, Failed, or Unknown |
| `Log { account, channel, text, thread, key }` | `Gateway::post` to the channel address, a top-level post. The manager adds a line that names `thread`, the tied thread with its account, as `Gateway::link` gives it, else by its address. | `Posted { message, at }`, Failed, or Unknown |

`Receive` is the one way a plan reaches a Conversation, and it changes
nothing outside the store. `Log` has no `Receive`: a log post creates no
Conversation.

### Changes to Plan and Probe

`Plan` and `Probe` in `fleet_api::plan` change as follows, each with its
architecture rule:

- `Plan.creates`, today only in the plugin protocol, is carried by
  ConversationController's plans. The manager's create verb performs it.
- `Plan.appends`, today ForgeController's, is also GatewayController's.
- `Plan.delete`, today ChangeRequestController's, is also
  ConversationController's.
- A create may carry an `exclusive: {workspace}` precondition. See
  [workspaces](#workspaces).
- New runtime calls, `Runtime::create_dir(host, dir)`, which says whether
  it made the directory, and `Runtime::remove_dir(host, dir)`. The manager
  calls the first before a conversation Task's create, after the
  precondition, and the second as a thread retires. See
  [workspaces](#workspaces).
- New actions, `DeleteTask { name, uid }` and `RemoveDir { host, path }`.
  Only ConversationController plans them, as steps of a thread's
  retirement. The manager performs `DeleteTask` through the store's
  `retire_conversation_task`, which refuses a Task that is not Concluded
  or not this thread's. It performs `RemoveDir` through the runtime, after
  it checks that `path` is the directory `status.dir` records and that no
  Task names it.
- New probes: `Probe::List { kind, labels }`, answered by the store;
  `Probe::History`, `Probe::Capabilities` and
  `Probe::GatewayWhoami { account }`, answered by the Gateway port. The
  existing `Whoami { host }` stays the Forge port's.
- New actions: `Open`, `Post` and `Receive`, above.
- A new action, `React`, above. Only GatewayController plans it. The
  manager performs it through `Gateway::react`, after the intent and under
  the lease.
- A new action, `Log`, above. Only GatewayController plans it. The
  architecture rule that keeps the Gateway port in the manager covers it:
  the manager performs it through `Gateway::post`, after the intent and
  under the lease, and no controller holds the port.

The kind names `Conversation` and `Listener` are reserved. Once they are
built in, a plugin manifest that claims either kind is refused.

## Declarations the Gateway reads

A Task talks by declaring items in `status.declared` with
`fleet declare`. The Gateway reads these kinds and ignores the rest:

| Kind | Fields | Tied Task | Untied Task | Child |
| --- | --- | --- | --- | --- |
| `Question` | `id`, `text` | Posted once. | Opens a thread, then is tied to it. | Sent to the parent's inbox. |
| `Comment` | `id`, `text` | Posted once. | Skipped. | Sent to the parent's inbox. |
| `Draft` | `id`, `title`, `text` or `url` | Logged once, as awaiting review. Never posted in the thread. | Logged the same way. | Sent to the parent's inbox, and logged. |
| `ChangeRequest` | `url`, `name`, as today | Logged when followed, and each time its state moves. The thread gets one ready notice with the URL, the first time `everReady` is true, and no other news. | Logged the same way. | Sent to the parent's inbox, and logged. |
| `status.result` | As today | Logged once per generation. Never posted in the thread. | Logged the same way. | Sent to the parent's inbox, and logged. |

"Logged" means a top-level post in the `gateway.logs` channel. Without
`gateway.logs`, nothing is logged.

`id` is a string unique for the Task. An `id` already posted is never posted
again. An item without an `id` is skipped and named in
`NeedsAttention/DeclarationInvalid`.

## Inbox entries

| Entry | Key | From | Holds |
| --- | --- | --- | --- |
| A relay, or the join | `task/<name>/<uid>/gateway/<conversation uid>/<revision>` | `gateway/<conversation>` | The new messages, each with its author, the author's platform id, its time and its text. `about` is `{uid, revision}` of the Conversation. |
| A child's item | `task/<name>/<uid>/gateway/child/<child uid>/<index or result/generation>` | `gateway/child/<child>` | One declaration or result of the child. |

- Messages from people not on the allowlist come first, under a header that
  marks them as content, never instructions.
- An edited message is relayed again, marked as an edit.
- An entry holds at most 64 KiB of messages. It keeps the newest and says
  how many older ones remain on the platform.
- While the inbox holds more than 20 entries still to deliver, new messages
  wait. An entry given up on does not count.
  Posting goes on meanwhile.
- One pass appends no more entries than the store accepts. The relay comes
  first, then each child's items in order.
- A child's items past that bound wait for the next pass after a delivery.
  The items that went out are not sent again.

### Answers

An inbox append may carry `answers`, a list of Question ids. In the same
write the store records each id in the Task's `status.answered`, as
`{key, from, at}`. The first answer wins.

- A relay carries `answers` when an allowlisted message was written after an
  open Question was posted in that thread.
- A plugin's append and `fleet send --answers ID` can carry it too.
- The store refuses an id that names no declared Question.

## Admission

Admission sorts each message by the route and the people table:

| Author | Class | Effect |
| --- | --- | --- |
| Fleet | `Own` | Never relayed. |
| A person under `route.allow` | `Admitted` | A turn. |
| A bot under `route.allowBots` | `Admitted` | A turn. |
| Anyone else | `Context` | Relayed with the next `Admitted` message. Starts no turn. |

An `Admitted` message starts a Task in an untied thread only when it meets
the thread's start rule:

- `route.mention = "required"`, the default: the message mentions Fleet.
- `route.mention = "top-level"`: the message starts a thread. This suits a
  channel where every top-level post is a request, such as an alert
  channel.
- The Conversation records `spec.direct`: the message starts the thread or
  mentions Fleet. This rule replaces `route.mention` in a DM thread.

In a tied thread, every `Admitted` message is a turn, with or without a
mention.

A person who is not on the allowlist gets one refusal per author and
thread. These messages earn it:

- A message that mentions Fleet.
- In a thread with `spec.direct`, a message that starts the thread.

Past 200 refused authors in a thread, no further refusal is sent. An
unlisted bot gets none.

## Effect and recovery keys

Every post and Task creation is a keyed effect. Fleet writes the intent
before it acts and the receipt after. Fleet never repeats a key blindly. It
recovers an unknown outcome by looking for the key on the platform or in the
store. On a platform with `marker = nonce { windowMs }`, it resends the post
under the same key, so with the same nonce, inside the window. The
[adapters](adapters/README.md#capabilities) define the rule.

| Item | Key |
| --- | --- |
| Question | `task/<name>/<uid>/gateway/question/<id>` |
| Comment | `task/<name>/<uid>/gateway/comment/<id>` |
| Draft, in the log channel | `task/<name>/<uid>/gateway/draft/<id>` |
| Ready notice, in the thread | `task/<name>/<uid>/gateway/ready/<cr uid>` |
| Change news, in the log channel | `task/<name>/<uid>/gateway/change/<cr uid>/<state.seq>` |
| Result, in the log channel | `task/<name>/<uid>/gateway/result/<generation>` |
| Working reaction, added | `task/<name>/<uid>/gateway/react/<message id>/working/on` |
| Replied reaction, added | `task/<name>/<uid>/gateway/react/<message id>/replied/on` |
| Working reaction, removed on the reply | `task/<name>/<uid>/gateway/react/<message id>/working/off` |
| Queued reaction, added at admission | `conversation/<conv uid>/react/<message id>/queued/on` |
| Working reaction, added at admission | `conversation/<conv uid>/react/<message id>/working/on` |
| Queued reaction, removed when the Task is created or the wait is withdrawn | `conversation/<conv uid>/react/<message id>/queued/off` |
| Refusal | `conversation/<conv uid>/refuse/<author id>` |
| Task creation | `conversation/<conv uid>/task/<n>` |
| Busy notice | `conversation/<conv uid>/busy/<n>` |
| Retirement: the Task's delete | `conversation/<conv uid>/retire/<at>/task` |
| Retirement: the directory's removal | `conversation/<conv uid>/retire/<at>/dir` |

A failed post is retried under the same key at most three times, then raises
`NeedsAttention/PostFailed`. A post whose outcome stays unknown, on a
platform where Fleet cannot find it again, raises
`NeedsAttention/PostUnconfirmed` and waits for a person. A split post is
found only when every part is found. A split that landed in part is never
repeated where markers read back: it raises `NeedsAttention/PostUnconfirmed`.
Where the platform honours a nonce, a resend inside the window finishes it.
Three recovery reads that cannot complete raise
`NeedsAttention/RecoveryIncomplete`, until one does.

A retirement step follows these rules:

- A refused step is tried again under the same key after the delay, at
  most three times. Then `NeedsAttention/RetireFailed` holds the thread
  for a person.
- A step with an unknown outcome is performed again under the same key,
  one `pollMs` later. A deleted Task and a removed directory stay so, so
  the repeat is harmless.
- A `RemoveDir` that finds another Task naming the directory waits one
  `pollMs` and raises `NeedsAttention/WorkspaceBusy`. The wait does not
  count as an attempt.
- A crash after the manager created a directory and before it recorded
  it leaves the directory unrecorded. Fleet then keeps that directory.

A reaction follows its own rules:

- A reaction is never looked for. Reason: a reaction is set, not counted,
  so performing it again under the same key changes nothing.
- A refused reaction, one with an unknown outcome, and one whose intent has
  no receipt are performed again under the same key after the delay.
- After three attempts in all, the reaction is given up.
- A reaction raises no condition and holds back nothing.
- A working reaction not yet settled when its message is marked replied is
  dropped. Reason: adding it then would undo the removal.
- A queued reaction not yet settled when its message is marked working is
  dropped, for the same reason.
- ConversationController's working reaction is tried once and never again.
  Its record stays while the mark stands, so the join can read whether it
  landed. Reason: a retry could land after the Task replied.
- These rules hold for the reactions of ConversationController and of
  GatewayController alike.

A log post follows the same rules. Its recovery read is a `since` read of
the log channel's address on the `gateway.logs` account. A log post that
waits holds back only the Task's later log posts. A thread post that waits
holds back no log post.

## Write ownership

| Writer | May change |
| --- | --- |
| `listener/<account>`, through `fleet gateway receive` | Creates a Conversation and adds events to it. Records heartbeats and refusals on a Listener. |
| `conversation-controller` | The rest of Conversation status. Creates conversation Tasks. Deletes its own objects. |
| `listener-controller` | The rest of Listener status. Deletes a Listener whose account left the config. |
| `gateway-controller` | `status.gateway`. Appends to the inbox, recording answers. Creates a thread's Conversation when Fleet opens it, and adds Fleet's opening post. |
| `apply` | Creates a Conversation by manifest, only under the name derived from its account and address. Requests deletion. |
| The manager, loading the config | Creates one Listener per account. |
| `apply`, `cli`, `plugin/<name>` | Inbox appends with `answers`. The parent labels on a Task they create. |

Every controller write needs the live lease. The receive verb needs none.

### The parent label

`gateway.fleet.dev/parent` and `gateway.fleet.dev/parent-uid` tie a child to
its parent. Naming a parent sends words into its inbox, so the label is
authority:

- The store accepts the labels only when the child is created, and never
  lets them change.
- Only a writer that may already append to the parent's inbox may set them:
  a person, a `tasks = "any"` plugin, or a `tasks = "own"` plugin that owns
  the parent.
- The parent must exist with that `uid`. A parent the store cannot read
  refuses the create as Unknown, to be retried.

A tie to a thread, once verified, wins over a parent label.

### The route label

`gateway.fleet.dev/route` chooses the route whose first channel id a Task's
Question opens in. It is an ordinary label. A route with no channel id, such
as a route with only `"*"` or `"dm:*"`, raises `NeedsAttention/RouteUnknown`.
