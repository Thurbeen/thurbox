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
- **Which agents Fleet ships**
  ([bundled profiles](../fleet/controllers/task.md#harness-profiles)):

  | Agent | Harness | Its hooks |
  | --- | --- | --- |
  | Claude Code | `claude`, `claude-code` | set at launch |
  | Codex | `codex` | set at launch |
  | pi | `pi` | set at launch |
  | opencode | `opencode` | set at launch |
  | kilo | `kilo` | set at launch |
  | omp | `omp` | set at launch |
  | Qwen Code | `qwen` | set at launch |
  | GitHub Copilot CLI | `copilot` | set at launch |
  | Kimi Code | `kimi` | installed in its `config.toml` by `fleet integration install kimi`; not on Windows yet |

  An agent whose hooks are set at launch keeps its config dir as it was.
  Kimi Code reads hooks from its own config file only, so Fleet writes a
  block there, and `fleet integration uninstall kimi` removes it
  ([installed integrations](../fleet/integration.md)). The install runs on
  this machine only.
- **How status works.** Each hook runs `fleet signal <state>`, and Fleet
  reports the agent's last state as `agentState`: `working`, `attention`
  (it waits on a person), `idle`, or `unknown` before its first report.
  thurbox shows what Fleet reports, and derives no state of its own.
- **Adding an agent.** A new agent is a new harness profile in Fleet, not a
  change to thurbox: a TOML file under `harnesses/` in Fleet's config dir
  ([profile format](../fleet/controllers/task.md#profile-format)).

An `agents.toml` left from thurbox 2.x is not read, and can be deleted.
