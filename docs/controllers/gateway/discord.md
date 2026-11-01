# Discord gateway adapter

This page is a design. Nothing on it is implemented. Code references are
`path:from-to` at Fleet commit `ab35108`.

## Summary

- Add a `discord` platform to `fleet-gateway`, next to the Slack adapter: REST
  calls through a bounded `curl`, and pure reading of Gateway dispatches.
- A Discord conversation is a thread. Its address is `discord:<parent
  channel>:<thread>`. The thread id equals the id of the message it started
  from.
- A listener outside Fleet holds the Gateway WebSocket and the bot token. It
  adds the thread's parent channel to each envelope, because Discord's message
  event does not carry it.
- Fleet recognises its posts by author id. It recovers an Unknown post by
  resending the same `nonce` with `enforce_nonce` inside a short window. Today
  the controllers recover only with a `metadata` marker, so this needs one
  controller change.
- Without the privileged `MESSAGE_CONTENT` intent, only mentions reach Fleet.
  The adapter then reports `unmentioned = false`, and the Conversation shows
  `NeedsAttention/MentionsOnly`.

## Why

The [adapters page](adapters.md) maps Discord on paper only. This page maps each
gateway port verb onto Discord and cites Discord's own developer documentation.
It is a design. Nothing on it is built.

**Sources.** Each Discord claim links to Discord's developer documentation. The
docs were read on 2026-11-01 from the `discord/discord-api-docs` repository at
commit `9d2128b` (2026-10-30), which is the source of
`docs.discord.com/developers`. A claim that those pages do not state is marked
**unverified**. The design argues from Fleet commit `ab35108` (origin/main on
2026-11-01).

## Proposed interface changes

| Interface | Change | What it lets somebody do | Site today |
| --- | --- | --- | --- |
| [gateway.accounts.<name>] platform = "discord" | added | An operator points a Fleet route at Discord channels in one guild, and the build lists discord among its platforms. | `crates/fleet-gateway/src/lib.rs:37-62` |
| inbound = "discord-gateway" | added | A Discord listener claims deliveries it read from an authenticated Gateway WebSocket; any other claim is refused. | `crates/fleet-api/src/gateway.rs:460-489` |
| [gateway.accounts.<name>] messageContent = true\|false | added | The operator states whether the bot holds the privileged MESSAGE_CONTENT intent; the listener requests it, and Fleet reports MentionsOnly when it is off. | `crates/fleet-api/src/gateway.rs:491-513` |
| fleet gateway account NAME --json (intents field) | changed | A Discord listener reads the exact intents to identify with from Fleet, and an intent change moves the digest so a stale listener shows ListenerConfigOld. | `crates/fleet-gateway/src/lib.rs:247-297` |
| discord:<channel>:<thread> address | changed | A Discord conversation names its parent channel and its thread, so routes match and a thread can be started lazily from a top-level message. | `docs/controllers/gateway/interface.md:17-30` |
| Discord envelope payload {dispatch, parentId} | added | The listener hands Fleet the raw Gateway dispatch plus the thread's parent channel, which Discord's message event does not carry. | `crates/fleet-api/src/gateway.rs:212-234` |
| Marker::Nonce recovery (resend under the same key inside windowMs) | changed | An Unknown Discord post is settled by resending the same nonce inside the window, instead of waiting for a person. | `crates/fleet-controllers/src/conversation.rs:262-280` |

## How the gateway works today

The port is one trait, and each platform is one adapter behind it. The platforms
this build has (`crates/fleet-gateway/src/lib.rs:37-62`) are `fake` and `slack`.
`GatewayAccounts::new` (`crates/fleet-gateway/src/lib.rs:85-127`) builds one
adapter per account and runs the platform's own checks.

The Slack adapter is the reference:

- Each call is one bounded `curl` with the operator's config file
  (`crates/fleet-gateway/src/slack.rs:125-180`). Fleet passes `--config` and
  never reads the file. Fleet never constructs an Authorization header.
- Event reading is pure (`crates/fleet-gateway/src/slack.rs:754-800`). The
  receive verb runs it without the lease.
