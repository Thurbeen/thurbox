---
name: thurbox-cli
description: The thurbox-cli binary: its subcommands (automation, editor, config, version, update, notify and notify send, perf, plugin, ui, schema, doctor), its output formats and exit codes. Use when changing or driving thurbox-cli. Sessions are Fleet Tasks, driven with `fleet`.
---

# thurbox-cli

*Working reference indexed by `AGENTS.md`. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## What it drives

Sessions are Fleet Tasks (ADR-41); their command line is `fleet`, not
thurbox-cli. thurbox-cli covers the interface's own files and a running interface:

| Subcommand | What it does |
| --- | --- |
| `automation …` | scheduled send, spawn and exec runs in `automations.json`; the interface ticks while it runs, `automation tick` from an OS timer |
| `editor get/set/mode` | the editor `Ctrl+O` opens, in `interface.json` |
| `config validate/show` | strict parse of every config file; the effective configuration and where each value comes from |
| `version [--check]`, `update [--force]` | the running version; the newest release of this major line |
| `notify [--test]`, `notify send` | which desktop-notification backend would deliver, and a sample; deliver one notification, with an optional `--key`/`--min-interval` dedupe |
| `perf [--plugins]` | the perf snapshot a running interface publishes to `perf.json` |
| `plugin …` | interface plugins: where they live, install, sync, check |
| `ui …`, `schema` | bounded requests to a running interface (`src/ui_control.rs`) |
| `doctor` | whether this machine has Fleet, with its harnesses and hosts, and the multiplexer |

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
dispatches; each subcommand is one file under `src/cli/`.
