---
name: thurbox-agents
description: The declarative coding-agent registry (agents.toml / AgentDef): the seeded built-ins and the shell entry, the entry format (the *_args groups, extends/append_args, vars, placeholders), what thurbox reads the registry for (the new-session agent list, the attach gate, doctor, config validate), and how a picked agent reaches Fleet as a harness. Use when adding or debugging an agent entry or the agent picker, or when an agent is refused at session creation.
---

# Thurbox agent definitions

*Working reference indexed by `AGENTS.md`. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

> Per-agent reference and the "adding a new built-in" checklist:
> `docs/thurbox/agents.md`. The full file contract: `docs/thurbox/config.md`.

## The registry

The set of agents is declared as data in `~/.config/thurbox/agents.toml`.
The file is seeded on first run with the built-ins `claude`, `codex`,
`antigravity`, `opencode`, `aider`, `copilot`, `vibe`, `pi` and `omp`, plus
`shell`. `default` names the entry a session gets when it names none.

`shell` is the one seeded entry that is not a coding agent: the platform's
interactive shell (`bash -i`, or `powershell -NoLogo` on native Windows;
`agent_config::interactive_shell_agent`).

- **Data type.** `session::AgentDef` and `session::AgentRegistry`
  (`src/session/agent_def.rs`, pure data and substitution).
- **Loading.** `agent::agent_config::load_or_seed()` reads the file and seeds a
  missing one. A syntax error, or a file with no usable entry, falls back to
  `builtin_registry()`. Unknown keys and skipped entries are warnings, never a
  failed load.
- **Live reload.** The snapshot re-reads the file
  (`agent_config::read_for_reload` / `parse_for_reload`) and publishes the new
  registry (`publish_registry`).

## What thurbox reads it for

Fleet launches every session (ADR-41). thurbox reads the registry for four
things:

- **The new-session agent list.** `read_agents` (`src/kernel/snapshot/mod.rs`)
  offers each entry's name and `command`, with whether that command resolves on
  `PATH` (`preflight::look_up`).
- **The attach gate.** A row whose agent has no definition, and no default
  either, is not attached (`start_attach`,
  `src/kernel/terminal/attach.rs`).
- **`thurbox-cli doctor`.** One `agent:<name>` finding per entry. A missing
  command is a warning; every command missing at once is a failure.
- **`thurbox-cli config validate`.** Parses the file and fails on any warning
  the load would only report.

## How a picked agent reaches Fleet

A create sends the picked agent's **name** as the Task's `spec.harness`
(`src/kernel/command/fleet.rs`). The name is refused unless `fleet version
--json` lists a harness profile of that name (`Fleet::harness`). Fleet's
profile (`crates/fleet-api/src/harness.rs`) owns the launch and resume argv,
the hooks that report state through `fleet signal`, and trust. So a custom
entry is launchable only under a name Fleet has a profile for, and the
launch-shaping fields below do not change what Fleet runs.

## The entry format

Each `[[agents]]` entry is an `AgentDef`:

```toml
default = "claude"

[[agents]]
name = "claude"
command = "claude"
args = []                                     # always passed
resume_args = ["--resume", "{id}"]
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]
additional_dir_args = ["--add-dir", "{dir}"]  # repeated per extra directory
```

Other fields: `resume_latest`, `hook_schema` (the hook family the CLI speaks,
e.g. `"claude"` on a rebranded claude), `env`, `telemetry_env`,
`telemetry_args`, `vars` and `setup`. The seeded file documents each inline
(`BUILTIN_AGENTS_HEAD` / `BUILTIN_AGENTS_TAIL` in `src/agent/agent_config.rs`).

- **Variants.** `extends = "base"` inherits every field except `name`; an
  explicit value (including an empty array or map, or `false`) replaces the
  base's. `append_args` appends after the resolved `args`. Chains resolve in
  either file order. A cycle, an unknown base, or a chain deeper than 32
  (`MAX_EXTENDS_DEPTH`) skips the entry with a warning.
- **`vars`** fill `{key}` once, on the fully resolved entry
  (`AgentDef::apply_vars`). They merge per key across `extends`.
- **Placeholders** must resolve (`AgentDef::placeholder_errors`): `{id}` and
  `{name}` only in the session groups, `{dir}` only in `additional_dir_args`,
  `{otlp_endpoint}`/`{otlp_protocol}` only in the telemetry fields, and `{home}`
  in command, args, groups, env and setup. An unknown placeholder skips the
  entry with a warning; it is never passed on literally.
- **`{session_args}`**, as a whole `args` token, is where `build_args` places
  the session group (`AgentDef::place_session_group`); without it the group
  leads.

No model is ever added. Put `["--model", "opus"]` in `args` (or in
`append_args` on a variant) to pin one.
