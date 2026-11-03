# Gateway controller

Implemented. The installed binary has steps 1 to 9 below: the `[gateway]`
key, the Gateway port, the `fleet-gateway` crate with its fake, Slack and
Discord adapters, the `Conversation` and `Listener` kinds,
ConversationController (the mirror, admission, Task creation in each
thread's own directory under the workspace precondition, the busy notice
and the refusal post),
ListenerController, and the `fleet gateway receive`, `account` and `status`
verbs. It has GatewayController's tie, the join and the relay,
`status.answered` with the `answers` inbox field, the parent labels and a
child's words reaching its parent's inbox. It posts what a tied Task
declares, opens a thread for an untied Task's Question, and posts every
Task's result and change news in the log channel. It marks each message
it relays with a working reaction, and with a replied reaction once Fleet
posted in the thread. The listener programs stay outside Fleet.

The Gateway ties chat threads to Tasks:

1. A person mentions Fleet's bot in a thread.
2. Fleet starts a Task for that thread.
3. Every later message in the thread reaches the Task's inbox.
4. What the Task declares for the person is posted in the thread.

A thread carries only what is useful to the person in it: the Task's
Comments and Questions, and one notice when a change it follows is first
ready for review. A Task's Drafts, its result and the news of its changes
are factory logs. They go to one log channel, for every Task, when the
config names one.

The Gateway never runs a model. It admits people, keeps the tie between a
thread and its Task, and moves messages. The Task does all agentic work.

The Gateway is the [Forge controller](../forge/README.md) applied to chat.
A Task declares what it wants said. A mirror object follows the thread.
Fleet relays new messages into the inbox under a cursor, and recognises its
own posts by a marker. One adapter per platform sits behind one port. Slack
and Discord have adapters. Telegram and Microsoft Teams are mapped on paper,
to check the model.