- Long text is split under derived keys
  (`crates/fleet-gateway/src/slack.rs:340-406`), `<key>/1`, `<key>/2`. A split
  that landed partly is Unknown.
- Slack reports fixed capabilities (`crates/fleet-gateway/src/slack.rs:62-71`):
  history, a `metadata` marker, unmentioned delivery, threads.

The port already names three marker kinds
(`crates/fleet-api/src/gateway.rs:236-260`): `metadata`, `nonce { windowMs }`
and `none`. The interface page states the nonce rule: a retry inside the window
resends the same nonce, and the platform returns the existing post. The fake
already models a nonce window (`crates/fleet-gateway/src/fake.rs:405-417`), and
it reads a key back only for a metadata marker
(`crates/fleet-gateway/src/fake.rs:375-389`).

The controllers do not use the nonce rule yet:

> **Recovery reads only where the marker is metadata**:
> `crates/fleet-controllers/src/conversation.rs:262-280` at `ab35108`.

`settle_posts` (`crates/fleet-controllers/src/conversation.rs:1072-1095`) uses
the same test. So with a `nonce` marker, every Unknown post raises
`PostUnconfirmed` and waits for a person. That rule is safe. But on Discord,
every lost answer would then need a person.

Two more facts constrain the design:

- A Fleet-opened thread is tied by its starter's key
  (`crates/fleet-controllers/src/conversation.rs:1268-1293`). On Discord, that
  key never comes back from the platform. The manager's `Receive` action records
  Fleet's opening post with its full key, so the tie still works. The first
  `startsThread` message becomes the starter
  (`crates/fleet-controllers/src/conversation.rs:657-661`), so the `Receive`
  action must run before the listener delivers that message. See D5.
- An address is platform, chat and an optional thread
  (`crates/fleet-api/src/gateway.rs:38-94`). The documented Discord example is
  `discord:<thread>` (addresses
  (`docs/controllers/gateway/interface.md:17-30`)). That form has no parent
  channel, so route matching and lazy thread creation cannot work. This design
  changes it. The on-paper Discord column
  (`docs/controllers/gateway/adapters.md:42-55`) is the starting point.

## Port mapping

