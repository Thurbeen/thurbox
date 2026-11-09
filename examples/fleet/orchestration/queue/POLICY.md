# Worker policy

A brief defines one worker's scope and publication method. Other workers may
be active concurrently: preserve their source, processes and build output.
Work in an isolated worktree and keep temporary and build output private.

For defects, reproduce the behavior in an end-to-end test before fixing it.
Validate focused changes locally, then run the canonical `cargo xtask check`
on the combined candidate before publication. Never weaken a test or skip a
required gate to report success. Native platform claims require native tests.

Publishing uses the installed publish skill, normal signed hooks and
independent review of the exact candidate commit. A local-only contribution
must remain local. Do not merge, tag, release or deploy without authorization.
Never put private records, paths, host names, credentials or session identities
in public artifacts. Read the operator's voice rules before posting prose.

Every change you open on a forge is followed by Fleet, which relays its
review feedback to you. Three rules make that work, and they are yours to
keep:

- Open every change as a draft. Fleet never puts a change back into draft and
  never marks ready one that was ever seen ready; a change opened ready is
  left alone.
- Declare it with `fleet declare TASK --change-request URL` the moment it is
  opened. A change never declared is never followed.
- End every comment, reply and review you post on it with
  `<!-- fleet:worker <task uid> -->` on its own line; the uid is in
  `fleet get task TASK`. That mark is how Fleet tells your words from a
  person's on the same account and from another worker's; a post without it
  comes back to you as feedback.

Report the worker result through `fleet result TASK` with the generation
from the delivered brief, its outcome, summary, declared outputs and artifacts.
A result is a claim: a concluded worker is not proof that its artifact landed.
If a decision is needed, ask the person who gave you the work, with the exact
question and evidence; Unknown must remain Unknown. Never abandon work or
clear a human hold by inference. Continue independent authorized work while a
dependency remains unresolved.
