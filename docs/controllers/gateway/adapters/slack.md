# Slack adapter

The Slack adapter maps one Slack workspace onto the
[Gateway port](../interface.md#gateway-port). Its code is
`crates/fleet-gateway/src/slack.rs`. It keeps the
[common rules](README.md#common-rules) of every adapter.

Every Slack claim cites Slack's own documentation, read on 2026-10-26. The
DM claims were read on 2026-11-04. A claim
that those pages do not state is marked **unverified**.

## Setup

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

## Configuration

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

### The wildcard route

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

### The DM route

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

## Address format

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

## Capabilities

| Capability | Value | Why |
| --- | --- | --- |
| `history` | `true` | `conversations.replies` reads a thread. |
| `marker` | `metadata` | Every post carries the intent key as message metadata. |
| `unmentioned` | `true` | Slack delivers every message in a channel the bot is in. |
| `threads` | `true` | Every conversation is a thread. |
| `reactions` | `true` | `reactions.add` and `reactions.remove` add and remove the bot's own reaction. |

The values are the same on every account. `capabilities` calls `auth.test`
first, so that a failing account answers Unknown.

### Port mapping

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

## Limits

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

## Troubleshooting

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
| `NeedsAttention/PostUnconfirmed` | A post stayed Unknown, and no recovery read found its key. | Check the thread, then act as the [Conversation](../../../resources/conversation.md) describes. |

[^slack-limits2025]: <https://docs.slack.dev/changelog/2025/05/29/rate-limit-changes-for-non-marketplace-apps>
[^slack-message]: <https://docs.slack.dev/reference/events/message>
[^slack-metadata]: <https://docs.slack.dev/messaging/message-metadata>
[^slack-react-add]: <https://docs.slack.dev/reference/methods/reactions.add>
[^slack-react-remove]: <https://docs.slack.dev/reference/methods/reactions.remove>
[^slack-socket]: <https://docs.slack.dev/apis/events-api/using-socket-mode>
[^slack-events]: <https://docs.slack.dev/apis/events-api/>
[^slack-java]: <https://docs.slack.dev/tools/java-slack-sdk/guides/socket-mode/>
[^slack-ent]: <https://docs.slack.dev/enterprise/developing-for-enterprise-orgs/>
[^slack-message-im]: <https://docs.slack.dev/reference/events/message.im>
[^slack-message-mpim]: <https://docs.slack.dev/reference/events/message.mpim>
[^slack-replies]: <https://docs.slack.dev/reference/methods/conversations.replies>
[^slack-app-home]: <https://docs.slack.dev/surfaces/app-home>
[^slack-threads]: <https://docs.slack.dev/messaging/retrieving-messages>
[^slack-post]: <https://docs.slack.dev/reference/methods/chat.postMessage>
[^slack-format]: <https://docs.slack.dev/messaging/formatting-message-text>
[^slack-mention]: <https://docs.slack.dev/reference/events/app_mention>
[^slack-user]: <https://docs.slack.dev/reference/objects/user-object/>
[^slack-markdown]: <https://docs.slack.dev/reference/block-kit/blocks/markdown-block>
[^slack-compare]: <https://docs.slack.dev/apis/events-api/comparing-http-socket-mode>
[^slack-ignoreself]: <https://docs.slack.dev/tools/bolt-js/reference/variables/ignoreself>
