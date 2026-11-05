# Task controller

A Task runs one agent session for one brief. Its controller decides the next
step from stored Task state and explicit observations. The manager performs
that decision through the store and runtime ports.

| Read | Answers |
| --- | --- |
| [Task](../../resources/task.md) | What do I apply, inspect and report as a worker? |
| [TaskConfig](../../resources/task-config.md) | How do reusable defaults and frozen launches work? |
| [EnvProvider](../../resources/env-provider.md) | How do stored/file/command environment sources work? |
| [Native config](../../configuration/fleet-toml.md) | How do I configure the store, runtime and model variables? |
| [State machine](state-machine.md) | Which transitions, retries and recovery paths are possible? |
| [Interface](interface.md) | What are the inputs, outputs, ports and write ordering? |
| [Overall architecture](../../ARCHITECTURE.md) | Where does this controller fit in Fleet? |

## One run

1. A person applies a Task. `dispatch: false` keeps it Pending.
2. A caller runs `fleet reconcile`. For a dispatched Task on a reachable host,
   the controller plans a spawn under a deterministic session key.
3. The manager persists the intent, runs the effect and persists its receipt.
   Later passes recover ambiguous effects by their keys.
4. A ready session receives its brief in its agent's own inbox. Its
   delivery journal prevents a blind resend when an answer is lost. Messages
   queued with `fleet send` follow the same way, oldest first, one per pass,
   each under its own key. A working turn receives them too.
5. The worker reports through `fleet result`. The controller concludes the
   Task, retaining the session until deletion; messages sent afterwards are
   still delivered.
6. With `pauseAfterMs` set, the controller pauses an idle session through
   the runtime. The next inbox entry resumes it. See
   [pausing an idle session](state-machine.md#pausing-an-idle-session).
7. Deletion enters Releasing. The finalizer holds the record until the
   recorded session is released, once proven at rest or gone.

`fleet reconcile` is a bounded command. It repeats passes while changes are
made, then exits at a fixed point or `--max-passes`. It does not sleep until a
retry deadline or run a background watch; the caller invokes it again.
`fleet up` is the resident alternative: it retains the manager lease and
periodically invokes bounded sweeps for all due Tasks, independently of agent
activity. Its default VEGA session is an ordinary Task from the bundled
starter. See [bootstrap and recovery](../../../README.md#build-and-try-it).
`NeedsAttention` is a condition, not a phase: delivery reads and releases can
recover while attention is raised. See the [state machine](state-machine.md)
for the distinction between bounded sends and continuing recovery attempts.

## Implementation and evidence

| Responsibility | Source | Tests |
| --- | --- | --- |
| Pure decisions and requested probes | [TaskController](../../../crates/fleet-controllers/src/task.rs) | [Controller transitions](../../../crates/fleet-controllers/tests/task_controller.rs) |
| Resource and effect types | [Task types](../../../crates/fleet-api/src/task.rs), [plans](../../../crates/fleet-api/src/plan.rs) | [CLI lifecycle](../../../crates/fleet-cli/tests/e2e_task_lifecycle.rs) |
| Lease, status, intent and receipt ordering | [Manager](../../../crates/fleet-manager/src/lib.rs) | [Store contract](../../../crates/fleet-store/tests/store_contract.rs), [CLI lifecycle](../../../crates/fleet-cli/tests/e2e_task_lifecycle.rs) |
| Session identity and delivery journal | [Thurbox runtime](../../../crates/fleet-runtime/src/thurbox.rs) | [Runtime E2E](../../../crates/fleet-cli/tests/e2e_runtime.rs) |
| Workspace trust before the brief | [Runtime port](../../../crates/fleet-api/src/runtime.rs), [Thurbox runtime](../../../crates/fleet-runtime/src/thurbox.rs) | [Workspace trust E2E](../../../crates/fleet-cli/tests/e2e_workspace_trust.rs), [Controller transitions](../../../crates/fleet-controllers/tests/task_controller.rs) |
| Pausing an idle session and resuming it | [TaskController](../../../crates/fleet-controllers/src/task.rs), [runtime port](../../../crates/fleet-api/src/runtime.rs), [Thurbox runtime](../../../crates/fleet-runtime/src/thurbox.rs) | [Pause E2E](../../../crates/fleet-cli/tests/e2e_task_pause.rs), [Controller transitions](../../../crates/fleet-controllers/tests/task_controller.rs) |
| Inbox: `fleet send`, typed mutations and delivery | [Inbox types](../../../crates/fleet-api/src/inbox.rs), [verbs](../../../crates/fleet-manager/src/verbs.rs), [store](../../../crates/fleet-store/src/lib.rs) | [Inbox E2E](../../../crates/fleet-cli/tests/e2e_inbox.rs), [Store contract](../../../crates/fleet-store/tests/store_contract.rs), [Controller transitions](../../../crates/fleet-controllers/tests/task_controller.rs) |

The lifecycle E2E suite drives the built CLI from apply to result and release,
including crashes, failed attempts and Unknown observations. The runtime E2E
suite also covers replacement session identities, lost release answers and
brief delivery against a bounded `thurbox-cli` stub.
