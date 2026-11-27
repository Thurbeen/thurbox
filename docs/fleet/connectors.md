# Write a gateway connector

A gateway connector brings a chat platform into the
[Gateway](controllers/gateway.md) without a change to Fleet. A connector is
any program. Fleet runs it once per call, through `fleet-exec`, as it runs a
[runtime](../runtime/README.md#the-runtime-protocol). The connector is the
platform's adapter: it maps the [Gateway port](controllers/gateway.md#gateway-port)
onto the platform's API. Fleet's controllers do not change.

Slack and Discord are built-in adapters
([gateway adapters](controllers/gateway-adapters.md)). Every other platform
is a connector. The reference connector,
[`examples/gateway-connectors/file.sh`](../../examples/gateway-connectors/file.sh),
keeps its platform in files. The end-to-end tests drive it through the built
`fleet` binary.

## The parts

| Part | Who runs it | What it does |
| --- | --- | --- |
| The connector | Fleet, one bounded process per call | Parses addresses and events, reads history, posts, opens threads. |
| The listener | The operator's service manager | Holds the platform's connection or queue. Pipes each event into `fleet gateway receive`. |
| `fleet gateway receive` | The listener, once per event | Asks the connector's `event` verb to read the event. Stores the result in the thread's Conversation. |

The listener is transport only, as for Slack and Discord
([inbound transport](controllers/gateway.md#inbound-transport)). A connector
and its listener can be one program with two modes.

## The protocol

Fleet runs `<program> [args] <verb>`. The request is one JSON object on
stdin. The answer is one JSON object on stdout, with exit status 0:

- `{"known": <value>}`: the verb's answer.
- `{"unknown": {"why": "…"}}`: the connector cannot tell.

Fleet takes these as Unknown: a program that is not there, a non-zero exit,
a timeout, and output that does not parse. Unknown never concludes. Fleet
never takes an Unknown write as absent.

Every request carries `"protocol": 1` and `account`:

| `account` field | Meaning |
| --- | --- |
| `name` | The account's name under `[gateway.accounts]`. `event` can omit it. |
| `platform` | The connector's name. |
| `bot` | Fleet's bot id on the platform. |
| `inbound` | How the listener delivers. |
| `team` | The platform's workspace identity, when the account sets one. |
| `credentials` | A path to the operator's credential file, when the account sets one. |
| `options` | The account's `options` table, as JSON, when the account sets one. |

The verbs map one to one onto the Gateway port. Each value has the JSON shape
that Fleet's resources use: `ConversationRef`, `Delivery`, `Message`,
`Capabilities` and `History` are in [Conversation](resources/conversation.md)
and in `crates/fleet-api/src/gateway.rs`.

| Verb | Required | Request (besides `protocol`, `account`) | Known answer |
| --- | --- | --- | --- |
| `manifest` | yes | — | `{"protocol": 1, "platform", "verbs": [...]}` |
| `parse` | yes | `address` | a `ConversationRef` |
| `event` | yes | `envelope`, as the listener sent it | a `Delivery`, or `null` for a payload that carries nothing for Fleet |
| `whoami` | yes | — | an `Identity`: `{"id", "name"}` |
| `capabilities` | yes | — | `Capabilities`: `history`, `marker`, `unmentioned`, `threads`, `reactions` |
| `post` | yes | `conversation`, `text`, `intentKey` | `{"type": "posted", "message", "at"}` or `{"type": "failed", "why"}` |
| `history` | no | `conversation`, `query` | a `History`: `messages`, `leftOut`, `starter` |
| `open` | no | `channel`, `text`, `intentKey` | `{"type": "opened", "conversation", "message", "at"}` or `failed` |
| `react` | no | `conversation`, `message`, `reaction`, `on` | `{"type": "reacted", "already"}` or `failed` |
| `link` | no | `thread` | a URL string, or `null` |
| `fetch` | no | `conversation`, `file`, `dir` | `{"outcome": "saved", "path", "bytes"}` or `{"outcome": "refused", "why"}` |

Fleet reads the manifest once per process. It refuses the connector when:

- the manifest's `protocol` is not 1,
- the manifest's `platform` is not the connector's name,
- a required verb is missing from `verbs`.

Fleet does not call an optional verb that `verbs` does not list:

- Without `history`, the account has no history.
- Without `open`, `react` or `fetch`, that write fails with a reason.
- Without `link`, a log post names no link.

A `history` Unknown can carry `kind` (`timeout`, `incomplete`, `throttled`,
`unreadable`, `refused`) and `retryAfter` in milliseconds.

## The rules a connector keeps

The [common rules](controllers/gateway-adapters.md#common-rules) of the
built-in adapters apply. These rules apply to every connector:

- **One verb, one bounded process.** Finish inside the account's
  `callTimeoutMs`. Fleet kills the process tree after it.
- **Every write is keyed.** Two `post` or `open` calls with one `intentKey`
  make one message. Split a long text yourself, under keys derived from the
  `intentKey`. A connector with no way to find an earlier post reports
  `marker: "none"`, and Fleet then never repeats an Unknown post.
- **`event` makes no write.** The receive verb runs it without the lease.
- **Names are your platform's.** The `platform` of each `ConversationRef` is
  the connector's name. An address is `<platform>:<chat>[:<thread>]`.
  Encode a `:` inside an id.
- **Identity is an opaque id.** `author.id` is the id that
  `[gateway.people]` maps. A message from `bot` has the author kind `fleet`.
- **A refusal is Known; anything unclear is Unknown.**
- **`fetch` saves into `dir` only.** Fleet refuses a path outside it.

## Configuration

```toml
[gateway.connectors.teams]
program = "fleet-gateway-teams"   # on PATH, or an absolute path
args = []                         # optional, before the verb
callTimeoutMs = 20000             # optional; else gateway.callTimeoutMs

[gateway.accounts.teams-main]
platform = "teams"                # the connector's name
team = "<tenant id>"
bot = "28:<app id>"
inbound = "connection"
credentials = "/etc/fleet/teams.conf"
options = { serviceUrl = "https://smba.trafficmanager.net/emea/" }
```

- A connector's name cannot be a built-in platform's name.
- A connector account's `inbound` is `connection`.
- A connector account takes `credentials` and `options`. It does not take
  `cli`, `curlConfig`, `dir` or `messageContent`.
- `credentials` is an absolute path.

[Configuration](configuration.md#the-gateway-section) lists every key.

## Secrets

Fleet holds no platform credential:

- Fleet passes the `credentials` path in each request. Fleet never opens
  the file.
- `credentials` and `options` stay in the config file. Fleet writes neither
  to the store, to a Task's environment, or to the listener's digest.
- The connector runs with the manager's environment.

Keep the credential file readable by the user that runs Fleet only.

## Check a connector

`fleet version --json` names the protocol as `gatewayProtocol`. To check a
connector by hand, configure an account and a route, then pipe one event:

```sh
printf '%s' "$envelope" | fleet gateway receive --account teams-main --json
fleet reconcile --json
```

`crates/fleet-cli/tests/e2e_gateway_connector.rs` holds the reference
connector to the protocol.
