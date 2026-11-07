# Gateway adapters

An adapter maps one chat platform onto the
[Gateway port](gateway.md#gateway-port). This page holds the rules that
every adapter keeps, and compares the platforms. Each implemented platform has
its own page:

| Platform | Page | Code |
| --- | --- | --- |
| `fake` | None. A test double, described in its module. | `crates/fleet-gateway/src/fake.rs` |
| `slack` | [Slack adapter](#slack-adapter) | `crates/fleet-gateway/src/slack.rs` |
| `discord` | [Discord adapter](#discord-adapter) | `crates/fleet-gateway/src/discord.rs` |

`fleet version` lists the platforms of a build: `discord`, `fake` and
`slack`. Telegram and Microsoft Teams are mapped on paper only, under the
[cross-platform check](#cross-platform-check).

An adapter's `post` carries ConversationController's refusals and busy
notices, GatewayController's posts for a tied Task, and its top-level log
posts in the `gateway.logs` channel. An adapter's `open` carries a Question
that opens a thread. An adapter's `react` adds and removes Fleet's queued,
working and replied reactions on a person's message. An adapter's `link` names a
thread in a log post. The [Gateway controller](gateway.md) lists what is
built.

## Common rules

Every adapter that reaches a real platform keeps these rules:

- **One bounded `curl` per call.** Each call is one `curl` process through
  `fleet-exec`. Past `gateway.callTimeoutMs`, `fleet-exec` kills the whole
  process tree. One operation makes at most 10 calls. The port's
  `operation_bound` is 10 worst-case calls.
- **Fleet holds no credential.** The token lives in a curl config file that
  the operator owns. The account names the file with `curlConfig`. Fleet
  passes it with `--config` and never reads it. Fleet never constructs an
  Authorization header.
- **No text on a command line.** A request body goes to `curl` on stdin.
- **Event reading is pure.** `event` makes no call. So the receive verb runs it
  without the lease.
- **A long text is split.** The adapter splits a text into consecutive posts
  under derived keys that name the part and the count: `<key>/1/3`,
  `<key>/2/3`, `<key>/3/3`. A split closes a code fence that it cuts, and
  opens it again in the next post. A text that needs more than 10 posts is a
  Known failure. A split that landed partly is Unknown.
- **Fleet's own posts are told by the bot id.** A message whose author is the
  account's `bot` is `fleet`. A Fleet post never mentions Fleet.
- **A refusal is Known; anything unclear is Unknown.** A write that the
  platform refused is `Failed`. A write whose outcome Fleet cannot read is
  Unknown, and Fleet never takes it as absent.
- **A history read is complete or Unknown.** A read pages under the budget of
  calls. When the budget runs out, the answer is Unknown (`Incomplete`), never
  a partial list.
- **A reaction is set, not counted.** Adding a reaction that is there, or
  removing one that is not, is a Known success. A refused reaction is
  `Failed`; one whose outcome Fleet cannot read is Unknown.
- **A throttled read is Unknown with `retryAfter`.** HTTP 429 carries the
  platform's wait time where it gives one. A throttled write is per platform:
  see each platform's page.

## Capabilities

`capabilities` reports what the platform offers on an account. The
controllers decide on these values, never on the platform's name.

| Capability | `fake` | `slack` | `discord` |
| --- | --- | --- | --- |
| `history` | Set by the test | `true` | `true` |
| `marker` | Set by the test | `metadata` | `nonce { windowMs: 60000 }` |
| `unmentioned` | Set by the test | `true` | `messageContent` and the application's message content flag |
| `threads` | Set by the test | `true` | `true` |
| `reactions` | Set by the test, `true` by default | `true` | `true` |
| Reaction names | Recorded as given | Slack's emoji names, colons stripped | A name Fleet maps to Unicode (`eyes`, `white_check_mark`, `hourglass_flowing_sand` and a few more), a Unicode emoji as it is, or a custom emoji as `name:id` |
| `react` | One call, recorded | `reactions.add` or `reactions.remove` | `PUT` or `DELETE` of the bot's own reaction |
| Recovery of an Unknown post | As the marker | One `since` read finds the key | Resend under the same nonce inside the window |
| `inbound` | `connection` | `socket-mode` or `events-api` | `discord-gateway` |
| Text | The port's text | A `markdown` block, with a `mrkdwn` fallback | Markdown, unchanged |
| Post limit | Set by the test | 4,000 characters | 2,000 characters |
| Open a thread | One call | One `chat.postMessage` | A post, then Start Thread from Message |
| Message `key` | Read back with a `metadata` marker | Message metadata | None |

When `unmentioned` is false, a tied thread relays only mentions. Its
Conversation raises `NeedsAttention/MentionsOnly`.

The `marker` decides how Fleet settles a post whose outcome is Unknown:

- `metadata`: Fleet looks for the key in the mirror, then in one `since` read.
- `nonce { windowMs }`: Fleet looks for the key in the mirror. Then it resends
  the post under the same key while both conditions hold:
  - the time is before the first intent's `at` plus `windowMs`,
  - fewer than three attempts were made.
- `none`, or a nonce window that closed: Fleet raises
  `NeedsAttention/PostUnconfirmed` and waits for a person.

## Address format

An address is `<platform>:<chat>[:<thread>]`. Only the adapter reads its parts.

| Platform | Thread | Channel | Parts |
| --- | --- | --- | --- |
| `fake` | `fake:<chat>:<thread>` | `fake:<chat>` | Any text. |
| `slack` | `slack:C0123ABCD:1730000000.000100` | `slack:C0123ABCD` | A channel id (`C`, `G` or `D`, then uppercase letters and digits) and the parent's `ts`. |
| `discord` | `discord:1300000000000000000:1300000000000000001` | `discord:1300000000000000000` | The parent channel's snowflake and the thread's snowflake. A thread started from a message has that message's id. |

A route covers channels by their `<chat>` part. A thread belongs to the
route of its channel. A wildcard route covers every conversation that the
account's listener delivers in a channel no other route lists:

| Platform | A wildcard route covers |
| --- | --- |
| `fake` | Every chat. |
| `slack` | Every public and private channel the bot is a member of, with the message events the setup subscribes to. A group DM only when the operator also subscribes the app to `message.mpim`. A one-to-one DM only when the app subscribes to `message.im` and the account has no DM route. |
| `discord` | Every text and announcement channel the bot can view in the account's guild, and their public threads. Never a DM, a private thread or a forum channel: the adapter ignores them. |

A DM route, with `"dm:*"` in `channels`, covers every one-to-one DM that the
adapter marks `direct` and that no route lists:

| Platform | A delivery is `direct` when |
| --- | --- |
| `fake` | The payload holds `"direct": true`. |
| `slack` | The message event's `channel_type` is `im`. A group DM is `mpim`, and is not `direct`. |
| `discord` | Never. The adapter ignores every DM, so a Discord account refuses `"dm:*"`. |

A thread address names a conversation. A channel address names where Fleet
opens a new thread, or where it posts a top-level log post. A `post` to a
channel address makes every part of its text a top-level post there and
starts no thread. The Slack and Discord adapters refuse a thread address in
`open`.

## Cross-platform check

Telegram and Microsoft Teams are mapped on paper only. The purpose is to check
the port against platforms that differ. Each platform claim cites the
platform's own documentation, read on 2026-10-26. A claim that those pages do
not state is marked **unverified**.

| | Slack | Telegram | Discord | Microsoft Teams |
| --- | --- | --- | --- | --- |
| Status | Implemented | On paper | Implemented | On paper |
| Conversation | A thread: channel and parent `ts`[^slack-threads] | A forum topic: chat and `message_thread_id`. Topics exist in supergroups, and in private chats with topics enabled[^tg-message][^tg-topics]. Otherwise the conversation is the whole chat. `reply_to_message` is one level deep[^tg-message], so reply chains are not threads. | A thread, which is a channel with a `parent_id`[^dc-threads]. A thread started from a message has that message's id[^dc-start]. | A channel reply chain: `conversation.id` ends with `;messageid=<root>`[^ms-channel]. Group chats are not threaded[^ms-concepts]. |
| Mention | `<@U…>` in text[^slack-format] | `mention` and `text_mention` entities[^tg-entity]. With privacy mode on, the documented deliveries are commands, replies to the bot and messages via the bot. A bare mention is not listed (**unverified**)[^tg-privacy]. | `mentions` array, `<@id>`[^dc-message][^dc-format] | A `mention` entity whose `mentioned.id` is the bot[^ms-rsc] |
| Unmentioned messages | Delivered with the history scopes[^slack-message] | Only with privacy mode off, or with the bot an admin[^tg-privacy] | Content only with the privileged `MESSAGE_CONTENT` intent. Mentions and DMs carry content without it[^dc-intent]. | Only with the RSC permission `ChannelMessage.Read.Group`. By default, only mentions[^ms-channel][^ms-rsc]. |
| Transport | Socket Mode or HTTPS events[^slack-socket][^slack-events] | `getUpdates` long poll: a higher `offset` confirms an update, and updates are kept 24 hours. Or a webhook, which excludes polling[^tg-updates]. | The Gateway WebSocket. Resume replays lost events; an expired session does not[^dc-gateway]. HTTP webhook events carry no channel messages[^dc-webhooks]. | HTTPS POST to one messaging endpoint, JWT-signed. A retry comes after 15 seconds, so duplicates can occur[^ms-auth][^ms-concepts]. |
| History | `conversations.replies`[^slack-replies] | No method for bots. None was found (**unverified** by absence). | `GET /channels/{id}/messages` with `after`, 100 per page[^dc-messages] | Microsoft Graph `…/messages/{id}/replies`, under `ChannelMessage.Read.Group` or `.All`[^ms-graph] |
| Own posts | Bot id and message metadata[^slack-metadata] | `from.id` equals `getMe`'s id[^tg-user]. `sendMessage` has no metadata field[^tg-send]. | `author.id`. A `nonce` with `enforce_nonce` returns the existing message for a repeat "in the past few minutes"[^dc-create]. | `28:<appId>`; Graph `from.application`[^ms-graph-from]. No idempotency key is documented (**unverified**). |
| Allowlist identity | User id, per workspace, or global in Enterprise[^slack-user][^slack-ent] | Numeric `User.id`[^tg-user] | User snowflake. Usernames change[^dc-user]. | `aadObjectId`, unique to the user. The `29:` id is per bot[^ms-proactive]. |
| Open a thread | One `chat.postMessage` | `createForumTopic`, then `sendMessage`[^tg-forum] | A message, then Start Thread from Message[^dc-start] | Create a conversation in the channel. The app must be installed, and the call needs the `serviceUrl` of an earlier inbound activity[^ms-proactive]. |
| Limits | 4,000 recommended, 40,000 truncated[^slack-post] | 4,096 characters. One message a second per chat, 20 a minute per group[^tg-send][^tg-limits]. | 2,000 characters[^dc-create] | About 100 KB; aim for 80 KB[^ms-format]. 7 sends a second per conversation[^ms-rate]. |
| Breaks the model | Nothing. Events may be lost while disconnected. | No history and no marker, so an Unknown send can never be recovered. Mid-chat mentions see only what Fleet received. Bot-to-bot messages arrive when opted in[^tg-b2b]. | Opening is two calls. Recovery depends on a short nonce window. | Unmentioned replies never arrive without RSC. No marker. The Bot Framework SDK is archived in favour of the Teams SDK, which keeps the HTTPS endpoint[^ms-sdk][^ms-teams-sdk]. |

## What the platforms changed

The [interface](gateway.md#gateway-controller-interface) and the
[resource](../resources/conversation.md) already hold each correction:

1. **A thread is optional.**
   - Telegram chats without topics, direct messages and Teams group chats
     have no threads.
   - The address is `platform:chat[:thread]`. A chat with no threads is
     one conversation.
   - A route over such a chat ties the whole chat to one Task.
2. **"Every message is a turn" depends on delivery.**
   - Slack delivers every message in a channel that the bot is in.
   - Telegram, Discord and Teams deliver unmentioned messages only under
     a platform permission.
   - The adapter's `capabilities` reports whether unmentioned messages
     reach Fleet on an account.
   - Without them, a tied thread relays only mentions. Its Conversation
     raises `NeedsAttention/MentionsOnly` rather than pretending.
3. **Recognising Fleet's posts is a capability, not a given.**
   - Slack has a metadata marker. Discord has a nonce window. Telegram
     and Teams have nothing.
   - `capabilities` reports `marker` as `metadata`, as `nonce` with its
     window, or as `none`.
   - With `none`, an Unknown post outcome raises
     `NeedsAttention/PostUnconfirmed` and is never repeated. The
     conversation's later posts wait for a person.
4. **History is a capability.**
   - Slack, Discord and Teams (under a separate Graph permission) can
     read a thread. Telegram cannot.
   - With no history, a mention partway through a chat carries only what
     the receive verb spooled. The join message says so.
   - The backstop poll exists only where history does.
5. **Opening a thread is not one call everywhere.**
   - On Discord and Telegram, opening takes two calls. The adapter keys
     both steps from the one intent key.
   - On Discord, the thread's id is the starter message's id. So a
     repeated second step finds the thread already there.
6. **Identity is an opaque id in a scope.**
   - No allowlist entry is a display name, a username or an email.
   - Reason: Discord and Telegram usernames change, and Teams' per-bot
     ids are not the person's.
   - `[gateway.people]` maps a person's name to one id per account.
7. **Fleet's own bot is not the only bot.**
   - Telegram now delivers other bots' messages when a bot opts
     in[^tg-b2b].
   - Every adapter filters on Fleet's own id from `whoami`.
   - Other bots are `bot` authors. Admission ignores them unless a route
     lists them.

## Adding a platform

A new platform adds one section to this page, `## <Platform> adapter`. The
section has these H3 headings, in this order, and no other H3:

1. `### Setup`: what the operator does on the platform, and what the listener
   does.
2. `### Configuration`: the `[gateway.accounts.<name>]` keys, and their errors.
3. `### Address format`: the thread and channel addresses, and how an event
   maps to one.
4. `### Capabilities`: the `capabilities` values, and how each port verb maps
   onto the platform's API.
5. `### Limits`: the platform's limits, and every claim that its documentation
   does not state.
6. `### Troubleshooting`: each symptom, its cause and its fix.

The same change also does these things:

- It adds a column to [capabilities](#capabilities).
- It adds a row to [address format](#address-format).
- It moves the platform's column in the
  [cross-platform check](#cross-platform-check) from "On paper" to
  "Implemented".
- It adds the platform to the account row in the
  [native config](../configuration.md#the-gateway-section).

[^slack-threads]: <https://docs.slack.dev/messaging/retrieving-messages>
[^tg-message]: <https://core.telegram.org/bots/api#message>
[^tg-topics]: <https://core.telegram.org/bots/features#topics-in-private-chats>
[^dc-threads]: <https://docs.discord.com/developers/topics/threads>
[^dc-start]: <https://docs.discord.com/developers/resources/channel#start-thread-from-message>
[^ms-channel]: <https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/channel-and-group-conversations>
[^ms-concepts]: <https://learn.microsoft.com/en-us/microsoftteams/platform/bots/bot-concepts>
[^slack-format]: <https://docs.slack.dev/messaging/formatting-message-text>
[^tg-entity]: <https://core.telegram.org/bots/api#messageentity>
[^tg-privacy]: <https://core.telegram.org/bots/features#privacy-mode>
[^dc-message]: <https://docs.discord.com/developers/resources/message#message-object>
[^dc-format]: <https://docs.discord.com/developers/reference#message-formatting>
[^ms-rsc]: <https://learn.microsoft.com/en-us/microsoftteams/platform/agents-in-teams/enable-receive-all-chat-messages>
[^slack-message]: <https://docs.slack.dev/reference/events/message>
[^dc-intent]: <https://docs.discord.com/developers/events/gateway#message-content-intent>
[^slack-socket]: <https://docs.slack.dev/apis/events-api/using-socket-mode>
[^slack-events]: <https://docs.slack.dev/apis/events-api/>
[^tg-updates]: <https://core.telegram.org/bots/api#getting-updates>
[^dc-gateway]: <https://docs.discord.com/developers/events/gateway#resuming>
[^dc-webhooks]: <https://docs.discord.com/developers/events/webhook-events#event-types>
[^ms-auth]: <https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication?view=azure-bot-service-4.0>
[^slack-replies]: <https://docs.slack.dev/reference/methods/conversations.replies>
[^dc-messages]: <https://docs.discord.com/developers/resources/message#get-channel-messages>
[^ms-graph]: <https://learn.microsoft.com/en-us/graph/api/chatmessage-list-replies?view=graph-rest-1.0>
[^slack-metadata]: <https://docs.slack.dev/messaging/message-metadata>
[^tg-user]: <https://core.telegram.org/bots/api#user>
[^tg-send]: <https://core.telegram.org/bots/api#sendmessage>
[^dc-create]: <https://docs.discord.com/developers/resources/message#create-message>
[^ms-graph-from]: <https://learn.microsoft.com/en-us/graph/api/resources/chatmessagefromidentityset?view=graph-rest-1.0>
[^slack-user]: <https://docs.slack.dev/reference/objects/user-object/>
[^slack-ent]: <https://docs.slack.dev/enterprise/developing-for-enterprise-orgs/>
[^dc-user]: <https://docs.discord.com/developers/resources/user#user-object>
[^ms-proactive]: <https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/send-proactive-messages>
[^tg-forum]: <https://core.telegram.org/bots/api#createforumtopic>
[^slack-post]: <https://docs.slack.dev/reference/methods/chat.postMessage>
[^tg-limits]: <https://core.telegram.org/bots/faq#my-bot-is-hitting-limits-how-do-i-avoid-this>
[^ms-format]: <https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/format-your-bot-messages>
[^ms-rate]: <https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/rate-limit>
[^tg-b2b]: <https://core.telegram.org/bots/features#bot-to-bot-communication>
[^ms-sdk]: <https://learn.microsoft.com/en-us/azure/bot-service/index-bf-sdk?view=azure-bot-service-4.0>
[^ms-teams-sdk]: <https://learn.microsoft.com/en-us/microsoftteams/platform/teams-sdk/why>

## Slack adapter

The Slack adapter maps one Slack workspace onto the
[Gateway port](gateway.md#gateway-port). Its code is
`crates/fleet-gateway/src/slack.rs`. It keeps the
[common rules](#common-rules) of every adapter.

Every Slack claim cites Slack's own documentation, read on 2026-10-26. The
DM claims were read on 2026-11-04. A claim
that those pages do not state is marked **unverified**.

### Setup

On the Slack side, the operator does these steps:

1. Create an app for the one workspace. Keep it an internal app.
   Reason: since 2025-05-29, some commercially distributed apps get Tier 1
   history limits on new installs. Internal customer-built apps are not
   affected[^slack-limits2025].
2. Give the bot user the history scopes `channels:history` and
   `groups:history`[^slack-message], and `message_metadata:read`. History
   reads need the last scope to return Fleet's marker[^slack-metadata].
   Add `reactions:write`, so Fleet can mark a person's message queued,
   working and replied[^slack-react-add][^slack-react-remove]. Without it, every
   reaction fails, and relays and posts go on.
3. Subscribe the app to the `message.channels` and `message.groups`
   events[^slack-message].
4. Choose the listener's transport, and set `inbound` to match:
   - `socket-mode`: the listener holds an app-level token with
     `connections:write`. `apps.connections.open` gives it a temporary `wss://`
     URL[^slack-socket].
   - `events-api`: the listener serves a public HTTPS endpoint, and verifies
     each request's signature[^slack-events].
5. Put the bot token in a curl config file that the operator owns, as an
   Authorization header. Name the file in `curlConfig`.
6. Invite the bot to every channel that a route lists. Message events and
   history reach the bot only in a channel where it is a member. On an
   account with a wildcard route, membership is the allowlist of channels:
   the wildcard covers every channel the bot is in.
7. For one-to-one DMs, also do the steps under
   [The DM route](#the-dm-route).

The listener is a program outside Fleet. It does these things:

1. It reads its account from `fleet gateway account NAME --json`.
2. It holds the Socket Mode connection, or serves the Events API endpoint.
3. It runs `fleet gateway receive --account NAME` once for each envelope, with
   the raw Socket Mode envelope or Events API body as the payload.
4. It acknowledges the envelope by its `envelope_id` only after the verb exits
   0. The acknowledgement is due within three seconds, per Slack's Java SDK
   guide[^slack-java].
5. It beats with `--beat` on every state change, and at least every
   `gateway.beatMs`.

### Configuration

```toml
[gateway.accounts.slack-main]
platform = "slack"
team = "T0123ABCD"            # the team id, or the enterprise id in an Enterprise org
bot = "U0BOT0001"             # Fleet's bot user id, checked against auth.test
inbound = "socket-mode"       # or "events-api"
cli = "curl"
curlConfig = "/etc/fleet/slack-curl.conf"   # operator-owned; holds the bot token header
```

| Key | Required/default | Meaning |
| --- | --- | --- |
| `platform` | Required | `slack`. |
| `team` | Optional | The `team_id`, or the `enterprise_id` in an Enterprise org. Slack advises keying stored data on the `enterprise_id`[^slack-ent]. When it is set, `whoami` checks it, and events of another workspace are ignored. |
| `bot` | Required | Fleet's bot user id. `whoami` compares it with the `user_id` of `auth.test`. |
| `inbound` | Required | `socket-mode`, an authenticated connection, or `events-api`, a signed HTTPS delivery. |
| `cli` | `curl` | The client executable, a path or a name on `PATH`. |
| `curlConfig` | Required | An absolute path. Fleet passes it to `curl --config` and never reads it. |

A Slack account refuses `messageContent`. The manager refuses an account that
breaks these rules, with one of these errors:

- `inbound "X" is not how a Slack listener delivers; use "socket-mode" or
  "events-api"`
- `a slack account needs a curlConfig`
- `messageContent is a Discord account's key, not "slack"'s`

#### The wildcard route

A wildcard route, `channels = ["*"]`, covers these Slack conversations:

- Every public channel (`message.channels`) and private channel
  (`message.groups`) where the bot is a member, apart from a channel id that
  another route lists and the `gateway.logs` channel.
- A group DM, only when the app also subscribes to `message.mpim`. A
  request there needs a mention, as in a channel.
- A one-to-one DM, only when the app also subscribes to `message.im` and
  the account has no DM route. A request there needs a mention.

The setup above subscribes to neither, so by default the wildcard covers
no DM. A subscription is the operator's choice of which conversations
reach Fleet.

Recommendation: to take requests in one-to-one DMs, add a DM route. Do not
rely on the wildcard there. Reason: under the wildcard, a DM request must
mention the bot, which people rarely do.

#### The DM route

A DM route has `"dm:*"` in `channels`. It covers every one-to-one DM
between the bot and a user of the workspace, apart from a channel id that a
route lists. In such a DM:

- A top-level message from a person on `allow` starts a Task. It needs no
  mention.
- A reply in a tied DM thread is a turn for that thread's Task. A new
  top-level message is a new thread, and it starts a new Task. To continue
  a Task, a person replies in its thread.
- A top-level message from anyone else earns the route's refusal, once per
  thread. It never becomes an instruction.

The adapter marks a delivery `direct` when the message event's
`channel_type` is `im`. Slack documents `channel_type` as the way to tell a
DM from other message events[^slack-message-im]. A group DM has
`channel_type` `mpim`[^slack-message-mpim], so it is not `direct`. The
adapter does not read the channel id prefix. Reason: the documented group
DM example has a `G…` id, the prefix of a private channel too.

`message_changed` and `message_deleted` events may lack `channel_type`:
Slack's example shows none (**unverified** in general). The receive verb
still routes such an edit to its DM thread from the Conversation's
`spec.direct`.

To take DMs on an account, the operator makes these Slack app changes:

1. Add the bot scope `im:history`. It reads DM history, including through
   `conversations.replies`[^slack-message-im][^slack-replies].
2. Subscribe the app to the `message.im` event[^slack-message-im].
3. In App Home, switch on the **Messages Tab**[^slack-app-home]. Slack's
   App Home page names `chat:write` and `im:history` for it. A checkbox that
   lets users send messages from that tab may also need to be on
   (**unverified**: the page does not name it).
4. Reinstall the app to the workspace, so that the new scope applies.
5. In the Fleet config, add `"dm:*"` to the route's `channels`. Keep every
   person who may DM the bot under that route's `allow`.

Group DMs need `message.mpim` and `mpim:history`[^slack-message-mpim]. They
are optional, and the wildcard covers them under the mention rule.

### Address format

A Slack conversation is a thread. Its address is
`slack:<channel>:<parent ts>`, for example `slack:C0123ABCD:1730000000.000100`.
A channel address, `slack:<channel>`, names where Fleet opens a thread.

- A channel id starts with `C`, `G` or `D`, then uppercase letters and digits.
- A `ts` is `<seconds>.<fraction>`. It is unique within a
  channel[^slack-threads].
- A message is in a thread when it has `thread_ts`. The parent is the message
  where `thread_ts == ts`[^slack-threads].
- A message without `thread_ts` starts its own thread. Its address uses its own
  `ts`.
- A reply is posted with the parent's `ts` as `thread_ts`, never with a reply's
  `ts`[^slack-post].

### Capabilities

| Capability | Value | Why |
| --- | --- | --- |
| `history` | `true` | `conversations.replies` reads a thread. |
| `marker` | `metadata` | Every post carries the intent key as message metadata. |
| `unmentioned` | `true` | Slack delivers every message in a channel the bot is in. |
| `threads` | `true` | Every conversation is a thread. |
| `reactions` | `true` | `reactions.add` and `reactions.remove` add and remove the bot's own reaction. |

The values are the same on every account. `capabilities` calls `auth.test`
first, so that a failing account answers Unknown.

#### Port mapping

| Port item | Slack |
| --- | --- |
| `whoami` | `auth.test`. Its `user_id` is the identity. When `team` is set, the answer's `team_id` or `enterprise_id` must equal it. |
| `event` | Reads a Socket Mode envelope (`type: events_api`) or an Events API body (`type: event_callback`). Message events become `Message`, `message_changed` becomes `Edited`, and `message_deleted` becomes `Removed`. A delivery is `direct` when the event's `channel_type` is `im`[^slack-message-im]. |
| Delivery id | The event wrapper's `event_id`, which is globally unique[^slack-events]. |
| Ignored events | Other envelope types, events that are not messages, other workspaces, and message subtypes other than none, `file_share`, `thread_broadcast` and `bot_message`. |
| Mention | `<@U…>` or `<@U…\|name>` with Fleet's bot user id in `text`[^slack-format]. The adapter does not rely on `app_mention`. Reason: `app_mention` needs `app_mentions.read`, and the reference does not state how it behaves inside threads (**unverified**)[^slack-mention]. |
| Authors | The account's `bot` is `fleet`. A message with a `bot_id`, or of subtype `bot_message`, is a `bot`. Anyone else is a `person`. |
| `history` | `conversations.replies` for a thread, `conversations.history` for a channel, both with `include_all_metadata`[^slack-replies]. Pages of 200 follow `next_cursor`. `has_more` without a cursor is Unknown (`Incomplete`). |
| `starter` | The parent message. A `tombstone` parent is `removed`. |
| `post` | `chat.postMessage` with `thread_ts`, one `markdown` block, a `mrkdwn` `text`, and `unfurl_links` and `unfurl_media` off[^slack-post]. |
| `open` | `chat.postMessage` without `thread_ts`. The new thread's address is the channel and the returned `ts`. Later chunks of a split text reply to it. |
| `post` to a channel address | `chat.postMessage` without `thread_ts`, for every chunk: a log post starts no thread. A `since` read of the channel uses `conversations.history`. |
| `react` | `reactions.add` to add, `reactions.remove` to remove, with `channel`, `timestamp` (the message's `ts`) and `name`[^slack-react-add][^slack-react-remove]. `name` is Slack's emoji name, such as `eyes` or `white_check_mark`; surrounding colons are stripped. Both methods list JSON and form bodies as accepted. The adapter sends form arguments, as it does for every method other than `chat.*`. `already_reacted` and `no_reaction` mean the reaction already stands as asked: a Known success. |
| `link` | None. A Slack permalink needs the workspace's domain, which only a `chat.getPermalink` call gives, so a log post names a Slack thread by its address. |
| Marker | `{"event_type": "fleet_post", "event_payload": {"key": "<intent key>"}}`[^slack-post]. History reads it back[^slack-metadata]. |
| Idempotency | None on `chat.postMessage`: `client_msg_id` is not an argument[^slack-post]. Recovery uses the marker in a later `since` read. |
| Identity | User ids, unique in their workspace[^slack-user]. In an Enterprise org, every user has one global id that starts with `U` or `W`[^slack-ent]. |

Each post carries its text twice:

- `blocks` holds one `markdown` block with the Markdown as written. Slack
  renders standard Markdown in this block, tables included[^slack-markdown].
- `text` holds the same Markdown converted to `mrkdwn`, Slack's own
  syntax[^slack-format]. Slack shows `text` in notifications. History reads
  `text` back.

In the `markdown` block, the adapter escapes with a backslash each `<` that
starts `<@`, `<!` or `<#` outside code. So a post makes no mention, no
broadcast and no channel link. Slack's reference does not state that a
`markdown` block parses these forms (**unverified**).

In `text`, the adapter escapes `&`, `<` and `>`[^slack-format]. Code spans
and fences stay code. A heading becomes one bold line. A table becomes a
code block, because `mrkdwn` has no table.

A Slack answer maps onto the port as follows:

| Answer | Port |
| --- | --- |
| `ok: true` | Known. |
| HTTP 429, or the error `ratelimited` | Unknown (`Throttled`), with `Retry-After` as `retryAfter`. |
| `ok: false` with `internal_error` or `fatal_error` | Unknown. Slack says some aspect of the operation may have succeeded. |
| `ok: false` with any other error | Refused: a Known failure for a write. |
| An unreadable body, or no `ok` field | Unknown. |

### Limits

- The adapter splits a text at 4,000 characters, Slack's recommended length.
  Slack truncates text over 40,000[^slack-post]. So a text that needs more
  than 10 posts is refused, never cut.
- Slack allows 12,000 characters across the `markdown` blocks of one
  payload[^slack-markdown]. One post holds one chunk of 4,000 at most.
- Slack allows about one message a second per channel[^slack-post].
- `conversations.replies` and `conversations.history` are Tier 3: 50 or more
  calls a minute[^slack-replies].
- Message metadata is visible to every member of the
  workspace[^slack-post]. So a key names a Task and its `uid`, never a
  secret.
- While the listener is disconnected, the app "may lose events". Slack
  refreshes connections every few hours[^slack-socket].
- In a tied thread, the history backstop recovers a lost message. A new
  thread's first mention during an outage is lost, and the person mentions
  again.
- Socket Mode apps are not allowed in the public Marketplace[^slack-compare].
  This does not matter for an internal app.
- The Events API needs a 2xx within three seconds. It retries up to three
  times: at once, after one minute and after five[^slack-events].

These points are **unverified**:

- The Socket Mode page gives no acknowledgement limit. The three seconds come
  from the Java SDK guide[^slack-java].
- The retry counts for Socket Mode[^slack-socket].
- Whether `message.channels` delivers without channel membership.
- Whether the app receives its own posts as events. Bolt's `ignoreSelf`
  implies that it does[^slack-ignoreself]. So the adapter reads them as
  `fleet` authors.
- Whether `message_changed` and `message_deleted` events carry
  `channel_type`. The receive verb does not need it for a DM thread that
  Fleet already mirrors.
- Whether App Home needs a separate checkbox, besides the Messages Tab, so
  that users can send messages to the app.
- Whether `im:history` alone, without `mpim:history`, lets
  `conversations.replies` read a one-to-one DM. The method lists all four
  history scopes[^slack-replies].

### Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| A DM to the bot starts no Task, and the receive verb logs nothing | The app does not subscribe to `message.im`, or App Home's Messages Tab is off. | Do the steps under [The DM route](#the-dm-route). |
| A DM to the bot is ignored as chatter | No route of the account holds `"dm:*"`, so the wildcard covers the DM under the mention rule. | Add `"dm:*"` to the route's `channels`. |
| The manager refuses the config: `inbound "X" is not how a Slack listener delivers` | `inbound` names another platform's method. | Set `socket-mode` or `events-api`. |
| The manager refuses the config: `a slack account needs a curlConfig` | The account has no `curlConfig`. | Name the operator's curl config file. |
| `Resolved` is Unknown: `auth.test answered for team …, not the account's …` | The token belongs to another workspace, or `team` is wrong. | Fix `team`, or the token in the curl config. |
| `Resolved` is False with `IdentityMismatch` | `bot` is not the token's bot user. | Set `bot` to the `user_id` that `auth.test` returns. |
| `Resolved` is Unknown: `auth.test: invalid_auth` or a similar error | The curl config holds no valid bot token. | Fix the token in the curl config. |
| `InboundReady` is True, but no message arrives | The bot is not in the channel, or the app has no message subscriptions. | Invite the bot. Subscribe to `message.channels` and `message.groups`. |
| A post fails with `chat.postMessage: not_in_channel` | The bot is not a member of the channel. | Invite the bot to the channel. |
| A message gets no ⏳, 👀 or ✅, and the manager's report names `reactions.add: missing_scope` | The app lacks `reactions:write`. | Add the scope, and reinstall the app. |
| `NeedsAttention/PostUnconfirmed` | A post stayed Unknown, and no recovery read found its key. | Check the thread, then act as the [Conversation](../resources/conversation.md) describes. |

[^slack-limits2025]: <https://docs.slack.dev/changelog/2025/05/29/rate-limit-changes-for-non-marketplace-apps>
[^slack-react-add]: <https://docs.slack.dev/reference/methods/reactions.add>
[^slack-react-remove]: <https://docs.slack.dev/reference/methods/reactions.remove>
[^slack-java]: <https://docs.slack.dev/tools/java-slack-sdk/guides/socket-mode/>
[^slack-message-im]: <https://docs.slack.dev/reference/events/message.im>
[^slack-message-mpim]: <https://docs.slack.dev/reference/events/message.mpim>
[^slack-app-home]: <https://docs.slack.dev/surfaces/app-home>
[^slack-mention]: <https://docs.slack.dev/reference/events/app_mention>
[^slack-markdown]: <https://docs.slack.dev/reference/block-kit/blocks/markdown-block>
[^slack-compare]: <https://docs.slack.dev/apis/events-api/comparing-http-socket-mode>
[^slack-ignoreself]: <https://docs.slack.dev/tools/bolt-js/reference/variables/ignoreself>

## Discord adapter

The Discord adapter maps one Discord guild onto the
[Gateway port](gateway.md#gateway-port). Its code is
`crates/fleet-gateway/src/discord.rs`. It keeps the
[common rules](#common-rules) of every adapter.
`crates/fleet-cli/tests/e2e_gateway_discord.rs` tests it end to end against a
stub `curl`.

Every Discord claim cites Discord's developer documentation. The docs were read
on 2026-11-01 from the `discord/discord-api-docs` repository at commit
`9d2128b` (2026-10-30). A claim that those pages do not state is marked
**unverified**. [Limits](#limits-1) lists them.

The listener program is not part of this repository. Fleet ships the adapter
and the account's `intents`, and the operator runs a listener.

### Setup

On the Discord side, the operator does these steps:

1. Create an application in the Developer Portal, and add a bot user.
2. Copy the bot token into two places only:
   - a curl config file that the operator owns, as
     `header = "Authorization: Bot <token>"`. The header form is
     `Authorization: TOKEN_TYPE TOKEN`
     ([authentication](https://docs.discord.com/developers/reference#authentication)).
   - the listener's own secret store, for example a service-manager
     credential.
3. Enable the Message Content privileged intent only when `messageContent =
   true`. At 100 guilds or more, the intent needs verification ([message
   content
   intent](https://docs.discord.com/developers/events/gateway#message-content-intent)).
   One guild needs none.
4. Invite the bot to the one guild with the `bot` scope ([bot authorization
   flow](https://docs.discord.com/developers/topics/oauth2#bot-authorization-flow)).
   Grant the six permissions below on the route's channels
   ([permissions](https://docs.discord.com/developers/topics/permissions#permissions-bitwise-permission-flags)).
5. Enable Developer Mode in the Discord client. Copy the guild id, the bot user
   id and the channel ids into `fleet.toml`.

| Permission | Bit | Why |
| --- | --- | --- |
| `VIEW_CHANNEL` | `1 << 10` | See the route's channels and their threads. |
| `SEND_MESSAGES` | `1 << 11` | Post the opening message in a channel. It "does not allow sending messages in threads". |
| `SEND_MESSAGES_IN_THREADS` | `1 << 38` | Post in threads. |
| `CREATE_PUBLIC_THREADS` | `1 << 35` | Start a thread from a message. |
| `READ_MESSAGE_HISTORY` | `1 << 16` | Read history. Without it, "no messages will be returned". Adding a reaction also needs it. |
| `ADD_REACTIONS` | `1 << 6` | Add the queued, working and replied reactions. Discord asks for it only when nobody has reacted with that emoji yet ([create reaction](https://docs.discord.com/developers/resources/message#create-reaction)). |

Fleet never passes the token to the listener. The listener never reads Fleet's
curl config. No token appears in `fleet.toml`, in an envelope, in a heartbeat,
in a record or in a log.

The listener is a program outside Fleet. It does these things:

1. It reads its account from `fleet gateway account NAME --json`, including
   `intents`.
2. It connects to the Gateway and Identifies with the bot token and those
   `intents` ([list of
   intents](https://docs.discord.com/developers/events/gateway#list-of-intents)).
3. It keeps a map from thread id to parent channel id. It fills the map from
   `GUILD_CREATE`, `THREAD_CREATE`, `THREAD_UPDATE` and `THREAD_LIST_SYNC`.
4. For each `MESSAGE_CREATE`, `MESSAGE_UPDATE` and `MESSAGE_DELETE` dispatch,
   it runs `fleet gateway receive --account NAME` with one envelope. The
   envelope claims `verified = "connection"`. The payload adds `parentId` to
   every envelope: the thread's parent channel for a message in a thread, and
   `null` for a message in a channel.
5. It beats with `--beat` on every state change, and at least every
   `gateway.beatMs`.
6. It persists `session_id`, `resume_gateway_url` and the sequence number of
   the last event that the verb accepted (exit 0). After a reconnect, it
   resumes from that sequence number. Discord then replays the missed events
   ([resuming](https://docs.discord.com/developers/events/gateway#resuming)).
7. On exit 1, 4 or 5, it retries the same envelope with backoff, and holds the
   later events.
8. On close code 4004 or 4014, it beats `closing` and stops. Neither code may
   reconnect ([close
   codes](https://docs.discord.com/developers/topics/opcodes-and-status-codes#gateway-gateway-close-event-codes)).

```json
{"verified": "connection", "receivedAt": 1730000300100,
 "payload": {"dispatch": {"op": 0, "t": "MESSAGE_CREATE", "s": 42, "d": {"…": "…"}},
             "parentId": "1300000000000000000"}}
```

### Configuration

```toml
[gateway.accounts.discord-main]
platform = "discord"
team = "1200000000000000000"     # the guild id
bot = "1210000000000000000"      # the bot user's snowflake
inbound = "discord-gateway"      # the listener holds the Gateway WebSocket
messageContent = false           # true only when the application holds MESSAGE_CONTENT
cli = "curl"
curlConfig = "/etc/fleet/discord-curl.conf"   # operator-owned; holds the Authorization header
```

| Key | Required/default | Meaning |
| --- | --- | --- |
| `platform` | Required | `discord`. |
| `team` | Required | The guild id, a snowflake. |
| `bot` | Required | The bot user's snowflake. `whoami` compares it with `GET /users/@me`. |
| `inbound` | Required | `discord-gateway`: the listener holds an authenticated Gateway WebSocket, so an envelope claims `verified = "connection"`. |
| `messageContent` | `false` | `true` when the application holds the privileged `MESSAGE_CONTENT` intent. Only a Discord account accepts the key. |
| `cli` | `curl` | The client executable, a path or a name on `PATH`. |
| `curlConfig` | Required | An absolute path. Fleet passes it to `curl --config` and never reads it. |

A route's `channels` are text or announcement channels. A thread belongs to the
route of its parent channel. A wildcard route, `channels = ["*"]`, covers
every text and announcement channel that the bot can view in the guild, and
their public threads, apart from a channel that another route lists and the
`gateway.logs` channel. On such an account, the `VIEW_CHANNEL` grants are the
allowlist of channels. A Discord account refuses the DM entry, `"dm:*"`.
Reason: the adapter ignores every DM. A person's id under `[gateway.people]`
is a user snowflake, never a username.

`fleet gateway account NAME --json` prints `intents` for a Discord account:

| `messageContent` | Intents | Value |
| --- | --- | --- |
| `false` | `GUILDS` (`1 << 0`), `GUILD_MESSAGES` (`1 << 9`) | `513` |
| `true` | The above, and `MESSAGE_CONTENT` (`1 << 15`) | `33281` |

The account's digest covers `intents`. So a change of `messageContent` makes a
running listener `ListenerConfigOld`. A Slack or fake account prints no
`intents`.

The manager refuses an account that breaks these rules, with one of these
errors:

- `inbound "X" is not how a Discord listener delivers; use "discord-gateway"`
- `a discord account needs a team: its guild id`
- `a discord account's team and bot are snowflakes`
- `a discord account needs a curlConfig`
- `the Discord adapter reads no direct message; "dm:*" needs a slack or fake
  account`, on a route of the account that lists `"dm:*"`
- `messageContent is a Discord account's key, not "slack"'s`, on an account of
  another platform

#### Decisions

- **Text and announcement channels, and public threads, only.** Forum
  channels, private threads and DMs are out of scope. Reason: forum thread
  creation takes no `nonce` ([start thread in
  forum](https://docs.discord.com/developers/resources/channel#start-thread-in-forum-or-media-channel)).
  So an Unknown forum open could never be recovered.
- **`messageContent` defaults to `false`.** Mentions-only is the safe default,
  and the Conversation says so with `MentionsOnly`.
- **The listener adds `parentId`.** `event` is pure, so it cannot look up a
  thread's parent. The listener already sees every thread event.
- **Give Fleet its own application.** The nonce check is "by the same author".
  So another program that posts as the bot could match Fleet's nonces.

### Address format

A Discord conversation is a thread. Its address is
`discord:<parent channel>:<thread>`. Both parts are snowflakes. A channel
address, `discord:<channel>`, names where Fleet opens a thread.

- A thread is a channel of type 10, 11 or 12, and its `parent_id` is the
  channel it lives in
  ([threads](https://docs.discord.com/developers/topics/threads#thread-fields)).
  A text channel's `parent_id` is its category, so the listener maps thread
  ids only.
- A thread started from a message has that message's id: "The id of the
  created thread will be the same as the id of the source message"
  ([start thread from
  message](https://docs.discord.com/developers/resources/channel#start-thread-from-message)).
- A top-level message `M` in channel `C` is the conversation `discord:C:M`. The
  address exists before the thread does. Fleet starts the thread on its first
  post there.
- A message in a thread `T` of channel `C` is the conversation `discord:C:T`.
  `C` comes from the envelope's `parentId`.

### Capabilities

| Capability | Value | Why |
| --- | --- | --- |
| `history` | `true` | `GET /channels/{id}/messages` reads a thread. |
| `marker` | `nonce { windowMs: 60000 }` | The only documented duplicate check is the nonce window. |
| `unmentioned` | `messageContent`, and the application holds the intent | `GET /applications/@me` returns `flags`. `GATEWAY_MESSAGE_CONTENT` is `1 << 18`, and `GATEWAY_MESSAGE_CONTENT_LIMITED` is `1 << 19` ([application flags](https://docs.discord.com/developers/resources/application#application-object-application-flags)). |
| `threads` | `true` | Every conversation is a thread. |
| `reactions` | `true` | Create and delete own reaction add and remove the bot's reaction. |

When `unmentioned` is false, the Conversation shows
`NeedsAttention/MentionsOnly`. In a tied thread, a person then mentions the bot
on each turn.

#### Port mapping

Every call goes to `https://discord.com/api/v10`. Every call carries
`User-Agent: DiscordBot (https://github.com/Thurbeen/fleet, <version>)`, the
documented form ([user
agent](https://docs.discord.com/developers/reference#user-agent)). Fleet sends
no Authorization header itself.

| Port item | Discord |
| --- | --- |
| `whoami` | `GET /users/@me` gives the identity. Then `GET /guilds/{team}` must succeed, else the answer is Unknown. |
| `capabilities` | `GET /applications/@me`, as [above](#capabilities-2). |
| `event` | Reads the envelope's `dispatch` with op `0`: `MESSAGE_CREATE` becomes `Message`, `MESSAGE_UPDATE` becomes `Edited`, and `MESSAGE_DELETE` becomes `Removed` ([gateway events](https://docs.discord.com/developers/events/gateway-events#message-create)). |
| Delivery id | `create/<id>`, `update/<id>/<edited_timestamp>` or `delete/<id>`. The Gateway has no event id, so the adapter derives one. A replayed event gets the same id. |
| Ignored events | Other dispatches; another guild; DMs (no `guild_id`); private threads (`channel_type` 12); message types other than `0` (DEFAULT) and `19` (REPLY), such as `21` starter references; a `MESSAGE_UPDATE` without `edited_timestamp` or without `content`. |
| Unknown events | A payload without a `dispatch` object; a message without `id` or `channel_id`; an envelope without the `parentId` key; a public thread's message (`channel_type` 10 or 11) whose `parentId` is `null`. Reason: an edit or a removal carries no channel type, so only the listener can say whether its channel is a thread. |
| Mention | The bot's user id in the `mentions` array ([message object](https://docs.discord.com/developers/resources/message#message-object-message-structure)). A role mention is not a mention of Fleet. |
| Authors | The account's `bot` is `fleet`. Another message with `author.bot` or a `webhook_id` is a `bot`. Anyone else is a `person`. The name is `global_name`, else `username`. |
| Message time | The snowflake's time: `(snowflake >> 22) + 1420070400000` ([snowflakes](https://docs.discord.com/developers/reference#snowflakes)). |
| `startsThread` | `true` for a top-level message, `false` in a thread. |
| Message `key` | None. Fleet recognises its posts by author id. |
| `history` | Reads the starter with `GET /channels/{C}/messages/{T}`. Then reads `GET /channels/{T}/messages?limit=100`, under a budget of 10 calls. A first read pages backward with `before` until it holds the newest `limit` messages; its `leftOut` counts only the older messages it happened to read, so it is a lower bound. Every other read pages forward with `after` from its bound. The starter is one of the messages. A `since` read starts after the snowflake for that time. |
| `starter` | Error 10008 Unknown Message on the starter read makes it `removed`. |
| `post` | `POST /channels/{T}/messages` ([create message](https://docs.discord.com/developers/resources/message#create-message-json/form-params)). On error 10003 Unknown Channel, the thread is not started yet. The adapter starts it with `POST /channels/{C}/messages/{T}/threads`, then sends again. Error 160004 (a thread already exists) counts as started. |
| `open` | Posts the first chunk in `C`, then starts a thread from that post. The other chunks go into the thread. The answer is `Opened { conversation: discord:C:M }`. |
| `post` to a channel address | `POST /channels/{C}/messages` for every chunk. It starts no thread. |
| `react` | `PUT /channels/{X}/messages/{M}/reactions/{emoji}/@me` to add, `DELETE` of the same path to remove ([create reaction](https://docs.discord.com/developers/resources/message#create-reaction), [delete own reaction](https://docs.discord.com/developers/resources/message#delete-own-reaction)). `X` is the thread `T`, or the parent channel `C` for the thread's starter message `M = T`. `emoji` is URL-encoded, as Discord requires. The adapter maps a name such as `eyes`, `white_check_mark` or `hourglass_flowing_sand` to its Unicode emoji, takes a Unicode emoji as it is, and takes a custom emoji as `name:id`. Another name is a Known failure, and no call is made. Both endpoints answer 204 with no body. |
| `link` | `https://discord.com/channels/{guild}/{T}`, with the account's `team` as the guild: the link Discord copies for a channel or thread. |
| Identity | User snowflakes. A `username` is "not unique across the platform" ([user object](https://docs.discord.com/developers/resources/user#user-object-user-structure)). |

A post's body has these fields:

- `content`: Fleet's Markdown, unchanged. Discord renders Markdown itself.
- `nonce`: the first 25 hex characters of `sha256(<intent key>)`. Every resend
  of one key carries the same nonce.
- `enforce_nonce: true`.
- `allowed_mentions: {"parse": []}`. So a post mentions nobody ([allowed
  mentions](https://docs.discord.com/developers/resources/message#allowed-mentions-object)).
- `flags: 4` (`SUPPRESS_EMBEDS`). So Fleet's links never unfurl.

A new thread's name is the text's first non-empty line, at most 100 characters.
A text with no such line names the thread `Fleet`.

A Discord answer maps onto the port as follows:

| Answer | Port |
| --- | --- |
| 2xx | Known. |
| 429 | Nothing was done. A read is Unknown (`Throttled`), and `retryAfter` is the body's `retry_after` in seconds, else the `Retry-After` header ([rate limits](https://docs.discord.com/developers/topics/rate-limits)). A write is a Known failure, which the controller retries after its delay inside the nonce window (see [Recovering a post](#recovering-a-post)). |
| Another 4xx | Refused: a Known failure for a write. The write was not made. |
| 5xx, or an unreadable body | Unknown. |

#### Recovering a post

`enforce_nonce` makes Discord return the existing message for a repeated nonce
"in the past few minutes"
([create message](https://docs.discord.com/developers/resources/message#create-message-json/form-params)).
ConversationController and GatewayController settle an Unknown post as
follows:

1. It looks for the key in the mirror.
2. It resends the post under the same key while both conditions hold:
   - the time is before the first intent's `at` plus `windowMs`,
   - fewer than three attempts were made.
3. Otherwise it raises `NeedsAttention/PostUnconfirmed` and waits for a person.

A resend keeps the first intent, so the window counts from the first send.
A refused post (a 4xx or a 429) is retried after the controller's delay
under the same rule: inside the first send's window, under the first intent.
Past the window, it raises `NeedsAttention/PostFailed`. Reason: an earlier
send under the key may have landed, and only the nonce window makes a resend
safe. A refused post is not retried in a pass that cannot read the
platform's capabilities.

The controller looks at a post's receipt on the next pass, not at the next
poll. Reason: the window is shorter than the poll interval.

Two cases end with a person, although a later send would be safe:

- A post planned while the thread's Task create is pending or busy keeps
  that create's interval. If its answer is lost, it raises
  `PostUnconfirmed`.
- A post refused twice, such as by two 429 answers, has its second retry
  due past the 60-second window. It raises `PostFailed`.
Fleet never reads history to find a nonce. Reason: a read without
`READ_MESSAGE_HISTORY` is an empty list, so an empty read proves nothing.

### Limits

- `content` holds at most 2,000 characters. The adapter splits at 2,000
  Unicode scalar values, into at most 10 posts. A longer text is a Known
  failure.
- A nonce holds at most 25 characters.
- A history page holds at most 100 messages. A read that needs more than 10
  calls is Unknown (`Incomplete`).
- A message can have only one thread created from it.
- Sending a message unarchives an archived thread, unless the thread is locked
  ([active and archived
  threads](https://docs.discord.com/developers/topics/threads#active-&-archived-threads)).
- A failed resume loses the events of the gap. In a tied thread, the history
  backstop recovers them. A mention in an untied thread is lost, and the person
  mentions again.
- API v10 and v9 are listed as "Available" ([API
  versioning](https://docs.discord.com/developers/reference#api-versioning)).

These points are **unverified**:

- The exact length of the nonce window. Discord says "the past few minutes".
  Fleet trusts 60 seconds.
- The unit of the 2,000 "characters".
- The order and paging of `after` beyond "newest to oldest", limit 1 to 100,
  default 50 ([get channel
  messages](https://docs.discord.com/developers/resources/message#get-channel-messages)).
- The HTTP status of error 160004.
- What create reaction answers when the bot already has the reaction, and
  what delete own reaction answers when it has none. Fleet takes a 2xx as
  success and any 4xx as a refusal, which it tries at most three times.
- Whether a role mention of the bot carries content without
  `MESSAGE_CONTENT`. The documented exceptions are the app's own messages, DMs,
  messages that mention the app, and a context menu command's target.
- How long a Gateway session stays resumable. Discord says "a few minutes".

### Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| The listener closes with 4014 | The listener asked for `MESSAGE_CONTENT`, and the application lacks it in the portal. | Set `messageContent = false`, or enable the Message Content intent in the portal. |
| The listener closes with 4004 | The listener's token is wrong. | Fix the token in the listener's secret store. |
| A message gets no ⏳, 👀 or ✅, and the manager's report names `HTTP 403 50013` | The bot lacks `ADD_REACTIONS` or `READ_MESSAGE_HISTORY` in the channel. | Grant both. Relays and posts go on meanwhile. |
| A message gets no reaction, and the manager's report names `HTTP 400 10014 Unknown Emoji` | A custom emoji in `gateway.reactions` is not in the guild. | Name a Unicode emoji, or a custom emoji of the guild as `name:id`. |
| `NeedsAttention/MentionsOnly` | `messageContent` is `false`, or the application lacks the intent. | Mention the bot on each turn. Or enable the intent in the portal and set `messageContent = true`. |
| `fleet gateway receive` exits 4: `… the envelope has no parentId …` or `… the envelope's parentId is null` | The listener sent no `parentId`, or does not know the thread's parent. | Send `parentId` on every envelope, and fix the listener's thread map. Discord replays the event after a resume. |
| `Resolved` is Unknown: `the bot cannot see guild …` | The bot is not in the guild, or `team` is wrong. | Invite the bot to the guild, or fix `team`. |
| `Resolved` is False with `IdentityMismatch` | `bot` is not the token's bot user. | Set `bot` to the id that `GET /users/@me` returns. |
| `ListenerConfigOld` after a change of `messageContent` | The digest covers `intents`. | Restart the listener, so it Identifies with the new `intents`. |
| `NeedsAttention/PostUnconfirmed` | A post stayed Unknown past the nonce window, or after three attempts. | Check the thread, then act as the [Conversation](../resources/conversation.md) describes. |
| A post fails with `HTTP 403 50013 Missing Permissions` | The bot lacks a permission on the channel. | Grant the six permissions under [Setup](#setup-1). |
