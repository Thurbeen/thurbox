# Gateway adapters

An adapter maps one chat platform onto the
[Gateway port](../interface.md#gateway-port). This page holds the rules that
every adapter keeps, and compares the platforms. Each implemented platform has
its own page:

| Platform | Page | Code |
| --- | --- | --- |
| `fake` | None. A test double, described in its module. | `crates/fleet-gateway/src/fake.rs` |
| `slack` | [Slack adapter](slack.md) | `crates/fleet-gateway/src/slack.rs` |
| `discord` | [Discord adapter](discord.md) | `crates/fleet-gateway/src/discord.rs` |

`fleet version` lists the platforms of a build: `discord`, `fake` and
`slack`. Telegram and Microsoft Teams are mapped on paper only, under the
[cross-platform check](#cross-platform-check).

An adapter's `post` carries ConversationController's refusals and busy
notices, GatewayController's posts for a tied Task, and its top-level log
posts in the `gateway.logs` channel. An adapter's `open` carries a Question
that opens a thread. An adapter's `react` adds and removes Fleet's queued,
working and replied reactions on a person's message. An adapter's `link` names a
thread in a log post. The [Gateway controller](../README.md) lists what is
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

The [interface](../interface.md) and the
[resource](../../../resources/conversation.md) already hold each correction:

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

A new platform adds one page, `adapters/<platform>.md`. The page has these H2
headings, in this order, and no other H2:

1. `## Setup`: what the operator does on the platform, and what the listener
   does.
2. `## Configuration`: the `[gateway.accounts.<name>]` keys, and their errors.
3. `## Address format`: the thread and channel addresses, and how an event
   maps to one.
4. `## Capabilities`: the `capabilities` values, and how each port verb maps
   onto the platform's API.
5. `## Limits`: the platform's limits, and every claim that its documentation
   does not state.
6. `## Troubleshooting`: each symptom, its cause and its fix.

The same change also does these things:

- It adds a column to [capabilities](#capabilities).
- It adds a row to [address format](#address-format).
- It moves the platform's column in the
  [cross-platform check](#cross-platform-check) from "On paper" to
  "Implemented".
- It adds the platform to the account row in the
  [native config](../../../configuration/fleet-toml.md#the-gateway-section).

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
