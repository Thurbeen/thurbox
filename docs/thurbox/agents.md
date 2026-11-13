# Agents

A session is a Fleet Task (ADR-41), and the agent it runs is the Task's
harness. Fleet starts the agent, resumes it and reports its status through
its harness profile.

- **Which agents a session can run.** The creation flow offers the harnesses
  Fleet has profiles for, as `fleet version --json` lists them under
  `harnesses`. `thurbox-cli doctor` lists them beside the Fleet in force.
- **How an agent is launched, resumed and watched.** The harness profile
  owns the command, its arguments, the resume argv and the status hooks:
  [Fleet's Task controller](../fleet/controllers/task.md#harness-profiles).
- **Which agents Fleet ships**
  ([bundled profiles](../fleet/controllers/task.md#harness-profiles),
  [screen rules](../fleet/controllers/task.md#screen-rules)):

  | Agent | Harness | Its hooks | Screen rules |
  | --- | --- | --- | --- |
  | Claude Code | `claude`, `claude-code` | set at launch | yes |
  | Codex | `codex` | set at launch | yes |
  | pi | `pi` | set at launch | yes |
  | opencode | `opencode` | set at launch | yes |
  | kilo | `kilo` | set at launch | no |
  | omp | `omp` | set at launch | no |
  | Qwen Code | `qwen` | set at launch | no |
  | GitHub Copilot CLI | `copilot` | set at launch; not tested on Windows yet | yes |
  | Kimi Code | `kimi` | installed in its `config.toml` by `fleet integration install kimi`; not on Windows yet | no |

  An agent whose hooks are set at launch keeps its config dir as it was.
  Kimi Code reads hooks from its own config file only, so Fleet writes a
  block there, and `fleet integration uninstall kimi` removes it
  ([installed integrations](../fleet/integration.md)). The install runs on
  this machine only.
- **How status works.** Each hook runs `fleet signal <state>`, and a screen rule
  reads the state off the agent's screen as a second, lower source. Fleet
  reports the result as `agentState`: `working`, `attention` (it waits on a
  person), `idle`, or `unknown` before its first report. thurbox shows
  `attention` as blocked, and derives no state of its own.
- **Adding an agent.** A new agent is a new harness profile in Fleet, not a
  change to thurbox: a TOML file under `harnesses/` in Fleet's config dir
  ([profile format](../fleet/controllers/task.md#profile-format)).
