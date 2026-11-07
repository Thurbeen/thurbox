# Gateway controller

Implemented. The installed binary has steps 1 to 12 below: the `[gateway]`
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
Task's result and change news in the log channel. It marks an admitted
request with a working reaction when admission starts its Task, or with a
queued reaction while the request waits. It marks each message it relays
with a working reaction, and with a replied reaction once Fleet posted in
the thread. A wildcard route covers every channel that no other route
lists. A DM route covers every one-to-one DM with the bot, where a
top-level message needs no mention. The listener programs stay outside
Fleet.

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

The Gateway is the [Forge controller](forge.md) applied to chat.
A Task declares what it wants said. A mirror object follows the thread.
Fleet relays new messages into the inbox under a cursor, and recognises its
own posts by a marker. One adapter per platform sits behind one port. Slack
and Discord have adapters. Telegram and Microsoft Teams are mapped on paper,
to check the model.

| Read | Answers |
| --- | --- |
| [Conversation](../resources/conversation.md) | What does the mirror of one thread hold? Who deletes it? |
| [Listener](../resources/listener.md) | Is an account's listener running, connected and current? |
| [Task gateway status](../resources/task.md#talking-in-a-conversation) | What does a Task declare to talk? What do `status.gateway` and `status.answered` hold? |
| [State machine](#gateway-controller-state-machine) | Which states does a thread and a tie go through? What is relayed or posted, and when? |
| [Interface](#gateway-controller-interface) | What are the port, the CLI verbs, the declarations, the inbox entries and the keys? |
| [Adapters](gateway-adapters.md) | Which rules does every adapter keep? How do the platforms compare? |
| [Slack adapter](gateway-adapters.md#slack-adapter) | How is a Slack account set up, configured and mapped onto the port? |
| [Discord adapter](gateway-adapters.md#discord-adapter) | How is a Discord account set up, configured and mapped onto the port? |
| [Native config](../configuration.md#the-gateway-section) | Where do accounts, people, routes and admission come from? |
| [Overall architecture](../architecture.md#the-gateway-controller) | Where does this fit in Fleet? |

## The parts

| Part | Role |
| --- | --- |
| Listener | A small program outside Fleet. It holds the platform connection and hands each event to `fleet gateway receive`. One per account. |
| `fleet gateway receive` | A bounded manager verb. It reads one event and adds it to the thread's Conversation. |
| [Conversation](../resources/conversation.md) | The mirror of one thread. It numbers the messages and records which Task the thread is tied to. |
| ConversationController | Keeps each Conversation current, starts a Task for an admitted mention, and retires old Conversations. |
| GatewayController | A second controller over each Task. It relays the thread into the inbox and posts what the Task declares. |
| [Listener resource](../resources/listener.md) and ListenerController | Show whether an account's listener is running, connected and on the current configuration. |
| Gateway port | The platform API behind one interface: read events, read history, open a thread, post, react. |

## Flow: a mention starts a Task

1. A person on a route's allowlist mentions Fleet's bot in a channel that
   the route covers. The thread can be new or old. See
   [Flow: which route covers a channel](#flow-which-route-covers-a-channel).
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

## Flow: which route covers a channel

The receive verb finds the route of each event's channel on the event's
account:

1. A route of the account lists the channel id. That route covers the
   channel.
2. Else, the channel is the `gateway.logs` channel. No route covers it.
3. Else, the adapter says the channel is a one-to-one DM, and the account
   has a DM route, with `"dm:*"` in `channels`. The DM route covers the
   channel by its DM entry.
4. Else, the account has a wildcard route, with `"*"` in `channels`. The
   wildcard route covers the channel.
5. Else, no route covers the channel. The verb ignores the event.

Rules:

- An explicit channel id always wins over the wildcard. The order of the
  routes in the file does not matter.
- An account has at most one wildcard route.
- The wildcard covers only conversations that the account's listener
  delivers. Each [adapter page](gateway-adapters.md) says which ones.
- Admission applies on a wildcard route as on any route: `allow`,
  `allowBots`, the mention rule and the refusal.
- A wildcard route needs `mention = "required"`.
- A thread that a wildcard route covers keeps that route. A later route
  that lists its channel id changes the route of new threads only.

Rules for the DM entry:

- An explicit channel id always wins over the DM entry.
- An account has at most one DM route.
- A group DM is no one-to-one DM. The wildcard covers it under the mention
  rule.
- The Conversation of a thread that the DM entry covers records
  `spec.direct`. In that thread, a message that starts the thread, or
  mentions Fleet, starts a Task. A stranger's top-level message there earns
  the refusal.
- An edit or a removal that does not say it is in a DM still reaches a
  thread whose Conversation records `spec.direct`.
- A thread keeps its `spec.direct`. A later config without `"dm:*"`
  changes the rule of new threads only.
- On Slack, each top-level DM message starts a new thread, so it starts a
  new Task. A person continues a Task by replying in its thread.

The cost rule holds on a wildcard route. In a thread that Fleet does not
mirror, only a message that can start a Task, earn a refusal, or is Fleet's
own keyed post creates a Conversation. Other chatter, in any channel, costs
no object.

## Flow: reactions show progress

1. Admission accepts a message that starts a Task. The trigger is that
   message.
2. Fleet creates the Task. The create has one of two results:
   - The Task is created. Fleet adds the working reaction, 👀 by default, to
     the trigger.
   - The Task waits, because another Task holds the thread's directory.
     Fleet adds the queued reaction, ⏳ by default, to the trigger.
3. While the Task waits, each later admitted message also gets the queued
   reaction.
4. When the Task is created after a wait, each message with the queued
   reaction gets the working reaction. Fleet removes its queued reaction.
5. GatewayController relays admitted messages into the tied Task's inbox:
   the join, or a later relay. Fleet adds the working reaction to each
   admitted message in that relay that has no reaction yet.
6. The Task works, and declares a `Comment` or a `Question`, or a change it
   follows becomes ready for review.
7. Fleet posts in the thread for the Task.
8. Each message in the thread that still has the working reaction gets the
   replied reaction, ✅ by default. Fleet removes its working reaction.

Rules:

- Only an `Admitted` message gets a reaction. A `Context` message and
  Fleet's own posts get none.
- Fleet adds the working reaction to a message once. The join adds no
  working reaction where admission's working reaction landed.
- Admission tries its working reaction once, when the Task is created.
  When that reaction fails, or the capabilities are Unknown then, the join
  adds the working reaction instead. Reason: a late working reaction from
  admission could land after the Task replied, and stay.
- Only a wait for the thread's directory (`WorkspaceBusy`) is queued. A
  create that is retried after a refusal, or whose outcome Fleet looks for,
  adds no reaction.
- Only the queued reaction shows that a request waits. The busy notice is
  no reply, so a message with the queued reaction keeps it until its Task
  is created.
- A wait can end without a Task: the trigger leaves the mirror. Fleet then
  removes the queued reaction from each message that still has it.
- A message with the queued reaction that leaves the mirror, removed or
  rotated out, loses the queued reaction.
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
2. GatewayController opens a new thread in the first channel id of the
   questions route. The wildcard and the DM entry are no channel id. A
   route without a channel id cannot take Questions. The first post is the
   Question, and it carries a marker that names the Task.
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
  route's channel, and a wildcard route never covers it. So the receive
  verb ignores every event there.
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

- accounts, people and routes in native config, a wildcard route
  included,
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
- marking an admitted request with a working reaction, or a queued one
  while it waits for its Task,
- marking a relayed message with a working reaction, and with a replied
  reaction once Fleet posted in the thread.

The Task controller pauses a conversation Task's idle session when the
Task's TaskConfig sets `pauseAfterMs`. The next message for the Task resumes
it. See [pausing an idle session](task.md#pausing-an-idle-session).

Out of scope, deliberately:

- merging, as for the Forge controller,
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
| Why is the log channel no route's channel? | So a reply under a log post is never admitted, relayed or answered. The receive verb ignores a channel that no route covers. A wildcard route skips the log channel for the same reason. |
| How does a route cover every channel? | With `channels = ["*"]`. The wildcard is an entry of the existing list, so a route keeps one key for what it covers, and an explicit list stays as it is. No platform's channel id is `*`. |
| Which route wins when a channel id is listed and a wildcard exists? | The route that lists the id. A wildcard is the fallback for its account. An account has one wildcard route at most, so the fallback is never ambiguous. |
| Where does a wildcard route open a thread for a Question? | In the first channel id it lists. A route with only `"*"` has none, so `gateway.questions` refuses it. A Task whose route label names such a route raises `NeedsAttention/RouteUnknown`. No separate setting is needed. |
| How can every allowlisted person DM the bot? | With `"dm:*"` in a route's `channels`. Like `"*"`, it is an entry of the existing list, so a route keeps one key for what it covers. No DM id is listed, so a new person needs only an `allow` entry. No platform's channel id holds `:`. |
| Why does a DM need no mention? | A one-to-one DM already addresses the bot, and people rarely type a mention there. The admission rule is the same: only `allow` and `allowBots` start a Task. |
| Why is a group DM no DM? | A group DM includes other people, as a channel does. So the wildcard covers it, and a request there needs a mention. |
| Why does the Conversation record `direct`? | Admission reads the rule from the Conversation, not from each event. A history read does not say whether its chat is a DM, and some edit events do not either. |
| Can a wildcard route use `mention = "top-level"`? | No. On every channel, it would make every top-level post of an allowlisted person a Task. |
| Does a wildcard route change the listener digest or the Listener conditions? | No. The digest covers the account's own settings, not its routes. `Accepted` asks only whether some route uses the account, and a wildcard route does. |
| Where does a mark live before the Task exists? | On the Conversation, under `status.reactions`. ConversationController owns admission, so it owns the queued mark and the first working mark. GatewayController owns every mark from the join on. |
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
| 10 | Reactions at admission: the trigger marked working when its Task is created, or queued while it waits for the thread's directory; a waiting request's later admitted messages marked queued; queued replaced by working when the Task is created; `[gateway] reactions` `queued`; the Conversation's `status.reactions` and its `React` effects; the join takes those marks without a second reaction | `fleet-api/src/conversation.rs`, `fleet-api/src/gateway.rs`, `fleet-api/src/talk.rs`, `fleet-controllers/src/conversation.rs`, `fleet-controllers/src/gateway.rs`, `fleet-manager` | `conversation_controller`, `gateway_controller`, `e2e_gateway_reactions` |
| 11 | The wildcard route: `"*"` in `channels`; an explicit channel id wins; the log channel stays uncovered; one wildcard route per account, with `mention = "required"`; a questions route needs a channel id | `fleet-api/src/gateway.rs`, `fleet-controllers/src/gateway.rs` | `fleet-api` unit tests, `gateway_controller`, `e2e_gateway_wildcard` |
| 12 | The DM route: `"dm:*"` in `channels`; the port's `direct` on a delivery, Slack's `channel_type` `im`; the Conversation's `spec.direct`; a top-level DM starts a Task without a mention, and a stranger's earns the refusal; an explicit channel id wins; one DM route per account; never on Discord | `fleet-api/src/gateway.rs`, `fleet-api/src/conversation.rs`, `fleet-controllers/src/conversation.rs`, `fleet-manager`, `crates/fleet-gateway` | `fleet-api` unit tests, Slack truth tables, `conversation_controller`, `e2e_gateway_dm` |

## Gateway controller state machine

Two things change state: a [Conversation](../resources/conversation.md),
which ConversationController owns, and the tie on each Task, which
GatewayController keeps under `status.gateway`. TaskController still owns
the Task's own `status.phase`. The Conversation phases, the transitions
that create a Task, orphaning and retention are implemented, and so are
every state of the tie on a Task, its `ConversationGone`, the join, the
relay, posting, logging and reactions.

![A Conversation starts Untied. The receive verb creates it, or Fleet creates it when it opens a thread for a Task's Question. A thread that Fleet opened is tied at once to the Task that its opening post's marker names. An admitted mention in an Untied thread creates a Task and ties it. When a tied Task is readably gone, the Conversation becomes Orphaned. The next admitted message there creates a new Task with the thread's history. When a Conversation has no message for its retention and is Untied, Orphaned, or tied to a Concluded Task, it retires: its controller deletes the Task, the manager removes the thread directory that it created, and the controller deletes the Conversation. While the Conversation is Tied, GatewayController relays new messages into the Task's inbox and posts what the Task declares.](gateway.svg)

### Conversation phases

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
  the Question's Task exists. The [interface](#conversationcontroller)
  gives the limits.

### The tie on a Task

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

### Joining

A newly tied Task joins at the beginning of the thread. Its first inbox
entry holds the retained messages, so a mention partway through a thread
brings its history. If the mirror already dropped older messages, the entry
says that they remain on the platform. A Task tied by its own Question has
nothing before its own post.

### Relay

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

### Posting

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

### Logging

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

### Reactions

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

### Children

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

### Conditions

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
[Listener](../resources/listener.md), not on any Conversation.

`NeedsAttention` stops nothing else. Relay goes on while a post waits, and a
held post in one thread holds back no other thread. A reaction raises no
condition.

### Deletion

Deleting a conversation Task deletes nothing else. Its Conversation becomes
Orphaned and is kept. Deleting a Conversation changes nothing on the
platform. Its Task, if any, raises `ConversationGone` and keeps running.
Neither controller adds a finalizer.

A retirement deletes the thread's Task, its directory and its
Conversation, in that order. A person who deletes a Task or a Conversation
by hand deletes only that object. The thread's directory then stays until
the thread retires. A Conversation that a person deletes takes its record
of the directory with it, so Fleet does not remove that directory.

## Gateway controller interface

This page defines the Gateway's APIs: the port that adapters implement, the
CLI verbs that listeners and people run, what a Task declares and receives,
and who may write what. The port, the addresses and admission are
implemented in `fleet_api::gateway`; the three verbs, the Conversation and
Listener kinds, ConversationController, ListenerController,
GatewayController's tie, join, relay, posting, logging and reactions, its
`Open`, `Receive`, `Post`, `Log` and `React` actions, `answers`, the parent
labels and `Probe::List` are implemented. It follows the shape of the
[Forge controller interface](forge.md#forge-controller-interface). The
[state machine](#gateway-controller-state-machine) defines when each thing happens.

### Addresses

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

### Gateway port

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
[adapters](gateway-adapters.md#what-the-platforms-changed) say why each
capability exists.

#### Events

| Event | Fields |
| --- | --- |
| `Message` | `id` (unique per conversation), `conversation`, `author` (`id`, `name`, `kind`: `person`, `bot` or `fleet`), `mentionsFleet`, `at`, `text`, `key` (the marker on a Fleet post), `startsThread` (true when the payload says the message starts its thread, as a Discord thread's starter or a Telegram topic's first message) |
| `Edited` | `id`, `conversation`, `text`, `editedAt` |
| `Removed` | `id`, `conversation` |

A mention is a `Message` with `mentionsFleet`, not an event of its own. A
newer edit replaces the text. An older edit delivered late changes nothing.

#### Deliveries

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

### The receive verb

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

#### Listener heartbeats

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
[Listener](../resources/listener.md) if the manager has not yet. The verb
also records on the Listener every envelope it refuses or cannot read (exit
1 or 4), counted by reason. An envelope for an account the config lacks is
refused without a record, since there is no Listener; `fleet gateway
status` shows that the account is missing.

#### Other verbs

```sh
fleet gateway account NAME --json   # what a listener needs to connect, with the digest
fleet gateway status                # per account: accepted, resolved, inbound, last delivery
```

### Controllers

All three controllers are pure: they read what the manager observes and
return a plan. The manager owns I/O, persistence and process execution, and
performs every effect and every write. Each controller has the shape of the
[Forge controller](forge.md#forge-controller-interface): `due`, `probes` and `reconcile`.

#### ConversationController

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

It works over each [Conversation](../resources/conversation.md):

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

##### Workspaces

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

The [state machine](#conversation-phases) gives the order
of the retirement's steps.

The config is refused when one route's base directory equals, contains or
lies inside another's. The check does not stop a Task applied or created by
a plugin into a thread's directory later, so no other Task should name a
directory below a route's base directory.

#### ListenerController

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

It works over each account's [Listener](../resources/listener.md).
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

#### GatewayController

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

It is a second controller over each [Task](../resources/task.md). It
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

##### Fields this controller owns

| Field | Meaning |
| --- | --- |
| `conversation` | `{name, uid, state}` of the tied Conversation. `state` is `opening`, `opened`, `waiting`, `tied` or `abandoned`, as the [state machine](#the-tie-on-a-task) defines. |
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

#### Actions

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

#### Changes to Plan and Probe

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

### Declarations the Gateway reads

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

### Inbox entries

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

#### Answers

An inbox append may carry `answers`, a list of Question ids. In the same
write the store records each id in the Task's `status.answered`, as
`{key, from, at}`. The first answer wins.

- A relay carries `answers` when an allowlisted message was written after an
  open Question was posted in that thread.
- A plugin's append and `fleet send --answers ID` can carry it too.
- The store refuses an id that names no declared Question.

### Admission

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

### Effect and recovery keys

Every post and Task creation is a keyed effect. Fleet writes the intent
before it acts and the receipt after. Fleet never repeats a key blindly. It
recovers an unknown outcome by looking for the key on the platform or in the
store. On a platform with `marker = nonce { windowMs }`, it resends the post
under the same key, so with the same nonce, inside the window. The
[adapters](gateway-adapters.md#capabilities) define the rule.

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

### Write ownership

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

#### The parent label

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

#### The route label

`gateway.fleet.dev/route` chooses the route whose first channel id a Task's
Question opens in. It is an ordinary label. A route with no channel id, such
as a route with only `"*"` or `"dm:*"`, raises `NeedsAttention/RouteUnknown`.
