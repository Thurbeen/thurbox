# Fleet: the contracts of the Task core

Fleet runs agent sessions through Task resources and their controller. A
Task is one agent session doing one brief. This file says what each part
guarantees
and which tests hold it to that. The [overall
architecture](architecture.md)
explains the component boundaries. The
[Task controller guide](controllers/task.md) defines the
resource fields, state machine and controller interface.

## What is served

[Resource kinds](README.md#resource-kinds) lists every kind, who writes its
spec and who writes its status.
[`kinds::status_owner`](../../crates/fleet-api/src/kinds.rs) is the rule
the store enforces on each status field.

Kinds not built in or registered by a plugin are refused on write as `unknown
kind "X"`. A Task spec
accepts the fields below (`deny_unknown_fields`), and no object may carry
`ownerReferences`.

The [Forge controller](controllers/forge.md) is a second
controller over the Task: it follows the changes a worker declared and
relays their feedback into the Task's inbox, a status field with no single
owner, `status.inbox`, reached only through the typed mutations under
[Store](#store), which refuse a duplicate key. `fleet send` and the Forge
controller append to it and TaskController delivers it. The Forge
controller's one effect, mark-ready, is the only write Fleet makes on a
forge: it takes a declared draft out of draft under a configured ready
policy, and nothing else.

The [Plugin controller](controllers/plugin.md) hosts external
controllers. The store serves their schema-validated
[kinds](resources/plugin-kinds.md); the manager calls their pure
`probes` and `reconcile` calls and bounded `observe` and `perform` calls through
the `Plugins` port's one `call`. A pure gate admits plans before leased status
writes and atomic verbs. `e2e_plugin_controller` and `plugin_gate` exercise the
host.

The [CronTask controller](controllers/cron-task.md) creates a Task at each
scheduled time of a CronTask, intent before the create and receipt after,
under a key and a name that the scheduled time fixes. It adopts a Task that
carries its uid and never creates a second one. `e2e_cron_task` holds it
to that, on the file store with a test clock.

The [Gateway controller](controllers/gateway.md) is three controllers over
the `Gateway` port in `fleet_api::gateway`. The `fleet-gateway` adapter
crate depends on no Fleet crate but `fleet-api` and `fleet-exec`, and has a
file-backed fake that models every `capabilities` combination. The CLI
loads and validates the `[gateway]` config, and opens its accounts before a
manager run so that the lease outlasts the port's worst case.
ConversationController keeps [the mirror](#the-conversation-mirror),
admits a thread and creates its Task under the workspace precondition.
ListenerController judges an account's listener. GatewayController owns
`status.gateway`, relays the thread into the inbox under a cursor and a
child's words into its parent's inbox ([talking in a
conversation](#talking-in-a-conversation)), and plans `Post`, `Open`,
`Receive`, `Log` and `React` actions. The manager performs `Post`, `Open`,
`Log` and `React` through the `Gateway` port and `Receive` through the
store, and writes a receipt under each key. `e2e_gateway_post`,
`e2e_gateway_logs` and `e2e_gateway_reactions` hold it to that.

## Crates and the allowed graph

```text
fleet-api          pure           types and ports
fleet-controllers  pure           controllers and gate   -> api
fleet-exec         adapter        the one process spawner
fleet-store        adapter        file store             -> api
fleet-runtime      adapter        exec, fake, term-bridge -> api, exec,
term-stream, otel-env
fleet-forge        adapter        az, curl, gh, glab, fake -> api, exec
fleet-gateway      adapter        curl (Slack, Discord), fake -> api, exec
fleet-plugin       adapter        bounded plugin stdio   -> api, exec
fleet-manager      orchestration  lease, plans, verbs    -> api, controllers
fleet-cli          edge           the `fleet` binary     -> api, manager, store,
runtime, forge, gateway, plugin, otel-env
arch-rules         tooling        the architecture test
fleet-dev          tooling        `cargo xtask`: gate and release
```

`crates/architecture.toml` declares the graph and these rules, and the
`architecture` test of `arch-rules` enforces them on every gate run:

- Both the declared and the actual crate graph are acyclic, and every actual
  edge (normal, build and dev dependencies, renames included) is declared.
- A pure crate reaches no filesystem, process, network, environment, thread
  or clock, and uses only the external crates its rule lists. Paths are
  resolved through aliases, globs and re-exports first, across crates.
- No crate reaches another crate it may not depend on through a third
  crate's re-export.
- Only the crates listed may name a status-writing field manager; the
  check is per crate, and fleet-api, which defines them, may name every
  one. Only fleet-controllers names `FieldManager::TaskController`,
  `FieldManager::ChangeRequestController`, `FieldManager::ForgeController`,
  `FieldManager::GatewayController`, `FieldManager::ConversationController`,
  `FieldManager::ListenerController` and `FieldManager::CronTaskController`,
  only fleet-api and fleet-controllers
  name `FieldManager::Plugin`; only fleet-manager's `result` and `declare`
  verbs name `FieldManager::Worker`, only fleet-manager's `send` and
  `amend` verbs (and the store that checks them) name `FieldManager::Cli`,
  the caller of the inbox append and of the amend mutation, and only
  fleet-manager's `receive` verb (and the store) names
  `FieldManager::Listener`, the caller of the spool and beat
  mutations.
- The CLI calls no store write, no controller method, no runtime effect and
  no forge read: it parses, calls a fleet-manager verb, and prints.
- Only fleet-exec reaches `std::process::Command`, `std::os::unix::process`,
  `std::os::windows::process` or `process_wrap`. Every program fleet runs is
  bounded and has its whole process tree killed past its bound or on an early
  return: a process group on Unix, a job object on Windows. The one program
  with no bound is the command a bridge runs on a pty (`fleet_exec::pty`),
  since its view ends it: it is killed when its handle is dropped.
- fleet-forge depends on nothing above the API and the spawner and is
  depended on by the CLI alone: no forge-specific name appears above the
  Forge port.
- The Forge port itself, which carries the one forge write, is held only by
  fleet-api, fleet-forge, fleet-manager and fleet-cli: no other adapter and
  no controller can reach `mark_ready`.
- fleet-gateway, likewise, depends on nothing above the API and the spawner
  and is depended on by the CLI alone, and the Gateway port, which carries
  the chat writes `open`, `post` and `react`, is held only by fleet-api,
  fleet-gateway, fleet-manager and fleet-cli. The CLI may not call the
  port's `history`, `capabilities`, `post` or `react`.
- otel-env, the telemetry crate every program shares, depends on no
  other crate of the repository. Only otel-env reaches `opentelemetry_sdk`
  or `opentelemetry_otlp`: the providers, batching, exporter and shutdown
  bound are in one crate. The manager and the runtime adapter record through
  the `opentelemetry` API, a no-op without a provider; the pure crates reach
  no telemetry at all.

Each rule has a negative fixture that breaks it, and the test requires the
checker to report exactly that rule. The pure crates also carry
`compile_fail` doc probes for every forbidden edge.

## Task

```text
spec.configRef      optional TaskConfig reference
spec.env            explicit environment overrides
spec.harness        the agent harness, explicit or inherited
spec.host           the runtime host (default "local")
spec.model          optional; reaches the harness through its model variable
spec.pauseAfterMs   optional; pause the session once its agent was idle
                    this long, over the TaskConfig's
spec.agentTelemetry optional; false keeps the agent's own telemetry off,
                    over the TaskConfig's
spec.brief          the brief, sent once, never changed
spec.amendments     append-only additions to the brief, through fleet amend
spec.workspace      a list of absolute URLs; the runtime makes them
spec.dispatch       nothing spawns while false
spec.outputs        optional: the structured outputs a result must carry
spec.forge          optional: relay and draft overrides for declared changes
spec.subscriptions  optional: the Boards whose new events reach the inbox,
                    one notice per subscription
```

The spec is immutable once created, except `dispatch`, `forge` and
`subscriptions`; a
different brief is a different Task. `amendments` only grows, through the
store's amend mutation (`amend_task` for `fleet amend`, a plugin's `amends[]`
verb through `plugin_mutate`): `update_spec`, and so `apply` and a plugin
patch, refuses any change to it, and `apply` keeps the stored amendments
when a manifest omits them, and a manifest that creates a Task with
`amendments` is refused. `harness`, `host` and `model` are non-empty and
may not start with `-`, since they reach the runtime as arguments; whether
each is present never changes after creation.

`spec.workspace` names what the session works in. It never says how the
workspace is made. [Workspace](resources/workspace.md)
defines both forms and their validation.

See [execution configuration](resources/task-config.md) for
reference validation, deterministic precedence and private launch freezing.
New configuration kinds have no lifecycle, finalizers or status writers.

## Task lifecycle

TaskController is a pure function from the Task and what the probes saw to a
plan: the status to write, the effects to run, and whether to drop the
finalizer. The manager performs it in this order:

1. **Intent first.** The status carrying each effect's intent is written
   (compare-and-swap, under the lease) before the effect runs.
2. **Effect.** Materialize, spawn, brief, release, revive, pause, resume or
   clean, each under a deterministic key
   (`task/<name>/<uid>/<action>[/<attempt>]`).
3. **Receipt after.** The outcome is written as the key's receipt. An intent
   without a receipt is a question the next pass answers by observing the
   key, never by repeating the effect blindly.

Guarantees, each with its test:

- **Workspace once.** A Task with a non-empty URL list plans `Materialize`,
  the runtime's `workspace` call, under `materialize/<n>` before its spawn,
  with the session key as the workspace key. `Spawned` waits until
  `status.workspace` holds the whole workspace, and the spawn gets it as
  `ready`. After an unknown outcome the controller asks the runtime again
  under the same key, which is idempotent: made is adopted, nothing made is
  retried after `RETRY_DELAY_MS`, unreadable waits. `MAX_ATTEMPTS` refusals
  stall the Task with `WorkspaceReady=False/MaterializeFailed`. A URL the
  runtime does not handle spends no attempt and waits with
  `WorkspaceUnsupported`. After the release, and also when nothing was
  spawned, `CleanWorkspace`, the runtime's `stop` with `release`, runs under
  `cleanWorkspace/<n>`. The finalizer goes only after a `Deleted` receipt.
  A failed or unknown release is repeated after each wait, and three raise
  `NeedsAttention/CleanFailed`. A Task deleted while its `workspace` call is
  unanswered releases the key without making anything. `task_workspace` and
  `e2e_workspace` cover each case.
- **Spawn once.** The session key is `<name>.<uid>`. After an unknown spawn
  outcome the controller looks the key up: found is adopted, absent is retried,
  unreadable waits.
- **Brief once.** The brief is sent only to a ready session, under one key
  the session stores. An unconfirmed send is asked about: delivered is a
  recovered receipt, not delivered is one more send under the same key, and
  "cannot tell" is retried after a wait. Three unreadable probes raise
  `NeedsAttention(BriefUnconfirmed)`; probes continue so recovery can settle it.
  A send the runtime only queued, where the agent does not read it, sets
  `Briefed=False/BriefUndelivered` and raises `NeedsAttention`; it is never
  sent again and holds the inbox.
- **Trust before the brief.** The brief is sent only after the runtime
  reports that the session's agent trusts its workspace. A known gap sets
  `WorkspaceTrusted=False`, raises `NeedsAttention/WorkspaceUntrusted` and
  is probed again after each wait. Unknown holds the brief, raises nothing
  and is probed again on the next pass. A held brief spends no send, and no
  inbox entry goes out before the brief.
- **Retries.** Spawns and brief sends are tried at most `MAX_ATTEMPTS` (3)
  times before `Stalled` and `NeedsAttention`. Failed attempts wait
  `RETRY_DELAY_MS` (30 seconds), persisted as `status.notBefore` across manager
  runs. Unreadable delivery probes and failed releases raise `NeedsAttention`
  after three attempts but continue after each wait: recovery can confirm
  delivery or remove the finalizer.
- **Unsupported URL.** A `workspace` call answered `Known(Unsupported { url
  })` spends no attempt. The Task stays Pending with `WorkspaceReady=False`
  and `NeedsAttention`, both with reason `WorkspaceUnsupported`. The call is
  tried again after `RETRY_DELAY_MS`, without limit, under the same effect
  key. A later answer that is not `Unsupported` clears that
  `NeedsAttention`.
- **Unmapped trust.** A spawn answered `Known(TrustUnmapped { harness })`
  spends no attempt. The Task returns to Pending with `Spawned=False` and
  `NeedsAttention`, both with reason `TrustUnmapped`, and the spawn is tried
  again after `RETRY_DELAY_MS`, without limit, under the same key. A later
  spawn that does not answer `TrustUnmapped` clears that `NeedsAttention`.
- **Result.** `fleet result` writes `status.result` as field manager
  `worker`, only once the brief has a receipt, only for
  `status.brief.generation`, never while an amendment waits to be delivered
  (unless its delivery was given up), and only when its outputs match
  `spec.outputs` plus the outputs of each amendment at or below that
  generation. The same result again is a no-op. Within one generation a
  different result is refused; a result for a newer generation replaces the
  older one. A result for `status.brief.generation` concludes the Task.
- **Label.** `fleet label task TASK KEY=VALUE…` sets each label on the Task
  in one compare-and-swap write and changes nothing else: the spec, the
  generation and the other labels stay. A label only Fleet writes (the
  Conversation and parent labels, a plugin's creator label) is refused with
  exit 1. `apply` cannot relabel a Task a Conversation made, since it refuses
  a manifest that carries those labels; `label` keeps them as they are, and
  leaves the creator label to the store, which keeps it on every update.
- **Amend.** `fleet amend TASK` appends `{text, reason?, outputs?, from, at,
  generation}` to `spec.amendments`, increments `metadata.generation` and
  queues an inbox entry with `amends` under
  `task/<name>/<uid>/amend/<generation>`, in one compare-and-swap write. It
  needs no generation from the caller; `--if-generation N` is an optional
  precondition, refused with exit 5. A Stalled or deleting Task, a full
  inbox, empty text and an output name already declared are refused. The
  brief's first send carries every amendment so far and records the
  generation of its intent, so an amendment queued before it is recorded
  without a send. A later amendment goes out as an inbox entry; a newer one
  queued behind an unsent one carries both, and the older is recorded
  without a send. Its receipt sets `status.brief` to its key and
  generation. A delivered amendment re-opens a Concluded Task to Briefed,
  with `Concluded=False/Amended`, and keeps the old result. A plugin's
  `amends[]` verb applies the same mutation and checks as
  `plugin/<name>`, with a required `ifGeneration`: on any other generation
  the store refuses it, and `host.verbs` records the refusal.
- **WorkerGone.** A session a readable list no longer shows sets `WorkerGone`
  and `NeedsAttention`. An unreadable list is never read as gone. Nothing
  abandons the Task by itself.
- **Lost session.** A session the runtime still holds without its agent's
  window, as after a host reboot, sets `SessionLost` and gets nothing sent.
  A dispatched Task plans `Revive` under `revive/<n>`, at once and then
  after each `RETRY_DELAY_MS`; three revives that leave the session lost
  raise `NeedsAttention/ReviveFailed`, and revives continue. Back, a briefed
  worker is owed one catch-up note under `catchup/<n>`, sent before its inbox
  as an entry is sent. In Briefed the note repeats the brief and its
  generation. Only Spawned, Briefed and Concluded-with-delivery Tasks are
  looked at, so a concluded worker at rest stays at rest. Unknown revives
  nothing, and a revive never makes a second session.
- **Pause.** With `spec.pauseAfterMs`, or the TaskConfig's `pauseAfterMs`
  frozen in `status.execution`, a dispatched Briefed or Concluded Task
  whose agent the runtime reports Known Idle on every pass for
  `pauseAfterMs`, and on the pass that plans it, plans `Pause` under
  `pause/<n>`. Its harness profile must have a resume argv and its hooks
  must have reported its conversation, so aider and a program never pause.
  The session must be observed not stopped and not known
  lost, its inbox empty, and no brief, entry, catch-up note, revive or
  resume in flight. Active or Unknown clears `status.idleSince`; the idle
  time starts again at the next Idle answer. Activity is what the agent's
  own hooks report through `fleet signal`, never the runtime's; Unknown
  never pauses, and a runtime that cannot pause never answers `paused`
  Known false. `Paused=True/Idle` changes no
  phase and raises no `WorkerGone`, `SessionLost` or revive.
- **Resume.** An inbox entry for a paused Task plans one `Resume` under
  `resume/<n>`, and so does a `status.wakeRequestedAt` at or after the
  `Paused=True` transition; entries that arrive meanwhile wait in the inbox. Resumes
  wait `RETRY_DELAY_MS` between tries; three resumes that leave the session
  stopped raise `NeedsAttention/ResumeFailed`, and resumes continue. Back
  (`Paused=False/Resumed`), the worker is owed the catch-up note before its
  inbox. A session found stopped by someone else is `Paused=True/External`
  and is resumed by the next entry. Stalled, held and deleting Tasks are
  never resumed. A missing or Unknown pause or resume receipt is settled by
  observing whether the session is stopped, never by a blind repeat. A
  catch-up note an older record keeps under `resume/<n>` is still sent
  under that key; new resume keys continue past it.
- **Release.** The `fleet.dev/release` finalizer holds a deleted Task until
  its session is deleted, once the session is proven at rest or gone, or
  once `RELEASE_GRACE_MS` (5 minutes) has passed since its
  `deletionTimestamp` while the session is busy or its rest is Unknown. A
  stale `working` report therefore cannot hold a delete for ever. The
  workspace clean still preserves uncommitted work.
- **Inbox.** `fleet send TASK TEXT` appends `{key, from, text, queuedAt}` to
  `status.inbox` as `cli/<user>`, under `task/<name>/<uid>/send/<n>`, `n`
  the resourceVersion appended against. The oldest entry not given up on is
  delivered as the brief is: to a session observed ready on that pass, under
  its key as the runtime intent key, one entry per pass, at most three
  sends. It counts as delivered once the agent's prompt hook reports its
  digest after the send's intent: a report of the same text before it
  proves an earlier send, not this one. A lost answer is settled by that
  report alone, never resent (the runtime's journal decides only for a
  harness with no prompt hook). A Known receipt moves the entry's receipt into
  `status.deliveries` and the entry out of `status.inbox` in one write, and
  drops its intent, receipt and attempt records. Three refused sends raise
  `NeedsAttention/DeliveryFailed` naming the entry, which stays while later
  entries go out; three unreadable journal reads, or a typed entry whose
  prompt hook reported another prompt since the send and not this one, raise
  `DeliveryUnconfirmed` naming the entry, which is never resent and stays while
  later entries go out; more than 20 entries still to deliver raise `InboxFull`.
  An entry given up on does not count. Such an entry can never be delivered, so
  it can never make room. Nothing bounds the entries given up on. Each one given
  up on raises `NeedsAttention`, which names the latest;
  `status.deliveryAttempts` marks every one as failed. Delivery runs in Spawned
  (after the brief), Briefed and Concluded, which stays terminal and is due
  while its inbox is nonempty.
  At rest, a Concluded Task is due once every `SESSION_RECHECK_MS` (5
  minutes), recorded as `status.sessionCheckedAt`, to look its session up.
  A readable list without the session there is `WorkerGone`, never a
  respawn. Stalled delivers nothing; deletion keeps precedence and refuses
  new sends.
- **No churn.** A condition keeps its `lastTransitionTime` unless its status
  changes against what was stored, so a pass that sees nothing new writes
  nothing and the manager goes idle.

Conditions: `HostReachable`, `WorkspaceReady`, `Spawned`,
`WorkspaceTrusted`, `Briefed`, `Stalled`, `WorkerGone`, `SessionLost`,
`Paused`, `Concluded`, `Released`, `NeedsAttention`.

## Following declared changes

ForgeController is a pure function from the Task, the ChangeRequests its
worker declared, Fleet's login on their hosts and, when asked, whether the
session is at rest, to a plan: the desired `status.forge`, the inbox
entries to land with it, and the `MarkReady` effects whose intents it
carries. It has one effect and no finalizer, and never writes a
ChangeRequest. Guarantees, each with its test in `forge_controller`,
`e2e_forge_controller` and `e2e_mark_ready`:

- **Declare.** `fleet declare TASK --change-request URL` parses the URL
  through the Forge port, ensures one ChangeRequest under the derived name
  (an existing one is reused after its `status.ref`, else its own URL, is
  verified) and appends `{kind, url, name}` to `status.declared` as
  `worker`, with CAS; a repeated URL, or any URL of the same change, is a
  no-op. `--json OBJECT` appends any
  object with a string `kind`. Both need a brief receipt and refuse a
  deleting Task, and an ineligible Task creates no orphan; a crash between
  the two steps leaves a ChangeRequest nobody follows, which the next
  `declare` of that URL finds.
- **Join at the present.** A declaration whose object is Known with a `ref`
  gets an entry with the object's `uid` and `cursor = revision`, and one
  message holding the whole snapshot, under
  `task/<name>/<uid>/relay/<change uid>/<revision>` from `forge/<name>`;
  when the mirror has already dropped notes, the message says older notes
  exist on the forge. An object without a `ref` yet, or Unknown, waits;
  readable absence, or an unreadable mirror status, raises
  `NeedsAttention/ChangeGone`.
- **Relay by cursor.** Every note with `seq` above the cursor that passes the
  filters (never this Task's own marked words; bots only per `relayBots`;
  never a resolved thread or one this Task already answered in) and every
  transition of state, review decision or passed/failed checks above it
  become one entry, with `about` naming the mirror's `uid`, `revision` and
  head; the cursor advances in the same write, whether or not anything was
  sent. Another worker's marked note is relayed and labelled with its Task.
  `spec.forge.relay.notes` and `.checks` stop each kind on its own.
- **Gaps.** `droppedBefore` above the cursor relays a gap notice with the
  whole retained snapshot, counts `gaps` and raises `NeedsAttention/RelayGap`
  until the next relay that carries something.
- **Attribution first.** An Unknown login on the change's host skips the
  entry this pass: nothing is relayed that could not be filtered.
- **Rebind.** A different `uid` under the same name is a new incarnation:
  the entry takes it, resets its cursor and mark-ready record, and joins
  again; the delivered-revision guard is per mirror `uid`, so the new join
  is not refused as a repeat.
- **Settled.** An entry whose ChangeRequest settled is relayed one last time
  and read no more; a Concluded Task is still relayed to; a deleted Task
  leaves every ChangeRequest alone.
- **One write.** The cursor and the entries land through `record_relay`, or
  not at all: a duplicate key or a stale version refuses the whole write.
  A mark-ready intent rides in the same write.
- **Mark ready once.** A following entry whose change is a draft that was
  never seen ready (`everReady` false), whose mirror is fresh (`Reachable`
  True, `observedAt` within `2 × pollMs`) and whose selected policy holds,
  `workerDone` (a `shipped` result for `status.brief.generation`) and
  `workerIdle` (`AtRest` Known true) always included, gets one intent under
  `task/<name>/<uid>/ready/<change uid>/<head>` and one `MarkReady` effect
  through the Forge port, after the lease is renewed; the receipt lands on
  the entry. The session is asked only once every predicate the snapshot
  answers holds. A change opened ready, or put back into draft by a person,
  is never marked, by this Task or any other following it; Fleet never puts
  a change back into draft.
- **Policies.** A policy is a named set of `checksPassed` (passed, or
  checkless for `noCheckGraceMs` since the head was first seen),
  `botReviewsAddressed`, `noUnresolvedThreads` and `forgeMergeable`;
  `spec.forge.draft.policy` selects one, else `forge.draftPolicy`, and
  `null` selects none. Any other key is refused at config load, a Task
  naming an undefined policy is refused at apply, and a policy removed from
  the config afterwards raises `NeedsAttention/PolicyUnknown`. A policy that
  waits on bots relays their blockers whatever `relayBots` says. `fleet
  init` writes `checks`, `checks-and-bots` and `strict`.
- **Moved is not an attempt.** The adapter re-reads head and state
  immediately before the mutation and answers `Moved` when either differs,
  mutating nothing; the record returns to `none` without counting, and that
  head is not tried again while the mirror still shows it. The window
  between the fresh read and the mutation is the forge's.
- **Retries.** A Known refusal waits `RETRY_DELAY_MS` on the entry's own
  `notBefore`, holding back no other entry; at most `MAX_ATTEMPTS` (3)
  intents are written against one change, then `NeedsAttention/MarkReadyFailed`
  and no more.
- **Unknown is a question.** An Unknown outcome, or an intent whose receipt
  never landed, is answered by the change's next observation made after the
  intent: ready is a recovered receipt (`done`), still a draft is a failed
  attempt. The effect is never repeated to find out.

Conditions, under `status.forge.conditions`: `Following`, `Settled`,
`NeedsAttention` (`RelayGap`, `ChangeGone`, `MarkReadyFailed`,
`PolicyUnknown`; `Multiple` when more than one is open, with one line per
entry in the message; False with `Resolved` once all clear).

## ChangeRequest

```text
spec.url             the change's URL, the only forge-specific input
spec.pollMs          poll interval while Watching (default 60000)
spec.closedGraceMs   how long a closed change stays watched (default
forge.closedGraceMs)
spec.settledTtlMs    how long a settled mirror is retained (default
forge.settledTtlMs)
```

The spec is fixed once created. `fleet apply` parses the URL through the
adapter of its host (a host not under `[forge.hosts]` is refused), derives
the name `cr-<12 hex of sha256(<kind>/<host>/<project>/<id>)>` when the
manifest gives none, and reuses an existing object only when it mirrors the
same change: its `status.ref` once written, else its own URL parsed. See the
[resource](resources/change-request.md),
[state machine](controllers/change-request.md#changerequest-state-machine) and
[interface](controllers/change-request.md#changerequest-controller-interface).

ChangeRequestController is a pure function from the ChangeRequest and one
observation of its change to a plan: the status to write and, after
retention, whether to delete its own object. It has no effects and no
finalizer. Guarantees, each with its test in `change_request_controller` and
`e2e_forge`:

- **One complete observation, or Unknown.** The adapter answers Known only
  when it read every page the forge holds; a timeout, a throttled answer, an
  incomplete or unparseable read or a missing login is Unknown with its
  reason, leaves the snapshot untouched, and waits one interval or the
  forge's `retryAfter`. Unknown never means closed, merged, empty or gone.
- **Numbered on first sight.** `revision` moves only when the snapshot
  differs; every note first seen, edited or unresolved, and every change of
  state, head (id, checks or failing) or review decision takes that
  revision as its `seq`. An unchanged poll writes `observedAt` and
  `notBefore` only. The newest 100 notes are kept; `droppedBefore` rises to
  the `seq` of a stored note rotated out.
- **Cadence.** Watching polls every `pollMs`; a closed change is polled ten
  times more slowly for `closedGraceMs`, reopens into Watching with a new
  `state.seq`, or settles (`ClosedGraceExpired`). Merged settles (`Merged`);
  proven absence settles (`Gone`). A settled object is not polled.
- **Retention.** `settledTtlMs` after settling, the controller plans
  `delete`; the manager performs it by `uid` and `resourceVersion` under the
  lease (`Store::delete_fenced`), so a recreated or rewritten object is not
  removed. A person may delete at any time; nothing on the forge changes.
- **Host-wide deferral.** The sweep visits ChangeRequests by host then name;
  a `retryAfter` from one defers every later one on that host without asking.
- **Nothing acted on.** `mergeable` is the forge's own answer, reported as
  the `Mergeable` condition and never acted on. The port has no merge method.

Conditions: `Reachable`, `Mergeable`, `Settled`.

## The Conversation mirror

ConversationController is a pure function from the Conversation, the
deliveries the receive verb spooled on it, one read of the thread's
history, the account's capabilities, the Task it is tied to or is creating,
and the Task a Fleet-opened thread's marker names, to a plan: the whole
status, with the spool drained of what it numbered and the intent of every
effect it asks for; at most one Task to create, under the route's
`exclusive: {workspace}` precondition; the refusal and busy-notice posts;
and after retention whether to delete its own object. It has no finalizer.
Every create and post is a keyed effect under `status.effects`: the manager
writes the receipt back under the key, a key with a Known receipt is never
performed again, and a missing or Unknown receipt is answered by looking
for the Task, or for the post's marker in the mirror, never by repeating
the effect blindly. Guarantees, each with its test in
`conversation_controller`, `e2e_gateway`, `e2e_gateway_wildcard`,
`e2e_gateway_dm` and `store_contract`:

- **Receive.** `fleet gateway receive --account NAME` reads one envelope
  from stdin, refuses a `verified` that is not the account's `inbound`,
  reads the payload through the adapter's pure `event`, ignores an event in
  a channel no route covers (a listed channel id wins over the account's
  one DM route, `"dm:*"`, for a delivery with `direct`, which wins over its
  one wildcard route, `"*"`; neither covers the log channel; an edit or
  removal without `direct` is `direct` when its Conversation records
  `spec.direct`), records `spec.direct` on a Conversation the DM entry
  covers, creates
  the thread's Conversation under
  `conv-<12 hex of sha256(<account>/<address>)>` only for an event that
  could start a Task, earn a refusal, or is Fleet's own keyed post, and
  spools the delivery under its delivery id through `receive_event`, as
  `listener/<account>`, without the lease. A redelivery is a no-op, the
  spool holds at most 100, and the exit codes are 0 (written, duplicate or
  ignored), 1 (refused: full, deleting, the wrong `verified`, an account
  the config lacks), 4 (an unreadable envelope, payload or record) and 5
  (a conflict the retries did not resolve). Every envelope refused or
  unreadable is counted by reason on the account's Listener, created by the
  verb if the manager has not yet, with `lastFaultAt` for the listener's
  own faults.
- **Numbered on first sight.** The first pass reads the thread's history
  (the newest 200) where the account has it, and numbers it with the
  spooled events in one revision, oldest first by the platform's time:
  each message's `seq` and `firstSeq` are that revision and `position` its
  place. A message seen from the listener and from history is one message.
  A newer edit moves `seq` and keeps `firstSeq`; an older edit delivered
  late changes nothing; a removal leaves the mirror. The newest 200 are
  kept and `droppedBefore` rises to a rotated message's `seq`; the newest
  500 delivery ids numbered are kept so a redelivery after the drain is
  still a no-op. The spool is numbered even while the history is Unknown;
  the read is retried after the platform's `retryAfter` or one `pollMs`.
- **Backstop.** While Tied and active (a message within `activeMs`), the
  history is read again every `pollMs` after the newest message; a changed
  text is an edit. Capabilities are asked once per account per sweep, and
  a platform without history is never read.
- **Tied until readably gone.** A Conversation whose `status.task` names a
  created Task asks for that Task every pass; readable absence, or another
  Task under the name, makes it Orphaned with `Tied/TaskGone`, the messages
  kept. An Unknown Task changes nothing.
- **End of life.** A thread retires once it had no message for
  `gateway.retainMs` (seven days by default) since the later of its last
  message, its orphaning and its creation, and is Untied, Orphaned, or
  Tied to a Task that is Concluded. `status.retiring` records the start
  (`at`, `revision`); a message numbered since, or a Task no longer
  Concluded, ends it. Its steps, each a keyed effect under
  `conversation/<uid>/retire/<at>/…`, run in order and resume after a
  crash at the first without a settled receipt:
  1. `DeleteTask` (`…/task`): the store's `retire_conversation_task`
     deletes the Task fenced on its `uid` and the lease, only when it
     carries this Conversation's uid label and is Concluded; its
     finalizer then releases the session, and the thread orphans.
  2. `RemoveDir` (`…/dir`): only when `status.dir` records the directory
     the manager made; the manager lists the Tasks first and answers
     `WorkspaceBusy`, uncounted and tried again one `pollMs` later, while
     one names it, then removes it through the runtime's `remove_dir`.
     Its `Deleted` receipt clears `status.dir`.
  3. `delete`, by `uid` and `resourceVersion` under the lease.

  A refused step is tried again after `RETRY_DELAY_MS`, three times, then
  `NeedsAttention/RetireFailed` holds the thread; an Unknown one is
  performed again one `pollMs` later, which is harmless. An Unknown Task
  read waits. A thread whose Task waits for its directory never retires,
  nor does one whose Task runs; it is looked at again every `pollMs`.
  While the tied Task is being deleted, new messages wait to be judged
  until the thread is Orphaned, so none is lost in a Tied thread.
- **One write.** The status lands through `record_received`, which checks
  the controller's fields as `update_status` does and lets `received`,
  which nobody owns, only lose entries, never gain or alter one.
- **Admission.** Every message numbered above `judged` is judged once, by
  `admit` over the route and the people table, and only a message the
  listener delivered (`via` `connection` or `signature`) can start a Task
  or earn a refusal: one read from history alone is context. A delivery of
  a message a history read numbered first takes over its `via`, and in an
  untied thread numbers it again, so it is judged once. Judging waits
  for the thread's starter where the account has history, for the Task a
  Fleet-opened thread's marker names, and for the route while it left the
  config; `judged` then moves to the revision judged.
- **Start.** In an Untied thread, an admitted message that meets
  `route.mention` (`required`: it mentions Fleet; `top-level`: it is the
  thread's starter), or with `spec.direct` is the thread's starter or
  mentions Fleet, starts the thread's next Task, `<conversation>-<n>`,
  exactly the interface page's JSON (the route's `taskConfig` and brief,
  the thread's workspace, `dispatch` true, the three labels), under
  `conversation/<uid>/task/<n>` with the tie `pending` and the trigger
  kept; in an Orphaned thread any admitted message does. One Task per
  pass, and never while a tie is pending, busy or created. A thread whose
  starter carries a marker naming a Task that exists with that uid is tied
  to it (`n` 0) and starts nothing; one whose Task is readably gone, or
  whose starter was removed, may; one whose Task is Unknown, or whose
  starter is Fleet's without a readable marker, ignores mentions.
- **Workspace.** On a route whose workspace is `{base}`, that directory is
  a base: every Task of one thread gets `["file://<base>/<conversation
  name>"]` (`conversation::thread_dir`, computed purely), and the create's
  `exclusive` names that directory. A URL list is the route's, given to
  each Task as it is, with no precondition. The manager lists the store's
  Tasks under the lease immediately before the fenced create and answers
  `WorkspaceBusy` naming the Task whose first URL is `file://` and names a
  directory that normalises to the thread's,
  creating nothing. Otherwise it makes the directory, with its missing
  parents, through the runtime's `create_dir` on the Task's host (its
  `host`, else its TaskConfig's, else `local`; an absent TaskConfig is a
  `Failed` create), then creates the Task: a directory the runtime refuses
  is a `Failed` create, and one it cannot tell about is Unknown, so no Task
  names a missing directory. When `create_dir` answers that it made the
  directory, the manager records it as `status.dir` (`path`, `host`) in the
  same write as the create's receipt, whatever the create answered; a
  directory that was there already is not recorded. A crash after the
  runtime made the directory and before that write leaves it unrecorded, so
  Fleet keeps it: the failure keeps a directory, never removes one. On
  `WorkspaceBusy` the tie turns `busy`, `NeedsAttention/WorkspaceBusy` is
  raised, one notice is posted under `conversation/<uid>/busy/<n>`,
  retention is held, and the create is planned again under the same key
  every `pollMs`, not counted as an attempt, until it lands. A busy tie
  whose trigger message left the mirror is withdrawn, and the next Task
  takes the next number.
- **Adopt.** A create with no receipt, or an Unknown one, asks for the Task
  under the name: one carrying this Conversation's uid label is adopted
  (`Created { adopted }`), one with another label is a refusal, readable
  absence is one more attempt under the same key, and Unknown waits. Three
  refused creates raise `NeedsAttention/TaskCreateFailed` and stop; a
  refusal waits `RETRY_DELAY_MS` first. Only the controller's fenced create
  may set the `gateway.fleet.dev/conversation` labels, and nothing changes
  them afterwards.
- **Refuse.** A person not on the allowlist who mentions Fleet, or with
  `spec.direct` starts the thread, gets one refusal post per author and
  thread, the route's `refusal` or the default, under
  `conversation/<uid>/refuse/<author id>`; past 200 refused authors none is
  sent. A refused post is tried again under the same key after
  `RETRY_DELAY_MS`, at most `MAX_ATTEMPTS` times, then
  `NeedsAttention/PostFailed`. An Unknown post is found again by its marker
  in the mirror, or by one `since` read where the platform reads markers
  back, a complete read without it being one more attempt; where it cannot
  be found, `NeedsAttention/PostUnconfirmed` and no repeat.
- **Reactions at admission.** Where `gateway.reactions` is on and the
  account's capabilities say `reactions`, a create that answers `Created`
  marks its trigger `working`; one that answers `WorkspaceBusy` marks its
  trigger, and each later admitted message the listener delivered while
  the create waits, `queued`. The pass that ties the Task marks each queued
  message working: the working reaction added, the queued one removed; a
  later pass only removes the queued reaction of a message still marked
  queued. Admission's working reaction is tried once, never again, and
  its record is kept while the mark stands. A wait withdrawn, or a queued
  message that left the mirror, loses the queued reaction; a tie that ends
  forgets the working marks. The marks are `status.reactions.marks`, and
  each reaction a keyed effect under
  `conversation/<uid>/react/<message id>/<queued|working>/<on|off>`, best
  effort as GatewayController's: a queued reaction tried at most three
  times, none looked for, each dropped once settled, given up or made
  pointless, holding back nothing and raising no condition. Unknown
  capabilities change no mark. Tested in `conversation_controller`'s
  `reactions` module and `e2e_gateway_reactions`.

Conditions: `Reachable`, `Tied` (`TaskKnown`, `Starting`, `Waiting`,
`Untied`, `TaskGone`), `NeedsAttention` (`TaskCreateFailed`,
`WorkspaceBusy`, `RouteUnknown`, `MentionsOnly`, `PostFailed`,
`PostUnconfirmed`, `RetireFailed`).

## Talking in a conversation

GatewayController is a pure function from the Task, the Conversation its
labels name and the Tasks whose parent labels name it, to a plan: the
desired `status.gateway`, the inbox entries to land with it and its post,
open, receive, log and reaction effects. It has no finalizer, and never
writes a Conversation.
Guarantees, each with its test in `gateway_controller`,
`e2e_gateway_controller` and `store_contract`:

- **The tie is verified, never assumed.** A Task carrying the
  `gateway.fleet.dev/conversation` and `conversation-uid` labels is
  `waiting` until the Conversation of that `uid` records a created tie
  naming this Task's `uid`, then `tied`. A Conversation readably absent,
  recreated under another `uid`, or tied to another Task puts the tie back
  to none with `NeedsAttention/ConversationGone` and `Tied/ConversationGone`;
  the cursor stays, and the tie returns when the Conversation names the
  Task again. Unknown changes nothing.
- **Join at the beginning.** The cursor starts at `0`, so the first relay
  carries every retained message, oldest first, under
  `task/<name>/<uid>/gateway/<conversation uid>/<revision>` from
  `gateway/<conversation>`, with `about` naming the Conversation's `uid`
  and `revision`; a mirror that already dropped older messages says they
  remain on the platform.
- **Relay by cursor, in one write.** Every message with `seq` above the
  cursor goes out as one entry, and the cursor and `seen` advance to the
  revision in the same `record_relay` write as the entry and its answers,
  or not at all. An edited message is relayed again, marked as an edit.
- **Admission.** Fleet's own posts are never relayed. A message from
  someone off the route's allowlist is context: it waits, and goes out
  first, under a header marking it as content, with the next admitted
  message; context alone advances `seen` and not the cursor. A route the
  config no longer has raises `NeedsAttention/RouteUnknown` and relays
  nothing.
- **Bounds.** An entry keeps the newest messages under 64 KiB and counts
  the older ones out, as still on the platform. Messages rotated out of
  the mirror above the cursor count a gap and raise
  `NeedsAttention/RelayGap` until the next relay that carries something.
  More than 20 entries still to deliver hold the relay without moving the
  cursor or `seen`. One pass plans no more appends than the store accepts
  in turn: the relay first, then each child's items in order; a child's
  counters stop at the first item left, and the child stays due.
- **Answers.** A relay carries `answers` for each Question posted in the
  thread, not yet answered, that an admitted message was written after.
  The store records every `answers` id of any append (`fleet send
  --answers ID`, a plugin's append, a relay) in `status.answered` as
  `{key, from, at}` in the same write, first answer wins, refuses an id
  that names no declared Question, and lets no status write change the
  field.
- **Children.** The parent's controller lists the Tasks whose
  `gateway.fleet.dev/parent` and `parent-uid` labels name it, from the
  sweep's one Task listing, and relays each of a child's Questions,
  Comments, Drafts and ChangeRequest declarations and each result
  generation as one entry from `gateway/child/<child>` under
  `task/<name>/<uid>/gateway/child/<child uid>/<index or result/generation>`,
  recording `{uid, seen, declared, result}` per child. A child tied to its
  own verified thread is left alone; more than 200 children raise
  `NeedsAttention/TooManyChildren` and none is relayed. The store accepts
  the parent labels only when the child is created, only together, only
  for a parent that exists with that `uid` and is not being deleted (an
  unreadable parent is Unknown, to be retried), and only from a writer
  that may already append to the parent's inbox: a person, a `tasks =
  "any"` plugin, or a `tasks = "own"` plugin that created the parent. They
  never change afterwards.
- **Reactions are best effort.** Each admitted message a relay delivers,
  with no mark yet, is marked working and gets the working reaction, where
  `gateway.reactions` is on and the account's capabilities say
  `reactions`. A message the Conversation marked working at admission,
  whose working reaction landed there, is marked working with no reaction
  of the Task's; any other is reacted to as usual. A thread
  post that lands marks every working message replied: the replied
  reaction is added and the working one removed.
  Each reaction is a keyed effect under
  `task/<name>/<uid>/gateway/react/<message id>/<working|replied>/<on|off>`,
  tried at most three times, never looked for, and dropped once settled or
  given up. A reaction holds back no relay and no post, and raises no
  condition. A message that leaves the mirror, a thread that is readably
  gone and reactions turned off forget the marks. Tested in
  `gateway_controller`'s `reactions` module and `e2e_gateway_reactions`.
- **Quiet costs nothing.** A tied Task is due only when its Conversation's
  revision moved past `seen`, its `uid` changed or it left the listing;
  a parent only when a child's `resourceVersion` moved; a Task with the
  labels and no verified tie, every pass. A pass that finds nothing new
  writes nothing.

Conditions, under `status.gateway.conditions`: `Tied` (`Verified`,
`Waiting`, `Opening`, `Opened`, `Abandoned`, `ConversationGone`),
`NeedsAttention` (`RelayGap`, `ConversationGone`, `RouteUnknown`,
`TooManyChildren`, `PostFailed`, `PostUnconfirmed`, `RecoveryIncomplete`,
`DeclarationInvalid`, `ReceiveFailed`).

ListenerController is a pure function from the Listener, the account as
the config has it with its digest, and the platform's `whoami` and
`capabilities`, to the three conditions: `Accepted` (`Loaded`, or
`NoRoute`), `Resolved` (`IdentityMatches`, `IdentityMismatch`, or Unknown
`Unreachable`), and `InboundReady` (`Connected`; or `NoListener`,
`Connecting`, `Closing`, `ListenerStale`, `ListenerConfigOld`,
`DeliveriesRefused`), judged from the beats `fleet gateway receive --beat`
recorded, stamped by the verb's clock, against three `gateway.beatMs` and
the current digest. The manager creates one Listener per configured
account when it loads the config, the verb creates it when a beat comes
first, and the controller deletes one whose account left the config.
`listener_controller` and `e2e_gateway` hold these.

## Store

A data directory holds `store.json` (format and version), `lock`, `seq`,
`lease.json`, `objects/<Kind>/<name>.json`, `boards/<name>/` and `tmp/`. The default is the
platform data directory (`$XDG_DATA_HOME/fleet`, `~/.local/share/fleet`,
`%LOCALAPPDATA%\fleet`); a non-empty directory that is not a store is
refused.

- Writes go to a synced temp file, then a hard link (exclusive create) or a
  rename (replace), then the directory is synced.
- Every write holds the store lock, an OS file lock that a writer waits for,
  and checks `resourceVersion`.
- A missing record is `Known(None)`; an unreadable, corrupt or misfiled one is
  Unknown and blocks writes to that name. A list with one bad record is
  Unknown as a whole.
- Each status field has one owner, checked on every write, except
  `Task.status.inbox`, `status.answered` and `status.wakeRequestedAt`,
  which have none and which no status write may change. A controller writes only under the current
  manager lease: an expired or superseded token is refused, and the token
  only grows.
- `status.inbox` is reached through three typed mutations, each named by
  its caller and compare-and-swap under the lock: `append_inbox` (`cli`,
  through the manager's `send` verb, no lease; or `forge-controller` or
  `gateway-controller`, under the lease) refuses a key already in `inbox`
  or `deliveries`, an `about.revision` at or below the mirror's
  (`about.uid`) highest delivered one, an append while more than 20
  entries are still to deliver, a deleting Task, a source not under the
  caller's prefix (`cli/`, `forge/`, `gateway/`), and an `answers` id that
  names no declared Question, and records the entry's `answers` in
  `status.answered`, first answer wins; `record_relay` (ForgeController,
  GatewayController or TaskController, under the lease) replaces the
  writer's own field, `status.forge`, `status.gateway` or
  `status.subscriptions`, and appends each listed entry under the same
  checks (TaskController's source prefix is `board/`), in one write that a
  single refused entry fails whole;
  `record_delivery` (TaskController,
  under the lease) writes the controller's status and, in the same write,
  the receipt into the newest-100 `status.deliveries` and the entry out of
  `inbox`, refusing a key no entry holds. None changes another field;
  `store_contract` races appenders against the remover and tears the
  record.
- `status.wakeRequestedAt` is reached through one typed mutation,
  compare-and-swap under the lock: `request_wake` (`cli`, through the
  manager's `wake` verb, no lease) sets it to the time of the request and
  refuses a deleting Task. It changes no other field.
- A Conversation's `status.received`, a Listener's `beats`, `refused`
  and `beatRevision`, and a Board's `status.head` and `status.oldest` have no
  owner either; only the store's typed `append_events` writes the Board's
  two, as the next bullet says. `receive_event` (the account's
  listener, no lease) spools one delivery, a duplicate id changing nothing
  and the hundred-and-first refused; `record_received`
  (ConversationController, under the lease) writes the controller's status
  and lets the spool only lose entries; `record_beat` and `record_refusal`
  (the account's listener) replace one instance's beat, evicting the oldest
  past eight, or count one refusal, each moving `beatRevision` and nothing
  else. A listener writes only the objects whose spec names its account.
  `create` takes `apply`, or the account's listener for a Conversation or
  a Listener, and checks a Task's parent labels under the lock, as
  [talking in a conversation](#talking-in-a-conversation) says; a spec
  update may not change them. `create_fenced`, under the lease, takes the
  Conversation's controller for the Task a thread starts, the one create
  that may carry the `gateway.fleet.dev/conversation` labels, which no
  other create or spec update sets or changes. It also takes the Gateway
  controller for the Conversation of a thread Fleet opened, and
  `receive_event` takes the Gateway controller for Fleet's own opening post
  there, a delivery whose `verified` is `fleet`, and nothing else.
  `create_fenced` also takes the CronTask's controller for the Task a
  scheduled time starts, the one create that may carry the
  `crontask.fleet.dev/` labels, which no other create or spec update sets or
  changes. `retire_scheduled_task`, under the lease, deletes such a Task
  only with its `uid` and its CronTask's uid label, and only once it is
  Concluded unless the CronTask's policy is `Replace`.
- A Board's log is `boards/<name>/<seq>.jsonl` segments, one event per line,
  each segment named by its first `seq`, and a `<seq>.keys` file beside each
  with the `(source, key, seq)` of its keyed events. `append_events` is the
  only writer. `cli` calls it as `cli/<user>`, through the manager's
  `publish_event` verb, with no lease. TaskController calls it as a Task
  (`Source::Task`), under the lease, as the next bullet says. Under the
  lock it checks each event
  (`event::validate_publish`: the type grammar, the refused `fleet.` prefix,
  256 characters for `subject`, `key` and `correlation`, the Board's
  `maxDataBytes`) and refuses the batch whole when one fails. It then drops,
  from the oldest, each segment whose events are all older than
  `retention.maxAgeSeconds`: a segment goes when the next segment's first
  event is that old, and the newest segment when its last event is. A publish
  whose key the same publisher (`Source::same_publisher`: a Task by its
  name and uid, whatever its lineage) used in a kept segment's keys file
  (or the newest segment itself) appends nothing and answers that event, so a key whose event
  aged out can be used again. The others get the next `seq`s, from
  `status.head + 1` with no gap, and the store's time. Each line is appended
  and fsynced, then its key line; each append cuts a torn last line off the
  newest segment, rewrites its keys file from it when they differ, and
  rebuilds a missing or torn keys file of an older segment from that
  segment. A reader skips a torn line. The log, not the status, decides the
  next `seq`, so an append that crashed before its status write keeps its
  `seq`, and the write that recovers it corrects `status.head` and
  `status.oldest`, even when it appends nothing. The directories are created
  `0700` and the files `0600` on Unix, and each new directory's parent is
  fsynced. After the append, retention drops whole segments from the oldest
  while the log holds more than `retention.maxCount` events, never the
  newest. Then `status.head` and `status.oldest` move, and the dropped files
  go. Stray files in the directory are ignored. No other writer, and no
  status write, changes a Board's status. Retention runs only on a publish.
  `read_events(board, after, limit)` takes no lock: it answers
  `Expired { oldest }` when `after + 1` is below the first `seq` the log
  holds, and otherwise the events above `after`, oldest first; a segment
  that retention drops during the read makes it read again. Creating or
  deleting a Board removes any log left under its name. `store_contract`
  tears a line and a keys file, drops a status write, ages segments out and
  leaves stray files.
- A session's outbox is `event::outbox_file` of its signal file:
  `<signal file>.events`, on the same place. Each spawn that gets
  `FLEET_SIGNAL_FILE` gets it as `FLEET_EVENT_FILE`. Inside a session,
  `fleet event publish` checks the line without a store (`OutboxLine::encode`:
  the Board's name, `event::validate_publish` without the Board's
  `maxDataBytes`, at most `event::MAX_OUTBOX_LINE` bytes) and appends it in
  one write. A new outbox is mode 0600 and a new signals directory 0700 on
  Unix, whoever makes it (`FileSignals::open`, `fleet signal`, `fleet event
  publish`). A session with `FLEET_SIGNAL_FILE` and no `FLEET_EVENT_FILE`
  refuses, and never falls back to `cli/<user>`. On each pass, before the
  Task's controllers, the manager reads the outbox of each Task whose
  `status.session` is set (renewing the lease first when the read goes to
  another host), unless its session is `Gone`,
  `status.outbox.closed` is set and the Task is not being deleted. A
  `Concluded` Task that is not being deleted is read only when the manager's
  `due` test holds for it (its `SESSION_RECHECK_MS` recheck, or a watched
  activity). A read
  of a `Gone` session that finds every whole line in sets `closed`; a read
  of a session that is not `Gone` clears it. It reads from
  `status.outbox.offset`, at most `event::MAX_OUTBOX_READ` bytes: through
  the signal port on this machine, or through the runtime's `exec`
  (`fleet signal-read --from <offset> <file>`) on a place. Both reads give
  each byte that is not valid UTF-8 as NUL, so the bytes cross the runtime
  as text with every offset kept; JSON takes no raw NUL, so such a line is
  skipped and counted. Both open the outbox only as a regular file: on Unix
  without blocking and without following a symlink, so a FIFO or a symlink
  the session put there reads as Unknown. The pure
  `event::ingest` turns the whole lines into publishes; a last line without
  its newline waits. A line over 64 KiB is skipped whole: one skipped before
  its newline sets `status.outbox.discarding`, and the next reads pass over
  its bytes up to that newline. A file shorter than the offset was made again: it is
  read from 0 under the next `generation`. A line without a key gets
  `outbox-<generation>-<offset>`. The source is `event::task_source` of the
  Task: its name, uid and parent-label lineage; a line's own fields never
  name it. Consecutive lines for one board are appended together as
  TaskController under the lease, at most 256 in one `append_events`: one
  lock, one lease renewal and one fsync per segment. A batch the store
  refuses whole (`Invalid`, `NotFound`, `Forbidden`) is appended again one
  line at a time, so only the refused line is skipped. Only then does
  `status.outbox` (TaskController's) move past it, so a
  crash between the two re-reads lines whose keys dedupe them. A cursor
  write that conflicts with another writer, such as the worker, re-reads the
  Task and tries again, up to 5 times. Delivery is at least once: when a
  crash falls between the appends and the cursor write, and the same appends
  made retention drop a re-read line's key (a `retention.maxCount` below one
  read's batch), that line is appended again under a new `seq`. A line that
  does not parse, or that the store refuses (`Invalid`, `NotFound`,
  `Forbidden`), is skipped: `status.outbox.skipped` counts it and
  `lastSkipped` says why. A new generation clears `lastSkipped`, because
  its offset names a line in a file that is gone. Any other store error
  stops the pass's ingest at that line. An outbox that cannot be read is Unknown: the offset stays and
  nothing is concluded. The ingest also answers the `outbox_drained`
  observation: Known(true) when every whole line is in (or there is no
  outbox), Known(false) when the read hit its cap or the status write
  conflicted, Unknown when the outbox or the store could not tell. After a
  Known `Released` receipt, TaskController keeps the finalizer until it is
  Known(true): `Released=False/OutboxDraining` with no limit, or
  `Unknown/OutboxUnread` for `RELEASE_GRACE_MS` from its first unread pass
  (the condition's `lastTransitionTime` while its reason is `OutboxUnread`;
  a first `OutboxUnread` is a transition even from `Unknown/Releasing`), after
  which the unread lines are given up (`Released=True/OutboxGivenUp`).
  The release stopped the session, so the read after it is the last one;
  a torn last line then never completes and is left. `e2e_event_board` drives it through the real
  binary.
- A Task's subscriptions are `spec.subscriptions`, which a spec update may
  change like `dispatch`; `subscription::validate` refuses more than 16, an
  invalid Board name and two with the same Board and filter. The manager's
  `subscribe` and `unsubscribe` verbs edit the list, compare-and-swap, as
  `apply`. On each pass, after the outbox step and before the Task's
  controllers, the manager reads the subscriptions of each Task that is not
  being deleted, `Concluded` or `Releasing`. Per subscription it reads the
  Board (`Unknown` keeps the cursor as it was, and the pass reports it) and
  places the cursor (`SubscriptionCursor::place`): the old one while the
  Board keeps its uid; `after`, else `event_head`, for a new one; `0` for a
  Board made, deleted or made again since. A subscription whose last notice
  is still in `status.inbox` and not given up on is not read, and nor is
  one when the inbox holds more than 20 entries still to deliver, or one
  whose Board's `status.head` is not above the cursor (a Board with no
  `status.head` yet counts as head 0; a log a crash left ahead of its status
  is read at the next append, which corrects it). Otherwise `read_events`
  reads from the cursor up to the head, at most 10000 events, and the
  lease is renewed before each page; a lost lease ends the sweep;
  `Expired { oldest }` moves the cursor to `oldest - 1` and marks the
  notice; a read error keeps the cursor as it was. A read with a match, or
  an expiry, owes one `event-notice` (`subscription::notice_entry`): from
  `board/<board>`, under `task/<name>/<uid>/notice/<n>` with `n` from
  `status.subscriptions.notices`, with `notice` (`board`, `count`, `first`,
  `last`, `expiredBefore`) and a text that gives the `fleet event list`
  command, each filter as `--flag=value`, so a value that starts with a
  dash stays a value. The cursors and the notices land in one `record_relay` as
  TaskController under the lease, so a stop between them cannot happen; a
  conflict leaves both for the next pass. A cursor of a subscription no
  longer in the spec is dropped. `e2e_event_subscriptions` drives it through
  the real binary.
- Deleting marks a Task while its finalizer remains; only its controller,
  holding the lease, removes the finalizer, and only from a deleting Task.
- A controller deletes its own object (`delete_fenced`) only with the
  object's `uid` and current `resourceVersion` and the lease; a
  ChangeRequest, a Conversation and a Listener have no finalizer, so they
  go at once.

## Runtime contract

A Session is one running agent (`fleet_api::session`): the unit every
call below starts, observes, briefs, sends to or releases. A Task owns at
most one, named by `status.session` (`SessionRef`: the runtime's `id`, the
`host` and Fleet's deterministic `key`). `status.sessionState` is what Fleet
last proved about it, folded by `SessionState::observe` from the Known
answers of `find`, `lost` and `paused`, and the activity the agent's hooks
reported through `fleet signal` (see [signal contract](#signal-contract)): `Gone` when a
readable list lacks the Session, else `Lost`, else `Paused`, else `Idle`
or `Running` by activity, else `Running` on any proof of presence. Unknown
answers keep the last state. Nothing decides on the state; a view reads it.

`fleet_api::runtime::Runtime`: `host`, `create_dir`, `remove_dir`,
`prepare_execution`, `env_source`, `spawn`, `find`, `ready`, `workspace_trust`,
`brief`, `brief_delivered`, `at_rest`, `release`, `lost`, `revive`,
`paused`, `pause` and `resume`, each answering Known or Unknown, and
`screen`, which returns a session's `capture` text and
title. A runtime reports no activity: the agent's hooks do, and below them
Fleet's screen rules read the screen `screen` returns
([agent state](#agent-state)). `revive`
and `resume` take the conversation the hooks last reported, which the
harness profile's resume argv names, and the spawn's launch (harness,
workspace, environment), for a runtime that starts the argv itself; the
manager completes it with the environment it froze and the signal file, as
for a spawn. `create_dir` makes a directory with its missing parents on a host
and says whether it made it; `remove_dir` removes one and what it holds, a
directory already gone included. The exec adapter runs each as a bounded
command on the host, through the runtime's `exec` verb. Each adapter reports its longest single operation
(`operation_bound`), and `fleet reconcile` refuses a lease that could not
outlast it.

- **exec:** every call is one bounded `<program> <verb>` of the
  [runtime protocol](../runtime/README.md#the-runtime-protocol), the program
  chosen by the host. `spawn` is `start` of the harness profile's launch argv
  with the spawn's environment, `FLEET_SIGNAL_FILE` included; `revive` and
  `resume` are `start` of its resume argv. A session's id is its runtime key,
  `<keyPrefix>.<uid>`, so a restart keeps it. `ready`, `workspace_trust` and
  a running agent's `at_rest` read the agent's signals; `lost` is an
  `exited` process. `release` is `stop` with `release: true`. `brief` is
  `input` of the text, and only the agent's prompt hook proves it.
  `pause` is the [`pause` verb](../runtime/README.md#pause), and `paused` is
  an `inspect` that answers `paused`, when the runtime's `manifest` declares
  the verb; on any other runtime `paused` stays Unknown. It passes no model to its
  harness, so a spawn with `spec.model` fails.
  `e2e_runtime_exec` drives the real `runtime-mux` through it.
- **fake:** a file-backed host for tests and trials, with injectable faults
  (lost answers, refusals, delays, hangs). It runs no process: it records
  the harness profile's launch and resume argv, and plays each agent's
  prompt hook by appending to its signal file, unless silenced.

The runtime makes every workspace (see
[Workspace contract](#workspace-contract)) before the first start: the
spawn's `ready` names its directories, and the runtime runs the session in
the first and makes nothing at the start. Every adapter keeps these rules:

- **Making.** The runtime decides how a URL becomes a directory: a
  worktree, a clone, a copy or a restored snapshot. Branches, bases, source
  checkouts and credentials are the runtime's. Fleet sends none.
- **Whole or nothing.** If a `workspace` call stops partway, the runtime
  rolls its partial state back on the next call under the key.
- **Cleanup.** `release` deletes the session, and `CleanWorkspace` releases
  its workspace through `stop` with `release`. Fleet never inspects or
  deletes a workspace itself, with one exception: a Gateway thread's
  directory that the manager made, which it removes when the thread retires
  (see [the Conversation mirror](#the-conversation-mirror)). Fleet never
  removes a directory it did not make or a route's base directory.
- **Refusal.** A workspace the runtime cannot make is `Known(Failed)`,
  never Unknown. A URL it does not handle is `Known(Unsupported { url })`.
  A runtime can answer `Known(TrustUnmapped { harness })` at spawn for a
  harness it knows no workspace-trust interface for. No shipped runtime
  answers `TrustUnmapped`.

The exec adapter starts a session in the first of the workspace's `dirs`,
and the harness profile adds one `--add-dir` per other directory. A session
with an empty list starts in the runtime's default directory. A spawn of a
non-empty list whose workspace is not made fails.

### Workspace trust

An agent harness can refuse to load a folder's project settings until a
person trusts the folder. This document calls that gate workspace trust.
Workspace trust is not a permission policy. It approves no command, edit or
tool, and Fleet changes no permission mode or sandbox to obtain it.

The trust root is the directory the agent keys the session's trust on:

- The trust root is the first directory of the workspace the runtime made:
  the directory a `file://` URL names, or the worktree of a `git+` URL.
- A session with an empty list has no trust root.
- The other directories get no trust. Neither agent loads their project
  settings.

The [harness profile](controllers/task.md#harness-profiles) gives each
harness its trust at spawn:

| Harness | At spawn |
| --- | --- |
| `codex` | The launch argv adds `-c projects={"<root>"={trust_level="trusted"}}`. This override trusts the root for this session only. |
| `claude`, `claude-code` | Nothing. Claude Code has no per-session trust interface. |
| `pi` | The launch argv adds `--approve`. pi trusts the project's own resources for this run only. |
| any other | Nothing. |

The exec runtime answers `workspace_trust` from the agent's signals and
screen, in this order:

1. An agent that reported a state other than its start is `Trusted`. Hooks
   run only in a trusted workspace.
2. Signals that cannot be read are Unknown.
3. Before the first report, the runtime reads the screen through `capture`.
   A Codex, Kimi Code, Copilot or Claude Code trust dialog is `Untrusted`
   with the gap `Unsupported`. Its message asks a person to trust the
   directory once in the session.
4. Codex's initial empty composer, with Codex in the foreground, is
   `Trusted`: the launch override trusts the workspace.
5. Kimi Code's empty composer, with Kimi in the foreground, is `Trusted`:
   Kimi asks its folder trust before it shows the composer.
6. GitHub Copilot CLI's empty composer, with Copilot in the foreground, is
   `Trusted`: Copilot asks its folder trust before it shows the composer.
7. Any other screen, or a screen that cannot be read, is Unknown.

The fake runtime answers from each session's `trusted` flag and its
`workspaceTrust` fault. The port also defines the gap `Mismatch`, for a
session that is not the one the runtime would start for its workspace. No
shipped runtime answers it.

These rules hold for every harness:

- The spawn intent records the harness and workspace. The trust that a spawn
  adds follows from them alone, so a respawn under the same intent adds the
  same trust.
- Codex's override lives in the session's own arguments. The resume argv
  keeps it. Fleet writes no Codex configuration file.
- Codex keeps only the last `-c projects=` on its command line. A wrapper
  that passes its own `projects` override therefore loses it to Fleet's, or
  replaces Fleet's. The `[projects]` table in Codex's `config.toml` merges
  with the override, so keep other trusted roots there.
- Fleet never sends a key to a dialog. Claude Code's only documented trust
  interface persists in `~/.claude.json` on the host. A person trusts the
  repository once, by the dialog or that file. The Task's message names the
  host.

`e2e_workspace_trust` covers an agent that waits for trust and trust that
cannot be read, on the fake runtime. `e2e_runtime_exec` covers a Codex Task
on `runtime-mux` that is briefed before its first turn ends, and a Kimi Code
Task and a Copilot Task that are briefed before their first report.

## Agent state

`status.agentState` has two sources. The agent's hooks are the first; a
screen rule is the second, lower one. `fleet_api::screen::merge` is the
rule, pure:

| Profile `authority` | Hooks reported since the last start | A screen rule matched | `agentState` from | `activity.source` |
| --- | --- | --- | --- | --- |
| `full` | yes | any | the hooks | `hook` |
| `full` | no | yes | the screen | `screen` |
| `partial`, `session`, `none` | any | yes | the screen | `screen` |
| `partial`, `session`, `none` | yes | no | the hooks | `hook` |
| any | no | no | neither: `unknown`, never `idle` | `none` |

- The runtime's `screen` returns the screen; Fleet evaluates the rules
  (`fleet_api::screen::Rules::evaluate`). The runtime knows no harness.
- `status.activity` is `{source, rule, at}`: the rule is `<set>/<rule id>`
  for a screen state, and `at` is when Fleet first saw this state from this
  source.
- TaskController asks the screen only of a harness with rules, and not while
  the last pass took the state from hooks whose authority is `full`. The
  `exec` adapter answers one capture per session for 2 s.
- A capture that fails keeps a screen state. An unreadable signal file, as
  on another host, keeps the state unless a screen rule matches.
- A screen state is for a view only. Readiness, rest, a pause and the proof
  of a delivery read the hooks alone.

## Signal contract

`fleet_api::signal::Signals`: `file` names the file a session's hooks
append to, passed as `FLEET_SIGNAL_FILE` in the spawn's environment, and
`read` folds it (`Reported`: the last state, every prompt digest with
when it was last reported, the last conversation id). A file never
written is Known and empty; a file that cannot be read is Unknown.
[signal-contract.md](signal-contract.md) owns what `fleet signal` writes and
reads. The `FileSignals` adapter keeps the
files under `<data dir>/signals` for sessions on this machine, and answers
Unknown for any other host: reading a remote file takes the runtime's
`exec`. TaskController reads the file for every live session, not only one that
may be paused: `status.sessionState` is `Idle` once the agent's last report
is `idle`, and `Running` while it reports `working`, `blocked` or `prompt`.
`status.agentState` is the agent's report itself, finer: `working` for
`working` and `prompt`, `attention` for `blocked`, `idle` for `idle`, and
`unknown` while the agent has reported nothing since it last started. An
unreadable file keeps the last `agentState`. TaskController is its only
writer.
`status.activity.source` says which source set `agentState`
([agent state](#agent-state)).
`e2e_signal` covers state, an interactive session and pause,
`e2e_delivery_proof` the prompt proof, and `e2e_harness` the profiles' argv
and resume.

## Workspace contract

A Workspace is the set of files one Session works in (`fleet_api::workspace`).
A Task owns at most one. TaskController records it in `status.workspace`
as `WorkspaceRef`: Fleet's `key`, the `host` whose runtime made it, and the
`dirs` the runtime answered, one per URL in the Task's order. The workspace
key is the session key.

The runtime port carries the workspace calls:
`workspace(host, key, urls)`, `find_workspace(host, key, urls)` and
`release_workspace(workspace)`, each answering Known or Unknown. The exec
runtime maps them onto the protocol (docs/runtime/README.md, "Workspaces"):

- **Schemes first.** Before any call, Fleet reads the runtime's manifest.
  A runtime that lists no `workspace` verb, or not a URL's scheme in
  `workspace.schemes`, answers `Known(Unsupported { url })` for it.
- **`workspace`.** `Made { dirs, adopted }` is `Materialized`;
  `Unsupported { url }` and `Failed { why }` are answered as they are. The
  call is bounded by the runtime's `workspaceTimeoutMs`.
- **Once per key.** The verb is idempotent by key, so `find_workspace` is the
  same call again: a lost answer adopts the whole workspace and starts no
  new attempt.
- **Release.** `release_workspace` is `stop { key, release: true }`. A
  workspace already gone is `Deleted`. Repeating it is harmless.
- **Observed host.** The host whose runtime answered is the workspace's.
  Fleet sends no credential: the host and its runtime hold them.

The runtime keeps the rest: whole or nothing, work kept on release, never
removing what it did not make. `fleet runtime check` tests a runtime that
declares the verb, and `workspace-git` is the shipped workspace provider for git URLs.

The fake runtime makes workspaces as records on the fake host: a `file://`
URL is its directory, and any other URL a directory named after its last
segment under `<fake dir>/workspaces/<key>`.

`e2e_runtime_exec` covers a `git+file` Task on `runtime-mux`, run in its
worktree and released with its uncommitted work preserved, and a scheme the
runtime does not list.

## Forge contract

`fleet_api::forge::Forge`: `operation_bound`, `parse`, `whoami`, `change`
and `mark_ready`. `change` answers `Known(Some)` with a complete
`ChangeSnapshot`, `Known(None)` for proven absence, or Unknown with an
`Uncertainty` and an optional `retryAfter`. `mark_ready` takes the expected
head and state and an intent key, reads both afresh, answers `Moved` when
either differs (nothing mutated), `MarkedReady { already }` once the change
is out of draft, Failed when the forge said no, and Unknown only when the
mutation's answer could not be read. An adapter whose forge offers no
atomic head-and-state precondition answers Failed without reading or
mutating, and so does an unconfigured host. Adapters live in
`fleet-forge`, one per `[forge.hosts]` entry, chosen by the host a URL or
ref names; a `<organization>.visualstudio.com` host falls back to the
`dev.azure.com` entry:

- **github:** bounded `gh api graphql --hostname <host> --include` calls, one
  query paginated over reviews, comments and review threads; at most six
  calls per observation, Unknown with `Incomplete` past that. Mark-ready is
  one fresh read of `isDraft`, `state` and `headRefOid`, then
  `markPullRequestReadyForReview` with the intent key as `clientMutationId`.
- **gitlab:** bounded `glab api --hostname <host> --include` calls: the
  merge request, its discussions by page, its approvals; a 404 is absence
  only when the project itself answers. Mark-ready is one fresh read, then
  the title updated with its draft marker removed.
- **azure-devops:** bounded `az devops invoke` calls for hosted Azure DevOps
  Services. Organization/project/repository identities are checked against
  the URL. A collection that fills its bounded page is Unknown with
  `Incomplete`; no continuation is followed. Mark-ready fails closed because
  the provider exposes no atomic head-and-state precondition.
- **bitbucket-cloud:** bounded authenticated `curl` reads against
  `https://api.bitbucket.org/2.0/` for `bitbucket.org` only. The adapter
  brackets paginated comments, commit statuses and mergeability checks with
  two pull-request reads, requires every status to link to that exact head,
  and rejects repository or head movement. Participant
  votes are not exposed as head-bound reviews. Inline thread counts require
  explicit resolution evidence. Mark-ready fails closed because Bitbucket's
  `PUT draft=false` has no atomic head-and-state precondition.
- **fake:** a file-backed forge for tests and trials, with injectable faults
  (lost, timeout, throttle with `retryAfter`, incomplete, malformed, absent;
  for mark-ready `moved`, `failed`, `lost` and `lost-applied`). It runs no
  process and records every mark-ready that mutated a change.

Credentials stay with each external client's own configuration; Fleet passes
none and stores none. Bitbucket Cloud uses curl configuration or netrc rather
than assuming an official provider CLI.

## Gateway contract

`fleet_api::gateway::Gateway`: `operation_bound`, `parse`, `event`,
`whoami`, `capabilities`, `history`, `open`, `post` and `react`, each
answering Known or Unknown; a refused write is `Known(EffectOutcome::Failed)`, and
`open` and `post` answer `Opened { conversation, message, at }` and
`Posted { message, at }` with the platform's own time. `history` answers a
complete `History` with its `starter`, `Unsupported` where bots cannot read
history, or Unknown with an `Uncertainty` and an optional `retryAfter`.
An address is `<platform>:<chat>[:<thread>]`, a Conversation is named
`conv-<12 hex of sha256(<account>/<address>)>` (`fleet_manager::conversation_name`),
and `admit` sorts a message into `Own`, `Admitted` or `Context` from a
route and the people table. The `[gateway]` section is validated at load
as the [config page](configuration.md#the-gateway-section)
states, a route directory that equals, contains or lies inside another's
included; a platform this build lacks, an `inbound` its listener cannot
deliver by, or a fake without a dir is refused when the gateway opens.
Adapters live in `fleet-gateway`, one per `[gateway.accounts]` entry:

- **fake:** a file-backed platform for tests and trials, with injectable
  faults (`lost`, `timeout`, `throttle` with `retryAfter`, `incomplete`,
  `malformed`; for `open` and `post` `failed`, `lost` and `lost-applied`)
  and settable `capabilities`: history on or off, a `metadata`, `nonce` or
  `none` marker, `unmentioned` and `threads`. It runs no process and
  records every post it landed, in order.
- **slack:** the Web API through one bounded `curl` per call, with the
  operator's curl config named and never read. Posts carry the intent key
  as message metadata, so the marker is `metadata`.
- **discord:** the REST API through one bounded `curl` per call, the same
  way. Posts carry a nonce derived from the intent key with
  `enforce_nonce`, so the marker is `nonce`, and a post with an Unknown
  outcome is resent under the same key inside the window.

`listener_account` is what a listener starts from, with its digest over the
account's name, platform, team, bot and inbound method, and a Discord
account's Gateway intents.

## The command line

`fleet
init|up|apply|get|list|status|send|amend|label|declare|result|delete|reconcile|watch|attach|term-bridge|event|gateway|signal|runtime|integration|doctor|version`.
Exit codes:
0 ok, 1 refused, 2 usage, 3 not found, 4 Unknown, 5 conflict, 6 lease, 7 timed out.
`apply`, `get`, `list`, `status` and `delete` take `changerequest`,
`conversation` and `listener` as well as `task`, `taskconfig` and
`envprovider`; `status changerequest NAME --json` prints the ref and the
whole numbered snapshot, `status conversation NAME --json` the tie, the
numbered messages and the spool. A Conversation manifest is applied
through the account's adapter, under its derived name; a Listener manifest
is refused, since the config is its only source. `fleet gateway receive
--account NAME [--beat]` is what a listener runs per envelope and per
heartbeat; `fleet gateway account NAME --json` is what it starts from, and
`fleet gateway status` shows each account's listener.

`fleet watch --json` is how a view follows the Tasks: one NDJSON line per
event, until Ctrl-C, or until the first `synced` with `--once`. It reads the
store every `--poll-ms` (500 by default) and takes no lease:

| Event | Fields | Meaning |
| --- | --- | --- |
| `changed` | `revision`, `object` | the whole Task, written at `revision` |
| `deleted` | `revision`, `name`, `uid` | the Task is gone from the store |
| `synced` | `revision`, `names` | the first read is reported; `names` are the Tasks present then |

`revision` is the store's write counter, the `resourceVersion` every write
raises. `--since N` reports only Tasks written after `N`, so a view resumes
from the last `revision` it read. A deletion leaves nothing behind to compare,
so a resuming view drops each Task it holds that `synced` does not name. An
unreadable store reports nothing but a warning on standard error, and the next
read tries again: Unknown never looks like a deletion. The first read that
works after an unreadable one ends with `synced` again. A list reads each
Task on its own, so one read can hold
a later write and miss an earlier one; a read is reported only once the read
after it is the same. Writes are serialized, so by then every write below the
highest `revision` reported is in it, and a resume skips none.

`fleet event publish [--board B] --type T [--subject S] [--key K]
[--correlation C] --data JSON [--wait [--timeout-ms MS]] [--json]` appends
one event to a Board, `shared` by default, through the manager's
`publish_event` verb, as `cli/<user>`, and prints it with its `seq`. A
refused event exits 1 and a missing Board exits 3. `--wait` changes nothing
there. Inside a session, `--wait` reads the session key from the outbox's
name (`fleet_runtime::events::outbox_session_key`, the inverse of
`<key>.jsonl` and `event::outbox_file`) and refuses, appending nothing, a
name Fleet did not make. It opens the store as the read-only commands do
(`--config`, then `FLEET_CONFIG`, then the per-user config), and before
the append reads, with no lock, the manager's `before_append`: the Task
whose `session_key(name, uid)` is that key, its `status.outbox`
(generation `g0`, offset `o0`) and the Board's head (0 for a Board that is
not there). The line starts at the outbox's length before the append. A
line that starts below `o0` is in a file made again, which Fleet reads
under `g0 + 1`; any other line under `g0`. That generation gives the
line's key when it names none (`event::outbox_key`). The manager's
`wait_ingested` then polls every 200 ms for `--timeout-ms` (60000 by
default), and reads again after a `Busy` or `Unknown` read, with a warning
on standard error. It waits until the Task's `status.outbox`, compared as a
(generation, offset) pair, reaches (that generation, the line's end).
TaskController appends a line's event before it moves the cursor past the
line, so the event is then on the Board, from that Task by name and uid,
under that key. `before_append` also keeps the Board's uid, read before
and after the head, and reads both again (at most 3 times) when the uid
changed between them. Once the cursor is past the line, a `lastSkipped`
that names the line's offset in the line's generation answers first: the store refuses a line, such
as one above `maxDataBytes`, before it looks at its key. While the Board's
uid is the same, the event is first looked for above the head read
before the append, then once from the start up to that head: an event found
there is a key the Task already used, and prints with `(already published
under this key)`, as a publish outside a session does. A Board made again
since is searched from its start, and an event found there is new. A line
with no event is not on the Board (skipped, dropped by retention, or in an
outbox made again): the command exits 1 and says so, with `lastSkipped`'s
reason when it names the line's offset (`OutboxCursor::skipped_at`). The
line's own fields never name the Task. A timeout prints the queued line, as
a publish without `--wait` does, and exits 7: Fleet has not read the line
in yet, and may still, unless the outbox was made again.
`fleet event list [--board B] [--after SEQ] [--type P]
[--subject P] [--source S] [--correlation C] [--limit N] [--json]` prints the
first `N` (100 by default, at most 10000) events above `SEQ` that match,
oldest first, up to the last `seq` in the log when the list starts: an event
published during the list waits for the next one; `--json` prints them as
an array. A `--type` or `--subject` value that ends in `*` matches a prefix.
A cursor older than the log exits 5 and names `oldest`; with `--json` it
also prints `{"expired": {"oldest": N}}`. The [Board page](resources/board.md#events) owns the event fields.

`fleet event wait --after SEQ --timeout-ms MS [--poll-ms MS] [--limit N]`
and `fleet event watch --after SEQ [--poll-ms MS]` take the Board and
filters of `list` and read through the manager's `EventCursor`, with no
lock. The cursor's `after` moves past each event a read returns, kept or
not, and stops at the last event answered when `--limit` cuts the answer,
so the next read repeats no event and passes none over. A read that
expires or fails after some events were kept answers those, and the next
read meets the expiry or the error again. One read takes at most
`verbs::MAX_EVENT_PAGES` (4) pages of 1000 events, so a reader behind a
Board that grows faster than it reads hands on its matches and checks its
deadline between reads; it reads again at once, with no poll, while its
cursor still moves. A torn last line
is not read until it is whole, and the log gives `seq`s with no gap, so an
event the log gains under a read is in the next one. `wait` (the manager's
`wait_events`) reads every `--poll-ms` (500 by default, 50 to 10000) until
a read holds a match, prints the matches as `list` does and exits 0; with
no match by `--timeout-ms` it prints nothing and exits 7. `watch` prints
each match, oldest first, and reads again every `--poll-ms` when a read
holds none. With `--json` each event is one compact JSON line (NDJSON),
flushed; without it each is one line of `list`'s table. It runs until a
write to stdout fails, so a closed reader ends it at the next event it
prints, with exit 0. For both, a `Busy` or `Unknown` read
(`verbs::is_transient`) is a warning on standard error, and the next poll
reads again, as in `fleet watch`; `wait` still ends at its timeout. An
expired cursor exits 5 and names `oldest` and the cursor the read was at,
with `{"expired": {"oldest": N}}` on stdout under `--json`.

`fleet event subscribe TASK [--board B] [--type P] [--subject P]
[--source S] [--correlation C] [--after SEQ]` adds one item to the Task's
`spec.subscriptions` through the manager's `subscribe` verb, and refuses
one with the same Board and filter (exit 1). `fleet event unsubscribe TASK`
with the same Board and filter flags removes it through `unsubscribe`; no
such item exits 3, and so does a missing Task. The
[Board page](resources/board.md#subscriptions) owns the notices.

`fleet apply --create-only` creates each object, of every kind it takes, or
refuses one whose name is taken with exit 5 and leaves it as it is; the
store's create is
exclusive, so two clients applying one name at once get one object and one
refusal. A view creates its Tasks this way.

`fleet runtime hosts --json` prints `{"hosts": [...]}`: the hosts a Task may
name, `local` first and then each other place, `<runtime>/<place>`, or
`null` when the fake `[runtime]` serves any. The command keeps its old name
until thurbox reads `fleet runtime places`. `fleet runtime places --json` prints
each place with its readiness
([configuration](configuration.md#runtimes-and-their-places)). `fleet runtime browse <place> [dir] --json`
prints `{"dir", "entries"}` and `fleet runtime branches <place> <repo> --json`
prints `{"names", "default"}`, read through the runtime's `browse` and
`branches` verbs. `fleet version --json` lists the `harnesses` with a profile of
their own; any other harness name is run as a program. A view checks a
Task against both before it applies it.

`fleet doctor [--json]` checks this machine. It reads and writes nothing in
the store. Each check has one level: `ok`, `info`, `warn` or `fail`. A check
that needs a step names it as a hint.

| Area | What it checks | Levels |
| --- | --- | --- |
| `integration` | each harness with an `[install]` table, as `fleet integration status` reports it ([installed integrations](integration.md)); each ledger entry that no harness profile with an `[install]` table names | `warn` for `outdated` and `modified`; `warn` for an entry left behind, with `fleet integration uninstall <agent>` as its hint; `info` for `not-installed`, `agent-missing` and `unsupported-platform`; `fail` when the ledger cannot be read |
| `harness` | each harness's `program`, found on PATH | `info` when it is not there: a user installs only the agents they run |
| `runtime` | each place of each runtime, by its id `<runtime>/<place>`: its runtime program answers `manifest` with protocol 1 or 2 and the required verbs, and reports the place ready; a version 1 runtime serves only `local` | `fail` when a place is not ready, with the runtime's why; `fail` when it does not answer; `fail` when no runtime is configured, or when `[runtime]` is not the fake with its `dir` |

The exit status is 1 only when a check fails. Warnings alone exit 0.
With `--json`, it prints `{"checks": [...], "failed": N}`. Each check has
`area`, `name`, `level`, `detail` and `hint`.

`fleet attach TASK [--wake] --json` prints `{"stream": {"argv": [...]}}`: the command
that opens the Task's [terminal stream](../runtime/README.md#the-terminal-stream),
run where the view runs. Fleet asks the runtime serving the session's host
for its `attach` answer. A `stream` answer is passed through; a `tty` answer
is wrapped as `fleet term-bridge -- <argv>`, so a view only ever gets a
stream. A Task with `Paused=True` exits 3 and says it is paused, unless
`--wake` is given: then Fleet first sets `status.wakeRequestedAt`, which
TaskController resumes the session for, and waits up to 60 seconds for
`Paused` to leave True. A Task still paused after that wait exits 3 and says
it is paused. With `--wake`, a paused Task that is held or Stalled exits 3 at
once, because Fleet never resumes it. A view that attaches every Task in the
background, as thurbox does, never passes `--wake`, so it wakes nothing. A
Task with no session yet, a
runtime that does not declare `attach` and a key the runtime holds no
process under exit 3; an Unknown answer exits 4.
Without `--json` the argv is printed one argument per line.
`fleet shell TASK [--json] [--wake]` is the same for an interactive shell beside the
Task's session, on its place: Fleet asks the runtime's
[`shell`](../runtime/README.md#the-shell) and answers as `fleet attach` does.
A runtime that does not declare `shell` exits 3.
`fleet term-bridge -- <argv>` is that bridge: it runs the command on a pty of
its own through `fleet-exec` and relays it as the terminal stream on its stdin
and stdout, until the view detaches or the command exits.

The config file is versioned TOML; relative paths are relative to it, and
[configuration](configuration.md#native-fleet-configuration-fleettoml) owns its
fields. This one runs the fake runtime, and the test
`the_contract_examples_apply_as_written` applies it with the Task below:

```toml
version = 1

[data]
dir = "data"

[runtime]
kind = "fake"
dir = "fake-host"
```

A real host is served by a runtime program, and a `[forge]` section names
the forges ChangeRequests are read from:

```text
[runtimes.mux]
program = "runtime-mux"   # a path or a name on PATH
callTimeoutMs = 15000     # the bound on every call; shorter than the lease
keyPrefix = "fleet"       # give a second fleet on the same runtime its own
                          # no places table: one place, spec.host = "local"

[forge.hosts]
"github.com" = { kind = "github", cli = "gh" }
"bitbucket.org" = { kind = "bitbucket-cloud", cli = "curl" }

[forge.readyPolicies.checks]   # selected by spec.forge.draft.policy or
forge.draftPolicy
checksPassed = true
```

A Task manifest:

```json
[
  {
    "apiVersion": "fleet/v1alpha1",
    "kind": "Task",
    "metadata": {"name": "design-305", "labels": {"topic": "fleet-305"}},
    "spec": {
      "harness": "claude-code",
      "host": "local",
      "brief": "Write the controller design.",
      "workspace": [],
      "dispatch": true,
      "outputs": {"prUrl": {"type": "string", "required": true}}
    }
  }
]
```

`fleet reconcile` takes the manager lease, runs passes until nothing changes
(or `--max-passes`), and gives the lease back. Its default lease is 30 s,
raised to twice the longest operation of the runtime, forge, gateway or
plugin port.

`fleet init` creates the native per-user config offline. A config that
exists is kept, never replaced. It also writes the TaskConfig `default`, with
`pauseAfterMs` 1800000, into the config's store when the store holds none
([the default TaskConfig](resources/task-config.md#the-default-taskconfig)),
and the Board `shared`, with the default spec, when the store holds none. The
[initialization guide](configuration.md#configuration-and-customization)
specifies precedence and paths.

`fleet up` creates no Task. It retains the manager lease across periodic
sweeps for all due Tasks. The poll interval
must fit inside the lease as well as runtime operations. Ctrl-C/SIGTERM stops
after bounded work and releases the lease; Tasks and sessions survive. Crash
recovery waits for lease expiry. Uncertain runtime observations remain Unknown.

`fleet up` reads its config file once, when it starts.
`e2e_bootstrap` covers installed first-run, repeat and concurrent
initialization, a kept customized config, the refused `[starter]`, resident
progress with no starter, exclusive startup and crash/reconnect.

## Tests

The `e2e_*` suites drive the real `fleet` binary against an isolated store,
the fake host or `runtime-mux`, and, for `e2e_forge`, the fake forge
or a stub copied as `az`, `curl`, `gh` and `glab`, across process
restarts and injected faults; `e2e_inbox` covers `fleet send` and delivery on
the fake host; `e2e_event_board` covers `fleet event publish` and `list`,
the Board `fleet init` makes, the refusals, the dedupe, the filters, an
expired cursor, and a session's publish through its outbox, as its Task;
`e2e_event_subscriptions` covers `fleet event subscribe` and
`unsubscribe`, the digest notice, its hold while undelivered, an expired
cursor, an unreadable Board and a Board made later on the fake host;
`e2e_task_amend` covers `fleet amend`, its delivery to the same session,
its refusals, `--if-generation` and the result rules on the fake host;
`e2e_forge_controller` covers `fleet declare`, the join and the relay
against the fake forge and the fake host.
`e2e_mark_ready` covers the mark-ready effect against the fake forge and
the fake host, and through the stub `az`, `gh` and `glab`.
`e2e_gateway` covers the `[gateway]` section, the receive verb, the
Conversation mirror and the Listener against the fake gateway;
`e2e_gateway_controller` covers the tie, the join and the relay into the
inbox, `fleet send --answers`, the parent labels and a child's relay into
its parent, against the fake gateway and the fake host.
`e2e_telemetry` exports to an in-process OTLP/HTTP collector: the spans and
metrics of a Task's lifecycle and of forge polls arrive with their
identities and outcomes and without a brief, message or environment value;
nothing is sent when export is off; an unreachable collector changes no
command; a caller's `TRACEPARENT` parents a command's root span.
`e2e_bootstrap` proves that Ctrl-C on `fleet up` flushes its sweeps.
`e2e_runtime_exec` runs a Task through `runtime-mux` and `workspace-git`
with one `OTEL_EXPORTER_OTLP_ENDPOINT`: the spans of all three programs
arrive in the Task's one trace, the key metrics arrive, and the agent's
window inherits no `OTEL_*` variable or `TRACEPARENT`.
`e2e_runtime_exec` runs `fleet attach` against the reference script, whose
`tty` answer comes back bridged, and against `runtime-mux`, whose stream a
test view drives: snapshot, input, output, and a detach that leaves the
agent running. It runs `fleet shell` against `runtime-mux`, where a view
types into the shell and finds it in the Task's directory, and against the
reference script, which has no shell; it also drives `fleet term-bridge` on its own, and reads
`fleet runtime hosts`, `fleet runtime places`, `fleet runtime browse` and
`branches` (and their refusal when a runtime does not offer the verb), and
the harnesses `fleet version` lists.
`e2e_interactive` covers a Task with no brief: its spawn, its inbox and the
refused amendment; and `apply --create-only`, which never changes a Task
already there.
`e2e_watch` covers `fleet watch`: the first read, a resume with `--since`,
a deletion while the watch runs, and `synced` again after an unreadable
store.
`e2e_workspace` covers the workspace calls against the fake runtime: one
`workspace` call, the ready spawn, the release after the session, a crash
between the call and its receipt, a lost request, an unreadable key, a URL
the runtime does not handle, and the refused shapes before the URL list.
`task_controller`, `task_workspace`, `change_request_controller`, `forge_controller`,
`conversation_controller`, `listener_controller` and `gateway_controller`
test each controller as a pure function, one transition or Unknown per
case.
`store_contract` tests the store against races, torn writes, stale leases
and the fenced delete. The `architecture` suite enforces the graph above.
