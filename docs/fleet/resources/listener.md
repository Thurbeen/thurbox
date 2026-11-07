# Listener resource

A Listener records the inbound health of one chat account for the
[Gateway controller](../controllers/gateway.md). Fleet makes no
inbound call: a separate listener program holds the platform's connection
and hands each delivery to `fleet gateway receive`. Whether that program is
running, connected and current is what it reports here, and what
ListenerController judges against the configuration Fleet runs.

## Example

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "Listener",
  "metadata": {"name": "listener-slack-main"},
  "spec": {"account": "slack-main"},
  "status": {
    "accepted": {"digest": "sha256:9c1e…", "at": 1730000000000},
    "beats": {"a1": {"state": "connected", "digest": "sha256:9c1e…", "at": 1730000420000,
                     "sentAt": 1730000419990, "lastDeliveryAt": 1730000300100}},
    "beatRevision": 7,
    "seen": 7,
    "observedAt": 1730000400000,
    "notBefore": 1730000600000,
    "conditions": [
      {"type": "Accepted", "status": "True", "reason": "Loaded", "lastTransitionTime": 1730000000000, "observedGeneration": 1},
      {"type": "Resolved", "status": "True", "reason": "IdentityMatches", "lastTransitionTime": 1730000001000, "observedGeneration": 1},
      {"type": "InboundReady", "status": "True", "reason": "Connected", "lastTransitionTime": 1730000002000, "observedGeneration": 1}
    ]
  }
}
```

## Spec

Written by the manager when it loads the config: one Listener per
configured account, named `listener-<account>`, by exclusive creation. The
receive verb creates it the same way when a listener beats before the
manager has loaded the config, for an account the config has. Fixed once
created.

| Field | Required | Meaning |
| --- | --- | --- |
| `account` | yes | The `[gateway.accounts]` entry. |

A manifest through `fleet apply` is refused: the config is the only source.

## Status

| Field | Writer | Meaning |
| --- | --- | --- |
| `accepted` | `listener-controller` | `{digest, at}`: the account's digest in the loaded config, the one `fleet gateway account NAME --json` prints for a listener to start from, and when it was loaded. |
| `beats` | `listener/<account>`, through `fleet gateway receive --beat` | Per listener `instance`, its last beat: `{state, digest, at, sentAt, lastDeliveryAt}`, `state` being `connecting`, `connected` or `closing`, `at` stamped by the verb's clock and `sentAt` the listener's. At most 8 instances; a new one past that replaces the entry with the oldest `at`. Written through the typed mutation `record_beat`, which changes nothing else. |
| `refused` | `listener/<account>`, through `fleet gateway receive` | `{counts, lastFaultAt}`: `counts` by reason (`NotEnvelope`, `WrongVerified`, `UnreadablePayload`, `SpoolFull`, `Deleting`, `UnknownRecord`) of envelopes the verb refused or could not read, and `lastFaultAt`, the last one that was the listener's own fault (not an envelope, or the wrong `verified`), through `record_refusal`. |
| `beatRevision` | `listener/<account>` | Incremented by every `record_beat` and `record_refusal`. |
| `seen` | `listener-controller` | The `beatRevision` last judged, so the Listener is due only when something new was reported. |
| `observedAt` | `listener-controller` | When `whoami` and `capabilities` last answered Known. |
| `notBefore` | `listener-controller` | The earlier of the next `gateway.pollMs` and the moment the freshest beat goes stale. |
| `conditions` | `listener-controller` | `Accepted`, `Resolved` and `InboundReady`, as the [interface](../controllers/gateway.md#listenercontroller) defines them. |

`InboundReady` is True only while some instance's beat is `connected`,
carries the accepted digest and is younger than three `gateway.beatMs` by
the verb's clock, and no envelope was refused for the listener's own fault
in that time. A listener that stops, stalls, runs on an older configuration
or sends envelopes the verb refuses turns it False within one sweep; one
thread's full spool does not. A connected listener that has delivered
nothing is still True, with its `lastDeliveryAt` beside it: Fleet cannot
see a platform's subscription settings, and does not guess.

## Metadata and ownership

`kind` is `Listener`. There is no finalizer and `ownerReferences` is
refused. ListenerController deletes a Listener whose account left the
config, by `uid` and `resourceVersion` under the lease; the manager creates
it again if the account returns. Deleting one by hand changes nothing on the
platform and it is created again at the next config load.

## Commands

```sh
fleet gateway account slack-main --json                  # what a listener starts from, with the digest
fleet gateway receive --account slack-main --beat < b.json   # what a listener runs on each state change
fleet gateway status                                     # each account: accepted, resolved, inbound, last delivery
fleet status listener listener-slack-main --json         # the conditions and the last beat
```
