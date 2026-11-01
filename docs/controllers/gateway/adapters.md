# Gateway adapters

Slack is the first adapter, implemented in `crates/fleet-gateway/src/slack.rs`
as this page maps it. The rest of this page is design, not implemented.

Telegram, Discord and Microsoft Teams are mapped on paper only. The
[Discord adapter design](discord.md) takes Discord further. The
purpose is to check the [port](interface.md#gateway-port) against
platforms that differ. What these platforms showed is folded back into
the model, under [what the platforms changed](#what-the-platforms-changed).

Every platform claim cites the platform's own documentation, read on
2026-10-26. A claim that those pages do not state is marked
**unverified**.

## Slack, the first adapter

| Port item | Slack |
| --- | --- |
| Account | One workspace: the account's `team`. The `team` is the `team_id`, or the `enterprise_id` in an Enterprise org. Slack advises keying stored data on the `enterprise_id`[^slack-ent]. One bot. One `[gateway.accounts.<name>]` entry with `platform = "slack"`. |
| Conversation | A thread: the channel id and the parent's `ts`. A message is in a thread when it has `thread_ts`. The parent is the message where `thread_ts == ts`[^slack-threads]. `ts` is unique within a channel[^slack-threads]. Address: `slack:C0123ABCD:1730000000.000100`. A reply is posted with the parent's `ts` as `thread_ts`, never with a reply's `ts`[^slack-post]. |
| Message ids | `ts`, unique within the channel. The event wrapper's `event_id` is globally unique. The receive verb uses `event_id` as its de-duplication key[^slack-events]. |
| Inbound | Message events through the `message.channels` and `message.groups` subscriptions, with `channels:history` and `groups:history`[^slack-message]. Thread replies arrive as ordinary message events. Over the Events API, `message_replied` lacks its subtype. So a reply is told by `thread_ts != ts`[^slack-replied]. |
| Mention | `<@U…>` with Fleet's bot user id in `text`[^slack-format]. The adapter reads the mention from the message event. It does not rely on `app_mention`. Reason: `app_mention` needs `app_mentions.read`, and the reference does not state how it behaves inside threads (**unverified**)[^slack-mention]. |
| Transport | Socket Mode. The app uses an app-level token with `connections:write`. `apps.connections.open` gives a temporary `wss://` URL. The app acknowledges each envelope by its `envelope_id`, so Slack knows whether to retry[^slack-socket]. The acknowledgement is due within three seconds, per Slack's Java SDK guide[^slack-java]. The Socket Mode page itself gives no limit. While disconnected, the app "may lose events". Slack refreshes connections every few hours. Retry counts for Socket Mode are **unverified**[^slack-socket]. The alternative is the Events API over HTTP. It needs a public endpoint and a 2xx within three seconds. It retries up to three times: at once, after one minute and after five[^slack-events]. Socket Mode apps are not allowed in the public Marketplace[^slack-compare]. This does not matter for an internal app. |
| History | `conversations.replies` for a thread and `conversations.history` for a channel. Both are Tier 3 (50+ per minute)[^slack-replies]. Since 2025-05-29, some apps get Tier 1 (one request a minute, 15 objects) on new installs. These are commercially distributed apps not approved for the Marketplace. Internal customer-built apps are not affected[^slack-limits2025]. For that reason, Fleet's Slack app is an internal app. |
| Post and open | `chat.postMessage`: with `thread_ts` for a post, and without it for an open. The new thread's address is the channel and the returned `ts`. The recommended length is 4,000 characters. Text over 40,000 is truncated[^slack-post]. The adapter splits at 4,000. `mrkdwn` is Slack's own syntax, not Markdown. `&`, `<` and `>` are escaped[^slack-format]. Slack allows about one message a second per channel[^slack-post]. So posts to one thread are sequential anyway. |
| Own posts | Every post carries message metadata: `{"event_type": "fleet_post", "event_payload": {"key": "<intent key>"}}`[^slack-post]. `conversations.history` and `conversations.replies` read it back with `include_all_metadata`, under `message_metadata:read`[^slack-metadata][^slack-replies]. Metadata is visible to every member of the workspace[^slack-post]. So a key names a Task and its `uid`, never a secret. A bot's message carries `bot_id`[^slack-bot]. The documentation does not state whether the app receives its own posts as events. Bolt's `ignoreSelf` implies that it does[^slack-ignoreself]. So the adapter reads them as `author.kind = fleet`. |
| Idempotency | None on `chat.postMessage`: `client_msg_id` is not an argument[^slack-post]. Recovery uses the metadata key in a later `history` read. |
| Identity | User ids, unique in their workspace, paired with the team[^slack-user]. In an Enterprise org, every user has one global id that starts with `U` or `W`[^slack-ent]. The allowlist names ids, never display names or emails. |
| Credentials | Bounded `curl` calls. The adapter keeps the Bitbucket Cloud forge adapter's rule: Fleet never constructs an Authorization header. That adapter relies on netrc; this one does not. The bot token lives in a curl config file that the operator owns. Fleet names that file with `curl -K`, from `curlConfig`, and never reads it. The `curlConfig` key is new. The listener holds the app-level token. |

A Slack route must know two things:

1. A message posted while the listener was disconnected may be
   lost[^slack-socket]. In a tied thread, the history backstop recovers
   it. A new thread's first mention during an outage is lost, and the
   person mentions again.
2. The bot must be a member of every channel that a route covers. Message
   events and history reach the bot only then. Whether `message.channels`
   delivers without membership is not stated (**unverified**).

## Cross-platform check

| | Slack | Telegram | Discord | Microsoft Teams |
| --- | --- | --- | --- | --- |
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

The [interface](interface.md) and the
[resource](../../resources/conversation.md) already hold each correction:

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

[^slack-ent]: <https://docs.slack.dev/enterprise/developing-for-enterprise-orgs/>
[^slack-threads]: <https://docs.slack.dev/messaging/retrieving-messages>
[^slack-post]: <https://docs.slack.dev/reference/methods/chat.postMessage>
[^slack-events]: <https://docs.slack.dev/apis/events-api/>
[^slack-message]: <https://docs.slack.dev/reference/events/message>
[^slack-replied]: <https://docs.slack.dev/reference/events/message/message_replied>
[^slack-format]: <https://docs.slack.dev/messaging/formatting-message-text>
[^slack-mention]: <https://docs.slack.dev/reference/events/app_mention>
[^slack-socket]: <https://docs.slack.dev/apis/events-api/using-socket-mode>
[^slack-java]: <https://docs.slack.dev/tools/java-slack-sdk/guides/socket-mode/>
[^slack-compare]: <https://docs.slack.dev/apis/events-api/comparing-http-socket-mode>
[^slack-replies]: <https://docs.slack.dev/reference/methods/conversations.replies>
[^slack-limits2025]: <https://docs.slack.dev/changelog/2025/05/29/rate-limit-changes-for-non-marketplace-apps>
[^slack-metadata]: <https://docs.slack.dev/messaging/message-metadata>
[^slack-bot]: <https://docs.slack.dev/reference/events/message/bot_message>
[^slack-ignoreself]: <https://docs.slack.dev/tools/bolt-js/reference/variables/ignoreself>
[^slack-user]: <https://docs.slack.dev/reference/objects/user-object/>
[^tg-message]: <https://core.telegram.org/bots/api#message>
[^tg-topics]: <https://core.telegram.org/bots/features#topics-in-private-chats>
[^dc-threads]: <https://docs.discord.com/developers/topics/threads>
[^dc-start]: <https://docs.discord.com/developers/resources/channel#start-thread-from-message>
[^ms-channel]: <https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/channel-and-group-conversations>
[^ms-concepts]: <https://learn.microsoft.com/en-us/microsoftteams/platform/bots/bot-concepts>
[^tg-entity]: <https://core.telegram.org/bots/api#messageentity>
[^tg-privacy]: <https://core.telegram.org/bots/features#privacy-mode>
[^dc-message]: <https://docs.discord.com/developers/resources/message#message-object>
[^dc-format]: <https://docs.discord.com/developers/reference#message-formatting>
[^ms-rsc]: <https://learn.microsoft.com/en-us/microsoftteams/platform/agents-in-teams/enable-receive-all-chat-messages>
[^dc-intent]: <https://docs.discord.com/developers/events/gateway#message-content-intent>
[^tg-updates]: <https://core.telegram.org/bots/api#getting-updates>
[^dc-gateway]: <https://docs.discord.com/developers/events/gateway#resuming>
[^dc-webhooks]: <https://docs.discord.com/developers/events/webhook-events#event-types>
[^ms-auth]: <https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication?view=azure-bot-service-4.0>
[^dc-messages]: <https://docs.discord.com/developers/resources/message#get-channel-messages>
[^ms-graph]: <https://learn.microsoft.com/en-us/graph/api/chatmessage-list-replies?view=graph-rest-1.0>
[^tg-user]: <https://core.telegram.org/bots/api#user>
[^tg-send]: <https://core.telegram.org/bots/api#sendmessage>
[^dc-create]: <https://docs.discord.com/developers/resources/message#create-message>
[^ms-graph-from]: <https://learn.microsoft.com/en-us/graph/api/resources/chatmessagefromidentityset?view=graph-rest-1.0>
[^dc-user]: <https://docs.discord.com/developers/resources/user#user-object>
[^ms-proactive]: <https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/send-proactive-messages>
[^tg-forum]: <https://core.telegram.org/bots/api#createforumtopic>
[^tg-limits]: <https://core.telegram.org/bots/faq#my-bot-is-hitting-limits-how-do-i-avoid-this>
[^ms-format]: <https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/format-your-bot-messages>
[^ms-rate]: <https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/rate-limit>
[^tg-b2b]: <https://core.telegram.org/bots/features#bot-to-bot-communication>
[^ms-sdk]: <https://learn.microsoft.com/en-us/azure/bot-service/index-bf-sdk?view=azure-bot-service-4.0>
[^ms-teams-sdk]: <https://learn.microsoft.com/en-us/microsoftteams/platform/teams-sdk/why>
