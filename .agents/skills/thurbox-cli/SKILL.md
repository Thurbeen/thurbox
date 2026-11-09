---
name: thurbox-cli
description: The thurbox-cli binary after ADR-41: its subcommands (editor, config, version, update, notify, perf, plugin, ui, schema, doctor), its output formats and exit codes, and where the session verbs went (Fleet's `fleet` command line). Use when changing or driving thurbox-cli, or when looking for a session, automation, task or message verb.
---

# thurbox-cli

*Working reference indexed by `AGENTS.md`. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## What it drives

thurbox keeps no session state (ADR-41). Every session is a Fleet Task, so
the session verbs are Fleet's command line, not thurbox-cli's:

| Was | Now |
| --- | --- |
| `thurbox-cli session list/get` | `fleet get tasks`, `fleet get task <name>` |
| `thurbox-cli session create` | `fleet apply -f <task>` (an interactive Task: no brief, dispatched) |
| `thurbox-cli session send` | `fleet send <task> <text>` |
| `thurbox-cli session delete` | `fleet delete task <name>` |
| `thurbox-cli watch` | `fleet watch --json` |
| `thurbox-cli task` | a Task with `dispatch: false` |
| `thurbox-cli message` | `fleet send`, and the Task's `status.inbox` |
| `thurbox-cli session signal` | `fleet signal` |
| `thurbox-cli automation` | none yet: a Fleet schedule kind is still to design |

thurbox-cli covers the interface's own files and a running interface:

| Subcommand | What it does |
| --- | --- |
| `editor get/set/mode` | the editor `Ctrl+O` opens, in `interface.json` |
| `config validate/show` | strict parse of every config file; the effective configuration and where each value comes from |
| `version [--check]`, `update [--force]` | the running version; the newest release of this major line |
| `notify [--test]` | which desktop-notification backend would deliver, and a sample |
| `perf [--plugins]` | the perf snapshot a running interface publishes to `perf.json` |
| `plugin …` | interface plugins: where they live, install, sync, check |
| `ui …`, `schema` | bounded requests to a running interface (`src/ui_control.rs`) |
| `doctor` | whether this machine has the multiplexer, each agent's command and each host's launcher |

## Output and exit codes

- Human text in a terminal, TOON down a pipe; `--json`, `--pretty`, `--toon`
  and `--text` force one. `--fields` picks a list's columns; `--full` keeps
  long text whole.
- stdout carries exactly one document. A command whose report is the answer
  and still fails (`config validate`) prints the report and exits 1, with the
  reason on stderr.
- A failure that printed nothing is a structured error on stdout:
  `{"error", "error_kind", "suggestion"}` and `help` next steps.
- Exit codes: `0` success, `1` ran and failed, `2` usage (clap's own
  wording), `3` ambiguous (several running interfaces and none chosen).
- A bare `thurbox-cli` prints its help.

## Where the code lives

`src/bin/thurbox-cli.rs` is the entrypoint; `src/cli/mod.rs` parses and
dispatches; each subcommand is one file under `src/cli/`. The architecture
rules hold `cli` to `session`, `paths`, `notifications`, `ui_control` and
`theme`, plus fully-qualified paths into `kernel`, `backend` and the
`agent`, `config` and `update` files it names.
