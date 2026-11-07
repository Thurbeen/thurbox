# Discord adapter

The Discord adapter maps one Discord guild onto the
[Gateway port](../interface.md#gateway-port). Its code is
`crates/fleet-gateway/src/discord.rs`. It keeps the
[common rules](README.md#common-rules) of every adapter.
`crates/fleet-cli/tests/e2e_gateway_discord.rs` tests it end to end against a
stub `curl`.

Every Discord claim cites Discord's developer documentation. The docs were read
on 2026-11-01 from the `discord/discord-api-docs` repository at commit
`9d2128b` (2026-10-30). A claim that those pages do not state is marked
**unverified**. [Limits](#limits) lists them.

The listener program is not part of this repository. Fleet ships the adapter
and the account's `intents`, and the operator runs a listener.

## Setup

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

## Configuration

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

### Decisions

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

## Address format

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

## Capabilities

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

### Port mapping

Every call goes to `https://discord.com/api/v10`. Every call carries
`User-Agent: DiscordBot (https://github.com/Thurbeen/fleet, <version>)`, the
documented form ([user
agent](https://docs.discord.com/developers/reference#user-agent)). Fleet sends
no Authorization header itself.

| Port item | Discord |
| --- | --- |
| `whoami` | `GET /users/@me` gives the identity. Then `GET /guilds/{team}` must succeed, else the answer is Unknown. |
| `capabilities` | `GET /applications/@me`, as [above](#capabilities). |
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

### Recovering a post

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

## Limits

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

## Troubleshooting

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
| `NeedsAttention/PostUnconfirmed` | A post stayed Unknown past the nonce window, or after three attempts. | Check the thread, then act as the [Conversation](../../../resources/conversation.md) describes. |
| A post fails with `HTTP 403 50013 Missing Permissions` | The bot lacks a permission on the channel. | Grant the six permissions under [Setup](#setup). |
