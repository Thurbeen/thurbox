# Worker policy documents

This directory retains reusable worker policy and the operator-policy form.
Operational work is represented by versioned Task resources in the
configured store. It is not stored as checkout queue files.
See [the Task resource](../../docs/resources/task.md) and
[the contracts](../../crates/CONTRACTS.md) for the resource model.

Keep private briefs, operator instructions and results outside this public
checkout. The installed binary does not parse or migrate previous records.
