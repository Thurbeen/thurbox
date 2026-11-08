# Fleet v2: the contracts of the Task core

Fleet v2 runs agent sessions through Task resources and their controller. A
Task is one agent session doing one brief. This file says what each part
guarantees
and which tests hold it to that. The [overall
architecture](architecture.md)
explains the component boundaries. The
[Task controller guide](controllers/task.md) defines the
resource fields, state machine and controller interface.

Fleet v2 is a clean break from the Python v1 control plane. It reads none of
v1's queue records, extension or operator configuration, and the root
`FLEET_MAJOR` file (`2`) keeps a v1 checkout from updating onto it.

## What is served

| Kind | Role | Status written by |
| --- | --- | --- |
| `Task` | one agent session doing one brief | TaskController; the worker writes `status.result` and `status.declared`; ForgeController writes `status.forge`; GatewayController writes `status.gateway`; an inbox append carrying `answers` writes `status.answered` |
| `TaskConfig` | reusable execution defaults | nobody (passive data) |
| `EnvProvider` | stored data and explicit file/command environment sources | nobody (passive data) |
| `ChangeRequest` | one mirrored change on a forge | ChangeRequestController |
| `Conversation` | the mirror of one chat thread | ConversationController; the account's listener, through `fleet gateway receive`, spools deliveries on `status.received` |
| `Listener` | the inbound health of one chat account | ListenerController; the account's listener, through `fleet gateway receive`, writes `beats`, `refused` and `beatRevision` |
| Plugin-defined | a registered external controller’s resource | `plugin/<name>`, including reserved host bookkeeping |

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
host. SDKs and issue-specific plugins remain separate projects.

