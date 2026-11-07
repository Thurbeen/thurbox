# Gateway controller state machine

Two things change state: a [Conversation](../../resources/conversation.md),
which ConversationController owns, and the tie on each Task, which
GatewayController keeps under `status.gateway`. TaskController still owns
the Task's own `status.phase`. The Conversation phases, the transitions
that create a Task, orphaning and retention are implemented, and so are
every state of the tie on a Task, its `ConversationGone`, the join, the
relay, posting, logging and reactions.

![A Conversation starts Untied. The receive verb creates it, or Fleet creates it when it opens a thread for a Task's Question. A thread that Fleet opened is tied at once to the Task that its opening post's marker names. An admitted mention in an Untied thread creates a Task and ties it. When a tied Task is readably gone, the Conversation becomes Orphaned. The next admitted message there creates a new Task with the thread's history. When a Conversation has no message for its retention and is Untied, Orphaned, or tied to a Concluded Task, it retires: its controller deletes the Task, the manager removes the thread directory that it created, and the controller deletes the Conversation. While the Conversation is Tied, GatewayController relays new messages into the Task's inbox and posts what the Task declares.](state-machine.svg)

## Conversation phases

| Phase | Meaning |
| --- | --- |
| Untied | Fleet mirrors the thread, but no Task is tied to it. |
| Tied | One live Task is tied to the thread. |
| Orphaned | The tied Task is gone. The messages are kept. |

| From | Event | To |
| --- | --- | --- |
| none | An event that could start a Task or earn a refusal, or Fleet's own Question post | Untied |
| Untied | Fleet's Question post, whose marker names a live Task | Tied, to that Task |
| Untied | An admitted message that meets the mention rule | Tied, to a new Task |
| Tied | The Task is readably gone, or another Task holds its name | Orphaned |
| Orphaned | Any admitted message, with or without a mention | Tied, to a new Task with the whole thread |
| Tied | No message for `gateway.retainMs`, and the Task is Concluded: the retirement deletes the Task | Orphaned, retiring |
| Untied or Orphaned | No message for `gateway.retainMs` since the later of the last message and the orphaning, and no Task waiting for the thread's directory; or a retirement in progress | deleted, after the thread's directory is removed |

Behaviour in every phase:

- A Conversation never leaves Tied because its Task is Unknown. Only a
  readably absent Task orphans it.
- A message seen twice, from the listener and from history, is one message.
- On a route with a `dir` workspace, every Task of the thread gets the
  thread's own directory, `<dir>/<conversation name>`. Threads never wait
  for each other.
- When another Task already holds the thread's own directory, the thread
  stays where it is, says so once, and starts its Task when the directory
  is free. A waiting thread never retires.
- A thread whose Task is not Concluded never retires.
- A retirement runs its steps in order: the Task, the directory, the
  Conversation. Each step is a keyed effect under
  `conversation/<uid>/retire/<at>/`, so a crash between two steps resumes
  at the first step without a settled receipt.
- A message numbered during a retirement stops it. The thread keeps its
  directory.
- A thread Fleet opened for a Question starts no Task from a mention while
  the Question's Task exists. The [interface](interface.md#conversationcontroller)
  gives the limits.

## The tie on a Task

| State | Meaning |
| --- | --- |
| none | The Task is not tied. It can still open a thread with a Question. |
| opening | Fleet is opening a thread for the Task's Question. |
| opened | The thread is open. Fleet is adding its opening post to the thread's Conversation, through `Receive`. |
| waiting | The Conversation exists, and Fleet waits for it to name this Task. |
| tied | The Task and the thread are tied. Fleet relays and posts. |
| abandoned | The opened thread never reached its Conversation. The Task acts as untied. |
| child | The Task has a parent. Its words go to the parent's inbox. |

| From | Event | To |
| --- | --- | --- |
| none | The Task carries the `conversation` label, because a thread created it | waiting |
| none | The Task declares a `Question` and has no live parent | opening |
| none | The Task carries parent labels naming a Task that exists with that `uid`, or that is Unknown | child |
| opening | The thread is open | opened |
| opening | Three attempts are refused | none, with `PostFailed` |
| opening | The outcome is Unknown, and the platform has no history or marker | opening, with `PostUnconfirmed`, until a person acts |
| opened | `Receive` succeeded | waiting |
| opened | The Conversation already names this Task | tied |
| opened | Three `Receive` attempts failed | abandoned, with `ReceiveFailed` |
| waiting | The Conversation names this Task | tied |
| abandoned | The Conversation later names this Task | tied |
| abandoned | A later `Question` | opening |
| abandoned | The Conversation is readably gone | none |
| abandoned | The Conversation names another Task | none, with `ConversationGone` |
| tied or waiting | The Conversation is readably gone, or names another Task | none, with `ConversationGone` |
| child | The parent is readably gone | none |