| Read | Answers |
| --- | --- |
| [Conversation](../../resources/conversation.md) | What does the mirror of one thread hold? Who deletes it? |
| [Listener](../../resources/listener.md) | Is an account's listener running, connected and current? |
| [Task gateway status](../../resources/task.md#talking-in-a-conversation) | What does a Task declare to talk? What do `status.gateway` and `status.answered` hold? |
| [State machine](state-machine.md) | Which states does a thread and a tie go through? What is relayed or posted, and when? |
| [Interface](interface.md) | What are the port, the CLI verbs, the declarations, the inbox entries and the keys? |
| [Adapters](adapters/README.md) | Which rules does every adapter keep? How do the platforms compare? |
| [Slack adapter](adapters/slack.md) | How is a Slack account set up, configured and mapped onto the port? |
| [Discord adapter](adapters/discord.md) | How is a Discord account set up, configured and mapped onto the port? |
| [Native config](../../configuration/fleet-toml.md#the-gateway-section) | Where do accounts, people, routes and admission come from? |
| [Overall architecture](../../ARCHITECTURE.md#the-gateway-controller) | Where does this fit in Fleet? |

## The parts

| Part | Role |
| --- | --- |
| Listener | A small program outside Fleet. It holds the platform connection and hands each event to `fleet gateway receive`. One per account. |
| `fleet gateway receive` | A bounded manager verb. It reads one event and adds it to the thread's Conversation. |
| [Conversation](../../resources/conversation.md) | The mirror of one thread. It numbers the messages and records which Task the thread is tied to. |
| ConversationController | Keeps each Conversation current, starts a Task for an admitted mention, and retires old Conversations. |
| GatewayController | A second controller over each Task. It relays the thread into the inbox and posts what the Task declares. |
| [Listener resource](../../resources/listener.md) and ListenerController | Show whether an account's listener is running, connected and on the current configuration. |
| Gateway port | The platform API behind one interface: read events, read history, open a thread, post, react. |

## Flow: a mention starts a Task

1. A person on a route's allowlist mentions Fleet's bot in a channel that
   the route covers. The thread can be new or old.
2. The listener receives the event and runs `fleet gateway receive`. The
   verb creates the thread's Conversation and adds the event to it. The
   listener acknowledges the event only after the verb succeeds.
3. ConversationController reads the thread's history, so a mention partway
   through a thread brings everything before it.
4. The mention passes the route's admission rules. ConversationController
   creates one Task from the route, with the route's TaskConfig and brief
   and the thread's own workspace, and records the tie. On a route with a
   `dir` workspace, the thread's directory is `<dir>/<conversation name>`.
   The manager creates it when it creates the Task.
5. GatewayController sends the whole retained thread to the Task as its
   first inbox entry, after the brief.
6. Each later message from an allowlisted person reaches the inbox and is a
   worker turn. Messages from other people start no turn. They go out with
   the next allowlisted message, marked as content, never instructions.
7. The worker answers with declarations: a `Comment` or a `Question`.
   Fleet posts each one in the thread, in order, and never twice. The
   worker holds no platform credential and names no destination.
8. When a `ChangeRequest` the Task follows first leaves `draft`
   (`everReady` turns true), Fleet posts one notice in the thread with the
   change's URL. This is the only change news a thread gets.
9. The Task's `Draft` declarations, its result and all of its change news
   go to the log channel, never to the thread.
10. The thread lives as long as people write in it. A Concluded Task still
    receives the thread, so a later message reaches the same Task.
11. If someone deletes the Task, the thread becomes Orphaned. The next
    admitted message, with or without a mention, starts a new Task with the
    thread's history, in the same directory.
12. A thread with no message for its retention period retires, if its Task
    is Concluded or it has no live Task. See
    [Flow: a thread's end of life](#flow-a-threads-end-of-life).

## Flow: reactions show progress

1. GatewayController relays admitted messages into a tied Task's inbox:
   the join, or a later relay.
2. Fleet adds the working reaction, 👀 by default, to each admitted message
   in that relay.
3. The Task works, and declares a `Comment` or a `Question`, or a change it
   follows becomes ready for review.
4. Fleet posts in the thread for the Task.
5. Each message in the thread that still has the working reaction gets the
   replied reaction, ✅ by default. Fleet removes its working reaction.

Rules:

- Only an `Admitted` message gets a reaction. A `Context` message and
  Fleet's own posts get none.
- Only a thread post counts as a reply: a Question, a Comment or the ready
  notice. A log post does not count.
- A message that is edited and relayed again keeps the mark it has. Fleet
  marks it working only when it has no mark yet.
- A message that leaves the mirror, removed or rotated out, gets nothing
  more.
- Reactions are best effort. A reaction that fails or has an unknown
  outcome never holds back a relay or a post, and raises no
  `NeedsAttention`.
- Each reaction is a keyed effect: Fleet writes the intent first and the
  receipt after. A reaction is set, not counted, so a repeat after a crash
  changes nothing.
- `gateway.reactions = false` turns reactions off. An adapter whose
  platform has no reactions answers `reactions = false` in its
  capabilities, and Fleet then adds none.

## Flow: a thread's end of life

A thread retires when both of these conditions are true:

- No message arrived for `gateway.retainMs`, seven days by default. The
  period counts from the later of the last message, the orphaning and the
  creation of the Conversation.
- The thread is Untied or Orphaned, or its Task is Concluded.

A retiring thread goes through these steps, in this order:

1. ConversationController deletes the Task that the thread created. The
   delete is fenced on the Task's `uid`. The Task's finalizer then releases
   its session, and the thread becomes Orphaned.
2. The manager removes the thread's directory, if the manager created it.
   It waits while another Task names the directory.
3. ConversationController deletes the Conversation.

Rules:

- A thread whose Task is not Concluded never retires.
- A message that arrives during the retirement stops it. The thread keeps
  its directory, and the next admitted message starts a new Task there.
  While the thread's Task is being deleted, Fleet judges no new message.
  It judges them once the thread is Orphaned, so no message is lost.
- Each step is a keyed effect. After a crash, the retirement continues at
  the first step that has no settled receipt.
- An Unknown read never counts as absence. The retirement waits.
- Fleet removes only a directory that it created for the thread. It never
  removes a directory that existed before, a route's base directory or a
  `repositories` workspace.
- Nothing on the platform changes. A later message there is treated as a
  new thread: a new Conversation, the mention rule again, the thread's
  history, and an empty directory.

## Flow: a Question opens a thread

1. A Task that nobody started from chat declares a `Question`. For example,
   a plugin created the Task.
2. GatewayController opens a new thread in the channel of the questions
   route. The first post is the Question, and it carries a marker that names
   the Task.
3. The new thread gets a Conversation, which is tied to that Task by the
   marker.
4. Replies in the thread reach the Task's inbox. The first allowlisted reply
   written after the Question records it as answered in
   `status.answered`.
5. An answer can also arrive another way, for example through a plugin that
   shows the Question as "Needs input". Whichever answer arrives first wins.
   Fleet does not repost, nudge or remind.

A `Comment` or a ready notice never opens a thread. Fleet posts them only
for a Task that is already tied. A `Draft`, a result or other change news
never goes to a thread.

## Flow: the log channel

1. The config names a log destination: `gateway.logs`, an account and one
   channel on it.
2. A Task declares a `Draft`, reports a result, or a ChangeRequest it
   follows is followed, becomes ready for review, merges or closes.
3. GatewayController posts one top-level post in the log channel. The post
   names the Task. For a tied Task, it also names its thread, as a link
   where the thread's adapter can make one.
4. This holds for every Task: a tied Task, an untied Task, a child, and a
   Task that the CLI, a plugin or another Task started.

Rules:

- A `Draft` is logged once per `id`. A Task's result is logged once per
  generation. Each change news item is logged once, the change becoming
  ready for review included.
- A log post opens no thread, creates no Conversation and ties nothing.
- Fleet relays no reply written under a log post. The log channel is no
  route's channel, so the receive verb ignores every event there.
- Without `gateway.logs`, Fleet posts Drafts, results and change news
  nowhere. It marks them read, so a log channel named later does not
  replay them. The thread's ready notice does not depend on
  `gateway.logs`.

## Flow: a child speaks through its parent

A Task can carry parent labels that name another Task. Its Questions,
Comments, Drafts, changes and result then go to the parent's inbox, not to
a thread. The parent answers with `fleet send CHILD --answers ID TEXT`.
Only the parent speaks in the thread. When the parent is gone, the child
acts as an untied Task.

A child's Drafts, result and change news also go to the log channel, as
every Task's do. The parent's inbox entry is the parent's turn of work. The log
post is the factory's record, which must show every Task.

## Behaviour

These rules hold across all flows:

- **One thread, one mirror, one Task at a time.** A thread has one
  Conversation per account. It is tied to at most one live Task.
- **Fleet speaks only through declarations.** Every post is a keyed effect:
  Fleet writes the intent first and the receipt after.
- **Fleet never posts twice.** A post whose outcome is unknown is looked
  for by its marker, never sent again blindly. On a platform with a nonce
  marker, Fleet resends it under the same nonce inside the platform's window.
  Where a platform cannot find it, Fleet raises
  `NeedsAttention/PostUnconfirmed` and waits for a person.
- **Posts in one thread go out in declaration order.** A post with an
  unknown outcome holds back the later posts to that thread only.
- **A Task's log posts go out in order, apart from its thread posts.** A
  log post with an unknown outcome holds back that Task's later log posts
  only. It holds back no thread post, and a thread post holds back no log
  post.
- **Fleet never relays its own posts.**
- **Reactions never hold anything back.** A failed or unknown reaction is
  tried again under its key at most three times in all, and is then given
  up. It holds back no relay and no post, and raises no condition.
- **Unknown never means absent.** A platform or store answer that Fleet
  cannot read makes Fleet wait. It never makes Fleet act as if something
  is gone.
- **Delivery is at least once.** A redelivered event changes nothing. In
  a tied, active thread, an event the listener lost is recovered from
  history where the platform has it. A mention lost in an untied thread is
  not recovered.
- **A thread Fleet opened belongs to its Question's Task.** While that Task
  exists, a mention there starts no other Task. If Fleet cannot tell which
  Task opened the thread, the thread ignores mentions rather than guess.
- **A route with a directory workspace gives each thread its own
  directory.** The route's `dir` is a base directory. Every Task of one
  thread gets `<dir>/<conversation name>`, so a later Task finds what an
  earlier one left. A route with a `repositories` workspace gives each Task
  its own workspace.
- **Threads never wait for each other.** A thread waits only while another
  Task already holds its own directory. Examples are the previous Task of
  an Orphaned thread that is still being deleted, or a Task that someone
  applied there. The thread then says so once and starts when the
  directory is free.
- **A thread ends once, as a whole.** A thread idle for its retention,
  with a Concluded Task or no live Task, retires: its Task, the directory
  that Fleet created for it, and its Conversation are deleted. Fleet
  deletes no other directory.
- **Every problem is a visible condition.** The Conversation, the Task and
  the Listener each report `NeedsAttention` with a reason. Nothing fails
  silently.

## Why it is shaped this way

### The Forge pattern, applied to chat

| Forge | Gateway |
| --- | --- |
| One adapter per forge kind | One adapter per chat platform |
| `ChangeRequest`, the mirror of one change | `Conversation`, the mirror of one thread |
| `revision` and per-note `seq` | `revision` and per-message `seq` |
| ForgeController owns `status.forge` | GatewayController owns `status.gateway` |
| Relay under a cursor, in one write | The same |
| The worker mark on forge posts | A platform marker on Fleet's posts |
| Mark-ready, a keyed effect | `Open`, `Post` and `Log`, keyed effects |

Chat differs from a forge in three ways:

- **A chat pushes.** Fleet polls a forge, but a chat platform delivers
  events. So Fleet adds an inbound verb and a listener.
- **A thread creates its Task.** A Task opens a change, but a conversation
  starts before any Task exists. So ConversationController creates the Task.
- **Fleet speaks often.** The Forge controller writes one thing. The Gateway
  posts on every declaration.

### A built-in, not a plugin

The Gateway reads the declarations of every Task, keeps a cursor on each
Task in the same write as the inbox append, and must wake when a Task
declares something. A plugin can do none of these today. A plugin also
cannot hold a resident connection, and the platforms need one either way.

A plugin that keeps its own records, works on its own Tasks and polls stays
a plugin. It meets the Gateway only on a Task: in `status.declared`, in the
inbox and in `status.answered`.

### Inbound transport

Fleet's rule is bounded calls and no service of its own to supervise. But
every platform except Telegram pushes events, over a WebSocket or a public
HTTPS endpoint.

| Option | For | Against |
| --- | --- | --- |
| (a) Poll inside bounded calls | Fits Fleet exactly. | Discord and Teams cannot be polled for mentions. Slack's history has rate limits. |
| (b) `fleet up` holds the connection | One resident process already exists. | The manager gains a network service. A listener stall stalls reconciliation. |
| (c) A separate listener hands events to a manager verb | The manager stays bounded. The listener holds nothing durable. | One more process for the operator to run. |

Recommendation: **(c)**, for every platform.

- The listener is transport only. It holds the connection, authenticates
  the platform, and pipes each event into `fleet gateway receive`.
- Platform parsing stays in Fleet's adapter, with its tests.
- The first listener is for Slack Socket Mode. It is a small program
  maintained outside the workspace, and the operator's service manager runs
  it. Any program that runs the verb per event is a listener.
- Fleet supervises no listener but observes each one. A listener reports
  heartbeats, and the account's Listener turns `InboundReady` False when the
  listener stops, stalls or runs an old configuration.

## Scope

In scope:

- accounts, people and routes in native config,
- admission: the allowlist, the mention rule, and one polite refusal,
- the Conversation mirror, with numbered messages and retention,
- one Task per thread, created from a route,
- relaying a thread into its Task's inbox,
- posting what a Task declares, in order,
- posting every Task's Drafts, result and change news in one log channel,
- opening a thread for a Question from an untied Task, and recording
  answers,
- routing a child Task's words to its parent,
- the Slack and Discord adapters, the receive verb and the Listener,
- marking a relayed message with a working reaction, and with a replied
  reaction once Fleet posted in the thread.

Out of scope, deliberately:

- merging, as for the Forge controller,
- suspending and resuming idle sessions,
- any judgement about the worth of a message: an intake model, a planner or
  a nudging heartbeat,
- implementing the Telegram or Teams adapters,
- reading people's reactions, files, edits of Fleet's own posts, and
  interactive components,
- the listener programs themselves.

## Open questions and recommendations

| Question | Recommendation |
| --- | --- |
| Where does the earlier history go when a mention lands partway through a thread? | In the first inbox entry, not in the brief. The brief stays the route's brief, the same for every conversation Task. |
| What brief and workspace does a conversation Task get? | The route's. TaskConfig holds execution defaults only. So a route names a TaskConfig, and carries the brief and the workspace itself. |
| How can a route run several conversations at once? | Every route can. A `dir` route gives each thread its own directory below the route's base directory. A `repositories` route has the runtime materialize a workspace for each Task, for example as thurbox worktrees, and the runtime cleans it up when the session is deleted. |
| Who deletes a thread's directory? | Fleet, when the thread retires, and only a directory that the manager created. The Conversation records that the manager created it. A directory that existed before stays, as do a route's base directory and anything else in it. |
| Who deletes a conversation Task? | ConversationController, when its thread retires: only a Concluded Task, idle for `gateway.retainMs`. A person or a conductor can also delete it at any time, as for any Task. The thread then becomes Orphaned. |
| How long is a Conversation kept? | Until its thread retires: `gateway.retainMs`, seven days by default, after the later of its last message and its orphaning, once its Task is Concluded or gone. A thread with a running Task, or one that waits for its own directory, is kept. |
| How large can a conversation Task's declarations grow? | `status.declared` has no cap today. Recommendation: a store cap per Task, past which the store refuses with `NeedsAttention/DeclaredFull`. |
| Why does a Comment carry an `id`? | `fleet declare` treats an identical object as a no-op. So two Comments that say "Done." need two ids to be two posts. |
| How is the questions route chosen per Task? | By the label `gateway.fleet.dev/route`, else the `[gateway]` default. Plugins already mark Tasks with labels. |
| How is an answer recognised? | The first allowlisted message written after the Question, by the platform's clock. The Gateway does not judge whether it really answers. A worker that needs more asks a new Question. |
| Does a child's result also go to the log channel? | Yes. The log channel is the record of every Task, a Task that another Task started included. The parent's inbox entry is input for the parent's work, not a record, and the parent does not log it again. |
| Why is the log channel no route's channel? | So a reply under a log post is never admitted, relayed or answered. The receive verb ignores a channel that no route covers. |
| Why are reactions best effort? | A reaction only shows progress. The relay and the post are what matter to the person, so a reaction never holds them back. A reaction is set, not counted, so a repeat is harmless and needs no marker. |
| What if a platform cannot find Fleet's posts again? | Fleet never repeats a post whose outcome is unknown there. It raises `NeedsAttention/PostUnconfirmed`, and later posts to that thread wait for a person. |

## Implementation steps

Each step is its own change. Each has its own E2E suite against the built
CLI, a fake gateway and a stub `curl`. Each keeps `cargo xtask check` green
and adds its crate edges, field managers and negative fixtures to
`crates/architecture.toml`.

| Step | What | Where | Evidence |
| --- | --- | --- | --- |
| 1 | The Gateway port; `fleet-gateway` with the fake adapter; the Conversation and Listener kinds; ConversationController with numbering and retention; ListenerController; `fleet gateway receive` and `--beat`; `fleet gateway account` and `status`; `[gateway]` config | `fleet-api/src/gateway.rs`, `crates/fleet-gateway`, `fleet-controllers/src/conversation.rs`, `fleet-manager`, `fleet-cli` | `conversation_controller`, `e2e_gateway`, `store_contract`, `architecture` |
| 2 | Admission; routes; Task creation with the workspace precondition; the refusal post | `fleet-controllers/src/conversation.rs`, `fleet-api/src/gateway.rs` | `conversation_controller`, `e2e_gateway` |
| 3 | GatewayController: the tie, the join and the relay; `status.answered` and the `answers` inbox field; the parent labels | `fleet-controllers/src/gateway.rs`, `fleet-store` | `gateway_controller`, `e2e_gateway_controller`, `store_contract` |
| 4 | Posting declarations, results and change news; opening a thread for a Question; children | `fleet-controllers/src/gateway.rs`, `fleet-manager` | `gateway_controller`, `e2e_gateway_post` |
| 5 | The Slack adapter through `curl` | `crates/fleet-gateway/src/slack.rs` | Truth tables in the adapter's unit tests; recorded Socket Mode payloads; `e2e_gateway` against the stub `curl` |
| 6 | The Discord adapter through `curl`; `inbound = "discord-gateway"`, `messageContent` and `intents`; nonce recovery in ConversationController | `crates/fleet-gateway/src/discord.rs`, `fleet-api/src/gateway.rs`, `fleet-controllers/src/conversation.rs` | Truth tables in the adapter's unit tests; synthetic dispatches; `conversation_controller`; `e2e_gateway_discord` against the stub `curl` |
| 7 | The log channel: `gateway.logs`; the `Log` action; a top-level `post` to a channel address; the port's `link`; Drafts, results and change news of every Task leave the thread, which keeps one ready notice per change | `fleet-api/src/gateway.rs`, `fleet-api/src/plan.rs`, `fleet-controllers/src/gateway.rs`, `fleet-manager`, `crates/fleet-gateway` | `gateway_controller`, `e2e_gateway_logs`, `e2e_gateway_post` |
| 8 | One directory per thread, and one end of life: a route's `dir` is a base directory; each thread's Tasks get `<dir>/<conversation name>`, which the manager creates through the runtime's `create_dir` and records; `WorkspaceBusy` only for a Task that holds the thread's own directory; an idle thread with a Concluded Task or none retires its Task, its recorded directory and its Conversation; `gateway.retainMs` defaults to seven days | `fleet-api/src/conversation.rs`, `fleet-api/src/runtime.rs`, `fleet-api/src/store.rs`, `fleet-controllers/src/conversation.rs`, `fleet-manager`, `fleet-runtime`, `fleet-store` | `conversation_controller`, `store_contract`, `e2e_gateway` |
| 9 | Reactions: the port's `react` and the `reactions` capability; `[gateway] reactions`; the `React` action; a relayed admitted message marked working, and marked replied once a thread post lands; Slack's `reactions.add` and `reactions.remove`, Discord's own-reaction endpoints, the fake's record | `fleet-api/src/gateway.rs`, `fleet-api/src/talk.rs`, `fleet-controllers/src/gateway.rs`, `fleet-manager`, `crates/fleet-gateway` | `gateway_controller`, `e2e_gateway_reactions`, `e2e_gateway`, `e2e_gateway_discord` |