The [Gateway controller](controllers/gateway.md) is built
in four steps. The first: the `Gateway` port in
`fleet_api::gateway`, with the `[gateway]` section's types, its validation
and the pure admission function; the `fleet-gateway` adapter crate,
depending on no Fleet crate but `fleet-api` and `fleet-exec`, with a
file-backed fake
that models every `capabilities` combination; the `[gateway]` config,
loaded and validated by the CLI, each route's brief read with it, whose
accounts are opened before a manager run so the lease outlasts the port's
worst case; the `Conversation` and `Listener` kinds with their controllers
([the mirror](#the-conversation-mirror)); and the `fleet gateway receive`,
`account` and `status` verbs. The second: ConversationController's
admission, Task creation under the workspace precondition, busy notice and
refusal post. The third: `GatewayController`, the third controller over the
Task, which owns `status.gateway`, keeps the tie to the Conversation its
labels name, relays the thread into the inbox under a cursor and a child's
words into its parent's inbox, from `gateway/` sources ([talking in a
conversation](#talking-in-a-conversation)); `status.answered`, written only
by an inbox append that carries `answers`; and the parent labels, which the
store checks at creation. The fourth: posting. GatewayController plans
`Post`, `Open`, `Receive`, `Log` and `React` actions; the manager performs
`Post`, `Open`, `Log` and `React` through the `Gateway` port and `Receive`
through the store, and writes a receipt under each key.
`e2e_gateway_post`, `e2e_gateway_logs` and `e2e_gateway_reactions` hold it
to that.

## Crates and the allowed graph

```text
fleet-api          pure           types and ports
fleet-controllers  pure           controllers and gate   -> api
fleet-exec         adapter        the one process spawner
fleet-store        adapter        file store             -> api
fleet-runtime      adapter        thurbox-cli, fake      -> api, exec
fleet-forge        adapter        az, curl, gh, glab, fake -> api, exec
fleet-gateway      adapter        curl (Slack, Discord), fake -> api, exec
fleet-plugin       adapter        bounded plugin stdio   -> api, exec
fleet-telemetry    adapter        OTLP export, off by default
fleet-manager      orchestration  lease, plans, verbs    -> api, controllers
fleet-cli          edge           the `fleet` binary     -> api, manager, store,
runtime, forge, gateway, plugin, telemetry
fleet-arch         tooling        the architecture test
fleet-dev          tooling        `cargo xtask`: gate and release
```

`crates/architecture.toml` declares the graph and these rules, and the
`architecture` test of `fleet-arch` enforces them on every gate run:

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
  `FieldManager::GatewayController`, `FieldManager::ConversationController`
  and `FieldManager::ListenerController`, only fleet-api and fleet-controllers
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
  return: a process group on Unix, a job object on Windows.
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
- fleet-telemetry depends on no other Fleet crate and is depended on by the
  CLI alone. Only fleet-telemetry reaches `opentelemetry_sdk` or
  `opentelemetry_otlp`: the providers, batching, exporter and shutdown bound
  are in one crate. The manager records through the `opentelemetry` API,
  a no-op without a provider; the pure crates reach no telemetry at all.

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
spec.brief          the brief, sent once, never changed
spec.amendments     append-only additions to the brief, through fleet amend
spec.workspace      {dir} or {repositories: [{name, url, ref?}]}
spec.dispatch       nothing spawns while false
spec.outputs        optional: the structured outputs a result must carry
spec.forge          optional: relay and draft overrides for declared changes
```

The spec is immutable once created, except `dispatch` and `forge`; a
different brief is a different Task. `amendments` only grows, through the
store's amend mutation (`amend_task` for `fleet amend`, a plugin's `amends[]`
verb through `plugin_mutate`): `update_spec`, and so `apply` and a plugin
patch, refuses any change to it, and `apply` keeps the stored amendments
when a manifest omits them, and a manifest that creates a Task with
`amendments` is refused. `harness`, `host` and `model` are non-empty and
may not start with `-`, since they reach the runtime as arguments; whether
each is present never changes after creation.

`spec.workspace` names what the session works in. It never says how the
workspace is made. [Workspaces](resources/task.md#workspaces)
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

- **Workspace once.** With a workspace port, a `{repositories}` Task plans
  `Materialize` under `materialize/<n>` before its spawn, with the session
  key as the workspace key. `Spawned` waits until `status.workspace` holds
  the whole workspace, and the spawn gets it as `ready`. After an unknown
  materialize outcome the controller looks the workspace key up: found is
  adopted, absent is retried after `RETRY_DELAY_MS`, unreadable waits.
  `MAX_ATTEMPTS` refusals stall the Task with
  `WorkspaceReady=False/MaterializeFailed`. An unmapped repository spends
  no attempt, as at spawn. After the release, and also when nothing was
  spawned, `CleanWorkspace` runs under `cleanWorkspace/<n>`. The finalizer
  goes only after a `Deleted` receipt. A failed or unknown clean is
  repeated after each wait, and three raise `NeedsAttention/CleanFailed`. A
  Task deleted while its materialize is unanswered looks the key up first.
  `task_workspace` and `e2e_workspace` cover each case.
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
- **Unmapped repository.** A spawn answered `Known(Unmapped { url })` spends
  no attempt. The Task returns to Pending with `Spawned=False` and
  `NeedsAttention`, both with reason `RepositoryUnmapped`. The spawn is tried
  again after `RETRY_DELAY_MS`, without limit, under the same effect key.
  A later spawn that does not answer `Unmapped` clears that `NeedsAttention`.
- **Unmapped trust.** A spawn answered `Known(TrustUnmapped { harness })`
  follows the same rules with reason `TrustUnmapped`. A later spawn that
  does not answer `TrustUnmapped` clears that `NeedsAttention`.
- **Result.** `fleet result` writes `status.result` as field manager
  `worker`, only once the brief has a receipt, only for
  `status.brief.generation`, never while an amendment waits to be delivered
  (unless its delivery was given up), and only when its outputs match
  `spec.outputs` plus the outputs of each amendment at or below that
  generation. The same result again is a no-op. Within one generation a
  different result is refused; a result for a newer generation replaces the
  older one. A result for `status.brief.generation` concludes the Task.
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
  `pause/<n>`. The session must be observed not stopped and not known
  lost, its inbox empty, and no brief, entry, catch-up note, revive or
  resume in flight. Active or Unknown clears `status.idleSince`; the idle
  time starts again at the next Idle answer. Unknown never pauses, and on
  a runtime that cannot pause the manager reads the agent's activity as
  Unknown. `Paused=True/Idle` changes no
  phase and raises no `WorkerGone`, `SessionLost` or revive.
- **Resume.** An inbox entry for a paused Task plans one `Resume` under
  `resume/<n>`; entries that arrive meanwhile wait in the inbox. Resumes
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
  its session is deleted, only once the session is proven at rest or gone.
- **Inbox.** `fleet send TASK TEXT` appends `{key, from, text, queuedAt}` to
  `status.inbox` as `cli/<user>`, under `task/<name>/<uid>/send/<n>`, `n`
  the resourceVersion appended against. The oldest entry not given up on is
  delivered as the brief is: to a session observed ready on that pass, under
  its key as the runtime intent key, journaled, one entry per pass, at most
  three sends; a lost answer is read back from the journal, never resent
  blindly. A Known receipt moves the entry's receipt into
  `status.deliveries` and the entry out of `status.inbox` in one write, and
  drops its intent, receipt and attempt records. Three refused sends raise
  `NeedsAttention/DeliveryFailed` naming the entry, which stays while later
  entries go out; three unreadable journal reads raise
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
- **Workspace.** On a route whose workspace is a `dir`, that directory is a
  base: every Task of one thread gets `<base>/<conversation name>`
  (`conversation::thread_dir`, computed purely), and the create's
  `exclusive` names that directory. A `repositories` workspace is the
  route's, with no precondition. The manager lists the store's Tasks under
  the lease immediately before the fenced create and answers `WorkspaceBusy`
  naming the Task whose `spec.workspace.dir` normalises to the thread's,
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
`lease.json`, `objects/<Kind>/<name>.json` and `tmp/`. The default is the
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
  `Task.status.inbox` and `status.answered`, which have none and which no
  status write may change. A controller writes only under the current
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
  `status.answered`, first answer wins; `record_relay` (ForgeController or
  GatewayController, under the lease) replaces the writer's own field,
  `status.forge` or `status.gateway`, and appends each listed entry under
  the same checks, in one write that a single refused entry fails whole;
  `record_delivery` (TaskController,
  under the lease) writes the controller's status and, in the same write,
  the receipt into the newest-100 `status.deliveries` and the entry out of
  `inbox`, refusing a key no entry holds. None changes another field;
  `store_contract` races appenders against the remover and tears the
  record.
- A Conversation's `status.received` and a Listener's `beats`, `refused`
  and `beatRevision` have no owner either. `receive_event` (the account's
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
`host` and Fleet's deterministic `key`). `status.sessionState` is what the
runtime last proved about it, folded by `SessionState::observe` from the
Known answers of `find`, `lost`, `paused` and `activity`: `Gone` when a
readable list lacks the Session, else `Lost`, else `Paused`, else `Idle`
or `Running` by activity, else `Running` on any proof of presence. Unknown
answers keep the last state. Nothing decides on the state; a view reads it.

`fleet_api::runtime::Runtime`: `host`, `create_dir`, `remove_dir`,
`prepare_execution`, `env_source`, `spawn`, `find`, `ready`, `workspace_trust`,
`brief`, `brief_delivered`, `at_rest`, `release`, `lost`, `revive`, `activity`,
`paused`, `pause` and `resume`, each answering Known or Unknown, and
`can_pause`. `create_dir` makes a directory with its missing parents on a host
and says whether it made it; `remove_dir` removes one and what it holds, a
directory already gone included. The thurbox adapter does both on the local host
and refuses any other, since thurbox has no verb that makes or removes a
directory on a remote host. Each adapter reports its longest single operation
(`operation_bound`), and `fleet reconcile` refuses a lease that could not
outlast it.

- **thurbox:** every call is one bounded `thurbox-cli … --json`. A session's
  thurbox key is `<keyPrefix>.<uid>`, and `--on-existing adopt` makes a
  repeated create return the session holding the key. The brief's delivery
  is journaled in the session's own metadata. Every brief, amendment, catch-up
  note and inbox entry goes to the agent's own inbox through `message send`,
  never into the terminal composer, so a working turn takes it. Thurbox
  picks the route and reports it; the receipt records it as `deliveredVia`.
  A `mailbox` answer is no delivery: the controller raises `BriefUndelivered`
  or `DeliveryQueued` and never sends that text again.
  `spec.model` reaches the harness as the variable `runtime.modelEnv` names
  for it. Before Codex's first
  lifecycle report, readiness additionally accepts its initial empty composer:
  a matching owned session, corroborated live Codex process, no reported or
  contradicted hook, and a visible-pane capture placing the cursor at the
  initial prompt under the welcome screen. Missing or different evidence
  remains Unknown. This input-only proof never authorizes release; `at_rest`
  still requires idle, done or stopped. `e2e_runtime` covers initial delivery,
  reconnect, dialog/cursor guards, delivery to a working session and
  unchanged deletion safety.
  thurbox keeps a session's row across a reboot. `lost` is `session get`
  reporting `hook_corroboration: "missing"`, and `revive` is `session
  restart --if-missing`, which resumes the conversation in the same row and
  worktree. A session with no window is at rest. `paused` reads `stopped`
  and `activity: "stopped"` from `session get`. Only `activity: "idle"` is
  Idle; `unknown`, `stopped` or a missing field is Unknown, and `state`
  words never prove Idle. `pause` is
  `session stop` and `resume` is `session start`, each after checking that
  the key still belongs to the recorded id. `e2e_task_pause` covers both.
- **fake:** a file-backed host for tests and trials, with injectable faults
  (lost answers, refusals, delays, hangs). It runs no process.

The runtime owns the workspace unless Fleet made it (see
[Workspace contract](#workspace-contract)). When Fleet made it, the spawn's
`ready` names the directory, and the runtime runs the session there and
makes nothing. Otherwise Fleet passes `spec.workspace` in the spawn and
deletes the session; every adapter keeps these rules:

- **Materialization.** The adapter decides how a workspace becomes files:
  a worktree, a clone, a copy or a restored snapshot. Branches, bases,
  source checkouts and credentials are the adapter's. Fleet sends none.
- **Whole or nothing.** If a spawn stops partway, the adapter rolls back or
  completes its own partial state. Fleet acts on `find` alone.
- **Cleanup.** `release` deletes the session. The runtime then cleans the
  workspace as it is configured to. Fleet never inspects or deletes a
  workspace, with one exception: a Gateway thread's directory that the
  manager made, which it removes when the thread retires (see
  [the Conversation mirror](#the-conversation-mirror)). Fleet
  never removes a directory it did not make, a route's base directory or
  a `repositories` workspace.
- **Refusal.** A workspace the adapter cannot make, or may not make, is
  `Known(Failed)` at spawn, never Unknown. A repository the adapter has no
  source for is `Known(Unmapped { url })`. A harness the adapter knows no
  workspace-trust interface for is `Known(TrustUnmapped { harness })`. The
  adapter answers both before any external call.

Thurbox maps a `{dir}` to `--repo-path`. It maps a ready workspace Fleet
made to `--repo-path dir` and one `--add-dir` per `extraDirs` entry, and
makes no worktree. It maps `{repositories}` to one worktree create:

- The first repository's source is `--repo-path`, and its `ref` is
  `--base-branch`.
- Each other repository is `--add-repo PATH[@REF]`. Thurbox starts a
  repository without a `ref` from the primary's `ref`.
- Every worktree is on the branch `<keyPrefix>/<session key>`. A source
  repository with a branch named exactly `<keyPrefix>` cannot hold these
  branches, so its spawns fail.
- `runtime.repositories` maps each URL to its source checkout on the host. A
  relative source is refused when the config loads. An unmapped URL is
  `Unmapped`. An `@` that `PATH@REF` cannot carry fails the spawn.
- A running `fleet up` reads `runtime.repositories` again before each sweep.
  The next spawn uses the new map. See
  [reloading](configuration.md#reloading-runtimerepositories).
- `release` runs `session delete --force` by default. Thurbox then removes
  the window and the worktrees it created.
- With `runtime.workspaceCleanup = "keep"`, `release` runs
  `session delete --soft`. The worktrees stay, and thurbox can restore the
  session.
- Cleanup is best effort. Fleet reads a delete that exits 0 as released,
  even when thurbox reports a worktree or remote teardown error.

The multi-repository layout is the same for both paths:

- With one repository, the agent starts in that repository's worktree.
- With several, thurbox starts the agent in a directory of its own. That
  directory holds one link per repository.

### Workspace trust

An agent harness can refuse to load a folder's project settings until a
person trusts the folder. This document calls that gate workspace trust.
Workspace trust is not a permission policy. It approves no command, edit or
tool, and Fleet changes no permission mode or sandbox to obtain it.

The trust root is the directory the agent keys the session's trust on:

- For `{dir}`, the trust root is the directory.
- For `{repositories}`, the trust root is the primary repository's source
  checkout. Codex and Claude Code key a worktree's trust on its main
  checkout. With one repository, that worktree is the session's cwd.
- Additional repositories get no trust. Neither agent loads their project
  settings.

`runtime.workspaceTrust` gives each thurbox harness one interface:

| Interface | Default harnesses | At spawn | `workspace_trust` answers |
| --- | --- | --- | --- |
| `codex` | `codex` | Adds `--agent-arg=-c --agent-arg=projects={"<root>"={trust_level="trusted"}}`. Initial readiness treats the harness as Codex, as it always treats `codex`. | `Trusted` when the row's `agent_args` are exactly that pair, the row's cwd and worktrees are the Task's workspace, and a readable screen shows no Codex trust dialog before its first hook. `Mismatch` otherwise. Unknown when thurbox does not report them or the screen cannot be read. Readiness also refuses a screen showing that dialog. |
| `claude` | `claude`, `claude-code` | Adds nothing. | `Trusted` once Claude reported a hook. `Unsupported` while Claude's trust dialog is current at the cursor. Unknown otherwise. |
| `none` | none | Adds nothing. | `Trusted`. |
| not listed | any other | Starts nothing: `TrustUnmapped`. | `Unsupported`, naming `runtime.workspaceTrust`, for a session that already exists. |

These rules hold for every interface:

- The spawn intent records the harness and workspace. The trust that a spawn
  adds follows from them alone, so a respawn under the same intent adds the
  same trust.
- The map is keyed by thurbox agent name, not by agent family. Thurbox does
  not report an agent's family. An agent entry such as `codex-coder` runs
  no session until `runtime.workspaceTrust` maps it. Its Task waits in
  Pending with `TrustUnmapped`, and the message names the entry to set.
  A session started without its trust could not gain it later.
- Codex's override lives in the session's own arguments. Thurbox keeps it
  and replays it on restart, resume and fork. Fleet writes no Codex
  configuration file.
- Codex keeps only the last `-c projects=` on its command line. An agent
  entry or a wrapper that passes its own `projects` override therefore loses
  it to Fleet's, or replaces Fleet's. A replaced override shows Codex's
  dialog, which the probe reports as `Mismatch`. The `[projects]` table in
  Codex's `config.toml` merges with the override, so keep other trusted
  roots there. Codex 0.165.0 showed this behaviour.
- An adopted session is checked like any other. A session without the exact
  override, or in another directory, is `Mismatch` and never gets a brief.
  The way out is to delete the Task and apply it again. Deleting only the
  session makes the Task `WorkerGone`, which is never respawned.
- A trust root loses any trailing separator. A root with a `.`, `..` or
  empty component is refused, because only its host can resolve it.
- On the local host, the trust root is resolved through symlinks, as Codex
  resolves its cwd. A remote host gets the path as configured. A remote
  symlink makes Codex show its dialog, which the probe reports as
  `Mismatch`.
- A filesystem root, or the local user's home, is never a trust root. A
  `codex` spawn there fails.
- Fleet never sends a key to a dialog. Claude Code has no per-session trust
  interface; its only documented one persists in `~/.claude.json` on the
  host. A person trusts the repository once, by the dialog or that file.
  A thurbox `session.pre_launch` script can write that file before each
  launch. The Task's message names the folder.

`e2e_workspace_trust` covers each interface, adoption, mismatch, Unknown and
recovery. Its rows and screens come from thurbox-cli 2.76.1, Codex 0.164.0
and Claude Code 2.1.312.

## Workspace contract

A Workspace is the set of files one Session works in (`fleet_api::workspace`).
A Task owns at most one. TaskController records it in `status.workspace`
as `WorkspaceRef`: Fleet's `key`, the `host` the adapter reported, the
primary `dir`, the `extraDirs` the session also spans and an optional
`trustRoot`. The workspace key is the session key.

`fleet_api::workspace::Workspaces`: `materialize`, `find` and `clean`, each
answering Known or Unknown, and `operation_bound`. The `[workspaces]`
section of the config selects the adapter. Without it, Fleet makes no
workspace. Every adapter keeps these rules:

- **Whole or nothing.** A materialize leaves a whole workspace or nothing.
  `find` reports a partial workspace as none, and the next `materialize`
  rolls it back first.
- **Once per key.** A `materialize` for a key that holds a whole workspace
  answers `Materialized { adopted: true }` and makes nothing.
- **Refusal.** A repository the adapter has no source for is
  `Known(Unmapped { url })`, answered before any command runs. A workspace
  the adapter cannot make is `Known(Failed)` and leaves nothing.
- **Observed host.** The adapter reports the host it made the workspace on.
  Fleet sends no credential: the host and the adapter hold them.
- **Bounded commands.** Each step is a bounded command on the host, through
  `fleet-exec`.
- **Cleanup.** `clean` removes the workspace. A workspace already gone is
  `Deleted`. Repeating it is harmless.

The `worktree` adapter serves the local host and refuses any other:

- Workspace `key` is the directory `<root>/<key>`. Each repository is a
  worktree at `<root>/<key>/<name>` on the branch `<branchPrefix>/<key>`
  of its source checkout, from its `ref` or the source's `HEAD`.
- `runtime.repositories` maps each URL to its source checkout.
- `dir` is the first repository's worktree. Its trust root is that
  repository's source checkout, as for a thurbox worktree.
- Each other repository's worktree is in `extraDirs`, in the Task's order.
  The runtime spans them; see the multi-repository layout under the
  [runtime contract](#runtime-contract).
- The marker `<root>/<key>/.fleet-workspace.json` is written last. A
  directory without it is partial.
- A branch left by an earlier attempt is checked out again, so no commit
  is lost. `clean` removes the worktrees and keeps the branches.
- A workspace names at most 4 repositories, so one call has a bound.

`worktree_adapter` covers the adapter against real repositories.

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
init|up|apply|get|list|status|send|amend|declare|result|delete|reconcile|gateway|version`.
Exit codes:
0 ok, 1 refused, 2 usage, 3 not found, 4 Unknown, 5 conflict, 6 lease.
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

The config file is versioned TOML; relative paths are relative to it. This
one runs the fake runtime:

```toml
version = 1

[data]
dir = "data"

[runtime]
kind = "fake"
dir = "fake-host"
```

A real host uses the thurbox runtime, and a `[forge]` section names the
forges ChangeRequests are read from:

```text
[runtime]
kind = "thurbox"
cli = "thurbox-cli"       # a path or a name on PATH
callTimeoutMs = 15000     # the bound on every call; shorter than the lease
keyPrefix = "fleet"       # give a second fleet on the same hosts its own

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
      "workspace": {"dir": "work/design-305"},
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

`fleet init` installs the pinned embedded VEGA starter offline and creates
native per-user config; `--update` advances unedited managed workspace files
and writes conflict candidates for customizations. Config is never replaced.
The [initialization guide](configuration.md#configuration-and-customization) specifies precedence,
paths, provenance and recovery. Bootstrap does not read v1 data.

`fleet up` ensures one ordinary `Task/vega` through the manager, then retains
the manager lease across periodic sweeps for all due Tasks. The poll interval
must fit inside the lease as well as runtime operations. Ctrl-C/SIGTERM stops
after bounded work and releases the lease; Tasks and sessions survive. Crash
recovery waits for lease expiry. Uncertain runtime observations remain Unknown.

`fleet up` reads its config file again before each sweep and applies
`runtime.repositories` only.
[Reloading](configuration.md#reloading-runtimerepositories)
states the rules.
`e2e_bootstrap` covers installed first-run, repeat/concurrent initialization,
customized updates, resident progress, exclusive startup, crash/reconnect and
the `runtime.repositories` reload.

## Tests

The `e2e_*` suites drive the real `fleet` binary against an isolated store,
the fake host or a stub `thurbox-cli`, and, for `e2e_forge`, the fake forge
or the same stub copied as `az`, `curl`, `gh` and `glab`, across process
restarts and injected faults; `e2e_inbox` covers `fleet send` and delivery on
both the fake host and the stub; `e2e_task_amend` covers `fleet amend`, its
delivery to the same session, its refusals, `--if-generation` and the result
rules on the fake host; `e2e_forge_controller` covers `fleet declare`,
the join and the relay against the fake forge and the fake host.
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
command. `e2e_bootstrap` proves that Ctrl-C on `fleet up` flushes its sweeps.
`e2e_workspace` covers the workspace port against the fake adapter: one
materialize, the ready spawn, the clean after the release, a crash between
the materialize and its receipt, a lost request and an unreadable key.
`task_controller`, `task_workspace`, `change_request_controller`, `forge_controller`,
`conversation_controller`, `listener_controller` and `gateway_controller`
test each controller as a pure function, one transition or Unknown per
case.
`store_contract` tests the store against races, torn writes, stale leases
and the fenced delete. The `architecture` suite enforces the graph above.