`child` is not a value of `status.gateway.conversation.state`. Fleet decides
it on each pass from the parent labels and the parent's existence, and a
child's `conversation` field is absent.

While the tie is `opening`, `opened` or `waiting`, the Task's later
declarations wait. They are posted in order once the tie is verified. A
Comment on an untied Task is skipped and never posted later. The tie does
not change [logging](#logging): a Task in any state of the tie logs its
Drafts, result and change news.

## Joining

A newly tied Task joins at the beginning of the thread. Its first inbox
entry holds the retained messages, so a mention partway through a thread
brings its history. If the mirror already dropped older messages, the entry
says that they remain on the platform. A Task tied by its own Question has
nothing before its own post.

## Relay

GatewayController relays when a tied thread has new admitted messages:

- Everything since the last relay goes out as one inbox entry. The entry,
  any answers it records and the cursor advance land in one write, or not
  at all.
- Messages from people not on the allowlist wait. They go out with the next
  admitted message, under a header that marks them as content.
- Fleet's own posts are never relayed.
- If the mirror dropped messages that were never relayed, because the
  thread outgrew the 200 it keeps, the entry says that they remain on the
  platform, and the Task raises `NeedsAttention/RelayGap`.
- Each admitted message in the entry that has no mark gets the working
  reaction. See [reactions](#reactions).

## Posting

GatewayController posts in a tied thread, in this order:

1. every `Question` and `Comment` the Task added since the last pass, in
   declaration order;
2. one ready notice per followed change, with its URL, the first time the
   change is in a state other than `draft` (`everReady` is true).

It posts nothing else in a thread. A `Draft`, a result and all other change
news go to the [log channel](#logging). The ready notice is posted whether
or not `gateway.logs` is set.

- Each post is a keyed effect: intent, then the platform call, then the
  receipt.
- Posts to one thread go out one at a time. A post with an unknown outcome
  holds back later posts to that thread, and no other thread.
- A full inbox does not hold back posting.
- A post that lands marks every message still marked working as replied.
  See [reactions](#reactions).

## Logging

GatewayController logs, for every Task, its `Draft` declarations, its
result and the news of each followed change, in this order. A Task needs no
tie and no route to log.

- A `Draft` is logged once per `id`. A result is logged once per
  generation. A change is logged each time its state moves: followed,
  draft, open (ready for review), merged or closed.
- Each log post is a keyed effect, as a thread post is. It is one
  top-level post in the `gateway.logs` channel.
- A Task's log posts go out one at a time. A log post with an unknown
  outcome holds back that Task's later log posts. It holds back no thread
  post, and no thread post holds it back.
- An unknown outcome is recovered from the log channel's history, with the
  thread posts' rules: a split is found only when every part is found, and
  three reads that cannot complete raise `RecoveryIncomplete`.
- Without `gateway.logs`, a Draft, a result or change news is marked read
  and posted nowhere. A log post whose outcome is not settled when `gateway.logs`
  leaves the config is dropped, never posted elsewhere.

## Reactions

Each message of a thread has one mark. Two controllers keep the marks:

- ConversationController keeps the marks that admission makes, before the
  Task has the message, under the Conversation's `status.reactions`.
- GatewayController keeps the marks from the join on, under the Task's
  `status.gateway.reactions`.

Reactions are on unless `gateway.reactions` is `false`, and only where the
account's capabilities say `reactions`.

| Mark | Meaning |
| --- | --- |
| none | Fleet added no reaction. |
| queued | Admission accepted the message, and the message waits for its Task. Fleet added the queued reaction. |
| working | The Task has the message, or admission just created the Task for it. Fleet added the working reaction, and removed the queued reaction. |
| replied | Fleet posted in the thread since. It added the replied reaction and removed the working reaction. |

| From | Event | To |
| --- | --- | --- |
| none | Admission starts a Task for the message, and the create answers `Created` | working |
| none | Admission starts a Task for the message, and the create answers `WorkspaceBusy` | queued |
| none | The thread's create waits for its directory, and the message is `Admitted` and later than the trigger | queued |
| queued | The create answers `Created`, and the capabilities are known on that pass | working |
| queued | The create answered `Created` on an earlier pass, whose capabilities were Unknown | none, and Fleet removes the queued reaction. The join marks the message working. |
| queued | The wait is withdrawn: the trigger left the mirror | none, and Fleet removes the queued reaction |
| none | A relay or the join delivers the message, and it is `Admitted` | working |
| working | A Question, a Comment or a ready notice lands in the thread for the Task | replied |
| queued | The message leaves the mirror, removed or rotated out | none, and Fleet removes the queued reaction |
| working or replied | The message leaves the mirror, removed or rotated out | none, and Fleet changes no reaction |
| queued, working or replied | `gateway.reactions` turns `false` | none, and Fleet changes no reaction |
| working or replied | The tie ends: the Conversation is readably gone, or names another Task | none, and Fleet changes no reaction |

Rules:

- Fleet adds the working reaction to a message once. The join marks a
  message that admission marked working, and adds no reaction where
  admission's working reaction landed. Otherwise the join adds it.
- Admission tries its working reaction once. Reason: a retry could land
  after the Task replied, and the working reaction would then stay.
- A message marked queued keeps the queued reaction when a thread post
  lands. Reason: nothing worked on it yet.
- A message that is edited and relayed again keeps its mark.
- A thread post counts as a reply only to the messages marked working
  before it landed.
- A log post, a refusal and a busy notice are no reply.
- A reaction of ConversationController is a keyed effect on the
  Conversation, under `conversation/<uid>/react/`. It is best effort, as a
  reaction of GatewayController is.
- The capabilities of the account are read before the relay. When they are
  Unknown, or say no `reactions`, the relay goes on and marks nothing.
- A mark changes when Fleet plans its reactions, not when they land. A
  reaction that fails changes no mark.

## Children

A child's Questions, Comments, Drafts, changes and result each become one
entry in its parent's inbox, from `gateway/child/<child>`. A child logs its
Drafts, result and change news as every Task does. The parent
answers with `fleet send CHILD --answers ID TEXT`, which records the child's
`status.answered`. A summary or declaration text over 4 000 bytes is not
quoted. The entry gives its size and `fleet get task CHILD` to read it, so
that the entry stays short enough to type. Only the parent speaks in the
thread. A child whose
parent is gone acts as an untied Task, and its earlier declarations are not
replayed.

## Conditions

On a Conversation:

| Type | True means |
| --- | --- |
| `Reachable` | The platform's history answered. |
| `Tied` | A live Task is tied. |
| `NeedsAttention` | One of `TaskCreateFailed`, `RouteUnknown` (the route left the config), `MentionsOnly` (the platform delivers only mentions, so only mentions are relayed), `WorkspaceBusy` (another Task holds the thread's own directory), `PostFailed` (three attempts at a refusal or busy notice were refused), `PostUnconfirmed` (a refusal or busy notice has an unknown outcome on a platform where Fleet cannot find it again), `RetireFailed` (three attempts at one step of the thread's retirement were refused). |

On a Task, under `status.gateway.conditions`:

| Type | True means |
| --- | --- |
| `Tied` | The Task is tied to a thread it verified. |
| `NeedsAttention` | One of `RelayGap`, `ConversationGone`, `PostFailed`, `PostUnconfirmed`, `DeclarationInvalid`, `TooManyChildren` (over 200), `RecoveryIncomplete` (Fleet cannot finish reading history to find a post), `ReceiveFailed`, `RouteUnknown`. With `TooManyChildren`, no child is relayed. |

An account's identity and its listener are conditions on its
[Listener](../../resources/listener.md), not on any Conversation.

`NeedsAttention` stops nothing else. Relay goes on while a post waits, and a
held post in one thread holds back no other thread. A reaction raises no
condition.

## Deletion

Deleting a conversation Task deletes nothing else. Its Conversation becomes
Orphaned and is kept. Deleting a Conversation changes nothing on the
platform. Its Task, if any, raises `ConversationGone` and keeps running.
Neither controller adds a finalizer.

A retirement deletes the thread's Task, its directory and its
Conversation, in that order. A person who deletes a Task or a Conversation
by hand deletes only that object. The thread's directory then stays until
the thread retires. A Conversation that a person deletes takes its record
of the directory with it, so Fleet does not remove that directory.