Every row cites Discord's developer documentation. Base URL:
`https://discord.com/api/v10`. v10 is listed as available ([API
versioning](https://docs.discord.com/developers/reference#api-versioning)). The
docs do not name a current version (**unverified**).

| Port item | Discord |
| --- | --- |
| Account | One guild: the account's `team` is the guild id. One bot application. `bot` is the bot user's snowflake. |
| Conversation | A thread. A thread is a channel with a `parent_id` ([threads](https://docs.discord.com/developers/topics/threads#thread-fields)). A thread started from a message has that message's id: "The created thread and the message it was started from will share the same id" ([threads](https://docs.discord.com/developers/topics/threads)). Address: `discord:<parent channel id>:<thread id>`. |
| A top-level mention | A person mentions the bot in a route's channel `C`. The message id is `M`. The conversation is `discord:C:M`. The thread does not exist yet. The adapter starts it on Fleet's first post (see Post). The message `startsThread`. |
| Message ids | Snowflakes. A message's time is `(snowflake >> 22) + 1420070400000` ([snowflakes](https://docs.discord.com/developers/reference#snowflakes)). `at` is the message `timestamp`. |
| Delivery id | The Gateway has no event id. The sequence number `s` belongs to one session. The adapter derives the id: `create/<message id>`, `update/<message id>/<edited_timestamp>`, `delete/<message id>`. The same event replayed after a resume gets the same id. |
| Mention | The message's `mentions` array holds the bot's user id ([message object](https://docs.discord.com/developers/resources/message#message-object-message-structure)). The text form is `<@USER_ID>`. The `<@!USER_ID>` form "is deprecated" ([message formatting](https://docs.discord.com/developers/reference#message-formatting)). A role mention `<@&ROLE_ID>` of the bot's role is not a mention of Fleet. The docs do not state that such a message carries content without `MESSAGE_CONTENT` (**unverified**). `@everyone` is not a mention of Fleet. |
| Inbound | `MESSAGE_CREATE`, `MESSAGE_UPDATE` and `MESSAGE_DELETE` from the Gateway WebSocket ([gateway events](https://docs.discord.com/developers/events/gateway-events#message-create)). `MESSAGE_CREATE` adds `guild_id`, `member` and `channel_type`. It does not carry a thread's parent id (see the listener section). `MESSAGE_UPDATE` is now a full message object. `MESSAGE_DELETE` has `id`, `channel_id` and `guild_id`. HTTP webhook events carry no channel messages ([webhook events](https://docs.discord.com/developers/events/webhook-events#event-types)). So the Gateway is the only inbound transport. |
| Unmentioned messages | Without the privileged `MESSAGE_CONTENT` intent (`1 << 15`), `content`, `embeds`, `attachments` and `components` are empty. Exceptions: the app's own messages, DMs, and messages that mention the app ([message content intent](https://docs.discord.com/developers/events/gateway#message-content-intent)). Replies that ping the bot are also listed as an exception ([you might not need a privileged intent](https://docs.discord.com/developers/gateway/you-might-not-need-a-privileged-intent#exceptions-when-you-get-message-content-without-the-privileged-intent)). |
| History | `GET /channels/{thread}/messages` with `after` or `before`, `limit` 1 to 100, default 50. The results come "from newest to oldest" ([get channel messages](https://docs.discord.com/developers/resources/message#get-channel-messages)). The adapter pages under a budget, reverses to oldest first, and answers Unknown (`Incomplete`) when the budget runs out. A `since` read starts `after` the snowflake for that time. |
| Starter | A thread started from a message holds a type 21 `THREAD_STARTER_MESSAGE`. That message points back to the message in the parent channel and "will never have content" ([thread starter messages](https://docs.discord.com/developers/resources/message#message-reference-content-attribution-thread-starter-messages)). The adapter reads the starter with `GET /channels/{parent}/messages/{thread}` ([get channel message](https://docs.discord.com/developers/resources/message#get-channel-message)). JSON error 10008 "Unknown message" means the starter is `removed`. A thread with no type 21 message was not started from a message: its first message is the starter. |
| Post | `POST /channels/{thread}/messages` with `content`, `nonce`, `enforce_nonce: true`, `allowed_mentions: {"parse": []}` and `flags: SUPPRESS_EMBEDS (1 << 2)` ([create message](https://docs.discord.com/developers/resources/message#create-message-json/form-params)). An empty `parse` list suppresses all mentions ([allowed mentions](https://docs.discord.com/developers/resources/message#allowed-mentions-object)). So a worker's text never pings `@everyone`, a role or a person. If the thread does not exist yet (JSON error 10003 "Unknown channel"), the adapter starts it from message `<thread>` in `<parent>`, then posts again with the same nonce. |
| Open | Two calls, both keyed from the one intent key. 1: `POST /channels/{C}/messages` with the nonce of the key. 2: `POST /channels/{C}/messages/{M}/threads` with a `name` of 1 to 100 characters ([start thread from message](https://docs.discord.com/developers/resources/channel#start-thread-from-message)). "a message can only have a single thread created from it". A repeat answers JSON error 160004 "A thread has already been created for this message" ([JSON error codes](https://docs.discord.com/developers/topics/opcodes-and-status-codes#json-json-error-codes)). The adapter reads 160004 as success, because the thread id is `M`. The HTTP status of 160004 is **unverified**. The answer is `Opened { conversation: discord:C:M, message: M, at }`. |
| Own posts | `author.id` equals the configured `bot`: `AuthorKind::Fleet`. Another message with `author.bot` or a `webhook_id` is a `bot` author. The adapter reports no `key` on a Discord message. The `nonce` is in the create response and in `MESSAGE_CREATE` ("Value will appear in the Message Create event"). The docs do not state that `GET` returns it (**unverified**). |
| Idempotency | `enforce_nonce`: "checked for uniqueness in the past few minutes. If another message was created by the same author with the same nonce, that message will be returned". A nonce is at most 25 characters ([create message](https://docs.discord.com/developers/resources/message#create-message-json/form-params)). The adapter derives the nonce from the intent key: the first 25 hex characters of `sha256(key)`. The window's length in milliseconds is **unverified**. |
| Identity | User snowflakes. `username` is "not unique across the platform", and `global_name` is a display name ([user object](https://docs.discord.com/developers/resources/user#user-object-user-structure)). The allowlist names snowflakes only. |
| Limits | `content` is at most 2,000 characters ([create message](https://docs.discord.com/developers/resources/message#create-message-json/form-params)). The adapter splits at 2,000, under derived keys, at most 10 posts. The unit of a "character" is **unverified**. A refusal (50035 "Invalid form body") is a Known failure. |
| Rate limits | Per route and per bucket. A 429 body has `retry_after` in seconds, as a float. The global limit is 50 requests a second. 401, 403 and 429 answers count toward an invalid-request limit of 10,000 per 10 minutes ([rate limits](https://docs.discord.com/developers/topics/rate-limits)). A 429 is Unknown with `retryAfter`. A 401 or 403 is a Known failure. The three-attempt cap keeps Fleet far below the invalid-request limit. |
| Archived threads | "Sending a message will automatically unarchive the thread, unless the thread has been locked" ([active and archived threads](https://docs.discord.com/developers/topics/threads#active-&-archived-threads)). A locked thread (160005 "Thread is locked") is a Known failure, so `PostFailed`. |
| Text | Discord renders Markdown itself (bold, italic, code, code blocks, quotes, lists, headers and masked links), per a Discord support article, not the developer docs ([Markdown Text 101](https://support.discord.com/hc/en-us/articles/210298617-Markdown-Text-101-Chat-Formatting-Bold-Italic-Underline)). Fleet's subset passes through. Outside code, the adapter escapes `\`, `*`, `_`, `~`, `` ` ``, `|`, and `>` at line start, with a backslash. The docs do not state the escaping rules (**unverified**). Masked links stay links. |

```text
nonce(key)  = hex(sha256(key))[0..25]            # 25 characters, the documented maximum
chunk keys  = key, or key/1, key/2, … when split  # as Slack
thread name = first line of the opening text, trimmed to 100 characters, else "Fleet"
```

## Capabilities

| Capability | Discord value | Why |
| --- | --- | --- |
| `history` | `true` | `GET /channels/{id}/messages`. It needs `VIEW_CHANNEL` and `READ_MESSAGE_HISTORY`. |
| `marker` | `nonce { windowMs: 60000 }` | The only documented duplicate check is the nonce window. 60 s is a conservative guess at "a few minutes" (**unverified**). |
| `unmentioned` | `messageContent` in the config **and** the application holds the intent | `GET /applications/@me` returns `flags` ([get current application](https://docs.discord.com/developers/resources/application#get-current-application)). `GATEWAY_MESSAGE_CONTENT` is `1 << 18`, and `GATEWAY_MESSAGE_CONTENT_LIMITED` (under 100 servers) is `1 << 19` ([application flags](https://docs.discord.com/developers/resources/application#application-object-application-flags)). |
| `threads` | `true` | Every conversation is a thread. |

When `unmentioned` is false, the existing rule
(`crates/fleet-controllers/src/conversation.rs:528-539`) raises
`NeedsAttention/MentionsOnly`. Nothing new is needed in the controller.

A history read without `READ_MESSAGE_HISTORY` gives an empty list, not an error
([get channel
messages](https://docs.discord.com/developers/resources/message#get-channel-messages)).
So an empty answer is not proof of absence. That is one reason the nonce
recovery below never treats an empty read as "not posted".

## Inbound: the listener

### A mention in a route's channel starts a Task

1. person → discord: @Fleet in channel C (message M)
2. discord → listener: dispatch op 0, t MESSAGE_CREATE, s 42
3. listener → receive: envelope, verified connection
4. receive → receive: read: discord:C:M, mentionsFleet, startsThread
5. receive → cc: Conversation created; admission; Task created
6. gc → adapter: post the reply to discord:C:M

The listener is a small program outside the workspace, like Slack's. It is
transport only:

1. It reads its account from `fleet gateway account NAME --json`, including the
   `intents` value Fleet computes.
2. It connects to the Gateway and Identifies with the bot token and those
   intents. `GUILDS` (`1 << 0`) gives thread events and the active threads in
   `GUILD_CREATE`. `GUILD_MESSAGES` (`1 << 9`) gives message events.
   `MESSAGE_CONTENT` (`1 << 15`) is added only when `messageContent = true`
   ([list of
   intents](https://docs.discord.com/developers/events/gateway#list-of-intents)).
3. It keeps a map from thread id to parent channel id. It fills the map from
   `GUILD_CREATE` `threads`, `THREAD_CREATE`, `THREAD_UPDATE` and
   `THREAD_LIST_SYNC`. For a thread it does not know, it calls `GET
   /channels/{id}` itself.
4. For each `MESSAGE_*` dispatch, it runs `fleet gateway receive --account NAME`
   with one envelope. The payload is the raw dispatch and the parent.
5. It beats with `--beat`, as today.

```json
{"verified": "connection", "receivedAt": 1730000300100,
 "payload": {"dispatch": {"op": 0, "t": "MESSAGE_CREATE", "s": 42, "d": {"…": "…"}}, "parentId": "1300000000000000000"}}
```

**Resume and lost events.** The Discord Gateway has no per-event
acknowledgement. On a successful resume, "the Gateway will send the missed
events in order, finishing with a Resumed event"
([resuming](https://docs.discord.com/developers/events/gateway#resuming)). Op 9
Invalid Session with `d = false` means the session cannot resume. The app
disconnects and Identifies again, and the events in the gap are lost ([invalid
session](https://docs.discord.com/developers/events/gateway-events#invalid-session)).
So the listener gets at-least-once delivery as follows:

- It persists `session_id`, `resume_gateway_url` and the sequence number of the
  last event that the receive verb accepted (exit 0). It does not persist the
  last event it saw.
- After a crash or a reconnect, it resumes from that sequence number. Discord
  then replays every event the verb did not accept.
- On exit 1, 4 or 5, it retries the same envelope with backoff, and holds later
  events.
- When resume fails, it Identifies again. The gap is lost. In a tied, active
  thread, the history backstop recovers it. A mention in an untied thread is
  lost, and the person mentions again. This is the Slack rule.
- How long a session stays resumable is **unverified**.

Close codes: 4004 is a wrong token. 4014 is "a disallowed intent", sent when the
application lacks an intent it asked for. Neither may reconnect ([close
codes](https://docs.discord.com/developers/topics/opcodes-and-status-codes#gateway-gateway-close-event-codes)).
The listener beats `closing` and stops, so the Listener shows
`InboundReady=False`.

Identify is limited to 1,000 calls in 24 hours. Sharding is required from 2,500
guilds
([sharding](https://docs.discord.com/developers/events/gateway#sharding)). One
guild per account is far below both limits. Several sessions for the same bot
are allowed
([sharding](https://docs.discord.com/developers/events/gateway#sharding)). Two
listener instances only cause duplicates, and the delivery ids remove those.

**What the adapter ignores.** The adapter returns `None` for these payloads:

- events of another guild (`guild_id` is not the `team`),
- DMs (no `guild_id`),
- type 21 starter references, and system message types,
- a `MESSAGE_UPDATE` with empty `content` when `messageContent` is false.

It answers Unknown for a dispatch without `d.id` or `d.channel_id`, and for a
thread message without `parentId`.

## Recovering a post: the nonce rule

### An Unknown post on Discord

1. gc → adapter: Post under key K: curl timed out
2. gc → gc: receipt Unknown; now - intent.at < windowMs
3. gc → adapter: Post again under K (same nonce)
4. adapter → gc: existing or new message: Posted

The rule, for a post or an open whose outcome is Unknown on an account with
`marker = nonce { windowMs }`:

1. While `now - intent.at < windowMs`, the controller plans the same effect
   again, under the same key, at once. The adapter resends the same nonce.
   Discord returns the existing message or creates it. Either answer is Known.
2. The window counts from the intent, which is written before the first send. So
   the rule is conservative.
3. Past the window, nothing is resent. The effect raises
   `NeedsAttention/PostUnconfirmed`, as for `marker = none` today.
4. A split post resends every chunk under its own derived key. The chunks that
   landed come back as they are.

This changes `recovery_due`
(`crates/fleet-controllers/src/conversation.rs:262-280`) and `settle_posts`
(`crates/fleet-controllers/src/conversation.rs:1072-1095`) in
ConversationController. GatewayController's posting is not built yet. It takes
the same rule when it is built (step 4 of the gateway plan).

The nonce check is "by the same author". So the rule holds only while no other
program posts as the bot with Fleet-like nonces. The operator gives Fleet its
own application.

## Credentials

- **REST.** The bot token lives in a curl config file that the operator owns,
  for example `header = "Authorization: Bot <token>"`. The header form is
  `Authorization: TOKEN_TYPE TOKEN`
  ([authentication](https://docs.discord.com/developers/reference#authentication)).
  Fleet names the file with `--config` from `curlConfig` and never reads it.
  This is the Slack rule (`crates/fleet-gateway/src/slack.rs:125-180`).
- **User-Agent.** Fleet sets `User-Agent: DiscordBot
  (https://github.com/Thurbeen/fleet, <fleet version>)`, the documented form
  ([user agent](https://docs.discord.com/developers/reference#user-agent)). This
  value is not a secret.
- **WebSocket.** Discord uses the same bot token for the Gateway. Slack uses a
  separate app-level token for its socket. The listener holds its own copy of
  the token in its own secret store, for example a service-manager credential.
  Fleet never passes the token to the listener, and the listener never reads
  Fleet's curl config.
- No token appears in `fleet.toml`, in an envelope, in a heartbeat, in a record
  or in a log.

## fleet.toml

```toml
[gateway.accounts.discord-main]
platform = "discord"
team = "1200000000000000000"     # the guild id
bot = "1210000000000000000"      # the bot user's snowflake, checked against GET /users/@me
inbound = "discord-gateway"      # the listener holds the Gateway WebSocket; verified = connection
messageContent = false           # true only when the application holds MESSAGE_CONTENT
cli = "curl"
curlConfig = "/etc/fleet/discord-curl.conf"   # operator-owned; holds the Authorization header

[gateway.people.alex]
discord-main = "1220000000000000000"          # a user snowflake, never a username

[gateway.routes.eng-discord]
account = "discord-main"
channels = ["1230000000000000000"]            # text channels; their threads belong to the route
mention = "required"
allow = ["alex"]
taskConfig = "conversation"
brief = "/etc/fleet/briefs/conversation.md"
workspace = { dir = "/work/discord-conversations" }
```

The account rules, beside today's account fields
(`crates/fleet-api/src/gateway.rs:491-513`) and the documented Slack account
(`docs/configuration/fleet-toml.md:276-314`):

- `platform = "discord"` requires `team`, `curlConfig` and `inbound =
  "discord-gateway"` (a new inbound value
  (`crates/fleet-api/src/gateway.rs:460-489`), verified as `connection`).
- `messageContent` defaults to `false`. Only a Discord account accepts the key.
- `fleet gateway account NAME --json` adds `intents`, the integer the listener
  Identifies with. The digest (`crates/fleet-gateway/src/lib.rs:247-297`) covers
  `intents`, so a changed `messageContent` makes a running listener
  `ListenerConfigOld`.
- A route's `channels` are parent text channels. A thread belongs to the route
  of its parent.
- Forum channels, private threads and DMs are out of scope (D3).

## Setup on the Discord side

1. Create an application in the Developer Portal. Add a bot user. Copy the bot
   token into the curl config file and into the listener's secret store, and
   nowhere else.
2. Under Privileged Gateway Intents, enable Message Content only when every
   message in a thread should be a turn. At 100 guilds or more, the intent needs
   verification ([message content
   intent](https://docs.discord.com/developers/events/gateway#message-content-intent)).
   One guild needs none.
3. Set the bot to private, so only the operator can add it to a guild. The
   setting's exact name in the portal is **unverified**.
4. Invite it to the one guild with the `bot` scope, as a guild install
   (`integration_type=0`), with the permissions below ([bot authorization
   flow](https://docs.discord.com/developers/topics/oauth2#bot-authorization-flow),
   [permissions](https://docs.discord.com/developers/topics/permissions#permissions-bitwise-permission-flags)).
5. Write the guild id, the bot user id and the channel ids into `fleet.toml`.
   Enable Developer Mode in the Discord client to copy the ids.

| Permission | Bit | Why |
| --- | --- | --- |
| `VIEW_CHANNEL` | `1 << 10` | See the route's channels and their threads. |
| `SEND_MESSAGES` | `1 << 11` | Post the opening message of a Question in a channel. It "has no effect in threads". |
| `SEND_MESSAGES_IN_THREADS` | `1 << 38` | Post in threads. |
| `CREATE_PUBLIC_THREADS` | `1 << 35` | Start a thread from a message. |
| `READ_MESSAGE_HISTORY` | `1 << 16` | History reads. Without it, reads come back empty. |

`MANAGE_THREADS` is not needed. Grant the permissions on the route's channels
only, where the guild allows that.

## Test plan

Each layer uses the same fixtures: recorded Discord dispatches and REST answers,
with fake snowflakes and no token.

| Layer | What it proves | Where |
| --- | --- | --- |
| Pure event reading | Truth tables for `read_event`: a top-level mention starts `discord:C:M`; a thread message uses `parentId`; Fleet's own message is `fleet`; another bot or a webhook is `bot`; the derived delivery ids; another guild, DMs and type 21 are ignored; a missing `parentId` is Unknown; a `MESSAGE_UPDATE` with empty content is ignored when `messageContent` is false. | `crates/fleet-gateway/src/discord.rs` unit tests |
| Text | Escaping outside code, code kept verbatim, splitting at 2,000 with fences kept whole, `nonce(key)` is 25 characters and stable. | the same |
| Fake transport | A stub `curl` on `PATH` replays recorded answers per URL: post; 10003, then start thread, then post; open with 160004 on retry; a 429 with `retry_after`; 10008 for a removed starter; history paging newest-first, reversed; an Incomplete budget. The stub checks that `--config` is given, that no argument holds `Authorization`, and that the body has `nonce`, `enforce_nonce` and `allowed_mentions.parse = []`. | adapter tests with the stub `curl`, as Slack's |
| Controller | Nonce recovery: an Unknown post inside the window is planned again under the same key; past the window it raises `PostUnconfirmed`; a split post resends all chunks. The fake with `marker = nonce` drives it. | `conversation_controller` tests; `gateway_controller` tests when posting lands |
| End to end | The built CLI with a `discord` account and the stub `curl`: `fleet gateway receive` reads recorded dispatches, a mention starts a Task, the reply starts the thread, a lost answer is recovered by the nonce, and `fleet gateway account --json` prints `intents`, with the digest moving when `messageContent` changes. | `crates/fleet-cli/tests/e2e_gateway.rs` |
| Live, manual | One private test guild and a real listener: a mention, a reply, a resume after a forced disconnect, and `messageContent` both ways. Discord's real nonce window is measured and recorded. | operator, outside CI |

## Implementation plan

Each step is its own change. Each starts with a failing end-to-end test, as
`AGENTS.md` asks, and keeps `cargo xtask check` green.

1. **Nonce recovery in the controllers.** Change `recovery_due` and
   `settle_posts` to follow the nonce rule. Drive it with the fake set to
   `marker = nonce`. This step is useful alone and touches no platform.
2. **Config and port values.** Add `Inbound::DiscordGateway` (`discord-gateway`,
   verified `connection`), the `messageContent` key, `intents` in
   `listener_account`, and the address form in the docs.
3. **Pure reading.** Add `discord.rs` with `KIND`, `parse_address`,
   `read_event`, the delivery ids, the text conversion, the splitting and
   `nonce(key)`. It has unit tests only and makes no call.
4. **REST calls.** Add `whoami` (`GET /users/@me`, plus a guild check),
   `capabilities` (`GET /applications/@me`), `history`, `post` with the lazy
   thread start, and the two-step `open`. Add `Platform::Discord` and the
   account checks in `GatewayAccounts::new`. Test with the stub `curl`.
5. **End to end and docs.** Add the `e2e_gateway` cases for a Discord account.
   Move Discord from "on paper" to an implemented adapter in `adapters.md` and
   `fleet-toml.md`. Add the setup steps.
6. **The listener.** A separate program outside the workspace, as for Slack,
   with its own repository and review.

No new crate and no new dependency. `discord.rs` uses what `slack.rs` uses (see
fleet-gateway's dependencies (`crates/architecture.toml:1-18`)).

## What it costs

- **One controller change, shared by all platforms.** `recovery_due` and
  `settle_posts` gain a third branch. It is pure and tested with the fake.
  Search: `git grep -n 'Marker::Metadata' ab35108 --
  crates/fleet-controllers/src` finds the two sites.
- **The documented address changes** from `discord:<thread>` to
  `discord:<channel>:<thread>`. No code reads the old form. Search: `git grep -n
  'discord:' ab35108 -- crates` finds no code.
- **The listener does more than Slack's.** It keeps a thread-to-parent map and
  persists a resume point. It is still transport only: it parses no message for
  Fleet.
- **Posts past the nonce window need a person.** A call that landed but whose
  answer was lost, if noticed after the window, raises `PostUnconfirmed`.
- **Without `MESSAGE_CONTENT`, only mentions reach Fleet.** In a tied thread, a
  person must mention the bot on each turn. The Conversation says so with
  `MentionsOnly`.

## Considered and dropped

- **The adapter looks up the parent itself, inside `event`.** `event` is pure,
  so the receive verb runs it without the lease (envelope
  (`crates/fleet-api/src/gateway.rs:212-234`)). A REST call there would break
  that rule. The listener already sees every thread event, so it holds the map.
- **A store table from thread to parent, written by the receive verb from
  `THREAD_*` events.** It adds a new record kind and a new writer for something
  the listener can hold. A missed `THREAD_CREATE` would still need a fallback.
- **Keep `discord:<thread>` as the address.** The receive verb could not match a
  thread to its route, and `post` could not start a thread from a top-level
  mention.
- **A visible marker in the text, an embed footer or a component `custom_id`.**
  People would see it, and it would cost characters. The nonce is invisible and
  documented.
- **Proving absence past the window from history.** A read without
  `READ_MESSAGE_HISTORY` is an empty list, so an empty read proves nothing.
  Fleet never takes Unknown as absent.
- **Fleet holds the WebSocket in `fleet up`.** The gateway design rejected this
  for every platform: the manager would gain a network service.
- **Forum channels.** `POST /channels/{id}/threads` creates a post and its
  thread in one call. But its message parameters list no `nonce` or
  `enforce_nonce` ([start thread in
  forum](https://docs.discord.com/developers/resources/channel#start-thread-in-forum-or-media-channel)).
  So an Unknown forum open could never be recovered. Left out for now (D3).

## Open questions and decisions

| # | Question | Recommendation |
| --- | --- | --- |
| D1 | Where does a thread's parent channel come from? | The listener adds `parentId` to the payload. The receive verb answers Unknown without it. |
| D2 | Which nonce window should Fleet trust? Discord says "the past few minutes". | 60 s. Measure it once in the live test, and keep the lower value. |
| D3 | Scope: text channels and public threads only? | Yes. Leave out forum channels, private threads and DMs until a route needs them. |
| D4 | Should `messageContent` default to off? | Yes. Mentions-only is the safe default, and the Conversation says so with `MentionsOnly`. |
| D5 | Order of the opening post: `Receive` and the listener's `MESSAGE_CREATE`. | Keep it. Fleet's top-level post in `C` carries no readable key, so the receive verb ignores it, and only `Receive` creates the Conversation. Cover this with an end-to-end test. |
| D6 | The listener: who writes and runs it? | A separate repository, like Slack's listener. Language and packaging are the operator's choice. |
| Q1 | Does a message that mentions the bot's role carry content without `MESSAGE_CONTENT`? | **Unverified.** Do not count a role mention as a mention. |
| Q2 | How long does a Gateway session stay resumable? | **Unverified.** The design does not depend on it. A failed resume loses the gap, and the history backstop covers tied threads. |
| Q3 | Is the 2,000 limit counted in characters, code points or UTF-16 units? | **Unverified.** Split by Unicode scalar values. If Discord refuses with 50035, measure it. |
| Q4 | Is `nonce` returned by `GET` messages? | **Not stated.** The design does not use it. |
