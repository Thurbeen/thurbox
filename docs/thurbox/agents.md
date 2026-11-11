# Agents

thurbox 3.0 keeps no agent registry. A session is a Fleet Task (ADR-41), and
the agent it runs is the Task's harness: Fleet starts it, resumes it and
reports its status through its harness profiles. thurbox no longer reads
`agents.toml`.

- **Which agents a session can run.** The creation flow offers the harnesses
  Fleet has profiles for, as `fleet version --json` lists them under
  `harnesses`. `thurbox-cli doctor` lists them beside the Fleet in force.
- **How an agent is launched, resumed and watched.** The harness profile
  owns the command, its arguments, the resume argv and the status hooks:
  [Fleet's Task controller](../fleet/controllers/task.md#harness-profiles).
- **Adding an agent.** A new agent is a new harness profile in Fleet, not a
  change to thurbox: a TOML file under `harnesses/` in Fleet's config dir
  ([profile format](../fleet/controllers/task.md#profile-format)).

An `agents.toml` left from thurbox 2.x is not read, and can be deleted.
